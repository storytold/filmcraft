//! Uncompressed QuickTime video and 8-bit PCM audio against ffmpeg. These layouts lose nothing, so
//! our decode has to match ffmpeg's sample for sample: `2vuy` (8-bit 4:2:2), `v210` (10-bit 4:2:2,
//! rows padded to a multiple of 48 pixels) and `raw ` (24-bit RGB — the same fourcc as 8-bit PCM
//! audio, which the track handler tells apart).

mod common;

use common::*;
use filmcraft_media::FrameRequest;
use filmcraft_time::Tick;

/// Not a multiple of 48: a `v210` row holds 48-pixel-aligned rows, 336 pixels (896 bytes) here.
const W: usize = 322;
const H: usize = 180;
const CW: usize = W.div_ceil(2);

/// A 5-frame 322×180 testsrc2 clip in the container the tail asks for.
fn clip(ff: &std::path::Path, name: &str, tail: &[&str]) -> Option<std::path::PathBuf> {
    let mut args = vec!["-f", "lavfi", "-i", "testsrc2=size=322x180:rate=25", "-t", "0.2"];
    args.extend_from_slice(tail);
    fixture(ff, name, &args)
}

fn video_entry(path: &std::path::Path) -> filmcraft_isobmff::SampleEntry {
    let file = filmcraft_isobmff::open(bytes(path)).unwrap();
    let track = file.tracks.iter().find(|t| t.kind == filmcraft_isobmff::TrackKind::Video).expect("a video track");
    track.entries[0].clone()
}

#[test]
fn quicktime_2vuy_decodes_bit_exactly() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = clip(&ff, "unc_2vuy.mov", &["-pix_fmt", "uyvy422", "-c:v", "rawvideo", "-f", "mov"]) else { panic!("could not write the 2vuy fixture") };
    let entry = video_entry(&path);
    assert_eq!(entry.format.0, *b"2vuy");
    assert_eq!(entry.video.as_ref().unwrap().depth, 24);
    let src = filmcraft_codecs::open_bytes("unc_2vuy.mov", bytes(&path)).unwrap();
    let info = src.info().video.clone().unwrap();
    assert_eq!(info.codec, "Uncompressed 8-bit 4:2:2");
    assert_eq!(info.pixel_format, "YUV 4:2:2 8-bit");
    let frames = ffmpeg_frames(&ff, &path, "uyvy422");
    let fsize = (W + 2 * CW) * H;
    assert_eq!(frames.len(), 5 * fsize, "5 frames of 8-bit 4:2:2");
    // `uyvy422` is packed U Y0 V Y1: ffmpeg is the oracle for the samples, we split the planes
    let mut want: Vec<Vec<u16>> = vec![Vec::new(), Vec::new(), Vec::new()];
    for q in frames[..fsize].as_chunks::<4>().0 {
        want[0].push(u16::from(q[1]));
        want[0].push(u16::from(q[3]));
        want[1].push(u16::from(q[0]));
        want[2].push(u16::from(q[2]));
    }
    let f = src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
    assert_eq!(max_diff(&planes(&f), &want), 0);
}

#[test]
fn quicktime_v210_decodes_bit_exactly_with_padded_rows() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = clip(&ff, "unc_v210.mov", &["-pix_fmt", "yuv422p10le", "-c:v", "v210", "-f", "mov"]) else { panic!("could not write the v210 fixture") };
    // 322 pixels of picture, 336 pixels (56 groups of 16 bytes) per row
    let file = filmcraft_isobmff::open(bytes(&path)).unwrap();
    let track = file.tracks.iter().find(|t| t.kind == filmcraft_isobmff::TrackKind::Video).unwrap();
    assert_eq!(track.entries[0].format.0, *b"v210");
    assert_eq!(track.samples[0].size as usize, 896 * H, "v210 rows are padded to 48 pixels");
    let src = filmcraft_codecs::open_bytes("unc_v210.mov", bytes(&path)).unwrap();
    let info = src.info().video.clone().unwrap();
    assert_eq!(info.codec, "Uncompressed 10-bit 4:2:2");
    assert_eq!(info.pixel_format, "YUV 4:2:2 10-bit");
    let frames = ffmpeg_frames(&ff, &path, "yuv422p10le");
    let fsize = (W * H + 2 * CW * H) * 2;
    assert_eq!(frames.len(), 5 * fsize, "5 frames of 10-bit 4:2:2");
    let f = src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
    assert_eq!(max_diff(&planes(&f), &raw_planes(&frames[..fsize], W, H, CW, H, 2)), 0);
}

#[test]
fn quicktime_raw_rgb24_decodes_bit_exactly() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = clip(&ff, "unc_rgb24.mov", &["-pix_fmt", "rgb24", "-c:v", "rawvideo", "-f", "mov"]) else { panic!("could not write the rgb24 fixture") };
    let entry = video_entry(&path);
    assert_eq!(entry.format.0, *b"raw ");
    assert_eq!(entry.video.as_ref().unwrap().depth, 24);
    let src = filmcraft_codecs::open_bytes("unc_rgb24.mov", bytes(&path)).unwrap();
    let info = src.info().video.clone().unwrap();
    assert_eq!(info.codec, "Uncompressed RGB 24-bit");
    assert_eq!(info.pixel_format, "RGB 24-bit");
    let frames = ffmpeg_frames(&ff, &path, "rgb24");
    let fsize = W * H * 3;
    assert_eq!(frames.len(), 5 * fsize);
    let f = src.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
    let rgba = f.to_rgba8();
    for (i, px) in frames[..fsize].as_chunks::<3>().0.iter().enumerate() {
        assert_eq!(&rgba[i * 4..i * 4 + 4], &[px[0], px[1], px[2], 255], "pixel {i}");
    }
}

#[test]
fn quicktime_raw_u8_pcm_audio_follows_the_track_handler() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(path) = fixture(&ff, "unc_pcm_u8.mov", &["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5", "-c:a", "pcm_u8", "-f", "mov"])
    else {
        panic!("could not write the 8-bit PCM fixture")
    };
    // ffmpeg tags 8-bit PCM audio `raw `, the fourcc of 24-bit RGB video
    let file = filmcraft_isobmff::open(bytes(&path)).unwrap();
    let track = file.tracks.iter().find(|t| t.kind == filmcraft_isobmff::TrackKind::Audio).expect("an audio track");
    assert!(matches!(track.entries[0].codec, filmcraft_isobmff::CodecConfig::Pcm(_)), "the sound track's `raw ` is PCM: {:?}", track.entries[0].codec);
    let src = filmcraft_codecs::open_bytes("unc_pcm_u8.mov", bytes(&path)).unwrap();
    let info = src.info();
    assert!(info.video.is_none(), "{:?}", info.video);
    assert_eq!(info.audio().unwrap().codec, "PCM 8-bit");
    let got = src.audio(0, 4800, 48_000).unwrap();
    let want = ffmpeg_audio_f32(&ff, &path, &[]);
    let n = got.channels[0].len().min(want.len()).min(4800);
    assert!(n > 4000, "only {n} of 4800 samples decoded");
    for i in 0..n {
        // 8-bit samples quantise to 1/128
        assert!((got.channels[0][i] - want[i]).abs() <= 0.008, "sample {i}: {} vs {}", got.channels[0][i], want[i]);
    }
}
