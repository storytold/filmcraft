//! Seeking in an open-GOP H.264 transport stream restarts the decoder at the recovery point I
//! picture of the wanted GOP, not at the IDR at the start of the file. This file has its own
//! process so that the global GOP counters are not shared with other tests.

#[path = "it/common/mod.rs"]
mod common;

use common::*;
use filmcraft_codecs::MpegSource;
use filmcraft_media::{FrameRequest, MediaSource};

#[test]
fn seek_restarts_at_recovery_point_not_at_idr() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    // same fixture as `mpeg_oracle`: after the first picture every I picture is a non-IDR one
    // with a recovery point SEI, one per 12 pictures
    let args = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=25",
        "-t",
        "2.4",
        "-c:v",
        "libx264",
        "-bf",
        "2",
        "-pix_fmt",
        "yuv420p",
        "-x264-params",
        "open-gop=1:keyint=12:min-keyint=12:scenecut=0",
        "-f",
        "mpegts",
    ];
    let f = fixture(&ff, "ts_h264_opengop.ts", &args).expect("fixture");
    let src = MpegSource::open("ts_h264_opengop.ts", bytes(&f)).unwrap();
    let rate = src.info().frame_rate();
    let n = rate.frame_at(src.info().duration);
    assert!(n >= 48, "{n} frames");
    // a frame in the last GOP: from the IDR it is 48+ samples away, from its recovery key at most 12
    let g0 = filmcraft_codecs::gop_stats();
    src.video_frame(FrameRequest::full(rate.tick_of(n - 3))).unwrap();
    let g1 = filmcraft_codecs::gop_stats();
    let (seeks, decoded) = (g1.seeks - g0.seeks, g1.decoded - g0.decoded);
    assert_eq!(seeks, 1, "one decoder restart");
    assert!(decoded <= 30, "{decoded} samples decoded for frame {} of {n}: the seek went back to the IDR", n - 3);
}
