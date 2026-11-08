//! Blackmagic RAW through Blackmagic's runtime (Linux, Windows, macOS). Skipped unless both are
//! available: `FILMCRAFT_BRAW_RUNTIME` names the folder holding `libBlackmagicRawAPI.so` /
//! `BlackmagicRawAPI.dll` / `BlackmagicRawAPI.framework` (or the runtime is installed where
//! FilmCraft looks for it), and `FILMCRAFT_BRAW_SAMPLES` names a folder of `.braw`
//! clips (the SDK's `sample.braw`, a camera's clips). Media are never committed.
#![cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]

use std::path::PathBuf;

use filmcraft_frame::PixelData;
use filmcraft_media::{FrameRequest, MediaKind};
use filmcraft_time::Tick;

fn samples() -> Vec<PathBuf> {
    let Some(dir) = std::env::var_os("FILMCRAFT_BRAW_SAMPLES") else { return Vec::new() };
    let mut clips: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("braw"))).collect())
        .unwrap_or_default();
    clips.sort();
    clips
}

#[test]
fn braw_clips_open_decode_and_play_their_audio() {
    let clips = samples();
    if clips.is_empty() {
        eprintln!("SKIPPED: set FILMCRAFT_BRAW_SAMPLES to a folder of .braw clips (and FILMCRAFT_BRAW_RUNTIME to Blackmagic's runtime)");
        return;
    }
    for path in clips {
        let name = path.display().to_string();
        let source = filmcraft_platform::braw::open_path(&name).expect("a .braw file is taken");
        let source = match source {
            Err(filmcraft_media::MediaError::Unsupported(e)) => {
                eprintln!("SKIPPED: no Blackmagic RAW runtime: {e}");
                return;
            }
            r => r.unwrap_or_else(|e| panic!("{name}: {e}")),
        };
        let info = source.info();
        let video = info.video.as_ref().expect("video");
        eprintln!(
            "{name}: {}x{} at {}/{} fps, {} ticks, timecode {:?}, {}, audio {:?}",
            video.width, video.height, video.frame_rate.num, video.frame_rate.den, info.duration.0, info.start_timecode, video.pixel_format, info.audio_streams
        );
        assert_eq!(info.kind, MediaKind::Movie);
        assert!(info.duration > Tick::ZERO);

        let full = source.video_frame(FrameRequest::full(Tick::ZERO)).unwrap_or_else(|e| panic!("{name}: frame 0: {e}"));
        eprintln!("  frame 0 full: {}x{}", full.width, full.height);
        let PixelData::Rgba8(rgba) = &full.data else { panic!("RGBA 8-bit frames") };
        assert_eq!(rgba.len(), full.width as usize * full.height as usize * 4);
        let lit = rgba.as_chunks::<4>().0.iter().filter(|&&[r, g, b, _]| r > 8 || g > 8 || b > 8).count();
        assert!(lit > rgba.len() / 4 / 10, "{name}: frame 0 is (nearly) black");

        let quarter = source.video_frame(FrameRequest { time: Tick::ZERO, scale: 0.25 }).unwrap_or_else(|e| panic!("{name}: quarter: {e}"));
        eprintln!("  frame 0 at 1/4: {}x{}", quarter.width, quarter.height);
        assert!(
            quarter.width < full.width && quarter.width * 4 >= full.width.saturating_sub(4),
            "{name}: a quarter-size request decodes about a quarter of the width"
        );

        let last = source.video_frame(FrameRequest::full(info.duration)).unwrap_or_else(|e| panic!("{name}: last frame: {e}"));
        assert_eq!((last.width, last.height), (full.width, full.height));

        if let Some(a) = info.audio() {
            let buffer = source.audio(0, 48_000, 48_000).unwrap_or_else(|e| panic!("{name}: audio: {e}"));
            assert_eq!(buffer.channels.len(), a.channels as usize);
            let peak = buffer.channels.iter().flatten().fold(0f32, |m, s| m.max(s.abs()));
            eprintln!("  audio: first second peaks at {peak}");
            assert!(peak <= 1.0);
        }
    }
}
