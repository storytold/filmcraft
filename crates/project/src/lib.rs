//! The FilmCraft document model.
//!
//! A [`Project`] owns a tree of bins and a flat map of [`ProjectItem`]s (media clips, sequences,
//! synthetic items). A [`Sequence`] has video and audio [`Track`]s holding [`TrackItem`]s (clip
//! instances) and [`Transition`]s. Everything is plain serde data; the engine wraps the project in an
//! `Arc` and edits copy-on-write, so undo snapshots and background readers are free.
//!
//! Time: timeline positions are sequence ticks; `source_in` is media time.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod caption;
pub mod effect;
pub mod essential;
pub mod find;
pub mod graphic;
pub mod graphic_design;
pub mod gtemplate;
pub mod keyframe;
pub mod mask;
pub mod mixer;
pub mod multicam;
pub mod narration;
pub mod transcript;
pub mod vtransition;

use std::collections::BTreeMap;

use filmcraft_geom::Vec2;
use filmcraft_media::{Generator, MediaInfo};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde::{Deserialize, Serialize};

pub use caption::{Caption, CaptionAlign, CaptionAnchor, CaptionFormat, CaptionStyle, CaptionTrack, plain_text};
pub use effect::{EffectDef, EffectInstance, EffectKind, ParamDef, ParamKind, effect_defs, find_effect};
pub use essential::{AudioType, EssentialSound};
pub use find::{FindOp, FindQuery, FindRow, SearchBin};
pub use graphic_design::{CharStyle, GraphicMeta, LayerExtra, Pin, PinTarget, Roll, RollMode, SourceGraphic, StyleRun};
pub use keyframe::{Interpolation, Keyframe, Param, ParamValue};
pub use mask::{Mask, MaskMode, MaskPath, MaskVertex, TrackMethod};
pub use mixer::{AutomationMode, InputMap, MixerStrip, TrackSend};
pub use multicam::{Camera, MergedClip, MulticamAudio, MulticamSel, MulticamSource};
pub use narration::{Narration, VocalPitch};
pub use transcript::{Speaker, Transcript, Word};

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);
    };
}
id_type!(ItemId);
id_type!(BinId);
id_type!(TrackId);
id_type!(ClipId);
id_type!(TransitionId);
id_type!(MarkerId);

/// Premiere-style label colours (names from the Label menu; values are our own).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Label {
    Violet,
    Iris,
    Caribbean,
    Lavender,
    Cerulean,
    Forest,
    Rose,
    Mango,
    Purple,
    Blue,
    Teal,
    Magenta,
    Tan,
    Green,
    Brown,
    Yellow,
}

impl Label {
    pub const ALL: [Label; 16] = [
        Label::Violet,
        Label::Iris,
        Label::Caribbean,
        Label::Lavender,
        Label::Cerulean,
        Label::Forest,
        Label::Rose,
        Label::Mango,
        Label::Purple,
        Label::Blue,
        Label::Teal,
        Label::Magenta,
        Label::Tan,
        Label::Green,
        Label::Brown,
        Label::Yellow,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Label::Violet => "Violet",
            Label::Iris => "Iris",
            Label::Caribbean => "Caribbean",
            Label::Lavender => "Lavender",
            Label::Cerulean => "Cerulean",
            Label::Forest => "Forest",
            Label::Rose => "Rose",
            Label::Mango => "Mango",
            Label::Purple => "Purple",
            Label::Blue => "Blue",
            Label::Teal => "Teal",
            Label::Magenta => "Magenta",
            Label::Tan => "Tan",
            Label::Green => "Green",
            Label::Brown => "Brown",
            Label::Yellow => "Yellow",
        }
    }
    /// Label colours (Premiere 26 Spectrum defaults, measured; see plan/premiere/02-ui-ux.md §1.5).
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Label::Violet => [0x38, 0x0e, 0xa7],
            Label::Iris => [0x1d, 0x4a, 0x64],
            Label::Caribbean => [0x35, 0x54, 0x18],
            Label::Lavender => [0x6b, 0x1c, 0x82],
            Label::Cerulean => [0x23, 0x53, 0x5a],
            Label::Forest => [0x40, 0x4a, 0x11],
            Label::Rose => [0x80, 0x18, 0x36],
            Label::Mango => [0x80, 0x3f, 0x17],
            Label::Purple => [0x59, 0x0d, 0xb0],
            Label::Blue => [0x19, 0x2d, 0x94],
            Label::Teal => [0x1f, 0x4d, 0x45],
            Label::Magenta => [0x79, 0x1c, 0x56],
            Label::Tan => [0x6c, 0x5b, 0x47],
            Label::Green => [0x29, 0x5c, 0x2d],
            Label::Brown => [0x58, 0x3d, 0x14],
            Label::Yellow => [0x6e, 0x66, 0x28],
        }
    }
    /// Marker colours (Markers panel chips).
    pub fn marker_rgb(self) -> [u8; 3] {
        match self {
            Label::Rose | Label::Magenta => [0xc1, 0x3c, 0x3d],
            Label::Purple | Label::Violet | Label::Lavender => [0xa9, 0x8c, 0xaf],
            Label::Mango | Label::Brown | Label::Tan => [0xda, 0x76, 0x39],
            Label::Yellow => [0xc9, 0xa3, 0x44],
            Label::Blue | Label::Iris | Label::Cerulean => [0x56, 0x8b, 0xf4],
            Label::Teal | Label::Caribbean => [0x72, 0xf0, 0xd7],
            _ => [0x75, 0x85, 0x42],
        }
    }
    pub fn from_name(s: &str) -> Option<Label> {
        Self::ALL.iter().copied().find(|l| l.name().eq_ignore_ascii_case(s))
    }
}

/// Where an item's pixels/samples come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MediaRef {
    /// A file on disk (native) or a named blob (web).
    File { path: String },
    /// A synthetic generator (Bars and Tone, Color Matte, demo footage…).
    Generator(Generator),
}

/// Overrides applied when interpreting footage (Modify ▸ Interpret Footage).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Interpretation {
    pub frame_rate: Option<FrameRate>,
    pub par: Option<(u32, u32)>,
    pub ignore_alpha: bool,
    pub invert_alpha: bool,
    /// Color Management ▸ override the colour space detected from the file's metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_space: Option<filmcraft_color::ColorSpace>,
    /// Modify ▸ Audio Channels: how the source channels map to the audio clips made when the item
    /// is edited into a sequence. None = one stereo clip of the first two channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_channels: Option<AudioChannelMap>,
}

/// Modify ▸ Audio Channels: the clip channel format and, per audio clip, the source channels it
/// plays (0-based). A mono clip has one channel (played on both sides); a stereo clip two.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioChannelMap {
    pub format: AudioChannels,
    pub clips: Vec<Vec<u16>>,
}

impl AudioChannelMap {
    /// The default mapping of a source with `channels` channels in `format`: mono = one clip per
    /// channel, stereo = channel pairs, 5.1 / adaptive = one clip with every channel.
    pub fn for_format(format: AudioChannels, channels: u16) -> Self {
        let n = channels.max(1);
        let clips = match format {
            AudioChannels::Mono => (0..n).map(|c| vec![c]).collect(),
            AudioChannels::Stereo => (0..n).step_by(2).map(|c| if c + 1 < n { vec![c, c + 1] } else { vec![c] }).collect(),
            AudioChannels::Surround51 | AudioChannels::Adaptive => vec![(0..n).collect()],
        };
        Self { format, clips }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaClip {
    pub media: MediaRef,
    pub info: MediaInfo,
    pub interpret: Interpretation,
    /// Source in/out marks (media time).
    pub mark_in: Option<Tick>,
    pub mark_out: Option<Tick>,
    pub markers: Vec<Marker>,
    /// Made offline on purpose (Make Offline / Offline All): renders the offline slate even if
    /// the file exists. Files that are merely missing are detected at run time instead (the
    /// engine's media pool), so a project whose media comes back needs no change.
    pub offline: bool,
    /// Proxy media, if attached.
    pub proxy: Option<MediaRef>,
    /// Size and content fingerprint of the file when it was imported (or last linked); relinking
    /// checks a candidate against it. None for generators and projects from older builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<MediaIdentity>,
}

/// What a media file looked like when it was linked: its size plus a fast content fingerprint
/// (a 64-bit hash of the size, the first MiB and the last MiB). Cheap to compute on any file and
/// enough to tell a moved original from a different file with the same name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaIdentity {
    pub size: u64,
    /// Hex-encoded in JSON (`"9f3c…"`), so the value survives JavaScript readers.
    #[serde(with = "hex_u64")]
    pub fingerprint: u64,
}

mod hex_u64 {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{v:016x}"))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        u64::from_str_radix(&s, 16).map_err(serde::de::Error::custom)
    }
}

impl MediaClip {
    pub fn frame_rate(&self) -> FrameRate {
        self.interpret.frame_rate.unwrap_or_else(|| self.info.frame_rate())
    }
    /// Pixel aspect ratio of the picture, in lowest terms: Modify ▸ Interpret Footage's override,
    /// else the file's. A ratio that makes no sense ([`checked_par`]) is ignored: an override in
    /// favour of the file's value, the file's in favour of square pixels.
    pub fn pixel_aspect(&self) -> (u32, u32) {
        self.interpret.par.and_then(checked_par).or_else(|| self.info.video.as_ref().and_then(|v| checked_par(v.par))).unwrap_or((1, 1))
    }
    pub fn duration(&self) -> Tick {
        self.info.duration
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum ItemKind {
    Media(MediaClip),
    /// Shared so project snapshots used by undo and background readers do not deep-copy every
    /// timeline. Mutators detach with `Arc::make_mut`.
    Sequence(std::sync::Arc<Sequence>),
    /// A subclip: a media item restricted to a range.
    Subclip {
        parent: ItemId,
        range: TimeRange,
        /// Make Subclip ▸ Restrict Trims To Subclip Boundaries: clips of the subclip can't be
        /// trimmed past its end (otherwise the parent's media is the limit).
        #[serde(default)]
        restrict_trims: bool,
    },
    AdjustmentLayer {
        width: u32,
        height: u32,
        rate: FrameRate,
        duration: Tick,
    },
    /// The source of graphic clips: a transparent canvas of the sequence frame size. The clip's
    /// text and shape layers are effect instances on the track item (see [`graphic`]). Not shown
    /// in the Project panel; unlimited duration.
    Graphic {
        width: u32,
        height: u32,
        rate: FrameRate,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectItem {
    pub id: ItemId,
    pub name: String,
    pub label: Label,
    pub kind: ItemKind,
    /// Free-form metadata (Description, Scene, Shot, Log Note…).
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    /// Import time counter (for "Date Created"/sorting), not wall clock.
    #[serde(default)]
    pub created: u64,
    /// Split In/Out points set in the Source monitor (Markers ▸ Mark Split); they override the
    /// item's In/Out for one channel (J- and L-cuts).
    #[serde(default, skip_serializing_if = "SplitMarks::is_empty")]
    pub split: SplitMarks,
}

impl ProjectItem {
    pub fn as_media(&self) -> Option<&MediaClip> {
        match &self.kind {
            ItemKind::Media(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_media_mut(&mut self) -> Option<&mut MediaClip> {
        match &mut self.kind {
            ItemKind::Media(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_sequence(&self) -> Option<&Sequence> {
        match &self.kind {
            ItemKind::Sequence(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_sequence_mut(&mut self) -> Option<&mut Sequence> {
        match &mut self.kind {
            ItemKind::Sequence(s) => Some(std::sync::Arc::make_mut(s)),
            _ => None,
        }
    }
    pub fn has_video(&self) -> bool {
        match &self.kind {
            ItemKind::Media(m) => m.info.has_video(),
            ItemKind::Sequence(_) | ItemKind::AdjustmentLayer { .. } | ItemKind::Graphic { .. } => true,
            ItemKind::Subclip { .. } => true,
        }
    }
    pub fn has_audio(&self) -> bool {
        match &self.kind {
            ItemKind::Media(m) => m.info.has_audio(),
            ItemKind::Sequence(s) => !s.audio_tracks.is_empty(),
            _ => false,
        }
    }
    pub fn duration(&self) -> Tick {
        match &self.kind {
            ItemKind::Media(m) => m.duration(),
            ItemKind::Sequence(s) => s.duration(),
            ItemKind::Subclip { range, .. } => range.duration,
            ItemKind::AdjustmentLayer { duration, .. } => *duration,
            ItemKind::Graphic { .. } => Tick(3600 * TICKS_PER_SECOND),
        }
    }
    pub fn frame_rate(&self) -> FrameRate {
        match &self.kind {
            ItemKind::Media(m) => m.frame_rate(),
            ItemKind::Sequence(s) => s.settings.frame_rate,
            ItemKind::AdjustmentLayer { rate, .. } | ItemKind::Graphic { rate, .. } => *rate,
            ItemKind::Subclip { .. } => FrameRate::default(),
        }
    }
    /// Human-readable media type for the Project panel "Media Type" column.
    pub fn type_label(&self) -> &'static str {
        match &self.kind {
            ItemKind::Media(m) => match m.info.kind {
                filmcraft_media::MediaKind::Movie => "Movie",
                filmcraft_media::MediaKind::AudioOnly => "Audio",
                filmcraft_media::MediaKind::Still => "Still Image",
                filmcraft_media::MediaKind::ImageSequence => "Image Sequence",
                filmcraft_media::MediaKind::Synthetic => "Synthetic",
            },
            ItemKind::Sequence(s) if s.multicam.is_some() => "Multi-Camera Source Sequence",
            ItemKind::Sequence(s) if s.merged.is_some() => "Merged Clip",
            ItemKind::Sequence(_) => "Sequence",
            ItemKind::Subclip { .. } => "Subclip",
            ItemKind::AdjustmentLayer { .. } => "Adjustment Layer",
            ItemKind::Graphic { .. } => "Graphic",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BinEntry {
    Item(ItemId),
    Bin(Bin),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bin {
    pub id: BinId,
    pub name: String,
    pub children: Vec<BinEntry>,
}

impl Bin {
    pub fn find_bin_mut(&mut self, id: BinId) -> Option<&mut Bin> {
        if self.id == id {
            return Some(self);
        }
        for c in &mut self.children {
            if let BinEntry::Bin(b) = c
                && let Some(f) = b.find_bin_mut(id)
            {
                return Some(f);
            }
        }
        None
    }
    pub fn find_bin(&self, id: BinId) -> Option<&Bin> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| if let BinEntry::Bin(b) = c { b.find_bin(id) } else { None })
    }
    /// Remove an item anywhere in the tree.
    pub fn remove_item(&mut self, id: ItemId) -> bool {
        let before = self.children.len();
        self.children.retain(|c| !matches!(c, BinEntry::Item(i) if *i == id));
        if self.children.len() != before {
            return true;
        }
        self.children.iter_mut().any(|c| if let BinEntry::Bin(b) = c { b.remove_item(id) } else { false })
    }
    /// Take a sub-bin, and everything in it, out of the tree. The bin itself is never removed from itself.
    pub fn remove_bin(&mut self, id: BinId) -> Option<Bin> {
        if let Some(at) = self.children.iter().position(|c| matches!(c, BinEntry::Bin(b) if b.id == id))
            && let BinEntry::Bin(b) = self.children.remove(at)
        {
            return Some(b);
        }
        self.children.iter_mut().find_map(|c| if let BinEntry::Bin(b) = c { b.remove_bin(id) } else { None })
    }
    /// All items in this bin and sub-bins.
    pub fn all_items(&self, out: &mut Vec<ItemId>) {
        for c in &self.children {
            match c {
                BinEntry::Item(i) => out.push(*i),
                BinEntry::Bin(b) => b.all_items(out),
            }
        }
    }
    /// The bin containing an item.
    pub fn parent_of(&self, id: ItemId) -> Option<BinId> {
        for c in &self.children {
            match c {
                BinEntry::Item(i) if *i == id => return Some(self.id),
                BinEntry::Bin(b) => {
                    if let Some(p) = b.parent_of(id) {
                        return Some(p);
                    }
                }
                _ => {}
            }
        }
        None
    }
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceSettings {
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub par: (u32, u32),
    pub sample_rate: u32,
    pub drop_frame: bool,
    /// Editing mode / preset name shown in the settings dialog.
    pub preset: String,
    pub audio_master: AudioChannels,
    /// Preview file codec name.
    pub preview_codec: String,
    pub max_bit_depth: bool,
    pub max_render_quality: bool,
    /// Working colour space (display label; [`SequenceSettings::color`] is authoritative).
    pub working_space: String,
    /// Colour pipeline: working space (Rec. 709 / Rec. 2100 PQ / HLG), wide-gamut compositing,
    /// Auto Tone Map Media.
    #[serde(default = "default_color_pipeline")]
    pub color: filmcraft_color::ColorPipeline,
}

fn default_color_pipeline() -> filmcraft_color::ColorPipeline {
    filmcraft_color::ColorPipeline::REC709
}

impl Default for SequenceSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            frame_rate: FrameRate::FPS_23_976,
            par: (1, 1),
            sample_rate: 48_000,
            drop_frame: false,
            preset: "HD 1080p 23.976".into(),
            audio_master: AudioChannels::Stereo,
            preview_codec: "ProRes 422".into(),
            max_bit_depth: false,
            max_render_quality: false,
            working_space: "Rec. 709".into(),
            color: filmcraft_color::ColorPipeline::REC709,
        }
    }
}

/// Largest frame side, in pixels.
pub const MAX_FRAME_SIDE: u32 = 32_768;
/// Largest frame area, in pixels (16384 x 16384: 16K and 16384 x 8192 panoramas fit).
pub const MAX_FRAME_PIXELS: u64 = 268_435_456;

/// Bound working-frame allocations while retaining 16K and wide panoramic frames.
pub fn validate_frame_size(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_FRAME_SIDE || height > MAX_FRAME_SIDE || u64::from(width) * u64::from(height) > MAX_FRAME_PIXELS {
        return Err(format!("frame size must be positive, at most {MAX_FRAME_SIDE} pixels per side and {MAX_FRAME_PIXELS} pixels total"));
    }
    Ok(())
}

/// The most a pixel aspect ratio may stretch a picture either way (8:1 or 1:8). Real footage stays
/// well inside (2:1 for 2x anamorphic lenses, 4:3 for HDV, 0.9 for NTSC DV); a ratio beyond it is
/// garbage from a damaged file and is treated as square.
pub const MAX_PIXEL_ASPECT: f64 = 8.0;

/// A pixel aspect ratio fit to compute with, in lowest terms, or `None` when it is unset (a zero
/// term, as decoders report "not signalled") or stretches more than [`MAX_PIXEL_ASPECT`] either
/// way. Ratios come from media, project and interchange files, so they are untrusted.
pub fn checked_par(par: (u32, u32)) -> Option<(u32, u32)> {
    let (n, d) = par;
    if n == 0 || d == 0 {
        return None;
    }
    let r = f64::from(n) / f64::from(d);
    if !(1.0 / MAX_PIXEL_ASPECT..=MAX_PIXEL_ASPECT).contains(&r) {
        return None;
    }
    let (mut a, mut b) = (n, d);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let g = a.max(1);
    Some((n / g, d / g))
}

/// [`checked_par`], with square pixels for a ratio that is unset or makes no sense.
pub fn sane_par(par: (u32, u32)) -> (u32, u32) {
    checked_par(par).unwrap_or((1, 1))
}

/// The width of one pixel over its height ([`sane_par`] first): 1.333… for 4:3 pixels.
pub fn par_ratio(par: (u32, u32)) -> f64 {
    let (n, d) = sane_par(par);
    f64::from(n) / f64::from(d)
}

/// The size of a `size` picture with `par` pixels in the pixels of a frame whose pixel aspect is
/// `frame_par`: the width scales by par / frame_par, the height stays. A 1440 x 1080 picture with
/// 4:3 pixels covers 1920 x 1080 square pixels.
pub fn conformed_size(size: (u32, u32), par: (u32, u32), frame_par: (u32, u32)) -> (f64, f64) {
    let (par, frame_par) = (sane_par(par), sane_par(frame_par));
    let w = if par == frame_par { f64::from(size.0) } else { f64::from(size.0) * par_ratio(par) / par_ratio(frame_par) };
    (w, f64::from(size.1))
}

impl SequenceSettings {
    pub fn validate(&self) -> Result<(), String> {
        validate_frame_size(self.width, self.height)?;
        if self.frame_rate.num <= 0
            || self.frame_rate.den <= 0
            || self.frame_rate.as_f64() > 1000.0
            || self.frame_rate.num > i64::from(u32::MAX)
            || self.frame_rate.den > i64::from(u32::MAX)
            || (i128::from(TICKS_PER_SECOND) * i128::from(self.frame_rate.den) / i128::from(self.frame_rate.num.max(1))) > i128::from(Tick::MAX.0)
        {
            return Err("frame rate must be positive, at most 1000 fps, with unsigned 32-bit rational components".into());
        }
        if self.sample_rate == 0 || self.sample_rate > 384_000 {
            return Err("sample rate must be between 1 and 384000 Hz".into());
        }
        if self.par.0 == 0 || self.par.1 == 0 {
            return Err("pixel aspect ratio must be positive".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sequence {
    pub settings: SequenceSettings,
    pub video_tracks: Vec<Track>,
    pub audio_tracks: Vec<Track>,
    pub markers: Vec<Marker>,
    pub mark_in: Option<Tick>,
    pub mark_out: Option<Tick>,
    /// Work area bar (optional; Premiere hides it by default now).
    pub work_area: Option<TimeRange>,
    /// Timecode of the first frame (frames), "Start Time" in Sequence settings.
    pub start_timecode: i64,
    /// Master audio volume (dB).
    #[serde(default)]
    pub master_volume_db: f64,
    /// Mix track inserts (`EffectInstance::post_fader` picks the side).
    #[serde(default)]
    pub master_effects: Vec<EffectInstance>,
    /// Mix track automation (`volume` lane) and mode.
    #[serde(default)]
    pub master_mixer: MixerStrip,
    /// Audio submix tracks (no clips); tracks and sends route into them, they route to the Mix
    /// or to a submix after them.
    #[serde(default)]
    pub submix_tracks: Vec<Track>,
    /// Caption tracks (drawn above the video tracks; first = top). Older files have none (serde default).
    #[serde(default)]
    pub caption_tracks: Vec<CaptionTrack>,
    /// Set on multi-camera source sequences (cameras, audio mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam: Option<MulticamSource>,
    /// Set on merged clips (the merged video and audio items).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged: Option<MergedClip>,
    /// Split In/Out points (Markers ▸ Mark Split): per-channel overrides of `mark_in`/`mark_out`.
    #[serde(default, skip_serializing_if = "SplitMarks::is_empty")]
    pub split: SplitMarks,
}

/// Split edit points: separate video and audio In/Out points (Markers ▸ Mark Split). `None` means
/// the channel uses the ordinary In/Out point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SplitMarks {
    pub video_in: Option<Tick>,
    pub video_out: Option<Tick>,
    pub audio_in: Option<Tick>,
    pub audio_out: Option<Tick>,
}

impl SplitMarks {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
    /// The video In point given the ordinary In point.
    pub fn video_in_or(&self, mark_in: Option<Tick>) -> Option<Tick> {
        self.video_in.or(mark_in)
    }
    pub fn video_out_or(&self, mark_out: Option<Tick>) -> Option<Tick> {
        self.video_out.or(mark_out)
    }
    pub fn audio_in_or(&self, mark_in: Option<Tick>) -> Option<Tick> {
        self.audio_in.or(mark_in)
    }
    pub fn audio_out_or(&self, mark_out: Option<Tick>) -> Option<Tick> {
        self.audio_out.or(mark_out)
    }
}

impl Sequence {
    pub fn duration(&self) -> Tick {
        let media = self.video_tracks.iter().chain(&self.audio_tracks).map(Track::end).max().unwrap_or(Tick::ZERO);
        media.max(self.caption_tracks.iter().map(CaptionTrack::end).max().unwrap_or(Tick::ZERO))
    }
    pub fn caption_track(&self, id: TrackId) -> Option<&CaptionTrack> {
        self.caption_tracks.iter().find(|t| t.id == id)
    }
    pub fn caption_track_mut(&mut self, id: TrackId) -> Option<&mut CaptionTrack> {
        self.caption_tracks.iter_mut().find(|t| t.id == id)
    }
    /// Find a caption anywhere: (caption track id, caption).
    pub fn find_caption(&self, id: ClipId) -> Option<(TrackId, &Caption)> {
        self.caption_tracks.iter().find_map(|t| t.caption(id).map(|c| (t.id, c)))
    }
    pub fn tracks(&self, kind: TrackKind) -> &Vec<Track> {
        match kind {
            TrackKind::Video => &self.video_tracks,
            TrackKind::Audio => &self.audio_tracks,
        }
    }
    pub fn tracks_mut(&mut self, kind: TrackKind) -> &mut Vec<Track> {
        match kind {
            TrackKind::Video => &mut self.video_tracks,
            TrackKind::Audio => &mut self.audio_tracks,
        }
    }
    pub fn all_tracks(&self) -> impl Iterator<Item = &Track> {
        self.video_tracks.iter().chain(self.audio_tracks.iter())
    }
    pub fn all_tracks_mut(&mut self) -> impl Iterator<Item = &mut Track> {
        self.video_tracks.iter_mut().chain(self.audio_tracks.iter_mut())
    }
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.all_tracks().find(|t| t.id == id)
    }
    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.all_tracks_mut().find(|t| t.id == id)
    }
    /// Find a track item anywhere: (track id, item).
    pub fn find_item(&self, id: ClipId) -> Option<(TrackId, &TrackItem)> {
        self.all_tracks().find_map(|t| t.item(id).map(|i| (t.id, i)))
    }
    pub fn find_item_mut(&mut self, id: ClipId) -> Option<(TrackId, &mut TrackItem)> {
        self.all_tracks_mut().find_map(|t| {
            let tid = t.id;
            t.item_mut(id).map(|i| (tid, i))
        })
    }
    pub fn frame_rate(&self) -> FrameRate {
        self.settings.frame_rate
    }
    /// Sorted, de-duplicated edit points (clip boundaries) on all tracks.
    pub fn edit_points(&self) -> Vec<Tick> {
        let mut v: Vec<Tick> = self.all_tracks().flat_map(|t| t.items.iter().flat_map(|i| [i.start, i.end()])).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
    pub fn check(&self) -> Result<(), String> {
        self.check_bounds()?;
        for t in self.all_tracks() {
            t.check()?;
        }
        for t in &self.caption_tracks {
            t.check()?;
        }
        Ok(())
    }
    /// The hostile-value bounds alone (settings, marks and every time range), without the
    /// structural invariants of [`Sequence::check`]: a project that breaks only those (e.g. an
    /// overlap written by an older build) still opens, but nothing in it can overflow.
    pub fn check_bounds(&self) -> Result<(), String> {
        self.settings.validate()?;
        if self.mark_in.into_iter().chain(self.mark_out).any(|t| t < Tick::MIN || t > Tick::MAX)
            || self.work_area.is_some_and(|r| !bounded_time_range(r.start, r.duration))
        {
            return Err("sequence marks or work area are outside supported time bounds".into());
        }
        for t in self.all_tracks() {
            t.check_bounds()?;
        }
        for t in &self.caption_tracks {
            t.check_bounds()?;
        }
        Ok(())
    }
}

/// Bound persisted timeline arithmetic before any start + duration operation: start, duration and
/// end all stay within `Tick::MIN..=Tick::MAX`, leaving headroom for later edit math. Negative
/// positions remain supported; the sign of the duration is a structural rule (track items and
/// captions require a positive one in their own invariant checks), not a bound.
pub(crate) fn bounded_time_range(start: Tick, duration: Tick) -> bool {
    let within = |t: i64| (Tick::MIN.0..=Tick::MAX.0).contains(&t);
    within(start.0) && within(duration.0) && start.0.checked_add(duration.0).is_some_and(within)
}

/// Project-level settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectSettings {
    pub renderer: String,
    pub video_display: filmcraft_time::TimeDisplay,
    pub audio_display_samples: bool,
    /// Kept for file compatibility; the editor uses Settings ▸ Timeline (user preferences) for
    /// still and transition default durations, as Premiere does.
    pub default_still_duration: Tick,
    pub default_transition_duration_frames: i64,
    pub default_audio_transition_duration: Tick,
    /// Project Settings ▸ Ingest Settings: what happens to media on import.
    #[serde(default)]
    pub ingest: IngestSettings,
    /// Project Settings ▸ General ▸ Action and Title Safe Areas: (horizontal, vertical) percent.
    #[serde(default = "title_safe_default")]
    pub title_safe: (f64, f64),
    #[serde(default = "action_safe_default")]
    pub action_safe: (f64, f64),
    /// Project Settings ▸ General ▸ Capture Format (`DV` or `HDV`; informational, FilmCraft has no
    /// tape capture). Schema v11.
    #[serde(default = "capture_format_default")]
    pub capture_format: String,
    /// Project Settings ▸ Scratch Disks. Schema v11.
    #[serde(default)]
    pub scratch: ScratchDisks,
}

fn title_safe_default() -> (f64, f64) {
    (20.0, 20.0)
}
fn action_safe_default() -> (f64, f64) {
    (10.0, 10.0)
}
fn capture_format_default() -> String {
    "DV".into()
}

/// Project Settings ▸ Scratch Disks: where generated files go. `None` = Same as Project (next to
/// the project file; the data directory for an unsaved project).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScratchDisks {
    /// Captured and Generated media: proxies (when no ingest destination is set), Extract Audio.
    pub captured: Option<String>,
    /// Video and audio render previews (`<dir>/<project name>`).
    pub video_previews: Option<String>,
    pub audio_previews: Option<String>,
    /// Project Auto Save versions.
    pub auto_save: Option<String>,
}

/// Ingest on import (Project Settings ▸ Ingest Settings).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IngestSettings {
    pub enabled: bool,
    pub action: IngestAction,
    /// Destination folder for copies, transcodes and proxies (None = next to the media:
    /// `<media dir>/Proxies` for proxies, `<media dir>/Ingest` for copies and transcodes).
    pub destination: Option<String>,
    /// Proxy / transcode preset id (the engine's `proxies::PRESETS`); empty = the default.
    pub preset: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IngestAction {
    /// Copy the files to the destination (verified), then use the copies.
    Copy,
    /// Transcode to the preset's format, then use the transcodes.
    Transcode,
    /// Create proxies (in the background) and attach them.
    #[default]
    CreateProxies,
    /// Copy, then create proxies of the copies.
    CopyAndCreateProxies,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            renderer: "FilmCraft GPU Acceleration (wgpu)".into(),
            video_display: filmcraft_time::TimeDisplay::Timecode,
            audio_display_samples: true,
            default_still_duration: Tick(5 * TICKS_PER_SECOND),
            default_transition_duration_frames: 24,
            default_audio_transition_duration: Tick(TICKS_PER_SECOND),
            ingest: IngestSettings::default(),
            title_safe: title_safe_default(),
            action_safe: action_safe_default(),
            capture_format: capture_format_default(),
            scratch: ScratchDisks::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub settings: ProjectSettings,
    pub root: Bin,
    pub items: BTreeMap<ItemId, ProjectItem>,
    /// Monotonic id source for all id types.
    pub next_id: u64,
    /// The project's LUT library (Lumetri Input LUT / Creative Look "Browse…"). LUT files are
    /// embedded, so projects render without the original files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub luts: Vec<ProjectLut>,
    /// Transcripts of media items (Text panel ▸ Transcript), keyed by the media item; word times
    /// are media time. Shared (`Arc`) so undo snapshots don't copy them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub transcripts: BTreeMap<ItemId, std::sync::Arc<Transcript>>,
    /// Search bins (File ▸ New ▸ Search Bin): saved Find queries listed in the Project panel.
    /// Schema v11.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_bins: Vec<SearchBin>,
    /// Source graphics (Graphics and Titles ▸ Upgrade to Source Graphic): shared layers by
    /// project item. Schema v12.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub source_graphics: BTreeMap<ItemId, SourceGraphic>,
    /// Narrations (Text to Speech): the script and voice settings each generated WAV item was
    /// made from, keyed by that item. Schema v14.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub narrations: BTreeMap<ItemId, Narration>,
    /// Project notes (Window ▸ Project Notes): free plain text kept with the project, for to-dos,
    /// instructions for whoever opens it next, frame numbers and the like. Schema v15.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

/// How a sequence is shown in the Timeline panel: its zoom, scroll position and track heights.
/// Each open sequence keeps its own (as Premiere's sequence tabs do).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequenceView {
    /// Zoom: points per second.
    pub pps: f64,
    /// Time at the left edge, in seconds.
    pub scroll: f64,
    /// Vertical scroll of the video and audio halves, in points.
    #[serde(default)]
    pub v_scroll: f32,
    #[serde(default)]
    pub a_scroll: f32,
    pub video_track_h: f32,
    pub audio_track_h: f32,
}

impl SequenceView {
    /// The view with every number brought into a usable range, or `None` when a number is not
    /// finite. Views are read from project files, so nothing in them is trusted.
    pub fn checked(self) -> Option<SequenceView> {
        let all = [self.pps, self.scroll, self.v_scroll as f64, self.a_scroll as f64, self.video_track_h as f64, self.audio_track_h as f64];
        if all.iter().any(|x| !x.is_finite()) {
            return None;
        }
        Some(SequenceView {
            pps: self.pps.clamp(1e-3, 1e5),
            scroll: self.scroll.clamp(0.0, 1e7),
            v_scroll: self.v_scroll.clamp(0.0, 1e6),
            a_scroll: self.a_scroll.clamp(0.0, 1e6),
            video_track_h: self.video_track_h.clamp(8.0, 600.0),
            audio_track_h: self.audio_track_h.clamp(8.0, 600.0),
        })
    }
}

/// What was open when a project was saved: the Timeline's sequence tabs in order, the active one,
/// how each sequence was shown and where its playhead was. Stored beside the project in its file,
/// not in it: it is not part of the edit and never an undo step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectView {
    #[serde(default)]
    pub open_sequences: Vec<ItemId>,
    #[serde(default)]
    pub active_sequence: Option<ItemId>,
    #[serde(default)]
    pub sequences: BTreeMap<ItemId, SequenceView>,
    /// Each sequence's playhead, so a sequence reopens where it was left. Files
    /// written before it existed have none, and every playhead starts at zero as it did then.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub playheads: BTreeMap<ItemId, Tick>,
}

impl ProjectView {
    /// The latest playhead a project file may ask for: ten million seconds (about 115 days), the
    /// same bound as a sequence view's scroll position. Playheads are read from project files, so
    /// nothing in them is trusted.
    pub const MAX_PLAYHEAD: Tick = Tick(10_000_000 * TICKS_PER_SECOND);
}

/// A LUT imported into the project (`lut.import`). Lumetri refers to it as `lib:<id>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectLut {
    pub id: String,
    pub name: String,
    /// Where it was imported from (informational).
    #[serde(default)]
    pub source_path: Option<String>,
    /// `cube` or `3dl`.
    pub format: String,
    /// The file's text.
    pub text: std::sync::Arc<str>,
}

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            settings: ProjectSettings::default(),
            root: Bin { id: BinId(0), name: name.into(), children: Vec::new() },
            items: BTreeMap::new(),
            next_id: 1,
            luts: Vec::new(),
            transcripts: BTreeMap::new(),
            search_bins: Vec::new(),
            source_graphics: BTreeMap::new(),
            narrations: BTreeMap::new(),
            notes: String::new(),
        }
    }

    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Add an item to a bin (root when None / not found).
    pub fn add_item(&mut self, name: &str, label: Label, kind: ItemKind, bin: Option<BinId>) -> ItemId {
        let id = ItemId(self.alloc_id());
        let created = id.0;
        self.items.insert(id, ProjectItem { id, name: name.into(), label, kind, metadata: BTreeMap::new(), created, split: SplitMarks::default() });
        if let Some(b) = bin.and_then(|b| self.root.find_bin_mut(b)) {
            b.children.push(BinEntry::Item(id));
        } else {
            self.root.children.push(BinEntry::Item(id));
        }
        id
    }

    pub fn add_bin(&mut self, name: &str, parent: Option<BinId>) -> BinId {
        let id = BinId(self.alloc_id());
        let bin = Bin { id, name: name.into(), children: Vec::new() };
        match parent.and_then(|p| self.root.find_bin_mut(p)) {
            Some(p) => p.children.push(BinEntry::Bin(bin)),
            None => self.root.children.push(BinEntry::Bin(bin)),
        }
        id
    }

    /// Move items, and bins named by id, into a bin (the root when `bin` is None). A bin goes with
    /// everything in it. Returns how many moved; unknown ids are skipped, and so is a bin that
    /// would land in itself or in one of its own sub-bins. Fails (returns None) when the target
    /// bin does not exist.
    pub fn move_to_bin(&mut self, items: &[ItemId], bin: Option<BinId>) -> Option<usize> {
        let target = bin.unwrap_or(self.root.id);
        self.root.find_bin(target)?;
        let mut moved = 0;
        for &id in items {
            let entry = if self.items.contains_key(&id) {
                if !self.root.remove_item(id) {
                    continue;
                }
                BinEntry::Item(id)
            } else {
                let b = BinId(id.0);
                // never the root, nor a bin that holds the target (or is it)
                if b == self.root.id || self.root.find_bin(b).is_none_or(|x| x.find_bin(target).is_some()) {
                    continue;
                }
                let Some(sub) = self.root.remove_bin(b) else { continue };
                BinEntry::Bin(sub)
            };
            match self.root.find_bin_mut(target) {
                Some(t) => t.children.push(entry),
                // cannot happen (the target is outside what was taken out), but never lose it
                None => self.root.children.push(entry),
            }
            moved += 1;
        }
        Some(moved)
    }

    pub fn item(&self, id: ItemId) -> Option<&ProjectItem> {
        self.items.get(&id)
    }
    pub fn item_mut(&mut self, id: ItemId) -> Option<&mut ProjectItem> {
        self.items.get_mut(&id)
    }
    pub fn sequence(&self, id: ItemId) -> Option<&Sequence> {
        self.items.get(&id).and_then(ProjectItem::as_sequence)
    }
    pub fn sequence_mut(&mut self, id: ItemId) -> Option<&mut Sequence> {
        self.items.get_mut(&id).and_then(ProjectItem::as_sequence_mut)
    }
    pub fn sequences(&self) -> impl Iterator<Item = &ProjectItem> {
        self.items.values().filter(|i| matches!(i.kind, ItemKind::Sequence(_)))
    }

    /// The part of a nested-sequence clip's timeline range that lies past the end of the nested
    /// sequence's contents: it shows nothing and is silent, and the Timeline hatches it. A nest
    /// keeps its length when its sequence gets shorter, so this is what is left to trim off.
    /// `None` for other clips and for nests that end within their contents.
    pub fn nest_overhang(&self, clip: &TrackItem) -> Option<TimeRange> {
        let contents = self.sequence(clip.item)?.duration();
        if let Some(held) = clip.frame_hold {
            return (held >= contents).then(|| clip.range());
        }
        let past = clip.source_out() - contents;
        if past <= Tick::ZERO {
            return None;
        }
        let speed = clip.speed.abs();
        if clip.source_in >= contents || !speed.is_finite() || speed < 1e-9 {
            return Some(clip.range());
        }
        // source time past the contents, as timeline time
        let len = Tick(((past.0 as f64 / speed).round() as i64).clamp(0, clip.duration.0.max(0)));
        if len <= Tick::ZERO {
            return None;
        }
        // a reversed clip plays its source's end first
        Some(if clip.reverse { TimeRange::new(clip.start, len) } else { TimeRange::new(clip.end() - len, len) })
    }

    /// A sequence that contains itself, directly or through the sequences nested in it (the lowest
    /// id of those on a cycle), or `None` when nesting is sound. Such a sequence has no frame to
    /// show, so edits that would make one are refused; only a damaged project file holds one.
    pub fn nest_cycle(&self) -> Option<ItemId> {
        // each sequence's nested sequences
        let mut nests: BTreeMap<ItemId, Vec<ItemId>> = BTreeMap::new();
        for it in self.items.values() {
            let ItemKind::Sequence(q) = &it.kind else { continue };
            let mut inner: Vec<ItemId> = q.all_tracks().flat_map(|t| t.items.iter().map(|i| i.item)).filter(|i| self.sequence(*i).is_some()).collect();
            inner.sort_unstable();
            inner.dedup();
            if !inner.is_empty() {
                nests.insert(it.id, inner);
            }
        }
        // drop sequences whose nests all lead out of the set, until none can be dropped: what is
        // left lies on a cycle or leads into one
        loop {
            let leaves: Vec<ItemId> = nests.iter().filter(|(_, inner)| inner.iter().all(|i| !nests.contains_key(i))).map(|(id, _)| *id).collect();
            if leaves.is_empty() {
                break;
            }
            for id in leaves {
                nests.remove(&id);
            }
        }
        // of those, the ones that can reach themselves
        nests.keys().copied().find(|start| {
            let mut seen: Vec<ItemId> = Vec::new();
            let mut todo: Vec<ItemId> = nests.get(start).cloned().unwrap_or_default();
            while let Some(id) = todo.pop() {
                if id == *start {
                    return true;
                }
                if !seen.contains(&id) {
                    seen.push(id);
                    todo.extend(nests.get(&id).into_iter().flatten().copied());
                }
            }
            false
        })
    }

    /// Create an empty sequence with `v` video and `a` audio tracks.
    pub fn new_sequence(&mut self, name: &str, settings: SequenceSettings, v: usize, a: usize, bin: Option<BinId>) -> ItemId {
        let mut seq = Sequence {
            settings,
            video_tracks: Vec::new(),
            audio_tracks: Vec::new(),
            markers: Vec::new(),
            mark_in: None,
            mark_out: None,
            work_area: None,
            start_timecode: 0,
            master_volume_db: 0.0,
            master_effects: Vec::new(),
            master_mixer: MixerStrip::default(),
            submix_tracks: Vec::new(),
            caption_tracks: Vec::new(),
            multicam: None,
            merged: None,
            split: SplitMarks::default(),
        };
        for i in 0..v {
            let id = TrackId(self.alloc_id());
            seq.video_tracks.push(Track::new(id, TrackKind::Video, format!("Video {}", i + 1)));
        }
        for i in 0..a {
            let id = TrackId(self.alloc_id());
            seq.audio_tracks.push(Track::new(id, TrackKind::Audio, format!("Audio {}", i + 1)));
        }
        self.add_item(name, Label::Forest, ItemKind::Sequence(std::sync::Arc::new(seq)), bin)
    }

    /// Re-base an item's media time: whatever was at media time `t` is at `t - delta` afterwards
    /// (a relink with Align Timecode, a consolidated file that starts later). Moves the source
    /// in-points, frame holds and keyframes of every track item that shows the item or one of its
    /// subclips, the subclip ranges, and the clip's own marks and markers.
    pub fn shift_media_time(&mut self, item: ItemId, delta: Tick) {
        if delta == Tick::ZERO {
            return;
        }
        let mut users = vec![item];
        for it in self.items.values_mut() {
            if let ItemKind::Subclip { parent, range, .. } = &mut it.kind
                && *parent == item
            {
                range.start -= delta;
                users.push(it.id);
            }
        }
        for it in self.items.values_mut() {
            if let ItemKind::Sequence(seq) = &mut it.kind {
                let seq = std::sync::Arc::make_mut(seq);
                for t in seq.all_tracks_mut() {
                    for ti in t.items.iter_mut().filter(|ti| users.contains(&ti.item)) {
                        ti.source_in -= delta;
                        if let Some(h) = &mut ti.frame_hold {
                            *h -= delta;
                        }
                        for e in &mut ti.effects {
                            for p in e.params.values_mut() {
                                for k in &mut p.keyframes {
                                    k.time -= delta;
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(m) = self.item_mut(item).and_then(|i| i.as_media_mut()) {
            m.mark_in = m.mark_in.map(|t| t - delta);
            m.mark_out = m.mark_out.map(|t| t - delta);
            m.markers.iter_mut().for_each(|mk| mk.start -= delta);
        }
    }

    /// Build a track item for `item` placed at `start` covering `source` range (media time).
    pub fn make_track_item(&mut self, item: ItemId, kind: TrackKind, start: Tick, source: TimeRange, seq_rate: FrameRate) -> Option<TrackItem> {
        let it = self.items.get(&item)?;
        let name = it.name.clone();
        let label = it.label;
        let mut effects = match kind {
            TrackKind::Video => effect::intrinsic_video(),
            TrackKind::Audio => effect::intrinsic_audio(),
        };
        // a source graphic edits in with its shared layers and settings
        let source_graphic = self.source_graphics.get(&item).filter(|_| kind == TrackKind::Video).cloned();
        if let Some(sg) = &source_graphic {
            effects.extend(sg.layers.iter().cloned());
        }
        // a multi-camera source sequence edits in as a multi-camera clip showing its first angle
        let multicam =
            it.as_sequence().and_then(|q| q.multicam.as_ref()).map(|m| MulticamSel { enabled: true, angle: m.first_video_angle().unwrap_or(0) as u32 });
        let source_channels = match (kind, &it.kind) {
            (TrackKind::Audio, ItemKind::Media(m)) => m.interpret.audio_channels.as_ref().and_then(|a| a.clips.first().cloned()).unwrap_or_default(),
            _ => Vec::new(),
        };
        let id = ClipId(self.alloc_id());
        let dur = seq_rate.snap_nearest(source.duration).max(seq_rate.frame_duration());
        Some(TrackItem {
            id,
            item,
            name,
            label,
            start,
            duration: dur,
            source_in: source.start,
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects,
            markers: Vec::new(),
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam,
            time_interpolation: TimeInterpolation::default(),
            hold_filters: false,
            field_options: None,
            source_channels,
            audio_stream: 0,
            graphic: source_graphic.and_then(|sg| sg.meta.map(Box::new)),
        })
    }

    /// The bare model as JSON. Project *files* add a schema-versioned envelope; read and write them
    /// with `filmcraft-format`, not with these.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
    pub fn from_json(s: &str) -> Result<Project, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

/// Resolve NaN "auto" point defaults (frame centre / source centre) in an effect instance.
pub fn resolve_auto_points(e: &mut EffectInstance, frame: (u32, u32), source: (u32, u32)) {
    resolve_auto_points_sized(e, frame, Some(source));
}

/// [`resolve_auto_points`] for a source whose size may not be known yet (`None`): the points
/// measured in the source (`anchor`) then stay "auto".
fn resolve_auto_points_sized(e: &mut EffectInstance, frame: (u32, u32), source: Option<(u32, u32)>) {
    for (k, p) in e.params.iter_mut() {
        if let ParamValue::Vec2(v) = &mut p.value {
            let Some((w, h)) = (if k == "anchor" { source } else { Some(frame) }) else { continue };
            if let Some((fx, fy)) = effect::auto_point(&e.effect, k) {
                if v.x.is_nan() {
                    v.x = w as f64 * fx;
                }
                if v.y.is_nan() {
                    v.y = h as f64 * fy;
                }
            }
            if v.x.is_nan() {
                v.x = w as f64 / 2.0;
            }
            if v.y.is_nan() {
                v.y = if k == "end" { h as f64 } else { h as f64 / 2.0 };
            }
        }
    }
    let _ = Vec2::ZERO;
}

impl Project {
    /// Resolve media through at most sixteen items, preserving the outermost subclip range.
    /// Missing parents, cycles and longer chains are unavailable media.
    pub fn resolve_media(&self, item: ItemId) -> Option<(ItemId, &MediaClip, Option<TimeRange>)> {
        let mut id = item;
        let mut range = None;
        for _ in 0..16 {
            match &self.item(id)?.kind {
                ItemKind::Media(media) => return Some((id, media, range)),
                ItemKind::Subclip { parent, range: span, .. } => {
                    range.get_or_insert(*span);
                    id = *parent;
                }
                _ => return None,
            }
        }
        None
    }

    /// Size in pixels of what an item shows: a media clip's picture, a sequence's frame, an
    /// adjustment layer or graphic; a subclip has the size of its parent. `None` without
    /// picture. This is the size the renderer centres a clip's "auto" anchor in.
    pub fn source_size(&self, item: ItemId) -> Option<(u32, u32)> {
        let mut id = item;
        // subclips of subclips: bounded, a damaged project could point a subclip at itself
        for _ in 0..16 {
            return match &self.item(id)?.kind {
                ItemKind::Media(m) => m.info.video.as_ref().map(|v| (v.width, v.height)),
                ItemKind::Sequence(q) => Some((q.settings.width, q.settings.height)),
                ItemKind::AdjustmentLayer { width, height, .. } | ItemKind::Graphic { width, height, .. } => Some((*width, *height)),
                ItemKind::Subclip { parent, .. } => {
                    id = *parent;
                    continue;
                }
            };
        }
        None
    }

    /// Pixel aspect ratio of what an item shows, in lowest terms: a media clip's
    /// ([`MediaClip::pixel_aspect`], Interpret Footage first), a nested sequence's, a subclip its
    /// parent's. `None` for items drawn in the pixels of whatever sequence they are in (graphics,
    /// adjustment layers) and for an item that doesn't exist.
    pub fn source_par(&self, item: ItemId) -> Option<(u32, u32)> {
        let mut id = item;
        // subclips of subclips: bounded, as in `source_size`
        for _ in 0..16 {
            return match &self.item(id)?.kind {
                ItemKind::Media(m) => Some(m.pixel_aspect()),
                ItemKind::Sequence(q) => Some(sane_par(q.settings.par)),
                ItemKind::AdjustmentLayer { .. } | ItemKind::Graphic { .. } => None,
                ItemKind::Subclip { parent, .. } => {
                    id = *parent;
                    continue;
                }
            };
        }
        None
    }

    /// [`Project::source_size`] in the pixels of a frame whose pixel aspect is `frame_par`
    /// ([`conformed_size`]): what Scale to Frame Size, Set to Frame Size and Fit / Fill fit into
    /// the frame. Items without a pixel aspect of their own take the frame's.
    pub fn conformed_source_size(&self, item: ItemId, frame_par: (u32, u32)) -> Option<(f64, f64)> {
        let size = self.source_size(item)?;
        Some(conformed_size(size, self.source_par(item).unwrap_or(frame_par), frame_par))
    }

    /// Resolve the "auto" (NaN) points of the clips in the sequences `sequences` selects, for
    /// clips that did not go through a placing command (an interchange import builds them with
    /// the effect defaults). Points measured in the frame use the sequence's size. `anchor` is
    /// measured in the clip's source ([`Project::source_size`], a subclip through its parent) and
    /// is resolved only when `size_is_final` says so for that source item (the media item for a
    /// subclip); otherwise it stays "auto", which the renderer centres in whatever size the media
    /// turns out to have. Points that are not NaN are never touched, so this can run again when
    /// more sizes are known.
    pub fn resolve_placed_auto_points(&mut self, sequences: impl Fn(ItemId) -> bool, size_is_final: impl Fn(&ProjectItem) -> bool) {
        let seq_ids: Vec<ItemId> = self.items.values().filter(|i| matches!(i.kind, ItemKind::Sequence(_)) && sequences(i.id)).map(|i| i.id).collect();
        for seq_id in seq_ids {
            let Some(seq) = self.sequence(seq_id) else { continue };
            // the source size of each clip that has an unresolved point, looked up before the edit
            let sources: BTreeMap<ClipId, Option<(u32, u32)>> = seq
                .all_tracks()
                .flat_map(|t| t.items.iter())
                .filter(|c| c.effects.iter().any(has_auto_point))
                .map(|c| (c.id, self.final_source_size(c.item, &size_is_final)))
                .collect();
            if sources.is_empty() {
                continue;
            }
            let Some(seq) = self.sequence_mut(seq_id) else { continue };
            let frame = (seq.settings.width, seq.settings.height);
            for clip in seq.all_tracks_mut().flat_map(|t| t.items.iter_mut()) {
                let Some(source) = sources.get(&clip.id) else { continue };
                for e in &mut clip.effects {
                    resolve_auto_points_sized(e, frame, *source);
                }
            }
        }
    }

    /// [`Project::source_size`] of `item` when the item that carries the size (the media item of
    /// a subclip) passes `size_is_final`.
    fn final_source_size(&self, item: ItemId, size_is_final: &impl Fn(&ProjectItem) -> bool) -> Option<(u32, u32)> {
        let mut id = item;
        for _ in 0..16 {
            let it = self.item(id)?;
            match &it.kind {
                ItemKind::Subclip { parent, .. } => id = *parent,
                _ => return if size_is_final(it) { self.source_size(id) } else { None },
            }
        }
        None
    }
}

fn has_auto_point(e: &EffectInstance) -> bool {
    e.params.values().any(|p| matches!(&p.value, ParamValue::Vec2(v) if v.x.is_nan() || v.y.is_nan()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_media::{DemoScene, MediaSource};

    fn demo_project() -> (Project, ItemId, ItemId) {
        let mut p = Project::new("Test");
        let src = filmcraft_media::generators::GeneratorSource::demo(DemoScene::OceanSunset);
        let info = src.info().clone();
        let clip = p.add_item(
            "Ocean_Sunset.mp4",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(Generator::Demo(DemoScene::OceanSunset)),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        let seq = p.new_sequence("Sequence 01", SequenceSettings::default(), 3, 3, None);
        (p, clip, seq)
    }

    #[test]
    fn json_roundtrip() {
        let (mut p, clip, seq) = demo_project();
        let rate = p.sequence(seq).unwrap().settings.frame_rate;
        let mut ti = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(5 * TICKS_PER_SECOND)), rate).unwrap();
        for e in &mut ti.effects {
            resolve_auto_points(e, (1920, 1080), (1920, 1080));
        }
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
        let s = p.to_json();
        let q = Project::from_json(&s).unwrap();
        assert_eq!(p, q);
        assert!(q.sequence(seq).unwrap().check().is_ok());
    }

    #[test]
    fn hostile_frame_rates_are_rejected_before_frame_arithmetic() {
        for frame_rate in [FrameRate { num: i64::MAX, den: i64::MAX }, FrameRate { num: 1, den: i64::from(u32::MAX) }] {
            let settings = SequenceSettings { frame_rate, ..Default::default() };
            assert!(settings.validate().is_err());
        }
        for (width, height) in [(7680, 4320), (15360, 8640), (16384, 8192), (16384, 16384), (32768, 8192)] {
            assert!(SequenceSettings { width, height, ..Default::default() }.validate().is_ok(), "{width}x{height}");
        }
        assert!(validate_frame_size(32768, 16384).is_err());
        assert!(validate_frame_size(65536, 1).is_err());
    }

    #[test]
    fn corrupt_timeline_times_are_rejected_before_end_arithmetic() {
        let (mut project, media, seq) = demo_project();
        let rate = project.sequence(seq).unwrap().settings.frame_rate;
        let range = TimeRange::new(Tick::ZERO, rate.tick_of(24));
        let first = project.make_track_item(media, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
        let second = project.make_track_item(media, TrackKind::Video, rate.tick_of(24), range, rate).unwrap();
        project.sequence_mut(seq).unwrap().video_tracks[0].items = vec![first, second];
        for start in [Tick(i64::MAX), Tick(i64::MIN), Tick::MAX] {
            project.sequence_mut(seq).unwrap().video_tracks[0].items[0].start = start;
            let result = std::panic::catch_unwind(|| project.sequence(seq).unwrap().check());
            assert!(result.is_ok());
            assert!(result.unwrap().is_err());
        }
        project.sequence_mut(seq).unwrap().video_tracks[0].items[0].start = Tick::ZERO;
        for mark in [Tick(i64::MAX), Tick(i64::MIN)] {
            project.sequence_mut(seq).unwrap().mark_out = Some(mark);
            assert!(project.sequence(seq).unwrap().check().is_err());
        }
    }

    #[test]
    fn point_params_keep_auto_through_json_and_no_other_point_reads_null() {
        // serde_json writes NaN as `null`; a point parameter reads it back as NaN ("auto")
        let json = serde_json::to_string(&ParamValue::Vec2(Vec2::new(f64::NAN, 540.0))).unwrap();
        assert_eq!(json, r#"{"Vec2":{"x":null,"y":540.0}}"#);
        let v = serde_json::from_str::<ParamValue>(&json).unwrap().as_vec2().unwrap();
        assert!(v.x.is_nan() && v.y == 540.0);
        let both = serde_json::from_str::<ParamValue>(r#"{"Vec2":{"x":null,"y":null}}"#).unwrap().as_vec2().unwrap();
        assert!(both.x.is_nan() && both.y.is_nan());
        assert_eq!(serde_json::from_str::<ParamValue>(r#"{"Vec2":{"x":1.5,"y":-2}}"#).unwrap(), ParamValue::Vec2(Vec2::new(1.5, -2.0)));
        // anything else is still an error, and so is a missing coordinate
        for bad in [r#"{"Vec2":{"x":"a","y":0}}"#, r#"{"Vec2":{"x":0}}"#, r#"{"Vec2":null}"#, r#"{"Float":null}"#] {
            assert!(serde_json::from_str::<ParamValue>(bad).is_err(), "{bad}");
        }
        // only point parameters: a `null` coordinate in a mask vertex (or any other point) is damage
        assert!(serde_json::from_str::<Vec2>(r#"{"x":null,"y":0}"#).is_err());
        let mut path = serde_json::to_value(MaskPath::ellipse(Vec2::new(100.0, 100.0), Vec2::new(50.0, 40.0))).unwrap();
        assert!(serde_json::from_value::<MaskPath>(path.clone()).is_ok());
        path["vertices"][0]["p"]["x"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<MaskPath>(path.clone()).is_err());
        assert!(serde_json::from_value::<ParamValue>(serde_json::json!({ "Path": path })).is_err());
    }

    #[test]
    fn media_resolution_bounds_depth_and_preserves_the_outer_range() {
        let (mut p, media, _) = demo_project();
        assert_eq!(p.resolve_media(media).map(|(id, _, range)| (id, range)), Some((media, None)));
        let mut parent = media;
        let mut last_range = TimeRange::default();
        for n in 0..15 {
            last_range = TimeRange::new(Tick(n * 1000), Tick(100));
            parent = p.add_item("Sub", Label::Iris, ItemKind::Subclip { parent, range: last_range, restrict_trims: false }, None);
        }
        assert_eq!(p.resolve_media(parent).map(|(id, _, range)| (id, range)), Some((media, Some(last_range))));
        let too_deep = p.add_item("Too deep", Label::Iris, ItemKind::Subclip { parent, range: last_range, restrict_trims: false }, None);
        assert!(p.resolve_media(too_deep).is_none());
        assert!(p.resolve_media(ItemId(u64::MAX)).is_none());
    }

    #[test]
    fn placed_auto_points_resolve_once_the_source_size_is_final() {
        let (mut p, clip, seq) = demo_project();
        let src = p.item(clip).and_then(|i| i.as_media()).and_then(|m| m.info.video.clone()).unwrap();
        let rate = p.sequence(seq).unwrap().settings.frame_rate;
        let second = TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND));
        let sub = p.add_item("Sub", Label::Iris, ItemKind::Subclip { parent: clip, range: second, restrict_trims: true }, None);
        let sub_of_sub = p.add_item("Sub 2", Label::Iris, ItemKind::Subclip { parent: sub, range: second, restrict_trims: true }, None);
        assert_eq!(p.source_size(clip), Some((src.width, src.height)));
        assert_eq!((p.source_size(sub), p.source_size(sub_of_sub)), (p.source_size(clip), p.source_size(clip)), "a subclip has its parent's size");
        assert_eq!(p.source_size(ItemId(987_654_321)), None);
        // clips whose Motion position / anchor were never resolved (as an interchange import builds them)
        let mut ids = Vec::new();
        for (n, item) in [clip, sub, sub_of_sub].into_iter().enumerate() {
            let ti = p.make_track_item(item, TrackKind::Video, Tick(n as i64 * TICKS_PER_SECOND), second, rate).unwrap();
            assert!(ti.effect("motion").unwrap().vec2_at("position", Tick::ZERO).x.is_nan());
            ids.push(ti.id);
            p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
        }
        let st = p.sequence(seq).unwrap().settings.clone();
        let point = |p: &Project, id: ClipId, k: &str| p.sequence(seq).unwrap().find_item(id).unwrap().1.effect("motion").unwrap().vec2_at(k, Tick::ZERO);
        let (frame_centre, source_centre) =
            (Vec2::new(st.width as f64 / 2.0, st.height as f64 / 2.0), Vec2::new(src.width as f64 / 2.0, src.height as f64 / 2.0));
        // sequences that are not selected are left alone
        let mut untouched = p.clone();
        untouched.resolve_placed_auto_points(|_| false, |_| true);
        assert!(point(&untouched, ids[0], "position").x.is_nan());
        // the source size is not final yet: the frame's points resolve, the anchor stays auto
        p.resolve_placed_auto_points(|_| true, |_| false);
        for id in &ids {
            assert_eq!(point(&p, *id, "position"), frame_centre);
            assert!(point(&p, *id, "anchor").x.is_nan() && point(&p, *id, "anchor").y.is_nan());
        }
        // ...and survives a save: NaN is written as `null` and read back as NaN
        let mut q = Project::from_json(&p.to_json()).unwrap();
        assert!(point(&q, ids[0], "anchor").x.is_nan());
        assert_eq!(q.to_json(), p.to_json());
        // final for the media item only: its clip and the clips of its subclips get the source centre
        q.resolve_placed_auto_points(|_| true, |i| i.id == clip);
        for id in &ids {
            assert_eq!((point(&q, *id, "position"), point(&q, *id, "anchor")), (frame_centre, source_centre));
        }
        assert!(!q.to_json().contains("null}}"), "no NaN point is left to write");
        // running it again changes nothing
        let once = q.clone();
        q.resolve_placed_auto_points(|_| true, |_| true);
        assert_eq!(q, once);
    }

    #[test]
    fn item_at_and_source_time() {
        let (mut p, clip, seq) = demo_project();
        let rate = FrameRate::FPS_24;
        let mut a = p.make_track_item(clip, TrackKind::Video, Tick(0), TimeRange::new(Tick(1000), rate.tick_of(48)), rate).unwrap();
        a.speed = 2.0;
        let t = &mut p.sequence_mut(seq).unwrap().video_tracks[0];
        t.items.push(a.clone());
        assert_eq!(t.item_at(rate.tick_of(10)).unwrap().id, a.id);
        assert!(t.item_at(rate.tick_of(48)).is_none());
        assert_eq!(a.source_time_at(rate.tick_of(10)), Tick(1000) + rate.tick_of(20));
    }

    #[test]
    fn projects_without_caption_tracks_load() {
        let (p, _, seq) = demo_project();
        let mut v: serde_json::Value = serde_json::from_str(&p.to_json()).unwrap();
        // a v1 file has no `caption_tracks` key
        let items = v["items"].as_object_mut().unwrap();
        for it in items.values_mut() {
            if let Some(s) = it["kind"].get_mut("Sequence") {
                s.as_object_mut().unwrap().remove("caption_tracks");
            }
        }
        let q = Project::from_json(&v.to_string()).unwrap();
        assert!(q.sequence(seq).unwrap().caption_tracks.is_empty());
    }

    #[test]
    fn caption_tracks_roundtrip() {
        let (mut p, _, seq) = demo_project();
        let id = TrackId(p.alloc_id());
        let mut ct = CaptionTrack::new(id, "Subtitle".into(), CaptionFormat::Cea608);
        ct.captions.push(Caption {
            id: ClipId(p.alloc_id()),
            start: Tick(10),
            duration: Tick(TICKS_PER_SECOND),
            text: "Hello\n<i>world</i>".into(),
            speaker: Some("Ann".into()),
            cue_id: Some("1".into()),
            settings: "line:90%".into(),
        });
        p.sequence_mut(seq).unwrap().caption_tracks.push(ct);
        let q = Project::from_json(&p.to_json()).unwrap();
        assert_eq!(p, q);
        assert_eq!(q.sequence(seq).unwrap().duration(), Tick(10 + TICKS_PER_SECOND));
    }

    #[test]
    fn bins() {
        let mut p = Project::new("x");
        let b = p.add_bin("Footage", None);
        let i = p.add_item("a", Label::Iris, ItemKind::AdjustmentLayer { width: 10, height: 10, rate: FrameRate::FPS_24, duration: Tick(1) }, Some(b));
        assert_eq!(p.root.parent_of(i), Some(b));
        assert!(p.root.remove_item(i));
        assert_eq!(p.root.parent_of(i), None);
    }

    #[test]
    fn a_bin_moves_into_another_bin_with_its_contents() {
        let mut p = Project::new("x");
        let outer = p.add_bin("Footage", None);
        let inner = p.add_bin("B-Roll", None);
        let deep = p.add_bin("Drone", Some(inner));
        let i = p.add_item("a", Label::Iris, ItemKind::AdjustmentLayer { width: 10, height: 10, rate: FrameRate::FPS_24, duration: Tick(1) }, Some(deep));
        assert_eq!(p.move_to_bin(&[ItemId(inner.0)], Some(outer)), Some(1));
        let o = p.root.find_bin(outer).unwrap();
        assert!(o.children.iter().any(|c| matches!(c, BinEntry::Bin(b) if b.id == inner)));
        assert!(!p.root.children.iter().any(|c| matches!(c, BinEntry::Bin(b) if b.id == inner)));
        assert_eq!(p.root.parent_of(i), Some(deep));
        assert!(o.find_bin(deep).is_some());
        // never into itself or its own sub-bin, and the root never moves
        assert_eq!(p.move_to_bin(&[ItemId(outer.0)], Some(outer)), Some(0));
        assert_eq!(p.move_to_bin(&[ItemId(outer.0)], Some(deep)), Some(0));
        assert_eq!(p.move_to_bin(&[ItemId(p.root.id.0)], Some(outer)), Some(0));
        assert!(p.root.find_bin(outer).unwrap().find_bin(deep).is_some());
        // and back out to the root
        assert_eq!(p.move_to_bin(&[ItemId(inner.0)], None), Some(1));
        assert!(p.root.children.iter().any(|c| matches!(c, BinEntry::Bin(b) if b.id == inner)));
    }

    #[test]
    fn project_clone_shares_sequences_until_mutated() {
        fn sequence_arc(p: &Project, id: ItemId) -> &std::sync::Arc<Sequence> {
            match &p.item(id).unwrap().kind {
                ItemKind::Sequence(q) => q,
                _ => unreachable!(),
            }
        }

        let (mut p, _, a) = demo_project();
        let b = p.new_sequence("Sequence 02", SequenceSettings::default(), 1, 1, None);
        let before = p.clone();

        assert!(std::sync::Arc::ptr_eq(sequence_arc(&p, a), sequence_arc(&before, a)));
        assert!(std::sync::Arc::ptr_eq(sequence_arc(&p, b), sequence_arc(&before, b)));

        p.sequence_mut(a).unwrap().mark_in = Some(Tick(1));

        assert!(!std::sync::Arc::ptr_eq(sequence_arc(&p, a), sequence_arc(&before, a)));
        assert!(std::sync::Arc::ptr_eq(sequence_arc(&p, b), sequence_arc(&before, b)));
        assert_eq!(before.sequence(a).unwrap().mark_in, None);
        assert_eq!(Project::from_json(&p.to_json()).unwrap(), p);
    }

    #[test]
    fn nest_cycle_finds_sequences_that_contain_themselves() {
        let (mut p, clip, a) = demo_project();
        let rate = p.sequence(a).unwrap().settings.frame_rate;
        let b = p.new_sequence("b", SequenceSettings::default(), 1, 1, None);
        let c = p.new_sequence("c", SequenceSettings::default(), 1, 1, None);
        let put = |p: &mut Project, outer: ItemId, inner: ItemId| {
            let it = p.make_track_item(inner, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(10)), rate).unwrap();
            let id = it.id;
            p.sequence_mut(outer).unwrap().video_tracks[0].items.push(it);
            id
        };
        // media and a chain of nests (a in b in c, a twice in c) are sound
        put(&mut p, a, clip);
        put(&mut p, b, a);
        put(&mut p, c, b);
        put(&mut p, c, a);
        assert_eq!(p.nest_cycle(), None);
        // c inside a closes the loop a → c → b → a
        let closing = put(&mut p, a, c);
        assert_eq!(p.nest_cycle(), Some(a));
        p.sequence_mut(a).unwrap().video_tracks[0].items.retain(|i| i.id != closing);
        assert_eq!(p.nest_cycle(), None);
        // a sequence directly inside itself; one that only leads into the loop is not reported
        put(&mut p, c, c);
        let d = p.new_sequence("d", SequenceSettings::default(), 1, 1, None);
        put(&mut p, d, c);
        assert_eq!(p.nest_cycle(), Some(c));
    }

    #[test]
    fn nest_overhang_is_the_part_past_the_contents() {
        let (mut p, clip, a) = demo_project();
        let rate = p.sequence(a).unwrap().settings.frame_rate;
        let f = |n: i64| rate.tick_of(n);
        // a holds 100 frames of media; b nests a for 150 frames starting at frame 10
        let media = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, f(100)), rate).unwrap();
        assert_eq!(p.nest_overhang(&media), None, "not a nest");
        p.sequence_mut(a).unwrap().video_tracks[0].items.push(media);
        let mut nest = p.make_track_item(a, TrackKind::Video, f(10), TimeRange::new(Tick::ZERO, f(150)), rate).unwrap();
        assert_eq!(p.nest_overhang(&nest), Some(TimeRange::new(f(110), f(50))));
        // within the contents, or exactly to their end: none
        nest.duration = f(100);
        assert_eq!(p.nest_overhang(&nest), None);
        // trimmed in: 60 frames of contents are left
        nest.source_in = f(40);
        assert_eq!(p.nest_overhang(&nest), Some(TimeRange::new(f(70), f(40))));
        // starting past the end: all of it
        nest.source_in = f(100);
        assert_eq!(p.nest_overhang(&nest), Some(nest.range()));
        // double speed uses the contents up twice as fast
        nest.source_in = Tick::ZERO;
        nest.speed = 2.0;
        assert_eq!(p.nest_overhang(&nest), Some(TimeRange::new(f(60), f(50))));
        // reversed, the empty part comes first
        nest.reverse = true;
        assert_eq!(p.nest_overhang(&nest), Some(TimeRange::new(f(10), f(50))));
        // a frame hold shows one frame throughout
        nest.reverse = false;
        nest.speed = 1.0;
        nest.frame_hold = Some(f(20));
        assert_eq!(p.nest_overhang(&nest), None);
        nest.frame_hold = Some(f(100));
        assert_eq!(p.nest_overhang(&nest), Some(nest.range()));
    }

    #[test]
    fn pixel_aspect_ratios_are_reduced_and_hostile_ones_are_square() {
        assert_eq!(checked_par((4, 3)), Some((4, 3)));
        assert_eq!(checked_par((40, 30)), Some((4, 3)));
        assert_eq!(checked_par((u32::MAX, u32::MAX)), Some((1, 1)));
        assert_eq!(checked_par((u32::MAX, u32::MAX - 1)), Some((u32::MAX, u32::MAX - 1)));
        assert_eq!(checked_par((8, 1)), Some((8, 1)));
        assert_eq!(checked_par((1, 8)), Some((1, 8)));
        for bad in [(0, 0), (0, 1), (1, 0), (9, 1), (1, 9), (u32::MAX, 1), (1, u32::MAX)] {
            assert_eq!(checked_par(bad), None, "{bad:?}");
            assert_eq!(sane_par(bad), (1, 1), "{bad:?}");
            assert_eq!(par_ratio(bad), 1.0, "{bad:?}");
        }
        assert!((par_ratio((4, 3)) - 4.0 / 3.0).abs() < 1e-12);
        // 1440 x 1080 with 4:3 pixels is 1920 x 1080 square pixels, and itself in a 4:3 frame
        assert_eq!(conformed_size((1440, 1080), (4, 3), (1, 1)), (1920.0, 1080.0));
        assert_eq!(conformed_size((1440, 1080), (4, 3), (8, 6)), (1440.0, 1080.0));
        assert_eq!(conformed_size((1920, 1080), (1, 1), (4, 3)), (1440.0, 1080.0));
        assert_eq!(conformed_size((u32::MAX, u32::MAX), (8, 1), (1, 8)), (u32::MAX as f64 * 64.0, u32::MAX as f64));
        assert_eq!(conformed_size((1440, 1080), (0, 0), (u32::MAX, 1)), (1440.0, 1080.0));
    }

    #[test]
    fn source_par_follows_interpret_footage_subclips_and_sequences() {
        let (mut p, clip, seq) = demo_project();
        assert_eq!(p.source_par(clip), Some((1, 1)));
        let sub = p.add_item("Sub", Label::Iris, ItemKind::Subclip { parent: clip, range: TimeRange::new(Tick::ZERO, Tick(100)), restrict_trims: false }, None);
        fn media(p: &mut Project, id: ItemId) -> &mut MediaClip {
            match &mut p.item_mut(id).unwrap().kind {
                ItemKind::Media(m) => m,
                _ => unreachable!(),
            }
        }
        // the file's ratio, reduced
        media(&mut p, clip).info.video.as_mut().unwrap().par = (40, 30);
        assert_eq!((p.source_par(clip), p.source_par(sub)), (Some((4, 3)), Some((4, 3))));
        assert_eq!(p.conformed_source_size(clip, (1, 1)), Some((1920.0 * 4.0 / 3.0, 1080.0)));
        // Interpret Footage wins over the file
        media(&mut p, clip).interpret.par = Some((2, 1));
        assert_eq!(p.source_par(sub), Some((2, 1)));
        // a nonsensical override falls back to the file, a nonsensical file value to square
        media(&mut p, clip).interpret.par = Some((0, 7));
        assert_eq!(p.source_par(clip), Some((4, 3)));
        media(&mut p, clip).info.video.as_mut().unwrap().par = (u32::MAX, 1);
        assert_eq!(p.source_par(clip), Some((1, 1)));
        // a nested sequence has its own ratio; graphics and missing items have none
        p.sequence_mut(seq).unwrap().settings.par = (0, 0);
        assert_eq!(p.source_par(seq), Some((1, 1)));
        p.sequence_mut(seq).unwrap().settings.par = (8, 6);
        assert_eq!(p.source_par(seq), Some((4, 3)));
        assert_eq!(p.conformed_source_size(seq, (1, 1)), Some((2560.0, 1080.0)));
        let adj = p.add_item("Adj", Label::Iris, ItemKind::AdjustmentLayer { width: 1440, height: 1080, rate: FrameRate::FPS_24, duration: Tick(100) }, None);
        assert_eq!(p.source_par(adj), None);
        assert_eq!(p.conformed_source_size(adj, (4, 3)), Some((1440.0, 1080.0)));
        assert_eq!(p.source_par(ItemId(987_654_321)), None);
    }
}
