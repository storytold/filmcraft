//! Edit / Clip / File menu commands for project items and clips (M3.10): labels and Select Label
//! Group, Paste / Remove Attributes, Select All Matching, Remove Unused, Consolidate Duplicates,
//! Sequence From Clip, Bin From Selection, Offline File, Close Project, Save All, Make / Edit
//! Subclip, Modify ▸ Audio Channels / Timecode, Video Options (Frame Hold Options, Add Frame Hold,
//! Insert Frame Hold Segment, Field Options, Time Interpolation, Fit / Fill frame), Audio Options
//! (Breakout to Mono, Extract Audio) and Replace With Clip.
//!
//! Every command mutates through `Session::edit` / `edit_sequence`, so it is one undo step.
//! [`apply_layout`] places these commands (and a few existing ones) in Premiere's menu order.

use std::collections::{BTreeMap, BTreeSet};

use filmcraft_edit as edit;
use filmcraft_project::{
    AudioChannelMap, AudioChannels, BinEntry, ClipId, FieldOptions, FieldProcessing, ItemId, ItemKind, Label, MediaClip, MediaRef, ParamValue, Project,
    TimeInterpolation, TrackItem, TrackKind,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange, parse_timecode};
use serde_json::{Value, json};

use crate::commands::{
    CommandSpec, always, bad, bool_p, clips_p, f64_p, has_project_selection, has_selection, has_seq, item_p, place_item, source_range, str_p, time_p, u64_p,
    with_links,
};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    shortcut: Option<&'static str>,
    params: &'static str,
    enabled: Enabled,
    run: Run,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut, params, enabled, run, journal: true }
}

macro_rules! label_cmd {
    ($id:literal, $name:literal, $l:expr) => {
        CommandSpec {
            id: $id,
            label: $name,
            menu: &["Edit", "Label"],
            shortcut: None,
            params: r#"{"items":[id]?,"clips":[id]?}"#,
            enabled: can_label,
            run: |s, p| apply_label(s, p, $l),
            journal: true,
        }
    };
}

/// The new commands.
pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        // ---------------- File ----------------
        spec("file.newSequenceFromClip", "Sequence From Clip", &["File", "New"], None, r#"{"items":[id]?}"#, has_clip_items, sequence_from_clip),
        spec(
            "file.newBinFromSelection",
            "Bin From Selection",
            &["File", "New"],
            Some("Shift+B"),
            r#"{"items":[id]?,"name":str?}"#,
            has_project_selection,
            bin_from_selection,
        ),
        spec(
            "file.newOfflineFile",
            "Offline File…",
            &["File", "New"],
            None,
            r#"{"name":str?,"fileName":str?,"tapeName":str?,"video":bool=true,"audio":bool=true,"width":u32?,"height":u32?,"fps":f64?,"sampleRate":u32=48000,"channels":u32=2,"timecode":"HH:MM:SS:FF"?,"seconds":f64=10,"description":str?}"#,
            always,
            offline_file,
        ),
        spec("file.closeProject", "Close Project", &["File"], Some("Cmd+Shift+W"), r#"{"force":bool?}"#, always, close_project),
        spec("file.saveAll", "Save All", &["File"], None, "{}", always, |s, _| s.execute("file.save", json!({}))),
        // ---------------- Edit ----------------
        spec(
            "edit.pasteAttributes",
            "Paste Attributes…",
            &["Edit"],
            Some("Cmd+Alt+V"),
            r#"{"clips":[id]?,"motion":bool=true,"opacity":bool=true,"timeRemapping":bool=true,"volume":bool=true,"channelVolume":bool=true,"panner":bool=true,"effects":bool|[effectId]=true,"scaleTimes":bool=true}"#,
            can_paste_attributes,
            paste_attributes,
        ),
        spec(
            "edit.removeAttributes",
            "Remove Attributes…",
            &["Edit"],
            None,
            r#"{"clips":[id]?,"motion":bool=true,"opacity":bool=true,"timeRemapping":bool=true,"volume":bool=true,"channelVolume":bool=true,"panner":bool=true,"effects":bool|[effectId]=true}"#,
            has_selection,
            remove_attributes,
        ),
        spec("edit.selectAllMatching", "Select All Matching", &["Edit"], None, r#"{"clips":[id]?}"#, has_selection, select_all_matching),
        spec("edit.selectLabelGroup", "Select Label Group", &["Edit", "Label"], None, "{}", can_label, select_label_group),
        label_cmd!("edit.label.violet", "Violet", Label::Violet),
        label_cmd!("edit.label.iris", "Iris", Label::Iris),
        label_cmd!("edit.label.caribbean", "Caribbean", Label::Caribbean),
        label_cmd!("edit.label.lavender", "Lavender", Label::Lavender),
        label_cmd!("edit.label.cerulean", "Cerulean", Label::Cerulean),
        label_cmd!("edit.label.forest", "Forest", Label::Forest),
        label_cmd!("edit.label.rose", "Rose", Label::Rose),
        label_cmd!("edit.label.mango", "Mango", Label::Mango),
        label_cmd!("edit.label.purple", "Purple", Label::Purple),
        label_cmd!("edit.label.blue", "Blue", Label::Blue),
        label_cmd!("edit.label.teal", "Teal", Label::Teal),
        label_cmd!("edit.label.magenta", "Magenta", Label::Magenta),
        label_cmd!("edit.label.tan", "Tan", Label::Tan),
        label_cmd!("edit.label.green", "Green", Label::Green),
        label_cmd!("edit.label.brown", "Brown", Label::Brown),
        label_cmd!("edit.label.yellow", "Yellow", Label::Yellow),
        spec("edit.removeUnused", "Remove Unused", &["Edit"], None, "{}", has_project_items, remove_unused),
        spec("edit.consolidateDuplicates", "Consolidate Duplicates", &["Edit"], None, "{}", has_project_items, consolidate_duplicates),
        // ---------------- Clip ----------------
        spec(
            "clip.makeSubclip",
            "Make Subclip…",
            &["Clip"],
            Some("Cmd+U"),
            r#"{"item":id?,"clip":id?,"name":str?,"start":ticks?,"end":ticks?,"startFrame":i64?,"endFrame":i64?,"restrictTrims":bool=true}"#,
            can_make_subclip,
            make_subclip,
        ),
        spec(
            "clip.editSubclip",
            "Edit Subclip…",
            &["Clip"],
            None,
            r#"{"item":id?,"start":ticks?,"end":ticks?,"startFrame":i64?,"endFrame":i64?,"restrictTrims":bool?,"convertToMaster":bool?}"#,
            has_subclip,
            edit_subclip,
        ),
        spec(
            "clip.audioChannels",
            "Audio Channels…",
            &["Clip", "Modify"],
            Some("Shift+G"),
            r#"{"items":[id]?,"format":"mono|stereo|5.1|adaptive","clips":[[channel]]?,"channels":[channel]?}"#,
            has_audio_target,
            audio_channels,
        ),
        spec(
            "clip.modifyTimecode",
            "Timecode…",
            &["Clip", "Modify"],
            None,
            r#"{"item":id?,"timecode":"HH:MM:SS:FF"?,"frame":i64?,"tapeName":str?,"reset":bool?}"#,
            has_media_selection,
            modify_timecode,
        ),
        spec(
            "clip.frameHoldOptions",
            "Frame Hold Options…",
            &["Clip", "Video Options"],
            None,
            r#"{"clips":[id]?,"enabled":bool=true,"holdOn":"sourceTimecode|sequenceTime|in|out|playhead","time":ticks?,"timecode":str?,"holdFilters":bool=false}"#,
            has_selection,
            frame_hold_options,
        ),
        spec(
            "clip.insertFrameHoldSegment",
            "Insert Frame Hold Segment",
            &["Clip", "Video Options"],
            None,
            r#"{"clip":id?,"time":ticks?,"seconds":f64=2}"#,
            has_seq,
            insert_frame_hold_segment,
        ),
        spec(
            "clip.fieldOptions",
            "Field Options…",
            &["Clip", "Video Options"],
            None,
            r#"{"clips":[id]?,"reverseFieldDominance":bool=false,"processing":"none|alwaysDeinterlace|flickerRemoval"}"#,
            has_selection,
            field_options,
        ),
        spec(
            "clip.timeInterpolation.frameSampling",
            "Frame Sampling",
            &["Clip", "Video Options", "Time Interpolation"],
            None,
            r#"{"clips":[id]?}"#,
            has_selection,
            |s, p| set_interpolation(s, p, TimeInterpolation::FrameSampling),
        ),
        spec(
            "clip.timeInterpolation.frameBlending",
            "Frame Blending",
            &["Clip", "Video Options", "Time Interpolation"],
            None,
            r#"{"clips":[id]?}"#,
            has_selection,
            |s, p| set_interpolation(s, p, TimeInterpolation::FrameBlending),
        ),
        spec(
            "clip.timeInterpolation.opticalFlow",
            "Optical Flow",
            &["Clip", "Video Options", "Time Interpolation"],
            None,
            r#"{"clips":[id]?}"#,
            has_selection,
            |s, p| set_interpolation(s, p, TimeInterpolation::OpticalFlow),
        ),
        spec(
            "clip.setTimeInterpolation",
            "Set Time Interpolation",
            &[],
            None,
            r#"{"clips":[id]?,"mode":"frameSampling|frameBlending|opticalFlow"}"#,
            has_selection,
            |s, p| {
                let m = str_p(p, "mode").and_then(TimeInterpolation::from_name).ok_or_else(|| bad("clip.setTimeInterpolation", "need `mode`"))?;
                set_interpolation(s, p, m)
            },
        ),
        spec("clip.fitToFrame", "Fit to frame", &["Clip", "Video Options"], None, r#"{"clips":[id]?}"#, has_selection, |s, p| fit_fill(s, p, false)),
        spec("clip.fillFrame", "Fill frame", &["Clip", "Video Options"], None, r#"{"clips":[id]?}"#, has_selection, |s, p| fit_fill(s, p, true)),
        spec("clip.breakoutToMono", "Breakout to Mono", &["Clip", "Audio Options"], None, r#"{"items":[id]?}"#, has_audio_items, breakout_to_mono),
        spec("clip.extractAudio", "Extract Audio", &["Clip", "Audio Options"], None, r#"{"items":[id]?,"dir":str?}"#, has_extractable, extract_audio),
        spec("clip.replaceFromSource", "From Source Monitor", &["Clip", "Replace With Clip"], None, r#"{"clips":[id]?}"#, can_replace_from_source, |s, p| {
            replace_with(s, p, Replace::Source)
        }),
        spec(
            "clip.replaceFromSourceMatchFrame",
            "From Source Monitor, Match Frame",
            &["Clip", "Replace With Clip"],
            None,
            r#"{"clips":[id]?}"#,
            can_replace_from_source,
            |s, p| replace_with(s, p, Replace::MatchFrame),
        ),
        spec(
            "clip.replaceFromBin",
            "From Bin",
            &["Clip", "Replace With Clip"],
            None,
            r#"{"clips":[id]?,"item":id?,"keepSourceIn":bool=false}"#,
            can_replace_from_bin,
            |s, p| replace_with(s, p, Replace::Bin),
        ),
    ]
}

/// Where a run of commands goes in the menus (the registry order is the menu order).
enum At {
    After(&'static str),
    Before(&'static str),
}

const LABEL_IDS: [&str; 16] = [
    "edit.label.violet",
    "edit.label.iris",
    "edit.label.caribbean",
    "edit.label.lavender",
    "edit.label.cerulean",
    "edit.label.forest",
    "edit.label.rose",
    "edit.label.mango",
    "edit.label.purple",
    "edit.label.blue",
    "edit.label.teal",
    "edit.label.magenta",
    "edit.label.tan",
    "edit.label.green",
    "edit.label.brown",
    "edit.label.yellow",
];

/// Add [`commands`] to the registry and arrange the File / Edit / Clip menus in Premiere's order
/// (moving a few existing commands into their submenus' order too).
pub(crate) fn apply_layout(v: &mut Vec<CommandSpec>) {
    let mut pool = commands();
    let mut edit_tail: Vec<&'static str> = vec!["edit.selectLabelGroup"];
    edit_tail.extend(LABEL_IDS);
    edit_tail.extend(["edit.removeUnused", "edit.consolidateDuplicates"]);
    let layout: Vec<(At, Vec<&'static str>)> = vec![
        (At::After("file.newSequence"), vec!["file.newSequenceFromClip"]),
        (At::After("file.newBin"), vec!["file.newBinFromSelection", "file.newOfflineFile"]),
        (At::Before("file.save"), vec!["file.closeProject"]),
        (At::After("file.saveCopy"), vec!["file.saveAll"]),
        (At::After("edit.pasteInsert"), vec!["edit.pasteAttributes", "edit.removeAttributes"]),
        (At::After("edit.selectAll"), vec!["edit.selectAllMatching"]),
        (At::After("edit.duplicate"), edit_tail),
        (
            At::After("clip.rename"),
            vec![
                "clip.makeSubclip",
                "clip.editSubclip",
                "clip.audioChannels",
                "clip.interpretFootage",
                "clip.modifyTimecode",
                "clip.frameHoldOptions",
                "clip.frameHold",
                "clip.insertFrameHoldSegment",
                "clip.fieldOptions",
                "clip.timeInterpolation.frameSampling",
                "clip.timeInterpolation.frameBlending",
                "clip.timeInterpolation.opticalFlow",
                "clip.scaleToFrameSize",
                "clip.fitToFrame",
                "clip.fillFrame",
                "clip.audioGain",
                "clip.breakoutToMono",
                "clip.extractAudio",
            ],
        ),
        (At::After("source.overwrite"), vec!["clip.replaceFromSource", "clip.replaceFromSourceMatchFrame", "clip.replaceFromBin"]),
    ];
    for (at, ids) in layout {
        let mut run = Vec::new();
        for id in ids {
            if let Some(i) = v.iter().position(|c| c.id == id) {
                run.push(v.remove(i));
            } else if let Some(i) = pool.iter().position(|c| c.id == id) {
                run.push(pool.remove(i));
            }
        }
        let idx = match at {
            At::After(a) => v.iter().position(|c| c.id == a).map(|i| i + 1),
            At::Before(b) => v.iter().position(|c| c.id == b),
        }
        .unwrap_or(v.len());
        v.splice(idx..idx, run);
    }
    v.extend(pool);
}

// ---------------------------------------------------------------------------------------------
// enablement
// ---------------------------------------------------------------------------------------------

fn can_label(s: &Session) -> std::result::Result<(), String> {
    if s.state.selection.is_empty() && s.state.project_selection.is_empty() { Err("select clips or project items".into()) } else { Ok(()) }
}
fn has_project_items(s: &Session) -> std::result::Result<(), String> {
    if s.project.items.values().any(|i| !matches!(i.kind, ItemKind::Graphic { .. })) { Ok(()) } else { Err("the project is empty".into()) }
}
fn is_clip_item(p: &Project, id: ItemId) -> bool {
    p.item(id).is_some_and(|i| matches!(i.kind, ItemKind::Media(_) | ItemKind::Subclip { .. } | ItemKind::Sequence(_) | ItemKind::AdjustmentLayer { .. }))
}
fn has_clip_items(s: &Session) -> std::result::Result<(), String> {
    if s.state.project_selection.iter().any(|i| is_clip_item(&s.project, *i)) { Ok(()) } else { Err("select a clip in the Project panel".into()) }
}
fn can_paste_attributes(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    if s.state.clipboard.is_empty() { Err("copy a clip first".into()) } else { Ok(()) }
}
fn subclip_source(s: &Session) -> Option<ItemId> {
    s.state
        .source_item
        .filter(|i| media_root(&s.project, *i).is_some())
        .or_else(|| s.state.project_selection.iter().copied().find(|i| media_root(&s.project, *i).is_some()))
}
fn can_make_subclip(s: &Session) -> std::result::Result<(), String> {
    if subclip_source(s).is_some() || !s.state.selection.is_empty() { Ok(()) } else { Err("open a clip in the Source monitor or select one".into()) }
}
fn has_subclip(s: &Session) -> std::result::Result<(), String> {
    if s.state.project_selection.iter().any(|i| s.project.item(*i).is_some_and(|it| matches!(it.kind, ItemKind::Subclip { .. }))) {
        Ok(())
    } else {
        Err("select a subclip in the Project panel".into())
    }
}
fn has_media_selection(s: &Session) -> std::result::Result<(), String> {
    if s.state.project_selection.iter().any(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some()) {
        Ok(())
    } else {
        Err("select a clip in the Project panel".into())
    }
}
fn audio_items(s: &Session) -> Vec<ItemId> {
    s.state.project_selection.iter().copied().filter(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some_and(|m| m.info.has_audio())).collect()
}
fn has_audio_items(s: &Session) -> std::result::Result<(), String> {
    if audio_items(s).is_empty() { Err("select a clip with audio in the Project panel".into()) } else { Ok(()) }
}
fn selected_audio_clips(s: &Session) -> Vec<ClipId> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    s.state.selection.iter().copied().filter(|c| q.audio_tracks.iter().any(|t| t.item(*c).is_some())).collect()
}
fn has_audio_target(s: &Session) -> std::result::Result<(), String> {
    if audio_items(s).is_empty() && selected_audio_clips(s).is_empty() { Err("select a clip with audio".into()) } else { Ok(()) }
}
fn extract_items(s: &Session) -> Vec<ItemId> {
    let mut out: Vec<ItemId> = s.state.project_selection.clone();
    if out.is_empty()
        && let Some(q) = s.active_sequence()
    {
        for c in &s.state.selection {
            if let Some((_, i)) = q.find_item(*c)
                && !out.contains(&i.item)
            {
                out.push(i.item);
            }
        }
    }
    out.retain(|i| media_root(&s.project, *i).is_some_and(|(_, m, _)| m.info.has_audio()));
    out
}
fn has_extractable(s: &Session) -> std::result::Result<(), String> {
    if extract_items(s).is_empty() { Err("select a clip with audio".into()) } else { Ok(()) }
}
fn can_replace_from_source(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    s.state.source_item.map(|_| ()).ok_or_else(|| "open a clip in the Source monitor".into())
}
fn can_replace_from_bin(s: &Session) -> std::result::Result<(), String> {
    has_selection(s)?;
    if s.state.project_selection.iter().any(|i| is_clip_item(&s.project, *i)) { Ok(()) } else { Err("select a clip in the Project panel".into()) }
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

/// The media item behind `id` (subclips resolve to their parent) and the subclip range.
fn media_root(p: &Project, id: ItemId) -> Option<(ItemId, &MediaClip, Option<TimeRange>)> {
    p.resolve_media(id)
}

/// What the Source monitor shows for an item: the media-time span of its time ruler, its frame
/// rate, In / Out marks and markers.
///
/// A subclip that restricts trims shows only its range (the playhead can't leave it); one that
/// doesn't shows its parent's whole media with the subclip's range as In / Out. Either way a
/// subclip shows the markers of its parent media that fall inside its range (inherited: they
/// belong to the master clip, so adding one there shows on every subclip that covers it).
#[derive(Clone, Debug, PartialEq)]
pub struct SourceView {
    pub item: ItemId,
    /// The media item whose frames are shown (the parent of a subclip).
    pub media: ItemId,
    pub start: Tick,
    pub end: Tick,
    pub rate: FrameRate,
    pub mark_in: Option<Tick>,
    pub mark_out: Option<Tick>,
    pub markers: Vec<filmcraft_project::Marker>,
    /// For a subclip: its range and whether trims are restricted to it.
    pub subclip: Option<(TimeRange, bool)>,
}

impl SourceView {
    /// The marked Source span. Unset marks mean the beginning/end of the source; Out is inclusive.
    pub fn selected_range(&self) -> TimeRange {
        if self.start.0 < 0 || self.end <= self.start {
            return TimeRange::new(self.start, Tick::ZERO);
        }
        let start = self.mark_in.unwrap_or(self.start).clamp(self.start, self.end);
        let end = self.mark_out.map(|o| Tick(o.0.saturating_add(self.rate.frame_duration().0))).unwrap_or(self.end).clamp(start, self.end);
        TimeRange::from_bounds(start, end)
    }

    pub fn to_json(&self, playhead: Tick) -> Value {
        json!({
            "item": self.item.0, "media": self.media.0, "start": self.start.0, "end": self.end.0, "fps": self.rate.as_f64(),
            "playhead": playhead.0, "markIn": self.mark_in.map(|t| t.0), "markOut": self.mark_out.map(|t| t.0),
            "markers": self.markers.iter().map(|m| json!({"id": m.id.0, "start": m.start.0, "name": m.name, "duration": m.duration.0})).collect::<Vec<_>>(),
            "subclip": self.subclip.map(|(r, restrict)| json!({"start": r.start.0, "end": r.end().0, "restrictTrims": restrict})),
        })
    }
}

/// The Source monitor view of `item` (see [`SourceView`]).
pub fn source_view(s: &Session, item: ItemId) -> Option<SourceView> {
    let pi = s.project.item(item)?;
    let still = |m: &MediaClip| matches!(m.info.kind, filmcraft_media::MediaKind::Still) || m.info.duration.0 <= 0;
    Some(match &pi.kind {
        ItemKind::Media(m) => {
            let rate = m.frame_rate();
            let end = if still(m) { s.prefs.timeline.still_duration(rate) } else { m.duration() };
            SourceView { item, media: item, start: Tick::ZERO, end, rate, mark_in: m.mark_in, mark_out: m.mark_out, markers: m.markers.clone(), subclip: None }
        }
        ItemKind::Sequence(q) => SourceView {
            item,
            media: item,
            start: Tick::ZERO,
            end: q.duration(),
            rate: q.settings.frame_rate,
            mark_in: q.mark_in,
            mark_out: q.mark_out,
            markers: q.markers.clone(),
            subclip: None,
        },
        ItemKind::Subclip { range, restrict_trims, .. } => {
            let (root, m, _) = media_root(&s.project, item)?;
            let rate = m.frame_rate();
            let fd = rate.frame_duration();
            let markers = m.markers.iter().filter(|k| k.start >= range.start && k.start < range.end()).cloned().collect();
            let full = if still(m) { range.end() } else { m.duration().max(range.end()) };
            let (start, end, mi, mo) =
                if *restrict_trims { (range.start, range.end(), None, None) } else { (Tick::ZERO, full, Some(range.start), Some(range.end() - fd)) };
            SourceView { item, media: root, start, end, rate, mark_in: mi, mark_out: mo, markers, subclip: Some((*range, *restrict_trims)) }
        }
        _ => SourceView {
            item,
            media: item,
            start: Tick::ZERO,
            end: pi.duration(),
            rate: pi.frame_rate(),
            mark_in: None,
            mark_out: None,
            markers: Vec::new(),
            subclip: None,
        },
    })
}

/// Frame rate of an item, resolving subclips to their media.
fn item_rate(p: &Project, id: ItemId) -> FrameRate {
    match media_root(p, id) {
        Some((_, m, _)) => m.frame_rate(),
        None => p.item(id).map(|i| i.frame_rate()).unwrap_or_default(),
    }
}

fn items_p(s: &Session, p: &Value) -> Vec<ItemId> {
    match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
        None => item_p(p, "item").map(|i| vec![i]).unwrap_or_else(|| s.state.project_selection.clone()),
    }
}

/// Fold the undo steps pushed since the history had `n0` entries into one step named `label`.
pub(crate) fn collapse_history(s: &mut Session, n0: usize, label: &str) {
    if s.history.undo.len() > n0 {
        let first = s.history.undo[n0].1.clone();
        s.history.undo.truncate(n0);
        s.history.undo.push((label.to_string(), first));
    }
}

/// Every item referenced by a clip in any sequence (subclips pull in their parents, nested
/// sequences count as used).
fn used_items(p: &Project) -> BTreeSet<ItemId> {
    let mut used = BTreeSet::new();
    for it in p.items.values() {
        if let ItemKind::Sequence(q) = &it.kind {
            for t in q.all_tracks() {
                for i in &t.items {
                    used.insert(i.item);
                }
            }
            if let Some(m) = &q.merged {
                let v = serde_json::to_value(m).unwrap_or_default();
                collect_ids(&v, &mut used, p);
            }
        }
    }
    // subclip parents of anything kept
    let mut changed = true;
    while changed {
        changed = false;
        for it in p.items.values() {
            if let ItemKind::Subclip { parent, .. } = it.kind
                && (used.contains(&it.id) || !is_removable(p, it.id))
                && used.insert(parent)
            {
                changed = true;
            }
        }
    }
    used
}

/// Item ids mentioned in a JSON value (keys named `item`/`items`/`video`/`audio` holding ids).
pub(crate) fn collect_ids(v: &Value, out: &mut BTreeSet<ItemId>, p: &Project) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                if matches!(k.as_str(), "item" | "items" | "video" | "audio") {
                    for n in x.as_array().cloned().unwrap_or_else(|| vec![x.clone()]) {
                        if let Some(id) = n.as_u64().map(ItemId).filter(|i| p.items.contains_key(i)) {
                            out.insert(id);
                        }
                    }
                }
                collect_ids(x, out, p);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| collect_ids(x, out, p)),
        _ => {}
    }
}

/// Remove Unused removes clips only: never sequences or graphic sources.
fn is_removable(p: &Project, id: ItemId) -> bool {
    p.item(id).is_some_and(|i| matches!(i.kind, ItemKind::Media(_) | ItemKind::Subclip { .. } | ItemKind::AdjustmentLayer { .. }))
}

fn remove_items(p: &mut Project, ids: &[ItemId]) {
    for i in ids {
        p.items.remove(i);
        p.root.remove_item(*i);
    }
}

/// Keyframe times of an effect instance shifted / scaled from clip `from` to clip `to` (media time).
fn retime_effect(e: &mut filmcraft_project::EffectInstance, from: &TrackItem, to: &TrackItem, scale: bool) {
    let (f0, f1) = (from.source_in, from.source_out());
    let (t0, t1) = (to.source_in, to.source_out());
    let map = |k: Tick| -> Tick {
        if scale && f1 > f0 {
            let r = (k - f0).0 as f64 / (f1 - f0).0 as f64;
            t0 + Tick((r * (t1 - t0).0 as f64).round() as i64)
        } else {
            t0 + (k - f0)
        }
    };
    for p in e.params.values_mut() {
        for k in &mut p.keyframes {
            k.time = map(k.time);
        }
    }
    for m in &mut e.masks {
        for p in [&mut m.path, &mut m.feather, &mut m.opacity, &mut m.expansion] {
            for k in &mut p.keyframes {
                k.time = map(k.time);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// File
// ---------------------------------------------------------------------------------------------

fn sequence_from_clip(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = items_p(s, p).into_iter().filter(|i| is_clip_item(&s.project, *i)).collect();
    let first = *items.first().ok_or_else(|| bad("file.newSequenceFromClip", "select a clip in the Project panel"))?;
    let pi = s.project.item(first).ok_or_else(|| bad("file.newSequenceFromClip", "no such item"))?.clone();
    let mut settings = match &pi.kind {
        ItemKind::Sequence(q) => q.settings.clone(),
        _ => media_root(&s.project, first).map(|(_, m, _)| crate::commands::default_seq_settings_for(&m.info)).unwrap_or_default(),
    };
    if let ItemKind::AdjustmentLayer { width, height, rate, .. } = &pi.kind {
        settings.width = *width;
        settings.height = *height;
        settings.frame_rate = *rate;
    }
    let channels = media_root(&s.project, first).and_then(|(_, m, _)| m.info.audio().map(|a| a.channels)).unwrap_or(2);
    let n_audio = media_root(&s.project, first)
        .map(|(_, m, _)| {
            crate::commands::audio_placement_specs(m.interpret.audio_channels.as_ref().map_or(&[], |a| a.clips.as_slice()), m.info.audio_streams.len()).len()
        })
        .unwrap_or(1);
    let n0 = s.history.undo.len();
    let name = pi.name.clone();
    let bin = s.project.root.parent_of(first).filter(|b| *b != s.project.root.id);
    let seq = s.edit("New Sequence From Clip", |pr, st| {
        let id = pr.new_sequence(&name, settings, 1, n_audio.max(1), bin);
        if channels == 1
            && let Some(q) = pr.sequence_mut(id)
        {
            q.settings.audio_master = AudioChannels::Stereo;
        }
        st.active_sequence = Some(id);
        if !st.open_sequences.contains(&id) {
            st.open_sequences.push(id);
        }
        Ok(id)
    })?;
    let mut at = Tick::ZERO;
    let mut placed = Vec::new();
    for item in items {
        let still = s.prefs.timeline.still_duration(item_rate(&s.project, item));
        let Some(range) = item_range(&s.project, item, still) else { continue };
        let tg = s.targeting();
        let ids = place_item(s, item, range, at, tg.video_dest, tg.audio_dest, false, "New Sequence From Clip", None)?;
        at = s.active_sequence().map(|q| q.duration()).unwrap_or(at);
        placed.extend(ids.iter().map(|c| c.0));
    }
    collapse_history(s, n0, "New Sequence From Clip");
    s.state.selection.clear();
    s.events.push(crate::Event::OpenSequence(seq));
    Ok(json!({"sequence": seq.0, "clips": placed}))
}

/// The media range an item edits in with: its In/Out marks, a subclip's range, or the whole item.
pub(crate) fn item_range(p: &Project, id: ItemId, still: Tick) -> Option<TimeRange> {
    let it = p.item(id)?;
    let rate = item_rate(p, id);
    Some(match &it.kind {
        ItemKind::Media(m) => {
            let is_still = matches!(m.info.kind, filmcraft_media::MediaKind::Still);
            let full = if m.info.duration.0 > 0 && !is_still { m.info.duration } else { still };
            let a = m.mark_in.unwrap_or(Tick::ZERO);
            let b = m.mark_out.map(|o| o + rate.frame_duration()).unwrap_or(full);
            TimeRange::from_bounds(a, b.max(a + rate.frame_duration()))
        }
        ItemKind::Subclip { range, .. } => *range,
        ItemKind::Sequence(q) => {
            let a = q.mark_in.unwrap_or(Tick::ZERO);
            let b = q.mark_out.map(|o| o + q.settings.frame_rate.frame_duration()).unwrap_or(q.duration());
            TimeRange::from_bounds(a, b.max(a + q.settings.frame_rate.frame_duration()))
        }
        _ => TimeRange::new(Tick::ZERO, it.duration()),
    })
}

fn bin_from_selection(s: &mut Session, p: &Value) -> Result<Value> {
    let items = items_p(s, p);
    if items.is_empty() {
        return Err(bad("file.newBinFromSelection", "select items in the Project panel"));
    }
    let parent = s.project.root.parent_of(items[0]).filter(|b| *b != s.project.root.id);
    let name = match str_p(p, "name") {
        Some(n) => n.to_string(),
        None => {
            let mut n = 1;
            let mut names = Vec::new();
            bin_names(&s.project.root, &mut names);
            while names.contains(&format!("Bin {n:02}")) {
                n += 1;
            }
            format!("Bin {n:02}")
        }
    };
    let bin = s.edit("New Bin From Selection", |pr, _| {
        let b = pr.add_bin(&name, parent);
        pr.move_to_bin(&items, Some(b));
        Ok(b)
    })?;
    Ok(json!({"bin": bin.0, "moved": items.len()}))
}

fn bin_names(b: &filmcraft_project::Bin, out: &mut Vec<String>) {
    for c in &b.children {
        if let BinEntry::Bin(x) = c {
            out.push(x.name.clone());
            bin_names(x, out);
        }
    }
}

fn offline_file(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.active_sequence().map(|q| q.settings.clone()).unwrap_or_default();
    let has_v = bool_p(p, "video").unwrap_or(true);
    let has_a = bool_p(p, "audio").unwrap_or(true);
    if !has_v && !has_a {
        return Err(bad("file.newOfflineFile", "an offline file needs video or audio"));
    }
    let rate = f64_p(p, "fps").map(FrameRate::from_f64).unwrap_or(st.frame_rate);
    let width = u64_p(p, "width").map(|v| v as u32).unwrap_or(st.width);
    let height = u64_p(p, "height").map(|v| v as u32).unwrap_or(st.height);
    let secs = f64_p(p, "seconds").unwrap_or(10.0).max(rate.frame_duration().seconds());
    let duration = rate.snap_nearest(Tick::from_seconds_f64(secs)).max(rate.frame_duration());
    let start_tc = match str_p(p, "timecode") {
        Some(tc) => Some(parse_timecode(tc, rate, false, 0).map_err(|e| bad("file.newOfflineFile", format!("timecode: {e:?}")))?),
        None => None,
    };
    let file_name = str_p(p, "fileName").map(str::to_string).filter(|n| !n.is_empty()).unwrap_or_else(|| "Offline.mov".into());
    let name = str_p(p, "name").map(str::to_string).filter(|n| !n.is_empty()).unwrap_or_else(|| file_name.clone());
    let info = filmcraft_media::MediaInfo {
        name: file_name.clone(),
        kind: if has_v { filmcraft_media::MediaKind::Movie } else { filmcraft_media::MediaKind::AudioOnly },
        duration,
        video: has_v.then(|| filmcraft_media::VideoStreamInfo {
            width,
            height,
            frame_rate: rate,
            par: (1, 1),
            codec: String::new(),
            pixel_format: String::new(),
            color: Default::default(),
            has_alpha: false,
            bitrate: None,
            hdr: None,
        }),
        audio_streams: has_a
            .then(|| filmcraft_media::AudioStreamInfo {
                sample_rate: u64_p(p, "sampleRate").unwrap_or(48_000) as u32,
                channels: u64_p(p, "channels").unwrap_or(2).max(1) as u32,
                codec: String::new(),
                bits_per_sample: None,
            })
            .into_iter()
            .collect(),
        container: String::new(),
        start_timecode: start_tc,
        file_size: None,
    };
    let mut meta = BTreeMap::new();
    if let Some(t) = str_p(p, "tapeName").filter(|t| !t.is_empty()) {
        meta.insert("Tape Name".to_string(), t.to_string());
    }
    if let Some(d) = str_p(p, "description").filter(|t| !t.is_empty()) {
        meta.insert("Description".to_string(), d.to_string());
    }
    let id = s.edit("New Offline File", |pr, stt| {
        let id = pr.add_item(
            &name,
            if has_v { Label::Iris } else { Label::Caribbean },
            ItemKind::Media(MediaClip {
                media: MediaRef::File { path: file_name },
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: true,
                proxy: None,
                identity: None,
            }),
            None,
        );
        if let Some(it) = pr.item_mut(id) {
            it.metadata = meta;
        }
        stt.project_selection = vec![id];
        Ok(id)
    })?;
    Ok(json!({"item": id.0}))
}

fn close_project(s: &mut Session, p: &Value) -> Result<Value> {
    if s.is_dirty() && !bool_p(p, "force").unwrap_or(false) {
        return Err(EngineError::Other("the project has unsaved changes: save it first, or pass {\"force\": true} to discard them".into()));
    }
    s.project = std::sync::Arc::new(Project::new("Untitled"));
    s.history = Default::default();
    s.history.limit = 200;
    s.state = Session::default().state;
    if s.path.take().is_some() {
        s.previews.reset_temp();
    }
    s.revision += 1;
    s.saved_revision = s.revision;
    s.events.push(crate::Event::ProjectChanged { revision: s.revision });
    Ok(json!({"closed": true}))
}

// ---------------------------------------------------------------------------------------------
// Edit
// ---------------------------------------------------------------------------------------------

fn apply_label(s: &mut Session, p: &Value, l: Label) -> Result<Value> {
    let items = match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
        None if p.get("clips").is_some() => Vec::new(),
        None => s.state.project_selection.clone(),
    };
    let clips = match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None if p.get("items").is_some() => Vec::new(),
        None => s.state.selection.clone(),
    };
    if items.is_empty() && clips.is_empty() {
        return Err(EngineError::Other("select clips or project items".into()));
    }
    let seq = s.state.active_sequence;
    let n = s.edit(&format!("Label {}", l.name()), |pr, _| {
        let mut n = 0;
        for i in &items {
            if let Some(it) = pr.item_mut(*i) {
                it.label = l;
                n += 1;
            }
        }
        if let Some(q) = seq.and_then(|q| pr.sequence_mut(q)) {
            for c in &clips {
                if let Some((_, it)) = q.find_item_mut(*c) {
                    it.label = l;
                    n += 1;
                }
            }
        }
        Ok(n)
    })?;
    Ok(json!({"label": l.name(), "labelled": n}))
}

fn select_label_group(s: &mut Session, _: &Value) -> Result<Value> {
    if !s.state.selection.is_empty() {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let labels: BTreeSet<&str> = s.state.selection.iter().filter_map(|c| q.find_item(*c).map(|(_, i)| i.label.name())).collect();
        let sel: Vec<ClipId> = q.all_tracks().flat_map(|t| t.items.iter()).filter(|i| labels.contains(i.label.name())).map(|i| i.id).collect();
        s.state.selection = sel;
        return Ok(json!({"clips": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>()}));
    }
    let labels: BTreeSet<&str> = s.state.project_selection.iter().filter_map(|i| s.project.item(*i).map(|it| it.label.name())).collect();
    let mut all = Vec::new();
    s.project.root.all_items(&mut all);
    let sel: Vec<ItemId> = all.into_iter().filter(|i| s.project.item(*i).is_some_and(|it| labels.contains(it.label.name()))).collect();
    s.state.project_selection = sel;
    Ok(json!({"items": s.state.project_selection.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

fn select_all_matching(s: &mut Session, p: &Value) -> Result<Value> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let sources: BTreeSet<ItemId> = clips_p(s, p).iter().filter_map(|c| q.find_item(*c).map(|(_, i)| i.item)).collect();
    let sel: Vec<ClipId> = q.all_tracks().flat_map(|t| t.items.iter()).filter(|i| sources.contains(&i.item)).map(|i| i.id).collect();
    s.state.selection = sel;
    Ok(json!({"clips": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

fn remove_unused(s: &mut Session, _: &Value) -> Result<Value> {
    let used = used_items(&s.project);
    let unused: Vec<ItemId> = s.project.items.keys().copied().filter(|i| is_removable(&s.project, *i) && !used.contains(i)).collect();
    if unused.is_empty() {
        return Ok(json!({"removed": []}));
    }
    let gone = unused.clone();
    s.edit("Remove Unused", |pr, st| {
        remove_items(pr, &gone);
        st.project_selection.retain(|i| !gone.contains(i));
        if st.source_item.is_some_and(|i| gone.contains(&i)) {
            st.source_item = None;
        }
        Ok(())
    })?;
    for i in &unused {
        s.media.remove(*i);
    }
    Ok(json!({"removed": unused.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

/// Media items that refer to the same file (or generator) with the same interpretation are
/// duplicates; the oldest one is kept and every reference to the others moves to it.
fn consolidate_duplicates(s: &mut Session, _: &Value) -> Result<Value> {
    let mut groups: BTreeMap<String, Vec<ItemId>> = BTreeMap::new();
    for it in s.project.items.values() {
        if let ItemKind::Media(m) = &it.kind {
            // same file (or generator), interpretation and streams
            let shape = (m.info.video.is_some(), m.info.has_audio(), m.info.duration);
            let key = serde_json::to_string(&(&m.media, &m.interpret, m.offline, shape)).unwrap_or_default();
            groups.entry(key).or_default().push(it.id);
        }
    }
    let mut remap: BTreeMap<ItemId, ItemId> = BTreeMap::new();
    for ids in groups.values().filter(|g| g.len() > 1) {
        for d in &ids[1..] {
            remap.insert(*d, ids[0]);
        }
    }
    if remap.is_empty() {
        return Ok(json!({"removed": []}));
    }
    let map = remap.clone();
    s.edit("Consolidate Duplicates", |pr, st| {
        for it in pr.items.values_mut() {
            match &mut it.kind {
                ItemKind::Sequence(q) => {
                    for t in q.all_tracks_mut() {
                        for i in &mut t.items {
                            if let Some(k) = map.get(&i.item) {
                                i.item = *k;
                            }
                        }
                    }
                }
                ItemKind::Subclip { parent, .. } => {
                    if let Some(k) = map.get(parent) {
                        *parent = *k;
                    }
                }
                _ => {}
            }
        }
        // keep the marks / markers of the kept item; carry markers of duplicates over
        let dups: Vec<ItemId> = map.keys().copied().collect();
        for d in &dups {
            let markers = pr.item(*d).and_then(|i| i.as_media()).map(|m| m.markers.clone()).unwrap_or_default();
            if let Some(m) = pr.item_mut(map[d]).and_then(|i| i.as_media_mut()) {
                for mk in markers {
                    if !m.markers.iter().any(|x| x.start == mk.start && x.name == mk.name) {
                        m.markers.push(mk);
                    }
                }
                m.markers.sort_by_key(|x| x.start);
            }
        }
        remove_items(pr, &dups);
        for i in st.project_selection.iter_mut() {
            if let Some(k) = map.get(i) {
                *i = *k;
            }
        }
        st.project_selection.dedup();
        if let Some(k) = st.source_item.and_then(|i| map.get(&i)) {
            st.source_item = Some(*k);
        }
        Ok(())
    })?;
    for d in remap.keys() {
        s.media.remove(*d);
    }
    Ok(json!({"removed": remap.keys().map(|i| i.0).collect::<Vec<_>>(), "kept": remap.values().map(|i| i.0).collect::<BTreeSet<_>>()}))
}

/// Which attributes a Paste / Remove Attributes call touches.
struct AttrSel {
    intrinsic: Vec<&'static str>,
    /// None = every standard effect; Some(ids) = those effects (Some(empty) = none).
    effects: Option<Vec<String>>,
}

fn attr_sel(p: &Value) -> AttrSel {
    let on = |k: &str| bool_p(p, k).unwrap_or(true);
    let mut intrinsic = Vec::new();
    for (k, id) in [
        ("motion", "motion"),
        ("opacity", "opacity"),
        ("timeRemapping", "time_remap"),
        ("volume", "volume"),
        ("channelVolume", "channel_volume"),
        ("panner", "panner"),
    ] {
        if on(k) {
            intrinsic.push(id);
        }
    }
    let effects = match p.get("effects") {
        Some(Value::Bool(false)) => Some(Vec::new()),
        Some(Value::Array(a)) => Some(a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()),
        _ => None,
    };
    AttrSel { intrinsic, effects }
}

fn is_standard(e: &filmcraft_project::EffectInstance) -> bool {
    e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e) && !e.essential
}

fn wants_effect(sel: &AttrSel, e: &filmcraft_project::EffectInstance) -> bool {
    sel.effects.as_ref().is_none_or(|ids| ids.contains(&e.effect))
}

fn paste_attributes(s: &mut Session, p: &Value) -> Result<Value> {
    let sel = attr_sel(p);
    let scale = bool_p(p, "scaleTimes").unwrap_or(true);
    let src_v = s.state.clipboard.iter().find(|c| c.0 == TrackKind::Video).map(|c| c.2.clone());
    let src_a = s.state.clipboard.iter().find(|c| c.0 == TrackKind::Audio).map(|c| c.2.clone());
    let targets = with_links(s, &clips_p(s, p));
    let n = s.edit_sequence("Paste Attributes", |q, _, _| {
        let mut n = 0;
        for c in &targets {
            let Some((tid, _)) = q.find_item(*c) else { continue };
            let kind = q.track(tid).map(|t| t.kind).unwrap_or_default();
            let Some(src) = (if kind == TrackKind::Video { src_v.as_ref() } else { src_a.as_ref() }) else { continue };
            let Some((_, dst)) = q.find_item_mut(*c) else { continue };
            let before = dst.clone();
            for id in &sel.intrinsic {
                let Some(e) = src.effect(id) else { continue };
                let mut e = e.clone();
                retime_effect(&mut e, src, &before, scale);
                match dst.effects.iter_mut().find(|x| x.effect == *id) {
                    Some(slot) => *slot = e,
                    None => dst.effects.push(e),
                }
            }
            for e in src.effects.iter().filter(|e| is_standard(e) && wants_effect(&sel, e)) {
                let mut e = e.clone();
                retime_effect(&mut e, src, &before, scale);
                dst.effects.push(e);
            }
            if *dst != before {
                n += 1;
            }
        }
        Ok(n)
    })?;
    Ok(json!({"clips": n}))
}

fn remove_attributes(s: &mut Session, p: &Value) -> Result<Value> {
    let sel = attr_sel(p);
    let targets = with_links(s, &clips_p(s, p));
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let frame = (seq.settings.width, seq.settings.height);
    let sizes: BTreeMap<ClipId, (u32, u32)> =
        targets.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| (*c, filmcraft_render::source_size(&s.project, i.item).unwrap_or(frame)))).collect();
    let n = s.edit_sequence("Remove Attributes", |q, _, _| {
        let mut n = 0;
        for c in &targets {
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            let before = it.clone();
            for id in &sel.intrinsic {
                if let Some(slot) = it.effects.iter_mut().find(|x| x.effect == *id)
                    && let Some(def) = slot.def()
                {
                    let mut fresh = def.instance();
                    filmcraft_project::resolve_auto_points(&mut fresh, frame, sizes.get(c).copied().unwrap_or(frame));
                    *slot = fresh;
                }
            }
            it.effects.retain(|e| !(is_standard(e) && wants_effect(&sel, e)));
            if *it != before {
                n += 1;
            }
        }
        Ok(n)
    })?;
    Ok(json!({"clips": n}))
}

// ---------------------------------------------------------------------------------------------
// Clip: subclips, Modify
// ---------------------------------------------------------------------------------------------

/// What Make Subclip would subclip: (parent media item, media range, name, label).
fn subclip_plan(s: &Session, p: &Value) -> Result<(ItemId, TimeRange, String, Label)> {
    Ok(if let Some(c) = clip_p_or_selection(s, p) {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let (_, it) = q.find_item(c).ok_or_else(|| bad("clip.makeSubclip", "no such clip"))?;
        let (root, _, _) = media_root(&s.project, it.item).ok_or_else(|| bad("clip.makeSubclip", "only media clips can be subclipped"))?;
        (root, TimeRange::from_bounds(it.source_in, it.source_out()), it.name.clone(), it.label)
    } else {
        let item = item_p(p, "item").or_else(|| subclip_source(s)).ok_or_else(|| bad("clip.makeSubclip", "open a clip in the Source monitor"))?;
        let (root, m, sub) = media_root(&s.project, item).ok_or_else(|| bad("clip.makeSubclip", "only media clips can be subclipped"))?;
        let pi = s.project.item(item).ok_or_else(|| EngineError::Other("no such item".into()))?;
        let rate = m.frame_rate();
        let still = s.prefs.timeline.still_duration(rate);
        let is_still = matches!(m.info.kind, filmcraft_media::MediaKind::Still);
        let full = sub.unwrap_or(TimeRange::new(Tick::ZERO, if m.info.duration.0 > 0 && !is_still { m.info.duration } else { still }));
        let range = if sub.is_some() {
            full
        } else {
            let a = m.mark_in.unwrap_or(full.start);
            let b = m.mark_out.map(|o| o + rate.frame_duration()).unwrap_or(full.end());
            TimeRange::from_bounds(a, b)
        };
        (root, range, pi.name.clone(), pi.label)
    })
}

/// The Make Subclip dialog's defaults: `{name, startFrame, endFrame, fps}` (media frames).
pub fn subclip_defaults(s: &Session, p: &Value) -> Option<Value> {
    let (parent, range, base, _) = subclip_plan(s, p).ok()?;
    let rate = item_rate(&s.project, parent);
    Some(json!({"name": format!("{base}.Subclip"), "startFrame": rate.frame_at(range.start), "endFrame": rate.frame_at(range.end()), "fps": rate.as_f64()}))
}

fn make_subclip(s: &mut Session, p: &Value) -> Result<Value> {
    let (parent, range, base, label) = subclip_plan(s, p)?;
    let rate = item_rate(&s.project, parent);
    let a = p
        .get("start")
        .and_then(Value::as_i64)
        .map(Tick)
        .or_else(|| p.get("startFrame").and_then(Value::as_i64).map(|f| rate.tick_of(f)))
        .unwrap_or(range.start);
    let b =
        p.get("end").and_then(Value::as_i64).map(Tick).or_else(|| p.get("endFrame").and_then(Value::as_i64).map(|f| rate.tick_of(f))).unwrap_or(range.end());
    if b <= a || a < Tick::ZERO {
        return Err(bad("clip.makeSubclip", "the subclip's end must be after its start"));
    }
    let restrict = bool_p(p, "restrictTrims").unwrap_or(true);
    let name = str_p(p, "name").map(str::to_string).filter(|n| !n.is_empty()).unwrap_or_else(|| format!("{base}.Subclip"));
    let bin = s.project.root.parent_of(parent).filter(|b| *b != s.project.root.id);
    let id = s.edit("Make Subclip", |pr, st| {
        let id = pr.add_item(&name, label, ItemKind::Subclip { parent, range: TimeRange::from_bounds(a, b), restrict_trims: restrict }, bin);
        st.project_selection = vec![id];
        Ok(id)
    })?;
    Ok(json!({"item": id.0}))
}

/// `clip` param, else the timeline selection when nothing is loaded in the Source monitor and no
/// `item` is given.
fn clip_p_or_selection(s: &Session, p: &Value) -> Option<ClipId> {
    if let Some(c) = crate::commands::clip_p(p, "clip") {
        return Some(c);
    }
    if item_p(p, "item").is_some() || subclip_source(s).is_some() {
        return None;
    }
    s.state.selection.first().copied()
}

fn edit_subclip(s: &mut Session, p: &Value) -> Result<Value> {
    let item = item_p(p, "item")
        .or_else(|| s.state.project_selection.iter().copied().find(|i| s.project.item(*i).is_some_and(|it| matches!(it.kind, ItemKind::Subclip { .. }))))
        .ok_or_else(|| bad("clip.editSubclip", "select a subclip"))?;
    let Some(ItemKind::Subclip { parent, range, restrict_trims }) = s.project.item(item).map(|i| i.kind.clone()) else {
        return Err(bad("clip.editSubclip", "not a subclip"));
    };
    let rate = item_rate(&s.project, parent);
    let a = p
        .get("start")
        .and_then(Value::as_i64)
        .map(Tick)
        .or_else(|| p.get("startFrame").and_then(Value::as_i64).map(|f| rate.tick_of(f)))
        .unwrap_or(range.start);
    let b =
        p.get("end").and_then(Value::as_i64).map(Tick).or_else(|| p.get("endFrame").and_then(Value::as_i64).map(|f| rate.tick_of(f))).unwrap_or(range.end());
    if b <= a || a < Tick::ZERO {
        return Err(bad("clip.editSubclip", "the subclip's end must be after its start"));
    }
    let restrict = bool_p(p, "restrictTrims").unwrap_or(restrict_trims);
    let convert = bool_p(p, "convertToMaster").unwrap_or(false);
    let parent_media = s.project.item(parent).and_then(|i| i.as_media()).cloned();
    s.edit("Edit Subclip", |pr, _| {
        let it = pr.item_mut(item).ok_or_else(|| bad("clip.editSubclip", "no such item"))?;
        if convert {
            // a master clip of the whole media, marked with the subclip's range; the parent's
            // markers inside that range come along (they were the subclip's inherited markers)
            let mut m = parent_media.ok_or_else(|| bad("clip.editSubclip", "the subclip's media is gone"))?;
            let fd = m.frame_rate().frame_duration();
            m.mark_in = Some(a);
            m.mark_out = Some((b - fd).max(a));
            m.markers.retain(|k| k.start >= a && k.start < b);
            it.kind = ItemKind::Media(m);
        } else {
            it.kind = ItemKind::Subclip { parent, range: TimeRange::from_bounds(a, b), restrict_trims: restrict };
        }
        Ok(())
    })?;
    if convert {
        s.media.remove(item);
    }
    Ok(json!({"item": item.0, "converted": convert}))
}

fn parse_format(f: &str) -> Option<AudioChannels> {
    match f.to_ascii_lowercase().as_str() {
        "mono" => Some(AudioChannels::Mono),
        "stereo" => Some(AudioChannels::Stereo),
        "5.1" | "surround51" | "surround" => Some(AudioChannels::Surround51),
        "adaptive" => Some(AudioChannels::Adaptive),
        _ => None,
    }
}

fn audio_channels(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> =
        items_p(s, p).into_iter().filter(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some_and(|m| m.info.has_audio())).collect();
    if items.is_empty() || p.get("channels").is_some() {
        // timeline audio clips: which source channels each plays
        let clips = match p.get("clips").and_then(Value::as_array) {
            Some(a) if a.iter().all(Value::is_u64) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
            _ => selected_audio_clips(s),
        };
        let chans: Vec<u16> =
            p.get("channels").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_u64().map(|c| c as u16)).collect()).unwrap_or_default();
        if clips.is_empty() {
            return Err(bad("clip.audioChannels", "select a clip with audio"));
        }
        s.edit_sequence("Modify Audio Channels", |q, _, _| {
            for c in &clips {
                if let Some((_, it)) = q.find_item_mut(*c) {
                    it.source_channels = chans.clone();
                }
            }
            Ok(())
        })?;
        return Ok(json!({"clips": clips.iter().map(|c| c.0).collect::<Vec<_>>(), "channels": chans}));
    }
    let format = match str_p(p, "format") {
        Some(f) => Some(parse_format(f).ok_or_else(|| bad("clip.audioChannels", "format is mono, stereo, 5.1 or adaptive"))?),
        None => None,
    };
    let custom: Option<Vec<Vec<u16>>> = p
        .get("clips")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|c| c.as_array().map(|x| x.iter().filter_map(|v| v.as_u64().map(|n| n as u16)).collect()).unwrap_or_default()).collect());
    let mut out = Vec::new();
    let mut maps = Vec::new();
    for i in &items {
        let Some(m) = s.project.item(*i).and_then(|it| it.as_media()) else { continue };
        let n = m.info.audio().map_or(2, |a| a.channels) as u16;
        let fmt = format.unwrap_or_else(|| m.interpret.audio_channels.as_ref().map_or(AudioChannels::Stereo, |a| a.format));
        let map = match &custom {
            Some(c) => {
                if c.is_empty() || c.iter().any(|x| x.is_empty() || x.iter().any(|ch| *ch >= n)) {
                    return Err(bad("clip.audioChannels", format!("each clip needs source channels between 0 and {}", n - 1)));
                }
                AudioChannelMap { format: fmt, clips: c.clone() }
            }
            None => AudioChannelMap::for_format(fmt, n),
        };
        out.push(json!({"item": i.0, "format": format!("{:?}", map.format), "clips": map.clips}));
        maps.push((*i, map));
    }
    s.edit("Modify Audio Channels", |pr, _| {
        for (i, map) in maps {
            if let Some(m) = pr.item_mut(i).and_then(|it| it.as_media_mut()) {
                // the default stereo pair is no override
                m.interpret.audio_channels = if map == AudioChannelMap::for_format(AudioChannels::Stereo, 2) { None } else { Some(map) };
            }
        }
        Ok(())
    })?;
    Ok(json!({"items": out}))
}

fn modify_timecode(s: &mut Session, p: &Value) -> Result<Value> {
    let item = item_p(p, "item")
        .or_else(|| s.state.project_selection.iter().copied().find(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some()))
        .ok_or_else(|| bad("clip.modifyTimecode", "select a clip"))?;
    let m = s.project.item(item).and_then(|i| i.as_media()).ok_or_else(|| bad("clip.modifyTimecode", "not a media clip"))?;
    let rate = m.frame_rate();
    let reset = bool_p(p, "reset").unwrap_or(false);
    let frame = if let Some(tc) = str_p(p, "timecode") {
        Some(parse_timecode(tc, rate, false, 0).map_err(|e| bad("clip.modifyTimecode", format!("timecode: {e:?}")))?)
    } else {
        p.get("frame").and_then(Value::as_i64)
    };
    let tape = str_p(p, "tapeName").map(str::to_string);
    if frame.is_none() && tape.is_none() && !reset {
        return Err(bad("clip.modifyTimecode", "need `timecode`, `frame`, `tapeName` or `reset`"));
    }
    if frame.is_some_and(|f| f < 0) {
        return Err(bad("clip.modifyTimecode", "timecode must not be negative"));
    }
    const FILE_TC: &str = "File Timecode";
    let r = s.edit("Modify Timecode", |pr, _| {
        let it = pr.item_mut(item).ok_or_else(|| bad("clip.modifyTimecode", "no such item"))?;
        let cur = it.as_media().and_then(|m| m.info.start_timecode);
        if reset {
            // back to the file's own timecode ("none" = the file had none)
            if let Some(orig) = it.metadata.remove(FILE_TC)
                && let Some(m) = it.as_media_mut()
            {
                m.info.start_timecode = orig.parse::<i64>().ok();
            }
        } else if let Some(f) = frame {
            it.metadata.entry(FILE_TC.to_string()).or_insert_with(|| cur.map(|c| c.to_string()).unwrap_or_else(|| "none".into()));
            if let Some(m) = it.as_media_mut() {
                m.info.start_timecode = Some(f);
            }
        }
        if let Some(t) = &tape {
            if t.is_empty() {
                it.metadata.remove("Tape Name");
            } else {
                it.metadata.insert("Tape Name".into(), t.clone());
            }
        }
        Ok(it.as_media().and_then(|m| m.info.start_timecode))
    })?;
    Ok(json!({"item": item.0, "startTimecode": r}))
}

// ---------------------------------------------------------------------------------------------
// Clip: Video Options
// ---------------------------------------------------------------------------------------------

fn video_clips(s: &Session, p: &Value) -> Vec<ClipId> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    clips_p(s, p).into_iter().filter(|c| q.video_tracks.iter().any(|t| t.item(*c).is_some())).collect()
}

fn frame_hold_options(s: &mut Session, p: &Value) -> Result<Value> {
    let clips = video_clips(s, p);
    if clips.is_empty() {
        return Err(bad("clip.frameHoldOptions", "select a video clip"));
    }
    let enabled = bool_p(p, "enabled").unwrap_or(true);
    let hold_filters = bool_p(p, "holdFilters").unwrap_or(false);
    let mode = str_p(p, "holdOn").unwrap_or("in").to_string();
    let ph = s.playhead();
    let seq_t = time_p(s, p, "");
    let tc = str_p(p, "timecode").map(str::to_string);
    let rates: BTreeMap<ClipId, (FrameRate, i64)> = {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        clips
            .iter()
            .filter_map(|c| {
                let (_, it) = q.find_item(*c)?;
                let start_tc = media_root(&s.project, it.item).and_then(|(_, m, _)| m.info.start_timecode).unwrap_or(0);
                Some((*c, (item_rate(&s.project, it.item), start_tc)))
            })
            .collect()
    };
    let raw_time = p.get("time").and_then(Value::as_i64).map(Tick);
    let held = s.edit_sequence("Frame Hold Options", |q, _, _| {
        let mut out = Vec::new();
        for c in &clips {
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            if !enabled {
                it.frame_hold = None;
                it.hold_filters = false;
                continue;
            }
            let last = it.end() - Tick(1);
            let unheld = |it: &TrackItem, t: Tick| it.moving_source_time_at(t.clamp(it.start, last));
            let h = match mode.as_str() {
                "in" => unheld(it, it.start),
                "out" => unheld(it, last),
                "playhead" => unheld(it, ph),
                "sequenceTime" => unheld(it, seq_t.ok_or_else(|| bad("clip.frameHoldOptions", "sequenceTime needs `time`/`timecode`"))?),
                "sourceTimecode" => {
                    let (rate, start_tc) = rates[c];
                    match (&tc, raw_time) {
                        (Some(tc), _) => {
                            let f = parse_timecode(tc, rate, false, 0).map_err(|e| bad("clip.frameHoldOptions", format!("timecode: {e:?}")))?;
                            rate.tick_of(f - start_tc)
                        }
                        (None, Some(t)) => t,
                        (None, None) => return Err(bad("clip.frameHoldOptions", "sourceTimecode needs `timecode` or `time` (media ticks)")),
                    }
                }
                m => return Err(bad("clip.frameHoldOptions", format!("unknown holdOn `{m}`"))),
            };
            let rate = rates.get(c).map(|r| r.0).unwrap_or_default();
            let h = rate.snap(h.max(Tick::ZERO));
            it.frame_hold = Some(h);
            it.hold_filters = hold_filters;
            out.push(json!({"clip": c.0, "hold": h.0}));
        }
        Ok(out)
    })?;
    Ok(json!({"clips": held}))
}

/// Add Frame Hold: the selected clips (or the targeted video clip under the playhead) hold the
/// frame at the playhead from there to their end; the clip is split at the playhead first.
pub(crate) fn add_frame_hold(s: &mut Session, p: &Value) -> Result<Value> {
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut clips: Vec<ClipId> = video_clips(s, p).into_iter().filter(|c| q.find_item(*c).is_some_and(|(_, i)| i.start <= t && t < i.end())).collect();
    if clips.is_empty() && p.get("clips").is_none() && p.get("clip").is_none() {
        let tg = s.targeting().targeted;
        clips = q.video_tracks.iter().filter(|tr| tg.contains(&tr.id) && !tr.locked).filter_map(|tr| tr.item_at(t).map(|i| i.id)).take(1).collect();
    }
    if clips.is_empty() {
        return Err(EngineError::Other("place the playhead over a selected video clip".into()));
    }
    let held = s.edit_sequence("Add Frame Hold", |q, ctx, st| {
        let mut out = Vec::new();
        for c in &clips {
            let Some((_, it)) = q.find_item(*c) else { continue };
            let hold = it.moving_source_time_at(t);
            let target = if it.start < t { edit::razor_items(q, &[*c], t, ctx).first().copied() } else { Some(*c) };
            if let Some(id) = target
                && let Some((_, r)) = q.find_item_mut(id)
            {
                r.frame_hold = Some(hold);
                out.push(id);
            }
        }
        st.selection = out.clone();
        Ok(out)
    })?;
    Ok(json!({"clips": held.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

fn insert_frame_hold_segment(s: &mut Session, p: &Value) -> Result<Value> {
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let rate = q.settings.frame_rate;
    let pick = |c: &ClipId| q.video_tracks.iter().find_map(|tr| tr.item(*c).filter(|i| i.start <= t && t < i.end()).map(|i| (tr.id, i.clone())));
    let mut hit = crate::commands::clip_p(p, "clip").and_then(|c| pick(&c)).or_else(|| s.state.selection.iter().find_map(pick));
    if hit.is_none() && crate::commands::clip_p(p, "clip").is_none() {
        let tg = s.targeting().targeted;
        hit = q.video_tracks.iter().filter(|tr| tg.contains(&tr.id) && !tr.locked).find_map(|tr| tr.item_at(t).map(|i| (tr.id, i.clone())));
    }
    let (track, it) = hit.ok_or_else(|| EngineError::Other("place the playhead over a video clip".into()))?;
    let secs = f64_p(p, "seconds").unwrap_or(2.0);
    let dur = rate.snap_nearest(Tick((secs * TICKS_PER_SECOND as f64).round() as i64)).max(rate.frame_duration());
    let hold = it.moving_source_time_at(t);
    let id = s.edit_sequence("Insert Frame Hold Segment", |q, ctx, st| {
        let mut seg = it.clone();
        seg.id = ClipId(ctx.alloc());
        seg.start = t;
        seg.duration = dur;
        seg.source_in = hold;
        seg.frame_hold = Some(hold);
        seg.link = None;
        seg.markers.clear();
        let ids = edit::insert(q, vec![(track, seg)], ctx)?;
        st.selection = ids.clone();
        Ok(ids[0])
    })?;
    Ok(json!({"clip": id.0, "duration": dur.0}))
}

fn field_options(s: &mut Session, p: &Value) -> Result<Value> {
    let clips = video_clips(s, p);
    if clips.is_empty() {
        return Err(bad("clip.fieldOptions", "select a video clip"));
    }
    let processing = match str_p(p, "processing").unwrap_or("none") {
        "none" => FieldProcessing::None,
        "alwaysDeinterlace" => FieldProcessing::AlwaysDeinterlace,
        "flickerRemoval" => FieldProcessing::FlickerRemoval,
        x => return Err(bad("clip.fieldOptions", format!("unknown processing `{x}`"))),
    };
    let fo = FieldOptions { reverse_field_dominance: bool_p(p, "reverseFieldDominance").unwrap_or(false), processing };
    s.edit_sequence("Field Options", |q, _, _| {
        for c in &clips {
            if let Some((_, it)) = q.find_item_mut(*c) {
                it.field_options = (fo != FieldOptions::default()).then_some(fo);
            }
        }
        Ok(())
    })?;
    Ok(json!({"clips": clips.len()}))
}

fn set_interpolation(s: &mut Session, p: &Value, m: TimeInterpolation) -> Result<Value> {
    let clips = video_clips(s, p);
    if clips.is_empty() {
        return Err(bad("clip.setTimeInterpolation", "select a video clip"));
    }
    s.edit_sequence(&format!("Time Interpolation: {}", m.label()), |q, _, _| {
        for c in &clips {
            if let Some((_, it)) = q.find_item_mut(*c) {
                it.time_interpolation = m;
            }
        }
        Ok(())
    })?;
    Ok(json!({"mode": m.name(), "clips": clips.len()}))
}

/// Fit to frame (the whole clip visible) / Fill frame (the frame covered): sets Motion ▸ Scale
/// and centres the clip.
fn fit_fill(s: &mut Session, p: &Value, fill: bool) -> Result<Value> {
    let clips = video_clips(s, p);
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (fw, fh) = (q.settings.width as f64, q.settings.height as f64);
    let sizes: Vec<(ClipId, (u32, u32))> =
        clips.iter().filter_map(|c| q.find_item(*c).and_then(|(_, i)| filmcraft_render::source_size(&s.project, i.item)).map(|sz| (*c, sz))).collect();
    if sizes.is_empty() {
        return Err(bad(if fill { "clip.fillFrame" } else { "clip.fitToFrame" }, "select a video clip"));
    }
    let label = if fill { "Fill Frame" } else { "Fit to Frame" };
    let out = s.edit_sequence(label, |q, _, _| {
        let mut out = Vec::new();
        for (c, (w, h)) in &sizes {
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            let (sx, sy) = (fw / (*w).max(1) as f64, fh / (*h).max(1) as f64);
            let pct = (if fill { sx.max(sy) } else { sx.min(sy) }) * 100.0;
            it.scale_to_frame = false;
            let Some(m) = it.effect_mut("motion") else { continue };
            for (k, v) in [
                ("scale", ParamValue::Float(pct)),
                ("uniform_scale", ParamValue::Bool(true)),
                ("position", ParamValue::Vec2(filmcraft_geom::Vec2::new(fw / 2.0, fh / 2.0))),
                ("anchor", ParamValue::Vec2(filmcraft_geom::Vec2::new(*w as f64 / 2.0, *h as f64 / 2.0))),
            ] {
                match m.param_mut(k) {
                    Some(prm) => {
                        prm.keyframes.clear();
                        prm.value = v;
                    }
                    None => {
                        m.params.insert(k.to_string(), filmcraft_project::Param::new(v));
                    }
                }
            }
            out.push(json!({"clip": c.0, "scale": pct}));
        }
        Ok(out)
    })?;
    Ok(json!({"clips": out}))
}

// ---------------------------------------------------------------------------------------------
// Clip: Audio Options
// ---------------------------------------------------------------------------------------------

fn breakout_to_mono(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> =
        items_p(s, p).into_iter().filter(|i| s.project.item(*i).and_then(|it| it.as_media()).is_some_and(|m| m.info.has_audio())).collect();
    if items.is_empty() {
        return Err(bad("clip.breakoutToMono", "select a clip with audio in the Project panel"));
    }
    let made = s.edit("Breakout to Mono", |pr, st| {
        let mut made = Vec::new();
        for i in &items {
            let Some(it) = pr.item(*i).cloned() else { continue };
            let Some(m) = it.as_media() else { continue };
            let n = m.info.audio().map_or(1, |a| a.channels).max(1) as u16;
            let bin = pr.root.parent_of(*i).filter(|b| *b != pr.root.id);
            for c in 0..n {
                let suffix = match (n, c) {
                    (2, 0) => "Left".to_string(),
                    (2, 1) => "Right".to_string(),
                    _ => format!("Ch {}", c + 1),
                };
                let mut mm = m.clone();
                mm.interpret.audio_channels = Some(AudioChannelMap { format: AudioChannels::Mono, clips: vec![vec![c]] });
                mm.mark_in = None;
                mm.mark_out = None;
                mm.markers.clear();
                mm.proxy = None;
                let id = pr.add_item(&format!("{} {suffix}", it.name), it.label, ItemKind::Media(mm), bin);
                made.push(id);
            }
        }
        st.project_selection = made.clone();
        Ok(made)
    })?;
    Ok(json!({"items": made.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

fn extract_audio(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<ItemId> = match p.get("items") {
        Some(_) => items_p(s, p),
        None => extract_items(s),
    };
    if items.is_empty() {
        return Err(bad("clip.extractAudio", "select a clip with audio"));
    }
    let n0 = s.history.undo.len();
    let mut out = Vec::new();
    for item in items {
        let (root, m, sub) = media_root(&s.project, item).ok_or_else(|| bad("clip.extractAudio", "not a media clip"))?;
        let a = m.info.audio().cloned().ok_or_else(|| bad("clip.extractAudio", "the clip has no audio"))?;
        let range = sub.unwrap_or(TimeRange::new(Tick::ZERO, m.info.duration));
        let dir = match str_p(p, "dir") {
            Some(d) => d.to_string(),
            None => s
                .path
                .as_deref()
                .and_then(|x| std::path::Path::new(x).parent())
                .map(|d| d.to_string_lossy().into_owned())
                .or_else(|| match &m.media {
                    MediaRef::File { path } => std::path::Path::new(path).parent().map(|d| d.to_string_lossy().into_owned()),
                    _ => None,
                })
                .ok_or_else(|| EngineError::Other("save the project first: extracted audio is written next to it".into()))?,
        };
        let pi = s.project.item(item).ok_or_else(|| EngineError::Other("no such item".into()))?;
        let name = pi.name.clone();
        let stem = std::path::Path::new(&name).file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_else(|| name.clone());
        let src = s.media.full_res_source(&s.project, root, &*s.services).ok_or_else(|| EngineError::Other(format!("{name}: media is offline")))?;
        let sr = a.sample_rate.max(8000);
        let start = range.start.to_units_floor(sr as i64);
        let frames = (range.end().to_units_floor(sr as i64) - start).max(0) as usize;
        let buf = src.audio(start, frames, sr).map_err(|e| EngineError::Other(format!("{name}: {e}")))?;
        let ch = buf.channel_count().max(1);
        let mut inter = Vec::with_capacity(frames * ch);
        for i in 0..frames {
            for c in 0..ch {
                inter.push(buf.channels.get(c).and_then(|x| x.get(i)).copied().unwrap_or(0.0));
            }
        }
        let bytes = filmcraft_media::wav::write_wav16(&inter, ch as u16, sr);
        let mut path = std::path::Path::new(&dir).join(format!("{stem} Audio Extracted.wav"));
        let mut k = 2;
        while s.services.file_size(&path.to_string_lossy()).is_ok() {
            path = std::path::Path::new(&dir).join(format!("{stem} Audio Extracted {k}.wav"));
            k += 1;
        }
        let path_s = path.to_string_lossy().into_owned();
        s.services.write_file(&path_s, &bytes).map_err(|e| EngineError::Other(format!("{path_s}: {e}")))?;
        let bin = s.project.root.parent_of(item).filter(|b| *b != s.project.root.id);
        let id = crate::commands::import_bytes(s, &path_s, bytes.into(), bin)?;
        out.push(json!({"item": id.0, "path": path_s, "from": item.0}));
    }
    collapse_history(s, n0, "Extract Audio");
    Ok(json!({"extracted": out}))
}

// ---------------------------------------------------------------------------------------------
// Clip: Replace With Clip
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Replace {
    Source,
    MatchFrame,
    Bin,
}

fn replace_with(s: &mut Session, p: &Value, how: Replace) -> Result<Value> {
    let cmd = match how {
        Replace::Source => "clip.replaceFromSource",
        Replace::MatchFrame => "clip.replaceFromSourceMatchFrame",
        Replace::Bin => "clip.replaceFromBin",
    };
    let new_item = match how {
        Replace::Bin => item_p(p, "item").or_else(|| s.state.project_selection.iter().copied().find(|i| is_clip_item(&s.project, *i))),
        _ => s.state.source_item,
    }
    .ok_or_else(|| bad(cmd, "no replacement clip"))?;
    let pi = s.project.item(new_item).ok_or_else(|| bad(cmd, "no such item"))?.clone();
    let (has_v, has_a) = (pi.has_video(), pi.has_audio());
    // where the replacement starts in its media: Source In (or the subclip / item start)
    let in_point = match how {
        Replace::Bin => item_range(&s.project, new_item, s.prefs.timeline.still_duration(s.sequence_rate())).map(|r| r.start).unwrap_or_default(),
        _ => source_range(s).map(|(_, r)| r.start).unwrap_or_default(),
    };
    // From Bin starts at the item's In point, as Premiere does; `keepSourceIn` keeps each clip's own
    let keep_in = how == Replace::Bin && bool_p(p, "keepSourceIn").unwrap_or(false);
    // ...but a subclip that restricts trims has no media outside its range: a kept In stays on
    // one of its frames (first frame, last frame)
    let keep_within = match &pi.kind {
        ItemKind::Subclip { range, restrict_trims: true, .. } => {
            Some((range.start, (range.end() - item_rate(&s.project, new_item).frame_duration()).max(range.start)))
        }
        _ => None,
    };
    let src_ph = s.state.source_playhead;
    let ph = s.playhead();
    let targets = with_links(s, &clips_p(s, p));
    let src_channels = pi.as_media().and_then(|m| m.interpret.audio_channels.as_ref()).and_then(|a| a.clips.first().cloned()).unwrap_or_default();
    let (replaced, clamped) = s.edit_sequence("Replace With Clip", |q, _, _| {
        let (mut out, mut clamped) = (Vec::new(), Vec::new());
        for c in &targets {
            let Some((tid, _)) = q.find_item(*c) else { continue };
            let kind = q.track(tid).map(|t| t.kind).unwrap_or_default();
            if (kind == TrackKind::Video && !has_v) || (kind == TrackKind::Audio && !has_a) {
                continue;
            }
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            let src_in = match how {
                // the Source monitor frame lines up with the sequence playhead
                Replace::MatchFrame => src_ph - Tick(((ph - it.start).0 as f64 * it.speed.abs()).round() as i64),
                _ if keep_in => {
                    let kept = keep_within.map(|(first, last)| it.source_in.clamp(first, last)).unwrap_or(it.source_in);
                    if kept != it.source_in {
                        clamped.push(c.0);
                    }
                    kept
                }
                _ => in_point,
            };
            if src_in < Tick::ZERO {
                return Err(EngineError::Other("not enough media before the Source monitor playhead to match the frame".into()));
            }
            it.item = new_item;
            it.name = pi.name.clone();
            it.label = pi.label;
            it.source_in = src_in;
            it.frame_hold = None;
            it.multicam = None;
            if kind == TrackKind::Audio {
                it.source_channels = src_channels.clone();
            }
            out.push(c.0);
        }
        if out.is_empty() {
            return Err(EngineError::Other("the replacement has no video or audio for the selected clips".into()));
        }
        Ok((out, clamped))
    })?;
    let mut result = json!({"clips": replaced, "item": new_item.0, "short": short_clips(s, &replaced)});
    if keep_in {
        // the clips whose In was outside a trims-restricted subclip and now sits on its nearest frame
        result["sourceInClamped"] = json!(clamped);
    }
    Ok(result)
}

/// The clips among `clips` that play past the end of their media, with how many sequence frames
/// of the clip have no media (at most the clip's own length). The edit is left as it is (the
/// renderer holds the last frame there); this is so whoever replaced the clip knows.
fn short_clips(s: &Session, clips: &[u64]) -> Vec<Value> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let frame = q.settings.frame_rate.frame_duration().0.max(1);
    clips
        .iter()
        .filter_map(|c| {
            let (_, it) = q.find_item(ClipId(*c))?;
            // stills, mattes and other sources without an end are never short; nor is a held frame
            let media_end = crate::media_duration(&s.project, &s.media, it.item)?;
            let over = it.source_out().0.saturating_sub(media_end.0);
            if over <= 0 || it.frame_hold.is_some() {
                return None;
            }
            // media ticks past the end, as sequence ticks of this clip
            let length = it.duration.0.max(0);
            let ticks = ((over as f64 / it.speed.abs().max(1e-9)).round().min(length as f64) as i64).min(length);
            let frames = ticks.saturating_add(frame - 1) / frame;
            (frames > 0).then(|| json!({"clip": c, "shortByFrames": frames}))
        })
        .collect()
}
