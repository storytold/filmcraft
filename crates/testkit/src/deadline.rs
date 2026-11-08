//! Hang guard for hostile-input sweeps: a test body runs on a worker thread, and a body that does not finish
//! within the budget fails the test (naming the input it was last working on) instead of stalling CI forever.
//! A merely slow body is not a failure, so a loaded runner or slow VM cannot fail a correct decoder.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

static PROGRESS: Mutex<String> = Mutex::new(String::new());

/// Record the input being worked on, so a hang report can name it. Cheap enough to call per input.
pub fn set_progress(label: &str) {
    let mut p = PROGRESS.lock().unwrap_or_else(PoisonError::into_inner);
    p.clear();
    p.push_str(label);
}

/// Run `f` on a worker thread and wait at most `secs` seconds (`FILMCRAFT_HOSTILE_TIMEOUT_SECS` overrides the
/// budget). A panic in `f` is re-raised here; a timeout panics with the last [`set_progress`] label. A hung
/// worker is abandoned (the failing test process exits anyway).
pub fn with_deadline(secs: u64, f: impl FnOnce() + Send + 'static) {
    let secs = std::env::var("FILMCRAFT_HOSTILE_TIMEOUT_SECS").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&n| n > 0).unwrap_or(secs);
    run_with_deadline(secs, f);
}

fn run_with_deadline(secs: u64, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    // a failed spawn shows up as "worker gone" below
    let _ = std::thread::Builder::new().name("hostile deadline".into()).spawn(move || {
        let _ = tx.send(catch_unwind(AssertUnwindSafe(f)));
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(Ok(())) => {}
        Ok(Err(payload)) => resume_unwind(payload),
        Err(RecvTimeoutError::Timeout) => {
            let last = PROGRESS.lock().unwrap_or_else(PoisonError::into_inner).clone();
            panic!("hostile-input test did not finish in {secs}s (hang); last input: {last}");
        }
        Err(RecvTimeoutError::Disconnected) => panic!("hostile-input worker thread could not run"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slow_body_passes() {
        with_deadline(30, || std::thread::sleep(Duration::from_millis(100)));
    }

    #[test]
    #[should_panic(expected = "boom")]
    fn a_panic_in_the_body_is_re_raised() {
        with_deadline(30, || panic!("boom"));
    }

    #[test]
    #[should_panic(expected = "last input: seed 7 byte 3")]
    fn a_hang_fails_and_names_the_input() {
        // a body that outlives the 1 s budget; the inner fn, so FILMCRAFT_HOSTILE_TIMEOUT_SECS cannot lengthen it
        set_progress("seed 7 byte 3");
        run_with_deadline(1, || std::thread::sleep(Duration::from_secs(30)));
    }
}
