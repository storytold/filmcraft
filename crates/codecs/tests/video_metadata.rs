//! Probe metadata from generated streams. ffmpeg is a fixture generator and oracle only.

mod common;

use common::*;

fn check_video(name: &str, encoder: &str, pixfmt: &str, want: &str) {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let encoders = std::process::Command::new(&ff).args(["-hide_banner", "-encoders"]).output().expect("list encoders");
    assert!(encoders.status.success());
    let encoders = String::from_utf8_lossy(&encoders.stdout);
    let (encoder, option, value) = match encoder {
        "av1" if encoders.contains("libaom-av1") => ("libaom-av1", "-cpu-used", "8"),
        "av1" => ("libsvtav1", "-preset", "12"),
        other => (other, "-crf", "23"),
    };
    let f = fixture(&ff, name, &["-f", "lavfi", "-i", "testsrc2=size=64x64:rate=25", "-frames:v", "2", "-c:v", encoder, "-pix_fmt", pixfmt, option, value])
        .expect("generate metadata fixture");
    let source = filmcraft_codecs::open_bytes(name, bytes(&f)).expect("open fixture");
    let video = source.info().video.as_ref().expect("video metadata");
    assert_eq!(video.pixel_format, want, "{name}");
    if encoder == "libx264" {
        assert_eq!(video.codec, "H.264 (High)");
    }
    // Metadata probing must leave decoding usable.
    let frame = source.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::Tick::ZERO)).expect("decode first frame");
    assert_eq!((frame.width, frame.height), (64, 64));
}

#[test]
fn av1_mp4_reports_8_bit_pixel_format() {
    check_video("metadata_av1_8.mp4", "av1", "yuv420p", "YUV 4:2:0 8-bit");
}

#[test]
fn av1_mp4_reports_10_bit_pixel_format() {
    check_video("metadata_av1_10.mp4", "av1", "yuv420p10le", "YUV 4:2:0 10-bit");
}

#[test]
fn avc_matroska_reports_pixel_format_and_profile() {
    check_video("metadata_avc.mkv", "libx264", "yuv420p", "YUV 4:2:0 8-bit");
}

#[test]
fn hevc_matroska_reports_10_bit_pixel_format() {
    check_video("metadata_hevc_10.mkv", "libx265", "yuv420p10le", "YUV 4:2:0 10-bit");
}
