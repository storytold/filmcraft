//! Speed/Duration ▸ Maintain Audio Pitch: a speed-changed clip's audio is time-stretched with
//! WSOLA ([`Wsola`]) instead of resampled (varispeed), so it keeps its pitch.
//!
//! Output hops lie on a grid of timeline samples (hop `k` starts at sample `k * hop`). Grain `k` is
//! read near the source sample the clip shows at the start of hop `k`, aligned to grain `k - 1`, so
//! a chain read in order gives the same samples however the reads are cut.
//!
//! Each reader keeps its own chain, the way clip effect chains are kept ([`crate::audio_fx`]): a
//! read continues the chain whose last read ended exactly where it starts, and takes that chain out
//! of the cache while it runs. Playback, Render Audio, a loudness pass or a scrub reading the same
//! clip at the same time therefore never continue each other's chains, and a read that continues
//! no chain (a seek) starts a fresh one a few grains early.
//!
//! Grains only read the clip's own source span, from its in point to its out point in playing
//! order; source outside it reads as silence, so no audio from the handles plays (varispeed never
//! plays it either). Grains are aligned on the channels the clip plays (its `source_channels` pick,
//! see [`crate::audio::played_channels`]), and only those channels are stretched.
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

/// Clips whose chains are kept. Past this many, the clip read longest ago loses its chains (its
/// next reads start new ones); the chains of the clips playing stay.
const MAX_CHAINS: usize = 256;
/// Chains kept per clip, one per reader (playback, export, a loudness pass, scrubs, a nest used
/// several times). Past this many, the clip's chain read longest ago goes.
const MAX_READERS: usize = 8;

/// Where one reader's chain stands after its last read.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Chain {
    /// The timeline sample a read starts at to continue this chain (where the last one ended).
    next_a0: i64,
    /// `(grain, q)` of the chain's last two grains.
    last: [(i64, i64); 2],
}

impl Chain {
    /// The position of grain `g`, when it is one of the chain's last two.
    fn position(&self, g: i64) -> Option<i64> {
        self.last.iter().find(|(k, _)| *k == g).map(|(_, q)| *q)
    }
}

/// The readers' chains, per clip, with when each clip was last read.
#[derive(Default)]
struct Chains {
    clock: u64,
    map: HashMap<Key, (u64, Vec<Chain>)>,
}

impl Chains {
    /// Takes out `key`'s chain that a read starting at timeline sample `a0` continues (the one whose
    /// last read ended at `a0`), and marks the clip as just read. The chain is back only when its
    /// read [`put`](Self::put)s it, so no other reader continues it meanwhile.
    fn take(&mut self, key: Key, a0: i64) -> Option<Chain> {
        self.clock += 1;
        let (used, v) = self.map.get_mut(&key)?;
        *used = self.clock;
        let i = v.iter().position(|c| c.next_a0 == a0)?;
        Some(v.remove(i))
    }

    /// Keeps `chain` for `key`, making room for the clip first when it is new and the cache is full.
    fn put(&mut self, key: Key, chain: Chain) {
        self.clock += 1;
        if !self.map.contains_key(&key)
            && self.map.len() >= MAX_CHAINS
            && let Some(oldest) = self.map.iter().min_by_key(|(_, (used, _))| *used).map(|(k, _)| *k)
        {
            self.map.remove(&oldest);
        }
        let (used, v) = self.map.entry(key).or_default();
        *used = self.clock;
        // two readers that ended at the same sample: the next read there continues the newer one
        v.retain(|c| c.next_a0 != chain.next_a0);
        v.push(chain);
        if v.len() > MAX_READERS {
            v.remove(0);
        }
    }
}

fn cache() -> &'static Mutex<Chains> {
    static C: OnceLock<Mutex<Chains>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn key(item: &TrackItem, sr: u32, w: usize) -> Key {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (item.item, item.start, item.duration, item.source_in, item.speed.to_bits(), item.reverse, item.audio_stream).hash(&mut h);
    (&item.source_channels, w).hash(&mut h);
    (item.id.0, h.finish(), sr)
}

/// Whether the clip's audio is time-stretched rather than resampled: Maintain Audio Pitch is on
/// and the clip plays at another speed (`speed` = its absolute speed). A frame hold has no speed.
pub(crate) fn applies(item: &TrackItem, speed: f64) -> bool {
    item.maintain_pitch && item.frame_hold.is_none() && speed.is_finite() && (speed - 1.0).abs() >= 1e-9
}

/// The clip's source audio for timeline samples `[a0, a0 + n)` (inside the clip), time-stretched
/// to the clip's speed with its pitch kept, in the source's channels; the channels the clip does
/// not play on a track of `w` channels are left silent. `None` when the source has no audio there
/// or the span is too long to read at once (the caller then resamples).
pub(crate) fn read(item: &TrackItem, src: SourceAudio, a0: i64, n: usize, sr: u32, w: usize) -> Option<AudioBuffer> {
    let ws = Wsola::new(sr);
    let (h, tol) = (ws.hop() as i64, ws.tolerance() as i64);
    let last = a0.checked_add(i64::try_from(n).ok()?)?.checked_sub(1)?;
    if last < a0 {
        return None;
    }
    let (k0, k1) = (a0.div_euclid(h), last.div_euclid(h));
    let rate = i64::from(sr.max(1));
    let dir = if item.reverse { -1 } else { 1 };
    // q of the source sample the clip shows at timeline sample `x`, and where hop `k` starts
    let q_at = |x: i64| item.source_time_at(Tick::from_units(x, rate)).to_units_floor(rate).saturating_mul(dir);
    let nominal = |k: i64| q_at(k.saturating_mul(h));
    // the clip's own source in playing order, `[q_in, q_out)`: grains read silence outside it
    let (q_in, q_out) = (q_at(item.start.to_units_floor(rate)), q_at(item.end().to_units_floor(rate)));
    let key = key(item, sr, w);
    // this reader's chain: the one its previous read left off at `a0`
    let taken = cache().lock().unwrap_or_else(|e| e.into_inner()).take(key, a0);
    // `before`: the position of grain `first - 1`, which the chain continues from
    let (first, before) = match taken.and_then(|c| c.position(k0 - 1)) {
        Some(q) => (k0, q),
        None => (k0 - WARMUP, nominal(k0 - WARMUP - 1)),
    };
    // every q a grain from `first - 1` to `k1` reads: positions stay within the tolerance of
    // their nominal ones, and a grain is two hops long
    let lo = before.min(nominal(first).saturating_sub(tol));
    let hi = before.max(nominal(k1).saturating_add(tol)).saturating_add(2 * h);
    let span = hi.checked_sub(lo).filter(|s| (1..=MAX_SPAN).contains(s))?;
    let len = usize::try_from(span).ok()?;
    // only the part inside the clip's source is read; the rest stays silent
    let (r0, r1) = (lo.max(q_in), hi.min(q_out));
    let rlen = usize::try_from(r1.checked_sub(r0)?).ok().filter(|l| *l > 0)?;
    let skip = usize::try_from(r0.checked_sub(lo)?).ok()?;
    let raw = if dir > 0 { src(r0, rlen)? } else { src(1 - r1, rlen)? }.channels;
    if raw.is_empty() {
        return None;
    }
    let played = crate::audio::played_channels(item, raw.len(), w);
    // channels the clip doesn't play stay empty: not stretched, and they don't steer the grains
    let input: Vec<Vec<f32>> = raw
        .into_iter()
        .enumerate()
        .map(|(c, mut ch)| {
            if !played.contains(&c) {
                return Vec::new();
            }
            ch.resize(rlen, 0.0);
            if dir < 0 {
                ch.reverse();
            }
            let mut full = vec![0.0f32; len];
            if let Some(dst) = full.get_mut(skip..skip.saturating_add(rlen)) {
                dst.copy_from_slice(&ch);
            }
            full
        })
        .collect();
    let mut guide = vec![0.0f32; len];
    for c in input.iter().filter(|c| !c.is_empty()) {
        for (g, x) in guide.iter_mut().zip(c) {
            *g += *x;
        }
    }
    // pos[i]: grain `first - 1 + i`
    let mut pos = Vec::with_capacity(usize::try_from(k1 - first + 2).unwrap_or(0));
    pos.push(before);
    for k in first..=k1 {
        let prev = pos.last().copied().unwrap_or(before);
        pos.push(lo + ws.align(&guide, prev + h - lo, nominal(k) - lo));
    }
    let at = |k: i64| usize::try_from(k - first + 1).ok().and_then(|i| pos.get(i)).copied();
    let mut out = AudioBuffer::silence(sr, input.len(), n);
    for (o, c) in out.channels.iter_mut().zip(&input) {
        if c.is_empty() {
            continue;
        }
        for k in k0..=k1 {
            let (Some(prev), Some(cur)) = (at(k - 1), at(k)) else { continue };
            let start = k * h;
            for x in start.max(a0)..(start + h).min(a0 + n as i64) {
                let t = (x - start) as usize;
                let r = ws.rise(t);
                let (a, b) = ((prev + h + t as i64 - lo) as usize, (cur + t as i64 - lo) as usize);
                let s = c.get(a).copied().unwrap_or(0.0) * (1.0 - r) + c.get(b).copied().unwrap_or(0.0) * r;
                if let Some(o) = o.get_mut((x - a0) as usize) {
                    *o = s;
                }
            }
        }
    }
    if let (Some(q0), Some(q1)) = (at(k1 - 1), at(k1)) {
        let chain = Chain { next_a0: last + 1, last: [(k1 - 1, q0), (k1, q1)] };
        cache().lock().unwrap_or_else(|e| e.into_inner()).put(key, chain);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(next_a0: i64, q: i64) -> Chain {
        Chain { next_a0, last: [(0, q), (1, q + 1)] }
    }

    /// A full cache drops the clip read longest ago, not every chain at once: a chain dropped in the
    /// middle of playback restarts a few grains early and its samples change there (a click).
    #[test]
    fn a_full_cache_drops_only_the_clip_read_longest_ago() {
        let mut c = Chains::default();
        let key = |i: u64| (i, 0, 48_000);
        for i in 0..MAX_CHAINS as u64 {
            c.put(key(i), chain(10, i as i64));
        }
        // clip 0 plays on: it is read again before the cache overflows
        assert_eq!(c.take(key(0), 10), Some(chain(10, 0)));
        c.put(key(0), chain(20, 0));
        c.put(key(MAX_CHAINS as u64), chain(10, 7));
        assert_eq!(c.map.len(), MAX_CHAINS);
        assert_eq!(c.take(key(0), 20), Some(chain(20, 0)), "the clip still playing keeps its chain");
        assert_eq!(c.take(key(1), 10), None, "the clip read longest ago lost its chain");
        assert_eq!(c.take(key(MAX_CHAINS as u64), 10), Some(chain(10, 7)));
    }

    /// A read continues only the chain whose last read ended where it starts, and holds it while it
    /// runs: another reader starting there meanwhile starts its own chain.
    #[test]
    fn a_read_continues_only_its_own_chain() {
        let mut c = Chains::default();
        let k = (1, 0, 48_000);
        c.put(k, chain(1024, 100));
        c.put(k, chain(5000, 900));
        assert_eq!(c.take(k, 2048), None, "no reader left off there");
        assert_eq!(c.take(k, 1024), Some(chain(1024, 100)));
        assert_eq!(c.take(k, 1024), None, "the chain is taken while its read runs");
        assert_eq!(c.take(k, 5000), Some(chain(5000, 900)));
        // past MAX_READERS chains, the oldest goes
        for i in 0..=MAX_READERS as i64 {
            c.put(k, chain(i, i));
        }
        assert_eq!(c.take(k, 0), None);
        assert_eq!(c.take(k, MAX_READERS as i64), Some(chain(MAX_READERS as i64, MAX_READERS as i64)));
        assert_eq!(chain(7, 3).position(1), Some(4));
        assert_eq!(chain(7, 3).position(2), None);
    }
}
