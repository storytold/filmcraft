use super::keyframe::{Interpolation, Param};
use super::HexColor;
use super::label::Label;
use super::sequence::bounded_time_range;
use super::{
    ClipId, Deserialize, EffectInstance, EssentialSound, GraphicMeta, ItemId, MarkerId, MixerStrip, MulticamSel, ParamValue, Serialize, Tick, TimeRange,
    TrackId, TransitionId, graphic, mask,
};

/// The smallest rate (media ticks per timeline tick) the Time Remapping integration walks with, so
/// a 0 % speed section freezes on the frame it reached instead of dividing by zero.
const MIN_REMAP_RATE: f64 = 1e-6;
/// The largest rate the integration walks with, so a hostile speed value cannot overflow the
/// timeline arithmetic.
const MAX_REMAP_RATE: f64 = 1e9;

/// One segment of the Time Remapping speed curve between two adjacent keyframes, with the rate
/// (media ticks per timeline tick, in magnitude) at each end. `hold` keeps the left keyframe's
/// rate across the segment (Hold interpolation); otherwise the rate changes linearly with the
/// media time.
#[derive(Clone, Copy)]
struct RemapSeg {
    a: f64,
    b: f64,
    va: f64,
    vb: f64,
    hold: bool,
}

/// Timeline time spent walking a speed-curve segment between media positions `x0` and `x1`
/// (distances from the segment's left edge, in either direction): exact for the linear rate
/// profile (the log-mean of the endpoint rates), `hold` keeps `va`.
fn seg_seconds(s: &RemapSeg, x0: f64, x1: f64) -> f64 {
    let l = (s.b - s.a).max(1.0);
    if s.hold || (s.vb - s.va).abs() <= s.va.abs() * 1e-12 {
        return (x0 - x1).abs() / s.va;
    }
    let c = (s.vb - s.va) / l;
    let v0 = (s.va + c * x0).clamp(MIN_REMAP_RATE, MAX_REMAP_RATE);
    let v1 = (s.va + c * x1).clamp(MIN_REMAP_RATE, MAX_REMAP_RATE);
    // the log of the endpoint rates and the rate difference share a sign per direction, so the
    // magnitude of their quotient is the (positive) crossing time either way
    ((v0 / v1).ln() / c).abs().max(0.0)
}

/// The media position reached walking from `x0` by signed timeline ticks `dt` within a speed-curve
/// segment, saturated to the segment.
fn seg_position(s: &RemapSeg, x0: f64, dt: f64) -> f64 {
    let l = (s.b - s.a).max(1.0);
    let c = (s.vb - s.va) / l;
    if s.hold || c.abs() <= s.va.abs() * 1e-12 / l {
        return (x0 + dt * s.va).clamp(0.0, l);
    }
    let arg = (c * dt).clamp(-40.0, 40.0);
    (x0 + s.va * arg.exp_m1() / c).clamp(0.0, l)
}

/// Walk `remaining` timeline ticks through the media from `from` (a media tick position) under the
/// speed-curve segments (ascending media order), up or down: beyond the outermost keyframes the
/// rate is constant (`v_hi` above, `v_lo` below), inside a segment the crossing time is exact.
/// Returns the media position reached.
fn walk_remapping(segs: &[RemapSeg], v_lo: f64, v_hi: f64, from: f64, mut remaining: f64, up: bool) -> f64 {
    let mut m = from;
    let mut i: isize = if up { segs.partition_point(|s| s.b <= m) as isize } else { segs.partition_point(|s| s.a < m) as isize - 1 };
    loop {
        let s = if i >= 0 { segs.get(i as usize) } else { None };
        match s {
            Some(s) if up && m < s.a => {
                // constant rate before the curve begins
                let dt = (s.a - m) / v_lo;
                if remaining < dt {
                    return m + remaining * v_lo;
                }
                remaining -= dt;
                m = s.a;
            }
            Some(s) if !up && m > s.b => {
                // constant rate after the curve ends (walking down)
                let dt = (m - s.b) / v_hi;
                if remaining < dt {
                    return m - remaining * v_hi;
                }
                remaining -= dt;
                m = s.b;
            }
            Some(s) => {
                let x0 = (m - s.a).clamp(0.0, s.b - s.a);
                let x1 = if up { s.b - s.a } else { 0.0 };
                let dt = seg_seconds(s, x0, x1);
                if remaining < dt {
                    let x = seg_position(s, x0, if up { remaining } else { -remaining });
                    return s.a + x;
                }
                remaining -= dt;
                m = if up { s.b } else { s.a };
                i = if up { i + 1 } else { i - 1 };
            }
            None => {
                // beyond the outermost keyframes the curve holds its end value
                return if up { m + remaining * v_hi } else { m - remaining * v_lo };
            }
        }
    }
}

/// A media position as f64 saturated to the representable `Tick` range.
fn saturate_tick(m: f64) -> i64 {
    if !m.is_finite() {
        return 0;
    }
    m.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkerKind {
    #[default]
    Comment,
    Chapter,
    Segmentation,
    WebLink,
    /// Markers ▸ Add Flash Cue Marker… (a cue point for interactive / Flash-style playback; kept
    /// for interchange). Schema v11.
    FlashCue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    /// Optional web address, separate from notes and retained when changing marker kind.
    /// Opening is explicit and must pass `marker_web_url`, including for imported projects.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Optional user attribution. Empty in older projects; never inferred from the OS account.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// Exact sRGB marker color. None retains the existing preset and its display mapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_color: Option<HexColor>,
    pub id: MarkerId,
    pub start: Tick,
    pub duration: Tick,
    pub name: String,
    pub comment: String,
    pub kind: MarkerKind,
    pub color: Label,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    #[default]
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioChannels {
    Mono,
    #[default]
    Stereo,
    Surround51,
    Adaptive,
}

/// A clip instance on a track.
fn is_zero_stream(stream: &usize) -> bool {
    *stream == 0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackItem {
    pub id: ClipId,
    pub item: ItemId,
    pub name: String,
    pub label: Label,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_label_color: Option<HexColor>,
    /// Timeline position (sequence ticks).
    pub start: Tick,
    /// Duration on the timeline.
    pub duration: Tick,
    /// Media time of the first frame shown.
    pub source_in: Tick,
    /// Playback speed (1.0 = 100%). Negative = reverse.
    pub speed: f64,
    #[serde(default)]
    pub reverse: bool,
    pub enabled: bool,
    /// Linked partner items (video ↔ audio of the same source).
    #[serde(default)]
    pub link: Option<u64>,
    #[serde(default)]
    pub group: Option<u64>,
    pub effects: Vec<EffectInstance>,
    #[serde(default)]
    pub markers: Vec<Marker>,
    /// Clip gain in dB (Audio Gain dialog), separate from the Volume effect.
    #[serde(default)]
    pub gain_db: f64,
    /// Frame hold: media time frozen (Frame Hold Options).
    #[serde(default)]
    pub frame_hold: Option<Tick>,
    /// Scale to frame size (Set to Frame Size / Scale to Frame Size).
    #[serde(default)]
    pub scale_to_frame: bool,
    /// Essential Sound audio type and settings (audio clips).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub essential: Option<EssentialSound>,
    /// Multi-camera clip (a nested multi-camera source sequence): enabled flag and angle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam: Option<MulticamSel>,
    /// Video Options ▸ Time Interpolation: how frames are made when the clip's speed is changed.
    #[serde(default, skip_serializing_if = "TimeInterpolation::is_default")]
    pub time_interpolation: TimeInterpolation,
    /// Frame Hold Options ▸ Hold Filters: effects are evaluated at the held frame's time instead
    /// of animating through the hold.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hold_filters: bool,
    /// Video Options ▸ Field Options… (stored; FilmCraft renders progressive frames).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_options: Option<FieldOptions>,
    /// Audio clips: the source channels this clip plays (0-based; one = mono on both sides, two =
    /// left/right). Empty = the first two channels (mono sources on both sides).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_channels: Vec<u16>,
    /// Audio stream within the media container (0 = the primary stream).
    #[serde(default, skip_serializing_if = "is_zero_stream")]
    pub audio_stream: usize,
    /// Graphic clips: roll / crawl, responsive time and the template the graphic came from
    /// ([`GraphicMeta`]). Schema v12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphic: Option<Box<GraphicMeta>>,
}

/// Time interpolation for speed-changed clips (Clip ▸ Video Options ▸ Time Interpolation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TimeInterpolation {
    /// Show the nearest earlier source frame (repeats or drops frames).
    #[default]
    FrameSampling,
    /// Cross-fade the two source frames around the exact media time.
    FrameBlending,
    /// Motion-compensated in-between frames. Rendered as frame blending for now.
    OpticalFlow,
}

impl TimeInterpolation {
    pub const ALL: [TimeInterpolation; 3] = [TimeInterpolation::FrameSampling, TimeInterpolation::FrameBlending, TimeInterpolation::OpticalFlow];
    pub fn is_default(&self) -> bool {
        *self == TimeInterpolation::FrameSampling
    }
    pub fn name(self) -> &'static str {
        match self {
            TimeInterpolation::FrameSampling => "frameSampling",
            TimeInterpolation::FrameBlending => "frameBlending",
            TimeInterpolation::OpticalFlow => "opticalFlow",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            TimeInterpolation::FrameSampling => "Frame Sampling",
            TimeInterpolation::FrameBlending => "Frame Blending",
            TimeInterpolation::OpticalFlow => "Optical Flow",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name().eq_ignore_ascii_case(s) || t.label().eq_ignore_ascii_case(s))
    }
}

/// Field Options… (stored with the clip).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldOptions {
    pub reverse_field_dominance: bool,
    pub processing: FieldProcessing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldProcessing {
    #[default]
    None,
    AlwaysDeinterlace,
    FlickerRemoval,
}

impl TrackItem {
    pub fn end(&self) -> Tick {
        self.start + self.duration
    }
    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.start, self.duration)
    }
    /// Media time displayed at timeline time `t` (a static Time Remapping speed scales the
    /// constant factor; an animated one integrates the speed curve).
    pub fn source_time_at(&self, t: Tick) -> Tick {
        if let Some(h) = self.frame_hold {
            return h;
        }
        self.moving_source_time_at(t)
    }
    /// Media time at which effects and intrinsic attributes are evaluated at timeline time `t`: a
    /// frame hold freezes them only with Hold Filters on; otherwise they animate through the hold.
    pub fn effect_time_at(&self, t: Tick) -> Tick {
        match self.frame_hold {
            Some(h) if self.hold_filters => h,
            Some(_) => self.moving_source_time_at(t),
            None => self.source_time_at(t),
        }
    }
    /// Media time at timeline time `t` ignoring any frame hold.
    pub fn moving_source_time_at(&self, t: Tick) -> Tick {
        if let Some(m) = self.remapped_media_time(t) {
            return m;
        }
        let k = self.effective_speed();
        let rel = t - self.start;
        let scaled = Tick((rel.0 as f64 * k).round() as i64);
        if self.reverse { self.source_in + Tick((self.duration.0 as f64 * k).round() as i64) - scaled - Tick(1) } else { self.source_in + scaled }
    }
    /// The media span the clip covers in `rel` timeline ticks from its start: the Time Remapping
    /// speed curve walked up when it is animated (whatever the direction flag — the covered span
    /// is the same), the constant factor otherwise. The clip's whole span is
    /// `media_span(duration)`, the far end of [`TrackItem::source_out`].
    pub fn media_span(&self, rel: Tick) -> Tick {
        if let Some((segs, v_lo, v_hi)) = self.remap_curve() {
            let far = Tick(saturate_tick(walk_remapping(&segs, v_lo, v_hi, self.source_in.0 as f64, rel.0.max(0) as f64, true)));
            return (far - self.source_in).max(Tick::ZERO);
        }
        Tick((rel.0.max(0) as f64 * self.effective_speed()).round() as i64)
    }
    /// Media time consumed by the item (the source out point): the far end of the media the clip
    /// plays, which a Time Remapping speed curve spreads differently than a constant factor.
    pub fn source_out(&self) -> Tick {
        self.source_in + self.media_span(self.duration)
    }
    /// The source out point at the clip's constant Speed/Duration factor, ignoring Time Remapping:
    /// what Speed/Duration and Rate Stretch scale, so their speed never counts the remap twice.
    pub fn source_out_unremapped(&self) -> Tick {
        self.source_in + Tick((self.duration.0 as f64 * self.speed.abs()).round() as i64)
    }
    /// The first timeline time at which the clip shows media time `src` (None when it never does):
    /// the inverse of [`TrackItem::source_time_at`].
    pub fn timeline_time_of_media(&self, src: Tick) -> Option<Tick> {
        if self.frame_hold.is_some() {
            return (self.source_time_at(self.start) == src).then_some(self.start);
        }
        if !self.remap_animated() {
            let k = self.effective_speed();
            if !(k.is_finite() && k > 0.0) {
                return None;
            }
            let total = self.media_span(self.duration);
            let rel_src = if self.reverse { total - Tick(1) - (src - self.source_in) } else { src - self.source_in };
            if rel_src < Tick::ZERO || rel_src >= total {
                return None;
            }
            let t = self.start + Tick((rel_src.0 as f64 / k).floor() as i64);
            return (t < self.end()).then_some(t);
        }
        // a curve: the shown media time moves monotonically through the clip, so the timeline time
        // for `src` is found by bisecting the mapping
        let (a, b) = (self.source_time_at(self.start), self.source_time_at((self.end() - Tick(1)).max(self.start)));
        let (lo_mt, hi_mt) = (a.min(b), a.max(b));
        if src < lo_mt || src > hi_mt {
            return None;
        }
        let (mut lo, mut hi) = (0i64, self.duration.0.max(1));
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let m = self.moving_source_time_at(self.start + Tick(mid));
            let hit = if self.reverse { m <= src } else { m >= src };
            if hit {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        let t = self.start + Tick(lo.min(self.duration.0 - 1));
        (t < self.end()).then_some(t)
    }
    /// The Time Remapping effect's Speed parameter, when the clip carries one (percent of the
    /// Speed/Duration factor).
    fn remap_speed(&self) -> Option<&Param> {
        self.effect("time_remap").and_then(|e| e.param("speed"))
    }
    /// Time Remapping speed (percent, in magnitude) at absolute media time `m`: the static value,
    /// or the speed curve's value there. 100 % leaves the clip's Speed/Duration factor unchanged;
    /// a hostile (non-finite) value stands for 100 %.
    pub fn remap_percent_at(&self, m: Tick) -> f64 {
        match self.remap_speed() {
            Some(p) => {
                let v = p.f64_at(m);
                if v.is_finite() { v.abs() } else { 100.0 }
            }
            None => 100.0,
        }
    }
    /// Whether the Time Remapping speed is animated: the media time then comes from integrating
    /// the speed curve, not from a constant factor.
    pub fn remap_animated(&self) -> bool {
        self.remap_speed().is_some_and(|p| p.is_animated())
    }
    /// The clip's Speed/Duration factor (1.0 = 100 %) with its static Time Remapping speed
    /// applied. The curve's value at the clip's In point stands for the whole clip; callers that
    /// need the local rate use [`TrackItem::rate_at`].
    pub fn effective_speed(&self) -> f64 {
        self.speed.abs() * self.remap_percent_at(self.source_in) / 100.0
    }
    /// The playback rate (media ticks per timeline tick, in magnitude) at timeline time `t`: the
    /// Speed/Duration factor with the Time Remapping speed shown there. 1.0 = normal.
    pub fn rate_at(&self, t: Tick) -> f64 {
        self.speed.abs() * self.remap_percent_at(self.moving_source_time_at(t)) / 100.0
    }
    /// The playback rate (magnitude) at one edge of the clip, for converting a media handle to
    /// timeline ticks when trimming: extending an edge consumes media at the position shown there.
    pub fn edge_rate(&self, out_edge: bool) -> f64 {
        let t = if out_edge { (self.end() - Tick(1)).max(self.start) } else { self.start };
        self.rate_at(t).max(1e-9)
    }
    /// The media time shown at timeline time `t` when the Time Remapping speed is animated: the
    /// speed curve (keyed at absolute media times) integrated over the clip, so a ramp trades
    /// timeline time for media time exactly. A reversed clip walks the covered span back from its
    /// far end. `None` without a curve or on hostile values (the constant factor applies).
    fn remapped_media_time(&self, t: Tick) -> Option<Tick> {
        let (segs, v_lo, v_hi) = self.remap_curve()?;
        let rel = ((t - self.start).0 as f64).max(0.0);
        if !rel.is_finite() {
            return None;
        }
        let (from, up) =
            if self.reverse { (self.source_in.0 as f64 + self.media_span(self.duration).0 as f64, false) } else { (self.source_in.0 as f64, true) };
        Some(Tick(saturate_tick(walk_remapping(&segs, v_lo, v_hi, from, rel, up))))
    }
    /// The speed-curve segments of the clip with the rate beyond either end, or `None` when the
    /// Time Remapping speed is not animated (or the values are hostile).
    fn remap_curve(&self) -> Option<(Vec<RemapSeg>, f64, f64)> {
        if !self.remap_animated() {
            return None;
        }
        let base = self.speed.abs();
        if !base.is_finite() || base <= 0.0 {
            return None;
        }
        let p = self.remap_speed()?;
        let mut keys: Vec<(i64, bool)> = p.keyframes.iter().map(|k| (k.time.0, k.interp == Interpolation::Hold)).collect();
        keys.sort_unstable();
        keys.dedup_by_key(|k| k.0);
        let (Some(&first), Some(&last)) = (keys.first(), keys.last()) else { return None };
        let rate = |m: i64| (base * self.remap_percent_at(Tick(m)) / 100.0).clamp(MIN_REMAP_RATE, MAX_REMAP_RATE);
        let v_lo = rate(first.0);
        let v_hi = rate(last.0);
        let segs: Vec<RemapSeg> =
            keys.windows(2).map(|w| RemapSeg { a: w[0].0 as f64, b: w[1].0 as f64, va: rate(w[0].0), vb: rate(w[1].0), hold: w[0].1 }).collect();
        Some((segs, v_lo, v_hi))
    }
    pub fn effect(&self, id: &str) -> Option<&EffectInstance> {
        self.effects.iter().find(|e| e.effect == id)
    }
    pub fn effect_mut(&mut self, id: &str) -> Option<&mut EffectInstance> {
        self.effects.iter_mut().find(|e| e.effect == id)
    }
    /// Standard (non-intrinsic) effects applied, for the fx badge.
    /// The Opacity effect has masks (the layer is cut out before Motion).
    pub fn has_opacity_masks(&self) -> bool {
        self.effect("opacity").is_some_and(|e| e.enabled && e.masks.iter().any(|m| m.mode != mask::MaskMode::None))
    }
    pub fn has_standard_effects(&self) -> bool {
        self.effects.iter().any(|e| e.def().is_some_and(|d| !d.intrinsic) && !graphic::is_layer(e))
    }
    /// The graphic layers (text / shape) of a graphic clip, in paint order (first = back).
    pub fn graphic_layers(&self) -> impl Iterator<Item = &EffectInstance> {
        self.effects.iter().filter(|e| graphic::is_layer(e))
    }
    pub fn has_modified_intrinsics(&self) -> bool {
        self.effects.iter().any(|e| {
            e.def().is_some_and(|d| d.intrinsic)
                && (e.is_animated()
                    || !e.masks.is_empty()
                    || e.def().is_some_and(|d| d.params.iter().any(|p| e.params.get(p.id).is_some_and(|v| !param_eq_default(&v.value, &p.default)))))
        })
    }
}

fn param_eq_default(v: &ParamValue, d: &ParamValue) -> bool {
    match (v, d) {
        (ParamValue::Vec2(_), ParamValue::Vec2(dd)) if dd.x.is_nan() => true,
        _ => v == d,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionAlign {
    #[default]
    CenterAtCut,
    StartAtCut,
    EndAtCut,
}

/// A transition on a track. `at` is the cut point (or clip edge for single-sided transitions).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub id: TransitionId,
    pub effect: EffectInstance,
    /// Timeline range covered by the transition.
    pub start: Tick,
    pub duration: Tick,
    /// Outgoing clip (left) and incoming clip (right); one may be None (fade from/to nothing).
    pub from: Option<ClipId>,
    pub to: Option<ClipId>,
    pub align: TransitionAlign,
    #[serde(default)]
    pub reverse: bool,
}

impl Transition {
    pub fn end(&self) -> Tick {
        self.start + self.duration
    }
    pub fn range(&self) -> TimeRange {
        TimeRange::new(self.start, self.duration)
    }
    /// Progress 0..1 at timeline `t`.
    pub fn progress(&self, t: Tick) -> f64 {
        if self.duration.0 <= 0 {
            return 1.0;
        }
        (((t - self.start).0 as f64) / self.duration.0 as f64).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub items: Vec<TrackItem>,
    pub transitions: Vec<Transition>,
    pub locked: bool,
    pub sync_lock: bool,
    /// Video: eye (output) toggle. Audio: not muted.
    pub enabled: bool,
    pub muted: bool,
    pub solo: bool,
    /// Audio track channel format.
    pub channels: AudioChannels,
    /// Audio track volume (dB) and pan (-100..100) for the Track Mixer.
    pub volume_db: f64,
    pub pan: f64,
    /// Track-level audio effects (mixer inserts, slots 1–5; `EffectInstance::post_fader` picks the side).
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
    /// Audio Track Mixer state: automation mode and lanes, sends, output, record arm.
    #[serde(default)]
    pub mixer: MixerStrip,
    /// Colour label tinting the track's header and lane background (`None` = no colour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<Label>,
}

impl Track {
    pub fn new(id: TrackId, kind: TrackKind, name: String) -> Self {
        Self {
            id,
            kind,
            name,
            items: Vec::new(),
            transitions: Vec::new(),
            locked: false,
            sync_lock: true,
            enabled: true,
            muted: false,
            solo: false,
            channels: AudioChannels::Stereo,
            volume_db: 0.0,
            pan: 0.0,
            effects: Vec::new(),
            mixer: MixerStrip::default(),
            label: None,
        }
    }
    pub fn end(&self) -> Tick {
        self.items.iter().map(|i| i.end()).max().unwrap_or(Tick::ZERO)
    }
    /// Item covering time `t`.
    pub fn item_at(&self, t: Tick) -> Option<&TrackItem> {
        // items are sorted by start
        let idx = self.items.partition_point(|i| i.start <= t);
        idx.checked_sub(1).map(|i| &self.items[i]).filter(|i| t < i.end())
    }
    pub fn item(&self, id: ClipId) -> Option<&TrackItem> {
        self.items.iter().find(|i| i.id == id)
    }
    pub fn item_mut(&mut self, id: ClipId) -> Option<&mut TrackItem> {
        self.items.iter_mut().find(|i| i.id == id)
    }
    pub fn sort(&mut self) {
        self.items.sort_by_key(|i| i.start);
        self.transitions.sort_by_key(|t| t.start);
    }
    /// Invariant check: items do not overlap.
    pub fn check(&self) -> Result<(), String> {
        // Validate before calling end(), including on a single-item track.
        self.check_bounds()?;
        for w in self.items.windows(2) {
            if w[0].end() > w[1].start {
                return Err(format!("{}: items {:?} and {:?} overlap", self.name, w[0].id, w[1].id));
            }
        }
        for i in &self.items {
            if i.duration.0 <= 0 {
                return Err(format!("{}: item {:?} has non-positive duration", self.name, i.id));
            }
        }
        Ok(())
    }
    /// Only the value bounds that keep timeline arithmetic from overflowing (no structural rules
    /// such as overlap or positive duration); what a loaded project must satisfy.
    pub fn check_bounds(&self) -> Result<(), String> {
        for item in &self.items {
            if !bounded_time_range(item.start, item.duration) {
                return Err(format!("{}: item {:?} is outside supported time bounds", self.name, item.id));
            }
        }
        for transition in &self.transitions {
            if !bounded_time_range(transition.start, transition.duration) {
                return Err(format!("{}: transition {:?} is outside supported time bounds", self.name, transition.id));
            }
        }
        Ok(())
    }
}
