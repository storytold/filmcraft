//! Audio Track Mixer model: automation modes and lanes, sends, submix routing, channel mapping.
//!
//! Every audio track, every submix track ([`crate::Sequence::submix_tracks`]) and the Mix (master)
//! track has a [`MixerStrip`]. Static values stay where they always were (`Track::volume_db`,
//! `Track::pan`, `Track::muted`, `TrackSend::level_db`, effect parameter values); automation is
//! keyframes in **sequence time** on top of them:
//!
//! | lane key | value | stored in |
//! |---|---|---|
//! | `volume` | fader level, dB | `MixerStrip::lanes` |
//! | `pan` | pan / balance, −100 … 100 | `MixerStrip::lanes` |
//! | `mute` | 0 / 1 (hold) | `MixerStrip::lanes` |
//! | `send.<i>.level` | send `i` level, dB | `MixerStrip::lanes` |
//! | `pan51.x`, `pan51.y` | 5.1 panner puck, −100 (left / rear) … 100 (right / front) | `MixerStrip::lanes` (static value = the lane's value) |
//! | `pan51.center` | 5.1 panner Center %, 0 … 100 | `MixerStrip::lanes` |
//! | `pan51.lfe` | 5.1 panner LFE level, dB | `MixerStrip::lanes` |
//! | `fx.<slot>.<param>` | insert effect parameter | the effect's own `Param` keyframes |
//!
//! [`AutomationMode`] decides whether playback reads the lanes (anything but Off) and how the
//! mixer writes them while recording (Latch / Touch / Write; see the engine's recorder).

use std::collections::BTreeMap;

use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

use crate::keyframe::{Interpolation, Keyframe, Param, ParamValue};
use crate::{Track, TrackId};

pub const LANE_VOLUME: &str = "volume";
pub const LANE_PAN: &str = "pan";
pub const LANE_MUTE: &str = "mute";
/// 5.1 panner puck, left (−100) … right (100). Used when the strip feeds a 5.1 submix or Mix.
pub const LANE_PAN51_X: &str = "pan51.x";
/// 5.1 panner puck, rear (−100) … front (100).
pub const LANE_PAN51_Y: &str = "pan51.y";
/// 5.1 panner Center % (0 … 100): the centre speaker's share of the front image.
pub const LANE_PAN51_CENTER: &str = "pan51.center";
/// 5.1 panner LFE level (dB).
pub const LANE_PAN51_LFE: &str = "pan51.lfe";
/// The 5.1 panner lanes.
pub const PAN51_LANES: [&str; 4] = [LANE_PAN51_X, LANE_PAN51_Y, LANE_PAN51_CENTER, LANE_PAN51_LFE];

/// Default value of a 5.1 panner lane for a strip of channel format `channels`: puck at front
/// centre, Center 100 %, LFE 0 dB for 5.1 strips (their LFE channel passes) and −∞ for mono /
/// stereo strips (nothing is sent to the subwoofer unless asked for).
pub fn pan51_default(key: &str, channels: crate::AudioChannels) -> Option<f64> {
    match key {
        LANE_PAN51_X => Some(0.0),
        LANE_PAN51_Y => Some(100.0),
        LANE_PAN51_CENTER => Some(100.0),
        LANE_PAN51_LFE => Some(if channels == crate::AudioChannels::Surround51 { 0.0 } else { FADER_MIN_DB }),
        _ => None,
    }
}

/// At most this many insert effects per strip (Premiere's effect slots 1–5).
pub const MAX_INSERTS: usize = 5;
/// At most this many sends per strip (Premiere's send slots 1–5).
pub const MAX_SENDS: usize = 5;
/// Fader range (dB): the Track Mixer's scale runs from −∞ to +15 dB (Clip Mixer and Mix too).
pub const FADER_MAX_DB: f64 = 15.0;
/// Values at or below this are −∞ (silence).
pub const FADER_MIN_DB: f64 = -96.0;

pub fn send_lane(i: usize) -> String {
    format!("send.{i}.level")
}

pub fn fx_lane(slot: usize, param: &str) -> String {
    format!("fx.{slot}.{param}")
}

/// Track automation mode (the dropdown above the M/S/R buttons).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AutomationMode {
    /// Ignore stored automation; play the static values.
    Off,
    /// Play automation; touching a control during playback does not record.
    #[default]
    Read,
    /// Record from the first touch; after release keep writing the last value until playback stops.
    Latch,
    /// Record while touched; after release return to the previous automation over the automatch time.
    Touch,
    /// Record every automatable control from the start of playback until it stops.
    Write,
}

impl AutomationMode {
    pub const ALL: [AutomationMode; 5] = [AutomationMode::Off, AutomationMode::Read, AutomationMode::Latch, AutomationMode::Touch, AutomationMode::Write];
    pub fn label(self) -> &'static str {
        match self {
            AutomationMode::Off => "Off",
            AutomationMode::Read => "Read",
            AutomationMode::Latch => "Latch",
            AutomationMode::Touch => "Touch",
            AutomationMode::Write => "Write",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.label().eq_ignore_ascii_case(s))
    }
    /// Plays stored automation.
    pub fn reads(self) -> bool {
        self != AutomationMode::Off
    }
    /// Records automation during playback.
    pub fn writes(self) -> bool {
        matches!(self, AutomationMode::Latch | AutomationMode::Touch | AutomationMode::Write)
    }
}

/// How the clips' (stereo) audio feeds the track input: basic channel mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InputMap {
    /// L → L, R → R.
    #[default]
    Stereo,
    /// Left channel to both sides.
    Left,
    /// Right channel to both sides.
    Right,
    /// L ↔ R.
    Swap,
    /// (L + R) / 2 to both sides.
    Mono,
}

impl InputMap {
    pub const ALL: [InputMap; 5] = [InputMap::Stereo, InputMap::Left, InputMap::Right, InputMap::Swap, InputMap::Mono];
    pub fn label(self) -> &'static str {
        match self {
            InputMap::Stereo => "Stereo",
            InputMap::Left => "Left",
            InputMap::Right => "Right",
            InputMap::Swap => "Swap",
            InputMap::Mono => "Mono",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.label().eq_ignore_ascii_case(s))
    }
}

/// A send from a strip to a submix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackSend {
    /// Destination submix track.
    pub target: TrackId,
    /// Send level (dB); automatable as lane `send.<i>.level`.
    pub level_db: f64,
    /// Send pan / balance (−100 … 100).
    #[serde(default)]
    pub pan: f64,
    /// Tap before the fader (after the pre-fader inserts) instead of after the post-fader inserts.
    #[serde(default)]
    pub pre_fader: bool,
    #[serde(default)]
    pub muted: bool,
}

impl TrackSend {
    pub fn new(target: TrackId) -> Self {
        TrackSend { target, level_db: 0.0, pan: 0.0, pre_fader: false, muted: false }
    }
}

/// Mixer-only state of a channel strip (audio track, submix or Mix).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixerStrip {
    pub mode: AutomationMode,
    /// Automation lanes (`volume`, `pan`, `mute`, `send.<i>.level`), keyframes in sequence time.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub lanes: BTreeMap<String, Param>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sends: Vec<TrackSend>,
    /// Output assignment: a submix track, or `None` for the Mix track.
    pub output: Option<TrackId>,
    /// Record-arm (R): the track records voice-over input.
    pub record_arm: bool,
    /// Solo safe: never solo-muted, and keeps its sources audible while other strips are soloed.
    pub solo_safe: bool,
    /// How clip audio feeds the track.
    pub input_map: InputMap,
}

/// Description of an automation lane for display and value ranges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaneInfo {
    pub min: f64,
    pub max: f64,
    /// Values hold between keyframes (switches such as mute).
    pub hold: bool,
    pub unit: &'static str,
}

/// Range and behaviour of a non-effect lane (`volume`, `pan`, `mute`, `send.<i>.level`).
pub fn lane_info(key: &str) -> Option<LaneInfo> {
    match key {
        LANE_VOLUME => Some(LaneInfo { min: FADER_MIN_DB, max: FADER_MAX_DB, hold: false, unit: "dB" }),
        LANE_PAN => Some(LaneInfo { min: -100.0, max: 100.0, hold: false, unit: "" }),
        LANE_MUTE => Some(LaneInfo { min: 0.0, max: 1.0, hold: true, unit: "" }),
        LANE_PAN51_X | LANE_PAN51_Y => Some(LaneInfo { min: -100.0, max: 100.0, hold: false, unit: "" }),
        LANE_PAN51_CENTER => Some(LaneInfo { min: 0.0, max: 100.0, hold: false, unit: "%" }),
        LANE_PAN51_LFE => Some(LaneInfo { min: FADER_MIN_DB, max: FADER_MAX_DB, hold: false, unit: "dB" }),
        k if parse_send_lane(k).is_some() => Some(LaneInfo { min: FADER_MIN_DB, max: FADER_MAX_DB, hold: false, unit: "dB" }),
        _ => None,
    }
}

pub fn parse_send_lane(key: &str) -> Option<usize> {
    key.strip_prefix("send.")?.strip_suffix(".level")?.parse().ok()
}

pub fn parse_fx_lane(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix("fx.")?;
    let (slot, param) = rest.split_once('.')?;
    Some((slot.parse().ok()?, param))
}

impl Track {
    /// Static (un-automated) value of a lane.
    pub fn lane_static(&self, key: &str) -> Option<f64> {
        match key {
            LANE_VOLUME => Some(self.volume_db),
            LANE_PAN => Some(self.pan),
            LANE_MUTE => Some(if self.muted { 1.0 } else { 0.0 }),
            k if PAN51_LANES.contains(&k) => self.mixer.lanes.get(k).and_then(|p| p.value.as_f64()).or_else(|| pan51_default(k, self.channels)),
            k => {
                if let Some(i) = parse_send_lane(k) {
                    return self.mixer.sends.get(i).map(|s| s.level_db);
                }
                let (slot, p) = parse_fx_lane(k)?;
                self.effects.get(slot)?.param(p)?.value.as_f64()
            }
        }
    }

    /// The keyframed parameter behind a lane, if any.
    pub fn lane(&self, key: &str) -> Option<&Param> {
        match parse_fx_lane(key) {
            Some((slot, p)) => self.effects.get(slot)?.param(p),
            None => self.mixer.lanes.get(key),
        }
    }

    /// The parameter behind a lane, created (static, from the current value) if missing.
    pub fn lane_mut(&mut self, key: &str) -> Option<&mut Param> {
        if let Some((slot, p)) = parse_fx_lane(key) {
            return self.effects.get_mut(slot)?.param_mut(p);
        }
        let stat = self.lane_static(key)?;
        lane_info(key)?;
        Some(self.mixer.lanes.entry(key.to_string()).or_insert_with(|| Param::new(ParamValue::Float(stat))))
    }

    /// Lane keyframes (empty when the lane is not automated).
    pub fn lane_keyframes(&self, key: &str) -> &[Keyframe] {
        self.lane(key).map(|p| p.keyframes.as_slice()).unwrap_or(&[])
    }

    /// Value of a lane at sequence time `t` as playback hears it (automation unless the mode is Off).
    pub fn lane_value(&self, key: &str, t: Tick) -> f64 {
        let stat = self.lane_static(key).unwrap_or(0.0);
        if !self.mixer.mode.reads() {
            return stat;
        }
        match self.lane(key) {
            Some(p) if p.is_animated() => p.scalar_at(t),
            _ => stat,
        }
    }

    /// All automated lane keys of this strip.
    pub fn automated_lanes(&self) -> Vec<String> {
        let mut v: Vec<String> = self.mixer.lanes.iter().filter(|(_, p)| p.is_animated()).map(|(k, _)| k.clone()).collect();
        for (slot, e) in self.effects.iter().enumerate() {
            for (pid, p) in &e.params {
                if p.is_animated() {
                    v.push(fx_lane(slot, pid));
                }
            }
        }
        v
    }

    /// Set a lane's static value (fader moves in Off/Read mode, or no automation yet).
    pub fn set_lane_static(&mut self, key: &str, v: f64) -> bool {
        match key {
            LANE_VOLUME => self.volume_db = v.clamp(FADER_MIN_DB, FADER_MAX_DB),
            LANE_PAN => self.pan = v.clamp(-100.0, 100.0),
            LANE_MUTE => self.muted = v >= 0.5,
            k if PAN51_LANES.contains(&k) => {
                let Some(i) = lane_info(k) else { return false };
                let v = v.clamp(i.min, i.max);
                self.mixer.lanes.entry(k.to_string()).or_insert_with(|| Param::new(ParamValue::Float(v))).value = ParamValue::Float(v);
            }
            k => {
                if let Some(i) = parse_send_lane(k) {
                    match self.mixer.sends.get_mut(i) {
                        Some(s) => s.level_db = v.clamp(FADER_MIN_DB, FADER_MAX_DB),
                        None => return false,
                    }
                } else if let Some((slot, p)) = parse_fx_lane(k) {
                    match self.effects.get_mut(slot).and_then(|e| e.param_mut(p)) {
                        Some(par) => par.value = ParamValue::Float(v),
                        None => return false,
                    }
                } else {
                    return false;
                }
            }
        }
        true
    }

    /// Remove insert `slot` (its automation goes with it: effect lanes live inside the effect).
    pub fn remove_insert(&mut self, slot: usize) -> bool {
        if slot < self.effects.len() {
            self.effects.remove(slot);
            true
        } else {
            false
        }
    }

    /// Remove send `i` and shift the automation lanes of the sends above it down.
    pub fn remove_send(&mut self, i: usize) -> bool {
        if i >= self.mixer.sends.len() {
            return false;
        }
        self.mixer.sends.remove(i);
        let mut lanes = std::mem::take(&mut self.mixer.lanes);
        let moved: Vec<(String, Param)> = lanes.iter().filter_map(|(k, p)| parse_send_lane(k).filter(|&j| j >= i).map(|_| (k.clone(), p.clone()))).collect();
        for (k, _) in &moved {
            lanes.remove(k);
        }
        for (k, p) in moved {
            let j = parse_send_lane(&k).unwrap_or(0);
            if j > i {
                lanes.insert(send_lane(j - 1), p);
            }
        }
        self.mixer.lanes = lanes;
        true
    }
}

/// Strip id of the Mix (master) track.
pub const MASTER_STRIP: TrackId = TrackId(u64::MAX);

impl crate::Sequence {
    /// The Mix track as a [`Track`] (volume, inserts and mixer state; no clips).
    pub fn master_strip(&self) -> Track {
        let mut m = Track::new(MASTER_STRIP, crate::TrackKind::Audio, "Mix".into());
        m.volume_db = self.master_volume_db;
        m.effects = self.master_effects.clone();
        m.mixer = self.master_mixer.clone();
        m.channels = self.settings.audio_master;
        m
    }

    /// Any mixer strip (audio track, submix or [`MASTER_STRIP`]).
    pub fn strip(&self, id: TrackId) -> Option<std::borrow::Cow<'_, Track>> {
        if id == MASTER_STRIP {
            return Some(std::borrow::Cow::Owned(self.master_strip()));
        }
        self.mix_track(id).map(std::borrow::Cow::Borrowed)
    }

    /// Edit any mixer strip; edits to the Mix are written back to the sequence's master fields.
    pub fn with_strip_mut<R>(&mut self, id: TrackId, f: impl FnOnce(&mut Track) -> R) -> Option<R> {
        if id == MASTER_STRIP {
            let mut m = self.master_strip();
            let r = f(&mut m);
            self.master_volume_db = m.volume_db;
            self.master_effects = m.effects;
            self.master_mixer = m.mixer;
            return Some(r);
        }
        self.mix_track_mut(id).map(f)
    }

    /// Ids of all strips in mixer order: audio tracks, submixes, Mix.
    pub fn strip_ids(&self) -> Vec<TrackId> {
        self.audio_tracks.iter().chain(&self.submix_tracks).map(|t| t.id).chain(std::iter::once(MASTER_STRIP)).collect()
    }

    /// An audio or submix track (the strips that have a fader besides the Mix).
    pub fn mix_track(&self, id: TrackId) -> Option<&Track> {
        self.audio_tracks.iter().chain(&self.submix_tracks).find(|t| t.id == id)
    }
    pub fn mix_track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.audio_tracks.iter_mut().chain(self.submix_tracks.iter_mut()).find(|t| t.id == id)
    }
    /// Channel format of the bus strip `id` feeds (its output submix, or the Mix).
    pub fn output_channels(&self, id: TrackId) -> crate::AudioChannels {
        if id == MASTER_STRIP {
            return self.settings.audio_master;
        }
        match self.mix_track(id).and_then(|t| t.mixer.output).and_then(|o| self.submix_tracks.iter().find(|s| s.id == o)) {
            Some(s) => s.channels,
            None => self.settings.audio_master,
        }
    }
    /// Whether strip `id` pans with the 5.1 panner (it feeds a 5.1 submix or a 5.1 Mix).
    pub fn pans_51(&self, id: TrackId) -> bool {
        id != MASTER_STRIP && self.output_channels(id) == crate::AudioChannels::Surround51
    }
    /// Mix track volume (dB) at sequence time `t` (automation unless the Mix is in Off mode).
    pub fn master_volume_at(&self, t: Tick) -> f64 {
        match self.master_mixer.lanes.get(LANE_VOLUME) {
            Some(p) if p.is_animated() && self.master_mixer.mode.reads() => p.scalar_at(t),
            _ => self.master_volume_db,
        }
    }
}

/// Thin a recorded gesture (time, value) into keyframes: Ramer–Douglas–Peucker on the value axis
/// (every dropped point lies within `tolerance` of the line between the kept neighbours), then drop
/// points closer than `min_interval` to the previous kept one (except the last). `hold` lanes keep
/// only the points where the value changes.
pub fn thin_points(points: &[(Tick, f64)], tolerance: f64, min_interval: Tick, hold: bool) -> Vec<(Tick, f64)> {
    let mut pts: Vec<(Tick, f64)> = Vec::with_capacity(points.len());
    for &(t, v) in points {
        match pts.last_mut() {
            Some(last) if last.0 == t => last.1 = v,
            Some(last) if last.0 > t => {}
            _ => pts.push((t, v)),
        }
    }
    if pts.len() <= 2 {
        return pts;
    }
    if hold {
        let mut out = vec![pts[0]];
        for w in pts.windows(2) {
            if (w[1].1 - w[0].1).abs() > tolerance.max(1e-9) {
                out.push(w[1]);
            }
        }
        return out;
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ta, va) = (pts[a].0.0 as f64, pts[a].1);
        let (tb, vb) = (pts[b].0.0 as f64, pts[b].1);
        let mut worst = (0.0f64, 0usize);
        for (i, p) in pts.iter().enumerate().take(b).skip(a + 1) {
            let u = (p.0.0 as f64 - ta) / (tb - ta);
            let d = (p.1 - (va + (vb - va) * u)).abs();
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    let mut out: Vec<(Tick, f64)> = Vec::new();
    let n = pts.len();
    for (i, p) in pts.into_iter().enumerate() {
        if !keep[i] {
            continue;
        }
        if i + 1 < n && min_interval.0 > 0 && out.last().is_some_and(|l: &(Tick, f64)| p.0 - l.0 < min_interval) {
            continue;
        }
        out.push(p);
    }
    out
}

/// What [`write_lane`] writes: the range `[t0, t1]`, its thinned `points`, and the surrounding context.
#[derive(Clone, Copy)]
pub struct LaneWrite<'a> {
    pub t0: Tick,
    pub t1: Tick,
    pub points: &'a [(Tick, f64)],
    pub before: Option<f64>,
    pub after: Option<f64>,
    pub eps: Tick,
    pub hold: bool,
}

/// Replace the keyframes of `param` within `[t0, t1]` by `points` (already thinned).
///
/// `before`/`after` are the lane values just outside the written range: when given, a keyframe is
/// placed one `eps` outside the range so the automation outside `[t0, t1]` is unchanged.
pub fn write_lane(param: &mut Param, write: &LaneWrite) {
    let LaneWrite { t0, t1, points, before, after, eps, hold } = *write;
    let interp = if hold { Interpolation::Hold } else { Interpolation::Linear };
    param.keyframes.retain(|k| k.time < t0 || k.time > t1);
    let mut add = |t: Tick, v: f64| {
        let mut k = Keyframe::new(t, ParamValue::Float(v));
        k.interp = interp;
        match param.keyframes.binary_search_by_key(&t, |k| k.time) {
            Ok(i) => param.keyframes[i] = k,
            Err(i) => param.keyframes.insert(i, k),
        }
    };
    if let Some(b) = before {
        let tb = t0 - eps;
        if tb.0 >= 0 {
            add(tb, b);
        }
    }
    for &(t, v) in points.iter().filter(|p| p.0 >= t0 && p.0 <= t1) {
        add(t, v);
    }
    if let Some(a) = after {
        add(t1 + eps, a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TrackKind;

    fn tr() -> Track {
        Track::new(TrackId(1), TrackKind::Audio, "Audio 1".into())
    }

    #[test]
    fn mode_names_roundtrip() {
        for m in AutomationMode::ALL {
            assert_eq!(AutomationMode::from_name(m.label()), Some(m));
        }
        assert_eq!(AutomationMode::default(), AutomationMode::Read);
        assert!(!AutomationMode::Off.reads() && AutomationMode::Touch.writes() && !AutomationMode::Read.writes());
    }

    #[test]
    fn lanes_read_static_and_keyframes() {
        let mut t = tr();
        t.volume_db = -6.0;
        assert_eq!(t.lane_value(LANE_VOLUME, Tick(0)), -6.0);
        let p = t.lane_mut(LANE_VOLUME).unwrap();
        p.keyframes.push(Keyframe::new(Tick(0), ParamValue::Float(0.0)));
        p.keyframes.push(Keyframe::new(Tick(100), ParamValue::Float(-20.0)));
        assert_eq!(t.lane_value(LANE_VOLUME, Tick(50)), -10.0);
        t.mixer.mode = AutomationMode::Off;
        assert_eq!(t.lane_value(LANE_VOLUME, Tick(50)), -6.0);
        assert_eq!(t.automated_lanes(), vec!["volume".to_string()]);
    }

    #[test]
    fn thinning_keeps_shape_within_tolerance() {
        // a ramp sampled densely collapses to its end points; a corner survives
        let pts: Vec<(Tick, f64)> = (0..=100).map(|i| (Tick(i * 10), if i <= 50 { i as f64 * 0.2 } else { 10.0 - (i - 50) as f64 * 0.2 })).collect();
        let th = thin_points(&pts, 0.01, Tick(0), false);
        assert_eq!(th, vec![(Tick(0), 0.0), (Tick(500), 10.0), (Tick(1000), 0.0)]);
        // every original point within tolerance of the thinned curve
        let mut p = Param::new(ParamValue::Float(0.0));
        write_lane(&mut p, &LaneWrite { t0: Tick(0), t1: Tick(1000), points: &th, before: None, after: None, eps: Tick(1), hold: false });
        for (t, v) in pts {
            assert!((p.scalar_at(t) - v).abs() <= 0.01 + 1e-9);
        }
        // hold lanes keep change points only
        let m = [(Tick(0), 0.0), (Tick(10), 0.0), (Tick(20), 1.0), (Tick(30), 1.0), (Tick(40), 0.0)];
        assert_eq!(thin_points(&m, 0.0, Tick(0), true), vec![(Tick(0), 0.0), (Tick(20), 1.0), (Tick(40), 0.0)]);
    }

    #[test]
    fn write_lane_preserves_outside() {
        let mut p = Param::new(ParamValue::Float(0.0));
        write_lane(
            &mut p,
            &LaneWrite { t0: Tick(0), t1: Tick(1000), points: &[(Tick(0), -3.0), (Tick(1000), -3.0)], before: None, after: None, eps: Tick(1), hold: false },
        );
        write_lane(
            &mut p,
            &LaneWrite {
                t0: Tick(400),
                t1: Tick(600),
                points: &[(Tick(400), -20.0), (Tick(600), -20.0)],
                before: Some(-3.0),
                after: Some(-3.0),
                eps: Tick(1),
                hold: false,
            },
        );
        assert_eq!(p.scalar_at(Tick(200)), -3.0);
        assert_eq!(p.scalar_at(Tick(500)), -20.0);
        assert_eq!(p.scalar_at(Tick(800)), -3.0);
        assert_eq!(p.keyframes.len(), 6);
    }

    #[test]
    fn send_removal_shifts_lanes() {
        let mut t = tr();
        for k in 0..3 {
            t.mixer.sends.push(TrackSend::new(TrackId(10 + k)));
            t.mixer.sends[k as usize].level_db = -(k as f64);
            t.lane_mut(&send_lane(k as usize)).unwrap().keyframes.push(Keyframe::new(Tick(0), ParamValue::Float(-(k as f64))));
        }
        assert!(t.remove_send(1));
        assert_eq!(t.mixer.sends.len(), 2);
        assert_eq!(t.lane_keyframes(&send_lane(1))[0].value, ParamValue::Float(-2.0));
        assert!(t.lane(&send_lane(2)).is_none());
    }

    #[test]
    fn pan51_lanes_have_defaults_and_store_statics_in_lanes() {
        let mut t = tr();
        assert_eq!(t.lane_static(LANE_PAN51_Y), Some(100.0));
        assert_eq!(t.lane_static(LANE_PAN51_CENTER), Some(100.0));
        assert_eq!(t.lane_static(LANE_PAN51_LFE), Some(FADER_MIN_DB));
        t.channels = crate::AudioChannels::Surround51;
        assert_eq!(t.lane_static(LANE_PAN51_LFE), Some(0.0));
        assert!(t.set_lane_static(LANE_PAN51_X, -250.0));
        assert_eq!(t.lane_static(LANE_PAN51_X), Some(-100.0));
        assert!(t.automated_lanes().is_empty());
        let p = t.lane_mut(LANE_PAN51_X).unwrap();
        p.keyframes.push(Keyframe::new(Tick(0), ParamValue::Float(-100.0)));
        p.keyframes.push(Keyframe::new(Tick(100), ParamValue::Float(100.0)));
        assert_eq!(t.lane_value(LANE_PAN51_X, Tick(50)), 0.0);
        assert_eq!(t.automated_lanes(), vec![LANE_PAN51_X.to_string()]);
        let back: Track = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn serde_defaults() {
        let s: MixerStrip = serde_json::from_str("{}").unwrap();
        assert_eq!(s, MixerStrip::default());
        let mut t = tr();
        t.mixer.mode = AutomationMode::Touch;
        let j = serde_json::to_string(&t).unwrap();
        let back: Track = serde_json::from_str(&j).unwrap();
        assert_eq!(back, t);
    }
}
