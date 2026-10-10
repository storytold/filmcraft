//! The command registry. Ids follow Premiere's menu structure; every menu item, panel button,
//! shortcut and timeline gesture maps to exactly one of these.

use std::sync::OnceLock;

use filmcraft_edit as edit;
use filmcraft_edit::{Edge, TrimMode};
use filmcraft_media::Generator;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_project::{
    ClipId, ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, MediaClip, MediaRef, ParamValue, Project, SequenceSettings, Track, TrackId, TrackKind,
    Transition, TransitionId, resolve_auto_points,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange, parse_timecode};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

/// Metadata + implementation for one command.
pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Menu placement, e.g. `["Sequence"]` or `["File", "New"]`. Empty = not in menus.
    pub menu: &'static [&'static str],
    /// Default shortcut (egui-style names, `Cmd` = ⌘ on macOS / Ctrl elsewhere).
    pub shortcut: Option<&'static str>,
    /// Parameter description (JSON-ish, for agents and the MCP schema).
    pub params: &'static str,
    pub enabled: Enabled,
    pub run: Run,
    /// Record in the journal (false for pure queries).
    pub journal: bool,
}

macro_rules! cmd {
    ($id:literal, $label:literal, [$($m:literal),*], $sc:expr, $params:literal, $en:expr, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[$($m),*], shortcut: $sc, params: $params, enabled: $en, run: $run, journal: true }
    };
}
macro_rules! query {
    ($id:literal, $label:literal, $params:literal, $run:expr) => {
        CommandSpec { id: $id, label: $label, menu: &[], shortcut: None, params: $params, enabled: always, run: $run, journal: false }
    };
}

pub fn command_specs() -> &'static [CommandSpec] {
    static SPECS: OnceLock<Vec<CommandSpec>> = OnceLock::new();
    SPECS.get_or_init(build)
}

pub fn find(id: &str) -> Option<&'static CommandSpec> {
    command_specs().iter().find(|c| c.id == id)
}

// ---------- enablement ----------

pub(crate) fn always(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}
fn has_edit_points(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.edit_points.is_empty() { Err("no edit points selected".into()) } else { Ok(()) }
}

fn has_saved_path(s: &Session) -> std::result::Result<(), String> {
    if s.path.is_some() { Ok(()) } else { Err("the project has not been saved yet".into()) }
}
fn has_recovery(s: &Session) -> std::result::Result<(), String> {
    if s.recovery_candidates().is_empty() { Err("there are no unsaved changes to recover".into()) } else { Ok(()) }
}

pub(crate) fn has_seq(s: &Session) -> std::result::Result<(), String> {
    s.active_sequence().map(|_| ()).ok_or_else(|| "no sequence is open".into())
}
pub(crate) fn has_selection(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.selection.is_empty() { Err("no clips selected".into()) } else { Ok(()) }
}
/// Clips or captions selected (Clear / Ripple Delete work on either).
fn has_any_selection(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.selection.is_empty() && s.state.caption_selection.is_empty() && s.state.transition_selection.is_empty() {
        Err("nothing selected".into())
    } else {
        Ok(())
    }
}
/// Clips or captions selected (Ripple Delete: a transition has no length of its own to close).
fn has_clip_or_caption_selection(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.selection.is_empty() && s.state.caption_selection.is_empty() { Err("nothing selected".into()) } else { Ok(()) }
}
fn has_source(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    s.state.source_item.map(|_| ()).ok_or_else(|| "no clip in the Source monitor".into())
}
fn can_undo(s: &Session) -> std::result::Result<(), String> {
    if s.history.can_undo() { Ok(()) } else { Err("nothing to undo".into()) }
}
fn can_redo(s: &Session) -> std::result::Result<(), String> {
    if s.history.can_redo() { Ok(()) } else { Err("nothing to redo".into()) }
}
fn has_in_out(s: &Session) -> std::result::Result<(), String> {
    let seq = s.active_sequence().ok_or("no sequence is open")?;
    if seq.mark_in.is_some() || seq.mark_out.is_some() { Ok(()) } else { Err("mark an In or Out point first".into()) }
}
fn has_previews(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.previews.count() == 0 { Err("there are no render files".into()) } else { Ok(()) }
}
pub(crate) fn has_project_selection(s: &Session) -> std::result::Result<(), String> {
    if s.state.project_selection.is_empty() { Err("select an item in the Project panel".into()) } else { Ok(()) }
}
fn has_clipboard(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.clipboard.is_empty() { Err("clipboard is empty".into()) } else { Ok(()) }
}

/// The ids a caller named under `many` (a list) or `one` (a single id), when the command documents
/// that key as ids (`"clips":[id]`, `"item":id`); `None` when it named none.
fn named_ids(spec: &CommandSpec, p: &Value, many: &str, one: &str) -> Option<Vec<u64>> {
    let ids: Vec<u64> = if spec.params.contains(&format!("\"{many}\":[id]"))
        && let Some(a) = p.get(many).and_then(Value::as_array)
    {
        a.iter().filter_map(Value::as_u64).collect()
    } else if spec.params.contains(&format!("\"{one}\":id")) {
        u64_p(p, one).into_iter().collect()
    } else {
        Vec::new()
    };
    (!ids.is_empty()).then_some(ids)
}
/// Timeline clips named explicitly in `clips` / `clip` (see [`Session::execute`]): only clips of
/// the active sequence count, so an id of nothing does not stand in for a selection.
pub(crate) fn named_clips(s: &Session, spec: &CommandSpec, p: &Value) -> Option<Vec<ClipId>> {
    let seq = s.active_sequence()?;
    let clips: Vec<ClipId> = named_ids(spec, p, "clips", "clip")?.into_iter().map(ClipId).filter(|c| seq.find_item(*c).is_some()).collect();
    (!clips.is_empty()).then_some(clips)
}
/// Project items named explicitly in `items` / `item`, likewise. The Project panel's selection
/// holds bins beside items (one id space, see `project.select`), so a bin named here counts too;
/// the project's own top bin does not, since no command acts on it (#244).
pub(crate) fn named_items(s: &Session, spec: &CommandSpec, p: &Value) -> Option<Vec<ItemId>> {
    let root = s.project.root.id;
    let exists = |i: &ItemId| {
        let b = filmcraft_project::BinId(i.0);
        s.project.item(*i).is_some() || (b != root && s.project.root.find_bin(b).is_some())
    };
    let items: Vec<ItemId> = named_ids(spec, p, "items", "item")?.into_iter().map(ItemId).filter(exists).collect();
    (!items.is_empty()).then_some(items)
}

// ---------- param helpers ----------

pub(crate) fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}
pub(crate) fn str_p<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}
pub(crate) fn f64_p(p: &Value, k: &str) -> Option<f64> {
    p.get(k).and_then(Value::as_f64)
}
/// `fps` as a sequence frame rate; zero or negative rates are refused (all frame maths divides by them).
fn fps_p(p: &Value, cmd: &str) -> Result<Option<FrameRate>> {
    let Some(fps) = f64_p(p, "fps") else { return Ok(None) };
    let r = FrameRate::from_f64(fps);
    if r.num <= 0 || r.frame_duration() <= Tick::ZERO {
        return Err(bad(cmd, format!("fps must be a positive frame rate, got {fps}")));
    }
    Ok(Some(r))
}
pub(crate) fn bool_p(p: &Value, k: &str) -> Option<bool> {
    p.get(k).and_then(Value::as_bool)
}
pub(crate) fn u64_p(p: &Value, k: &str) -> Option<u64> {
    p.get(k).and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f as u64)))
}

/// Typed dimensions/counts must not truncate, wrap or silently accept negative/fractional values.
/// Integer-valued floats (`1920.0`, as JSON from many clients) are integers.
pub(crate) fn checked_u32_p(p: &Value, key: &str, cmd: &str) -> Result<Option<u32>> {
    let Some(value) = p.get(key).filter(|v| !v.is_null()) else { return Ok(None) };
    let exact = value
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .or_else(|| value.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(f)).map(|f| f as u32));
    exact.map(Some).ok_or_else(|| bad(cmd, format!("`{key}` must be an unsigned 32-bit integer")))
}

/// Parse a time from params: `time` (ticks), `frame`, `seconds` or `timecode`, with `prefix`.
pub(crate) fn time_p(s: &Session, p: &Value, prefix: &str) -> Option<Tick> {
    let rate = s.sequence_rate();
    let k = |n: &str| if prefix.is_empty() { n.to_string() } else { format!("{prefix}{}{}", n[..1].to_uppercase(), &n[1..]) };
    if let Some(t) = p.get(k("time")).and_then(Value::as_i64) {
        return Some(Tick(t));
    }
    if let Some(f) = p.get(k("frame")).and_then(Value::as_i64) {
        return Some(rate.tick_of(f));
    }
    if let Some(sec) = p.get(k("seconds")).and_then(Value::as_f64) {
        return Some(Tick::from_seconds_f64(sec));
    }
    if let Some(tc) = p.get(k("timecode")).and_then(Value::as_str) {
        let df = s.active_sequence().map(|q| q.settings.drop_frame).unwrap_or(false);
        let cur = rate.frame_at(s.playhead());
        return parse_timecode(tc, rate, df, cur).ok().map(|f| rate.tick_of(f));
    }
    None
}

pub(crate) fn clip_p(p: &Value, k: &str) -> Option<ClipId> {
    u64_p(p, k).map(ClipId)
}
/// Effect by id or display name. Names shared by several definitions ("Invert", "Volume",
/// "Channel Volume") resolve to an applicable (non-intrinsic) effect whose kind matches the
/// target clips' tracks.
fn resolve_effect_name(s: &Session, p: &Value, name: &str) -> Option<&'static filmcraft_project::EffectDef> {
    if let Some(d) = filmcraft_project::find_effect(name) {
        return Some(d);
    }
    let n = name.to_ascii_lowercase();
    let cands: Vec<_> = filmcraft_project::effect_defs().iter().filter(|e| e.name.to_ascii_lowercase() == n).collect();
    if cands.len() <= 1 {
        return cands.first().copied().or_else(|| filmcraft_project::find_effect(&n));
    }
    let on_audio = s.active_sequence().is_some_and(|q| {
        let clips = clips_p(s, p);
        !clips.is_empty() && clips.iter().all(|c| q.find_item(*c).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.kind == TrackKind::Audio))
    });
    let want = if on_audio { filmcraft_project::EffectKind::Audio } else { filmcraft_project::EffectKind::Video };
    cands.iter().find(|d| !d.intrinsic && d.kind == want).or_else(|| cands.iter().find(|d| !d.intrinsic)).or(cands.first()).copied()
}

pub(crate) fn clips_p(s: &Session, p: &Value) -> Vec<ClipId> {
    match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None => clip_p(p, "clip").map(|c| vec![c]).unwrap_or_else(|| s.state.selection.clone()),
    }
}
/// The track parameter `k` names: a track id, or "V1" / "A2" (kind and 1-based number).
/// `Ok(None)` when the parameter is absent. Naming a track the active sequence does not have is
/// an error (bad parameters of `cmd`): callers fall back to a default track only when no track
/// was asked for.
pub(crate) fn track_p(s: &Session, p: &Value, k: &str, cmd: &str) -> Result<Option<TrackId>> {
    let v = match p.get(k) {
        None | Some(Value::Null) => return Ok(None),
        Some(v) => v,
    };
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let by_name = |name: &str| {
        let mut rest = name.chars();
        let tracks = match rest.next()? {
            'v' | 'V' => &seq.video_tracks,
            'a' | 'A' => &seq.audio_tracks,
            _ => return None,
        };
        let i: usize = rest.as_str().parse().ok()?;
        tracks.get(i.checked_sub(1)?).map(|t| t.id)
    };
    let found = match v.as_u64() {
        Some(id) => seq.track(TrackId(id)).map(|t| t.id),
        None => v.as_str().and_then(by_name),
    };
    let named = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
    found.map(Some).ok_or_else(|| bad(cmd, format!("no track {named} in this sequence (`{k}`)")))
}
pub(crate) fn item_p(p: &Value, k: &str) -> Option<ItemId> {
    u64_p(p, k).map(ItemId)
}

/// `timeline.move`: move the listed clips to (track, time). With Linked Selection on (or
/// `linked: true`) the linked partners of a listed clip that are not listed themselves follow by
/// the same time offset on their own tracks, so picture and sound stay in sync; a partner on a
/// locked track, or one whose position would not change, stays as it is. Nothing moves before the sequence start: when a clip would,
/// every clip of the call lands later by the same amount, so the spacing asked for is kept.
/// `overwritten` lists every clip the move covered without being asked to move it: its length
/// before and after (ticks) and the clips what is left of it now lives on as (none = removed; a
/// clip a moved clip lands inside is split in two).
fn move_clips(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "timeline.move";
    let mut listed: Vec<(ClipId, TrackId, Tick)> = Vec::new();
    for m in p.get("moves").and_then(Value::as_array).into_iter().flatten() {
        let track = track_p(s, m, "track", CMD)?;
        if let (Some(clip), Some(track), Some(time)) = (clip_p(m, "clip"), track, m.get("time").and_then(Value::as_i64)) {
            listed.push((clip, track, Tick(time)));
        }
    }
    if listed.is_empty() {
        return Err(bad(CMD, "need `moves`"));
    }
    let follow = bool_p(p, "linked").unwrap_or(s.state.linked_selection);
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // (clip, destination track, new start, where a partner starts now). `time` is any i64: the
    // sums are done in i128, where a few i64 values cannot overflow, and checked on the way back.
    let mut plan: Vec<(ClipId, TrackId, i128, Option<i128>)> = Vec::new();
    for &(clip, track, time) in &listed {
        plan.push((clip, track, i128::from(time.0), None));
        let Some((_, it)) = q.find_item(clip) else { continue }; // `edit::move_items` reports it
        let Some(link) = it.link.filter(|_| follow) else { continue };
        let offset = i128::from(time.0) - i128::from(it.start.0);
        for t in q.all_tracks().filter(|t| !t.locked) {
            for partner in t.items.iter().filter(|i| i.link == Some(link)) {
                if !listed.iter().any(|m| m.0 == partner.id) && !plan.iter().any(|m| m.0 == partner.id) {
                    plan.push((partner.id, t.id, i128::from(partner.start.0) + offset, Some(i128::from(partner.start.0))));
                }
            }
        }
    }
    let late = plan.iter().map(|m| -m.2).max().unwrap_or(0).max(0);
    let mut moves: Vec<(ClipId, TrackId, Tick)> = Vec::with_capacity(plan.len());
    for (clip, track, start, was) in plan {
        // a partner that ends up where it is does not move (its listed clip only changed track):
        // re-placing it would drop the transitions at its edges
        if was == Some(start + late) {
            continue;
        }
        let start = i64::try_from(start + late).ok().map(Tick).filter(|t| *t <= Tick::MAX).ok_or_else(|| bad(CMD, "`time` is out of range"))?;
        moves.push((clip, track, start));
    }
    let ins = bool_p(p, "insert").unwrap_or(false);
    // every clip on the sequence now: id -> (track, start, end, project item)
    let spots = |s: &Session| -> Vec<(u64, TrackId, Tick, Tick, ItemId)> {
        let q = s.active_sequence();
        q.into_iter().flat_map(|q| q.all_tracks()).flat_map(|t| t.items.iter().map(move |i| (i.id.0, t.id, i.start, i.end(), i.item))).collect()
    };
    let before = spots(s);
    s.edit_sequence(if ins { "Move (Insert)" } else { "Move" }, |q, ctx, _| Ok(edit::move_items(q, &moves, ins, ctx)?))?;
    // An overwrite shortens, splits or removes whatever already sits where a clip lands, and a
    // shortened clip may live on under a new id: name each clip the move changed without being
    // asked to, with what is left of it, so the caller hears about it instead of finding out later.
    let after = spots(s);
    let moved = |c: u64| moves.iter().any(|m| m.0.0 == c);
    let new = |c: u64| !before.iter().any(|b| b.0 == c);
    let mut overwritten = Vec::new();
    for &(c, track, start, end, item) in before.iter().filter(|b| !moved(b.0)) {
        let kept = after.iter().find(|a| a.0 == c);
        if ins || kept.is_some_and(|k| (k.2, k.3) == (start, end)) {
            continue; // untouched (an insert only shifts clips, it never covers one)
        }
        let pieces: Vec<_> =
            after.iter().filter(|a| a.0 == c || (new(a.0) && !moved(a.0) && a.1 == track && a.4 == item && a.2 >= start && a.3 <= end)).collect();
        let now: i64 = pieces.iter().map(|a| (a.3 - a.2).0).sum();
        overwritten.push(json!({"clip": c, "was": (end - start).0, "now": now, "pieces": pieces.iter().map(|a| a.0).collect::<Vec<_>>()}));
    }
    Ok(json!({"moved": moves.iter().map(|m| m.0.0).collect::<Vec<_>>(), "overwritten": overwritten}))
}

/// Expand a clip selection with linked partners (when linked selection is on).
pub fn with_links(s: &Session, clips: &[ClipId]) -> Vec<ClipId> {
    let Some(seq) = s.active_sequence() else { return clips.to_vec() };
    let mut out = clips.to_vec();
    if s.state.linked_selection {
        let links: Vec<u64> = clips.iter().filter_map(|c| seq.find_item(*c).and_then(|(_, i)| i.link)).collect();
        for t in seq.all_tracks() {
            for i in &t.items {
                if i.link.is_some_and(|l| links.contains(&l)) && !out.contains(&i.id) {
                    out.push(i.id);
                }
            }
        }
    }
    out
}

/// Sequence settings matching a media clip (New Sequence From Clip, New Sequence from an item):
/// its frame size, rate and pixel aspect ratio (Interpret Footage's, else the file's), so a
/// 1440 x 1080 clip with 4:3 pixels makes a 1440 x 1080 sequence that displays 16:9.
pub(crate) fn default_seq_settings_for(media: &MediaClip) -> SequenceSettings {
    let info = &media.info;
    let mut st = SequenceSettings::default();
    if let Some(v) = &info.video {
        st.width = v.width;
        st.height = v.height;
        st.frame_rate = v.frame_rate;
        st.par = media.pixel_aspect();
        st.preset = format!("{}x{} {}", v.width, v.height, v.frame_rate.label());
    }
    if let Some(a) = info.audio() {
        st.sample_rate = a.sample_rate.max(8000);
    }
    st
}

/// Import bytes as a media item (engine-level; the UI reads files through services).
pub fn import_bytes(s: &mut Session, path: &str, bytes: std::sync::Arc<[u8]>, bin: Option<filmcraft_project::BinId>) -> Result<ItemId> {
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let identity = crate::relink::identity_of_bytes(&bytes);
    let src = s.media.open_bytes(&name, bytes)?;
    import_source(s, path, name, src, identity, bin)
}

/// Import a media file through the host's random-access reader ([`crate::Services::reader`]):
/// only the container index is read now (web `Blob`s are never copied whole for containers).
pub fn import_streamed(s: &mut Session, path: &str, bin: Option<filmcraft_project::BinId>) -> Result<ItemId> {
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let identity = crate::relink::identity_of(&*s.services, path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let src = s.media.open_file(path, &*s.services)?;
    import_source(s, path, name, src, identity, bin)
}

/// Import the image sequence that the numbered still `path` starts (File ▸ Import with Image
/// Sequence): one movie item at Settings ▸ Media ▸ Indeterminate Media Timebase. Returns the item,
/// the frame count and the missing frame numbers (relative to the first).
pub fn import_image_sequence(s: &mut Session, path: &str, bin: Option<filmcraft_project::BinId>) -> Result<(ItemId, usize, Vec<usize>)> {
    let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
    let rate = crate::settings::timebase_rate(&s.prefs.media.indeterminate_timebase);
    let frames = crate::media_pool::image_sequence_frames(path, &*s.services)?;
    let missing: Vec<usize> = frames.iter().enumerate().filter(|(_, f)| f.is_none()).map(|(i, _)| i).collect();
    let src = crate::media_pool::open_image_sequence(path, &*s.services, rate, &name)?;
    let identity = crate::relink::identity_of(&*s.services, path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let id = import_source(s, path, name, src, identity, bin)?;
    Ok((id, frames.len(), missing))
}

/// Settings ▸ Media ▸ Import image sequences: a single numbered still with later frames beside it.
fn detect_image_sequence(s: &Session, path: &str) -> bool {
    filmcraft_media::sequence::Numbered::parse(path).is_some() && crate::media_pool::image_sequence_frames(path, &*s.services).is_ok_and(|f| f.len() > 1)
}

/// The item a `file.import` result is about: the first it added, else the first it found already
/// in the project (`duplicates`).
pub(crate) fn imported_item(r: &Value) -> Option<ItemId> {
    let first = |k: &str| r.get(k).and_then(Value::as_array).and_then(|a| a.first());
    first("items").and_then(Value::as_u64).or_else(|| first("duplicates").and_then(|d| d.get("item")).and_then(Value::as_u64)).map(ItemId)
}

fn import_source(
    s: &mut Session,
    path: &str,
    name: String,
    src: filmcraft_media::SharedSource,
    identity: filmcraft_project::MediaIdentity,
    bin: Option<filmcraft_project::BinId>,
) -> Result<ItemId> {
    let mut info = src.info().clone();
    // Settings ▸ Labels ▸ Label Defaults
    let label = s.prefs.labels.for_media(info.kind, info.has_video(), info.has_audio());
    // Settings ▸ Media ▸ Indeterminate Media Timebase: the frame rate stills report
    if info.kind == filmcraft_media::MediaKind::Still
        && let Some(v) = info.video.as_mut()
    {
        v.frame_rate = crate::settings::timebase_rate(&s.prefs.media.indeterminate_timebase);
    }
    let path_s = path.to_string();
    let id = s.edit(&format!("Import {name}"), |p, _| {
        Ok(p.add_item(
            &name,
            label,
            ItemKind::Media(MediaClip {
                media: MediaRef::File { path: path_s },
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: Some(identity),
            }),
            bin,
        ))
    })?;
    s.media.insert_file(id, path, src);
    Ok(id)
}

fn new_generator(s: &mut Session, g: Generator, name: &str, label: Label, p: &Value) -> Result<Value> {
    let st = s.active_sequence().map(|q| q.settings.clone()).unwrap_or_default();
    let w = u64_p(p, "width").map(|v| v as u32).unwrap_or(st.width);
    let h = u64_p(p, "height").map(|v| v as u32).unwrap_or(st.height);
    let secs = f64_p(p, "seconds").unwrap_or(5.0);
    let name = str_p(p, "name").unwrap_or(name).to_string();
    let src = GeneratorSource::new(g, w, h, st.frame_rate, Tick::from_seconds_f64(secs));
    let pool = s.media.clone();
    let par = filmcraft_project::sane_par(st.par);
    let id = s.edit(&format!("New {name}"), |pr, st| {
        let id = crate::demo::add_generator(pr, &pool, src, &name, label, None);
        // made for the sequence: its pixels have the sequence's aspect, as in Premiere
        if let Some(ItemKind::Media(m)) = pr.item_mut(id).map(|i| &mut i.kind)
            && let Some(v) = m.info.video.as_mut()
        {
            v.par = par;
        }
        st.project_selection = vec![id];
        Ok(id)
    })?;
    Ok(json!({"item": id.0}))
}

/// `project.matteColor`: change a Color Matte's color (`item`, or the one selected in the Project
/// panel). Every clip of the matte follows; undo brings the old color back.
fn set_matte_color(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "project.matteColor";
    let color = str_p(p, "color").and_then(filmcraft_color::parse_hex).ok_or_else(|| bad(CMD, "`color` must be #rrggbb"))?;
    let is_matte = |s: &Session, id: ItemId| {
        s.project.item(id).is_some_and(|it| matches!(&it.kind, ItemKind::Media(m) if matches!(m.media, MediaRef::Generator(Generator::ColorMatte { .. }))))
    };
    let item = match item_p(p, "item") {
        Some(id) => id,
        None => match s.state.project_selection.as_slice() {
            [id] => *id,
            _ => return Err(bad(CMD, "select one Color Matte (or pass `item`)")),
        },
    };
    if !is_matte(s, item) {
        return Err(bad(CMD, format!("item {} is not a Color Matte", item.0)));
    }
    s.edit("Color Matte Color", |pr, _| {
        if let Some(ItemKind::Media(m)) = pr.item_mut(item).map(|it| &mut it.kind)
            && let MediaRef::Generator(Generator::ColorMatte { color: c }) = &mut m.media
        {
            *c = color;
        }
        Ok(())
    })?;
    Ok(json!({"item": item.0, "color": filmcraft_color::to_hex(color)}))
}

/// One audio clip of a placement: how many tracks below the destination it goes, and the source
/// channels it plays (`None` = the clip's own default, used for the first one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AudioPlacementSpec {
    pub track_offset: usize,
    pub audio_stream: usize,
    pub source_channels: Option<Vec<u16>>,
}

/// The audio clips to place for a source whose Modify ▸ Audio Channels map has `clips` (one entry
/// per clip): always at least one (on the destination track), then one per further entry on the
/// tracks below. The count is capped at [`crate::sequence_tools::MAX_TRACKS`] (the map comes from
/// project files and scripts: never trusted).
pub(crate) fn audio_placement_specs(clips: &[Vec<u16>], streams: usize) -> Vec<AudioPlacementSpec> {
    let mut specs = vec![AudioPlacementSpec { track_offset: 0, audio_stream: 0, source_channels: None }];
    let extra = clips.iter().skip(1).take(crate::sequence_tools::MAX_TRACKS.saturating_sub(1));
    specs.extend(extra.enumerate().map(|(k, chans)| AudioPlacementSpec {
        track_offset: k.saturating_add(1),
        audio_stream: 0,
        source_channels: Some(chans.clone()),
    }));
    // Interpret Footage's channel map describes stream 0; further container streams keep their
    // own channel layouts and get one clip each, after any channel-map clips.
    for stream in 1..streams.min(filmcraft_media::MAX_AUDIO_STREAMS) {
        if specs.len() >= crate::sequence_tools::MAX_TRACKS {
            break;
        }
        specs.push(AudioPlacementSpec { track_offset: specs.len(), audio_stream: stream, source_channels: None });
    }
    specs
}

/// Make sure the sequence has audio tracks for `count` clips starting at track index `first`,
/// adding tracks at the bottom when it has fewer. A new track is a copy of the destination track
/// (channel format, volume, pan, inserts) without its clips, transitions, lock / mute / solo
/// state or mixer routing. Returns the ids of all the sequence's audio tracks, in order.
pub(crate) fn ensure_audio_tracks(p: &mut Project, seq_id: ItemId, first: usize, count: usize, label: &str) -> Result<Vec<TrackId>> {
    let (have, template) = {
        let q = p.sequence(seq_id).ok_or(EngineError::NoSequence)?;
        (q.audio_tracks.len(), q.audio_tracks.get(first).or(q.audio_tracks.last()).cloned())
    };
    let need = first.saturating_add(count);
    if need > crate::sequence_tools::MAX_TRACKS {
        return Err(bad(label, format!("a sequence has at most {} audio tracks", crate::sequence_tools::MAX_TRACKS)));
    }
    if need > have {
        let ids: Vec<(usize, TrackId)> = (have..need).map(|k| (k, TrackId(p.alloc_id()))).collect();
        let q = p.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        for (k, id) in ids {
            let mut t = template.clone().unwrap_or_else(|| Track::new(id, TrackKind::Audio, String::new()));
            t.id = id;
            t.name = format!("Audio {}", k + 1);
            t.items.clear();
            t.transitions.clear();
            t.locked = false;
            t.muted = false;
            t.solo = false;
            t.mixer = Default::default();
            q.audio_tracks.push(t);
        }
    }
    Ok(p.sequence(seq_id).map(|q| q.audio_tracks.iter().map(|t| t.id).collect()).unwrap_or_default())
}

/// Place a project item on the timeline (drag from Project, or Insert/Overwrite from source).
pub(crate) fn place_item(
    s: &mut Session,
    item: ItemId,
    range: TimeRange,
    at: Tick,
    vdest: Option<TrackId>,
    adest: Option<TrackId>,
    insert: bool,
    label: &str,
    audio_split: Option<(TimeRange, Tick)>,
) -> Result<Vec<ClipId>> {
    let pi = s.project.item(item).ok_or_else(|| bad(label, "no such item"))?.clone();
    // the nest toggle off: a sequence edits in as its clips (a multi-camera source stays one clip)
    if s.state.sequences_as_clips && pi.as_sequence().is_some_and(|q| q.multicam.is_none()) {
        return crate::sequence_tools::place_sequence_clips(s, item, range, at, vdest, adest, insert, label);
    }
    let has_v = pi.has_video() && vdest.is_some();
    let has_a = pi.has_audio() && adest.is_some();
    if !has_v && !has_a {
        return Err(EngineError::Other("no destination track for this media (check source patching)".into()));
    }
    let src_size = match &pi.kind {
        ItemKind::Media(m) => m.info.video.as_ref().map(|v| (v.width, v.height)),
        ItemKind::Sequence(q) => Some((q.settings.width, q.settings.height)),
        ItemKind::AdjustmentLayer { width, height, .. } => Some((*width, *height)),
        _ => None,
    }
    .unwrap_or((1920, 1080));
    // `None`: drawn in the pixels of the sequence it is placed in
    let src_par = s.project.source_par(item);
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let media = s.media.clone();
    let scaling = s.prefs.media.default_media_scaling.clone();
    let is_media = matches!(pi.kind, ItemKind::Media(_) | ItemKind::Subclip { .. });
    s.edit(label, |p, st| {
        let seq = p.sequence(seq_id).ok_or(EngineError::NoSequence)?;
        let rate = seq.settings.frame_rate;
        let frame = (seq.settings.width, seq.settings.height);
        let seq_par = seq.settings.par;
        let mut placements = Vec::new();
        let link = if has_v && has_a { Some(p.alloc_id()) } else { None };
        if let Some(vdest) = vdest.filter(|_| has_v) {
            let mut v = p.make_track_item(item, TrackKind::Video, at, range, rate).ok_or_else(|| bad(label, "bad item"))?;
            v.link = link;
            for e in &mut v.effects {
                resolve_auto_points(e, frame, src_size);
            }
            // the picture's size at its display aspect, in sequence pixels
            let shown = filmcraft_project::conformed_size(src_size, src_par.unwrap_or(seq_par), seq_par);
            if is_media && shown != (f64::from(frame.0), f64::from(frame.1)) {
                crate::settings::apply_media_scaling(&mut v, &scaling, frame, shown);
            }
            placements.push((vdest, v));
        }
        if let Some(adest) = adest.filter(|_| has_a) {
            let (arange, aat) = audio_split.unwrap_or((range, at));
            let mut a = p.make_track_item(item, TrackKind::Audio, aat, arange, rate).ok_or_else(|| bad(label, "bad item"))?;
            // Modify ▸ Audio Channels with several audio clips: one per clip, on the tracks below
            let clips: Vec<Vec<u16>> =
                p.item(item).and_then(|i| i.as_media()).and_then(|m| m.interpret.audio_channels.as_ref()).map(|m| m.clips.clone()).unwrap_or_default();
            let streams = p.resolve_media(item).map_or(1, |(_, m, _)| m.info.audio_streams.len());
            let specs = audio_placement_specs(&clips, streams);
            let first = p.sequence(seq_id).and_then(|q| q.audio_tracks.iter().position(|t| t.id == adest)).unwrap_or(0);
            // too few audio tracks below the destination: grow the sequence from the destination track
            let tracks = ensure_audio_tracks(p, seq_id, first, specs.len(), label)?;
            a.link = if link.is_none() && specs.len() > 1 { Some(p.alloc_id()) } else { link };
            for spec in specs {
                let tid = match spec.track_offset {
                    0 => &adest,
                    off => match first.checked_add(off).and_then(|i| tracks.get(i)) {
                        Some(t) => t,
                        None => continue,
                    },
                };
                let mut b = a.clone();
                b.audio_stream = spec.audio_stream;
                if spec.track_offset > 0 {
                    b.id = ClipId(p.alloc_id());
                }
                if spec.audio_stream > 0 {
                    b.source_channels.clear();
                }
                if let Some(chans) = spec.source_channels {
                    b.source_channels = chans;
                }
                placements.push((*tid, b));
            }
        }
        let snapshot = std::sync::Arc::new(p.clone());
        let snap = snapshot.clone();
        let durations = move |id: ItemId| crate::media_duration(&snapshot, &media, id);
        let starts = move |id: ItemId| crate::media_start(&snap, id);
        let min = rate.frame_duration();
        let mut next = p.next_id;
        let seq = p.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let mut ctx = edit::EditCtx { next_id: &mut next, media_duration: &durations, media_start: &starts, min_duration: min };
        let span = placements.iter().map(|x| x.1.start).min().zip(placements.iter().map(|x| x.1.end()).max());
        let ids = if insert { edit::insert(seq, placements, &mut ctx)? } else { edit::overwrite(seq, placements, &mut ctx)? };
        if insert
            && st.ripple_sequence_markers
            && let Some((a, b)) = span
        {
            crate::sequence_tools::ripple_markers(&mut seq.markers, a, b - a);
        }
        p.next_id = next;
        st.selection = ids.clone();
        Ok(ids)
    })
}

pub(crate) fn source_range(s: &Session) -> Option<(ItemId, TimeRange)> {
    let item = s.state.source_item?;
    let pi = s.project.item(item)?;
    let dur = match &pi.kind {
        ItemKind::Media(m) if matches!(m.info.kind, filmcraft_media::MediaKind::Still) => s.prefs.timeline.still_duration(s.sequence_rate()),
        _ => pi.duration(),
    };
    let (mi, mo) = match &pi.kind {
        ItemKind::Media(m) => (m.mark_in, m.mark_out),
        ItemKind::Sequence(q) => (q.mark_in, q.mark_out),
        _ => (None, None),
    };
    // a subclip shows its range of the parent's media (its clips use the parent's media time)
    if let ItemKind::Subclip { range, .. } = &pi.kind {
        return Some((item, *range));
    }
    let start = mi.unwrap_or(Tick::ZERO);
    let rate = pi.frame_rate();
    let end = mo.map(|o| o + rate.frame_duration()).unwrap_or(dur).min(if dur.0 > 0 { dur } else { Tick::MAX });
    Some((item, TimeRange::from_bounds(start, end.max(start + rate.frame_duration()))))
}

fn edit_at_source(s: &mut Session, insert: bool) -> Result<Value> {
    let (item, range) = source_range(s).ok_or_else(|| EngineError::Other("no source clip".into()))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // Three-point editing: sequence In wins over the playhead.
    let at = seq.mark_in.unwrap_or(s.playhead());
    let tg = s.targeting();
    let label = if insert { "Insert" } else { "Overwrite" };
    // split points: each channel takes its own range, offset by the difference of the In points
    let (vr, ar) = crate::sequence_tools::split_source_ranges(s, item, range);
    let base = vr.start.min(ar.start);
    let (vat, aat) = (at + (vr.start - base), at + (ar.start - base));
    let ids = if vr == ar {
        place_item(s, item, range, at, tg.video_dest, tg.audio_dest, insert, label, None)?
    } else {
        place_item(s, item, vr, vat, tg.video_dest, tg.audio_dest, insert, label, Some((ar, aat)))?
    };
    let end =
        if vr == ar { at + s.sequence_rate().snap_nearest(range.duration) } else { s.sequence_rate().snap_nearest((vat + vr.duration).max(aat + ar.duration)) };
    s.set_playhead(end);
    Ok(json!({"clips": ids.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

fn marker_list_mut(s: &mut Session) -> Result<(ItemId, Tick)> {
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    Ok((seq, s.playhead()))
}

fn build() -> Vec<CommandSpec> {
    let mut v = vec![
        // ================= File =================
        cmd!("file.newProject", "Project…", ["File", "New"], Some("Cmd+Alt+N"), r#"{"name":str}"#, always, |s, p| {
            let name = str_p(p, "name").unwrap_or("Untitled").to_string();
            s.project = std::sync::Arc::new(filmcraft_project::Project::new(&name));
            s.history = Default::default();
            s.history.limit = 200;
            s.state = crate::Session::default().state;
            if s.path.take().is_some() {
                s.previews.reset_temp();
            }
            s.revision += 1;
            s.saved_revision = s.revision;
            s.events.push(crate::Event::ProjectChanged { revision: s.revision });
            Ok(Value::Null)
        }),
        cmd!("file.openDemoProject", "Demo Project", ["File", "New"], None, "{}", always, |s, _| {
            let (p, seq) = crate::demo::demo_project(&s.media);
            s.project = std::sync::Arc::new(p);
            s.history = Default::default();
            s.history.limit = 200;
            s.state = crate::Session::default().state;
            s.state.active_sequence = Some(seq);
            s.state.open_sequences = vec![seq];
            s.state.playheads.insert(seq, FrameRate::FPS_23_976.tick_of(4 * 24 + 6));
            s.revision += 1;
            s.saved_revision = s.revision;
            s.events.push(crate::Event::ProjectChanged { revision: s.revision });
            s.events.push(crate::Event::OpenSequence(seq));
            Ok(json!({"sequence": seq.0}))
        }),
        cmd!(
            "file.newSequence",
            "Sequence…",
            ["File", "New"],
            Some("Cmd+N"),
            r#"{"name":str,"width":u32=1920,"height":u32=1080,"fps":f64=23.976,"sampleRate":u32=48000,"video":n=3,"audio":n=3,"mix":"Stereo|Mono|5.1|Adaptive"?,"trackType":"Standard|Mono|5.1|Adaptive"?,"fromItem":itemId?}"#,
            always,
            |s, p| {
                let mut st = SequenceSettings::default();
                if let Some(from) = item_p(p, "fromItem").and_then(|i| s.project.item(i)).and_then(|i| i.as_media()) {
                    st = default_seq_settings_for(from);
                }
                if let Some(w) = checked_u32_p(p, "width", "file.newSequence")? {
                    st.width = w;
                }
                if let Some(h) = checked_u32_p(p, "height", "file.newSequence")? {
                    st.height = h;
                }
                if let Some(fps) = fps_p(p, "file.newSequence")? {
                    st.frame_rate = fps;
                }
                if let Some(sr) = checked_u32_p(p, "sampleRate", "file.newSequence")? {
                    st.sample_rate = sr;
                }
                if let Some(m) = str_p(p, "mix") {
                    st.audio_master =
                        crate::mixer::channels_from(m).ok_or_else(|| bad("file.newSequence", format!("unknown mix `{m}` (Stereo, Mono, 5.1, Adaptive)")))?;
                }
                let track_type = match str_p(p, "trackType") {
                    Some(t) => Some(
                        crate::mixer::channels_from(t)
                            .ok_or_else(|| bad("file.newSequence", format!("unknown track type `{t}` (Standard, Mono, 5.1, Adaptive)")))?,
                    ),
                    None => None,
                };
                st.preset = format!("{}x{} {}", st.width, st.height, st.frame_rate.label());
                st.validate().map_err(|e| bad("file.newSequence", e))?;
                let nv = checked_u32_p(p, "video", "file.newSequence")?.unwrap_or(3);
                let na = checked_u32_p(p, "audio", "file.newSequence")?.unwrap_or(3);
                if nv > 256 || na > 256 {
                    return Err(bad("file.newSequence", "at most 256 video and 256 audio tracks are supported"));
                }
                let n = s.project.sequences().count() + 1;
                let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| format!("Sequence {n:02}"));
                let seq_label = s.prefs.labels.defaults.sequence;
                let id = s.edit("New Sequence", |pr, st2| {
                    let id = pr.new_sequence(&name, st, nv as usize, na as usize, None);
                    if let Some(it) = pr.item_mut(id) {
                        it.label = seq_label;
                    }
                    if let (Some(c), Some(q)) = (track_type, pr.sequence_mut(id)) {
                        q.audio_tracks.iter_mut().for_each(|t| t.channels = c);
                    }
                    st2.active_sequence = Some(id);
                    if !st2.open_sequences.contains(&id) {
                        st2.open_sequences.push(id);
                    }
                    Ok(id)
                })?;
                if let Some(from) = item_p(p, "fromItem") {
                    let dur = s.project.item(from).map(|i| i.duration()).unwrap_or_default();
                    let tg = s.targeting();
                    place_item(s, from, TimeRange::new(Tick::ZERO, dur), Tick::ZERO, tg.video_dest, tg.audio_dest, false, "New Sequence From Clip", None)?;
                }
                s.events.push(crate::Event::OpenSequence(id));
                Ok(json!({"sequence": id.0}))
            }
        ),
        cmd!("file.newBin", "Bin", ["File", "New"], Some("Cmd+B"), r#"{"name":str,"parent":binId?}"#, always, |s, p| {
            let n = str_p(p, "name").unwrap_or("New Bin").to_string();
            let parent = u64_p(p, "parent").map(filmcraft_project::BinId);
            let id = s.edit("New Bin", |pr, _| Ok(pr.add_bin(&n, parent)))?;
            Ok(json!({"bin": id.0}))
        }),
        cmd!("file.newAdjustmentLayer", "Adjustment Layer…", ["File", "New"], None, r#"{"seconds":f64=5}"#, always, |s, p| {
            let st = s.active_sequence().map(|q| q.settings.clone()).unwrap_or_default();
            let secs = f64_p(p, "seconds").unwrap_or(5.0);
            let id = s.edit("New Adjustment Layer", |pr, st2| {
                let id = pr.add_item(
                    "Adjustment Layer",
                    Label::Lavender,
                    ItemKind::AdjustmentLayer { width: st.width, height: st.height, rate: st.frame_rate, duration: Tick::from_seconds_f64(secs) },
                    None,
                );
                st2.project_selection = vec![id];
                Ok(id)
            })?;
            Ok(json!({"item": id.0}))
        }),
        cmd!("file.newBarsAndTone", "Bars and Tone…", ["File", "New"], None, r#"{"seconds":f64=10}"#, always, |s, p| new_generator(
            s,
            Generator::BarsAndTone,
            "Bars and Tone",
            Label::Lavender,
            p
        )),
        cmd!("file.newBlackVideo", "Black Video…", ["File", "New"], None, r#"{"seconds":f64}"#, always, |s, p| new_generator(
            s,
            Generator::BlackVideo,
            "Black Video",
            Label::Lavender,
            p
        )),
        cmd!("file.newColorMatte", "Color Matte…", ["File", "New"], None, r##"{"color":"#rrggbb","seconds":f64}"##, always, |s, p| {
            let c = str_p(p, "color").and_then(filmcraft_color::parse_hex).unwrap_or([0.1, 0.1, 0.1, 1.0]);
            new_generator(s, Generator::ColorMatte { color: c }, "Color Matte", Label::Lavender, p)
        }),
        cmd!("project.matteColor", "Color Matte Color…", [], None, r##"{"item":id?,"color":"#rrggbb"}"##, always, set_matte_color),
        cmd!("file.newCountingLeader", "Universal Counting Leader…", ["File", "New"], None, "{}", always, |s, p| new_generator(
            s,
            Generator::CountingLeader,
            "Universal Counting Leader",
            Label::Lavender,
            p
        )),
        cmd!("file.newTransparentVideo", "Transparent Video…", ["File", "New"], None, "{}", always, |s, p| new_generator(
            s,
            Generator::TransparentVideo,
            "Transparent Video",
            Label::Lavender,
            p
        )),
        cmd!(
            "file.importDemoFootage",
            "Demo Footage",
            ["File", "Import From"],
            None,
            r#"{"scene":"OceanSunset|Aurora|CityNight|Dunes|Plasma|Forest"}"#,
            always,
            |s, p| {
                let scenes: Vec<filmcraft_media::DemoScene> = match str_p(p, "scene") {
                    Some(n) => filmcraft_media::DemoScene::ALL.iter().copied().filter(|d| format!("{d:?}").eq_ignore_ascii_case(n)).collect(),
                    None => filmcraft_media::DemoScene::ALL.to_vec(),
                };
                let pool = s.media.clone();
                let ids = s.edit("Import Demo Footage", |pr, _| {
                    Ok(scenes
                        .iter()
                        .map(|sc| crate::demo::add_generator(pr, &pool, GeneratorSource::demo(*sc), sc.file_name(), Label::Iris, None))
                        .collect::<Vec<_>>())
                })?;
                Ok(json!({"items": ids.iter().map(|i| i.0).collect::<Vec<_>>()}))
            }
        ),
        // Result: `items` (the ids added), `errors` (one `path: reason` per path that failed; the
        // command itself fails only when nothing else came of it), `duplicates` (one
        // `{path, item, tier}` per path whose file the project already has: nothing is added) and
        // `relink` (one `{path, item}` per offline item a path is the media of: nothing is added,
        // Link Media is offered). See `import_duplicates` and docs/project-files.md, "Importing a
        // file twice".
        cmd!("file.import", "Import…", ["File"], Some("Cmd+I"), r#"{"paths":[str],"bin":binId?,"imageSequence":bool?}"#, always, |s, p| {
            use crate::import_duplicates::Found;
            let bin = u64_p(p, "bin").map(filmcraft_project::BinId);
            let paths: Vec<String> = match p.get("paths").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
                None => str_p(p, "path").map(|x| vec![x.to_string()]).unwrap_or_default(),
            };
            if paths.is_empty() {
                return Err(bad("file.import", "need `paths`"));
            }
            let mut ids = Vec::new();
            let mut errors = Vec::new();
            let mut sequences = Vec::new();
            let mut reports = Vec::new();
            let mut image_sequences = Vec::new();
            // Image Sequence: each path is the first frame of a numbered still sequence. With
            // Settings ▸ Media ▸ Import image sequences on, a single numbered still is detected.
            let as_sequence = p.get("imageSequence").and_then(Value::as_bool).unwrap_or(false);
            let detect = !as_sequence && s.prefs.media.import_image_sequences && paths.len() == 1;
            // A file the project already has is not imported again (#356, `import_duplicates`):
            // the project's files as the three tiers look them up, asked of the host once.
            let mut known = crate::import_duplicates::Known::of(s);
            let mut duplicates = Vec::new();
            let mut relink = Vec::new();
            for path in paths {
                let sequence = as_sequence || (detect && detect_image_sequence(s, &path));
                match known.find(s, &path, sequence) {
                    Some(Found::Duplicate { item, tier }) => {
                        duplicates.push(json!({"path": path, "item": item.0, "tier": tier}));
                        continue;
                    }
                    Some(Found::Relink(items)) => {
                        relink.extend(items.iter().map(|i| json!({"path": path, "item": i.0})));
                        continue;
                    }
                    None => {}
                }
                let added = if sequence {
                    match import_image_sequence(s, &path, bin) {
                        Ok((id, frames, missing)) => {
                            image_sequences.push(json!({"item": id.0, "frames": frames, "missing": missing}));
                            Some(id)
                        }
                        Err(e) => {
                            errors.push(format!("{path}: {e}"));
                            None
                        }
                    }
                } else {
                    // Media files through the host's reader (no whole-file read) when it has one.
                    let streamed = filmcraft_media::is_importable(std::path::Path::new(&path)) && s.services.reader(&path).is_some();
                    let read = if streamed { Ok(Vec::new()) } else { s.services.read_file(&path) };
                    let media = match read {
                        Ok(_) if streamed => import_streamed(s, &path, bin).map(Some),
                        Ok(b) => {
                            if let Some(fmt) = crate::captions::detect(&path, &b) {
                                crate::captions::import(s, &path, &b, fmt, None).map(|r| {
                                    reports.push(r);
                                    None
                                })
                            } else if let Some(fmt) = crate::interchange::detect(&path, &b) {
                                crate::interchange::import(s, &path, &b, fmt).map(|r| {
                                    sequences.extend(r["sequences"].as_array().cloned().unwrap_or_default());
                                    reports.push(r);
                                    None
                                })
                            } else {
                                import_bytes(s, &path, b.into(), bin).map(Some)
                            }
                        }
                        Err(e) => Err(EngineError::Other(e.to_string())),
                    };
                    media.unwrap_or_else(|e| {
                        errors.push(format!("{path}: {e}"));
                        None
                    })
                };
                if let Some(id) = added {
                    ids.push(id.0);
                    // the same file again later in `paths` (or through a link) is a duplicate too
                    known.add(&*s.services, &path, sequence, id);
                }
            }
            // every path failed: an error. A file the project knows is a result, not a failure.
            if ids.is_empty() && sequences.is_empty() && reports.is_empty() && duplicates.is_empty() && relink.is_empty() && !errors.is_empty() {
                return Err(EngineError::Other(errors.join("; ")));
            }
            // Project Settings ▸ Ingest: copy / transcode / create proxies
            // (image sequences are many files: ingest copies and transcodes single files only)
            let item_ids: Vec<ItemId> = ids.iter().filter(|i| !image_sequences.iter().any(|q| q["item"].as_u64() == Some(**i))).map(|i| ItemId(*i)).collect();
            // Settings ▸ Media Analysis & Transcription ▸ Automatically transcribe clips
            let ma = &s.prefs.media_analysis;
            if ma.auto_transcribe && ma.auto_transcribe_scope == "allImported" && !ids.is_empty() {
                let audio: Vec<u64> = ids.iter().copied().filter(|i| s.project.item(ItemId(*i)).is_some_and(|it| it.has_audio())).collect();
                if !audio.is_empty() {
                    // without speech-to-text the setting can't act: say so instead of importing
                    // silently untranscribed (#89)
                    let r = crate::transcript::can_transcribe(s)
                        .map_err(EngineError::Other)
                        .and_then(|()| s.execute("transcript.generate", json!({"items": audio})));
                    if let Err(e) = r {
                        errors.push(format!("transcription: {e}"));
                    }
                }
            }
            let ingest = match crate::proxies::ingest(s, &item_ids) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(format!("ingest: {e}"));
                    Value::Null
                }
            };
            let mut out = if !ingest.is_null() {
                json!({"items": ids, "sequences": sequences, "documents": reports, "errors": errors, "ingest": ingest})
            } else if reports.is_empty() {
                json!({"items": ids, "errors": errors})
            } else {
                json!({"items": ids, "sequences": sequences, "documents": reports, "errors": errors})
            };
            if !image_sequences.is_empty() {
                out["imageSequences"] = json!(image_sequences);
            }
            // Newly imported files may resolve paths the project already listed as offline
            // (issue #110): rebuild s.offline.missing so the "Media missing" badge clears, and drop
            // the cached slate of every item that came back so the monitors show the file again.
            let was_missing = s.offline.missing.clone();
            crate::relink::refresh(s);
            for item in was_missing.iter().filter(|i| !s.offline.missing.contains(i)) {
                s.media.remove(*item);
            }
            let names = |rows: &[Value]| {
                let names: Vec<&str> = rows.iter().take(3).filter_map(|d| d["path"].as_str()).map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p)).collect();
                let more = rows.len().saturating_sub(names.len());
                format!("{}{}", names.join(", "), if more > 0 { format!(" and {more} more") } else { String::new() })
            };
            if !duplicates.is_empty() {
                s.toast(format!("Already in the project: {}", names(&duplicates)));
            }
            // the media of an offline clip: offer to link it (the Link Media dialog) instead of
            // adding the file beside the clip that wants it
            if !relink.is_empty() {
                s.toast(format!("Media of offline clips, not imported: {}. Use Link Media to reconnect.", names(&relink)));
                s.offline.prompt = true;
            }
            out["duplicates"] = json!(duplicates);
            out["relink"] = json!(relink);
            Ok(out)
        }),
        cmd!(
            "file.exportInterchange",
            "Export Interchange",
            [],
            None,
            r#"{"format":"edl|xml|fcpxml|otio|aaf|omf"=xml,"path":str,"sequence":id?}"#,
            has_seq,
            crate::interchange::export
        ),
        cmd!("file.exportEdl", "EDL…", ["File", "Export"], None, r#"{"path":str}"#, has_seq, |s, p| {
            let mut q = p.clone();
            q["format"] = json!("edl");
            crate::interchange::export(s, &q)
        }),
        cmd!("file.exportFcp7Xml", "Final Cut Pro XML…", ["File", "Export"], None, r#"{"path":str}"#, has_seq, |s, p| {
            let mut q = p.clone();
            q["format"] = json!("xml");
            crate::interchange::export(s, &q)
        }),
        cmd!("file.exportFcpxml", "FCPXML…", ["File", "Export"], None, r#"{"path":str}"#, has_seq, |s, p| {
            let mut q = p.clone();
            q["format"] = json!("fcpxml");
            crate::interchange::export(s, &q)
        }),
        cmd!("file.exportOtio", "OpenTimelineIO…", ["File", "Export"], None, r#"{"path":str}"#, has_seq, |s, p| {
            let mut q = p.clone();
            q["format"] = json!("otio");
            crate::interchange::export(s, &q)
        }),
        cmd!(
            "file.exportAaf",
            "AAF…",
            ["File", "Export"],
            None,
            r#"{"path":str,"sequence":id?,"mixdownVideo":bool?,"mixdownFormat":"mov|mxf"?,"breakoutToMono":bool?,"audio":"embedded|separate|linked"?,"audioFormat":"wav|aiff|mxf"?,"sampleRate":int?,"bitDepth":"16|24"?,"trimAudio":bool?,"handles":frames?,"renderAudioEffects":bool?,"smallSectors":bool?}"#,
            has_seq,
            |s, p| crate::aaf_omf::export_aaf(s, p)
        ),
        cmd!(
            "file.exportOmf",
            "OMF…",
            ["File", "Export"],
            None,
            r#"{"path":str,"sequence":id?,"title":str?,"audio":"embedded|separate"?,"audioFormat":"wav|aiff"?,"sampleRate":int?,"bitDepth":"16|24"?,"trimAudio":bool?,"handles":frames?,"renderAudioEffects":bool?,"breakoutToMono":bool?}"#,
            has_seq,
            |s, p| crate::aaf_omf::export_omf(s, p)
        ),
        cmd!("file.importAaf", "Import AAF…", [], None, r#"{"path":str}"#, always, |s, p| crate::aaf_omf::import_document(s, p, "file.importAaf")),
        cmd!("file.save", "Save", ["File"], Some("Cmd+S"), r#"{"path":str?}"#, always, |s, p| {
            let path = str_p(p, "path").map(str::to_string).or_else(|| s.path.clone()).ok_or_else(|| bad("file.save", "no path (use Save As)"))?;
            write_project(s, &path, true)
        }),
        cmd!("file.saveAs", "Save As…", ["File"], Some("Cmd+Shift+S"), r#"{"path":str}"#, always, |s, p| {
            let path = str_p(p, "path").ok_or_else(|| bad("file.saveAs", "need `path`"))?.to_string();
            write_project(s, &path, true)
        }),
        cmd!("file.saveCopy", "Save a Copy…", ["File"], Some("Cmd+Alt+S"), r#"{"path":str}"#, always, |s, p| {
            let path = str_p(p, "path").ok_or_else(|| bad("file.saveCopy", "need `path`"))?.to_string();
            write_project(s, &path, false)
        }),
        cmd!("file.revert", "Revert", ["File"], None, "{}", has_saved_path, |s, _| {
            let path = s.path.clone().ok_or_else(|| bad("file.revert", "project was never saved"))?;
            let keep = s.state.active_sequence;
            let r = open_project(s, &path)?;
            if let Some(k) = keep.filter(|k| s.project.sequence(*k).is_some()) {
                s.state.active_sequence = Some(k);
                s.state.open_sequences = vec![k];
            }
            Ok(r)
        }),
        cmd!("file.open", "Open Project…", ["File"], Some("Cmd+O"), r#"{"path":str}"#, always, |s, p| {
            let path = str_p(p, "path").ok_or_else(|| bad("file.open", "need `path`"))?.to_string();
            open_project(s, &path)
        }),
        cmd!("file.recover", "Recover Unsaved Changes…", ["File"], None, r#"{"id":str?}"#, has_recovery, |s, p| recover(s, str_p(p, "id"))),
        cmd!("file.discardRecovery", "Discard Unsaved Changes", [], None, r#"{"id":str?,"all":bool?}"#, has_recovery, |s, p| {
            let all = bool_p(p, "all").unwrap_or(false);
            let id = str_p(p, "id").map(str::to_string);
            let Some(per) = s.persistence.as_mut() else { return Ok(json!({"discarded": 0})) };
            let mut n = 0;
            let mut keep = Vec::new();
            for (i, c) in std::mem::take(&mut per.candidates).into_iter().enumerate() {
                let hit = all || id.as_deref().map_or(i == 0, |want| want == c.id);
                if hit {
                    crate::autosave::discard_candidate(&c).map_err(|e| EngineError::Other(e.to_string()))?;
                    n += 1;
                } else {
                    keep.push(c);
                }
            }
            per.candidates = keep;
            if n == 0 {
                return Err(bad("file.discardRecovery", "no such recovery session"));
            }
            Ok(json!({"discarded": n}))
        }),
        query!("file.recoveryList", "List Recoverable Sessions", "{}", |s, _| {
            Ok(s.persistence.as_ref().map(|p| p.candidates_json()).unwrap_or_else(|| json!([])))
        }),
        cmd!("file.autoSaveNow", "Auto Save Now", [], None, "{}", always, |s, _| {
            let per = s.persistence.as_ref().ok_or_else(|| EngineError::Other("auto-save is not running in this session".into()))?;
            let r = per.auto_save_now().map_err(EngineError::Other)?;
            s.poll_persistence();
            Ok(json!({"path": r.map(|p| p.to_string_lossy().into_owned())}))
        }),
        query!("file.autoSaveStatus", "Auto Save Status", "{}", |s, _| {
            let Some(per) = s.persistence.as_ref() else { return Ok(json!({"running": false, "prefs": s.prefs.auto_save})) };
            Ok(json!({
                "running": true,
                "prefs": s.prefs.auto_save,
                "dataDir": per.data_dir,
                "sessionDir": per.session_dir,
                "autoSaveDir": auto_save_dir(s),
                "status": per.status,
                "lastAutoSaveAt": per.status.last_auto_save_unix.map(|u| per.local_time(u)),
                "lastJournalAt": per.status.last_journal_unix.map(|u| per.local_time(u)),
                "recoverable": per.candidates.len(),
            }))
        }),
        query!("file.listAutoSaves", "Browse Auto-Saves", "{}", |s, _| {
            let dir = auto_save_dir(s);
            let files = filmcraft_format::autosave::list_auto_saves(&dir, &auto_save_name(s));
            Ok(json!({"dir": dir, "files": files.iter().rev().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>()}))
        }),
        query!("prefs.get", "Get Preferences", r#"{"key":str?}"#, |s, p| match str_p(p, "key") {
            Some(k) => s.prefs.get(k).ok_or_else(|| bad("prefs.get", format!("unknown preference `{k}`"))),
            None => Ok(json!({"values": s.prefs.to_value(), "keys": s.prefs.keys()})),
        }),
        cmd!("prefs.set", "Set Preferences", [], None, r#"{"key":str,"value":any}|{"values":{key:value}}"#, always, |s, p| {
            let mut next = s.prefs.clone();
            if let Some(k) = str_p(p, "key") {
                next.set(k, p.get("value").cloned().ok_or_else(|| bad("prefs.set", "need `value`"))?).map_err(|e| bad("prefs.set", e))?;
            }
            if let Some(m) = p.get("values").and_then(Value::as_object) {
                for (k, v) in m {
                    next.set(k, v.clone()).map_err(|e| bad("prefs.set", e))?;
                }
            }
            s.set_prefs(next).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
            Ok(s.prefs.to_value())
        }),
        cmd!("prefs.reset", "Reset Preferences", [], None, r#"{"category":str?}"#, always, |s, p| {
            let next = match str_p(p, "category") {
                Some(c) => {
                    let mut n = s.prefs.clone();
                    n.reset_category(c).map_err(|e| bad("prefs.reset", e))?;
                    n
                }
                None => Default::default(),
            };
            s.set_prefs(next).map_err(|e| EngineError::Other(format!("saving preferences: {e}")))?;
            Ok(s.prefs.to_value())
        }),
        cmd!(
            "file.exportMedia",
            "Media…",
            ["File", "Export"],
            None,
            r#"{"path":str,"preset":str?,"settings":ExportSettings?,"format":"h264|hevc|prores|dnxhr|apv|mjpeg|mxf-op1a|mxf-opatom|png|tiff|bmp|gif|wav|aiff"?,"width":u32?,"height":u32?,"fps":f64?,"bitrateKbps":u32?,"bitrateMode":"cbr|vbr1Pass|vbr2Pass"?,"hardwareEncoding":"off|auto"?,"scale":f32=1,"audio":bool=true,"quality":0..100,"burnCaptions":bool=false,"alpha":bool=false,"captionSidecar":"srt|vtt"?,"loudnessLufs":f64?,"proresProfile":"proxy|lt|standard|hq"?,"dnxProfile":"lb|sq|hq|hqx"?,"apvProfile":"422-10|422-12|444-10|444-12"?,"mxfVideoCodec":"dnxhr|proRes|h264"?,"sequence":id?,"range":"entire|inOut|workArea|custom"?,"startSeconds":f64?,"endSeconds":f64?,"wait":bool=false}"#,
            has_seq,
            crate::export_tools::export_media
        ),
        query!("jobs.list", "List Jobs", "{}", |s, _| Ok(Value::Array(s.jobs.iter().map(crate::Job::to_json).collect()))),
        query!("perf.stats", "Performance Statistics", "{}", |s, _| Ok(crate::perf::stats(s))),
        cmd!("jobs.cancel", "Cancel Job", [], None, r#"{"job":id}"#, always, |s, p| {
            let id = u64_p(p, "job").ok_or_else(|| bad("jobs.cancel", "need `job`"))?;
            let j = s.jobs.iter().find(|j| j.id == id).ok_or_else(|| bad("jobs.cancel", "no such job"))?;
            j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(Value::Null)
        }),
        // ================= Edit =================
        cmd!("edit.undo", "Undo", ["Edit"], Some("Cmd+Z"), "{}", can_undo, |s, _| Ok(json!({"undone": s.undo()}))),
        cmd!("edit.redo", "Redo", ["Edit"], Some("Cmd+Shift+Z"), "{}", can_redo, |s, _| Ok(json!({"redone": s.redo()}))),
        cmd!("edit.cut", "Cut", ["Edit"], Some("Cmd+X"), "{}", has_selection, |s, _| {
            copy_selection(s);
            let sel = with_links(s, &s.state.selection.clone());
            s.edit_sequence("Cut", |q, _, st| {
                edit::delete_items(q, &sel);
                st.selection.clear();
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("edit.copy", "Copy", ["Edit"], Some("Cmd+C"), "{}", has_selection, |s, _| {
            copy_selection(s);
            Ok(json!({"copied": s.state.clipboard.len()}))
        }),
        cmd!("edit.paste", "Paste", ["Edit"], Some("Cmd+V"), "{}", has_clipboard, |s, _| paste(s, false)),
        cmd!("edit.pasteInsert", "Paste Insert", ["Edit"], Some("Cmd+Shift+V"), "{}", has_clipboard, |s, _| paste(s, true)),
        cmd!("edit.clear", "Clear", ["Edit"], Some("Backspace"), r#"{"clips":[id]?}"#, has_any_selection, |s, p| {
            if p.get("clips").is_none() && p.get("clip").is_none() && s.state.selection.is_empty() {
                // a selected transition (clicked in the Timeline) goes, as a clip would
                if !s.state.transition_selection.is_empty() {
                    let ids = s.state.transition_selection.clone();
                    return remove_transitions(s, &ids, "Clear");
                }
                let caps = s.state.caption_selection.clone();
                return crate::captions::delete(s, &caps, false);
            }
            let sel = with_links(s, &clips_p(s, p));
            s.edit_sequence("Clear", |q, _, st| {
                edit::delete_items(q, &sel);
                st.selection.clear();
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("edit.rippleDelete", "Ripple Delete", ["Edit"], Some("Shift+Delete"), r#"{"clips":[id]?}"#, has_clip_or_caption_selection, |s, p| {
            if p.get("clips").is_none() && p.get("clip").is_none() && s.state.selection.is_empty() {
                let caps = s.state.caption_selection.clone();
                return crate::captions::delete(s, &caps, true);
            }
            let sel = with_links(s, &clips_p(s, p));
            s.edit_sequence("Ripple Delete", |q, _, st| {
                let spans = edit::ripple_delete_items(q, &sel)?;
                if st.ripple_sequence_markers {
                    for r in spans.iter().rev() {
                        crate::sequence_tools::ripple_markers(&mut q.markers, r.end(), -r.duration);
                    }
                }
                st.selection.clear();
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("edit.selectAll", "Select All", ["Edit"], Some("Cmd+A"), "{}", has_seq, |s, _| {
            let all: Vec<ClipId> = s.active_sequence().map(|q| q.all_tracks().flat_map(|t| t.items.iter().map(|i| i.id)).collect()).unwrap_or_default();
            s.state.selection = all;
            Ok(json!({"selected": s.state.selection.len()}))
        }),
        cmd!("edit.deselectAll", "Deselect All", ["Edit"], Some("Cmd+Shift+A"), "{}", always, |s, _| {
            s.state.selection.clear();
            s.state.caption_selection.clear();
            Ok(Value::Null)
        }),
        cmd!("edit.duplicate", "Duplicate", ["Edit"], Some("Cmd+Shift+/"), "{}", has_project_selection, |s, _| {
            let sel = s.state.project_selection.clone();
            let ids = s.edit("Duplicate", |p, st| {
                let mut out = Vec::new();
                for id in sel {
                    if let Some(it) = p.item(id).cloned() {
                        let nid = p.add_item(&format!("{} Copy", it.name), it.label, it.kind.clone(), None);
                        out.push(nid);
                    }
                }
                st.project_selection = out.clone();
                Ok(out)
            })?;
            Ok(json!({"items": ids.iter().map(|i| i.0).collect::<Vec<_>>()}))
        }),
        cmd!("edit.label", "Label", [], None, r#"{"label":"Violet|Iris|…"}"#, always, |s, p| {
            let l = str_p(p, "label").and_then(Label::from_name).ok_or_else(|| bad("edit.label", "unknown label"))?;
            let clips = s.state.selection.clone();
            let items = s.state.project_selection.clone();
            let seq = s.state.active_sequence;
            s.edit("Label", |pr, _| {
                for i in &items {
                    if let Some(it) = pr.item_mut(*i) {
                        it.label = l;
                    }
                }
                if let Some(q) = seq.and_then(|q| pr.sequence_mut(q)) {
                    for c in &clips {
                        if let Some((_, it)) = q.find_item_mut(*c) {
                            it.label = l;
                        }
                    }
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        // ================= Clip =================
        cmd!("clip.rename", "Rename…", ["Clip"], None, r#"{"clip":id?,"item":id?,"name":str}"#, always, |s, p| {
            let name = str_p(p, "name").ok_or_else(|| bad("clip.rename", "need `name`"))?.to_string();
            let item = item_p(p, "item");
            let clip = clip_p(p, "clip").or_else(|| s.state.selection.first().copied());
            let seq = s.state.active_sequence;
            s.edit("Rename", |pr, _| {
                if let Some(i) = item {
                    pr.item_mut(i).ok_or_else(|| bad("clip.rename", "no such item"))?.name = name;
                } else if let (Some(c), Some(q)) = (clip, seq) {
                    pr.sequence_mut(q).and_then(|q| q.find_item_mut(c)).ok_or_else(|| bad("clip.rename", "no such clip"))?.1.name = name;
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "clip.speedDuration",
            "Speed/Duration…",
            ["Clip"],
            Some("Cmd+R"),
            r#"{"clips":[id]?,"speed":percent=100,"reverse":bool,"ripple":bool,"interpolation":"frameSampling|frameBlending|opticalFlow"?}"#,
            has_selection,
            |s, p| {
                let speed = f64_p(p, "speed").unwrap_or(100.0) / 100.0;
                let reverse = bool_p(p, "reverse").unwrap_or(false);
                let ripple = bool_p(p, "ripple").unwrap_or(false);
                let interp = match str_p(p, "interpolation") {
                    Some(m) => Some(filmcraft_project::TimeInterpolation::from_name(m).ok_or_else(|| bad("clip.speedDuration", "unknown interpolation"))?),
                    None => None,
                };
                let sel = with_links(s, &clips_p(s, p));
                s.edit_sequence("Speed/Duration", |q, ctx, _| {
                    // a clip and its linked partners on other tracks change as one edit, so a ripple
                    // moves the later clips once
                    let place = |q: &filmcraft_project::Sequence, c: ClipId| q.find_item(c).map(|(t, i)| (t, i.link));
                    let mut groups: Vec<Vec<ClipId>> = Vec::new();
                    for c in &sel {
                        // a clip that is not there goes on alone, so the edit fails with "no such item"
                        let Some((track, link)) = place(q, *c) else {
                            groups.push(vec![*c]);
                            continue;
                        };
                        let joins = |g: &Vec<ClipId>| link.is_some() && g.iter().all(|o| place(q, *o).is_some_and(|(t, l)| l == link && t != track));
                        match groups.iter_mut().find(|g| joins(g)) {
                            Some(g) => g.push(*c),
                            None => groups.push(vec![*c]),
                        }
                    }
                    for group in &groups {
                        edit::set_speed_group(q, group, speed, reverse, ripple, ctx)?;
                    }
                    for c in &sel {
                        if let (Some(m), Some((_, it))) = (interp, q.find_item_mut(*c)) {
                            it.time_interpolation = m;
                        }
                    }
                    Ok(())
                })?;
                Ok(Value::Null)
            }
        ),
        // Each clip flips on its own, as in Premiere (#484): with one enabled and one disabled clip
        // selected, Enable swaps them rather than first making both the same.
        cmd!("clip.enable", "Enable", ["Clip"], Some("Shift+E"), r#"{"clips":[id]?}"#, has_selection, |s, p| {
            let mut sel = with_links(s, &clips_p(s, p));
            // a clip named twice must still flip once
            sel.sort();
            sel.dedup();
            s.edit_sequence("Enable", |q, _, _| {
                for c in &sel {
                    if let Some((_, i)) = q.find_item_mut(*c) {
                        i.enabled = !i.enabled;
                    }
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("clip.link", "Link", ["Clip"], Some("Cmd+L"), r#"{"clips":[id]?}"#, has_selection, |s, p| {
            let sel = clips_p(s, p);
            let linked = sel.iter().all(|c| s.active_sequence().and_then(|q| q.find_item(*c)).is_some_and(|(_, i)| i.link.is_some()));
            s.edit_sequence(if linked { "Unlink" } else { "Link" }, |q, ctx, _| {
                if linked {
                    for c in &sel {
                        if let Some((_, i)) = q.find_item_mut(*c) {
                            i.link = None;
                        }
                    }
                } else {
                    // each selected clip with its track kind, worked out once
                    type Spec = (ClipId, filmcraft_project::ItemId, filmcraft_time::TimeRange);
                    let (mut v_clips, mut audio_specs): (Vec<Spec>, Vec<Spec>) = (Vec::new(), Vec::new());
                    for c in &sel {
                        if let Some((tid, i)) = q.find_item(*c) {
                            if q.video_tracks.iter().any(|t| t.id == tid) {
                                v_clips.push((*c, i.item, i.range()));
                            } else {
                                audio_specs.push((*c, i.item, i.range()));
                            }
                        }
                    }
                    if !audio_specs.is_empty() && v_clips.len() > 1 {
                        let mut linked_audios = std::collections::HashSet::new();
                        for (vid, item_id, vrange) in &v_clips {
                            // Pair with overlapping audio clips from the same item, or overlapping audio clips if no same-item audio
                            let matching: Vec<ClipId> = audio_specs
                                .iter()
                                .filter(|(aid, aitem, arange)| !linked_audios.contains(aid) && aitem == item_id && arange.overlaps(vrange))
                                .map(|(aid, _, _)| *aid)
                                .collect();
                            let to_link = if !matching.is_empty() {
                                matching
                            } else {
                                audio_specs
                                    .iter()
                                    .filter(|(aid, _, arange)| !linked_audios.contains(aid) && arange.overlaps(vrange))
                                    .map(|(aid, _, _)| *aid)
                                    .collect()
                            };
                            // a video clip with no audio partner stays unlinked: a one-member link would read as "linked"
                            let l = (!to_link.is_empty()).then(|| ctx.alloc());
                            if let Some((_, vi)) = q.find_item_mut(*vid) {
                                vi.link = l;
                            }
                            for aid in to_link {
                                linked_audios.insert(aid);
                                if let Some((_, ai)) = q.find_item_mut(aid) {
                                    ai.link = l;
                                }
                            }
                        }
                        // selected audio clips with no overlapping video stay unlinked
                        for (aid, _, _) in audio_specs {
                            if !linked_audios.contains(&aid)
                                && let Some((_, ai)) = q.find_item_mut(aid)
                            {
                                ai.link = None;
                            }
                        }
                    } else {
                        let l = ctx.alloc();
                        for c in &sel {
                            if let Some((_, i)) = q.find_item_mut(*c) {
                                i.link = Some(l);
                            }
                        }
                    }
                }
                Ok(())
            })?;
            Ok(json!({"linked": !linked}))
        }),
        cmd!("clip.group", "Group", ["Clip"], Some("Cmd+G"), "{}", has_selection, |s, _| {
            let sel = s.state.selection.clone();
            s.edit_sequence("Group", |q, ctx, _| {
                let g = ctx.alloc();
                for c in &sel {
                    if let Some((_, i)) = q.find_item_mut(*c) {
                        i.group = Some(g);
                    }
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("clip.ungroup", "Ungroup", ["Clip"], Some("Cmd+Shift+G"), "{}", has_selection, |s, _| {
            let sel = s.state.selection.clone();
            s.edit_sequence("Ungroup", |q, _, _| {
                for c in &sel {
                    if let Some((_, i)) = q.find_item_mut(*c) {
                        i.group = None;
                    }
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("clip.scaleToFrameSize", "Scale to Frame Size", ["Clip", "Video Options"], None, "{}", has_selection, |s, _| {
            let sel = s.state.selection.clone();
            s.edit_sequence("Scale to Frame Size", |q, _, _| {
                for c in &sel {
                    if let Some((_, i)) = q.find_item_mut(*c) {
                        i.scale_to_frame = !i.scale_to_frame;
                    }
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "clip.audioGain",
            "Audio Gain…",
            ["Clip", "Audio Options"],
            Some("G"),
            r#"{"clips":[id]?,"mode":"set|adjust|normalizeMax|normalizeAll"?,"db":f64,"relative":bool?}"#,
            has_selection,
            |s, p| crate::mixer::audio_gain(s, p)
        ),
        query!("clip.audioPeak", "Audio Clip Peak Amplitude", r#"{"clips":[id]?}"#, |s, p| crate::mixer::audio_peak(s, p)),
        cmd!("clip.frameHold", "Add Frame Hold", ["Clip", "Video Options"], None, r#"{"clips":[id]?,"time":ticks?}"#, has_seq, |s, p| {
            crate::clip_ops::add_frame_hold(s, p)
        }),
        cmd!("clip.nest", "Nest…", ["Clip"], None, r#"{"name":str}"#, has_selection, |s, p| nest(s, p)),
        // Reveal in Project (clip context menu) and Reveal Sequence in Project (Timeline tab menu):
        // select the item in the Project panel and show it there
        cmd!("clip.revealInProject", "Reveal in Project", [], None, r#"{"clip":id?}"#, has_seq, |s, p| {
            let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let t = s.playhead();
            let tg = s.targeting().targeted;
            // the named or first selected clip, else the topmost clip under the playhead on a targeted track
            let item = clips_p(s, p)
                .iter()
                .find_map(|c| seq.find_item(*c).map(|(_, i)| i.item))
                .or_else(|| {
                    seq.video_tracks.iter().rev().chain(seq.audio_tracks.iter()).filter(|tr| tg.contains(&tr.id)).find_map(|tr| tr.item_at(t).map(|i| i.item))
                })
                .ok_or_else(|| EngineError::Other("select a clip to reveal".into()))?;
            reveal_in_project(s, item)
        }),
        cmd!("sequence.revealInProject", "Reveal Sequence in Project", [], None, r#"{"item":id?}"#, has_seq, |s, p| {
            let item = item_p(p, "item").or(s.state.active_sequence).ok_or(EngineError::NoSequence)?;
            reveal_in_project(s, item)
        }),
        // ================= Sequence =================
        cmd!("sequence.open", "Open in Timeline", [], None, r#"{"item":id}"#, always, |s, p| {
            let id = item_p(p, "item").ok_or_else(|| bad("sequence.open", "need `item`"))?;
            s.project.sequence(id).ok_or_else(|| bad("sequence.open", "not a sequence"))?;
            s.state.active_sequence = Some(id);
            if !s.state.open_sequences.contains(&id) {
                s.state.open_sequences.push(id);
            }
            s.state.selection.clear();
            s.events.push(crate::Event::OpenSequence(id));
            Ok(Value::Null)
        }),
        cmd!("sequence.close", "Close Sequence", [], None, r#"{"item":id?}"#, has_seq, |s, p| {
            let id = item_p(p, "item").or(s.state.active_sequence).ok_or(EngineError::NoSequence)?;
            s.state.open_sequences.retain(|x| *x != id);
            if s.state.active_sequence == Some(id) {
                s.state.active_sequence = s.state.open_sequences.last().copied();
            }
            Ok(Value::Null)
        }),
        cmd!("sequence.closeOthers", "Close Other Timeline Panels", [], None, r#"{"item":id?}"#, has_seq, |s, p| {
            let id = item_p(p, "item").or(s.state.active_sequence).ok_or(EngineError::NoSequence)?;
            if !s.state.open_sequences.contains(&id) {
                return Err(bad("sequence.closeOthers", "that sequence is not open"));
            }
            let closed = s.state.open_sequences.len().saturating_sub(1);
            s.state.open_sequences = vec![id];
            if s.state.active_sequence != Some(id) {
                s.state.active_sequence = Some(id);
                s.state.selection.clear();
                s.events.push(crate::Event::OpenSequence(id));
            }
            Ok(json!({"closed": closed}))
        }),
        cmd!("sequence.moveTab", "Move Sequence Tab", [], None, r#"{"item":id?,"index":int}"#, has_seq, |s, p| {
            let id = item_p(p, "item").or(s.state.active_sequence).ok_or(EngineError::NoSequence)?;
            let to = u64_p(p, "index").ok_or_else(|| bad("sequence.moveTab", "need `index`"))?;
            let from = s.state.open_sequences.iter().position(|x| *x == id).ok_or_else(|| bad("sequence.moveTab", "that sequence is not open"))?;
            // past the end is the end
            let to = usize::try_from(to).unwrap_or(usize::MAX).min(s.state.open_sequences.len().saturating_sub(1));
            let tab = s.state.open_sequences.remove(from);
            s.state.open_sequences.insert(to, tab);
            Ok(json!({"index": to}))
        }),
        cmd!("sequence.addEdit", "Add Edit", ["Sequence"], Some("Cmd+K"), r#"{"time":ticks?}"#, has_seq, |s, p| {
            let t = time_p(s, p, "").unwrap_or(s.playhead());
            // Like Premiere: selected clips under the playhead are cut, and only they (#164);
            // with none, the targeted tracks are.
            let sel = s.state.selection.clone();
            let selected_here =
                s.active_sequence().is_some_and(|q| sel.iter().any(|c| q.find_item(*c).is_some_and(|(_, it)| it.start < t && t < it.start + it.duration)));
            let n = if selected_here {
                s.edit_sequence("Add Edit", |q, ctx, _| Ok(edit::razor_items(q, &sel, t, ctx)))?
            } else {
                let tg = s.targeting().targeted;
                s.edit_sequence("Add Edit", |q, ctx, _| Ok(edit::razor(q, &tg, t, ctx)))?
            };
            Ok(json!({"cuts": n.len()}))
        }),
        cmd!("sequence.addEditAllTracks", "Add Edit to All Tracks", ["Sequence"], Some("Cmd+Shift+K"), r#"{"time":ticks?}"#, has_seq, |s, p| {
            let t = time_p(s, p, "").unwrap_or(s.playhead());
            let n = s.edit_sequence("Add Edit to All Tracks", |q, ctx, _| {
                let mut n = edit::razor(q, &[], t, ctx);
                n.extend(edit::captions::split_captions_at(q, &[], t, ctx));
                Ok(n)
            })?;
            Ok(json!({"cuts": n.len()}))
        }),
        cmd!("sequence.lift", "Lift", ["Sequence"], Some(";"), "{}", has_in_out, |s, _| {
            let r = in_out_range(s)?;
            let tg = s.targeting().targeted;
            s.edit_sequence("Lift", |q, ctx, _| Ok(edit::lift(q, &tg, r, ctx)))?;
            Ok(Value::Null)
        }),
        cmd!("sequence.extract", "Extract", ["Sequence"], Some("'"), "{}", has_in_out, |s, _| {
            let r = in_out_range(s)?;
            let tg = s.targeting().targeted;
            s.edit_sequence("Extract", |q, ctx, st| {
                edit::extract(q, &tg, r, ctx);
                if st.ripple_sequence_markers {
                    crate::sequence_tools::ripple_markers(&mut q.markers, r.end(), -r.duration);
                }
                q.mark_in = None;
                q.mark_out = None;
                q.split = Default::default();
                Ok(())
            })?;
            s.set_playhead(r.start);
            Ok(Value::Null)
        }),
        cmd!(
            "sequence.applyVideoTransition",
            "Apply Video Transition",
            ["Sequence"],
            Some("Cmd+D"),
            r#"{"clip":id?,"effect":"cross_dissolve|Cross Dissolve|…"?,"frames":i64?,"edge":"in"|"out"?,"params":{param:value}?,"reverse":bool?}"#,
            has_seq,
            |s, p| apply_transition(s, p, TrackKind::Video)
        ),
        cmd!(
            "sequence.applyAudioTransition",
            "Apply Audio Transition",
            ["Sequence"],
            Some("Cmd+Shift+D"),
            r#"{"clip":id?,"effect":str?,"frames":i64?}"#,
            has_seq,
            |s, p| apply_transition(s, p, TrackKind::Audio)
        ),
        cmd!(
            "sequence.setTransition",
            "Edit Transition Settings",
            [],
            None,
            r#"{"transition":id,"params":{param:value}?,"reverse":bool?,"reset":bool?,"duration":ticks?|"frames":n?,"align":"center"|"start"|"end"?|"start":ticks?,"merge":bool?,"begin":bool?}"#,
            has_seq,
            set_transition
        ),
        cmd!("sequence.removeTransition", "Clear Transition", [], None, r#"{"transitions":[id]?}"#, has_seq, |s, p| {
            let ids: Vec<TransitionId> = match p.get("transitions").or_else(|| p.get("transition")) {
                Some(v) => transition_ids_p(v).ok_or_else(|| bad("sequence.removeTransition", "`transitions` is a list of transition ids"))?,
                None => s.state.transition_selection.clone(),
            };
            if ids.is_empty() {
                return Err(bad("sequence.removeTransition", "no transition given or selected"));
            }
            remove_transitions(s, &ids, "Clear Transition")
        }),
        cmd!("sequence.closeGap", "Close Gap", ["Sequence"], None, r#"{"track":"V1"|id,"time":ticks}"#, has_seq, |s, p| {
            let tr = track_p(s, p, "track", "sequence.closeGap")?.ok_or_else(|| bad("sequence.closeGap", "need `track`"))?;
            let t = time_p(s, p, "").unwrap_or(s.playhead());
            s.edit_sequence("Ripple Delete", |q, _, st| {
                let before: Vec<(ClipId, Tick)> = q.track(tr).map(|x| x.items.iter().map(|i| (i.id, i.start)).collect()).unwrap_or_default();
                edit::close_gap(q, tr, t)?;
                if st.ripple_sequence_markers {
                    // the gap ended where the first clip that moved started
                    let moved = before
                        .iter()
                        .filter_map(|(id, old)| q.find_item(*id).filter(|(_, i)| i.start != *old).map(|(_, i)| (*old, *old - i.start)))
                        .min_by_key(|m| m.0);
                    if let Some((end, len)) = moved {
                        crate::sequence_tools::ripple_markers(&mut q.markers, end, -len);
                    }
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("sequence.snap", "Snap in Timeline", ["Sequence"], Some("S"), r#"{"on":bool?}"#, always, |s, p| {
            s.state.snapping = bool_p(p, "on").unwrap_or(!s.state.snapping);
            Ok(json!({"snapping": s.state.snapping}))
        }),
        cmd!("sequence.linkedSelection", "Linked Selection", ["Sequence"], None, r#"{"on":bool?}"#, always, |s, p| {
            s.state.linked_selection = bool_p(p, "on").unwrap_or(!s.state.linked_selection);
            Ok(json!({"linkedSelection": s.state.linked_selection}))
        }),
        cmd!("sequence.nestSequences", "Insert and overwrite sequences as nests or individual clips", [], None, r#"{"on":bool?}"#, always, |s, p| {
            let nest = bool_p(p, "on").unwrap_or(s.state.sequences_as_clips);
            s.state.sequences_as_clips = !nest;
            Ok(json!({"nest": nest}))
        }),
        cmd!(
            "sequence.addTracks",
            "Add Tracks…",
            ["Sequence"],
            None,
            r#"{"video":n=1,"audio":n=0,"submix":n=0,"videoAfter":"first"|"V2"|n?,"audioAfter":"first"|"A2"|n?,"submixAfter":"first"|"S1"|n?,"audioType":"standard|5.1|adaptive|mono"?,"submixType":"stereo|5.1|adaptive|mono"?}"#,
            has_seq,
            crate::sequence_tools::add_tracks
        ),
        cmd!("sequence.deleteTrack", "Delete Track", [], None, r#"{"track":"V3"|id}"#, has_seq, |s, p| {
            let tr = track_p(s, p, "track", "sequence.deleteTrack")?.ok_or_else(|| bad("sequence.deleteTrack", "need `track`"))?;
            s.edit_sequence("Delete Track", |q, _, _| {
                q.video_tracks.retain(|t| t.id != tr);
                q.audio_tracks.retain(|t| t.id != tr);
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "sequence.settings",
            "Sequence Settings…",
            ["Sequence"],
            None,
            r#"{"width":u32?,"height":u32?,"fps":f64?,"name":str?,"sampleRate":u32?,"mix":"Stereo|Mono|5.1|Adaptive"?,"dropFrame":bool?,"maxRenderQuality":bool?,"workingSpace":"rec709"|"rec2100-pq"|"rec2100-hlg"?,"wideGamut":bool?,"autoToneMap":bool?,"scaleMotion":bool=false}"#,
            has_seq,
            |s, p| {
                const CMD: &str = "sequence.settings";
                let id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
                let fps = fps_p(p, CMD)?;
                let mix = match str_p(p, "mix") {
                    Some(m) => Some(crate::mixer::channels_from(m).ok_or_else(|| bad(CMD, format!("unknown mix `{m}` (Stereo, Mono, 5.1, Adaptive)")))?),
                    None => None,
                };
                let working = match str_p(p, "workingSpace") {
                    Some(w) => Some(filmcraft_color::WorkingSpace::parse(w).ok_or_else(|| bad(CMD, format!("unknown working space `{w}`")))?),
                    None => None,
                };
                let drop_frame = bool_p(p, "dropFrame");
                let rate = fps.or_else(|| s.active_sequence().map(|q| q.settings.frame_rate)).unwrap_or(FrameRate::FPS_23_976);
                if drop_frame == Some(true) && !rate.supports_drop_frame() {
                    return Err(bad(CMD, format!("drop-frame timecode needs a 29.97, 59.94 or 119.88 fps timebase, not {rate}")));
                }
                let scale_motion = bool_p(p, "scaleMotion").unwrap_or(false);
                let p = p.clone();
                let scaled = s.edit("Sequence Settings", |pr, _| {
                    if let Some(n) = str_p(&p, "name") {
                        pr.item_mut(id).ok_or(EngineError::NoSequence)?.name = n.to_string();
                    }
                    let q = pr.sequence_mut(id).ok_or(EngineError::NoSequence)?;
                    let old_size = (q.settings.width, q.settings.height);
                    if let Some(w) = checked_u32_p(&p, "width", CMD)? {
                        q.settings.width = w;
                    }
                    if let Some(h) = checked_u32_p(&p, "height", CMD)? {
                        q.settings.height = h;
                    }
                    if let Some(f) = fps {
                        q.settings.frame_rate = f;
                    }
                    if let Some(sr) = checked_u32_p(&p, "sampleRate", CMD)? {
                        q.settings.sample_rate = sr;
                    }
                    if let Some(m) = mix {
                        q.settings.audio_master = m;
                    }
                    if let Some(df) = drop_frame {
                        q.settings.drop_frame = df;
                    }
                    if let Some(v) = bool_p(&p, "maxRenderQuality") {
                        q.settings.max_render_quality = v;
                    }
                    let c = &mut q.settings.color;
                    if let Some(w) = working {
                        c.working = w;
                    }
                    if let Some(v) = bool_p(&p, "wideGamut") {
                        c.wide_gamut = v;
                    }
                    if let Some(v) = bool_p(&p, "autoToneMap") {
                        c.auto_tone_map = v;
                    }
                    q.settings.working_space = q.settings.color.working.label().into();
                    q.settings.validate().map_err(|e| bad(CMD, e))?;
                    let new_size = (q.settings.width, q.settings.height);
                    Ok(if scale_motion { edit::frame_size::scale_motion(q, old_size, new_size) } else { 0 })
                })?;
                Ok(json!({"scaledClips": scaled}))
            }
        ),
        cmd!("sequence.renderEffectsInToOut", "Render Effects In to Out", ["Sequence"], Some("Enter"), r#"{"wait":bool=false}"#, has_seq, |s, p| {
            crate::previews::render(s, crate::previews::RenderMode::EffectsInToOut, p)
        }),
        cmd!("sequence.renderInToOut", "Render In to Out", ["Sequence"], None, r#"{"wait":bool=false}"#, has_seq, |s, p| {
            crate::previews::render(s, crate::previews::RenderMode::InToOut, p)
        }),
        cmd!("sequence.renderSelection", "Render Selection", ["Sequence"], None, r#"{"wait":bool=false}"#, has_selection, |s, p| {
            crate::previews::render(s, crate::previews::RenderMode::Selection, p)
        }),
        cmd!("sequence.renderAudio", "Render Audio", ["Sequence"], None, r#"{"wait":bool=false}"#, has_seq, |s, p| crate::previews::render_audio(s, p)),
        cmd!("sequence.deleteRenderFiles", "Delete Render Files", ["Sequence"], None, "{}", has_previews, |s, _| crate::previews::delete(s, false)),
        cmd!("sequence.deleteRenderFilesInToOut", "Delete Render Files In to Out", ["Sequence"], None, "{}", has_in_out, |s, _| {
            crate::previews::delete(s, true)
        }),
        query!("sequence.renderBar", "Render Bar", "{}", |s, _| crate::previews::bar_json(s)),
        cmd!("sequence.matchFrame", "Match Frame", ["Sequence"], Some("F"), "{}", has_seq, |s, _| {
            let t = s.playhead();
            let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let hit = seq.video_tracks.iter().rev().chain(seq.audio_tracks.iter()).find_map(|tr| tr.item_at(t).map(|i| (i.item, i.source_time_at(t))));
            let (item, st) = hit.ok_or_else(|| EngineError::Other("no clip at the playhead".into()))?;
            s.state.source_item = Some(item);
            s.state.source_playhead = st;
            s.events.push(crate::Event::OpenSource(item));
            Ok(json!({"item": item.0}))
        }),
        // ================= Markers =================
        cmd!("markers.markIn", "Mark In", ["Markers"], Some("I"), r#"{"time":ticks?,"target":"program|source"}"#, always, |s, p| mark(s, p, true)),
        cmd!("markers.markOut", "Mark Out", ["Markers"], Some("O"), r#"{"time":ticks?,"target":"program|source"}"#, always, |s, p| mark(s, p, false)),
        cmd!("markers.markClip", "Mark Clip", ["Markers"], Some("X"), "{}", has_seq, |s, _| {
            let t = s.playhead();
            let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let tg = s.targeting().targeted;
            let r = seq
                .all_tracks()
                .filter(|tr| tg.contains(&tr.id))
                .find_map(|tr| tr.item_at(t).map(|i| i.range()))
                .ok_or_else(|| EngineError::Other("no clip at the playhead".into()))?;
            let fd = s.sequence_rate().frame_duration();
            s.edit_sequence("Mark Clip", |q, _, _| {
                q.mark_in = Some(r.start);
                q.mark_out = Some(r.end() - fd);
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("markers.markSelection", "Mark Selection", ["Markers"], Some("/"), "{}", has_selection, |s, _| {
            let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let sel = s.state.selection.clone();
            let items: Vec<_> = sel.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| i.range())).collect();
            let a = items.iter().map(|r| r.start).min().unwrap_or_default();
            let b = items.iter().map(|r| r.end()).max().unwrap_or_default();
            let fd = s.sequence_rate().frame_duration();
            s.edit_sequence("Mark Selection", |q, _, _| {
                q.mark_in = Some(a);
                q.mark_out = Some(b - fd);
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        // With no In / Out mark, like Premiere: the start / end of the sequence (#208).
        cmd!("markers.goToIn", "Go to In", ["Markers"], Some("Shift+I"), r#"{"target":"program|source"}"#, always, |s, p| {
            if let Some(v) = crate::source_monitor::route(s, "markers.goToIn", p)? {
                return Ok(v);
            }
            s.active_sequence().ok_or(EngineError::NoSequence)?;
            if let Some(i) = s.active_sequence().map(|q| q.mark_in.unwrap_or(Tick::ZERO)) {
                s.set_playhead(i);
            }
            Ok(Value::Null)
        }),
        cmd!("markers.goToOut", "Go to Out", ["Markers"], Some("Shift+O"), r#"{"target":"program|source"}"#, always, |s, p| {
            if let Some(v) = crate::source_monitor::route(s, "markers.goToOut", p)? {
                return Ok(v);
            }
            s.active_sequence().ok_or(EngineError::NoSequence)?;
            if let Some(o) = s.active_sequence().map(|q| q.mark_out.unwrap_or_else(|| q.duration())) {
                s.set_playhead(o);
            }
            Ok(Value::Null)
        }),
        cmd!("markers.clearIn", "Clear In", ["Markers"], Some("Cmd+Shift+I"), r#"{"target":"program|source"}"#, always, |s, p| {
            if let Some(v) = crate::source_monitor::route(s, "markers.clearIn", p)? {
                return Ok(v);
            }
            s.edit_sequence("Clear In", |q, _, _| {
                q.mark_in = None;
                q.split.video_in = None;
                q.split.audio_in = None;
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("markers.clearOut", "Clear Out", ["Markers"], Some("Cmd+Shift+O"), r#"{"target":"program|source"}"#, always, |s, p| {
            if let Some(v) = crate::source_monitor::route(s, "markers.clearOut", p)? {
                return Ok(v);
            }
            s.edit_sequence("Clear Out", |q, _, _| {
                q.mark_out = None;
                q.split.video_out = None;
                q.split.audio_out = None;
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("markers.clearInOut", "Clear In and Out", ["Markers"], Some("Cmd+Shift+X"), r#"{"target":"program|source"}"#, always, |s, p| {
            if let Some(v) = crate::source_monitor::route(s, "markers.clearInOut", p)? {
                return Ok(v);
            }
            s.edit_sequence("Clear In and Out", |q, _, _| {
                q.mark_in = None;
                q.mark_out = None;
                q.split = Default::default();
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "markers.add",
            "Add Marker",
            ["Markers"],
            Some("M"),
            r#"{"target":"program|source","time":ticks?,"name":str?,"comment":str?,"color":label?,"durationFrames":i64?}"#,
            always,
            |s, p| {
                if let Some(v) = crate::source_monitor::route(s, "markers.add", p)? {
                    return Ok(v);
                }
                let (_seq, ph) = marker_list_mut(s)?;
                let t = time_p(s, p, "").unwrap_or(ph);
                let name = str_p(p, "name").unwrap_or("").to_string();
                let comment = str_p(p, "comment").unwrap_or("").to_string();
                let color = str_p(p, "color").and_then(Label::from_name).unwrap_or(Label::Green);
                let dur = p.get("durationFrames").and_then(Value::as_i64).map(|f| s.sequence_rate().tick_of(f)).unwrap_or_default();
                let id = s.edit_sequence("Add Marker", |q, ctx, _| {
                    let id = MarkerId(ctx.alloc());
                    q.markers.push(Marker { id, start: t, duration: dur, name, comment, kind: MarkerKind::Comment, color });
                    q.markers.sort_by_key(|m| m.start);
                    Ok(id)
                })?;
                Ok(json!({"marker": id.0}))
            }
        ),
        cmd!("markers.goNext", "Go to Next Marker", ["Markers"], Some("Shift+M"), r#"{"target":"program|source"}"#, always, |s, p| {
            if let Some(v) = crate::source_monitor::route(s, "markers.goNext", p)? {
                return Ok(v);
            }
            let t = s.playhead();
            s.active_sequence().ok_or(EngineError::NoSequence)?;
            if let Some(m) = s.active_sequence().and_then(|q| q.markers.iter().map(|m| m.start).find(|m| *m > t)) {
                s.set_playhead(m);
            }
            Ok(Value::Null)
        }),
        cmd!("markers.goPrev", "Go to Previous Marker", ["Markers"], Some("Cmd+Shift+M"), r#"{"target":"program|source"}"#, always, |s, p| {
            if let Some(v) = crate::source_monitor::route(s, "markers.goPrev", p)? {
                return Ok(v);
            }
            let t = s.playhead();
            s.active_sequence().ok_or(EngineError::NoSequence)?;
            if let Some(m) = s.active_sequence().and_then(|q| q.markers.iter().rev().map(|m| m.start).find(|m| *m < t)) {
                s.set_playhead(m);
            }
            Ok(Value::Null)
        }),
        cmd!("markers.clearCurrent", "Clear Selected Marker", ["Markers"], Some("Cmd+Alt+M"), r#"{"target":"program|source"}"#, always, |s, p| {
            if let Some(v) = crate::source_monitor::route(s, "markers.clearCurrent", p)? {
                return Ok(v);
            }
            let t = s.playhead();
            s.edit_sequence("Clear Marker", |q, _, _| {
                q.markers.retain(|m| m.start != t);
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "markers.exportCsv",
            "Export Markers as CSV…",
            ["Markers"],
            None,
            r#"{"path":str} (all markers in the active sequence)"#,
            has_seq,
            crate::marker_export::export
        ),
        cmd!("markers.clearAll", "Clear Markers", ["Markers"], Some("Cmd+Alt+Shift+M"), r#"{"target":"program|source"}"#, always, |s, p| {
            if let Some(v) = crate::source_monitor::route(s, "markers.clearAll", p)? {
                return Ok(v);
            }
            s.edit_sequence("Clear All Markers", |q, _, _| {
                q.markers.clear();
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "markers.edit",
            "Edit Marker…",
            [],
            None,
            r#"{"target":"program|source","marker":id,"name":str?,"comment":str?,"color":label?,"durationFrames":i64?}"#,
            always,
            |s, p| {
                if let Some(v) = crate::source_monitor::route(s, "markers.edit", p)? {
                    return Ok(v);
                }
                let id = u64_p(p, "marker").map(MarkerId).ok_or_else(|| bad("markers.edit", "need `marker`"))?;
                let rate = s.sequence_rate();
                let p = p.clone();
                s.edit_sequence("Edit Marker", |q, _, _| {
                    let m = q.markers.iter_mut().find(|m| m.id == id).ok_or_else(|| bad("markers.edit", "no such marker"))?;
                    if let Some(n) = str_p(&p, "name") {
                        m.name = n.into();
                    }
                    if let Some(c) = str_p(&p, "comment") {
                        m.comment = c.into();
                    }
                    if let Some(c) = str_p(&p, "color").and_then(Label::from_name) {
                        m.color = c;
                    }
                    if let Some(f) = p.get("durationFrames").and_then(Value::as_i64) {
                        m.duration = rate.tick_of(f);
                    }
                    Ok(())
                })?;
                Ok(Value::Null)
            }
        ),
        // ================= Playhead / navigation =================
        cmd!("playhead.set", "Set Playhead", [], None, r#"{"time":ticks|"frame":i64|"seconds":f64|"timecode":str}"#, has_seq, |s, p| {
            let t = time_p(s, p, "").ok_or_else(|| bad("playhead.set", "need time/frame/seconds/timecode"))?;
            s.set_playhead(t);
            Ok(json!({"time": s.playhead().0}))
        }),
        cmd!("playhead.step", "Step Frames", [], None, r#"{"frames":i64}"#, has_seq, |s, p| {
            let n = p.get("frames").and_then(Value::as_i64).unwrap_or(1);
            let r = s.sequence_rate();
            let f = r.frame_at(s.playhead()).checked_add(n).ok_or_else(|| bad("playhead.step", "frame offset is too large"))?.max(0);
            if f > r.frame_at(Tick::MAX) {
                return Err(bad("playhead.step", "target frame exceeds the supported time range"));
            }
            s.set_playhead(r.tick_of(f));
            Ok(json!({"frame": r.frame_at(s.playhead())}))
        }),
        cmd!("playhead.stepForward", "Step Forward One Frame", [], Some("Right"), "{}", has_seq, |s, _| s.execute("playhead.step", json!({"frames": 1}))),
        cmd!("playhead.stepBack", "Step Back One Frame", [], Some("Left"), "{}", has_seq, |s, _| s.execute("playhead.step", json!({"frames": -1}))),
        // Settings ▸ Playback ▸ Step forward/back many
        cmd!("playhead.stepForward5", "Step Forward Five Frames", [], Some("Shift+Right"), "{}", has_seq, |s, _| {
            let n = s.prefs.playback.step_many_frames as i64;
            s.execute("playhead.step", json!({"frames": n}))
        }),
        cmd!("playhead.stepBack5", "Step Back Five Frames", [], Some("Shift+Left"), "{}", has_seq, |s, _| {
            let n = s.prefs.playback.step_many_frames as i64;
            s.execute("playhead.step", json!({"frames": -n}))
        }),
        // Up / Down: edit points on the targeted tracks (Shift+Up / Shift+Down: any track)
        cmd!("playhead.nextEdit", "Go to Next Edit Point", [], Some("Down"), "{}", has_seq, |s, _| crate::keyboard::go_to_edit(s, true, false)),
        cmd!("playhead.prevEdit", "Go to Previous Edit Point", [], Some("Up"), "{}", has_seq, |s, _| crate::keyboard::go_to_edit(s, false, false)),
        cmd!("playhead.start", "Go to Sequence Start", [], Some("Home"), "{}", has_seq, |s, _| {
            s.set_playhead(Tick::ZERO);
            Ok(Value::Null)
        }),
        cmd!("playhead.end", "Go to Sequence End", [], Some("End"), "{}", has_seq, |s, _| {
            let e = s.active_sequence().map(|q| q.duration()).unwrap_or_default();
            s.set_playhead(e);
            Ok(Value::Null)
        }),
        // ================= Source monitor =================
        cmd!("source.open", "Open in Source Monitor", [], None, r#"{"item":id?}"#, always, |s, p| {
            let id = item_p(p, "item")
                .or(s.state.project_selection.first().copied())
                .ok_or_else(|| bad("source.open", "need `item` (or a Project panel selection)"))?;
            s.project.item(id).ok_or_else(|| bad("source.open", "no such item"))?;
            s.state.source_item = Some(id);
            // a subclip opens at its In point
            s.state.source_playhead = crate::clip_ops::source_view(s, id).map(|v| v.start).unwrap_or_default();
            s.events.push(crate::Event::OpenSource(id));
            Ok(Value::Null)
        }),
        cmd!("source.setPlayhead", "Set Source Playhead", [], None, r#"{"time":ticks|"frame":i64|"seconds":f64}"#, always, |s, p| {
            let item = s.state.source_item.ok_or_else(|| EngineError::Other("no source clip".into()))?;
            let view = crate::clip_ops::source_view(s, item);
            let rate = view.as_ref().map(|v| v.rate).unwrap_or_default();
            let t = p
                .get("time")
                .and_then(Value::as_i64)
                .map(Tick)
                .or_else(|| p.get("frame").and_then(Value::as_i64).map(|f| rate.tick_of(f)))
                .or_else(|| f64_p(p, "seconds").map(Tick::from_seconds_f64))
                .unwrap_or_default();
            // the Source monitor's span: a subclip that restricts trims keeps the playhead inside it
            let (lo, hi) = view.map(|v| (v.start, v.end)).unwrap_or((Tick::ZERO, Tick::MAX));
            s.state.source_playhead = rate.snap(t.clamp(lo, (hi - rate.frame_duration()).max(lo))).max(lo);
            Ok(json!({"time": s.state.source_playhead.0}))
        }),
        query!("source.inspect", "Inspect Source Monitor", "{}", |s, _| {
            let Some(item) = s.state.source_item else { return Ok(json!({"item": null})) };
            let v = crate::clip_ops::source_view(s, item).ok_or_else(|| bad("source.inspect", "the source item is gone"))?;
            Ok(v.to_json(s.state.source_playhead))
        }),
        cmd!("source.insert", "Insert", ["Clip"], Some(","), "{}", has_source, |s, _| edit_at_source(s, true)),
        cmd!("source.overwrite", "Overwrite", ["Clip"], Some("."), "{}", has_source, |s, _| edit_at_source(s, false)),
        // ================= Timeline gestures =================
        cmd!(
            "timeline.place",
            "Place Clip",
            [],
            None,
            r#"{"item":id,"track":"V1"|id|"A1" (sound only)?,"audioTrack":"A1"|id?,"time":ticks|"frame":i64|"seconds":f64,"insert":bool,"sourceIn":ticks?,"duration":ticks?,"video":bool=true,"audio":bool=true}"#,
            has_seq,
            |s, p| {
                let stream_flag = |key: &str| match p.get(key) {
                    None => Ok(true),
                    Some(Value::Bool(v)) => Ok(*v),
                    _ => Err(bad("timeline.place", format!("`{key}` must be a boolean"))),
                };
                let (video, audio) = (stream_flag("video")?, stream_flag("audio")?);
                if !video && !audio {
                    return Err(bad("timeline.place", "enable video, audio, or both"));
                }
                let item = item_p(p, "item").ok_or_else(|| bad("timeline.place", "need `item`"))?;
                let at = time_p(s, p, "").unwrap_or(s.playhead());
                let tg = s.targeting();
                let pi = s.project.item(item).ok_or_else(|| bad("timeline.place", "no such item"))?;
                let is_audio = |t: TrackId| s.active_sequence().is_some_and(|q| q.audio_tracks.iter().any(|x| x.id == t));
                // `track` is where the picture goes and `audioTrack` where the sound goes. A caller that
                // names an audio track as `track` asks for the sound alone, on that track.
                let (v, a) = match (track_p(s, p, "track", "timeline.place")?, track_p(s, p, "audioTrack", "timeline.place")?) {
                    (Some(t), Some(_)) if is_audio(t) => {
                        return Err(bad("timeline.place", "`track` names an audio track and `audioTrack` is given as well: name the sound's track once"));
                    }
                    (_, Some(t)) if !is_audio(t) => return Err(bad("timeline.place", "`audioTrack` names a video track")),
                    (Some(t), None) if is_audio(t) && !pi.has_audio() => {
                        return Err(bad("timeline.place", "`track` names an audio track and this item has no sound"));
                    }
                    (Some(t), None) if is_audio(t) => (None, Some(t)),
                    (Some(_), None) if !pi.has_video() => {
                        return Err(bad("timeline.place", "`track` names a video track and this item has no picture: name an audio track"));
                    }
                    (v, a) => (v.or(tg.video_dest), a.or(tg.audio_dest)),
                };
                let v = v.filter(|_| video);
                let a = a.filter(|_| audio);
                if p.get("video").is_some() && video && pi.has_video() && v.is_none() {
                    return Err(bad("timeline.place", "no video destination: add or enable a video track"));
                }
                if p.get("audio").is_some() && audio && pi.has_audio() && a.is_none() {
                    return Err(bad("timeline.place", "no audio destination: add or enable an audio track"));
                }
                let full = match &pi.kind {
                    // Settings ▸ Timeline ▸ Still Image Default Duration
                    ItemKind::Media(m)
                        if matches!(m.info.kind, filmcraft_media::MediaKind::Still)
                            || (matches!(m.info.kind, filmcraft_media::MediaKind::Synthetic) && m.info.duration.0 <= 0) =>
                    {
                        s.prefs.timeline.still_duration(s.sequence_rate())
                    }
                    _ => pi.duration(),
                };
                let (mi, mo) = match &pi.kind {
                    ItemKind::Media(m) => (m.mark_in, m.mark_out.map(|o| o + pi.frame_rate().frame_duration())),
                    ItemKind::Subclip { range, .. } => (Some(range.start), Some(range.end())),
                    _ => (None, None),
                };
                let sin = p.get("sourceIn").and_then(Value::as_i64).map(Tick).or(mi).unwrap_or_default();
                let dur = p.get("duration").and_then(Value::as_i64).map(Tick).unwrap_or_else(|| mo.unwrap_or(full) - sin);
                let ids = place_item(s, item, TimeRange::new(sin, dur), at, v, a, bool_p(p, "insert").unwrap_or(false), "Place Clip", None)?;
                Ok(json!({"clips": ids.iter().map(|c| c.0).collect::<Vec<_>>()}))
            }
        ),
        cmd!("timeline.select", "Select Clips", [], None, r#"{"clips":[id],"transitions":[id]?,"add":bool,"toggle":bool}"#, has_seq, |s, p| {
            if let Some(ids) = p.get("transitions") {
                return select_transitions(s, ids, bool_p(p, "add").unwrap_or(false), bool_p(p, "toggle").unwrap_or(false));
            }
            let clips: Vec<ClipId> =
                p.get("clips").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect()).unwrap_or_default();
            let clips = with_links(s, &clips);
            // selecting clips leaves trim mode (Premiere: clip and edit point selections are exclusive)
            s.state.edit_points.clear();
            s.state.trim_shift = Default::default();
            // and a clip selection replaces a transition one
            s.state.transition_selection.clear();
            if bool_p(p, "toggle").unwrap_or(false) {
                for c in clips {
                    if let Some(i) = s.state.selection.iter().position(|x| *x == c) {
                        s.state.selection.remove(i);
                    } else {
                        s.state.selection.push(c);
                    }
                }
            } else if bool_p(p, "add").unwrap_or(false) {
                for c in clips {
                    if !s.state.selection.contains(&c) {
                        s.state.selection.push(c);
                    }
                }
            } else {
                s.state.selection = clips;
                s.state.caption_selection.clear();
            }
            Ok(json!({"selection": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>()}))
        }),
        cmd!(
            "timeline.move",
            "Move Clips",
            [],
            None,
            r#"{"moves":[{"clip":id,"track":id|"V2","time":ticks}],"insert":bool,"linked":bool?}"#,
            has_seq,
            move_clips
        ),
        cmd!(
            "timeline.trim",
            "Trim Edit",
            [],
            None,
            r#"{"clip":id,"edge":"in|out","mode":"regular|ripple","delta":ticks|"deltaFrames":i64}"#,
            has_seq,
            |s, p| {
                let c = clip_p(p, "clip").ok_or_else(|| bad("timeline.trim", "need `clip`"))?;
                let edge = if str_p(p, "edge") == Some("in") { Edge::In } else { Edge::Out };
                let mode = if str_p(p, "mode") == Some("ripple") { TrimMode::Ripple } else { TrimMode::Regular };
                let d = p
                    .get("delta")
                    .and_then(Value::as_i64)
                    .map(Tick)
                    .or_else(|| p.get("deltaFrames").and_then(Value::as_i64).map(|f| s.sequence_rate().tick_of(f)))
                    .unwrap_or_default();
                let clips = with_links(s, &[c]);
                let applied = s.edit_sequence(if mode == TrimMode::Ripple { "Ripple Trim" } else { "Trim" }, |q, ctx, st| {
                    let before = q.find_item(c).map(|(_, i)| i.range());
                    // clamp across all linked partners, then apply the common delta
                    let mut dd = d;
                    for c in &clips {
                        let x = edit::clamp_trim(q, *c, edge, mode, dd, ctx)?;
                        if x.abs() < dd.abs() {
                            dd = x;
                        }
                    }
                    if mode == TrimMode::Ripple {
                        // the clip and its linked partners ripple as one edit
                        let mut group = vec![c];
                        group.extend(clips.iter().copied().filter(|o| *o != c));
                        dd = edit::ripple_trim_group(q, &group, edge, dd, ctx)?;
                        if st.ripple_sequence_markers
                            && let Some(r) = before
                        {
                            let (from, shift) = match edge {
                                Edge::In if dd > Tick::ZERO => (r.start + dd, -dd),
                                Edge::In => (r.start, -dd),
                                Edge::Out => (r.end(), dd),
                            };
                            crate::sequence_tools::ripple_markers(&mut q.markers, from, shift);
                        }
                    } else {
                        for c in &clips {
                            edit::trim(q, *c, edge, mode, dd, ctx)?;
                        }
                    }
                    Ok(dd)
                })?;
                Ok(json!({"delta": applied.0}))
            }
        ),
        // ================= Trim mode =================
        cmd!("trim.selectEditPoint", "Select Edit Point", [], None, r#"{"clip":id,"edge":"in|out","kind":"trim|ripple|roll","add":bool?}"#, has_seq, |s, p| {
            crate::trim::select(s, p)
        }),
        cmd!("trim.selectNearest", "Select Nearest Edit Point", [], None, r#"{"kind":"rippleIn|rippleOut|roll|trimIn|trimOut"}"#, has_seq, |s, p| {
            crate::trim::select_nearest(s, p)
        }),
        cmd!("trim.edit", "Trim Edit", ["Sequence"], Some("Shift+T"), "{}", has_seq, |s, _| {
            if s.state.edit_points.is_empty() {
                crate::trim::select_nearest(s, &json!({"kind": "roll"}))
            } else {
                Ok(json!({"editPoints": s.state.edit_points}))
            }
        }),
        cmd!("trim.clear", "Clear Edit Point Selection", [], None, "{}", has_edit_points, |s, _| {
            s.state.edit_points.clear();
            s.state.trim_shift = Default::default();
            s.trim_play.around = None;
            Ok(Value::Null)
        }),
        cmd!("trim.toggleType", "Toggle Trim Type", [], Some("Ctrl+T"), "{}", has_edit_points, |s, _| crate::trim::toggle_type(s)),
        cmd!("trim.backward", "Trim Backward", [], Some("Alt+Left"), "{}", has_edit_points, |s, _| crate::trim::nudge(s, &json!({"frames": -1}))),
        cmd!("trim.forward", "Trim Forward", [], Some("Alt+Right"), "{}", has_edit_points, |s, _| crate::trim::nudge(s, &json!({"frames": 1}))),
        cmd!("trim.backwardMany", "Trim Backward Many", [], Some("Alt+Shift+Left"), "{}", has_edit_points, |s, _| {
            let n = -(s.prefs.trim.large_trim_offset as i64);
            crate::trim::nudge(s, &json!({"frames": n}))
        }),
        cmd!("trim.forwardMany", "Trim Forward Many", [], Some("Alt+Shift+Right"), "{}", has_edit_points, |s, _| {
            let n = s.prefs.trim.large_trim_offset as i64;
            crate::trim::nudge(s, &json!({"frames": n}))
        }),
        cmd!("trim.applyDefaultTransition", "Apply Default Transitions to Selection", ["Sequence"], None, "{}", has_edit_points, |s, _| {
            crate::trim::apply_default_transitions(s)
        }),
        cmd!("trim.shuttle", "Dynamic Trim (Shuttle)", [], None, r#"{"direction":"forward|reverse","slow":bool?,"clock":seconds}"#, has_edit_points, |s, p| {
            crate::trim::shuttle(s, p)
        }),
        cmd!("trim.shuttleStop", "Dynamic Trim Stop", [], None, r#"{"clock":seconds?}"#, has_seq, |s, p| crate::trim::stop(s, p)),
        cmd!("trim.cancelDynamic", "Cancel Dynamic Trim", [], None, "{}", has_seq, |s, _| crate::trim::cancel(s)),
        cmd!("trim.playAround", "Play Around Edit", [], None, r#"{"clock":seconds,"loop":bool=true,"toggle":bool?}"#, has_seq, |s, p| {
            crate::trim::play_around(s, p)
        }),
        CommandSpec {
            id: "trim.tick",
            label: "Advance Trim Playback",
            menu: &[],
            shortcut: None,
            params: r#"{"clock":seconds}"#,
            enabled: has_seq,
            run: |s, p| crate::trim::tick(s, p.get("clock").and_then(Value::as_f64).unwrap_or(0.0)),
            journal: false,
        },
        query!("trim.monitor", "Trim Monitor State", "{}", |s, _| Ok(crate::trim::monitor_info(s))),
        cmd!("trim.nudge", "Trim by Frames", [], None, r#"{"frames":i64}"#, has_edit_points, |s, p| crate::trim::nudge(s, p)),
        cmd!("trim.extendToPlayhead", "Extend Selected Edit to Playhead", [], Some("E"), "{}", has_edit_points, |s, _| { crate::trim::extend_to_playhead(s) }),
        cmd!("trim.ripplePrevious", "Ripple Trim Previous Edit to Playhead", [], Some("Q"), "{}", has_seq, |s, _| crate::trim::to_playhead(
            s,
            &json!({"side": "previous", "ripple": true})
        )),
        cmd!("trim.rippleNext", "Ripple Trim Next Edit to Playhead", [], Some("W"), "{}", has_seq, |s, _| crate::trim::to_playhead(
            s,
            &json!({"side": "next", "ripple": true})
        )),
        cmd!("trim.previous", "Trim Previous Edit to Playhead", [], Some("Alt+Q"), "{}", has_seq, |s, _| crate::trim::to_playhead(
            s,
            &json!({"side": "previous", "ripple": false})
        )),
        cmd!("trim.next", "Trim Next Edit to Playhead", [], Some("Alt+W"), "{}", has_seq, |s, _| crate::trim::to_playhead(
            s,
            &json!({"side": "next", "ripple": false})
        )),
        cmd!("timeline.roll", "Rolling Edit", [], None, r#"{"left":id,"right":id,"delta":ticks|"deltaFrames":i64}"#, has_seq, |s, p| {
            let (l, r) = (
                clip_p(p, "left").ok_or_else(|| bad("timeline.roll", "need `left`"))?,
                clip_p(p, "right").ok_or_else(|| bad("timeline.roll", "need `right`"))?,
            );
            let d = p
                .get("delta")
                .and_then(Value::as_i64)
                .map(Tick)
                .or_else(|| p.get("deltaFrames").and_then(Value::as_i64).map(|f| s.sequence_rate().tick_of(f)))
                .unwrap_or_default();
            let x = s.edit_sequence("Rolling Edit", |q, ctx, _| Ok(edit::roll(q, l, r, d, ctx)?))?;
            Ok(json!({"delta": x.0}))
        }),
        cmd!("timeline.slip", "Slip", [], None, r#"{"clip":id,"delta":ticks|"deltaFrames":i64}"#, has_seq, |s, p| {
            let c = clip_p(p, "clip").ok_or_else(|| bad("timeline.slip", "need `clip`"))?;
            let d = p
                .get("delta")
                .and_then(Value::as_i64)
                .map(Tick)
                .or_else(|| p.get("deltaFrames").and_then(Value::as_i64).map(|f| s.sequence_rate().tick_of(f)))
                .unwrap_or_default();
            let clips = with_links(s, &[c]);
            s.edit_sequence("Slip", |q, ctx, _| {
                // clamp across all linked partners, then slip them by the common delta
                let mut dd = d;
                for c in &clips {
                    let x = edit::slip(&mut q.clone(), *c, dd, ctx)?;
                    if x.abs() < dd.abs() {
                        dd = x;
                    }
                }
                for c in &clips {
                    edit::slip(q, *c, dd, ctx)?;
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("timeline.slide", "Slide", [], None, r#"{"clip":id,"delta":ticks|"deltaFrames":i64}"#, has_seq, |s, p| {
            let c = clip_p(p, "clip").ok_or_else(|| bad("timeline.slide", "need `clip`"))?;
            let d = p
                .get("delta")
                .and_then(Value::as_i64)
                .map(Tick)
                .or_else(|| p.get("deltaFrames").and_then(Value::as_i64).map(|f| s.sequence_rate().tick_of(f)))
                .unwrap_or_default();
            let clips = with_links(s, &[c]);
            s.edit_sequence("Slide", |q, ctx, _| {
                // clamp across all linked partners, then slide them by the common delta
                let mut dd = d;
                for c in &clips {
                    let x = edit::slide(&mut q.clone(), *c, dd, ctx)?;
                    if x.abs() < dd.abs() {
                        dd = x;
                    }
                }
                for c in &clips {
                    edit::slide(q, *c, dd, ctx)?;
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("timeline.rateStretch", "Rate Stretch", [], None, r#"{"clip":id,"edge":"in|out","delta":ticks}"#, has_seq, |s, p| {
            let c = clip_p(p, "clip").ok_or_else(|| bad("timeline.rateStretch", "need `clip`"))?;
            let edge = if str_p(p, "edge") == Some("in") { Edge::In } else { Edge::Out };
            let d = p
                .get("delta")
                .and_then(Value::as_i64)
                .map(Tick)
                .or_else(|| p.get("deltaFrames").and_then(Value::as_i64).map(|f| s.sequence_rate().tick_of(f)))
                .unwrap_or_default();
            let clips = with_links(s, &[c]);
            let sp = s.edit_sequence("Rate Stretch", |q, ctx, _| {
                let mut sp = 1.0;
                for c in &clips {
                    sp = edit::rate_stretch(q, *c, edge, d, ctx)?;
                }
                Ok(sp)
            })?;
            Ok(json!({"speed": sp}))
        }),
        cmd!("timeline.razor", "Razor", [], None, r#"{"time":ticks,"track":"V1"|id?,"clip":id?}"#, has_seq, |s, p| {
            let t = time_p(s, p, "").unwrap_or(s.playhead());
            let tr = track_p(s, p, "track", "timeline.razor")?;
            let clip = clip_p(p, "clip");
            let n = if let Some(c) = clip {
                let clips = with_links(s, &[c]);
                s.edit_sequence("Razor", |q, ctx, _| Ok(edit::razor_items(q, &clips, t, ctx)))?
            } else {
                let tracks: Vec<TrackId> = tr.into_iter().collect();
                s.edit_sequence("Razor", |q, ctx, _| Ok(edit::razor(q, &tracks, t, ctx)))?
            };
            Ok(json!({"cuts": n.len()}))
        }),
        cmd!(
            "timeline.setTrack",
            "Track Settings",
            [],
            None,
            r#"{"track":"V1"|id,"locked":bool?,"syncLock":bool?,"enabled":bool?,"muted":bool?,"solo":bool?,"name":str?,"volumeDb":f64?,"pan":f64?}"#,
            has_seq,
            |s, p| {
                let tr = track_p(s, p, "track", "timeline.setTrack")?.ok_or_else(|| bad("timeline.setTrack", "need `track`"))?;
                let p = p.clone();
                s.edit_sequence("Track Settings", |q, _, _| {
                    let t = q.track_mut(tr).ok_or(filmcraft_edit::EditError::NoTrack(tr))?;
                    if let Some(v) = bool_p(&p, "locked") {
                        t.locked = v;
                    }
                    if let Some(v) = bool_p(&p, "syncLock") {
                        t.sync_lock = v;
                    }
                    if let Some(v) = bool_p(&p, "enabled") {
                        t.enabled = v;
                    }
                    if let Some(v) = bool_p(&p, "muted") {
                        t.muted = v;
                    }
                    if let Some(v) = bool_p(&p, "solo") {
                        t.solo = v;
                    }
                    if let Some(v) = str_p(&p, "name") {
                        t.name = v.to_string();
                    }
                    if let Some(v) = f64_p(&p, "volumeDb") {
                        t.volume_db = v;
                    }
                    if let Some(v) = f64_p(&p, "pan") {
                        t.pan = v.clamp(-100.0, 100.0);
                    }
                    Ok(())
                })?;
                Ok(Value::Null)
            }
        ),
        cmd!("timeline.setTargeting", "Track Targeting", [], None, r#"{"track":"V1"|id,"targeted":bool?,"sourcePatch":bool?}"#, has_seq, |s, p| {
            let tr = track_p(s, p, "track", "timeline.setTargeting")?.ok_or_else(|| bad("timeline.setTargeting", "need `track`"))?;
            let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
            let mut tg = s.targeting();
            let is_video = s.active_sequence().is_some_and(|q| q.video_tracks.iter().any(|t| t.id == tr));
            if let Some(on) = bool_p(p, "targeted") {
                tg.targeted.retain(|t| *t != tr);
                if on {
                    tg.targeted.push(tr);
                }
            }
            if let Some(on) = bool_p(p, "sourcePatch") {
                let slot = if is_video { &mut tg.video_dest } else { &mut tg.audio_dest };
                *slot = if on {
                    Some(tr)
                } else if *slot == Some(tr) {
                    None
                } else {
                    *slot
                };
            }
            s.state.targeting.insert(seq_id, tg);
            Ok(Value::Null)
        }),
        // ================= Effects =================
        cmd!("effects.apply", "Apply Effect", [], None, r#"{"clips":[id]?,"effect":"gaussian_blur|Gaussian Blur|…"}"#, has_seq, |s, p| {
            let name = str_p(p, "effect").ok_or_else(|| bad("effects.apply", "need `effect`"))?;
            let def = resolve_effect_name(s, p, name).ok_or_else(|| bad("effects.apply", format!("unknown effect `{name}`")))?;
            if filmcraft_project::graphic::is_layer_id(def.id) {
                return Err(bad("effects.apply", "graphic layers are added with graphics.newText / graphics.newShape"));
            }
            if matches!(def.kind, filmcraft_project::EffectKind::VideoTransition | filmcraft_project::EffectKind::AudioTransition) {
                let mut q = p.clone();
                q["effect"] = json!(def.id);
                return apply_transition(s, &q, if def.kind == filmcraft_project::EffectKind::VideoTransition { TrackKind::Video } else { TrackKind::Audio });
            }
            let clips = clips_p(s, p);
            if clips.is_empty() {
                return Err(bad("effects.apply", "select clips first"));
            }
            let is_audio = def.kind == filmcraft_project::EffectKind::Audio;
            let (fw, fh) = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
            let label = format!("Apply {}", def.name);
            s.edit_sequence(&label, |q, _, _| {
                let mut n = 0;
                for t in q.all_tracks_mut() {
                    if (t.kind == TrackKind::Audio) != is_audio {
                        continue;
                    }
                    for i in t.items.iter_mut().filter(|i| clips.contains(&i.id)) {
                        let mut inst = def.instance();
                        resolve_auto_points(&mut inst, (fw, fh), (fw, fh));
                        // standard effects go before the intrinsic ones (render order)
                        let pos = i.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(i.effects.len());
                        let pos = if is_audio { i.effects.len() } else { pos };
                        i.effects.insert(pos, inst);
                        n += 1;
                    }
                }
                Ok(n)
            })?;
            Ok(Value::Null)
        }),
        cmd!("effects.remove", "Remove Effect", [], None, r#"{"clip":id,"index":n}"#, has_seq, |s, p| {
            let c = clip_p(p, "clip").ok_or_else(|| bad("effects.remove", "need `clip`"))?;
            let idx = u64_p(p, "index").ok_or_else(|| bad("effects.remove", "need `index`"))? as usize;
            s.edit_sequence("Remove Effect", |q, _, _| {
                let (_, it) = q.find_item_mut(c).ok_or(filmcraft_edit::EditError::NoItem(c))?;
                if it.effects.get(idx).and_then(|e| e.def()).is_some_and(|d| d.intrinsic) {
                    return Err(EngineError::Other("intrinsic effects cannot be removed".into()));
                }
                if idx < it.effects.len() {
                    it.effects.remove(idx);
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "effects.setParam",
            "Set Effect Parameter",
            [],
            None,
            r##"{"clip":id,"effect":"motion"|index,"param":str,"mask":n?,"value":num|[x,y]|"#rrggbb"|bool|path|"option name" (a choice takes its index or its name),"time":ticks? (where the keyframe goes when the parameter is animated; a parameter that is not animated gets a new static value whatever the time),"keyframe":bool? (true: write a keyframe at `time` and animate the parameter),"merge":bool?,"begin":bool?}"##,
            has_seq,
            |s, p| {
                let c = clip_p(p, "clip").ok_or_else(|| bad("effects.setParam", "need `clip`"))?;
                let pid = str_p(p, "param").ok_or_else(|| bad("effects.setParam", "need `param`"))?.to_string();
                let val = p.get("value").cloned().ok_or_else(|| bad("effects.setParam", "need `value`"))?;
                let eff = p.get("effect").cloned().unwrap_or(json!("motion"));
                let keyframe = bool_p(p, "keyframe").unwrap_or(false);
                let ph = s.playhead();
                let tl = time_p(s, p, "").unwrap_or(ph);
                let pq = p.clone();
                // `merge`: a drag of one parameter is one undo step (#201); `begin` starts a new one
                let merge = bool_p(p, "merge").unwrap_or(false).then(|| format!("setParam:{}:{eff}:{pid}:{}", c.0, p.get("mask").unwrap_or(&Value::Null)));
                if bool_p(p, "begin").unwrap_or(false) {
                    s.history.merge_key = None;
                }
                let keyframes = s.edit_sequence_as("Change Effect Parameter", merge.as_deref(), |q, _, _| {
                    let (_, it) = q.find_item_mut(c).ok_or(filmcraft_edit::EditError::NoItem(c))?;
                    let mt = it.source_time_at(tl.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
                    let e = match &eff {
                        Value::Number(n) => it.effects.get_mut(n.as_u64().unwrap_or(0) as usize),
                        Value::String(sid) => it.effects.iter_mut().find(|e| &e.effect == sid),
                        _ => None,
                    }
                    .ok_or_else(|| bad("effects.setParam", "no such effect on clip"))?;
                    // the parameter's definition (a mask's own parameters have none)
                    let def = if pq.get("mask").is_none_or(Value::is_null) { e.def().and_then(|d| d.param(&pid)) } else { None };
                    // a choice takes its option name as well as its index, as transition parameters do
                    let val = match (def.map(|d| &d.kind), &val) {
                        (Some(filmcraft_project::ParamKind::Choice(opts)), Value::String(name)) => {
                            let i = opts
                                .iter()
                                .position(|o| o.eq_ignore_ascii_case(name))
                                .ok_or_else(|| bad("effects.setParam", format!("`{pid}` must be one of {opts:?}")))?;
                            json!(i)
                        }
                        _ => val.clone(),
                    };
                    if keyframe && def.is_some_and(|d| !d.animatable) {
                        return Err(bad("effects.setParam", format!("`{pid}` cannot be animated")));
                    }
                    if eff == json!("opacity") || eff == json!("motion") {
                        e.enabled = true;
                    }
                    // instances saved before a parameter existed get it from the definition
                    if !e.params.contains_key(&pid)
                        && let Some(d) = e.def().and_then(|d| d.param(&pid))
                    {
                        e.params.insert(pid.clone(), filmcraft_project::Param::new(d.default.clone()));
                    }
                    let prm = crate::masks::target_param(e, &pq, &pid).ok_or_else(|| bad("effects.setParam", format!("no param `{pid}`")))?;
                    let v = json_to_param(&prm.value, &val).ok_or_else(|| bad("effects.setParam", "value has the wrong type"))?;
                    if keyframe {
                        // the stopwatch and the value in one step
                        prm.put_keyframe(mt, v);
                    } else {
                        prm.set_at(mt, v);
                    }
                    Ok(prm.keyframes.len())
                })?;
                // 0 keyframes: the value is static (`time` was not used)
                Ok(json!({"keyframes": keyframes}))
            }
        ),
        cmd!("effects.toggleAnimation", "Toggle Animation", [], None, r#"{"clip":id,"effect":str|index,"param":str,"mask":n?}"#, has_seq, |s, p| {
            let c = clip_p(p, "clip").ok_or_else(|| bad("effects.toggleAnimation", "need `clip`"))?;
            let pid = str_p(p, "param").ok_or_else(|| bad("effects.toggleAnimation", "need `param`"))?.to_string();
            let eff = p.get("effect").cloned().unwrap_or(json!("motion"));
            let ph = s.playhead();
            s.edit_sequence("Toggle Animation", |q, _, _| {
                let (_, it) = q.find_item_mut(c).ok_or(filmcraft_edit::EditError::NoItem(c))?;
                let mt = it.source_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
                let e = match &eff {
                    Value::Number(n) => it.effects.get_mut(n.as_u64().unwrap_or(0) as usize),
                    Value::String(sid) => it.effects.iter_mut().find(|e| &e.effect == sid),
                    _ => None,
                }
                .ok_or_else(|| bad("effects.toggleAnimation", "no such effect"))?;
                crate::masks::target_param(e, p, &pid).ok_or_else(|| bad("effects.toggleAnimation", "no such param"))?.toggle_animation(mt);
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("effects.toggleEnabled", "Toggle Effect", [], None, r#"{"clip":id,"index":n}"#, has_seq, |s, p| {
            let c = clip_p(p, "clip").ok_or_else(|| bad("effects.toggleEnabled", "need `clip`"))?;
            let idx = u64_p(p, "index").unwrap_or(0) as usize;
            s.edit_sequence("Toggle Effect", |q, _, _| {
                let (_, it) = q.find_item_mut(c).ok_or(filmcraft_edit::EditError::NoItem(c))?;
                if let Some(e) = it.effects.get_mut(idx) {
                    e.enabled = !e.enabled;
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("effects.reset", "Reset Effect", [], None, r#"{"clip":id,"index":n}"#, has_seq, |s, p| {
            let c = clip_p(p, "clip").ok_or_else(|| bad("effects.reset", "need `clip`"))?;
            let idx = u64_p(p, "index").unwrap_or(0) as usize;
            let (fw, fh) = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
            s.edit_sequence("Reset Effect", |q, _, _| {
                let (_, it) = q.find_item_mut(c).ok_or(filmcraft_edit::EditError::NoItem(c))?;
                if let Some(e) = it.effects.get_mut(idx)
                    && let Some(d) = e.def()
                {
                    let mut fresh = d.instance();
                    resolve_auto_points(&mut fresh, (fw, fh), (fw, fh));
                    *e = fresh;
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!(
            "effects.addKeyframe",
            "Add/Remove Keyframe",
            [],
            None,
            r#"{"clip":id,"effect":str|index,"param":str,"mask":n?,"time":ticks?}"#,
            has_seq,
            |s, p| { keyframe_op(s, p, "add") }
        ),
        cmd!("effects.deleteKeyframe", "Delete Keyframe", [], None, r#"{"clip":id,"effect":str|index,"param":str,"mediaTime":ticks}"#, has_seq, |s, p| {
            keyframe_op(s, p, "delete")
        }),
        cmd!(
            "effects.moveKeyframe",
            "Move Keyframe",
            [],
            None,
            r#"{"clip":id,"effect":str|index,"param":str,"mediaTime":ticks,"to":ticks,"value":any?,"merge":bool?,"begin":bool?}"#,
            has_seq,
            |s, p| keyframe_op(s, p, "move")
        ),
        cmd!(
            "effects.setInterpolation",
            "Keyframe Interpolation",
            [],
            None,
            r#"{"clip":id,"effect":str|index,"param":str,"mediaTime":ticks,"interpolation":"linear|bezier|autoBezier|continuousBezier|hold|easeIn|easeOut"}"#,
            has_seq,
            |s, p| keyframe_op(s, p, "interp")
        ),
        cmd!(
            "effects.setKeyframe",
            "Edit Keyframe",
            [],
            None,
            r#"{"clip":id,"effect":str|index,"param":str,"mediaTime":ticks,"value":any?,"inInfluence":0..1?,"outInfluence":0..1?,"merge":bool?,"begin":bool?}"#,
            has_seq,
            |s, p| keyframe_op(s, p, "set")
        ),
        // ================= Project panel =================
        cmd!("project.select", "Select Project Items", [], None, r#"{"items":[id]}"#, always, |s, p| {
            s.state.project_selection =
                p.get("items").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect()).unwrap_or_default();
            Ok(Value::Null)
        }),
        cmd!("project.delete", "Clear", [], Some("Delete"), r#"{"items":[id]?}"#, has_project_selection, |s, p| {
            let asked: Vec<ItemId> = p
                .get("items")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect())
                .unwrap_or_else(|| s.state.project_selection.clone());
            // A bin named here goes with everything in it, as in the Project panel of any editor.
            let root = s.project.root.id;
            let named: Vec<filmcraft_project::BinId> =
                asked.iter().map(|i| filmcraft_project::BinId(i.0)).filter(|b| *b != root && s.project.root.find_bin(*b).is_some()).collect();
            // a bin inside another named bin goes with that one
            let bins: Vec<filmcraft_project::BinId> = named
                .iter()
                .copied()
                .filter(|b| !named.iter().any(|o| o != b && s.project.root.find_bin(*o).is_some_and(|o| o.find_bin(*b).is_some())))
                .collect();
            let mut items: Vec<ItemId> = asked.iter().copied().filter(|i| !named.contains(&filmcraft_project::BinId(i.0))).collect();
            for b in &bins {
                if let Some(bin) = s.project.root.find_bin(*b) {
                    bin.all_items(&mut items);
                }
            }
            items.sort_by_key(|i| i.0);
            items.dedup();
            s.edit("Clear", |pr, st| {
                for b in &bins {
                    pr.root.remove_bin(*b);
                }
                for i in &items {
                    pr.items.remove(i);
                    pr.root.remove_item(*i);
                }
                // remove track items that referenced deleted media
                let seq_ids: Vec<ItemId> = pr.sequences().map(|q| q.id).collect();
                for sid in seq_ids {
                    if let Some(q) = pr.sequence_mut(sid) {
                        for t in q.all_tracks_mut() {
                            t.items.retain(|x| !items.contains(&x.item));
                            filmcraft_edit::remove_orphan_transitions(t);
                        }
                    }
                }
                st.project_selection.clear();
                Ok(())
            })?;
            for i in &items {
                s.media.remove(*i);
            }
            s.fix_state();
            Ok(json!({"items": items.len(), "bins": bins.len()}))
        }),
        cmd!("project.moveToBin", "Move to Bin", [], None, r#"{"items":[id]?,"bin":binId|null}"#, always, |s, p| {
            let items: Vec<ItemId> = match p.get("items").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
                None => s.state.project_selection.clone(),
            };
            if items.is_empty() {
                return Err(bad("project.moveToBin", "need `items` (or a Project panel selection)"));
            }
            let bin = u64_p(p, "bin").map(filmcraft_project::BinId);
            let moved = s.edit("Move to Bin", |pr, _| pr.move_to_bin(&items, bin).ok_or_else(|| bad("project.moveToBin", "no such bin")))?;
            Ok(json!({"moved": moved}))
        }),
        cmd!("project.setMarks", "Set Source In/Out", [], None, r#"{"item":id,"in":ticks?|null,"out":ticks?|null}"#, always, |s, p| {
            let id = item_p(p, "item").or(s.state.source_item).ok_or_else(|| bad("project.setMarks", "need `item`"))?;
            let p = p.clone();
            s.edit("Mark", |pr, _| {
                let it = pr.item_mut(id).ok_or_else(|| bad("project.setMarks", "no such item"))?;
                // an ordinary In/Out replaces that side's split points
                if p.get("in").is_some() {
                    it.split.video_in = None;
                    it.split.audio_in = None;
                }
                if p.get("out").is_some() {
                    it.split.video_out = None;
                    it.split.audio_out = None;
                }
                if let Some(m) = it.as_media_mut() {
                    if let Some(v) = p.get("in") {
                        m.mark_in = v.as_i64().map(Tick);
                    }
                    if let Some(v) = p.get("out") {
                        m.mark_out = v.as_i64().map(Tick);
                    }
                } else if let Some(q) = it.as_sequence_mut() {
                    // a sequence loaded in the Source Monitor is marked like a clip: these are the
                    // sequence's own In and Out
                    if let Some(v) = p.get("in") {
                        q.mark_in = v.as_i64().map(Tick);
                        q.split.video_in = None;
                        q.split.audio_in = None;
                    }
                    if let Some(v) = p.get("out") {
                        q.mark_out = v.as_i64().map(Tick);
                        q.split.video_out = None;
                        q.split.audio_out = None;
                    }
                }
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        // ================= Queries =================
        query!("command.list", "List Commands", "{}", |s, _| {
            Ok(Value::Array(
                command_specs()
                    .iter()
                    .map(|c| {
                        let all: Vec<&str> = s.shortcuts.for_command(c.id).iter().map(|b| b.keys.as_str()).collect();
                        json!({"id": c.id, "label": c.label, "menu": c.menu, "shortcut": s.shortcuts.primary(c.id), "shortcuts": all, "defaultShortcut": c.shortcut, "params": c.params, "enabled": (c.enabled)(s).is_ok()})
                    })
                    .collect(),
            ))
        }),
        query!("project.inspect", "Inspect Project", "{}", |s, _| Ok(crate::commands::inspect_project(s))),
        query!("sequence.inspect", "Inspect Sequence", r#"{"item":id?}"#, |s, p| {
            let id = item_p(p, "item").or(s.state.active_sequence).ok_or(EngineError::NoSequence)?;
            let q = s.project.sequence(id).ok_or(EngineError::NoSequence)?;
            Ok(inspect_sequence(s, id, q))
        }),
        query!("state.inspect", "Inspect Editor State", "{}", |s, _| Ok(serde_json::to_value(&s.state).unwrap_or_default())),
        query!(
            "effects.list",
            "List Effects",
            r#"{"kind":"Video"|"Audio"|"VideoTransition"|"AudioTransition"?,"folder":"Video Transitions/Wipe"?,"detail":bool?}"#,
            |_, p| Ok(list_effects(p))
        ),
        query!("history.list", "List History", "{}", |s, _| Ok(
            json!({"undo": s.history.undo.iter().map(|h| &h.0).collect::<Vec<_>>(), "redo": s.history.redo.iter().map(|h| &h.0).collect::<Vec<_>>()})
        )),
    ];
    crate::sequence_tools::splice(&mut v);
    v.extend(crate::captions::commands());
    v.extend(crate::settings::commands());
    v.extend(crate::mixer::commands());
    v.extend(crate::multicam::commands());
    v.extend(crate::essential_sound::commands());
    v.extend(crate::color::commands());
    v.extend(crate::graphics::commands());
    v.extend(crate::graphic_templates::commands());
    v.extend(crate::shortcuts::commands());
    v.extend(crate::relink::commands());
    v.extend(crate::proxies::commands());
    v.extend(crate::project_manager::commands());
    v.extend(crate::masks::commands());
    v.extend(crate::presets::commands());
    v.extend(crate::export_tools::commands());
    v.extend(crate::transcript::commands());
    v.extend(crate::panels::commands());
    v.extend(crate::scopes::commands());
    v.extend(crate::remix::commands());
    v.extend(crate::voiceover::commands());
    v.extend(crate::narration::commands());
    v.extend(crate::keyboard::commands());
    v.extend(crate::project_panel::commands());
    v.extend(crate::media_browser::commands());
    // Edit ▸ Label ▸ <colour>, Paste Attributes, subclips, Video / Audio Options, Replace With Clip…
    // and their menu order
    crate::clip_ops::apply_layout(&mut v);
    // Search Bin, Find, Project Settings, Scene Edit Detection, Normalize Mix Track… (M3.11)
    crate::project_tools::apply_layout(&mut v);
    v.shrink_to_fit();
    v
}

fn keyframe_op(s: &mut Session, p: &Value, op: &str) -> Result<Value> {
    let c = clip_p(p, "clip").ok_or_else(|| bad("keyframe", "need `clip`"))?;
    let pid = str_p(p, "param").ok_or_else(|| bad("keyframe", "need `param`"))?.to_string();
    let eff = p.get("effect").cloned().unwrap_or(json!("motion"));
    let ph = time_p(s, p, "").unwrap_or(s.playhead());
    let mtime = p.get("mediaTime").and_then(Value::as_i64).map(Tick);
    let to = p.get("to").and_then(Value::as_i64).map(Tick);
    let interp = str_p(p, "interpolation").map(str::to_string);
    let label = match op {
        "add" => "Add Keyframe",
        "delete" => "Delete Keyframe",
        "move" => "Move Keyframe",
        "set" => "Edit Keyframe",
        _ => "Keyframe Interpolation",
    };
    // a keyframe dragged on the timeline is one undo step, like a dragged value
    let merge = bool_p(p, "merge").unwrap_or(false).then(|| format!("keyframe:{op}:{}:{eff}:{pid}:{}", c.0, p.get("mask").unwrap_or(&Value::Null)));
    if bool_p(p, "begin").unwrap_or(false) {
        s.history.merge_key = None;
    }
    s.edit_sequence_as(label, merge.as_deref(), |q, _, _| {
        let (_, it) = q.find_item_mut(c).ok_or(filmcraft_edit::EditError::NoItem(c))?;
        let mt_now = it.source_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
        let e = match &eff {
            Value::Number(n) => it.effects.get_mut(n.as_u64().unwrap_or(0) as usize),
            Value::String(sid) => it.effects.iter_mut().find(|e| &e.effect == sid),
            _ => None,
        }
        .ok_or_else(|| bad("keyframe", "no such effect"))?;
        let prm = crate::masks::target_param(e, p, &pid).ok_or_else(|| bad("keyframe", "no such param"))?;
        match op {
            "add" => {
                // toggle: remove if a keyframe sits at the playhead, else add
                if !prm.remove_keyframe_at(mt_now) {
                    prm.add_keyframe(mt_now);
                }
            }
            "delete" => {
                prm.remove_keyframe_at(mtime.ok_or_else(|| bad("keyframe", "need `mediaTime`"))?);
            }
            "move" => {
                let from = mtime.ok_or_else(|| bad("keyframe", "need `mediaTime`"))?;
                let to = to.ok_or_else(|| bad("keyframe", "need `to`"))?;
                if let Some(i) = prm.keyframes.iter().position(|k| k.time == from) {
                    let mut k = prm.keyframes.remove(i);
                    k.time = to;
                    if let Some(v) = p.get("value") {
                        k.value = json_to_param(&k.value, v).ok_or_else(|| bad("keyframe", "value has the wrong type"))?;
                    }
                    prm.keyframes.retain(|x| x.time != to);
                    let at = prm.keyframes.partition_point(|x| x.time < to);
                    prm.keyframes.insert(at, k);
                }
            }
            "set" => {
                let at = mtime.ok_or_else(|| bad("keyframe", "need `mediaTime`"))?;
                let k = prm.keyframes.iter_mut().find(|k| k.time == at).ok_or_else(|| bad("keyframe", "no keyframe at that time"))?;
                if let Some(v) = p.get("value") {
                    k.value = json_to_param(&k.value, v).ok_or_else(|| bad("keyframe", "value has the wrong type"))?;
                }
                if let Some(x) = f64_p(p, "inInfluence") {
                    k.in_influence = x.clamp(0.01, 1.0);
                }
                if let Some(x) = f64_p(p, "outInfluence") {
                    k.out_influence = x.clamp(0.01, 1.0);
                }
            }
            _ => {
                let at = mtime.unwrap_or(mt_now);
                let kind = match interp.as_deref().unwrap_or("linear").to_ascii_lowercase().as_str() {
                    "bezier" => filmcraft_project::Interpolation::Bezier,
                    "autobezier" => filmcraft_project::Interpolation::AutoBezier,
                    "continuousbezier" => filmcraft_project::Interpolation::ContinuousBezier,
                    "hold" => filmcraft_project::Interpolation::Hold,
                    "easein" => filmcraft_project::Interpolation::EaseIn,
                    "easeout" => filmcraft_project::Interpolation::EaseOut,
                    _ => filmcraft_project::Interpolation::Linear,
                };
                let k = prm.keyframes.iter_mut().find(|k| k.time == at).ok_or_else(|| bad("keyframe", "no keyframe at that time"))?;
                k.interp = kind;
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn in_out_range(s: &Session) -> Result<TimeRange> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let fd = q.settings.frame_rate.frame_duration();
    let a = q.mark_in.unwrap_or(Tick::ZERO);
    let b = q.mark_out.map(|o| o + fd).unwrap_or(q.duration());
    if b <= a {
        return Err(EngineError::Other("In point is after Out point".into()));
    }
    Ok(TimeRange::from_bounds(a, b))
}

fn mark(s: &mut Session, p: &Value, is_in: bool) -> Result<Value> {
    let target_source = str_p(p, "target") == Some("source");
    if target_source {
        let item = s.state.source_item.ok_or_else(|| EngineError::Other("no source clip".into()))?;
        let t = p.get("time").and_then(Value::as_i64).map(Tick).unwrap_or(s.state.source_playhead);
        let key = if is_in { "in" } else { "out" };
        return s.execute("project.setMarks", json!({"item": item.0, key: t.0}));
    }
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    s.edit_sequence(if is_in { "Mark In" } else { "Mark Out" }, |q, _, _| {
        if is_in {
            q.mark_in = Some(t);
            q.split.video_in = None;
            q.split.audio_in = None;
            if q.mark_out.is_some_and(|o| o < t) {
                q.mark_out = None;
            }
        } else {
            q.mark_out = Some(t);
            q.split.video_out = None;
            q.split.audio_out = None;
            if q.mark_in.is_some_and(|i| i > t) {
                q.mark_in = None;
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

pub(crate) fn json_to_param(template: &ParamValue, v: &Value) -> Option<ParamValue> {
    Some(match template {
        ParamValue::Float(_) => ParamValue::Float(v.as_f64()?),
        ParamValue::Vec2(_) => {
            let a = v.as_array()?;
            ParamValue::Vec2(filmcraft_geom::Vec2::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?))
        }
        ParamValue::Color(_) => match v {
            Value::String(s) => ParamValue::Color(filmcraft_color::parse_hex(s)?),
            Value::Array(a) => ParamValue::Color([
                a.first()?.as_f64()? as f32,
                a.get(1)?.as_f64()? as f32,
                a.get(2)?.as_f64()? as f32,
                a.get(3).and_then(Value::as_f64).unwrap_or(1.0) as f32,
            ]),
            _ => return None,
        },
        ParamValue::Bool(_) => ParamValue::Bool(v.as_bool()?),
        ParamValue::Choice(_) => ParamValue::Choice(v.as_u64()? as u32),
        ParamValue::Text(_) => ParamValue::Text(v.as_str()?.to_string()),
        ParamValue::Path(_) => ParamValue::Path(crate::masks::path_from_json(v)?),
        ParamValue::Curve(_) => {
            ParamValue::Curve(v.as_array()?.iter().filter_map(|p| Some([p.get(0)?.as_f64()? as f32, p.get(1)?.as_f64()? as f32])).collect())
        }
    })
}

fn copy_selection(s: &mut Session) {
    let Some(q) = s.active_sequence() else { return };
    let sel = with_links(s, &s.state.selection);
    let min = sel.iter().filter_map(|c| q.find_item(*c).map(|(_, i)| i.start)).min().unwrap_or_default();
    let max = sel.iter().filter_map(|c| q.find_item(*c).map(|(_, i)| i.end())).max().unwrap_or_default();
    // Copy Paste Includes Sequence Markers: the markers inside the copied span go along
    let markers: Vec<Marker> = if s.state.copy_paste_sequence_markers {
        q.markers.iter().filter(|m| m.start >= min && m.start < max).map(|m| Marker { start: m.start - min, ..m.clone() }).collect()
    } else {
        Vec::new()
    };
    let mut clip = Vec::new();
    for (kind, tracks) in [(TrackKind::Video, &q.video_tracks), (TrackKind::Audio, &q.audio_tracks)] {
        for (ti, t) in tracks.iter().enumerate() {
            for i in t.items.iter().filter(|i| sel.contains(&i.id)) {
                let mut c = i.clone();
                c.start -= min;
                clip.push((kind, ti, c));
            }
        }
    }
    s.state.clipboard = clip;
    s.state.clipboard_markers = markers;
}

fn paste(s: &mut Session, insert: bool) -> Result<Value> {
    let at = s.playhead();
    let clip = s.state.clipboard.clone();
    let markers = if s.state.copy_paste_sequence_markers { s.state.clipboard_markers.clone() } else { Vec::new() };
    let end = s.edit_sequence(if insert { "Paste Insert" } else { "Paste" }, |q, ctx, st| {
        let mut placements = Vec::new();
        let mut links = std::collections::HashMap::new();
        for (kind, ti, item) in clip {
            let tracks = q.tracks(kind);
            let Some(t) = tracks.get(ti).or(tracks.last()) else { continue };
            let mut it = item.clone();
            it.id = ClipId(ctx.alloc());
            it.start += at;
            if let Some(l) = it.link {
                it.link = Some(*links.entry(l).or_insert_with(|| ctx.alloc()));
            }
            placements.push((t.id, it));
        }
        let end = placements.iter().map(|p| p.1.end()).max().unwrap_or(at);
        let ids = if insert { edit::insert(q, placements, ctx)? } else { edit::overwrite(q, placements, ctx)? };
        if insert && st.ripple_sequence_markers {
            crate::sequence_tools::ripple_markers(&mut q.markers, at, end - at);
        }
        for m in &markers {
            q.markers.push(Marker { id: MarkerId(ctx.alloc()), start: m.start + at, ..m.clone() });
        }
        q.markers.sort_by_key(|m| m.start);
        st.selection = ids;
        Ok(end)
    })?;
    s.set_playhead(end);
    Ok(Value::Null)
}

/// Select `item` in the Project panel and ask the UI to show it there.
fn reveal_in_project(s: &mut Session, item: ItemId) -> Result<Value> {
    s.project.item(item).ok_or_else(|| EngineError::Other("that clip's source is not in the project".into()))?;
    s.state.project_selection = vec![item];
    s.events.push(crate::Event::RevealInProject(item));
    Ok(json!({"item": item.0}))
}

/// The name Nest… offers: the first unused "Nested Sequence NN".
pub fn next_nested_name(s: &Session) -> String {
    (1..=s.project.items.len().saturating_add(1))
        .map(|k| format!("Nested Sequence {k:02}"))
        .find(|n| !s.project.items.values().any(|i| &i.name == n))
        .unwrap_or_else(|| "Nested Sequence".to_string())
}

fn nest(s: &mut Session, p: &Value) -> Result<Value> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let sel = with_links(s, &s.state.selection.clone());
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let name = str_p(p, "name").filter(|n| !n.trim().is_empty()).map(str::to_string).unwrap_or_else(|| next_nested_name(s));
    let mut items: Vec<(TrackKind, usize, filmcraft_project::TrackItem)> = Vec::new();
    // transitions go into the nest with their clips: those whose clips are all nested
    let mut transitions: Vec<(TrackKind, usize, filmcraft_project::Transition)> = Vec::new();
    for (k, ts) in [(TrackKind::Video, &q.video_tracks), (TrackKind::Audio, &q.audio_tracks)] {
        // clips on a locked track stay where they are (they cannot be removed from it)
        for (ti, t) in ts.iter().enumerate().filter(|(_, t)| !t.locked) {
            for i in t.items.iter().filter(|i| sel.contains(&i.id)) {
                items.push((k, ti, i.clone()));
            }
            let nested = |c: Option<ClipId>| c.is_none_or(|c| sel.contains(&c));
            for trn in t.transitions.iter().filter(|x| (x.from.is_some() || x.to.is_some()) && nested(x.from) && nested(x.to)) {
                transitions.push((k, ti, trn.clone()));
            }
        }
    }
    if items.is_empty() {
        return Err(EngineError::Other("nothing selected".into()));
    }
    let start = items.iter().map(|i| i.2.start).min().unwrap_or_default();
    let end = items.iter().map(|i| i.2.end()).max().unwrap_or_default();
    let span = TimeRange::new(start, end - start);
    let tracks_of = |kind: TrackKind| -> Vec<usize> {
        let mut v: Vec<usize> = items.iter().filter(|i| i.0 == kind).map(|i| i.1).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let (video_tracks, audio_tracks) = (tracks_of(TrackKind::Video), tracks_of(TrackKind::Audio));
    // Premiere (26.5.2): the nest is a linked picture + sound pair only when the selected sound is
    // on one track and that track is free for the nest's whole length. With sound on several
    // tracks, or another clip in the way on its track, the nest is picture only: the selected sound
    // stays where it is in this sequence, and a copy of it goes into the nest.
    let sound_blocked = |ai: usize| q.audio_tracks.get(ai).is_none_or(|t| t.items.iter().any(|i| !sel.contains(&i.id) && i.range().overlaps(&span)));
    let video_only = !video_tracks.is_empty() && (audio_tracks.len() > 1 || audio_tracks.first().is_some_and(|ai| sound_blocked(*ai)));
    let (lowest_v, lowest_a) = (video_tracks.first().copied(), audio_tracks.first().copied().filter(|_| !video_only));
    // In the nest, picture tracks keep their positions (V1 up to the highest one used). Sound
    // tracks do not: A1 is always there, and the other tracks used follow it in order
    // (A1 + A3 become A1, A2; A3 alone becomes an empty A1 and A2).
    let nv = video_tracks.last().map_or(1, |i| i + 1);
    let audio_at: std::collections::HashMap<usize, usize> =
        audio_tracks.iter().filter(|i| **i != 0).enumerate().map(|(n, i)| (*i, n + 1)).chain(std::iter::once((0, 0))).collect();
    let na = audio_at.values().max().map_or(1, |i| i + 1);
    let nested_track = |k: TrackKind, ti: usize| if k == TrackKind::Audio { audio_at.get(&ti).copied().unwrap_or(ti) } else { ti };
    // what leaves this sequence: everything nested, less the sound that stays behind
    let removed: Vec<ClipId> = items.iter().filter(|i| !(video_only && i.0 == TrackKind::Audio)).map(|i| i.2.id).collect();
    let stays = |k: TrackKind| video_only && k == TrackKind::Audio;
    let r = s.edit("Nest", |pr, st| {
        let nid = pr.new_sequence(&name, q.settings.clone(), nv, na, None);
        // copies of clips and transitions that also stay behind need ids of their own
        let fresh_clips: std::collections::HashMap<ClipId, ClipId> = items.iter().filter(|i| stays(i.0)).map(|i| (i.2.id, ClipId(pr.alloc_id()))).collect();
        let fresh_transitions: Vec<Option<u64>> = transitions.iter().map(|t| stays(t.0).then(|| pr.alloc_id())).collect();
        let copy = |c: ClipId| fresh_clips.get(&c).copied().unwrap_or(c);
        {
            let nq = pr.sequence_mut(nid).ok_or(EngineError::NoSequence)?;
            // tracks take the name and channel layout of the track their clips came from
            for (tr, src) in nq.video_tracks.iter_mut().zip(&q.video_tracks) {
                tr.name = src.name.clone();
            }
            for (from, to) in &audio_at {
                if let (Some(tr), Some(src)) = (nq.audio_tracks.get_mut(*to), q.audio_tracks.get(*from)) {
                    tr.channels = src.channels;
                    if from == to {
                        tr.name = src.name.clone();
                    }
                }
            }
            for (k, ti, it) in &items {
                let mut it = it.clone();
                it.id = copy(it.id);
                it.start -= start;
                nq.tracks_mut(*k).get_mut(nested_track(*k, *ti)).ok_or(EngineError::NoSequence)?.items.push(it);
            }
            for ((k, ti, trn), fresh) in transitions.iter().zip(&fresh_transitions) {
                let mut trn = trn.clone();
                if let Some(id) = fresh {
                    trn.id = filmcraft_project::TransitionId(*id);
                }
                (trn.from, trn.to) = (trn.from.map(copy), trn.to.map(copy));
                trn.start -= start;
                nq.tracks_mut(*k).get_mut(nested_track(*k, *ti)).ok_or(EngineError::NoSequence)?.transitions.push(trn);
            }
            for t in nq.all_tracks_mut() {
                t.sort();
                edit::remove_orphan_transitions(t);
            }
            nq.check().map_err(EngineError::Other)?;
        }
        let rate = q.settings.frame_rate;
        // the nest shows its sequence from the beginning, for the length of what was nested
        let whole = TimeRange::new(Tick::ZERO, span.duration);
        let mut v =
            pr.make_track_item(nid, TrackKind::Video, start, whole, rate).ok_or_else(|| EngineError::Other("cannot place the nested sequence".into()))?;
        for e in &mut v.effects {
            resolve_auto_points(e, (q.settings.width, q.settings.height), (q.settings.width, q.settings.height));
        }
        let a = pr.make_track_item(nid, TrackKind::Audio, start, whole, rate).ok_or_else(|| EngineError::Other("cannot place the nested sequence".into()))?;
        let (link, new_video, new_audio) = (pr.alloc_id(), pr.alloc_id(), pr.alloc_id());
        let seq = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        edit::delete_items(seq, &removed);
        // sound left behind has lost its picture: it is no longer linked
        if video_only {
            let links: Vec<u64> = items.iter().filter(|i| i.0 == TrackKind::Video).filter_map(|i| i.2.link).collect();
            for it in seq.audio_tracks.iter_mut().flat_map(|t| t.items.iter_mut()) {
                if it.link.is_some_and(|l| links.contains(&l)) {
                    it.link = None;
                }
            }
        }
        // Where the nest goes, as Premiere Pro 26.5.2 places it (about 35 nests observed). With L
        // the lowest video track of the selection and `a` the number of the highest audio track
        // holding selected sound, the picture takes the lowest track T from L up such that
        //   1. T is free of other clips for the nest's whole length, and
        //   2. so is every video track numbered from T up to a - 1 that the selection had clips on.
        // If no track will do, a new one is added on top. So the nest never lands on another clip,
        // and it can be pushed above an unselected clip that shares a track with the selection
        // when the sound sits on a higher-numbered track. The sound of a linked nest is on the one
        // track its clips came from, which is free (a selection of sound alone moves up to the
        // first free track).
        let highest_sound = audio_tracks.last().map_or(0, |i| i + 1);
        let mut ids = Vec::new();
        for (kind, lowest, mut clip, new_track) in [(TrackKind::Video, lowest_v, v, new_video), (TrackKind::Audio, lowest_a, a, new_audio)] {
            let Some(lowest) = lowest else { continue };
            let tracks = seq.tracks_mut(kind);
            let clear = |i: usize| tracks.get(i).is_some_and(|t| !t.locked && edit::track_range_empty(t, span));
            let selection_clear = |t: usize| kind != TrackKind::Video || video_tracks.iter().filter(|i| **i >= t && **i + 1 < highest_sound).all(|i| clear(*i));
            let free = (lowest..tracks.len()).find(|t| clear(*t) && selection_clear(*t));
            let at = match free {
                Some(i) => i,
                None => {
                    let (word, n) = (if kind == TrackKind::Video { "Video" } else { "Audio" }, tracks.len() + 1);
                    tracks.push(filmcraft_project::Track::new(TrackId(new_track), kind, format!("{word} {n}")));
                    tracks.len() - 1
                }
            };
            clip.link = (lowest_v.is_some() && lowest_a.is_some()).then_some(link);
            ids.push(clip.id);
            let t = tracks.get_mut(at).ok_or(EngineError::NoSequence)?;
            t.items.push(clip);
            t.sort();
        }
        seq.check().map_err(EngineError::Other)?;
        // like Premiere: nothing stays selected in the timeline; the new sequence is selected in the Project panel
        st.selection.clear();
        st.project_selection = vec![nid];
        Ok((nid, ids))
    });
    r.map(|(n, ids)| json!({"sequence": n.0, "clips": ids.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

/// `effects.list`: every effect definition with its Effects-panel folder (optionally filtered by
/// kind or folder prefix); `detail` adds each parameter's label, type, default and choices.
fn list_effects(p: &Value) -> Value {
    let kind = str_p(p, "kind");
    let folder = str_p(p, "folder");
    let detail = p.get("detail").and_then(Value::as_bool).unwrap_or(false);
    let out = filmcraft_project::effect_defs()
        .iter()
        .filter(|d| kind.is_none_or(|k| format!("{:?}", d.kind).eq_ignore_ascii_case(k)))
        .filter(|d| folder.is_none_or(|f| d.category.join("/").starts_with(f)))
        .map(|d| {
            let mut v = json!({
                "id": d.id,
                "name": d.name,
                "kind": format!("{:?}", d.kind),
                "category": d.category,
                "folder": d.category.get(1),
                "path": d.category.join("/"),
                "params": d.params.iter().map(|p| p.id).collect::<Vec<_>>(),
                "badges": {"accelerated": d.accelerated, "float32": d.float32, "yuv": d.yuv},
            });
            if detail {
                v["paramInfo"] = Value::Array(d.params.iter().map(param_info).collect());
            }
            v
        })
        .collect();
    Value::Array(out)
}

fn param_info(p: &filmcraft_project::ParamDef) -> Value {
    use filmcraft_project::ParamKind as K;
    let mut j = json!({"id": p.id, "label": p.label, "animatable": p.animatable, "default": serde_json::to_value(&p.default).unwrap_or_default()});
    let ty = match &p.kind {
        K::Choice(opts) => {
            j["options"] = json!(opts);
            "choice"
        }
        K::Float { min, max, unit, .. } => {
            j["min"] = json!(min);
            j["max"] = json!(max);
            j["unit"] = json!(unit);
            "float"
        }
        K::Point => "point",
        K::Color => "color",
        K::Bool => "bool",
        K::Angle => "angle",
        K::Text => "text",
        K::Curve { .. } => "curve",
        K::Wheel => "wheel",
        K::Path => "path",
    };
    j["type"] = json!(ty);
    j
}

/// Set transition parameters from a JSON object (`{"direction": 1}` or `{"direction": "From East"}`,
/// colours as `"#rrggbb"` or `[r,g,b]`, points as `[x,y]`). Floats are clamped to their range.
fn set_transition_params(e: &mut filmcraft_project::EffectInstance, params: &Value, cmd: &str) -> Result<()> {
    use filmcraft_project::{ParamKind as K, ParamValue as V};
    let Some(obj) = params.as_object() else { return Err(bad(cmd, "`params` must be an object")) };
    let def = e.def().ok_or_else(|| bad(cmd, "unknown transition"))?;
    for (k, v) in obj {
        let pd = def.param(k).ok_or_else(|| bad(cmd, format!("`{}` has no param `{k}`", def.name)))?;
        let val = match (&pd.kind, v) {
            (K::Choice(opts), Value::String(sv)) => {
                V::Choice(opts.iter().position(|o| o.eq_ignore_ascii_case(sv)).ok_or_else(|| bad(cmd, format!("`{k}` must be one of {opts:?}")))? as u32)
            }
            _ => json_to_param(&pd.default, v).ok_or_else(|| bad(cmd, format!("`{k}`: value has the wrong type")))?,
        };
        let val = match (&pd.kind, val) {
            (K::Choice(opts), V::Choice(c)) if c as usize >= opts.len() => return Err(bad(cmd, format!("`{k}` must be < {}", opts.len()))),
            (K::Float { min, max, .. }, V::Float(f)) => V::Float(f.clamp(*min, *max)),
            (_, v) => v,
        };
        match e.param_mut(k) {
            Some(prm) => prm.value = val,
            None => {
                e.params.insert(k.clone(), filmcraft_project::Param::new(val));
            }
        }
    }
    Ok(())
}

/// A JSON list of transition ids (or one id).
fn transition_ids_p(v: &Value) -> Option<Vec<TransitionId>> {
    match v {
        Value::Array(a) => a.iter().map(|x| x.as_u64().map(TransitionId)).collect(),
        x => x.as_u64().map(|i| vec![TransitionId(i)]),
    }
}

/// `timeline.select {"transitions": […]}`: select transitions (clicking one in the Timeline). It
/// replaces the clip, edit point and caption selections, as a clip selection replaces it.
fn select_transitions(s: &mut Session, ids: &Value, add: bool, toggle: bool) -> Result<Value> {
    const CMD: &str = "timeline.select";
    let ids = transition_ids_p(ids).ok_or_else(|| bad(CMD, "`transitions` is a list of transition ids"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if let Some(missing) = ids.iter().find(|id| !q.all_tracks().any(|t| t.transitions.iter().any(|x| x.id == **id))) {
        return Err(bad(CMD, format!("no transition {}", missing.0)));
    }
    if toggle {
        for id in ids {
            match s.state.transition_selection.iter().position(|x| *x == id) {
                Some(i) => {
                    s.state.transition_selection.remove(i);
                }
                None => s.state.transition_selection.push(id),
            }
        }
    } else if add {
        for id in ids {
            if !s.state.transition_selection.contains(&id) {
                s.state.transition_selection.push(id);
            }
        }
    } else {
        s.state.transition_selection = ids;
    }
    s.state.selection.clear();
    s.state.caption_selection.clear();
    s.state.edit_points.clear();
    s.state.trim_shift = Default::default();
    Ok(json!({"selection": [], "transitionSelection": s.state.transition_selection.iter().map(|t| t.0).collect::<Vec<_>>()}))
}

/// Remove transitions in one undo step and drop them from the selection.
fn remove_transitions(s: &mut Session, ids: &[TransitionId], label: &str) -> Result<Value> {
    let ids = ids.to_vec();
    let n = s.edit_sequence(label, |q, _, st| {
        let n = edit::transitions::remove(q, &ids)?;
        st.transition_selection.retain(|x| !ids.contains(x));
        Ok(n)
    })?;
    Ok(json!({"removed": n}))
}

/// `sequence.setTransition`: edit an applied transition (Effect Controls, the Timeline): its
/// settings, Reverse, and where it sits. `duration` / `frames` resize it the way its alignment
/// says (Center at Cut and Custom Start: both ends; Start at Cut: the end; End at Cut: the
/// beginning), `align` re-aligns it on its cut, `start` moves it (dragging it over the cut; with
/// `duration`, dragging one edge). One undo step; with `merge`, a drag's steps share one.
fn set_transition(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "sequence.setTransition";
    let id = p.get("transition").and_then(Value::as_u64).ok_or_else(|| bad(CMD, "need `transition`"))?;
    let tid = TransitionId(id);
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let rate = q.settings.frame_rate.sane();
    let frame = rate.frame_duration();
    let found = q.all_tracks().find_map(|tr| tr.transitions.iter().find(|x| x.id == tid).map(|x| (tr, x)));
    let (track, cur) = found.ok_or_else(|| bad(CMD, format!("no transition {id}")))?;
    let mut cur = cur.clone();
    let (lo, cut, hi) = edit::transitions::bounds(track, &cur).ok_or_else(|| bad(CMD, "the transition's clips are gone"))?;
    // times outside the clips it joins are refused before any frame arithmetic (hostile input)
    let room = Tick(hi.0.saturating_sub(lo.0));
    let duration = match (p.get("frames").and_then(Value::as_i64), p.get("duration").and_then(Value::as_i64)) {
        (Some(_), Some(_)) => return Err(bad(CMD, "give `duration` or `frames`, not both")),
        (Some(f), None) if (1..=10_000_000).contains(&f) && rate.tick_of(f) <= room => Some(rate.tick_of(f)),
        (None, Some(d)) if d > 0 && d <= room.0 => Some(rate.snap_nearest(Tick(d)).max(frame)),
        (Some(_), None) | (None, Some(_)) => return Err(bad(CMD, "a transition lasts at least one frame and can't extend past the clips it joins")),
        (None, None) => None,
    };
    let align = match str_p(p, "align") {
        Some(a) => Some(edit::transitions::Alignment::parse(a).ok_or_else(|| bad(CMD, format!("`align` is center, start or end, not `{a}`")))?),
        None => None,
    };
    let start = match p.get("start").and_then(Value::as_i64) {
        Some(t) if (lo.0..=hi.0).contains(&t) => Some(rate.snap_nearest(Tick(t))),
        Some(_) => return Err(bad(CMD, "a transition can't extend past the clips it joins")),
        None => None,
    };
    if start.is_some() && align.is_some() {
        return Err(bad(CMD, "give `start` or `align`, not both"));
    }
    let span = if duration.is_some() || align.is_some() || start.is_some() {
        let d = duration.unwrap_or(cur.duration);
        let st = match start {
            Some(st) => st,
            None => {
                let al = align.unwrap_or_else(|| edit::transitions::alignment(&cur, cut, frame));
                edit::transitions::start_for(q, tid, d, al, frame, |t| rate.snap(t)).ok_or_else(|| bad(CMD, format!("no transition {id}")))?
            }
        };
        Some((st, d))
    } else {
        None
    };
    if p.get("reset").and_then(Value::as_bool).unwrap_or(false)
        && let Some(def) = cur.effect.def()
    {
        cur.effect = def.instance();
    }
    if let Some(params) = p.get("params") {
        set_transition_params(&mut cur.effect, params, CMD)?;
    }
    if let Some(r) = p.get("reverse").and_then(Value::as_bool) {
        cur.reverse = r;
    }
    let name = cur.effect.def().map_or("Transition", |d| d.name).to_string();
    if bool_p(p, "begin").unwrap_or(false) {
        s.history.merge_key = None;
    }
    let merge = bool_p(p, "merge").unwrap_or(false).then(|| format!("setTransition:{id}"));
    let after = s.edit_sequence_as(&format!("Edit {name}"), merge.as_deref(), |q, _, _| {
        if let Some((st, d)) = span {
            edit::transitions::set_span(q, tid, st, d, frame, frame)?;
        }
        let mut after = None;
        for tr in q.video_tracks.iter_mut().chain(q.audio_tracks.iter_mut()) {
            if let Some(x) = tr.transitions.iter_mut().find(|x| x.id == tid) {
                x.effect = cur.effect.clone();
                x.reverse = cur.reverse;
                after = Some((x.start, x.duration));
            }
        }
        Ok(after)
    })?;
    let (st, d) = after.unwrap_or((cur.start, cur.duration));
    let al = s
        .active_sequence()
        .and_then(|q| {
            q.all_tracks().find_map(|tr| tr.transitions.iter().find(|x| x.id == tid).and_then(|x| edit::transitions::bounds(tr, x).map(|b| (x.clone(), b.1))))
        })
        .map(|(x, cut)| edit::transitions::alignment(&x, cut, frame).id());
    Ok(json!({"transition": id, "effect": cur.effect.effect, "reverse": cur.reverse, "start": st.0, "duration": d.0, "align": al}))
}

fn apply_transition(s: &mut Session, p: &Value, kind: TrackKind) -> Result<Value> {
    let (track, tr) = preview_transition(s, p, kind)?;
    let label = format!("Apply {}", tr.effect.def().map_or("Transition", |d| d.name));
    let id = s.edit_sequence(&label, |q, ctx, _| Ok(edit::add_transition(q, track, tr, ctx)?))?;
    Ok(json!({"transition": id.0}))
}

/// Plan an applied transition without editing the project or allocating an id.
/// Uses the same `effect`, `clip`, `edge`, `frames` and settings as transition application,
/// so a drag preview shows the exact range that release will commit.
pub fn preview_transition(s: &Session, p: &Value, kind: TrackKind) -> Result<(TrackId, Transition)> {
    let eff_id = str_p(p, "effect")
        .map(str::to_string)
        .unwrap_or_else(|| if kind == TrackKind::Video { s.state.default_video_transition.clone() } else { s.state.default_audio_transition.clone() });
    let ekind = if kind == TrackKind::Video { filmcraft_project::EffectKind::VideoTransition } else { filmcraft_project::EffectKind::AudioTransition };
    let def = filmcraft_project::vtransition::find_transition(&eff_id, ekind).ok_or_else(|| bad("transition", format!("unknown transition `{eff_id}`")))?;
    let mut instance = def.instance();
    if let Some(params) = p.get("params") {
        set_transition_params(&mut instance, params, "transition")?;
    }
    let reverse = p.get("reverse").and_then(Value::as_bool).unwrap_or(false);
    let rate = s.sequence_rate();
    // Settings ▸ Timeline ▸ Video / Audio Transition Default Duration
    let dur = match p.get("frames").and_then(Value::as_i64) {
        Some(frames) => rate.tick_of(frames),
        None if kind == TrackKind::Video => s.prefs.timeline.video_transition_duration(rate),
        None => s.prefs.timeline.audio_transition_duration(rate),
    };
    let t = s.playhead();
    let clip = clip_p(p, "clip");
    let edge = str_p(p, "edge").map(str::to_string);
    let targeted = s.targeting().targeted;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // Find (track, from, to, cut) at the playhead (nearest edit on targeted tracks) or at a clip edge.
    let mut found = None;
    for tr in q.tracks(kind).iter().filter(|tr| clip.is_some() || targeted.contains(&tr.id)) {
        let items = &tr.items;
        for (i, it) in items.iter().enumerate() {
            if let Some(c) = clip
                && it.id != c
            {
                continue;
            }
            let prev = i.checked_sub(1).map(|j| &items[j]).filter(|pv| pv.end() == it.start);
            let next = items.get(i + 1).filter(|nx| nx.start == it.end());
            let use_in = match edge.as_deref() {
                Some("in") => true,
                Some("out") => false,
                _ => (t - it.start).abs() <= (t - it.end()).abs(),
            };
            if clip.is_none() && !(it.start == t || it.end() == t || (it.start - t).abs() < rate.tick_of(3) || (it.end() - t).abs() < rate.tick_of(3)) {
                continue;
            }
            found = Some(if use_in { (tr.id, prev.map(|x| x.id), Some(it.id), it.start) } else { (tr.id, Some(it.id), next.map(|x| x.id), it.end()) });
            break;
        }
        if found.is_some() {
            break;
        }
    }
    let (track, from, to, cut) = found.ok_or_else(|| EngineError::Other("no edit point at the playhead on targeted tracks".into()))?;
    if q.track(track).is_some_and(|tr| tr.locked) {
        return Err(edit::EditError::Locked.into());
    }
    let start = if from.is_some() && to.is_some() {
        cut - dur.mul_ratio(1, 2)
    } else if to.is_some() {
        cut
    } else {
        cut - dur
    };
    let tr = Transition { id: TransitionId(0), effect: instance, start: rate.snap(start), duration: dur, from, to, align: Default::default(), reverse };
    Ok((track, tr))
}

/// JSON description of the project (bins, items) for agents.
pub fn inspect_project(s: &Session) -> Value {
    let p = &s.project;
    fn bin(b: &filmcraft_project::Bin, p: &filmcraft_project::Project) -> Value {
        json!({
            "id": b.id.0,
            "name": b.name,
            "children": b.children.iter().map(|c| match c {
                filmcraft_project::BinEntry::Item(i) => {
                    let it = p.item(*i);
                    json!({"item": i.0, "name": it.map(|x| x.name.clone()), "type": it.map(|x| x.type_label()), "duration": it.map(|x| x.duration().0), "label": it.map(|x| x.label.name())})
                }
                filmcraft_project::BinEntry::Bin(b) => bin(b, p),
            }).collect::<Vec<_>>()
        })
    }
    json!({
        "name": p.name,
        "path": s.path,
        "dirty": s.is_dirty(),
        "revision": s.revision,
        "root": bin(&p.root, p),
        "activeSequence": s.state.active_sequence.map(|i| i.0),
        "sourceItem": s.state.source_item.map(|i| i.0),
    })
}

pub fn inspect_sequence(s: &Session, id: ItemId, q: &filmcraft_project::Sequence) -> Value {
    let rate = q.settings.frame_rate;
    let tr = |t: &filmcraft_project::Track| {
        json!({
            "id": t.id.0, "name": t.name, "locked": t.locked, "syncLock": t.sync_lock, "enabled": t.enabled, "muted": t.muted, "solo": t.solo,
            "items": t.items.iter().map(|i| json!({
                "clip": i.id.0, "item": i.item.0, "name": i.name, "start": i.start.0, "duration": i.duration.0,
                "startFrame": rate.frame_at(i.start), "durationFrames": rate.frame_at(i.duration), "sourceIn": i.source_in.0, "speed": i.speed, "reverse": i.reverse,
                "end": i.end().0, "endFrame": rate.frame_at(i.end()), "sourceOut": i.source_out().0, "gainDb": i.gain_db,
                "enabled": i.enabled, "link": i.link, "label": i.label.name(),
                "effects": i.effects.iter().map(|e| json!({"effect": e.effect, "enabled": e.enabled, "masks": e.masks.len(), "params": e.params.iter().map(|(k, p)| (k.clone(), json!({"value": format!("{:?}", p.value), "keyframes": p.keyframes.len()}))).collect::<serde_json::Map<_, _>>()})).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "transitions": t.transitions.iter().map(|x| json!({"id": x.id.0, "effect": x.effect.effect, "start": x.start.0, "duration": x.duration.0, "from": x.from.map(|c| c.0), "to": x.to.map(|c| c.0), "reverse": x.reverse, "align": filmcraft_edit::transitions::bounds(t, x).map(|b| filmcraft_edit::transitions::alignment(x, b.1, rate.frame_duration()).id()), "params": x.effect.params.iter().map(|(k, v)| (k.clone(), serde_json::to_value(&v.value).unwrap_or_default())).collect::<serde_json::Map<_, _>>()})).collect::<Vec<_>>(),
        })
    };
    json!({
        "id": id.0,
        "name": s.project.item(id).map(|i| i.name.clone()),
        "settings": q.settings,
        "duration": q.duration().0,
        "durationFrames": rate.frame_at(q.duration()),
        "playhead": s.state.playheads.get(&id).map(|t| t.0),
        "in": q.mark_in.map(|t| t.0),
        "out": q.mark_out.map(|t| t.0),
        "split": q.split,
        "markers": q.markers.iter().map(|m| json!({"id": m.id.0, "start": m.start.0, "duration": m.duration.0, "kind": format!("{:?}", m.kind), "name": m.name, "comment": m.comment, "color": m.color.name()})).collect::<Vec<_>>(),
        "video": q.video_tracks.iter().map(tr).collect::<Vec<_>>(),
        "audio": q.audio_tracks.iter().map(tr).collect::<Vec<_>>(),
        "selection": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>(),
        "transitionSelection": s.state.transition_selection.iter().map(|t| t.0).collect::<Vec<_>>(),
    })
}

/// Seconds → ticks helper for callers.
pub fn secs(s: f64) -> Tick {
    Tick((s * TICKS_PER_SECOND as f64).round() as i64)
}

// ---------- project files ----------

/// Serialize the project in the current schema and write it (atomically) to `path`. `adopt` =
/// Save / Save As (the file becomes the project's path and the project is clean); otherwise Save a
/// Copy. The first save over a file upgraded from an older schema keeps the original as
/// `<name> (schema vN backup).fcproj`.
/// A project is named after its file (as in Premiere): `/a/b/Trailer v2.fcproj` → `Trailer v2`.
fn project_name_for(path: &str) -> Option<String> {
    let stem = std::path::Path::new(path).file_stem()?.to_string_lossy().to_string();
    (!stem.is_empty()).then_some(stem)
}

fn name_project(p: &mut filmcraft_project::Project, name: String) {
    p.root.name.clone_from(&name);
    p.name = name;
}

fn write_project(s: &mut Session, path: &str, adopt: bool) -> Result<Value> {
    let t0 = web_time::Instant::now();
    // Save As renames the project after its new file (not an edit: no undo step, not dirty).
    if adopt
        && let Some(name) = project_name_for(path)
        && s.project.name != name
    {
        name_project(std::sync::Arc::make_mut(&mut s.project), name);
    }
    // what is open (sequence tabs, how each is shown) goes into the file beside the project
    let view = s.project_view();
    let bytes = filmcraft_format::encode_with_view(&s.project, Some(&view), false);
    let mut backup = None;
    if adopt && s.path.as_deref() == Some(path) && s.loaded_schema < filmcraft_format::SCHEMA_VERSION {
        let b = schema_backup_path(path, s.loaded_schema);
        if s.services.read_file(&b).is_err()
            && let Ok(old) = s.services.read_file(path)
        {
            s.services.write_file(&b, &old).map_err(|e| EngineError::Other(format!("{b}: {e}")))?;
            backup = Some(b);
        }
    }
    s.services.write_file(path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    if adopt {
        s.path = Some(path.to_string());
        s.previews_follow_path();
        s.note_recent_project();
        s.saved_revision = s.revision;
        s.loaded_schema = filmcraft_format::SCHEMA_VERSION;
    }
    Ok(json!({"path": path, "bytes": bytes.len(), "ms": t0.elapsed().as_secs_f64() * 1000.0, "backup": backup}))
}

fn schema_backup_path(path: &str, schema: u32) -> String {
    let stem = path.strip_suffix(".fcproj").unwrap_or(path);
    format!("{stem} (schema v{schema} backup).fcproj")
}

/// Make `proj` the session's project (fresh history, media pool and editor state).
fn install_project(s: &mut Session, proj: filmcraft_project::Project, path: Option<String>, clean: bool) {
    let proxies = s.media.use_proxies();
    s.media = std::sync::Arc::new(crate::MediaPool::default());
    s.media.set_use_proxies(proxies);
    s.offline = Default::default();
    let first = proj.sequences().next().map(|i| i.id);
    s.project = std::sync::Arc::new(proj);
    s.history = Default::default();
    s.history.limit = 200;
    s.state = crate::Session::default().state;
    s.state.active_sequence = first;
    s.state.open_sequences = first.into_iter().collect();
    if let Some(p) = &path
        && !cfg!(target_arch = "wasm32")
    {
        s.previews.set_dir(Some(crate::project_tools::previews_dir_for(&s.project, p)));
    }
    s.path = path;
    s.revision += 1;
    // A recovered project is unsaved: its saved revision is one that never existed.
    s.saved_revision = if clean { s.revision } else { 0 };
    s.loaded_schema = filmcraft_format::SCHEMA_VERSION;
    s.events.push(crate::Event::ProjectChanged { revision: s.revision });
}

fn open_project(s: &mut Session, path: &str) -> Result<Value> {
    let bytes = s.services.read_file(path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let loaded = filmcraft_format::decode(&bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let (from, migrated) = (loaded.schema_version, loaded.migrated());
    install_project(s, loaded.project, Some(path.to_string()), true);
    if let Some(view) = loaded.view.filter(|_| s.prefs.timeline.restore_open_sequences) {
        s.restore_project_view(view);
    }
    // the frontend shows the project's active sequence (with its own view, when one was restored)
    if let Some(seq) = s.state.active_sequence {
        s.events.push(crate::Event::OpenSequence(seq));
    }
    s.note_recent_project();
    s.loaded_schema = from;
    if migrated {
        s.toast(format!("Upgraded project from schema v{from} to v{}; the original is kept as a backup when you save", filmcraft_format::SCHEMA_VERSION));
    }
    let missing = crate::relink::on_open(s);
    Ok(json!({"path": path, "schemaVersion": from, "migrated": migrated, "missingMedia": missing}))
}

/// Load a recovery candidate (newest when `id` is None) as the current, unsaved project.
fn recover(s: &mut Session, id: Option<&str>) -> Result<Value> {
    let per = s.persistence.as_ref().ok_or_else(|| EngineError::Other("recovery is not running".into()))?;
    let idx = match id {
        Some(id) => per.candidates.iter().position(|c| c.id == id).ok_or_else(|| bad("file.recover", format!("no recovery session `{id}`")))?,
        None => 0,
    };
    let c = per.candidates[idx].clone();
    let saved_at = per.local_time(c.meta.saved_unix);
    let loaded = crate::autosave::load_candidate(&c).map_err(EngineError::Other)?;
    install_project(s, loaded.project, c.meta.project_path.clone(), false);
    if let Some(seq) = s.state.active_sequence {
        s.events.push(crate::Event::OpenSequence(seq));
    }
    crate::relink::on_open(s);
    // Our own journal must hold the recovered state before the old one is deleted.
    s.sync_persistence();
    if let Some(per) = s.persistence.as_mut() {
        per.flush();
        per.candidates.remove(idx);
    }
    crate::autosave::discard_candidate(&c).map_err(|e| EngineError::Other(e.to_string()))?;
    s.toast(format!("Recovered unsaved changes to {} from {saved_at}", c.meta.project_name));
    Ok(json!({"id": c.id, "name": c.meta.project_name, "path": c.meta.project_path, "savedAt": saved_at, "revision": c.meta.revision}))
}

/// Where auto-saves of the current project go.
fn auto_save_dir(s: &Session) -> std::path::PathBuf {
    if let Some(d) = s.project.settings.scratch.auto_save.as_deref().filter(|d| !d.is_empty() && s.path.is_some()) {
        return std::path::PathBuf::from(d);
    }
    match (&s.path, &s.persistence) {
        (Some(p), _) => filmcraft_format::autosave::auto_save_dir(std::path::Path::new(p)),
        (None, Some(per)) => per.data_dir.join(filmcraft_format::autosave::AUTO_SAVE_DIR),
        (None, None) => std::path::PathBuf::from(filmcraft_format::autosave::AUTO_SAVE_DIR),
    }
}

fn auto_save_name(s: &Session) -> String {
    s.path.as_deref().and_then(|p| std::path::Path::new(p).file_stem()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| s.project.name.clone())
}
