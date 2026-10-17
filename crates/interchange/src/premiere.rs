//! Premiere project reader, based on llm-video-editor's `premiere.py` and user-owned XML structure.
//! No Adobe code or assets. MIT OR Apache-2.0. Unsupported features remain in the source snapshot
//! and are reported; this is not a promise of pixel-identical Premiere rendering.

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

pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_NODES: u32 = 500_000;
const MAX_TRACKS: usize = 256;
const MAX_SEQUENCES: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub sequences: Vec<Sequence>,
    pub media: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sequence {
    pub name: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub frame_rate: Option<FrameRate>,
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
    pub transitions: Vec<SourceTransition>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceTransition {
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
        .checked_sub(node.descendants().count().saturating_add(node.range().len() / 64))
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

/// Decode bounded gzip/plain XML without any filesystem or network access.
pub fn read(bytes: &[u8], base_dir: Option<&str>, report: &mut Report) -> Result<Document> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(error("project exceeds 64 MiB input limit"));
    }
    let mut expanded = Vec::new();
    if bytes.starts_with(&[0x1f, 0x8b]) {
        flate2::read::MultiGzDecoder::new(bytes).take(MAX_DOCUMENT_BYTES as u64 + 1).read_to_end(&mut expanded).map_err(|e| error(format!("gzip: {e}")))?;
    } else {
        expanded.extend_from_slice(bytes);
    }
    if expanded.len() > MAX_DOCUMENT_BYTES {
        return Err(error("expanded project exceeds 64 MiB limit"));
    }
    let xml = std::str::from_utf8(&expanded).map_err(|e| error(e.to_string()))?;
    let doc = roxmltree::Document::parse_with_options(xml, roxmltree::ParsingOptions { nodes_limit: MAX_NODES, ..Default::default() })
        .map_err(|e| error(e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "PremiereData" {
        return Err(error("expected PremiereData root"));
    }
    let mut ids = BTreeMap::new();
    let mut uids = BTreeMap::new();
    let mut names = BTreeSet::new();
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
            let name = child_text(n, "Name").ok_or_else(|| error("sequence without name"))?;
            if !names.insert(name) {
                return Err(error(format!("ambiguous sequence name: {name}")));
            }
        }
        if n.has_tag_name("Media")
            && let (Some(title), Some(path)) = (child_text(n, "Title"), child_text(n, "ActualMediaFilePath").or_else(|| child_text(n, "FilePath")))
        {
            if title.len() > 4096 || path.len() > 16384 {
                return Err(error("media title or path is too long"));
            }
            let path = crate::resolve_path(path, base_dir);
            if media.get(title).is_some_and(|p| p != &path) {
                ambiguous.insert(title.to_string());
            }
            media.insert(title.to_string(), path);
        }
    }
    if names.len() > MAX_SEQUENCES {
        return Err(error("too many sequences"));
    }
    for title in ambiguous {
        media.remove(&title);
        warning(report, format!("ambiguous media title {title}; left offline rather than guessing"));
    }
    let mut work_left = 5_000_000usize;
    let mut sequences = Vec::new();
    let mut item_count = 0usize;
    for n in root.children().filter(|n| n.has_tag_name("Sequence")) {
        spend(&mut work_left, n)?;
        let name = child_text(n, "Name").unwrap_or("Sequence").to_string();
        let width = dimension(value(n, "MZ.Sequence.PreviewFrameSizeWidth"), 1920)?;
        let height = dimension(value(n, "MZ.Sequence.PreviewFrameSizeHeight"), 1080)?;
        filmcraft_project::validate_frame_size(width, height).map_err(error)?;
        let mut frame_rate = None;
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
                    let start = ticks(value(item, "Start"), Tick::ZERO)?;
                    let end = ticks(value(item, "End"), start)?;
                    if end <= start {
                        return Err(error("timeline item must have positive duration"));
                    }
                    let sub = referred(descendant(item, "SubClip"), &ids, "ObjectRef");
                    if let Some(sub) = sub {
                        spend(&mut work_left, sub)?;
                    }
                    let label = sub.and_then(|s| child_text(s, "Name")).unwrap_or(item.tag_name().name()).to_string();
                    let clip = referred(sub.and_then(|s| child(s, "Clip")), &ids, "ObjectRef");
                    if let Some(clip) = clip {
                        spend(&mut work_left, clip)?;
                    }
                    if let Some(path) = media.get(&label) {
                        work_left = work_left.checked_sub(path.len() / 64).ok_or_else(|| error("media reference work limit exceeded"))?;
                    }
                    let source_in = ticks(clip.and_then(|s| value(s, "InPoint")), Tick::ZERO)?;
                    let source_out = ticks(
                        clip.and_then(|s| value(s, "OutPoint")),
                        Tick(source_in.0.checked_add(end.0 - start.0).ok_or_else(|| error("source time overflow"))?),
                    )?;
                    let speed = clip.and_then(|s| value(s, "PlaybackSpeed")).unwrap_or("1").parse::<f64>().map_err(|_| error("invalid playback speed"))?;
                    if !speed.is_finite()
                        || !(0.001..=1000.0).contains(&speed)
                        || source_out < source_in
                        || source_in.0 as f64 + (end.0 - start.0) as f64 * speed > Tick::MAX.0 as f64
                    {
                        return Err(error("unsupported speed or source range"));
                    }
                    let mut effects = Vec::new();
                    let mut motion = BTreeMap::new();
                    let mut text = None;
                    let chain = referred(descendant(item, "ComponentOwner").and_then(|n| child(n, "Components")), &ids, "ObjectRef");
                    if let Some(chain) = chain {
                        spend(&mut work_left, chain)?;
                        for ptr in chain.descendants().filter(|n| n.has_tag_name("Component")) {
                            let Some(c) = referred(Some(ptr), &ids, "ObjectRef") else { continue };
                            spend(&mut work_left, c)?;
                            let effect = value(c, "DisplayName").or_else(|| child_text(c, "FilterMatchName")).unwrap_or("Unknown").to_string();
                            if effect == "Text" {
                                text = value(c, "InstanceName").map(|s| s.replace('\r', "\n"));
                            }
                            if effect == "Motion" {
                                for ptr in c.descendants().filter(|n| n.has_tag_name("Param")) {
                                    if let Some(p) = referred(Some(ptr), &ids, "ObjectRef")
                                        && let (Some(k), Some(v)) = (child_text(p, "Name"), child_text(p, "CurrentValue"))
                                    {
                                        spend(&mut work_left, p)?;
                                        motion.insert(k.to_string(), v.to_string());
                                    }
                                }
                            } else if effect != "Text" {
                                warning(report, format!("{label}: effect {effect} retained in provenance, not rendered"));
                            }
                            effects.push(effect);
                        }
                    }
                    if kind == "caption" {
                        text = referred(descendant(item, "BlockVectorItem"), &ids, "ObjectRef")
                            .and_then(|b| child_text(b, "FormattedTextData"))
                            .and_then(caption_text);
                        if text.is_none() {
                            warning(report, format!("{label}: caption text could not be decoded"));
                        }
                    }
                    let mut transitions = Vec::new();
                    for key in ["HeadTransition", "TailTransition"] {
                        if let Some(t) = referred(descendant(item, key), &ids, "ObjectRef") {
                            let name = value(t, "DisplayName").unwrap_or("Transition").to_string();
                            let start = signed_ticks(value(t, "Start"), Tick::ZERO)?;
                            let end = signed_ticks(value(t, "End"), start)?;
                            if end < start {
                                return Err(error("transition end precedes start"));
                            }
                            transitions.push(SourceTransition { head: key == "HeadTransition", name, duration: end - start });
                        }
                    }
                    let audio_category = value(item, "ESP.Tag").map(str::to_string);
                    items.push(Item {
                        object_id: item.attribute("ObjectID").unwrap_or("").into(),
                        media_path: media.get(&label).cloned(),
                        nested_sequence: names.contains(label.as_str()).then(|| label.clone()),
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
        sequences.push(Sequence { name, width, height, frame_rate, work_out: ticks(value(n, "MZ.WorkOutPoint"), Tick::ZERO)?, tracks });
    }
    if sequences.is_empty() {
        return Err(Error::Empty);
    }
    validate_nesting(&sequences)?;
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
    let graph: BTreeMap<_, _> = sequences.iter().map(|s| (s.name.as_str(), s)).collect();
    let mut depths = BTreeMap::new();
    for seq in sequences {
        depth(&seq.name, &graph, &mut BTreeSet::new(), &mut depths)?;
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
        let settings = settings_for(fps, seq.width, seq.height, false);
        settings.validate().map_err(error)?;
        let id = b.reserve_sequence(&seq.name, settings, None);
        sequences.insert(seq.name.clone(), id);
        b.top.push(id);
    }
    for src in &doc.sequences {
        let fps = rate.or(src.frame_rate).unwrap_or(FrameRate::FPS_24);
        let mut seq = empty_sequence(settings_for(fps, src.width, src.height, false));
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
            for item in &track.items {
                let nested = item.nested_sequence.as_ref().and_then(|n| sequences.get(n)).copied();
                let source = if let Some(id) = nested {
                    id
                } else if item.label == "Graphic" && item.text.is_some() {
                    warning(&mut report, "text graphic imported with default font/style; source styling retained in original snapshot");
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
                if item.label == "Graphic"
                    && let Some(text) = &item.text
                {
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
                for transition in &item.transitions {
                    let duration = Tick(transition.duration.0.min(clip.duration.0));
                    if duration.0 == 0 {
                        continue;
                    }
                    if duration != transition.duration {
                        warning(&mut report, format!("{}: transition shortened to clip duration", item.label));
                    }
                    // The Python importer maps these to clip-edge fades. Preserve that behavior;
                    // do not invent an overlapping edit or duplicate shared transition handles.
                    let effect = crate::common::transition_effect(
                        if kind == TrackKind::Audio { "Constant Gain" } else { "Cross Dissolve" },
                        kind == TrackKind::Audio,
                        &mut report,
                    );
                    limited_report(&mut report, crate::Level::Info, format!("{}: {} imported as a clip-edge fade", item.label, transition.name));
                    let tr = Transition {
                        id: TransitionId(b.alloc()),
                        effect,
                        start: if transition.head { clip.start } else { clip.end() - duration },
                        duration,
                        from: (!transition.head).then_some(clip.id),
                        to: transition.head.then_some(clip.id),
                        align: if transition.head { TransitionAlign::StartAtCut } else { TransitionAlign::EndAtCut },
                        reverse: false,
                    };
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
        if let Some(id) = sequences.get(&src.name) {
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
            assert!(!report.has_warnings());
            let ItemKind::Media(m) = &fragment.project.item(clip.item).unwrap().kind else { panic!() };
            assert!(m.offline);
        }
    }
    #[test]
    fn unsafe_sizes_times_speeds_entities_and_duplicate_ids_are_errors() {
        let xml = fixture();
        let cases = [
            xml.replace("<Index>0", "<Index>999999999999"),
            xml.replace("<Start>0", "<Start>-1"),
            xml.replace("<PlaybackSpeed>2", "<PlaybackSpeed>NaN"),
            xml.replace("<InPoint>", "<InPoint>999999999999999999999"),
            xml.replace("<PremiereData Version=\"3\">", "<!DOCTYPE x [<!ENTITY a SYSTEM 'file:///etc/passwd'>]><PremiereData Version=\"3\">"),
            xml.replace("<Media>", "<Media ObjectID=\"20\">"),
            xml.replace("1080</MZ.Sequence", "4294967295</MZ.Sequence"),
        ];
        for case in cases {
            let r = std::panic::catch_unwind(|| import(case.as_bytes(), &ImportOptions::default(), None));
            assert!(r.is_ok());
            assert!(r.unwrap().is_err());
        }
    }
    #[test]
    fn self_nesting_refused_and_ambiguous_media_stays_offline() {
        let cycle = fixture().replace("<Name>camera.mov</Name>", "<Name>Sequence 01</Name>");
        assert!(import(cycle.as_bytes(), &ImportOptions::default(), None).unwrap_err().to_string().contains("cyclic"));
        let xml = fixture().replace("</PremiereData>", "<Media><Title>camera.mov</Title><FilePath>/different.mov</FilePath></Media></PremiereData>");
        let (_, tree, report) = import(xml.as_bytes(), &ImportOptions::default(), None).unwrap();
        assert!(tree.sequences[0].tracks[0].items[0].media_path.is_none());
        assert!(report.mentions("ambiguous"));
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
        for _ in 0..1025 {
            gz.write_all(&chunk).unwrap();
        }
        assert!(import(&gz.finish().unwrap(), &ImportOptions::default(), None).unwrap_err().to_string().contains("64 MiB"));
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
            transitions: vec![],
        };
        let mut seqs = Vec::new();
        for i in 0..65 {
            let items = if i < 64 { vec![item("64".into()), item((i + 1).to_string())] } else { vec![] };
            seqs.push(Sequence {
                name: i.to_string(),
                width: 16,
                height: 16,
                frame_rate: None,
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
}
