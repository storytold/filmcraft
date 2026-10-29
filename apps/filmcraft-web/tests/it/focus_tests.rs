//! The focus throttle: which files fetch with priority while a project loads.

use crate::focus::{BACKGROUND_INFLIGHT, Class, FOCUS_PARALLEL, Focus};
use crate::readahead::{BASE_AHEAD, Scan};

fn files(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A scan that already looks sequential, so read-ahead would be wide.
fn sequential() -> Scan {
    let mut s = Scan::default();
    s.note_miss(1, 12);
    s.note_miss(1, 12);
    assert!(s.sequential());
    s
}

/// Nothing is throttled until a focus file is loading or has chunks in flight.
#[test]
fn no_focus_means_normal() {
    let f = Focus::default();
    assert_eq!(f.class("/a", false), Class::Normal);
    let mut f = Focus::default();
    f.set(&files(&["/a"]), &[]);
    assert_eq!(f.class("/a", false), Class::Focus);
    assert_eq!(f.class("/b", false), Class::Normal);
}

/// Other files are background while a focus file warms up or has chunks in flight, and normal again after.
#[test]
fn others_are_background_while_a_focus_file_loads() {
    let mut f = Focus::default();
    f.set(&files(&["/a", "/c"]), &files(&["/a", "/x"]));
    assert_eq!(f.class("/b", false), Class::Background);
    assert_eq!(f.class("/a", false), Class::Focus);
    f.finish("/a");
    assert_eq!(f.class("/b", false), Class::Normal);
    assert_eq!(f.class("/b", true), Class::Background, "a focus chunk in flight");
    // a "loading" file that is not in the focus is ignored
    f.finish("/x");
    assert_eq!(f.class("/b", false), Class::Normal);
}

/// A new focus replaces the old one.
#[test]
fn the_focus_follows_the_latest_set() {
    let mut f = Focus::default();
    f.set(&files(&["/a"]), &files(&["/a"]));
    f.set(&files(&["/b"]), &[]);
    assert!(!f.contains("/a") && f.contains("/b"));
    assert_eq!(f.class("/a", false), Class::Normal);
}

/// Background: base read-ahead only and a couple of requests; focus: parallelism capped; normal: unchanged.
#[test]
fn limits_per_class() {
    let s = sequential();
    let (chunk, budget) = (4 << 20, 384 << 20);
    assert_eq!(Class::Background.ahead(&s, 12, chunk, budget), BASE_AHEAD);
    assert_eq!(Class::Focus.ahead(&s, 12, chunk, budget), FOCUS_PARALLEL);
    assert_eq!(Class::Normal.ahead(&s, 12, chunk, budget), 12);
    assert_eq!(Class::Focus.parallel(12), FOCUS_PARALLEL);
    assert_eq!(Class::Focus.parallel(2), 2);
    assert_eq!(Class::Focus.parallel(0), 1);
    assert_eq!(Class::Normal.parallel(12), 12);
    assert_eq!(Class::Background.parallel(12), BACKGROUND_INFLIGHT as u64);
}

/// Only background requests are held back, and only beyond the allowed number in flight.
#[test]
fn background_requests_are_capped() {
    assert!(Class::Background.may_start(BACKGROUND_INFLIGHT - 1));
    assert!(!Class::Background.may_start(BACKGROUND_INFLIGHT));
    assert!(Class::Focus.may_start(100));
    assert!(Class::Normal.may_start(100));
}
