//! Decoded pictures are written into recycled plane buffers (`filmcraft_frame::pool`). A recycled
//! buffer still holds an old picture, so the decoder must overwrite every byte: here the pool is
//! primed with buffers full of 0xAA before decoding, and every frame must still match ffmpeg bit
//! for bit. (Its own process: the pool is global.)

#[path = "it/common/mod.rs"]
mod common;

use std::sync::Arc;

use common::*;
use filmcraft_frame::{Chroma, PixelData, VideoFrame};
use filmcraft_media::FrameRequest;

#[test]
fn dirty_recycled_planes_decode_bit_exact() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    // 640×480: luma and chroma planes are both large enough to be pooled
    let Some(file) = fixture(
        &ff,
        "pool_h264.mp4",
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=640x480:r=24:d=1,noise=alls=10:allf=t",
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-x264-params",
            "bframes=2:keyint=12",
            "-pix_fmt",
            "yuv420p",
        ],
    ) else {
        return;
    };
    let (w, h) = (640usize, 480usize);
    let raw = ffmpeg_frames(&ff, &file, "yuv420p");
    let fsize = w * h * 3 / 2;
    let n = raw.len() / fsize;
    assert!(n >= 20, "{n} frames");
    let reference = |i: usize| raw_planes(&raw[i * fsize..(i + 1) * fsize], w, h, w / 2, h / 2, 1);

    // prime the pool with dirty buffers of exactly the plane sizes the decoder asks for
    for _ in 0..8 {
        let dirty = |len: usize| Arc::new(vec![0xAAu8; len]);
        let f = VideoFrame {
            data: PixelData::Yuv8 { planes: [dirty(w * h), dirty(w * h / 4), dirty(w * h / 4)], chroma: Chroma::C420, alpha: None },
            ..VideoFrame::rgba8(w as u32, h as u32, vec![0; w * h * 4])
        };
        filmcraft_frame::pool::recycle(Arc::new(f));
    }
    let reused_before = filmcraft_frame::pool::stats().reused;

    let src = filmcraft_codecs::open_bytes("pool_h264.mp4", bytes(&file)).expect("open");
    let rate = src.info().frame_rate();
    for i in 0..n {
        let f = src.video_frame(FrameRequest::full(rate.tick_of(i as i64))).expect("frame");
        assert_eq!(max_diff(&planes(&f), &reference(i)), 0, "frame {i} decoded into a recycled buffer");
    }
    assert!(filmcraft_frame::pool::stats().reused > reused_before, "the decoder took the dirty pooled buffers");
}
