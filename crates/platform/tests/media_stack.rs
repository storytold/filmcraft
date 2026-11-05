//! VP9 and AV1 in WebM / Matroska through the media stack with Settings > Playback > Hardware
//! decoding Auto and Off (Windows). Its own test binary: the switch is process-wide.
#![cfg(target_os = "windows")]

#[path = "it/common/mod.rs"]
mod common;

use common::*;

/// WebM / Matroska VP9 and AV1 through the media stack: the same pictures with hardware decoding
/// Auto as with it Off, and the hardware decoder really ran.
#[test]
fn webm_and_mkv_match_software() {
    use filmcraft_media::FrameRequest;
    let ff = filmcraft_testkit::require_ffmpeg!();
    let src = "testsrc2=s=640x360:r=24:d=2,noise=alls=12:allf=t";
    let cases: [(&str, Vec<&str>); 2] = [
        (
            "vp9.webm",
            vec![
                "-f",
                "lavfi",
                "-i",
                src,
                "-c:v",
                "libvpx-vp9",
                "-b:v",
                "0",
                "-crf",
                "32",
                "-deadline",
                "good",
                "-cpu-used",
                "8",
                "-g",
                "24",
                "-auto-alt-ref",
                "1",
                "-lag-in-frames",
                "8",
                "-pix_fmt",
                "yuv420p",
            ],
        ),
        (
            "av1.mkv",
            vec![
                "-f",
                "lavfi",
                "-i",
                src,
                "-c:v",
                "libaom-av1",
                "-crf",
                "34",
                "-b:v",
                "0",
                "-cpu-used",
                "8",
                "-g",
                "24",
                "-lag-in-frames",
                "8",
                "-pix_fmt",
                "yuv420p",
            ],
        ),
    ];
    filmcraft_platform::register();
    for (name, args) in cases {
        let Some(path) = fixture(&ff, name, &args) else { continue };
        let bytes: std::sync::Arc<[u8]> = std::fs::read(&path).unwrap().into();
        let frames = |hw: bool| {
            filmcraft_codecs::hw::set_hardware_decoding(hw);
            let src = filmcraft_codecs::open_bytes(name, bytes.clone()).unwrap();
            let rate = src.info().frame_rate();
            (0..40).map(|i| src.video_frame(FrameRequest::full(rate.tick_of(i))).unwrap()).collect::<Vec<_>>()
        };
        let off = frames(false);
        let before = filmcraft_codecs::hw::hw_stats();
        let auto = frames(true);
        let after = filmcraft_codecs::hw::hw_stats();
        filmcraft_codecs::hw::set_hardware_decoding(true);
        if after.sessions == before.sessions {
            eprintln!("SKIPPED: no Media Foundation decoder for {name}");
            continue;
        }
        assert!(after.frames > before.frames, "{name}: hardware frames");
        for (i, (a, b)) in auto.iter().zip(&off).enumerate() {
            assert_eq!((a.width, a.height, a.color, a.par), (b.width, b.height, b.color, b.par), "{name} frame {i}");
            let (x, y) = (a.as_ref().clone(), b.as_ref().clone());
            assert_same(
                &format!("{name} frame {i}"),
                &[filmcraft_codecs::DecodedFrame { pts: 0, frame: x, draft: false }],
                &[filmcraft_codecs::DecodedFrame { pts: 0, frame: y, draft: false }],
            );
        }
    }
}
