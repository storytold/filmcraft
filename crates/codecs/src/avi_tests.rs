//! AVI files made by ffmpeg (skipped without it), checked against ffmpeg's decode of the same file:
//! bit-exact where the decoder is (H.264, uncompressed video, PCM), within decoder tolerance where
//! it differs (JPEG, MP3 / MP2 / AC-3).

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use filmcraft_media::FrameRequest;

use crate::tests::{ffmpeg, fixture_path, reference_yuv, yuv_bytes};

const PIC: [&str; 4] = ["-f", "lavfi", "-i", "testsrc2=s=160x120:r=25:d=2"];
const TONE: [&str; 4] = ["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:d=2"];

fn avi(name: &str, video: &[&str], audio: &[&str]) -> Option<(std::path::PathBuf, filmcraft_media::SharedSource)> {
    let mut args: Vec<&str> = PIC.to_vec();
    args.extend_from_slice(&TONE);
    args.extend_from_slice(video);
    args.extend_from_slice(audio);
    let path = fixture_path(name, &args)?;
    let bytes: Arc<[u8]> = std::fs::read(&path).ok()?.into();
    Some((path.clone(), crate::open_bytes(name, bytes).unwrap_or_else(|e| panic!("{name}: {e}"))))
}

/// ffmpeg's decode of the first audio stream as interleaved f32 at its own rate.
fn reference_audio(path: &Path) -> Vec<f32> {
    let out = Command::new(ffmpeg().unwrap())
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .unwrap();
    out.stdout.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

/// The first `n` mono samples of stream 0 as FilmCraft decodes them.
fn ours(src: &filmcraft_media::SharedSource, n: usize) -> Vec<f32> {
    src.audio(0, n, 48_000).unwrap().channels[0].clone()
}

fn check_info(name: &str, src: &filmcraft_media::SharedSource, vcodec: &str, acodec: &str) {
    let info = src.info();
    assert_eq!(info.container, "AVI", "{name}");
    let v = info.video.as_ref().unwrap();
    assert_eq!((v.width, v.height, v.codec.as_str()), (160, 120, vcodec), "{name}");
    assert_eq!(v.frame_rate.as_f64(), 25.0, "{name}");
    let a = &info.audio_streams[0];
    assert_eq!((a.sample_rate, a.channels, a.codec.as_str()), (48_000, 1, acodec), "{name}");
    let secs = info.duration.seconds();
    assert!((secs - 2.0).abs() < 0.1, "{name}: duration {secs}");
}

#[test]
fn h264_frames_are_bit_exact_with_random_access_and_mp3_plays() {
    let Some((path, src)) =
        avi("avi_h264_mp3.avi", &["-c:v", "libx264", "-bf", "2", "-g", "12", "-pix_fmt", "yuv420p"], &["-c:a", "libmp3lame", "-b:a", "128k"])
    else {
        return;
    };
    check_info("h264", &src, "H.264", "MP3");
    let reference = reference_yuv(&path, "yuv420p").unwrap();
    let frame = reference.len() / 50;
    let rate = src.info().frame_rate();
    for k in [37, 3, 24, 49, 0, 13, 12, 11, 30, 31, 32] {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(k as i64))).unwrap();
        assert!(yuv_bytes(&f) == reference[k * frame..(k + 1) * frame], "H.264 frame {k} differs from ffmpeg");
    }
    // MP3 (a different decoder): the tone, in time with ffmpeg's decode
    let r = reference_audio(&path);
    let a = ours(&src, 48_000);
    let (rms_ref, rms_diff) = rms_and_diff(&r, &a, 4_800..43_200);
    // ffmpeg's test tone is at 1/8 of full scale
    assert!(rms_ref > 0.05 && rms_diff < rms_ref * 0.05, "MP3: tone {rms_ref}, difference {rms_diff}");
    // random access into the middle matches sequential decoding
    let mid = src.audio(24_000, 4_800, 48_000).unwrap().channels[0].clone();
    let worst = mid.iter().zip(&a[24_000..28_800]).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
    assert!(worst < 0.01, "MP3 seek: {worst}");
}

fn rms_and_diff(reference: &[f32], ours: &[f32], range: std::ops::Range<usize>) -> (f32, f32) {
    let n = range.len() as f32;
    let rms = (range.clone().map(|i| reference[i].powi(2)).sum::<f32>() / n).sqrt();
    let diff = (range.map(|i| (reference[i] - ours[i]).powi(2)).sum::<f32>() / n).sqrt();
    (rms, diff)
}

#[test]
fn mjpeg_and_pcm() {
    let Some((path, src)) = avi("avi_mjpeg_pcm.avi", &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj420p"], &["-c:a", "pcm_s16le"]) else { return };
    check_info("mjpeg", &src, "Motion JPEG", "PCM 16-bit");
    let reference = reference_yuv(&path, "rgba").unwrap();
    let frame = reference.len() / 50;
    let rate = src.info().frame_rate();
    for k in [10, 0, 49] {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(k as i64))).unwrap().to_rgba8();
        let r = &reference[k * frame..(k + 1) * frame];
        let mean = f.iter().zip(r).map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs()).sum::<u32>() as f64 / f.len() as f64;
        // another JPEG decoder and chroma upsampling than ffmpeg's: close, not identical
        assert!(mean < 6.0, "JPEG frame {k}: mean difference {mean}");
    }
    // PCM: exactly ffmpeg's samples
    let r = reference_audio(&path);
    assert_eq!(ours(&src, 48_000), r[..48_000].to_vec());
}

#[test]
fn uncompressed_video_is_bit_exact() {
    for (name, pix, codec, reference_fmt) in [
        ("avi_bgr24.avi", "bgr24", "Uncompressed RGB 24-bit", "rgba"),
        ("avi_yuyv.avi", "yuyv422", "Uncompressed YUV 4:2:2", "yuv422p"),
        ("avi_i420.avi", "yuv420p", "Uncompressed YUV 4:2:0", "yuv420p"),
        ("avi_nv12.avi", "nv12", "Uncompressed YUV 4:2:0", "yuv420p"),
    ] {
        let Some((path, src)) = avi(name, &["-c:v", "rawvideo", "-pix_fmt", pix], &["-c:a", "pcm_s16le"]) else { return };
        check_info(name, &src, codec, "PCM 16-bit");
        let reference = reference_yuv(&path, reference_fmt).unwrap();
        let frame = reference.len() / 50;
        let rate = src.info().frame_rate();
        for k in [7, 0, 49] {
            let f = src.video_frame(FrameRequest::full(rate.tick_of(k as i64))).unwrap();
            let got = if reference_fmt == "rgba" { f.to_rgba8() } else { yuv_bytes(&f) };
            assert!(got == reference[k * frame..(k + 1) * frame], "{name}: frame {k} differs from ffmpeg");
        }
    }
}

#[test]
fn mp2_and_ac3_audio() {
    for (name, codec, label) in [("avi_mp2.avi", "mp2", "MPEG Audio"), ("avi_ac3.avi", "ac3", "AC-3")] {
        let Some((path, src)) = avi(name, &["-c:v", "mjpeg"], &["-c:a", codec, "-b:a", "192k"]) else { return };
        assert_eq!(src.info().audio_streams[0].codec, label, "{name}");
        let r = reference_audio(&path);
        let a = ours(&src, 48_000);
        let (rms_ref, rms_diff) = rms_and_diff(&r, &a, 4_800..43_200);
        assert!(rms_ref > 0.05 && rms_diff < rms_ref * 0.05, "{name}: tone {rms_ref}, difference {rms_diff}");
    }
}

/// Dropped frames are stored as empty chunks: their slot shows the picture before.
#[test]
fn drop_frames_hold_the_previous_picture() {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=160x120:r=25:d=2",
        "-vf",
        "select='not(eq(n\\,10))'",
        "-fps_mode",
        "passthrough",
        "-c:v",
        "rawvideo",
        "-pix_fmt",
        "bgr24",
    ];
    let Some(path) = fixture_path("avi_drop.avi", &args) else { return };
    let src = crate::open_bytes("avi_drop.avi", std::fs::read(&path).unwrap().into()).unwrap();
    let rate = src.info().frame_rate();
    let at = |k: i64| src.video_frame(FrameRequest::full(rate.tick_of(k))).unwrap().to_rgba8();
    assert!(at(10) == at(9), "the dropped slot holds frame 9");
    assert!(at(11) != at(9));
}

/// DivX / Xvid has no decoder yet: the file opens, the codec is named, the picture reports it, and
/// the sound plays.
#[test]
fn mpeg4_part2_is_named_and_its_audio_plays() {
    let Some((_, src)) = avi("avi_xvid.avi", &["-c:v", "mpeg4", "-vtag", "XVID"], &["-c:a", "pcm_s16le"]) else { return };
    assert_eq!(src.info().video.as_ref().unwrap().codec, "MPEG-4 Part 2 (DivX / Xvid)");
    let e = src.video_frame(FrameRequest::full(filmcraft_time::Tick::ZERO)).unwrap_err().to_string();
    assert!(e.contains("MPEG-4 Part 2"), "{e}");
    assert!(ours(&src, 4_800).iter().any(|s| s.abs() > 0.1));
}

/// A Motion JPEG frame without Huffman tables (the AVI1 convention) decodes with the standard ones:
/// exactly as the same frame with them.
#[test]
fn jpeg_without_huffman_tables_uses_the_standard_ones() {
    let Some(path) = fixture_path(
        "default_huffman.jpg",
        &["-f", "lavfi", "-i", "testsrc2=s=160x120", "-frames:v", "1", "-c:v", "mjpeg", "-huffman", "default", "-q:v", "3"],
    ) else {
        return;
    };
    let jpeg = std::fs::read(&path).unwrap();
    // remove every DHT segment
    let mut stripped = jpeg[..2].to_vec();
    let mut at = 2;
    while at + 4 <= jpeg.len() && jpeg[at] == 0xFF && jpeg[at + 1] != 0xDA {
        let len = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        if jpeg[at + 1] != 0xC4 {
            stripped.extend_from_slice(&jpeg[at..at + 2 + len]);
        }
        at += 2 + len;
    }
    stripped.extend_from_slice(&jpeg[at..]);
    assert!(stripped.len() < jpeg.len() && !stripped.windows(2).any(|w| w == [0xFF, 0xC4]));
    use crate::video::VideoDecoder;
    let mut d = crate::video::MjpegDecoder;
    let a = d.decode(&jpeg, 0).unwrap().remove(0).frame.to_rgba8();
    let b = d.decode(&stripped, 0).unwrap().remove(0).frame.to_rgba8();
    assert!(a == b);
    // the inserted segment is 418 bytes and well-formed: 4 tables of 12, 12, 162, 162 codes
    let with = crate::video::with_default_huffman(&stripped);
    assert_eq!(with.len(), stripped.len() + 420);
}

/// H.264 with B-frames copied from an MP4 whose time base is 1/600 (ffmpeg keeps it): 600 frame
/// slots a second, a picture in every 24th at 25 fps, empty drop chunks between. The pictures stay
/// at their times in presentation order, and the stream reports the rate they play at.
#[test]
fn copied_h264_keeps_its_timing_with_a_fine_time_base() {
    let mp4 =
        ["-f", "lavfi", "-i", "testsrc2=s=160x120:r=25:d=2", "-c:v", "libx264", "-bf", "2", "-g", "12", "-pix_fmt", "yuv420p", "-video_track_timescale", "600"];
    let Some(src_mp4) = fixture_path("avi_copy_source.mp4", &mp4) else { return };
    let src_path = src_mp4.to_string_lossy().into_owned();
    let Some(path) = fixture_path("avi_copy_600.avi", &["-i", &src_path, "-c:v", "copy", "-bsf:v", "h264_mp4toannexb", "-f", "avi"]) else { return };
    let src = crate::open_bytes("avi_copy_600.avi", std::fs::read(&path).unwrap().into()).unwrap();
    let v = src.info().video.clone().unwrap();
    assert_eq!(v.frame_rate.as_f64(), 25.0, "the rate the pictures play at, not the 600 slots");
    let reference = reference_yuv(&path, "yuv420p").unwrap();
    let frame = reference.len() / 50;
    for k in [37usize, 3, 24, 49, 0, 13, 12, 11, 30] {
        let at = filmcraft_time::Tick::from_rational(k as i64, 1, 25);
        let f = src.video_frame(FrameRequest::full(at)).unwrap();
        assert!(yuv_bytes(&f) == reference[k * frame..(k + 1) * frame], "picture {k} at {:.2} s differs from ffmpeg", k as f64 / 25.0);
    }
    // between pictures, the one before holds
    let mid = src.video_frame(FrameRequest::full(filmcraft_time::Tick::from_rational(1, 1, 50))).unwrap();
    assert!(yuv_bytes(&mid) == reference[0..frame]);
}
