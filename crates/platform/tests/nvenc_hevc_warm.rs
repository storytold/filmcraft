//! `register` asks the NVENC HEVC probe on a thread of its own, so the first draw of Export's format
//! list does not open an encoder session on the UI thread. Its own test binary: anything else asking
//! `hevc_available` first would make the wait meaningless.
#![cfg(target_os = "windows")]

use std::time::{Duration, Instant};

use filmcraft_platform::nvenc::{hevc_available, hevc_hdr_available, hevc_hdr_probed, hevc_probed};

#[test]
fn register_answers_the_hevc_question_in_the_background() {
    assert!(!hevc_probed() && !hevc_hdr_probed(), "nothing has asked yet");
    filmcraft_platform::register();
    let t0 = Instant::now();
    while !hevc_probed() {
        assert!(t0.elapsed() < Duration::from_secs(60), "register() did not start the probe");
        std::thread::sleep(Duration::from_millis(10));
    }
    // the Main 10 probe runs on the same thread, right after
    while !hevc_hdr_probed() {
        assert!(t0.elapsed() < Duration::from_secs(60), "register() did not start the Main 10 probe");
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("answered in the background after {:?}", t0.elapsed());
    let hdr = hevc_hdr_available();
    assert!(!hdr || hevc_available(), "Main 10 implies HEVC");
    assert_eq!(filmcraft_export::hdr_available(filmcraft_export::Format::Hevc), hdr);
    // the answer is kept: asking and registering again change nothing
    let answer = hevc_available();
    filmcraft_platform::register();
    assert_eq!(hevc_available(), answer);
}
