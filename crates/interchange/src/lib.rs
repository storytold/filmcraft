//! Timeline interchange for FilmCraft: import and export of
//!
//! - **CMX 3600 EDL** (`.edl`) with the common comment extensions (`* FROM CLIP NAME:`,
//!   `* SOURCE FILE:`, `* LOC:`), `M2` speed lines, dissolves, wipes, key events, audio channels and
//!   drop-frame timecode ([`edl`]);
//! - **Final Cut Pro 7 XML** (`xmeml` v4/v5, the dialect Premiere Pro reads and writes) ([`fcp7`]);
//! - **FCPXML** 1.9–1.11 ([`fcpxml`]);
//! - **OpenTimelineIO** JSON (`.otio`) ([`otio`]);
//! - **AAF** (`.aaf`, Edit Protocol, structured storage) ([`aaf`]);
//! - **OMF Interchange 2.0** (`.omf`, Bento) for audio post ([`omf`]);
//! - **DaVinci Resolve projects** (`.drp`, import only) ([`drp`]).
//!
//! AAF and OMF can embed or consolidate audio: [`essence::audio_needs`] lists what the caller
//! (the engine) has to render, and [`essence::MediaOptions`] passes the result back in.
//!
//! Every importer returns a standalone [`Project`] fragment ([`Imported`]) that the caller merges into
//! the open project with [`merge_into`], plus a [`Report`] of everything that could not be represented
//! (unmapped effects, speed ramps, …). Exporters take a sequence of a project and never fail on
//! unsupported features either; they record them in the report instead.
//!
//! All time math is exact on [`filmcraft_time::Tick`]s: document frame counts, SMPTE timecode and
//! rational seconds (`1001/24000s`) map onto ticks without rounding for every broadcast rate.
//!
//! This crate is L2: it does no file I/O. Media references are strings (absolute paths, or paths
//! resolved against the `base_dir` passed to the importer); the engine probes/relinks them.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod aaf;
pub mod ale;
pub mod drp;
pub mod edl;
pub mod essence;
pub mod fcp7;
pub mod fcpxml;
pub mod omf;
pub mod otio;

mod common;
mod comp;
pub mod wav;
mod xml;

use std::collections::{BTreeMap, HashMap};

use filmcraft_project::{BinEntry, BinId, ClipId, ItemId, ItemKind, MarkerId, Project, TrackId, TransitionId};
use filmcraft_time::{FrameRate, Tick};
use serde::{Deserialize, Serialize};

pub use common::{file_url_to_path, path_to_file_url, resolve_path};
pub use comp::ExtractedMedia;

/// An interchange format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    /// CMX 3600 edit decision list.
    Edl,
    /// Final Cut Pro 7 XML interchange (`xmeml`), as read/written by Premiere Pro.
    Fcp7Xml,
    /// Final Cut Pro X XML (1.9–1.11).
    Fcpxml,
    /// OpenTimelineIO JSON.
    Otio,
    /// Advanced Authoring Format (AAF Edit Protocol, structured storage).
    Aaf,
    /// OMF Interchange 2.0 (Bento container).
    Omf,
    /// DaVinci Resolve project archive (`.drp`). Import only.
    Drp,
}

impl Format {
    /// The formats FilmCraft exports (every format imports).
    pub const ALL: [Format; 6] = [Format::Edl, Format::Fcp7Xml, Format::Fcpxml, Format::Otio, Format::Aaf, Format::Omf];
    /// The formats FilmCraft imports.
    pub const IMPORTABLE: [Format; 7] = [Format::Edl, Format::Fcp7Xml, Format::Fcpxml, Format::Otio, Format::Aaf, Format::Omf, Format::Drp];

    /// Whether FilmCraft can write this format.
    pub fn can_export(self) -> bool {
        self != Format::Drp
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Edl => "CMX 3600 EDL",
            Format::Fcp7Xml => "Final Cut Pro XML",
            Format::Fcpxml => "FCPXML",
            Format::Otio => "OpenTimelineIO",
            Format::Aaf => "AAF",
            Format::Omf => "OMF",
            Format::Drp => "DaVinci Resolve Project",
        }
    }

    /// Default file extension (without the dot).
    pub fn extension(self) -> &'static str {
        match self {
            Format::Edl => "edl",
            Format::Fcp7Xml => "xml",
            Format::Fcpxml => "fcpxml",
            Format::Otio => "otio",
            Format::Aaf => "aaf",
            Format::Omf => "omf",
            Format::Drp => "drp",
        }
    }

    /// Format for a file extension (case-insensitive, with or without the dot). `.xml` is ambiguous
    /// between xmeml and FCPXML; use [`detect`] when the bytes are available.
    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "edl" => Some(Format::Edl),
            "xml" => Some(Format::Fcp7Xml),
            "fcpxml" | "fcpxmld" => Some(Format::Fcpxml),
            "otio" => Some(Format::Otio),
            "aaf" => Some(Format::Aaf),
            "omf" | "omfi" => Some(Format::Omf),
            "drp" => Some(Format::Drp),
            _ => None,
        }
    }
}

/// Sniff the format of a document from its bytes, using the file extension as a hint.
pub fn detect(bytes: &[u8], extension: Option<&str>) -> Option<Format> {
    if aaf::sniff(bytes) {
        return Some(Format::Aaf);
    }
    if drp::sniff(bytes) {
        return Some(Format::Drp);
    }
    if omf::sniff(bytes) && extension.is_none_or(|e| matches!(Format::from_extension(e), None | Some(Format::Omf))) {
        return Some(Format::Omf);
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let head = head.trim_start_matches('\u{feff}').trim_start();
    if head.starts_with('<') {
        if head.contains("<xmeml") {
            return Some(Format::Fcp7Xml);
        }
        if head.contains("<fcpxml") {
            return Some(Format::Fcpxml);
        }
        return extension.and_then(Format::from_extension).filter(|f| matches!(f, Format::Fcp7Xml | Format::Fcpxml));
    }
    if head.starts_with('{') {
        if head.contains("\"OTIO_SCHEMA\"") {
            return Some(Format::Otio);
        }
        return extension.and_then(Format::from_extension).filter(|f| *f == Format::Otio);
    }
    if edl::looks_like_edl(head) {
        return Some(Format::Edl);
    }
    extension.and_then(Format::from_extension).filter(|f| *f == Format::Edl)
}

/// Error from an importer or exporter. Unsupported *features* never error; they go in the [`Report`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("{format} parse error: {message}")]
    Parse { format: &'static str, message: String },
    #[error("the document contains no timeline")]
    Empty,
    #[error("no sequence {0:?} in the project")]
    NoSequence(ItemId),
    #[error("{0}")]
    Other(String),
}

impl Error {
    pub(crate) fn parse(format: Format, message: impl Into<String>) -> Error {
        Error::Parse { format: format.name(), message: message.into() }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Severity of a report entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Level {
    /// Informational (an assumption the importer made).
    Info,
    /// Something was dropped or approximated.
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportEntry {
    pub level: Level,
    pub message: String,
    /// How many times this message occurred.
    pub count: u32,
}

/// What an import/export could not represent exactly. Identical messages are merged with a count.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub entries: Vec<ReportEntry>,
}

impl Report {
    pub fn push(&mut self, level: Level, message: impl Into<String>) {
        let message = message.into();
        if let Some(e) = self.entries.iter_mut().find(|e| e.level == level && e.message == message) {
            e.count += 1;
        } else {
            self.entries.push(ReportEntry { level, message, count: 1 });
        }
    }
    pub fn warn(&mut self, message: impl Into<String>) {
        self.push(Level::Warning, message);
    }
    pub fn info(&mut self, message: impl Into<String>) {
        self.push(Level::Info, message);
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn warnings(&self) -> impl Iterator<Item = &ReportEntry> {
        self.entries.iter().filter(|e| e.level == Level::Warning)
    }
    pub fn has_warnings(&self) -> bool {
        self.warnings().next().is_some()
    }
    /// Whether any entry contains `needle` (case-insensitive).
    pub fn mentions(&self, needle: &str) -> bool {
        let n = needle.to_ascii_lowercase();
        self.entries.iter().any(|e| e.message.to_ascii_lowercase().contains(&n))
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for e in &self.entries {
            let lvl = match e.level {
                Level::Info => "info",
                Level::Warning => "warning",
            };
            if e.count > 1 {
                writeln!(f, "{lvl}: {} (×{})", e.message, e.count)?;
            } else {
                writeln!(f, "{lvl}: {}", e.message)?;
            }
        }
        Ok(())
    }
}

/// The result of an import: a standalone project fragment.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    /// Bins, media items and sequences read from the document (ids local to this fragment).
    pub project: Project,
    /// Top-level sequences, in document order (nested sequences are items too but not listed here).
    pub sequences: Vec<ItemId>,
}

/// Import options beyond the defaults of [`import`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportOptions {
    /// Directory relative media paths are resolved against (normally the document's directory).
    pub base_dir: Option<String>,
    /// EDLs carry no frame rate: the rate to interpret timecode at (default: guessed, 29.97 for
    /// drop-frame lists, else 24 unless frame fields show otherwise).
    pub edl_frame_rate: Option<FrameRate>,
    /// Name for documents without a title (EDL without `TITLE:`).
    pub name: Option<String>,
}

/// Import a document. `base_dir` resolves relative media paths.
pub fn import(bytes: &[u8], format: Format, base_dir: Option<&str>) -> Result<(Imported, Report)> {
    import_with(bytes, format, &ImportOptions { base_dir: base_dir.map(str::to_string), ..Default::default() })
}

/// Import a document with explicit options.
///
/// AAF and OMF documents may embed audio: [`aaf::import`] / [`omf::import`] also return it.
pub fn import_with(bytes: &[u8], format: Format, opts: &ImportOptions) -> Result<(Imported, Report)> {
    if format == Format::Drp {
        return drp::import(bytes, opts);
    }
    if matches!(format, Format::Aaf | Format::Omf) {
        let (imported, extracted, mut report) = if format == Format::Aaf { aaf::import(bytes, opts)? } else { omf::import(bytes, opts)? };
        if !extracted.is_empty() {
            report.warn(format!("{} embedded audio file(s) were not extracted", extracted.len()));
        }
        return Ok((imported, report));
    }
    let text = decode_text(bytes);
    let mut report = Report::default();
    let imported = match format {
        Format::Edl => edl::import(&text, opts, &mut report)?,
        Format::Fcp7Xml => fcp7::import(&text, opts, &mut report)?,
        Format::Fcpxml => fcpxml::import(&text, opts, &mut report)?,
        Format::Otio => otio::import(&text, opts, &mut report)?,
        Format::Aaf | Format::Omf | Format::Drp => return Err(Error::Other("AAF / OMF / DRP are binary documents".into())),
    };
    Ok((imported, report))
}

fn decode_text(bytes: &[u8]) -> String {
    // UTF-8 (with or without BOM); fall back to Latin-1 for old EDLs.
    let b = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}

/// How EDL reel names are derived from media.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReelMode {
    /// From the media file name (without extension), sanitised and truncated.
    #[default]
    FileName,
    /// From the clip name.
    ClipName,
    /// Every event uses reel `AX` (clip names/source files identify the media in comments).
    Ax,
}

/// EDL export options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdlOptions {
    /// Video track to export (0 = V1). `None` exports audio only.
    pub video_track: Option<usize>,
    /// Audio tracks to include (0 = A1); at most four (A, A2, A3, A4).
    pub audio_tracks: Vec<usize>,
    pub reel_mode: ReelMode,
    /// Maximum reel name length: 8 (strict CMX 3600) or 32 (extended).
    pub reel_len: usize,
    /// Write `* FROM CLIP NAME:` / `* TO CLIP NAME:` comments.
    pub clip_names: bool,
    /// Write `* SOURCE FILE:` comments.
    pub source_files: bool,
    /// Write `* EFFECT NAME:` comments for transitions.
    pub effect_names: bool,
    /// Write sequence markers as `* LOC:` comments.
    pub markers: bool,
}

impl Default for EdlOptions {
    fn default() -> Self {
        Self {
            video_track: Some(0),
            audio_tracks: vec![0, 1, 2, 3],
            reel_mode: ReelMode::FileName,
            reel_len: 8,
            clip_names: true,
            source_files: true,
            effect_names: true,
            markers: true,
        }
    }
}

/// FCPXML document version to write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FcpxmlVersion {
    V1_9,
    #[default]
    V1_10,
    V1_11,
}

impl FcpxmlVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            FcpxmlVersion::V1_9 => "1.9",
            FcpxmlVersion::V1_10 => "1.10",
            FcpxmlVersion::V1_11 => "1.11",
        }
    }
}

/// Export options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportOptions {
    /// Write media paths relative to this directory where possible (EDL `SOURCE FILE`, OTIO
    /// `target_url`). XML formats always use absolute `file://` URLs for absolute paths.
    pub relative_to: Option<String>,
    /// Document/title name (default: the sequence name).
    pub name: Option<String>,
    pub edl: EdlOptions,
    /// `xmeml` version attribute (4 or 5).
    pub xmeml_version: u32,
    pub fcpxml_version: FcpxmlVersion,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self { relative_to: None, name: None, edl: EdlOptions::default(), xmeml_version: 4, fcpxml_version: FcpxmlVersion::default() }
    }
}

/// Export `sequence` of `project` (and the media/nested sequences it uses).
pub fn export(project: &Project, sequence: ItemId, format: Format, opts: &ExportOptions) -> Result<(Vec<u8>, Report)> {
    if project.sequence(sequence).is_none() {
        return Err(Error::NoSequence(sequence));
    }
    if !format.can_export() {
        return Err(Error::Other(format!("{} documents can be imported but not exported", format.name())));
    }
    if format == Format::Aaf {
        return aaf::export(project, sequence, &aaf::AafOptions { name: opts.name.clone(), ..Default::default() });
    }
    if format == Format::Omf {
        return omf::export(project, sequence, &omf::OmfOptions { name: opts.name.clone(), ..Default::default() });
    }
    let mut report = Report::default();
    let text = match format {
        Format::Edl => edl::export(project, sequence, opts, &mut report)?,
        Format::Fcp7Xml => fcp7::export(project, sequence, opts, &mut report)?,
        Format::Fcpxml => fcpxml::export(project, sequence, opts, &mut report)?,
        Format::Otio => otio::export(project, sequence, opts, &mut report)?,
        Format::Aaf | Format::Omf | Format::Drp => return Err(Error::Other("AAF / OMF / DRP are binary documents".into())),
    };
    Ok((text.into_bytes(), report))
}

/// Premiere's "Export ▸ EDL" with one list per video track: returns `(suggested suffix, document)`
/// for every non-empty video track (`"V1"`, `"V2"`, …). Audio tracks go in the V1 list.
pub fn export_edl_per_track(project: &Project, sequence: ItemId, opts: &ExportOptions) -> Result<(Vec<(String, Vec<u8>)>, Report)> {
    let seq = project.sequence(sequence).ok_or(Error::NoSequence(sequence))?;
    let mut report = Report::default();
    let mut out = Vec::new();
    for (i, t) in seq.video_tracks.iter().enumerate() {
        if t.items.is_empty() {
            continue;
        }
        let mut o = opts.clone();
        o.edl.video_track = Some(i);
        if !out.is_empty() {
            o.edl.audio_tracks.clear();
        }
        let text = edl::export(project, sequence, &o, &mut report)?;
        out.push((format!("V{}", i + 1), text.into_bytes()));
    }
    if out.is_empty() {
        let text = edl::export(project, sequence, opts, &mut report)?;
        out.push(("V1".into(), text.into_bytes()));
    }
    Ok((out, report))
}

/// Merge an imported fragment into `target`, re-allocating every id so nothing collides. The
/// fragment's root children go into `bin` (or the root). Returns the new ids of the fragment's
/// top-level sequences.
pub fn merge_into(target: &mut Project, fragment: Imported, bin: Option<BinId>) -> Vec<ItemId> {
    let Imported { project: frag, sequences } = fragment;
    let mut items: HashMap<ItemId, ItemId> = HashMap::new();
    for id in frag.items.keys() {
        items.insert(*id, ItemId(target.alloc_id()));
    }
    let mut map_u64: HashMap<u64, u64> = HashMap::new();
    let mut fresh = |target: &mut Project, old: u64| *map_u64.entry(old).or_insert_with(|| target.alloc_id());

    let mut new_items: BTreeMap<ItemId, filmcraft_project::ProjectItem> = BTreeMap::new();
    for (old, mut it) in frag.items {
        it.id = items[&old];
        it.created = it.id.0;
        match &mut it.kind {
            ItemKind::Sequence(seq) => {
                let seq = std::sync::Arc::make_mut(seq);
                for m in &mut seq.markers {
                    m.id = MarkerId(target.alloc_id());
                }
                for t in seq.all_tracks_mut() {
                    t.id = TrackId(target.alloc_id());
                    let mut clips: HashMap<ClipId, ClipId> = HashMap::new();
                    for ti in &mut t.items {
                        let n = ClipId(target.alloc_id());
                        clips.insert(ti.id, n);
                        ti.id = n;
                        if let Some(i) = items.get(&ti.item) {
                            ti.item = *i;
                        }
                        ti.link = ti.link.map(|l| fresh(target, l | (1 << 62)));
                        ti.group = ti.group.map(|g| fresh(target, g | (1 << 61)));
                        for m in &mut ti.markers {
                            m.id = MarkerId(target.alloc_id());
                        }
                    }
                    for tr in &mut t.transitions {
                        tr.id = TransitionId(target.alloc_id());
                        tr.from = tr.from.and_then(|c| clips.get(&c).copied());
                        tr.to = tr.to.and_then(|c| clips.get(&c).copied());
                    }
                }
            }
            ItemKind::Subclip { parent, .. } => {
                if let Some(p) = items.get(parent) {
                    *parent = *p;
                }
            }
            ItemKind::Media(m) => {
                for mk in &mut m.markers {
                    mk.id = MarkerId(target.alloc_id());
                }
            }
            ItemKind::AdjustmentLayer { .. } | ItemKind::Graphic { .. } => {}
        }
        new_items.insert(it.id, it);
    }
    fn remap_bin(b: &mut filmcraft_project::Bin, items: &HashMap<ItemId, ItemId>, target: &mut Project) {
        for c in &mut b.children {
            match c {
                BinEntry::Item(i) => *i = items[i],
                BinEntry::Bin(sub) => {
                    sub.id = BinId(target.alloc_id());
                    remap_bin(sub, items, target);
                }
            }
        }
    }
    let mut root = frag.root;
    remap_bin(&mut root, &items, target);
    target.items.extend(new_items);
    let dest = match bin.and_then(|b| target.root.find_bin_mut(b)) {
        Some(b) => b,
        None => &mut target.root,
    };
    dest.children.extend(root.children);
    sequences.iter().filter_map(|s| items.get(s).copied()).collect()
}

/// After probing a media file imported from an EDL (whose source times are absolute timecode),
/// shift every use of `item` so source times become media-relative: subtracts the media's start
/// timecode (`start_tc_frames` at `rate`) from `source_in`, frame holds and keyframes.
pub fn rebase_source_timecode(project: &mut Project, item: ItemId, start_tc_frames: i64, rate: FrameRate) {
    let off = rate.tick_of(start_tc_frames);
    if off == Tick::ZERO {
        return;
    }
    for it in project.items.values_mut() {
        if let ItemKind::Sequence(seq) = &mut it.kind {
            let seq = std::sync::Arc::make_mut(seq);
            for t in seq.all_tracks_mut() {
                for ti in t.items.iter_mut().filter(|ti| ti.item == item) {
                    ti.source_in -= off;
                    if let Some(h) = &mut ti.frame_hold {
                        *h -= off;
                    }
                    for e in &mut ti.effects {
                        for p in e.params.values_mut() {
                            for k in &mut p.keyframes {
                                k.time -= off;
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(m) = project.item_mut(item).and_then(|i| i.as_media_mut()) {
        m.info.start_timecode = Some(start_tc_frames);
        m.markers.iter_mut().for_each(|mk| mk.start -= off);
    }
}
