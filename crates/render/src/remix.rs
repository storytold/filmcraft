//! Playback of remixed clips (Clip ▸ Remix): maps the clip's timeline samples through its remix
//! plan to media samples, crossfading at the joints.
//!
//! A remixed clip carries a hidden effect instance [`EFFECT`] (no effect definition, so the
//! Effects panel, Effect Controls, the audio-effect chain and Paste Attributes ignore it). Its
//! parameters hold the Remix settings and the plan:
//!
//! | param | value |
//! |---|---|
//! | `target` | target duration (ticks, as a float) |
//! | `segments`, `variations` | the Remix sliders (0 … 100) |
//! | `original` | the clip duration before the remix (ticks), restored by Revert Remix |
//! | `plan` | text `xfade;src,len;src,len;…` in ticks: crossfade length, then each piece's media start and length |
//!
//! The plan is computed by the engine (`clip.remix`) with [`filmcraft_audio_dsp::remix`]; playback
//! only reads it, so rendering a remixed clip costs no analysis.

use std::collections::BTreeMap;

use filmcraft_audio_dsp::remix::{self as dsp, Piece};
use filmcraft_frame::AudioBuffer;
use filmcraft_project::{EffectInstance, Param, ParamValue, TrackItem};
use filmcraft_time::Tick;

/// Effect id of the hidden remix state.
pub const EFFECT: &str = "remix";

/// One piece of a remix in ticks: `len` ticks of media from media time `src`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickPiece {
    pub src: Tick,
    pub len: Tick,
}

/// The remix state of a clip.
#[derive(Clone, Debug, PartialEq)]
pub struct Remix {
    pub target: Tick,
    pub segments: f64,
    pub variations: f64,
    /// Clip duration before the remix.
    pub original: Tick,
    /// Crossfade at each joint.
    pub xfade: Tick,
    pub pieces: Vec<TickPiece>,
}

impl Remix {
    /// Output length of the plan.
    pub fn len(&self) -> Tick {
        Tick(self.pieces.iter().map(|p| p.len.0).sum())
    }
    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// The remix of `item`, if it has one.
    pub fn of(item: &TrackItem) -> Option<Remix> {
        let e = item.effects.iter().find(|e| e.effect == EFFECT)?;
        let f = |k: &str| e.param(k).and_then(|p| p.value.as_f64());
        let plan = match e.param("plan").map(|p| &p.value) {
            Some(ParamValue::Text(s)) => dsp::Plan::from_text(s),
            _ => None,
        };
        let (xfade, pieces) = match plan {
            Some(p) => (Tick(p.xfade), p.pieces.iter().map(|p| TickPiece { src: Tick(p.src), len: Tick(p.len) }).collect()),
            None => (Tick::ZERO, Vec::new()),
        };
        Some(Remix {
            target: Tick(f("target").unwrap_or(0.0) as i64),
            segments: f("segments").unwrap_or(50.0),
            variations: f("variations").unwrap_or(50.0),
            original: Tick(f("original").unwrap_or(item.duration.0 as f64) as i64),
            xfade,
            pieces,
        })
    }

    /// The hidden effect instance that stores this remix.
    pub fn to_effect(&self) -> EffectInstance {
        let plan = dsp::Plan { pieces: self.pieces.iter().map(|p| Piece { src: p.src.0, len: p.len.0 }).collect(), xfade: self.xfade.0 };
        let mut params = BTreeMap::new();
        params.insert("target".to_string(), Param::new(ParamValue::Float(self.target.0 as f64)));
        params.insert("segments".to_string(), Param::new(ParamValue::Float(self.segments)));
        params.insert("variations".to_string(), Param::new(ParamValue::Float(self.variations)));
        params.insert("original".to_string(), Param::new(ParamValue::Float(self.original.0 as f64)));
        params.insert("plan".to_string(), Param::new(ParamValue::Text(if self.pieces.is_empty() { String::new() } else { plan.to_text() })));
        EffectInstance { effect: EFFECT.to_string(), enabled: true, params, masks: Vec::new(), post_fader: false, essential: false, layer: None }
    }

    /// Store this remix on `item` (replacing an earlier one).
    pub fn store(&self, item: &mut TrackItem) {
        let e = self.to_effect();
        match item.effects.iter_mut().find(|x| x.effect == EFFECT) {
            Some(x) => *x = e,
            None => item.effects.push(e),
        }
    }

    /// The plan in samples at `sr` (piece boundaries are floored on the timeline, so consecutive
    /// pieces tile the clip exactly at every rate).
    pub fn plan_at(&self, sr: u32) -> dsp::Plan {
        let r = sr as i64;
        let mut pieces = Vec::with_capacity(self.pieces.len());
        let mut d = Tick::ZERO;
        for p in &self.pieces {
            let a = d.to_units_floor(r);
            d += p.len;
            pieces.push(Piece { src: p.src.to_units_floor(r), len: d.to_units_floor(r) - a });
        }
        dsp::Plan { pieces, xfade: self.xfade.to_units_floor(r) }
    }
}

/// Remove the remix state from `item`; returns it.
pub fn take(item: &mut TrackItem) -> Option<Remix> {
    let r = Remix::of(item)?;
    item.effects.retain(|e| e.effect != EFFECT);
    Some(r)
}

/// Whether `item` is remixed (has a plan that playback follows).
pub fn is_remixed(item: &TrackItem) -> bool {
    item.effects.iter().any(|e| e.effect == EFFECT && e.enabled)
}

/// Audio of a remixed clip for clip-relative timeline samples `[rel0, rel0 + n)` at rate `sr`,
/// reading the media through `read(media_sample, len)`. `None` when the clip is not remixed (the
/// caller then reads the media linearly). Speed and reverse do not apply to remixed clips.
pub fn read(item: &TrackItem, read: &dyn Fn(i64, usize) -> Option<AudioBuffer>, rel0: i64, n: usize, sr: u32) -> Option<AudioBuffer> {
    if !is_remixed(item) {
        return None;
    }
    let rm = Remix::of(item)?;
    if rm.pieces.is_empty() {
        return None;
    }
    let plan = rm.plan_at(sr);
    let channels = read(plan.pieces[0].src, 1).map(|b| b.channel_count()).unwrap_or(2).max(1);
    let mut rd = |s: i64, len: usize| -> Vec<Vec<f32>> {
        let mut b = read(s, len).map(|b| b.channels).unwrap_or_default();
        b.resize(channels, Vec::new());
        for c in b.iter_mut() {
            c.resize(len, 0.0);
        }
        b
    };
    let out = dsp::render(&plan, rel0, n, channels, &mut rd);
    Some(AudioBuffer { sample_rate: sr, channels: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> TrackItem {
        let d = Tick::from_seconds_f64(1.0).0;
        serde_json::from_value(serde_json::json!({
            "id": 1, "item": 2, "name": "m", "label": "Violet", "start": 0, "duration": d, "source_in": 0,
            "speed": 1.0, "enabled": true, "effects": []
        }))
        .expect("track item")
    }

    fn source(m: i64, len: usize) -> Option<AudioBuffer> {
        // a ramp: sample value = media sample index / 1e5 (silence outside 0 … 48 000)
        let ch: Vec<f32> = (0..len as i64).map(|k| if (0..48_000).contains(&(m + k)) { (m + k) as f32 / 1e5 } else { 0.0 }).collect();
        Some(AudioBuffer { sample_rate: 48_000, channels: vec![ch.clone(), ch] })
    }

    fn remix() -> Remix {
        let s = |x: f64| Tick::from_seconds_f64(x);
        Remix {
            target: s(0.6),
            segments: 50.0,
            variations: 50.0,
            original: s(1.0),
            xfade: s(0.002),
            pieces: vec![TickPiece { src: s(0.0), len: s(0.25) }, TickPiece { src: s(0.65), len: s(0.35) }],
        }
    }

    #[test]
    fn not_remixed_reads_nothing() {
        assert!(read(&item(), &source, 0, 10, 48_000).is_none());
    }

    #[test]
    fn state_roundtrips_through_the_hidden_effect() {
        let mut it = item();
        let r = remix();
        r.store(&mut it);
        r.store(&mut it);
        assert_eq!(it.effects.len(), 1);
        assert_eq!(Remix::of(&it), Some(r.clone()));
        assert!(it.effects[0].def().is_none(), "no definition: hidden from panels and the fx chain");
        assert_eq!(take(&mut it), Some(r));
        assert!(it.effects.is_empty());
    }

    #[test]
    fn plays_the_pieces_and_ignores_request_cuts() {
        let mut it = item();
        remix().store(&mut it);
        let total = (0.6 * 48_000.0) as usize;
        let whole = read(&it, &source, 0, total, 48_000).unwrap();
        // away from the joint (12 000) the output is the media ramp of each piece
        assert_eq!(whole.channels[0][100], 100.0 / 1e5);
        assert_eq!(whole.channels[0][11_000], 11_000.0 / 1e5);
        assert_eq!(whole.channels[0][13_000], (31_200 + 1_000) as f32 / 1e5);
        assert_eq!(whole.channels[1][20_000], (31_200 + 8_000) as f32 / 1e5);
        let mut cut: Vec<f32> = Vec::new();
        let mut pos = 0usize;
        let mut step = 3usize;
        while pos < total {
            let n = step.min(total - pos);
            cut.extend(read(&it, &source, pos as i64, n, 48_000).unwrap().channels[0].iter());
            pos += n;
            step = step * 5 % 1999 + 1;
        }
        assert_eq!(cut, whole.channels[0]);
        // other rates tile the same plan
        let p = remix().plan_at(44_100);
        assert_eq!(p.len(), (0.6 * 44_100.0) as i64);
    }
}
