//! FCPXML (1.9–1.11) import/export.
//!
//! Mapping between FilmCraft's track model and FCPXML's magnetic model:
//! - V1 is the primary storyline (`spine`): clips, `gap`s and `transition`s.
//! - Other video tracks become connected clips with `lane = track index` (V2 → lane 1); audio-only
//!   items on audio track n become connected clips on lane `-n`. A V1/V2… clip whose linked audio
//!   sits on the same-numbered audio track with identical timing is written as one clip carrying
//!   both (`srcEnable="all"`), otherwise `srcEnable="video"`/`"audio"`.
//! - Rational times (`1001/24000s`) map exactly onto ticks.
//! - `adjust-transform` (position in percent of frame height, FCP y-up, counter-clockwise
//!   rotation), `adjust-blend` (opacity), `adjust-volume` (dB) and their keyframes are mapped;
//!   constant speed, reverse and freeze frames use a two-point `timeMap`.
//! - Markers on primary-storyline elements become sequence markers; markers on connected clips stay
//!   clip markers. Transitions are supported on the primary storyline (and read from secondary
//!   storylines); transitions elsewhere are reported.

use std::collections::HashMap;

use filmcraft_geom::Vec2;
use filmcraft_media::MediaKind;
use filmcraft_project::{
    BinId, ItemId, ItemKind, Keyframe, Label, Marker, MarkerId, MarkerKind, Param, ParamValue, Project, Sequence, TrackItem, TrackKind, Transition,
    TransitionAlign, TransitionId, effect::find_effect_by_name,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use roxmltree::Node;

use crate::common::{
    Builder, MediaSpec, base_item, db_to_gain, empty_sequence, file_name, gain_to_db, generator_of, item_media, item_path, kind_from_ext, param,
    param_modified, path_to_file_url, resolve_path, scaled, set_param, settings_for, standard_effects, transition_effect, transition_name,
};
use crate::xml::{child, children, elements, num};
use crate::{Error, ExportOptions, Format, ImportOptions, Imported, Report, Result};

// ---------------------------------------------------------------------------------------------
// Rational time
// ---------------------------------------------------------------------------------------------

fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// Parse an FCPXML time value (`"1001/24000s"`, `"5s"`, `"0s"`, `"3.5s"`) into ticks.
pub(crate) fn parse_time(s: &str) -> Option<Tick> {
    let s = s.trim().strip_suffix('s').unwrap_or(s.trim());
    if s.is_empty() {
        return None;
    }
    if let Some((a, b)) = s.split_once('/') {
        let a: i128 = a.trim().parse().ok()?;
        let b: i128 = b.trim().parse().ok()?;
        if b == 0 {
            return None;
        }
        let n = a * TICKS_PER_SECOND as i128;
        // Exact for every broadcast rate; round to the nearest tick otherwise.
        let q = n.div_euclid(b);
        let r = n.rem_euclid(b);
        return Some(Tick((if 2 * r >= b.abs() { q + 1 } else { q }) as i64));
    }
    if let Ok(i) = s.parse::<i64>() {
        return Some(Tick(i * TICKS_PER_SECOND));
    }
    s.parse::<f64>().ok().map(Tick::from_seconds_f64)
}

/// Format ticks as an FCPXML rational time, frame-based at `rate` when the value is on a frame.
pub(crate) fn fmt_time(t: Tick, rate: FrameRate) -> String {
    if t == Tick::ZERO {
        return "0s".into();
    }
    let f = rate.frame_at(t);
    if rate.tick_of(f) == t {
        let num = f as i128 * rate.den as i128;
        let den = rate.num as i128;
        if num % den == 0 {
            return format!("{}s", num / den);
        }
        return format!("{num}/{den}s");
    }
    let g = gcd(t.0 as i128, TICKS_PER_SECOND as i128);
    let (a, b) = (t.0 as i128 / g, TICKS_PER_SECOND as i128 / g);
    if b == 1 { format!("{a}s") } else { format!("{a}/{b}s") }
}

fn attr_time(n: Node, name: &str) -> Option<Tick> {
    n.attribute(name).and_then(parse_time)
}

fn rate_from_frame_duration(s: &str) -> Option<FrameRate> {
    let s = s.trim().strip_suffix('s')?;
    let (a, b) = s.split_once('/').unwrap_or((s, "1"));
    let (a, b): (i64, i64) = (a.parse().ok()?, b.parse().ok()?);
    (a > 0).then(|| FrameRate::new(b, a))
}

fn audio_rate_str(sr: u32) -> String {
    if sr.is_multiple_of(1000) { format!("{}k", sr / 1000) } else { format!("{}k", sr as f64 / 1000.0) }
}

fn parse_audio_rate(s: &str) -> Option<u32> {
    let s = s.trim();
    match s.strip_suffix('k') {
        Some(k) => k.parse::<f64>().ok().map(|v| (v * 1000.0).round() as u32),
        None => s.parse().ok(),
    }
}

// ---------------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct Asset {
    item: ItemId,
    start: Tick,
    has_video: bool,
    has_audio: bool,
}

struct Imp<'a, 'i, 'r> {
    b: Builder,
    opts: &'a ImportOptions,
    report: &'r mut Report,
    formats: HashMap<String, (FrameRate, u32, u32)>,
    asset_nodes: HashMap<String, Node<'a, 'i>>,
    assets: HashMap<String, Asset>,
    media_nodes: HashMap<String, Node<'a, 'i>>,
    media_items: HashMap<String, ItemId>,
    effects: HashMap<String, String>,
}

/// Placement context of a storyline element.
struct Ctx {
    seq_rate: FrameRate,
    frame: (u32, u32),
}

pub(crate) fn import(text: &str, opts: &ImportOptions, report: &mut Report) -> Result<Imported> {
    let doc = crate::xml::parse(text).map_err(|e| Error::parse(Format::Fcpxml, e))?;
    let root = doc.root_element();
    if root.tag_name().name() != "fcpxml" {
        return Err(Error::parse(Format::Fcpxml, "root element is not <fcpxml>"));
    }
    if let Some(v) = root.attribute("version") {
        let minor = v.split('.').nth(1).and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
        if !v.starts_with("1.") || minor > 11 {
            report.info(format!("FCPXML version {v} is newer than 1.11; unknown elements were ignored"));
        }
    }
    let lib_name = child(root, "library").and_then(|l| child(l, "event")).and_then(|e| e.attribute("name")).unwrap_or("Imported FCPXML");
    let mut imp = Imp {
        b: Builder::new(lib_name),
        opts,
        report,
        formats: HashMap::new(),
        asset_nodes: HashMap::new(),
        assets: HashMap::new(),
        media_nodes: HashMap::new(),
        media_items: HashMap::new(),
        effects: HashMap::new(),
    };
    if let Some(res) = child(root, "resources") {
        for r in elements(res) {
            let Some(id) = r.attribute("id") else { continue };
            match r.tag_name().name() {
                "format" => {
                    let rate = r.attribute("frameDuration").and_then(rate_from_frame_duration).unwrap_or_default();
                    let w = r.attribute("width").and_then(|v| v.parse().ok()).unwrap_or(1920);
                    let h = r.attribute("height").and_then(|v| v.parse().ok()).unwrap_or(1080);
                    imp.formats.insert(id.into(), (rate, w, h));
                }
                "asset" => {
                    imp.asset_nodes.insert(id.into(), r);
                }
                "media" => {
                    imp.media_nodes.insert(id.into(), r);
                }
                "effect" => {
                    imp.effects.insert(id.into(), r.attribute("name").unwrap_or("").to_string());
                }
                _ => {}
            }
        }
    }
    let mut events: Vec<Node> = Vec::new();
    if let Some(lib) = child(root, "library") {
        events.extend(children(lib, "event"));
    }
    events.extend(children(root, "event"));
    for ev in events {
        let bin = imp.b.bin(ev.attribute("name").unwrap_or("Event"), None);
        for c in elements(ev) {
            match c.tag_name().name() {
                "project" => {
                    if let Some(s) = child(c, "sequence") {
                        let name = c.attribute("name").unwrap_or("Project");
                        let id = imp.sequence(s, name, Some(bin));
                        imp.b.top.push(id);
                    }
                }
                "asset-clip" | "clip" => {
                    if let Some(r) = c.attribute("ref") {
                        imp.asset(r, Some(bin));
                    }
                }
                _ => {}
            }
        }
    }
    for p in children(root, "project") {
        if let Some(s) = child(p, "sequence") {
            let id = imp.sequence(s, p.attribute("name").unwrap_or("Project"), None);
            imp.b.top.push(id);
        }
    }
    if imp.b.p.items.is_empty() {
        return Err(Error::Empty);
    }
    Ok(imp.b.finish())
}

fn flag(n: Node, name: &str) -> Option<bool> {
    n.attribute(name).map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

impl<'a, 'i> Imp<'a, 'i, '_> {
    fn asset(&mut self, id: &str, bin: Option<BinId>) -> Option<Asset> {
        if let Some(a) = self.assets.get(id) {
            return Some(a.clone());
        }
        let n = *self.asset_nodes.get(id)?;
        let name = n.attribute("name").unwrap_or("Asset").to_string();
        let src = child(n, "media-rep").and_then(|m| m.attribute("src")).or_else(|| n.attribute("src"));
        let fmt = n.attribute("format").and_then(|f| self.formats.get(f)).copied();
        let mut has_video = flag(n, "hasVideo").unwrap_or(false);
        let has_audio = flag(n, "hasAudio").unwrap_or(false);
        if !has_video && !has_audio {
            has_video = kind_from_ext(src.unwrap_or(&name)) != MediaKind::AudioOnly;
        }
        let start = attr_time(n, "start").unwrap_or(Tick::ZERO);
        let rate = fmt.map(|f| f.0).unwrap_or_default();
        let spec = MediaSpec {
            duration: attr_time(n, "duration"),
            video: has_video.then(|| fmt.map(|(r, w, h)| (w, h, r)).unwrap_or((1920, 1080, rate))),
            audio: has_audio.then(|| {
                (n.attribute("audioRate").and_then(parse_audio_rate).unwrap_or(48_000), n.attribute("audioChannels").and_then(|c| c.parse().ok()).unwrap_or(2))
            }),
            start_tc: (start != Tick::ZERO).then(|| rate.frame_at(start)),
            kind: None,
        };
        let item = match src {
            Some(url) => {
                let p = resolve_path(url, self.opts.base_dir.as_deref());
                self.b.file_media(&format!("file:{p}"), &name, &p, &spec, bin)
            }
            None => {
                self.report.warn(format!("asset \"{name}\" has no media; imported offline"));
                let p = resolve_path(&name, self.opts.base_dir.as_deref());
                let it = self.b.file_media(&format!("asset:{id}"), &name, &p, &spec, bin);
                if let Some(m) = self.b.p.item_mut(it).and_then(|i| i.as_media_mut()) {
                    m.offline = true;
                }
                it
            }
        };
        let a = Asset { item, start, has_video, has_audio };
        self.assets.insert(id.into(), a.clone());
        Some(a)
    }

    /// Nested sequence from a `<media>` resource.
    fn media(&mut self, id: &str) -> Option<ItemId> {
        if let Some(i) = self.media_items.get(id) {
            return Some(*i);
        }
        let n = *self.media_nodes.get(id)?;
        let s = child(n, "sequence")?;
        // Reserve first so recursion terminates.
        let name = n.attribute("name").unwrap_or("Compound Clip").to_string();
        let item = self.sequence(s, &name, None);
        self.media_items.insert(id.into(), item);
        Some(item)
    }

    fn sequence(&mut self, s: Node<'a, 'i>, name: &str, bin: Option<BinId>) -> ItemId {
        let (rate, w, h) = s.attribute("format").and_then(|f| self.formats.get(f)).copied().unwrap_or((FrameRate::default(), 1920, 1080));
        let df = s.attribute("tcFormat") == Some("DF");
        let mut settings = settings_for(rate, w, h, df);
        if let Some(sr) = s.attribute("audioRate").and_then(parse_audio_rate) {
            settings.sample_rate = sr;
        }
        let id = self.b.reserve_sequence(name, settings.clone(), bin);
        let mut seq = empty_sequence(settings);
        let tc_start = attr_time(s, "tcStart").unwrap_or(Tick::ZERO);
        seq.start_timecode = rate.frame_at(tc_start);
        let ctx = Ctx { seq_rate: rate, frame: (w, h) };
        self.b.ensure_tracks(&mut seq, TrackKind::Video, 1);
        self.b.ensure_tracks(&mut seq, TrackKind::Audio, 1);
        if let Some(spine) = child(s, "spine") {
            // Sequence time origin is tcStart.
            self.storyline(&mut seq, &ctx, spine, -tc_start, 0, true);
        }
        seq.markers.sort_by_key(|m| m.start);
        self.b.put_sequence(id, seq);
        id
    }

    /// Lay out a storyline. `origin` maps the storyline's offsets to timeline time
    /// (`timeline = origin + offset`). `lane` is the storyline's lane (0 = primary).
    fn storyline(&mut self, seq: &mut Sequence, ctx: &Ctx, spine: Node<'a, 'i>, origin: Tick, lane: i32, primary: bool) {
        // (timeline start, timeline end, video clip on this storyline's track)
        let mut laid: Vec<(Tick, Tick, Option<filmcraft_project::ClipId>)> = Vec::new();
        let mut transitions: Vec<(usize, Node)> = Vec::new();
        for el in elements(spine) {
            let tag = el.tag_name().name();
            if tag == "transition" {
                transitions.push((laid.len(), el));
                continue;
            }
            let offset = attr_time(el, "offset").unwrap_or(Tick::ZERO);
            let t0 = origin + offset;
            let (end, clip) = self.element(seq, ctx, el, t0, lane, primary);
            laid.push((t0, end, clip));
        }
        let vidx = if lane >= 0 { lane as usize } else { usize::MAX };
        for (pos, el) in transitions {
            if vidx == usize::MAX {
                self.report.warn("transitions on audio storylines are not supported");
                continue;
            }
            let start = origin + attr_time(el, "offset").unwrap_or(Tick::ZERO);
            let dur = attr_time(el, "duration").unwrap_or(Tick::ZERO);
            if dur <= Tick::ZERO {
                continue;
            }
            let name = child(el, "filter-video")
                .and_then(|f| f.attribute("ref").and_then(|r| self.effects.get(r).cloned()).or_else(|| f.attribute("name").map(str::to_string)))
                .filter(|n| !n.is_empty())
                .or_else(|| el.attribute("name").map(str::to_string))
                .unwrap_or_else(|| "Cross Dissolve".into());
            let prev = pos.checked_sub(1).and_then(|i| laid.get(i)).copied();
            let next = laid.get(pos).copied();
            let from = prev.and_then(|p| p.2.map(|c| (c, p.1)));
            let to = next.and_then(|n| n.2.map(|c| (c, n.0)));
            if from.is_none() && to.is_none() {
                continue;
            }
            let cut = to.map(|t| t.1).or(from.map(|f| f.1)).unwrap_or(start);
            let align = if cut == start {
                TransitionAlign::StartAtCut
            } else if cut == start + dur {
                TransitionAlign::EndAtCut
            } else {
                TransitionAlign::CenterAtCut
            };
            let effect = transition_effect(&name, false, self.report);
            let id = TransitionId(self.b.alloc());
            // The clips may have been moved to another track by placement; find the track holding them.
            let clip = from.or(to).map(|c| c.0);
            if let Some(t) = seq.video_tracks.iter_mut().find(|t| clip.is_some_and(|c| t.item(c).is_some())) {
                t.transitions.push(Transition { id, effect, start, duration: dur, from: from.map(|f| f.0), to: to.map(|t| t.0), align, reverse: false });
                t.sort();
            }
        }
    }

    /// Lay out one storyline element at timeline time `t0`. Returns (timeline end, video clip).
    fn element(&mut self, seq: &mut Sequence, ctx: &Ctx, el: Node<'a, 'i>, t0: Tick, lane: i32, primary: bool) -> (Tick, Option<filmcraft_project::ClipId>) {
        let tag = el.tag_name().name();
        let dur = attr_time(el, "duration").unwrap_or(Tick::ZERO);
        let local_start = attr_time(el, "start").unwrap_or(Tick::ZERO);
        let mut vclip = None;
        let mut placed: Vec<(TrackKind, filmcraft_project::ClipId)> = Vec::new();
        let name = el.attribute("name").unwrap_or("Clip").to_string();
        // Media and source in.
        let media: Option<(ItemId, Tick, bool, bool)> = match tag {
            "asset-clip" => el.attribute("ref").and_then(|r| self.asset(r, None)).map(|a| (a.item, a.start, a.has_video, a.has_audio)),
            "clip" => {
                // A clip wraps video/audio/asset-clip children that reference the asset.
                let inner = elements(el).find(|c| matches!(c.tag_name().name(), "video" | "audio" | "asset-clip") && c.attribute("ref").is_some());
                inner.and_then(|c| {
                    let a = self.asset(c.attribute("ref").unwrap_or(""), None)?;
                    // inner local time = inner.start + (t - inner.offset)
                    let shift = attr_time(c, "start").unwrap_or(Tick::ZERO) - attr_time(c, "offset").unwrap_or(Tick::ZERO);
                    let v = a.has_video && c.tag_name().name() != "audio";
                    let au = a.has_audio && (c.tag_name().name() != "video" || elements(el).any(|x| x.has_tag_name("audio")));
                    Some((a.item, a.start - shift, v, au))
                })
            }
            "ref-clip" => el.attribute("ref").and_then(|r| {
                let item = self.media(r)?;
                let tc = self.b.p.sequence(item).map(|s| s.settings.frame_rate.tick_of(s.start_timecode)).unwrap_or(Tick::ZERO);
                Some((item, tc, true, self.b.p.sequence(item).is_some_and(|s| s.audio_tracks.iter().any(|t| !t.items.is_empty()))))
            }),
            "gap" => None,
            "title" | "video" | "audio" | "sync-clip" | "mc-clip" | "live-drawing" => {
                self.report.warn(format!("FCPXML <{tag}> elements are not supported and were skipped"));
                None
            }
            _ => None,
        };
        let (speed, reverse, hold, src_base) = time_map(el, local_start);
        if let Some((item, origin, has_v, has_a)) = media
            && dur > Tick::ZERO
        {
            let src_in = src_base - origin;
            let src_enable = el.attribute("srcEnable").unwrap_or("all");
            let want_v = has_v && src_enable != "audio" && lane >= 0;
            let want_a = has_a && src_enable != "video";
            let link = if want_v && want_a { Some(self.b.link_id()) } else { None };
            for (kind, want) in [(TrackKind::Video, want_v), (TrackKind::Audio, want_a)] {
                if !want {
                    continue;
                }
                let mut ti = self.b.clip(item, kind, &name, t0, dur, src_in.max(Tick::ZERO));
                ti.speed = speed;
                ti.reverse = reverse;
                if hold {
                    ti.frame_hold = Some(src_in.max(Tick::ZERO));
                }
                ti.link = link;
                ti.enabled = flag(el, "enabled").unwrap_or(true);
                if let Some(l) = el.attribute("label").and_then(Label::from_name) {
                    ti.label = l;
                }
                self.adjustments(&mut ti, el, local_start, src_in, speed, ctx, kind);
                if !primary || kind == TrackKind::Audio {
                    ti.markers = self.markers(el, local_start, src_in, speed);
                }
                let pref = match kind {
                    TrackKind::Video => lane.max(0) as usize,
                    TrackKind::Audio => {
                        if lane < 0 {
                            (-lane - 1) as usize
                        } else {
                            lane as usize
                        }
                    }
                };
                let id = ti.id;
                self.b.place(seq, kind, pref, ti);
                placed.push((kind, id));
                if kind == TrackKind::Video {
                    vclip = Some(id);
                }
            }
        }
        if primary {
            // Markers on primary storyline elements are timeline markers.
            for m in self.markers(el, local_start, Tick::ZERO, 1.0) {
                seq.markers.push(Marker { start: t0 + m.start, ..m });
            }
        }
        // Connected clips and secondary storylines.
        for c in elements(el) {
            let Some(l) = c.attribute("lane").and_then(|l| l.parse::<i32>().ok()) else { continue };
            let off = attr_time(c, "offset").unwrap_or(Tick::ZERO);
            let ct0 = t0 + (off - local_start);
            let clane = if lane > 0 && l > 0 { lane + l } else { l };
            if c.has_tag_name("spine") {
                // Secondary storyline: children offsets are in the storyline's local time.
                let first = elements(c).filter(|x| !x.has_tag_name("transition")).find_map(|x| attr_time(x, "offset")).unwrap_or(Tick::ZERO);
                self.storyline(seq, ctx, c, ct0 - first, clane, false);
            } else {
                self.element(seq, ctx, c, ct0, clane, false);
            }
        }
        let _ = placed;
        (t0 + dur, vclip)
    }

    fn markers(&mut self, el: Node, local_start: Tick, src_in: Tick, speed: f64) -> Vec<Marker> {
        let mut out = Vec::new();
        for m in elements(el).filter(|m| matches!(m.tag_name().name(), "marker" | "chapter-marker")) {
            let rel = attr_time(m, "start").unwrap_or(Tick::ZERO) - local_start;
            let start = if src_in == Tick::ZERO && speed == 1.0 { rel } else { src_in + scaled(rel, speed) };
            let d = attr_time(m, "duration").unwrap_or(Tick::ZERO);
            out.push(Marker {
                id: MarkerId(self.b.alloc()),
                start,
                duration: if d.0 <= 1 { Tick::ZERO } else { d },
                name: m.attribute("value").unwrap_or("").to_string(),
                comment: m.attribute("note").unwrap_or("").to_string(),
                kind: if m.has_tag_name("chapter-marker") { MarkerKind::Chapter } else { MarkerKind::Comment },
                color: if m.has_tag_name("chapter-marker") { Label::Mango } else { Label::Blue },
            });
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn adjustments(&mut self, ti: &mut TrackItem, el: Node, local_start: Tick, src_in: Tick, speed: f64, ctx: &Ctx, kind: TrackKind) {
        // keyframe time (element-local) -> media time
        let kt = |t: Tick| src_in + scaled(t - local_start, speed);
        let (fw, fh) = (ctx.frame.0 as f64, ctx.frame.1 as f64);
        let _ = ctx.seq_rate;
        for a in elements(el) {
            match (a.tag_name().name(), kind) {
                ("adjust-transform", TrackKind::Video) => {
                    let to_px = move |v: &str| {
                        let (x, y) = pair(v);
                        Vec2::new(fw / 2.0 + x * fh / 100.0, fh / 2.0 - y * fh / 100.0)
                    };
                    if let Some(p) = self.kparam(a, "position", &kt, |v| ParamValue::Vec2(to_px(v)))
                        && (p.is_animated() || p.value.as_vec2().is_some_and(|v| (v.x - fw / 2.0).abs() > 1e-9 || (v.y - fh / 2.0).abs() > 1e-9))
                    {
                        set_param(ti, "motion", "position", p);
                    }
                    let mut nonuniform = false;
                    if let Some(p) = self.kparam(a, "scale", &kt, |v| {
                        let (x, y) = pair(v);
                        nonuniform |= (x - y).abs() > 1e-9;
                        ParamValue::Float(x * 100.0)
                    }) {
                        set_param(ti, "motion", "scale", p);
                    }
                    if nonuniform {
                        self.report.warn("non-uniform scale was imported as uniform scale");
                    }
                    if let Some(p) = self.kparam(a, "rotation", &kt, |v| ParamValue::Float(-v.trim().parse::<f64>().unwrap_or(0.0))) {
                        set_param(ti, "motion", "rotation", p);
                    }
                    if let Some(p) = self.kparam(a, "anchor", &kt, |v| ParamValue::Vec2(to_px(v)))
                        && p.value.as_vec2().is_some_and(|v| (v.x - fw / 2.0).abs() > 1e-9 || (v.y - fh / 2.0).abs() > 1e-9)
                    {
                        set_param(ti, "motion", "anchor", p);
                    }
                }
                ("adjust-blend", TrackKind::Video) => {
                    if let Some(p) = self.kparam(a, "amount", &kt, |v| ParamValue::Float(v.trim().parse::<f64>().unwrap_or(1.0) * 100.0)) {
                        set_param(ti, "opacity", "opacity", p);
                    }
                }
                ("adjust-volume", TrackKind::Audio) => {
                    if let Some(p) = self.kparam(a, "amount", &kt, |v| ParamValue::Float(parse_db(v))) {
                        set_param(ti, "volume", "level", p);
                    }
                }
                ("filter-video", TrackKind::Video) | ("filter-audio", TrackKind::Audio) => {
                    let name =
                        a.attribute("ref").and_then(|r| self.effects.get(r).cloned()).or_else(|| a.attribute("name").map(str::to_string)).unwrap_or_default();
                    match find_effect_by_name(&name).filter(|d| !d.intrinsic) {
                        Some(d) => {
                            let mut inst = d.instance();
                            inst.enabled = flag(a, "enabled").unwrap_or(true);
                            for p in children(a, "param") {
                                let (Some(k), Some(v)) = (p.attribute("key").or(p.attribute("name")), p.attribute("value")) else { continue };
                                if let (Some(pd), Ok(f)) = (d.params.iter().find(|x| x.id == k || x.label.eq_ignore_ascii_case(k)), v.parse::<f64>())
                                    && matches!(pd.default, ParamValue::Float(_))
                                {
                                    inst.params.insert(pd.id.to_string(), Param::new(ParamValue::Float(f)));
                                }
                            }
                            ti.effects.push(inst);
                        }
                        None => self.report.warn(format!("effect \"{name}\" is not supported and was skipped")),
                    }
                }
                _ => {}
            }
        }
    }

    /// A parameter from an adjustment attribute plus optional `<param name><keyframeAnimation>`.
    fn kparam(&mut self, a: Node, name: &str, kt: &dyn Fn(Tick) -> Tick, mut map: impl FnMut(&str) -> ParamValue) -> Option<Param> {
        let attr = a.attribute(name);
        let pnode = children(a, "param").find(|p| p.attribute("name") == Some(name));
        let mut p = Param::new(map(attr.or(pnode.and_then(|p| p.attribute("value")))?));
        if let Some(anim) = pnode.and_then(|p| child(p, "keyframeAnimation")) {
            for k in children(anim, "keyframe") {
                let (Some(t), Some(v)) = (attr_time(k, "time"), k.attribute("value")) else { continue };
                let mut kf = Keyframe::new(kt(t), map(v));
                if k.attribute("interp") == Some("hold") {
                    kf.interp = filmcraft_project::Interpolation::Hold;
                }
                p.keyframes.push(kf);
            }
            p.keyframes.sort_by_key(|k| k.time);
            if let Some(k) = p.keyframes.first() {
                p.value = k.value.clone();
            }
        }
        Some(p)
    }
}

fn pair(v: &str) -> (f64, f64) {
    let mut it = v.split_whitespace().filter_map(|s| s.parse::<f64>().ok());
    let x = it.next().unwrap_or(0.0);
    (x, it.next().unwrap_or(x))
}

fn parse_db(v: &str) -> f64 {
    let s = v.trim();
    let s = s.strip_suffix("dB").or_else(|| s.strip_suffix("db")).unwrap_or(s);
    s.trim().parse().unwrap_or(0.0)
}

/// (speed, reverse, freeze, source time at the element's local start) from a `timeMap`.
fn time_map(el: Node, local_start: Tick) -> (f64, bool, bool, Tick) {
    let Some(tm) = child(el, "timeMap") else { return (1.0, false, false, local_start) };
    let pts: Vec<(Tick, Tick)> = children(tm, "timept").filter_map(|p| Some((attr_time(p, "time")?, attr_time(p, "value")?))).collect();
    if pts.len() < 2 {
        return (1.0, false, false, pts.first().map(|p| p.1).unwrap_or(local_start));
    }
    let (t0, v0) = pts[0];
    let (t1, v1) = pts[pts.len() - 1];
    let dt = (t1 - t0).0 as f64;
    let dv = (v1 - v0).0 as f64;
    let at_start = |s: f64| v0 + Tick(((local_start - t0).0 as f64 * s).round() as i64);
    if dv == 0.0 {
        return (1.0, false, true, v0);
    }
    let s = (dv / dt * 1_000_000.0).round() / 1_000_000.0;
    if s < 0.0 {
        // Reverse: source runs from v0 down; our source_in is the lowest source time used.
        let speed = -s;
        let hi = at_start(s);
        let len = (t1 - local_start).max(Tick::ZERO);
        return (speed, true, false, hi - scaled(len, speed));
    }
    (s, false, false, at_start(s))
}

// ---------------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------------

struct Res {
    id: String,
}

struct Exp<'a, 'r> {
    p: &'a Project,
    report: &'r mut Report,
    formats: Vec<(String, FrameRate, u32, u32)>,
    assets: HashMap<ItemId, Res>,
    medias: HashMap<ItemId, Res>,
    effects: HashMap<String, Res>,
    next: usize,
    /// Resource XML fragments, written before the library.
    res_out: Vec<String>,
    building: Vec<ItemId>,
}

pub(crate) fn export(p: &Project, seq_id: ItemId, opts: &ExportOptions, report: &mut Report) -> Result<String> {
    let seq = p.sequence(seq_id).ok_or(Error::NoSequence(seq_id))?;
    let mut x = Exp {
        p,
        report,
        formats: Vec::new(),
        assets: HashMap::new(),
        medias: HashMap::new(),
        effects: HashMap::new(),
        next: 1,
        res_out: Vec::new(),
        building: Vec::new(),
    };
    let fmt = x.format(seq.settings.frame_rate, seq.settings.width, seq.settings.height);
    let name = opts.name.clone().unwrap_or_else(|| p.item(seq_id).map(|i| i.name.clone()).unwrap_or_default());
    let body = x.sequence_xml(seq_id, &fmt, 4);
    let mut out =
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n<fcpxml version=\"{}\">\n  <resources>\n", opts.fcpxml_version.as_str());
    for r in &x.res_out {
        out.push_str(r);
    }
    out.push_str("  </resources>\n");
    out.push_str(&format!("  <library>\n    <event name=\"{}\">\n", crate::xml::escape(&p.name)));
    out.push_str(&format!("      <project name=\"{}\">\n", crate::xml::escape(&name)));
    out.push_str(&body);
    out.push_str("      </project>\n    </event>\n  </library>\n</fcpxml>\n");
    Ok(out)
}

/// One storyline element being written.
struct El {
    start: Tick,
    end: Tick,
    /// Local time of the element at its start.
    local: Tick,
    xml_open: String,
    children: Vec<String>,
    close: String,
}

impl Exp<'_, '_> {
    fn rid(&mut self) -> String {
        let r = format!("r{}", self.next);
        self.next += 1;
        r
    }

    fn format(&mut self, rate: FrameRate, w: u32, h: u32) -> String {
        if let Some(f) = self.formats.iter().find(|f| f.1 == rate && f.2 == w && f.3 == h) {
            return f.0.clone();
        }
        let id = self.rid();
        self.res_out.push(format!(
            "    <format id=\"{id}\" name=\"FFVideoFormat{h}p{}\" frameDuration=\"{}/{}s\" width=\"{w}\" height=\"{h}\"/>\n",
            rate.label().replace('.', ""),
            rate.den,
            rate.num
        ));
        self.formats.push((id.clone(), rate, w, h));
        id
    }

    fn asset(&mut self, item: ItemId, rate: FrameRate) -> String {
        if let Some(r) = self.assets.get(&item) {
            return r.id.clone();
        }
        let Some(m) = item_media(self.p, item) else {
            self.report.warn("clip without media skipped");
            return String::new();
        };
        let name = self.p.item(item).map(|i| i.name.clone()).unwrap_or_default();
        let fmt = m.info.video.as_ref().map(|v| self.format(m.interpret.frame_rate.unwrap_or(v.frame_rate), v.width, v.height));
        let id = self.rid();
        let mrate = m.info.video.as_ref().map(|v| v.frame_rate).unwrap_or(rate);
        let start = m.info.start_timecode.map(|f| mrate.tick_of(f)).unwrap_or(Tick::ZERO);
        let mut a = format!(
            "    <asset id=\"{id}\" name=\"{}\" start=\"{}\" duration=\"{}\" hasVideo=\"{}\" hasAudio=\"{}\"",
            crate::xml::escape(&name),
            fmt_time(start, mrate),
            fmt_time(m.info.duration, mrate),
            m.info.video.is_some() as u8,
            m.info.has_audio() as u8
        );
        if let Some(f) = fmt {
            a.push_str(&format!(" format=\"{f}\""));
        }
        if let Some(au) = m.info.audio() {
            a.push_str(&format!(" audioSources=\"1\" audioChannels=\"{}\" audioRate=\"{}\"", au.channels, au.sample_rate));
        }
        let path = item_path(self.p, item).unwrap_or("");
        let src = if crate::common::is_absolute(path) { path_to_file_url(path, false) } else { path.to_string() };
        a.push_str(&format!(">\n      <media-rep kind=\"original-media\" src=\"{}\"/>\n    </asset>\n", crate::xml::escape(&src)));
        self.res_out.push(a);
        self.assets.insert(item, Res { id: id.clone() });
        let _ = file_name;
        id
    }

    fn media(&mut self, item: ItemId) -> String {
        if let Some(r) = self.medias.get(&item) {
            return r.id.clone();
        }
        let Some(s) = self.p.sequence(item) else {
            self.report.warn("missing nested sequence skipped");
            return String::new();
        };
        let fmt = self.format(s.settings.frame_rate, s.settings.width, s.settings.height);
        let id = self.rid();
        self.medias.insert(item, Res { id: id.clone() });
        self.building.push(item);
        let body = self.sequence_xml(item, &fmt, 3);
        self.building.pop();
        let name = self.p.item(item).map(|i| i.name.clone()).unwrap_or_default();
        self.res_out.push(format!("    <media id=\"{id}\" name=\"{}\">\n{body}    </media>\n", crate::xml::escape(&name)));
        id
    }

    fn effect(&mut self, name: &str, id_hint: &str) -> String {
        if let Some(r) = self.effects.get(name) {
            return r.id.clone();
        }
        let id = self.rid();
        self.res_out.push(format!("    <effect id=\"{id}\" name=\"{}\" uid=\"filmcraft.{}\"/>\n", crate::xml::escape(name), crate::xml::escape(id_hint)));
        self.effects.insert(name.to_string(), Res { id: id.clone() });
        id
    }

    fn sequence_xml(&mut self, seq_id: ItemId, fmt: &str, indent: usize) -> String {
        let Some(seq) = self.p.sequence(seq_id) else { return String::new() };
        let rate = seq.settings.frame_rate;
        let df = seq.settings.drop_frame && rate.supports_drop_frame();
        let tc0 = rate.tick_of(seq.start_timecode);
        let pad = "  ".repeat(indent);
        let mut o = format!(
            "{pad}<sequence format=\"{fmt}\" duration=\"{}\" tcStart=\"{}\" tcFormat=\"{}\" audioLayout=\"stereo\" audioRate=\"{}\">\n{pad}  <spine>\n",
            fmt_time(seq.duration(), rate),
            fmt_time(tc0, rate),
            if df { "DF" } else { "NDF" },
            audio_rate_str(seq.settings.sample_rate)
        );
        // Which audio items merge into video clips (same index track, linked, identical timing).
        let mut merged: HashMap<filmcraft_project::ClipId, filmcraft_project::ClipId> = HashMap::new();
        for (vi, vt) in seq.video_tracks.iter().enumerate() {
            let Some(at) = seq.audio_tracks.get(vi) else { continue };
            for v in &vt.items {
                if let Some(a) = at.items.iter().find(|a| {
                    a.link.is_some()
                        && a.link == v.link
                        && a.item == v.item
                        && a.start == v.start
                        && a.duration == v.duration
                        && a.source_in == v.source_in
                        && a.speed == v.speed
                        && a.reverse == v.reverse
                        && a.frame_hold == v.frame_hold
                        && a.enabled == v.enabled
                }) {
                    merged.insert(v.id, a.id);
                }
            }
        }
        let merged_audio: std::collections::HashSet<_> = merged.values().copied().collect();
        if seq.all_tracks().skip(1).any(|t| !t.transitions.is_empty()) {
            self.report.warn("FCPXML export keeps transitions on V1 only; transitions on other tracks were dropped");
        }
        // Primary storyline from V1 (gaps fill holes, plus a trailing gap covering connected clips).
        let v1 = seq.video_tracks.first();
        let mut els: Vec<El> = Vec::new();
        let mut trans_after: Vec<(usize, String)> = Vec::new();
        let mut t = Tick::ZERO;
        let push_gap = |els: &mut Vec<El>, a: Tick, b: Tick| {
            els.push(El {
                start: a,
                end: b,
                local: tc0 + a,
                xml_open: format!(
                    "<gap name=\"Gap\" offset=\"{}\" start=\"{}\" duration=\"{}\"",
                    fmt_time(tc0 + a, rate),
                    fmt_time(tc0 + a, rate),
                    fmt_time(b - a, rate)
                ),
                children: vec![],
                close: "gap".into(),
            })
        };
        if let Some(v1) = v1 {
            for it in &v1.items {
                if it.start > t {
                    push_gap(&mut els, t, it.start);
                }
                for tr in v1.transitions.iter().filter(|tr| tr.to == Some(it.id)) {
                    trans_after.push((els.len(), self.transition_xml(tr, tc0, rate)));
                }
                let audio = merged.get(&it.id).and_then(|a| seq.audio_tracks[0].item(*a));
                let el = self.clip_el(it, audio, TrackKind::Video, tc0 + it.start, None, seq, true);
                els.push(el);
                for tr in v1.transitions.iter().filter(|tr| tr.from == Some(it.id) && tr.to.is_none()) {
                    trans_after.push((els.len(), self.transition_xml(tr, tc0, rate)));
                }
                t = it.end();
            }
        }
        let last_start = seq.all_tracks().skip(1).flat_map(|tr| tr.items.iter().map(|i| i.start)).chain(seq.markers.iter().map(|m| m.start)).max();
        if let Some(ls) = last_start
            && ls >= t
        {
            let end = seq.duration().max(ls + rate.frame_duration());
            push_gap(&mut els, t, end);
        }
        // Connected clips.
        for (kind, tracks) in [(TrackKind::Video, &seq.video_tracks), (TrackKind::Audio, &seq.audio_tracks)] {
            for (ti, track) in tracks.iter().enumerate() {
                if kind == TrackKind::Video && ti == 0 {
                    continue;
                }
                let lane = if kind == TrackKind::Video { ti as i32 } else { -(ti as i32 + 1) };
                for it in &track.items {
                    if merged_audio.contains(&it.id) {
                        continue;
                    }
                    let Some(pi) = els.iter().position(|e| e.start <= it.start && it.start < e.end) else {
                        self.report.warn("a connected clip could not be attached and was dropped");
                        continue;
                    };
                    let local = els[pi].local + (it.start - els[pi].start);
                    let audio =
                        if kind == TrackKind::Video { merged.get(&it.id).and_then(|a| seq.audio_tracks.get(ti).and_then(|t| t.item(*a))) } else { None };
                    let el = self.clip_el(it, audio, kind, local, Some(lane), seq, false);
                    let s = self.render_el(&el, 0);
                    els[pi].children.push(s);
                }
            }
        }
        // Sequence markers on the covering primary element.
        for m in &seq.markers {
            match els.iter().position(|e| e.start <= m.start && m.start < e.end) {
                Some(pi) => {
                    let local = els[pi].local + (m.start - els[pi].start);
                    let s = marker_xml(m, local, rate);
                    els[pi].children.push(s);
                }
                None => self.report.warn("a sequence marker past the end of the timeline was dropped"),
            }
        }
        let inner = format!("{pad}    ");
        let mut ti = 0;
        for (i, e) in els.iter().enumerate() {
            while ti < trans_after.len() && trans_after[ti].0 == i {
                o.push_str(&format!("{inner}{}\n", trans_after[ti].1));
                ti += 1;
            }
            o.push_str(&self.render_el(e, indent + 2));
        }
        while ti < trans_after.len() {
            o.push_str(&format!("{inner}{}\n", trans_after[ti].1));
            ti += 1;
        }
        o.push_str(&format!("{pad}  </spine>\n{pad}</sequence>\n"));
        o
    }

    fn render_el(&self, e: &El, indent: usize) -> String {
        let pad = "  ".repeat(indent);
        if e.children.is_empty() {
            return format!("{pad}{}/>\n", e.xml_open);
        }
        let mut s = format!("{pad}{}>\n", e.xml_open);
        for c in &e.children {
            for line in c.lines() {
                s.push_str(&format!("{pad}  {line}\n"));
            }
        }
        s.push_str(&format!("{pad}</{}>\n", e.close));
        s
    }

    fn transition_xml(&mut self, tr: &Transition, tc0: Tick, rate: FrameRate) -> String {
        let name = transition_name(&tr.effect);
        let r = self.effect(&name, &tr.effect.effect);
        format!(
            "<transition name=\"{n}\" offset=\"{}\" duration=\"{}\"><filter-video ref=\"{r}\" name=\"{n}\"/></transition>",
            fmt_time(tc0 + tr.start, rate),
            fmt_time(tr.duration, rate),
            n = crate::xml::escape(&name)
        )
    }

    /// A clip element at `offset` (in its parent's local time).
    #[allow(clippy::too_many_arguments)]
    fn clip_el(&mut self, it: &TrackItem, audio: Option<&TrackItem>, kind: TrackKind, offset: Tick, lane: Option<i32>, seq: &Sequence, primary: bool) -> El {
        let rate = seq.settings.frame_rate;
        let base = base_item(self.p, it.item);
        let is_seq = matches!(self.p.item(base).map(|i| &i.kind), Some(ItemKind::Sequence(_)));
        let generator = generator_of(self.p, base).cloned();
        let lane_attr = lane.map(|l| format!(" lane=\"{l}\"")).unwrap_or_default();
        let name = crate::xml::escape(&it.name);
        if let Some(g) = generator {
            self.report.warn(format!("generator \"{}\" is not supported in FCPXML; written as a gap", g.label()));
            return El {
                start: it.start,
                end: it.end(),
                local: offset,
                xml_open: format!(
                    "<gap name=\"{name}\"{lane_attr} offset=\"{}\" start=\"{}\" duration=\"{}\"",
                    fmt_time(offset, rate),
                    fmt_time(offset, rate),
                    fmt_time(it.duration, rate)
                ),
                children: vec![],
                close: "gap".into(),
            };
        }
        // Local timeline of the element: origin + source time (retimed clips use a timeMap).
        let (tag, r, origin) = if is_seq {
            let r = if self.building.contains(&base) {
                self.report.warn("recursive nested sequence");
                String::new()
            } else {
                self.media(base)
            };
            let origin = self.p.sequence(base).map(|s| s.settings.frame_rate.tick_of(s.start_timecode)).unwrap_or(Tick::ZERO);
            ("ref-clip", r, origin)
        } else {
            let r = match crate::common::uncarried_clip(self.p, it, rate) {
                Some(clip) => {
                    self.report.warn(format!("{clip} is not supported in FCPXML: it is written as a clip without media and is not read back on import"));
                    String::new()
                }
                None => self.asset(base, rate),
            };
            let m = item_media(self.p, base);
            let mrate = m.and_then(|m| m.info.video.as_ref().map(|v| v.frame_rate)).unwrap_or(rate);
            let st = m.and_then(|m| m.info.start_timecode).map(|f| mrate.tick_of(f)).unwrap_or(Tick::ZERO);
            ("asset-clip", r, st)
        };
        let retimed = it.speed != 1.0 || it.reverse || it.frame_hold.is_some();
        let local = origin + it.source_in;
        let has_audio = item_media(self.p, base).is_some_and(|m| m.info.has_audio()) || (is_seq && audio.is_some());
        let has_video = item_media(self.p, base).is_none_or(|m| m.info.video.is_some());
        let src_enable = match (kind, audio.is_some()) {
            (TrackKind::Video, true) => "",
            (TrackKind::Video, false) if has_audio => " srcEnable=\"video\"",
            (TrackKind::Audio, _) if has_video => " srcEnable=\"audio\"",
            _ => "",
        };
        let enabled = if it.enabled { "" } else { " enabled=\"0\"" };
        let mut children = Vec::new();
        if retimed {
            let v0 = origin + it.frame_hold.unwrap_or(it.source_in);
            let (a, b) = if let Some(h) = it.frame_hold {
                (origin + h, origin + h)
            } else if it.reverse {
                (v0 + scaled(it.duration, it.speed), v0)
            } else {
                (v0, v0 + scaled(it.duration, it.speed))
            };
            children.push(format!(
                "<timeMap>\n  <timept time=\"{}\" value=\"{}\" interp=\"linear\"/>\n  <timept time=\"{}\" value=\"{}\" interp=\"linear\"/>\n</timeMap>",
                fmt_time(local, rate),
                fmt_time(a, rate),
                fmt_time(local + it.duration, rate),
                fmt_time(b, rate)
            ));
        }
        // keyframe media time -> element local time
        let to_local = |t: Tick| -> Tick {
            if it.speed == 0.0 || it.frame_hold.is_some() { local } else { local + Tick(((t - it.source_in).0 as f64 / it.speed).round() as i64) }
        };
        if kind == TrackKind::Video {
            self.transform(it, seq, &to_local, &mut children);
        }
        if let Some(a) = audio.or((kind == TrackKind::Audio).then_some(it)) {
            if param_modified(a, "volume", "level") || a.gain_db != 0.0 {
                let g = a.gain_db;
                children.push(kparam_xml(
                    "adjust-volume",
                    "amount",
                    param(a, "volume", "level"),
                    rate,
                    &to_local,
                    |v| format!("{}dB", num(v.as_f64().unwrap_or(0.0) + g)),
                    "0dB",
                ));
            }
            if param_modified(a, "panner", "balance") || param_modified(a, "channel_volume", "left") || param_modified(a, "channel_volume", "right") {
                self.report.warn("pan and channel volume are not exported to FCPXML");
            }
        }
        for e in standard_effects(it) {
            let n = e.def().map(|d| d.name).unwrap_or(&e.effect).to_string();
            let r = self.effect(&n, &e.effect);
            let tag = if kind == TrackKind::Video { "filter-video" } else { "filter-audio" };
            let mut s = format!("<{tag} ref=\"{r}\" name=\"{}\"", crate::xml::escape(&n));
            if !e.enabled {
                s.push_str(" enabled=\"0\"");
            }
            let floats: Vec<_> =
                e.params.iter().filter_map(|(k, p)| p.value.as_f64().filter(|_| matches!(p.value, ParamValue::Float(_))).map(|v| (k, v))).collect();
            if floats.is_empty() {
                s.push_str("/>");
            } else {
                s.push_str(">\n");
                for (k, v) in floats {
                    s.push_str(&format!("  <param name=\"{k}\" key=\"{k}\" value=\"{}\"/>\n", num(v)));
                }
                s.push_str(&format!("</{tag}>"));
            }
            self.report.info(format!("effect \"{n}\" written as an FCPXML filter reference (FilmCraft-specific)"));
            children.push(s);
        }
        if !primary || kind == TrackKind::Audio {
            for m in &it.markers {
                children.push(marker_xml(m, to_local(m.start), rate));
            }
        } else if !it.markers.is_empty() {
            self.report.info("clip markers on V1 are written as primary-storyline markers and import as sequence markers");
            for m in &it.markers {
                children.push(marker_xml(m, to_local(m.start), rate));
            }
        }
        El {
            start: it.start,
            end: it.end(),
            local,
            xml_open: format!(
                "<{tag} ref=\"{r}\"{lane_attr} name=\"{name}\" offset=\"{}\" start=\"{}\" duration=\"{}\"{src_enable}{enabled}",
                fmt_time(offset, rate),
                fmt_time(local, rate),
                fmt_time(it.duration, rate)
            ),
            children,
            close: tag.into(),
        }
    }

    fn transform(&mut self, it: &TrackItem, seq: &Sequence, to_local: &dyn Fn(Tick) -> Tick, out: &mut Vec<String>) {
        let rate = seq.settings.frame_rate;
        let (fw, fh) = (seq.settings.width as f64, seq.settings.height as f64);
        let pos = |v: &ParamValue| {
            let p = v.as_vec2().unwrap_or(Vec2::new(f64::NAN, f64::NAN));
            let x = if p.x.is_nan() { 0.0 } else { (p.x - fw / 2.0) / fh * 100.0 };
            let y = if p.y.is_nan() { 0.0 } else { -(p.y - fh / 2.0) / fh * 100.0 };
            format!("{} {}", num(x), num(y))
        };
        let changed = ["position", "scale", "rotation", "anchor"].iter().any(|p| param_modified(it, "motion", p));
        if changed {
            if param_modified(it, "motion", "scale_width") || param(it, "motion", "uniform_scale").and_then(|p| p.value.as_bool()) == Some(false) {
                self.report.warn("non-uniform scale is not exported to FCPXML");
            }
            let mut attrs = Vec::new();
            let mut params = Vec::new();
            for (id, attr, f) in [
                ("position", "position", &pos as &dyn Fn(&ParamValue) -> String),
                ("scale", "scale", &|v: &ParamValue| {
                    let s = v.as_f64().unwrap_or(100.0) / 100.0;
                    format!("{} {}", num(s), num(s))
                }),
                ("rotation", "rotation", &|v: &ParamValue| num(-v.as_f64().unwrap_or(0.0))),
                ("anchor", "anchor", &pos),
            ] {
                let Some(p) = param(it, "motion", id) else { continue };
                if !param_modified(it, "motion", id) {
                    continue;
                }
                attrs.push(format!(" {attr}=\"{}\"", f(&p.value)));
                if p.is_animated() {
                    let mut s = format!("  <param name=\"{attr}\">\n    <keyframeAnimation>\n");
                    for k in &p.keyframes {
                        let interp = if k.interp == filmcraft_project::Interpolation::Hold { " interp=\"hold\"" } else { "" };
                        s.push_str(&format!("      <keyframe time=\"{}\" value=\"{}\"{interp}/>\n", fmt_time(to_local(k.time), rate), f(&k.value)));
                    }
                    s.push_str("    </keyframeAnimation>\n  </param>");
                    params.push(s);
                }
            }
            out.push(wrap("adjust-transform", &attrs.concat(), &params));
        }
        if param_modified(it, "opacity", "opacity") {
            out.push(kparam_xml("adjust-blend", "amount", param(it, "opacity", "opacity"), rate, to_local, |v| num(v.as_f64().unwrap_or(100.0) / 100.0), "1"));
        }
        if param(it, "opacity", "blend").and_then(|p| p.value.as_f64()).is_some_and(|b| b != 0.0) {
            self.report.warn("blend modes are not exported to FCPXML");
        }
        let _ = (db_to_gain(0.0), gain_to_db(1.0));
    }
}

fn wrap(tag: &str, attrs: &str, params: &[String]) -> String {
    if params.is_empty() { format!("<{tag}{attrs}/>") } else { format!("<{tag}{attrs}>\n{}\n</{tag}>", params.join("\n")) }
}

fn kparam_xml(
    tag: &str,
    name: &str,
    p: Option<&Param>,
    rate: FrameRate,
    to_local: &dyn Fn(Tick) -> Tick,
    f: impl Fn(&ParamValue) -> String,
    default: &str,
) -> String {
    let Some(p) = p else { return format!("<{tag} {name}=\"{default}\"/>") };
    let attrs = format!(" {name}=\"{}\"", f(&p.value));
    let mut params = Vec::new();
    if p.is_animated() {
        let mut s = format!("  <param name=\"{name}\">\n    <keyframeAnimation>\n");
        for k in &p.keyframes {
            let interp = if k.interp == filmcraft_project::Interpolation::Hold { " interp=\"hold\"" } else { "" };
            s.push_str(&format!("      <keyframe time=\"{}\" value=\"{}\"{interp}/>\n", fmt_time(to_local(k.time), rate), f(&k.value)));
        }
        s.push_str("    </keyframeAnimation>\n  </param>");
        params.push(s);
    }
    wrap(tag, &attrs, &params)
}

fn marker_xml(m: &Marker, local: Tick, rate: FrameRate) -> String {
    let tag = if m.kind == MarkerKind::Chapter { "chapter-marker" } else { "marker" };
    let dur = if m.duration > Tick::ZERO { m.duration } else { rate.frame_duration() };
    let note = if m.comment.is_empty() { String::new() } else { format!(" note=\"{}\"", crate::xml::escape(&m.comment)) };
    format!("<{tag} start=\"{}\" duration=\"{}\" value=\"{}\"{note}/>", fmt_time(local, rate), fmt_time(dur, rate), crate::xml::escape(&m.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rational_times_are_exact() {
        let r = FrameRate::FPS_23_976;
        assert_eq!(parse_time("1001/24000s"), Some(r.frame_duration()));
        assert_eq!(parse_time("3600s"), Some(Tick(3600 * TICKS_PER_SECOND)));
        assert_eq!(parse_time("0s"), Some(Tick::ZERO));
        assert_eq!(fmt_time(r.tick_of(1), r), "1001/24000s");
        assert_eq!(fmt_time(r.tick_of(24000), r), "1001s");
        assert_eq!(fmt_time(FrameRate::FPS_25.tick_of(50), FrameRate::FPS_25), "2s");
        for rate in FrameRate::COMMON {
            for f in [0, 1, 7, 1799, 1800, 86_313, 1_000_003] {
                let t = rate.tick_of(f);
                assert_eq!(parse_time(&fmt_time(t, rate)), Some(t), "{rate} {f}");
            }
        }
        let odd = Tick(12_345);
        assert_eq!(parse_time(&fmt_time(odd, r)), Some(odd));
        assert_eq!(rate_from_frame_duration("1001/30000s"), Some(FrameRate::FPS_29_97));
        assert_eq!(rate_from_frame_duration("100/2500s"), Some(FrameRate::FPS_25));
    }
}
