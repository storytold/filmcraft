//! A first-in first-out line for background media jobs (proxies, transcodes, ingest).
//!
//! Each of those jobs is an export that already uses every core and holds decoded frames, so
//! starting a second one while the first runs only makes both slower and doubles the memory in
//! use (#1831). A job takes a [`Ticket`] when it is started, in click order, and waits for its turn
//! on its own thread; a cancelled job gives its place up.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

struct State {
    /// Tickets handed out so far.
    issued: u64,
    /// The ticket whose job may run now.
    serving: u64,
    /// Tickets given up before their turn came.
    skipped: BTreeSet<u64>,
}

/// One line of jobs.
pub struct Queue {
    state: Mutex<State>,
    turn: Condvar,
}

/// How often a waiting job looks at its cancel flag.
const POLL: Duration = Duration::from_millis(100);

impl Queue {
    pub const fn new() -> Self {
        Self { state: Mutex::new(State { issued: 0, serving: 0, skipped: BTreeSet::new() }), turn: Condvar::new() }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A place at the back of the line. The ticket frees its place when dropped.
    pub fn ticket(&'static self) -> Ticket {
        let mut st = self.lock();
        let n = st.issued;
        st.issued += 1;
        Ticket { queue: self, n }
    }

    /// How many jobs a new ticket would stand behind (running or waiting).
    pub fn ahead(&self) -> u64 {
        let st = self.lock();
        st.issued - st.serving - st.skipped.len() as u64
    }

    fn finish(&self, n: u64) {
        let mut st = self.lock();
        if n == st.serving {
            st.serving += 1;
            loop {
                let next = st.serving;
                if !st.skipped.remove(&next) {
                    break;
                }
                st.serving += 1;
            }
        } else {
            st.skipped.insert(n);
        }
        drop(st);
        self.turn.notify_all();
    }
}

/// A place in a [`Queue`].
pub struct Ticket {
    queue: &'static Queue,
    n: u64,
}

impl Ticket {
    /// Wait until every earlier job is done. `false` when `cancel` was raised first; the place is
    /// then given up when the ticket is dropped.
    pub fn wait(&self, cancel: &AtomicBool) -> bool {
        let mut st = self.queue.lock();
        loop {
            if st.serving == self.n {
                return true;
            }
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            st = self.queue.turn.wait_timeout(st, POLL).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.queue.finish(self.n);
    }
}

/// The line every media job in this process stands in.
pub static MEDIA_JOBS: Queue = Queue::new();

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    fn leaked() -> &'static Queue {
        Box::leak(Box::new(Queue::new()))
    }

    #[test]
    fn jobs_run_one_at_a_time_in_the_order_they_were_started() {
        let q = leaked();
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));
        // every ticket is taken here, in order, before any job thread runs
        let tickets: Vec<Ticket> = (0..6).map(|_| q.ticket()).collect();
        assert_eq!(q.ahead(), 6);
        let handles: Vec<_> = tickets
            .into_iter()
            .enumerate()
            .rev() // start the threads in the wrong order on purpose
            .map(|(i, t)| {
                let (running, most, order) = (running.clone(), most.clone(), order.clone());
                std::thread::spawn(move || {
                    assert!(t.wait(&AtomicBool::new(false)));
                    most.fetch_max(running.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    order.lock().unwrap().push(i);
                    std::thread::sleep(Duration::from_millis(10));
                    running.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(most.load(Ordering::SeqCst), 1, "never two at once");
        assert_eq!(*order.lock().unwrap(), [0, 1, 2, 3, 4, 5]);
        assert_eq!(q.ahead(), 0);
    }

    #[test]
    fn a_cancelled_job_gives_its_place_up_without_stalling_the_line() {
        let q = leaked();
        let (first, second, third) = (q.ticket(), q.ticket(), q.ticket());
        let cancel = Arc::new(AtomicBool::new(false));
        let waiting = {
            let cancel = cancel.clone();
            std::thread::spawn(move || second.wait(&cancel))
        };
        // the second job is cancelled while the first still runs
        std::thread::sleep(Duration::from_millis(30));
        cancel.store(true, Ordering::Relaxed);
        assert!(!waiting.join().unwrap(), "a cancelled wait does not get a turn");
        assert_eq!(q.ahead(), 2, "its ticket was dropped with the thread, the first and third remain");
        drop(first);
        assert!(third.wait(&AtomicBool::new(false)), "the third job is next, not stuck behind the cancelled one");
        drop(third);
        assert_eq!(q.ahead(), 0);
    }

    #[test]
    fn a_ticket_dropped_out_of_order_is_skipped_when_its_turn_comes() {
        let q = leaked();
        let (a, b, c) = (q.ticket(), q.ticket(), q.ticket());
        drop(b);
        assert_eq!(q.ahead(), 2);
        drop(a);
        assert!(c.wait(&AtomicBool::new(false)));
    }
}
