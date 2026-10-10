//! Multi-camera editing and clip synchronisation commands.
//!
//! | Id | Menu | |
//! |---|---|---|
//! | `clip.synchronize` | Clip ▸ Synchronize… | align selected timeline clips to a reference by In, Out, timecode, clip marker or audio |
//! | `clip.mergeClips` | Clip ▸ Merge Clips… | one video + audio clips → a merged clip |
//! | `clip.createMulticam` | Clip ▸ Create Multi-Camera Source Sequence… | synchronised cameras → a multi-camera source sequence |
//! | `clip.multicamEnable` | Clip ▸ Multi-Camera ▸ Enable | toggle multi-camera behaviour of nested clips |
//! | `clip.multicamFlatten` | Clip ▸ Multi-Camera ▸ Flatten | replace multi-camera clips by the clips their angle shows |
//! | `multicam.switchAngle` | | set the angle of clips (keys 1–9 when stopped, clicking an angle) |
//! | `multicam.recordStart` / `multicam.cut` / `multicam.recordStop` | | live switching while playing: one undo step per pass |
//! | `multicam.selectCamera1`…`9` | keys 1–9 | while recording: cut to camera N; stopped: switch the clip at the playhead |
//! | `multicam.cutToCamera1`…`9` | Ctrl+1–9 | add an edit at the playhead and show camera N from there |
//! | `multicam.audioFollowsVideo` | | switching video switches the linked audio clips too |
//! | `multicam.editCameras` | | rename / show / hide cameras, change the audio mode |
//! | `multicam.inspect` | | the multi-camera clip at the playhead, its cameras and the recorder state |
//! | `multicam.gridLayout` | Program ▸ Multi-Camera ▸ Layout | angle grid: automatic, 2×2, 3×3 or 4×4 (paged when the angles don't fit) |
//! | `multicam.page` / `multicam.nextPage` / `multicam.prevPage` | page arrows | page of the angle grid (keys 1–9 pick cameras on the shown page) |
//! | `multicam.selectionTopDown` | Multi-Camera Selection Top Down | stacked multi-camera clips: switch the topmost instead of the lowest |
//! | `multicam.showPreviewMonitor` | Show Multi-Camera Preview Monitor | the program next to the grid (off: the grid fills the monitor) |
//! | `multicam.autoAdjustQuality` | Auto-Adjust Multi-Camera Playback Quality | lower-resolution grid decode while playing |
//! | `multicam.transmitView` | Transmit Multi-Camera View | send the grid to the transmit device (no transmit output yet: stored only) |
//! | `multicam.grid` | | the grid page at the playhead: layout, pages, cells (camera, angle, name, active) |
//!
//! Sync methods are described in [`crate::sync`].

use std::collections::HashMap;
use std::sync::Arc;

use filmcraft_edit as edit;
use filmcraft_edit::EditCtx;
use filmcraft_edit::multicam::Cut;
use filmcraft_project::{
    BinEntry, Camera, ClipId, ItemId, ItemKind, Label, MergedClip, MulticamAudio, MulticamSel, MulticamSource, Project, Sequence, SequenceSettings, Track,
    TrackId, TrackItem, TrackKind, resolve_auto_points,
};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, clips_p, has_selection, has_seq, item_p, str_p, time_p, track_p, u64_p, with_links};
use crate::sync::{self, Method, SyncClip};
use crate::{EngineError, MediaPool, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

#[allow(clippy::too_many_arguments)]
fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run, journal: bool) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal }
}

macro_rules! camera_keys {
    ($($n:literal),*) => {
        [$(
            CommandSpec {
                id: concat!("multicam.selectCamera", $n),
                label: concat!("Select Camera ", $n),
                menu: &[],
                shortcut: Some(concat!($n)),
                params: r#"{"videoOnly":bool?}"#,
                enabled: has_seq,
                run: |s, p| cut(s, &on_page(with_camera(p, $n))),
                journal: true,
            },
            CommandSpec {
                id: concat!("multicam.cutToCamera", $n),
                label: concat!("Cut to Camera ", $n),
                menu: &[],
                shortcut: Some(concat!("Ctrl+", $n)),
                params: r#"{"videoOnly":bool?}"#,
                enabled: has_seq,
                run: |s, p| cut_to_camera(s, &on_page(with_camera(p, $n))),
                journal: true,
            },
        )*]
    };
}

fn with_camera(p: &Value, n: u64) -> Value {
    let mut v = if p.is_object() { p.clone() } else { json!({}) };
    v["camera"] = json!(n);
    v
}

/// Keys 1–9 pick the camera on the grid page shown.
fn on_page(mut v: Value) -> Value {
    v["pageRelative"] = json!(true);
    v
}

/// Multi-Camera view settings (Program monitor ▸ wrench menu); editor state, not project data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MulticamView {
    /// Grid side (2 = 2×2, 3 = 3×3, 4 = 4×4); None = automatic.
    pub layout: Option<usize>,
    /// Grid page (0-based).
    pub page: usize,
    /// Multi-Camera Selection Top Down: of stacked multi-camera clips, the topmost one is switched.
    pub top_down: bool,
    /// Show Multi-Camera Preview Monitor: the program next to the grid.
    pub show_preview: bool,
    /// Auto-Adjust Multi-Camera Playback Quality: lower-resolution grid decode while playing.
    pub auto_quality: bool,
    /// Transmit Multi-Camera View: send the grid (not the program) to the transmit device.
    pub transmit: bool,
}

impl Default for MulticamView {
    fn default() -> Self {
        MulticamView { layout: None, page: 0, top_down: false, show_preview: true, auto_quality: false, transmit: false }
    }
}

impl MulticamView {
    pub fn layout_name(&self) -> &'static str {
        match self.layout {
            Some(2) => "2x2",
            Some(3) => "3x3",
            Some(4) => "4x4",
            _ => "auto",
        }
    }
    /// The grid page for `shown` angles.
    pub fn page_layout(&self, shown: usize) -> filmcraft_render::multicam::PageLayout {
        filmcraft_render::multicam::page_layout(shown, self.layout, self.page)
    }
}

pub fn commands() -> Vec<CommandSpec> {
    let mut v = base_commands();
    v.extend(camera_keys!(1, 2, 3, 4, 5, 6, 7, 8, 9));
    v
}

fn base_commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "clip.synchronize",
            "Synchronize…",
            &["Clip"],
            r#"{"method":"in|out|timecode|marker|audio","clips":[id]?,"reference":clip?,"track":"V1"|"A1"?,"ignoreHours":bool?,"marker":str?,"offset":frames?}"#,
            can_synchronize,
            synchronize,
            true,
        ),
        spec(
            "clip.mergeClips",
            "Merge Clips…",
            &["Clip"],
            r#"{"items":[id]?,"method":"in|out|timecode|marker|audio","name":str?,"removeVideoAudio":bool?,"ignoreHours":bool?,"marker":str?,"offset":frames?}"#,
            has_two_items,
            merge_clips,
            true,
        ),
        spec(
            "clip.createMulticam",
            "Create Multi-Camera Source Sequence…",
            &["Clip"],
            r#"{"items":[id]?,"name":str?,"method":"in|out|timecode|marker|audio","ignoreHours":bool?,"marker":str?,"offset":frames?,"audio":"camera1|all|switch","cameraNames":"clip|track|metadata","processedBin":bool?,"reference":item?}"#,
            has_items,
            create_multicam,
            true,
        ),
        spec("clip.multicamEnable", "Enable", &["Clip", "Multi-Camera"], r#"{"clips":[id]?,"enabled":bool?}"#, has_selection, enable, true),
        spec("clip.multicamFlatten", "Flatten", &["Clip", "Multi-Camera"], r#"{"clips":[id]?}"#, has_selection, flatten, true),
        spec(
            "multicam.switchAngle",
            "Switch Multi-Camera Angle",
            &[],
            r#"{"camera":1..16 (shown order)|"angle":0-based,"clips":[id]?,"time":ticks?,"videoOnly":bool?,"audioOnly":bool?}"#,
            has_seq,
            switch_angle,
            true,
        ),
        spec("multicam.recordStart", "Start Multi-Camera Recording", &[], r#"{"time":ticks?}"#, has_seq, record_start, false),
        spec("multicam.cut", "Cut to Camera", &[], r#"{"camera":1..16|"angle":0-based,"time":ticks?,"videoOnly":bool?}"#, has_seq, cut, true),
        spec("multicam.recordStop", "Stop Multi-Camera Recording", &[], r#"{"time":ticks?}"#, always, record_stop, true),
        spec("multicam.audioFollowsVideo", "Multi-Camera Audio Follows Video", &[], r#"{"enabled":bool?}"#, always, audio_follows, true),
        spec(
            "multicam.editCameras",
            "Edit Cameras…",
            &[],
            r#"{"sequence":id?,"cameras":[{"angle":0-based,"name":str?,"enabled":bool?}]?,"audio":"camera1|all|switch"?}"#,
            always,
            edit_cameras,
            true,
        ),
        spec("multicam.cutToCamera", "Cut to Camera", &[], r#"{"camera":1..16|"angle":0-based,"time":ticks?,"videoOnly":bool?}"#, has_seq, cut_to_camera, true),
        spec("multicam.gridLayout", "Multi-Camera Layout", &[], r#"{"layout":"auto|2x2|3x3|4x4"}"#, always, grid_layout, true),
        spec("multicam.page", "Multi-Camera Page", &[], r#"{"page":0-based|"next"|"prev"}"#, always, set_page, true),
        spec("multicam.nextPage", "Next Multi-Camera Page", &[], "{}", always, |s, _| set_page(s, &json!({"page": "next"})), true),
        spec("multicam.prevPage", "Previous Multi-Camera Page", &[], "{}", always, |s, _| set_page(s, &json!({"page": "prev"})), true),
        spec("multicam.selectionTopDown", "Multi-Camera Selection Top Down", &[], r#"{"enabled":bool?}"#, always, |s, p| view_flag(s, p, Flag::TopDown), true),
        spec(
            "multicam.showPreviewMonitor",
            "Show Multi-Camera Preview Monitor",
            &[],
            r#"{"enabled":bool?}"#,
            always,
            |s, p| view_flag(s, p, Flag::Preview),
            true,
        ),
        spec(
            "multicam.autoAdjustQuality",
            "Auto-Adjust Multi-Camera Playback Quality",
            &[],
            r#"{"enabled":bool?}"#,
            always,
            |s, p| view_flag(s, p, Flag::AutoQuality),
            true,
        ),
        spec("multicam.transmitView", "Transmit Multi-Camera View", &[], r#"{"enabled":bool?}"#, always, |s, p| view_flag(s, p, Flag::Transmit), true),
        CommandSpec {
            id: "multicam.grid",
            label: "Multi-Camera Grid",
            menu: &[],
            shortcut: None,
            params: r#"{"time":ticks?,"playing":bool?,"cellPixels":f32?,"playbackScale":f32?}"#,
            enabled: always,
            run: grid_info,
            journal: false,
        },
        CommandSpec {
            id: "multicam.inspect",
            label: "Inspect Multi-Camera",
            menu: &[],
            shortcut: None,
            params: r#"{"time":ticks?}"#,
            enabled: always,
            run: inspect,
            journal: false,
        },
    ]
}

// ---------------------------------------------------------------- enablement

fn can_synchronize(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    let sel = with_links(s, &s.state.selection);
    if sel.len() < 2 { Err("select at least two clips to synchronize".into()) } else { Ok(()) }
}
fn has_items(s: &Session) -> std::result::Result<(), String> {
    if s.state.project_selection.iter().any(|i| sync::project_clip(&s.project, *i).is_some()) {
        Ok(())
    } else {
        Err("select clips in the Project panel".into())
    }
}
fn has_two_items(s: &Session) -> std::result::Result<(), String> {
    if s.state.project_selection.iter().filter(|i| sync::project_clip(&s.project, **i).is_some()).count() >= 2 {
        Ok(())
    } else {
        Err("select a video clip and audio clips in the Project panel".into())
    }
}

// ---------------------------------------------------------------- helpers

fn items_p(s: &Session, p: &Value) -> Vec<ItemId> {
    match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
        None => s.state.project_selection.clone(),
    }
}

fn method_p(p: &Value, cmd: &str) -> Result<Method> {
    Method::from_params(p).map_err(|e| bad(cmd, e))
}

fn offset_p(p: &Value, rate: FrameRate) -> Tick {
    p.get("offset").and_then(Value::as_i64).map(|f| rate.tick_of(f)).unwrap_or(Tick::ZERO)
}

/// Run `f` on sequence `seq_id` of `p` with an edit context (ids, media durations), then check it.
fn seq_edit<R>(p: &mut Project, seq_id: ItemId, media: &Arc<MediaPool>, f: impl FnOnce(&mut Sequence, &mut EditCtx) -> Result<R>) -> Result<R> {
    let snapshot = Arc::new(p.clone());
    let snap = snapshot.clone();
    let media = media.clone();
    let durations = move |id: ItemId| crate::media_duration(&snapshot, &media, id);
    let starts = move |id: ItemId| crate::media_start(&snap, id);
    let min = p.sequence(seq_id).map(|s| s.settings.frame_rate.frame_duration()).unwrap_or(Tick(1));
    let mut next = p.next_id;
    let r = {
        let seq = p.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let mut ctx = EditCtx { next_id: &mut next, media_duration: &durations, media_start: &starts, min_duration: min };
        f(seq, &mut ctx)?
    };
    p.next_id = next;
    if let Some(q) = p.sequence(seq_id) {
        q.check().map_err(EngineError::Other)?;
    }
    Ok(r)
}

/// Frame-aligned placement that keeps sub-frame (sample) accuracy: a clip whose media range should
/// start at the exact timeline time `exact` starts at the next frame boundary instead, with its
/// source In moved by the difference. Returns (start, source In shift).
fn frame_align(exact: Tick, rate: FrameRate) -> (Tick, Tick) {
    let snapped = rate.snap(exact);
    let start = if snapped < exact { snapped + rate.frame_duration() } else { snapped };
    (start, start - exact)
}

/// A track item showing `range` of `item` whose media time `range.start` sits at timeline `exact`.
fn placed_item(p: &mut Project, item: ItemId, kind: TrackKind, exact: Tick, range: TimeRange, settings: &SequenceSettings) -> Option<TrackItem> {
    let rate = settings.frame_rate;
    let (start, shift) = frame_align(exact, rate);
    let src = TimeRange::from_bounds(range.start + shift, range.end());
    let mut ti = p.make_track_item(item, kind, start, src, rate)?;
    // whole frames that fit in the media
    let frames = rate.frame_at(src.duration).max(1);
    ti.duration = rate.tick_of(frames);
    if kind == TrackKind::Video {
        let size = sync_source_size(p, item).unwrap_or((settings.width, settings.height));
        for e in &mut ti.effects {
            resolve_auto_points(e, (settings.width, settings.height), size);
        }
    }
    Some(ti)
}

fn sync_source_size(p: &Project, item: ItemId) -> Option<(u32, u32)> {
    p.resolve_media(item).and_then(|(_, media, _)| media.info.video.as_ref().map(|video| (video.width, video.height)))
}

// ---------------------------------------------------------------- Synchronize

/// A set of linked timeline clips that move together, with the clip used to sync it.
struct Unit {
    members: Vec<ClipId>,
    rep: TrackItem,
    rep_track: (TrackKind, usize),
}

fn synchronize(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "clip.synchronize";
    let method = method_p(p, cmd)?;
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let rate = q.settings.frame_rate;
    let sel = with_links(s, &clips_p(s, p));
    // group into link units
    let mut units: Vec<Unit> = Vec::new();
    let mut seen: Vec<ClipId> = Vec::new();
    let pos = |c: ClipId| -> Option<(TrackKind, usize, TrackItem)> {
        for (k, ts) in [(TrackKind::Video, &q.video_tracks), (TrackKind::Audio, &q.audio_tracks)] {
            for (ti, t) in ts.iter().enumerate() {
                if let Some(it) = t.item(c) {
                    return Some((k, ti, it.clone()));
                }
            }
        }
        None
    };
    for &c in &sel {
        if seen.contains(&c) {
            continue;
        }
        let Some((_, _, it)) = pos(c) else { continue };
        let members: Vec<ClipId> = match it.link {
            Some(l) => sel.iter().copied().filter(|o| pos(*o).is_some_and(|x| x.2.link == Some(l))).collect(),
            None => vec![c],
        };
        seen.extend(&members);
        let mut cands: Vec<(TrackKind, usize, TrackItem)> = members.iter().filter_map(|m| pos(*m)).collect();
        // the representative: audio for audio sync, else video; lowest track first
        let audio_first = method == Method::Audio;
        cands.sort_by_key(|(k, ti, _)| (if (*k == TrackKind::Audio) == audio_first { 0 } else { 1 }, *ti));
        let (k, ti, rep) = cands.remove(0);
        units.push(Unit { members, rep, rep_track: (k, ti) });
    }
    if units.len() < 2 {
        return Err(bad(cmd, "select clips from at least two different sources"));
    }
    // reference unit
    let reference = if let Some(c) = u64_p(p, "reference").map(ClipId) {
        units.iter().position(|u| u.members.contains(&c)).ok_or_else(|| bad(cmd, "`reference` is not among the clips"))?
    } else if let Some(t) = track_p(s, p, "track", cmd)? {
        units
            .iter()
            .position(|u| u.members.iter().any(|m| q.track(t).is_some_and(|tr| tr.item(*m).is_some())))
            .ok_or_else(|| bad(cmd, "no selected clip on the reference track"))?
    } else {
        (0..units.len()).min_by_key(|&i| (if units[i].rep_track.0 == TrackKind::Video { 0 } else { 1 }, units[i].rep_track.1)).unwrap_or(0)
    };
    let clips: Vec<SyncClip> = units.iter().map(|u| sync::timeline_clip(&s.project, &u.rep)).collect();
    let anchors = sync::anchors(s, &clips, reference, &method, offset_p(p, rate))?;
    // timeline time of each unit's anchor must equal the reference's
    let at = |u: &Unit, a: Tick| u.rep.start + Tick(((a - u.rep.source_in).0 as f64 / u.rep.speed.abs().max(1e-9)).round() as i64);
    let t_ref = at(&units[reference], anchors[reference].anchor);
    let mut deltas: Vec<Tick> = units.iter().zip(&anchors).map(|(u, a)| t_ref - at(u, a.anchor)).collect();
    // nothing may move before the sequence start: shift everything (reference too) right
    let min_start = units
        .iter()
        .zip(&deltas)
        .flat_map(|(u, d)| u.members.iter().filter_map(|m| pos(*m)).map(move |x| x.2.start + *d).collect::<Vec<_>>())
        .min()
        .unwrap_or(Tick::ZERO);
    if min_start < Tick::ZERO {
        deltas.iter_mut().for_each(|d| *d -= min_start);
    }
    let mut moved = 0usize;
    let mut placements: Vec<(TrackId, TrackItem)> = Vec::new();
    let mut remove: Vec<ClipId> = Vec::new();
    for (u, d) in units.iter().zip(&deltas) {
        if *d == Tick::ZERO {
            continue;
        }
        moved += 1;
        // frame-aligned start, sub-frame remainder taken from the source In
        let (start, shift) = frame_align(u.rep.start + *d, rate);
        let dd = start - u.rep.start;
        for m in &u.members {
            let Some((tid, it)) = q.find_item(*m) else { continue };
            let mut it = it.clone();
            it.start += dd;
            if shift > Tick::ZERO {
                it.source_in += Tick((shift.0 as f64 * it.speed.abs()).round() as i64);
                it.duration = (it.duration - rate.frame_duration()).max(rate.frame_duration());
            }
            remove.push(*m);
            placements.push((tid, it));
        }
    }
    let starts: Vec<Tick> = units.iter().zip(&deltas).map(|(u, d)| u.rep.start + *d).collect();
    let rep = sync::report(&clips, &anchors, &starts, rate);
    if moved > 0 {
        let media = s.media.clone();
        s.edit(&format!("Synchronize ({})", method.name()), |pr, st| {
            seq_edit(pr, seq_id, &media, |seq, ctx| {
                edit::delete_items(seq, &remove);
                let ids = edit::overwrite(seq, placements, ctx)?;
                Ok(ids)
            })
            .map(|ids| {
                st.selection = ids;
            })
        })?;
    }
    Ok(json!({"moved": moved, "reference": reference, "clips": rep}))
}

// ---------------------------------------------------------------- Merge Clips / Create Multi-Camera

fn camera_name(p: &Project, item: ItemId, mode: &str, n: usize) -> String {
    let it = p.item(item);
    let clip = || it.map(|i| i.name.clone()).unwrap_or_else(|| format!("Camera {n}"));
    match mode {
        "track" | "tracks" | "trackname" | "trackNames" | "enumerate" => format!("Camera {n}"),
        "metadata" | "cameraAngle" | "label" => it
            .and_then(|i| {
                ["Camera Angle", "Camera Label", "Camera", "camera", "cameraAngle"].iter().find_map(|k| i.metadata.get(*k).filter(|v| !v.is_empty()).cloned())
            })
            .unwrap_or_else(clip),
        _ => clip(),
    }
}

/// Resolve and order the clips for Merge Clips / Create Multi-Camera.
fn sync_items(s: &Session, items: &[ItemId], cmd: &str) -> Result<Vec<SyncClip>> {
    let mut out = Vec::new();
    for i in items {
        let c = sync::project_clip(&s.project, *i).ok_or_else(|| bad(cmd, format!("item {} is not a clip", i.0)))?;
        if !out.iter().any(|o: &SyncClip| o.item == c.item) {
            out.push(c);
        }
    }
    if out.is_empty() {
        return Err(bad(cmd, "no clips"));
    }
    Ok(out)
}

fn settings_for(s: &Session, item: ItemId) -> SequenceSettings {
    let media = match s.project.item(item).map(|i| &i.kind) {
        Some(ItemKind::Media(m)) => Some(m),
        Some(ItemKind::Subclip { parent, .. }) => s.project.item(*parent).and_then(|i| i.as_media()),
        _ => None,
    };
    let mut st = media.map(crate::commands::default_seq_settings_for).unwrap_or_default();
    if let Some(m) = s.project.item(item).and_then(|i| i.as_media())
        && let Some(r) = m.interpret.frame_rate
    {
        st.frame_rate = r;
    }
    st
}

fn processed_bin(p: &mut Project) -> filmcraft_project::BinId {
    if let Some(b) = p.root.children.iter().find_map(|c| match c {
        BinEntry::Bin(b) if b.name == "Processed Clips" => Some(b.id),
        _ => None,
    }) {
        return b;
    }
    p.add_bin("Processed Clips", None)
}

fn merge_clips(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "clip.mergeClips";
    let method = method_p(p, cmd)?;
    let clips = sync_items(s, &items_p(s, p), cmd)?;
    let has_v = |c: &SyncClip| s.project.item(c.item).is_some_and(|i| i.has_video());
    let has_a = |c: &SyncClip| s.project.item(c.item).is_some_and(|i| i.has_audio());
    let videos: Vec<usize> = (0..clips.len()).filter(|&i| has_v(&clips[i])).collect();
    if videos.len() > 1 {
        return Err(bad(cmd, "Merge Clips takes one video clip plus audio clips"));
    }
    let audio_n = clips.iter().filter(|c| !has_v(c)).count();
    if audio_n == 0 || audio_n > 16 {
        return Err(bad(cmd, "Merge Clips takes 1–16 audio clips"));
    }
    // the video clip (or the first audio clip) is the reference
    let reference = videos.first().copied().unwrap_or(0);
    let mut order: Vec<usize> = vec![reference];
    order.extend((0..clips.len()).filter(|&i| i != reference));
    let clips: Vec<SyncClip> = order.iter().map(|&i| clips[i].clone()).collect();
    let settings = settings_for(s, clips[0].item);
    let rate = settings.frame_rate;
    let anchors = sync::anchors(s, &clips, 0, &method, offset_p(p, rate))?;
    let starts = sync::placements(&clips, &anchors, rate, false);
    let remove_video_audio = bool_p(p, "removeVideoAudio").unwrap_or(false);
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| format!("{} - Merged", clips[0].name));
    let video = videos.first().map(|_| clips[0].item);
    let vid_audio = video.is_some() && has_a(&clips[0]) && !remove_video_audio;
    let audio_items: Vec<usize> = (1..clips.len()).collect();
    let n_audio = audio_items.len() + vid_audio as usize;
    let rep = sync::report(&clips, &anchors, &starts, rate);
    let bin = s.project.root.parent_of(clips[0].item).filter(|b| *b != s.project.root.id);
    let id = s.edit("Merge Clips", |pr, st| {
        let sid = pr.new_sequence(&name, settings.clone(), video.is_some() as usize, n_audio, bin);
        let mut v_items = Vec::new();
        let mut a_items = Vec::new();
        let link = pr.alloc_id();
        if video.is_some() {
            let mut v = placed_item(pr, clips[0].item, TrackKind::Video, starts[0], clips[0].range, &settings).ok_or_else(|| bad(cmd, "bad video clip"))?;
            v.link = Some(link);
            v_items.push(v);
            if vid_audio {
                let mut a = placed_item(pr, clips[0].item, TrackKind::Audio, starts[0], clips[0].range, &settings).ok_or_else(|| bad(cmd, "bad clip"))?;
                a.link = Some(link);
                a_items.push(a);
            }
        }
        for &i in &audio_items {
            let mut a = placed_item(pr, clips[i].item, TrackKind::Audio, starts[i], clips[i].range, &settings).ok_or_else(|| bad(cmd, "bad audio clip"))?;
            a.link = Some(link);
            a_items.push(a);
        }
        let q = pr.sequence_mut(sid).ok_or(EngineError::NoSequence)?;
        for (t, it) in q.video_tracks.iter_mut().zip(v_items) {
            t.items.push(it);
        }
        for (t, it) in q.audio_tracks.iter_mut().zip(a_items) {
            t.items.push(it);
        }
        q.merged = Some(MergedClip { video, audio: audio_items.iter().map(|&i| clips[i].item).collect(), sync: method.name().into() });
        q.check().map_err(EngineError::Other)?;
        if let Some(it) = pr.item_mut(sid) {
            it.label = Label::Iris;
        }
        st.project_selection = vec![sid];
        Ok(sid)
    })?;
    Ok(json!({"item": id.0, "clips": rep}))
}

fn create_multicam(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "clip.createMulticam";
    let method = method_p(p, cmd)?;
    let mut clips = sync_items(s, &items_p(s, p), cmd)?;
    let has_v = |s: &Session, c: &SyncClip| s.project.item(c.item).is_some_and(|i| i.has_video());
    let has_a = |s: &Session, c: &SyncClip| s.project.item(c.item).is_some_and(|i| i.has_audio());
    // cameras with video first (selection order), then audio-only sources
    clips.sort_by_key(|c| !has_v(s, c));
    if !clips.iter().any(|c| has_v(s, c)) {
        return Err(bad(cmd, "select at least one clip with video"));
    }
    let names_mode = str_p(p, "cameraNames").unwrap_or("clip").to_string();
    if names_mode == "metadata" {
        // cameras ordered by their Camera Angle metadata when present
        let key = |c: &SyncClip| s.project.item(c.item).and_then(|i| i.metadata.get("Camera Angle").cloned());
        let mut v: Vec<(bool, Option<String>, SyncClip)> = clips.iter().map(|c| (!has_v(s, c), key(c), c.clone())).collect();
        v.sort_by_key(|a| (a.0, a.1.is_none(), a.1.clone()));
        clips = v.into_iter().map(|x| x.2).collect();
    }
    let reference = match item_p(p, "reference") {
        Some(r) => clips.iter().position(|c| c.item == r).ok_or_else(|| bad(cmd, "`reference` is not among the items"))?,
        None => 0,
    };
    let audio = match str_p(p, "audio") {
        Some(a) => MulticamAudio::from_name(a).ok_or_else(|| bad(cmd, format!("unknown audio mode `{a}` (camera1, all, switch)")))?,
        None => MulticamAudio::Camera1,
    };
    let settings = settings_for(s, clips[reference].item);
    let rate = settings.frame_rate;
    let anchors = sync::anchors(s, &clips, reference, &method, offset_p(p, rate))?;
    let starts = sync::placements(&clips, &anchors, rate, false);
    let rep = sync::report(&clips, &anchors, &starts, rate);
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| format!("{} Multicam", clips[reference].name));
    let move_processed = bool_p(p, "processedBin").unwrap_or(false);
    let flags: Vec<(bool, bool)> = clips.iter().map(|c| (has_v(s, c), has_a(s, c))).collect();
    let names: Vec<String> = clips.iter().enumerate().map(|(i, c)| camera_name(&s.project, c.item, &names_mode, i + 1)).collect();
    let nv = flags.iter().filter(|f| f.0).count();
    let na = flags.iter().filter(|f| f.1).count();
    let id = s.edit("Create Multi-Camera Source Sequence", |pr, st| {
        let sid = pr.new_sequence(&name, settings.clone(), nv, na.max(1), None);
        let mut cameras = Vec::new();
        let (mut vi, mut ai) = (0usize, 0usize);
        let mut placed: Vec<(TrackKind, usize, TrackItem)> = Vec::new();
        let (vtracks, atracks): (Vec<TrackId>, Vec<TrackId>) = {
            let q = pr.sequence(sid).ok_or(EngineError::NoSequence)?;
            (q.video_tracks.iter().map(|t| t.id).collect(), q.audio_tracks.iter().map(|t| t.id).collect())
        };
        for (k, c) in clips.iter().enumerate() {
            let (hv, ha) = flags[k];
            let link = (hv && ha).then(|| pr.alloc_id());
            let mut cam = Camera { name: names[k].clone(), video_track: None, audio_tracks: Vec::new(), enabled: true, source: Some(c.item) };
            if hv {
                let mut v = placed_item(pr, c.item, TrackKind::Video, starts[k], c.range, &settings).ok_or_else(|| bad(cmd, "bad clip"))?;
                v.link = link;
                placed.push((TrackKind::Video, vi, v));
                cam.video_track = Some(vtracks[vi]);
                vi += 1;
            }
            if ha {
                let mut a = placed_item(pr, c.item, TrackKind::Audio, starts[k], c.range, &settings).ok_or_else(|| bad(cmd, "bad clip"))?;
                a.link = link;
                placed.push((TrackKind::Audio, ai, a));
                cam.audio_tracks.push(atracks[ai]);
                ai += 1;
            }
            cameras.push(cam);
        }
        {
            let q = pr.sequence_mut(sid).ok_or(EngineError::NoSequence)?;
            for (kind, idx, it) in placed {
                q.tracks_mut(kind)[idx].items.push(it);
            }
            for cam in &cameras {
                if let Some(vt) = cam.video_track.and_then(|id| q.video_tracks.iter_mut().find(|x| x.id == id)) {
                    vt.name = cam.name.clone();
                }
                for at in &cam.audio_tracks {
                    if let Some(x) = q.audio_tracks.iter_mut().find(|x| x.id == *at) {
                        x.name = cam.name.clone();
                    }
                }
            }
            q.multicam = Some(MulticamSource { cameras, audio, sync: method.name().into() });
            q.check().map_err(EngineError::Other)?;
        }
        if let Some(it) = pr.item_mut(sid) {
            it.label = Label::Mango;
        }
        if move_processed {
            let bin = processed_bin(pr);
            for c in &clips {
                pr.root.remove_item(c.item);
                if let Some(b) = pr.root.find_bin_mut(bin) {
                    b.children.push(BinEntry::Item(c.item));
                }
            }
        }
        st.project_selection = vec![sid];
        Ok(sid)
    })?;
    Ok(json!({"sequence": id.0, "cameras": names, "clips": rep}))
}

// ---------------------------------------------------------------- multi-camera clips

/// The topmost enabled multi-camera video clip at `t` (targeted tracks first) in the active
/// sequence: (track, clip).
///
/// Targeted tracks are searched first, then every video track; from the lowest track up, or from
/// the topmost down with Multi-Camera Selection Top Down.
pub fn multicam_clip_at(s: &Session, t: Tick) -> Option<(TrackId, TrackItem)> {
    let q = s.active_sequence()?;
    let tg = s.targeting().targeted;
    let at = |tr: &Track| tr.item_at(t).filter(|i| i.multicam.is_some_and(|m| m.enabled) && s.project.sequence(i.item).is_some()).cloned();
    let order: Vec<&Track> = if s.state.multicam_view.top_down { q.video_tracks.iter().rev().collect() } else { q.video_tracks.iter().collect() };
    let targeted = order.iter().filter(|tr| tg.contains(&tr.id)).find_map(|tr| at(tr).map(|i| (tr.id, i)));
    targeted.or_else(|| order.iter().find_map(|tr| at(tr).map(|i| (tr.id, i))))
}

/// Resolve `camera` (1-based, shown order) or `angle` (0-based) for the source of `clip`.
fn angle_p(s: &Session, p: &Value, source: ItemId, cmd: &str) -> Result<u32> {
    let q = s.project.sequence(source).ok_or_else(|| bad(cmd, "not a multi-camera clip"))?;
    let cams = q.cameras();
    if let Some(a) = u64_p(p, "angle") {
        if (a as usize) < cams.cameras.len() {
            return Ok(a as u32);
        }
        return Err(bad(cmd, format!("angle {a} out of range (0–{})", cams.cameras.len().saturating_sub(1))));
    }
    let mut n = u64_p(p, "camera").ok_or_else(|| bad(cmd, "need `camera` (1-based) or `angle`"))? as usize;
    let shown = cams.shown_angles();
    if bool_p(p, "pageRelative").unwrap_or(false) {
        n += s.state.multicam_view.page_layout(shown.len()).first;
    }
    shown.get(n.wrapping_sub(1)).map(|a| *a as u32).ok_or_else(|| bad(cmd, format!("there is no camera {n} ({} shown)", shown.len())))
}

/// Linked audio clips of `video` that are multi-camera clips.
fn audio_partners(q: &Sequence, video: &TrackItem) -> Vec<ClipId> {
    let Some(l) = video.link else { return Vec::new() };
    q.audio_tracks.iter().flat_map(|t| t.items.iter()).filter(|i| i.link == Some(l) && i.multicam.is_some()).map(|i| i.id).collect()
}

fn switch_angle(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "multicam.switchAngle";
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let video_only = bool_p(p, "videoOnly").unwrap_or(false);
    let audio_only = bool_p(p, "audioOnly").unwrap_or(false);
    let follows = s.state.multicam_audio_follows_video;
    let videos: Vec<TrackItem> = if p.get("clips").is_some() || p.get("clip").is_some() {
        clips_p(s, p).iter().filter_map(|c| q.find_item(*c).map(|x| x.1.clone())).filter(|i| i.multicam.is_some()).collect()
    } else {
        multicam_clip_at(s, t).map(|x| vec![x.1]).unwrap_or_default()
    };
    if videos.is_empty() {
        return Err(EngineError::Other("no multi-camera clip at the playhead".into()));
    }
    let angle = angle_p(s, p, videos[0].item, cmd)?;
    let mut targets: Vec<ClipId> = Vec::new();
    for v in &videos {
        let is_audio = q.audio_tracks.iter().any(|t| t.item(v.id).is_some());
        if is_audio {
            targets.push(v.id);
            continue;
        }
        if !audio_only {
            targets.push(v.id);
        }
        if audio_only || (follows && !video_only) {
            targets.extend(audio_partners(&q, v));
        }
    }
    let n = s.edit_sequence("Switch Multi-Camera Angle", |seq, _, _| Ok(edit::multicam::set_angle(seq, &targets, angle)))?;
    Ok(json!({"angle": angle, "changed": n}))
}

fn enable(s: &mut Session, p: &Value) -> Result<Value> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let clips = with_links(s, &clips_p(s, p));
    let nested: Vec<TrackItem> = clips.iter().filter_map(|c| q.find_item(*c).map(|x| x.1.clone())).filter(|i| s.project.sequence(i.item).is_some()).collect();
    if nested.is_empty() {
        return Err(EngineError::Other("select nested sequence or multi-camera clips".into()));
    }
    let on = bool_p(p, "enabled").unwrap_or_else(|| nested.iter().any(|i| !i.multicam.is_some_and(|m| m.enabled)));
    let project = s.project.clone();
    let ids: Vec<ClipId> = nested.iter().map(|i| i.id).collect();
    let n = s.edit_sequence(if on { "Enable Multi-Camera" } else { "Disable Multi-Camera" }, |seq, _, _| {
        let mut n = 0;
        for t in seq.all_tracks_mut() {
            for it in t.items.iter_mut().filter(|i| ids.contains(&i.id)) {
                let first = project.sequence(it.item).and_then(|q| q.cameras().first_video_angle()).unwrap_or(0) as u32;
                let sel = it.multicam.get_or_insert(MulticamSel { enabled: false, angle: first });
                if sel.enabled != on {
                    sel.enabled = on;
                    n += 1;
                }
            }
        }
        Ok(n)
    })?;
    Ok(json!({"enabled": on, "changed": n}))
}

fn flatten(s: &mut Session, p: &Value) -> Result<Value> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let clips = with_links(s, &clips_p(s, p));
    let mut targets: Vec<(TrackKind, usize, TrackItem)> = Vec::new();
    for (k, ts) in [(TrackKind::Video, &q.video_tracks), (TrackKind::Audio, &q.audio_tracks)] {
        for (ti, t) in ts.iter().enumerate() {
            for it in t.items.iter().filter(|i| clips.contains(&i.id) && i.multicam.is_some_and(|m| m.enabled)) {
                targets.push((k, ti, it.clone()));
            }
        }
    }
    if targets.is_empty() {
        return Err(EngineError::Other("select multi-camera clips to flatten".into()));
    }
    let project = s.project.clone();
    let media = s.media.clone();
    let n = targets.len();
    let ids = s.edit("Flatten Multi-Camera", |pr, st| {
        // extra audio tracks needed for sources whose audio spans several tracks
        let need_a = targets
            .iter()
            .filter(|t| t.0 == TrackKind::Audio)
            .map(|(_, ti, it)| {
                let nq = project.sequence(it.item).map(|nq| audible(nq, it).len()).unwrap_or(1);
                ti + nq.max(1)
            })
            .max()
            .unwrap_or(0);
        let have = pr.sequence(seq_id).map(|q| q.audio_tracks.len()).unwrap_or(0);
        for k in have..need_a {
            let id = TrackId(pr.alloc_id());
            if let Some(q) = pr.sequence_mut(seq_id) {
                q.audio_tracks.push(Track::new(id, TrackKind::Audio, format!("Audio {}", k + 1)));
            }
        }
        let new_ids = seq_edit(pr, seq_id, &media, |seq, ctx| {
            let mut links = HashMap::new();
            let mut out = Vec::new();
            for (kind, ti, it) in &targets {
                let Some(nested) = project.sequence(it.item) else { continue };
                let mut pieces: Vec<(TrackId, TrackItem)> = Vec::new();
                match kind {
                    TrackKind::Video => {
                        let angle = it.multicam.map(|m| m.angle as usize).unwrap_or(0);
                        if let Some(nt) = nested.angle_video_track_index(angle).and_then(|i| nested.video_tracks.get(i)) {
                            let dest = seq.video_tracks[*ti].id;
                            pieces.extend(edit::multicam::flatten_items(it, nt, ctx, &mut links)?.into_iter().map(|x| (dest, x)));
                        }
                    }
                    TrackKind::Audio => {
                        for (k, at) in audible(nested, it).iter().enumerate() {
                            let Some(nt) = nested.audio_tracks.iter().find(|x| x.id == *at) else { continue };
                            let Some(dest) = seq.audio_tracks.get(ti + k).map(|t| t.id) else { continue };
                            pieces.extend(edit::multicam::flatten_items(it, nt, ctx, &mut links)?.into_iter().map(|x| (dest, x)));
                        }
                    }
                }
                out.extend(edit::multicam::replace_with(seq, it.id, pieces, ctx)?);
            }
            Ok(out)
        })?;
        st.selection = new_ids.clone();
        Ok(new_ids)
    })?;
    Ok(json!({"flattened": n, "clips": ids.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

/// Audio tracks of `nested` a multi-camera audio clip plays.
fn audible(nested: &Sequence, it: &TrackItem) -> Vec<TrackId> {
    let angle = it.multicam_angle(nested);
    match &nested.multicam {
        Some(mc) => mc.audible_tracks(if mc.audio == MulticamAudio::SwitchAudio { angle } else { None }),
        None => nested.audio_tracks.iter().filter(|t| !t.muted).map(|t| t.id).collect(),
    }
}

// ---------------------------------------------------------------- live switching

/// A live switching pass (Multi-Camera view, playing): every cut is applied at once, and the whole
/// pass is one undo step.
#[derive(Clone, Debug, Default)]
pub struct Recorder {
    /// Sequence being recorded (None = no pass running).
    pub seq: Option<ItemId>,
    pub start: Tick,
    /// Tracks the pass switches (fixed by the first cut).
    pub tracks: Vec<TrackId>,
    pub cuts: Vec<Cut>,
    /// The multi-camera source being switched (fixed by the first cut).
    pub source: Option<ItemId>,
    /// The project before the pass's first cut.
    base: Option<Arc<Project>>,
    key: String,
    pass: u64,
}

impl Recorder {
    pub fn active(&self) -> bool {
        self.seq.is_some()
    }
}

const RECORD_LABEL: &str = "Record Multi-Camera";

fn record_start(s: &mut Session, p: &Value) -> Result<Value> {
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let pass = s.mcrec.pass + 1;
    s.mcrec = Recorder { seq: s.state.active_sequence, start: t, pass, key: format!("multicam-record-{pass}"), ..Default::default() };
    Ok(json!({"recording": true, "start": t.0}))
}

/// Re-apply the pass from its base project up to `end`.
fn apply_pass(s: &mut Session, end: Tick) -> Result<usize> {
    let Some(seq_id) = s.mcrec.seq else { return Ok(0) };
    let merging = s.history.merge_key.as_deref() == Some(s.mcrec.key.as_str()) && s.history.undo.last().is_some_and(|u| u.0 == RECORD_LABEL);
    let base = match s.mcrec.base.clone() {
        Some(b) if merging => b,
        _ => {
            let b = s.project.clone();
            s.mcrec.base = Some(b.clone());
            b
        }
    };
    let (tracks, cuts, key) = (s.mcrec.tracks.clone(), s.mcrec.cuts.clone(), s.mcrec.key.clone());
    let Some(source) = s.mcrec.source else { return Ok(0) };
    let media = s.media.clone();
    s.edit_merged(RECORD_LABEL, &key, |pr, _| {
        *pr = (*base).clone();
        seq_edit(pr, seq_id, &media, |seq, ctx| Ok(edit::multicam::record(seq, &tracks, source, &cuts, end, ctx)))
    })
}

fn cut(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "multicam.cut";
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    if !s.mcrec.active() || s.mcrec.seq != s.state.active_sequence {
        // stopped: switch the clip at the playhead
        return switch_angle(s, p);
    }
    let (_, clip) = multicam_clip_at(s, t).ok_or_else(|| EngineError::Other("no multi-camera clip at the playhead".into()))?;
    let angle = angle_p(s, p, clip.item, cmd)?;
    if s.mcrec.tracks.is_empty() {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let mut tracks: Vec<TrackId> =
            q.video_tracks.iter().filter(|tr| tr.item_at(t).is_some_and(|i| i.item == clip.item && i.multicam.is_some())).map(|tr| tr.id).collect();
        tracks.truncate(1);
        if s.state.multicam_audio_follows_video && !bool_p(p, "videoOnly").unwrap_or(false) {
            let partners = audio_partners(q, &clip);
            tracks.extend(q.audio_tracks.iter().filter(|tr| tr.items.iter().any(|i| partners.contains(&i.id))).map(|tr| tr.id));
        }
        s.mcrec.tracks = tracks;
    }
    s.mcrec.source = Some(clip.item);
    s.mcrec.cuts.push(Cut { time: t, angle });
    // provisional: the new angle runs to the end of this multi-camera clip (its contiguous pieces)
    // until the pass stops
    let end = s
        .active_sequence()
        .and_then(|q| q.track(s.mcrec.tracks[0]))
        .map(|tr| {
            let mut end = clip.end();
            for it in tr.items.iter().filter(|i| i.start >= clip.end()) {
                if it.start != end || it.item != clip.item {
                    break;
                }
                end = it.end();
            }
            end
        })
        .unwrap_or(clip.end());
    apply_pass(s, end.max(t))?;
    Ok(json!({"angle": angle, "time": t.0, "cuts": s.mcrec.cuts.len()}))
}

/// Ctrl+N: an edit at the playhead on the multi-camera clip (and, when audio follows video, its
/// linked audio), and camera N from there to the clip's end.
fn cut_to_camera(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "multicam.cutToCamera";
    if s.mcrec.active() && s.mcrec.seq == s.state.active_sequence {
        return cut(s, p);
    }
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let (_, clip) = multicam_clip_at(s, t).ok_or_else(|| EngineError::Other("no multi-camera clip at the playhead".into()))?;
    let angle = angle_p(s, p, clip.item, cmd)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut clips = vec![clip.id];
    if s.state.multicam_audio_follows_video && !bool_p(p, "videoOnly").unwrap_or(false) {
        clips.extend(audio_partners(q, &clip));
    }
    let n = s.edit_sequence("Cut to Camera", |seq, ctx, _| {
        let new = edit::razor_items(seq, &clips, t, ctx);
        // the pieces starting at t (new right halves, or the clips themselves at an edit point)
        let right: Vec<ClipId> =
            seq.all_tracks().flat_map(|tr| tr.items.iter()).filter(|i| i.start == t && (new.contains(&i.id) || clips.contains(&i.id))).map(|i| i.id).collect();
        Ok(edit::multicam::set_angle(seq, &right, angle))
    })?;
    Ok(json!({"angle": angle, "changed": n}))
}

fn record_stop(s: &mut Session, p: &Value) -> Result<Value> {
    if !s.mcrec.active() {
        return Ok(json!({"recording": false, "cuts": 0}));
    }
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let n = s.mcrec.cuts.len();
    let mut points = 0;
    if n > 0 {
        points = apply_pass(s, t)?;
    }
    let pass = s.mcrec.pass;
    s.mcrec = Recorder { pass, ..Default::default() };
    s.history.merge_key = None;
    Ok(json!({"recording": false, "cuts": n, "editPoints": points}))
}

fn audio_follows(s: &mut Session, p: &Value) -> Result<Value> {
    let on = bool_p(p, "enabled").unwrap_or(!s.state.multicam_audio_follows_video);
    s.state.multicam_audio_follows_video = on;
    Ok(json!({"enabled": on}))
}

fn edit_cameras(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "multicam.editCameras";
    let id = match item_p(p, "sequence") {
        Some(i) => i,
        None => multicam_clip_at(s, s.playhead()).map(|c| c.1.item).ok_or_else(|| bad(cmd, "need `sequence` (or a multi-camera clip at the playhead)"))?,
    };
    let q = s.project.sequence(id).ok_or_else(|| bad(cmd, "no such sequence"))?;
    if q.multicam.is_none() {
        return Err(bad(cmd, "not a multi-camera source sequence"));
    }
    let changes: Vec<Value> = p.get("cameras").and_then(Value::as_array).cloned().unwrap_or_default();
    let audio = match str_p(p, "audio") {
        Some(a) => Some(MulticamAudio::from_name(a).ok_or_else(|| bad(cmd, format!("unknown audio mode `{a}`")))?),
        None => None,
    };
    s.edit("Edit Cameras", |pr, _| {
        let q = pr.sequence_mut(id).ok_or_else(|| bad(cmd, "no such sequence"))?;
        let mc = q.multicam.as_mut().ok_or_else(|| bad(cmd, "not a multi-camera source"))?;
        for c in &changes {
            let a = c.get("angle").and_then(Value::as_u64).ok_or_else(|| bad(cmd, "each camera needs `angle`"))? as usize;
            let cam = mc.cameras.get_mut(a).ok_or_else(|| bad(cmd, format!("no angle {a}")))?;
            if let Some(n) = c.get("name").and_then(Value::as_str) {
                cam.name = n.to_string();
            }
            if let Some(e) = c.get("enabled").and_then(Value::as_bool) {
                cam.enabled = e;
            }
        }
        if let Some(a) = audio {
            mc.audio = a;
        }
        Ok(())
    })?;
    Ok(json!({"sequence": id.0}))
}

/// The multi-camera clip at the playhead (or `time`), for the Multi-Camera view and agents.
pub fn inspect_at(s: &Session, t: Tick) -> Value {
    let rec = json!({"recording": s.mcrec.active(), "cuts": s.mcrec.cuts.iter().map(|c| json!({"time": c.time.0, "angle": c.angle})).collect::<Vec<_>>()});
    let Some((track, clip)) = multicam_clip_at(s, t) else {
        return json!({"clip": null, "recorder": rec, "audioFollowsVideo": s.state.multicam_audio_follows_video});
    };
    let q = s.project.sequence(clip.item);
    let cams = q.map(|q| q.cameras()).unwrap_or_default();
    let audio_angle = s
        .active_sequence()
        .map(|seq| audio_partners(seq, &clip))
        .and_then(|ps| ps.first().and_then(|c| s.active_sequence().and_then(|seq| seq.find_item(*c)).and_then(|x| x.1.multicam.map(|m| m.angle))));
    json!({
        "clip": clip.id.0,
        "track": track.0,
        "source": clip.item.0,
        "sourceName": s.project.item(clip.item).map(|i| i.name.clone()),
        "sourceTime": clip.source_time_at(t).0,
        "angle": clip.multicam.map(|m| m.angle),
        "audioAngle": audio_angle,
        "audio": cams.audio.name(),
        "shown": cams.shown_angles(),
        "cameras": cams.cameras.iter().enumerate().map(|(i, c)| json!({"angle": i, "name": c.name, "enabled": c.enabled, "video": c.video_track.is_some()})).collect::<Vec<_>>(),
        "recorder": rec,
        "audioFollowsVideo": s.state.multicam_audio_follows_video,
        "view": s.state.multicam_view,
    })
}

// ---------------------------------------------------------------- Multi-Camera view settings

fn grid_layout(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "multicam.gridLayout";
    let l = str_p(p, "layout").ok_or_else(|| bad(cmd, "need `layout` (auto, 2x2, 3x3, 4x4)"))?;
    s.state.multicam_view.layout = match l.to_ascii_lowercase().replace(['×', ' '], "x").as_str() {
        "auto" | "automatic" => None,
        "2x2" | "2" => Some(2),
        "3x3" | "3" => Some(3),
        "4x4" | "4" => Some(4),
        _ => return Err(bad(cmd, format!("unknown layout `{l}` (auto, 2x2, 3x3, 4x4)"))),
    };
    s.state.multicam_view.page = 0;
    Ok(json!({"layout": s.state.multicam_view.layout_name()}))
}

/// Shown angles of the multi-camera clip at the playhead (0 without one).
fn shown_count(s: &Session) -> usize {
    multicam_clip_at(s, s.playhead()).and_then(|(_, c)| s.project.sequence(c.item).map(|q| q.cameras().shown_angles().len())).unwrap_or(0)
}

fn set_page(s: &mut Session, p: &Value) -> Result<Value> {
    let n = shown_count(s);
    let l = s.state.multicam_view.page_layout(n);
    let page = match p.get("page") {
        Some(Value::String(d)) if d == "next" => (l.page + 1).min(l.pages - 1),
        Some(Value::String(d)) if d == "prev" || d == "previous" => l.page.saturating_sub(1),
        Some(v) => v.as_u64().ok_or_else(|| bad("multicam.page", "`page` is a 0-based number, \"next\" or \"prev\""))? as usize,
        None => return Err(bad("multicam.page", "need `page`")),
    };
    let l = filmcraft_render::multicam::page_layout(n, s.state.multicam_view.layout, page);
    s.state.multicam_view.page = l.page;
    Ok(json!({"page": l.page, "pages": l.pages}))
}

enum Flag {
    TopDown,
    Preview,
    AutoQuality,
    Transmit,
}

fn view_flag(s: &mut Session, p: &Value, f: Flag) -> Result<Value> {
    let v = &mut s.state.multicam_view;
    let slot = match f {
        Flag::TopDown => &mut v.top_down,
        Flag::Preview => &mut v.show_preview,
        Flag::AutoQuality => &mut v.auto_quality,
        Flag::Transmit => &mut v.transmit,
    };
    *slot = bool_p(p, "enabled").unwrap_or(!*slot);
    let on = *slot;
    let mut out = json!({"enabled": on});
    if matches!(f, Flag::Transmit) {
        // no transmit (video output) devices yet: the setting is kept for when there are
        out["device"] = Value::Null;
    }
    Ok(out)
}

/// The grid page at the playhead (or `time`): layout, pages and cells. With `cellPixels` (on-screen
/// cell width in pixels), `playing` and `playbackScale` it also returns the decode scale the
/// view uses (Auto-Adjust Multi-Camera Playback Quality).
fn grid_info(s: &mut Session, p: &Value) -> Result<Value> {
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let v = s.state.multicam_view.clone();
    let Some((_, clip)) = multicam_clip_at(s, t) else {
        return Ok(json!({"clip": null, "view": v}));
    };
    let q = s.project.sequence(clip.item).ok_or(EngineError::NoSequence)?;
    let cams = q.cameras();
    let shown = cams.shown_angles();
    let l = v.page_layout(shown.len());
    let active = clip.multicam.map(|m| m.angle as usize);
    let cells: Vec<Value> = shown
        .iter()
        .enumerate()
        .skip(l.first)
        .take(l.count)
        .map(|(k, &a)| {
            json!({"cell": k - l.first, "camera": k + 1, "angle": a, "name": cams.cameras.get(a).map(|c| c.name.clone()), "active": active == Some(a)})
        })
        .collect();
    let mut out = json!({
        "clip": clip.id.0, "source": clip.item.0, "layout": v.layout_name(), "cols": l.cols, "rows": l.rows,
        "page": l.page, "pages": l.pages, "angles": shown.len(), "cells": cells, "view": v,
    });
    if let Some(px) = p.get("cellPixels").and_then(Value::as_f64) {
        let playing = bool_p(p, "playing").unwrap_or(false);
        let pb = p.get("playbackScale").and_then(Value::as_f64).unwrap_or(1.0) as f32;
        out["cellScale"] = json!(filmcraft_render::multicam::grid_cell_scale(q.settings.width, px as f32, pb, playing, v.auto_quality, l.per_page));
    }
    Ok(out)
}

fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    Ok(inspect_at(s, t))
}
