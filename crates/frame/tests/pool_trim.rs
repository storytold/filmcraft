//! `pool::trim_f32` frees the idle float images of the process-wide pool and leaves the plane
//! shelves alone. This file holds one test on purpose: the pool is shared by every test of a binary,
//! and another test recycling or taking buffers while this one counts them would make the counts
//! meaningless.

use std::sync::Arc;

use filmcraft_frame::{Chroma, PixelData, VideoFrame, pool};

const FLOATS: usize = 300_000;
const BYTES: usize = 200_000;
const WORDS: usize = 100_000;

fn frame(data: PixelData) -> Arc<VideoFrame> {
    Arc::new(VideoFrame { width: 1, height: 1, data, color: filmcraft_color::ColorInfo::SRGB_FULL, par: (1, 1), pts: filmcraft_time::Tick::ZERO })
}

/// Hand the pool one 8-bit and one 16-bit plane (the other planes are too small to be kept).
fn recycle_planes() {
    let empty = || Arc::new(Vec::<u8>::new());
    pool::recycle(frame(PixelData::Yuv8 { planes: [Arc::new(vec![7u8; BYTES]), empty(), empty()], chroma: Chroma::C420, alpha: None }));
    let empty = || Arc::new(Vec::<u16>::new());
    pool::recycle(frame(PixelData::Yuv16 { planes: [Arc::new(vec![7u16; WORDS]), empty(), empty()], chroma: Chroma::C420, bits: 10, alpha: None }));
}

#[test]
fn trim_f32_frees_the_float_images_keeps_the_planes_and_the_pool_keeps_working() {
    assert_eq!(pool::stats().idle_bytes, 0, "nothing has been recycled yet");

    // one idle buffer on each of the three shelves
    pool::recycle_f32(vec![f32::NAN; FLOATS]);
    recycle_planes();
    let planes = BYTES + WORDS * 2;
    assert_eq!(pool::stats().f32_idle_bytes, FLOATS * 4);
    assert!(pool::stats().idle_bytes >= FLOATS * 4 + planes, "idle {}", pool::stats().idle_bytes);

    pool::trim_f32();
    assert_eq!(pool::stats().f32_idle_bytes, 0, "the float shelf is empty");
    assert!(pool::stats().idle_bytes >= planes, "the plane shelves are untouched");

    // the float image is a fresh (zeroed) one and doesn't count as a reuse; the planes come back
    let reused = pool::stats().reused;
    let image = pool::take_f32_overwritten(FLOATS);
    assert!(image.iter().all(|v| *v == 0.0), "the recycled image was freed");
    assert_eq!(pool::stats().reused, reused);
    drop((pool::take_u8(BYTES), pool::take_u16(WORDS)));
    assert!(pool::stats().reused > reused, "the planes were kept");

    // the pool works after a trim, and trimming twice (or an empty shelf) is fine
    pool::recycle_f32(image);
    assert_eq!(pool::stats().f32_idle_bytes, FLOATS * 4);
    pool::trim_f32();
    pool::trim_f32();
    assert_eq!(pool::stats().f32_idle_bytes, 0);
}
