//! Recycled plane buffers.
//!
//! A decoder allocates three planes per picture and the caches free them a few hundred frames
//! later. At megabytes per plane that traffic leaves the system allocator holding gigabytes of
//! freed pages (measured: 2.5 GB retained next to 2.5 GB live after twelve clips). Decoders take
//! their planes here instead ([`take_u8`], [`take_u16`]) and caches hand evicted frames back
//! ([`recycle`]), so steady-state decoding allocates nothing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::{PixelData, VideoFrame};

/// Bytes of idle buffers kept per sample type; a buffer that does not fit is freed.
const MAX_BYTES: usize = 192 << 20;

/// Smaller buffers are not worth keeping (the allocator recycles those well).
const MIN_BYTES: usize = 64 << 10;

struct Shelf<T> {
    bufs: Vec<Vec<T>>,
    bytes: usize,
}

impl<T> Shelf<T> {
    const fn new() -> Self {
        Self { bufs: Vec::new(), bytes: 0 }
    }

    fn size(b: &Vec<T>) -> usize {
        b.capacity().saturating_mul(std::mem::size_of::<T>())
    }

    /// The smallest idle buffer that holds `len` samples without wasting over a quarter of it.
    fn take(&mut self, len: usize) -> Option<Vec<T>> {
        let k = self
            .bufs
            .iter()
            .enumerate()
            .filter(|(_, b)| b.capacity() >= len && b.capacity() - len <= len / 4)
            .min_by_key(|(_, b)| b.capacity())
            .map(|(k, _)| k)?;
        let b = self.bufs.swap_remove(k);
        self.bytes = self.bytes.saturating_sub(Self::size(&b));
        Some(b)
    }

    fn give(&mut self, mut b: Vec<T>, max_bytes: usize) {
        let n = Self::size(&b);
        if n < MIN_BYTES || self.bytes.saturating_add(n) > max_bytes {
            return;
        }
        b.clear();
        self.bytes += n;
        self.bufs.push(b);
    }
}

static U8: Mutex<Shelf<u8>> = Mutex::new(Shelf::new());
static U16: Mutex<Shelf<u16>> = Mutex::new(Shelf::new());
static REUSED: AtomicU64 = AtomicU64::new(0);

fn take<T>(shelf: &Mutex<Shelf<T>>, len: usize) -> Vec<T> {
    let found = shelf.lock().unwrap_or_else(PoisonError::into_inner).take(len);
    match found {
        Some(b) => {
            REUSED.fetch_add(1, Ordering::Relaxed);
            b
        }
        None => Vec::with_capacity(len),
    }
}

fn give<T>(shelf: &Mutex<Shelf<T>>, plane: Arc<Vec<T>>) {
    // a plane something else still shows (a monitor, a GPU upload in flight) is theirs
    if let Ok(b) = Arc::try_unwrap(plane) {
        shelf.lock().unwrap_or_else(PoisonError::into_inner).give(b, MAX_BYTES);
    }
}

/// An empty buffer with room for `len` samples: a recycled one when one fits.
pub fn take_u8(len: usize) -> Vec<u8> {
    take(&U8, len)
}

/// [`take_u8`] for 16-bit planes.
pub fn take_u16(len: usize) -> Vec<u16> {
    take(&U16, len)
}

/// Hand back a frame nobody needs any more: its planes are kept for the next [`take_u8`] /
/// [`take_u16`] unless something else still holds the frame or a plane.
pub fn recycle(frame: Arc<VideoFrame>) {
    let Ok(f) = Arc::try_unwrap(frame) else { return };
    match f.data {
        PixelData::Rgba8(d) => give(&U8, d),
        // a GPU picture has no planes; its surface goes back to its own pool when dropped
        PixelData::RgbaF32(_) | PixelData::Gpu(_) => {}
        PixelData::Yuv8 { planes, alpha, .. } => planes.into_iter().chain(alpha).for_each(|p| give(&U8, p)),
        PixelData::Yuv16 { planes, alpha, .. } => planes.into_iter().chain(alpha).for_each(|p| give(&U16, p)),
    }
}

/// Idle buffers held and buffers handed out again so far (`perf.stats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolStats {
    pub idle_bytes: usize,
    pub reused: u64,
}

pub fn stats() -> PoolStats {
    let idle = |n: usize, m: usize| n.saturating_add(m);
    let a = U8.lock().unwrap_or_else(PoisonError::into_inner).bytes;
    let b = U16.lock().unwrap_or_else(PoisonError::into_inner).bytes;
    PoolStats { idle_bytes: idle(a, b), reused: REUSED.load(Ordering::Relaxed) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Chroma;

    #[test]
    fn shelf_hands_back_a_fitting_buffer_and_stays_within_its_budget() {
        let mut s = Shelf::<u8>::new();
        s.give(Vec::with_capacity(1 << 20), 3 << 20);
        s.give(Vec::with_capacity(2 << 20), 3 << 20);
        assert_eq!((s.bufs.len(), s.bytes), (2, 3 << 20));
        // over the budget and too small to keep: freed
        s.give(Vec::with_capacity(1 << 20), 3 << 20);
        s.give(Vec::with_capacity(1024), 3 << 20);
        assert_eq!(s.bufs.len(), 2);
        // nothing fits a far smaller or a larger request
        assert!(s.take(100 << 10).is_none());
        assert!(s.take(3 << 20).is_none());
        // the smallest buffer that fits, with its contents gone
        let b = s.take((1 << 20) - 4096).expect("fits");
        assert_eq!((b.len(), b.capacity()), (0, 1 << 20));
        assert_eq!((s.bufs.len(), s.bytes), (1, 2 << 20));
        assert!(s.take(1 << 20).is_none(), "2 MiB wastes too much for 1 MiB");
    }

    #[test]
    fn recycled_frames_feed_the_next_take_unless_still_shared() {
        // sizes nothing else in this test binary asks for
        let (n, c) = (777_001usize, 333_003usize);
        let yuv = |fill: u8| {
            Arc::new(VideoFrame {
                width: 1,
                height: 1,
                data: PixelData::Yuv8 {
                    planes: [Arc::new(vec![fill; n]), Arc::new(vec![fill; c]), Arc::new(vec![fill; c])],
                    chroma: Chroma::C420,
                    alpha: None,
                },
                color: filmcraft_color::ColorInfo::SRGB_FULL,
                par: (1, 1),
                pts: filmcraft_time::Tick::ZERO,
            })
        };
        let before = stats().reused;
        // a frame someone else still holds is left alone
        let shared = yuv(1);
        let held = shared.clone();
        recycle(shared);
        assert!(take_u8(n).capacity() >= n);
        assert_eq!(stats().reused, before);
        assert_eq!(held.byte_size(), n + 2 * c);
        // an unshared frame's planes come back, empty
        recycle(yuv(2));
        let (y, u, v) = (take_u8(n), take_u8(c), take_u8(c));
        assert!(y.is_empty() && y.capacity() >= n && u.capacity() >= c && v.capacity() >= c);
        assert_eq!(stats().reused, before + 3);
        // and are not handed out twice
        take_u8(n);
        assert_eq!(stats().reused, before + 3);
    }
}
