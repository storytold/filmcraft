//! Thread-local retry signal for asynchronous media reads and CPU frame preparation.
//! Callers clear at job entry, inspect at exit, and never cache incomplete results as terminal failures.

use std::cell::Cell;

thread_local! {
    static PENDING: Cell<bool> = const { Cell::new(false) };
}

/// Record deferred work on this thread; its owner requests a retry when data arrives.
pub fn mark() {
    PENDING.with(|p| p.set(true));
}

/// Whether work was deferred since the last take.
pub fn is_set() -> bool {
    PENDING.with(Cell::get)
}

/// Return and clear the retry signal.
pub fn take() -> bool {
    PENDING.with(|p| p.replace(false))
}

/// Mark deferred media access and return its I/O error.
pub fn would_block() -> std::io::Error {
    mark();
    std::io::Error::new(std::io::ErrorKind::WouldBlock, "media data is still loading")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_clears_the_flag() {
        assert!(!take());
        let error = would_block();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert!(is_set());
        assert!(take());
        assert!(!is_set());
    }
}
