//! The Lumetri Scopes' background worker: the scope signal of a frame (for HDR scopes a
//! working-space render, sources decoded) and the scope images are computed here, never in the UI
//! pass, so an open Scopes panel cannot stall the preview.
//!
//! Work is keyed per *slot* (a scope signal or one scope image of a panel) and the newest request
//! wins: a request that has not started yet is replaced by the next one for its slot, so playback
//! never queues up scope work. The panel shows the last finished result until the new one lands
//! (the worker asks egui for a repaint then). Without threads (wasm32) a request runs at once.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

/// A finished piece of scope work, shared with the panels that show it.
pub type Done = Arc<dyn Any + Send + Sync>;

type Job = Box<dyn FnOnce() -> Option<Done> + Send>;

#[derive(Default)]
struct State {
    /// Slot → (key, job) not started yet.
    pending: HashMap<String, (u64, Job)>,
    /// Slot → the key of the job running for it.
    running: HashMap<String, u64>,
    /// Slot → the newest finished result and its key.
    done: HashMap<String, (u64, Done)>,
    /// Jobs run so far (tests).
    ran: u64,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    repaint: Mutex<Option<egui::Context>>,
}

pub struct ScopeWorker {
    shared: Arc<Shared>,
}

impl Default for ScopeWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeWorker {
    /// A worker with its thread (none on wasm32: requests run on submit).
    pub fn new() -> Self {
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), cv: Condvar::new(), repaint: Mutex::new(None) });
        #[cfg(not(target_arch = "wasm32"))]
        {
            let sh = shared.clone();
            if let Err(e) = std::thread::Builder::new().name("filmcraft-scopes".into()).spawn(move || run(&sh)) {
                log::error!("scopes: no worker thread ({e}); scopes are computed in the UI pass");
            }
        }
        Self { shared }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask for `job`'s result under `key` in `slot`, unless that result is already there or on
    /// its way; a job for an older key that has not started is dropped. `ctx` is repainted when
    /// the result lands.
    pub fn submit(&self, ctx: &egui::Context, slot: &str, key: u64, job: impl FnOnce() -> Option<Done> + Send + 'static) {
        *self.shared.repaint.lock().unwrap_or_else(PoisonError::into_inner) = Some(ctx.clone());
        let mut st = self.state();
        if st.done.get(slot).is_some_and(|d| d.0 == key) || st.running.get(slot) == Some(&key) || st.pending.get(slot).is_some_and(|p| p.0 == key) {
            return;
        }
        st.pending.insert(slot.to_string(), (key, Box::new(job)));
        drop(st);
        if cfg!(target_arch = "wasm32") || !self.has_thread() {
            self.run_pending();
        } else {
            self.shared.cv.notify_one();
        }
    }

    /// The newest finished result of `slot` (any key) and its key.
    pub fn result(&self, slot: &str) -> Option<(u64, Done)> {
        self.state().done.get(slot).cloned()
    }

    /// Jobs run so far.
    pub fn ran(&self) -> u64 {
        self.state().ran
    }

    /// Whether work for `slot` is queued or running.
    pub fn busy(&self, slot: &str) -> bool {
        let st = self.state();
        st.pending.contains_key(slot) || st.running.contains_key(slot)
    }

    fn has_thread(&self) -> bool {
        // the worker thread holds the only other reference
        Arc::strong_count(&self.shared) > 1
    }

    /// Run the queued jobs on this thread (wasm32, or no worker thread).
    fn run_pending(&self) {
        while let Some((slot, key, job)) = take(&self.shared) {
            finish(&self.shared, slot, key, job);
        }
    }
}

impl Drop for ScopeWorker {
    fn drop(&mut self) {
        self.state().stop = true;
        self.shared.cv.notify_all();
    }
}

/// The next queued job, marked running.
fn take(sh: &Shared) -> Option<(String, u64, Job)> {
    let mut st = sh.state.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = st.pending.keys().next().cloned()?;
    let (key, job) = st.pending.remove(&slot)?;
    st.running.insert(slot.clone(), key);
    Some((slot, key, job))
}

/// Run `job` (a panic is logged and leaves the slot's last result) and store what it made.
fn finish(sh: &Shared, slot: String, key: u64, job: Job) {
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).unwrap_or_else(|_| {
        log::error!("scopes: computing {slot} panicked");
        None
    });
    let mut st = sh.state.lock().unwrap_or_else(PoisonError::into_inner);
    st.running.remove(&slot);
    st.ran += 1;
    if let Some(out) = out {
        st.done.insert(slot, (key, out));
    }
    drop(st);
    if let Some(ctx) = sh.repaint.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
        ctx.request_repaint();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run(sh: &Arc<Shared>) {
    loop {
        let next = {
            let mut st = sh.state.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if st.stop {
                    return;
                }
                if !st.pending.is_empty() {
                    break;
                }
                st = sh.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
            drop(st);
            take(sh)
        };
        if let Some((slot, key, job)) = next {
            finish(sh, slot, key, job);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn wait(w: &ScopeWorker, slot: &str, key: u64) -> Option<Done> {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if let Some((k, d)) = w.result(slot)
                && k == key
            {
                return Some(d);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }

    #[test]
    fn the_newest_request_of_a_slot_wins() {
        let w = ScopeWorker::new();
        let ctx = egui::Context::default();
        // hold the worker on a slow job, then queue three requests for one slot
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        w.submit(&ctx, "block", 1, move || {
            let _ = rx.recv_timeout(Duration::from_secs(10));
            None
        });
        while !w.busy("block") || w.state().pending.contains_key("block") {
            std::thread::sleep(Duration::from_millis(1));
        }
        let runs = Arc::new(AtomicUsize::new(0));
        for key in [10u64, 11, 12] {
            let runs = runs.clone();
            w.submit(&ctx, "sig", key, move || {
                runs.fetch_add(1, Ordering::Relaxed);
                Some(Arc::new(key) as Done)
            });
        }
        let _ = tx.send(());
        let d = wait(&w, "sig", 12).expect("result");
        assert_eq!(d.downcast_ref::<u64>(), Some(&12));
        assert_eq!(runs.load(Ordering::Relaxed), 1, "the two older requests never ran");
        // the same key again: nothing to do
        let before = w.ran();
        w.submit(&ctx, "sig", 12, || Some(Arc::new(0u64) as Done));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(w.ran(), before);
    }

    #[test]
    fn jobs_run_on_the_worker_thread_not_the_caller() {
        let w = ScopeWorker::new();
        let ctx = egui::Context::default();
        w.submit(&ctx, "sig", 1, || Some(Arc::new(std::thread::current().name().map(str::to_string)) as Done));
        let d = wait(&w, "sig", 1).expect("result");
        assert_eq!(d.downcast_ref::<Option<String>>(), Some(&Some("filmcraft-scopes".to_string())));
    }

    #[test]
    fn a_panicking_job_leaves_the_last_result_and_the_worker_running() {
        let w = ScopeWorker::new();
        let ctx = egui::Context::default();
        w.submit(&ctx, "img", 1, || Some(Arc::new("first") as Done));
        assert!(wait(&w, "img", 1).is_some());
        #[allow(clippy::panic)]
        w.submit(&ctx, "img", 2, || panic!("scope maths failed"));
        let t0 = Instant::now();
        while w.busy("img") && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(w.result("img").map(|r| r.0), Some(1), "the last good result stays");
        w.submit(&ctx, "img", 3, || Some(Arc::new("third") as Done));
        assert!(wait(&w, "img", 3).is_some(), "the worker still runs");
    }
}
