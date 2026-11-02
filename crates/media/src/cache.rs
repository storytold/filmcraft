//! A byte-budgeted LRU cache of decoded/rendered frames, shared by monitors, thumbnails and export.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use filmcraft_frame::{MemoryAccount, MemoryLease, VideoFrame};

struct Entry {
    frame: Arc<VideoFrame>,
    _charge: MemoryLease,
    stamp: u64,
}

struct Inner<K> {
    map: HashMap<K, Entry>,
    bytes: MemoryAccount,
    clock: u64,
    hits: u64,
    misses: u64,
}

pub struct FrameCache<K: Eq + Hash + Clone = (u64, i64, u16)> {
    inner: Mutex<Inner<K>>,
    budget: usize,
}

impl<K: Eq + Hash + Clone> FrameCache<K> {
    pub fn new(budget_bytes: usize) -> Self {
        Self { inner: Mutex::new(Inner { map: HashMap::new(), bytes: MemoryAccount::default(), clock: 0, hits: 0, misses: 0 }), budget: budget_bytes }
    }

    pub fn get(&self, k: &K) -> Option<Arc<VideoFrame>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.clock += 1;
        let c = g.clock;
        match g.map.get_mut(k) {
            Some(e) => {
                e.stamp = c;
                let f = e.frame.clone();
                g.hits += 1;
                Some(f)
            }
            None => {
                g.misses += 1;
                None
            }
        }
    }

    pub fn insert(&self, k: K, frame: Arc<VideoFrame>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.clock += 1;
        let stamp = g.clock;
        let charge = frame.memory_charge(&g.bytes);
        g.map.insert(k, Entry { frame, _charge: charge, stamp });
        if g.bytes.bytes() > self.budget {
            // Evict oldest entries until under 90% of budget.
            let mut v: Vec<(u64, K)> = g.map.iter().map(|(k, e)| (e.stamp, k.clone())).collect();
            v.sort_unstable_by_key(|x| x.0);
            let target = self.budget / 10 * 9 + self.budget % 10 * 9 / 10;
            for (_, k) in v {
                if g.bytes.bytes() <= target {
                    break;
                }
                g.map.remove(&k);
            }
        }
    }

    pub fn get_or_insert_with<E>(&self, k: K, f: impl FnOnce() -> Result<Arc<VideoFrame>, E>) -> Result<Arc<VideoFrame>, E> {
        if let Some(v) = self.get(&k) {
            return Ok(v);
        }
        let v = f()?;
        self.insert(k, v.clone());
        Ok(v)
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.map.clear();
    }

    pub fn retain(&self, mut keep: impl FnMut(&K) -> bool) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.map.retain(|k, _| keep(k));
    }

    /// (entries, bytes, hits, misses)
    pub fn stats(&self) -> (usize, usize, u64, u64) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (g.map.len(), g.bytes.bytes(), g.hits, g.misses)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Surface;

    impl filmcraft_frame::GpuSurface for Surface {
        fn size(&self) -> (u32, u32) {
            (2, 2)
        }
        fn byte_len(&self) -> usize {
            2 * 2 * 4
        }
        fn id(&self) -> u64 {
            1
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn prepare_cpu(&self, transfer: filmcraft_frame::CpuTransfer) {
            transfer.complete(Ok(filmcraft_frame::PixelData::RgbaF32(Arc::new(vec![0.0; 2 * 2 * 4]))));
        }
    }

    #[test]
    fn late_readback_replacement_retain_and_clear_use_live_entry_charges() {
        use filmcraft_frame::{Chroma, GpuFormat, GpuPixels, PixelData};
        let gpu_bytes = 2 * 2 * 4;
        let cpu_bytes = 2 * 2 * 4 * size_of::<f32>();
        let frame = Arc::new(VideoFrame {
            data: PixelData::Gpu(GpuPixels::new(Arc::new(Surface), GpuFormat::Yuv { chroma: Chroma::C420, bits: 8 })),
            ..VideoFrame::rgba8(2, 2, vec![0; gpu_bytes])
        });
        let cache: FrameCache<u8> = FrameCache::new((gpu_bytes + cpu_bytes) * 4);
        cache.insert(0, frame.clone());
        cache.insert(1, frame.clone());
        assert_eq!(cache.stats().1, gpu_bytes * 2);
        frame.cpu().unwrap();
        assert_eq!(cache.stats().1, (gpu_bytes + cpu_bytes) * 2);
        cache.retain(|key| *key == 1);
        assert_eq!(cache.stats().1, gpu_bytes + cpu_bytes);
        let replacement = Arc::new(VideoFrame::rgba8(2, 2, vec![0; gpu_bytes]));
        cache.insert(1, replacement);
        assert_eq!(cache.stats().1, gpu_bytes);
        cache.clear();
        assert_eq!(cache.stats().1, 0);
        assert!(frame.cpu().is_ok(), "cache eviction must not invalidate external frame references");
    }

    #[test]
    fn evicts_lru() {
        let c: FrameCache<u32> = FrameCache::new(4 * 4 * 3);
        for i in 0..3 {
            c.insert(i, Arc::new(VideoFrame::rgba8(2, 2, vec![0; 16])));
        }
        assert!(c.get(&0).is_some()); // touch 0
        c.insert(3, Arc::new(VideoFrame::rgba8(2, 2, vec![0; 16])));
        assert!(c.get(&0).is_some());
        assert!(c.get(&1).is_none());
        assert!(c.stats().1 <= 48);
    }

    #[derive(Debug)]
    struct DeferredSurface {
        started: std::sync::mpsc::Sender<()>,
        result: Mutex<std::sync::mpsc::Receiver<Result<filmcraft_frame::PixelData, String>>>,
    }

    impl filmcraft_frame::GpuSurface for DeferredSurface {
        fn size(&self) -> (u32, u32) {
            (2, 2)
        }
        fn byte_len(&self) -> usize {
            2 * 2 * 4
        }
        fn id(&self) -> u64 {
            1
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn prepare_cpu(&self, transfer: filmcraft_frame::CpuTransfer) {
            self.started.send(()).unwrap();
            let result = self.result.lock().unwrap().recv().unwrap_or_else(|_| Err("test readback producer dropped".into()));
            transfer.complete(result);
        }
    }

    struct DeferredReadback {
        result: Option<std::sync::mpsc::Sender<Result<filmcraft_frame::PixelData, String>>>,
        worker: Option<std::thread::JoinHandle<Result<(), String>>>,
    }

    impl DeferredReadback {
        fn complete(mut self, result: Result<filmcraft_frame::PixelData, String>) {
            self.result.take().unwrap().send(result).unwrap();
        }
    }

    impl Drop for DeferredReadback {
        fn drop(&mut self) {
            drop(self.result.take());
            let _ = self.worker.take().unwrap().join().unwrap();
        }
    }

    fn deferred_frame() -> (Arc<VideoFrame>, DeferredReadback) {
        use filmcraft_frame::{Chroma, GpuFormat, GpuPixels, PixelData};
        // Harness startup bound; no decode-time or throughput assertion.
        const START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
        let (started, receive_start) = std::sync::mpsc::channel();
        let (result, receive_result) = std::sync::mpsc::channel();
        let gpu = GpuPixels::new(Arc::new(DeferredSurface { started, result: Mutex::new(receive_result) }), GpuFormat::Yuv { chroma: Chroma::C420, bits: 8 });
        let frame = Arc::new(VideoFrame { data: PixelData::Gpu(gpu), ..VideoFrame::rgba8(2, 2, vec![0; 2 * 2 * 4]) });
        let copy = frame.clone();
        let worker = std::thread::spawn(move || copy.cpu().map(|_| ()));
        let transfer = DeferredReadback { result: Some(result), worker: Some(worker) };
        receive_start.recv_timeout(START_TIMEOUT).unwrap();
        (frame, transfer)
    }

    #[test]
    fn delayed_completion_respects_replaced_removed_and_dropped_cache_entries() {
        let gpu_bytes = 2 * 2 * 4;
        let cpu_bytes = 2 * 2 * 4 * size_of::<f32>();
        for complete_first in [false, true] {
            let (frame, transfer) = deferred_frame();
            let a: FrameCache<u8> = FrameCache::new(usize::MAX);
            let b: FrameCache<u8> = FrameCache::new(usize::MAX);
            let a_total = a.inner.lock().unwrap().bytes.clone();
            let b_total = b.inner.lock().unwrap().bytes.clone();
            a.insert(0, frame.clone());
            a.insert(1, frame.clone());
            b.insert(0, frame.clone());
            assert_eq!((a_total.bytes(), b_total.bytes()), (gpu_bytes * 2, gpu_bytes));
            let mut transfer = Some(transfer);
            let mut finish = || transfer.take().unwrap().complete(Ok(filmcraft_frame::PixelData::RgbaF32(Arc::new(vec![0.0; 2 * 2 * 4]))));
            if complete_first {
                finish();
            }
            a.insert(0, Arc::new(VideoFrame::rgba8(2, 2, vec![0; gpu_bytes])));
            b.retain(|_| false);
            drop(b);
            if !complete_first {
                finish();
            }
            assert_eq!((a_total.bytes(), b_total.bytes()), (gpu_bytes * 2 + cpu_bytes, 0));
            assert!(frame.cpu().is_ok());
            drop(a);
            assert_eq!((a_total.bytes(), b_total.bytes()), (0, 0));
            assert!(frame.cpu().is_ok());
        }
    }

    #[test]
    fn delayed_completion_racing_cache_removal_restores_both_accounts() {
        // Bounded schedule sampling; ordered cases above cover both terminal orders.
        const RACE_ATTEMPTS: usize = 32;
        let gpu_bytes = 2 * 2 * 4;
        let cpu_bytes = 2 * 2 * 4 * size_of::<f32>();
        for _ in 0..RACE_ATTEMPTS {
            let (frame, transfer) = deferred_frame();
            let a: FrameCache<u8> = FrameCache::new(usize::MAX);
            let b: FrameCache<u8> = FrameCache::new(usize::MAX);
            a.insert(0, frame.clone());
            a.insert(1, frame.clone());
            b.insert(0, frame.clone());
            let a_total = a.inner.lock().unwrap().bytes.clone();
            let b_total = b.inner.lock().unwrap().bytes.clone();
            let gate = Arc::new(std::sync::Barrier::new(2));
            let worker_gate = gate.clone();
            let worker = std::thread::spawn(move || {
                worker_gate.wait();
                transfer.complete(Ok(filmcraft_frame::PixelData::RgbaF32(Arc::new(vec![0.0; 2 * 2 * 4]))));
            });
            gate.wait();
            a.insert(0, Arc::new(VideoFrame::rgba8(2, 2, vec![0; gpu_bytes])));
            drop(b);
            worker.join().unwrap();
            assert_eq!((a_total.bytes(), b_total.bytes()), (gpu_bytes * 2 + cpu_bytes, 0));
            drop(a);
            assert_eq!((a_total.bytes(), b_total.bytes()), (0, 0));
        }
    }

    #[test]
    fn delayed_failure_and_abandonment_preserve_error_without_cache_cpu_charge() {
        let gpu_bytes = 2 * 2 * 4;
        for abandon in [false, true] {
            let (frame, transfer) = deferred_frame();
            let cache: FrameCache<u8> = FrameCache::new(usize::MAX);
            let total = cache.inner.lock().unwrap().bytes.clone();
            cache.insert(0, frame.clone());
            cache.insert(1, frame.clone());
            if abandon {
                drop(transfer);
            } else {
                transfer.complete(Err("injected delayed readback failure".into()));
            }
            let expected = if abandon { "test readback producer dropped" } else { "injected delayed readback failure" };
            for _ in [0, 1] {
                assert_eq!(frame.cpu().err().as_deref(), Some(expected));
                assert_eq!(total.bytes(), gpu_bytes * 2);
            }
            drop(cache);
            assert_eq!(total.bytes(), 0);
            assert_eq!(frame.cpu().err().as_deref(), Some(expected));
        }
    }

    #[test]
    fn late_growth_keeps_external_pixels_and_uses_normal_eviction_timing() {
        use filmcraft_frame::{Chroma, GpuFormat, GpuPixels, PixelData};
        let gpu_bytes = 2 * 2 * 4;
        let cpu_bytes = 2 * 2 * 4 * size_of::<f32>();
        let frame = Arc::new(VideoFrame {
            data: PixelData::Gpu(GpuPixels::new(Arc::new(Surface), GpuFormat::Yuv { chroma: Chroma::C420, bits: 8 })),
            ..VideoFrame::rgba8(2, 2, vec![0; gpu_bytes])
        });
        let cache: FrameCache<u8> = FrameCache::new(gpu_bytes * 2);
        cache.insert(0, frame.clone());
        frame.cpu().unwrap();
        assert_eq!((cache.stats().0, cache.stats().1), (1, gpu_bytes + cpu_bytes));
        cache.insert(1, Arc::new(VideoFrame::rgba8(2, 2, vec![0; gpu_bytes])));
        assert!(cache.get(&0).is_none());
        assert_eq!(cache.stats().1, gpu_bytes);
        assert!(frame.cpu().is_ok());
    }
}
