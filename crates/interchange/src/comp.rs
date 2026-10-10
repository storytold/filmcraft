//! A format-neutral mob-style composition model shared by AAF and OMF.
//!
//! Both formats describe a timeline as a composition of slots (tracks) whose segments are
//! sequences of fillers, source clips and transitions, where a transition *overlaps* the end of
//! the segment before it and the start of the segment after it (the sequence's length is the sum of
//! its segment lengths minus its transition lengths). Source clips reference media through master
//! mobs and file source mobs. [`from_project`] turns a FilmCraft sequence into that shape;
//! [`to_project`] builds FilmCraft sequences and media from it. All times are ticks; the format
//! serialisers convert to and from edit units.

use std::collections::HashMap;

use filmcraft_media::MediaKind;
use filmcraft_project::{
    AudioChannels, ClipId, Interpolation, ItemId, ItemKind, Keyframe, Label, Marker, MarkerId, MarkerKind, MediaRef, Param, ParamValue, Project, Sequence,
    Track, TrackItem, TrackKind, Transition, TransitionAlign, TransitionId, find_effect,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

use crate::common::{Builder, MediaSpec, base_item, db_to_gain, file_name, file_stem, gain_to_db, param, set_param, settings_for, transition_effect};
use crate::essence::{EssenceData, EssenceKey, MediaOptions};
use crate::{ImportOptions, Imported, Report};

/// Picture or sound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CKind {
    Picture,
    Sound,
}

/// A whole document: compositions and the media they reference.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Document {
    /// The top-level compositions.
    pub compositions: Vec<Composition>,
    pub sources: Vec<Source>,
    /// Compositions that are only used inside others (nested sequences); a [`Source`] refers to
    /// one by its index here. They may nest further.
    pub nested: Vec<Composition>,
}

/// How a nested sequence is written. Premiere Pro writes one as a composition of its own in AAF
/// (clips refer to it the way they refer to media) and renders its sound in OMF, which has no
/// picture and whose readers do not follow compositions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Nests {
    Compositions,
    Rendered,
}

/// Nested sequences deeper than this are left as gaps (a sequence cannot be inside itself, but a
/// damaged project may claim so).
const MAX_NESTING: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Composition {
    pub name: String,
    pub rate: FrameRate,
    pub sample_rate: u32,
    pub width: u32,
    pub height: u32,
    pub start_tc: i64,
    pub drop: bool,
    pub tracks: Vec<CTrack>,
    pub markers: Vec<CMarker>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CTrack {
    pub kind: CKind,
    pub name: String,
    /// Physical track number (1-based, per kind).
    pub number: u32,
    /// Sound: channels of the track (1 mono, 2 stereo, 6 5.1).
    pub channels: u32,
    pub items: Vec<CItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CItem {
    Filler(Tick),
    Clip(CClip),
    Transition(CTransition),
}

impl CItem {
    pub fn len(&self) -> Tick {
        match self {
            CItem::Filler(l) => *l,
            CItem::Clip(c) => c.len,
            CItem::Transition(t) => t.len,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CClip {
    pub len: Tick,
    /// Index into [`Document::sources`].
    pub source: usize,
    /// Source (file) time of the segment's first frame.
    pub start: Tick,
    pub gain: Option<Gain>,
    pub name: String,
}

/// Clip gain (linear amplitude).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Gain {
    Constant(f64),
    /// Points at offsets from the segment start; `linear` false = hold (step) interpolation.
    Varying {
        linear: bool,
        points: Vec<(Tick, f64)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CTransition {
    pub len: Tick,
    /// Where the cut lies, from the transition start.
    pub cut: Tick,
    /// FilmCraft effect id.
    pub effect: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CMarker {
    pub start: Tick,
    pub duration: Tick,
    pub name: String,
    pub comment: String,
    pub color: Option<String>,
}

/// One essence: a file (or embedded data) carrying the picture, or some channels of sound, of a
/// media item.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Source {
    /// Groups the sources of one media item (one master mob).
    pub key: String,
    pub name: String,
    pub kind: CKind,
    pub path: Option<String>,
    /// Sound: the (0-based) channel of the file this source exposes (broken out to mono), or
    /// `None` for all of them.
    pub channel: Option<u32>,
    /// Sound: channels this source exposes (1 when `channel` is set, else `file_channels`).
    pub channels: u32,
    /// Sound: channels of the file / embedded data.
    pub file_channels: u32,
    pub width: u32,
    pub height: u32,
    /// Picture: the frame rate of the media.
    pub frame_rate: FrameRate,
    pub sample_rate: u32,
    pub bits: u16,
    pub length: Tick,
    /// Media time of the essence's time 0 (trimmed / consolidated essence).
    pub offset: Tick,
    /// Start timecode in frames at `tc_rate`.
    pub start_tc: Option<i64>,
    pub tc_rate: FrameRate,
    pub embedded: Option<Vec<u8>>,
    pub markers: Vec<CMarker>,
    /// Not media but a nested composition: its index in [`Document::nested`]. `start` of a clip
    /// that uses it is a time in that composition.
    pub nested: Option<usize>,
}

impl Source {
    pub fn is_still(&self) -> bool {
        self.kind == CKind::Picture && self.length == Tick::ZERO
    }
}

// ---------------------------------------------------------------------------------------------
// FilmCraft → composition
// ---------------------------------------------------------------------------------------------

/// How a transition sits on a clip.
#[derive(Clone, Copy, Debug)]
enum Side {
    /// Shared with the adjacent clip: the transition's timeline range.
    TwoSided(Tick, Tick),
    /// Fade from / to nothing: the transition's length, anchored at the clip edge.
    OneSided(Tick),
}

struct Exporter<'a> {
    p: &'a Project,
    media: &'a MediaOptions,
    doc: Document,
    memo: HashMap<(String, CKind, usize, Option<u32>), usize>,
    report: &'a mut Report,
    seq_rate: FrameRate,
    nests: Nests,
    /// Nested sequences already written: their index in `doc.nested`.
    nested: HashMap<ItemId, usize>,
    /// The sequences being written, outermost first.
    open: Vec<ItemId>,
}

pub(crate) fn from_project(p: &Project, seq_id: ItemId, name: &str, media: &MediaOptions, nests: Nests, report: &mut Report) -> crate::Result<Document> {
    let seq = p.sequence(seq_id).ok_or(crate::Error::NoSequence(seq_id))?;
    let rate = seq.settings.frame_rate;
    let mut ex =
        Exporter { p, media, doc: Document::default(), memo: HashMap::new(), report, seq_rate: rate, nests, nested: HashMap::new(), open: vec![seq_id] };
    let comp = ex.composition(seq, name, true);
    ex.doc.compositions.push(comp);
    Ok(ex.doc)
}

impl Exporter<'_> {
    /// `seq` as a composition. `top`: the exported sequence itself (a video mixdown replaces its
    /// video tracks; a nested sequence under a mixdown has no video tracks left to show).
    fn composition(&mut self, seq: &Sequence, name: &str, top: bool) -> Composition {
        let rate = seq.settings.frame_rate;
        let outer_rate = std::mem::replace(&mut self.seq_rate, rate);
        let media = self.media;
        let mut comp = Composition {
            name: name.to_string(),
            rate,
            sample_rate: seq.settings.sample_rate.max(1),
            width: seq.settings.width,
            height: seq.settings.height,
            start_tc: seq.start_timecode,
            drop: seq.settings.drop_frame,
            tracks: Vec::new(),
            markers: seq
                .markers
                .iter()
                .map(|m| CMarker {
                    start: m.start,
                    duration: m.duration,
                    name: m.name.clone(),
                    comment: m.comment.clone(),
                    color: Some(m.color.name().to_string()),
                })
                .collect(),
        };
        if !media.audio_only {
            if let Some(mix) = media.mixdown_video.as_ref().filter(|_| top) {
                let src = self.doc.sources.len();
                self.doc.sources.push(Source {
                    key: "mixdown".into(),
                    name: format!("{name} (video mixdown)"),
                    kind: CKind::Picture,
                    path: Some(mix.path.clone()),
                    channel: None,
                    channels: 0,
                    file_channels: 0,
                    width: mix.width,
                    height: mix.height,
                    frame_rate: rate,
                    sample_rate: 0,
                    bits: 0,
                    length: mix.duration,
                    offset: Tick::ZERO,
                    start_tc: Some(seq.start_timecode + rate.frame_at(mix.start)),
                    tc_rate: rate,
                    embedded: None,
                    markers: Vec::new(),
                    nested: None,
                });
                let mut items = Vec::new();
                if mix.start > Tick::ZERO {
                    items.push(CItem::Filler(mix.start));
                }
                items.push(CItem::Clip(CClip { len: mix.duration, source: src, start: Tick::ZERO, gain: None, name: format!("{name} (video mixdown)") }));
                comp.tracks.push(CTrack { kind: CKind::Picture, name: "V1".into(), number: 1, channels: 0, items });
            } else if media.mixdown_video.is_none() {
                for (i, t) in seq.video_tracks.iter().enumerate() {
                    let items = self.track(t, TrackKind::Video, None);
                    comp.tracks.push(CTrack { kind: CKind::Picture, name: format!("V{}", i + 1), number: i as u32 + 1, channels: 0, items });
                }
            }
        }
        let mut n = 0;
        for t in &seq.audio_tracks {
            let ch = match t.channels {
                AudioChannels::Mono => 1,
                AudioChannels::Surround51 => 6,
                AudioChannels::Stereo | AudioChannels::Adaptive => 2,
            };
            if media.breakout_to_mono {
                for c in 0..ch {
                    n += 1;
                    let items = self.track(t, TrackKind::Audio, Some(c));
                    comp.tracks.push(CTrack { kind: CKind::Sound, name: format!("A{n}"), number: n, channels: 1, items });
                }
            } else {
                n += 1;
                let items = self.track(t, TrackKind::Audio, None);
                comp.tracks.push(CTrack { kind: CKind::Sound, name: format!("A{n}"), number: n, channels: ch, items });
            }
        }
        self.seq_rate = outer_rate;
        comp
    }

    /// The source behind a clip of the nested sequence `nested` (project item `item`): the
    /// sequence's sound as the caller rendered it for this clip, or the sequence as a composition
    /// of its own.
    fn nest_source(&mut self, c: &TrackItem, item: ItemId, name: &str, nested: &Sequence, ckind: CKind, channel: Option<u32>) -> Option<usize> {
        let clip_key = EssenceKey::Clip(c.id);
        if ckind == CKind::Sound
            && let Some(e) = self.media.essence_for(clip_key, channel).cloned()
        {
            let memo_key = (format!("clip:{}", c.id.0), ckind, 0, channel);
            if let Some(&i) = self.memo.get(&memo_key) {
                return Some(i);
            }
            let (path, embedded) = match e.data {
                EssenceData::Embedded(d) => (None, Some(d)),
                EssenceData::File { path } => (Some(path), None),
            };
            let s = Source {
                key: memo_key.0.clone(),
                // (Premiere names the rendered clip after the nested sequence)
                name: name.to_string(),
                kind: ckind,
                path,
                channel: None,
                channels: e.channels.max(1),
                file_channels: e.channels.max(1),
                width: 0,
                height: 0,
                frame_rate: self.seq_rate,
                sample_rate: e.sample_rate.max(1),
                bits: e.bits,
                length: Tick::from_units(e.frames as i64, e.sample_rate.max(1) as i64),
                offset: e.start,
                start_tc: None,
                tc_rate: self.seq_rate,
                embedded,
                markers: c.markers.iter().map(marker_out).collect(),
                nested: None,
            };
            let i = self.doc.sources.len();
            self.doc.sources.push(s);
            self.memo.insert(memo_key, i);
            return Some(i);
        }
        if self.nests == Nests::Rendered {
            self.report.warn(format!("the sound of the nested sequence \"{name}\" was not rendered (left as a gap)"));
            return None;
        }
        let memo_key = (format!("seq:{}", item.0), ckind, 0, channel);
        if let Some(&i) = self.memo.get(&memo_key) {
            return Some(i);
        }
        let index = match self.nested.get(&item) {
            Some(&i) => i,
            None => {
                if self.open.contains(&item) || self.open.len() >= MAX_NESTING {
                    self.report.warn(format!("the nested sequence \"{name}\" is inside itself or nested too deeply (left as a gap)"));
                    return None;
                }
                self.open.push(item);
                let comp = self.composition(nested, name, false);
                self.open.pop();
                let i = self.doc.nested.len();
                self.doc.nested.push(comp);
                self.nested.insert(item, i);
                i
            }
        };
        // nothing to show or play on this kind of track: a gap (as in the sequence itself)
        let comp = self.doc.nested.get(index)?;
        if !comp.tracks.iter().any(|t| t.kind == ckind) {
            return None;
        }
        let channels = if ckind == CKind::Sound { 2 } else { 0 };
        let s = Source {
            key: memo_key.0.clone(),
            name: name.to_string(),
            kind: ckind,
            path: None,
            channel: if ckind == CKind::Sound { channel } else { None },
            channels: if channel.is_some() { 1 } else { channels },
            file_channels: channels,
            width: comp.width,
            height: comp.height,
            frame_rate: comp.rate,
            sample_rate: comp.sample_rate.max(1),
            bits: 16,
            length: nested.duration(),
            offset: Tick::ZERO,
            start_tc: None,
            tc_rate: comp.rate,
            embedded: None,
            markers: Vec::new(),
            nested: Some(index),
        };
        let i = self.doc.sources.len();
        self.doc.sources.push(s);
        self.memo.insert(memo_key, i);
        Some(i)
    }

    /// The source behind `c` on a track of `kind` (`sub`: broken-out channel slot), if exportable.
    fn source_of(&mut self, c: &TrackItem, kind: TrackKind, sub: Option<u32>) -> Option<usize> {
        let item = base_item(self.p, c.item);
        let it = self.p.item(item)?;
        let m = match &it.kind {
            ItemKind::Media(m) => m,
            ItemKind::Sequence(nested) => {
                let ckind = if kind == TrackKind::Video { CKind::Picture } else { CKind::Sound };
                let channel = sub.map(|s| c.source_channels.get(s as usize).map(|&x| x as u32).unwrap_or(s));
                return self.nest_source(c, item, &it.name, nested, ckind, channel);
            }
            _ => {
                match crate::common::uncarried_clip(self.p, c, self.seq_rate) {
                    Some(clip) => self.report.warn(format!("{clip} is not exported (left as a gap)")),
                    None => self.report.warn("graphics, titles and adjustment layers are not exported (left as gaps)"),
                }
                return None;
            }
        };
        let path = match &m.media {
            MediaRef::File { path } => path.clone(),
            MediaRef::Generator(_) => {
                self.report.warn("synthetic media (bars, colour mattes, …) is not exported (left as gaps)");
                return None;
            }
        };
        let ckind = if kind == TrackKind::Video { CKind::Picture } else { CKind::Sound };
        // the source channel this slot plays when broken out to mono
        let channel = sub.map(|s| c.source_channels.get(s as usize).map(|&x| x as u32).unwrap_or(s));
        let clip_key = EssenceKey::Clip(c.id);
        let media_key = EssenceKey::media(item, c.audio_stream);
        let ess = (ckind == CKind::Sound)
            .then(|| self.media.essence_for(clip_key, channel).or_else(|| self.media.essence_for(media_key, channel)))
            .flatten()
            .cloned();
        let key = match &ess {
            Some(e) if e.key == clip_key => format!("clip:{}", c.id.0),
            _ if ckind == CKind::Sound && c.audio_stream > 0 => format!("item:{}:stream:{}", item.0, c.audio_stream),
            _ => format!("item:{}", item.0),
        };
        // Original linked file locators cannot name a container stream. Leave an explicit
        // unsupported-feature report rather than reference its first audio stream.
        if ckind == CKind::Sound && c.audio_stream > 0 && ess.is_none() {
            self.report.warn("selected container audio streams require separate or embedded audio essence (left as gaps)");
            return None;
        }
        let stream = if ckind == CKind::Sound { c.audio_stream } else { 0 };
        let memo_key = (key.clone(), ckind, stream, channel);
        if let Some(&i) = self.memo.get(&memo_key) {
            return Some(i);
        }
        let video = m.info.video.as_ref();
        let audio = m.info.audio_streams.get(c.audio_stream);
        let still = m.info.kind == MediaKind::Still;
        match ckind {
            CKind::Picture if video.is_none() && !still => return None,
            CKind::Sound if audio.is_none() => return None,
            _ => {}
        }
        let frame_rate = video.map(|v| v.frame_rate).filter(|r| r.num > 0).unwrap_or(self.seq_rate);
        let mut markers: Vec<CMarker> = m.markers.iter().chain(c.markers.iter()).map(marker_out).collect();
        markers.sort_by(|a, b| (a.start, &a.name).cmp(&(b.start, &b.name)));
        markers.dedup();
        let mut s = Source {
            key,
            name: it.name.clone(),
            kind: ckind,
            path: Some(path.clone()),
            channel: None,
            channels: 0,
            file_channels: 0,
            width: video.map_or(0, |v| v.width),
            height: video.map_or(0, |v| v.height),
            frame_rate,
            sample_rate: audio.map_or(48_000, |a| a.sample_rate.max(1)),
            bits: audio.and_then(|a| a.bits_per_sample).unwrap_or(16).clamp(8, 32) as u16,
            length: if still { Tick::ZERO } else { m.info.duration },
            offset: Tick::ZERO,
            start_tc: m.info.start_timecode,
            tc_rate: frame_rate,
            embedded: None,
            markers,
            nested: None,
        };
        if ckind == CKind::Sound {
            s.file_channels = audio.map_or(2, |a| a.channels.max(1));
            s.channel = channel;
            if let Some(e) = ess {
                s.sample_rate = e.sample_rate.max(1);
                s.bits = e.bits;
                s.file_channels = e.channels.max(1);
                // per-channel essence is a mono file: its only channel
                if e.channel.is_some() {
                    s.channel = None;
                }
                s.offset = e.start;
                s.length = Tick::from_units(e.frames as i64, e.sample_rate.max(1) as i64);
                match e.data {
                    EssenceData::Embedded(d) => {
                        s.path = None;
                        s.embedded = Some(d);
                    }
                    EssenceData::File { path } => s.path = Some(path),
                }
                if e.key == clip_key {
                    s.name = format!("{} (rendered)", c.name);
                }
            }
        }
        s.channels = if s.channel.is_some() { 1 } else { s.file_channels };
        let i = self.doc.sources.len();
        self.doc.sources.push(s);
        self.memo.insert(memo_key, i);
        Some(i)
    }

    fn gain_of(&mut self, c: &TrackItem, source: &Source) -> Option<Gain> {
        if source.key.starts_with("clip:") {
            return None; // rendered: effects baked in
        }
        let g = c.gain_db;
        match param(c, "volume", "level") {
            Some(p) if p.is_animated() => {
                let speed = if c.speed.abs() > 1e-9 { c.speed.abs() } else { 1.0 };
                let linear = !p.keyframes.iter().all(|k| k.interp == Interpolation::Hold);
                if p.keyframes.iter().any(|k| !matches!(k.interp, Interpolation::Linear | Interpolation::Hold)) {
                    self.report.info("Bezier volume keyframes were exported as linear");
                }
                let points = p
                    .keyframes
                    .iter()
                    .map(|k| {
                        let off = Tick(((k.time - c.source_in).0 as f64 / speed).round() as i64);
                        (off, db_to_gain(k.value.as_f64().unwrap_or(0.0) + g))
                    })
                    .collect();
                Some(Gain::Varying { linear, points })
            }
            Some(p) => {
                let db = p.value.as_f64().unwrap_or(0.0) + g;
                (db.abs() > 1e-9).then(|| Gain::Constant(db_to_gain(db)))
            }
            None => (g.abs() > 1e-9).then(|| Gain::Constant(db_to_gain(g))),
        }
    }

    fn track(&mut self, t: &Track, kind: TrackKind, sub: Option<u32>) -> Vec<CItem> {
        // exportable clips with their sources
        let clips: Vec<(&TrackItem, usize)> = t.items.iter().filter_map(|c| self.source_of(c, kind, sub).map(|s| (c, s))).collect();
        let idx_of: HashMap<ClipId, usize> = clips.iter().enumerate().map(|(i, (c, _))| (c.id, i)).collect();
        let mut tin: Vec<Option<(Side, &Transition)>> = vec![None; clips.len()];
        let mut tout: Vec<Option<(Side, &Transition)>> = vec![None; clips.len()];
        for tr in &t.transitions {
            let a = tr.from.and_then(|f| idx_of.get(&f).copied());
            let b = tr.to.and_then(|f| idx_of.get(&f).copied());
            match (a, b) {
                (Some(a), Some(b)) if b == a + 1 && clips[a].0.end() == clips[b].0.start && tr.start <= clips[b].0.start && tr.end() >= clips[a].0.end() => {
                    tout[a] = Some((Side::TwoSided(tr.start, tr.end()), tr));
                    tin[b] = Some((Side::TwoSided(tr.start, tr.end()), tr));
                }
                (_, Some(b)) if tr.from.is_none() || a.is_none() => {
                    let c = clips[b].0;
                    let len = (tr.end().min(c.end()) - tr.start.max(c.start)).max(Tick::ZERO);
                    if tr.start != c.start {
                        self.report.info("a fade-in transition not starting at its clip was moved to the clip start");
                    }
                    if len > Tick::ZERO {
                        tin[b] = Some((Side::OneSided(len), tr));
                    }
                }
                (Some(a), _) => {
                    let c = clips[a].0;
                    let len = (tr.end().min(c.end()) - tr.start.max(c.start)).max(Tick::ZERO);
                    if tr.end() != c.end() {
                        self.report.info("a fade-out transition not ending at its clip was moved to the clip end");
                    }
                    if len > Tick::ZERO {
                        tout[a] = Some((Side::OneSided(len), tr));
                    }
                }
                _ => self.report.warn("a transition without an exported clip was dropped"),
            }
        }
        let mut out = Vec::new();
        let mut cursor = Tick::ZERO;
        let mut tail = Tick::ZERO; // the timeline must reach at least here (after a fade-out)
        for (i, &(c, src)) in clips.iter().enumerate() {
            let (s, e) = (c.start, c.end());
            let mut seg_start = s;
            match tin[i] {
                Some((Side::TwoSided(ts, _), _)) => seg_start = ts,
                Some((Side::OneSided(len), tr)) => {
                    let fill_to = s + len;
                    if fill_to > cursor {
                        out.push(CItem::Filler(fill_to - cursor));
                        cursor = fill_to;
                    }
                    out.push(CItem::Transition(CTransition { len, cut: Tick::ZERO, effect: tr.effect.effect.clone() }));
                    cursor -= len;
                }
                None => {
                    if s > cursor {
                        out.push(CItem::Filler(s - cursor));
                    }
                }
            }
            let seg_end = match tout[i] {
                Some((Side::TwoSided(_, te), _)) => te,
                _ => e,
            };
            if c.speed != 1.0 || c.reverse || c.frame_hold.is_some() {
                self.report.warn("speed changes, reverse playback and frame holds are not exported (clips play at 100%)");
            }
            let source = self.doc.sources[src].clone();
            // clips play at 100% here, so timeline and media advance together
            let start = c.source_in + (seg_start - s) - source.offset;
            if start < Tick::ZERO {
                self.report.warn("a clip uses media before the start of its (trimmed) essence");
            }
            let mut gain = if kind == TrackKind::Audio { self.gain_of(c, &source) } else { None };
            if let Some(Gain::Varying { points, .. }) = &mut gain {
                for p in points.iter_mut() {
                    p.0 += s - seg_start;
                }
            }
            out.push(CItem::Clip(CClip { len: seg_end - seg_start, source: src, start, gain, name: c.name.clone() }));
            cursor = seg_end;
            match tout[i] {
                Some((Side::TwoSided(ts, _), tr)) => {
                    out.push(CItem::Transition(CTransition { len: seg_end - ts, cut: e - ts, effect: tr.effect.effect.clone() }));
                    cursor = ts;
                    tail = tail.max(seg_end);
                }
                Some((Side::OneSided(len), tr)) => {
                    out.push(CItem::Transition(CTransition { len, cut: len, effect: tr.effect.effect.clone() }));
                    cursor = e - len;
                    tail = tail.max(e);
                }
                None => tail = tail.max(e),
            }
        }
        if tail > cursor {
            out.push(CItem::Filler(tail - cursor));
        }
        out
    }
}

fn marker_out(m: &Marker) -> CMarker {
    CMarker { start: m.start, duration: m.duration, name: m.name.clone(), comment: m.comment.clone(), color: Some(m.color.name().to_string()) }
}

fn marker_in(b: &mut Builder, m: &CMarker) -> Marker {
    Marker {
        id: MarkerId(b.alloc()),
        start: m.start,
        duration: m.duration,
        name: m.name.clone(),
        comment: m.comment.clone(),
        kind: MarkerKind::Comment,
        color: m.color.as_deref().and_then(Label::from_name).unwrap_or(Label::Forest),
    }
}

// ---------------------------------------------------------------------------------------------
// Composition → FilmCraft
// ---------------------------------------------------------------------------------------------

/// Media a document embeds, to be written next to it by the caller (the item's path).
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractedMedia {
    /// The imported media item (fragment-local id) whose path is `path`.
    pub item: ItemId,
    pub path: String,
    /// A complete WAV file.
    pub wav: Vec<u8>,
}

pub(crate) fn to_project(doc: Document, opts: &ImportOptions, report: &mut Report) -> crate::Result<(Imported, Vec<ExtractedMedia>)> {
    if doc.compositions.is_empty() {
        return Err(crate::Error::Empty);
    }
    let name = opts.name.clone().unwrap_or_else(|| doc.compositions[0].name.clone());
    let mut b = Builder::new(&name);
    let media_bin = b.bin("Media", None);
    let mut extracted = Vec::new();
    // one media item per (key, path)
    let mut item_of: HashMap<usize, ItemId> = HashMap::new();
    let mut groups: Vec<((String, Option<String>), Vec<usize>)> = Vec::new();
    for (i, s) in doc.sources.iter().enumerate() {
        if s.nested.is_some() {
            continue; // a nested composition, not media: a sequence (below)
        }
        let k = (s.key.clone(), s.path.clone().or_else(|| s.embedded.is_some().then(|| format!("embedded:{i}"))));
        match groups.iter_mut().find(|(g, _)| *g == k) {
            Some((_, v)) => v.push(i),
            None => groups.push((k, vec![i])),
        }
    }
    let doc_stem = opts.name.clone().unwrap_or_else(|| "Imported".into());
    let mut used_paths = std::collections::HashSet::new();
    for ((_, path), members) in &groups {
        let srcs: Vec<&Source> = members.iter().map(|&i| &doc.sources[i]).collect();
        let first = srcs[0];
        let pic = srcs.iter().find(|s| s.kind == CKind::Picture);
        let snd = srcs.iter().find(|s| s.kind == CKind::Sound);
        let mut spec = MediaSpec {
            duration: Some(srcs.iter().map(|s| s.length).max().unwrap_or(Tick::ZERO)),
            video: pic.map(|p| (p.width.max(1), p.height.max(1), p.frame_rate)),
            audio: snd.map(|s| {
                (s.sample_rate, srcs.iter().filter(|x| x.kind == CKind::Sound).map(|x| x.file_channels.max(x.channel.map_or(0, |c| c + 1))).max().unwrap_or(1))
            }),
            start_tc: first.start_tc.or_else(|| srcs.iter().find_map(|s| s.start_tc)),
            kind: None,
        };
        if pic.is_some_and(|p| p.is_still()) {
            spec.kind = Some(MediaKind::Still);
        }
        let embedded = snd.and_then(|s| s.embedded.as_ref().map(|d| (*s, d)));
        let (resolved, display) = match (path.as_deref(), embedded) {
            (_, Some((s, _))) => {
                let safe: String = first.name.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect();
                let base = format!("{doc_stem} Media/{}{}", safe.trim(), s.channel.map(|c| format!(" ch{}", c + 1)).unwrap_or_default());
                let mut rel = format!("{base}.wav");
                let mut n = 1;
                while !used_paths.insert(rel.to_ascii_lowercase()) {
                    n += 1;
                    rel = format!("{base} ({n}).wav");
                }
                (crate::common::resolve_path(&rel, opts.base_dir.as_deref()), first.name.clone())
            }
            (Some(p), None) => {
                (crate::common::resolve_path(p, opts.base_dir.as_deref()), if first.name.is_empty() { file_name(p).to_string() } else { first.name.clone() })
            }
            (None, None) => (String::new(), first.name.clone()),
        };
        let display = if display.is_empty() { file_stem(&resolved).to_string() } else { display };
        let key = format!("{}|{}", first.key, resolved);
        let id = b.file_media(&key, &display, &resolved, &spec, Some(media_bin));
        if resolved.is_empty()
            && let Some(m) = b.p.item_mut(id).and_then(|i| i.as_media_mut())
        {
            m.offline = true;
            report.warn(format!("\"{display}\" has no media location; imported offline"));
        }
        let mut markers: Vec<CMarker> = srcs.iter().flat_map(|s| s.markers.iter().cloned()).collect();
        markers.sort_by(|a, b| (a.start, &a.name).cmp(&(b.start, &b.name)));
        markers.dedup();
        let mk: Vec<Marker> = markers.iter().map(|m| marker_in(&mut b, m)).collect();
        if let Some(m) = b.p.item_mut(id).and_then(|i| i.as_media_mut()) {
            m.markers = mk;
        }
        if let Some((s, data)) = embedded {
            extracted.push(ExtractedMedia {
                item: id,
                path: resolved.clone(),
                wav: crate::wav::wav_file(data, s.file_channels.max(1) as u16, s.sample_rate, s.bits),
            });
        }
        for &i in members {
            item_of.insert(i, id);
        }
    }
    // nested compositions are sequences that clips use as their source: reserve them all first
    // (they may use each other), then build them
    let settings_of = |comp: &Composition| {
        let mut settings = settings_for(comp.rate, comp.width.max(16), comp.height.max(16), comp.drop);
        settings.sample_rate = comp.sample_rate.max(1);
        settings
    };
    let nested_ids: Vec<ItemId> = doc.nested.iter().map(|comp| b.reserve_sequence(&comp.name, settings_of(comp), None)).collect();
    for (i, s) in doc.sources.iter().enumerate() {
        if let Some(&id) = s.nested.and_then(|n| nested_ids.get(n)) {
            item_of.insert(i, id);
        }
    }
    for (comp, &seq_id) in doc.nested.iter().zip(&nested_ids) {
        let seq = build_sequence(&mut b, comp, settings_of(comp), &doc.sources, &item_of, report);
        b.put_sequence(seq_id, seq);
    }
    for comp in &doc.compositions {
        let seq_id = b.reserve_sequence(&comp.name, settings_of(comp), None);
        let seq = build_sequence(&mut b, comp, settings_of(comp), &doc.sources, &item_of, report);
        b.put_sequence(seq_id, seq);
        b.top.push(seq_id);
    }
    Ok((b.finish(), extracted))
}

fn build_sequence(
    b: &mut Builder,
    comp: &Composition,
    settings: filmcraft_project::SequenceSettings,
    sources: &[Source],
    item_of: &HashMap<usize, ItemId>,
    report: &mut Report,
) -> Sequence {
    let mut seq = crate::common::empty_sequence(settings);
    seq.start_timecode = comp.start_tc;
    seq.markers = comp.markers.iter().map(|m| marker_in(b, m)).collect();
    for t in &comp.tracks {
        let kind = if t.kind == CKind::Picture { TrackKind::Video } else { TrackKind::Audio };
        let idx = seq.tracks(kind).len();
        let mut track = b.track(kind, idx);
        if kind == TrackKind::Audio {
            track.channels = match t.channels {
                1 => AudioChannels::Mono,
                6 => AudioChannels::Surround51,
                _ => AudioChannels::Stereo,
            };
        }
        build_track(b, &mut track, kind, t, sources, item_of, report);
        seq.tracks_mut(kind).push(track);
    }
    if seq.video_tracks.is_empty() {
        let t = b.track(TrackKind::Video, 0);
        seq.video_tracks.push(t);
    }
    if seq.audio_tracks.is_empty() {
        let t = b.track(TrackKind::Audio, 0);
        seq.audio_tracks.push(t);
    }
    link_clips(b, &mut seq);
    seq
}

enum Placed {
    Gap,
    Clip(usize),
    Tr { start: Tick, len: Tick, cut: Tick, effect: String },
}

fn build_track(b: &mut Builder, track: &mut Track, kind: TrackKind, t: &CTrack, sources: &[Source], item_of: &HashMap<usize, ItemId>, report: &mut Report) {
    let mut pos = Tick::ZERO;
    let mut placed: Vec<Placed> = Vec::new();
    let mut clips: Vec<TrackItem> = Vec::new();
    for it in &t.items {
        if pos.0.abs() > 4 * MAX_TIME.0 {
            report.warn("a track longer than FilmCraft supports was cut short");
            break;
        }
        match it {
            CItem::Filler(l) => {
                pos += *l;
                placed.push(Placed::Gap);
            }
            CItem::Transition(tr) => {
                pos -= tr.len;
                placed.push(Placed::Tr { start: pos, len: tr.len, cut: tr.cut, effect: tr.effect.clone() });
            }
            CItem::Clip(c) => {
                let Some(&item) = item_of.get(&c.source) else {
                    pos += c.len;
                    placed.push(Placed::Gap);
                    continue;
                };
                let mut ti = b.clip(item, kind, if c.name.is_empty() { &sources[c.source].name } else { &c.name }, pos, c.len, c.start);
                if kind == TrackKind::Audio {
                    let src = &sources[c.source];
                    if t.channels == 1 && src.nested.is_none() {
                        ti.source_channels = vec![src.channel.unwrap_or(0) as u16];
                    }
                    if let Some(g) = &c.gain {
                        apply_gain(&mut ti, g);
                    }
                }
                placed.push(Placed::Clip(clips.len()));
                clips.push(ti);
                pos += c.len;
            }
        }
    }
    // resolve transitions against their neighbours
    let mut transitions = Vec::new();
    for k in 0..placed.len() {
        let Placed::Tr { start, len, cut, effect } = &placed[k] else { continue };
        let prev = k.checked_sub(1).and_then(|i| match placed[i] {
            Placed::Clip(c) => Some(c),
            _ => None,
        });
        let next = placed.get(k + 1).and_then(|p| match p {
            Placed::Clip(c) => Some(*c),
            _ => None,
        });
        let (start, len, cut) = (*start, *len, (*cut).clamp(Tick::ZERO, *len));
        let cut_at = start + cut;
        match (prev, next) {
            (Some(a), Some(bi)) => {
                let a_end = clips[a].end();
                if a_end > cut_at {
                    clips[a].duration = cut_at - clips[a].start;
                }
                let shift = cut_at - clips[bi].start;
                if shift > Tick::ZERO {
                    // keyframes are in media time: unaffected
                    clips[bi].start += shift;
                    clips[bi].source_in += shift;
                    clips[bi].duration -= shift;
                }
            }
            (None, None) => {
                report.warn("a transition between two fillers was dropped");
                continue;
            }
            _ => {}
        }
        let align = if cut == Tick::ZERO {
            TransitionAlign::StartAtCut
        } else if cut == len {
            TransitionAlign::EndAtCut
        } else {
            TransitionAlign::CenterAtCut
        };
        let audio = kind == TrackKind::Audio;
        let effect = match find_effect(effect) {
            Some(d) if d.kind == if audio { filmcraft_project::EffectKind::AudioTransition } else { filmcraft_project::EffectKind::VideoTransition } => {
                d.instance()
            }
            _ => transition_effect(effect, audio, report),
        };
        transitions.push(Transition {
            id: TransitionId(b.alloc()),
            effect,
            start,
            duration: len,
            from: prev.map(|a| clips[a].id),
            to: next.map(|c| clips[c].id),
            align,
            reverse: false,
        });
    }
    clips.retain(|c| c.duration > Tick::ZERO);
    track.items = clips;
    track.transitions = transitions;
    track.sort();
}

fn apply_gain(ti: &mut TrackItem, g: &Gain) {
    match g {
        Gain::Constant(a) => set_param(ti, "volume", "level", Param::new(ParamValue::Float(gain_to_db(*a)))),
        Gain::Varying { linear, points } => {
            let mut p = Param::new(ParamValue::Float(points.first().map_or(0.0, |x| gain_to_db(x.1))));
            for (off, a) in points {
                let off = (*off).clamp(-MAX_TIME, MAX_TIME);
                let mut k = Keyframe::new(ti.source_in + off, ParamValue::Float(gain_to_db(*a)));
                k.interp = if *linear { Interpolation::Linear } else { Interpolation::Hold };
                p.keyframes.push(k);
            }
            p.keyframes.sort_by_key(|k| k.time);
            set_param(ti, "volume", "level", p);
        }
    }
}

/// Link video and audio clips of the same media that start and end together.
fn link_clips(b: &mut Builder, seq: &mut Sequence) {
    let mut groups: HashMap<(ItemId, Tick, Tick, Tick), Vec<(TrackKind, usize, usize)>> = HashMap::new();
    for kind in [TrackKind::Video, TrackKind::Audio] {
        for (ti, t) in seq.tracks(kind).iter().enumerate() {
            for (ci, c) in t.items.iter().enumerate() {
                groups.entry((c.item, c.start, c.duration, c.source_in)).or_default().push((kind, ti, ci));
            }
        }
    }
    let mut keys: Vec<_> = groups.keys().copied().collect();
    keys.sort_by_key(|k| (k.1, k.0.0, k.2, k.3));
    for k in keys {
        let g = &groups[&k];
        if g.len() < 2 {
            continue;
        }
        let l = b.link_id();
        for &(kind, ti, ci) in g {
            seq.tracks_mut(kind)[ti].items[ci].link = Some(l);
        }
    }
}

/// The largest time read from a document (about 3.3 days).
pub(crate) const MAX_TIME: Tick = Tick(1 << 56);

/// Ticks of `n` edit units at rate `num/den` (rounded to the nearest tick).
pub(crate) fn units_to_ticks(n: i64, num: i64, den: i64) -> Tick {
    if num <= 0 || den <= 0 {
        return Tick::ZERO;
    }
    let t = n as i128 * den as i128 * TICKS_PER_SECOND as i128;
    let q = t.div_euclid(num as i128);
    let r = t.rem_euclid(num as i128);
    // documents are untrusted: keep every time far from overflow (±3 days)
    Tick((q + if r * 2 >= num as i128 { 1 } else { 0 }).clamp(-MAX_TIME.0 as i128, MAX_TIME.0 as i128) as i64)
}

/// Edit units at rate `num/den` nearest to `t`; the flag is false when `t` is not on a unit.
pub(crate) fn ticks_to_units(t: Tick, num: i64, den: i64) -> (i64, bool) {
    if num <= 0 || den <= 0 {
        return (0, false);
    }
    let x = t.0 as i128 * num as i128;
    let d = den as i128 * TICKS_PER_SECOND as i128;
    let q = x.div_euclid(d);
    let r = x.rem_euclid(d);
    let n = q + if r * 2 >= d { 1 } else { 0 };
    (n.clamp(i64::MIN as i128, i64::MAX as i128) as i64, r == 0)
}
