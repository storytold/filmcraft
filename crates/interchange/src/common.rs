//! Shared helpers: fragment builder, track placement, media paths/URLs, effect and transition
//! mapping, frame/tick conversion.

use std::collections::HashMap;

use filmcraft_color::ColorInfo;
use filmcraft_media::{AudioStreamInfo, Generator, MediaInfo, MediaKind, VideoStreamInfo};
use filmcraft_project::{
    BinId, ClipId, EffectInstance, EffectKind, Interpretation, ItemId, ItemKind, Label, MediaClip, MediaRef, Param, Project, Sequence, SequenceSettings, Track,
    TrackId, TrackItem, TrackKind, effect, effect::find_effect_by_name, find_effect,
};
use filmcraft_time::{FrameRate, Tick};

use crate::{Imported, Report};

// ---------------------------------------------------------------------------------------------
// Paths and URLs
// ---------------------------------------------------------------------------------------------

fn is_windows_abs(p: &str) -> bool {
    let b = p.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

pub(crate) fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || p.starts_with("\\\\") || is_windows_abs(p)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = h {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn percent_encode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&c) {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// Convert a `file://` URL (`file:///a/b`, `file://localhost/a/b`, `file:///C:/x`) to a path.
/// Strings that are not file URLs are returned unchanged.
pub fn file_url_to_path(url: &str) -> String {
    let lower = url.get(..7).map(|s| s.to_ascii_lowercase());
    if lower.as_deref() != Some("file://") {
        return url.to_string();
    }
    let rest = &url[7..];
    let rest = if rest.len() >= 9 && rest[..9].eq_ignore_ascii_case("localhost") { &rest[9..] } else { rest };
    let decoded = percent_decode(rest);
    // file:///C:/x -> C:/x
    if decoded.len() >= 3 && decoded.starts_with('/') && is_windows_abs(&decoded[1..]) {
        return decoded[1..].to_string();
    }
    if !decoded.starts_with('/') {
        // file://server/share -> UNC
        return format!("//{decoded}");
    }
    decoded
}

/// Convert an absolute path to a `file://` URL (`localhost` form for FCP7 XML).
pub fn path_to_file_url(path: &str, localhost: bool) -> String {
    let p = path.replace('\\', "/");
    // Windows verbatim paths (`\\?\C:\x`, `\\?\UNC\server\share`, as `canonicalize` returns them)
    // are not network shares: drop the prefix so `?` doesn't become the URL's host.
    let p = if let Some(unc) = p.strip_prefix("//?/UNC/") {
        format!("//{unc}")
    } else if let Some(local) = p.strip_prefix("//?/") {
        local.to_string()
    } else {
        p
    };
    let host = if localhost { "localhost" } else { "" };
    if is_windows_abs(&p) {
        format!("file://{host}/{}", percent_encode_path(&p))
    } else if let Some(unc) = p.strip_prefix("//") {
        format!("file://{}", percent_encode_path(unc))
    } else {
        format!("file://{host}{}", percent_encode_path(&p))
    }
}

fn join(base: &str, rel: &str) -> String {
    let sep = if base.contains('\\') && !base.contains('/') { '\\' } else { '/' };
    let mut parts: Vec<&str> = base.split(['/', '\\']).collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    for seg in rel.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            s => parts.push(s),
        }
    }
    parts.join(&sep.to_string())
}

/// Resolve a media reference from a document: file URLs become paths, relative paths are joined to
/// `base_dir` (when given).
pub fn resolve_path(reference: &str, base_dir: Option<&str>) -> String {
    let p = file_url_to_path(reference.trim());
    if is_absolute(&p) {
        return p;
    }
    match base_dir {
        Some(b) if !b.is_empty() => join(b, &p),
        _ => p,
    }
}

/// `path` relative to directory `base`, if `path` lies under it (or shares a prefix).
pub(crate) fn relative_path(path: &str, base: &str) -> Option<String> {
    if !is_absolute(path) || !is_absolute(base) {
        return None;
    }
    let a: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let b: Vec<&str> = base.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if common == 0 {
        return None;
    }
    let mut out: Vec<&str> = std::iter::repeat_n("..", b.len() - common).collect();
    out.extend(&a[common..]);
    Some(out.join("/"))
}

/// Final path component.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// File name without extension.
pub(crate) fn file_stem(path: &str) -> &str {
    let n = file_name(path);
    match n.rfind('.') {
        Some(i) if i > 0 => &n[..i],
        _ => n,
    }
}

pub(crate) fn extension(path: &str) -> String {
    let n = file_name(path);
    n.rfind('.').map(|i| n[i + 1..].to_ascii_lowercase()).unwrap_or_default()
}

/// Media kind guess from a file extension.
pub(crate) fn kind_from_ext(path: &str) -> MediaKind {
    match extension(path).as_str() {
        "wav" | "aif" | "aiff" | "mp3" | "aac" | "m4a" | "flac" | "ogg" | "opus" | "bwf" => MediaKind::AudioOnly,
        "png" | "jpg" | "jpeg" | "tif" | "tiff" | "psd" | "bmp" | "gif" | "webp" | "exr" | "dpx" | "tga" => MediaKind::Still,
        _ => MediaKind::Movie,
    }
}

// ---------------------------------------------------------------------------------------------
// Frames and rates
// ---------------------------------------------------------------------------------------------

/// Frame count nearest to `t` at `rate`.
pub(crate) fn frames_round(rate: FrameRate, t: Tick) -> i64 {
    let f = rate.frame_at(t);
    if rate.tick_of(f + 1) - t < t - rate.tick_of(f) { f + 1 } else { f }
}

/// Whether `t` is exactly on a frame boundary of `rate`.
pub(crate) fn on_frame(rate: FrameRate, t: Tick) -> bool {
    rate.tick_of(rate.frame_at(t)) == t
}

/// FCP7-style `(timebase, ntsc)` for a rate.
pub(crate) fn timebase(rate: FrameRate) -> (i64, bool) {
    (rate.timecode_base(), rate.is_ntsc())
}

/// Rate from FCP7-style `(timebase, ntsc)`.
pub(crate) fn rate_from_timebase(timebase: i64, ntsc: bool) -> FrameRate {
    let tb = timebase.max(1);
    if ntsc { FrameRate::new(tb * 1000, 1001) } else { FrameRate::new(tb, 1) }
}

/// Snap a floating rate (OTIO) to an exact rate.
pub(crate) fn rate_from_f64(r: f64) -> FrameRate {
    if r <= 0.0 || !r.is_finite() {
        return FrameRate::FPS_24;
    }
    for c in FrameRate::COMMON {
        if (c.as_f64() - r).abs() < 1e-3 {
            return c;
        }
    }
    let n = (r * 1001.0).round();
    if ((n / 1001.0) - r).abs() < 1e-9 && (n as i64) % 1000 == 0 {
        return FrameRate::new(n as i64, 1001);
    }
    if (r - r.round()).abs() < 1e-9 {
        return FrameRate::new(r.round() as i64, 1);
    }
    FrameRate::new((r * 1_000_000.0).round() as i64, 1_000_000)
}

// ---------------------------------------------------------------------------------------------
// Media descriptions
// ---------------------------------------------------------------------------------------------

/// What a document says about a media file.
#[derive(Clone, Debug, Default)]
pub(crate) struct MediaSpec {
    pub duration: Option<Tick>,
    /// (width, height, rate).
    pub video: Option<(u32, u32, FrameRate)>,
    /// (sample rate, channels).
    pub audio: Option<(u32, u32)>,
    pub start_tc: Option<i64>,
    pub kind: Option<MediaKind>,
}

pub(crate) fn media_info(name: &str, spec: &MediaSpec) -> MediaInfo {
    let kind = spec.kind.unwrap_or(match (spec.video.is_some(), spec.audio.is_some()) {
        (false, true) => MediaKind::AudioOnly,
        _ => MediaKind::Movie,
    });
    MediaInfo {
        name: name.to_string(),
        kind,
        duration: spec.duration.unwrap_or(Tick::ZERO),
        video: spec.video.map(|(w, h, r)| VideoStreamInfo {
            width: w,
            height: h,
            frame_rate: r,
            par: (1, 1),
            codec: String::new(),
            pixel_format: String::new(),
            color: ColorInfo::REC709,
            has_alpha: false,
            bitrate: None,
            hdr: None,
        }),
        audio_streams: spec
            .audio
            .map(|(sr, ch)| AudioStreamInfo { sample_rate: sr, channels: ch, codec: String::new(), bits_per_sample: None })
            .into_iter()
            .collect(),
        container: String::new(),
        start_timecode: spec.start_tc,
        file_size: None,
    }
}

pub(crate) fn media_clip(media: MediaRef, info: MediaInfo) -> MediaClip {
    MediaClip {
        media,
        info,
        interpret: Interpretation::default(),
        mark_in: None,
        mark_out: None,
        markers: Vec::new(),
        offline: false,
        proxy: None,
        identity: None,
    }
}

/// The file path behind an item (following subclips), if it is file media.
pub(crate) fn item_path(p: &Project, id: ItemId) -> Option<&str> {
    match &p.item(id)?.kind {
        ItemKind::Media(m) => match &m.media {
            MediaRef::File { path } => Some(path),
            MediaRef::Generator(_) => None,
        },
        ItemKind::Subclip { parent, .. } => item_path(p, *parent),
        _ => None,
    }
}

/// The media clip behind an item (following subclips).
pub(crate) fn item_media(p: &Project, id: ItemId) -> Option<&MediaClip> {
    match &p.item(id)?.kind {
        ItemKind::Media(m) => Some(m),
        ItemKind::Subclip { parent, .. } => item_media(p, *parent),
        _ => None,
    }
}

/// The underlying item for exports (subclip → parent media).
pub(crate) fn base_item(p: &Project, id: ItemId) -> ItemId {
    match p.item(id).map(|i| &i.kind) {
        Some(ItemKind::Subclip { parent, .. }) => base_item(p, *parent),
        _ => id,
    }
}

/// How an export report names a clip whose source is a graphic (title) or an adjustment layer,
/// which most interchange formats cannot carry: `graphic clip "Title" at frame 48`. `None` for
/// every other clip. A lost title must be named, not folded into a count.
pub(crate) fn uncarried_clip(p: &Project, c: &TrackItem, rate: FrameRate) -> Option<String> {
    let what = match &p.item(c.item)?.kind {
        ItemKind::Graphic { .. } => "graphic clip",
        ItemKind::AdjustmentLayer { .. } => "adjustment layer",
        _ => return None,
    };
    Some(format!("{what} \"{}\" at frame {}", c.name, rate.frame_at(c.start)))
}

pub(crate) fn generator_of(p: &Project, id: ItemId) -> Option<&Generator> {
    match &item_media(p, id)?.media {
        MediaRef::Generator(g) => Some(g),
        MediaRef::File { .. } => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Fragment builder
// ---------------------------------------------------------------------------------------------

pub(crate) struct Builder {
    pub p: Project,
    media: HashMap<String, ItemId>,
    pub top: Vec<ItemId>,
    next_link: u64,
}

impl Builder {
    pub fn new(name: &str) -> Self {
        Self { p: Project::new(name), media: HashMap::new(), top: Vec::new(), next_link: 1 }
    }

    pub fn bin(&mut self, name: &str, parent: Option<BinId>) -> BinId {
        self.p.add_bin(name, parent)
    }

    pub fn find_media(&self, key: &str) -> Option<ItemId> {
        self.media.get(key).copied()
    }

    pub fn register_media(&mut self, key: &str, id: ItemId) {
        self.media.insert(key.to_string(), id);
    }

    /// File media deduplicated by `key` (normally the resolved path).
    pub fn file_media(&mut self, key: &str, name: &str, path: &str, spec: &MediaSpec, bin: Option<BinId>) -> ItemId {
        if let Some(id) = self.media.get(key) {
            return *id;
        }
        let info = media_info(name, spec);
        let id = self.p.add_item(name, Label::Iris, ItemKind::Media(media_clip(MediaRef::File { path: path.to_string() }, info)), bin);
        self.media.insert(key.to_string(), id);
        id
    }

    pub fn generator_media(&mut self, key: &str, name: &str, g: Generator, spec: &MediaSpec, bin: Option<BinId>) -> ItemId {
        if let Some(id) = self.media.get(key) {
            return *id;
        }
        let mut spec = spec.clone();
        spec.kind = Some(MediaKind::Synthetic);
        let info = media_info(name, &spec);
        let id = self.p.add_item(name, Label::Lavender, ItemKind::Media(media_clip(MediaRef::Generator(g), info)), bin);
        self.media.insert(key.to_string(), id);
        id
    }

    /// Add an empty sequence item (filled later with [`Builder::put_sequence`]).
    pub fn reserve_sequence(&mut self, name: &str, settings: SequenceSettings, bin: Option<BinId>) -> ItemId {
        let seq = empty_sequence(settings);
        self.p.add_item(name, Label::Forest, ItemKind::Sequence(std::sync::Arc::new(seq)), bin)
    }

    pub fn put_sequence(&mut self, id: ItemId, mut seq: Sequence) {
        for t in seq.all_tracks_mut() {
            t.sort();
        }
        if let Some(s) = self.p.sequence_mut(id) {
            *s = seq;
        }
    }

    pub fn track(&mut self, kind: TrackKind, index: usize) -> Track {
        let id = TrackId(self.p.alloc_id());
        let name = match kind {
            TrackKind::Video => format!("Video {}", index + 1),
            TrackKind::Audio => format!("Audio {}", index + 1),
        };
        Track::new(id, kind, name)
    }

    pub fn ensure_tracks(&mut self, seq: &mut Sequence, kind: TrackKind, n: usize) {
        while seq.tracks(kind).len() < n {
            let i = seq.tracks(kind).len();
            let t = self.track(kind, i);
            seq.tracks_mut(kind).push(t);
        }
    }

    pub fn link_id(&mut self) -> u64 {
        let l = self.next_link;
        self.next_link += 1;
        l
    }

    pub fn alloc(&mut self) -> u64 {
        self.p.alloc_id()
    }

    /// A track item with the intrinsic effects of its track kind.
    pub fn clip(&mut self, item: ItemId, kind: TrackKind, name: &str, start: Tick, duration: Tick, source_in: Tick) -> TrackItem {
        let label = self.p.item(item).map(|i| i.label).unwrap_or(Label::Iris);
        TrackItem {
            id: ClipId(self.p.alloc_id()),
            item,
            name: name.to_string(),
            label,
            start,
            duration,
            source_in,
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: match kind {
                TrackKind::Video => effect::intrinsic_video(),
                TrackKind::Audio => effect::intrinsic_audio(),
            },
            markers: Vec::new(),
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: Default::default(),
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        }
    }

    /// Place `item` on the first track at or after `preferred` where it does not overlap anything,
    /// creating tracks as needed. Returns the track index.
    pub fn place(&mut self, seq: &mut Sequence, kind: TrackKind, preferred: usize, item: TrackItem) -> usize {
        let mut i = preferred;
        loop {
            self.ensure_tracks(seq, kind, i + 1);
            let t = &mut seq.tracks_mut(kind)[i];
            if !t.items.iter().any(|o| o.start < item.end() && item.start < o.end()) {
                t.items.push(item);
                t.sort();
                return i;
            }
            i += 1;
        }
    }

    /// Finish: grow media durations to cover every use, return the fragment.
    pub fn finish(mut self) -> Imported {
        let mut need: HashMap<ItemId, Tick> = HashMap::new();
        for it in self.p.items.values() {
            if let Some(seq) = it.as_sequence() {
                for t in seq.all_tracks() {
                    for ti in &t.items {
                        let e = need.entry(ti.item).or_insert(Tick::ZERO);
                        *e = (*e).max(ti.source_out()).max(ti.source_in + Tick(1));
                    }
                }
            }
        }
        for (id, end) in need {
            if let Some(m) = self.p.item_mut(id).and_then(|i| i.as_media_mut())
                && m.info.duration < end
            {
                m.info.duration = end;
            }
        }
        Imported { project: self.p, sequences: self.top }
    }
}

pub(crate) fn empty_sequence(settings: SequenceSettings) -> Sequence {
    Sequence {
        settings,
        video_tracks: Vec::new(),
        audio_tracks: Vec::new(),
        markers: Vec::new(),
        mark_in: None,
        mark_out: None,
        work_area: None,
        start_timecode: 0,
        master_volume_db: 0.0,
        master_effects: Vec::new(),
        master_mixer: Default::default(),
        submix_tracks: Vec::new(),
        caption_tracks: Vec::new(),
        multicam: None,
        merged: None,
        split: Default::default(),
    }
}

pub(crate) fn settings_for(rate: FrameRate, width: u32, height: u32, drop_frame: bool) -> SequenceSettings {
    SequenceSettings {
        width,
        height,
        frame_rate: rate,
        drop_frame: drop_frame && rate.supports_drop_frame(),
        preset: format!("Custom {width}x{height} {}", rate.label()),
        ..SequenceSettings::default()
    }
}

/// Source time advanced by a timeline duration at `speed` (exact for integral speeds).
pub(crate) fn scaled(d: Tick, speed: f64) -> Tick {
    let s = speed.abs();
    if s == s.round() && s < 1e6 { Tick(d.0 * s as i64) } else { Tick((d.0 as f64 * s).round() as i64) }
}

/// Timeline duration of a source duration at `speed`.
pub(crate) fn unscaled(d: Tick, speed: f64) -> Tick {
    let s = speed.abs();
    if s == 0.0 {
        return d;
    }
    if s == 1.0 { d } else { Tick((d.0 as f64 / s).round() as i64) }
}

/// Overwrite `item` onto `track` (EDL record semantics): whatever was under it is trimmed/split.
pub(crate) fn overwrite(track: &mut Track, item: TrackItem, alloc: &mut dyn FnMut() -> u64) {
    let (s, e) = (item.start, item.end());
    let mut out = Vec::with_capacity(track.items.len() + 2);
    for o in track.items.drain(..) {
        if o.end() <= s || o.start >= e {
            out.push(o);
            continue;
        }
        if o.start < s {
            let mut l = o.clone();
            l.duration = s - o.start;
            out.push(l);
        }
        if o.end() > e {
            let mut r = o.clone();
            r.id = ClipId(alloc());
            r.source_in = o.source_in + scaled(e - o.start, o.speed);
            r.start = e;
            r.duration = o.end() - e;
            out.push(r);
        }
    }
    out.push(item);
    track.items = out;
    track.sort();
}

// ---------------------------------------------------------------------------------------------
// Effects
// ---------------------------------------------------------------------------------------------

pub(crate) fn param<'a>(ti: &'a TrackItem, effect: &str, param: &str) -> Option<&'a Param> {
    ti.effect(effect).and_then(|e| e.param(param))
}

/// Set a parameter, adding the effect instance (defaults) if the item lacks it.
pub(crate) fn set_param(ti: &mut TrackItem, effect: &str, param: &str, value: Param) {
    if ti.effect(effect).is_none()
        && let Some(d) = find_effect(effect)
    {
        ti.effects.push(d.instance());
    }
    if let Some(e) = ti.effect_mut(effect) {
        e.params.insert(param.to_string(), value);
    }
}

/// Whether a parameter differs from its definition default or is animated.
pub(crate) fn param_modified(ti: &TrackItem, effect: &str, param_id: &str) -> bool {
    let Some(p) = param(ti, effect, param_id) else { return false };
    if p.is_animated() {
        return true;
    }
    let def = find_effect(effect).and_then(|d| d.param(param_id)).map(|d| d.default.clone());
    match (&p.value, def) {
        (filmcraft_project::ParamValue::Vec2(v), Some(filmcraft_project::ParamValue::Vec2(_))) => !(v.x.is_nan() && v.y.is_nan()),
        (v, Some(d)) => *v != d,
        _ => false,
    }
}

/// Non-intrinsic effects on an item.
pub(crate) fn standard_effects(ti: &TrackItem) -> impl Iterator<Item = &EffectInstance> {
    ti.effects.iter().filter(|e| !e.def().is_some_and(|d| d.intrinsic))
}

/// A transition effect for a document's transition name. Unknown names become Cross Dissolve
/// (Constant Power on audio) with a warning.
pub(crate) fn transition_effect(name: &str, audio: bool, report: &mut Report) -> EffectInstance {
    if let Some(id) = transition_alias(name, audio)
        && let Some(d) = find_effect(id)
    {
        return d.instance();
    }
    let want = if audio { EffectKind::AudioTransition } else { EffectKind::VideoTransition };
    if let Some(d) = find_effect_by_name(name.trim()).filter(|d| d.kind == want) {
        return d.instance();
    }
    let fallback = if audio { "constant_power" } else { "cross_dissolve" };
    report.warn(format!("transition \"{name}\" is not supported; imported as {}", find_effect(fallback).map(|d| d.name).unwrap_or(fallback)));
    find_effect(fallback).map(|d| d.instance()).unwrap_or_else(|| EffectInstance {
        effect: fallback.into(),
        enabled: true,
        params: Default::default(),
        masks: vec![],
        post_fader: false,
        essential: false,
        layer: None,
    })
}

/// Effect id of a well-known document transition name, if `name` is one.
fn transition_alias(name: &str, audio: bool) -> Option<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "dissolve" | "cross dissolve" | "smpte_dissolve" | "d" | "crossdissolve" => Some(if audio { "constant_power" } else { "cross_dissolve" }),
        "cross fade (+3db)" | "cross fade +3db" | "crossfade (+3db)" | "cross fade" => Some("constant_power"),
        "cross fade (0db)" | "cross fade 0db" | "crossfade (0db)" => Some("constant_gain"),
        "dip to color dissolve" | "fade in fade out dissolve" | "fade to color" => Some("dip_to_black"),
        "edge wipe" | "wipe" | "smpte_wipe" => Some("wipe"),
        _ => None,
    }
}

/// The audio transition a document's transition name stands for (alias, id or display name),
/// or `None` when the name is not an audio transition. Reports nothing.
pub(crate) fn audio_transition_named(name: &str) -> Option<EffectInstance> {
    transition_alias(name, true)
        .and_then(find_effect)
        .or_else(|| find_effect_by_name(name.trim()))
        .filter(|d| d.kind == EffectKind::AudioTransition)
        .map(|d| d.instance())
}

/// Display name of a transition (for documents).
pub(crate) fn transition_name(e: &EffectInstance) -> String {
    e.def().map(|d| d.name.to_string()).unwrap_or_else(|| e.effect.clone())
}

/// Label from a document label name (Premiere label names; FCP7 names like "Good Take" ignored).
pub(crate) fn label_from(s: &str) -> Option<Label> {
    Label::from_name(s.trim())
}

/// dB ↔ linear gain.
pub(crate) fn db_to_gain(db: f64) -> f64 {
    if db <= -287.0 { 0.0 } else { 10f64.powf(db / 20.0) }
}
pub(crate) fn gain_to_db(g: f64) -> f64 {
    if g <= 1e-15 { -287.5 } else { 20.0 * g.log10() }
}

/// Sanitise a name for fixed-width EDL fields.
pub(crate) fn sanitize_reel(s: &str, max: usize) -> String {
    let r: String = s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c.to_ascii_uppercase() } else { '_' }).take(max).collect();
    if r.is_empty() { "AX".into() } else { r }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(file_url_to_path("file://localhost/Users/me/My%20Clip.mov"), "/Users/me/My Clip.mov");
        assert_eq!(file_url_to_path("file:///C:/Media/a.mxf"), "C:/Media/a.mxf");
        assert_eq!(file_url_to_path("/plain/path.mov"), "/plain/path.mov");
        assert_eq!(path_to_file_url("/Users/me/My Clip.mov", true), "file://localhost/Users/me/My%20Clip.mov");
        assert_eq!(path_to_file_url("C:\\Media\\a b.mxf", false), "file:///C:/Media/a%20b.mxf");
        assert_eq!(path_to_file_url("\\\\?\\C:\\Media\\a.mp4", true), "file://localhost/C:/Media/a.mp4");
        assert_eq!(path_to_file_url("\\\\?\\UNC\\srv\\share\\a.mp4", false), "file://srv/share/a.mp4");
        for p in ["/a/b c/ü.mov", "C:/x/y.mp4", "/r/%/#?.mov"] {
            assert_eq!(file_url_to_path(&path_to_file_url(p, true)), p);
            assert_eq!(file_url_to_path(&path_to_file_url(p, false)), p);
        }
    }

    #[test]
    fn paths() {
        assert_eq!(resolve_path("media/a.mov", Some("/proj")), "/proj/media/a.mov");
        assert_eq!(resolve_path("../a.mov", Some("/proj/x/")), "/proj/a.mov");
        assert_eq!(resolve_path("/abs/a.mov", Some("/proj")), "/abs/a.mov");
        assert_eq!(relative_path("/proj/media/a.mov", "/proj").as_deref(), Some("media/a.mov"));
        assert_eq!(relative_path("/proj/a.mov", "/proj/edl").as_deref(), Some("../a.mov"));
        assert_eq!(file_stem("/x/y/Clip 01.final.mov"), "Clip 01.final");
    }

    #[test]
    fn rates() {
        assert_eq!(rate_from_timebase(24, true), FrameRate::FPS_23_976);
        assert_eq!(rate_from_timebase(30, true), FrameRate::FPS_29_97);
        assert_eq!(rate_from_timebase(60, true), FrameRate::FPS_59_94);
        assert_eq!(rate_from_timebase(25, false), FrameRate::FPS_25);
        assert_eq!(rate_from_f64(24000.0 / 1001.0), FrameRate::FPS_23_976);
        assert_eq!(rate_from_f64(23.976), FrameRate::FPS_23_976);
        assert_eq!(rate_from_f64(48000.0), FrameRate::new(48000, 1));
        assert_eq!(frames_round(FrameRate::FPS_24, FrameRate::FPS_24.tick_of(5) + Tick(10)), 5);
    }
}
