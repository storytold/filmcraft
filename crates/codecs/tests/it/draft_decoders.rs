//! Draft decoding of H.264, VP9, HEVC and AV1 through the media stack:
//! off by default, approximate frames only reach draft requests, and decoding for an export after
//! draft playback (same source, same GOP cache) still matches ffmpeg bit for bit.

use crate::common;

use common::*;
use filmcraft_media::FrameRequest;
use filmcraft_media::cancel::with_draft;

/// Play `name` without and with draft decoding, then "export" it: every non-draft request must
/// be bit-exact with ffmpeg, and draft playback must have produced some approximate frames.
fn check(name: &str, args: &[&str], w: usize, h: usize) {
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some(file) = fixture(&ff, name, args) else { return };
    let raw = ffmpeg_frames(&ff, &file, "yuv420p");
    let fsize = w * h * 3 / 2;
    let n = raw.len() / fsize;
    assert!(n >= 16, "{name}: {n} frames");
    let reference = |i: usize| raw_planes(&raw[i * fsize..(i + 1) * fsize], w, h, w / 2, h / 2, 1);

    // Default: nothing is a draft, every frame exact.
    let src = filmcraft_codecs::open_bytes(name, bytes(&file)).expect("open");
    let rate = src.info().frame_rate();
    let g0 = filmcraft_codecs::gop_stats();
    for i in 0..n {
        let f = src.video_frame(FrameRequest { time: rate.tick_of(i as i64), scale: 0.5 }).expect("frame");
        assert_eq!(max_diff(&planes(&f), &reference(i)), 0, "{name}: frame {i} without draft mode");
    }
    assert_eq!(filmcraft_codecs::gop_stats().draft, g0.draft, "{name}: draft decoding is off by default");

    // Draft playback at 1/2 resolution on a fresh source: non-reference frames are drafts.
    let src = filmcraft_codecs::open_bytes(name, bytes(&file)).expect("open");
    let g1 = filmcraft_codecs::gop_stats();
    let mut approximate = 0;
    for i in 0..n {
        let f = with_draft(true, || src.video_frame(FrameRequest { time: rate.tick_of(i as i64), scale: 0.5 })).expect("frame");
        approximate += (max_diff(&planes(&f), &reference(i)) > 0) as usize;
    }
    let drafts = filmcraft_codecs::gop_stats().draft - g1.draft;
    assert!(drafts > 0 && approximate > 0 && approximate as u64 <= drafts, "{name}: {drafts} draft frames, {approximate} differ");

    // Then an export from the same source (no draft hint): every frame is exact again.
    for i in 0..n {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(i as i64))).expect("frame");
        assert_eq!(max_diff(&planes(&f), &reference(i)), 0, "{name}: export frame {i} after draft playback");
    }
}

/// One test (the GOP cache statistics are process-wide, so the codecs run one after another).
#[test]
fn draft_playback_never_reaches_exports() {
    crate::h264_draft::check();
    vp9();
    hevc();
    av1();
}

fn vp9() {
    // Two temporal layers: every other frame refreshes no reference slot. (25 fps: WebM stores
    // millisecond timestamps.)
    check(
        "draft_vp9.webm",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x240:r=25:d=1,noise=alls=10:allf=t",
            "-c:v",
            "libvpx-vp9",
            "-deadline",
            "realtime",
            "-speed",
            "8",
            "-b:v",
            "600k",
            "-ts-parameters",
            "ts_number_layers=2:ts_target_bitrate=300,600:ts_rate_decimator=2,1:ts_periodicity=2:ts_layer_id=0,1:ts_layering_mode=2",
            "-pix_fmt",
            "yuv420p",
        ],
        320,
        240,
    );
}

fn hevc() {
    // B-pyramid: non-reference B pictures (sub-layer non-reference NAL types).
    check(
        "draft_hevc.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=320x240:r=25:d=1,noise=alls=10:allf=t",
            "-c:v",
            "libx265",
            "-preset",
            "fast",
            "-x265-params",
            "bframes=4:b-pyramid=1:keyint=24:log-level=error",
            "-pix_fmt",
            "yuv420p",
            "-tag:v",
            "hvc1",
        ],
        320,
        240,
    );
}

fn av1() {
    // SVT-AV1 hierarchical GOP: the top layer refreshes no reference slot.
    check(
        "draft_av1.mp4",
        &["-f", "lavfi", "-i", "testsrc2=s=320x240:r=25:d=1,noise=alls=10:allf=t", "-c:v", "libsvtav1", "-preset", "8", "-crf", "35", "-pix_fmt", "yuv420p"],
        320,
        240,
    );
}
