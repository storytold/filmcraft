//! Speed/Duration ▸ Maintain Audio Pitch: a speed-changed clip's audio is time-stretched with
//! WSOLA ([`Wsola`]) instead of resampled (varispeed), so it keeps its pitch.
//!
//! Output hops lie on a grid of timeline samples (hop `k` starts at sample `k * hop`). Grain `k` is
//! read near the source sample the clip shows at the start of hop `k`, aligned to grain `k - 1`, so
//! a chain read in order gives the same samples however the reads are cut. The last positions of
//! each chain are kept, as clip effect chains are ([`crate::audio_fx`]): playback and export carry
//! on with them, and a read anywhere else (a seek) starts a new chain a few grains early.
//!
//! Positions count along the clip's playing direction: `q = s` forwards and `q = -s` reversed,
//! where `s` is the source sample (at the sequence rate). Grains always read forwards in `q`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use filmcraft_audio_dsp::stretch::Wsola;
use filmcraft_frame::AudioBuffer;
use filmcraft_project::TrackItem;
use filmcraft_time::Tick;

use crate::audio::SourceAudio;

/// Grains a new chain starts before the first hop it plays.
const WARMUP: i64 = 4;
/// Longest source span (samples) one read takes (a very fast clip read in one long request); a
/// longer one falls back to varispeed.
const MAX_SPAN: i64 = 1 << 24;

/// Clip id, what the grain positions depend on, sample rate.
type Key = (u64, u64, u32);

/// Clips whose chains are kept. Past this many, the clip read longest ago loses its chain (its next
/// read starts a new one); the chains of the clips playing stay.
const MAX_CHAINS: usize = 256;

/// Known grain positions of the chains read so far, per clip: `(grain, q)`, with when each clip
/// was last read.
#[derive(Default)]
struct Chains {
    clock: u64,
    map: HashMap<Key, (u64, Vec<(i64, i64)>)>,
}

impl Chains {
    /// The kept position of grain `g` of `key`'s chain; marks the clip as just read.
    fn position(&mut self, key: Key, g: i64) -> Option<i64> {
        self.clock += 1;
        let (used, v) = self.map.get_mut(&key)?;
        *used = self.clock;
        v.iter().find(|(k, _)| *k == g).map(|(_, q)| *q)
    }

    /// `key`'s kept positions, making room for it first when it is new and the cache is full.
    fn positions(&mut self, key: Key) -> &mut Vec<(i64, i64)> {
        self.clock += 1;
        if !self.map.contains_key(&key)
            && self.map.len() >= MAX_CHAINS
            && let Some(oldest) = self.map.iter().min_by_key(|(_, (used, _))| *used).map(|(k, _)| *k)
        {
            self.map.remove(&oldest);
        }
        let (used, v) = self.map.entry(key).or_default();
        *used = self.clock;
        v
    }
}

fn cache() -> &'static Mutex<Chains> {
    static C: OnceLock<Mutex<Chains>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn key(item: &TrackItem, sr: u32) -> Key {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (item.item, item.start, item.duration, item.source_in, item.speed.to_bits(), item.reverse, item.audio_stream).hash(&mut h);
    (item.id.0, h.finish(), sr)
}

/// Whether the clip's audio is time-stretched rather than resampled: Maintain Audio Pitch is on
/// and the clip plays at another speed (`speed` = its absolute speed). A frame hold has no speed.
pub(crate) fn applies(item: &TrackItem, speed: f64) -> bool {
    item.maintain_pitch && item.frame_hold.is_none() && speed.is_finite() && (speed - 1.0).abs() >= 1e-9
}

/// The clip's source audio for timeline samples `[a0, a0 + n)` (inside the clip), time-stretched
/// to the clip's speed with its pitch kept, in the source's channels. `None` when the source has
/// no audio there or the span is too long to read at once (the caller then resamples).
pub(crate) fn read(item: &TrackItem, src: SourceAudio, a0: i64, n: usize, sr: u32) -> Option<AudioBuffer> {
    let w = Wsola::new(sr);
    let (h, tol) = (w.hop() as i64, w.tolerance() as i64);
    let last = a0.checked_add(i64::try_from(n).ok()?)?.checked_sub(1)?;
    if last < a0 {
        return None;
    }
    let (k0, k1) = (a0.div_euclid(h), last.div_euclid(h));
    let rate = i64::from(sr.max(1));
    let dir = if item.reverse { -1 } else { 1 };
    // q of the source sample the clip shows where hop `k` starts
    let nominal = |k: i64| item.source_time_at(Tick::from_units(k.saturating_mul(h), rate)).to_units_floor(rate).saturating_mul(dir);
    let key = key(item, sr);
    let known = cache().lock().unwrap_or_else(|e| e.into_inner()).position(key, k0 - 1);
    // `before`: the position of grain `first - 1`, which the chain continues from
    let (first, before) = match known {
        Some(q) => (k0, q),
        None => (k0 - WARMUP, nominal(k0 - WARMUP - 1)),
    };
    // every q a grain from `first - 1` to `k1` reads: positions stay within the tolerance of
    // their nominal ones, and a grain is two hops long
    let lo = before.min(nominal(first).saturating_sub(tol));
    let hi = before.max(nominal(k1).saturating_add(tol)).saturating_add(2 * h);
    let span = hi.checked_sub(lo).filter(|s| (1..=MAX_SPAN).contains(s))?;
    let len = usize::try_from(span).ok()?;
    let mut input = if dir > 0 { src(lo, len)? } else { src(1 - hi, len)? }.channels;
    if input.is_empty() {
        return None;
    }
    for c in &mut input {
        c.resize(len, 0.0);
        if dir < 0 {
            c.reverse();
        }
    }
    let mut guide = vec![0.0f32; len];
    for c in &input {
        for (g, x) in guide.iter_mut().zip(c) {
            *g += *x;
        }
    }
    // pos[i]: grain `first - 1 + i`
    let mut pos = Vec::with_capacity(usize::try_from(k1 - first + 2).unwrap_or(0));
    pos.push(before);
    for k in first..=k1 {
        let prev = pos.last().copied().unwrap_or(before);
        pos.push(lo + w.align(&guide, prev + h - lo, nominal(k) - lo));
    }
    let at = |k: i64| usize::try_from(k - first + 1).ok().and_then(|i| pos.get(i)).copied();
    let mut out = AudioBuffer::silence(sr, input.len(), n);
    for k in k0..=k1 {
        let (Some(prev), Some(cur)) = (at(k - 1), at(k)) else { continue };
        let start = k * h;
        for x in start.max(a0)..(start + h).min(a0 + n as i64) {
            let t = (x - start) as usize;
            let r = w.rise(t);
            let (a, b) = ((prev + h + t as i64 - lo) as usize, (cur + t as i64 - lo) as usize);
            for (o, c) in out.channels.iter_mut().zip(&input) {
                let s = c.get(a).copied().unwrap_or(0.0) * (1.0 - r) + c.get(b).copied().unwrap_or(0.0) * r;
                if let Some(o) = o.get_mut((x - a0) as usize) {
                    *o = s;
                }
            }
        }
    }
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    let kept = c.positions(key);
    for k in [k1 - 1, k1] {
        if let Some(q) = at(k) {
            kept.retain(|(g, _)| *g != k);
            kept.push((k, q));
        }
    }
    // the last two positions of up to four chains (playback, export, a scrub…)
    let extra = kept.len().saturating_sub(8);
    kept.drain(..extra);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full cache drops the clip read longest ago, not every chain at once: a chain dropped in the
    /// middle of playback restarts a few grains early and its samples change there (a click).
    #[test]
    fn a_full_cache_drops_only_the_clip_read_longest_ago() {
        let mut c = Chains::default();
        let key = |i: u64| (i, 0, 48_000);
        for i in 0..MAX_CHAINS as u64 {
            c.positions(key(i)).push((0, i as i64));
        }
        // clip 0 plays on: it is read again before the cache overflows
        assert_eq!(c.position(key(0), 0), Some(0));
        c.positions(key(MAX_CHAINS as u64)).push((0, 7));
        assert_eq!(c.map.len(), MAX_CHAINS);
        assert_eq!(c.position(key(0), 0), Some(0), "the clip still playing keeps its chain");
        assert_eq!(c.position(key(1), 0), None, "the clip read longest ago lost its chain");
        assert_eq!(c.position(key(MAX_CHAINS as u64), 0), Some(7));
    }
}
