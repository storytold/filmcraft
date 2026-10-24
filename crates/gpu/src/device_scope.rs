//! Serializes wgpu error scopes per process.
//!
//! An error scope stack belongs to the device, not to the thread. The export renderer pool shares one
//! device across rayon workers, so two threads pushing and popping scopes at once would pop each
//! other's scopes: an out-of-memory error lands on the wrong caller, or a stack is left unbalanced.
//! Every push/pop region takes this lock. A thread that already holds it (a nested call) does not
//! lock again, so the lock cannot deadlock on its own thread.

use std::cell::Cell;
use std::sync::{Mutex, MutexGuard, PoisonError};

static SCOPES: Mutex<()> = Mutex::new(());

thread_local! {
    static HELD: Cell<bool> = const { Cell::new(false) };
}

/// Holds the process-wide scope lock for the lifetime of the value (nested takes on one thread are no-ops).
#[must_use]
pub struct ScopeGuard {
    _lock: Option<MutexGuard<'static, ()>>,
}

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if self._lock.is_some() {
            HELD.with(|held| held.set(false));
        }
    }
}

/// Take the lock before pushing error scopes; keep the guard until they are popped.
pub fn lock() -> ScopeGuard {
    if HELD.with(Cell::get) {
        return ScopeGuard { _lock: None };
    }
    let guard = SCOPES.lock().unwrap_or_else(PoisonError::into_inner);
    HELD.with(|held| held.set(true));
    ScopeGuard { _lock: Some(guard) }
}

#[cfg(test)]
mod tests {
    use super::lock;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn nested_take_on_one_thread_does_not_deadlock() {
        let outer = lock();
        let inner = lock();
        drop(inner);
        drop(outer);
        // released: a fresh take succeeds at once
        let _again = lock();
    }

    #[test]
    fn a_second_thread_waits_until_the_scope_region_ends() {
        let (tx, rx) = mpsc::channel();
        let outer = lock();
        let handle = std::thread::spawn(move || {
            let _held = lock();
            tx.send(()).ok();
        });
        // blocked while this thread holds the region
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err(), "second thread entered during the region");
        drop(outer);
        rx.recv_timeout(Duration::from_secs(10)).expect("second thread enters after release");
        handle.join().expect("worker finished");
    }
}
