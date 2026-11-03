//! A bound on the nested-sequence work one frame (or audio window) may start. The depth limit
//! ([`crate::MAX_NEST_DEPTH`]) does not bound it: a sequence that places the next one on several
//! tracks (or a track-reading effect that reads its own track) fans out at every level.

use std::cell::Cell;

/// Most sequence renders and mixes one outermost call may start (a frame has tens at most).
pub(crate) const MAX_NESTED_VISITS: u32 = 1024;

thread_local! {
    static ACTIVE: Cell<u32> = const { Cell::new(0) };
    static VISITS: Cell<u32> = const { Cell::new(0) };
}

/// One sequence render or mix in progress on this thread. The outermost one starts a fresh count.
pub(crate) struct Visit {
    exhausted: bool,
}

impl Visit {
    pub(crate) fn enter() -> Visit {
        let outermost = ACTIVE.with(|a| {
            a.set(a.get() + 1);
            a.get() == 1
        });
        if outermost {
            VISITS.with(|v| v.set(0));
        }
        let n = VISITS.with(|v| {
            v.set(v.get().saturating_add(1));
            v.get()
        });
        Visit { exhausted: n > MAX_NESTED_VISITS }
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
}
