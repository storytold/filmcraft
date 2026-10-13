//! this is the importer for premiere pro project files (.prproj)
//! so you can take a project made in premiere and open it in filmcraft
//! i worked out the format from project files premiere saved, i never looked inside premiere itself
//!
//! what this brings over is the bins, the media files, the sequences and their tracks,
//! the clips with their in points speed reverse and gain, linked audio and video, nested sequences
//! and the track mixer mute and fader volume
//! for media it tries the path premiere saved first and if thats missing it uses the relative one
//!
//! for effects i match the premiere effect to the filmcraft one by name since filmcraft copies
//! premiere's effects pretty closely, and the settings get matched by name too
//! theres a small alias list for the few settings premiere doesn't name or names differently
//! and the values get converted where the units are different like volume and position
//! keyframes come over too, premiere saves them in the source clips time and filmcraft uses
//! that same time so they come over as is, and transitions get hooked up to the clips on each side of the cut
//!
//! some stuff cant come over yet like titles and shapes, and masks

use std::collections::{HashMap, HashSet};

use filmcraft_geom::Vec2;
use filmcraft_media::{MediaKind, inflate};
use filmcraft_project::effect::{ParamDef, ParamKind, find_effect_by_name};
use filmcraft_project::{
    BinId, ClipId, EffectKind, Interpolation, ItemId, Keyframe, Param, ParamValue, Sequence, TrackItem, TrackKind, Transition, TransitionAlign, TransitionId,
    graphic,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use roxmltree::{Node, NodeId};

use crate::common::{Builder, MediaSpec, empty_sequence, resolve_path, settings_for, transition_effect};
use crate::xml::{child, child_bool, child_f64, child_i64, child_text, children, path_node, path_text};
use crate::{Error, Format, ImportOptions, Imported, Report, Result};

const MAX_XML: usize = 512 << 20;
const MAX_BIN_DEPTH: usize = 64;
const MAX_TRACKS: usize = 256;
const ALIAS: &[(&str, &str, &str)] = &[
    ("motion", "4", "Uniform Scale"),
    ("crop", "5", "Zoom"),
    ("gaussian_blur", "3", "Repeat Edge Pixels"),
    ("transform", "3", "Scale Height"),
    ("transform", "4", "Scale Width"),
    ("levels", "(RGB) Black Input Level", "(RGB) Input Black Level"),
    ("levels", "(RGB) White Input Level", "(RGB) Input White Level"),
    ("levels", "(RGB) Black Output Level", "(RGB) Output Black Level"),
    ("levels", "(RGB) White Output Level", "(RGB) Output White Level"),
    ("studio_reverb", "Damping", "High Frequency Damping"),
    ("studio_reverb", "Dry Output Level", "Dry"),
    ("studio_reverb", "Wet Output Level", "Wet"),
];
const NAMES: &[(&str, &str)] = &[("7ea99d31-ca8f-454f-acd7-81a258efb336", "Studio Reverb")];

type Edge = (ClipId, Tick);

pub(crate) fn sniff(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x1f, 0x8b])
}

pub(crate) fn import(bytes: &[u8], opts: &ImportOptions, report: &mut Report) -> Result<Imported> {
    let xml = inflate::gzip(bytes, MAX_XML).map_err(|e| Error::parse(Format::Prproj, e))?;
    let text = std::str::from_utf8(&xml).map_err(|_| Error::parse(Format::Prproj, "the project is not UTF-8 text"))?;
    let doc = crate::xml::parse(text).map_err(|e| Error::parse(Format::Prproj, e))?;
    let root = doc.root_element();
    if !root.has_tag_name("PremiereData") {
        return Err(Error::parse(Format::Prproj, "root element is not <PremiereData>"));
    }
    let objects = Objects::new(root);
    let top_bin = objects.first("RootProjectItem").ok_or_else(|| Error::parse(Format::Prproj, "the project has no root bin"))?;
    let mut imp = Imp {
        objects,
        b: Builder::new(opts.name.as_deref().unwrap_or("Premiere Pro project")),
        opts,
        report,
        sources: HashMap::new(),
        sequences: Vec::new(),
        nests: HashMap::new(),
    };
    imp.read_bin(top_bin, None, 0);
    imp.read_sequences();
    if imp.b.p.items.is_empty() {
        return Err(Error::Empty);
    }
    Ok(imp.b.finish())
}

struct Objects<'a, 'i> {
    ids: HashMap<&'a str, Node<'a, 'i>>,
    uids: HashMap<&'a str, Node<'a, 'i>>,
}

impl<'a, 'i> Objects<'a, 'i> {
    fn new(root: Node<'a, 'i>) -> Self {
        let mut ids = HashMap::new();
        let mut uids = HashMap::new();
        for n in root.children().filter(Node::is_element) {
            if let Some(id) = n.attribute("ObjectID") {
                ids.insert(id, n);
            } else if let Some(uid) = n.attribute("ObjectUID") {
                uids.insert(uid, n);
            }
        }
        Self { ids, uids }
    }

    fn first(&self, tag: &str) -> Option<Node<'a, 'i>> {
        self.uids.values().chain(self.ids.values()).copied().find(|n| n.has_tag_name(tag))
    }

    fn target(&self, reference: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
        match reference.attribute("ObjectRef") {
            Some(id) => self.ids.get(id).copied(),
            None => self.uids.get(reference.attribute("ObjectURef")?).copied(),
        }
    }

    fn at(&self, n: Node<'a, 'i>, path: &[&str]) -> Option<Node<'a, 'i>> {
        self.target(path_node(n, path)?)
    }

    fn list(&self, n: Node<'a, 'i>, path: &[&str], tag: &'static str) -> Vec<Node<'a, 'i>> {
        path_node(n, path).map(|list| children(list, tag).filter_map(|r| self.target(r)).collect()).unwrap_or_default()
    }

    fn track_groups(&self, seq: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
        child(seq, "TrackGroups").map(|list| children(list, "TrackGroup").filter_map(|g| self.at(g, &["Second"])).collect()).unwrap_or_default()
    }
}

struct Imp<'a, 'i, 'r> {
    objects: Objects<'a, 'i>,
    b: Builder,
    opts: &'a ImportOptions,
    report: &'r mut Report,
    sources: HashMap<NodeId, ItemId>,
    sequences: Vec<(Node<'a, 'i>, ItemId)>,
    nests: HashMap<ItemId, HashSet<ItemId>>,
}

impl<'a, 'i> Imp<'a, 'i, '_> {
    fn read_bin(&mut self, bin_item: Node<'a, 'i>, bin: Option<BinId>, depth: usize) {
        if depth > MAX_BIN_DEPTH {
            self.report.warn("bins nested too deeply were skipped");
            return;
        }
        for item in self.objects.list(bin_item, &["ProjectItemContainer", "Items"], "Item") {
            let name = path_text(item, &["ProjectItem", "Name"]);
            match item.tag_name().name() {
                "BinProjectItem" => {
                    let sub = self.b.bin(name.unwrap_or("Bin"), bin);
                    self.read_bin(item, Some(sub), depth + 1);
                }
                "ClipProjectItem" => {
                    if let Some(master) = self.objects.at(item, &["MasterClip"]) {
                        self.master_clip(master, name, bin);
                    }
                }
                other => self.report.warn(format!("project item type \"{other}\" is not supported")),
            }
        }
    }

    fn master_clip(&mut self, master: Node<'a, 'i>, name: Option<&str>, bin: Option<BinId>) -> Option<ItemId> {
        if let Some(&id) = self.sources.get(&master.id()) {
            return Some(id);
        }
        let name = name.or_else(|| child_text(master, "Name")).filter(|n| !n.is_empty()).unwrap_or("Untitled");
        let source = self.objects.list(master, &["Clips"], "Clip").into_iter().find_map(|c| self.objects.at(c, &["Clip", "Source"]));
        let Some(source) = source else {
            self.report.warn(format!("\"{name}\" has no source; skipped"));
            return None;
        };
        let id = match source.tag_name().name() {
            "VideoMediaSource" | "AudioMediaSource" => self.media(self.objects.at(source, &["MediaSource", "Media"])?, name, bin),
            "VideoSequenceSource" | "AudioSequenceSource" => self.sequence(self.objects.at(source, &["SequenceSource", "Sequence"])?, name, bin),
            other => {
                self.report.warn(format!("\"{name}\" has an unsupported source type \"{other}\"; skipped"));
                return None;
            }
        };
        self.sources.insert(master.id(), id);
        Some(id)
    }

    fn media(&mut self, media: Node<'a, 'i>, name: &str, bin: Option<BinId>) -> ItemId {
        let video = self.objects.at(media, &["VideoStream"]);
        let audio = self.objects.at(media, &["AudioStream"]);
        let still = video.and_then(|v| child_bool(v, "IsStill")).unwrap_or(false);
        let rate = video.and_then(|v| child_i64(v, "FrameRate")).and_then(frame_rate).unwrap_or_default();
        let duration = [video, audio].into_iter().flatten().filter_map(|s| child_i64(s, "Duration")).filter(|&d| d > 0).max().map(Tick);
        let spec = MediaSpec {
            duration: duration.filter(|_| !still),
            video: video.and_then(|v| child_text(v, "FrameRect")).and_then(frame_size).map(|(w, h)| (w, h, rate)),
            audio: audio.map(|a| (sample_rate(a).unwrap_or(48_000), channel_count(a))),
            start_tc: None,
            kind: still.then_some(MediaKind::Still),
        };
        let (path, alt) = self.media_path(media);
        let id = self.b.file_media(&format!("file:{path}"), name, &path, &spec, bin);
        if let Some(alt) = alt {
            self.b.alternates.push((id, alt));
        }
        if path.is_empty() {
            self.report.warn(format!("\"{name}\" has no file path; imported offline"));
            if let Some(m) = self.b.p.item_mut(id).and_then(|i| i.as_media_mut()) {
                m.offline = true;
            }
        }
        id
    }

    fn media_path(&self, media: Node) -> (String, Option<String>) {
        let base = self.opts.base_dir.as_deref().filter(|b| !b.is_empty());
        let saved = |tag| child_text(media, tag).filter(|p| !p.is_empty()).map(|p| resolve_path(p, base));
        let rel = saved("RelativePath").filter(|_| base.is_some());
        match saved("FilePath").or_else(|| saved("ActualMediaFilePath")) {
            Some(abs) => {
                let alt = rel.filter(|r| *r != abs);
                (abs, alt)
            }
            None => (rel.unwrap_or_default(), None),
        }
    }

    fn sequence(&mut self, seq: Node<'a, 'i>, name: &str, bin: Option<BinId>) -> ItemId {
        if let Some(&id) = self.sources.get(&seq.id()) {
            return id;
        }
        let groups = self.objects.track_groups(seq);
        let group = |tag| groups.iter().copied().find(|g| g.has_tag_name(tag));
        let video = group("VideoTrackGroup");
        let rate = video.and_then(|v| path_text(v, &["TrackGroup", "FrameRate"])).and_then(|t| t.parse().ok()).and_then(frame_rate).unwrap_or_default();
        let (w, h) = video.and_then(|v| child_text(v, "FrameRect")).and_then(frame_size).unwrap_or((1920, 1080));
        let mut settings = settings_for(rate, w, h, false);
        if let Some(sr) = group("AudioTrackGroup").and_then(|a| path_node(a, &["TrackGroup"])).and_then(sample_rate) {
            settings.sample_rate = sr;
        }
        let id = self.b.reserve_sequence(name, settings, bin);
        self.b.top.push(id);
        self.sources.insert(seq.id(), id);
        self.sequences.push((seq, id));
        id
    }

    fn read_sequences(&mut self) {
        let mut next = 0;
        while let Some(&(seq, id)) = self.sequences.get(next) {
            self.read_sequence(seq, id);
            next += 1;
        }
    }

    fn read_sequence(&mut self, node: Node<'a, 'i>, id: ItemId) {
        let Some(settings) = self.b.p.sequence(id).map(|s| s.settings.clone()) else { return };
        let mut seq = empty_sequence(settings);
        let mut clips: HashMap<NodeId, ClipId> = HashMap::new();
        for group in self.objects.track_groups(node) {
            let kind = match group.tag_name().name() {
                "VideoTrackGroup" => TrackKind::Video,
                "AudioTrackGroup" => TrackKind::Audio,
                _ => continue,
            };
            for track in self.objects.list(group, &["TrackGroup", "Tracks"], "Track") {
                self.read_track(&mut seq, id, kind, track, &mut clips);
            }
        }
        for kind in [TrackKind::Video, TrackKind::Audio] {
            if seq.tracks(kind).is_empty() {
                self.b.ensure_tracks(&mut seq, kind, 1);
            }
        }
        self.read_links(node, &mut seq, &clips);
        self.b.put_sequence(id, seq);
    }

    fn read_track(&mut self, seq: &mut Sequence, seq_id: ItemId, kind: TrackKind, node: Node<'a, 'i>, clips: &mut HashMap<NodeId, ClipId>) {
        let header = path_node(node, &["ClipTrack", "Track"]);
        let index = header.and_then(|t| child_i64(t, "Index")).and_then(|i| usize::try_from(i).ok()).unwrap_or(seq.tracks(kind).len());
        if index >= MAX_TRACKS {
            self.report.warn(format!("tracks after the first {MAX_TRACKS} were skipped"));
            return;
        }
        self.b.ensure_tracks(seq, kind, index + 1);
        let fader = self.fader(node);
        if let (Some(track), Some(header)) = (seq.tracks_mut(kind).get_mut(index), header) {
            let muted = child_bool(header, "IsMuted").unwrap_or(false);
            track.locked = child_bool(header, "IsLocked").unwrap_or(false);
            match kind {
                TrackKind::Video => track.enabled = !muted,
                TrackKind::Audio => track.muted = muted,
            }
            for (name, v) in fader {
                match name {
                    "Mute" => track.muted |= v == "true",
                    "Volume" => {
                        if let Some(g) = v.parse::<f64>().ok().filter(|g| g.is_finite() && *g > 0.0) {
                            track.volume_db = (20.0 * g.log10()).clamp(-96.0, 15.0);
                        }
                    }
                    _ => {}
                }
            }
        }
        let frame = (seq.settings.width, seq.settings.height);
        let (mut heads, mut tails) = (HashMap::new(), HashMap::new());
        for item in self.objects.list(node, &["ClipTrack", "ClipItems", "TrackItems"], "TrackItem") {
            let Some(clip) = self.clip(item, kind, seq_id, frame) else { continue };
            clips.insert(item.id(), clip.id);
            if let Some(t) = self.objects.at(item, &["ClipTrackItem", "HeadTransition"]) {
                heads.insert(t.id(), (clip.id, clip.start));
            }
            if let Some(t) = self.objects.at(item, &["ClipTrackItem", "TailTransition"]) {
                tails.insert(t.id(), (clip.id, clip.end()));
            }
            if self.b.place(seq, kind, index, clip) != index {
                self.report.warn("overlapping clips on one track; the later one was moved to a new track");
            }
        }
        for t in self.objects.list(node, &["ClipTrack", "TransitionItems", "TrackItems"], "TrackItem") {
            self.transition(seq, kind, index, t, tails.get(&t.id()).copied(), heads.get(&t.id()).copied());
        }
    }

    fn fader(&self, track: Node<'a, 'i>) -> Vec<(&'a str, &'a str)> {
        let Some(chain) = self.objects.at(track, &["AudioTrack", "ComponentOwner", "Components"]) else { return Vec::new() };
        self.objects
            .list(chain, &["ComponentChain", "Components"], "Component")
            .into_iter()
            .filter(|c| c.has_tag_name("AudioFader"))
            .flat_map(|c| self.objects.list(c, &["AudioComponent", "Component", "Params"], "Param"))
            .filter_map(|p| Some((child_text(p, "Name")?, start(p)?)))
            .collect()
    }

    fn transition(&mut self, seq: &mut Sequence, kind: TrackKind, index: usize, node: Node, from: Option<Edge>, to: Option<Edge>) {
        let name = path_text(node, &["TransitionTrackItem", "DisplayName"]).unwrap_or("Cross Dissolve");
        let range = path_node(node, &["TransitionTrackItem", "TrackItem"]);
        let start = range.and_then(|r| child_i64(r, "Start")).unwrap_or(-1);
        let end = range.and_then(|r| child_i64(r, "End")).unwrap_or(-1);
        let (Some(dur), true, true) = (end.checked_sub(start).filter(|&d| d > 0), start >= 0, from.is_some() || to.is_some()) else {
            self.report.warn(format!("transition \"{name}\" has no valid position or clip; skipped"));
            return;
        };
        let (start, end) = (Tick(start), Tick(end));
        let cut = from.or(to).map_or(start, |c| c.1);
        let align = if cut <= start {
            TransitionAlign::StartAtCut
        } else if cut >= end {
            TransitionAlign::EndAtCut
        } else {
            TransitionAlign::CenterAtCut
        };
        let effect = transition_effect(name, kind == TrackKind::Audio, self.report);
        let id = TransitionId(self.b.alloc());
        if let Some(t) = seq.tracks_mut(kind).get_mut(index) {
            t.transitions.push(Transition { id, effect, start, duration: Tick(dur), from: from.map(|c| c.0), to: to.map(|c| c.0), align, reverse: false });
        }
    }

    fn clip(&mut self, item: Node<'a, 'i>, kind: TrackKind, seq_id: ItemId, frame: (u32, u32)) -> Option<TrackItem> {
        let sub = self.objects.at(item, &["ClipTrackItem", "SubClip"])?;
        let name = child_text(sub, "Name").filter(|n| !n.is_empty()).unwrap_or("Clip");
        let source = self.objects.at(sub, &["MasterClip"]).and_then(|m| self.master_clip(m, None, None));
        let Some(source) = source else {
            self.report.warn(format!("clip \"{name}\" has no source; skipped"));
            return None;
        };
        if !self.can_nest(seq_id, source) {
            self.report.warn(format!("clip \"{name}\" would put a sequence inside itself; skipped"));
            return None;
        }
        let range = path_node(item, &["ClipTrackItem", "TrackItem"]);
        let start = range.and_then(|r| child_i64(r, "Start")).unwrap_or(0);
        let end = range.and_then(|r| child_i64(r, "End")).unwrap_or(0);
        let duration = end.checked_sub(start).filter(|&d| d > 0);
        let (Some(duration), true) = (duration, start >= 0) else {
            self.report.warn(format!("clip \"{name}\" has no valid position; skipped"));
            return None;
        };
        let use_clip = self.objects.at(sub, &["Clip"]);
        let fields = use_clip.and_then(|c| child(c, "Clip"));
        let source_in = fields.and_then(|f| child_i64(f, "InPoint")).unwrap_or(0).max(0);
        let mut speed = fields.and_then(|f| child_f64(f, "PlaybackSpeed")).unwrap_or(1.0);
        if !(speed.is_finite() && speed > 0.0 && speed <= 10_000.0) {
            self.report.warn(format!("clip \"{name}\" has an invalid speed; 100% was used"));
            speed = 1.0;
        }
        let mut clip = self.b.clip(source, kind, name, Tick(start), Tick(duration), Tick(source_in));
        clip.speed = speed;
        clip.reverse = fields.and_then(|f| child_bool(f, "PlayBackwards")).unwrap_or(false);
        if let Some(g) = use_clip.and_then(|c| child_f64(c, "Gain")).filter(|g| g.is_finite() && *g > 0.0) {
            clip.gain_db = (20.0 * g.log10()).clamp(-96.0, 96.0);
        }
        self.read_fx(&mut clip, item, frame);
        Some(clip)
    }

    fn read_fx(&mut self, clip: &mut TrackItem, item: Node<'a, 'i>, frame: (u32, u32)) {
        let Some(chain) = self.objects.at(item, &["ClipTrackItem", "ComponentOwner", "Components"]) else { return };
        let want = if item.has_tag_name("AudioClipTrackItem") { EffectKind::Audio } else { EffectKind::Video };
        let src = self.b.source_size(clip.item).unwrap_or(frame);
        for comp in self.objects.list(chain, &["ComponentChain", "Components"], "Component") {
            if child_text(comp, "MatchName").is_some_and(|m| m.ends_with("SourceSettings")) {
                continue;
            }
            let c = child(comp, "AudioComponent").unwrap_or(comp);
            let id = child_text(comp, "FilterMatchName");
            let name = path_text(c, &["Component", "DisplayName"]).or_else(|| NAMES.iter().find(|n| id == Some(n.0)).map(|n| n.1)).or(id).unwrap_or("?");
            let Some(def) = find_effect_by_name(name).filter(|d| d.kind == want && !graphic::is_layer_id(d.id)) else {
                self.report.warn(format!("effect \"{name}\" is not supported"));
                continue;
            };
            if path_node(c, &["SubComponents", "SubComponent"]).is_some() {
                self.report.warn(format!("masks on \"{name}\" are not supported"));
            }
            let mut fx = def.instance();
            fx.enabled = path_text(c, &["Component", "Bypass"]) != Some("true");
            let mut seen: HashMap<&str, usize> = HashMap::new();
            for p in self.objects.list(c, &["Component", "Params"], "Param") {
                let Some(label) = label(def.id, p).filter(|_| child_i64(p, "ParameterControlType").is_some_and(|t| (1..=8).contains(&t))) else { continue };
                let n = seen.entry(label).or_default();
                let pd = def.params.iter().filter(|d| d.label.eq_ignore_ascii_case(label)).nth(*n);
                *n += 1;
                if let Some(pd) = pd
                    && let Some(v) = param(p, pd, frame, src)
                {
                    fx.params.insert(pd.id.to_string(), v);
                }
            }
            if def.intrinsic
                && let Some(e) = clip.effect_mut(def.id)
            {
                *e = fx;
            } else {
                clip.effects.push(fx);
            }
        }
    }

    fn can_nest(&mut self, seq: ItemId, source: ItemId) -> bool {
        if self.b.p.sequence(source).is_none() {
            return true;
        }
        let mut stack = vec![source];
        let mut seen = HashSet::new();
        while let Some(s) = stack.pop() {
            if s == seq {
                return false;
            }
            if seen.insert(s) {
                stack.extend(self.nests.get(&s).into_iter().flatten().copied());
            }
        }
        self.nests.entry(seq).or_default().insert(source);
        true
    }

    fn read_links(&mut self, node: Node<'a, 'i>, seq: &mut Sequence, clips: &HashMap<NodeId, ClipId>) {
        for link in self.objects.list(node, &["PersistentGroupContainer", "LinkContainer", "Links"], "Link") {
            let members: Vec<ClipId> =
                self.objects.list(link, &["TrackItemGroup", "TrackItems"], "TrackItem").iter().filter_map(|n| clips.get(&n.id()).copied()).collect();
            if members.len() < 2 {
                continue;
            }
            let group = self.b.link_id();
            for member in members {
                if let Some((_, clip)) = seq.find_item_mut(member) {
                    clip.link = Some(group);
                }
            }
        }
    }
}

fn label<'a>(fx: &str, p: Node<'a, '_>) -> Option<&'a str> {
    let (id, name) = (child_text(p, "ParameterID"), child_text(p, "Name").map(str::trim));
    let alias = ALIAS.iter().find(|a| a.0 == fx && (id == Some(a.1) || name == Some(a.1))).map(|a| a.2);
    alias.or(name.filter(|n| !n.is_empty() && *n != "None"))
}

fn start<'a>(p: Node<'a, '_>) -> Option<&'a str> {
    child_text(p, "StartKeyframe").and_then(|s| s.split(',').nth(1)).or_else(|| child_text(p, "CurrentValue"))
}

fn param(p: Node, pd: &ParamDef, frame: (u32, u32), src: (u32, u32)) -> Option<Param> {
    let upper = child_f64(p, "UpperBound");
    let norm = p.has_tag_name("AudioComponentParam");
    let val = |s: &str| value(pd, s, upper, norm, frame, src);
    let mut pr = Param::new(val(start(p)?)?);
    let Some(keys) = child_text(p, "Keyframes") else { return Some(pr) };
    for k in keys.split(';') {
        let mut f = k.split(',');
        let (Some(t), Some(v)) = (f.next().and_then(|t| t.parse::<i64>().ok()), f.next().and_then(val)) else { continue };
        let mut kf = Keyframe::new(Tick(t), v);
        kf.interp = if f.next() == Some("5") { Interpolation::Bezier } else { Interpolation::Linear };
        let infl = |s: Option<&str>| s.and_then(|s| s.parse::<f64>().ok()).filter(|x| (0.0..=1.0).contains(x));
        kf.in_influence = infl(f.nth(2)).unwrap_or(kf.in_influence);
        kf.out_influence = infl(f.nth(1)).unwrap_or(kf.out_influence);
        pr.keyframes.push(kf);
    }
    pr.keyframes.sort_by_key(|k| k.time);
    if let Some(k) = pr.keyframes.first() {
        pr.value = k.value.clone();
    }
    Some(pr)
}

fn value(pd: &ParamDef, s: &str, upper: Option<f64>, norm: bool, frame: (u32, u32), src: (u32, u32)) -> Option<ParamValue> {
    let num = |s: &str| s.parse::<f64>().ok().filter(|v| v.is_finite());
    Some(match &pd.kind {
        ParamKind::Float { min, max, unit, .. } => {
            let v = num(s)?;
            let v = if *unit == "dB" {
                20.0 * v.log10() + 15.0
            } else if norm && *unit == "%" {
                v * 100.0
            } else if upper == Some(255.0) && *max == 100.0 {
                v / 2.55
            } else {
                v
            };
            ParamValue::Float(v.clamp(*min, *max))
        }
        ParamKind::Angle => ParamValue::Float(num(s)?),
        ParamKind::Bool => ParamValue::Bool(s == "true"),
        ParamKind::Choice(o) => {
            let v = s.parse::<u32>().ok()?;
            (upper.map(|u| u as usize + 1) == Some(o.len()) && (v as usize) < o.len()).then_some(ParamValue::Choice(v))?
        }
        ParamKind::Point => {
            let (x, y) = s.split_once(':')?;
            let (w, h) = if pd.id == "anchor" { src } else { frame };
            ParamValue::Vec2(Vec2::new(num(x)? * f64::from(w), num(y)? * f64::from(h)))
        }
        ParamKind::Color => {
            let v = s.parse::<u64>().ok()?;
            let c = |sh: u32| ((v >> sh & 0xffff) >> 8) as f32 / 255.0;
            ParamValue::Color([c(32), c(16), c(0), c(48)])
        }
        _ => return None,
    })
}

fn frame_rate(ticks_per_frame: i64) -> Option<FrameRate> {
    let r = TICKS_PER_SECOND as f64 / (ticks_per_frame > 0).then_some(ticks_per_frame)? as f64;
    let near = |f: FrameRate| (f.as_f64() - r).abs() < r * 1e-5;
    let exact = || (1..=1001).map(|d| FrameRate::new((r * f64::from(d)).round() as i64, d.into())).find(|f| (f.as_f64() - r).abs() < r * 1e-9);
    Some(FrameRate::COMMON.into_iter().find(|f| near(*f)).or_else(exact).unwrap_or_else(|| FrameRate::new((r * 1000.0).round() as i64, 1000)))
}

fn sample_rate(n: Node) -> Option<u32> {
    let ticks = child_i64(n, "FrameRate").filter(|&t| t > 0)?;
    u32::try_from(TICKS_PER_SECOND / ticks).ok().filter(|r| (1_000..=768_000).contains(r))
}

fn channel_count(stream: Node) -> u32 {
    child_text(stream, "AudioChannelLayout")
        .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(j).ok())
        .and_then(|l| u32::try_from(l.len()).ok())
        .filter(|n| (1..=64).contains(n))
        .unwrap_or(2)
}

fn frame_size(rect: &str) -> Option<(u32, u32)> {
    let mut v = rect.split(',').map(|s| s.trim().parse::<i64>().ok());
    let (left, top, right, bottom) = (v.next()??, v.next()??, v.next()??, v.next()??);
    let w = u32::try_from(right.checked_sub(left)?).ok().filter(|w| (1..=32_768).contains(w))?;
    let h = u32::try_from(bottom.checked_sub(top)?).ok().filter(|h| (1..=32_768).contains(h))?;
    Some((w, h))
}
