//! Import-only reader for PremiereData v3 project documents (gzip or plain UTF-8 XML).
//!
//! Clean-room mapping from user-authored project data, never application code or bundled assets.
//! See the crate README for the supported subset and loss reporting.

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;

use filmcraft_geom::Vec2;
use filmcraft_project::{ItemId, Param, ParamValue, ProjectView, SequenceSettings, Track, TrackItem, TrackKind, Transition, TransitionAlign, TransitionId};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use roxmltree::{Document, Node};

use crate::common::{Builder, MediaSpec, empty_sequence, kind_from_ext, resolve_path, set_param, transition_effect};
use crate::xml::{child, child_text, elements};
use crate::{Error, Format, ImportOptions, Imported, Report, Result};

/// Bounds apply before decompression and DOM construction; gzip trailer sizes are not trusted.
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_XML_BYTES: usize = 128 * 1024 * 1024;
const MAX_NODES: u32 = 500_000;
const MAX_TRACKS: usize = 1024;
const MAX_SEQUENCES: usize = 4096;
const MAX_MODEL_OBJECTS: usize = 50_000;
const MAX_REFERENCES: usize = 100_000;
const MAX_IMPORTED_TEXT: usize = 32 * 1024 * 1024;
const MAX_WARNINGS: usize = 4096;

fn bad(message: impl Into<String>) -> Error {
    Error::parse(Format::Premiere, message)
}

/// Convert a Premiere project to a standalone FilmCraft project without accessing media files.
pub fn import(bytes: &[u8], opts: &ImportOptions) -> Result<(Imported, Report)> {
    import_with_view(bytes, opts).map(|(imported, _, report)| (imported, report))
}

/// Also retain the saved open sequence tabs, so opening a conversion shows the edit rather than
/// whichever sequence happens to be stored first. If several tabs were open, select the first
/// saved tab; the source's focused-tab state is not yet mapped.
pub fn import_with_view(bytes: &[u8], opts: &ImportOptions) -> Result<(Imported, ProjectView, Report)> {
    if opts.name.as_ref().is_some_and(|s| s.len() > 1024) || opts.base_dir.as_ref().is_some_and(|s| s.len() > 32768) {
        return Err(bad("import name or base directory exceeds the text budget"));
    }
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(bad("project exceeds the 64 MiB input limit"));
    }
    let decoded;
    let xml = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(bytes)
            .take(MAX_XML_BYTES as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| bad(format!("cannot decompress project: {e}")))?;
        if out.len() > MAX_XML_BYTES {
            return Err(bad("decompressed project exceeds the 128 MiB limit"));
        }
        decoded = out;
        decoded.as_slice()
    } else {
        bytes
    };
    let text = std::str::from_utf8(xml).map_err(|e| bad(format!("project XML is not UTF-8: {e}")))?;
    preflight_xml(text)?;
    let doc = Document::parse_with_options(text, roxmltree::ParsingOptions { nodes_limit: MAX_NODES, ..Default::default() })
        .map_err(|e| bad(format!("invalid project XML: {e}")))?;
    let root = doc.root_element();
    if !root.has_tag_name("PremiereData") || root.attribute("Version") != Some("3") {
        return Err(bad("expected <PremiereData Version=\"3\">; this project version is unsupported"));
    }
    validate_text_limits(root)?;
    let graph = Graph::new(root)?;
    let mut builder = Builder::new(opts.name.as_deref().unwrap_or("Premiere Project"));
    let mut report = Report::default();
    let mut sources = HashMap::new();
    let sequences: Vec<_> = elements(root).filter(|n| n.has_tag_name("Sequence") && definition(*n).is_some()).collect();
    if sequences.is_empty() {
        return Err(Error::Empty);
    }
    if sequences.len() > MAX_SEQUENCES {
        return Err(bad("too many sequences (limit 4096)"));
    }
    // Reserve every sequence first: nested references do not recurse through the document.
    for &n in &sequences {
        let name = child_text(n, "Name").unwrap_or("Sequence");
        let settings = graph.settings(n)?;
        graph.claim_object()?;
        graph.claim_text(name.len().saturating_mul(4))?;
        let id = builder.reserve_sequence(name, settings, None);
        sources.insert(n.id(), id);
        builder.top.push(id);
    }
    for n in elements(root).filter(|n| n.has_tag_name("Media") && definition(*n).is_some()) {
        let path = ["ActualMediaFilePath", "FilePath", "RelativePath"].into_iter().find_map(|tag| child_text(n, tag).filter(|s| !s.is_empty()));
        let Some(path) = path else {
            warning(&mut report, format!("media {} has no file path and cannot be imported", definition(n).unwrap_or("?")))?;
            continue;
        };
        let path = resolve_path(path, opts.base_dir.as_deref());
        if path.len() > 32768 {
            return Err(bad("resolved media path exceeds the text budget"));
        }
        let video = graph.optional_at(n, &["VideoStream"])?;
        let audio = graph.optional_at(n, &["AudioStream"])?;
        let mut spec = MediaSpec { kind: Some(kind_from_ext(&path)), ..Default::default() };
        if let Some(v) = video {
            let (w, h) = frame_size(v)?;
            spec.video = Some((w, h, rate(v)?));
            spec.duration = Some(ticks(v, "Duration", None)?);
        }
        if let Some(a) = audio {
            let sample_rate = sample_rate(a)?;
            let channels = match child_text(a, "AudioChannelLayout") {
                Some(s) => {
                    let layout: Vec<serde_json::Value> = serde_json::from_str(s).map_err(|e| bad(format!("invalid audio channel layout: {e}")))?;
                    if layout.is_empty() || layout.len() > 64 {
                        return Err(bad("audio channel count must be 1..64"));
                    }
                    layout.len() as u32
                }
                None => 2,
            };
            spec.audio = Some((sample_rate, channels));
            spec.duration = Some(spec.duration.unwrap_or(Tick::ZERO).max(ticks(a, "Duration", None)?));
        }
        let name = child_text(n, "Title").unwrap_or_else(|| crate::common::file_name(&path));
        graph.claim_object()?;
        graph.claim_text(name.len().saturating_mul(4).saturating_add(path.len().saturating_mul(2)))?;
        let id = builder.file_media(definition(n).unwrap_or(name), name, &path, &spec, None);
        sources.insert(n.id(), id);
    }
    let mut links = HashMap::new();
    for n in elements(root).filter(|n| n.has_tag_name("Link") && definition(*n).is_some()) {
        let id = builder.link_id();
        for r in at(n, &["TrackItemGroup", "TrackItems"]).into_iter().flat_map(elements) {
            let target = graph.resolve(r)?;
            if links.insert(target.id(), id).is_some() {
                return Err(bad("a clip belongs to more than one link group"));
            }
        }
    }
    for n in sequences {
        let id = *sources.get(&n.id()).ok_or_else(|| bad("missing reserved sequence"))?;
        let mut seq = empty_sequence(graph.settings(n)?);
        let name = child_text(n, "Name").unwrap_or("Sequence");
        for group in graph.groups(n)? {
            let kind = match group.tag_name().name() {
                "VideoTrackGroup" => TrackKind::Video,
                "AudioTrackGroup" => TrackKind::Audio,
                _ => {
                    warning(&mut report, format!("sequence {name:?}: unsupported {} (captions/data tracks are not imported)", group.tag_name().name()))?;
                    continue;
                }
            };
            let refs = ordered(at(group, &["TrackGroup", "Tracks"]))?;
            if refs.len() > MAX_TRACKS {
                return Err(bad("too many tracks (limit 1024 per group)"));
            }
            for r in refs {
                let tr = graph.resolve(r)?;
                graph.claim_object()?;
                let mut track = builder.track(kind, seq.tracks(kind).len());
                let metadata = at(tr, &["ClipTrack", "Track"]).ok_or_else(|| bad("unsupported track layout"))?;
                if let Some(s) = child_text(metadata, "Name") {
                    graph.claim_text(s.len())?;
                    track.name = s.into();
                }
                track.enabled = !boolean(metadata, "IsDisabled", false)?;
                track.muted = boolean(metadata, "IsMuted", false)?;
                track.locked = boolean(metadata, "IsLocked", false)?;
                for cr in ordered(at(tr, &["ClipTrack", "ClipItems", "TrackItems"]))? {
                    let node = graph.resolve(cr)?;
                    let base = child(node, "ClipTrackItem").ok_or_else(|| bad(format!("unsupported timeline item {}", node.tag_name().name())))?;
                    let timing = child(base, "TrackItem").ok_or_else(|| bad("clip has no TrackItem timing"))?;
                    let start = ticks(timing, "Start", Some(Tick::ZERO))?;
                    let end = ticks(timing, "End", None)?;
                    if end <= start {
                        return Err(bad("clip End must be after Start"));
                    }
                    let sub = graph.required_at(base, &["SubClip"])?;
                    let clip = graph.required_at(sub, &["Clip"])?;
                    let clip_data = child(clip, "Clip").ok_or_else(|| bad("unsupported source clip layout"))?;
                    let source = graph.required_at(clip_data, &["Source"])?;
                    let target = match source.tag_name().name() {
                        "VideoMediaSource" | "AudioMediaSource" => graph.required_at(source, &["MediaSource", "Media"]),
                        "VideoSequenceSource" | "AudioSequenceSource" => graph.required_at(source, &["SequenceSource", "Sequence"]),
                        _ => Err(bad(format!("unsupported source {}; export Final Cut Pro XML from Premiere for this project", source.tag_name().name()))),
                    }?;
                    let item = *sources.get(&target.id()).ok_or_else(|| bad("clip references media without a supported file path"))?;
                    let clip_name = child_text(sub, "Name").unwrap_or("Clip");
                    let source_in = ticks(clip_data, "InPoint", Some(Tick::ZERO))?;
                    graph.claim_object()?;
                    graph.claim_text(clip_name.len().saturating_mul(2))?;
                    let mut ti = builder.clip(item, kind, clip_name, start, end - start, source_in);
                    ti.speed = number(clip_data, "PlaybackSpeed", 1.0)?;
                    if ti.speed <= 0.0 || ti.speed > 1000.0 {
                        return Err(bad("unsupported playback speed (must be positive and at most 1000)"));
                    }
                    ti.enabled = !boolean(base, "IsDisabled", false)? && !boolean(timing, "IsDisabled", false)?;
                    ti.link = links.get(&node.id()).copied();
                    let sw_sh = builder
                        .p
                        .item(item)
                        .and_then(|i| {
                            i.as_media()
                                .and_then(|m| m.info.video.as_ref())
                                .map(|v| (v.width, v.height))
                                .or_else(|| i.as_sequence().map(|s| (s.settings.width, s.settings.height)))
                        })
                        .unwrap_or((seq.settings.width, seq.settings.height));
                    graph.effects(base, &mut ti, (seq.settings.width, seq.settings.height), sw_sh, &mut report)?;
                    track.items.push(ti);
                }
                track.sort();
                for pair in track.items.windows(2) {
                    if let [left, right] = pair
                        && left.end() > right.start
                    {
                        return Err(bad("overlapping clips on one track are unsupported"));
                    }
                }
                for r in ordered(at(tr, &["ClipTrack", "TransitionItems", "TrackItems"]))? {
                    let transition = graph.resolve(r)?;
                    graph.transition(transition, &mut track, &mut builder, &mut report)?;
                }
                seq.tracks_mut(kind).push(track);
            }
        }
        if let Some(props) = at(n, &["Node", "Properties"])
            && child(props, "MZ.WorkInPoint").is_some()
            && child(props, "MZ.WorkOutPoint").is_some()
        {
            let start = ticks(props, "MZ.WorkInPoint", None)?;
            let end = ticks(props, "MZ.WorkOutPoint", None)?;
            if end >= start {
                seq.work_area = Some(TimeRange::from_bounds(start, end));
            }
        }
        builder.put_sequence(id, seq);
    }
    reject_sequence_cycles(&builder)?;
    let nested: HashSet<_> = builder
        .top
        .iter()
        .filter_map(|id| builder.p.sequence(*id))
        .flat_map(|s| s.all_tracks())
        .flat_map(|t| &t.items)
        .filter(|c| builder.p.sequence(c.item).is_some())
        .map(|c| c.item)
        .collect();
    builder.top.retain(|id| !nested.contains(id));
    // Resolve intrinsic Motion defaults to finite centres before serialising the native file.
    builder.p.resolve_placed_auto_points(|_| true, |_| true);
    warning(
        &mut report,
        "Premiere import is a subset: bins, markers, captions, graphics, colour management, mixer automation, source effects and export settings are not transferred; inspect the result before continuing the edit",
    )?;
    let imported = builder.finish();
    let view = project_view(root, &graph, &sources, &imported.sequences)?;
    Ok((imported, view, report))
}

fn project_view(root: Node<'_, '_>, graph: &Graph<'_, '_>, sources: &HashMap<roxmltree::NodeId, ItemId>, top: &[ItemId]) -> Result<ProjectView> {
    let mut tabs = std::collections::BTreeMap::new();
    let mut entries = 0usize;
    for props in elements(root).filter(|n| n.has_tag_name("Project") && definition(*n).is_some()).filter_map(|n| at(n, &["Node", "Properties"])) {
        for n in elements(props) {
            let Some(index) = n.tag_name().name().strip_prefix("MZ.PrefixKey.OpenSequenceGuidList.") else { continue };
            entries = entries.saturating_add(1);
            if entries > MAX_SEQUENCES {
                return Err(bad("too many saved sequence tabs (limit 4096)"));
            }
            // View metadata can be stale. Only known sequence UIDs and bounded numeric indices
            // are used; no reference is allowed to create media or alter the edit.
            let Some(index) = index.parse::<u32>().ok() else { continue };
            let Some(sequence) = n.text().and_then(|uid| graph.uids.get(uid.trim())).filter(|n| n.has_tag_name("Sequence")) else { continue };
            let Some(&id) = sources.get(&sequence.id()) else { continue };
            if tabs.insert(index, id).is_some() {
                return Err(bad("duplicate saved sequence tab index"));
            }
        }
    }
    let mut seen = HashSet::new();
    let mut open_sequences: Vec<_> = tabs.into_values().filter(|id| seen.insert(*id)).collect();
    if open_sequences.is_empty() {
        open_sequences.extend(top.first().copied());
    }
    Ok(ProjectView { active_sequence: open_sequences.first().copied(), open_sequences, ..Default::default() })
}

/// roxmltree's tokenizer recurses into elements. Bound the depth with a streaming parser before
/// constructing the DOM, including for files whose deep branch is otherwise never imported.
fn preflight_xml(text: &str) -> Result<()> {
    // roxmltree estimates its initial node/attribute capacities from literal delimiters, even
    // inside text and comments. Bound these counts as well as actual parsed node counts.
    for delimiter in *b"<=" {
        if text.bytes().filter(|b| *b == delimiter).take(MAX_NODES as usize + 1).count() > MAX_NODES as usize {
            return Err(bad("XML exceeds the node/attribute allocation budget"));
        }
    }
    let mut reader = quick_xml::Reader::from_str(text);
    let mut depth = 0usize;
    loop {
        match reader.read_event().map_err(|e| bad(format!("invalid project XML: {e}")))? {
            quick_xml::events::Event::Start(_) => {
                depth = depth.saturating_add(1);
                if depth > 128 {
                    return Err(bad("XML nesting exceeds 128 levels"));
                }
            }
            quick_xml::events::Event::End(_) => depth = depth.saturating_sub(1),
            quick_xml::events::Event::DocType(_) => return Err(bad("XML DTDs and external entities are unsupported")),
            quick_xml::events::Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

fn warning(report: &mut Report, message: impl Into<String>) -> Result<()> {
    if report.entries.len() >= MAX_WARNINGS {
        return Err(bad("import warning budget exceeded"));
    }
    report.warn(message);
    Ok(())
}

fn validate_text_limits(root: Node<'_, '_>) -> Result<()> {
    for n in root.descendants().filter(|n| n.is_element()) {
        let tag = n.tag_name().name();
        if tag.len() > 1024 {
            return Err(bad("XML element name exceeds the text budget"));
        }
        let limit = match tag {
            "Name" | "Title" | "MatchName" | "FilterMatchName" | "StartKeyframe" | "CurrentValue" => Some(1024),
            "ParameterID" => Some(64),
            "FrameRect" => Some(256),
            "AudioChannelLayout" => Some(8192),
            "FilePath" | "ActualMediaFilePath" | "RelativePath" => Some(32768),
            _ if tag.starts_with("MZ.PrefixKey.OpenSequenceGuidList.") => Some(256),
            _ => None,
        };
        if limit.is_some_and(|limit| n.text().is_some_and(|s| s.len() > limit)) {
            return Err(bad(format!("{tag} exceeds the text budget")));
        }
        for a in n.attributes() {
            if matches!(a.name(), "ObjectID" | "ObjectUID" | "ObjectRef" | "ObjectURef") && a.value().len() > 256 {
                return Err(bad("object identifier exceeds the text budget"));
            }
        }
    }
    Ok(())
}

fn definition<'a>(n: Node<'a, '_>) -> Option<&'a str> {
    n.attribute("ObjectID").or_else(|| n.attribute("ObjectUID"))
}

fn at<'a, 'i>(mut n: Node<'a, 'i>, path: &[&str]) -> Option<Node<'a, 'i>> {
    for name in path {
        n = child(n, name)?;
    }
    Some(n)
}

fn ordered<'a, 'i>(parent: Option<Node<'a, 'i>>) -> Result<Vec<Node<'a, 'i>>> {
    let mut entries = Vec::new();
    let mut indices = HashSet::new();
    for n in parent.into_iter().flat_map(elements) {
        let index = n.attribute("Index").ok_or_else(|| bad("list entry lacks Index"))?.parse::<u32>().map_err(|_| bad("invalid list Index"))?;
        if !indices.insert(index) {
            return Err(bad("duplicate list Index"));
        }
        entries.push((index, n));
    }
    entries.sort_by_key(|(i, _)| *i);
    Ok(entries.into_iter().map(|(_, n)| n).collect())
}

struct Graph<'a, 'i> {
    ids: HashMap<&'a str, Node<'a, 'i>>,
    uids: HashMap<&'a str, Node<'a, 'i>>,
    objects_left: Cell<usize>,
    references_left: Cell<usize>,
    text_left: Cell<usize>,
}

impl<'a, 'i> Graph<'a, 'i> {
    fn new(root: Node<'a, 'i>) -> Result<Self> {
        let mut g = Self {
            ids: HashMap::new(),
            uids: HashMap::new(),
            objects_left: Cell::new(MAX_MODEL_OBJECTS),
            references_left: Cell::new(MAX_REFERENCES),
            text_left: Cell::new(MAX_IMPORTED_TEXT),
        };
        for n in elements(root) {
            for (attribute, map) in [("ObjectID", &mut g.ids), ("ObjectUID", &mut g.uids)] {
                if let Some(id) = n.attribute(attribute)
                    && (id.is_empty() || map.insert(id, n).is_some())
                {
                    return Err(bad(format!("duplicate or empty {attribute}")));
                }
            }
        }
        Ok(g)
    }

    fn resolve(&self, n: Node<'a, 'i>) -> Result<Node<'a, 'i>> {
        consume(&self.references_left, 1, "object-reference work")?;
        let found = if let Some(id) = n.attribute("ObjectRef") {
            self.ids.get(id)
        } else if let Some(id) = n.attribute("ObjectURef") {
            self.uids.get(id)
        } else {
            return Err(bad(format!("{} has no object reference", n.tag_name().name())));
        };
        found.copied().ok_or_else(|| bad(format!("dangling {} reference", n.tag_name().name())))
    }

    fn claim_object(&self) -> Result<()> {
        consume(&self.objects_left, 1, "imported object")
    }
    fn claim_text(&self, bytes: usize) -> Result<()> {
        consume(&self.text_left, bytes, "imported text")
    }

    fn optional_at(&self, n: Node<'a, 'i>, path: &[&str]) -> Result<Option<Node<'a, 'i>>> {
        at(n, path).map(|r| self.resolve(r)).transpose()
    }

    fn required_at(&self, n: Node<'a, 'i>, path: &[&str]) -> Result<Node<'a, 'i>> {
        self.optional_at(n, path)?.ok_or_else(|| bad(format!("{} lacks {}", n.tag_name().name(), path.join("/"))))
    }

    fn groups(&self, sequence: Node<'a, 'i>) -> Result<Vec<Node<'a, 'i>>> {
        ordered(child(sequence, "TrackGroups"))?.into_iter().map(|r| self.required_at(r, &["Second"])).collect()
    }

    fn settings(&self, sequence: Node<'a, 'i>) -> Result<SequenceSettings> {
        let groups = self.groups(sequence)?;
        let video = groups.iter().find(|n| n.has_tag_name("VideoTrackGroup")).copied().ok_or_else(|| bad("sequence has no VideoTrackGroup"))?;
        let (width, height) = frame_size(video)?;
        let vg = child(video, "TrackGroup").ok_or_else(|| bad("missing video TrackGroup"))?;
        let frame_rate = rate(vg)?;
        let sample_rate =
            groups.iter().find(|n| n.has_tag_name("AudioTrackGroup")).and_then(|n| child(*n, "TrackGroup")).map(sample_rate).transpose()?.unwrap_or(48000);
        Ok(SequenceSettings { width, height, frame_rate, sample_rate, preset: "Imported Premiere sequence".into(), ..Default::default() })
    }

    fn effects(&self, base: Node<'a, 'i>, ti: &mut TrackItem, size: (u32, u32), source_size: (u32, u32), report: &mut Report) -> Result<()> {
        let Some(chain) = self.optional_at(base, &["ComponentOwner", "Components"])? else { return Ok(()) };
        for r in ordered(at(chain, &["ComponentChain", "Components"]))? {
            let effect = self.resolve(r)?;
            let name = child_text(effect, "MatchName").or_else(|| child_text(effect, "FilterMatchName")).unwrap_or("unknown effect");
            let component = child(effect, "Component").or_else(|| at(effect, &["AudioComponent", "Component"]));
            let params: Vec<_> = ordered(component.and_then(|n| child(n, "Params")))?.into_iter().map(|r| self.resolve(r)).collect::<Result<_>>()?;
            match name {
                "AE.ADBE Motion" => {
                    for p in params {
                        let id = child_text(p, "ParameterID").unwrap_or("");
                        let value = static_value(p, report)?;
                        let Some(value) = value else { continue };
                        let mapping = match id {
                            "1" | "6" => {
                                let (x, y) = value.split_once(':').ok_or_else(|| bad("invalid Motion point"))?;
                                let (w, h) = if id == "1" { size } else { source_size };
                                let point = Vec2::new(finite(x)? * f64::from(w), finite(y)? * f64::from(h));
                                if point.x.abs() > 1e9 || point.y.abs() > 1e9 {
                                    return Err(bad("Motion point exceeds supported bounds"));
                                }
                                Some((if id == "1" { "position" } else { "anchor" }, ParamValue::Vec2(point)))
                            }
                            "2" => Some(("scale", ParamValue::Float(bounded(value, 0.0, 10000.0)?))),
                            "3" => Some(("scale_width", ParamValue::Float(bounded(value, 0.0, 10000.0)?))),
                            "4" => Some(("uniform_scale", ParamValue::Bool(parse_bool(value)?))),
                            "5" => Some(("rotation", ParamValue::Float(bounded(value, -32768.0, 32767.0)?))),
                            "7" => Some(("anti_flicker", ParamValue::Float(bounded(value, 0.0, 1.0)?))),
                            "8" | "9" | "10" | "11" if finite(value)? == 0.0 => None,
                            _ => {
                                warning(report, format!("clip {:?}: Motion parameter {} was not imported", ti.name, child_text(p, "Name").unwrap_or(id)))?;
                                None
                            }
                        };
                        if let Some((param, value)) = mapping {
                            set_param(ti, "motion", param, Param::new(value));
                        }
                    }
                }
                "Internal Volume Stereo" | "Internal Volume Mono" | "Internal Channel Volume Stereo" => {
                    let channel = name == "Internal Channel Volume Stereo";
                    for p in params {
                        match child_text(p, "Name").unwrap_or("") {
                            param @ ("Level" | "Left" | "Right") => {
                                if let Some(v) = static_value(p, report)? {
                                    let linear = bounded(v, 0.0, 1e6)?;
                                    let db = if linear == 0.0 { -287.5 } else { (20.0 * linear.log10()).max(-287.5) };
                                    let effect = if channel { "channel_volume" } else { "volume" };
                                    let param = match param {
                                        "Left" => "left",
                                        "Right" => "right",
                                        _ => "level",
                                    };
                                    set_param(ti, effect, param, Param::new(ParamValue::Float(db)));
                                }
                            }
                            "Bypass" => {
                                if let Some(v) = static_value(p, report)? {
                                    set_param(ti, if channel { "channel_volume" } else { "volume" }, "bypass", Param::new(ParamValue::Bool(parse_bool(v)?)));
                                }
                            }
                            "Mute" => {
                                if let Some(v) = static_value(p, report)?
                                    && parse_bool(v)?
                                {
                                    ti.enabled = false;
                                }
                            }
                            // Stereo channel-volume records reserve unnamed slots for channels
                            // outside their declared stereo layout. They have no active channel.
                            "" if channel => {}
                            _ => warning(report, format!("clip {:?}: unsupported Volume parameter", ti.name))?,
                        }
                    }
                }
                _ => warning(report, format!("clip {:?}: effect {name:?} was not imported", ti.name))?,
            }
            if component.is_some_and(|n| child(n, "IsBypassed").is_some()) {
                warning(report, format!("clip {:?}: effect bypass state needs checking", ti.name))?;
            }
        }
        Ok(())
    }

    fn transition(&self, node: Node<'a, 'i>, track: &mut Track, builder: &mut Builder, report: &mut Report) -> Result<()> {
        self.claim_object()?;
        let base = child(node, "TransitionTrackItem").ok_or_else(|| bad("unsupported transition item"))?;
        let timing = child(base, "TrackItem").ok_or_else(|| bad("transition lacks timing"))?;
        let start = ticks(timing, "Start", Some(Tick::ZERO))?;
        let end = ticks(timing, "End", None)?;
        if end <= start {
            return Err(bad("transition End must be after Start"));
        }
        // Premiere stores the cut's offset within the transition in ticks, not a fraction.
        let alignment = ticks(base, "Alignment", Some((end - start).mul_ratio(1, 2)))?;
        if alignment > end - start {
            return Err(bad("transition Alignment exceeds its duration"));
        }
        let cut = start + alignment;
        let from = if boolean(base, "HasOutgoingClip", true)? {
            track.items.iter().filter(|c| c.start < cut && c.end() >= cut).max_by_key(|c| c.start).map(|c| c.id)
        } else {
            None
        };
        let to = if boolean(base, "HasIncomingClip", true)? {
            track.items.iter().filter(|c| c.start <= cut && c.end() > cut && Some(c.id) != from).min_by_key(|c| c.start).map(|c| c.id)
        } else {
            None
        };
        if from.is_none() && to.is_none() {
            return Err(bad("transition has no adjacent clip"));
        }
        let name = child_text(base, "DisplayName").unwrap_or("Unknown transition");
        let match_name = child_text(base, "MatchName").unwrap_or(name);
        let audio = track.kind == TrackKind::Audio;
        let standard = matches!(match_name, "AE.ADBE Cross Dissolve" | "AE.ADBE Dip To Black" | "AE.ADBE Dip To White" | "Constant Power" | "Constant Gain");
        if !standard {
            warning(
                report,
                format!(
                    "transition {match_name:?} ({name}) substituted with {}; third-party appearance and settings are not preserved",
                    if audio { "Constant Power" } else { "Cross Dissolve" }
                ),
            )?;
        }
        let effect = transition_effect(
            if standard {
                name
            } else if audio {
                "constant power"
            } else {
                "cross dissolve"
            },
            audio,
            report,
        );
        if report.entries.len() > MAX_WARNINGS {
            return Err(bad("import warning budget exceeded"));
        }
        track.transitions.push(Transition {
            id: TransitionId(builder.alloc()),
            effect,
            start,
            duration: end - start,
            from,
            to,
            align: if alignment == Tick::ZERO {
                TransitionAlign::StartAtCut
            } else if alignment == end - start {
                TransitionAlign::EndAtCut
            } else {
                TransitionAlign::CenterAtCut
            },
            reverse: false,
        });
        Ok(())
    }
}

fn consume(left: &Cell<usize>, count: usize, what: &str) -> Result<()> {
    let next = left.get().checked_sub(count).ok_or_else(|| bad(format!("{what} budget exceeded")))?;
    left.set(next);
    Ok(())
}

fn ticks(n: Node<'_, '_>, field: &str, default: Option<Tick>) -> Result<Tick> {
    let Some(s) = child_text(n, field) else { return default.ok_or_else(|| bad(format!("missing {field}"))) };
    let t = s.parse::<i64>().map_err(|_| bad(format!("invalid integer ticks in {field}")))?;
    if !(0..=Tick::MAX.0).contains(&t) {
        return Err(bad(format!("{field} is outside supported time bounds")));
    }
    Ok(Tick(t))
}

fn frame_size(n: Node<'_, '_>) -> Result<(u32, u32)> {
    let s = child_text(n, "FrameRect").ok_or_else(|| bad("missing FrameRect"))?;
    let rect: Vec<i64> = s.split(',').map(|v| v.trim().parse::<i64>().map_err(|_| bad("invalid FrameRect"))).collect::<Result<_>>()?;
    let [left, top, right, bottom] = rect.as_slice() else { return Err(bad("FrameRect must have four coordinates")) };
    let width = right.checked_sub(*left).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| bad("invalid frame width"))?;
    let height = bottom.checked_sub(*top).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| bad("invalid frame height"))?;
    filmcraft_project::validate_frame_size(width, height).map_err(bad)?;
    Ok((width, height))
}

fn rate(n: Node<'_, '_>) -> Result<FrameRate> {
    let duration = ticks(n, "FrameRate", None)?.0;
    if !(TICKS_PER_SECOND / 1000..=TICKS_PER_SECOND).contains(&duration) {
        return Err(bad("frame duration must represent 1..1000 fps"));
    }
    Ok(FrameRate::new(TICKS_PER_SECOND, duration))
}

fn sample_rate(n: Node<'_, '_>) -> Result<u32> {
    let duration = ticks(n, "FrameRate", None)?.0;
    if duration == 0 || TICKS_PER_SECOND % duration != 0 {
        return Err(bad("invalid audio sample duration"));
    }
    let rate = TICKS_PER_SECOND / duration;
    if !(8000..=384000).contains(&rate) {
        return Err(bad("audio sample rate must be 8000..384000 Hz"));
    }
    u32::try_from(rate).map_err(|_| bad("invalid audio sample rate"))
}

fn finite(s: &str) -> Result<f64> {
    let f = s.parse::<f64>().map_err(|_| bad("invalid numeric parameter"))?;
    if !f.is_finite() {
        return Err(bad("numeric parameter must be finite"));
    }
    Ok(f)
}
fn bounded(s: &str, min: f64, max: f64) -> Result<f64> {
    let f = finite(s)?;
    if f < min || f > max {
        return Err(bad("effect parameter exceeds supported bounds"));
    }
    Ok(f)
}
fn number(n: Node<'_, '_>, field: &str, default: f64) -> Result<f64> {
    child_text(n, field).map(finite).transpose().map(|f| f.unwrap_or(default))
}
fn parse_bool(s: &str) -> Result<bool> {
    match s {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(bad("invalid boolean parameter")),
    }
}
fn boolean(n: Node<'_, '_>, field: &str, default: bool) -> Result<bool> {
    child_text(n, field).map(parse_bool).transpose().map(|v| v.unwrap_or(default))
}
fn static_value<'a>(n: Node<'a, '_>, report: &mut Report) -> Result<Option<&'a str>> {
    if child(n, "Keyframes").is_some() || child_text(n, "IsTimeVarying") == Some("true") {
        warning(
            report,
            format!("animated parameter {:?} imported at its initial value; keyframes are not transferred", child_text(n, "Name").unwrap_or("unnamed")),
        )?;
    }
    if let Some(k) = child_text(n, "StartKeyframe") {
        let v = k.split(',').nth(1).ok_or_else(|| bad("invalid StartKeyframe"))?;
        return Ok(Some(v));
    }
    Ok(child_text(n, "CurrentValue"))
}

fn reject_sequence_cycles(builder: &Builder) -> Result<()> {
    let mut degree: HashMap<ItemId, usize> = builder.top.iter().map(|&id| (id, 0)).collect();
    let mut parents: HashMap<ItemId, Vec<ItemId>> = HashMap::new();
    let mut depths: HashMap<ItemId, usize> = builder.top.iter().map(|&id| (id, 1)).collect();
    for &id in &builder.top {
        if let Some(seq) = builder.p.sequence(id) {
            for ti in seq.all_tracks().flat_map(|t| &t.items) {
                if builder.p.sequence(ti.item).is_some() {
                    let d = degree.get_mut(&id).ok_or_else(|| bad("missing sequence"))?;
                    *d = d.saturating_add(1);
                    parents.entry(ti.item).or_default().push(id);
                }
            }
        }
    }
    let mut queue: VecDeque<_> = degree.iter().filter(|(_, d)| **d == 0).map(|(&id, _)| id).collect();
    let mut visited = 0usize;
    while let Some(id) = queue.pop_front() {
        visited = visited.saturating_add(1);
        for p in parents.get(&id).into_iter().flatten() {
            let next_depth = depths.get(&id).copied().unwrap_or(1).saturating_add(1);
            if next_depth > 64 {
                return Err(bad("nested sequences exceed 64 levels"));
            }
            let parent_depth = depths.entry(*p).or_insert(1);
            *parent_depth = (*parent_depth).max(next_depth);
            let d = degree.get_mut(p).ok_or_else(|| bad("missing parent sequence"))?;
            *d = d.saturating_sub(1);
            if *d == 0 {
                queue.push_back(*p);
            }
        }
    }
    if visited != degree.len() {
        return Err(bad("cyclic nested sequences are unsupported"));
    }
    Ok(())
}
