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

mod item;
mod label;
mod sequence;
mod settings;
mod source;
mod track;
mod view;

pub use item::{AudioChannelMap, Bin, BinEntry, Interpretation, ItemKind, MediaClip, MediaIdentity, MediaRef, ProjectItem};
pub use label::Label;
pub(crate) use sequence::bounded_time_range;
pub use sequence::{
    MAX_FRAME_PIXELS, MAX_FRAME_SIDE, MAX_PIXEL_ASPECT, Sequence, SequenceSettings, SplitMarks, checked_par, conformed_size, par_ratio, sane_par,
    validate_frame_size,
};
pub use settings::{IngestAction, IngestSettings, ProjectSettings, ScratchDisks};
pub use source::resolve_auto_points;
pub use track::{
    AudioChannels, FieldOptions, FieldProcessing, Marker, MarkerKind, TimeInterpolation, Track, TrackItem, TrackKind, Transition, TransitionAlign,
};
pub use view::{MAX_LUT_TEXT_BYTES, ProjectLut, ProjectView, SequenceView};
id_type!(ItemId);
id_type!(BinId);
id_type!(TrackId);
id_type!(ClipId);
id_type!(TransitionId);
id_type!(MarkerId);

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

impl Default for Project {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Project {
    /// The highest `next_id` a loaded project may carry. Ids are handed out one at a time, so no
    /// amount of editing gets near it; a file past it is damaged, and refusing it keeps
    /// `alloc_id` from overflowing.
    pub const MAX_NEXT_ID: u64 = u64::MAX / 2;

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

    /// The largest id in use, of any kind. They all come from `next_id`, so a project read from a
    /// file must have its `next_id` past this. Ids that are only referenced count too (a clip whose
    /// item is gone): a new item taking that id would be picked up by the reference. The root bin
    /// counts, so there is always one.
    pub fn max_id(&self) -> u64 {
        let mut max = 0;
        let mut see = |id: u64| max = max.max(id);
        let mut bins = vec![&self.root];
        while let Some(bin) = bins.pop() {
            see(bin.id.0);
            for c in &bin.children {
                match c {
                    BinEntry::Item(i) => see(i.0),
                    BinEntry::Bin(sub) => bins.push(sub),
                }
            }
        }
        self.search_bins.iter().for_each(|b| see(b.id.0));
        // deleting an item leaves these entries behind: a new item taking that id would inherit them
        self.transcripts.keys().chain(self.source_graphics.keys()).chain(self.narrations.keys()).for_each(|i| see(i.0));
        // `lut.import` names LUTs `lut<id>`
        self.luts.iter().filter_map(|l| l.id.strip_prefix("lut")?.parse().ok()).for_each(&mut see);
        fn mixer(m: &MixerStrip) -> impl Iterator<Item = u64> + '_ {
            m.sends.iter().map(|s| s.target.0).chain(m.output.map(|t| t.0))
        }
        for (id, item) in &self.items {
            see(id.0);
            see(item.id.0);
            match &item.kind {
                ItemKind::Media(m) => m.markers.iter().for_each(|mk| see(mk.id.0)),
                ItemKind::Subclip { parent, .. } => see(parent.0),
                ItemKind::Sequence(s) => {
                    s.markers.iter().for_each(|mk| see(mk.id.0));
                    mixer(&s.master_mixer).for_each(&mut see);
                    for t in s.all_tracks().chain(&s.submix_tracks) {
                        see(t.id.0);
                        mixer(&t.mixer).for_each(&mut see);
                        for c in &t.items {
                            see(c.id.0);
                            see(c.item.0);
                            c.link.into_iter().chain(c.group).for_each(&mut see);
                            c.markers.iter().for_each(|mk| see(mk.id.0));
                        }
                        for tr in &t.transitions {
                            see(tr.id.0);
                            tr.from.into_iter().chain(tr.to).for_each(|c| see(c.0));
                        }
                    }
                    for t in &s.caption_tracks {
                        see(t.id.0);
                        t.captions.iter().for_each(|c| see(c.id.0));
                    }
                    for cam in s.multicam.iter().flat_map(|m| &m.cameras) {
                        cam.video_track.iter().chain(&cam.audio_tracks).for_each(|t| see(t.0));
                        cam.source.iter().for_each(|i| see(i.0));
                    }
                    if let Some(m) = &s.merged {
                        m.video.iter().chain(&m.audio).for_each(|i| see(i.0));
                    }
                }
                _ => {}
            }
        }
        max
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
        let source_channels = if kind == TrackKind::Audio {
            self.resolve_media(item).and_then(|(_, m, _)| m.interpret.audio_channels.as_ref()).and_then(|a| a.clips.first().cloned()).unwrap_or_default()
        } else {
            Vec::new()
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

#[cfg(test)]
mod tests;
