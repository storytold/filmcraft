use std::collections::{HashMap, HashSet};

use filmcraft_media::MediaKind;
use filmcraft_project::{BinId, ItemId, SequenceSettings, TrackKind, Transition, TransitionAlign, TransitionId, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use roxmltree::Node;

use super::effects;
use super::graph::{Graph, PROJECT, at, decode, error, key, parse, text_at};
use crate::common::{Builder, MediaSpec, empty_sequence, kind_from_ext, rate_from_f64, resolve_path, settings_for};
use crate::xml::{child_text, elements};
use crate::{ImportOptions, Imported, Report, Result};

pub fn import_project(bytes: &[u8], opts: &ImportOptions) -> Result<(Imported, Report)> {
    let text = decode(bytes, PROJECT)?;
    let doc = parse(&text, PROJECT)?;
    let graph = Graph::new(doc.root_element(), PROJECT)?;
    let mut report = Report::default();
    let mut importer = Importer {
        graph,
        opts,
        builder: Builder::new(opts.name.as_deref().unwrap_or("Imported Premiere project")),
        sequences: HashMap::new(),
        leaves: Vec::new(),
        nesting: HashMap::new(),
        report: &mut report,
    };
    let project = elements(importer.graph.root)
        .find(|n| n.has_tag_name("Project") && n.attribute("ObjectID").is_some())
        .ok_or_else(|| error(PROJECT, "document has no Project object"))?;
    let root = importer.graph.reference(project, &["RootProjectItem"])?;
    importer.walk_bins(root, None, true, &mut HashSet::new(), 0)?;
    let mut sequence_bins = HashMap::new();
    for (leaf, bin) in &importer.leaves {
        let master = importer.graph.reference(*leaf, &["MasterClip"])?;
        if let Some(source) = importer.master_source(master)?
            && let Some(reference) = at(source, &["SequenceSource", "Sequence"])
        {
            sequence_bins.entry(key(importer.graph.resolve(reference)?)).or_insert(*bin);
        }
    }
    let definitions: Vec<_> = elements(importer.graph.root)
        .filter(|n| n.has_tag_name("Sequence") && (n.attribute("ObjectUID").is_some() || n.attribute("ObjectID").is_some()))
        .collect();
    if definitions.is_empty() {
        importer.report.info("Premiere project has no sequences; imported bins and media only")
    }
    for node in &definitions {
        let settings = importer.settings(*node)?;
        let name = child_text(*node, "Name").unwrap_or("Imported sequence");
        let bin = sequence_bins.get(&key(*node)).copied().flatten();
        let id = importer.builder.reserve_sequence(name, settings, bin);
        importer.builder.top.push(id);
        importer.sequences.insert(key(*node), id);
    }
    for (leaf, bin) in importer.leaves.clone() {
        let master = importer.graph.reference(leaf, &["MasterClip"])?;
        if let Some(source) = importer.master_source(master)? {
            let name = text_at(leaf, &["ProjectItem", "Name"]).or_else(|| child_text(master, "Name")).unwrap_or("Imported media");
            importer.source_item(source, name, bin)?;
        }
    }
    for node in definitions {
        importer.read_sequence(node)?;
    }
    let imported = importer.builder.finish();
    Ok((imported, report))
}

struct Importer<'a, 'i, 'o, 'r> {
    graph: Graph<'a, 'i>,
    opts: &'o ImportOptions,
    builder: Builder,
    sequences: HashMap<String, ItemId>,
    leaves: Vec<(Node<'a, 'i>, Option<BinId>)>,
    nesting: HashMap<ItemId, Vec<ItemId>>,
    report: &'r mut Report,
}

impl<'a, 'i> Importer<'a, 'i, '_, '_> {
    fn walk_bins(&mut self, node: Node<'a, 'i>, parent: Option<BinId>, root: bool, active: &mut HashSet<String>, depth: usize) -> Result<()> {
        if depth >= 64 || !active.insert(key(node)) {
            return Err(error(PROJECT, "cyclic or excessively deep project bins"));
        }
        let bin = if root { parent } else { Some(self.builder.bin(text_at(node, &["ProjectItem", "Name"]).unwrap_or("Imported bin"), parent)) };
        for item in self.graph.references(node, &["ProjectItemContainer", "Items"])? {
            if at(item, &["ProjectItemContainer"]).is_some() {
                self.walk_bins(item, bin, false, active, depth + 1)?;
            } else if item.has_tag_name("ClipProjectItem") {
                self.leaves.push((item, bin));
            } else {
                self.report.warn(format!("Premiere project item {} is not supported and was skipped", item.tag_name().name()));
            }
        }
        active.remove(&key(node));
        Ok(())
    }

    fn master_source(&self, master: Node<'a, 'i>) -> Result<Option<Node<'a, 'i>>> {
        for clip in self.graph.references(master, &["Clips"])? {
            if matches!(clip.tag_name().name(), "VideoClip" | "AudioClip") {
                return self.graph.reference(clip, &["Clip", "Source"]).map(Some);
            }
        }
        Ok(None)
    }

    fn groups(&self, sequence: Node<'a, 'i>) -> Result<Vec<Node<'a, 'i>>> {
        at(sequence, &["TrackGroups"]).into_iter().flat_map(elements).map(|n| self.graph.reference(n, &["Second"])).collect()
    }

    fn settings(&mut self, sequence: Node<'a, 'i>) -> Result<SequenceSettings> {
        let mut settings = settings_for(FrameRate::FPS_24, 1920, 1080, false);
        for group in self.groups(sequence)? {
            if group.has_tag_name("VideoTrackGroup") {
                let (width, height) = frame_size(group, (1920, 1080))?;
                settings.width = width;
                settings.height = height;
                let (frame_rate, approximate) = rate(self.graph.required_integer(group, &["TrackGroup", "FrameRate"])?)?;
                settings.frame_rate = frame_rate;
                if approximate {
                    self.report.info("Premiere sequence frame duration was rounded in the file; the frame rate uses a bounded rational approximation")
                }
            } else if group.has_tag_name("AudioTrackGroup") {
                settings.sample_rate = sample_rate(self.graph.required_integer(group, &["TrackGroup", "FrameRate"])?)?;
            }
        }
        settings.validate().map_err(|e| error(PROJECT, e))?;
        settings.preset = format!("Premiere {}x{} {}", settings.width, settings.height, settings.frame_rate.label());
        Ok(settings)
    }

    fn source_item(&mut self, source: Node<'a, 'i>, name: &str, bin: Option<BinId>) -> Result<ItemId> {
        if let Some(reference) = at(source, &["SequenceSource", "Sequence"]) {
            let sequence = self.graph.resolve(reference)?;
            return self.sequences.get(&key(sequence)).copied().ok_or_else(|| error(PROJECT, "missing nested sequence definition"));
        }
        let native_key = key(source);
        if let Some(id) = self.builder.find_media(&native_key) {
            return Ok(id);
        }
        let Some(reference) = at(source, &["MediaSource", "Media"]) else {
            return Ok(self.offline_source(&native_key, name, source.tag_name().name(), bin));
        };
        let media = self.graph.resolve(reference)?;
        let media_key = key(media);
        if let Some(id) = self.builder.find_media(&media_key) {
            return Ok(id);
        }
        let path = ["ActualMediaFilePath", "FilePath", "RelativePath"].into_iter().filter_map(|p| child_text(media, p)).find(|p| !p.is_empty());
        let Some(path) = path else { return Ok(self.offline_source(&media_key, name, "generated or missing media", bin)) };
        if path.contains('\0') {
            return Err(error(PROJECT, "media path contains a NUL character"));
        }
        let path = resolve_path(path, self.opts.base_dir.as_deref());
        let mut spec = MediaSpec { kind: Some(kind_from_ext(&path)), ..Default::default() };
        if let Some(video) = at(media, &["VideoStream"]) {
            let video = self.graph.resolve(video)?;
            let size = frame_size(video, (1920, 1080))?;
            let (frame_rate, approximate) = if spec.kind == Some(MediaKind::Still) {
                (FrameRate::FPS_24, false)
            } else {
                rate(self.graph.integer(video, &["FrameRate"], TICKS_PER_SECOND / 24)?)?
            };
            if approximate {
                self.report.info("Premiere media frame duration was rounded in the file; the frame rate uses a bounded rational approximation")
            }
            spec.video = Some((size.0, size.1, frame_rate));
            let duration = self.graph.integer(video, &["Duration"], 0)?.max(0);
            if spec.kind != Some(MediaKind::Still) && duration > super::MAX_TIME_TICKS {
                return Err(error(PROJECT, "media duration is out of range"));
            }
            spec.duration = Some(Tick(if spec.kind == Some(MediaKind::Still) { 0 } else { duration }));
        }
        if let Some(audio) = at(media, &["AudioStream"]) {
            let audio = self.graph.resolve(audio)?;
            let sr = sample_rate(self.graph.integer(audio, &["FrameRate"], TICKS_PER_SECOND / 48_000)?)?;
            let channels = child_text(audio, "AudioChannelLayout")
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                .and_then(|v| v.as_array().map(|a| a.len()))
                .unwrap_or(2)
                .clamp(1, 64) as u32;
            spec.audio = Some((sr, channels));
            let duration = Tick(self.graph.integer(audio, &["Duration"], 0)?.max(0));
            if duration.0 > super::MAX_TIME_TICKS {
                return Err(error(PROJECT, "audio duration is out of range"));
            }
            spec.duration = Some(spec.duration.unwrap_or(Tick::ZERO).max(duration));
        }
        let id = self.builder.file_media(&media_key, name, &path, &spec, bin);
        if let Some(media) = self.builder.p.item_mut(id).and_then(|i| i.as_media_mut()) {
            media.offline = true
        }
        Ok(id)
    }

    fn offline_source(&mut self, id: &str, name: &str, kind: &str, bin: Option<BinId>) -> ItemId {
        self.report.warn(format!("Premiere source \"{name}\" ({kind}) was preserved as an offline placeholder"));
        let spec = MediaSpec { kind: Some(MediaKind::Movie), ..Default::default() };
        let id = self.builder.file_media(id, name, "", &spec, bin);
        if let Some(media) = self.builder.p.item_mut(id).and_then(|i| i.as_media_mut()) {
            media.offline = true
        }
        id
    }

    fn read_sequence(&mut self, node: Node<'a, 'i>) -> Result<()> {
        let id = self.sequences.get(&key(node)).copied().ok_or_else(|| error(PROJECT, "sequence was not reserved"))?;
        let settings = self.builder.p.sequence(id).map(|s| s.settings.clone()).ok_or_else(|| error(PROJECT, "sequence is missing"))?;
        let mut sequence = empty_sequence(settings);
        let mut clip_ids = HashMap::new();
        for group in self.groups(node)? {
            let kind = match group.tag_name().name() {
                "VideoTrackGroup" => TrackKind::Video,
                "AudioTrackGroup" => TrackKind::Audio,
                other => {
                    if !self.graph.references(group, &["TrackGroup", "Tracks"])?.is_empty() {
                        self.report.warn(format!("Premiere {other} tracks are not supported and were skipped"));
                    }
                    continue;
                }
            };
            let tracks = self.graph.references(group, &["TrackGroup", "Tracks"])?;
            if tracks.len() > 1024 {
                return Err(error(PROJECT, "too many tracks in a sequence"));
            }
            self.builder.ensure_tracks(&mut sequence, kind, tracks.len());
            for (index, track) in tracks.into_iter().enumerate() {
                let mut imported_track = self.builder.track(kind, index);
                imported_track.locked = self.graph.boolean(track, &["ClipTrack", "Track", "IsLocked"], false)?;
                imported_track.sync_lock = self.graph.boolean(track, &["ClipTrack", "Track", "IsSyncLocked"], true)?;
                imported_track.muted = self.graph.boolean(track, &["ClipTrack", "Track", "IsMuted"], false)?;
                imported_track.enabled = !imported_track.muted;
                for item in self.graph.references(track, &["ClipTrack", "ClipItems", "TrackItems"])? {
                    let start = self.graph.required_integer(item, &["ClipTrackItem", "TrackItem", "Start"])?;
                    let end = self.graph.required_integer(item, &["ClipTrackItem", "TrackItem", "End"])?;
                    let duration = end.checked_sub(start).filter(|d| start >= 0 && *d > 0).ok_or_else(|| error(PROJECT, "invalid clip timeline range"))?;
                    if end > super::MAX_TIME_TICKS {
                        return Err(error(PROJECT, "clip timeline time is out of range"));
                    }
                    let sub = self.graph.reference(item, &["ClipTrackItem", "SubClip"])?;
                    let clip = self.graph.reference(sub, &["Clip"])?;
                    let source = self.graph.reference(clip, &["Clip", "Source"])?;
                    let name = child_text(sub, "Name").unwrap_or("Imported clip");
                    let source_item = self.source_item(source, name, None)?;
                    if self.builder.p.sequence(source_item).is_some() {
                        if cyclic(id, source_item, &self.nesting, &self.graph)? {
                            return Err(error(PROJECT, "cyclic nested sequences"));
                        }
                        self.nesting.entry(id).or_default().push(source_item);
                    }
                    let source_in = self.graph.integer(clip, &["Clip", "InPoint"], 0)?;
                    let source_out = self.graph.integer(
                        clip,
                        &["Clip", "OutPoint"],
                        source_in.checked_add(duration).ok_or_else(|| error(PROJECT, "clip source range overflows"))?,
                    )?;
                    let still = self.builder.p.item(source_item).and_then(|i| i.as_media()).is_some_and(|m| m.info.kind == MediaKind::Still);
                    if !(0..=super::MAX_TIME_TICKS).contains(&source_in) || source_out < source_in || (!still && source_out > super::MAX_TIME_TICKS) {
                        return Err(error(PROJECT, "invalid clip source range"));
                    }
                    let mut imported = self.builder.clip(source_item, kind, name, Tick(start), Tick(duration), Tick(source_in));
                    if !still && source_out != source_in {
                        imported.speed = (source_out - source_in) as f64 / duration as f64;
                        if !(0.0001..=10_000.0).contains(&imported.speed) {
                            return Err(error(PROJECT, "clip speed is out of range"));
                        }
                    }
                    let sequence_size = (sequence.settings.width, sequence.settings.height);
                    let source_size = self
                        .builder
                        .p
                        .item(source_item)
                        .and_then(|i| i.as_media())
                        .and_then(|m| m.info.video.as_ref())
                        .map(|v| (v.width, v.height))
                        .or_else(|| self.builder.p.sequence(source_item).map(|s| (s.settings.width, s.settings.height)))
                        .unwrap_or(sequence_size);
                    if let Some(reference) = at(item, &["ClipTrackItem", "ComponentOwner", "Components"]) {
                        let chain = self.graph.resolve(reference)?;
                        for component in self.graph.references(chain, &["ComponentChain", "Components"])? {
                            if let Some(effect) = effects::component(&self.graph, component, Tick::ZERO, sequence_size, source_size, self.report)? {
                                effects::put_effect(&mut imported.effects, effect);
                            }
                        }
                    }
                    clip_ids.insert(key(item), imported.id);
                    imported_track.items.push(imported);
                }
                imported_track.sort();
                if imported_track.items.windows(2).any(|pair| pair[0].end() > pair[1].start) {
                    return Err(error(PROJECT, "overlapping clips on the same native track cannot be imported safely"));
                }
                let clip_ends = imported_track.items.iter().map(|clip| (clip.end().0, clip.id)).collect();
                let clip_starts = imported_track.items.iter().map(|clip| (clip.start.0, clip.id)).collect();
                for transition in self.graph.references(track, &["ClipTrack", "TransitionItems", "TrackItems"])? {
                    if let Some(transition) = self.transition(transition, &clip_ends, &clip_starts)? {
                        imported_track.transitions.push(transition)
                    }
                }
                if kind == TrackKind::Audio && at(track, &["AudioTrack", "ComponentOwner"]).is_some() {
                    self.report.info("Premiere Track Mixer automation and inserts are not imported; clip audio levels are retained");
                }
                let target = sequence.tracks_mut(kind).get_mut(index).ok_or_else(|| error(PROJECT, "track index is missing"))?;
                *target = imported_track;
            }
        }
        for link in self.graph.references(node, &["PersistentGroupContainer", "LinkContainer", "Links"])? {
            let ids: HashSet<_> =
                self.graph.references(link, &["TrackItemGroup", "TrackItems"])?.into_iter().filter_map(|item| clip_ids.get(&key(item)).copied()).collect();
            if ids.len() > 1 {
                let link_id = self.builder.link_id();
                for track in sequence.all_tracks_mut() {
                    for clip in &mut track.items {
                        if ids.contains(&clip.id) {
                            clip.link = Some(link_id)
                        }
                    }
                }
            }
        }
        if at(node, &["Node", "Properties"]).is_some_and(|p| elements(p).any(|n| n.tag_name().name().contains("Color") && n.text().is_some())) {
            self.report.info("Premiere sequence colour-management settings use the FilmCraft sequence defaults");
        }
        self.builder.put_sequence(id, sequence);
        Ok(())
    }

    fn transition(
        &mut self,
        node: Node<'a, 'i>,
        clip_ends: &HashMap<i64, filmcraft_project::ClipId>,
        clip_starts: &HashMap<i64, filmcraft_project::ClipId>,
    ) -> Result<Option<Transition>> {
        let name = text_at(node, &["TransitionTrackItem", "MatchName"]).unwrap_or("");
        let effect = match name {
            "AE.ADBE Cross Dissolve New" | "Cross Dissolve" => "cross_dissolve",
            "Constant Power" => "constant_power",
            "Constant Gain" => "constant_gain",
            "Exponential Fade" => "exponential_fade",
            _ => {
                self.report.warn(format!("Premiere transition \"{name}\" is not supported and was skipped"));
                return Ok(None);
            }
        };
        let Some(effect) = find_effect(effect).map(|d| d.instance()) else { return Ok(None) };
        let start = self.graph.required_integer(node, &["TransitionTrackItem", "TrackItem", "Start"])?;
        let end = self.graph.required_integer(node, &["TransitionTrackItem", "TrackItem", "End"])?;
        let duration = end.checked_sub(start).filter(|d| start >= 0 && *d > 0).ok_or_else(|| error(PROJECT, "invalid transition range"))?;
        if end > super::MAX_TIME_TICKS {
            return Err(error(PROJECT, "transition time is out of range"));
        }
        let offset = self.graph.integer(node, &["TransitionTrackItem", "Alignment"], duration / 2)?;
        let cut = start.checked_add(offset).filter(|_| (0..=duration).contains(&offset)).ok_or_else(|| error(PROJECT, "invalid transition alignment"))?;
        let from = self.graph.boolean(node, &["TransitionTrackItem", "HasOutgoingClip"], true)?.then(|| clip_ends.get(&cut).copied()).flatten();
        let to = self.graph.boolean(node, &["TransitionTrackItem", "HasIncomingClip"], true)?.then(|| clip_starts.get(&cut).copied()).flatten();
        if from.is_none() && to.is_none() {
            self.report.warn(format!("Premiere transition \"{name}\" has no matching adjacent clip and was skipped"));
            return Ok(None);
        }
        Ok(Some(Transition {
            id: TransitionId(self.builder.alloc()),
            effect,
            start: Tick(start),
            duration: Tick(duration),
            from,
            to,
            align: if offset == 0 {
                TransitionAlign::StartAtCut
            } else if offset == duration {
                TransitionAlign::EndAtCut
            } else {
                TransitionAlign::CenterAtCut
            },
            reverse: false,
        }))
    }
}

fn rate(period: i64) -> Result<(FrameRate, bool)> {
    if period <= 0 {
        return Err(error(PROJECT, "frame duration must be positive"));
    }
    let (mut a, mut b) = (TICKS_PER_SECOND, period);
    while b != 0 {
        (a, b) = (b, a % b)
    }
    let rate = FrameRate::new(TICKS_PER_SECOND / a, period / a);
    if rate.as_f64() > 1000.0 || period > super::MAX_TIME_TICKS {
        return Err(error(PROJECT, "frame rate is out of range"));
    }
    let approximate = rate.num > i64::from(u32::MAX) || rate.den > i64::from(u32::MAX);
    let rate = if approximate { rate_from_f64(rate.as_f64()) } else { rate };
    if rate.num <= 0 || rate.num > i64::from(u32::MAX) || rate.den <= 0 || rate.den > i64::from(u32::MAX) {
        return Err(error(PROJECT, "frame rate is out of range"));
    }
    Ok((rate, approximate))
}

fn sample_rate(period: i64) -> Result<u32> {
    if period <= 0 || TICKS_PER_SECOND % period != 0 {
        return Err(error(PROJECT, "invalid audio sample duration"));
    }
    u32::try_from(TICKS_PER_SECOND / period).ok().filter(|n| (1..=384_000).contains(n)).ok_or_else(|| error(PROJECT, "sample rate is out of range"))
}

fn frame_size(node: Node<'_, '_>, default: (u32, u32)) -> Result<(u32, u32)> {
    let Some(rect) = child_text(node, "FrameRect") else { return Ok(default) };
    let values: Vec<i64> =
        rect.split(',').take(5).map(|v| v.trim().parse::<i64>()).collect::<std::result::Result<_, _>>().map_err(|e| error(PROJECT, e.to_string()))?;
    let [left, top, right, bottom] = values.as_slice() else { return Err(error(PROJECT, "invalid frame rectangle")) };
    let width = right.checked_sub(*left).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| error(PROJECT, "invalid frame width"))?;
    let height = bottom.checked_sub(*top).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| error(PROJECT, "invalid frame height"))?;
    filmcraft_project::validate_frame_size(width, height).map_err(|e| error(PROJECT, e))?;
    Ok((width, height))
}

fn cyclic(from: ItemId, to: ItemId, edges: &HashMap<ItemId, Vec<ItemId>>, graph: &Graph<'_, '_>) -> Result<bool> {
    let mut pending = vec![to];
    let mut seen = HashSet::new();
    while let Some(next) = pending.pop() {
        graph.charge()?;
        if next == from {
            return Ok(true);
        }
        if seen.insert(next)
            && let Some(children) = edges.get(&next)
        {
            pending.extend(children)
        }
    }
    Ok(false)
}
