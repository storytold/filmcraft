//! Cooperative cancellation of frame requests.
//!
//! A frame worker runs a job inside [`with_cancel`]; sources that do long work for a request (a
//! decoder seeking back to a keyframe and decoding forward) poll [`cancelled`] and give up with
//! [`MediaError::Cancelled`](crate::MediaError::Cancelled) once the job is no longer wanted, e.g.
//! playback has moved past its frame. The flag travels through a thread-local, so the
//! [`MediaSource`](crate::MediaSource) API is unchanged. Whether anything did give up is
//! recorded ([`gave_up`]): a job cancelled after its sources had already produced their frames
//! is complete, and caching it costs nothing while a late frame is still the best picture to
//! show.
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

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_time::Tick;

thread_local! {
    static CURRENT: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    /// Set once work inside the current [`with_cancel`] scope saw the flag ([`cancelled`]
    /// returned true), on this thread or on one a [`Context`] carried the scope to.
    static GAVE_UP: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
    static CATCH_UP: Cell<Option<Tick>> = const { Cell::new(None) };
    static PLAYING: Cell<bool> = const { Cell::new(false) };
    static DRAFT: Cell<bool> = const { Cell::new(false) };
    static BACKGROUND: Cell<bool> = const { Cell::new(false) };
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

/// Run `f` as playback work (restoring the previous setting after): the frame is one of a run
/// the playhead moves through, so a sequential decoder may decode ahead for the frames to come
/// and a frame it has passed is dropped rather than seeked back to. A scrub or a paused monitor
/// is not playback: its frame is decoded exactly.
pub fn with_playing<R>(on: bool, f: impl FnOnce() -> R) -> R {
    let prev = PLAYING.with(|c| c.replace(on));
    let r = f();
    PLAYING.with(|c| c.set(prev));
    r
}

/// Whether the work running on this thread is playback ([`with_playing`]).
pub fn playing() -> bool {
    PLAYING.with(Cell::get)
}

/// Run `f` with `flag` as this thread's cancellation flag (restoring the previous one after).
/// [`gave_up`] starts out false for the scope.
pub fn with_cancel<R>(flag: &Arc<AtomicBool>, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT.with(|c| c.replace(Some(flag.clone())));
    let prev_gave_up = GAVE_UP.with(|g| g.replace(Some(Arc::new(AtomicBool::new(false)))));
    let r = f();
    CURRENT.with(|c| *c.borrow_mut() = prev);
    GAVE_UP.with(|g| *g.borrow_mut() = prev_gave_up);
    r
}

/// Whether the work running on this thread has been cancelled. Work that asks is expected to
/// give up when it has: the answer true is recorded for [`gave_up`].
pub fn cancelled() -> bool {
    let c = CURRENT.with(|c| c.borrow().as_ref().is_some_and(|f| f.load(Ordering::Relaxed)));
    if c {
        GAVE_UP.with(|g| {
            if let Some(g) = g.borrow().as_ref() {
                g.store(true, Ordering::Relaxed);
            }
        });
    }
    c
}

/// Whether work in the current [`with_cancel`] scope saw that it was cancelled (and so may have
/// stopped short: a frame with layers missing, a decode given up). False when nothing asked:
/// the scope's result is complete even if its flag was set meanwhile.
pub fn gave_up() -> bool {
    GAVE_UP.with(|g| g.borrow().as_ref().is_some_and(|f| f.load(Ordering::Relaxed)))
}

/// Record that the work in the current [`with_cancel`] scope stops short on purpose, whether or
/// not it was cancelled (a decoder dropping a late frame rather than restarting): the scope's
/// result is incomplete and must not be cached as the frame.
pub fn give_up() {
    GAVE_UP.with(|g| {
        if let Some(g) = g.borrow().as_ref() {
            g.store(true, Ordering::Relaxed);
        }
    });
}

/// The hints of the work running on this thread ([`with_cancel`], [`with_catch_up`],
/// [`with_draft`], [`with_background`]), to carry them to another thread doing part of that work.
#[derive(Clone, Default)]
pub struct Context {
    cancel: Option<Arc<AtomicBool>>,
    gave_up: Option<Arc<AtomicBool>>,
    catch_up: Option<Tick>,
    playing: bool,
    draft: bool,
    background: bool,
}

impl Context {
    /// This thread's hints.
    pub fn current() -> Self {
        Context {
            cancel: CURRENT.with(|c| c.borrow().clone()),
            gave_up: GAVE_UP.with(|g| g.borrow().clone()),
            catch_up: catch_up(),
            playing: playing(),
            draft: draft(),
            background: background(),
        }
    }

    /// Run `f` with these hints, restoring this thread's however `f` ends (a pool thread must
    /// not keep another job's hints after a panic).
    pub fn run<R>(self, f: impl FnOnce() -> R) -> R {
        struct Restore(Context);
        impl Drop for Restore {
            fn drop(&mut self) {
                let prev = std::mem::take(&mut self.0);
                CURRENT.with(|c| *c.borrow_mut() = prev.cancel);
                GAVE_UP.with(|g| *g.borrow_mut() = prev.gave_up);
                CATCH_UP.with(|c| c.set(prev.catch_up));
                PLAYING.with(|c| c.set(prev.playing));
                DRAFT.with(|c| c.set(prev.draft));
                BACKGROUND.with(|c| c.set(prev.background));
            }
        }
        let _restore = Restore(Context::current());
        CURRENT.with(|c| *c.borrow_mut() = self.cancel);
        GAVE_UP.with(|g| *g.borrow_mut() = self.gave_up);
        CATCH_UP.with(|c| c.set(self.catch_up));
        PLAYING.with(|c| c.set(self.playing));
        DRAFT.with(|c| c.set(self.draft));
        BACKGROUND.with(|c| c.set(self.background));
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

    /// A job cancelled after its sources finished is complete; one whose source saw the flag
    /// is not, also when the source saw it on a thread the context was carried to.
    #[test]
    fn playing_is_off_by_default_and_scoped() {
        assert!(!playing());
        let (inner, carried) = with_playing(true, || {
            let ctx = Context::current();
            let carried = std::thread::scope(|s| s.spawn(|| ctx.run(playing)).join()).unwrap_or(false);
            (playing(), carried)
        });
        assert!(inner && carried, "the hint holds inside the scope and travels with the context");
        assert!(!playing());
    }

    #[test]
    fn giving_up_on_purpose_is_recorded_without_a_cancellation() {
        let flag = Arc::new(AtomicBool::new(false));
        give_up();
        assert!(!gave_up(), "outside a scope there is nothing to record it in");
        let (before, after) = with_cancel(&flag, || {
            let before = gave_up();
            give_up();
            (before, gave_up())
        });
        assert!(!before && after, "an explicit give-up is recorded although the flag is clear");
        assert!(!gave_up(), "and does not leak out of the scope");
    }

    #[test]
    fn giving_up_is_recorded_for_the_scope_and_across_threads() {
        let flag = Arc::new(AtomicBool::new(false));
        assert!(!gave_up(), "outside a scope nothing gave up");
        let (before, after, inner) = with_cancel(&flag, || {
            let before = gave_up();
            flag.store(true, Ordering::Relaxed);
            // the flag is set, but nothing has asked yet
            let after = gave_up();
            let ctx = Context::current();
            let inner = std::thread::scope(|s| s.spawn(|| ctx.run(|| (cancelled(), gave_up()))).join()).unwrap_or((false, false));
            (before, after, inner)
        });
        assert!(!before && !after, "a set flag alone is not giving up");
        assert_eq!(inner, (true, true), "a source on another thread saw the flag");
        assert!(!gave_up(), "the scope's record does not leak out");
        with_cancel(&flag, || assert!(!gave_up(), "a new scope starts clean, even with the flag set"));
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
