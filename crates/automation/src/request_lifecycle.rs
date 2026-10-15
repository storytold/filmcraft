//! Admission and completion ownership independent of an MCP response future.
//!
//! Closing rejects new requests but admitted cleanup may retain blocking work.
//! Clones of a permit count as one operation and release it only after the last
//! owner finishes, including a closure whose awaiting request was cancelled.

use crate::AutomationError;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::Notify;

const MAX_ACTIVE_REQUESTS: usize = 1024;
const DRAIN_TIMEOUT: Duration = Duration::from_secs(65);

#[derive(Default)]
struct State {
    closed: bool,
    active: usize,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    drained: Notify,
}

#[derive(Clone, Default)]
pub(crate) struct RequestLifecycle {
    inner: Arc<Inner>,
}

struct Permit {
    inner: Arc<Inner>,
}

#[derive(Clone)]
pub(crate) struct RequestGuard {
    _permit: Arc<Permit>,
}

impl RequestLifecycle {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn enter(&self) -> Result<RequestGuard, AutomationError> {
        self.retain(true)
    }

    /// Internal commands belonging to an admitted request can continue cleanup
    /// after admission closes. Move this guard into the blocking closure.
    pub(crate) fn retain_work(&self) -> Result<RequestGuard, AutomationError> {
        self.retain(false)
    }

    fn retain(&self, admission: bool) -> Result<RequestGuard, AutomationError> {
        let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
        if admission && state.closed {
            return Err(AutomationError::Other("MCP service is closing; new requests are not accepted".into()));
        }
        if state.active >= MAX_ACTIVE_REQUESTS {
            return Err(AutomationError::Other("MCP active request and blocking-work limit reached (1024)".into()));
        }
        let active = state.active.checked_add(1).ok_or_else(|| AutomationError::Other("MCP active request count overflow".into()))?;
        let guard = RequestGuard { _permit: Arc::new(Permit { inner: self.inner.clone() }) };
        state.active = active;
        Ok(guard)
    }

    /// Wait while the main runtime remains alive. A timeout retains all permits
    /// and their state; it never establishes that pending work was stopped.
    pub(crate) async fn close_and_drain(&self) -> Result<(), AutomationError> {
        let active = {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            state.active
        };
        if active == 0 {
            return Ok(());
        }
        tokio::time::timeout(DRAIN_TIMEOUT, self.wait_until_drained())
            .await
            .map_err(|_| AutomationError::Other("MCP request cleanup timed out; active requests or blocking work remain, completion unconfirmed".into()))
    }

    async fn wait_until_drained(&self) {
        loop {
            let notified = self.inner.drained.notified();
            tokio::pin!(notified);
            // Register before checking the count so concurrent final completion
            // cannot leave this waiter asleep. Multiple drainers are supported.
            notified.as_mut().enable();
            if self.inner.state.lock().unwrap_or_else(PoisonError::into_inner).active == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let drained = {
            let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.active = state.active.saturating_sub(1);
            state.active == 0
        };
        if drained {
            self.inner.drained.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn idle_close_does_not_require_a_timer_enabled_runtime() {
        let owner = RequestLifecycle::default();
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(owner.close_and_drain()).unwrap();
        assert!(owner.enter().is_err());
    }

    #[tokio::test]
    async fn close_waits_for_request_and_rejects_late_admission() {
        let owner = RequestLifecycle::default();
        let request = owner.enter().unwrap();
        let closing = owner.close_and_drain();
        tokio::pin!(closing);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut closing).await.is_err());
        assert!(owner.enter().is_err());
        drop(request);
        tokio::time::timeout(Duration::from_secs(1), closing).await.unwrap().unwrap();
        owner.close_and_drain().await.unwrap();
    }

    #[tokio::test]
    async fn blocking_clone_retains_admission_after_request_is_dropped() {
        let owner = RequestLifecycle::new();
        let request = owner.enter().unwrap();
        let closure = request.clone();
        let (release, wait) = std::sync::mpsc::channel();
        let work = tokio::task::spawn_blocking(move || {
            let _closure = closure;
            wait.recv().unwrap();
        });
        drop(request);
        let closing = owner.close_and_drain();
        tokio::pin!(closing);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut closing).await.is_err());
        release.send(()).unwrap();
        work.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), closing).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cancelled_drain_can_resume_and_multiple_waiters_finish() {
        let owner = RequestLifecycle::default();
        let request = owner.enter().unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(20), owner.close_and_drain()).await.is_err());
        assert!(owner.enter().is_err());
        let left = owner.clone();
        let right = owner.clone();
        let left = tokio::spawn(async move { left.close_and_drain().await });
        let right = tokio::spawn(async move { right.close_and_drain().await });
        drop(request);
        left.await.unwrap().unwrap();
        right.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn admission_is_bounded_and_recovers_when_a_request_finishes() {
        let owner = RequestLifecycle::default();
        let guards: Vec<_> = (0..MAX_ACTIVE_REQUESTS).map(|_| owner.enter().unwrap()).collect();
        assert!(owner.enter().is_err());
        drop(guards);
        assert!(owner.enter().is_ok());
        owner.close_and_drain().await.unwrap();
    }

    #[tokio::test]
    async fn admitted_cleanup_can_retain_blocking_work_after_close() {
        let owner = RequestLifecycle::default();
        let request = owner.enter().unwrap();
        let closing = owner.close_and_drain();
        tokio::pin!(closing);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut closing).await.is_err());
        assert!(owner.enter().is_err());
        let work = owner.retain_work().unwrap();
        drop(request);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut closing).await.is_err());
        drop(work);
        tokio::time::timeout(Duration::from_secs(1), closing).await.unwrap().unwrap();
    }
}
