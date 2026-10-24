//! A byte-budgeted LRU cache of decoded/rendered frames, shared by monitors, thumbnails and export.

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
    /// What an eviction brings `bytes` down to: 90% of the budget. Computed without multiplying
    /// the budget first, which overflows a 32-bit `usize` (wasm32) past about 455 MiB.
    evict_to: usize,
}

impl<K: Eq + Hash + Clone> FrameCache<K> {
    pub fn new(budget_bytes: usize) -> Self {
        Self { inner: Mutex::new(Inner { map: HashMap::new(), bytes: 0, clock: 0, hits: 0, misses: 0 }), budget: budget_bytes, evict_to: budget_bytes / 10 * 9 }
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

    /// Insert `frame` under `k`, evicting the least recently used entries past the budget.
    ///
    /// The entry just inserted is never one of them, so a frame larger than the budget stays
    /// cached on its own (over budget) until the next insert pushes it out, instead of being
    /// decoded again every time it is asked for.
    pub fn insert(&self, k: K, frame: Arc<VideoFrame>) {
        let bytes = frame.byte_size();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.clock += 1;
        let stamp = g.clock;
        if let Some(old) = g.map.insert(k.clone(), Entry { frame, bytes, stamp }) {
            g.bytes -= old.bytes;
        }
        g.bytes += bytes;
        if g.bytes > self.budget {
            // Evict oldest entries until under 90% of budget.
            let mut v: Vec<(u64, K, usize)> = g.map.iter().filter(|(ek, _)| **ek != k).map(|(k, e)| (e.stamp, k.clone(), e.bytes)).collect();
            v.sort_unstable_by_key(|x| x.0);
            for (_, k, b) in v {
                if g.bytes <= self.evict_to {
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

    /// Drop one entry (nothing happens when `k` is not cached).
    pub fn remove(&self, k: &K) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = g.map.remove(k) {
            g.bytes -= e.bytes;
        }
    }

    /// Bytes of the frames held.
    pub fn bytes(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).bytes
    }

    pub fn budget(&self) -> usize {
        self.budget
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

    /// The eviction target of a budget near `usize::MAX` must not overflow: on wasm32 `usize` is
    /// 32 bits, and `budget * 9` overflowed there for any budget past about 455 MiB.
    #[test]
    fn eviction_target_of_a_huge_budget_does_not_overflow() {
        let c: FrameCache<u32> = FrameCache::new(usize::MAX);
        assert_eq!(c.evict_to, usize::MAX / 10 * 9);
        assert_eq!(FrameCache::<u32>::new(1 << 30).evict_to, 966_367_638);
    }

    #[test]
    fn keeps_an_entry_larger_than_the_budget_until_the_next_insert() {
        let c: FrameCache<u32> = FrameCache::new(16);
        c.insert(0, Arc::new(VideoFrame::rgba8(4, 4, vec![0; 64])));
        assert!(c.get(&0).is_some(), "the entry just inserted is never evicted");
        assert_eq!(c.bytes(), 64);
        c.insert(1, Arc::new(VideoFrame::rgba8(2, 2, vec![0; 16])));
        assert!(c.get(&0).is_none() && c.get(&1).is_some());
        assert_eq!(c.bytes(), 16);
    }
}
