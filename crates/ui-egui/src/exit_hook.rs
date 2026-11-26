//! The app's exit hook: one closure that must run on every controlled exit.
//!
//! macOS ends the process inside AppKit's terminate, after eframe's `on_exit`, so anything that has
//! to happen on a clean quit (finishing the Nightly session record) goes here rather than after
//! `run_native`, which that path never reaches. It is taken once, so a second exit does nothing.

use std::sync::Mutex;

type Hook = Box<dyn FnOnce() + Send>;

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

/// Install the hook; a later call replaces nothing (the desktop app installs it once).
pub fn set(hook: Hook) {
    *HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
}

/// Run the installed hook, if any. Only the first call runs it.
pub fn run_once() {
    let hook = HOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn the_exit_hook_runs_once() {
        let runs = Arc::new(AtomicU64::new(0));
        let counted = runs.clone();
        super::set(Box::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }));
        super::run_once();
        super::run_once();
        assert_eq!(runs.load(Ordering::Relaxed), 1, "a second exit must not run the hook again");
    }
}
