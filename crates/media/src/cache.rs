//! A byte-budgeted LRU cache of decoded/rendered frames, shared by monitors, thumbnails and export,
//! and of decoded audio PCM, shared by every standalone audio source.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

use filmcraft_frame::VideoFrame;

struct Entry {
    frame: Arc<VideoFrame>,
    bytes: usize,
    stamp: u64,
}

struct Inner<K> {
    map: HashMap<K, Entry>,
    bytes: usize,
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
        Self { inner: Mutex::new(Inner { map: HashMap::new(), bytes: 0, clock: 0, hits: 0, misses: 0 }), budget: budget_bytes }
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
        let bytes = frame.byte_size();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.clock += 1;
        let stamp = g.clock;
        if let Some(old) = g.map.insert(k, Entry { frame, bytes, stamp }) {
            g.bytes -= old.bytes;
        }
        g.bytes += bytes;
        if g.bytes > self.budget {
            // Evict oldest entries until under 90% of budget.
            let mut v: Vec<(u64, K, usize)> = g.map.iter().map(|(k, e)| (e.stamp, k.clone(), e.bytes)).collect();
            v.sort_unstable_by_key(|x| x.0);
            let target = self.budget * 9 / 10;
            for (_, k, b) in v {
                if g.bytes <= target {
                    break;
                }
                g.map.remove(&k);
                g.bytes -= b;
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
        g.bytes = 0;
    }

    pub fn retain(&self, mut keep: impl FnMut(&K) -> bool) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut freed = 0;
        g.map.retain(|k, e| {
            let k2 = keep(k);
            if !k2 {
                freed += e.bytes;
            }
            k2
        });
        g.bytes -= freed;
    }

    /// (entries, bytes, hits, misses)
    pub fn stats(&self) -> (usize, usize, u64, u64) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (g.map.len(), g.bytes, g.hits, g.misses)
    }
}

struct PcmEntry {
    samples: Arc<Vec<Vec<f32>>>,
    bytes: usize,
    stamp: u64,
}

struct AudioInner<K> {
    map: HashMap<K, PcmEntry>,
    bytes: usize,
    clock: u64,
    hits: u64,
    misses: u64,
}

/// A byte-budgeted LRU cache of decoded planar PCM, shared by every standalone audio source:
/// the decoded samples of a compressed file cost far more than the file (a 5 MB MP3 is ~230 MB
/// of 48 kHz stereo f32), so sources hold their bytes and borrow their samples here. A source
/// whose samples were evicted decodes its file again on the next read.
pub struct AudioCache<K: Eq + Hash + Clone = u64> {
    inner: Mutex<AudioInner<K>>,
    budget: usize,
}

impl<K: Eq + Hash + Clone> AudioCache<K> {
    pub fn new(budget_bytes: usize) -> Self {
        Self { inner: Mutex::new(AudioInner { map: HashMap::new(), bytes: 0, clock: 0, hits: 0, misses: 0 }), budget: budget_bytes }
    }

    pub fn get(&self, k: &K) -> Option<Arc<Vec<Vec<f32>>>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.clock += 1;
        let c = g.clock;
        match g.map.get_mut(k) {
            Some(e) => {
                e.stamp = c;
                let s = e.samples.clone();
                g.hits += 1;
                Some(s)
            }
            None => {
                g.misses += 1;
                None
            }
        }
    }

    pub fn insert(&self, k: K, samples: Arc<Vec<Vec<f32>>>) {
        let bytes = samples.iter().map(Vec::len).sum::<usize>().saturating_mul(core::mem::size_of::<f32>());
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.clock += 1;
        let stamp = g.clock;
        if let Some(old) = g.map.insert(k.clone(), PcmEntry { samples, bytes, stamp }) {
            g.bytes = g.bytes.saturating_sub(old.bytes);
        }
        g.bytes = g.bytes.saturating_add(bytes);
        if g.bytes > self.budget {
            // Evict oldest entries until under 90% of budget, but never the one just inserted:
            // a file whose PCM alone exceeds the budget still has to play, and re-decoding it on
            // every read would cost more than the memory it holds.
            let mut v: Vec<(u64, K)> = g.map.iter().filter(|(key, _)| **key != k).map(|(key, e)| (e.stamp, key.clone())).collect();
            v.sort_unstable_by_key(|x| x.0);
            let target = self.budget * 9 / 10;
            for (_, key) in v {
                if g.bytes <= target {
                    break;
                }
                if let Some(e) = g.map.remove(&key) {
                    g.bytes = g.bytes.saturating_sub(e.bytes);
                }
            }
        }
    }

    pub fn get_or_insert_with<E>(&self, k: K, f: impl FnOnce() -> Result<Arc<Vec<Vec<f32>>>, E>) -> Result<Arc<Vec<Vec<f32>>>, E> {
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
        g.bytes = 0;
    }

    /// (entries, bytes, hits, misses)
    pub fn stats(&self) -> (usize, usize, u64, u64) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (g.map.len(), g.bytes, g.hits, g.misses)
    }

    /// Drop one entry (the source it belonged to re-decodes on its next read).
    pub fn remove(&self, k: &K) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.map.remove(k) {
            g.bytes = g.bytes.saturating_sub(e.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn pcm(frames: usize) -> Arc<Vec<Vec<f32>>> {
        Arc::new(vec![vec![0.0; frames]; 2])
    }

    #[test]
    fn audio_evicts_lru_by_bytes() {
        // budget = three entries of 2 channels x 4 frames x 4 bytes = 32 bytes each
        let c: AudioCache<u32> = AudioCache::new(96);
        c.insert(0, pcm(4));
        c.insert(1, pcm(4));
        c.insert(2, pcm(4));
        assert!(c.get(&0).is_some()); // touch 0
        c.insert(3, pcm(4));
        assert!(c.get(&0).is_some());
        assert!(c.get(&1).is_none());
        assert!(c.stats().1 <= 96);
    }

    #[test]
    fn audio_never_evicts_the_entry_just_inserted() {
        // one entry alone is over budget: it stays (re-decoding every read would be worse)
        let c: AudioCache<u32> = AudioCache::new(16);
        c.insert(0, pcm(8));
        assert!(c.get(&0).is_some());
        assert_eq!(c.stats().0, 1);
    }

    #[test]
    fn audio_get_or_insert_decodes_once() {
        let c: AudioCache<u32> = AudioCache::new(1 << 20);
        let mut calls = 0;
        for _ in 0..2 {
            c.get_or_insert_with(7, || {
                calls += 1;
                Ok::<_, ()>(pcm(4))
            })
            .unwrap();
        }
        assert_eq!(calls, 1);
        let (entries, bytes, hits, misses) = c.stats();
        assert_eq!((entries, bytes, hits, misses), (1, 32, 1, 1));
    }

    #[test]
    fn audio_clear_empties() {
        let c: AudioCache<u32> = AudioCache::new(1 << 20);
        c.insert(0, pcm(4));
        c.clear();
        assert_eq!(c.stats(), (0, 0, 0, 0));
        assert!(c.get(&0).is_none());
    }

    #[test]
    fn audio_remove_drops_one_entry() {
        let c: AudioCache<u32> = AudioCache::new(1 << 20);
        c.insert(0, pcm(4));
        c.insert(1, pcm(4));
        c.remove(&0);
        assert!(c.get(&0).is_none());
        assert!(c.get(&1).is_some());
        assert_eq!(c.stats().1, 32);
    }
}
