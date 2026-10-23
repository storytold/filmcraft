//! DaVinci Resolve project import (`.drp`, File ▸ Export Project in Resolve).
//!
//! A `.drp` is a ZIP archive (see [`zip`]) of XML documents that serialise Resolve's project
//! database: `project.xml` (the project), `MediaPool/**/MpFolder.xml` (bins, clips and timelines)
//! and `SeqContainer/<id>.xml` (the tracks and items of each timeline). Element names are the
//! database's class and field names (`Sm2MpFolder`, `Sm2Timeline`, `Sm2Sequence`, `Sm2TiTrack`,
//! `Sm2TiVideoClip`…); binary fields are hex (see [`blob`]). Blackmagic publishes no
//! specification: the layout was read off projects Resolve 19 and 20 exported, never from
//! Resolve itself. A single XML document (any of the above, or all of them concatenated under one
//! root) imports too.
//!
//! What is imported:
//!
//! | Resolve | FilmCraft |
//! |---|---|
//! | Media Pool folders, clips and timelines | bins, media items (offline until linked) and sequences |
//! | Timeline frame rate, resolution, start timecode | sequence settings and start time |
//! | Video / audio tracks, their names | tracks |
//! | Video and audio clips: position, duration, source in (sub-frame exact) | clips (video and audio of one take linked) |
//! | Text titles (rich text) | graphic clips with a text layer: text, first run's font, style and colour |
//! | Inspector ▸ Transform: zoom, position, rotation | Motion: scale (width), position, rotation |
//! | Inspector ▸ Volume | Volume level |
//! | Fade handles (video and audio) | Cross Dissolve / Constant Power from or to nothing |
//! | Cross Dissolve, Cross Fade ±3 dB / 0 dB and other transitions | the matching transition, else Cross Dissolve / Constant Power (reported) |
//!
//! Not imported, and reported: colour grades (each graded clip is counted), Fusion compositions,
//! other effects and generators (a generator without text becomes Transparent Video), compound,
//! multicam and other special clips (a gap), retiming, keyframes and markers.
//!
//! Resolve's `Start` is the timeline frame (counting from the timeline's start timecode, normally
//! 01:00:00:00); `In` is the source offset expressed in *timeline* frames, which is why media at
//! another rate has fractional values (`64|<double>`: 64 frames plus a fraction).

mod blob;
mod zip;

use std::collections::{HashMap, HashSet};

use filmcraft_geom::Vec2;
use filmcraft_media::{Generator, MediaKind};
use filmcraft_project::{
    BinId, ClipId, ItemId, ItemKind, Label, Param, ParamValue, Sequence, TrackItem, TrackKind, Transition, TransitionAlign, TransitionId, graphic,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use roxmltree::Node;

use crate::common::{Builder, MediaSpec, file_name, kind_from_ext, rate_from_f64, resolve_path, settings_for, transition_effect};
use crate::xml::{child, child_text, elements};
use crate::{Error, Format, ImportOptions, Imported, Report, Result};

use blob::params as P;

/// Whether `bytes` are a DaVinci Resolve project archive (a ZIP holding `project.xml` or
/// `SeqContainer/` documents).
pub fn sniff(bytes: &[u8]) -> bool {
    zip::sniff(bytes) && zip::names(bytes).is_ok_and(|n| n.iter().any(|n| n == "project.xml" || n.starts_with("SeqContainer/") || n.starts_with("MediaPool/")))
}

/// Times beyond this many seconds are damage, not a timeline.
const MAX_SECONDS: i64 = 1_000_000;
/// Most media pool clips / timeline items we read.
const MAX_OBJECTS: usize = 1_000_000;

// ---------------------------------------------------------------------------------------------
// Document model (owned, read from the XML)
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Doc {
    project_name: Option<String>,
    folders: Vec<Folder>,
    pool: Vec<PoolItem>,
    timelines: Vec<TimelineDoc>,
    sequences: HashMap<String, SeqDoc>,
    tracks: Vec<TrackDoc>,
}

struct Folder {
    id: String,
    name: String,
    parent: Option<String>,
}

struct PoolItem {
    id: String,
    name: String,
    folder: Option<String>,
    class: String,
    path: Option<String>,
    /// The timeline inside a compound or multicam clip.
    sequence: Option<String>,
}

struct TimelineDoc {
    name: String,
    sequence: String,
    folder: Option<String>,
}

#[derive(Default, Clone)]
struct SeqDoc {
    rate: Option<f64>,
    size: Option<(u32, u32)>,
    start_seconds: Option<f64>,
}

struct TrackDoc {
    sequence: String,
    kind: TrackKind,
    index: usize,
    name: String,
    items: Vec<ItemDoc>,
}

#[derive(Clone)]
struct ItemDoc {
    class: String,
    name: String,
    start: Option<(i64, f64)>,
    duration: Option<(i64, f64)>,
    source_in: (i64, f64),
    media_ref: Option<String>,
    path: Option<String>,
    media_rate: Option<f64>,
    media_seconds: Option<f64>,
    effects: String,
    pretty_type: String,
    graded: bool,
    media_track: i64,
    /// `FieldsBlob` (a multicam clip's active angle).
    fields: String,
    /// A Fusion clip's composition (`CompositionBA`).
    composition: String,
}

fn attr_id(n: Node) -> Option<String> {
    n.attribute("DbId").map(str::to_string)
}

fn nonempty(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// The nearest enclosing element named `tag`.
fn ancestor<'a, 'i>(n: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    n.ancestors().skip(1).find(|a| a.has_tag_name(tag))
}

/// The objects of a vector element (`<Vec><Element><Obj/></Element>…</Vec>` or `<Vec><Obj/>…`).
fn vec_objects<'a, 'i>(v: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    elements(v).filter_map(|e| if e.has_tag_name("Element") { elements(e).next() } else { Some(e) })
}

/// Resolve writes namespaced class names (`ListMgt::LmVersion`) as element names, which XML
/// parsers reject: rename `::` inside tag names to `__`.
fn fix_tag_names(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                in_tag = !matches!(chars.peek(), Some('!' | '?'));
                out.push(c);
            }
            ' ' | '\t' | '\r' | '\n' | '>' | '/' if in_tag && !(c == '/' && out.ends_with('<')) => {
                in_tag = false;
                out.push(c);
            }
            ':' if in_tag && chars.peek() == Some(&':') => {
                chars.next();
                out.push_str("__");
            }
            _ => out.push(c),
        }
    }
    out
}

impl Doc {
    fn read(&mut self, text: &str, report: &mut Report) -> Result<()> {
        let fixed = fix_tag_names(text.trim_start_matches('\u{feff}'));
        let doc = crate::xml::parse(&fixed).map_err(|e| Error::parse(Format::Drp, e))?;
        for n in doc.root().descendants().filter(Node::is_element) {
            if self.pool.len() + self.tracks.len() > MAX_OBJECTS {
                return Err(Error::parse(Format::Drp, "too many objects"));
            }
            let tag = n.tag_name().name();
            match tag {
                "SM_Project" => {
                    if self.project_name.is_none() {
                        self.project_name = nonempty(child_text(n, "ProjectName"));
                    }
                }
                "Sm2MpFolder" => {
                    let Some(id) = attr_id(n) else { continue };
                    let parent = nonempty(child_text(n, "MpFolder")).filter(|p| *p != id).or_else(|| ancestor(n, "Sm2MpFolder").and_then(attr_id));
                    self.folders.push(Folder { id, name: nonempty(child_text(n, "Name")).unwrap_or_else(|| "Bin".into()), parent });
                }
                "Sm2Timeline" => {
                    let seq = child(n, "Sequence");
                    let sequence = seq
                        .and_then(|s| elements(s).find(|e| e.has_tag_name("Sm2Sequence")).and_then(attr_id))
                        .or_else(|| nonempty(seq.and_then(|s| s.text())));
                    let Some(sequence) = sequence else { continue };
                    let holder = ancestor(n, "Sm2MpTimelineClip");
                    let folder = holder.and_then(|h| nonempty(child_text(h, "MpFolder"))).or_else(|| ancestor(n, "Sm2MpFolder").and_then(attr_id));
                    let name =
                        nonempty(child_text(n, "Name")).or_else(|| holder.and_then(|h| nonempty(child_text(h, "Name")))).unwrap_or_else(|| "Timeline".into());
                    self.timelines.push(TimelineDoc { name, sequence, folder });
                }
                "Sm2Sequence" => {
                    let Some(id) = attr_id(n) else { continue };
                    let s = SeqDoc {
                        rate: child_text(n, "FrameRate").and_then(blob::frame_rate),
                        size: child_text(n, "Resolution").and_then(blob::resolution),
                        start_seconds: child_text(n, "MediaExtents").and_then(blob::extents).map(|e| e.0),
                    };
                    self.sequences.insert(id, s);
                }
                "VideoTrackVec" | "AudioTrackVec" => {
                    let kind = if tag == "VideoTrackVec" { TrackKind::Video } else { TrackKind::Audio };
                    // tracks are numbered per timeline, in document order
                    let mut next_index: HashMap<String, usize> = HashMap::new();
                    for t in vec_objects(n).filter(|t| t.has_tag_name("Sm2TiTrack")) {
                        let sequence = nonempty(child_text(t, "Sequence")).or_else(|| ancestor(n, "Sm2Sequence").and_then(attr_id));
                        let Some(sequence) = sequence else {
                            report.warn("a track that belongs to no timeline was skipped");
                            continue;
                        };
                        let counter = next_index.entry(sequence.clone()).or_default();
                        let index = *counter;
                        *counter += 1;
                        let items = child(t, "Items").map(|i| vec_objects(i).map(read_item).collect()).unwrap_or_default();
                        self.tracks.push(TrackDoc { sequence, kind, index, name: nonempty(child_text(t, "UserDefinedName")).unwrap_or_default(), items });
                    }
                }
                _ if tag.starts_with("Sm2Mp")
                    && tag != "Sm2MpFolder"
                    && tag != "Sm2MpSmartFolder"
                    && child(n, "Name").is_some()
                    && child(n, "MpFolder").is_some() =>
                {
                    let Some(id) = attr_id(n) else { continue };
                    let path = n
                        .descendants()
                        .filter(|c| {
                            c.has_tag_name("Clip") && c.parent_element().is_some_and(|p| p.has_tag_name("BtVideoInfo") || p.has_tag_name("BtAudioInfo"))
                        })
                        .find_map(|c| c.text().and_then(blob::clip_path));
                    let folder = nonempty(child_text(n, "MpFolder")).or_else(|| ancestor(n, "Sm2MpFolder").and_then(attr_id));
                    let seq = child(n, "Sequence");
                    let sequence = seq
                        .and_then(|s| elements(s).find(|e| e.has_tag_name("Sm2Sequence")).and_then(attr_id))
                        .or_else(|| nonempty(seq.and_then(|s| s.text())));
                    self.pool.push(PoolItem {
                        id,
                        name: nonempty(child_text(n, "Name")).unwrap_or_else(|| "Clip".into()),
                        folder,
                        class: tag.to_string(),
                        path,
                        sequence,
                    });
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn read_item(n: Node) -> ItemDoc {
    let t = |name: &str| child_text(n, name).unwrap_or("");
    ItemDoc {
        class: n.tag_name().name().to_string(),
        name: t("Name").trim().to_string(),
        start: blob::frames(t("Start")),
        duration: blob::frames(t("Duration")),
        source_in: blob::frames(t("In")).unwrap_or((0, 0.0)),
        media_ref: nonempty(child_text(n, "MediaRef")),
        path: nonempty(child_text(n, "MediaFilePath")),
        media_rate: blob::frame_rate(t("MediaFrameRate")),
        media_seconds: blob::timemap_duration(t("MediaTimemapBA")),
        effects: t("EffectFiltersBA").to_string(),
        pretty_type: t("PrettyType").trim().to_string(),
        graded: n
            .descendants()
            .any(|v| v.tag_name().name() == "ListMgt__LmVersion" && child_text(v, "HasCorrection") == Some("true") && child_text(v, "VerType") == Some("0")),
        media_track: t("MediaTrackIdx").trim().parse().unwrap_or(0),
        fields: t("FieldsBlob").to_string(),
        composition: n.descendants().find(|c| c.has_tag_name("CompositionBA")).and_then(|c| c.text()).unwrap_or("").to_string(),
    }
}

// ---------------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------------

/// Import a `.drp` archive (or one of its XML documents).
pub fn import(bytes: &[u8], opts: &ImportOptions) -> Result<(Imported, Report)> {
    let mut report = Report::default();
    let mut doc = Doc::default();
    if zip::sniff(bytes) {
        let entries = zip::read(bytes, |n| n.to_ascii_lowercase().ends_with(".xml")).map_err(|e| Error::parse(Format::Drp, e))?;
        if entries.is_empty() {
            return Err(Error::parse(Format::Drp, "the archive holds no project documents"));
        }
        // project and media pool first, so timelines find their bins and clips
        let rank = |n: &str| {
            if n == "project.xml" {
                0
            } else if n.starts_with("MediaPool/") {
                1
            } else {
                2
            }
        };
        let mut entries = entries;
        entries.sort_by_key(|e| rank(&e.name));
        for e in entries {
            if e.name.eq_ignore_ascii_case("Gallery.xml") {
                continue;
            }
            doc.read(&String::from_utf8_lossy(&e.data), &mut report)?;
        }
    } else {
        doc.read(&String::from_utf8_lossy(bytes), &mut report)?;
    }
    if doc.timelines.is_empty() && doc.tracks.is_empty() && doc.pool.is_empty() {
        return Err(Error::Empty);
    }
    let name = doc.project_name.clone().or_else(|| opts.name.clone()).unwrap_or_else(|| "DaVinci Resolve Project".into());
    let mut imp = Imp {
        b: Builder::new(&name),
        opts,
        report: &mut report,
        bins: HashMap::new(),
        pool_media: HashMap::new(),
        graphic_items: HashMap::new(),
        graded: 0,
        seq_items: HashMap::new(),
        building: HashSet::new(),
    };
    imp.build_bins(&doc, &name);
    imp.build_pool(&doc);
    imp.build_timelines(&doc);
    if imp.graded > 0 {
        let n = imp.graded;
        imp.report.warn(format!("colour grades are not imported ({n} graded clip{})", if n == 1 { "" } else { "s" }));
    }
    let imported = imp.b.finish();
    if imported.project.items.is_empty() {
        return Err(Error::Empty);
    }
    Ok((imported, report))
}

struct Imp<'o, 'r> {
    b: Builder,
    opts: &'o ImportOptions,
    report: &'r mut Report,
    /// Resolve folder id → bin.
    bins: HashMap<String, BinId>,
    /// Media pool clip id → media item.
    pool_media: HashMap<String, ItemId>,
    /// (width, height, rate) → the graphic source item titles use.
    graphic_items: HashMap<(u32, u32, FrameRate), ItemId>,
    graded: usize,
    /// Resolve sequence id → its FilmCraft sequence (each timeline is built once).
    seq_items: HashMap<String, ItemId>,
    /// Sequences being built (a compound clip inside itself is damage, not a loop).
    building: HashSet<String>,
}

impl Imp<'_, '_> {
    fn root_bin(&self) -> Option<BinId> {
        self.bins.get("").copied()
    }

    fn bin_of(&self, folder: Option<&String>) -> Option<BinId> {
        folder.and_then(|f| self.bins.get(f)).copied().or_else(|| self.root_bin())
    }

    /// One bin for the project; Resolve's Master folder is that bin, its sub-folders are bins in it.
    fn build_bins(&mut self, doc: &Doc, name: &str) {
        let root = self.b.bin(name, None);
        self.bins.insert(String::new(), root);
        let ids: HashSet<&str> = doc.folders.iter().map(|f| f.id.as_str()).collect();
        // Masters: folders without a known parent.
        for f in doc.folders.iter().filter(|f| !f.parent.as_deref().is_some_and(|p| ids.contains(p))) {
            self.bins.insert(f.id.clone(), root);
        }
        // Children, parents first (bounded: a cycle leaves the rest under the root).
        for _ in 0..doc.folders.len() {
            let mut progressed = false;
            for f in &doc.folders {
                if self.bins.contains_key(&f.id) {
                    continue;
                }
                if let Some(parent) = f.parent.as_ref().and_then(|p| self.bins.get(p)).copied() {
                    let b = self.b.bin(&f.name, Some(parent));
                    self.bins.insert(f.id.clone(), b);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        for f in &doc.folders {
            self.bins.entry(f.id.clone()).or_insert(root);
        }
    }

    fn media_spec(path: &str, rate: Option<f64>, seconds: Option<f64>, size: (u32, u32)) -> MediaSpec {
        let kind = kind_from_ext(path);
        let rate = rate.map(rate_from_f64);
        MediaSpec {
            duration: seconds.filter(|s| *s < MAX_SECONDS as f64).map(Tick::from_seconds_f64),
            video: (kind != MediaKind::AudioOnly).then(|| (size.0, size.1, rate.unwrap_or(FrameRate::FPS_24))),
            audio: (kind != MediaKind::Still).then_some((48_000, 2)),
            start_tc: None,
            kind: Some(kind),
        }
    }

    fn file_item(&mut self, path: &str, name: &str, spec: &MediaSpec, bin: Option<BinId>) -> ItemId {
        let path = resolve_path(path, self.opts.base_dir.as_deref());
        self.b.file_media(&path, name, &path, spec, bin)
    }

    fn build_pool(&mut self, doc: &Doc) {
        // Media rates and durations are on the timeline items that use a clip.
        let mut seen: HashMap<&str, (Option<f64>, Option<f64>, Option<&str>)> = HashMap::new();
        for it in doc.tracks.iter().flat_map(|t| &t.items) {
            if let Some(r) = &it.media_ref {
                seen.entry(r.as_str()).or_insert((it.media_rate, it.media_seconds, it.path.as_deref()));
            }
        }
        let mut skipped: HashMap<&str, usize> = HashMap::new();
        for p in &doc.pool {
            match p.class.as_str() {
                "Sm2MpTimelineClip" => {}
                _ if p.sequence.is_some() => {}
                "Sm2MpVideoClip" | "Sm2MpAudioClip" => {
                    let used = seen.get(p.id.as_str());
                    let Some(path) = p.path.as_deref().or(used.and_then(|u| u.2)) else {
                        *skipped.entry("media pool clip without a file").or_default() += 1;
                        continue;
                    };
                    let spec = Self::media_spec(path, used.and_then(|u| u.0), used.and_then(|u| u.1), (1920, 1080));
                    let bin = self.bin_of(p.folder.as_ref());
                    let id = self.file_item(path, &p.name, &spec, bin);
                    self.pool_media.insert(p.id.clone(), id);
                }
                other => *skipped.entry(other.trim_start_matches("Sm2Mp")).or_default() += 1,
            }
        }
        let mut skipped: Vec<_> = skipped.into_iter().collect();
        skipped.sort();
        for (what, n) in skipped {
            self.report.warn(format!("{n} media pool item(s) of type {what} were not imported"));
        }
    }

    fn build_timelines(&mut self, doc: &Doc) {
        let mut timelines: Vec<(String, String, Option<String>)> =
            doc.timelines.iter().map(|t| (t.name.clone(), t.sequence.clone(), t.folder.clone())).collect();
        // Track sets whose timeline record is missing (a lone SeqContainer document).
        let known: HashSet<&str> = doc.timelines.iter().map(|t| t.sequence.as_str()).chain(doc.pool.iter().filter_map(|p| p.sequence.as_deref())).collect();
        let mut orphans: Vec<&str> = doc.tracks.iter().map(|t| t.sequence.as_str()).filter(|s| !known.contains(s)).collect();
        orphans.dedup();
        let mut orphan_seen = HashSet::new();
        for s in orphans {
            if orphan_seen.insert(s) {
                timelines.push((format!("Timeline {}", timelines.len() + 1), s.to_string(), None));
            }
        }
        for (name, seq_id, folder) in timelines {
            if self.seq_items.contains_key(&seq_id) {
                continue;
            }
            let bin = self.bin_of(folder.as_ref());
            if let Some(id) = self.timeline(doc, &seq_id, &name, bin, None) {
                self.b.top.push(id);
            }
        }
        // Compound clips no timeline uses still belong in their bins.
        for p in doc.pool.iter().filter(|p| p.class != "Sm2MpMulticamClip") {
            if let Some(seq_id) = &p.sequence
                && !self.seq_items.contains_key(seq_id)
            {
                let bin = self.bin_of(p.folder.as_ref());
                self.timeline(doc, seq_id, &p.name, bin, None);
            }
        }
    }

    /// The FilmCraft sequence of Resolve sequence `seq_id`, built on first use. `parent_rate` is
    /// the rate of the timeline a compound clip sits in (its own when it has none).
    fn timeline(&mut self, doc: &Doc, seq_id: &str, name: &str, bin: Option<BinId>, parent_rate: Option<FrameRate>) -> Option<ItemId> {
        // checked first: a sequence being built is already in `seq_items`
        if self.building.contains(seq_id) {
            self.report.warn(format!("\"{name}\" contains itself; the inner use was left as a gap"));
            return None;
        }
        if let Some(id) = self.seq_items.get(seq_id) {
            return Some(*id);
        }
        self.building.insert(seq_id.to_string());
        let info = doc.sequences.get(seq_id).cloned().unwrap_or_default();
        let tracks: Vec<&TrackDoc> = doc.tracks.iter().filter(|t| t.sequence == seq_id).collect();
        let r = self.build_sequence(doc, seq_id, name, &info, &tracks, bin, parent_rate);
        self.building.remove(seq_id);
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn build_sequence(
        &mut self,
        doc: &Doc,
        seq_id: &str,
        name: &str,
        info: &SeqDoc,
        tracks: &[&TrackDoc],
        bin: Option<BinId>,
        parent_rate: Option<FrameRate>,
    ) -> Option<ItemId> {
        let rate = match info.rate.map(rate_from_f64).or(parent_rate) {
            Some(r) => r,
            None => {
                // the rate most of its clips have, else 24 fps
                let mut counts: Vec<(FrameRate, usize)> = Vec::new();
                for r in tracks.iter().flat_map(|t| &t.items).filter_map(|i| i.media_rate).map(rate_from_f64) {
                    match counts.iter_mut().find(|c| c.0 == r) {
                        Some(c) => c.1 += 1,
                        None => counts.push((r, 1)),
                    }
                }
                let rate = counts.iter().max_by_key(|c| c.1).map(|c| c.0).unwrap_or(FrameRate::FPS_24);
                self.report.warn(format!("timeline \"{name}\" has no frame rate; {} assumed", rate.label()));
                rate
            }
        };
        let (w, h) = info.size.unwrap_or((1920, 1080));
        let fps = rate.as_f64();
        let start_frames = info.start_seconds.filter(|s| (0.0..MAX_SECONDS as f64).contains(s)).map(|s| (s * fps).round() as i64);
        // Without extents, the earliest item is at or after the start timecode.
        let start_frames = start_frames.unwrap_or_else(|| {
            let first = tracks.iter().flat_map(|t| &t.items).filter_map(|i| i.start.map(|s| s.0)).min().unwrap_or(0);
            let hour = (3600.0 * fps).round() as i64;
            if first >= hour { hour } else { 0 }
        });
        let id = self.b.reserve_sequence(name, settings_for(rate, w, h, false), bin);
        self.seq_items.insert(seq_id.to_string(), id);
        let mut seq = crate::common::empty_sequence(settings_for(rate, w, h, false));
        seq.start_timecode = start_frames;
        let to_ticks = |(f, frac): (i64, f64)| -> Option<Tick> {
            let t = rate.tick_of(f) + Tick((rate.frame_duration().0 as f64 * frac).round() as i64);
            (t.0.unsigned_abs() <= (MAX_SECONDS * TICKS_PER_SECOND) as u64).then_some(t)
        };
        let origin = rate.tick_of(start_frames);
        let mut ordered: Vec<&TrackDoc> = tracks.to_vec();
        ordered.sort_by_key(|t| (t.kind == TrackKind::Audio, t.index));
        for t in &ordered {
            self.b.ensure_tracks(&mut seq, t.kind, t.index.saturating_add(1).min(1000));
            if !t.name.is_empty()
                && let Some(tr) = seq.tracks_mut(t.kind).get_mut(t.index)
            {
                tr.name = t.name.clone();
            }
        }
        let mut unsupported: HashMap<String, usize> = HashMap::new();
        for t in &ordered {
            let ti = t.index.min(999);
            let mut transitions = Vec::new();
            for it in &t.items {
                let (Some(start), Some(dur)) = (it.start.and_then(to_ticks), it.duration.and_then(to_ticks)) else {
                    self.report.warn(format!("\"{}\" has no valid position and was skipped", it.name));
                    continue;
                };
                let start = start - origin;
                if dur <= Tick::ZERO {
                    continue;
                }
                if it.graded {
                    self.graded += 1;
                }
                match it.class.as_str() {
                    "Sm2TiTransition" => transitions.push((it, start, dur)),
                    "Sm2TiVideoClip" | "Sm2TiAudioClip" => {
                        if let Some(ti_item) = self.timeline_clip(doc, it, t.kind, start, dur, &to_ticks, (w, h), rate) {
                            self.put(&mut seq, t.kind, ti, ti_item);
                        }
                    }
                    "Sm2TiGenerator" if t.kind == TrackKind::Video => {
                        let clip = self.generator(it, start, dur, (w, h), rate);
                        self.put(&mut seq, t.kind, ti, clip);
                    }
                    other => {
                        let what = if it.pretty_type.is_empty() { other.trim_start_matches("Sm2Ti").to_string() } else { it.pretty_type.clone() };
                        *unsupported.entry(what).or_default() += 1;
                    }
                }
            }
            for (it, start, dur) in transitions {
                self.transition(&mut seq, t.kind, ti, &it.name, start, dur);
            }
            self.fades(&mut seq, t.kind, ti, t, origin, &to_ticks);
        }
        let mut unsupported: Vec<_> = unsupported.into_iter().collect();
        unsupported.sort();
        for (what, n) in unsupported {
            self.report.warn(format!("{n} timeline item(s) of type {what} in \"{name}\" are not supported and were left as gaps"));
        }
        self.link(&mut seq);
        self.b.put_sequence(id, seq);
        Some(id)
    }

    /// Place a clip on its track, or on the next free one if Resolve's layout overlaps there.
    fn put(&mut self, seq: &mut Sequence, kind: TrackKind, index: usize, item: TrackItem) {
        let placed = self.b.place(seq, kind, index, item);
        if placed != index {
            self.report.info("overlapping clips were moved to the next free track");
        }
    }

    fn media_clip(
        &mut self,
        it: &ItemDoc,
        kind: TrackKind,
        start: Tick,
        dur: Tick,
        to_ticks: &dyn Fn((i64, f64)) -> Option<Tick>,
        size: (u32, u32),
    ) -> Option<TrackItem> {
        let item = match it.media_ref.as_ref().and_then(|r| self.pool_media.get(r)).copied() {
            Some(m) => m,
            None => {
                let path = it.path.as_deref()?;
                let spec = Self::media_spec(path, it.media_rate, it.media_seconds, size);
                let root = self.root_bin();
                self.file_item(path, file_name(path), &spec, root)
            }
        };
        let source_in = to_ticks(it.source_in).unwrap_or(Tick::ZERO).max(Tick::ZERO);
        let name = if it.name.is_empty() { file_name(it.path.as_deref().unwrap_or("Clip")).to_string() } else { it.name.clone() };
        let mut ti = self.b.clip(item, kind, &name, start, dur, source_in);
        if kind == TrackKind::Audio {
            ti.audio_stream = usize::try_from(it.media_track).unwrap_or(0).min(64);
        } else {
            // Resolve scales every clip to fit the timeline (Input Scaling: Scale entire image to fit).
            ti.scale_to_frame = true;
        }
        self.apply_effects(&mut ti, &it.effects, kind, size);
        Some(ti)
    }

    /// A video or audio clip of a timeline: media, a compound clip (a nested sequence), a
    /// multicam clip (its active angle) or a Fusion clip (a title, else a placeholder).
    #[allow(clippy::too_many_arguments)]
    fn timeline_clip(
        &mut self,
        doc: &Doc,
        it: &ItemDoc,
        kind: TrackKind,
        start: Tick,
        dur: Tick,
        to_ticks: &dyn Fn((i64, f64)) -> Option<Tick>,
        size: (u32, u32),
        rate: FrameRate,
    ) -> Option<TrackItem> {
        let pool = it.media_ref.as_ref().and_then(|r| doc.pool.iter().find(|p| &p.id == r));
        if let Some(p) = pool
            && let Some(seq_id) = &p.sequence
        {
            if p.class == "Sm2MpMulticamClip" {
                return self.multicam(doc, it, seq_id, kind, start, dur, to_ticks, size, rate);
            }
            let bin = self.bin_of(p.folder.as_ref());
            let nested = self.timeline(doc, seq_id, &p.name, bin, Some(rate))?;
            let source_in = to_ticks(it.source_in).unwrap_or(Tick::ZERO).max(Tick::ZERO);
            let name = if it.name.is_empty() { p.name.clone() } else { it.name.clone() };
            let mut ti = self.b.clip(nested, kind, &name, start, dur, source_in);
            if kind == TrackKind::Video {
                ti.scale_to_frame = true;
            }
            self.apply_effects(&mut ti, &it.effects, kind, size);
            return Some(ti);
        }
        if let Some(ti) = self.media_clip(it, kind, start, dur, to_ticks, size) {
            return Some(ti);
        }
        if kind == TrackKind::Video {
            return Some(self.generator(it, start, dur, size, rate));
        }
        self.report.warn(format!("audio clip \"{}\" has no media file and was skipped", it.name));
        None
    }

    /// A multicam clip becomes a clip of its active angle's media (Resolve names the angle in the
    /// clip's `FieldsBlob`; the angles are the tracks of the multicam's own timeline).
    #[allow(clippy::too_many_arguments)]
    fn multicam(
        &mut self,
        doc: &Doc,
        it: &ItemDoc,
        seq_id: &str,
        kind: TrackKind,
        start: Tick,
        dur: Tick,
        to_ticks: &dyn Fn((i64, f64)) -> Option<Tick>,
        size: (u32, u32),
        rate: FrameRate,
    ) -> Option<TrackItem> {
        let info = doc.sequences.get(seq_id).cloned().unwrap_or_default();
        let inner_fps = info.rate.map(rate_from_f64).unwrap_or(rate).as_f64();
        let fps = rate.as_f64();
        let inner_start = info.start_seconds.filter(|s| (0.0..MAX_SECONDS as f64).contains(s)).unwrap_or(0.0);
        let mut angles: Vec<&TrackDoc> = doc.tracks.iter().filter(|t| t.sequence == seq_id && t.kind == kind).collect();
        angles.sort_by_key(|t| t.index);
        if angles.is_empty() {
            self.report.warn(format!("multicam clip \"{}\" has no angles and was skipped", it.name));
            return None;
        }
        let names = blob::strings(&it.fields);
        let by_name = angles.iter().position(|t| !t.name.is_empty() && names.contains(&t.name));
        let by_number = || {
            names.iter().find_map(|n| {
                let k = n.strip_prefix("Camera ").or_else(|| n.strip_prefix("Angle "))?;
                k.trim().parse::<usize>().ok()?.checked_sub(1).filter(|k| *k < angles.len())
            })
        };
        let angle = match by_name.or_else(by_number) {
            Some(a) => a,
            None => {
                self.report.warn("a multicam clip names no angle; the first angle was used");
                0
            }
        };
        let secs = |(f, frac): (i64, f64), fps: f64| (f as f64 + frac) / fps;
        let pos = inner_start + secs(it.source_in, fps);
        let clip_like = |i: &&ItemDoc| matches!(i.class.as_str(), "Sm2TiVideoClip" | "Sm2TiAudioClip");
        let inner = angles.get(angle)?.items.iter().filter(clip_like).find(|i| {
            let (Some(s), Some(d)) = (i.start, i.duration) else { return false };
            let s = secs(s, inner_fps);
            s <= pos + 1e-6 && pos < s + secs(d, inner_fps)
        });
        let Some(inner) = inner else {
            self.report.warn(format!("multicam clip \"{}\": the active angle has no clip there; left as a gap", it.name));
            return None;
        };
        let inner_start_of_clip = secs(inner.start.unwrap_or((0, 0.0)), inner_fps);
        // the source position in the angle's media, in this timeline's frames
        let f = ((secs(inner.source_in, inner_fps) + pos - inner_start_of_clip) * fps).max(0.0);
        let (mut whole, mut frac) = (f.floor(), f - f.floor());
        if frac > 0.9999 {
            (whole, frac) = (whole + 1.0, 0.0);
        }
        let mut flat = inner.clone();
        flat.source_in = (whole as i64, frac);
        flat.effects = it.effects.clone();
        self.report.info("multicam clips were flattened to their active angle");
        self.media_clip(&flat, kind, start, dur, to_ticks, size)
    }

    fn graphic_source(&mut self, w: u32, h: u32, rate: FrameRate) -> ItemId {
        if let Some(id) = self.graphic_items.get(&(w, h, rate)) {
            return *id;
        }
        let bin = self.root_bin();
        let id = self.b.p.add_item("Graphic", Label::Rose, ItemKind::Graphic { width: w, height: h, rate }, bin);
        self.graphic_items.insert((w, h, rate), id);
        id
    }

    fn generator(&mut self, it: &ItemDoc, start: Tick, dur: Tick, (w, h): (u32, u32), rate: FrameRate) -> TrackItem {
        let name = if it.name.is_empty() { "Title".to_string() } else { it.name.clone() };
        if let Some(t) = blob::title_text(&it.effects).or_else(|| blob::fusion_title(&it.composition)) {
            let src = self.graphic_source(w, h, rate);
            let mut ti = self.b.clip(src, TrackKind::Video, &name, start, dur, Tick::ZERO);
            let size = t.size.map(|s| s * f64::from(w)).unwrap_or(f64::from(h) / 10.0).clamp(1.0, 2000.0);
            let mut layer = graphic::new_text_layer(&t.text, Vec2::new(f64::from(w) / 2.0, f64::from(h) / 2.0), size);
            layer.params.insert("align".into(), Param::new(ParamValue::Choice(1)));
            if let Some(f) = t.font {
                layer.params.insert("font".into(), Param::new(ParamValue::Text(f)));
            }
            if let Some(s) = t.style {
                layer.params.insert("font_style".into(), Param::new(ParamValue::Text(s)));
            }
            if let Some(c) = t.color {
                layer.params.insert("fill_color".into(), Param::new(ParamValue::Color(c)));
            }
            ti.effects.push(layer);
            self.apply_effects(&mut ti, &it.effects, TrackKind::Video, (w, h));
            self.report.info("titles were imported as text graphics with their text, font and colour; animation and layout were not");
            return ti;
        }
        let what = if it.class == "Sm2TiGenerator" { "generator" } else { "Fusion clip" };
        self.report.warn(format!("{what} \"{name}\" has no text FilmCraft can use and was imported as Transparent Video"));
        let key = format!("drp-generator:{name}");
        let spec = MediaSpec { duration: None, video: Some((w, h, rate)), audio: None, start_tc: None, kind: None };
        let root = self.root_bin();
        let item = self.b.generator_media(&key, &name, Generator::TransparentVideo, &spec, root);
        self.b.clip(item, TrackKind::Video, &name, start, dur, Tick::ZERO)
    }

    fn apply_effects(&mut self, ti: &mut TrackItem, hex_text: &str, kind: TrackKind, (w, h): (u32, u32)) {
        if hex_text.trim().is_empty() {
            return;
        }
        let fx = blob::effect_params(hex_text);
        let set = |ti: &mut TrackItem, effect: &str, p: &str, v: ParamValue| {
            if let Some(e) = ti.effect_mut(effect) {
                e.params.insert(p.to_string(), Param::new(v));
            }
        };
        if kind == TrackKind::Video
            && let Some(t) = fx.get(&P::TRANSFORM)
        {
            let zx = t.get(&P::ZOOM_X).copied().filter(|z| z.is_finite() && *z > 0.0 && *z < 100.0);
            let zy = t.get(&P::ZOOM_Y).copied().filter(|z| z.is_finite() && *z > 0.0 && *z < 100.0);
            match (zx, zy) {
                (Some(x), Some(y)) if (x - y).abs() > 1e-9 => {
                    set(ti, "motion", "uniform_scale", ParamValue::Bool(false));
                    set(ti, "motion", "scale", ParamValue::Float(y * 100.0));
                    set(ti, "motion", "scale_width", ParamValue::Float(x * 100.0));
                }
                (Some(z), _) | (None, Some(z)) => set(ti, "motion", "scale", ParamValue::Float(z * 100.0)),
                (None, None) => {}
            }
            let px = t.get(&P::POSITION_X).copied().filter(|v| v.is_finite() && v.abs() < 100.0);
            let py = t.get(&P::POSITION_Y).copied().filter(|v| v.is_finite() && v.abs() < 100.0);
            if px.is_some() || py.is_some() {
                // Resolve: offsets from the centre in frame widths / heights, Y up.
                let (fw, fh) = (f64::from(w), f64::from(h));
                let pos = Vec2::new(fw / 2.0 + px.unwrap_or(0.0) * fw, fh / 2.0 - py.unwrap_or(0.0) * fh);
                set(ti, "motion", "position", ParamValue::Vec2(pos));
            }
            if let Some(r) = t.get(&P::ROTATION).copied().filter(|v| v.is_finite() && v.abs() < 1e6) {
                // Resolve turns counter-clockwise for positive angles, FilmCraft clockwise.
                set(ti, "motion", "rotation", ParamValue::Float(-r));
            }
        }
        if kind == TrackKind::Audio
            && let Some(v) = fx.get(&P::AUDIO).and_then(|a| a.get(&P::AUDIO_VOLUME_DB)).copied().filter(|v| v.is_finite())
        {
            set(ti, "volume", "level", ParamValue::Float(v.clamp(-287.5, 15.0)));
        }
    }

    fn transition(&mut self, seq: &mut Sequence, kind: TrackKind, ti: usize, name: &str, s: Tick, d: Tick) {
        let e = s + d;
        let audio = kind == TrackKind::Audio;
        let n = name.trim().to_ascii_lowercase().replace(" db", "db");
        let effect = match n.as_str() {
            "additive dissolve" | "smooth cut" => {
                self.report.info(format!("transition \"{name}\" was imported as Cross Dissolve"));
                transition_effect("Cross Dissolve", audio, self.report)
            }
            "cross fade +3db" => transition_effect("cross fade (+3db)", true, self.report),
            "cross fade 0db" | "cross fade -3db" => transition_effect("cross fade (0db)", true, self.report),
            "dip to color dissolve" => transition_effect("dip to color dissolve", audio, self.report),
            _ => transition_effect(name, audio, self.report),
        };
        let Some(track) = seq.tracks_mut(kind).get_mut(ti) else { return };
        let mut from = track.items.iter().filter(|i| i.end() >= s && i.end() <= e).max_by_key(|i| i.end()).map(|i| (i.id, i.end()));
        let mut to = track.items.iter().filter(|i| i.start >= s && i.start <= e).min_by_key(|i| i.start).map(|i| (i.id, i.start));
        if let (Some((_, fe)), Some((_, ts))) = (from, to)
            && fe != ts
        {
            // Two edits inside one transition: keep the side nearer its centre.
            let mid = s + Tick((e - s).0 / 2);
            if (fe - mid).0.abs() <= (ts - mid).0.abs() { to = None } else { from = None }
        }
        let cut = match (from, to) {
            (Some((_, c)), _) | (None, Some((_, c))) => c,
            (None, None) => {
                self.report.warn(format!("transition \"{name}\" has no adjacent clip and was skipped"));
                return;
            }
        };
        let align = if cut == s {
            TransitionAlign::StartAtCut
        } else if cut == e {
            TransitionAlign::EndAtCut
        } else {
            TransitionAlign::CenterAtCut
        };
        let id = TransitionId(self.b.alloc());
        track.transitions.push(Transition { id, effect, start: s, duration: d, from: from.map(|f| f.0), to: to.map(|t| t.0), align, reverse: false });
        track.sort();
    }

    /// Fade handles become transitions from or to nothing at the clip's edges (unless a
    /// transition is already there).
    fn fades(&mut self, seq: &mut Sequence, kind: TrackKind, ti: usize, t: &TrackDoc, origin: Tick, to_ticks: &dyn Fn((i64, f64)) -> Option<Tick>) {
        let (effect_id, fade_in, fade_out, name) = match kind {
            TrackKind::Video => (P::VIDEO_FADE, P::VIDEO_FADE_IN, P::VIDEO_FADE_OUT, "Cross Dissolve"),
            TrackKind::Audio => (P::AUDIO, P::AUDIO_FADE_IN, P::AUDIO_FADE_OUT, "cross fade (+3db)"),
        };
        let rate = seq.settings.frame_rate;
        let mut wanted: Vec<(Tick, Tick, bool)> = Vec::new();
        for it in &t.items {
            if it.effects.trim().is_empty() {
                continue;
            }
            let fx = blob::effect_params(&it.effects);
            let Some(f) = fx.get(&effect_id) else { continue };
            let (Some(start), Some(dur)) = (it.start.and_then(to_ticks), it.duration.and_then(to_ticks)) else { continue };
            let start = start - origin;
            let frames = |id| f.get(&id).copied().filter(|v| v.is_finite() && *v >= 0.5 && *v < 1e6).map(|v| rate.tick_of(v.round() as i64).min(dur));
            if let Some(d) = frames(fade_in) {
                wanted.push((start, d, true));
            }
            if let Some(d) = frames(fade_out) {
                wanted.push((start + dur, d, false));
            }
        }
        for (edge, d, is_in) in wanted {
            let effect = transition_effect(name, kind == TrackKind::Audio, self.report);
            let id = TransitionId(self.b.alloc());
            let Some(track) = seq.tracks_mut(kind).get_mut(ti) else { return };
            let (s, clip): (Tick, Option<ClipId>) = if is_in {
                (edge, track.items.iter().find(|i| i.start == edge).map(|i| i.id))
            } else {
                (edge - d, track.items.iter().find(|i| i.end() == edge).map(|i| i.id))
            };
            let Some(clip) = clip else { continue };
            if track.transitions.iter().any(|x| x.start < s + d && s < x.end()) {
                continue;
            }
            let (from, to, align) = if is_in { (None, Some(clip), TransitionAlign::StartAtCut) } else { (Some(clip), None, TransitionAlign::EndAtCut) };
            track.transitions.push(Transition { id, effect, start: s, duration: d, from, to, align, reverse: false });
            track.sort();
        }
    }

    /// Link the video and audio clips of one take: same media, position, length and source in.
    fn link(&mut self, seq: &mut Sequence) {
        let key = |i: &TrackItem| (i.item, i.start, i.duration, i.source_in);
        let videos: HashSet<_> = seq.video_tracks.iter().flat_map(|t| &t.items).map(key).collect();
        let mut groups: HashMap<(ItemId, Tick, Tick, Tick), u64> = HashMap::new();
        for t in &seq.audio_tracks {
            for i in &t.items {
                if videos.contains(&key(i)) && !groups.contains_key(&key(i)) {
                    groups.insert(key(i), self.b.link_id());
                }
            }
        }
        for t in seq.video_tracks.iter_mut().chain(seq.audio_tracks.iter_mut()) {
            for i in &mut t.items {
                if let Some(g) = groups.get(&key(i)) {
                    i.link = Some(*g);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
