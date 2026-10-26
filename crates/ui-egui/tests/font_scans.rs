//! The start-up font scans (`theme::run_font_scans`). Its own test binary, because a scan panics
//! on purpose and the crash hook's tests read the process-wide last panic.

use std::sync::atomic::{AtomicBool, Ordering};

use filmcraft_ui_egui::theme::run_font_scans;

#[test]
fn a_failed_font_scan_leaves_the_others() {
    // one catch_unwind around both scans: a failed Chinese scan skipped the right-to-left one
    static FOUND: AtomicBool = AtomicBool::new(false);
    fn fails() {
        panic!("a font file the parser chokes on");
    }
    fn finds() {
        FOUND.store(true, Ordering::SeqCst);
    }
    assert_eq!(run_font_scans(&[("first", fails), ("second", finds)]), 1);
    assert!(FOUND.load(Ordering::SeqCst), "the second scan ran");
}
