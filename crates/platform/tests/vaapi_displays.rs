//! VA-API decoders reuse displays (Linux). Opening a display takes 0.2-0.3 s with NVIDIA's driver
//! (about 100 MB of video memory each) and terminating one 60-70 ms, on the thread that creates or
//! drops the decoder (a frame worker at every clip, the UI thread when media is removed), so the
//! process keeps what it opens and every decoder makes only its own context and surfaces: one
//! display for decoders one after another, a few at most (Mesa only) for decoders at once. Its own
//! test binary: the count of opened displays is process-wide, and other tests running at the same
//! time would change it.
//! Skips without ffmpeg (fixture generator) or without a VA-API driver.
#![cfg(target_os = "linux")]

#[path = "it/common/mod.rs"]
mod common;

use common::*;
use filmcraft_codecs::VideoDecoder;
use filmcraft_platform::vaapi::va::{display_limit, displays_opened, probe};

fn hardware(s: &Stream) -> Option<Box<dyn VideoDecoder>> {
    match filmcraft_platform::vaapi_factory(&s.entry) {
        Some(Ok(d)) if d.name().starts_with("VA-API") => Some(d),
        _ => {
            eprintln!("SKIPPED: no VA-API decoder for this stream on this machine");
            None
        }
    }
}

#[test]
fn decoders_reuse_displays() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let paths: Vec<_> = FIXTURES.iter().filter_map(|(name, _)| named(&ff, name)).collect();
    let streams: Vec<_> = paths.iter().map(|p| read_stream(p)).collect();
    let Some(first) = streams.first() else { return };
    let Some(hw) = hardware(first) else { return };
    drop(hw);
    let opened = displays_opened();
    assert_eq!(opened, 1, "the first decoder opened one display");

    // one decoder after another, of every fixture (H.264, HEVC Main, HEVC Main 10)
    for round in 0..10 {
        for s in &streams {
            let Some(mut hw) = hardware(s) else { return };
            let n = s.samples.len().min(13);
            let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples[..n]);
            assert_same(&format!("round {round}"), &decode_all(hw.as_mut(), &s.samples[..n]), &reference);
        }
    }
    assert_eq!(displays_opened(), opened, "decoders one after another open no display");

    // several decoders at once on different threads, each used from its own thread, repeatedly
    for round in 0..3 {
        let jobs: Vec<_> = (0..6)
            .map(|i| {
                let path = paths[i % paths.len()].clone();
                std::thread::spawn(move || {
                    let s = read_stream(&path);
                    let Some(mut hw) = hardware(&s) else { return };
                    let reference = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples);
                    assert_same(&format!("round {round}, thread {i}"), &decode_all(hw.as_mut(), &s.samples), &reference);
                    hw.reset();
                    assert_same(&format!("round {round}, thread {i}, after a reset"), &decode_all(hw.as_mut(), &s.samples), &reference);
                })
            })
            .collect();
        for j in jobs {
            j.join().unwrap();
        }
    }
    // six decoders at once: at most the limit of this driver (one, or Mesa's four), and rounds after
    // the first reuse them
    let limit = display_limit(probe().as_ref().unwrap());
    assert!((opened..=limit).contains(&displays_opened()), "decoders at once opened {} displays, limit {limit}", displays_opened());
}
