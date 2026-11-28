//! A background-thread start hook that panics must not take the process down. The hook runs from
//! rayon's start handler, and the background pools set no panic handler, so a panic there aborted
//! the process. Its own test binary: the hook it registers is process-wide.

fn panics() {
    panic!("a start hook that fails");
}

#[test]
fn a_panicking_start_hook_does_not_stop_a_background_job() {
    filmcraft_export::register_background_thread_start(panics);
    // the shared pool, and a capped one of its own: both are made here, so both run the hook
    assert_eq!(filmcraft_export::on_own_threads(0, || 6 * 7), 42);
    assert_eq!(filmcraft_export::on_own_threads(1, || 6 * 7), 42);
}
