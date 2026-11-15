//! Regression and contrast tests for open-GOP leading pictures in H.264.
//!
//! When a seek starts a decoder run at a non-IDR I picture, the decoder emits that picture's
//! leading pictures without the previous GOP's references. Those broken leading pictures must not
//! be cached, or a later seek to one returns the wrong pixels. Closed-GOP (including B-frame
//! reordering) and IDR-only streams have no leading pictures before the run's start and keep the
//! exact same pixels and cache behaviour.

#[path = "it/common/mod.rs"]
mod common;

use std::path::Path;

use common::*;
use filmcraft_media::FrameRequest;

fn check_stream(ff: &Path, name: &str, x264_args: &[&str], extra_seeks: &[usize]) {
    let mut args = vec!["-f", "lavfi", "-i", "testsrc2=s=320x240:r=30:d=3,noise=alls=10:allf=t"];
    args.extend_from_slice(x264_args);
    let Some(file) = fixture(ff, name, &args) else {
        eprintln!("SKIPPED: ffmpeg without libx264 for {name}");
        return;
    };

    let (w, h) = (320usize, 240usize);
    let fsize = w * h + 2 * (w / 2) * (h / 2);
    let raw = ffmpeg_frames(ff, &file, "yuv420p");
    let n = raw.len() / fsize;
    assert_eq!(n, 90, "{name}: expected 90 frames, got {n}");
    let ref_planes = |j: usize| raw_planes(&raw[j * fsize..(j + 1) * fsize], w, h, w / 2, h / 2, 1);

    // Seek sequence on a shared source: reproduces leading-picture cache poisoning when
    // `store_output` caches pictures before the run's floor.
    let src = filmcraft_codecs::open_bytes(name, bytes(&file)).expect("open");
    let rate = src.info().frame_rate();
    let mut order = vec![0, n - 1, n / 2, 3, n / 2, n - 2, 1, n / 3, n - 1, 0];
    order.extend_from_slice(extra_seeks);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    order.extend((0..24).map(|_| rng.below(n as u64) as usize));

    for (step, &j) in order.iter().enumerate() {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(j as i64))).expect("frame");
        assert_eq!((f.width as usize, f.height as usize), (w, h), "{name}: step {step} frame {j}: size");
        let diff = max_diff(&planes(&f), &ref_planes(j));
        assert_eq!(diff, 0, "{name}: step {step}: seek to frame {j} differs from ffmpeg by {diff}");
    }

    // Sequential decode on a fresh source also matches ffmpeg bit for bit.
    let seq = filmcraft_codecs::open_bytes(name, bytes(&file)).expect("open sequential");
    for j in 0..n {
        let f = seq.video_frame(FrameRequest::full(rate.tick_of(j as i64))).expect("seq frame");
        let diff = max_diff(&planes(&f), &ref_planes(j));
        assert_eq!(diff, 0, "{name}: sequential frame {j} differs from ffmpeg by {diff}");
    }
}

#[test]
fn open_gop_seeks_do_not_cache_broken_leading_pictures() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    check_stream(
        &ff,
        "opengop_seek_90.mp4",
        &["-c:v", "libx264", "-preset", "veryfast", "-x264-params", "open-gop=1:bframes=2:keyint=20:min-keyint=20:scenecut=0", "-pix_fmt", "yuv420p"],
        &[39, 38, 19, 18, 59, 58, 79, 78],
    );
    check_stream(
        &ff,
        "opengop_keyint30_90.mp4",
        &["-c:v", "libx264", "-preset", "veryfast", "-x264-params", "open-gop=1:bframes=2:keyint=30:min-keyint=30:scenecut=0", "-pix_fmt", "yuv420p"],
        &[35, 29, 28, 65, 59, 58],
    );
}

#[test]
fn closed_gop_and_idr_only_seeks_remain_bit_exact() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    check_stream(&ff, "opengop_contrast_idr_only_90.mp4", &["-c:v", "libx264", "-preset", "veryfast", "-g", "1", "-bf", "0", "-pix_fmt", "yuv420p"], &[39, 38]);
    check_stream(
        &ff,
        "opengop_contrast_closed_bframes_90.mp4",
        &["-c:v", "libx264", "-preset", "veryfast", "-x264-params", "bframes=3:b-pyramid=normal:keyint=30:scenecut=0", "-pix_fmt", "yuv420p"],
        &[39, 38, 29, 28, 59, 58],
    );
}
