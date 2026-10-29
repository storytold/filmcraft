//! Read-ahead decisions of the chunk cache and the containers a remote project load warms up.

use crate::readahead::{BASE_AHEAD, Scan, ahead, warms_at_load};

/// Two misses at a frontier that moves forward make a scan sequential; a seek back or a jump ends it.
#[test]
fn a_forward_frontier_is_sequential() {
    let mut s = Scan::default();
    assert!(!s.sequential());
    s.note_miss(5, 12);
    assert!(!s.sequential(), "one miss is not a scan");
    // the retry of the same read, then the next chunk: the same scan
    s.note_miss(5, 12);
    assert!(s.sequential());
    s.note_miss(9, 12);
    assert!(s.sequential());
    // back to the start (a seek): over
    s.note_miss(0, 12);
    assert!(!s.sequential());
    s.note_miss(1, 12);
    assert!(s.sequential());
    // further than the window: a seek forward, over
    s.note_miss(40, 12);
    assert!(!s.sequential());
}

/// Read-ahead is the base until the scan is sequential, then up to `parallel`, capped by the cache.
#[test]
fn read_ahead_grows_with_a_sequential_scan() {
    let mut s = Scan::default();
    assert_eq!(ahead(&s, 12, 4 << 20, 384 << 20), BASE_AHEAD);
    s.note_miss(2, 12);
    s.note_miss(3, 12);
    assert_eq!(ahead(&s, 12, 4 << 20, 384 << 20), 12);
    // a quarter of a 64 MiB cache in 4 MiB chunks is 4
    assert_eq!(ahead(&s, 12, 4 << 20, 64 << 20), 4);
    // never below the base, and no division by a zero chunk size
    assert_eq!(ahead(&s, 1, 4 << 20, 384 << 20), BASE_AHEAD);
    assert_eq!(ahead(&s, 12, 0, 0), BASE_AHEAD);
    assert_eq!(ahead(&Scan::default(), 32, 1 << 20, usize::MAX), BASE_AHEAD);
}

/// Only WebM, Matroska and QuickTime media are warmed up at project load.
#[test]
fn only_slow_index_containers_are_warmed() {
    for p in ["/data/a.webm", "/data/B.MKV", "/data/c.mov"] {
        assert!(warms_at_load(p), "{p}");
    }
    for p in ["/data/a.mp4", "/data/a.wav", "/data/webm", "/data/a.webm.txt", ""] {
        assert!(!warms_at_load(p), "{p}");
    }
}
