//! `pool::trim_planes` frees the idle 8- and 16-bit plane buffers, leaves the float shelf alone and
//! the pool keeps working afterwards. One test on purpose: the pool is shared by every test of a
//! binary, and another test taking buffers while this one counts them would make the counts
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
fn trim_planes_frees_the_plane_shelves_keeps_the_floats_and_the_pool_keeps_working() {
    assert_eq!(pool::stats().idle_bytes, 0, "nothing has been recycled yet");
    pool::recycle_f32(vec![0.0; FLOATS]);
    recycle_planes();
    assert!(pool::stats().idle_bytes >= FLOATS * 4 + BYTES + WORDS * 2);

    pool::trim_planes();
    assert_eq!(pool::stats().idle_bytes, FLOATS * 4, "only the float image is left");
    assert_eq!(pool::stats().f32_idle_bytes, FLOATS * 4);

    // the planes were freed, so nothing is reused; the shelves work again afterwards
    let reused = pool::stats().reused;
    drop((pool::take_u8(BYTES), pool::take_u16(WORDS)));
    assert_eq!(pool::stats().reused, reused);
    recycle_planes();
    drop((pool::take_u8(BYTES), pool::take_u16(WORDS)));
    assert!(pool::stats().reused > reused);

    // trimming an empty shelf twice is fine
    pool::trim_planes();
    pool::trim_planes();
    pool::trim_f32();
    assert_eq!(pool::stats().idle_bytes, 0);
}
