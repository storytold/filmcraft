//! Audio played ahead: playback mixes on its own thread into a ring buffer that stays
//! [`LEAD_S`] ahead of the device, and the device callback only copies samples out of it.
//!
//! Mixing (decoding AAC, effects, opening a clip's file at a cut) used to run inside the device
//! callback, which has one I/O buffer (~10 ms) to return. On a busy Mac any stall there missed the
//! device's deadline, heard as static and dropouts that were different on every play. Now a stall
//! only drains part of the lead; the callback never decodes, allocates or takes a lock.
//!
//! When the ring does run dry (an underrun), the callback plays silence for the missing frames and
//! counts them. Ring positions name absolute device samples, including silence: late publication
//! cannot replay an expired span. The mixer catches up before its next block, keeping sound locked
//! to the picture without a separate, racy underrun-debt counter.
//!
//! Desktop only: the web build has no threads and keeps mixing in its callback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

/// How far ahead of the device the mixer keeps the ring (s). Edits made while playing are heard
/// this much later.
pub const LEAD_S: f64 = 0.2;
/// Frames mixed per block on the mixing thread.
pub const BLOCK_FRAMES: usize = 1024;
/// Ceiling on mixed channels (large interfaces and aggregate devices stay well under it).
const MAX_CHANNELS: usize = 64;

/// Underrun counters for the current (or last) play, readable from any thread (`perf.stats`).
#[derive(Debug, Default)]
pub struct AudioStats {
    /// Device callbacks served.
    pub callbacks: AtomicU64,
    /// Callbacks the ring could not fill completely.
    pub underruns: AtomicU64,
    /// Frames played as silence because the ring was empty.
    pub missing_frames: AtomicU64,
    /// Fewest frames left in the ring after a callback, once it first filled to the lead
    /// (`u64::MAX` until then).
    pub min_lead_frames: AtomicU64,
}

impl AudioStats {
    pub fn reset(&self) {
        self.callbacks.store(0, Ordering::Relaxed);
        self.underruns.store(0, Ordering::Relaxed);
        self.missing_frames.store(0, Ordering::Relaxed);
        self.min_lead_frames.store(u64::MAX, Ordering::Relaxed);
    }

    pub fn to_json(&self, sample_rate: u32) -> serde_json::Value {
        let min = self.min_lead_frames.load(Ordering::Relaxed);
        serde_json::json!({
            "callbacks": self.callbacks.load(Ordering::Relaxed),
            "underruns": self.underruns.load(Ordering::Relaxed),
            "missingFrames": self.missing_frames.load(Ordering::Relaxed),
            "minLeadMs": if min == u64::MAX || sample_rate == 0 { serde_json::Value::Null } else { serde_json::json!(min as f64 * 1e3 / sample_rate as f64) },
        })
    }
}

/// Single-producer, single-consumer ring of interleaved f32 samples (stored as bits, so it needs
/// no `unsafe`). Indices count absolute device samples since the start and only grow.
/// Invariant: consuming silence expires its sample positions permanently. Publishing an old
/// block must never make those positions audible again (including across ring wraparound).
struct Ring {
    slots: Vec<AtomicU32>,
    write: AtomicU64,
    read: AtomicU64,
}

impl Ring {
    fn new(samples: usize) -> Self {
        Self { slots: (0..samples.max(1)).map(|_| AtomicU32::new(0)).collect(), write: AtomicU64::new(0), read: AtomicU64::new(0) }
    }

    fn len(&self) -> usize {
        self.write.load(Ordering::Acquire).saturating_sub(self.read.load(Ordering::Acquire)) as usize
    }

    /// Producer: skip expired device samples before preparing the next block.
    fn catch_up(&self) -> u64 {
        let w = self.write.load(Ordering::Relaxed);
        let r = self.read.load(Ordering::Acquire);
        if r > w {
            self.write.store(r, Ordering::Release);
        }
        r.saturating_sub(w)
    }

    /// Producer: publish at the position reserved before mixing, even if now expired.
    /// The consumer's absolute position rejects late samples.
    /// Returns how many samples were written.
    fn push(&self, src: &[f32]) -> usize {
        let cap = self.slots.len();
        let w = self.write.load(Ordering::Relaxed);
        let free = cap.saturating_sub(w.saturating_sub(self.read.load(Ordering::Acquire)) as usize);
        let n = src.len().min(free);
        for (i, x) in src.iter().take(n).enumerate() {
            if let Some(s) = self.slots.get(((w + i as u64) % cap as u64) as usize) {
                s.store(x.to_bits(), Ordering::Relaxed);
            }
        }
        self.write.store(w + n as u64, Ordering::Release);
        n
    }

    /// Consumer: fill `dst` (`dev_ch` interleaved channels) with whole frames of `ch` channels from
    /// the ring: the first `min(ch, dev_ch)` channels copied, any further device channels zeroed.
    /// Returns the frames copied; the rest of `dst` is left for the caller.
    fn pop(&self, dst: &mut [f32], dev_ch: usize, ch: usize) -> usize {
        let cap = self.slots.len();
        let r = self.read.load(Ordering::Relaxed);
        let avail = self.write.load(Ordering::Acquire).saturating_sub(r) as usize / ch.max(1);
        let n = (dst.len() / dev_ch.max(1)).min(avail);
        for (f, frame) in dst.chunks_mut(dev_ch.max(1)).take(n).enumerate() {
            for (c, x) in frame.iter_mut().enumerate() {
                *x = if c < ch {
                    self.slots.get(((r + (f * ch + c) as u64) % cap as u64) as usize).map_or(0.0, |s| f32::from_bits(s.load(Ordering::Relaxed)))
                } else {
                    0.0
                };
            }
        }
        // The device advances through silence too. Never rewind it when a late block arrives.
        let requested = (dst.len() / dev_ch.max(1)).saturating_mul(ch);
        self.read.store(r.saturating_add(requested as u64), Ordering::Release);
        n
    }
}

/// Stops the mixing thread when the device callback that owns it is dropped (stream stopped).
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Mixes `mix(cursor, buf, channels)` (interleaved frames from device frame `cursor`) on its own
/// thread, from `start`, and returns the device callback that plays it (`fill(buf, channels)`).
/// The first block is mixed before this returns, so playback starts with sound. The thread runs
/// until the returned callback is dropped.
pub fn spawn<M>(mut mix: M, start: i64, sample_rate: u32, channels: usize, stats: Arc<AudioStats>) -> Box<dyn FnMut(&mut [f32], usize) + Send>
where
    M: FnMut(i64, &mut [f32], usize) + Send + 'static,
{
    stats.reset();
    let ch = channels.clamp(1, MAX_CHANNELS);
    let lead = ((sample_rate as f64 * LEAD_S) as usize).max(BLOCK_FRAMES * 2);
    let ring = Arc::new(Ring::new(lead * ch));
    let mut block = vec![0.0; BLOCK_FRAMES * ch];
    mix(start, &mut block, ch);
    ring.push(&block);
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (ring, stop) = (ring.clone(), stop.clone());
        let body = move || {
            let mut cursor = start.saturating_add(BLOCK_FRAMES as i64);
            while !stop.load(Ordering::Acquire) {
                if ring.len() + block.len() > lead * ch {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                let expired = ring.catch_up() / ch as u64;
                cursor = cursor.saturating_add(i64::try_from(expired).unwrap_or(i64::MAX));
                block.fill(0.0);
                mix(cursor, &mut block, ch);
                cursor = cursor.saturating_add(BLOCK_FRAMES as i64);
                ring.push(&block);
            }
        };
        let spawned = std::thread::Builder::new().name("filmcraft-audio-mix".into()).spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err() {
                log::error!("audio mixing stopped after a panic; playback continues silent");
            }
        });
        if let Err(e) = spawned {
            log::error!("audio mixing thread: {e}");
        }
    }
    let guard = StopOnDrop(stop);
    let mut primed = false;
    Box::new(move |buf: &mut [f32], dev_ch: usize| {
        let _keep = &guard;
        stats.callbacks.fetch_add(1, Ordering::Relaxed);
        // a device wider or narrower than the mix (it changed since `channels` was read) still
        // plays the channels both have
        let dev_ch = dev_ch.max(1);
        let have = ring.pop(buf, dev_ch, ch);
        if let Some(rest) = buf.get_mut(have * dev_ch..) {
            rest.fill(0.0);
        }
        let n = buf.len() / dev_ch;
        if have < n {
            stats.underruns.fetch_add(1, Ordering::Relaxed);
            stats.missing_frames.fetch_add((n - have) as u64, Ordering::Relaxed);
        }
        let left = ring.len();
        primed |= left + 2 * BLOCK_FRAMES * ch > lead * ch;
        if primed {
            stats.min_lead_frames.fetch_min((left / ch) as u64, Ordering::Relaxed);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::Ring;

    #[test]
    fn expired_blocks_recover_at_the_device_position_across_ring_wrap() {
        let ring = Ring::new(8);
        assert_eq!(ring.push(&[1.0, 2.0, 3.0, 4.0]), 4);
        assert_eq!(ring.pop(&mut [0.0; 12], 1, 1), 4);
        assert_eq!(ring.push(&[5.0, 6.0, 7.0, 8.0]), 4);
        assert_eq!(ring.pop(&mut [0.0; 2], 1, 1), 0);
        assert_eq!(ring.catch_up(), 6);
        assert_eq!(ring.push(&[15.0, 16.0, 17.0, 18.0]), 4);
        let mut output = [0.0; 4];
        assert_eq!(ring.pop(&mut output, 1, 1), 4);
        assert_eq!(output, [15.0, 16.0, 17.0, 18.0]);
        assert_eq!(ring.catch_up(), 0);
    }

    #[test]
    fn late_publication_cannot_replay_frames_already_played_as_silence() {
        for channels in [1, 2, 6] {
            for (prime, requested, late, next, expected) in [(4, 6, 4, 2, vec![7.0, 8.0]), (3, 5, 4, 2, vec![6.0, 7.0]), (4, 12, 4, 2, vec![])] {
                let ring = Ring::new(32 * channels);
                let samples = |start: usize, frames: usize| (start..start + frames).flat_map(|f| vec![f as f32; channels]).collect::<Vec<_>>();
                assert_eq!(ring.push(&samples(1, prime)), prime * channels);
                let mut first = vec![0.0; requested * channels];
                assert_eq!(ring.pop(&mut first, channels, channels), prime);
                // The producer prepared this block before the callback consumed silence,
                // but publishes only after that callback completes.
                assert_eq!(ring.push(&samples(prime + 1, late)), late * channels);
                let mut output = vec![0.0; next * channels];
                let copied = ring.pop(&mut output, channels, channels);
                assert_eq!(copied, expected.len());
                for (frame, value) in output.chunks(channels).zip(expected) {
                    assert!(frame.iter().all(|sample| *sample == value), "{frame:?} != {value}");
                }
            }
        }
    }
}
