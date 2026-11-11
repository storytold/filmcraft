//! A bound on the nested-sequence work one frame (or audio window) may start. The depth limit
//! ([`crate::MAX_NEST_DEPTH`]) does not bound it: a sequence that places the next one on several
//! tracks (or a track-reading effect that reads its own track) fans out at every level.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// Most sequence renders and mixes one outermost call may start (a frame has tens at most).
pub(crate) const MAX_NESTED_VISITS: u32 = 1024;

/// The visits counted so far by one outermost call, shared by the threads working for it.
pub(crate) type Count = Arc<AtomicU32>;

thread_local! {
    static ACTIVE: Cell<u32> = const { Cell::new(0) };
    static COUNT: RefCell<Option<Count>> = const { RefCell::new(None) };
}

/// One sequence render or mix in progress on this thread. The outermost one starts a fresh count.
pub(crate) struct Visit {
    exhausted: bool,
}

impl Visit {
    pub(crate) fn enter() -> Visit {
        let count = COUNT.with(|c| {
            let mut c = c.borrow_mut();
            if ACTIVE.with(Cell::get) == 0 {
                *c = Some(Count::default());
            }
            c.get_or_insert_with(Count::default).clone()
        });
        ACTIVE.with(|a| a.set(a.get() + 1));
        Visit { exhausted: count.fetch_add(1, Ordering::Relaxed).saturating_add(1) > MAX_NESTED_VISITS }
    }

    /// Whether this call is past the budget and must produce nothing.
    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted
    }
}

impl Drop for Visit {
    fn drop(&mut self) {
        ACTIVE.with(|a| a.set(a.get().saturating_sub(1)));
    }
}

/// The count of the call in progress on this thread, to hand to the tasks it fans out to
/// ([`Adopt::new`]); a worker thread has no visit of its own and would start a fresh count.
pub(crate) fn current() -> Option<Count> {
    COUNT.with(|c| c.borrow().clone())
}

/// Makes this thread's visits count against `count` for as long as it lives, then restores what
/// the thread had (a worker that steals another task keeps that task's count).
pub(crate) struct Adopt {
    prev: Option<Count>,
    prev_active: u32,
}

impl Adopt {
    pub(crate) fn new(count: &Option<Count>) -> Adopt {
        let Some(count) = count else { return Adopt { prev: None, prev_active: u32::MAX } };
        let prev = COUNT.with(|c| c.borrow_mut().replace(count.clone()));
        let prev_active = ACTIVE.with(|a| a.replace(1));
        Adopt { prev, prev_active }
    }
}

impl Drop for Adopt {
    fn drop(&mut self) {
        if self.prev_active != u32::MAX {
            COUNT.with(|c| *c.borrow_mut() = self.prev.take());
            ACTIVE.with(|a| a.set(self.prev_active));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_calls_share_one_count_and_the_next_outermost_call_starts_over() {
        {
            let outer = Visit::enter();
            assert!(!outer.exhausted());
            let inner: Vec<Visit> = (0..MAX_NESTED_VISITS - 1).map(|_| Visit::enter()).collect();
            assert!(inner.iter().all(|v| !v.exhausted()));
            assert!(Visit::enter().exhausted());
        }
        assert!(!Visit::enter().exhausted());
    }

    #[test]
    fn tasks_on_other_threads_share_the_callers_count() {
        let outer = Visit::enter();
        let count = current();
        let n = std::thread::scope(|sc| {
            (0..4)
                .map(|_| {
                    let count = count.clone();
                    sc.spawn(move || {
                        let _a = Adopt::new(&count);
                        (0..MAX_NESTED_VISITS).filter(|_| Visit::enter().exhausted()).count()
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().unwrap())
                .sum::<usize>()
        });
        // 4 x 1024 more visits against a budget of 1024, one already spent by `outer`
        assert_eq!(n, 4 * MAX_NESTED_VISITS as usize - (MAX_NESTED_VISITS as usize - 1));
        drop(outer);
    }
}
