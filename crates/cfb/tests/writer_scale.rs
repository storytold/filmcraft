//! The writer scales: adding many children to one storage, and the duplicate-name rules that check keeps.

use filmcraft_cfb::{Version, Writer};

/// Adding a child compared its name with every sibling (allocating twice per comparison), so n children
/// cost O(n^2): 20 000 AAF markers in one storage took about a minute. The duplicate check is a hash
/// lookup now. 40 000 plain names take milliseconds now and about 18 s before (a minute for real marker
/// objects), so the limit separates them by orders of magnitude on any machine.
#[test]
fn adding_many_children_is_not_quadratic() {
    let mut w = Writer::new(Version::V3);
    let start = std::time::Instant::now();
    for i in 0..40_000 {
        w.stream(Writer::ROOT, &format!("{i}"), vec![1]).expect("unique name");
    }
    assert!(start.elapsed() < std::time::Duration::from_secs(3), "{:?}", start.elapsed());
}

/// The faster duplicate check still means what sibling comparison means: case-insensitive, also beyond ASCII,
/// per storage (the same name in another storage is fine), and a name that differs only in length is distinct.
#[test]
fn duplicate_names_are_still_rejected() {
    let mut w = Writer::new(Version::V3);
    let a = w.storage(Writer::ROOT, "Alpha", [0; 16]).unwrap();
    assert!(w.stream(Writer::ROOT, "ALPHA", vec![]).is_err(), "case-insensitive");
    assert!(w.stream(Writer::ROOT, "alpha", vec![]).is_err());
    assert!(w.stream(Writer::ROOT, "Alph", vec![]).is_ok());
    assert!(w.stream(Writer::ROOT, "Alpha2", vec![]).is_ok());
    assert!(w.stream(a, "Alpha", vec![]).is_ok(), "same name in another storage");
    assert!(w.stream(a, "Alpha", vec![]).is_err());
    w.stream(Writer::ROOT, "Ünï", vec![]).unwrap();
    assert!(w.stream(Writer::ROOT, "ÜNÏ", vec![]).is_err(), "non-ASCII case");
}
