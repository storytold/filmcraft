//! Test helpers: building projects and comparing edit structure.
#![allow(dead_code)]

use filmcraft_interchange::{ExportOptions, Format, Imported, Report, export, import};
use filmcraft_media::{Generator, MediaKind};
use filmcraft_project::{
    ClipId, ItemId, ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind, Transition, TransitionAlign, TransitionId, find_effect,
};
use filmcraft_time::{FrameRate, Tick, TimeRange};

pub fn media_info(name: &str, video: bool, audio: bool, rate: FrameRate, secs: i64) -> filmcraft_media::MediaInfo {
    filmcraft_media::MediaInfo {
        name: name.into(),
        kind: if video { MediaKind::Movie } else { MediaKind::AudioOnly },
        duration: Tick(secs * filmcraft_time::TICKS_PER_SECOND),
        video: video.then(|| filmcraft_media::VideoStreamInfo {
            width: 1920,
            height: 1080,
            frame_rate: rate,
            par: (1, 1),
            codec: "h264".into(),
            pixel_format: "yuv420p".into(),
            color: Default::default(),
            has_alpha: false,
            bitrate: None,
            hdr: None,
        }),
        audio_streams: audio
            .then(|| filmcraft_media::AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: "aac".into(), bits_per_sample: None })
            .into_iter()
            .collect(),
        container: "mp4".into(),
        start_timecode: None,
        file_size: None,
    }
}

pub fn media(p: &mut Project, path: &str, video: bool, audio: bool, rate: FrameRate) -> ItemId {
    let name = path.rsplit('/').next().unwrap().to_string();
    let info = media_info(&name, video, audio, rate, 600);
    p.add_item(
        &name,
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::File { path: path.into() },
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    )
}

pub fn generator(p: &mut Project, name: &str, g: Generator) -> ItemId {
    let mut info = media_info(name, true, false, FrameRate::FPS_24, 600);
    info.kind = MediaKind::Synthetic;
    p.add_item(
        name,
        Label::Lavender,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(g),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    )
}

pub fn sequence(p: &mut Project, name: &str, rate: FrameRate, df: bool) -> ItemId {
    let settings = SequenceSettings { frame_rate: rate, drop_frame: df, ..Default::default() };
    p.new_sequence(name, settings, 3, 4, None)
}

/// Place a clip (frames at the sequence rate). Returns its id.
pub fn clip(p: &mut Project, seq: ItemId, kind: TrackKind, track: usize, item: ItemId, start: i64, dur: i64, src: i64) -> ClipId {
    let rate = p.sequence(seq).unwrap().settings.frame_rate;
    let ti = p.make_track_item(item, kind, rate.tick_of(start), TimeRange::new(rate.tick_of(src), rate.tick_of(dur)), rate).unwrap();
    let id = ti.id;
    let t = &mut p.sequence_mut(seq).unwrap().tracks_mut(kind)[track];
    t.items.push(ti);
    t.sort();
    id
}

pub fn link(p: &mut Project, seq: ItemId, clips: &[ClipId]) {
    let l = p.alloc_id();
    let s = p.sequence_mut(seq).unwrap();
    for c in clips {
        s.find_item_mut(*c).unwrap().1.link = Some(l);
    }
}

pub fn transition(
    p: &mut Project,
    seq: ItemId,
    kind: TrackKind,
    track: usize,
    effect: &str,
    from: Option<ClipId>,
    to: Option<ClipId>,
    start: i64,
    dur: i64,
    align: TransitionAlign,
) {
    let id = TransitionId(p.alloc_id());
    let rate = p.sequence(seq).unwrap().settings.frame_rate;
    let t = &mut p.sequence_mut(seq).unwrap().tracks_mut(kind)[track];
    t.transitions.push(Transition {
        id,
        effect: find_effect(effect).unwrap().instance(),
        start: rate.tick_of(start),
        duration: rate.tick_of(dur),
        from,
        to,
        align,
        reverse: false,
    });
    t.sort();
}

/// A comparable description of one clip.
#[derive(Clone, Debug, PartialEq)]
pub struct C {
    pub start: Tick,
    pub dur: Tick,
    pub src: Tick,
    pub media: String,
    pub speed: String,
    pub reverse: bool,
    pub enabled: bool,
}

/// A comparable description of one transition (from/to as clip indices on the track).
#[derive(Clone, Debug, PartialEq)]
pub struct T {
    pub start: Tick,
    pub dur: Tick,
    pub effect: String,
    pub align: TransitionAlign,
    pub from: Option<usize>,
    pub to: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tr {
    pub clips: Vec<C>,
    pub transitions: Vec<T>,
}

pub fn media_key(p: &Project, item: ItemId) -> String {
    match &p.item(item).unwrap().kind {
        ItemKind::Media(m) => match &m.media {
            MediaRef::File { path } => path.clone(),
            MediaRef::Generator(g) => format!("gen:{}", g.label()),
        },
        ItemKind::Sequence(_) => format!("seq:{}", p.item(item).unwrap().name),
        _ => "other".into(),
    }
}

/// Non-empty tracks of a sequence (trailing empty tracks ignored).
pub fn structure(p: &Project, seq: ItemId, kind: TrackKind) -> Vec<Tr> {
    let s = p.sequence(seq).unwrap();
    let mut out: Vec<Tr> = s
        .tracks(kind)
        .iter()
        .map(|t| Tr {
            clips: t
                .items
                .iter()
                .map(|i| C {
                    start: i.start,
                    dur: i.duration,
                    src: i.frame_hold.unwrap_or(i.source_in),
                    media: media_key(p, i.item),
                    speed: if i.frame_hold.is_some() { "hold".into() } else { format!("{:.3}", i.speed) },
                    reverse: i.reverse,
                    enabled: i.enabled,
                })
                .collect(),
            transitions: t
                .transitions
                .iter()
                .map(|x| T {
                    start: x.start,
                    dur: x.duration,
                    effect: x.effect.effect.clone(),
                    align: x.align,
                    from: x.from.and_then(|f| t.items.iter().position(|i| i.id == f)),
                    to: x.to.and_then(|f| t.items.iter().position(|i| i.id == f)),
                })
                .collect(),
        })
        .collect();
    while out.last().is_some_and(|t| t.clips.is_empty()) {
        out.pop();
    }
    out
}

/// Link partition as sets of (kind, track, clip index).
pub fn links(p: &Project, seq: ItemId) -> Vec<Vec<(u8, usize, usize)>> {
    let s = p.sequence(seq).unwrap();
    let mut groups: std::collections::BTreeMap<u64, Vec<(u8, usize, usize)>> = Default::default();
    for (k, kind) in [(0u8, TrackKind::Video), (1, TrackKind::Audio)] {
        for (ti, t) in s.tracks(kind).iter().enumerate() {
            for (ci, c) in t.items.iter().enumerate() {
                if let Some(l) = c.link {
                    groups.entry(l).or_default().push((k, ti, ci));
                }
            }
        }
    }
    let mut v: Vec<Vec<_>> = groups.into_values().filter(|g| g.len() > 1).collect();
    for g in &mut v {
        g.sort();
    }
    v.sort();
    v
}

pub fn roundtrip(p: &Project, seq: ItemId, format: Format, opts: &ExportOptions) -> (Imported, String, Report) {
    let (bytes, _rep) = export(p, seq, format, opts).expect("export");
    let text = String::from_utf8(bytes.clone()).unwrap();
    let (imp, rep) = import(&bytes, format, None).unwrap_or_else(|e| panic!("import failed: {e}\n{text}"));
    (imp, text, rep)
}

pub fn only_seq(imp: &Imported) -> ItemId {
    assert_eq!(imp.sequences.len(), 1, "expected one top-level sequence");
    imp.sequences[0]
}

pub fn f(rate: FrameRate, n: i64) -> Tick {
    rate.tick_of(n)
}
