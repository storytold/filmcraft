//! MLT XML: Kdenlive projects (`.kdenlive`). Import only.
//!
//! Written from the public MLT documentation (mltframework.org: *MLT XML*, *Framework Design*,
//! *Profiles*) and from looking at project files Kdenlive writes; MLT and Kdenlive source (both
//! GPL) were not consulted.
//!
//! What is read:
//!
//! - `<profile>`: frame size, frame rate and pixel aspect become the sequence settings. Without
//!   one, MLT's default (DV PAL: 720×576 at 25 fps), with an info entry.
//! - Producers and chains with a `resource`: file media, one item per file (relative paths
//!   resolve against `<mlt root>`, else the document's directory). `color` producers become
//!   colour mattes (Black Video for black). The clips of the bin (the playlist `<mlt producer>`
//!   names, Kdenlive's `main_bin`) are imported even when the timeline doesn't use them.
//! - The timeline: the tractor `<mlt producer>` names, else the last tractor of the document
//!   (MLT plays the last producer). A tractor whose only track is another multitrack tractor is
//!   a wrapper (a project tractor around a Kdenlive sequence) and is looked through.
//!   Each track is a playlist, or a tractor of playlists (a Kdenlive track: its playlists share
//!   the track). In a playlist, `<entry producer in out>` is a clip (`out` is inclusive: a
//!   producer runs from `in` to `length - 1`) and `<blank length>` a gap. An entry without
//!   `in` / `out` takes its producer's.
//! - Track kind: audio when flagged `kdenlive:audio_track`; in documents without Kdenlive
//!   properties, when the track hides its video but not its audio (`hide="video"`). A hidden
//!   video track is disabled, a hidden audio track muted. Video tracks keep MLT's stacking order
//!   (the first is V1, at the bottom). Kdenlive stacks its audio tracks under the video ones,
//!   A1 nearest them, so the last audio track of a Kdenlive document is A1; other documents keep
//!   their order. Kdenlive's black background track is left out. `kdenlive:track_name` and
//!   `kdenlive:locked_track` are kept.
//! - Times: frame counts, clock values (`HH:MM:SS.mmm`, also `MM:SS.mmm`, with `.` or `,`
//!   before the fraction, rounded to the nearest frame) and SMPTE timecode (`HH:MM:SS:FF`,
//!   drop-frame `HH:MM:SS;FF`), all at the profile's rate.
//! - A video clip and an audio clip that play the same media over the same time are linked,
//!   as Kdenlive groups the two halves of a clip.
//!
//! Not imported, each named in the [`Report`] (the clip is left as a gap): transitions, filters
//! (effects), title clips, speed changes (`timewarp`), nested timelines, guides, clip markers
//! and bin folders. Kdenlive's own track plumbing (filters and transitions marked
//! `internal_added`) is not an edit and is left out without a report entry.

use std::collections::{HashMap, HashSet, VecDeque};

use filmcraft_media::{Generator, MediaKind};
use filmcraft_project::{ItemId, Sequence, SequenceSettings, TrackItem, TrackKind};
use filmcraft_time::FrameRate;
use roxmltree::{Node, NodeId};

use crate::common::{Builder, MediaSpec, empty_sequence, file_name, kind_from_ext, resolve_path, settings_for};
use crate::xml::{children, elements};
use crate::{Error, Format, ImportOptions, Imported, Report, Result};

/// Deepest chain of wrapper tractors looked through.
const MAX_DEPTH: usize = 16;
/// Positions beyond 30 days are refused as damaged.
const MAX_SECONDS: i128 = 30 * 86_400;

fn tag<'a>(n: Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}

/// A property: a `<property name>` child, else an attribute of that name.
fn prop<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    match children(n, "property").find(|p| p.attribute("name") == Some(name)) {
        Some(p) => Some(p.text().unwrap_or("")),
        None => n.attribute(name),
    }
}

fn is_producer_like(n: Node<'_, '_>) -> bool {
    matches!(tag(n), "producer" | "chain" | "playlist" | "tractor")
}

/// A non-negative decimal integer (at most 18 digits).
fn digits(s: &str) -> Option<i128> {
    if s.is_empty() || s.len() > 18 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A time value as a frame count at `rate`: frames (`125`), a clock value (`HH:MM:SS.mmm` or
/// `MM:SS.mmm`, `.` or `,` before the fraction, rounded to the nearest frame) or SMPTE timecode
/// (`HH:MM:SS:FF`, drop-frame `HH:MM:SS;FF`). `None` for anything else, negative values included.
pub(crate) fn parse_time(s: &str, rate: FrameRate) -> Option<i64> {
    let s = s.trim();
    if !s.contains([':', ';']) {
        return digits(s).and_then(|v| i64::try_from(v).ok());
    }
    let parts: Vec<&str> = s.split([':', ';']).collect();
    match parts.as_slice() {
        [h, m, sec, f] if !s.contains(['.', ',']) => {
            let field = |p: &str| digits(p).and_then(|v| i64::try_from(v).ok());
            let frames = filmcraft_time::fields_to_frames(field(h)?, field(m)?, field(sec)?, field(f)?, rate.sane(), s.contains(';'));
            (frames >= 0).then_some(frames)
        }
        [_, _] | [_, _, _] if !s.contains(';') => clock_frames(&parts, rate),
        _ => None,
    }
}

/// `HH:MM:SS.fff` (or `MM:SS.fff`) to the nearest frame, exactly.
fn clock_frames(parts: &[&str], rate: FrameRate) -> Option<i64> {
    let (last, whole) = parts.split_last()?;
    let mut secs: i128 = 0;
    for p in whole {
        secs = secs.checked_mul(60)?.checked_add(digits(p)?)?;
    }
    let (int, frac) = last.split_once(['.', ',']).unwrap_or((last, ""));
    secs = secs.checked_mul(60)?.checked_add(digits(int)?)?;
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // nanoseconds are finer than any frame: further digits change nothing
    let frac = frac.get(..frac.len().min(9)).unwrap_or("");
    let scale = 10i128.checked_pow(u32::try_from(frac.len()).ok()?)?;
    let frac = if frac.is_empty() { 0 } else { frac.parse::<i128>().ok()? };
    let r = rate.sane();
    let n = secs.checked_mul(scale)?.checked_add(frac)?.checked_mul(i128::from(r.num))?;
    let d = scale.checked_mul(i128::from(r.den))?;
    i64::try_from(n.checked_add(d / 2)?.checked_div(d)?).ok()
}

/// Pairs of hex digits as bytes.
fn hex_bytes(h: &str) -> Option<Vec<u8>> {
    if !h.len().is_multiple_of(2) || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..h.len()).step_by(2).map(|i| h.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok())).collect()
}

/// A `color` producer's colour as RGBA bytes: `0xRRGGBBAA`, `0xRRGGBB`, `#AARRGGBB`, `#RRGGBB`,
/// `black`, `white` or `transparent`.
fn parse_rgba(s: &str) -> Option<[u8; 4]> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return match hex_bytes(h)?.as_slice() {
            [r, g, b, a] => Some([*r, *g, *b, *a]),
            [r, g, b] => Some([*r, *g, *b, 255]),
            _ => None,
        };
    }
    if let Some(h) = s.strip_prefix('#') {
        return match hex_bytes(h)?.as_slice() {
            [a, r, g, b] => Some([*r, *g, *b, *a]),
            [r, g, b] => Some([*r, *g, *b, 255]),
            _ => None,
        };
    }
    match s.to_ascii_lowercase().as_str() {
        "black" => Some([0, 0, 0, 255]),
        "white" => Some([255, 255, 255, 255]),
        "transparent" => Some([0, 0, 0, 0]),
        _ => None,
    }
}

const OPAQUE_BLACK: [u8; 4] = [0, 0, 0, 255];

fn is_color(p: Node<'_, '_>) -> bool {
    matches!(prop(p, "mlt_service").map(str::trim), Some("color" | "colour"))
}

/// The name a producer's clips get: Kdenlive's clip name, else the file name, else the id.
fn clip_name(p: Node<'_, '_>) -> String {
    if let Some(n) = prop(p, "kdenlive:clipname").map(str::trim).filter(|n| !n.is_empty()) {
        return n.to_string();
    }
    if is_color(p) {
        return "Color".into();
    }
    match prop(p, "resource").map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => file_name(r).to_string(),
        None => p.attribute("id").unwrap_or("Clip").to_string(),
    }
}

/// An element a tractor plays as a track, with the track's `hide` attribute.
#[derive(Clone, Copy)]
struct TrackRef<'a, 'i> {
    node: Node<'a, 'i>,
    hide: Option<&'a str>,
}

impl TrackRef<'_, '_> {
    /// Whether this track hides `what` (`video` or `audio`).
    fn hides(&self, what: &str) -> bool {
        self.hide.is_some_and(|h| h == what || h == "both")
    }
}

/// A timeline track as read, before it gets its place among the sequence's tracks.
struct ReadTrack {
    kind: TrackKind,
    name: Option<String>,
    enabled: bool,
    muted: bool,
    locked: bool,
    items: Vec<TrackItem>,
    /// Clips overlapping one already on the track (Kdenlive's same-track mixes).
    overlapping: Vec<TrackItem>,
}

/// What a producer brings to the timeline.
#[derive(Clone, Copy)]
enum Source {
    Media(ItemId),
    /// Not imported; what it is, for the report.
    Skipped(&'static str),
}

struct Imp<'a, 'i, 'r> {
    b: Builder,
    report: &'r mut Report,
    rate: FrameRate,
    size: (u32, u32),
    /// Last frame a position may reach ([`MAX_SECONDS`] at the profile's rate).
    max_frames: i64,
    base_dir: Option<String>,
    /// The document carries Kdenlive properties (its tracks say whether they are audio).
    kdenlive: bool,
    by_id: HashMap<&'a str, Node<'a, 'i>>,
    sources: HashMap<NodeId, Source>,
}

pub(crate) fn import(text: &str, opts: &ImportOptions, report: &mut Report) -> Result<Imported> {
    let doc = crate::xml::parse(text).map_err(|e| Error::parse(Format::Mlt, e))?;
    let root = doc.root_element();
    if tag(root) != "mlt" {
        return Err(Error::parse(Format::Mlt, format!("the root element is <{}>, not <mlt>", tag(root))));
    }
    let mut by_id = HashMap::new();
    for n in root.descendants().filter(|n| is_producer_like(*n)) {
        if let Some(id) = n.attribute("id") {
            by_id.entry(id).or_insert(n);
        }
    }
    let kdenlive = root.descendants().any(|n| tag(n) == "property" && n.attribute("name").is_some_and(|p| p.starts_with("kdenlive:")));
    let (rate, size, par) = profile(root, report);
    let r = rate.sane();
    let max_frames = i64::try_from(MAX_SECONDS * i128::from(r.num) / i128::from(r.den.max(1))).unwrap_or(i64::MAX);
    let base_dir = match root.attribute("root").map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => Some(resolve_path(d, opts.base_dir.as_deref())),
        None => opts.base_dir.clone(),
    };
    let name = opts.name.clone().filter(|n| !n.trim().is_empty());
    let mut imp = Imp {
        b: Builder::new(name.as_deref().unwrap_or("Kdenlive Project")),
        report,
        rate,
        size,
        max_frames,
        base_dir,
        kdenlive,
        by_id,
        sources: HashMap::new(),
    };
    imp.undefined_tracks(root);
    imp.bin(root);
    let (timeline, tracks) = imp.timeline(root).ok_or(Error::Empty)?;
    let mut settings = settings_for(rate, size.0, size.1, false);
    settings.par = par;
    let seq_name =
        prop(timeline, "kdenlive:clipname").map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).or(name).unwrap_or_else(|| "Timeline".into());
    imp.sequence(&seq_name, settings, &tracks);
    imp.report_skipped(root);
    Ok(imp.b.finish())
}

/// Sequence frame rate, frame size and pixel aspect from `<profile>`.
fn profile(root: Node<'_, '_>, report: &mut Report) -> (FrameRate, (u32, u32), (u32, u32)) {
    let Some(p) = root.descendants().find(|n| tag(*n) == "profile") else {
        report.info("the document has no <profile>: read with MLT's default, DV PAL (720×576 at 25 fps)");
        return (FrameRate::FPS_25, (720, 576), (59, 54));
    };
    let int = |name: &str| prop(p, name).and_then(|v| v.trim().parse::<i64>().ok());
    let rate = match (int("frame_rate_num"), int("frame_rate_den")) {
        // 1 to 1000 frames per second
        (Some(n), Some(d)) if d > 0 && n >= d && n <= d.saturating_mul(1000) => FrameRate::new(n, d),
        _ => {
            report.warn("the profile's frame rate is missing or out of range: read as 25 fps");
            FrameRate::FPS_25
        }
    };
    let dim = |name: &str| int(name).filter(|v| (16..=16_384).contains(v)).and_then(|v| u32::try_from(v).ok());
    let size = match (dim("width"), dim("height")) {
        (Some(w), Some(h)) => (w, h),
        _ => {
            report.warn("the profile's frame size is missing or out of range: read as 1920×1080");
            (1920, 1080)
        }
    };
    let aspect = |name: &str| int(name).filter(|v| (1..=1000).contains(v)).and_then(|v| u32::try_from(v).ok());
    let par = match (aspect("sample_aspect_num"), aspect("sample_aspect_den")) {
        (Some(n), Some(d)) => (n, d),
        _ => (1, 1),
    };
    (rate, size, par)
}

impl<'a, 'i> Imp<'a, 'i, '_> {
    fn time(&self, n: Node<'_, '_>, name: &str) -> Option<i64> {
        prop(n, name).and_then(|v| parse_time(v, self.rate))
    }

    /// The tracks a tractor plays, in MLT order (the first is the bottom of the stack). Tracks
    /// naming an undefined producer are left out ([`Imp::undefined_tracks`] reports them).
    fn tracks_of(&self, t: Node<'a, 'i>) -> Vec<TrackRef<'a, 'i>> {
        let mut out = Vec::new();
        for c in elements(t) {
            match tag(c) {
                "track" => out.extend(self.track_ref(c)),
                "multitrack" => {
                    for m in elements(c) {
                        if tag(m) == "track" {
                            out.extend(self.track_ref(m));
                        } else if is_producer_like(m) {
                            out.push(TrackRef { node: m, hide: None });
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn track_ref(&self, track: Node<'a, 'i>) -> Option<TrackRef<'a, 'i>> {
        let node = match track.attribute("producer") {
            Some(id) => self.by_id.get(id).copied(),
            None => elements(track).find(|n| is_producer_like(*n)),
        }?;
        Some(TrackRef { node, hide: track.attribute("hide") })
    }

    /// A tractor whose tracks are all playlists: one timeline track (Kdenlive's tracks).
    fn is_track_tractor(&self, t: Node<'a, 'i>) -> bool {
        let tracks = self.tracks_of(t);
        !tracks.is_empty() && tracks.iter().all(|r| tag(r.node) == "playlist")
    }

    /// The black background under the timeline (Kdenlive's `black_track`): not an edit.
    fn is_background(&self, index: usize, n: Node<'a, 'i>) -> bool {
        if tag(n) != "playlist" {
            return false;
        }
        if n.attribute("id") == Some("black_track") {
            return true;
        }
        let mut entries = elements(n).filter(|e| tag(*e) == "entry").peekable();
        index == 0
            && entries.peek().is_some()
            && entries.all(|e| {
                e.attribute("producer")
                    .and_then(|id| self.by_id.get(id))
                    .is_some_and(|p| is_color(*p) && prop(*p, "resource").and_then(parse_rgba) == Some(OPAQUE_BLACK))
            })
    }

    fn undefined_tracks(&mut self, root: Node<'a, 'i>) {
        for t in root.descendants().filter(|n| tag(*n) == "track") {
            if let Some(id) = t.attribute("producer")
                && !self.by_id.contains_key(id)
            {
                self.report.warn(format!("a track plays \"{id}\", which the document does not define: skipped"));
            }
        }
    }

    /// The timeline (tractor, or a lone playlist) and its tracks.
    fn timeline(&self, root: Node<'a, 'i>) -> Option<(Node<'a, 'i>, Vec<TrackRef<'a, 'i>>)> {
        let named = root.attribute("producer").and_then(|id| self.by_id.get(id)).copied();
        let last = |kind: &str| elements(root).filter(|n| tag(*n) == kind).last().or_else(|| root.descendants().rfind(|n| tag(*n) == kind));
        let Some(mut tl) = named.filter(|n| tag(*n) == "tractor").or_else(|| last("tractor")) else {
            // no tractor: a playlist plays as a timeline of one track
            let p = named.filter(|n| tag(*n) == "playlist").or_else(|| last("playlist"))?;
            return Some((p, vec![TrackRef { node: p, hide: None }]));
        };
        let mut seen = HashSet::from([tl.id()]);
        let mut tracks = self.tracks_of(tl);
        for _ in 0..MAX_DEPTH {
            let real: Vec<Node<'a, 'i>> = tracks.iter().enumerate().filter(|(i, t)| !self.is_background(*i, t.node)).map(|(_, t)| t.node).collect();
            let [inner] = real.as_slice() else { break };
            if tag(*inner) != "tractor" || self.is_track_tractor(*inner) || !seen.insert(inner.id()) {
                break;
            }
            tl = *inner;
            tracks = self.tracks_of(tl);
        }
        Some((tl, tracks))
    }

    /// The clips of the bin: the entries of the playlist `<mlt producer>` names.
    fn bin(&mut self, root: Node<'a, 'i>) {
        let Some(bin) = root.attribute("producer").and_then(|id| self.by_id.get(id)).copied().filter(|n| tag(*n) == "playlist") else { return };
        for e in elements(bin).filter(|e| tag(*e) == "entry") {
            // sequences (tractors) in Kdenlive's bin are the timelines themselves
            let Some(p) = e.attribute("producer").and_then(|id| self.by_id.get(id)).copied().filter(|p| matches!(tag(*p), "producer" | "chain")) else {
                continue;
            };
            if let Source::Skipped(what) = self.source(p) {
                self.report.warn(format!("{what} \"{}\" in the bin was not imported", clip_name(p)));
            }
        }
    }

    fn sequence(&mut self, name: &str, settings: SequenceSettings, tracks: &[TrackRef<'a, 'i>]) {
        let id = self.b.reserve_sequence(name, settings.clone(), None);
        let mut read = Vec::new();
        for (i, t) in tracks.iter().enumerate() {
            if !self.is_background(i, t.node)
                && let Some(rt) = self.track(t)
            {
                read.push(rt);
            }
        }
        let (video, audio): (Vec<ReadTrack>, Vec<ReadTrack>) = read.into_iter().partition(|t| t.kind == TrackKind::Video);
        let mut seq = empty_sequence(settings);
        let mut overlapping = Vec::new();
        // video from the bottom of the stack up; Kdenlive's audio from the track nearest the video
        let audio: Vec<ReadTrack> = if self.kdenlive { audio.into_iter().rev().collect() } else { audio };
        for rt in video.into_iter().chain(audio) {
            let kind = rt.kind;
            let index = seq.tracks(kind).len();
            let mut track = self.b.track(kind, index);
            if let Some(n) = rt.name {
                track.name = n;
            }
            track.enabled = rt.enabled;
            track.muted = rt.muted;
            track.locked = rt.locked;
            track.items = rt.items;
            seq.tracks_mut(kind).push(track);
            overlapping.extend(rt.overlapping.into_iter().map(|c| (kind, index, c)));
        }
        for (kind, index, clip) in overlapping {
            let (name, frame) = (clip.name.clone(), self.rate.frame_at(clip.start));
            let at = self.b.place(&mut seq, kind, index.saturating_add(1), clip);
            let on = seq.tracks(kind).get(at).map(|t| t.name.clone()).unwrap_or_default();
            self.report.warn(format!("clip \"{name}\" at frame {frame} overlaps another on its track (a same-track mix): put on {on}"));
        }
        self.link(&mut seq);
        self.b.ensure_tracks(&mut seq, TrackKind::Video, 1);
        self.b.ensure_tracks(&mut seq, TrackKind::Audio, 1);
        self.b.put_sequence(id, seq);
        self.b.top.push(id);
    }

    fn track(&mut self, t: &TrackRef<'a, 'i>) -> Option<ReadTrack> {
        let playlists = match tag(t.node) {
            "playlist" => vec![TrackRef { node: t.node, hide: None }],
            "tractor" if self.is_track_tractor(t.node) => self.tracks_of(t.node),
            other => {
                let id = t.node.attribute("id").unwrap_or("");
                let what = if other == "tractor" { "nested timeline" } else { "single clip" };
                self.report.warn(format!("a track playing a {what} (\"{id}\") is not supported and was skipped"));
                return None;
            }
        };
        let hides = |what: &str| t.hides(what) || playlists.iter().all(|p| p.hides(what));
        let nodes: Vec<Node<'a, 'i>> = std::iter::once(t.node).chain(playlists.iter().map(|p| p.node)).collect();
        let flag = |name: &str| nodes.iter().any(|n| prop(*n, name).is_some_and(|v| v.trim() == "1"));
        let audio = if self.kdenlive { flag("kdenlive:audio_track") } else { hides("video") && !hides("audio") };
        let kind = if audio { TrackKind::Audio } else { TrackKind::Video };
        let mut rt = ReadTrack {
            kind,
            name: nodes.iter().find_map(|n| prop(*n, "kdenlive:track_name")).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string),
            enabled: kind == TrackKind::Audio || !hides("video"),
            muted: kind == TrackKind::Audio && hides("audio"),
            locked: flag("kdenlive:locked_track"),
            items: Vec::new(),
            overlapping: Vec::new(),
        };
        for p in &playlists {
            self.playlist(p.node, &mut rt);
        }
        Some(rt)
    }

    fn playlist(&mut self, pl: Node<'a, 'i>, rt: &mut ReadTrack) {
        let mut pos: i64 = 0;
        for c in elements(pl) {
            match tag(c) {
                "blank" => match self.time(c, "length") {
                    Some(len) => pos = pos.saturating_add(len),
                    None => self.report.warn(format!("a blank at frame {pos} has no valid length and was ignored")),
                },
                "entry" => {
                    let id = c.attribute("producer").unwrap_or("");
                    let target = self.by_id.get(id).copied().or_else(|| elements(c).find(|n| is_producer_like(*n)));
                    self.entry(c, target, id, &mut pos, rt);
                }
                // an abbreviated entry: the producer itself, in place
                "producer" | "chain" => self.entry(c, Some(c), "", &mut pos, rt),
                _ => {}
            }
            if pos > self.max_frames {
                self.report.warn(format!("a track runs past {} days: the rest of it was skipped", MAX_SECONDS / 86_400));
                break;
            }
        }
    }

    fn entry(&mut self, e: Node<'a, 'i>, target: Option<Node<'a, 'i>>, id: &str, pos: &mut i64, rt: &mut ReadTrack) {
        let start = *pos;
        let src_in = self.time(e, "in").or_else(|| target.and_then(|p| self.time(p, "in"))).unwrap_or(0);
        let length_out = |p: Node<'_, '_>| self.time(p, "length").and_then(|l| l.checked_sub(1));
        let Some(src_out) = self.time(e, "out").or_else(|| target.and_then(|p| self.time(p, "out").or_else(|| length_out(p)))) else {
            self.report.warn(format!("a clip at frame {start} has no out point and was skipped"));
            return;
        };
        if src_out < src_in {
            self.report.warn(format!("a clip at frame {start} ends (out {src_out}) before it starts (in {src_in}) and was skipped"));
            return;
        }
        // both are non-negative: no overflow
        let len = src_out - src_in + 1;
        *pos = start.saturating_add(len);
        if *pos > self.max_frames {
            return;
        }
        let Some(p) = target else {
            self.report.warn(format!("a clip at frame {start} plays \"{id}\", which the document does not define: left as a gap"));
            return;
        };
        let name = clip_name(p);
        match self.source(p) {
            Source::Media(item) => {
                let r = self.rate;
                let clip_start = r.tick_of(start);
                let clip = self.b.clip(item, rt.kind, &name, clip_start, r.tick_of(*pos) - clip_start, r.tick_of(src_in));
                if rt.items.iter().any(|o| o.start < clip.end() && clip.start < o.end()) {
                    rt.overlapping.push(clip);
                } else {
                    rt.items.push(clip);
                }
            }
            Source::Skipped(what) => self.report.warn(format!("{what} \"{name}\" at frame {start} was not imported (left as a gap)")),
        }
    }

    /// What a producer is, read once per producer.
    fn source(&mut self, p: Node<'a, 'i>) -> Source {
        if let Some(s) = self.sources.get(&p.id()) {
            return *s;
        }
        let s = self.read_source(p);
        self.sources.insert(p.id(), s);
        s
    }

    fn read_source(&mut self, p: Node<'a, 'i>) -> Source {
        if matches!(tag(p), "tractor" | "playlist") {
            return Source::Skipped("nested timeline");
        }
        let service = prop(p, "mlt_service").map(str::trim).unwrap_or("");
        let resource = prop(p, "resource").map(str::trim).unwrap_or("");
        let duration = self.time(p, "length").filter(|l| (1..=self.max_frames).contains(l)).map(|l| self.rate.tick_of(l));
        let (w, h) = self.size;
        match service {
            "kdenlivetitle" => Source::Skipped("title clip"),
            "timewarp" => Source::Skipped("speed-changed clip (timewarp)"),
            "color" | "colour" => {
                let name = clip_name(p);
                let g = match parse_rgba(resource) {
                    Some(OPAQUE_BLACK) => Generator::BlackVideo,
                    Some(c) => Generator::ColorMatte { color: c.map(|b| f32::from(b) / 255.0) },
                    None => {
                        self.report.warn(format!("the colour \"{resource}\" of clip \"{name}\" is not understood: read as black"));
                        Generator::BlackVideo
                    }
                };
                let spec = MediaSpec { duration, video: Some((w, h, self.rate)), ..Default::default() };
                Source::Media(self.b.generator_media(&format!("color:{resource}"), &name, g, &spec, None))
            }
            _ if resource.is_empty() => Source::Skipped("clip without a media file"),
            // MLT's placeholders for a producer defined elsewhere (`<tractor>`, `<producer>`)
            _ if resource.starts_with('<') => Source::Skipped("nested timeline"),
            _ => {
                let path = resolve_path(resource, self.base_dir.as_deref());
                let kind = kind_from_ext(&path);
                let has = |stream: &str| prop(p, stream).map(str::trim) != Some("-1");
                let spec = MediaSpec {
                    duration,
                    video: (kind != MediaKind::AudioOnly && has("video_index")).then_some((w, h, self.rate)),
                    audio: (kind != MediaKind::Still && has("audio_index")).then_some((48_000, 2)),
                    start_tc: None,
                    kind: Some(kind),
                };
                Source::Media(self.b.file_media(&format!("file:{path}"), file_name(&path), &path, &spec, None))
            }
        }
    }

    /// Link each video clip to the audio clip that plays the same media over the same time.
    fn link(&mut self, seq: &mut Sequence) {
        let key = |c: &TrackItem| (c.item, c.start, c.duration, c.source_in);
        let mut audio: HashMap<_, VecDeque<(usize, usize)>> = HashMap::new();
        for (ti, t) in seq.audio_tracks.iter().enumerate() {
            for (ci, c) in t.items.iter().enumerate() {
                audio.entry(key(c)).or_default().push_back((ti, ci));
            }
        }
        let mut pairs = Vec::new();
        for (ti, t) in seq.video_tracks.iter().enumerate() {
            for (ci, c) in t.items.iter().enumerate() {
                if let Some(a) = audio.get_mut(&key(c)).and_then(VecDeque::pop_front) {
                    pairs.push(((ti, ci), a));
                }
            }
        }
        for ((vt, vc), (at, ac)) in pairs {
            let l = self.b.link_id();
            if let Some(c) = seq.video_tracks.get_mut(vt).and_then(|t| t.items.get_mut(vc)) {
                c.link = Some(l);
            }
            if let Some(c) = seq.audio_tracks.get_mut(at).and_then(|t| t.items.get_mut(ac)) {
                c.link = Some(l);
            }
        }
    }

    /// Report what the document has that was not imported: transitions, filters, guides, clip
    /// markers and bin folders. (Skipped clips are reported where they are met.)
    fn report_skipped(&mut self, root: Node<'a, 'i>) {
        let (mut guides, mut marked_clips, mut folders) = (0usize, 0usize, 0usize);
        for n in root.descendants() {
            match tag(n) {
                what @ ("filter" | "transition") if prop(n, "internal_added").is_none() => {
                    let name =
                        ["kdenlive_id", "mlt_service"].into_iter().find_map(|k| prop(n, k).map(str::trim).filter(|s| !s.is_empty())).unwrap_or("unnamed");
                    let what = if what == "filter" { "effect (filter)" } else { "transition" };
                    self.report.warn(format!("{what} \"{name}\" was not imported"));
                }
                "property" => {
                    let name = n.attribute("name").unwrap_or("");
                    let count = || serde_json::from_str::<Vec<serde_json::Value>>(n.text().unwrap_or("")).map(|v| v.len()).unwrap_or(0);
                    if name.ends_with(".guides") {
                        // Kdenlive may keep the same guides on the document and on the sequence
                        guides = guides.max(count());
                    } else if name == "kdenlive:markers" && count() > 0 {
                        marked_clips += 1;
                    } else if name.starts_with("kdenlive:folder.") {
                        folders += 1;
                    }
                }
                _ => {}
            }
        }
        if guides > 0 {
            self.report.warn(format!("{guides} timeline guide(s) were not imported"));
        }
        if marked_clips > 0 {
            self.report.warn(format!("the markers of {marked_clips} clip(s) were not imported"));
        }
        if folders > 0 {
            self.report.warn(format!("{folders} bin folder(s) were not imported: their clips are at the top of the project"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_counts() {
        assert_eq!(parse_time("0", FrameRate::FPS_25), Some(0));
        assert_eq!(parse_time(" 125 ", FrameRate::FPS_25), Some(125));
        assert_eq!(parse_time("2147483647", FrameRate::FPS_30), Some(2_147_483_647));
        for bad in ["", "-1", "+5", "1.5", "abc", "99999999999999999999999", "1e3"] {
            assert_eq!(parse_time(bad, FrameRate::FPS_25), None, "{bad:?}");
        }
    }

    #[test]
    fn clock_values() {
        let r30 = FrameRate::FPS_30;
        assert_eq!(parse_time("00:00:00.000", r30), Some(0));
        assert_eq!(parse_time("00:00:01.000", r30), Some(30));
        // the producer out point of the issue's snippet: 366.1 s at 30 fps
        assert_eq!(parse_time("00:06:06.100", r30), Some(10_983));
        assert_eq!(parse_time("01:00:00.000", FrameRate::FPS_25), Some(90_000));
        // MM:SS.mmm and a decimal comma (LC_NUMERIC other than C)
        assert_eq!(parse_time("01:02.500", FrameRate::FPS_24), Some(62 * 24 + 12));
        assert_eq!(parse_time("00:00:02,000", FrameRate::FPS_24), Some(48));
        // rounded to the nearest frame, exactly at NTSC rates
        assert_eq!(parse_time("00:00:01.001", FrameRate::FPS_29_97), Some(30));
        assert_eq!(parse_time("00:00:00.033", FrameRate::FPS_30), Some(1));
        assert_eq!(parse_time("00:00:00.016", FrameRate::FPS_30), Some(0));
        assert_eq!(parse_time("00:01:00.060", FrameRate::FPS_23_976), Some(1440));
        // more digits than nanoseconds change nothing
        assert_eq!(parse_time("00:00:01.0000000001", r30), Some(30));
        for bad in ["00:00:.5", "00:x0:01.000", "00:00:01.0a", "00:00:00:00.5", "1:2:3:4:5", "::", "-00:00:01.000"] {
            assert_eq!(parse_time(bad, r30), None, "{bad:?}");
        }
    }

    #[test]
    fn smpte_timecode() {
        assert_eq!(parse_time("00:00:01:05", FrameRate::FPS_25), Some(30));
        assert_eq!(parse_time("01:00:00:00", FrameRate::FPS_24), Some(86_400));
        // drop-frame: 00:01:00;02 is the first label after the two dropped at the minute
        assert_eq!(parse_time("00:01:00;02", FrameRate::FPS_29_97), Some(1800));
        assert_eq!(parse_time("00:10:00;00", FrameRate::FPS_29_97), Some(17_982));
        // non-drop at the same rate counts every label
        assert_eq!(parse_time("00:01:00:02", FrameRate::FPS_29_97), Some(1802));
        assert_eq!(parse_time("00:00:01;xx", FrameRate::FPS_29_97), None);
    }

    #[test]
    fn colours() {
        assert_eq!(parse_rgba("0xff0000ff"), Some([255, 0, 0, 255]));
        assert_eq!(parse_rgba("0x00ff00"), Some([0, 255, 0, 255]));
        assert_eq!(parse_rgba("#800000ff"), Some([0, 0, 255, 128]));
        assert_eq!(parse_rgba("#ffffff"), Some([255, 255, 255, 255]));
        assert_eq!(parse_rgba("black"), Some(OPAQUE_BLACK));
        for bad in ["", "0x", "0xfff", "#ggg000", "0x+f+f+f", "purple-ish", "0xé0"] {
            assert_eq!(parse_rgba(bad), None, "{bad:?}");
        }
    }
}
