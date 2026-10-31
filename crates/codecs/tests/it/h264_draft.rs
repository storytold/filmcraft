//! H.264 draft decoding (opt-in, reduced-resolution playback) through the media stack: it is off
//! by default, its approximate frames only ever reach draft requests, and decoding for an export
//! after draft playback (same source, same GOP cache) still matches ffmpeg bit for bit.

use crate::common;

use common::*;
use filmcraft_media::FrameRequest;
use filmcraft_media::cancel::with_draft;

/// Verify H.264 default, draft and export decoding against ffmpeg.
/// Called by the shared draft-codec test because GOP statistics are process-wide.
pub(super) fn check() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    // B-pyramid: references and non-reference B pictures (the ones draft mode leaves unfiltered)
    let Some(file) = fixture(
        &ff,
        "draft_h264.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x240:r=24:d=2,noise=alls=10:allf=t",
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-x264-params",
            "bframes=3:b-pyramid=normal:keyint=48",
            "-pix_fmt",
            "yuv420p",
        ],
    ) else {
        return;
    };
    let raw = ffmpeg_frames(&ff, &file, "yuv420p");
    let (w, h) = (320usize, 240usize);
    let fsize = w * h * 3 / 2;
    let n = raw.len() / fsize;
    assert!(n >= 40, "{n} frames");
    let reference = |i: usize| raw_planes(&raw[i * fsize..(i + 1) * fsize], w, h, w / 2, h / 2, 1);
    let src = filmcraft_codecs::open_bytes("draft_h264.mp4", bytes(&file)).expect("open");
    let rate = src.info().frame_rate();

    // Default: nothing is a draft, every frame exact.
    let g0 = filmcraft_codecs::gop_stats();
    for i in 0..n {
        let f = src.video_frame(FrameRequest { time: rate.tick_of(i as i64), scale: 0.5 }).expect("frame");
        assert_eq!(max_diff(&planes(&f), &reference(i)), 0, "frame {i} without draft mode");
    }
    assert_eq!(filmcraft_codecs::gop_stats().draft, g0.draft, "draft decoding is off by default");

    // Draft playback at 1/2 resolution on a fresh source: non-reference pictures are drafts.
    let src = filmcraft_codecs::open_bytes("draft_h264.mp4", bytes(&file)).expect("open");
    let g1 = filmcraft_codecs::gop_stats();
    let mut approximate = 0;
    for i in 0..n {
        let f = with_draft(true, || src.video_frame(FrameRequest { time: rate.tick_of(i as i64), scale: 0.5 })).expect("frame");
        approximate += (max_diff(&planes(&f), &reference(i)) > 0) as usize;
    }
    let drafts = filmcraft_codecs::gop_stats().draft - g1.draft;
    assert!(drafts > 0 && approximate > 0 && approximate as u64 <= drafts, "{drafts} draft frames, {approximate} differ");

    // Then an export from the same source (no draft hint): every frame is exact again.
    for i in 0..n {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(i as i64))).expect("frame");
        assert_eq!(max_diff(&planes(&f), &reference(i)), 0, "export frame {i} after draft playback");
    }
}
