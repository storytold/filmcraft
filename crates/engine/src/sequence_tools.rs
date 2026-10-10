//! Sequence and Markers menu commands: gaps, reverse match frame, subsequences, track deletion,
//! through edits, split edit points, range and chapter markers and the marker toggles.
//!
//! | Id | Menu | Default key |
//! |---|---|---|
//! | `sequence.reverseMatchFrame` | Sequence ▸ Reverse Match Frame | Shift+R |
//! | `sequence.goToNextGap` / `sequence.goToPrevGap` | Sequence ▸ Go to Gap ▸ Next / Previous in Sequence | Shift+; / Cmd+Shift+; |
//! | `sequence.goToNextGapInTrack` / `sequence.goToPrevGapInTrack` | Sequence ▸ Go to Gap ▸ Next / Previous in Track | |
//! | `sequence.selectionFollowsPlayhead` | Sequence ▸ Selection Follows Playhead | |
//! | `sequence.showThroughEdits` | Sequence ▸ Show Through Edits | |
//! | `sequence.joinThroughEdits` | (clip and edit point context menus) Join Through Edits | |
//! | `sequence.throughEdits` | query: the through edits of the active sequence | |
//! | `sequence.makeSubsequence` | Sequence ▸ Make Subsequence | Shift+U |
//! | `sequence.deleteTracks` | Sequence ▸ Delete Tracks… | |
//! | `markers.markSplitVideoIn` … `markers.markSplitAudioOut` | Markers ▸ Mark Split ▸ Video In … Audio Out | |
//! | `markers.goToSplitVideoIn` … `markers.goToSplitAudioOut` | Markers ▸ Go to Split ▸ Video In … Audio Out | |
//! | `markers.addRange` | Markers ▸ Add Range Marker | Ctrl+Shift+M |
//! | `markers.addRangeInOut` | Markers ▸ Add Range Marker to In and Out | Ctrl+M |
//! | `markers.showAllMarkerColors` / `markers.filterColors` | Markers ▸ Show All Marker Colors / Markers panel colour filter | |
//! | `markers.addChapter` | Markers ▸ Add Chapter Marker… | |
//! | `markers.rippleSequenceMarkers` | Markers ▸ Ripple Sequence Markers | |
//! | `markers.copyPasteIncludesSequenceMarkers` | Markers ▸ Copy Paste Includes Sequence Markers | |
//!
//! Split points: Mark Split sets a video-only or audio-only In/Out (on the sequence, or with
//! `"target":"source"` on the Source monitor clip). Ordinary Mark In / Mark Out clear the split of
//! that side. Insert / Overwrite from the Source monitor honour source split points: each channel
//! takes its own range and lands offset by the difference of the In points (J- and L-cuts).

use filmcraft_edit as edit;
use filmcraft_project::{AudioChannels, ClipId, ItemId, Label, Marker, MarkerId, MarkerKind, SplitMarks, TrackId, TrackItem, TrackKind};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, clips_p, has_seq, str_p, time_p, track_p, with_links};
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

fn query(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

/// Insert this module's commands into the registry after their anchors, so the menus list them in
/// Premiere's order.
pub(crate) fn splice(v: &mut Vec<CommandSpec>) {
    for (anchor, specs) in groups() {
        let at = v.iter().position(|c| c.id == anchor).map(|i| i + 1).unwrap_or(v.len());
        for (k, c) in specs.into_iter().enumerate() {
            v.insert(at + k, c);
        }
    }
}

const SPLIT: &str = r#"{"time":ticks?,"target":"program|source"}"#;

fn groups() -> Vec<(&'static str, Vec<CommandSpec>)> {
    vec![
        (
            "sequence.matchFrame",
            vec![spec("sequence.reverseMatchFrame", "Reverse Match Frame", &["Sequence"], Some("Shift+R"), "{}", has_source, |s, _| reverse_match_frame(s))],
        ),
        (
            "sequence.closeGap",
            vec![
                spec("sequence.goToNextGap", "Next in Sequence", &["Sequence", "Go to Gap"], Some("Shift+;"), "{}", has_seq, |s, p| {
                    go_to_gap(s, p, true, false)
                }),
                spec("sequence.goToPrevGap", "Previous in Sequence", &["Sequence", "Go to Gap"], Some("Cmd+Shift+;"), "{}", has_seq, |s, p| {
                    go_to_gap(s, p, false, false)
                }),
                spec("sequence.goToNextGapInTrack", "Next in Track", &["Sequence", "Go to Gap"], None, r#"{"track":"V1"|id?}"#, has_seq, |s, p| {
                    go_to_gap(s, p, true, true)
                }),
                spec("sequence.goToPrevGapInTrack", "Previous in Track", &["Sequence", "Go to Gap"], None, r#"{"track":"V1"|id?}"#, has_seq, |s, p| {
                    go_to_gap(s, p, false, true)
                }),
            ],
        ),
        (
            "sequence.linkedSelection",
            vec![
                spec("sequence.selectionFollowsPlayhead", "Selection Follows Playhead", &["Sequence"], None, r#"{"on":bool?}"#, always, |s, p| {
                    s.state.selection_follows_playhead = bool_p(p, "on").unwrap_or(!s.state.selection_follows_playhead);
                    if s.state.selection_follows_playhead {
                        select_under_playhead(s);
                    }
                    Ok(json!({"selectionFollowsPlayhead": s.state.selection_follows_playhead}))
                }),
                spec("sequence.showThroughEdits", "Show Through Edits", &["Sequence"], None, r#"{"on":bool?}"#, always, |s, p| {
                    s.state.show_through_edits = bool_p(p, "on").unwrap_or(!s.state.show_through_edits);
                    Ok(json!({"showThroughEdits": s.state.show_through_edits}))
                }),
                spec(
                    "sequence.joinThroughEdits",
                    "Join Through Edits",
                    &[],
                    None,
                    r#"{"clips":[id]?,"cut":[id,id]?,"all":bool?}"#,
                    has_seq,
                    join_through_edits,
                ),
                query("sequence.throughEdits", "List Through Edits", "{}", |s, _| {
                    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
                    Ok(Value::Array(
                        edit::through::through_edits(q)
                            .iter()
                            .map(|e| json!({"track": e.track.0, "left": e.left.0, "right": e.right.0, "time": e.time.0}))
                            .collect(),
                    ))
                }),
                spec("sequence.makeSubsequence", "Make Subsequence", &["Sequence"], Some("Shift+U"), r#"{"name":str?}"#, has_seq, make_subsequence),
            ],
        ),
        (
            "sequence.addTracks",
            vec![spec(
                "sequence.deleteTracks",
                "Delete Tracks…",
                &["Sequence"],
                None,
                r#"{"video":"empty"|"V2"|id?,"audio":"empty"|"A2"|id?,"captions":"empty"|"C2"|id?}"#,
                has_seq,
                delete_tracks,
            )],
        ),
        (
            "markers.markSelection",
            vec![
                spec("markers.markSplitVideoIn", "Video In", &["Markers", "Mark Split"], None, SPLIT, always, |s, p| mark_split(s, p, Side::VideoIn)),
                spec("markers.markSplitVideoOut", "Video Out", &["Markers", "Mark Split"], None, SPLIT, always, |s, p| mark_split(s, p, Side::VideoOut)),
                spec("markers.markSplitAudioIn", "Audio In", &["Markers", "Mark Split"], None, SPLIT, always, |s, p| mark_split(s, p, Side::AudioIn)),
                spec("markers.markSplitAudioOut", "Audio Out", &["Markers", "Mark Split"], None, SPLIT, always, |s, p| mark_split(s, p, Side::AudioOut)),
            ],
        ),
        (
            "markers.goToOut",
            vec![
                spec("markers.goToSplitVideoIn", "Video In", &["Markers", "Go to Split"], None, r#"{"target":"program|source"}"#, has_split, |s, p| {
                    go_to_split(s, p, Side::VideoIn)
                }),
                spec("markers.goToSplitVideoOut", "Video Out", &["Markers", "Go to Split"], None, r#"{"target":"program|source"}"#, has_split, |s, p| {
                    go_to_split(s, p, Side::VideoOut)
                }),
                spec("markers.goToSplitAudioIn", "Audio In", &["Markers", "Go to Split"], None, r#"{"target":"program|source"}"#, has_split, |s, p| {
                    go_to_split(s, p, Side::AudioIn)
                }),
                spec("markers.goToSplitAudioOut", "Audio Out", &["Markers", "Go to Split"], None, r#"{"target":"program|source"}"#, has_split, |s, p| {
                    go_to_split(s, p, Side::AudioOut)
                }),
            ],
        ),
        (
            "markers.add",
            vec![
                spec(
                    "markers.addRange",
                    "Add Range Marker",
                    &["Markers"],
                    Some("Ctrl+Shift+M"),
                    r#"{"time":ticks?,"durationFrames":i64=1s,"duration":ticks?,"name":str?,"comment":str?,"color":label?}"#,
                    has_seq,
                    add_range_marker,
                ),
                spec(
                    "markers.addRangeInOut",
                    "Add Range Marker to In and Out",
                    &["Markers"],
                    Some("Ctrl+M"),
                    r#"{"name":str?,"comment":str?,"color":label?}"#,
                    has_in_and_out,
                    add_range_marker_in_out,
                ),
            ],
        ),
        (
            "markers.clearAll",
            vec![
                spec("markers.showAllMarkerColors", "Show All Marker Colors", &["Markers"], None, "{}", has_hidden_colors, |s, _| {
                    s.state.hidden_marker_colors.clear();
                    Ok(json!({"hidden": []}))
                }),
                spec("markers.filterColors", "Marker Colour Filter", &[], None, r#"{"hidden":[label]}|{"color":label,"visible":bool?}"#, always, filter_colors),
            ],
        ),
        (
            "markers.edit",
            vec![
                spec(
                    "markers.addChapter",
                    "Add Chapter Marker…",
                    &["Markers"],
                    None,
                    r#"{"time":ticks?,"name":str?,"comment":str?,"color":label?}"#,
                    has_seq,
                    |s, p| {
                        let t = time_p(s, p, "").unwrap_or(s.playhead());
                        let n = s.active_sequence().map(|q| q.markers.iter().filter(|m| m.kind == MarkerKind::Chapter).count()).unwrap_or(0) + 1;
                        let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| format!("Chapter {n}"));
                        let id = add_marker(s, "Add Chapter Marker", t, Tick::ZERO, MarkerKind::Chapter, name, p)?;
                        Ok(json!({"marker": id.0}))
                    },
                ),
                spec("markers.rippleSequenceMarkers", "Ripple Sequence Markers", &["Markers"], None, r#"{"on":bool?}"#, always, |s, p| {
                    s.state.ripple_sequence_markers = bool_p(p, "on").unwrap_or(!s.state.ripple_sequence_markers);
                    Ok(json!({"rippleSequenceMarkers": s.state.ripple_sequence_markers}))
                }),
                spec(
                    "markers.copyPasteIncludesSequenceMarkers",
                    "Copy Paste Includes Sequence Markers",
                    &["Markers"],
                    None,
                    r#"{"on":bool?}"#,
                    always,
                    |s, p| {
                        s.state.copy_paste_sequence_markers = bool_p(p, "on").unwrap_or(!s.state.copy_paste_sequence_markers);
                        Ok(json!({"copyPasteIncludesSequenceMarkers": s.state.copy_paste_sequence_markers}))
                    },
                ),
            ],
        ),
    ]
}

// ---------- enablement ----------

fn has_source(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    s.state.source_item.map(|_| ()).ok_or_else(|| "no clip in the Source monitor".into())
}
fn has_in_and_out(s: &Session) -> std::result::Result<(), String> {
    let q = s.active_sequence().ok_or("no sequence is open")?;
    if q.mark_in.is_some() && q.mark_out.is_some() { Ok(()) } else { Err("mark an In and an Out point first".into()) }
}
fn has_split(s: &Session) -> std::result::Result<(), String> {
    let program = s.active_sequence().is_some_and(|q| !q.split.is_empty());
    let source = s.state.source_item.and_then(|i| s.project.item(i)).is_some_and(|i| !i.split.is_empty());
    if program || source { Ok(()) } else { Err("there are no split points".into()) }
}
fn has_hidden_colors(s: &Session) -> std::result::Result<(), String> {
    if s.state.hidden_marker_colors.is_empty() { Err("all marker colours are shown".into()) } else { Ok(()) }
}

// ---------- selection follows playhead ----------

/// Select the clips under the playhead on targeted tracks (with linked partners).
pub fn select_under_playhead(s: &mut Session) {
    let t = s.playhead();
    let tg = s.targeting().targeted;
    let Some(q) = s.active_sequence() else { return };
    let ids: Vec<ClipId> = q.all_tracks().filter(|tr| tg.contains(&tr.id)).filter_map(|tr| tr.item_at(t).map(|i| i.id)).collect();
    s.state.selection = with_links(s, &ids);
}

// ---------- match frame ----------

/// Timeline time at which `it` shows media time `src` (None when it doesn't).
pub fn timeline_time_of(it: &TrackItem, src: Tick) -> Option<Tick> {
    if let Some(h) = it.frame_hold {
        return (h == src).then_some(it.start);
    }
    let speed = it.speed.abs();
    if speed <= 0.0 {
        return None;
    }
    let total = it.source_out() - it.source_in;
    let rel_src = if it.reverse { it.source_in + total - Tick(1) - src } else { src - it.source_in };
    if rel_src < Tick::ZERO || rel_src >= total {
        return None;
    }
    let t = it.start + Tick((rel_src.0 as f64 / speed).floor() as i64);
    (t < it.end()).then_some(t)
}

/// Sequence ▸ Reverse Match Frame: find the Source monitor frame in the active sequence (targeted
/// tracks first, top video track down, then audio) and move the playhead there.
fn reverse_match_frame(s: &mut Session) -> Result<Value> {
    let item = s.state.source_item.ok_or_else(|| EngineError::Other("no clip in the Source monitor".into()))?;
    let src = s.state.source_playhead;
    let tg = s.targeting().targeted;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut hits: Vec<(bool, Tick, ClipId)> = Vec::new();
    for tr in q.video_tracks.iter().rev().chain(&q.audio_tracks) {
        for it in tr.items.iter().filter(|i| i.item == item) {
            if let Some(t) = timeline_time_of(it, src) {
                hits.push((!tg.contains(&tr.id), t, it.id));
            }
        }
    }
    // stable: targeted first, then track order, then time
    hits.sort_by_key(|h| h.0);
    let (_, t, clip) = *hits.first().ok_or_else(|| EngineError::Other("the Source monitor frame is not used in this sequence".into()))?;
    s.set_playhead(t);
    Ok(json!({"clip": clip.0, "time": s.playhead().0}))
}

// ---------- gaps ----------

fn go_to_gap(s: &mut Session, p: &Value, next: bool, in_track: bool) -> Result<Value> {
    let t = s.playhead();
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let gaps: Vec<TimeRange> = if in_track {
        let tracks: Vec<TrackId> = match track_p(s, p, "track", if next { "sequence.goToNextGapInTrack" } else { "sequence.goToPrevGapInTrack" })? {
            Some(id) => vec![id],
            None => s.targeting().targeted,
        };
        q.all_tracks().filter(|tr| tracks.contains(&tr.id)).flat_map(edit::through::track_gaps).collect()
    } else {
        edit::through::sequence_gaps(q)
    };
    let starts = gaps.iter().map(|g| g.start);
    let hit = if next { starts.filter(|g| *g > t).min() } else { starts.filter(|g| *g < t).max() };
    match hit {
        Some(g) => {
            s.set_playhead(g);
            Ok(json!({"time": s.playhead().0}))
        }
        None => Ok(json!({"time": null})),
    }
}

// ---------- through edits ----------

fn join_through_edits(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(cut) = p.get("cut") {
        return join_through_edit_at(s, cut);
    }
    let all = bool_p(p, "all").unwrap_or(false) || (p.get("clips").is_none() && p.get("clip").is_none() && s.state.selection.is_empty());
    let only = if all { Vec::new() } else { with_links(s, &clips_p(s, p)) };
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let any = edit::through::through_edits(q)
        .iter()
        .any(|e| (only.is_empty() || only.contains(&e.left) || only.contains(&e.right)) && q.track(e.track).is_some_and(|t| !t.locked));
    if !any {
        return Err(EngineError::Other("there are no through edits to join".into()));
    }
    let n = s.edit_sequence("Join Through Edits", |q, _, st| {
        let n = edit::through::join_through_edits(q, &only);
        st.selection.retain(|c| q.find_item(*c).is_some());
        Ok(n)
    })?;
    Ok(json!({"joined": n}))
}

/// `sequence.joinThroughEdits {cut: [left, right]}`: join just that one cut (the edit point menu,
/// #219), with the cuts between the same pieces' linked partners when linked selection is on.
fn join_through_edit_at(s: &mut Session, cut: &Value) -> Result<Value> {
    let ids: Vec<ClipId> = cut.as_array().map(|a| a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect()).unwrap_or_default();
    let [left, right] = ids[..] else {
        return Err(EngineError::Other("sequence.joinThroughEdits: `cut` needs two clip ids, [left, right]".into()));
    };
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if !edit::through::through_edits(q).iter().any(|e| e.left == left && e.right == right) {
        return Err(EngineError::Other("that cut is not a through edit".into()));
    }
    let (lefts, rights) = (with_links(s, &[left]), with_links(s, &[right]));
    let n = s.edit_sequence("Join Through Edits", |q, _, st| {
        let n = edit::through::join_through_edits_where(q, |a, b| lefts.contains(&a) && rights.contains(&b));
        st.selection.retain(|c| q.find_item(*c).is_some());
        Ok(n)
    })?;
    Ok(json!({"joined": n}))
}

// ---------- subsequence ----------

/// Sequence ▸ Make Subsequence: a new sequence (same settings and track layout) holding copies of the
/// selected clips, or of everything on targeted tracks between In and Out, trimmed to In/Out when
/// they are set. The new sequence is added to the project and selected; the original is unchanged.
/// Give a sequence made from part of `from` (Nest…, Make Subsequence) the track names and audio
/// channel layouts of `from`'s tracks, track for track.
pub(crate) fn lay_out_tracks_like(new: &mut filmcraft_project::Sequence, from: &filmcraft_project::Sequence) {
    for (tr, src) in new.video_tracks.iter_mut().zip(&from.video_tracks) {
        tr.name = src.name.clone();
    }
    for (tr, src) in new.audio_tracks.iter_mut().zip(&from.audio_tracks) {
        tr.name = src.name.clone();
        tr.channels = src.channels;
    }
}

fn make_subsequence(s: &mut Session, p: &Value) -> Result<Value> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?.clone();
    let seq_name = s.project.item(seq_id).map(|i| i.name.clone()).unwrap_or_default();
    let sel = with_links(s, &s.state.selection);
    let fd = q.settings.frame_rate.frame_duration();
    let range = (q.mark_in.is_some() || q.mark_out.is_some())
        .then(|| TimeRange::from_bounds(q.mark_in.unwrap_or(Tick::ZERO), q.mark_out.map(|o| o + fd).unwrap_or(q.duration())))
        .filter(|r| r.duration > Tick::ZERO);
    if sel.is_empty() && range.is_none() {
        return Err(EngineError::Other("select clips or mark In and Out first".into()));
    }
    let tg = s.targeting().targeted;
    let mut items: Vec<(TrackKind, usize, TrackItem)> = Vec::new();
    for (kind, tracks) in [(TrackKind::Video, &q.video_tracks), (TrackKind::Audio, &q.audio_tracks)] {
        for (ti, tr) in tracks.iter().enumerate() {
            for it in &tr.items {
                let wanted = if sel.is_empty() { tg.contains(&tr.id) } else { sel.contains(&it.id) };
                if !wanted {
                    continue;
                }
                if let Some(c) = edit::through::clip_item_to(it, range.unwrap_or(it.range())) {
                    items.push((kind, ti, c));
                }
            }
        }
    }
    if items.is_empty() {
        return Err(EngineError::Other("there are no clips between In and Out on the targeted tracks".into()));
    }
    let offset = range.map(|r| r.start).unwrap_or_else(|| items.iter().map(|i| i.2.start).min().unwrap_or_default());
    let name = match str_p(p, "name") {
        Some(n) => n.to_string(),
        None => (1..)
            .map(|k| format!("{seq_name}_Sub_{k:02}"))
            .find(|n| !s.project.items.values().any(|i| &i.name == n))
            .unwrap_or_else(|| format!("{seq_name}_Sub")),
    };
    let id = s.edit("Make Subsequence", |pr, st| {
        let nid = pr.new_sequence(&name, q.settings.clone(), q.video_tracks.len(), q.audio_tracks.len(), None);
        let mut ids = std::collections::HashMap::new();
        let mut links = std::collections::HashMap::new();
        let mut placed: Vec<(TrackKind, usize, TrackItem)> = Vec::new();
        for (kind, ti, mut it) in items.clone() {
            let new_id = ClipId(pr.alloc_id());
            ids.insert(it.id, new_id);
            it.id = new_id;
            it.start -= offset;
            if let Some(l) = it.link {
                it.link = Some(*links.entry(l).or_insert_with(|| pr.alloc_id()));
            }
            placed.push((kind, ti, it));
        }
        let mut transitions = Vec::new();
        for (kind, tracks) in [(TrackKind::Video, &q.video_tracks), (TrackKind::Audio, &q.audio_tracks)] {
            for (ti, tr) in tracks.iter().enumerate() {
                for trn in &tr.transitions {
                    let from = trn.from.map(|c| ids.get(&c).copied());
                    let to = trn.to.map(|c| ids.get(&c).copied());
                    let inside = range.is_none_or(|r| trn.start >= r.start && trn.end() <= r.end());
                    if inside && from != Some(None) && to != Some(None) {
                        let mut t = trn.clone();
                        t.id = filmcraft_project::TransitionId(pr.alloc_id());
                        t.from = from.flatten();
                        t.to = to.flatten();
                        t.start -= offset;
                        transitions.push((kind, ti, t));
                    }
                }
            }
        }
        let nq = pr.sequence_mut(nid).ok_or(EngineError::NoSequence)?;
        lay_out_tracks_like(nq, &q);
        for (kind, ti, it) in placed {
            nq.tracks_mut(kind)[ti].items.push(it);
        }
        for (kind, ti, t) in transitions {
            nq.tracks_mut(kind)[ti].transitions.push(t);
        }
        for tr in nq.all_tracks_mut() {
            tr.sort();
            edit::remove_orphan_transitions(tr);
        }
        nq.check().map_err(EngineError::Other)?;
        st.project_selection = vec![nid];
        // like Premiere: the subsequence is loaded in the Source Monitor, ready to edit from
        st.source_item = Some(nid);
        st.source_playhead = Tick::ZERO;
        Ok(nid)
    })?;
    s.events.push(crate::Event::OpenSource(id));
    Ok(json!({"sequence": id.0, "name": name}))
}

// ---------- a sequence edited in as its clips ----------

/// The nest toggle off ("Insert and overwrite sequences as nests or individual clips"): edit the
/// clips of sequence `item` that lie in `range` of it into the active sequence at `at`, instead of
/// one nested clip. As in Premiere, the clips come with their transitions and links; the source's
/// tracks that hold clips go to consecutive tracks from `vdest` / `adest` up (source V1 and V3
/// land on the destination track and the one above it), and tracks that are missing are added.
/// A kind without a destination track is left out.
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_sequence_clips(
    s: &mut Session,
    item: ItemId,
    range: TimeRange,
    at: Tick,
    vdest: Option<TrackId>,
    adest: Option<TrackId>,
    insert: bool,
    label: &str,
) -> Result<Vec<ClipId>> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let src = s.project.sequence(item).ok_or_else(|| bad(label, "not a sequence"))?.clone();
    let dst = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // (kind, how many tracks above the destination track, the clip trimmed to the range)
    let mut items: Vec<(TrackKind, usize, TrackItem)> = Vec::new();
    // source track index → tracks above the destination track, per kind
    // (TrackKind has no Hash: the maps are keyed by "is audio")
    let audio = |k: TrackKind| k == TrackKind::Audio;
    let mut lanes: std::collections::HashMap<(bool, usize), usize> = std::collections::HashMap::new();
    let mut base: std::collections::HashMap<bool, usize> = std::collections::HashMap::new();
    for (kind, tracks, dest) in [(TrackKind::Video, &src.video_tracks, vdest), (TrackKind::Audio, &src.audio_tracks, adest)] {
        let Some(first) = dest.and_then(|d| dst.tracks(kind).iter().position(|t| t.id == d)) else { continue };
        base.insert(audio(kind), first);
        let mut lane = 0;
        for (ti, tr) in tracks.iter().enumerate() {
            let clips: Vec<TrackItem> = tr.items.iter().filter_map(|it| edit::through::clip_item_to(it, range)).collect();
            if clips.is_empty() {
                continue;
            }
            lanes.insert((audio(kind), ti), lane);
            items.extend(clips.into_iter().map(|c| (kind, lane, c)));
            lane += 1;
        }
    }
    if items.is_empty() {
        return Err(EngineError::Other("no destination track for this sequence's clips (check source patching)".into()));
    }
    let media = s.media.clone();
    s.edit(label, |p, st| {
        // copies with ids, links and transitions of their own, moved to `at`
        let mut ids = std::collections::HashMap::new();
        let mut links = std::collections::HashMap::new();
        let mut placed: Vec<(TrackKind, usize, TrackItem)> = Vec::new();
        for (kind, lane, mut it) in items.clone() {
            let id = ClipId(p.alloc_id());
            ids.insert(it.id, id);
            it.id = id;
            it.start = at + (it.start - range.start);
            if let Some(l) = it.link {
                it.link = Some(*links.entry(l).or_insert_with(|| p.alloc_id()));
            }
            placed.push((kind, lane, it));
        }
        let mut transitions = Vec::new();
        for (kind, tracks) in [(TrackKind::Video, &src.video_tracks), (TrackKind::Audio, &src.audio_tracks)] {
            for (ti, tr) in tracks.iter().enumerate() {
                let Some(lane) = lanes.get(&(audio(kind), ti)).copied() else { continue };
                for trn in &tr.transitions {
                    let from = trn.from.map(|c| ids.get(&c).copied());
                    let to = trn.to.map(|c| ids.get(&c).copied());
                    if trn.start >= range.start && trn.end() <= range.end() && from != Some(None) && to != Some(None) {
                        let mut t = trn.clone();
                        t.id = filmcraft_project::TransitionId(p.alloc_id());
                        (t.from, t.to) = (from.flatten(), to.flatten());
                        t.start = at + (t.start - range.start);
                        transitions.push((kind, lane, t));
                    }
                }
            }
        }
        // tracks for every lane, added on top when the sequence has too few
        let mut dest: std::collections::HashMap<(bool, usize), TrackId> = std::collections::HashMap::new();
        for (is_audio, lane) in placed.iter().map(|x| (audio(x.0), x.1)).collect::<std::collections::BTreeSet<_>>() {
            let kind = if is_audio { TrackKind::Audio } else { TrackKind::Video };
            let index = base.get(&is_audio).copied().unwrap_or(0).saturating_add(lane);
            while p.sequence(seq_id).ok_or(EngineError::NoSequence)?.tracks(kind).len() <= index {
                let id = TrackId(p.alloc_id());
                let tracks = p.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?.tracks_mut(kind);
                let (word, n) = (if kind == TrackKind::Video { "Video" } else { "Audio" }, tracks.len() + 1);
                tracks.push(filmcraft_project::Track::new(id, kind, format!("{word} {n}")));
            }
            let id = p.sequence(seq_id).and_then(|q| q.tracks(kind).get(index)).map(|t| t.id).ok_or(EngineError::NoSequence)?;
            dest.insert((is_audio, lane), id);
        }
        let placements: Vec<(TrackId, TrackItem)> = placed.into_iter().filter_map(|(k, lane, it)| Some((*dest.get(&(audio(k), lane))?, it))).collect();
        let span = placements.iter().map(|x| x.1.start).min().zip(placements.iter().map(|x| x.1.end()).max());
        let snapshot = std::sync::Arc::new(p.clone());
        let snap = snapshot.clone();
        let durations = move |id: ItemId| crate::media_duration(&snapshot, &media, id);
        let starts = move |id: ItemId| crate::media_start(&snap, id);
        let mut next = p.next_id;
        let seq = p.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let min = seq.settings.frame_rate.frame_duration();
        let mut ctx = edit::EditCtx { next_id: &mut next, media_duration: &durations, media_start: &starts, min_duration: min };
        let new = if insert { edit::insert(seq, placements, &mut ctx)? } else { edit::overwrite(seq, placements, &mut ctx)? };
        for (kind, lane, t) in transitions {
            if let Some(tr) = dest.get(&(audio(kind), lane)).and_then(|id| seq.track_mut(*id)) {
                tr.transitions.push(t);
            }
        }
        for tr in seq.all_tracks_mut() {
            tr.sort();
            edit::remove_orphan_transitions(tr);
        }
        seq.check().map_err(EngineError::Other)?;
        if insert
            && st.ripple_sequence_markers
            && let Some((a, b)) = span
        {
            ripple_markers(&mut seq.markers, a, b - a);
        }
        p.next_id = next;
        st.selection = new.clone();
        Ok(new)
    })
}

// ---------- delete tracks ----------

/// Sequence ▸ Delete Tracks…: delete all empty video / audio tracks (`"empty"`) or one track per
/// kind. A sequence always keeps at least one video and one audio track.
/// The most tracks one `sequence.addTracks` adds of a kind, and the most a sequence may then
/// have of it. (Amounts come from dialogs, scripts and the control channel: never trusted.)
const MAX_ADDED_TRACKS: u64 = 99;
pub(crate) const MAX_TRACKS: usize = 999;

/// Where `sequence.addTracks` puts new tracks among the `len` tracks of their kind: the number of
/// tracks before them. `key` holds `"first"` (Before First Track), a track of that kind by name
/// (`"V2"`: after Video 2; `letter` is V, A or S) or a number (after that many tracks); without it
/// they go after the last track.
fn placement(p: &Value, key: &str, letter: char, len: usize) -> Result<usize> {
    let cmd = "sequence.addTracks";
    let after = match p.get(key) {
        None | Some(Value::Null) => return Ok(len),
        Some(v) => match (v.as_u64(), v.as_str()) {
            (Some(n), _) => usize::try_from(n).ok(),
            (None, Some(x)) if matches!(x.to_ascii_lowercase().as_str(), "first" | "before" | "beforefirst") => Some(0),
            (None, Some(x)) if matches!(x.to_ascii_lowercase().as_str(), "last" | "end") => Some(len),
            (None, Some(x)) => {
                let mut rest = x.chars();
                rest.next().filter(|c| c.eq_ignore_ascii_case(&letter)).and_then(|_| rest.as_str().parse::<usize>().ok())
            }
            (None, None) => None,
        },
    };
    after.filter(|n| *n <= len).ok_or_else(|| bad(cmd, format!("`{key}` must be \"first\", a track ({letter}1–{letter}{len}) or how many tracks come before")))
}

/// Give the tracks that still carry a default name (`<prefix> <number>`) the number of their
/// place, as Premiere numbers its tracks; a track the user named keeps its name.
fn renumber(tracks: &mut [filmcraft_project::Track], prefix: &str) {
    for (i, t) in tracks.iter_mut().enumerate() {
        let default = t.name.strip_prefix(prefix).and_then(|r| r.strip_prefix(' ')).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if default {
            t.name = format!("{prefix} {}", i + 1);
        }
    }
}

/// Sequence ▸ Add Tracks…: `video` / `audio` / `submix` tracks, each kind at its own place
/// (`videoAfter`, `audioAfter`, `submixAfter`; after the last track when not given), audio tracks
/// of `audioType` (Standard unless given) and submix tracks of `submixType` (Stereo). One undo
/// step. Tracks are numbered by their place, so default names after the new tracks move up.
pub(crate) fn add_tracks(s: &mut Session, p: &Value) -> Result<Value> {
    use crate::commands::u64_p;
    let cmd = "sequence.addTracks";
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let amount = |key: &str, default: u64, have: usize| -> Result<usize> {
        let n = match p.get(key) {
            None | Some(Value::Null) => default,
            Some(_) => u64_p(p, key).ok_or_else(|| bad(cmd, format!("`{key}` must be a number of tracks")))?,
        };
        if n > MAX_ADDED_TRACKS {
            return Err(bad(cmd, format!("`{key}`: at most {MAX_ADDED_TRACKS} tracks at a time")));
        }
        if have.saturating_add(n as usize) > MAX_TRACKS {
            return Err(bad(cmd, format!("`{key}`: a sequence has at most {MAX_TRACKS} tracks of a kind")));
        }
        Ok(n as usize)
    };
    let (nv, na, ns) = (amount("video", 1, q.video_tracks.len())?, amount("audio", 0, q.audio_tracks.len())?, amount("submix", 0, q.submix_tracks.len())?);
    let (at_v, at_a, at_s) = (
        placement(p, "videoAfter", 'V', q.video_tracks.len())?,
        placement(p, "audioAfter", 'A', q.audio_tracks.len())?,
        placement(p, "submixAfter", 'S', q.submix_tracks.len())?,
    );
    let channels = |key: &str, default: AudioChannels| match p.get(key).and_then(Value::as_str) {
        None => Ok(default),
        Some(x) => crate::mixer::channels_from(x).ok_or_else(|| bad(cmd, format!("`{key}`: unknown track type {x:?} (standard, stereo, 5.1, adaptive, mono)"))),
    };
    let (audio_type, submix_type) = (channels("audioType", AudioChannels::Stereo)?, channels("submixType", AudioChannels::Stereo)?);
    if nv + na + ns == 0 {
        return Err(bad(cmd, "no tracks to add"));
    }
    let added = s.edit_sequence("Add Tracks", |q, ctx, _| {
        let mut new = |kind: TrackKind, prefix: &str, channels: Option<AudioChannels>| {
            let mut t = filmcraft_project::Track::new(TrackId(ctx.alloc()), kind, format!("{prefix} 0"));
            if let Some(c) = channels {
                t.channels = c;
            }
            t
        };
        let mut ids: [Vec<u64>; 3] = Default::default();
        for (k, (tracks, n, at, kind, prefix, ch)) in [
            (&mut q.video_tracks, nv, at_v, TrackKind::Video, "Video", None),
            (&mut q.audio_tracks, na, at_a, TrackKind::Audio, "Audio", Some(audio_type)),
            (&mut q.submix_tracks, ns, at_s, TrackKind::Audio, "Submix", Some(submix_type)),
        ]
        .into_iter()
        .enumerate()
        {
            let at = at.min(tracks.len());
            for i in 0..n {
                let t = new(kind, prefix, ch);
                if let Some(slot) = ids.get_mut(k) {
                    slot.push(t.id.0);
                }
                tracks.insert(at + i, t);
            }
            if n > 0 {
                renumber(tracks, prefix);
            }
        }
        Ok(ids)
    })?;
    let [video, audio, submix] = added;
    Ok(json!({"video": video, "audio": audio, "submix": submix}))
}

fn delete_tracks(s: &mut Session, p: &Value) -> Result<Value> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut doomed: Vec<TrackId> = Vec::new();
    for (key, kind) in [("video", TrackKind::Video), ("audio", TrackKind::Audio)] {
        let tracks = q.tracks(kind);
        let mut mine: Vec<TrackId> = match p.get(key) {
            None | Some(Value::Null) => continue,
            Some(v) if v.as_str().is_some_and(|x| matches!(x, "empty" | "allEmpty" | "all-empty")) => {
                tracks.iter().filter(|t| t.items.is_empty()).map(|t| t.id).collect()
            }
            Some(_) => {
                let id = track_p(s, p, key, "sequence.deleteTracks")?.ok_or_else(|| bad("sequence.deleteTracks", format!("unknown {key} track")))?;
                if !tracks.iter().any(|t| t.id == id) {
                    return Err(bad("sequence.deleteTracks", format!("`{key}` must name a {key} track")));
                }
                vec![id]
            }
        };
        if mine.len() == tracks.len() {
            // keep the first one
            mine.retain(|id| *id != tracks[0].id);
        }
        doomed.extend(mine);
    }
    // caption tracks: unlike video/audio, a sequence may have none, so all of them can go
    match p.get("captions") {
        None | Some(Value::Null) => {}
        Some(v) if v.as_str().is_some_and(|x| matches!(x, "empty" | "allEmpty" | "all-empty")) => {
            doomed.extend(q.caption_tracks.iter().filter(|t| t.captions.is_empty()).map(|t| t.id));
        }
        Some(v @ (Value::String(_) | Value::Number(_))) => {
            let id = crate::captions::track_param(s, &json!({"track": v})).ok_or_else(|| bad("sequence.deleteTracks", "unknown caption track"))?;
            doomed.push(id);
        }
        Some(_) => return Err(bad("sequence.deleteTracks", "`captions` must be \"empty\", a name like \"C2\" or a track id")),
    }
    if doomed.is_empty() {
        return Err(EngineError::Other("no tracks to delete".into()));
    }
    let n = doomed.len();
    s.edit_sequence("Delete Tracks", |q, _, st| {
        q.video_tracks.retain(|t| !doomed.contains(&t.id));
        q.audio_tracks.retain(|t| !doomed.contains(&t.id));
        q.caption_tracks.retain(|t| !doomed.contains(&t.id));
        st.selection.retain(|c| q.find_item(*c).is_some());
        st.caption_selection.retain(|c| q.find_caption(*c).is_some());
        if let Some(tg) = st.targeting.get_mut(&seq_id) {
            tg.targeted.retain(|t| !doomed.contains(t));
            if tg.video_dest.is_some_and(|t| doomed.contains(&t)) {
                tg.video_dest = q.video_tracks.first().map(|t| t.id);
            }
            if tg.audio_dest.is_some_and(|t| doomed.contains(&t)) {
                tg.audio_dest = q.audio_tracks.first().map(|t| t.id);
            }
        }
        Ok(())
    })?;
    Ok(json!({"deleted": n}))
}

// ---------- split points ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    VideoIn,
    VideoOut,
    AudioIn,
    AudioOut,
}

impl Side {
    fn label(self) -> &'static str {
        match self {
            Side::VideoIn => "Video In",
            Side::VideoOut => "Video Out",
            Side::AudioIn => "Audio In",
            Side::AudioOut => "Audio Out",
        }
    }
    /// Set this split point; a split point of the same channel on the wrong side of it is cleared.
    fn set(self, m: &mut SplitMarks, t: Tick) {
        match self {
            Side::VideoIn => {
                m.video_in = Some(t);
                m.video_out = m.video_out.filter(|o| *o >= t);
            }
            Side::VideoOut => {
                m.video_out = Some(t);
                m.video_in = m.video_in.filter(|i| *i <= t);
            }
            Side::AudioIn => {
                m.audio_in = Some(t);
                m.audio_out = m.audio_out.filter(|o| *o >= t);
            }
            Side::AudioOut => {
                m.audio_out = Some(t);
                m.audio_in = m.audio_in.filter(|i| *i <= t);
            }
        }
    }
    /// The effective point (the split point, or the ordinary In/Out).
    fn get(self, m: &SplitMarks, mark_in: Option<Tick>, mark_out: Option<Tick>) -> Option<Tick> {
        match self {
            Side::VideoIn => m.video_in_or(mark_in),
            Side::VideoOut => m.video_out_or(mark_out),
            Side::AudioIn => m.audio_in_or(mark_in),
            Side::AudioOut => m.audio_out_or(mark_out),
        }
    }
}

fn source_marks(s: &Session, item: ItemId) -> (Option<Tick>, Option<Tick>) {
    match s.project.item(item).map(|i| &i.kind) {
        Some(filmcraft_project::ItemKind::Media(m)) => (m.mark_in, m.mark_out),
        Some(filmcraft_project::ItemKind::Sequence(q)) => (q.mark_in, q.mark_out),
        _ => (None, None),
    }
}

fn mark_split(s: &mut Session, p: &Value, side: Side) -> Result<Value> {
    let label = format!("Mark Split {}", side.label());
    if str_p(p, "target") == Some("source") {
        let item = s.state.source_item.ok_or_else(|| EngineError::Other("no clip in the Source monitor".into()))?;
        let t = p.get("time").and_then(Value::as_i64).map(Tick).unwrap_or(s.state.source_playhead);
        let split = s.edit(&label, |pr, _| {
            let it = pr.item_mut(item).ok_or_else(|| EngineError::Other("no such item".into()))?;
            side.set(&mut it.split, t);
            Ok(it.split)
        })?;
        return Ok(serde_json::to_value(split).unwrap_or_default());
    }
    s.active_sequence().ok_or(EngineError::NoSequence)?;
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let split = s.edit_sequence(&label, |q, _, _| {
        side.set(&mut q.split, t);
        Ok(q.split)
    })?;
    Ok(serde_json::to_value(split).unwrap_or_default())
}

fn go_to_split(s: &mut Session, p: &Value, side: Side) -> Result<Value> {
    let none = || EngineError::Other(format!("there is no {} point", side.label()));
    if str_p(p, "target") == Some("source") {
        let item = s.state.source_item.ok_or_else(|| EngineError::Other("no clip in the Source monitor".into()))?;
        let (mi, mo) = source_marks(s, item);
        let split = s.project.item(item).map(|i| i.split).unwrap_or_default();
        let t = side.get(&split, mi, mo).ok_or_else(none)?;
        s.state.source_playhead = t;
        return Ok(json!({"time": t.0}));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let t = side.get(&q.split, q.mark_in, q.mark_out).ok_or_else(none)?;
    s.set_playhead(t);
    Ok(json!({"time": s.playhead().0}))
}

/// Source ranges for Insert / Overwrite with split points: (video range, audio range). Equal when
/// the source has no split points. `full` is the range the ordinary In/Out give.
pub(crate) fn split_source_ranges(s: &Session, item: ItemId, full: TimeRange) -> (TimeRange, TimeRange) {
    let Some(pi) = s.project.item(item) else { return (full, full) };
    let sp = pi.split;
    if sp.is_empty() {
        return (full, full);
    }
    let fd = pi.frame_rate().frame_duration();
    let range = |i: Option<Tick>, o: Option<Tick>| {
        let a = i.unwrap_or(full.start);
        let b = o.map(|o| o + fd).unwrap_or(full.end());
        TimeRange::from_bounds(a, b.max(a + fd))
    };
    (range(sp.video_in, sp.video_out), range(sp.audio_in, sp.audio_out))
}

// ---------- markers ----------

/// Move sequence markers for a ripple edit at `from` by `shift`: later markers move; with a
/// negative shift, markers in the removed span `[from + shift, from)` are deleted.
pub fn ripple_markers(markers: &mut Vec<Marker>, from: Tick, shift: Tick) {
    if shift == Tick::ZERO {
        return;
    }
    if shift < Tick::ZERO {
        markers.retain(|m| !(m.start >= from + shift && m.start < from));
    }
    for m in markers.iter_mut().filter(|m| m.start >= from) {
        m.start += shift;
    }
    markers.sort_by_key(|m| m.start);
}

fn add_marker(s: &mut Session, label: &str, t: Tick, dur: Tick, kind: MarkerKind, name: String, p: &Value) -> Result<MarkerId> {
    let comment = str_p(p, "comment").unwrap_or("").to_string();
    let color = str_p(p, "color").and_then(Label::from_name).unwrap_or(Label::Green);
    s.edit_sequence(label, |q, ctx, _| {
        let id = MarkerId(ctx.alloc());
        q.markers.push(Marker { id, start: t, duration: dur, name, comment, kind, color });
        q.markers.sort_by_key(|m| m.start);
        Ok(id)
    })
}

/// Markers ▸ Add Range Marker: a marker with a duration (one second unless `durationFrames` or
/// `duration` say otherwise) at the playhead.
fn add_range_marker(s: &mut Session, p: &Value) -> Result<Value> {
    let rate = s.sequence_rate();
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let dur = p
        .get("duration")
        .and_then(Value::as_i64)
        .map(Tick)
        .or_else(|| p.get("durationFrames").and_then(Value::as_i64).map(|f| rate.tick_of(f)))
        .unwrap_or(rate.tick_of(rate.timecode_base()));
    if dur <= Tick::ZERO {
        return Err(bad("markers.addRange", "the duration must be positive"));
    }
    let name = str_p(p, "name").unwrap_or("").to_string();
    let id = add_marker(s, "Add Range Marker", t, dur, MarkerKind::Comment, name, p)?;
    Ok(json!({"marker": id.0}))
}

/// Markers ▸ Add Range Marker to In and Out: a marker spanning the sequence In to Out (inclusive).
fn add_range_marker_in_out(s: &mut Session, p: &Value) -> Result<Value> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (Some(i), Some(o)) = (q.mark_in, q.mark_out) else { return Err(EngineError::Other("mark an In and an Out point first".into())) };
    let dur = o + q.settings.frame_rate.frame_duration() - i;
    let name = str_p(p, "name").unwrap_or("").to_string();
    let id = add_marker(s, "Add Range Marker", i, dur, MarkerKind::Comment, name, p)?;
    Ok(json!({"marker": id.0}))
}

fn filter_colors(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(a) = p.get("hidden").and_then(Value::as_array) {
        let mut v = Vec::new();
        for x in a {
            let n = x.as_str().unwrap_or_default();
            v.push(Label::from_name(n).ok_or_else(|| bad("markers.filterColors", format!("unknown colour `{n}`")))?);
        }
        s.state.hidden_marker_colors = v;
    } else if let Some(n) = str_p(p, "color") {
        let c = Label::from_name(n).ok_or_else(|| bad("markers.filterColors", format!("unknown colour `{n}`")))?;
        let hidden = s.state.hidden_marker_colors.contains(&c);
        let visible = bool_p(p, "visible").unwrap_or(hidden);
        s.state.hidden_marker_colors.retain(|x| *x != c);
        if !visible {
            s.state.hidden_marker_colors.push(c);
        }
    } else {
        return Err(bad("markers.filterColors", "need `hidden` or `color`"));
    }
    Ok(json!({"hidden": s.state.hidden_marker_colors.iter().map(|c| c.name()).collect::<Vec<_>>()}))
}
