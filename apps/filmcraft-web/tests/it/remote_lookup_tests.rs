//! Cache misses of remote files: one fetch per path, a definite answer ends the waiting.

use crate::remote_lookup::{ABSENT_RETRY_S, FAILED_RETRY_S, Lookups, Next, Outcome};

/// First miss fetches and the rest wait.
#[test]
fn first_miss_fetches_and_the_rest_wait() {
    let mut l = Lookups::default();
    assert_eq!(l.miss("/data/a.mp4", 0.0), Next::Fetch);
    assert_eq!(l.miss("/data/a.mp4", 0.1), Next::Wait);
    assert_eq!(l.miss("/data/b.mp4", 0.1), Next::Fetch);
}

/// Found ends the lookup.
#[test]
fn found_ends_the_lookup() {
    let mut l = Lookups::default();
    assert_eq!(l.miss("/data/a.mp4", 0.0), Next::Fetch);
    l.done("/data/a.mp4", Outcome::Found, 1.0);
    // the stat cache has it now; if it was evicted, a new lookup starts
    assert_eq!(l.miss("/data/a.mp4", 2.0), Next::Fetch);
}

/// A 404 becomes missing not loading.
#[test]
fn a_404_becomes_missing_not_loading() {
    let mut l = Lookups::default();
    assert_eq!(l.miss("/data/gone.mp4", 0.0), Next::Fetch);
    l.done("/data/gone.mp4", Outcome::Absent, 1.0);
    assert_eq!(l.miss("/data/gone.mp4", 2.0), Next::Missing);
    assert_eq!(l.miss("/data/gone.mp4", 1.0 + ABSENT_RETRY_S - 0.5), Next::Missing);
    // asked again after the hold
    assert_eq!(l.miss("/data/gone.mp4", 1.0 + ABSENT_RETRY_S + 0.5), Next::Fetch);
}

/// A failed lookup is missing for a short while.
#[test]
fn a_failed_lookup_is_missing_for_a_short_while() {
    let mut l = Lookups::default();
    assert_eq!(l.miss("/data/x.mp4", 0.0), Next::Fetch);
    l.done("/data/x.mp4", Outcome::Failed, 1.0);
    assert_eq!(l.miss("/data/x.mp4", 2.0), Next::Missing);
    assert_eq!(l.miss("/data/x.mp4", 1.0 + FAILED_RETRY_S + 0.5), Next::Fetch);
}

/// Forget clears a miss.
#[test]
fn forget_clears_a_miss() {
    let mut l = Lookups::default();
    assert_eq!(l.miss("/data/w.mp4", 0.0), Next::Fetch);
    l.done("/data/w.mp4", Outcome::Absent, 0.5);
    l.forget("/data/w.mp4");
    assert_eq!(l.miss("/data/w.mp4", 1.0), Next::Fetch);
}

/// Concurrent lookups are capped.
#[test]
fn concurrent_lookups_are_capped() {
    let mut l = Lookups::default();
    let mut fetches = 0;
    for i in 0..500 {
        if l.miss(&format!("/data/{i}.mp4"), 0.0) == Next::Fetch {
            fetches += 1;
        }
    }
    assert!(fetches > 0 && fetches < 500);
    // a finished lookup frees a slot
    l.done("/data/0.mp4", Outcome::Found, 1.0);
    assert_eq!(l.miss("/data/9999.mp4", 1.0), Next::Fetch);
}

/// The miss table stays bounded.
#[test]
fn the_miss_table_stays_bounded() {
    let mut l = Lookups::default();
    for i in 0..30_000 {
        let p = format!("/data/{i}.mp4");
        l.miss(&p, 0.0);
        l.done(&p, Outcome::Absent, 0.0);
    }
    assert_eq!(l.miss("/data/29999.mp4", 1.0), Next::Missing);
}
