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

#[path = "premiere_color.rs"]
mod color;

#[path = "premiere_motion.rs"]
mod motion;

#[path = "premiere_audio.rs"]
mod audio;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub sequences: Vec<Sequence>,
    pub media: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sequence {
    pub object_uid: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub frame_rate: Option<FrameRate>,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    pub work_out: Tick,
    pub tracks: Vec<Track>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub kind: String,
    pub index: usize,
    pub muted: bool,
    pub items: Vec<Item>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub object_id: String,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub audio: BTreeMap<String, Param>,
    #[serde(default)]
    pub video_effects: Vec<filmcraft_project::EffectInstance>,
    #[serde(default)]
    pub graphic: bool,
    pub label: String,
    pub start: Tick,
    pub end: Tick,
    pub source_in: Tick,
    pub source_out: Tick,
    pub speed: f64,
    pub media_path: Option<String>,
    pub nested_sequence: Option<String>,
    pub text: Option<String>,
    pub audio_category: Option<String>,
    pub effects: Vec<String>,
    pub motion: BTreeMap<String, String>,
    #[serde(default)]
    pub motion_keys: BTreeMap<String, Param>,
    pub transitions: Vec<SourceTransition>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceTransition {
    #[serde(default)]
    pub object_id: String,
    #[serde(default)]
    pub start: Option<Tick>,
    #[serde(default)]
    pub end: Option<Tick>,
    pub head: bool,
    pub name: String,
    pub duration: Tick,
}

/// Cap diagnostic growth independently of graph size; the original snapshot retains all details.
pub fn warning(report: &mut Report, message: impl Into<String>) {
    limited_report(report, crate::Level::Warning, message.into());
}
fn limited_report(report: &mut Report, level: crate::Level, message: String) {
    if report.entries.len() < 1024 {
        report.push(level, message);
    } else if report.entries.len() == 1024 {
        report.warn("further import diagnostics omitted; inspect the original snapshot");
    }
}
fn spend(work_left: &mut usize, node: Node<'_, '_>) -> Result<()> {
    *work_left = work_left
        .checked_sub(node.descendants().len().saturating_add(node.range().len() / 64))
        .ok_or_else(|| error("object graph traversal exceeds import work limit"))?;
    Ok(())
}

fn error(message: impl Into<String>) -> Error {
    Error::Parse { format: "Premiere", message: message.into() }
}
fn descendant<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.descendants().find(|c| c.is_element() && c.tag_name().name() == name)
}
fn value<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    descendant(n, name).and_then(|n| n.text()).map(str::trim)
}
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
fn ticks(s: Option<&str>, fallback: Tick) -> Result<Tick> {
    let tick = signed_ticks(s, fallback)?;
    if tick < Tick::ZERO {
        return Err(error("time outside supported nonnegative tick bounds"));
    }
    Ok(tick)
}

fn dimension(s: Option<&str>, default: u32) -> Result<u32> {
    match s {
        None | Some("0") => Ok(default),
        Some(s) => s.parse().map_err(|_| error("invalid frame dimension")),
    }
}
fn referred<'a, 'i>(n: Option<Node<'a, 'i>>, map: &BTreeMap<&str, Node<'a, 'i>>, attr: &str) -> Option<Node<'a, 'i>> {
    map.get(n?.attribute(attr)?).copied()
}

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

fn media_path(n: Node<'_, '_>, base_dir: Option<&str>) -> Option<String> {
    child_text(n, "ActualMediaFilePath").or_else(|| child_text(n, "FilePath")).filter(|p| !p.is_empty()).map(|p| crate::resolve_path(p, base_dir))
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
            {
                return Err(error("media title or path is too long"));
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
    let mut links = BTreeMap::new();
    for group in root.children().filter(|n| n.has_tag_name("Link")) {
        spend(&mut work_left, group)?;
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
    let mut sequences = Vec::new();
    let mut item_count = 0usize;
    let mut skipped_items = BTreeSet::new();
    let mut unsupported_effects = BTreeMap::new();
    for n in root.children().filter(|n| n.has_tag_name("Sequence")) {
        spend(&mut work_left, n)?;
        let name = child_text(n, "Name").unwrap_or("Sequence").to_string();
        let width = dimension(value(n, "MZ.Sequence.PreviewFrameSizeWidth"), 1920)?;
        let height = dimension(value(n, "MZ.Sequence.PreviewFrameSizeHeight"), 1080)?;
        filmcraft_project::validate_frame_size(width, height).map_err(error)?;
        let mut frame_rate = None;
        let mut sample_rate = None;
        let mut tracks = Vec::new();
        let pointers = child(n, "TrackGroups")
            .into_iter()
            .flat_map(|groups| groups.children())
            .filter(|group| group.has_tag_name("TrackGroup"))
            .filter_map(|group| child(group, "Second"));
        for pointer in pointers {
            let Some(group) = referred(Some(pointer), &ids, "ObjectRef") else {
                warning(report, format!("{name}: unresolved track group"));
                continue;
            };
            spend(&mut work_left, group)?;
            let kind = if group.tag_name().name().starts_with("Video") {
                "video"
            } else if group.tag_name().name().starts_with("Audio") {
                "audio"
            } else {
                "caption"
            };
            if group.has_tag_name("VideoTrackGroup")
                && let Some(rate) = child(group, "TrackGroup").and_then(|group| child(group, "FrameRate"))
            {
                let rate = sequence_rate(rate.text().unwrap_or(""))?;
                if frame_rate.is_some_and(|previous| previous != rate) {
                    return Err(error(format!("{name}: conflicting video track group frame rates")));
                }
                frame_rate = Some(rate);
            }
            if group.has_tag_name("AudioTrackGroup")
                && let Some(rate) = child(group, "TrackGroup").and_then(|g| child(g, "FrameRate"))
            {
                let raw = rate.text().unwrap_or("").trim().parse::<i64>().map_err(|_| error("invalid audio ticks per sample"))?;
                if raw <= 0 || filmcraft_time::TICKS_PER_SECOND % raw != 0 {
                    return Err(error("invalid audio ticks per sample"));
                }
                let parsed = u32::try_from(filmcraft_time::TICKS_PER_SECOND / raw).map_err(|_| error("invalid audio sample rate"))?;
                if !(8000..=384000).contains(&parsed) || sample_rate.is_some_and(|old| old != parsed) {
                    return Err(error("unsupported or conflicting audio sample rate"));
                }
                sample_rate = Some(parsed);
            }
            for (fallback, pointer) in group.descendants().filter(|n| n.has_tag_name("Track") && n.attribute("ObjectURef").is_some()).enumerate() {
                let Some(track) = referred(Some(pointer), &uids, "ObjectURef") else {
                    warning(report, format!("{name}: unresolved {kind} track"));
                    continue;
                };
                spend(&mut work_left, track)?;
                let index = value(track, "Index").map(|s| s.parse::<usize>()).transpose().map_err(|_| error("invalid track index"))?.unwrap_or(fallback);
                if index >= MAX_TRACKS || tracks.len() >= MAX_TRACKS * 3 {
                    return Err(error("too many tracks"));
                }
                if tracks.iter().any(|t: &Track| t.kind == kind && t.index == index) {
                    return Err(error("duplicate track index"));
                }
                let muted = value(track, "IsMuted").is_some_and(|s| s.eq_ignore_ascii_case("true") || s == "1");
                let mut items = Vec::new();
                for pointer in track.descendants().filter(|n| n.has_tag_name("TrackItem") && n.attribute("ObjectRef").is_some()) {
                    let Some(item) = referred(Some(pointer), &ids, "ObjectRef") else {
                        warning(report, format!("{name}: unresolved timeline item"));
                        continue;
                    };
                    spend(&mut work_left, item)?;
                    item_count += 1;
                    if item_count > 100_000 {
                        return Err(error("too many timeline items"));
                    }
                    // TransitionItems are a separate list, not source clips (also separated
                    // by upstream PR #1694). Their Head/Tail references are handled below.
                    if item.tag_name().name().ends_with("TransitionTrackItem") {
                        continue;
                    }
                    let start = ticks(value(item, "Start"), Tick::ZERO)?;
                    let end = ticks(value(item, "End"), start)?;
                    let sub = referred(descendant(item, "SubClip"), &ids, "ObjectRef");
                    if let Some(sub) = sub {
                        spend(&mut work_left, sub)?;
                    }
                    let label = sub.and_then(|s| child_text(s, "Name")).unwrap_or(item.tag_name().name()).to_string();
                    let clip = referred(sub.and_then(|s| child(s, "Clip")), &ids, "ObjectRef");
                    if let Some(clip) = clip {
                        spend(&mut work_left, clip)?;
                    }
                    let source_in = ticks(clip.and_then(|s| value(s, "InPoint")), Tick::ZERO)?;
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
                        warning(report, format!("{label}: {reason}; clip skipped"));
                        skipped_items.insert(item.attribute("ObjectID").unwrap_or("").to_string());
                    }
                    let mut effects = Vec::new();
                    let mut motion = BTreeMap::new();
                    let mut motion_keys = BTreeMap::new();
                    let mut audio = BTreeMap::new();
                    let mut video_effects = Vec::new();
                    let mut graphic = false;
                    let mut text = None;
                    let chain = referred(descendant(item, "ComponentOwner").and_then(|n| child(n, "Components")), &ids, "ObjectRef");
                    if let Some(chain) = chain {
                        spend(&mut work_left, chain)?;
                        for ptr in chain.descendants().filter(|n| n.has_tag_name("Component")) {
                            let Some(c) = referred(Some(ptr), &ids, "ObjectRef") else { continue };
                            spend(&mut work_left, c)?;
                            let effect = value(c, "DisplayName").or_else(|| child_text(c, "FilterMatchName")).unwrap_or("Unknown").to_string();
                            let match_name = child_text(c, "MatchName").or_else(|| child_text(c, "FilterMatchName"));
                            let is_text = matches!(match_name, Some("AE.ADBE Text" | "ADBE Text"));
                            let is_shape = matches!(match_name, Some("AE.ADBE Shape" | "ADBE Shape"));
                            graphic |= is_text || is_shape;
                            if is_shape {
                                warning(
                                    report,
                                    format!(
                                        "{label}: native shape retained as a graphic canvas; opaque path/appearance data is retained in provenance, not rendered"
                                    ),
                                );
                            }
                            if is_text {
                                // User-owned XML stores Source Text in parameter 1. InstanceName
                                // is the renameable layer label, not the displayed text.
                                for ptr in c.descendants().filter(|n| n.has_tag_name("Param")) {
                                    let Some(p) = referred(Some(ptr), &ids, "ObjectRef") else { continue };
                                    spend(&mut work_left, p)?;
                                    if child_text(p, "ParameterID") == Some("1")
                                        && !child_text(p, "IsTimeVarying").is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1")
                                        && let Some(v) = child(p, "StartKeyframeValue").filter(|v| v.attribute("Encoding") == Some("base64"))
                                    {
                                        text = v.text().and_then(caption_text);
                                    }
                                }
                                if text.is_none() {
                                    warning(report, format!("{label}: static Source Text unavailable; layer name not used as visible text"));
                                }
                            }
                            if matches!(match_name, Some("AE.ADBE Motion" | "ADBE Motion")) {
                                for ptr in c.descendants().filter(|n| n.has_tag_name("Param")) {
                                    if let Some(p) = referred(Some(ptr), &ids, "ObjectRef") {
                                        spend(&mut work_left, p)?;
                                        if let Some(key) = motion_parameter(p) {
                                            if child_text(p, "IsTimeVarying").is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1") {
                                                if motion_keys.insert(key.to_string(), motion::read(p, key, report)?).is_some() {
                                                    return Err(error("duplicate animated Motion control"));
                                                }
                                            } else if let Some(v) = static_parameter_value(p) {
                                                motion.insert(key.to_string(), v.to_string());
                                            }
                                        }
                                    }
                                }
                            } else if kind == "video" && matches!(match_name, Some("AE.ADBE Lumetri" | "ADBE Lumetri")) {
                                video_effects.push(color::read(c, &ids, &mut work_left, report)?);
                            } else if kind == "audio" && matches!(match_name, Some("Internal Volume Stereo" | "Internal Channel Volume Stereo")) {
                                audio::read(c, &ids, &mut work_left, &mut audio, report)?;
                            } else if !is_text && !is_shape {
                                let count = unsupported_effects.entry((effect.clone(), match_name.unwrap_or("unknown").to_string())).or_insert(0usize);
                                *count = count.saturating_add(1);
                            }
                            effects.push(effect);
                        }
                    }
                    if kind == "caption" {
                        let block = referred(descendant(item, "BlockVectorItem"), &ids, "ObjectRef");
                        if let Some(block) = block {
                            spend(&mut work_left, block)?;
                        }
                        text = block.and_then(|b| child_text(b, "FormattedTextData")).and_then(caption_text);
                        if text.is_none() {
                            warning(report, format!("{label}: caption text could not be decoded"));
                        }
                    }
                    let mut transitions = Vec::new();
                    for key in ["HeadTransition", "TailTransition"] {
                        if let Some(t) = referred(descendant(item, key), &ids, "ObjectRef") {
                            spend(&mut work_left, t)?;
                            let name = value(t, "DisplayName").unwrap_or("Transition").to_string();
                            let start = signed_ticks(value(t, "Start"), Tick::ZERO)?;
                            let end = signed_ticks(value(t, "End"), start)?;
                            if end < start {
                                warning(report, format!("{label}: {name} end precedes start; transition skipped"));
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
                    let audio_category = value(item, "ESP.Tag").map(str::to_string);
                    let target = source_target(clip, &ids, &uids, &mut work_left)?;
                    let mut media_path = target.filter(|n| n.has_tag_name("Media")).and_then(|n| media_path(n, base_dir));
                    let mut nested_sequence = target.filter(|n| n.has_tag_name("Sequence")).and_then(|n| n.attribute("ObjectUID")).map(str::to_string);
                    if graphic {
                        // Generated graphic media may have an internal numeric pseudo-path.
                        // It is a native canvas, not a missing filesystem asset.
                        media_path = None;
                        nested_sequence = None;
                    }
                    if media_path.is_none() && nested_sequence.is_none() && kind != "caption" && !graphic && text.is_none() {
                        let named_sequence = names.get(label.as_str()).copied().flatten();
                        let named_media = media.get(&label);
                        if ambiguous.contains(&label) || names.get(label.as_str()) == Some(&None) || (named_sequence.is_some() && named_media.is_some()) {
                            warning(report, format!("{label}: unresolved source reference; ambiguous title fallback refused; left offline"));
                        } else if named_sequence.is_some() || named_media.is_some() {
                            warning(report, format!("{label}: unresolved source reference; using title fallback"));
                            media_path = named_media.cloned();
                            nested_sequence = named_sequence.map(str::to_string);
                        }
                    }
                    if let Some(path) = &media_path {
                        work_left = work_left.checked_sub(path.len() / 64).ok_or_else(|| error("media reference work limit exceeded"))?;
                    }
                    items.push(Item {
                        object_id: item.attribute("ObjectID").unwrap_or("").into(),
                        link: item.attribute("ObjectID").and_then(|id| links.get(id)).map(|id| (*id).to_string()),
                        audio,
                        video_effects,
                        graphic,
                        motion_keys,
                        media_path,
                        nested_sequence,
                        label,
                        start,
                        end,
                        source_in,
                        source_out,
                        speed,
                        text,
                        audio_category,
                        effects,
                        motion,
                        transitions,
                    });
                }
                tracks.push(Track { kind: kind.into(), index, muted, items });
            }
        }
        let object_uid = n.attribute("ObjectUID").ok_or_else(|| error("sequence without ObjectUID"))?.to_string();
        sequences.push(Sequence {
            object_uid,
            name,
            width,
            height,
            frame_rate,
            sample_rate,
            work_out: ticks(value(n, "MZ.WorkOutPoint"), Tick::ZERO)?,
            tracks,
        });
    }
    if sequences.is_empty() {
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
    Ok(Document { sequences, media })
}

fn validate_nesting(sequences: &[Sequence]) -> Result<()> {
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
        for track in &src.tracks {
            if track.kind == "caption" {
                let mut captions = CaptionTrack::new(TrackId(b.alloc()), format!("Captions {}", track.index + 1), CaptionFormat::Subtitle);
                captions.enabled = !track.muted;
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
            }
            let mut transition_ids = BTreeMap::new();
            for item in &track.items {
                let nested = item.nested_sequence.as_ref().and_then(|n| sequences.get(n)).copied();
                let source = if let Some(id) = nested {
                    id
                } else if item.graphic || item.text.is_some() {
                    if item.text.is_some() {
                        warning(&mut report, "text graphic imported with default font/style; source styling retained in original snapshot");
                    }
                    b.p.add_item(&item.label, Label::Lavender, ItemKind::Graphic { width: src.width, height: src.height, rate: fps }, None)
                } else {
                    let path = item.media_path.clone().unwrap_or_else(|| format!("__missing_premiere__/{}", item.object_id));
                    let spec = MediaSpec {
                        duration: Some(item.source_out),
                        video: (kind == TrackKind::Video).then_some((src.width, src.height, fps)),
                        audio: (kind == TrackKind::Audio).then_some((48000, 2)),
                        ..Default::default()
                    };
                    let id = b.file_media(&format!("{}:{path}", track.kind), &item.label, &path, &spec, None);
                    if let Some(pi) = b.p.item_mut(id)
                        && let ItemKind::Media(m) = &mut pi.kind
                    {
                        m.offline = true;
                    }
                    if item.media_path.is_none() {
                        warning(&mut report, format!("{}: media unresolved; kept offline placeholder", item.label));
                    }
                    id
                };
                let mut clip = b.clip(source, kind, &item.label, item.start, item.end - item.start, item.source_in);
                clip.speed = item.speed;
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
    Ok((b.finish(), doc, report))
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
            xml.replace("<InPoint>", "<InPoint>-"),
            xml.replace(&format!("<OutPoint>{}", 10 * TPS), "<OutPoint>-1"),
            xml.replace(&format!("<OutPoint>{}", 10 * TPS), "<OutPoint>invalid"),
            xml.replace("<PlaybackSpeed>2", "<PlaybackSpeed>invalid"),
            xml.replace("<InPoint>", "<InPoint>999999999999999999999"),
            xml.replace("<PremiereData Version=\"3\">", "<!DOCTYPE x [<!ENTITY a SYSTEM 'file:///etc/passwd'>]><PremiereData Version=\"3\">"),
            xml.replace("<Media>", "<Media ObjectID=\"20\">"),
            xml.replace("1080</MZ.Sequence", "4294967295</MZ.Sequence"),
            mixed_support_fixture(3 * TPS, "-1", "", "1"),
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
            label: name.clone(),
            start: Tick::ZERO,
            end: Tick(TPS),
            source_in: Tick::ZERO,
            source_out: Tick(TPS),
            speed: 1.0,
            media_path: None,
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
                tracks: vec![Track { kind: "video".into(), index: 0, muted: false, items }],
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
