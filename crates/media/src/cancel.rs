//! Cooperative cancellation of frame requests.
//!
//! A frame worker runs a job inside [`with_cancel`]; sources that do long work for a request (a
//! decoder seeking back to a keyframe and decoding forward) poll [`cancelled`] and give up with
//! [`MediaError::Cancelled`](crate::MediaError::Cancelled) once the job is no longer wanted, e.g.
//! playback has moved past its frame. The flag travels through a thread-local, so the
//! [`MediaSource`](crate::MediaSource) API is unchanged.
//!
//! The same way, [`with_catch_up`] tells sources which frames before the requested one are late
//! (playback has passed them): a decoder that has to decode forward to the frame may skip
//! pictures that only produce late frames and that nothing else references.
//!
//! [`with_draft`] marks reduced-resolution playback (opt-in): decoders may take spec-safe
//! shortcuts that only change pictures nothing references (H.264: no deblocking of non-reference
//! pictures). Never set for exports, renders or a frame shown while paused.
//!
//! [`with_background`] marks work nobody waits on interactively (thumbnails of the Project panel,
//! the Media Browser and the timeline): sources should not keep expensive state, such as a
//! frame-threaded 4K decoder, for it once it is done.
//!
//! [`with_max_threads`] caps how many worker threads the work on a thread may start (#1216): an
//! export sets it on its pool so its decoders and encoders do not use the whole machine. `None`
//! (the default) means no cap.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_time::Tick;

thread_local! {
    static CURRENT: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static CATCH_UP: Cell<Option<Tick>> = const { Cell::new(None) };
    static DRAFT: Cell<bool> = const { Cell::new(false) };
    static BACKGROUND: Cell<bool> = const { Cell::new(false) };
    static MAX_THREADS: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Run `f` as background work when `on` (restoring the previous hint after): a thumbnail, not a
/// frame a monitor or playback waits for.
pub fn with_background<R>(on: bool, f: impl FnOnce() -> R) -> R {
    /// Restores the previous hint however `f` ends (a panic caught further up must not leave the
    /// worker thread marked as background).
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            BACKGROUND.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(BACKGROUND.with(|c| c.replace(on)));
    f()
}

/// Whether the work running on this thread is background work (off unless inside
/// [`with_background`]`(true, …)`).
pub fn background() -> bool {
    BACKGROUND.with(Cell::get)
}

/// Run `f` with a cap of `max_threads` worker threads on this thread (restoring the previous one
/// after). Decoders and encoders made inside size their own pools to at most `max_threads`
/// ([`max_threads`]); `None` means no cap. Set by an export so its decoders do not use the whole
/// machine (#1216).
pub fn with_max_threads<R>(max_threads: Option<usize>, f: impl FnOnce() -> R) -> R {
    let prev = MAX_THREADS.with(|c| c.replace(max_threads));
    let r = f();
    MAX_THREADS.with(|c| c.set(prev));
    r
}

/// The most worker threads work on this thread may start (None: no cap). See [`with_max_threads`].
pub fn max_threads() -> Option<usize> {
    MAX_THREADS.with(Cell::get)
}

/// Set this thread's thread cap for good (a rayon pool worker: `ThreadPoolBuilder::start_handler`),
/// where [`with_max_threads`] is scoped to a closure. The pool lives for one export, so the cap
/// does too.
pub fn set_max_threads(max_threads: Option<usize>) {
    MAX_THREADS.with(|c| c.set(max_threads));
}

/// Run `f` with the draft-decoding hint `on` (restoring the previous one after). Frames decoded
/// in draft mode are only handed to requests made in draft mode.
pub fn with_draft<R>(on: bool, f: impl FnOnce() -> R) -> R {
    let prev = DRAFT.with(|c| c.replace(on));
    let r = f();
    DRAFT.with(|c| c.set(prev));
    r
}

/// Whether the work running on this thread accepts draft-quality decoding (off unless inside
/// [`with_draft`]`(true, …)`).
pub fn draft() -> bool {
    DRAFT.with(Cell::get)
}

/// Run `f` with the catch-up hint `margin` (restoring the previous one after): frames shown more
/// than `margin` before the requested time are late (`Some(Tick::ZERO)`: everything before it).
/// The margin is measured in media time at normal speed; a wrong guess (a sped-up or reversed
/// clip) only costs a later re-seek, never a wrong frame.
pub fn with_catch_up<R>(margin: Option<Tick>, f: impl FnOnce() -> R) -> R {
    let prev = CATCH_UP.with(|c| c.replace(margin));
    let r = f();
    CATCH_UP.with(|c| c.set(prev));
    r
}

/// The catch-up margin of the work running on this thread (None: no frame is late).
pub fn catch_up() -> Option<Tick> {
    CATCH_UP.with(Cell::get)
}

/// Run `f` with `flag` as this thread's cancellation flag (restoring the previous one after).
pub fn with_cancel<R>(flag: &Arc<AtomicBool>, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT.with(|c| c.replace(Some(flag.clone())));
    let r = f();
    CURRENT.with(|c| *c.borrow_mut() = prev);
    r
}

/// Whether the work running on this thread has been cancelled.
pub fn cancelled() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|f| f.load(Ordering::Relaxed)))
}

/// The hints of the work running on this thread ([`with_cancel`], [`with_catch_up`],
/// [`with_draft`], [`with_background`], [`with_max_threads`]), to carry them to another thread
/// doing part of that work.
#[derive(Clone, Default)]
pub struct Context {
    cancel: Option<Arc<AtomicBool>>,
    catch_up: Option<Tick>,
    draft: bool,
    background: bool,
    max_threads: Option<usize>,
}

impl Context {
    /// This thread's hints.
    pub fn current() -> Self {
        Context { cancel: CURRENT.with(|c| c.borrow().clone()), catch_up: catch_up(), draft: draft(), background: background(), max_threads: max_threads() }
    }

    /// Run `f` with these hints, restoring this thread's however `f` ends (a pool thread must
    /// not keep another job's hints after a panic).
    pub fn run<R>(self, f: impl FnOnce() -> R) -> R {
        struct Restore(Context);
        impl Drop for Restore {
            fn drop(&mut self) {
                let prev = std::mem::take(&mut self.0);
                CURRENT.with(|c| *c.borrow_mut() = prev.cancel);
                CATCH_UP.with(|c| c.set(prev.catch_up));
                DRAFT.with(|c| c.set(prev.draft));
                BACKGROUND.with(|c| c.set(prev.background));
                MAX_THREADS.with(|c| c.set(prev.max_threads));
            }
        }
        let _restore = Restore(Context::current());
        CURRENT.with(|c| *c.borrow_mut() = self.cancel);
        CATCH_UP.with(|c| c.set(self.catch_up));
        DRAFT.with(|c| c.set(self.draft));
        BACKGROUND.with(|c| c.set(self.background));
        MAX_THREADS.with(|c| c.set(self.max_threads));
        f()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_carries_the_hints_to_another_thread_and_restores_its_own() {
        let flag = Arc::new(AtomicBool::new(true));
        let ctx = with_cancel(&flag, || with_draft(true, || with_background(true, || with_catch_up(Some(Tick(5)), Context::current))));
        std::thread::scope(|s| {
            s.spawn(|| {
                let seen = with_draft(false, || ctx.clone().run(|| (cancelled(), draft(), background(), catch_up())));
                assert_eq!(seen, (true, true, true, Some(Tick(5))));
                // a panic inside leaves this thread's own hints in place
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ctx.clone().run(|| panic!("job"))));
                assert!(r.is_err());
                assert_eq!((cancelled(), draft(), background(), catch_up()), (false, false, false, None));
            });
        });
    }

    #[test]
    fn flag_is_scoped_to_the_closure() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!cancelled());
        with_cancel(&flag, || {
            assert!(!cancelled());
            flag.store(true, Ordering::Relaxed);
            assert!(cancelled());
        });
        assert!(!cancelled());
    }

    #[test]
    fn draft_is_off_by_default_and_scoped() {
        assert!(!draft());
        with_draft(true, || {
            assert!(draft());
            with_draft(false, || assert!(!draft()));
            assert!(draft());
        });
        assert!(!draft());
    }

    #[test]
    fn background_is_off_by_default_and_scoped() {
        assert!(!background());
        with_background(true, || {
            assert!(background());
            with_background(false, || assert!(!background()));
            assert!(background());
        });
        assert!(!background());
    }

    #[test]
    fn a_panic_inside_background_work_does_not_leave_the_thread_marked() {
        let r = std::panic::catch_unwind(|| with_background(true, || panic!("boom")));
        assert!(r.is_err());
        assert!(!background());
    }

    #[test]
    fn max_threads_is_scoped_and_carried_in_the_context() {
        assert_eq!(max_threads(), None, "no cap by default");
        with_max_threads(Some(3), || {
            assert_eq!(max_threads(), Some(3));
            let ctx = Context::current();
            std::thread::scope(|s| {
                s.spawn(|| {
                    assert_eq!(max_threads(), None, "another thread keeps its own hint");
                    ctx.clone().run(|| assert_eq!(max_threads(), Some(3)));
                    assert_eq!(max_threads(), None, "run restores this thread's own hint");
                    set_max_threads(Some(5));
                    assert_eq!(max_threads(), Some(5), "set_max_threads lasts past a closure");
                });
            });
            assert_eq!(max_threads(), Some(3));
        });
        assert_eq!(max_threads(), None);
    }

    #[test]
    fn catch_up_is_scoped_to_the_closure() {
        assert_eq!(catch_up(), None);
        with_catch_up(Some(Tick(5)), || {
            assert_eq!(catch_up(), Some(Tick(5)));
            with_catch_up(None, || assert_eq!(catch_up(), None));
            assert_eq!(catch_up(), Some(Tick(5)));
        });
        assert_eq!(catch_up(), None);
    }
}
