//! Settings ▸ Playback ▸ Hardware decoding through the decoder registry and the media stack:
//! Off gives the software decoder (and no hardware frames), Auto the hardware one where the
//! system has it. Its own test binary: the switch is process-wide.

mod common;

use common::*;
use filmcraft_media::FrameRequest;

#[test]
fn off_uses_the_software_decoder() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = named(&ff, "hevc_main.mp4") else { return };
    let s = read_stream(&path);
    let available = filmcraft_platform::register();
    assert_eq!(filmcraft_platform::register(), available, "registering twice is harmless");

    filmcraft_codecs::hw::set_hardware_decoding(false);
    let d = filmcraft_codecs::make_video_decoder(&s.entry).unwrap();
    assert_eq!(d.name(), "FilmCraft HEVC");
    let bytes: std::sync::Arc<[u8]> = std::fs::read(&path).unwrap().into();
    let (g0, h0) = (filmcraft_codecs::gop_stats(), filmcraft_codecs::hw::hw_stats());
    let src = filmcraft_codecs::open_bytes("hevc_main.mp4", bytes.clone()).unwrap();
    let rate = src.info().frame_rate();
    for i in 0..30 {
        src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap();
    }
    assert!(filmcraft_codecs::gop_stats().frames > g0.frames, "frames counted");
    assert_eq!(filmcraft_codecs::hw::hw_stats().frames, h0.frames, "Off: no hardware frames");

    filmcraft_codecs::hw::set_hardware_decoding(true);
    let d = filmcraft_codecs::make_video_decoder(&s.entry).unwrap();
    let hw = filmcraft_platform::hardware_decoder_for(&s.entry);
    if !hw {
        eprintln!("SKIPPED (Auto half): no hardware decoder ({available:?})");
        assert_eq!(d.name(), "FilmCraft HEVC");
        return;
    }
    let backend = if cfg!(target_os = "linux") {
        assert!(d.name().starts_with("VA-API HEVC ("), "Linux Auto decoder: {}", d.name());
        "VA-API"
    } else {
        let (backend, hw_name) = if cfg!(target_os = "macos") { ("VideoToolbox", "VideoToolbox HEVC") } else { ("Media Foundation", "Media Foundation HEVC") };
        assert_eq!(d.name(), hw_name);
        backend
    };
    assert_eq!(filmcraft_codecs::hw::hw_backend(), Some(backend), "perf.stats reports the backend");
    assert!(filmcraft_platform::registered(), "register() put the factory in the registry");
    let h1 = filmcraft_codecs::hw::hw_stats();
    let src = filmcraft_codecs::open_bytes("hevc_main.mp4", bytes).unwrap();
    for i in 0..30 {
        src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap();
    }
    let h2 = filmcraft_codecs::hw::hw_stats();
    assert!(h2.frames > h1.frames && h2.sessions > h1.sessions, "Auto: hardware frames {h1:?} → {h2:?}");
}
