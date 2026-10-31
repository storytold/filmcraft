//! Its own test binary: the font database is process-global, so only a fresh process starts
//! with the system fonts not yet scanned (any `resolve` in the lib tests scans them).

use filmcraft_text::fonts::system_scanned;
use filmcraft_text::{families, picker_families};

/// The first list a picker takes must already hold the system families, not the bundled ones only.
#[test]
fn picker_families_scans_before_listing() {
    assert!(!system_scanned(), "fresh process: nothing scanned yet");
    let fams = picker_families();
    assert!(system_scanned(), "picker_families must scan the system fonts");
    assert_eq!(fams, families(), "the snapshot is taken after the scan");
}
