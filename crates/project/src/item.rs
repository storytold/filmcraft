use super::label::Label;
use super::sequence::{Sequence, SplitMarks, checked_par};
use super::track::{AudioChannels, Marker};
use super::*;

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
