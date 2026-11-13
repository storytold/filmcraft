//! PNG-coded video in MP4/MOV (`png ` sample entries): every sample is a whole PNG image file, so
//! a graphics render with an alpha channel comes in as straight RGBA. ffmpeg writes the fixture
//! (its PNG encoder) and serves as the oracle for the pictures.

use crate::common;

use common::*;

/// A PNG track decodes to the same RGBA pixels as ffmpeg's decode of the same file, alpha included.
#[test]
fn png_samples_decode_to_the_same_rgba_as_ffmpeg() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    // a picture with a real alpha ramp: a red disc fading out over transparency
    let args = [
        "-f",
        "lavfi",
        "-i",
        "color=c=black@0.0:s=64x48:d=0.1,format=rgba,geq=r='255':g='0':b='0':a='X/W*255'",
        "-frames:v",
        "3",
        "-c:v",
        "png",
        "-pix_fmt",
        "rgba",
    ];
    let Some(f) = fixture(&ff, "png_rgba_alpha.mov", &args) else {
        eprintln!("SKIPPED: ffmpeg without the PNG encoder");
        return;
    };
    let want = ffmpeg_frames(&ff, &f, "rgba");
    let src = filmcraft_codecs::open_bytes("png_rgba_alpha.mov", bytes(&f)).unwrap();
    let info = src.info().video.clone().expect("a video stream");
    assert_eq!(info.codec, "PNG");
    assert!(info.has_alpha, "the PNG's alpha channel should be advertised");
    assert_eq!(info.pixel_format, "RGBA 8-bit");
    let n = want.len() / (64 * 48 * 4);
    let mut got = Vec::new();
    for i in 0..n {
        let t = filmcraft_time::FrameRate::FPS_25.tick_of(i as i64);
        let frame = src.video_frame(filmcraft_media::FrameRequest::full(t)).unwrap();
        match &frame.data {
            filmcraft_frame::PixelData::Rgba8(px) => got.extend_from_slice(px),
            other => panic!("expected RGBA, got {other:?}"),
        }
    }
    assert_eq!(got.len(), want.len(), "frame count differs");
    let worst = got.iter().zip(&want).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    assert!(worst <= 1, "pixels differ from ffmpeg's by {worst}");
    // the alpha ramp really came through
    assert!(got.chunks_exact(4).any(|p| p[3] < 40), "no transparent pixels decoded");
    assert!(got.chunks_exact(4).any(|p| p[3] > 215), "no opaque pixels decoded");
}

/// A QuickTime Animation (`rle `) track has no decoder (its bitstream has no public specification,
/// and we implement only from public specs): it must say so by name, not "unknown".
#[test]
fn a_codec_without_a_decoder_names_itself() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let args = ["-f", "lavfi", "-i", "color=c=black@0.0:s=32x24:d=0.1,format=argb", "-frames:v", "2", "-c:v", "qtrle", "-pix_fmt", "argb"];
    let Some(f) = fixture(&ff, "qtrle_argb.mov", &args) else {
        eprintln!("SKIPPED: ffmpeg without the qtrle encoder");
        return;
    };
    let src = filmcraft_codecs::open_bytes("qtrle_argb.mov", bytes(&f)).unwrap();
    assert_eq!(src.info().video.as_ref().expect("a video stream").codec, "QuickTime Animation");
    let err = src.video_frame(filmcraft_media::FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap_err();
    let filmcraft_media::MediaError::Unsupported(why) = err else { panic!("expected Unsupported, got {err:?}") };
    assert!(why.contains("QuickTime Animation"), "the codec should be named: {why}");
}
