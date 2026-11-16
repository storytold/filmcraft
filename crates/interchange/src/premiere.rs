//! Premiere project reader, based on the author's [MIT-licensed][python-license] LLM Video Editor
//! [`premiere.py`][python-parser] and user-owned XML structure.
//! Contributor fidelity mappings and their additional FilmCraft PR references are documented in
//! `docs/premiere-import.md` and this crate's README.
//! No Adobe application code, proprietary documentation or assets were consulted or copied.
//! MIT OR Apache-2.0. Unsupported features remain in the source snapshot
//! and are reported; this is not a promise of pixel-identical Premiere rendering.
//!
//! [python-parser]: https://github.com/willluecke/llm-video-editor/blob/1539ae2be12c0a783df86182dffb08dabef25ba1/src/video_pipeline/premiere.py
//! [python-license]: https://github.com/willluecke/llm-video-editor/blob/1539ae2be12c0a783df86182dffb08dabef25ba1/LICENSE

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

use base64::Engine as _;
use filmcraft_geom::Vec2;
use filmcraft_project::{
    Caption, CaptionFormat, CaptionTrack, ClipId, ItemKind, Label, Param, ParamValue, TrackId, TrackKind, Transition, TransitionAlign, TransitionId,
};
use filmcraft_time::{FrameRate, Tick};
use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::common::{Builder, MediaSpec, empty_sequence, set_param, settings_for};
use crate::xml::{child, child_text};
use crate::{Error, ImportOptions, Imported, Report, Result};

/// The largest plain or gzip-expanded project XML accepted (512 MiB).
pub const MAX_DOCUMENT_BYTES: usize = 512 * 1024 * 1024;
const MAX_COMPRESSED_BYTES: usize = 64 * 1024 * 1024;
const MAX_NODES: u32 = 16_000_000;
const MAX_TRACKS: usize = 256;
const MAX_SEQUENCES: usize = 256;

#[cfg(test)]
#[path = "premiere_reference_tests.rs"]
mod reference_tests;

#[cfg(test)]
#[path = "premiere_limits_tests.rs"]
mod limits_tests;

#[cfg(test)]
#[path = "premiere_fidelity_tests.rs"]
mod fidelity_tests;

#[cfg(test)]
#[path = "premiere_picture_tests.rs"]
mod picture_tests;

#[cfg(test)]
#[path = "premiere_truth_tests.rs"]
mod truth_tests;

#[path = "premiere_color.rs"]
mod color;

#[path = "premiere_motion.rs"]
mod motion;

#[path = "premiere_audio.rs"]
mod audio;

#[path = "premiere_markers.rs"]
mod markers;

#[path = "premiere_items.rs"]
mod items;

/// What [`read`] found in a project; an import keeps it as its provenance source tree.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    /// Every top-level sequence, in document order.
    pub sequences: Vec<Sequence>,
    /// Resolved media path → its project-relative `RelativePath`, resolved against the project
    /// folder: the fallback for a stored absolute path that is missing on this computer.
    #[serde(default)]
    pub relative_media: BTreeMap<String, String>,
    /// The project panel's bins, parents before children; smart/search bins are not included.
    #[serde(default)]
    pub bins: Vec<SourceBin>,
    /// Project panel items: sequences, media, subclips and adjustment layers, in panel order.
    #[serde(default)]
    pub items: Vec<ProjectEntry>,
}
/// A project panel bin; `parent` indexes [`Document::bins`] (none: the root bin).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceBin {
    /// The bin's name (`Bin` when it has none).
    pub name: String,
    /// The bin holding it, indexing [`Document::bins`]; none for the root bin.
    pub parent: Option<usize>,
}
/// A project panel item: a master clip's source and, for a subclip, its range of that media.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// The master clip's name, else the panel item's.
    pub name: String,
    /// Its bin, indexing [`Document::bins`] (none: the root bin).
    #[serde(default)]
    pub bin: Option<usize>,
    /// The resolved path of its media file.
    pub media_path: Option<String>,
    /// Generated media, in place of a media file.
    #[serde(default)]
    pub synthetic: Option<SyntheticMedia>,
    /// A sequence's ObjectUID.
    #[serde(default)]
    pub nested_sequence: Option<String>,
    /// An adjustment layer's `MasterClip` ObjectUID.
    #[serde(default)]
    pub adjustment_layer: Option<String>,
    /// An adjustment layer's clip out point: how long its project item lasts.
    #[serde(default)]
    pub duration: Option<Tick>,
    /// Whether the master clip's media source is video (otherwise audio).
    pub video: bool,
    /// A subclip's range of its media.
    #[serde(default)]
    pub subclip: Option<SubclipRange>,
    /// Media only: its master clip's markers, when no kept timeline item took them first.
    #[serde(default)]
    pub markers: Vec<filmcraft_project::Marker>,
}
/// A subclip's media ticks; `hard` is Restrict Trims To Subclip Boundaries (`BoundariesAreHard`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubclipRange {
    /// The first media tick (`StartBoundary`).
    pub start: Tick,
    /// The media tick it ends before (`EndBoundary`).
    pub end: Tick,
    /// Restrict Trims To Subclip Boundaries.
    pub hard: bool,
}
/// A sequence: its frame, rates, work area, markers and tracks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sequence {
    /// Its ObjectUID, by which nested clips of it refer to it.
    pub object_uid: String,
    /// Its name (`Name`).
    pub name: String,
    /// Frame width: the video track group's `FrameRect`, else the preview width, else 1920.
    pub width: u32,
    /// Frame height: the video track group's `FrameRect`, else the preview height, else 1080.
    pub height: u32,
    /// The video track group's frame rate; none when no group states one.
    #[serde(default)]
    pub frame_rate: Option<FrameRate>,
    /// The audio track group's sample rate; none when no group states one.
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// The work area's out point (`MZ.WorkOutPoint`); zero when unset.
    pub work_out: Tick,
    /// Sequence markers, by start; ids are allocated on import.
    #[serde(default)]
    pub markers: Vec<filmcraft_project::Marker>,
    /// Video, audio and caption tracks, in track group order.
    pub tracks: Vec<Track>,
}
/// A video, audio or caption track and its clips.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    /// `video`, `audio` or `caption`.
    pub kind: String,
    /// Its place among the sequence's tracks of its kind (`ClipTrack/Track/Index`).
    pub index: usize,
    /// Muted (`IsMuted`) or, for an audio track, muted on its mixer fader.
    pub muted: bool,
    /// The stored track name; absent for tracks keeping Premiere's default name.
    #[serde(default)]
    pub name: Option<String>,
    /// Locked (`IsLocked`).
    #[serde(default)]
    pub locked: bool,
    /// Its clips, in document order.
    pub items: Vec<Item>,
}
/// A timeline clip or caption: its source, timing, controls and transitions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    /// Its ObjectID.
    pub object_id: String,
    /// The ObjectID of its link group; clips of one group are linked on import.
    #[serde(default)]
    pub link: Option<String>,
    /// Intrinsic audio controls, by `effect.param` (`volume.level`, `channel_volume.left`...).
    #[serde(default)]
    pub audio: BTreeMap<String, Param>,
    /// Imported picture effects (Lumetri).
    #[serde(default)]
    pub video_effects: Vec<filmcraft_project::EffectInstance>,
    /// A graphic (text or shape layers, or a generated graphic canvas) rather than media.
    #[serde(default)]
    pub graphic: bool,
    /// Clip > Enable turned off (`ClipTrackItem/IsMuted`).
    #[serde(default)]
    pub disabled: bool,
    /// Its name: the subclip's `Name`, else the item's element name.
    pub label: String,
    /// Timeline start, in ticks.
    pub start: Tick,
    /// Timeline end, in ticks (exclusive).
    pub end: Tick,
    /// Media in point, in ticks; a clip slipped before the media's start starts at zero.
    pub source_in: Tick,
    /// Media out point, in ticks; the in point plus the timeline duration when not stored.
    pub source_out: Tick,
    /// Playback speed (`PlaybackSpeed`; 1 when not stored).
    pub speed: f64,
    /// The resolved path of its media file; none for generated media, adjustment layers, nests,
    /// graphics and unresolved sources.
    pub media_path: Option<String>,
    /// Media Premiere generates itself (black video, colour matte...), in place of a file path.
    #[serde(default)]
    pub synthetic: Option<SyntheticMedia>,
    /// The `MasterClip` ObjectUID of an adjustment layer (`IsAdjustmentLayer`).
    #[serde(default)]
    pub adjustment_layer: Option<String>,
    /// Markers of the source media's master clip, on the first kept timeline item that uses it.
    #[serde(default)]
    pub media_markers: Vec<filmcraft_project::Marker>,
    /// The ObjectUID of the sequence it plays, for a nested sequence.
    pub nested_sequence: Option<String>,
    /// Static text: a caption's, or a text layer's Source Text.
    pub text: Option<String>,
    /// Its stored `ESP.Tag` text, kept for provenance.
    pub audio_category: Option<String>,
    /// The display name (else match name) of every component on the clip, in chain order.
    pub effects: Vec<String>,
    /// Static Motion values by control: `Position` (`x:y`, normalised), `Scale`, `Rotation`.
    pub motion: BTreeMap<String, String>,
    /// Animated Motion controls, with their source-time keyframes.
    #[serde(default)]
    pub motion_keys: BTreeMap<String, Param>,
    /// Its head and tail transitions.
    pub transitions: Vec<SourceTransition>,
}

/// Generated media: the four-character code Premiere stores as its file path (`BLAK` is stored
/// as 1112293707), and the `Media` object it belongs to. Titles are UI-language text.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyntheticMedia {
    /// The four-character code (`BLAK`, `COLR`, `BARS`, `TRNV`, `LEAD`...).
    pub code: String,
    /// The `Media` object's ObjectUID, else its ObjectID: one project item per object.
    pub media: String,
}
/// A transition at a clip's head or tail.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceTransition {
    /// Its ObjectID, which the clips on both sides of a cut share; empty in older trees.
    #[serde(default)]
    pub object_id: String,
    /// A transition track item's timeline start; none for a legacy clip-edge fade.
    #[serde(default)]
    pub start: Option<Tick>,
    /// A transition track item's timeline end; none for a legacy clip-edge fade.
    #[serde(default)]
    pub end: Option<Tick>,
    /// At the clip's head (otherwise its tail).
    pub head: bool,
    /// Its `DisplayName`, which picks the FilmCraft transition.
    pub name: String,
    /// End minus start: a legacy fade's length.
    pub duration: Tick,
}

/// Cap diagnostic growth independently of graph size; the original snapshot retains all details.
pub fn warning(report: &mut Report, message: impl Into<String>) {
    limited_report(report, crate::Level::Warning, message.into());
}
/// Add a diagnostic at `level`; past 1,024 entries, one notice stands for the rest.
fn limited_report(report: &mut Report, level: crate::Level, message: String) {
    if report.entries.len() < 1024 {
        report.push(level, message);
    } else if report.entries.len() == 1024 {
        report.warn("further import diagnostics omitted; inspect the original snapshot");
    }
}
/// Charge a visit of `node` to the work budget: its node count plus one unit per 64 bytes. Running
/// out is an error, so repeated references cannot multiply the work without bound.
fn spend(work_left: &mut usize, node: Node<'_, '_>) -> Result<()> {
    *work_left = work_left
        .checked_sub(node.descendants().len().saturating_add(node.range().len() / 64))
        .ok_or_else(|| error("object graph traversal exceeds import work limit"))?;
    Ok(())
}

/// A Premiere parse error.
fn error(message: impl Into<String>) -> Error {
    Error::Parse { format: "Premiere", message: message.into() }
}
/// The first element named `name` at any depth below `n`, in document order.
fn descendant<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.descendants().find(|c| c.is_element() && c.tag_name().name() == name)
}
/// The trimmed text of [`descendant`].
fn value<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    descendant(n, name).and_then(|n| n.text()).map(str::trim)
}
/// A stored tick count, `fallback` when absent; refused outside `Tick::MIN..=Tick::MAX`.
fn signed_ticks(s: Option<&str>, fallback: Tick) -> Result<Tick> {
    let n: i64 = match s {
        Some(s) => s.parse().map_err(|_| error(format!("invalid integer tick {s}")))?,
        None => fallback.0,
    };
    if !(Tick::MIN.0..=Tick::MAX.0).contains(&n) {
        return Err(error("time outside supported tick bounds"));
    }
    Ok(Tick(n))
}
/// [`signed_ticks`], refusing a negative time.
fn ticks(s: Option<&str>, fallback: Tick) -> Result<Tick> {
    let tick = signed_ticks(s, fallback)?;
    if tick < Tick::ZERO {
        return Err(error("time outside supported nonnegative tick bounds"));
    }
    Ok(tick)
}

/// Width and height of a `FrameRect` (`left,top,right,bottom`).
fn frame_rect_size(rect: &str) -> Result<(u32, u32)> {
    let v: Vec<i64> = rect.split(',').map(|f| f.trim().parse::<i64>()).collect::<std::result::Result<_, _>>().map_err(|_| error("invalid frame rectangle"))?;
    let [left, top, right, bottom] = v[..] else { return Err(error("invalid frame rectangle")) };
    let w = u32::try_from(right.checked_sub(left).ok_or_else(|| error("invalid frame rectangle"))?).map_err(|_| error("invalid frame rectangle"))?;
    let h = u32::try_from(bottom.checked_sub(top).ok_or_else(|| error("invalid frame rectangle"))?).map_err(|_| error("invalid frame rectangle"))?;
    Ok((w, h))
}
/// A stored frame dimension; `default` when absent or zero.
fn dimension(s: Option<&str>, default: u32) -> Result<u32> {
    match s {
        None | Some("0") => Ok(default),
        Some(s) => s.parse().map_err(|_| error("invalid frame dimension")),
    }
}
/// The object in `map` that `n`'s `attr` (`ObjectRef` or `ObjectURef`) names.
fn referred<'a, 'i>(n: Option<Node<'a, 'i>>, map: &BTreeMap<&str, Node<'a, 'i>>, attr: &str) -> Option<Node<'a, 'i>> {
    map.get(n?.attribute(attr)?).copied()
}

/// The object `n` points to by `ObjectRef`, else by `ObjectURef`.
fn reference<'a, 'i>(n: Option<Node<'a, 'i>>, ids: &BTreeMap<&str, Node<'a, 'i>>, uids: &BTreeMap<&str, Node<'a, 'i>>) -> Option<Node<'a, 'i>> {
    referred(n, ids, "ObjectRef").or_else(|| referred(n, uids, "ObjectURef"))
}

/// Follow only the known two-edge source chain, never arbitrary references or display names.
fn source_target<'a, 'i>(
    clip: Option<Node<'a, 'i>>,
    ids: &BTreeMap<&str, Node<'a, 'i>>,
    uids: &BTreeMap<&str, Node<'a, 'i>>,
    work_left: &mut usize,
) -> Result<Option<Node<'a, 'i>>> {
    let Some(clip) = clip else { return Ok(None) };
    let clip = child(clip, "Clip").unwrap_or(clip);
    let Some(source) = reference(child(clip, "Source"), ids, uids) else { return Ok(None) };
    spend(work_left, source)?;
    let (container, target) = match source.tag_name().name() {
        "VideoMediaSource" | "AudioMediaSource" => ("MediaSource", "Media"),
        "VideoSequenceSource" | "AudioSequenceSource" => ("SequenceSource", "Sequence"),
        _ => return Ok(None),
    };
    let pointer = child(source, container).and_then(|n| child(n, target));
    let Some(node) = reference(pointer, ids, uids).filter(|n| n.has_tag_name(target)) else { return Ok(None) };
    spend(work_left, node)?;
    Ok(Some(node))
}

/// The stored absolute path, or the project-relative `RelativePath` when the absolute path was
/// saved on another system (a macOS project opened on Windows, or the reverse).
fn media_path(n: Node<'_, '_>, base_dir: Option<&str>) -> Option<String> {
    if synthetic_code(n).is_some() {
        return None;
    }
    let stored = child_text(n, "ActualMediaFilePath").or_else(|| child_text(n, "FilePath")).filter(|p| !p.is_empty())?;
    if let (Some(base), Some(relative)) = (base_dir, relative_media_path(n, base_dir))
        && foreign_absolute(stored, base)
    {
        return Some(relative);
    }
    Some(crate::resolve_path(stored, base_dir))
}

/// The four-character code generated media stores as its decimal "file path", if it has one.
fn synthetic_code(n: Node<'_, '_>) -> Option<String> {
    let stored = child_text(n, "ActualMediaFilePath").or_else(|| child_text(n, "FilePath"))?.trim();
    if stored.is_empty() || !stored.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let code = stored.parse::<u32>().ok()?.to_be_bytes();
    code.iter().all(u8::is_ascii_alphanumeric).then(|| code.iter().map(|&b| char::from(b)).collect())
}

/// FilmCraft's generator for a Premiere generated-media code. A colour matte's colour is not read.
fn generator(code: &str) -> Option<filmcraft_media::Generator> {
    use filmcraft_media::Generator;
    match code {
        "BLAK" => Some(Generator::BlackVideo),
        "COLR" => Some(Generator::ColorMatte { color: [0.0, 0.0, 0.0, 1.0] }),
        "TRNV" => Some(Generator::TransparentVideo),
        "LEAD" => Some(Generator::CountingLeader),
        "BARS" => Some(Generator::BarsAndTone),
        _ => None,
    }
}

/// A `Media` object's `RelativePath` resolved against the project folder; none without a folder or
/// a `RelativePath`, or when that path names a drive.
fn relative_media_path(n: Node<'_, '_>, base_dir: Option<&str>) -> Option<String> {
    let base = base_dir.filter(|b| !b.is_empty())?;
    // Premiere sometimes writes a path on another drive as `..\..\C:\...`: joining that to the
    // project folder names a place that never existed, so such a value is not used.
    let drive = |seg: &str| matches!(seg.as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic());
    child_text(n, "RelativePath").filter(|r| !r.is_empty() && !r.split(['/', '\\']).any(drive)).map(|r| crate::resolve_path(r, Some(base)))
}

/// Whether `path` is an absolute path of the other system family than `base` (POSIX against a
/// Windows drive or share, or the reverse).
fn foreign_absolute(path: &str, base: &str) -> bool {
    let windows = |p: &str| matches!(p.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic()) || p.starts_with(r"\\");
    let posix = |p: &str| p.starts_with('/') && !p.starts_with("//");
    (posix(path) && windows(base)) || (windows(path) && posix(base))
}

/// Stable identifiers observed in user-owned XML. Display names remain diagnostic text only.
fn motion_parameter(n: Node<'_, '_>) -> Option<&'static str> {
    match child_text(n, "ParameterID")? {
        "1" => Some("Position"),
        "2" => Some("Scale"),
        "5" => Some("Rotation"),
        _ => None,
    }
}

/// A parameter's static value: `CurrentValue`, else the value in its `StartKeyframe` record.
fn static_parameter_value<'a>(n: Node<'a, '_>) -> Option<&'a str> {
    child_text(n, "CurrentValue").or_else(|| child_text(n, "StartKeyframe")?.split(',').nth(1).map(str::trim))
}

/// Premiere stores ticks per frame, not frames per second. Compare rational fps distances
/// exactly; common-rate numerators/denominators keep these i128 products within bounds even
/// for an i64::MAX input. This also snaps slightly rounded phone rates to the intended rate.
fn sequence_rate(value: &str) -> Result<FrameRate> {
    let ticks: i64 = value.trim().parse().map_err(|_| error("invalid sequence FrameRate ticks per frame"))?;
    if ticks <= 0 {
        return Err(error("sequence FrameRate ticks per frame must be positive"));
    }
    let distance = |rate: &FrameRate| (i128::from(filmcraft_time::TICKS_PER_SECOND) * i128::from(rate.den) - i128::from(ticks) * i128::from(rate.num)).abs();
    FrameRate::COMMON
        .into_iter()
        .min_by(|a, b| (distance(a) * i128::from(b.den)).cmp(&(distance(b) * i128::from(a.den))))
        .ok_or_else(|| error("no supported sequence frame rates"))
}

/// The Python reader's bounded, length-prefixed UTF-8 caption envelope heuristic.
fn caption_text(encoded: &str) -> Option<String> {
    if encoded.len() > 1024 * 1024 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded.chars().filter(|c| !c.is_whitespace()).collect::<String>()).ok()?;
    let mut found = None;
    for i in 0..bytes.len().saturating_sub(4) {
        let len = u32::from_le_bytes(bytes.get(i..i + 4)?.try_into().ok()?) as usize;
        if !(1..=2000).contains(&len) {
            continue;
        }
        let start = i + 4;
        // Premiere's prefix may count Unicode characters instead of UTF-8 bytes.
        let tail = bytes.get(start..)?;
        let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
        let Ok(text) = std::str::from_utf8(&tail[..end]) else { continue };
        if (text.len().abs_diff(len).min(text.chars().count().abs_diff(len)) <= 8)
            && text.chars().any(char::is_alphabetic)
            && text.chars().all(|c| !c.is_control() || "\r\n\t".contains(c))
        {
            found = Some(text.replace('\r', "\n"));
        }
    }
    found
}

/// The byte limit for gzip or plain input, and the error that names it.
fn input_limit(header: &[u8]) -> (usize, &'static str) {
    if header.starts_with(&[0x1f, 0x8b]) {
        (MAX_COMPRESSED_BYTES, "compressed project exceeds 64 MiB input limit")
    } else {
        (MAX_DOCUMENT_BYTES, "project exceeds 512 MiB input limit")
    }
}

/// Read caller-supplied input with the same gzip/plain caps as the slice API. Reads at most
/// the applicable limit plus one byte, even when metadata is unavailable or the input grows.
pub fn read_input(mut reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.by_ref().take(2).read_to_end(&mut bytes).map_err(|e| error(format!("project input: {e}")))?;
    let (limit, message) = input_limit(&bytes);
    reader.take((limit + 1 - bytes.len()) as u64).read_to_end(&mut bytes).map_err(|e| error(format!("project input: {e}")))?;
    if bytes.len() > limit {
        return Err(error(message));
    }
    Ok(bytes)
}

/// Decode bounded gzip/plain XML without any filesystem or network access.
pub fn read(bytes: &[u8], base_dir: Option<&str>, report: &mut Report) -> Result<Document> {
    let (limit, message) = input_limit(bytes);
    if bytes.len() > limit {
        return Err(error(message));
    }
    let expanded = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut expanded = Vec::new();
        flate2::read::MultiGzDecoder::new(bytes).take(MAX_DOCUMENT_BYTES as u64 + 1).read_to_end(&mut expanded).map_err(|e| error(format!("gzip: {e}")))?;
        if expanded.len() > MAX_DOCUMENT_BYTES {
            return Err(error("expanded project exceeds 512 MiB limit"));
        }
        Cow::Owned(expanded)
    } else {
        Cow::Borrowed(bytes)
    };
    let xml = std::str::from_utf8(&expanded).map_err(|e| error(e.to_string()))?;
    let doc = roxmltree::Document::parse_with_options(xml, roxmltree::ParsingOptions { nodes_limit: MAX_NODES, ..Default::default() })
        .map_err(|e| error(e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "PremiereData" {
        return Err(error("expected PremiereData root"));
    }
    let mut ids = BTreeMap::new();
    let mut uids = BTreeMap::new();
    let mut names = BTreeMap::new();
    let mut sequence_count = 0usize;
    let mut media = BTreeMap::new();
    let mut relative_media = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for n in root.children().filter(Node::is_element) {
        for (attr, map) in [("ObjectID", &mut ids), ("ObjectUID", &mut uids)] {
            if let Some(id) = n.attribute(attr)
                && map.insert(id, n).is_some()
            {
                return Err(error(format!("duplicate {attr}: {id}")));
            }
        }
        if n.has_tag_name("Sequence") {
            sequence_count += 1;
            let name = child_text(n, "Name").ok_or_else(|| error("sequence without name"))?;
            let uid = n.attribute("ObjectUID").filter(|uid| !uid.is_empty()).ok_or_else(|| error("sequence without ObjectUID"))?;
            names.entry(name).and_modify(|entry| *entry = None).or_insert(Some(uid));
        }
        if n.has_tag_name("Media") {
            if child_text(n, "Title").is_some_and(|s| s.len() > 4096)
                || child_text(n, "ActualMediaFilePath").or_else(|| child_text(n, "FilePath")).is_some_and(|s| s.len() > 16384)
                || child_text(n, "RelativePath").is_some_and(|s| s.len() > 16384)
            {
                return Err(error("media title or path is too long"));
            }
            if let (Some(path), Some(relative)) = (media_path(n, base_dir), relative_media_path(n, base_dir))
                && path != relative
            {
                // One stored path saved with different relative paths has no single fallback.
                match relative_media.get(&path) {
                    None => {
                        relative_media.insert(path, Some(relative));
                    }
                    Some(Some(known)) if *known != relative => {
                        relative_media.insert(path, None);
                    }
                    Some(_) => {}
                }
            }
            if let (Some(title), Some(path)) = (child_text(n, "Title"), media_path(n, base_dir)) {
                if media.get(title).is_some_and(|p| p != &path) {
                    ambiguous.insert(title.to_string());
                }
                media.insert(title.to_string(), path);
            }
        }
    }
    if sequence_count > MAX_SEQUENCES {
        return Err(error("too many sequences"));
    }
    for title in &ambiguous {
        media.remove(title);
    }
    // Scale with real document structure, not repeated references. Every visit still spends
    // subtree nodes and bytes, so a small document cannot request unbounded repeated decoding.
    let mut work_left = doc.descendants().len().saturating_mul(64).max(5_000_000);
    let links = link_groups(root, &mut work_left)?;
    let mut reader = Reader {
        objects: &objects,
        links,
        base_dir,
        work: &mut work_left,
        report,
        markers: markers::MasterMarkers::default(),
        item_count: 0,
        skipped_items: BTreeSet::new(),
        unsupported_effects: BTreeMap::new(),
        decoded: DecodedText::default(),
    };
    let mut sequences = Vec::new();
    for n in root.children().filter(|n| n.has_tag_name("Sequence")) {
        sequences.push(reader.sequence(n)?);
    }
    let Reader { skipped_items, unsupported_effects, mut markers, .. } = reader;
    // After the timeline: a master clip's media in the panel takes the markers no kept item took.
    let (bins, items) = items::read(root, &objects.ids, &objects.uids, base_dir, &mut work_left, report, &mut markers)?;
    // A project with bins or media but no sequence still imports them.
    if sequences.is_empty() && bins.is_empty() && items.is_empty() {
        return Err(Error::Empty);
    }
    validate_nesting(&sequences)?;
    // Validate the entire referenced graph before dropping unsupported clips, so skipping one
    // cannot hide malformed ticks, cycles or resource-limit violations elsewhere in its graph.
    for sequence in &mut sequences {
        for track in &mut sequence.tracks {
            track.items.retain(|item| !skipped_items.contains(&item.object_id));
        }
    }
    for ((effect, identifier), count) in unsupported_effects {
        let occurrences = if count == 1 { "occurrence" } else { "occurrences" };
        warning(report, format!("effect {effect} retained in provenance, not rendered ({count} {occurrences}; identifier {identifier})"));
    }
    let relative_media = objects.relative_media.into_iter().filter_map(|(path, relative)| Some((path, relative?))).collect();
    Ok(Document { sequences, relative_media, bins, items })
}

/// The document's top-level objects by `ObjectID` and `ObjectUID`, and the sequence names and
/// media titles an unresolved source reference may fall back to.
struct Objects<'a, 'i> {
    ids: BTreeMap<&'a str, Node<'a, 'i>>,
    uids: BTreeMap<&'a str, Node<'a, 'i>>,
    /// Sequence names and ObjectUIDs; `None` for a name several sequences share.
    names: BTreeMap<&'a str, Option<&'a str>>,
    /// Media titles and their paths, without the ambiguous titles.
    media: BTreeMap<String, String>,
    /// A stored media path's project-relative fallback; `None` when it was saved with several.
    relative_media: BTreeMap<String, Option<String>>,
    /// Media titles naming different paths.
    ambiguous: BTreeSet<String>,
}

impl<'a, 'i> Objects<'a, 'i> {
    /// Index the root's objects. Duplicate identifiers, sequences without a name or ObjectUID,
    /// overlong media titles or paths and too many sequences are errors.
    fn index(root: Node<'a, 'i>, base_dir: Option<&str>) -> Result<Self> {
        let mut objects = Objects {
            ids: BTreeMap::new(),
            uids: BTreeMap::new(),
            names: BTreeMap::new(),
            media: BTreeMap::new(),
            relative_media: BTreeMap::new(),
            ambiguous: BTreeSet::new(),
        };
        let mut sequence_count = 0usize;
        for n in root.children().filter(Node::is_element) {
            for (attr, map) in [("ObjectID", &mut objects.ids), ("ObjectUID", &mut objects.uids)] {
                if let Some(id) = n.attribute(attr)
                    && map.insert(id, n).is_some()
                {
                    return Err(error(format!("duplicate {attr}: {id}")));
                }
            }
            if n.has_tag_name("Sequence") {
                sequence_count += 1;
                let name = child_text(n, "Name").ok_or_else(|| error("sequence without name"))?;
                let uid = n.attribute("ObjectUID").filter(|uid| !uid.is_empty()).ok_or_else(|| error("sequence without ObjectUID"))?;
                objects.names.entry(name).and_modify(|entry| *entry = None).or_insert(Some(uid));
            }
            if n.has_tag_name("Media") {
                objects.add_media(n, base_dir)?;
            }
        }
        if sequence_count > MAX_SEQUENCES {
            return Err(error("too many sequences"));
        }
        for title in &objects.ambiguous {
            objects.media.remove(title);
        }
        Ok(objects)
    }

    /// A `Media` object's title and path, and the project-relative fallback for its path.
    fn add_media(&mut self, n: Node<'a, 'i>, base_dir: Option<&str>) -> Result<()> {
        if child_text(n, "Title").is_some_and(|s| s.len() > 4096)
            || child_text(n, "ActualMediaFilePath").or_else(|| child_text(n, "FilePath")).is_some_and(|s| s.len() > 16384)
            || child_text(n, "RelativePath").is_some_and(|s| s.len() > 16384)
        {
            return Err(error("media title or path is too long"));
        }
        if let (Some(path), Some(relative)) = (media_path(n, base_dir), relative_media_path(n, base_dir))
            && path != relative
        {
            // One stored path saved with different relative paths has no single fallback.
            match self.relative_media.get(&path) {
                None => {
                    self.relative_media.insert(path, Some(relative));
                }
                Some(Some(known)) if *known != relative => {
                    self.relative_media.insert(path, None);
                }
                Some(_) => {}
            }
        }
        if let (Some(title), Some(path)) = (child_text(n, "Title"), media_path(n, base_dir)) {
            if self.media.get(title).is_some_and(|p| p != &path) {
                self.ambiguous.insert(title.to_string());
            }
            self.media.insert(title.to_string(), path);
        }
        Ok(())
    }
}

/// The link group of every linked timeline item, by the item's ObjectID.
fn link_groups<'a>(root: Node<'a, '_>, work_left: &mut usize) -> Result<BTreeMap<&'a str, &'a str>> {
    let mut links = BTreeMap::new();
    for group in root.children().filter(|n| n.has_tag_name("Link")) {
        spend(work_left, group)?;
        let Some(id) = group.attribute("ObjectID") else { continue };
        // This shared identifier is copied into each linked item's provenance.
        if id.len() > 4096 {
            return Err(error("link group identifier is too long"));
        }
        for pointer in group.descendants().filter(|n| n.has_tag_name("TrackItem")) {
            let Some(item) = pointer.attribute("ObjectRef") else { continue };
            if links.insert(item, id).is_some_and(|old| old != id) {
                return Err(error("timeline item belongs to multiple link groups"));
            }
        }
    }
    Ok(links)
}

/// A sequence's frame size and rates, as its track groups state them.
#[derive(Default)]
struct SequenceFormat {
    frame_rect: Option<(u32, u32)>,
    frame_rate: Option<FrameRate>,
    sample_rate: Option<u32>,
}

impl SequenceFormat {
    /// A video track group's frame rate and `FrameRect`, or an audio track group's sample rate;
    /// every group of a kind must agree.
    fn add_group(&mut self, group: Node<'_, '_>, name: &str) -> Result<()> {
        if group.has_tag_name("VideoTrackGroup")
            && let Some(rate) = child(group, "TrackGroup").and_then(|group| child(group, "FrameRate"))
        {
            let rate = sequence_rate(rate.text().unwrap_or(""))?;
            if self.frame_rate.is_some_and(|previous| previous != rate) {
                return Err(error(format!("{name}: conflicting video track group frame rates")));
            }
            self.frame_rate = Some(rate);
        }
        if group.has_tag_name("VideoTrackGroup")
            && let Some(rect) = child_text(group, "FrameRect")
        {
            let size = frame_rect_size(rect)?;
            if self.frame_rect.is_some_and(|previous| previous != size) {
                return Err(error(format!("{name}: conflicting video track group frame sizes")));
            }
            self.frame_rect = Some(size);
        }
        if group.has_tag_name("AudioTrackGroup")
            && let Some(rate) = child(group, "TrackGroup").and_then(|g| child(g, "FrameRate"))
        {
            let raw = rate.text().unwrap_or("").trim().parse::<i64>().map_err(|_| error("invalid audio ticks per sample"))?;
            if raw <= 0 || filmcraft_time::TICKS_PER_SECOND % raw != 0 {
                return Err(error("invalid audio ticks per sample"));
            }
            let parsed = u32::try_from(filmcraft_time::TICKS_PER_SECOND / raw).map_err(|_| error("invalid audio sample rate"))?;
            if !(8000..=384000).contains(&parsed) || self.sample_rate.is_some_and(|old| old != parsed) {
                return Err(error("unsupported or conflicting audio sample rate"));
            }
            self.sample_rate = Some(parsed);
        }
        Ok(())
    }
}

/// What a timeline item's component chain holds.
#[derive(Default)]
struct Components {
    effects: Vec<String>,
    motion: BTreeMap<String, String>,
    motion_keys: BTreeMap<String, Param>,
    audio: BTreeMap<String, Param>,
    video_effects: Vec<filmcraft_project::EffectInstance>,
    graphic: bool,
    text: Option<String>,
}

/// Where a timeline item's picture or sound comes from, and its master clip's markers.
struct ItemSource {
    media_path: Option<String>,
    synthetic: Option<SyntheticMedia>,
    adjustment_layer: Option<String>,
    nested_sequence: Option<String>,
    media_markers: Vec<filmcraft_project::Marker>,
}

/// State shared while reading a document's sequences: the object index and link groups, the work
/// budget and report, and the item count, skipped clips and unsupported effects found so far.
struct Reader<'a, 'i, 'r> {
    objects: &'r Objects<'a, 'i>,
    links: BTreeMap<&'a str, &'a str>,
    base_dir: Option<&'r str>,
    work: &'r mut usize,
    report: &'r mut Report,
    /// Master clip markers, read once and taken by the first kept timeline item that uses each; the
    /// project panel's media takes the rest.
    markers: markers::MasterMarkers,
    item_count: usize,
    skipped_items: BTreeSet<String>,
    unsupported_effects: BTreeMap<(String, String), usize>,
    decoded: DecodedText,
}

/// Text payloads (caption blocks, static Source Text) decoded once per element: any number of
/// timeline items can reference one, and each decode reads its whole payload.
#[derive(Default)]
struct DecodedText(std::collections::HashMap<roxmltree::NodeId, Option<String>>);

impl DecodedText {
    /// [`caption_text`] of `encoded`, the text of `element`, decoded on the element's first use.
    fn get(&mut self, element: Node<'_, '_>, encoded: Option<&str>) -> Option<String> {
        self.0.entry(element.id()).or_insert_with(|| encoded.and_then(caption_text)).clone()
    }
}

impl<'a, 'i> Reader<'a, 'i, '_> {
    /// A sequence: its frame size and rates, its tracks and its markers.
    fn sequence(&mut self, n: Node<'a, 'i>) -> Result<Sequence> {
        spend(self.work, n)?;
        let name = child_text(n, "Name").unwrap_or("Sequence").to_string();
        // The sequence frame is the video track group's `FrameRect`; the preview size is only the
        // fallback (it can differ: a 2560x1440 sequence previewing at 1920x1080).
        let mut width = dimension(value(n, "MZ.Sequence.PreviewFrameSizeWidth"), 1920)?;
        let mut height = dimension(value(n, "MZ.Sequence.PreviewFrameSizeHeight"), 1080)?;
        let mut format = SequenceFormat::default();
        let mut tracks = Vec::new();
        let pointers = child(n, "TrackGroups")
            .into_iter()
            .flat_map(|groups| groups.children())
            .filter(|group| group.has_tag_name("TrackGroup"))
            .filter_map(|group| child(group, "Second"));
        for pointer in pointers {
            let Some(group) = referred(Some(pointer), &self.objects.ids, "ObjectRef") else {
                warning(self.report, format!("{name}: unresolved track group"));
                continue;
            };
            spend(self.work, group)?;
            let kind = if group.tag_name().name().starts_with("Video") {
                "video"
            } else if group.tag_name().name().starts_with("Audio") {
                "audio"
            } else {
                "caption"
            };
            format.add_group(group, &name)?;
            self.track_group(group, kind, &name, &mut tracks)?;
        }
        if let Some((w, h)) = format.frame_rect {
            (width, height) = (w, h);
        }
        filmcraft_project::validate_frame_size(width, height).map_err(error)?;
        let object_uid = n.attribute("ObjectUID").ok_or_else(|| error("sequence without ObjectUID"))?.to_string();
        let markers = markers::read(n, &self.objects.ids, self.work, self.report, &name)?;
        Ok(Sequence {
            object_uid,
            name,
            width,
            height,
            frame_rate: format.frame_rate,
            sample_rate: format.sample_rate,
            work_out: ticks(value(n, "MZ.WorkOutPoint"), Tick::ZERO)?,
            markers,
            tracks,
        })
    }

    /// The tracks a track group lists, appended to the sequence's `tracks`.
    fn track_group(&mut self, group: Node<'a, 'i>, kind: &'static str, name: &str, tracks: &mut Vec<Track>) -> Result<()> {
        for (fallback, pointer) in group.descendants().filter(|n| n.has_tag_name("Track") && n.attribute("ObjectURef").is_some()).enumerate() {
            let Some(track) = referred(Some(pointer), &self.objects.uids, "ObjectURef") else {
                warning(self.report, format!("{name}: unresolved {kind} track"));
                continue;
            };
            spend(self.work, track)?;
            let track = self.track(track, kind, fallback, name, tracks)?;
            tracks.push(track);
        }
        Ok(())
    }

    /// A track and its timeline items. `fallback` is its index when it stores none; `tracks` are
    /// the sequence's tracks read before it.
    fn track(&mut self, track: Node<'a, 'i>, kind: &'static str, fallback: usize, name: &str, tracks: &[Track]) -> Result<Track> {
        // The track's own fields live in `ClipTrack/Track`; elements nested deeper (such as a
        // clip track item's `IsMuted`, which disables only that clip) are not the track's.
        let base = child(track, "ClipTrack").and_then(|c| child(c, "Track"));
        let index =
            base.and_then(|b| child_text(b, "Index")).map(|s| s.parse::<usize>()).transpose().map_err(|_| error("invalid track index"))?.unwrap_or(fallback);
        if index >= MAX_TRACKS || tracks.len() >= MAX_TRACKS * 3 {
            return Err(error("too many tracks"));
        }
        if tracks.iter().any(|t: &Track| t.kind == kind && t.index == index) {
            return Err(error("duplicate track index"));
        }
        let muted = base.and_then(|b| child_text(b, "IsMuted")).is_some_and(|s| s.eq_ignore_ascii_case("true") || s == "1")
            || (kind == "audio" && audio::fader_muted(track, &self.objects.ids, self.work, self.report, name)?);
        let locked = base.and_then(|b| child_text(b, "IsLocked")).is_some_and(|s| s.eq_ignore_ascii_case("true") || s == "1");
        let track_name = base
            .and_then(|b| child(b, "Node"))
            .and_then(|n| child(n, "Properties"))
            .and_then(|p| child_text(p, "MZ.TrackName"))
            .filter(|s| !s.trim().is_empty());
        if track_name.is_some_and(|s| s.len() > 4096) {
            return Err(error("track name is too long"));
        }
        let track_name = track_name.map(str::to_string);
        let mut items = Vec::new();
        for pointer in track.descendants().filter(|n| n.has_tag_name("TrackItem") && n.attribute("ObjectRef").is_some()) {
            let Some(item) = referred(Some(pointer), &self.objects.ids, "ObjectRef") else {
                warning(self.report, format!("{name}: unresolved timeline item"));
                continue;
            };
            if let Some(item) = self.item(item, kind)? {
                items.push(item);
            }
        }
        Ok(Track { kind: kind.into(), index, muted, name: track_name, locked, items })
    }

    /// A timeline item, or `None` for a transition item: transitions are read with the clips
    /// they join.
    fn item(&mut self, item: Node<'a, 'i>, kind: &'static str) -> Result<Option<Item>> {
        spend(self.work, item)?;
        self.item_count += 1;
        if self.item_count > 100_000 {
            return Err(error("too many timeline items"));
        }
        // TransitionItems are a separate list, not source clips (also separated
        // by upstream PR #1694). Their Head/Tail references are handled below.
        if item.tag_name().name().ends_with("TransitionTrackItem") {
            return Ok(None);
        }
        let start = ticks(value(item, "Start"), Tick::ZERO)?;
        let end = ticks(value(item, "End"), start)?;
        let sub = referred(descendant(item, "SubClip"), &self.objects.ids, "ObjectRef");
        if let Some(sub) = sub {
            spend(self.work, sub)?;
        }
        let label = sub.and_then(|s| child_text(s, "Name")).unwrap_or(item.tag_name().name()).to_string();
        let clip = referred(sub.and_then(|s| child(s, "Clip")), &self.objects.ids, "ObjectRef");
        if let Some(clip) = clip {
            spend(self.work, clip)?;
        }
        let mut source_in = signed_ticks(clip.and_then(|s| value(s, "InPoint")), Tick::ZERO)?;
        if source_in < Tick::ZERO {
            // A slip past the source's first frame stores a negative in point.
            warning(self.report, format!("{label}: in point precedes the source start; clip starts at the source start"));
            source_in = Tick::ZERO;
        }
        let source_out = match clip.and_then(|s| value(s, "OutPoint")) {
            Some(raw) => ticks(Some(raw), Tick::ZERO)?,
            None => Tick(source_in.0.checked_add((end.0 - start.0).max(0)).ok_or_else(|| error("source time overflow"))?),
        };
        let speed = clip.and_then(|s| value(s, "PlaybackSpeed")).unwrap_or("1").parse::<f64>().map_err(|_| error("invalid playback speed"))?;
        let unsupported = if end <= start {
            Some("timeline item must have positive duration")
        } else if !speed.is_finite()
            || !(0.001..=1000.0).contains(&speed)
            || source_out < source_in
            || source_out > Tick::MAX
            || source_in.0 as f64 + (end.0 - start.0) as f64 * speed > Tick::MAX.0 as f64
        {
            Some("unsupported playback speed or source range")
        } else {
            None
        };
        if let Some(reason) = unsupported {
            warning(self.report, format!("{label}: {reason}; clip skipped"));
            self.skipped_items.insert(item.attribute("ObjectID").unwrap_or("").to_string());
        }
        let mut parts = self.components(item, kind, &label)?;
        if kind == "caption" {
            parts.text = self.caption(item, &label)?;
        }
        let transitions = self.transitions(item, &label)?;
        let audio_category = value(item, "ESP.Tag").map(str::to_string);
        let disabled = child(item, "ClipTrackItem").and_then(|c| child_text(c, "IsMuted")).is_some_and(|s| s.eq_ignore_ascii_case("true") || s == "1");
        let source = self.source(sub, clip, kind, &label, &mut parts, unsupported.is_some())?;
        Ok(Some(Item {
            object_id: item.attribute("ObjectID").unwrap_or("").into(),
            link: item.attribute("ObjectID").and_then(|id| self.links.get(id)).map(|id| (*id).to_string()),
            audio: parts.audio,
            video_effects: parts.video_effects,
            graphic: parts.graphic,
            disabled,
            motion_keys: parts.motion_keys,
            media_path: source.media_path,
            synthetic: source.synthetic,
            adjustment_layer: source.adjustment_layer,
            media_markers: source.media_markers,
            nested_sequence: source.nested_sequence,
            label,
            start,
            end,
            source_in,
            source_out,
            speed,
            text: parts.text,
            audio_category,
            effects: parts.effects,
            motion: parts.motion,
            transitions,
        }))
    }

    /// The item's component chain: effect names, Motion, Lumetri, intrinsic audio, and the text
    /// and shape layers that make it a graphic.
    fn components(&mut self, item: Node<'a, 'i>, kind: &str, label: &str) -> Result<Components> {
        let mut parts = Components::default();
        let chain = referred(descendant(item, "ComponentOwner").and_then(|n| child(n, "Components")), &self.objects.ids, "ObjectRef");
        if let Some(chain) = chain {
            spend(self.work, chain)?;
            for ptr in chain.descendants().filter(|n| n.has_tag_name("Component")) {
                let Some(c) = referred(Some(ptr), &self.objects.ids, "ObjectRef") else { continue };
                spend(self.work, c)?;
                self.component(c, kind, label, &mut parts)?;
            }
        }
        Ok(parts)
    }

    /// One component of an item's chain; an effect FilmCraft does not import is counted for the
    /// report.
    fn component(&mut self, c: Node<'a, 'i>, kind: &str, label: &str, parts: &mut Components) -> Result<()> {
        let effect = value(c, "DisplayName").or_else(|| child_text(c, "FilterMatchName")).unwrap_or("Unknown").to_string();
        let match_name = child_text(c, "MatchName").or_else(|| child_text(c, "FilterMatchName"));
        let is_text = matches!(match_name, Some("AE.ADBE Text" | "ADBE Text"));
        let is_shape = matches!(match_name, Some("AE.ADBE Shape" | "ADBE Shape"));
        parts.graphic |= is_text || is_shape;
        if is_shape {
            warning(
                self.report,
                format!("{label}: native shape retained as a graphic canvas; opaque path/appearance data is retained in provenance, not rendered"),
            );
        }
        if is_text {
            self.source_text(c, label, &mut parts.text)?;
        }
        if matches!(match_name, Some("AE.ADBE Motion" | "ADBE Motion")) {
            self.motion_params(c, parts)?;
        } else if kind == "video" && matches!(match_name, Some("AE.ADBE Lumetri" | "ADBE Lumetri")) {
            parts.video_effects.push(color::read(c, &self.objects.ids, self.work, self.report)?);
        } else if kind == "audio" && match_name.is_some_and(|m| m.starts_with("Internal Volume") || m.starts_with("Internal Channel Volume")) {
            audio::read(c, match_name.unwrap_or_default(), &self.objects.ids, self.work, &mut parts.audio, self.report)?;
        } else if !is_text && !is_shape {
            let count = self.unsupported_effects.entry((effect.clone(), match_name.unwrap_or("unknown").to_string())).or_insert(0usize);
            *count = count.saturating_add(1);
        }
        parts.effects.push(effect);
        Ok(())
    }

    /// A text layer's static Source Text into `text`, which the item's text layers share.
    fn source_text(&mut self, c: Node<'a, 'i>, label: &str, text: &mut Option<String>) -> Result<()> {
        // User-owned XML stores Source Text in parameter 1. InstanceName
        // is the renameable layer label, not the displayed text.
        for ptr in c.descendants().filter(|n| n.has_tag_name("Param")) {
            let Some(p) = referred(Some(ptr), &self.objects.ids, "ObjectRef") else { continue };
            spend(self.work, p)?;
            if child_text(p, "ParameterID") == Some("1")
                && !child_text(p, "IsTimeVarying").is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1")
                && let Some(v) = child(p, "StartKeyframeValue").filter(|v| v.attribute("Encoding") == Some("base64"))
            {
                *text = self.decoded.get(v, v.text());
            }
        }
        if text.is_none() {
            warning(self.report, format!("{label}: static Source Text unavailable; layer name not used as visible text"));
        }
        Ok(())
    }

    /// Motion's Position, Scale and Rotation: static values, or keyframes when animated.
    fn motion_params(&mut self, c: Node<'a, 'i>, parts: &mut Components) -> Result<()> {
        for ptr in c.descendants().filter(|n| n.has_tag_name("Param")) {
            if let Some(p) = referred(Some(ptr), &self.objects.ids, "ObjectRef") {
                spend(self.work, p)?;
                if let Some(key) = motion_parameter(p) {
                    if child_text(p, "IsTimeVarying").is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1") {
                        if parts.motion_keys.insert(key.to_string(), motion::read(p, key, self.report)?).is_some() {
                            return Err(error("duplicate animated Motion control"));
                        }
                    } else if let Some(v) = static_parameter_value(p) {
                        parts.motion.insert(key.to_string(), v.to_string());
                    }
                }
            }
        }
        Ok(())
    }

    /// A caption item's text, from its `BlockVectorItem`.
    fn caption(&mut self, item: Node<'a, 'i>, label: &str) -> Result<Option<String>> {
        let block = referred(descendant(item, "BlockVectorItem"), &self.objects.ids, "ObjectRef");
        if let Some(block) = block {
            spend(self.work, block)?;
        }
        let data = block.and_then(|b| child(b, "FormattedTextData"));
        let text = data.and_then(|data| self.decoded.get(data, Some(crate::xml::text(data))));
        if text.is_none() {
            warning(self.report, format!("{label}: caption text could not be decoded"));
        }
        Ok(text)
    }

    /// The item's head and tail transitions; one that ends before it starts is skipped.
    fn transitions(&mut self, item: Node<'a, 'i>, label: &str) -> Result<Vec<SourceTransition>> {
        let mut transitions = Vec::new();
        for key in ["HeadTransition", "TailTransition"] {
            if let Some(t) = referred(descendant(item, key), &self.objects.ids, "ObjectRef") {
                spend(self.work, t)?;
                let name = value(t, "DisplayName").unwrap_or("Transition").to_string();
                let start = signed_ticks(value(t, "Start"), Tick::ZERO)?;
                let end = signed_ticks(value(t, "End"), start)?;
                if end < start {
                    warning(self.report, format!("{label}: {name} end precedes start; transition skipped"));
                    continue;
                }
                transitions.push(SourceTransition {
                    object_id: t.attribute("ObjectID").unwrap_or("").to_string(),
                    start: t.tag_name().name().ends_with("TransitionTrackItem").then_some(start),
                    end: t.tag_name().name().ends_with("TransitionTrackItem").then_some(end),
                    head: key == "HeadTransition",
                    name,
                    duration: end - start,
                });
            }
        }
        Ok(transitions)
    }

    /// The item's source: media, generated media, a nested sequence or an adjustment layer, by
    /// title for an unresolved reference. Generated graphic media sets `parts.graphic`. A `skipped`
    /// item does not take its master clip's markers.
    fn source(
        &mut self,
        sub: Option<Node<'a, 'i>>,
        clip: Option<Node<'a, 'i>>,
        kind: &str,
        label: &str,
        parts: &mut Components,
        skipped: bool,
    ) -> Result<ItemSource> {
        let target = source_target(clip, &self.objects.ids, &self.objects.uids, self.work)?;
        let generated = target.filter(|n| n.has_tag_name("Media")).and_then(|n| Some((n, synthetic_code(n)?)));
        let master = referred(sub.and_then(|s| child(s, "MasterClip")), &self.objects.uids, "ObjectURef");
        // An adjustment layer's master clip is flagged over generated black video.
        let adjustment_layer = master
            .filter(|_| kind == "video")
            .filter(|m| child_text(*m, "IsAdjustmentLayer").is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1"))
            .and_then(|m| m.attribute("ObjectUID"))
            .map(str::to_string);
        let mut synthetic = None;
        match &generated {
            _ if adjustment_layer.is_some() => {}
            // A graphic's canvas; its text and shapes are read from the components.
            Some((_, code)) if code == "GRFV" => parts.graphic = true,
            // The source of caption clips; their text is read with the caption track.
            Some((_, code)) if code == "SCPF" => {}
            Some((media, code)) => {
                if generator(code).is_none() {
                    warning(self.report, format!("{label}: generated media {code} is not supported; kept offline placeholder"));
                }
                let media = media.attribute("ObjectUID").or_else(|| media.attribute("ObjectID")).unwrap_or("").to_string();
                synthetic = Some(SyntheticMedia { code: code.clone(), media });
            }
            None => {}
        }
        let mut media_path = target.filter(|n| n.has_tag_name("Media")).and_then(|n| media_path(n, self.base_dir));
        let mut nested_sequence = target.filter(|n| n.has_tag_name("Sequence")).and_then(|n| n.attribute("ObjectUID")).map(str::to_string);
        if adjustment_layer.is_some() {
            media_path = None;
            nested_sequence = None;
        }
        if parts.graphic {
            // Generated graphic media may have an internal numeric pseudo-path.
            // It is a native canvas, not a missing filesystem asset.
            media_path = None;
            nested_sequence = None;
        }
        if media_path.is_none()
            && nested_sequence.is_none()
            && generated.is_none()
            && adjustment_layer.is_none()
            && kind != "caption"
            && !parts.graphic
            && parts.text.is_none()
        {
            (media_path, nested_sequence) = self.title_fallback(label);
        }
        if let Some(path) = &media_path {
            *self.work = self.work.checked_sub(path.len() / 64).ok_or_else(|| error("media reference work limit exceeded"))?;
        }
        let media_markers = if media_path.is_some() { self.media_markers(master, label, skipped)? } else { Vec::new() };
        Ok(ItemSource { media_path, synthetic, adjustment_layer, nested_sequence, media_markers })
    }

    /// The media path or nested sequence an unresolved source reference names by its title. A
    /// title several sources share, or both media and a sequence use, is refused.
    fn title_fallback(&mut self, label: &str) -> (Option<String>, Option<String>) {
        let objects = self.objects;
        let named_sequence = objects.names.get(label).copied().flatten();
        let named_media = objects.media.get(label);
        if objects.ambiguous.contains(label) || objects.names.get(label) == Some(&None) || (named_sequence.is_some() && named_media.is_some()) {
            warning(self.report, format!("{label}: unresolved source reference; ambiguous title fallback refused; left offline"));
            (None, None)
        } else if named_sequence.is_some() || named_media.is_some() {
            warning(self.report, format!("{label}: unresolved source reference; using title fallback"));
            (named_media.cloned(), named_sequence.map(str::to_string))
        } else {
            (None, None)
        }
    }

    /// The markers of the item's master clip, if no earlier item took them. Read once per master
    /// clip, onto the first kept timeline item that uses it: re-reading for every cut would repeat
    /// the work, the warnings and the copy. A `skipped` item only reads them: the import drops it,
    /// and they would be lost with it.
    fn media_markers(&mut self, master: Option<Node<'a, 'i>>, label: &str, skipped: bool) -> Result<Vec<filmcraft_project::Marker>> {
        let Some(master) = master else { return Ok(Vec::new()) };
        if skipped {
            self.markers.read(master, &self.objects.ids, self.work, self.report, label)?;
            return Ok(Vec::new());
        }
        self.markers.take(master, &self.objects.ids, self.work, self.report, label)
    }
}

/// Refuse cyclic nesting, and nesting more than 64 sequences deep.
fn validate_nesting(sequences: &[Sequence]) -> Result<()> {
    /// How many sequences deep the nesting below `name` goes, memoised in `depths`; `active` is the
    /// path being walked.
    fn depth(name: &str, graph: &BTreeMap<&str, &Sequence>, active: &mut BTreeSet<String>, depths: &mut BTreeMap<String, usize>) -> Result<usize> {
        if let Some(&n) = depths.get(name) {
            return Ok(n);
        }
        if active.len() >= 64 || !active.insert(name.to_string()) {
            return Err(error("cyclic or too-deep nested sequences"));
        }
        let mut longest = 1;
        if let Some(seq) = graph.get(name) {
            for child in seq.tracks.iter().flat_map(|t| &t.items).filter_map(|i| i.nested_sequence.as_deref()) {
                longest = longest.max(1 + depth(child, graph, active, depths)?);
            }
        }
        if longest > 64 {
            return Err(error("nested sequences exceed 64 levels"));
        }
        active.remove(name);
        depths.insert(name.to_string(), longest);
        Ok(longest)
    }
    let graph: BTreeMap<_, _> = sequences.iter().map(|s| (s.object_uid.as_str(), s)).collect();
    let mut depths = BTreeMap::new();
    for seq in sequences {
        depth(&seq.object_uid, &graph, &mut BTreeSet::new(), &mut depths)?;
    }
    Ok(())
}

/// Build native sequences, preserving nesting and exact ticks. An explicit frame-rate override
/// wins over each sequence's video track group rate; only absent rates use a warned 24 fps fallback.
pub fn import(bytes: &[u8], opts: &ImportOptions, rate: Option<FrameRate>) -> Result<(Imported, Document, Report)> {
    let mut report = Report::default();
    let doc = read(bytes, opts.base_dir.as_deref(), &mut report)?;
    let mut b = Builder::new(opts.name.as_deref().unwrap_or("Premiere import"));
    let mut sequences = BTreeMap::new();
    for seq in &doc.sequences {
        let fps = rate.or(seq.frame_rate).unwrap_or_else(|| {
            warning(&mut report, format!("{}: no video track group frame rate; using 24 fps (pass frameRate to override)", seq.name));
            FrameRate::FPS_24
        });
        let mut settings = settings_for(fps, seq.width, seq.height, false);
        settings.sample_rate = seq.sample_rate.unwrap_or(48000);
        settings.validate().map_err(error)?;
        let id = b.reserve_sequence(&seq.name, settings, None);
        sequences.insert(seq.object_uid.clone(), id);
        b.top.push(id);
    }
    for src in &doc.sequences {
        let fps = rate.or(src.frame_rate).unwrap_or(FrameRate::FPS_24);
        let mut settings = settings_for(fps, src.width, src.height, false);
        settings.sample_rate = src.sample_rate.unwrap_or(48000);
        let mut seq = empty_sequence(settings);
        let mut link_ids = BTreeMap::new();
        if src.work_out > Tick::ZERO {
            seq.work_area = Some(filmcraft_time::TimeRange::new(Tick::ZERO, src.work_out));
        }
        for marker in &src.markers {
            seq.markers.push(filmcraft_project::Marker { id: filmcraft_project::MarkerId(b.alloc()), ..marker.clone() });
        }
        for track in &src.tracks {
            if track.kind == "caption" {
                let mut captions = CaptionTrack::new(TrackId(b.alloc()), format!("Captions {}", track.index + 1), CaptionFormat::Subtitle);
                captions.enabled = !track.muted;
                captions.locked = track.locked;
                if let Some(name) = &track.name {
                    captions.name = name.clone();
                }
                for item in &track.items {
                    captions.captions.push(Caption {
                        id: ClipId(b.alloc()),
                        start: item.start,
                        duration: item.end - item.start,
                        text: item.text.clone().unwrap_or_default(),
                        speaker: None,
                        cue_id: Some(item.object_id.clone()),
                        settings: String::new(),
                    });
                }
                captions.captions.sort_by_key(|c| c.start);
                seq.caption_tracks.push(captions);
                continue;
            }
            let kind = if track.kind == "audio" { TrackKind::Audio } else { TrackKind::Video };
            b.ensure_tracks(&mut seq, kind, track.index + 1);
            if let Some(t) = seq.tracks_mut(kind).get_mut(track.index) {
                t.enabled = !track.muted;
                t.muted = track.muted;
                t.locked = track.locked;
                if let Some(name) = &track.name {
                    t.name = name.clone();
                }
            }
            let mut transition_ids = BTreeMap::new();
            for item in &track.items {
                let nested = item.nested_sequence.as_ref().and_then(|n| sequences.get(n)).copied();
                let adjustment_key = item.adjustment_layer.as_ref().map(|master| format!("premiere-adjustment-layer:{master}"));
                let source = if let Some(id) = nested {
                    id
                } else if let Some(key) = &adjustment_key {
                    // Clips of one adjustment layer share its project item, long enough for each use.
                    let needed = item.source_out.max(fps.frame_duration());
                    match b.find_media(key) {
                        Some(id) => {
                            if let Some(ItemKind::AdjustmentLayer { duration, .. }) = b.p.item_mut(id).map(|i| &mut i.kind) {
                                *duration = (*duration).max(needed);
                            }
                            id
                        }
                        None => {
                            let kind = ItemKind::AdjustmentLayer { width: src.width, height: src.height, rate: fps, duration: needed };
                            let id = b.p.add_item(&item.label, Label::Lavender, kind, None);
                            b.register_media(key, id);
                            id
                        }
                    }
                } else if item.graphic || item.text.is_some() {
                    if item.text.is_some() {
                        warning(&mut report, "text graphic imported with default font/style; source styling retained in original snapshot");
                    }
                    b.p.add_item(&item.label, Label::Lavender, ItemKind::Graphic { width: src.width, height: src.height, rate: fps }, None)
                } else if let Some((synthetic, g)) = item.synthetic.as_ref().and_then(|s| Some((s, generator(&s.code)?))) {
                    // Generated media is not a file: it never reaches the document-path policy or probe.
                    let key = format!("premiere-generator:{}:{}", synthetic.code, synthetic.media);
                    if b.find_media(&key).is_none() && matches!(g, filmcraft_media::Generator::ColorMatte { .. }) {
                        warning(&mut report, format!("{}: colour matte imported black (its colour is not read)", item.label));
                    }
                    let audio = matches!(g, filmcraft_media::Generator::BarsAndTone | filmcraft_media::Generator::CountingLeader);
                    let spec = MediaSpec { video: Some((src.width, src.height, fps)), audio: audio.then_some((48000, 2)), ..Default::default() };
                    b.generator_media(&key, &item.label, g, &spec, None)
                } else {
                    let path = item.media_path.clone().unwrap_or_else(|| format!("__missing_premiere__/{}", item.object_id));
                    let spec = MediaSpec {
                        duration: Some(item.source_out),
                        video: (kind == TrackKind::Video).then_some((src.width, src.height, fps)),
                        audio: (kind == TrackKind::Audio).then_some((48000, 2)),
                        ..Default::default()
                    };
                    let id = b.file_media(&format!("{}:{path}", track.kind), &item.label, &path, &spec, None);
                    let media_markers: Vec<_> = item
                        .media_markers
                        .iter()
                        .map(|marker| filmcraft_project::Marker { id: filmcraft_project::MarkerId(b.alloc()), ..marker.clone() })
                        .collect();
                    if let Some(pi) = b.p.item_mut(id)
                        && let ItemKind::Media(m) = &mut pi.kind
                    {
                        m.offline = true;
                        // Several timeline clips share one media item; its first markers win.
                        if m.markers.is_empty() {
                            m.markers = media_markers;
                        }
                    }
                    if item.media_path.is_none() {
                        warning(&mut report, format!("{}: media unresolved; kept offline placeholder", item.label));
                    }
                    id
                };
                let mut clip = b.clip(source, kind, &item.label, item.start, item.end - item.start, item.source_in);
                clip.speed = item.speed;
                clip.enabled = !item.disabled;
                if let Some(link) = &item.link {
                    clip.link = Some(*link_ids.entry(link.clone()).or_insert_with(|| b.alloc()));
                }
                for (key, param) in &item.audio {
                    if let Some((effect, name)) = key.split_once('.') {
                        set_param(&mut clip, effect, name, param.clone());
                    }
                }
                clip.effects.extend(item.video_effects.iter().cloned());
                if let Some(text) = &item.text {
                    clip.effects.push(filmcraft_project::graphic::new_text_layer(text, Vec2::new(src.width as f64 / 2.0, src.height as f64 / 2.0), 54.0));
                }
                for (key, raw) in &item.motion {
                    let param = match key.as_str() {
                        "Scale" => raw.parse::<f64>().ok().filter(|v| v.is_finite() && (0.0..=10000.0).contains(v)).map(ParamValue::Float),
                        "Rotation" => raw.parse::<f64>().ok().filter(|v| v.is_finite() && v.abs() <= 1_000_000.0).map(ParamValue::Float),
                        "Position" => raw
                            .split_once(':')
                            .and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?)))
                            .filter(|(x, y)| x.is_finite() && y.is_finite() && x.abs() <= 1_000_000.0 && y.abs() <= 1_000_000.0)
                            .map(|(x, y)| ParamValue::Vec2(Vec2::new(x * src.width as f64, y * src.height as f64))),
                        _ => None,
                    };
                    if let Some(p) = param {
                        set_param(&mut clip, "motion", &key.to_ascii_lowercase(), Param::new(p));
                    } else {
                        warning(&mut report, format!("{}: unsupported Motion parameter {key}", item.label));
                    }
                }
                for (key, param) in &item.motion_keys {
                    set_param(&mut clip, "motion", &key.to_ascii_lowercase(), motion::in_pixels(param.clone(), src.width, src.height));
                }
                for transition in &item.transitions {
                    let legacy_duration = Tick(transition.duration.0.min(clip.duration.0));
                    let start = transition.start.unwrap_or_else(|| if transition.head { clip.start } else { clip.end() - legacy_duration });
                    let end = transition.end.unwrap_or(start + legacy_duration);
                    if transition.start.is_none() {
                        limited_report(&mut report, crate::Level::Info, format!("{}: {} imported as a legacy clip-edge fade", item.label, transition.name));
                    }
                    let start = start.max(Tick::ZERO);
                    if end <= start {
                        continue;
                    }
                    if !transition.object_id.is_empty()
                        && let Some(id) = transition_ids.get(&transition.object_id)
                        && let Some(existing) = seq.tracks_mut(kind).get_mut(track.index).and_then(|t| t.transitions.iter_mut().find(|t| t.id == *id))
                    {
                        if transition.head {
                            existing.to = Some(clip.id);
                        } else {
                            existing.from = Some(clip.id);
                        }
                        continue;
                    }
                    let cut = if transition.head { clip.start } else { clip.end() };
                    let tr = Transition {
                        id: TransitionId(b.alloc()),
                        effect: crate::common::transition_effect(&transition.name, kind == TrackKind::Audio, &mut report),
                        start,
                        duration: end - start,
                        from: (!transition.head).then_some(clip.id),
                        to: transition.head.then_some(clip.id),
                        align: if cut <= start {
                            TransitionAlign::StartAtCut
                        } else if cut >= end {
                            TransitionAlign::EndAtCut
                        } else {
                            TransitionAlign::CenterAtCut
                        },
                        reverse: false,
                    };
                    if !transition.object_id.is_empty() {
                        transition_ids.insert(transition.object_id.clone(), tr.id);
                    }
                    if let Some(t) = seq.tracks_mut(kind).get_mut(track.index) {
                        t.transitions.push(tr);
                    }
                }
                if let Some(t) = seq.tracks_mut(kind).get_mut(track.index) {
                    t.items.push(clip);
                }
            }
        }
        seq.check_bounds().map_err(error)?;
        if let Some(id) = sequences.get(&src.object_uid) {
            b.put_sequence(*id, seq);
        }
    }
    import_bins(&doc, rate, &mut b, &mut report, &sequences);
    Ok((b.finish(), doc, report))
}

/// The native sequence for `src`: its work area, markers, tracks, clips and transitions.
/// `sequences` are the native sequences reserved for the document's sequences, by ObjectUID.
fn native_sequence(
    src: &Sequence,
    rate: Option<FrameRate>,
    b: &mut Builder,
    report: &mut Report,
    sequences: &BTreeMap<String, ItemId>,
) -> Result<filmcraft_project::Sequence> {
    let fps = rate.or(src.frame_rate).unwrap_or(FrameRate::FPS_24);
    let mut settings = settings_for(fps, src.width, src.height, false);
    settings.sample_rate = src.sample_rate.unwrap_or(48000);
    let mut build = SequenceImport { b, report, sequences, src, fps, seq: empty_sequence(settings), link_ids: BTreeMap::new() };
    if src.work_out > Tick::ZERO {
        build.seq.work_area = Some(filmcraft_time::TimeRange::new(Tick::ZERO, src.work_out));
    }
    for marker in &src.markers {
        build.seq.markers.push(filmcraft_project::Marker { id: filmcraft_project::MarkerId(build.b.alloc()), ..marker.clone() });
    }
    for track in &src.tracks {
        if track.kind == "caption" {
            build.caption_track(track);
        } else {
            build.track(track);
        }
    }
    build.seq.check_bounds().map_err(error)?;
    Ok(build.seq)
}

/// One native sequence being built from its source sequence.
struct SequenceImport<'b> {
    b: &'b mut Builder,
    report: &'b mut Report,
    /// The native sequence reserved for each source sequence, by ObjectUID.
    sequences: &'b BTreeMap<String, ItemId>,
    src: &'b Sequence,
    fps: FrameRate,
    seq: filmcraft_project::Sequence,
    /// The native link id of each source link group.
    link_ids: BTreeMap<String, u64>,
}

impl SequenceImport<'_> {
    /// A caption track and its captions.
    fn caption_track(&mut self, track: &Track) {
        let mut captions = CaptionTrack::new(TrackId(self.b.alloc()), format!("Captions {}", track.index + 1), CaptionFormat::Subtitle);
        captions.enabled = !track.muted;
        captions.locked = track.locked;
        if let Some(name) = &track.name {
            captions.name = name.clone();
        }
        for item in &track.items {
            captions.captions.push(Caption {
                id: ClipId(self.b.alloc()),
                start: item.start,
                duration: item.end - item.start,
                text: item.text.clone().unwrap_or_default(),
                speaker: None,
                cue_id: Some(item.object_id.clone()),
                settings: String::new(),
            });
        }
        captions.captions.sort_by_key(|c| c.start);
        self.seq.caption_tracks.push(captions);
    }

    /// A video or audio track, its clips and their transitions.
    fn track(&mut self, track: &Track) {
        let kind = if track.kind == "audio" { TrackKind::Audio } else { TrackKind::Video };
        self.b.ensure_tracks(&mut self.seq, kind, track.index + 1);
        if let Some(t) = self.seq.tracks_mut(kind).get_mut(track.index) {
            t.enabled = !track.muted;
            t.muted = track.muted;
            t.locked = track.locked;
            if let Some(name) = &track.name {
                t.name = name.clone();
            }
        }
        let mut added_transitions = BTreeMap::new();
        for item in &track.items {
            let source = self.source(kind, item);
            let clip = self.clip(source, kind, item);
            self.transitions(kind, track.index, item, &clip, &mut added_transitions);
            if let Some(t) = self.seq.tracks_mut(kind).get_mut(track.index) {
                t.items.push(clip);
            }
        }
    }

    /// The project item a clip plays: a nested sequence, an adjustment layer, a graphic,
    /// generated media or file media.
    fn source(&mut self, kind: TrackKind, item: &Item) -> ItemId {
        let nested = item.nested_sequence.as_ref().and_then(|n| self.sequences.get(n)).copied();
        let adjustment_key = item.adjustment_layer.as_ref().map(|master| format!("premiere-adjustment-layer:{master}"));
        if let Some(id) = nested {
            id
        } else if let Some(key) = &adjustment_key {
            self.adjustment_layer(key, item)
        } else if item.graphic || item.text.is_some() {
            if item.text.is_some() {
                warning(self.report, "text graphic imported with default font/style; source styling retained in original snapshot");
            }
            self.b.p.add_item(&item.label, Label::Lavender, ItemKind::Graphic { width: self.src.width, height: self.src.height, rate: self.fps }, None)
        } else if let Some((synthetic, g)) = item.synthetic.as_ref().and_then(|s| Some((s, generator(&s.code)?))) {
            // Generated media is not a file: it never reaches the document-path policy or probe.
            let key = format!("premiere-generator:{}:{}", synthetic.code, synthetic.media);
            if self.b.find_media(&key).is_none() && matches!(g, filmcraft_media::Generator::ColorMatte { .. }) {
                warning(self.report, format!("{}: colour matte imported black (its colour is not read)", item.label));
            }
            let audio = matches!(g, filmcraft_media::Generator::BarsAndTone | filmcraft_media::Generator::CountingLeader);
            let spec = MediaSpec { video: Some((self.src.width, self.src.height, self.fps)), audio: audio.then_some((48000, 2)), ..Default::default() };
            self.b.generator_media(&key, &item.label, g, &spec, None)
        } else {
            self.file_media(kind, item)
        }
    }

    /// Clips of one adjustment layer share its project item, long enough for each use.
    fn adjustment_layer(&mut self, key: &str, item: &Item) -> ItemId {
        let needed = item.source_out.max(self.fps.frame_duration());
        match self.b.find_media(key) {
            Some(id) => {
                if let Some(ItemKind::AdjustmentLayer { duration, .. }) = self.b.p.item_mut(id).map(|i| &mut i.kind) {
                    *duration = (*duration).max(needed);
                }
                id
            }
            None => {
                let kind = ItemKind::AdjustmentLayer { width: self.src.width, height: self.src.height, rate: self.fps, duration: needed };
                let id = self.b.p.add_item(&item.label, Label::Lavender, kind, None);
                self.b.register_media(key, id);
                id
            }
        }
    }

    /// The clip's media file, offline until probed; unresolved media keeps a placeholder path.
    fn file_media(&mut self, kind: TrackKind, item: &Item) -> ItemId {
        let path = item.media_path.clone().unwrap_or_else(|| format!("__missing_premiere__/{}", item.object_id));
        let spec = MediaSpec {
            duration: Some(item.source_out),
            video: (kind == TrackKind::Video).then_some((self.src.width, self.src.height, self.fps)),
            audio: (kind == TrackKind::Audio).then_some((48000, 2)),
            ..Default::default()
        };
        let id = self.b.file_media(&media_key(&path), &item.label, &path, &spec, None);
        add_streams(self.b, id, &spec);
        if let Some(ItemKind::Media(m)) = self.b.p.item_mut(id).map(|i| &mut i.kind) {
            m.offline = true;
        }
        attach_markers(self.b, id, &item.media_markers);
        if item.media_path.is_none() {
            warning(self.report, format!("{}: media unresolved; kept offline placeholder", item.label));
        }
        id
    }

    /// The clip of `item` on `source`, with its link, audio levels, effects, text and Motion.
    fn clip(&mut self, source: ItemId, kind: TrackKind, item: &Item) -> TrackItem {
        let mut clip = self.b.clip(source, kind, &item.label, item.start, item.end - item.start, item.source_in);
        clip.speed = item.speed;
        clip.enabled = !item.disabled;
        if let Some(link) = &item.link {
            clip.link = Some(*self.link_ids.entry(link.clone()).or_insert_with(|| self.b.alloc()));
        }
        for (key, param) in &item.audio {
            if let Some((effect, name)) = key.split_once('.') {
                set_param(&mut clip, effect, name, param.clone());
            }
        }
        clip.effects.extend(item.video_effects.iter().cloned());
        if let Some(text) = &item.text {
            clip.effects.push(filmcraft_project::graphic::new_text_layer(text, Vec2::new(self.src.width as f64 / 2.0, self.src.height as f64 / 2.0), 54.0));
        }
        for (key, raw) in &item.motion {
            let param = match key.as_str() {
                "Scale" => raw.parse::<f64>().ok().filter(|v| v.is_finite() && (0.0..=10000.0).contains(v)).map(ParamValue::Float),
                "Rotation" => raw.parse::<f64>().ok().filter(|v| v.is_finite() && v.abs() <= 1_000_000.0).map(ParamValue::Float),
                "Position" => raw
                    .split_once(':')
                    .and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?)))
                    .filter(|(x, y)| x.is_finite() && y.is_finite() && x.abs() <= 1_000_000.0 && y.abs() <= 1_000_000.0)
                    .map(|(x, y)| ParamValue::Vec2(Vec2::new(x * self.src.width as f64, y * self.src.height as f64))),
                _ => None,
            };
            if let Some(p) = param {
                set_param(&mut clip, "motion", &key.to_ascii_lowercase(), Param::new(p));
            } else {
                warning(self.report, format!("{}: unsupported Motion parameter {key}", item.label));
            }
        }
        for (key, param) in &item.motion_keys {
            set_param(&mut clip, "motion", &key.to_ascii_lowercase(), motion::in_pixels(param.clone(), self.src.width, self.src.height));
        }
        clip
    }

    /// The transitions at the edges of `clip`, on track `index` of its kind. A transition the
    /// clips on both sides of a cut refer to is added once and joined to the second clip;
    /// `added` holds each added transition's position on the track, by its ObjectID.
    fn transitions(&mut self, kind: TrackKind, index: usize, item: &Item, clip: &TrackItem, added: &mut BTreeMap<String, usize>) {
        for transition in &item.transitions {
            let legacy_duration = Tick(transition.duration.0.min(clip.duration.0));
            let start = transition.start.unwrap_or_else(|| if transition.head { clip.start } else { clip.end() - legacy_duration });
            let end = transition.end.unwrap_or(start + legacy_duration);
            if transition.start.is_none() {
                limited_report(self.report, crate::Level::Info, format!("{}: {} imported as a legacy clip-edge fade", item.label, transition.name));
            }
            let start = start.max(Tick::ZERO);
            if end <= start {
                continue;
            }
            // By position, not by searching the track's transitions for each cut.
            if !transition.object_id.is_empty()
                && let Some(&at) = added.get(&transition.object_id)
                && let Some(existing) = self.seq.tracks_mut(kind).get_mut(index).and_then(|t| t.transitions.get_mut(at))
            {
                if transition.head {
                    existing.to = Some(clip.id);
                } else {
                    existing.from = Some(clip.id);
                }
                continue;
            }
            let cut = if transition.head { clip.start } else { clip.end() };
            let tr = Transition {
                id: TransitionId(self.b.alloc()),
                effect: crate::common::transition_effect(&transition.name, kind == TrackKind::Audio, self.report),
                start,
                duration: end - start,
                from: (!transition.head).then_some(clip.id),
                to: transition.head.then_some(clip.id),
                align: if cut <= start {
                    TransitionAlign::StartAtCut
                } else if cut >= end {
                    TransitionAlign::EndAtCut
                } else {
                    TransitionAlign::CenterAtCut
                },
                reverse: false,
            };
            if let Some(t) = self.seq.tracks_mut(kind).get_mut(index) {
                if !transition.object_id.is_empty() {
                    added.insert(transition.object_id.clone(), t.transitions.len());
                }
                t.transitions.push(tr);
            }
        }
    }
}

/// The builder key of a media file. A file is one project item whichever kinds of track cut it
/// and whichever bin lists it, as in the other importers.
fn media_key(path: &str) -> String {
    format!("premiere-file:{path}")
}

/// Give file media `id` the streams `spec` describes that it lacks: a file cut on a video and an
/// audio track describes both until the engine's probe replaces the description.
fn add_streams(b: &mut Builder, id: ItemId, spec: &MediaSpec) {
    let Some(ItemKind::Media(m)) = b.p.item_mut(id).map(|i| &mut i.kind) else { return };
    let described = crate::common::media_info(&m.info.name, spec);
    if m.info.video.is_none() && described.video.is_some() {
        m.info.video = described.video;
        if m.info.kind == filmcraft_media::MediaKind::AudioOnly {
            m.info.kind = filmcraft_media::MediaKind::Movie;
        }
    }
    if m.info.audio_streams.is_empty() {
        m.info.audio_streams = described.audio_streams;
    }
}

/// Give media `id` its master clip's `markers`, with new ids, unless it has markers: several
/// timeline clips and project panel items can share one media item, and its first markers win.
fn attach_markers(b: &mut Builder, id: ItemId, markers: &[filmcraft_project::Marker]) {
    if markers.is_empty() || b.p.item(id).and_then(|i| i.as_media()).is_none_or(|m| !m.markers.is_empty()) {
        return;
    }
    let markers: Vec<_> = markers.iter().map(|marker| filmcraft_project::Marker { id: filmcraft_project::MarkerId(b.alloc()), ..marker.clone() }).collect();
    if let Some(m) = b.p.item_mut(id).and_then(|i| i.as_media_mut()) {
        m.markers = markers;
    }
}

/// Project panel bins and the items they hold: the timeline's own item where it is the same
/// source, a new one otherwise. An item listed in several bins stays in the first.
fn import_bins(doc: &Document, rate: Option<FrameRate>, b: &mut Builder, report: &mut Report, sequences: &BTreeMap<String, ItemId>) {
    let (width, height, fps) = doc.sequences.first().map(|s| (s.width, s.height, rate.or(s.frame_rate).unwrap_or(FrameRate::FPS_24))).unwrap_or((
        1920,
        1080,
        rate.unwrap_or(FrameRate::FPS_24),
    ));
    let mut bins = Vec::new();
    for bin in &doc.bins {
        let parent = bin.parent.and_then(|i| bins.get(i).copied());
        bins.push(b.bin(&bin.name, parent));
    }
    // The items already listed, each item's bin when the first listing of it is in one, and those
    // items in panel order. An item first listed at the root stays there.
    let mut listed = std::collections::HashSet::new();
    let mut filed = std::collections::HashMap::new();
    let mut order = Vec::new();
    for entry in &doc.items {
        let id = bin_item(entry, (width, height, fps), b, report, sequences);
        if let Some(id) = id
            && listed.insert(id)
            && let Some(bin) = entry.bin.and_then(|i| bins.get(i).copied())
        {
            filed.insert(id, bin);
            order.push(id);
        }
        // The panel shows media by its master clip name (a rename), not a timeline clip's label.
        if let Some(id) = id
            && entry.subclip.is_none()
            && entry.adjustment_layer.is_none()
            && entry.nested_sequence.is_none()
            && !entry.name.is_empty()
            && let Some(item) = b.p.item_mut(id)
        {
            item.name = entry.name.clone();
        }
    }
    file_items(&mut b.p, &filed, &order);
}

/// Move `order`'s items out of the root bin, where the import created them, into the bins
/// `filed` names, keeping their order. One pass over the root and one traversal of the bin tree:
/// moving them one at a time searched the whole tree for each item, and searching it once per bin
/// was still quadratic in the number of bins.
fn file_items(p: &mut filmcraft_project::Project, filed: &std::collections::HashMap<ItemId, filmcraft_project::BinId>, order: &[ItemId]) {
    use filmcraft_project::BinEntry;
    let mut taken = std::collections::HashSet::new();
    p.root.children.retain(|c| match c {
        BinEntry::Item(id) if filed.contains_key(id) => {
            taken.insert(*id);
            false
        }
        _ => true,
    });
    let mut by_bin: BTreeMap<filmcraft_project::BinId, Vec<BinEntry>> = BTreeMap::new();
    for id in order.iter().filter(|id| taken.contains(*id)) {
        if let Some(bin) = filed.get(id) {
            by_bin.entry(*bin).or_default().push(BinEntry::Item(*id));
        }
    }
    // Each bin takes its items when the walk reaches it. Only the bins it held before are walked
    // into: what it takes are items, not bins to search.
    let mut stack = vec![&mut p.root];
    while let Some(bin) = stack.pop() {
        if by_bin.is_empty() {
            break;
        }
        let held = bin.children.len();
        if let Some(entries) = by_bin.remove(&bin.id) {
            bin.children.extend(entries);
        }
        stack.extend(bin.children.iter_mut().take(held).filter_map(|c| match c {
            BinEntry::Bin(sub) => Some(sub),
            BinEntry::Item(_) => None,
        }));
    }
    // The bins were created above; never lose an item if one is missing.
    for entries in by_bin.into_values() {
        p.root.children.extend(entries);
    }
}

/// The project item of a project panel entry: its sequence, adjustment layer, media or subclip.
/// New media and adjustment layers take `frame`'s width, height and rate.
fn bin_item(entry: &ProjectEntry, frame: (u32, u32, FrameRate), b: &mut Builder, report: &mut Report, sequences: &BTreeMap<String, ItemId>) -> Option<ItemId> {
    let (width, height, fps) = frame;
    if let Some(uid) = &entry.nested_sequence {
        sequences.get(uid).copied()
    } else if let Some(master) = &entry.adjustment_layer {
        let key = format!("premiere-adjustment-layer:{master}");
        Some(b.find_media(&key).unwrap_or_else(|| {
            let duration = entry.duration.unwrap_or(Tick::ZERO).max(fps.frame_duration());
            let id = b.p.add_item(&entry.name, Label::Lavender, ItemKind::AdjustmentLayer { width, height, rate: fps, duration }, None);
            b.register_media(&key, id);
            id
        }))
    } else {
        let media = if let Some(path) = &entry.media_path {
            let spec = MediaSpec {
                duration: entry.subclip.as_ref().map(|r| r.end),
                video: entry.video.then_some((width, height, fps)),
                audio: (!entry.video).then_some((48000, 2)),
                ..Default::default()
            };
            // A subclip's parent is the media file itself.
            let file = path.rsplit(['/', '\\']).next().filter(|t| !t.is_empty());
            let name = if entry.subclip.is_some() { file.unwrap_or(&entry.name) } else { &entry.name };
            let id = b.file_media(&media_key(path), name, path, &spec, None);
            add_streams(b, id, &spec);
            if let Some(ItemKind::Media(m)) = b.p.item_mut(id).map(|i| &mut i.kind) {
                m.offline = true;
                // An item an earlier, shorter use created (a timeline cut, another subclip) grows to
                // cover this subclip; it never shrinks.
                if let Some(end) = spec.duration {
                    m.info.duration = m.info.duration.max(end);
                }
            }
            attach_markers(b, id, &entry.markers);
            Some(id)
        } else if let Some((synthetic, g)) = entry.synthetic.as_ref().and_then(|s| Some((s, generator(&s.code)?))) {
            let key = format!("premiere-generator:{}:{}", synthetic.code, synthetic.media);
            let name = if entry.subclip.is_some() { g.label() } else { entry.name.clone() };
            if b.find_media(&key).is_none() && matches!(g, filmcraft_media::Generator::ColorMatte { .. }) {
                warning(report, format!("{name}: colour matte imported black (its colour is not read)"));
            }
            let audio = matches!(g, filmcraft_media::Generator::BarsAndTone | filmcraft_media::Generator::CountingLeader);
            let spec = MediaSpec { video: Some((width, height, fps)), audio: audio.then_some((48000, 2)), ..Default::default() };
            Some(b.generator_media(&key, &name, g, &spec, None))
        } else {
            None
        };
        match (media, &entry.subclip) {
            (Some(parent), Some(range)) => {
                let kind = ItemKind::Subclip { parent, range: filmcraft_time::TimeRange::from_bounds(range.start, range.end), restrict_trims: range.hard };
                Some(b.p.add_item(&entry.name, Label::Iris, kind, None))
            }
            (media, _) => media,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    const TPS: i64 = filmcraft_time::TICKS_PER_SECOND;
    fn fixture() -> String {
        format!(
            r#"<PremiereData Version="3">
<Sequence ObjectUID="sequence"><Name>Sequence 01</Name><Node><Properties><MZ.Sequence.PreviewFrameSizeWidth>1080</MZ.Sequence.PreviewFrameSizeWidth><MZ.Sequence.PreviewFrameSizeHeight>1920</MZ.Sequence.PreviewFrameSizeHeight></Properties></Node><TrackGroups><TrackGroup><Second ObjectRef="10"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="10"><TrackGroup><Tracks><Track ObjectURef="track"/></Tracks></TrackGroup></VideoTrackGroup>
<VideoClipTrack ObjectUID="track"><ClipTrack><Track><Index>0</Index><IsMuted>false</IsMuted></Track><ClipItems><TrackItems><TrackItem ObjectRef="20"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<VideoClipTrackItem ObjectID="20"><ClipTrackItem><TrackItem><Start>0</Start><End>{}</End></TrackItem><SubClip ObjectRef="21"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="21"><Name>camera.mov</Name><Clip ObjectRef="22"/></SubClip>
<VideoClip ObjectID="22"><Clip><InPoint>{}</InPoint><OutPoint>{}</OutPoint><PlaybackSpeed>2</PlaybackSpeed></Clip></VideoClip>
<Media><Title>camera.mov</Title><ActualMediaFilePath>/missing/camera.mov</ActualMediaFilePath></Media></PremiereData>"#,
            3 * TPS,
            4 * TPS,
            10 * TPS
        )
    }
    // Hand-written from the reference chain observed in user-owned project XML; no project
    // names, IDs, media paths or payloads are copied. Audio and media rates are distractors.
    fn real_shaped_rates(first: &str, second: &str) -> String {
        format!(
            r#"<PremiereData Version="3">
<Sequence ObjectUID="episode"><Name>Episode</Name><TrackGroups>
<TrackGroup Index="0"><First>1</First><Second ObjectRef="audio"/></TrackGroup>
<TrackGroup Index="1"><First>0</First><Second ObjectRef="video"/></TrackGroup>
</TrackGroups></Sequence>
<Sequence ObjectUID="nest"><Name>Nest</Name><TrackGroups>
<TrackGroup Index="0"><First>0</First><Second ObjectRef="nested-video"/></TrackGroup>
</TrackGroups></Sequence>
<AudioTrackGroup ObjectID="audio"><TrackGroup><FrameRate>5292000</FrameRate><Tracks/></TrackGroup></AudioTrackGroup>
<VideoTrackGroup ObjectID="video"><TrackGroup><Node><Properties><FrameRate>10584000000</FrameRate></Properties></Node>{first}<Tracks/></TrackGroup></VideoTrackGroup>
<VideoTrackGroup ObjectID="nested-video"><TrackGroup>{second}<Tracks/></TrackGroup></VideoTrackGroup>
<VideoMediaSource ObjectID="unused"><FrameRate>4233600000</FrameRate></VideoMediaSource>
</PremiereData>"#
        )
    }
    #[test]
    fn real_shaped_sequence_rates_use_video_group_and_exact_ntsc_rationals() {
        for (ticks, expected) in [
            (8467191533i64, FrameRate::FPS_30),
            (8468512416, FrameRate::FPS_30),
            (10594584000, FrameRate::FPS_23_976),
            (8475667200, FrameRate::FPS_29_97),
            (4237833600, FrameRate::FPS_59_94),
        ] {
            let xml = real_shaped_rates(&format!("<FrameRate>{ticks}</FrameRate>"), "<FrameRate>10160640000</FrameRate>");
            for override_rate in [None, Some(FrameRate::FPS_60)] {
                let (fragment, _, report) = import(xml.as_bytes(), &ImportOptions::default(), override_rate).unwrap();
                let rates: Vec<_> = fragment.sequences.iter().map(|id| fragment.project.sequence(*id).unwrap().settings.frame_rate).collect();
                assert_eq!(rates, vec![override_rate.unwrap_or(expected), override_rate.unwrap_or(FrameRate::FPS_25)]);
                assert!(!report.has_warnings(), "{report:?}");
            }
        }
    }
    #[test]
    fn only_sequences_without_video_rate_warn_about_fallback() {
        let xml = real_shaped_rates("", "<FrameRate>8475667200</FrameRate>");
        let (fragment, _, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(fragment.project.sequence(fragment.sequences[0]).unwrap().settings.frame_rate, FrameRate::FPS_24);
        assert_eq!(fragment.project.sequence(fragment.sequences[1]).unwrap().settings.frame_rate, FrameRate::FPS_29_97);
        assert_eq!(report.entries.len(), 1);
        assert!(report.mentions("Episode") && report.mentions("24 fps"));
        let (_, _, report) = import(xml.as_bytes(), &ImportOptions::default(), Some(FrameRate::FPS_30)).unwrap();
        assert!(!report.has_warnings());
    }
    #[test]
    fn malformed_sequence_frame_ticks_fail_without_panicking() {
        for ticks in ["0", "-1", "NaN", "", "1.5", "9223372036854775808"] {
            let xml = real_shaped_rates(&format!("<FrameRate>{ticks}</FrameRate>"), "");
            let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None));
            assert!(result.is_ok());
            assert!(result.unwrap().is_err(), "{ticks}");
        }
    }
    #[test]
    fn gzip_plain_and_exact_times_match_python_reference() {
        let xml = fixture();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(xml.as_bytes()).unwrap();
        let zipped = gz.finish().unwrap();
        for data in [xml.as_bytes(), zipped.as_slice()] {
            let (fragment, tree, report) = import(data, &ImportOptions::default(), Some(FrameRate::FPS_30)).unwrap();
            let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
            assert_eq!((seq.settings.width, seq.settings.height), (1080, 1920));
            let clip = &seq.video_tracks[0].items[0];
            assert_eq!(
                (clip.start, clip.duration, clip.source_in, clip.source_out(), clip.speed),
                (Tick::ZERO, Tick(3 * TPS), Tick(4 * TPS), Tick(10 * TPS), 2.0)
            );
            assert_eq!(tree.sequences[0].tracks[0].items[0].media_path.as_deref(), Some("/missing/camera.mov"));
            assert!(report.mentions("using title fallback"));
            let ItemKind::Media(m) = &fragment.project.item(clip.item).unwrap().kind else { panic!() };
            assert!(m.offline);
        }
    }
    #[test]
    fn corrupt_sizes_ticks_speeds_entities_and_duplicate_ids_are_errors() {
        let xml = fixture();
        let cases = [
            xml.replace("<Index>0", "<Index>999999999999"),
            xml.replace("<Start>0", "<Start>-1"),
            xml.replace("<Start>0", "<Start>NaN"),
            xml.replace(&format!("<End>{}", 3 * TPS), "<End>-1"),
            xml.replace(&format!("<End>{}", 3 * TPS), "<End>invalid"),
            xml.replace(&format!("<OutPoint>{}", 10 * TPS), "<OutPoint>-1"),
            xml.replace(&format!("<OutPoint>{}", 10 * TPS), "<OutPoint>invalid"),
            xml.replace("<PlaybackSpeed>2", "<PlaybackSpeed>invalid"),
            xml.replace("<InPoint>", "<InPoint>999999999999999999999"),
            xml.replace("<PremiereData Version=\"3\">", "<!DOCTYPE x [<!ENTITY a SYSTEM 'file:///etc/passwd'>]><PremiereData Version=\"3\">"),
            xml.replace("<Media>", "<Media ObjectID=\"20\">"),
            xml.replace("1080</MZ.Sequence", "4294967295</MZ.Sequence"),
            mixed_support_fixture(3 * TPS, "invalid", "", "1"),
        ];
        for case in cases {
            let r = std::panic::catch_unwind(|| import(case.as_bytes(), &ImportOptions::default(), None));
            assert!(r.is_ok());
            assert!(r.unwrap().is_err());
        }
    }
    // Hand-written additions keep supported clips before and after one unsupported item.
    fn mixed_support_fixture(end: i64, source_in: &str, source_out: &str, speed: &str) -> String {
        let extra = format!(
            r#"<VideoClipTrackItem ObjectID="unsupported"><ClipTrackItem><TrackItem><Start>{}</Start><End>{end}</End></TrackItem><SubClip ObjectRef="unsupported-sub"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="unsupported-sub"><Name>Unsupported clip</Name><Clip ObjectRef="unsupported-clip"/></SubClip>
<VideoClip ObjectID="unsupported-clip"><Clip><InPoint>{source_in}</InPoint>{source_out}<PlaybackSpeed>{speed}</PlaybackSpeed></Clip></VideoClip>
<VideoClipTrackItem ObjectID="after"><ClipTrackItem><TrackItem><Start>{}</Start><End>{}</End></TrackItem><SubClip ObjectRef="21"/></ClipTrackItem></VideoClipTrackItem>"#,
            3 * TPS,
            4 * TPS,
            5 * TPS,
        );
        fixture()
            .replace(r#"<TrackItem ObjectRef="20"/>"#, r#"<TrackItem ObjectRef="20"/><TrackItem ObjectRef="unsupported"/><TrackItem ObjectRef="after"/>"#)
            .replace("</PremiereData>", &format!("{extra}</PremiereData>"))
    }
    #[test]
    fn unsupported_clips_are_skipped_without_losing_supported_neighbors() {
        let cases = [
            mixed_support_fixture(3 * TPS, "0", "", "1"),
            mixed_support_fixture(2 * TPS, "0", "", "1"),
            mixed_support_fixture(4 * TPS, "0", "", "0"),
            mixed_support_fixture(4 * TPS, "0", "", "-1"),
            mixed_support_fixture(4 * TPS, "0", "", "0.0009"),
            mixed_support_fixture(4 * TPS, "0", "", "1000.001"),
            mixed_support_fixture(4 * TPS, "0", "", "NaN"),
            mixed_support_fixture(4 * TPS, "0", "", "inf"),
            mixed_support_fixture(4 * TPS, "0", "", "-inf"),
            mixed_support_fixture(4 * TPS, "1", "<OutPoint>0</OutPoint>", "1"),
            mixed_support_fixture(4 * TPS, &Tick::MAX.0.to_string(), "", "1"),
            mixed_support_fixture(4 * TPS, &(Tick::MAX.0 - TPS).to_string(), "", "1000"),
        ];
        for (index, xml) in cases.iter().enumerate() {
            let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None)).unwrap();
            let (fragment, tree, report) = result.unwrap_or_else(|error| panic!("case {index}: {error}"));
            let items = &tree.sequences[0].tracks[0].items;
            assert_eq!(items.iter().map(|item| item.object_id.as_str()).collect::<Vec<_>>(), ["20", "after"], "case {index}");
            let clips = &fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items;
            assert_eq!(clips.iter().map(|clip| clip.start).collect::<Vec<_>>(), [Tick::ZERO, Tick(4 * TPS)]);
            assert!(
                report.entries.iter().any(|entry| entry.message.contains("Unsupported clip") && entry.message.contains("skipped")),
                "case {index}: {report:?}"
            );
        }
        for speed in ["0.001", "1000"] {
            let xml = mixed_support_fixture(4 * TPS, "0", &format!("<OutPoint>{}</OutPoint>", 1000 * TPS), speed);
            let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
            assert_eq!(tree.sequences[0].tracks[0].items.len(), 3);
            assert!(!report.mentions("clip skipped"));
        }
    }
    #[test]
    fn unsupported_clips_do_not_bypass_timeline_item_limit() {
        let xml = fixture()
            .replace(&format!("<End>{}</End>", 3 * TPS), "<End>0</End>")
            .replace(r#"<TrackItem ObjectRef="20"/>"#, &r#"<TrackItem ObjectRef="20"/>"#.repeat(100_001));
        let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None)).unwrap();
        assert!(result.unwrap_err().to_string().contains("too many timeline items"));
    }
    #[test]
    fn repeated_effect_warnings_keep_late_media_diagnostics_visible() {
        let pointers: String = (0..2000).map(|index| format!(r#"<TrackItem ObjectRef="effect-item-{index}"/>"#)).collect();
        let items: String = (0..2000)
            .map(|index| {
                format!(
                    r#"<VideoClipTrackItem ObjectID="effect-item-{index}"><ClipTrackItem><TrackItem><Start>{}</Start><End>{}</End></TrackItem><SubClip ObjectRef="effect-sub-{index}"/></ClipTrackItem><ComponentOwner><Components ObjectRef="effects"/></ComponentOwner></VideoClipTrackItem>
<SubClip ObjectID="effect-sub-{index}"><Name>Effect clip {index}</Name><Clip ObjectRef="22"/></SubClip>"#,
                    index * TPS,
                    (index + 1) * TPS,
                )
            })
            .collect();
        let extra = format!(
            r#"{items}<VideoMediaSource ObjectID="media-source"><MediaSource><Media ObjectURef="media"/></MediaSource></VideoMediaSource>
<VideoComponentChain ObjectID="effects"><Components><Component ObjectRef="unknown-effect"/></Components></VideoComponentChain>
<VideoFilterComponent ObjectID="unknown-effect"><DisplayName>Example effect</DisplayName><MatchName>example.unknown</MatchName></VideoFilterComponent>
<VideoClipTrackItem ObjectID="unresolved"><ClipTrackItem><TrackItem><Start>{}</Start><End>{}</End></TrackItem><SubClip ObjectRef="unresolved-sub"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="unresolved-sub"><Name>Unresolved media clip</Name><Clip ObjectRef="unresolved-clip"/></SubClip>
<VideoClip ObjectID="unresolved-clip"><Clip><InPoint>0</InPoint><OutPoint>{TPS}</OutPoint></Clip></VideoClip></PremiereData>"#,
            2000 * TPS,
            2001 * TPS,
        );
        let xml = fixture()
            .replace(r#"<TrackItem ObjectRef="20"/>"#, &format!(r#"{pointers}<TrackItem ObjectRef="unresolved"/>"#))
            .replace("<Media>", r#"<Media ObjectUID="media">"#)
            .replace("<Clip><InPoint>", r#"<Clip><Source ObjectRef="media-source"/><InPoint>"#)
            .replace("</PremiereData>", &extra);
        let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(tree.sequences[0].tracks[0].items.len(), 2001);
        assert!(report.entries.iter().any(|entry| entry.message.contains("Unresolved media clip") && entry.message.contains("media unresolved")), "{report:?}");
        let effects: Vec<_> =
            report.entries.iter().filter(|entry| entry.message.contains("effect Example effect retained in provenance, not rendered")).collect();
        assert_eq!(effects.len(), 1);
        assert!(effects[0].message.contains("2000"));
        assert!(!report.mentions("further import diagnostics omitted"));
    }
    #[test]
    fn self_nesting_refused_and_ambiguous_media_stays_offline() {
        let cycle = fixture().replace("<Name>camera.mov</Name>", "<Name>Sequence 01</Name>");
        assert!(import(cycle.as_bytes(), &ImportOptions::default(), None).unwrap_err().to_string().contains("cyclic"));
        let unsupported_cycle = cycle.replace(&format!("<End>{}</End>", 3 * TPS), "<End>0</End>");
        assert!(import(unsupported_cycle.as_bytes(), &ImportOptions::default(), None).unwrap_err().to_string().contains("cyclic"));
        let xml = fixture().replace("</PremiereData>", "<Media><Title>camera.mov</Title><FilePath>/different.mov</FilePath></Media></PremiereData>");
        let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert!(tree.sequences[0].tracks[0].items[0].media_path.is_none());
        assert!(report.mentions("ambiguous"));
    }
    // Hand-written XML: many pointers share one small caption item and one large payload.
    fn repeated_caption_block(pointers: usize) -> String {
        let mut bytes = vec![0; 3 * (1024 * 1024 / 4)];
        bytes[..4].copy_from_slice(&4u32.to_le_bytes());
        bytes[4..8].copy_from_slice(b"Test");
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        assert_eq!(encoded.len(), 1024 * 1024);
        let pointers = r#"<TrackItem ObjectRef="caption"/>"#.repeat(pointers);
        format!(
            r#"<PremiereData><Sequence ObjectUID="sequence"><Name>Captions</Name><TrackGroups><TrackGroup><Second ObjectRef="group"/></TrackGroup></TrackGroups></Sequence>
<CaptionTrackGroup ObjectID="group"><TrackGroup><Tracks><Track ObjectURef="track"/></Tracks></TrackGroup></CaptionTrackGroup>
<CaptionTrack ObjectUID="track"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems>{pointers}</TrackItems></ClipItems></ClipTrack></CaptionTrack>
<CaptionTrackItem ObjectID="caption"><TrackItem><Start>0</Start><End>254016000000</End></TrackItem><BlockVectorItem ObjectRef="block"/></CaptionTrackItem>
<BlockVectorItem ObjectID="block"><FormattedTextData>{encoded}</FormattedTextData></BlockVectorItem></PremiereData>"#
        )
    }
    #[test]
    fn repeated_large_caption_blocks_exhaust_import_work_budget() {
        let small = repeated_caption_block(2);
        let (fragment, tree, _) = import(small.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(tree.sequences[0].tracks[0].items.len(), 2);
        let captions = &fragment.project.sequence(fragment.sequences[0]).unwrap().caption_tracks[0].captions;
        assert_eq!(captions.len(), 2);
        assert!(captions.iter().all(|caption| caption.text == "Test"));

        let repeated = repeated_caption_block(400);
        assert!(repeated.len() < MAX_DOCUMENT_BYTES);
        match import(repeated.as_bytes(), &ImportOptions::default(), None) {
            Err(error) => assert!(error.to_string().contains("import work limit"), "{error}"),
            Ok(_) => panic!("repeated caption payload bypassed the import work limit"),
        }
    }
    #[test]
    fn a_shared_text_payload_is_decoded_once() {
        let doc = roxmltree::Document::parse("<FormattedTextData/>").unwrap();
        let element = doc.root_element();
        let mut bytes = b"synthetic-envelope".to_vec();
        bytes.extend(4u32.to_le_bytes());
        bytes.extend(b"Test\0");
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mut decoded = DecodedText::default();
        assert_eq!(decoded.get(element, Some(&encoded)).as_deref(), Some("Test"));
        // Later references to the same element reuse that result instead of reading it again.
        assert_eq!(decoded.get(element, Some("not decoded again")).as_deref(), Some("Test"));
        assert_eq!(decoded.get(element, None).as_deref(), Some("Test"));
        // Many caption items sharing one large block all show its text.
        let (fragment, _, _) = import(repeated_caption_block(40).as_bytes(), &ImportOptions::default(), None).unwrap();
        let captions = &fragment.project.sequence(fragment.sequences[0]).unwrap().caption_tracks[0].captions;
        assert_eq!(captions.len(), 40);
        assert!(captions.iter().all(|caption| caption.text == "Test"));
    }
    #[test]
    fn missing_caption_block_references_remain_optional() {
        let xml = repeated_caption_block(1);
        for xml in [xml.replace(r#"<BlockVectorItem ObjectRef="block"/>"#, ""), xml.replace(r#"ObjectRef="block""#, r#"ObjectRef="absent""#)] {
            let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
            assert!(tree.sequences[0].tracks[0].items[0].text.is_none());
            assert!(report.mentions("caption text could not be decoded"));
        }
    }
    #[test]
    fn caption_unicode_envelope_and_gzip_bomb_bounds() {
        let text = "Maybe it fits\rIt’s a 2 + 2.";
        let mut bytes = b"binary-prefix".to_vec();
        bytes.extend((text.chars().count() as u32).to_le_bytes());
        bytes.extend(text.as_bytes());
        bytes.extend([0, 0]);
        assert_eq!(caption_text(&base64::engine::general_purpose::STANDARD.encode(bytes)).as_deref(), Some("Maybe it fits\nIt’s a 2 + 2."));
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let chunk = [b' '; 65536];
        for _ in 0..=MAX_DOCUMENT_BYTES / chunk.len() {
            gz.write_all(&chunk).unwrap();
        }
        assert!(import(&gz.finish().unwrap(), &ImportOptions::default(), None).unwrap_err().to_string().contains("expanded project exceeds 512 MiB"));
    }
    #[test]
    fn caption_payload_truncation_bit_flips_and_size_limits_never_panic() {
        let mut bytes = b"synthetic-envelope".to_vec();
        let text = "Caption\rUnicode: é 日本";
        bytes.extend((text.len() as u32).to_le_bytes());
        bytes.extend(text.as_bytes());
        bytes.push(0);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        assert_eq!(caption_text(&encoded).as_deref(), Some("Caption\nUnicode: é 日本"));
        for end in 0..=encoded.len() {
            assert!(std::panic::catch_unwind(|| caption_text(&encoded[..end])).is_ok());
        }
        for index in 0..bytes.len() {
            for bit in 0..8 {
                let mut mutated = bytes.clone();
                mutated[index] ^= 1 << bit;
                let encoded = base64::engine::general_purpose::STANDARD.encode(mutated);
                assert!(std::panic::catch_unwind(|| caption_text(&encoded)).is_ok());
            }
        }
        for index in 0..encoded.len() {
            let mut mutated = encoded.as_bytes().to_vec();
            mutated[index] ^= 1;
            let mutated = String::from_utf8(mutated).unwrap();
            assert!(std::panic::catch_unwind(|| caption_text(&mutated)).is_ok());
        }
        for payload in ["A".repeat(1024 * 1024 + 1), " ".repeat(1024 * 1024 + 1)] {
            assert_eq!(std::panic::catch_unwind(|| caption_text(&payload)).unwrap(), None);
        }
    }
    #[test]
    fn inverted_transition_is_skipped_without_losing_clip_or_valid_transition() {
        let xml = fixture()
            .replace("<SubClip ObjectRef=\"21\"/>", "<SubClip ObjectRef=\"21\"/><HeadTransition ObjectRef=\"bad\"/><TailTransition ObjectRef=\"good\"/>")
            .replace(
                "</PremiereData>",
                r#"<Transition ObjectID="bad"><DisplayName>Bad fade</DisplayName><Start>2</Start><End>1</End></Transition>
<Transition ObjectID="good"><DisplayName>Cross Dissolve</DisplayName><Start>0</Start><End>25401600000</End></Transition></PremiereData>"#,
            );
        let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(tree.sequences[0].tracks[0].items.len(), 1);
        let transitions = &tree.sequences[0].tracks[0].items[0].transitions;
        assert_eq!(transitions.len(), 1);
        assert!(!transitions[0].head);
        assert_eq!(fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].transitions.len(), 1);
        assert!(report.entries.iter().any(|e| e.message.contains("camera.mov") && e.message.contains("Bad fade") && e.message.contains("transition skipped")));
    }
    #[test]
    fn native_nested_sequences_mute_and_edge_fades_are_preserved() {
        let xml=fixture().replace("<IsMuted>false", "<IsMuted>true").replace("<SubClip ObjectRef=\"21\"/>","<SubClip ObjectRef=\"21\"/><HeadTransition ObjectRef=\"30\"/>")
            .replace("</PremiereData>",&format!(r#"<Transition ObjectID="30"><DisplayName>Cross Dissolve</DisplayName><Start>-63504000000</Start><End>{}</End></Transition>
<Sequence ObjectUID="parent"><Name>Parent</Name><TrackGroups><TrackGroup><Second ObjectRef="40"/></TrackGroup></TrackGroups></Sequence>
<VideoTrackGroup ObjectID="40"><TrackGroup><Tracks><Track ObjectURef="pt"/></Tracks></TrackGroup></VideoTrackGroup>
<VideoClipTrack ObjectUID="pt"><ClipTrack><Track><Index>2</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="41"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<VideoClipTrackItem ObjectID="41"><TrackItem><Start>{}</Start><End>{}</End></TrackItem><SubClip ObjectRef="42"/></VideoClipTrackItem>
<SubClip ObjectID="42"><Name>Sequence 01</Name><Clip ObjectRef="43"/></SubClip><Clip ObjectID="43"><InPoint>{}</InPoint><OutPoint>{}</OutPoint></Clip></PremiereData>"#,TPS/4,5*TPS,7*TPS,TPS,3*TPS));
        let (fragment, _, report) = import(xml.as_bytes(), &ImportOptions::default(), Some(FrameRate::FPS_30)).unwrap();
        let child = fragment.project.sequence(fragment.sequences[0]).unwrap();
        assert!(!child.video_tracks[0].enabled);
        assert_eq!(child.video_tracks[0].transitions[0].duration, Tick(TPS / 2));
        assert!(report.mentions("clip-edge fade"));
        let parent = fragment.project.sequence(fragment.sequences[1]).unwrap();
        let clip = &parent.video_tracks[2].items[0];
        assert_eq!(clip.item, fragment.sequences[0]);
        assert_eq!((clip.start, clip.duration, clip.source_in), (Tick(5 * TPS), Tick(2 * TPS), Tick(TPS)));
    }
    #[test]
    fn nesting_depth_is_checked_even_with_shortcuts_to_shared_sequences() {
        let item = |name: String| Item {
            object_id: String::new(),
            link: None,
            audio: BTreeMap::new(),
            video_effects: Vec::new(),
            graphic: false,
            disabled: false,
            label: name.clone(),
            start: Tick::ZERO,
            end: Tick(TPS),
            source_in: Tick::ZERO,
            source_out: Tick(TPS),
            speed: 1.0,
            media_path: None,
            synthetic: None,
            adjustment_layer: None,
            media_markers: Vec::new(),
            nested_sequence: Some(name),
            text: None,
            audio_category: None,
            effects: vec![],
            motion: BTreeMap::new(),
            motion_keys: BTreeMap::new(),
            transitions: vec![],
        };
        let mut seqs = Vec::new();
        for i in 0..65 {
            let items = if i < 64 { vec![item("64".into()), item((i + 1).to_string())] } else { vec![] };
            seqs.push(Sequence {
                object_uid: i.to_string(),
                name: i.to_string(),
                width: 16,
                height: 16,
                frame_rate: None,
                sample_rate: None,
                work_out: Tick::ZERO,
                markers: Vec::new(),
                tracks: vec![Track { kind: "video".into(), index: 0, muted: false, name: None, locked: false, items }],
            });
        }
        assert!(validate_nesting(&seqs).is_err());
        seqs.remove(0);
        assert!(validate_nesting(&seqs).is_ok());
    }
    #[test]
    fn truncated_and_mutated_documents_never_panic() {
        let data = fixture().into_bytes();
        for n in (0..data.len()).step_by(7) {
            assert!(std::panic::catch_unwind(|| import(&data[..n], &ImportOptions::default(), None)).is_ok());
        }
        for n in (0..data.len()).step_by(11) {
            let mut changed = data.clone();
            changed[n] ^= 0xff;
            assert!(std::panic::catch_unwind(|| import(&changed, &ImportOptions::default(), None)).is_ok());
        }
        for data in [&[0x1f, 0x8b][..], &[0x1f, 0x8b, 0x08, 0xff][..], b"<PremiereData/>".as_slice()] {
            assert!(import(data, &ImportOptions::default(), None).is_err());
        }
    }

    #[test]
    fn text_uses_match_name_and_not_timeline_label() {
        let xml = fixture().replace("camera.mov", "Título renombrado").replace(
            "<SubClip ObjectRef=\"21\"/>",
            "<SubClip ObjectRef=\"21\"/><ComponentOwner><Components ObjectRef=\"text-chain\"/></ComponentOwner>",
        ).replace("</PremiereData>", r#"<VideoComponentChain ObjectID="text-chain"><Components><Component ObjectRef="text-component"/></Components></VideoComponentChain>
<VideoFilterComponent ObjectID="text-component"><Component><DisplayName>Texto</DisplayName><InstanceName>Renamed layer</InstanceName><Params><Param ObjectRef="source-text"/></Params></Component><MatchName>AE.ADBE Text</MatchName></VideoFilterComponent>
<ArbVideoComponentParam ObjectID="source-text"><IsTimeVarying>false</IsTimeVarying><ParameterID>1</ParameterID><Name>Texto de origen</Name><StartKeyframeValue Encoding="base64">SOURCE_TEXT</StartKeyframeValue></ArbVideoComponentParam></PremiereData>"#);
        // Hand-written envelope: length-prefixed UTF-8, independent of the layer's name.
        let mut bytes = b"synthetic-envelope".to_vec();
        bytes.extend(10u32.to_le_bytes());
        bytes.extend(b"Hola mundo\0");
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let xml = xml.replace("SOURCE_TEXT", &encoded);
        let (fragment, tree, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(tree.sequences[0].tracks[0].items[0].text.as_deref(), Some("Hola mundo"));
        assert!(fragment.project.items.values().any(|i| matches!(i.kind, ItemKind::Graphic { .. })));
        let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
        assert!(
            seq.video_tracks[0].items[0]
                .effects
                .iter()
                .any(|e| e.params.get("text").is_some_and(|p| matches!(&p.value, ParamValue::Text(t) if t == "Hola mundo")))
        );
        let unknown = xml.replace("AE.ADBE Text", "custom.not-text").replace("Texto", "Text");
        let (_, tree, _) = import(unknown.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert!(tree.sequences[0].tracks[0].items[0].text.is_none());
        for xml in
            [xml.replace(&encoded, "invalid!"), xml.replace("<IsTimeVarying>false", "<IsTimeVarying>true"), xml.replace("<ParameterID>1", "<ParameterID>99")]
        {
            let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
            assert!(tree.sequences[0].tracks[0].items[0].text.is_none());
            assert!(report.mentions("Source Text") && report.mentions("layer name"));
        }
    }
    #[test]
    fn clip_slipped_before_source_start_imports_at_source_start() {
        // Hand-written: a clip slipped earlier than its source's first frame has a negative in point.
        let xml = fixture().replace(&format!("<InPoint>{}", 4 * TPS), &format!("<InPoint>-{}", TPS / 3));
        let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        let item = &tree.sequences[0].tracks[0].items[0];
        assert_eq!((item.start, item.end, item.source_in, item.source_out), (Tick::ZERO, Tick(3 * TPS), Tick::ZERO, Tick(10 * TPS)));
        assert_eq!(fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0].source_in, Tick::ZERO);
        assert!(report.entries.iter().any(|e| e.message.contains("camera.mov") && e.message.contains("in point")), "{report:?}");
    }
    #[test]
    fn disabled_clips_import_disabled() {
        // Hand-written: Clip > Enable is stored as the clip track item's IsMuted.
        let xml = fixture().replace("<SubClip ObjectRef=\"21\"/></ClipTrackItem>", "<SubClip ObjectRef=\"21\"/><IsMuted>true</IsMuted></ClipTrackItem>");
        let (fragment, tree, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert!(tree.sequences[0].tracks[0].items[0].disabled);
        let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
        assert!(!seq.video_tracks[0].items[0].enabled);
        assert!(seq.video_tracks[0].enabled, "a disabled clip does not disable its track");
        let (fragment, _, _) = import(fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
        assert!(fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0].enabled);
    }
    #[test]
    fn track_names_and_locks_are_imported() {
        // Hand-written: a renamed, locked track; names are user text, never resolved as references.
        let xml = fixture().replace(
            "<Track><Index>0</Index>",
            "<Track><Node><Properties><MZ.TrackName>Interviews</MZ.TrackName></Properties></Node><Index>0</Index><IsLocked>true</IsLocked>",
        );
        let (fragment, tree, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!((tree.sequences[0].tracks[0].name.as_deref(), tree.sequences[0].tracks[0].locked), (Some("Interviews"), true));
        let track = &fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0];
        assert_eq!((track.name.as_str(), track.locked), ("Interviews", true));
        let (default, _, _) = import(fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
        let track = &default.project.sequence(default.sequences[0]).unwrap().video_tracks[0];
        let blank = xml.replace("<MZ.TrackName>Interviews", "<MZ.TrackName> ");
        let (blank, _, _) = import(blank.as_bytes(), &ImportOptions::default(), None).unwrap();
        let blank = &blank.project.sequence(blank.sequences[0]).unwrap().video_tracks[0];
        assert!(!track.locked && !track.name.is_empty());
        assert_eq!(blank.name, track.name, "a blank stored name keeps the generated name");
        let long = xml.replace("Interviews", &"n".repeat(4097));
        assert!(import(long.as_bytes(), &ImportOptions::default(), None).unwrap_err().to_string().contains("track name"));
    }
    #[test]
    fn track_mute_and_index_come_from_the_track_element_only() {
        // Hand-written: the track's own `ClipTrack/Track` stores neither IsMuted nor Index; an
        // element nested deeper in the track (an inlined clip track item) stores both.
        let xml = fixture().replace(
            r#"<Track><Index>0</Index><IsMuted>false</IsMuted></Track><ClipItems><TrackItems><TrackItem ObjectRef="20"/></TrackItems>"#,
            r#"<Track></Track><ClipItems><TrackItems><TrackItem ObjectRef="20"/></TrackItems><ClipTrackItem><Index>5</Index><IsMuted>true</IsMuted></ClipTrackItem>"#,
        );
        let (fragment, tree, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        let track = &tree.sequences[0].tracks[0];
        assert_eq!((track.index, track.muted), (0, false), "a nested clip's fields are not the track's");
        let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
        assert_eq!(seq.video_tracks.len(), 1);
        assert!(seq.video_tracks[0].enabled && !seq.video_tracks[0].muted);
        // The track's own fields still apply.
        let own = fixture().replace("<Index>0</Index><IsMuted>false", "<Index>2</Index><IsMuted>true");
        let (_, tree, _) = import(own.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!((tree.sequences[0].tracks[0].index, tree.sequences[0].tracks[0].muted), (2, true));
    }
    // Hand-written marker records in the DVAMarker JSON shape; colours are 0xAABBGGRR.
    fn marker_fixture() -> String {
        let record = |id: &str, json: &str| format!(r#"<Marker ObjectID="{id}"><DVAMarker>{}</DVAMarker></Marker>"#, json.replace('"', "&quot;"));
        let markers = [
            record("m-comment", r#"{"DVAMarker":{"mComment":"with end","mCuePointList":[{"mKey":"k","mValue":"{\"color\":4281740498}"}],"mDuration":{"ticks":254016000000},"mName":"comment","mStartTime":{"ticks":127008000000},"mType":"Comment"}}"#),
            record("m-web", r#"{"DVAMarker":{"mLocation":"https://example.com","mName":"web","mStartTime":{"ticks":"381024000000"},"mType":"WebLink"}}"#),
            record("m-chapter", r#"{"DVAMarker":{"mName":"chapter","mStartTime":{"ticks":0},"mType":"Chapter"}}"#),
            record("m-bad", "not json"),
            record("m-media", r#"{"DVAMarker":{"mName":"media note","mStartTime":{"ticks":63504000000},"mType":"Segmentation"}}"#),
        ]
        .concat();
        fixture()
            .replace("<Name>Sequence 01</Name>", r#"<MarkerOwner><Markers ObjectRef="seq-markers"/></MarkerOwner><Name>Sequence 01</Name>"#)
            .replace(r#"<Name>camera.mov</Name><Clip ObjectRef="22"/>"#, r#"<Name>camera.mov</Name><MasterClip ObjectURef="master"/><Clip ObjectRef="22"/>"#)
            .replace(
                "</PremiereData>",
                &format!(
                    r#"<Markers ObjectID="seq-markers"><Markers><Marker Index="0"><Second ObjectRef="m-comment"/></Marker><Marker Index="1"><Second ObjectRef="m-web"/></Marker><Marker Index="2"><Second ObjectRef="m-chapter"/></Marker><Marker Index="3"><Second ObjectRef="m-bad"/></Marker></Markers></Markers>
<MasterClip ObjectUID="master"><Clips><Clip Index="0" ObjectRef="master-clip"/></Clips></MasterClip>
<VideoClip ObjectID="master-clip"><Clip><MarkerOwner><Markers ObjectRef="media-markers"/></MarkerOwner></Clip></VideoClip>
<Markers ObjectID="media-markers"><Markers><Marker Index="0"><Second ObjectRef="m-media"/></Marker></Markers></Markers>
{markers}</PremiereData>"#
                ),
            )
    }
    #[test]
    fn sequence_and_master_clip_markers_are_imported() {
        let (fragment, _, report) = import(marker_fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
        let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
        let got: Vec<_> = seq.markers.iter().map(|m| (m.start, m.duration, m.name.as_str(), m.comment.as_str(), m.kind)).collect();
        assert_eq!(
            got,
            [
                (Tick::ZERO, Tick::ZERO, "chapter", "", filmcraft_project::MarkerKind::Chapter),
                (Tick(TPS / 2), Tick(TPS), "comment", "with end", filmcraft_project::MarkerKind::Comment),
                (Tick(3 * TPS / 2), Tick::ZERO, "web", "", filmcraft_project::MarkerKind::WebLink),
            ]
        );
        assert_eq!(seq.markers[1].color, Label::Rose, "red cue-point colour");
        assert_eq!(seq.markers[0].color, Label::Green, "Premiere's default marker colour");
        assert!(seq.markers.iter().all(|m| seq.markers.iter().filter(|o| o.id == m.id).count() == 1));
        let urls: Vec<_> = seq.markers.iter().map(|m| m.url.as_str()).collect();
        assert_eq!(urls, ["", "", "https://example.com"], "a web link's URL is imported");
        assert!(!report.mentions("URL"), "{report:?}");
        assert!(report.mentions("marker could not be read"), "{report:?}");
        let clip = &seq.video_tracks[0].items[0];
        let ItemKind::Media(media) = &fragment.project.item(clip.item).unwrap().kind else { panic!() };
        assert_eq!(
            media.markers.iter().map(|m| (m.start, m.name.as_str(), m.kind)).collect::<Vec<_>>(),
            [(Tick(TPS / 4), "media note", filmcraft_project::MarkerKind::Segmentation)]
        );
    }
    #[test]
    fn master_clip_markers_are_read_once_however_often_the_media_is_cut() {
        // A second cut of the same media; its master clip also holds a web-link marker and an
        // unreadable record, whose warning would repeat for every cut if the markers were read
        // per timeline item.
        let xml = marker_fixture()
            .replace(r#"<TrackItem ObjectRef="20"/>"#, r#"<TrackItem ObjectRef="20"/><TrackItem ObjectRef="20b"/>"#)
            .replace(
                r#"<Second ObjectRef="m-media"/></Marker>"#,
                r#"<Second ObjectRef="m-media"/></Marker><Marker Index="1"><Second ObjectRef="m-web"/></Marker><Marker Index="2"><Second ObjectRef="m-bad"/></Marker>"#,
            )
            .replace(
                "</PremiereData>",
                &format!(
                    r#"<VideoClipTrackItem ObjectID="20b"><ClipTrackItem><TrackItem><Start>{}</Start><End>{}</End></TrackItem><SubClip ObjectRef="21"/></ClipTrackItem></VideoClipTrackItem></PremiereData>"#,
                    3 * TPS,
                    6 * TPS
                ),
            );
        let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        let items = &tree.sequences[0].tracks[0].items;
        assert_eq!(items.iter().map(|i| i.media_markers.len()).collect::<Vec<_>>(), [2, 0]);
        assert_eq!(report.entries.iter().filter(|e| e.message.contains("camera.mov: a marker could not be read")).count(), 1, "{report:?}");
        let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
        let ItemKind::Media(media) = &fragment.project.item(seq.video_tracks[0].items[1].item).unwrap().kind else { panic!() };
        assert_eq!(media.markers.len(), 2, "every cut shows the media's markers");
        assert!(media.markers.iter().any(|m| m.url == "https://example.com"));
    }
    #[test]
    fn a_skipped_cut_leaves_its_master_clip_markers_to_a_kept_cut() {
        // Hand-written: before the kept cut, a zero-length cut of the same subclip and a cut at an
        // unsupported speed of another subclip of the same master clip. The import skips both.
        let xml = marker_fixture()
            .replace(r#"<TrackItem ObjectRef="20"/>"#, r#"<TrackItem ObjectRef="zero-length"/><TrackItem ObjectRef="zero-speed"/><TrackItem ObjectRef="20"/>"#)
            .replace(
                "</PremiereData>",
                &format!(
                    r#"<VideoClipTrackItem ObjectID="zero-length"><ClipTrackItem><TrackItem><Start>0</Start><End>0</End></TrackItem><SubClip ObjectRef="21"/></ClipTrackItem></VideoClipTrackItem>
<VideoClipTrackItem ObjectID="zero-speed"><ClipTrackItem><TrackItem><Start>0</Start><End>{TPS}</End></TrackItem><SubClip ObjectRef="zero-speed-sub"/></ClipTrackItem></VideoClipTrackItem>
<SubClip ObjectID="zero-speed-sub"><Name>camera.mov</Name><MasterClip ObjectURef="master"/><Clip ObjectRef="zero-speed-clip"/></SubClip>
<VideoClip ObjectID="zero-speed-clip"><Clip><InPoint>0</InPoint><PlaybackSpeed>0</PlaybackSpeed></Clip></VideoClip></PremiereData>"#
                ),
            );
        let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(report.entries.iter().filter(|e| e.message.contains("clip skipped")).count(), 2, "{report:?}");
        let items = &tree.sequences[0].tracks[0].items;
        assert_eq!(items.iter().map(|i| (i.object_id.as_str(), i.media_markers.len())).collect::<Vec<_>>(), [("20", 1)], "{report:?}");
        let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
        let ItemKind::Media(media) = &fragment.project.item(seq.video_tracks[0].items[0].item).unwrap().kind else { panic!() };
        assert_eq!(media.markers.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["media note"]);
    }
    #[test]
    fn marker_urls_are_kept_as_bounded_text_and_opened_only_through_the_web_url_check() {
        // `url` is JSON text inside the XML-escaped DVAMarker record.
        let with_url = |url: &str| {
            let json = serde_json::to_string(url).unwrap().replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;");
            let xml = marker_fixture().replace("&quot;https://example.com&quot;", &json);
            let (fragment, _, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
            let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
            let web = seq.markers.iter().find(|m| m.name == "web").unwrap();
            (web.url.clone(), report.entries.iter().any(|e| e.message.contains("marker web") && e.message.contains("URL")))
        };
        assert_eq!(with_url("  https://example.com/review?q=1  "), ("https://example.com/review?q=1".into(), false), "trimmed");
        // Imported text stays editable but never becomes permission to open another handler.
        let (kept, warned) = with_url("javascript:alert(1)");
        assert_eq!((kept.as_str(), warned), ("javascript:alert(1)", false));
        assert!(filmcraft_project::marker_web_url(&kept).is_none());
        // What a marker edit could never store is not imported, and the report says so.
        for dropped in [format!("https://example.com/{}", "a".repeat(8192)), "https://exa\nmple.com".into(), "https://example.com/\u{7}".into()] {
            assert_eq!(with_url(&dropped), (String::new(), true), "{dropped:?}");
        }
        let at_limit = format!("https://example.com/{}", "a".repeat(8192 - "https://example.com/".len()));
        assert_eq!(with_url(&at_limit), (at_limit.clone(), false));
    }
    #[test]
    fn more_markers_than_the_limit_are_truncated_with_a_warning() {
        let entries: String = (0..10_001).map(|i| format!(r#"<Marker Index="{i}"><Second ObjectRef="m-chapter"/></Marker>"#)).collect();
        let xml = marker_fixture().replace(
            r#"<Marker Index="0"><Second ObjectRef="m-comment"/></Marker><Marker Index="1"><Second ObjectRef="m-web"/></Marker><Marker Index="2"><Second ObjectRef="m-chapter"/></Marker><Marker Index="3"><Second ObjectRef="m-bad"/></Marker>"#,
            &entries,
        );
        let (fragment, _, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(fragment.project.sequence(fragment.sequences[0]).unwrap().markers.len(), 10_000);
        assert!(report.mentions("more than 10000 markers"), "{report:?}");
    }
    #[test]
    fn hostile_marker_records_never_panic_or_escape_bounds() {
        let base = marker_fixture();
        for bad in ["{\"ticks\":-5}", "{\"ticks\":9223372036854775807}", "{\"ticks\":1.5}", "{\"ticks\":\"x\"}", "[]", "null"] {
            let xml = base.replace("{&quot;ticks&quot;:0}", &bad.replace('"', "&quot;"));
            let result = std::panic::catch_unwind(|| import(xml.as_bytes(), &ImportOptions::default(), None)).unwrap();
            let (fragment, _, _) = result.unwrap_or_else(|e| panic!("{bad}: {e}"));
            let seq = fragment.project.sequence(fragment.sequences[0]).unwrap();
            assert!(seq.markers.iter().all(|m| m.start >= Tick::ZERO && m.duration >= Tick::ZERO && m.start.0 + m.duration.0 <= Tick::MAX.0), "{bad}");
        }
        let cyclic = base.replace(r#"<Second ObjectRef="m-comment"/>"#, r#"<Second ObjectRef="seq-markers"/>"#);
        assert!(std::panic::catch_unwind(|| import(cyclic.as_bytes(), &ImportOptions::default(), None)).unwrap().is_ok());
    }
    #[test]
    fn paths_saved_on_another_system_use_the_relative_path() {
        // Hand-written: Media keeps the absolute path where it was saved and a project-relative one.
        let media = |absolute: &str, relative: &str| {
            fixture().replace(
                "<ActualMediaFilePath>/missing/camera.mov</ActualMediaFilePath>",
                &format!("<ActualMediaFilePath>{absolute}</ActualMediaFilePath><RelativePath>{relative}</RelativePath>"),
            )
        };
        let read_with = |xml: &str, base: &str| read(xml.as_bytes(), Some(base), &mut Report::default()).unwrap();
        let cases = [
            ("/Users/editor/Shoot/camera.mov", r"..\Shoot\camera.mov", r"D:\Edit\Project", r"D:\Edit\Shoot\camera.mov"),
            (r"C:\Users\editor\Shoot\camera.mov", "../Shoot/camera.mov", "/home/editor/Project", "/home/editor/Shoot/camera.mov"),
            (r"\\server\share\camera.mov", "camera.mov", "/home/editor/Project", "/home/editor/Project/camera.mov"),
        ];
        for (absolute, relative, base, expected) in cases {
            let doc = read_with(&media(absolute, relative), base);
            assert_eq!(doc.sequences[0].tracks[0].items[0].media_path.as_deref(), Some(expected), "{absolute}");
            assert!(doc.relative_media.is_empty());
        }
        // Same system: the absolute path stays first; the relative one is the engine's fallback.
        let doc = read_with(&media("/Volumes/Shoot/camera.mov", "../Shoot/camera.mov"), "/Users/editor/Project");
        assert_eq!(doc.sequences[0].tracks[0].items[0].media_path.as_deref(), Some("/Volumes/Shoot/camera.mov"));
        assert_eq!(doc.relative_media.get("/Volumes/Shoot/camera.mov").map(String::as_str), Some("/Users/editor/Shoot/camera.mov"));
        // Without a project folder, or without a relative path, nothing changes.
        let doc = read(media("/Users/editor/Shoot/camera.mov", r"..\Shoot\camera.mov").as_bytes(), None, &mut Report::default()).unwrap();
        assert_eq!(doc.sequences[0].tracks[0].items[0].media_path.as_deref(), Some("/Users/editor/Shoot/camera.mov"));
        let doc = read_with(&fixture(), r"D:\Edit\Project");
        assert_eq!(doc.sequences[0].tracks[0].items[0].media_path.as_deref(), Some("/missing/camera.mov"));
        // A "relative" path that climbs onto another drive is never joined to the project folder.
        let doc = read_with(&media(r"C:\Users\editor\Shoot\camera.mov", r"..\..\C:\Users\editor\Shoot\camera.mov"), "/home/editor/Project");
        assert_eq!(doc.sequences[0].tracks[0].items[0].media_path.as_deref(), Some(r"C:\Users\editor\Shoot\camera.mov"));
        assert!(doc.relative_media.is_empty());
        let long = media("/Users/editor/camera.mov", &"r".repeat(16385));
        assert!(read(long.as_bytes(), Some("/x"), &mut Report::default()).unwrap_err().to_string().contains("too long"));
    }
    // Hand-written: generated media stores a four-character code as its file path, so its title
    // is UI-language text (here Spanish) and never a file to probe.
    fn generator_fixture(code: u32, title: &str) -> String {
        fixture().replace("<PlaybackSpeed>2</PlaybackSpeed>", "<PlaybackSpeed>1</PlaybackSpeed><Source ObjectRef=\"23\"/>").replace(
            "<Media><Title>camera.mov</Title><ActualMediaFilePath>/missing/camera.mov</ActualMediaFilePath></Media>",
            &format!(
                r#"<VideoMediaSource ObjectID="23"><MediaSource><Media ObjectURef="generated"/></MediaSource></VideoMediaSource>
<Media ObjectUID="generated"><FilePath>{code}</FilePath><Title>{title}</Title><ActualMediaFilePath>{code}</ActualMediaFilePath></Media>"#
            ),
        )
    }
    #[test]
    fn generated_media_imports_by_four_character_code() {
        use filmcraft_media::Generator;
        let code = |c: &[u8; 4]| u32::from_be_bytes(*c);
        let cases = [
            (b"BLAK", Generator::BlackVideo),
            (b"COLR", Generator::ColorMatte { color: [0.0, 0.0, 0.0, 1.0] }),
            (b"BARS", Generator::BarsAndTone),
            (b"TRNV", Generator::TransparentVideo),
            (b"LEAD", Generator::CountingLeader),
        ];
        for (fourcc, expected) in cases {
            let xml = generator_fixture(code(fourcc), "Vídeo generado");
            let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions { base_dir: Some("/project".into()), ..Default::default() }, None).unwrap();
            let item = &tree.sequences[0].tracks[0].items[0];
            assert_eq!(item.media_path, None, "a code is not a file path");
            assert!(tree.relative_media.is_empty());
            let clip = &fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0];
            let ItemKind::Media(media) = &fragment.project.item(clip.item).unwrap().kind else { panic!("{report:?}") };
            assert_eq!(media.media, filmcraft_project::MediaRef::Generator(expected.clone()));
            assert!(!media.offline);
            assert!(!report.mentions("unresolved"), "{report:?}");
            assert_eq!(report.mentions("colour is not read"), matches!(expected, Generator::ColorMatte { .. }), "{report:?}");
        }
        // A graphic's canvas is a native graphic, not media.
        let (fragment, _, _) = import(generator_fixture(code(b"GRFV"), "Gráfico").as_bytes(), &ImportOptions::default(), None).unwrap();
        let clip = &fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0];
        assert!(matches!(fragment.project.item(clip.item).unwrap().kind, ItemKind::Graphic { .. }));
        // An unknown code stays a visible offline placeholder and is reported, never probed as a path.
        let (fragment, tree, report) = import(generator_fixture(code(b"ZZZZ"), "Unknown").as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(tree.sequences[0].tracks[0].items[0].media_path, None);
        assert!(report.mentions("ZZZZ"), "{report:?}");
        let clip = &fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0];
        assert!(fragment.project.item(clip.item).and_then(|i| i.as_media()).is_some_and(|m| m.offline));
    }
    #[test]
    fn adjustment_layers_import_as_adjustment_layers() {
        // Hand-written: an adjustment layer is a master clip flagged IsAdjustmentLayer over black video.
        let xml = generator_fixture(u32::from_be_bytes(*b"BLAK"), "Capa de ajuste")
            .replace(r#"<Name>camera.mov</Name><Clip ObjectRef="22"/>"#, r#"<Name>Grade</Name><MasterClip ObjectURef="layer"/><Clip ObjectRef="22"/>"#)
            .replace("</PremiereData>", r#"<MasterClip ObjectUID="layer"><Clips><Clip Index="0" ObjectRef="22"/></Clips><Name>Grade</Name><IsAdjustmentLayer>true</IsAdjustmentLayer></MasterClip></PremiereData>"#);
        let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        let item = &tree.sequences[0].tracks[0].items[0];
        assert!(item.synthetic.is_none() && item.media_path.is_none());
        let clip = &fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0];
        let layer = fragment.project.item(clip.item).unwrap();
        assert_eq!(layer.name, "Grade");
        let ItemKind::AdjustmentLayer { width, height, duration, .. } = layer.kind else { panic!("{:?} {report:?}", layer.kind) };
        assert_eq!((width, height), (1080, 1920), "the sequence frame");
        assert!(duration >= clip.source_in + clip.duration, "covers the clip's source range");
        assert!(!report.mentions("unresolved") && !report.mentions("offline"), "{report:?}");
        // The same master clip without the flag is plain black video.
        let plain = xml.replace("<IsAdjustmentLayer>true", "<IsAdjustmentLayer>false");
        let (fragment, _, _) = import(plain.as_bytes(), &ImportOptions::default(), None).unwrap();
        let clip = &fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0];
        assert!(fragment.project.item(clip.item).and_then(|i| i.as_media()).is_some_and(|m| matches!(m.media, filmcraft_project::MediaRef::Generator(_))));
    }
    // Hand-written project panel: Project → RootProjectItem → ClipProjectItem → MasterClip, whose
    // clip's media source carries the subclip boundaries.
    fn subclip_fixture(boundaries: &str) -> String {
        fixture().replace(
            "</PremiereData>",
            &format!(
                r#"<Project ObjectRef="project"/><Project ObjectID="project"><RootProjectItem ObjectURef="root"/></Project>
<RootProjectItem ObjectUID="root"><ProjectItem><Name>Root Bin</Name></ProjectItem><ProjectItemContainer><Items><Item Index="0" ObjectURef="answer"/><Item Index="1" ObjectURef="root"/></Items></ProjectItemContainer></RootProjectItem>
<ClipProjectItem ObjectUID="answer"><ProjectItem><Name>interview.mov</Name></ProjectItem><MasterClip ObjectURef="answer-master"/></ClipProjectItem>
<MasterClip ObjectUID="answer-master"><Clips><Clip Index="0" ObjectRef="answer-clip"/></Clips><Name>Answer</Name></MasterClip>
<VideoClip ObjectID="answer-clip"><Clip><Source ObjectRef="answer-source"/></Clip></VideoClip>
<VideoMediaSource ObjectID="answer-source"><MediaSource><Content>{boundaries}</Content><Media ObjectURef="interview"/></MediaSource></VideoMediaSource>
<Media ObjectUID="interview"><Title>interview.mov</Title><ActualMediaFilePath>/media/interview.mov</ActualMediaFilePath></Media></PremiereData>"#
            ),
        )
    }
    #[test]
    fn subclips_import_as_project_items() {
        let bounds = |start: i64, end: i64, hard: bool| {
            format!("<StartBoundary>{start}</StartBoundary><EndBoundary>{end}</EndBoundary><BoundariesAreHard>{hard}</BoundariesAreHard>")
        };
        let (fragment, tree, report) = import(subclip_fixture(&bounds(TPS, 3 * TPS, true)).as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(tree.items.len(), 1, "{report:?}");
        let subclips: Vec<_> = fragment.project.items.values().filter(|i| matches!(i.kind, ItemKind::Subclip { .. })).collect();
        assert_eq!(subclips.len(), 1, "{report:?}");
        assert_eq!(subclips[0].name, "Answer", "a renamed subclip keeps its master clip name");
        let ItemKind::Subclip { parent, range, restrict_trims } = &subclips[0].kind else { unreachable!() };
        assert_eq!((range.start, range.duration, *restrict_trims), (Tick(TPS), Tick(2 * TPS), true));
        let parent = fragment.project.item(*parent).and_then(|i| i.as_media()).unwrap();
        assert_eq!(parent.media, filmcraft_project::MediaRef::File { path: "/media/interview.mov".into() });
        assert!(parent.offline, "the engine probes it like timeline media");
        // A master clip without boundaries is not a subclip; hostile boundaries are skipped and reported.
        let (fragment, tree, _) = import(subclip_fixture("").as_bytes(), &ImportOptions::default(), None).unwrap();
        assert!(tree.items.iter().all(|e| e.subclip.is_none()) && !fragment.project.items.values().any(|i| matches!(i.kind, ItemKind::Subclip { .. })));
        for bad in [bounds(3 * TPS, TPS, false), bounds(-TPS, TPS, false), "<StartBoundary>x</StartBoundary><EndBoundary>1</EndBoundary>".into()] {
            let (fragment, _, report) = import(subclip_fixture(&bad).as_bytes(), &ImportOptions::default(), None).unwrap();
            assert!(!fragment.project.items.values().any(|i| matches!(i.kind, ItemKind::Subclip { .. })), "{bad}");
            assert!(report.mentions("Answer") && report.mentions("subclip"), "{bad}: {report:?}");
        }
    }
    // Hand-written project panel: nested bins, a timeline clip's media filed in a sub-bin, the
    // sequence at the root, bin-only black video, and a search bin.
    fn bins_fixture() -> String {
        let black = u32::from_be_bytes(*b"BLAK");
        fixture().replace(
            "</PremiereData>",
            &format!(
                r#"<Project ObjectRef="project"/><Project ObjectID="project"><RootProjectItem ObjectURef="root"/></Project>
<RootProjectItem ObjectUID="root"><ProjectItem><Name>Root Bin</Name></ProjectItem><ProjectItemContainer><Items><Item Index="0" ObjectURef="footage"/><Item Index="1" ObjectURef="sequence-item"/><Item Index="2" ObjectURef="search"/><Item Index="3" ObjectURef="unused"/></Items></ProjectItemContainer></RootProjectItem>
<BinProjectItem ObjectUID="footage"><ProjectItem><Name>Footage</Name></ProjectItem><ProjectItemContainer><Items><Item Index="0" ObjectURef="selects"/></Items></ProjectItemContainer></BinProjectItem>
<BinProjectItem ObjectUID="selects"><ProjectItem><Name>Selects</Name></ProjectItem><ProjectItemContainer><Items><Item Index="0" ObjectURef="camera-item"/><Item Index="1" ObjectURef="footage"/></Items></ProjectItemContainer></BinProjectItem>
<BinProjectItem ObjectUID="unused"><ProjectItem><Name>Unused</Name></ProjectItem><ProjectItemContainer><Items><Item Index="0" ObjectURef="black-item"/></Items></ProjectItemContainer></BinProjectItem>
<SmartBinProjectItem ObjectUID="search"><BinProjectItem><ProjectItem><Name>Reds</Name></ProjectItem></BinProjectItem></SmartBinProjectItem>
<ClipProjectItem ObjectUID="camera-item"><ProjectItem><Name>camera.mov</Name></ProjectItem><MasterClip ObjectURef="camera-master"/></ClipProjectItem>
<MasterClip ObjectUID="camera-master"><Clips><Clip Index="0" ObjectRef="camera-clip"/></Clips><Name>Camera A</Name></MasterClip>
<VideoClip ObjectID="camera-clip"><Clip><Source ObjectRef="camera-source"/></Clip></VideoClip>
<VideoMediaSource ObjectID="camera-source"><MediaSource><Media ObjectURef="camera-media"/></MediaSource></VideoMediaSource>
<Media ObjectUID="camera-media"><Title>camera.mov</Title><ActualMediaFilePath>/missing/camera.mov</ActualMediaFilePath></Media>
<ClipProjectItem ObjectUID="sequence-item"><ProjectItem><Name>Sequence 01</Name></ProjectItem><MasterClip ObjectURef="sequence-master"/></ClipProjectItem>
<MasterClip ObjectUID="sequence-master"><Clips><Clip Index="0" ObjectRef="sequence-clip"/></Clips><Name>Sequence 01</Name></MasterClip>
<VideoClip ObjectID="sequence-clip"><Clip><Source ObjectRef="sequence-source"/></Clip></VideoClip>
<VideoSequenceSource ObjectID="sequence-source"><SequenceSource><Sequence ObjectURef="sequence"/></SequenceSource></VideoSequenceSource>
<ClipProjectItem ObjectUID="black-item"><ProjectItem><Name>Vídeo negro</Name></ProjectItem><MasterClip ObjectURef="black-master"/></ClipProjectItem>
<MasterClip ObjectUID="black-master"><Clips><Clip Index="0" ObjectRef="black-clip"/></Clips><Name>Vídeo negro</Name></MasterClip>
<VideoClip ObjectID="black-clip"><Clip><Source ObjectRef="black-source"/></Clip></VideoClip>
<VideoMediaSource ObjectID="black-source"><MediaSource><Media ObjectURef="black-media"/></MediaSource></VideoMediaSource>
<Media ObjectUID="black-media"><Title>Vídeo negro</Title><FilePath>{black}</FilePath></Media></PremiereData>"#
            ),
        )
    }
    #[test]
    fn bins_and_their_items_are_imported() {
        let (fragment, tree, report) = import(bins_fixture().as_bytes(), &ImportOptions::default(), None).unwrap();
        let names: Vec<_> = tree.bins.iter().map(|b| (b.name.as_str(), b.parent)).collect();
        assert_eq!(names, [("Footage", None), ("Selects", Some(0)), ("Unused", None)], "a bin listing its parent is read once");
        let p = &fragment.project;
        let bin_of = |id: filmcraft_project::ItemId| p.root.parent_of(id).and_then(|b| p.root.find_bin(b)).map(|b| b.name.clone());
        let clip = &p.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0];
        assert_eq!(bin_of(clip.item).as_deref(), Some("Selects"), "timeline media sits in its bin");
        assert_eq!((p.item(clip.item).unwrap().name.as_str(), clip.name.as_str()), ("Camera A", "camera.mov"), "renamed in the panel only");
        assert_eq!(bin_of(fragment.sequences[0]), Some(p.root.name.clone()), "the sequence stays at the root");
        let black = p.items.values().find(|i| i.name == "Vídeo negro").unwrap();
        assert!(matches!(&black.kind, ItemKind::Media(m) if matches!(m.media, filmcraft_project::MediaRef::Generator(filmcraft_media::Generator::BlackVideo))));
        assert_eq!(bin_of(black.id).as_deref(), Some("Unused"), "bin-only media is imported into its bin");
        assert!(report.mentions("Reds") && report.mentions("search bin"), "{report:?}");
        // Media and bins without any sequence still import.
        let no_sequence = bins_fixture().replace(r#"<Sequence ObjectUID="sequence">"#, r#"<Unused ObjectUID="sequence">"#).replace("</Sequence>", "</Unused>");
        let (fragment, tree, _) = import(no_sequence.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert!(fragment.sequences.is_empty() && tree.sequences.is_empty());
        assert!(fragment.project.items.values().any(|i| i.name == "Camera A" && i.as_media().is_some()));
        // A project with neither sequences nor project items is still empty.
        let empty = r#"<PremiereData><Project ObjectID="p"><RootProjectItem ObjectURef="r"/></Project><RootProjectItem ObjectUID="r"/></PremiereData>"#;
        assert!(matches!(import(empty.as_bytes(), &ImportOptions::default(), None), Err(Error::Empty)));
    }
    // Hand-written: one file cut on a video and an audio track, its master clip filed in a bin.
    // `groups` orders the sequence's track groups, so either kind of clip can come first.
    fn av_fixture(groups: &str) -> String {
        format!(
            r#"<PremiereData>
<Sequence ObjectUID="s"><Name>Sequence 01</Name><TrackGroups>{groups}</TrackGroups></Sequence>
<VideoTrackGroup ObjectID="vg"><TrackGroup><FrameRate>10584000000</FrameRate><Tracks><Track ObjectURef="vt"/></Tracks></TrackGroup></VideoTrackGroup>
<AudioTrackGroup ObjectID="ag"><TrackGroup><Tracks><Track ObjectURef="at"/></Tracks></TrackGroup></AudioTrackGroup>
<VideoClipTrack ObjectUID="vt"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="v"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
<AudioClipTrack ObjectUID="at"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem ObjectRef="a"/></TrackItems></ClipItems></ClipTrack></AudioClipTrack>
<VideoClipTrackItem ObjectID="v"><ClipTrackItem><TrackItem><Start>0</Start><End>{TPS}</End></TrackItem><SubClip ObjectRef="sv"/></ClipTrackItem></VideoClipTrackItem>
<AudioClipTrackItem ObjectID="a"><ClipTrackItem><TrackItem><Start>0</Start><End>{TPS}</End></TrackItem><SubClip ObjectRef="sa"/></ClipTrackItem></AudioClipTrackItem>
<SubClip ObjectID="sv"><Name>interview.mov</Name><MasterClip ObjectURef="master"/><Clip ObjectRef="cv"/></SubClip>
<SubClip ObjectID="sa"><Name>interview.mov</Name><MasterClip ObjectURef="master"/><Clip ObjectRef="ca"/></SubClip>
<VideoClip ObjectID="cv"><Clip><Source ObjectRef="vs"/><InPoint>0</InPoint><OutPoint>{TPS}</OutPoint></Clip></VideoClip>
<AudioClip ObjectID="ca"><Clip><Source ObjectRef="as"/><InPoint>0</InPoint><OutPoint>{TPS}</OutPoint></Clip></AudioClip>
<VideoMediaSource ObjectID="vs"><MediaSource><Media ObjectURef="media"/></MediaSource></VideoMediaSource>
<AudioMediaSource ObjectID="as"><MediaSource><Media ObjectURef="media"/></MediaSource></AudioMediaSource>
<Media ObjectUID="media"><Title>interview.mov</Title><ActualMediaFilePath>/media/interview.mov</ActualMediaFilePath></Media>
<Project ObjectRef="project"/><Project ObjectID="project"><RootProjectItem ObjectURef="root"/></Project>
<RootProjectItem ObjectUID="root"><ProjectItemContainer><Items><Item Index="0" ObjectURef="footage"/></Items></ProjectItemContainer></RootProjectItem>
<BinProjectItem ObjectUID="footage"><ProjectItem><Name>Footage</Name></ProjectItem><ProjectItemContainer><Items><Item Index="0" ObjectURef="clip-item"/></Items></ProjectItemContainer></BinProjectItem>
<ClipProjectItem ObjectUID="clip-item"><ProjectItem><Name>interview.mov</Name></ProjectItem><MasterClip ObjectURef="master"/></ClipProjectItem>
<MasterClip ObjectUID="master"><Clips><Clip Index="0" ObjectRef="master-video"/><Clip Index="1" ObjectRef="master-audio"/></Clips><Name>Interview</Name></MasterClip>
<VideoClip ObjectID="master-video"><Clip><Source ObjectRef="vs"/></Clip></VideoClip>
<AudioClip ObjectID="master-audio"><Clip><Source ObjectRef="as"/></Clip></AudioClip>
</PremiereData>"#
        )
    }
    #[test]
    fn one_file_cut_on_video_and_audio_tracks_is_one_project_item_in_its_bin() {
        let video = r#"<TrackGroup><Second ObjectRef="vg"/></TrackGroup>"#;
        let audio = r#"<TrackGroup><Second ObjectRef="ag"/></TrackGroup>"#;
        for groups in [format!("{video}{audio}"), format!("{audio}{video}")] {
            let (fragment, _, report) = import(av_fixture(&groups).as_bytes(), &ImportOptions::default(), None).unwrap();
            let p = &fragment.project;
            let file = filmcraft_project::MediaRef::File { path: "/media/interview.mov".into() };
            let items: Vec<_> = p.items.values().filter(|i| i.as_media().is_some_and(|m| m.media == file)).collect();
            assert_eq!(items.len(), 1, "{groups}: {report:?}");
            let seq = p.sequence(fragment.sequences[0]).unwrap();
            assert_eq!((seq.video_tracks[0].items[0].item, seq.audio_tracks[0].items[0].item), (items[0].id, items[0].id));
            let bin = p.root.parent_of(items[0].id).and_then(|b| p.root.find_bin(b)).map(|b| b.name.clone());
            assert_eq!((bin.as_deref(), items[0].name.as_str()), (Some("Footage"), "Interview"), "{groups}");
            // Offline until probed, it describes both streams its clips use.
            let media = items[0].as_media().unwrap();
            assert!(media.offline && media.info.video.is_some() && !media.info.audio_streams.is_empty(), "{groups}: {:?}", media.info);
            assert_eq!(media.info.kind, filmcraft_media::MediaKind::Movie, "{groups}");
        }
    }
    // A hand-written comment marker in the DVAMarker JSON shape.
    fn marker_record(id: &str, name: &str, ticks: i64) -> String {
        let json = format!(r#"{{"DVAMarker":{{"mName":"{name}","mStartTime":{{"ticks":{ticks}}},"mType":"Comment"}}}}"#);
        format!(r#"<Marker ObjectID="{id}"><DVAMarker>{}</DVAMarker></Marker>"#, json.replace('"', "&quot;"))
    }
    // `av_fixture` with two markers on its master clip, held by the master clip's first clip.
    fn marked_av_fixture(groups: &str) -> String {
        av_fixture(groups)
            .replace(
                r#"<VideoClip ObjectID="master-video"><Clip><Source ObjectRef="vs"/></Clip></VideoClip>"#,
                r#"<VideoClip ObjectID="master-video"><Clip><MarkerOwner><Markers ObjectRef="master-markers"/></MarkerOwner><Source ObjectRef="vs"/></Clip></VideoClip>"#,
            )
            .replace(
                "</PremiereData>",
                &format!(
                    r#"<Markers ObjectID="master-markers"><Markers><Marker Index="0"><Second ObjectRef="mark-a"/></Marker><Marker Index="1"><Second ObjectRef="mark-b"/></Marker></Markers></Markers>
{}{}</PremiereData>"#,
                    marker_record("mark-a", "first", 0),
                    marker_record("mark-b", "second", TPS / 2)
                ),
            )
    }
    #[test]
    fn a_marked_master_cut_on_video_and_audio_tracks_gives_its_one_media_item_the_markers_once() {
        let video = r#"<TrackGroup><Second ObjectRef="vg"/></TrackGroup>"#;
        let audio = r#"<TrackGroup><Second ObjectRef="ag"/></TrackGroup>"#;
        for groups in [format!("{video}{audio}"), format!("{audio}{video}")] {
            let (fragment, tree, report) = import(marked_av_fixture(&groups).as_bytes(), &ImportOptions::default(), None).unwrap();
            let p = &fragment.project;
            let media: Vec<_> = p.items.values().filter_map(|i| i.as_media()).collect();
            assert_eq!(media.len(), 1, "{groups}: {report:?}");
            let names: Vec<_> = media[0].markers.iter().map(|m| m.name.as_str()).collect();
            assert_eq!(names, ["first", "second"], "{groups}: {report:?}");
            assert_ne!(media[0].markers[0].id, media[0].markers[1].id, "{groups}");
            // The source tree records them on one of the two cuts.
            let carried: Vec<_> = tree.sequences[0].tracks.iter().flat_map(|t| &t.items).map(|i| i.media_markers.len()).collect();
            assert_eq!(carried.iter().sum::<usize>(), 2, "{groups}: {carried:?}");
        }
    }
    #[test]
    fn a_master_clip_no_kept_cut_uses_brings_its_markers_to_its_panel_media() {
        let names = |fragment: &Imported, path: &str| {
            let file = filmcraft_project::MediaRef::File { path: path.into() };
            let media = fragment.project.items.values().filter_map(|i| i.as_media()).find(|m| m.media == file).unwrap();
            media.markers.iter().map(|m| (m.start, m.name.clone())).collect::<Vec<_>>()
        };
        // Hand-written: the panel's interview master clip, which no timeline clip uses, holds a marker.
        let xml = subclip_fixture("")
            .replace(
                r#"<VideoClip ObjectID="answer-clip"><Clip><Source ObjectRef="answer-source"/></Clip></VideoClip>"#,
                r#"<VideoClip ObjectID="answer-clip"><Clip><MarkerOwner><Markers ObjectRef="answer-markers"/></MarkerOwner><Source ObjectRef="answer-source"/></Clip></VideoClip>"#,
            )
            .replace(
                "</PremiereData>",
                &format!(
                    r#"<Markers ObjectID="answer-markers"><Markers><Marker Index="0"><Second ObjectRef="question"/></Marker></Markers></Markers>{}</PremiereData>"#,
                    marker_record("question", "question", TPS)
                ),
            );
        let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(names(&fragment, "/media/interview.mov"), [(Tick(TPS), "question".to_string())], "{report:?}");
        assert_eq!(tree.items.iter().map(|e| e.markers.len()).collect::<Vec<_>>(), [1]);
        // The same when the master clip's only cuts are skipped.
        let groups = r#"<TrackGroup><Second ObjectRef="vg"/></TrackGroup><TrackGroup><Second ObjectRef="ag"/></TrackGroup>"#;
        let skipped = marked_av_fixture(groups).replace(&format!("<End>{TPS}</End>"), "<End>0</End>");
        let (fragment, tree, report) = import(skipped.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert!(tree.sequences[0].tracks.iter().all(|t| t.items.is_empty()), "{report:?}");
        assert_eq!(names(&fragment, "/media/interview.mov"), [(Tick::ZERO, "first".to_string()), (Tick(TPS / 2), "second".to_string())], "{report:?}");
        // A kept cut brings them instead; its media, which the panel also lists, gets them once.
        let (fragment, tree, report) = import(marked_av_fixture(groups).as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(names(&fragment, "/media/interview.mov").len(), 2, "{report:?}");
        assert!(tree.items.iter().all(|e| e.markers.is_empty()), "the source tree records them on the cut only");
    }
    #[test]
    fn filed_items_keep_panel_order_and_their_first_bin() {
        // Hand-written: a second file after the interview in Footage, and a second master clip of
        // the interview's file in a later bin; the file is one item, which stays in Footage.
        let groups = r#"<TrackGroup><Second ObjectRef="vg"/></TrackGroup>"#;
        let xml = av_fixture(groups)
            .replace(r#"<Item Index="0" ObjectURef="footage"/>"#, r#"<Item Index="0" ObjectURef="footage"/><Item Index="1" ObjectURef="selects"/>"#)
            .replace(r#"<Item Index="0" ObjectURef="clip-item"/>"#, r#"<Item Index="0" ObjectURef="clip-item"/><Item Index="1" ObjectURef="broll-item"/>"#)
            .replace(
                "</PremiereData>",
                r#"<BinProjectItem ObjectUID="selects"><ProjectItem><Name>Selects</Name></ProjectItem><ProjectItemContainer><Items><Item Index="0" ObjectURef="copy-item"/></Items></ProjectItemContainer></BinProjectItem>
<ClipProjectItem ObjectUID="copy-item"><ProjectItem><Name>interview.mov</Name></ProjectItem><MasterClip ObjectURef="copy-master"/></ClipProjectItem>
<MasterClip ObjectUID="copy-master"><Clips><Clip Index="0" ObjectRef="copy-video"/></Clips><Name>Interview</Name></MasterClip>
<VideoClip ObjectID="copy-video"><Clip><Source ObjectRef="vs"/></Clip></VideoClip>
<ClipProjectItem ObjectUID="broll-item"><ProjectItem><Name>broll.mov</Name></ProjectItem><MasterClip ObjectURef="broll-master"/></ClipProjectItem>
<MasterClip ObjectUID="broll-master"><Clips><Clip Index="0" ObjectRef="broll-video"/></Clips><Name>B-roll</Name></MasterClip>
<VideoClip ObjectID="broll-video"><Clip><Source ObjectRef="broll-source"/></Clip></VideoClip>
<VideoMediaSource ObjectID="broll-source"><MediaSource><Media ObjectURef="broll"/></MediaSource></VideoMediaSource>
<Media ObjectUID="broll"><Title>broll.mov</Title><ActualMediaFilePath>/media/broll.mov</ActualMediaFilePath></Media></PremiereData>"#,
            );
        let (fragment, _, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        let p = &fragment.project;
        let names_in = |bin: &str| -> Vec<String> {
            let found = p.root.children.iter().find_map(|c| match c {
                filmcraft_project::BinEntry::Bin(b) if b.name == bin => Some(b),
                _ => None,
            });
            let items = found.into_iter().flat_map(|b| &b.children).filter_map(|c| match c {
                filmcraft_project::BinEntry::Item(id) => p.item(*id).map(|i| i.name.clone()),
                filmcraft_project::BinEntry::Bin(_) => None,
            });
            items.collect()
        };
        assert_eq!(names_in("Footage"), ["Interview", "B-roll"], "{report:?}");
        assert!(names_in("Selects").is_empty());
    }
    /// Filing searched the bin tree from the root once per populated bin, so n bins cost O(n²):
    /// 100,000 took about 22 s in the test profile. One traversal takes milliseconds, so the limit
    /// separates them on any machine.
    #[test]
    fn filing_items_into_many_bins_is_not_quadratic() {
        use filmcraft_project::BinEntry;
        let mut p = filmcraft_project::Project::new("Bins");
        let mut filed = std::collections::HashMap::new();
        let mut order = Vec::new();
        for _ in 0..100_000 {
            let bin = p.add_bin("Bin", None);
            let layer = ItemKind::AdjustmentLayer { width: 16, height: 16, rate: FrameRate::FPS_24, duration: Tick(TPS) };
            let item = p.add_item("Layer", Label::Lavender, layer, None);
            filed.insert(item, bin);
            order.push(item);
        }
        let start = std::time::Instant::now();
        file_items(&mut p, &filed, &order);
        assert!(start.elapsed() < std::time::Duration::from_secs(3), "{:?}", start.elapsed());
        let in_their_bins = p.root.children.iter().all(|c| match c {
            BinEntry::Bin(bin) => matches!(bin.children.as_slice(), [BinEntry::Item(id)] if filed.get(id) == Some(&bin.id)),
            BinEntry::Item(_) => false,
        });
        assert!(in_their_bins && p.root.children.len() == 100_000);
    }
    #[test]
    fn filing_reaches_nested_bins_in_order_and_keeps_items_of_a_missing_bin_at_the_root() {
        use filmcraft_project::BinEntry;
        let mut p = filmcraft_project::Project::new("Bins");
        let outer = p.add_bin("Outer", None);
        let inner = p.add_bin("Inner", Some(outer));
        let layer = || ItemKind::AdjustmentLayer { width: 16, height: 16, rate: FrameRate::FPS_24, duration: Tick(TPS) };
        let [a, b, c, d] = ["a", "b", "c", "d"].map(|name| p.add_item(name, Label::Lavender, layer(), None));
        let filed = std::collections::HashMap::from([(a, inner), (b, outer), (c, filmcraft_project::BinId(u64::MAX)), (d, inner)]);
        file_items(&mut p, &filed, &[d, c, b, a]);
        let names = |bin: filmcraft_project::BinId| -> Vec<String> {
            let children = p.root.find_bin(bin).map(|b| b.children.as_slice()).unwrap_or_default();
            children
                .iter()
                .map(|c| match c {
                    BinEntry::Item(id) => p.item(*id).map(|i| i.name.clone()).unwrap_or_default(),
                    BinEntry::Bin(sub) => sub.name.clone(),
                })
                .collect()
        };
        assert_eq!(names(inner), ["d", "a"], "panel order");
        assert_eq!(names(outer), ["Inner", "b"]);
        assert_eq!(names(p.root.id), ["Outer", "c"], "an item of a missing bin stays at the root");
    }
    #[test]
    fn an_item_first_listed_at_the_root_stays_at_the_root() {
        // Hand-written: the interview's master clip listed at the root, then a second master clip of
        // the same file in Footage. The file is one item, which stays where it is listed first.
        let groups = r#"<TrackGroup><Second ObjectRef="vg"/></TrackGroup>"#;
        let xml = av_fixture(groups)
            .replace(r#"<Item Index="0" ObjectURef="footage"/>"#, r#"<Item Index="0" ObjectURef="clip-item"/><Item Index="1" ObjectURef="footage"/>"#)
            .replace(
                r#"<Item Index="0" ObjectURef="clip-item"/></Items></ProjectItemContainer></BinProjectItem>"#,
                r#"<Item Index="0" ObjectURef="copy-item"/></Items></ProjectItemContainer></BinProjectItem>"#,
            )
            .replace(
                "</PremiereData>",
                r#"<ClipProjectItem ObjectUID="copy-item"><ProjectItem><Name>interview.mov</Name></ProjectItem><MasterClip ObjectURef="copy-master"/></ClipProjectItem>
<MasterClip ObjectUID="copy-master"><Clips><Clip Index="0" ObjectRef="copy-video"/></Clips><Name>Interview</Name></MasterClip>
<VideoClip ObjectID="copy-video"><Clip><Source ObjectRef="vs"/></Clip></VideoClip></PremiereData>"#,
            );
        let (fragment, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(tree.items.iter().map(|e| e.bin).collect::<Vec<_>>(), [None, Some(0)], "{report:?}");
        let p = &fragment.project;
        let file = filmcraft_project::MediaRef::File { path: "/media/interview.mov".into() };
        let items: Vec<_> = p.items.values().filter(|i| i.as_media().is_some_and(|m| m.media == file)).collect();
        assert_eq!(items.len(), 1, "{report:?}");
        let bin = p.root.parent_of(items[0].id).and_then(|b| p.root.find_bin(b)).map(|b| b.name.clone());
        assert_eq!(bin, Some(p.root.name.clone()), "{report:?}");
    }
    #[test]
    fn a_subclip_lengthens_media_a_shorter_timeline_cut_created_and_never_shortens_it() {
        // Hand-written: the timeline cuts the first second of the interview, whose panel bin also
        // holds a subclip of it.
        let subclip = |start: i64, end: i64| {
            av_fixture(r#"<TrackGroup><Second ObjectRef="vg"/></TrackGroup>"#)
                .replace(r#"<Item Index="0" ObjectURef="clip-item"/>"#, r#"<Item Index="0" ObjectURef="clip-item"/><Item Index="1" ObjectURef="answer-item"/>"#)
                .replace(
                    "</PremiereData>",
                    &format!(
                        r#"<ClipProjectItem ObjectUID="answer-item"><ProjectItem><Name>interview.mov</Name></ProjectItem><MasterClip ObjectURef="answer-master"/></ClipProjectItem>
<MasterClip ObjectUID="answer-master"><Clips><Clip Index="0" ObjectRef="answer-clip"/></Clips><Name>Answer</Name></MasterClip>
<VideoClip ObjectID="answer-clip"><Clip><Source ObjectRef="answer-source"/></Clip></VideoClip>
<VideoMediaSource ObjectID="answer-source"><MediaSource><Content><StartBoundary>{start}</StartBoundary><EndBoundary>{end}</EndBoundary></Content><Media ObjectURef="media"/></MediaSource></VideoMediaSource></PremiereData>"#
                    ),
                )
        };
        let parent = |start: i64, end: i64| {
            let (fragment, _, report) = import(subclip(start, end).as_bytes(), &ImportOptions::default(), None).unwrap();
            let p = &fragment.project;
            let parent = p.items.values().find_map(|i| match &i.kind {
                ItemKind::Subclip { parent, range, .. } => Some((*parent, range.start, range.duration)),
                _ => None,
            });
            let (parent, range_start, range_duration) = parent.unwrap_or_else(|| panic!("{report:?}"));
            assert_eq!((range_start, range_duration), (Tick(start), Tick(end - start)));
            assert_eq!(p.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0].item, parent, "the cut's media is the subclip's parent");
            p.item(parent).and_then(|i| i.as_media()).unwrap().info.duration
        };
        assert_eq!(parent(10 * TPS, 20 * TPS), Tick(20 * TPS), "it covers the subclip");
        assert_eq!(parent(0, TPS / 2), Tick(TPS), "it still covers the cut");
    }
    #[test]
    fn speed_reads_stored_ratio_not_frame_grid_durations() {
        let xml = fixture().replace("<PlaybackSpeed>2</PlaybackSpeed>", "<PlaybackSpeed>1</PlaybackSpeed>");
        let (fragment, tree, _) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert_eq!(tree.sequences[0].tracks[0].items[0].speed, 1.0);
        assert_eq!(fragment.project.sequence(fragment.sequences[0]).unwrap().video_tracks[0].items[0].speed, 1.0);
        assert_eq!(tree.sequences[0].tracks[0].items[0].start, Tick(0));
        assert_eq!(tree.sequences[0].tracks[0].items[0].end, Tick(3 * TPS));
        assert_eq!(tree.sequences[0].tracks[0].items[0].source_in, Tick(4 * TPS));
        assert_eq!(tree.sequences[0].tracks[0].items[0].source_out, Tick(10 * TPS));
    }
}
