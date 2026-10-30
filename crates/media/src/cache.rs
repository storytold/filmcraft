//! A byte-budgeted LRU cache of decoded/rendered frames, shared by monitors, thumbnails and export,
//! and of decoded audio PCM, shared by every standalone audio source.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex};

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
    in_flight: HashSet<K>,
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
    cvar: Condvar,
    budget: usize,
}

struct InFlightGuard<'a, K: Eq + Hash + Clone> {
    cache: &'a AudioCache<K>,
    key: K,
    active: bool,
}

impl<'a, K: Eq + Hash + Clone> Drop for InFlightGuard<'a, K> {
    fn drop(&mut self) {
        if self.active {
            let mut g = self.cache.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.in_flight.remove(&self.key);
            self.cache.cvar.notify_all();
        }
    }
}

impl<K: Eq + Hash + Clone> AudioCache<K> {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(AudioInner { map: HashMap::new(), in_flight: HashSet::new(), bytes: 0, clock: 0, hits: 0, misses: 0 }),
            cvar: Condvar::new(),
            budget: budget_bytes,
        }
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

    fn insert_locked(&self, g: &mut AudioInner<K>, k: K, samples: Arc<Vec<Vec<f32>>>) {
        let bytes = samples.iter().map(Vec::len).sum::<usize>().saturating_mul(core::mem::size_of::<f32>());
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

    pub fn insert(&self, k: K, samples: Arc<Vec<Vec<f32>>>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.in_flight.remove(&k);
        self.insert_locked(&mut g, k, samples);
        self.cvar.notify_all();
    }

    pub fn get_or_insert_with<E>(&self, k: K, f: impl FnOnce() -> Result<Arc<Vec<Vec<f32>>>, E>) -> Result<Arc<Vec<Vec<f32>>>, E> {
        loop {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            g.clock += 1;
            let c = g.clock;
            if let Some(e) = g.map.get_mut(&k) {
                e.stamp = c;
                let s = e.samples.clone();
                g.hits += 1;
                return Ok(s);
            }
            if g.in_flight.contains(&k) {
                g = self.cvar.wait(g).unwrap_or_else(|e| e.into_inner());
                continue;
            }
            g.misses += 1;
            g.in_flight.insert(k.clone());
            break;
        }

        let mut guard = InFlightGuard { cache: self, key: k.clone(), active: true };
        let samples = f()?;
        guard.active = false;

        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.in_flight.remove(&k);
        self.insert_locked(&mut g, k, samples.clone());
        self.cvar.notify_all();
        Ok(samples)
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.map.clear();
        g.in_flight.clear();
        g.bytes = 0;
        self.cvar.notify_all();
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
        g.in_flight.remove(k);
        self.cvar.notify_all();
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

    #[test]
    fn audio_get_or_insert_coalesces_concurrent_misses() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;

        let c = Arc::new(AudioCache::<u64>::new(1 << 20));
        let decode_count = Arc::new(AtomicUsize::new(0));
        let num_threads = 8;
        let barrier = Arc::new(Barrier::new(num_threads));

        let mut handles = Vec::new();
        for _ in 0..num_threads {
            let c = Arc::clone(&c);
            let decode_count = Arc::clone(&decode_count);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                c.get_or_insert_with(42, || {
                    decode_count.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(std::time::Duration::from_millis(20));
                    Ok::<_, ()>(pcm(16))
                })
                .unwrap()
            }));
        }

        let results: Vec<Arc<Vec<Vec<f32>>>> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // Exactly one thread decoded
        assert_eq!(decode_count.load(Ordering::SeqCst), 1);
        // All threads received the identical Arc pointer
        for res in &results[1..] {
            assert!(Arc::ptr_eq(&results[0], res));
        }

        // Stats: 1 miss, 7 hits
        let (entries, _bytes, hits, misses) = c.stats();
        assert_eq!(entries, 1);
        assert_eq!(misses, 1);
        assert_eq!(hits, (num_threads - 1) as u64);
    }

    #[test]
    fn audio_get_or_insert_failure_retries_on_waiters() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;

        #[allow(dead_code)]
        #[derive(Debug)]
        struct NonCloneError(usize);

        let c = Arc::new(AudioCache::<u64>::new(1 << 20));
        let attempts = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));

        let mut handles = Vec::new();

        // Thread 1 fails
        {
            let c = Arc::clone(&c);
            let attempts = Arc::clone(&attempts);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                c.get_or_insert_with(99, || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    thread::sleep(std::time::Duration::from_millis(20));
                    Err::<Arc<Vec<Vec<f32>>>, _>(NonCloneError(1))
                })
            }));
        }

        // Thread 2 waits, then retries and succeeds
        {
            let c = Arc::clone(&c);
            let attempts = Arc::clone(&attempts);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                // brief delay so thread 1 becomes in-flight leader
                thread::sleep(std::time::Duration::from_millis(5));
                c.get_or_insert_with(99, || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, NonCloneError>(pcm(16))
                })
            }));
        }

        let res1 = handles.remove(0).join().unwrap();
        let res2 = handles.remove(0).join().unwrap();

        assert!(res1.is_err());
        assert!(res2.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }
}
