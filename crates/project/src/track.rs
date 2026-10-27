use super::label::Label;
use super::sequence::bounded_time_range;
use super::*;

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
    /// Media time displayed at timeline time `t` (ignores time remapping keyframes).
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
        let rel = t - self.start;
        let scaled = Tick((rel.0 as f64 * self.speed.abs()).round() as i64);
        if self.reverse {
            self.source_in + Tick((self.duration.0 as f64 * self.speed.abs()).round() as i64) - scaled - Tick(1)
        } else {
            self.source_in + scaled
        }
    }
    /// Media time consumed by the item (the source out point).
    pub fn source_out(&self) -> Tick {
        self.source_in + Tick((self.duration.0 as f64 * self.speed.abs()).round() as i64)
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
