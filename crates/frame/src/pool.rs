//! Recycled plane buffers.
//!
//! A decoder allocates three planes per picture and the caches free them a few hundred frames
//! later. At megabytes per plane that traffic leaves the system allocator holding gigabytes of
//! freed pages (measured: 2.5 GB retained next to 2.5 GB live after twelve clips). Decoders take
//! their planes here instead ([`take_u8`], [`take_u16`]) and caches hand evicted frames back
//! ([`recycle`]), so steady-state decoding allocates nothing.
//!
//! The compositor's working images (premultiplied linear `f32`, 33 MB at 1080p) get the same
//! treatment ([`take_f32_overwritten`], [`recycle_f32`]): allocated and freed once per layer per
//! frame they cost a zero-fill and fresh page faults every time.
//!
//! The shelves are sized for the busiest work, so they are a lot to keep once it is over: whoever
//! finishes such work (a standalone export) calls [`trim_f32`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::{PixelData, VideoFrame};

/// Bytes of idle buffers kept per sample type; a buffer that does not fit is freed.
#[cfg(test)]
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

/// Idle float images. Unlike the plane shelves a buffer keeps its length *and its old contents*:
/// whoever overwrites every element gets it back without a zero-fill pass over tens of megabytes.
static F32: Mutex<Vec<Vec<f32>>> = Mutex::new(Vec::new());

/// Bytes of idle float images kept; a buffer that does not fit is freed.
#[cfg(test)]
const F32_MAX_BYTES: usize = 320 << 20;

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
        shelf.lock().unwrap_or_else(PoisonError::into_inner).give(b, crate::memory::budgets().plane_shelf);
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

/// A buffer of exactly `len` floats whose contents are unspecified (whatever it held last, or zeros
/// when none was idle): the caller must write every element before it reads any. Handing it back
/// ([`recycle_f32`]) is optional; a buffer that is dropped is simply freed.
pub fn take_f32_overwritten(len: usize) -> Vec<f32> {
    if len.saturating_mul(4) >= MIN_BYTES {
        let mut idle = F32.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(k) = idle.iter().position(|b| b.len() == len) {
            REUSED.fetch_add(1, Ordering::Relaxed);
            return idle.swap_remove(k);
        }
    }
    vec![0.0; len]
}

/// Keep a float image nobody needs any more for the next [`take_f32_overwritten`] of its length.
pub fn recycle_f32(buf: Vec<f32>) {
    let bytes = buf.capacity().saturating_mul(4);
    if bytes < MIN_BYTES {
        return;
    }
    let mut idle = F32.lock().unwrap_or_else(PoisonError::into_inner);
    let held: usize = idle.iter().map(|b| b.capacity().saturating_mul(4)).sum();
    if held.saturating_add(bytes) <= crate::memory::budgets().float_shelf {
        idle.push(buf);
    }
}

/// Hand back a frame nobody needs any more: its planes are kept for the next [`take_u8`] /
/// [`take_u16`] unless something else still holds the frame or a plane.
pub fn recycle(frame: Arc<VideoFrame>) {
    let Ok(f) = Arc::try_unwrap(frame) else { return };
    match f.data {
        PixelData::Rgba8(d) => give(&U8, d),
        PixelData::Native(_) | PixelData::RgbaF32(_) => {}
        PixelData::Yuv8 { planes, alpha, .. } => planes.into_iter().chain(alpha).for_each(|p| give(&U8, p)),
        PixelData::Yuv16 { planes, alpha, .. } => planes.into_iter().chain(alpha).for_each(|p| give(&U16, p)),
    }
}

/// Free the idle float images. A standalone export leaves that shelf full (up to the float budget
/// of images, each the size of that export's frames) and nothing else asks for those sizes again,
/// so they would sit idle until the next export. Images still in use are not touched. The 8- and
/// 16-bit plane shelves are left alone: playback recycles decoded frames through them all the time.
pub fn trim_f32() {
    // taken under the lock, freed after it: giving back hundreds of megabytes takes a moment
    let floats = std::mem::take(&mut *F32.lock().unwrap_or_else(PoisonError::into_inner));
    drop(floats);
}

/// Idle buffers held and buffers handed out again so far (`perf.stats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolStats {
    pub idle_bytes: usize,
    /// The part of `idle_bytes` held as float images ([`recycle_f32`]).
    pub f32_idle_bytes: usize,
    pub reused: u64,
}

pub fn stats() -> PoolStats {
    let idle = |n: usize, m: usize| n.saturating_add(m);
    let a = U8.lock().unwrap_or_else(PoisonError::into_inner).bytes;
    let b = U16.lock().unwrap_or_else(PoisonError::into_inner).bytes;
    let c: usize = F32.lock().unwrap_or_else(PoisonError::into_inner).iter().map(|b| b.capacity().saturating_mul(4)).sum();
    PoolStats { idle_bytes: idle(idle(a, b), c), f32_idle_bytes: c, reused: REUSED.load(Ordering::Relaxed) }
}

/// Release idle allocations after a budget or memory-pressure change. Nothing in use is touched.
pub fn trim_to_budget() {
    let b = crate::memory::budgets();
    fn planes<T>(shelf: &Mutex<Shelf<T>>, budget: usize) {
        let mut shelf = shelf.lock().unwrap_or_else(PoisonError::into_inner);
        let mut freed = Vec::new();
        while shelf.bytes > budget {
            let Some(buf) = shelf.bufs.pop() else { break };
            shelf.bytes = shelf.bytes.saturating_sub(Shelf::<T>::size(&buf));
            freed.push(buf);
        }
        drop(shelf);
        drop(freed);
    }
    planes(&U8, b.plane_shelf);
    planes(&U16, b.plane_shelf);
    let mut floats = F32.lock().unwrap_or_else(PoisonError::into_inner);
    let mut held = floats.iter().fold(0usize, |n, v| n.saturating_add(v.capacity().saturating_mul(4)));
    let mut freed = Vec::new();
    while held > b.float_shelf {
        let Some(buf) = floats.pop() else { break };
        held = held.saturating_sub(buf.capacity().saturating_mul(4));
        freed.push(buf);
    }
    drop(floats);
    drop(freed);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Chroma;

    /// The plane shelves may hold up to this much alongside the float shelf in a test binary.
    const U8_BYTES_UPPER_BOUND: usize = 2 * MAX_BYTES;

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
    fn float_images_come_back_whole_and_unchanged_within_their_budget() {
        // a length nothing else in this test binary asks for
        let len = 1_000_003usize;
        let mut b = take_f32_overwritten(len);
        assert_eq!(b.len(), len);
        assert!(b.iter().all(|v| *v == 0.0), "a fresh buffer is zeroed");
        b[17] = 4.5;
        let at = b.as_ptr() as usize;
        recycle_f32(b);
        // the same allocation, length and contents (the caller overwrites it)
        let again = take_f32_overwritten(len);
        assert_eq!((again.as_ptr() as usize, again.len(), again[17]), (at, len, 4.5));
        // another length does not match, and is not handed the idle one
        recycle_f32(again);
        let other = take_f32_overwritten(len + 1);
        assert_eq!(other.len(), len + 1);
        assert_ne!(other.as_ptr() as usize, at);
        // small buffers are not kept, and nothing exceeds the budget
        recycle_f32(vec![0.0; 100]);
        let budget_len = F32_MAX_BYTES / 4;
        recycle_f32(vec![0.0; budget_len + 1]);
        assert!(stats().idle_bytes <= F32_MAX_BYTES + (U8_BYTES_UPPER_BOUND), "idle {}", stats().idle_bytes);
        // leave nothing behind for other tests
        drop(take_f32_overwritten(len));
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
        let plane_ptrs = |frame: &VideoFrame| match &frame.data {
            PixelData::Yuv8 { planes, .. } => planes.each_ref().map(|plane| plane.as_ptr() as usize),
            _ => panic!("fixture has three byte planes"),
        };
        // A frame someone else still holds is left alone. The reuse statistic is global:
        // concurrent float-buffer tests can change it without touching these planes.
        let shared = yuv(1);
        let held = shared.clone();
        let held_ptrs = plane_ptrs(&held);
        recycle(shared);
        let fresh = take_u8(n);
        assert!(fresh.capacity() >= n);
        assert!(!held_ptrs.contains(&(fresh.as_ptr() as usize)), "a shared plane cannot be handed out");
        assert_eq!(held.byte_size(), n + 2 * c);
        // An unshared frame's exact allocations come back, empty. Equal-size chroma
        // planes may be taken in either order.
        let unshared = yuv(2);
        let returned_ptrs = plane_ptrs(&unshared);
        recycle(unshared);
        let (y, u, v) = (take_u8(n), take_u8(c), take_u8(c));
        assert!(y.is_empty() && u.is_empty() && v.is_empty());
        assert!(y.capacity() >= n && u.capacity() >= c && v.capacity() >= c);
        assert_eq!(y.as_ptr() as usize, returned_ptrs[0]);
        let mut expected_chroma = [returned_ptrs[1], returned_ptrs[2]];
        let mut actual_chroma = [u.as_ptr() as usize, v.as_ptr() as usize];
        expected_chroma.sort_unstable();
        actual_chroma.sort_unstable();
        assert_eq!(actual_chroma, expected_chroma);
        // The returned allocations remain owned by y/u/v and cannot be handed out twice.
        let another = take_u8(n);
        assert!(another.capacity() >= n);
        assert!(!returned_ptrs.contains(&(another.as_ptr() as usize)));
    }
}
