//! Read-only, on-demand descriptions of the project snapshots already kept for undo.
//!
//! No second journal is maintained: merged gestures, grouped edits, direct UI edits and redo
//! therefore describe the same state transitions that Undo actually applies. The default
//! `history.list` response stays unchanged; callers opt in to the more expensive snapshot scan.

use std::collections::{BTreeMap, BTreeSet};

use filmcraft_project::{ClipId, ItemId, Project, TrackId, TrackItem};
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

const DEFAULT_ENTRIES: usize = 20;
const MAX_ENTRIES: usize = 200;
const MAX_CHANGES: usize = 100;

pub(crate) fn list(s: &Session, params: &Value) -> Result<Value> {
    let detail = match params.get("detail") {
        None => false,
        Some(Value::Bool(v)) => *v,
        Some(_) => return Err(bad("detail must be a boolean")),
    };
    let limit = match params.get("limit") {
        None => DEFAULT_ENTRIES,
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=MAX_ENTRIES).contains(n))
            .ok_or_else(|| bad("limit must be an integer from 1 to 200"))?,
    };
    let mut result = json!({
        "undo": s.history.undo.iter().map(|h| &h.0).collect::<Vec<_>>(),
        "redo": s.history.redo.iter().map(|h| &h.0).collect::<Vec<_>>(),
    });
    if !detail {
        return Ok(result);
    }
    let undo_start = s.history.undo.len().saturating_sub(limit);
    let redo_start = s.history.redo.len().saturating_sub(limit);
    let undo: Vec<Value> = s
        .history
        .undo
        .iter()
        .enumerate()
        .skip(undo_start)
        .map(|(index, (label, before))| {
            let after = s.history.undo.get(index.saturating_add(1)).map_or(s.project.as_ref(), |entry| entry.1.as_ref());
            describe(index, label, before, after)
        })
        .collect();
    // Redo's last entry is the next state to restore. Describe the original forward edit,
    // rather than reversing its added/removed semantics just because it is currently undone.
    let redo: Vec<Value> = s
        .history
        .redo
        .iter()
        .enumerate()
        .skip(redo_start)
        .map(|(index, (label, after))| {
            let before = s.history.redo.get(index.saturating_add(1)).map_or(s.project.as_ref(), |entry| entry.1.as_ref());
            describe(index, label, before, after)
        })
        .collect();
    result["details"] = json!({
        "undo": undo, "redo": redo,
        "undoOmitted": undo_start, "redoOmitted": redo_start,
        "maxChangesPerEntry": MAX_CHANGES,
        "order": "stack order; last entry is the next undo or redo",
        "direction": "original forward edit",
    });
    Ok(result)
}

// Project defaults contain NaN auto-points. serde represents those consistently as null;
// raw PartialEq would mark untouched clips as changed (NaN != NaN).
fn same<T: serde::Serialize>(a: T, b: T) -> bool {
    match (serde_json::to_value(a), serde_json::to_value(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn bad(message: &str) -> EngineError {
    EngineError::BadParams { cmd: "history.list".into(), msg: message.into() }
}

/// Numeric ids remain ids, times remain exact ticks. Names are display hints only.
fn clip_state(sequence: ItemId, track: TrackId, clip: &TrackItem) -> Value {
    json!({
        "sequence": sequence.0, "track": track.0, "item": clip.item.0,
        "name": short(&clip.name), "start": clip.start.0, "duration": clip.duration.0,
        "sourceIn": clip.source_in.0, "speed": clip.speed, "reverse": clip.reverse,
        "enabled": clip.enabled, "audioStream": clip.audio_stream,
        "sourceChannels": clip.source_channels.iter().take(32).collect::<Vec<_>>(),
        "sourceChannelsOmitted": clip.source_channels.len().saturating_sub(32), "gainDb": clip.gain_db,
        "holdFilters": clip.hold_filters, "timeInterpolation": clip.time_interpolation,
    })
}

struct ClipRef<'a> {
    sequence: ItemId,
    track: TrackId,
    clip: &'a TrackItem,
}

fn clips(project: &Project) -> BTreeMap<(ItemId, ClipId), ClipRef<'_>> {
    let mut result = BTreeMap::new();
    for item in project.items.values() {
        if let Some(sequence) = item.as_sequence() {
            for track in sequence.all_tracks() {
                for clip in &track.items {
                    result.insert((item.id, clip.id), ClipRef { sequence: item.id, track: track.id, clip });
                }
            }
        }
    }
    result
}

fn clip_json(c: &ClipRef<'_>) -> Value {
    clip_state(c.sequence, c.track, c.clip)
}

/// Return field names without dumping file paths, transcript text, media metadata or keyframes.
/// The snapshot is the evidence. We intentionally do not guess a command id or its parameters:
/// direct UI edits can bypass Session::execute, and one gesture can contain several commands.
fn clip_fields(a: &ClipRef<'_>, b: &ClipRef<'_>) -> Vec<&'static str> {
    let (x, y) = (a.clip, b.clip);
    let checks = [
        (a.sequence != b.sequence, "sequence"),
        (a.track != b.track, "track"),
        (x.item != y.item, "item"),
        (x.name != y.name, "name"),
        (x.start != y.start, "start"),
        (x.duration != y.duration, "duration"),
        (x.source_in != y.source_in, "sourceIn"),
        (x.speed != y.speed, "speed"),
        (x.reverse != y.reverse, "reverse"),
        (x.enabled != y.enabled, "enabled"),
        (x.audio_stream != y.audio_stream, "audioStream"),
        (x.source_channels != y.source_channels, "sourceChannels"),
        (!same(&x.effects, &y.effects), "effects"),
        (x.markers != y.markers, "markers"),
        (x.link != y.link, "link"),
        (x.group != y.group, "group"),
        (x.label != y.label, "label"),
        (x.frame_hold != y.frame_hold, "frameHold"),
        (x.scale_to_frame != y.scale_to_frame, "scaleToFrame"),
        (x.gain_db != y.gain_db, "gainDb"),
        (!same(&x.essential, &y.essential), "essentialSound"),
        (x.multicam != y.multicam, "multicam"),
        (x.time_interpolation != y.time_interpolation, "timeInterpolation"),
        (x.hold_filters != y.hold_filters, "holdFilters"),
        (x.field_options != y.field_options, "fieldOptions"),
        (!same(&x.graphic, &y.graphic), "graphic"),
    ];
    checks.into_iter().filter_map(|(changed, name)| changed.then_some(name)).collect()
}

// Text can come from imported project files. Bound display hints, and never repeat arbitrary
// parameter text or entire keyframe arrays in a history response.
fn short(s: &str) -> String {
    let mut chars = s.chars();
    let mut result: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() {
        result.push_str("...");
    }
    result
}

fn param_summary(p: &filmcraft_project::Param) -> Value {
    use filmcraft_project::ParamValue;
    let value = match &p.value {
        ParamValue::Float(v) => json!(v),
        ParamValue::Bool(v) => json!(v),
        ParamValue::Choice(v) => json!(v),
        _ => json!({"structured": true}),
    };
    json!({"value": value, "keyframeCount": p.keyframes.len()})
}

fn effect_changes(a: Option<&TrackItem>, b: Option<&TrackItem>) -> Value {
    let empty = Vec::new();
    let left = a.map_or(&empty, |c| &c.effects);
    let right = b.map_or(&empty, |c| &c.effects);
    let mut effects = Vec::new();
    let mut count = 0usize;
    // Index is significant: two instances with the same id may be reordered independently.
    for index in 0..left.len().max(right.len()) {
        let (a, b) = (left.get(index), right.get(index));
        if same(&a, &b) {
            continue;
        }
        count = count.saturating_add(1);
        if effects.len() >= 20 {
            continue;
        }
        let mut parameters = Vec::new();
        let keys: BTreeSet<_> = a.into_iter().flat_map(|e| e.params.keys()).chain(b.into_iter().flat_map(|e| e.params.keys())).collect();
        let mut param_count = 0usize;
        for key in keys {
            let (x, y) = (a.and_then(|e| e.params.get(key)), b.and_then(|e| e.params.get(key)));
            if same(&x, &y) {
                continue;
            }
            param_count = param_count.saturating_add(1);
            if parameters.len() < 20 {
                parameters.push(json!({"id": short(key), "before": x.map(param_summary), "after": y.map(param_summary)}));
            }
        }
        effects.push(json!({
            "index": index, "beforeId": a.map(|e| short(&e.effect)), "afterId": b.map(|e| short(&e.effect)),
            "beforeEnabled": a.map(|e| e.enabled), "afterEnabled": b.map(|e| e.enabled),
            "parametersOmitted": param_count.saturating_sub(parameters.len()), "parameters": parameters,
        }));
    }
    json!({"changes": effects, "omitted": count.saturating_sub(effects.len())})
}

fn describe(index: usize, label: &str, before: &Project, after: &Project) -> Value {
    let mut items = Vec::new();
    let mut item_count = 0usize;
    let ids: BTreeSet<_> = before.items.keys().chain(after.items.keys()).copied().collect();
    for id in ids {
        let (a, b) = (before.item(id), after.item(id));
        if same(&a, &b) {
            continue;
        }
        item_count = item_count.saturating_add(1);
        if items.len() < MAX_CHANGES {
            let status = match (a, b) {
                (None, _) => "added",
                (_, None) => "removed",
                _ => "changed",
            };
            items.push(json!({"id": id.0, "change": status,
                "beforeName": a.map(|i| short(&i.name)), "afterName": b.map(|i| short(&i.name))}));
        }
    }
    let (a_clips, b_clips) = (clips(before), clips(after));
    let ids: BTreeSet<_> = a_clips.keys().chain(b_clips.keys()).copied().collect();
    let mut changes = Vec::new();
    let mut clip_count = 0usize;
    for id in ids {
        let (a, b) = (a_clips.get(&id), b_clips.get(&id));
        let fields = match (a, b) {
            (Some(x), Some(y)) if x.track == y.track && same(x.clip, y.clip) => continue,
            (Some(x), Some(y)) => clip_fields(x, y),
            _ => Vec::new(),
        };
        clip_count = clip_count.saturating_add(1);
        if changes.len() < MAX_CHANGES {
            let status = match (a, b) {
                (None, _) => "added",
                (_, None) => "removed",
                _ => "changed",
            };
            changes.push(json!({"id": id.1.0, "sequence": id.0.0, "change": status, "fields": fields,
                "before": a.map(clip_json), "after": b.map(clip_json),
                "effects": effect_changes(a.map(|c| c.clip), b.map(|c| c.clip))}));
        }
    }
    json!({
        "index": index, "label": short(label), "items": items, "clips": changes,
        "itemCount": item_count, "clipCount": clip_count,
        "itemsOmitted": item_count.saturating_sub(items.len()),
        "clipsOmitted": clip_count.saturating_sub(changes.len()),
        "binsChanged": before.root != after.root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineError, History};
    use filmcraft_project::{Label, TrackKind};
    use std::sync::Arc;

    fn demo() -> Session {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.history = History { limit: 200, ..Default::default() };
        s
    }

    fn detail(s: &mut Session) -> Value {
        s.execute("history.list", json!({"detail": true})).unwrap()
    }

    fn first_clip(s: &Session) -> ClipId {
        s.active_sequence().unwrap().video_tracks[0].items[0].id
    }

    fn change(s: &mut Session, label: &str, delta: i64) {
        let sequence = s.state.active_sequence.unwrap();
        s.edit(label, |p, _| {
            p.sequence_mut(sequence).unwrap().video_tracks[0].items[0].source_in.0 += delta;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn duplicated_sequence_keeps_both_clip_namespaces_visible() {
        let mut s = demo();
        let original = s.state.active_sequence.unwrap();
        let clip = first_clip(&s);
        let original_count = s.active_sequence().unwrap().all_tracks().map(|t| t.items.len()).sum::<usize>();
        s.state.project_selection = vec![original];
        let duplicated = s.execute("edit.duplicate", json!({})).unwrap();
        let copy = ItemId(duplicated["items"][0].as_u64().unwrap());
        assert_eq!(s.project.sequence(copy).unwrap().video_tracks[0].items[0].id, clip);
        let d = detail(&mut s);
        let entry = &d["details"]["undo"][0];
        assert_eq!(entry["clipCount"], original_count);
        assert!(entry["clips"].as_array().unwrap().iter().all(|c| c["change"] == "added" && c["after"]["sequence"] == copy.0));
        s.history = History { limit: 200, ..Default::default() };
        change(&mut s, "Trim original", 17);
        let d = detail(&mut s);
        let entry = &d["details"]["undo"][0];
        assert_eq!(entry["clipCount"], 1);
        assert_eq!(entry["clips"][0]["id"], clip.0);
        assert_eq!(entry["clips"][0]["before"]["sequence"], original.0);
        assert_eq!(entry["clips"][0]["fields"], json!(["sourceIn"]));
    }

    #[test]
    fn duplicate_sequence_removal_and_redo_keep_original_clips() {
        let mut s = demo();
        let original = s.state.active_sequence.unwrap();
        s.state.project_selection = vec![original];
        let r = s.execute("edit.duplicate", json!({})).unwrap();
        let copy = ItemId(r["items"][0].as_u64().unwrap());
        s.history = History { limit: 200, ..Default::default() };
        s.edit("Remove copied clips", |p, _| {
            p.sequence_mut(copy).unwrap().video_tracks[0].items.clear();
            Ok(())
        })
        .unwrap();
        let before = detail(&mut s)["details"]["undo"][0].clone();
        assert!(before["clipCount"].as_u64().unwrap() > 0);
        assert!(before["clips"].as_array().unwrap().iter().all(|c| c["sequence"] == copy.0 && c["change"] == "removed"));
        s.undo().unwrap();
        assert_eq!(detail(&mut s)["details"]["redo"][0], before);
        s.redo().unwrap();
        assert_eq!(detail(&mut s)["details"]["undo"][0], before);
        assert!(!s.project.sequence(original).unwrap().video_tracks[0].items.is_empty());
    }

    #[test]
    fn cross_sequence_transfer_reports_two_sequence_scoped_changes() {
        let mut s = demo();
        let original = s.state.active_sequence.unwrap();
        let id = first_clip(&s);
        s.edit("Transfer", |p, _| {
            let other = p.new_sequence("Destination", Default::default(), 1, 0, None);
            let clip = p.sequence_mut(original).unwrap().video_tracks[0].items.remove(0);
            p.sequence_mut(other).unwrap().video_tracks[0].items.push(clip);
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        let e = &d["details"]["undo"][0];
        assert_eq!(e["clipCount"], 2);
        let c = e["clips"].as_array().unwrap();
        assert!(c.iter().all(|c| c["id"] == id.0));
        assert!(c.iter().any(|c| c["sequence"] == original.0 && c["change"] == "removed"));
        assert!(c.iter().any(|c| c["sequence"] != original.0 && c["change"] == "added"));
    }

    #[test]
    fn default_response_remains_exactly_the_label_stacks() {
        let mut s = demo();
        change(&mut s, "Trim", 10);
        assert_eq!(s.execute("history.list", json!({})).unwrap(), json!({"undo": ["Trim"], "redo": []}));
        assert_eq!(s.execute("history.list", json!({"detail": false})).unwrap(), json!({"undo": ["Trim"], "redo": []}));
    }

    #[test]
    fn empty_history_has_empty_details_and_no_omissions() {
        let mut s = demo();
        let d = detail(&mut s);
        assert_eq!(d["details"]["undo"], json!([]));
        assert_eq!(d["details"]["redo"], json!([]));
        assert_eq!(d["details"]["undoOmitted"], 0);
        assert_eq!(d["details"]["redoOmitted"], 0);
    }

    #[test]
    fn direct_edits_report_actual_clip_ids_and_exact_ticks() {
        let mut s = demo();
        let id = first_clip(&s);
        let before = s.active_sequence().unwrap().video_tracks[0].items[0].source_in.0;
        change(&mut s, "Trim", 17);
        let d = detail(&mut s);
        let entry = &d["details"]["undo"][0];
        assert_eq!(entry["label"], "Trim");
        assert_eq!(entry["clipCount"], 1);
        assert_eq!(entry["clips"][0]["id"], id.0);
        assert_eq!(entry["clips"][0]["fields"], json!(["sourceIn"]));
        assert_eq!(entry["clips"][0]["before"]["sourceIn"], before);
        assert_eq!(entry["clips"][0]["after"]["sourceIn"], before + 17);
    }

    #[test]
    fn repeated_labels_still_describe_distinct_transitions() {
        let mut s = demo();
        change(&mut s, "Trim", 10);
        change(&mut s, "Trim", 20);
        let d = detail(&mut s);
        let entries = d["details"]["undo"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        let first = &entries[0]["clips"][0];
        let second = &entries[1]["clips"][0];
        assert_eq!(first["after"]["sourceIn"], second["before"]["sourceIn"]);
        assert_eq!(second["after"]["sourceIn"].as_i64().unwrap() - second["before"]["sourceIn"].as_i64().unwrap(), 20);
    }

    #[test]
    fn undo_redo_keep_original_forward_semantics() {
        let mut s = demo();
        change(&mut s, "First", 10);
        change(&mut s, "Second", 20);
        let original = detail(&mut s)["details"]["undo"].clone();
        s.undo().unwrap();
        s.undo().unwrap();
        let d = detail(&mut s);
        assert_eq!(d["details"]["redo"][0]["clips"], original[1]["clips"]);
        assert_eq!(d["details"]["redo"][1]["clips"], original[0]["clips"]);
        s.redo().unwrap();
        s.redo().unwrap();
        assert_eq!(detail(&mut s)["details"]["undo"], original);
    }

    #[test]
    fn merged_gesture_has_one_net_transition() {
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        for value in [10, 20, 30] {
            s.edit_merged("Trim", "gesture", |p, _| {
                p.sequence_mut(sequence).unwrap().video_tracks[0].items[0].source_in.0 = value;
                Ok(())
            })
            .unwrap();
        }
        let d = detail(&mut s);
        assert_eq!(d["details"]["undo"].as_array().unwrap().len(), 1);
        assert_eq!(d["details"]["undo"][0]["clips"][0]["after"]["sourceIn"], 30);
    }

    #[test]
    fn groups_report_net_effect_without_internal_steps() {
        let mut s = demo();
        s.edit_group("Batch", |s| {
            change(s, "One", 10);
            change(s, "Two", 20);
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        assert_eq!(d["undo"], json!(["Batch"]));
        assert_eq!(d["details"]["undo"].as_array().unwrap().len(), 1);
        assert_eq!(d["details"]["undo"][0]["clipCount"], 1);
    }

    #[test]
    fn failed_group_leaves_details_unchanged() {
        let mut s = demo();
        change(&mut s, "Before", 10);
        let before = detail(&mut s);
        let result: crate::Result<()> = s.edit_group("Failed", |s| {
            change(s, "Temporary", 100);
            Err(EngineError::Other("failure".into()))
        });
        assert!(result.is_err());
        assert_eq!(detail(&mut s), before);
    }

    #[test]
    fn branch_after_undo_discards_redo_details() {
        let mut s = demo();
        change(&mut s, "Old", 10);
        s.undo().unwrap();
        change(&mut s, "New", 20);
        let d = detail(&mut s);
        assert_eq!(d["redo"], json!([]));
        assert_eq!(d["details"]["redo"], json!([]));
        assert_eq!(d["details"]["undo"][0]["label"], "New");
    }

    #[test]
    fn limit_selects_nearest_stack_entries_without_changing_legacy_labels() {
        let mut s = demo();
        for _ in 0..5 {
            change(&mut s, "Trim", 1);
        }
        let d = s.execute("history.list", json!({"detail": true, "limit": 2})).unwrap();
        assert_eq!(d["undo"].as_array().unwrap().len(), 5);
        assert_eq!(d["details"]["undo"].as_array().unwrap().len(), 2);
        assert_eq!(d["details"]["undo"][0]["index"], 3);
        assert_eq!(d["details"]["undoOmitted"], 3);
    }

    #[test]
    fn invalid_limits_and_detail_values_fail_without_editing() {
        let mut s = demo();
        change(&mut s, "Trim", 1);
        let project = s.project.clone();
        for p in [
            json!({"detail": "yes"}),
            json!({"limit": 0}),
            json!({"limit": 201}),
            json!({"limit": -1}),
            json!({"limit": 1.5}),
            json!({"limit": "2"}),
            json!({"limit": null}),
        ] {
            assert!(s.execute("history.list", p).is_err());
            assert!(Arc::ptr_eq(&s.project, &project));
            assert_eq!(s.history.undo.len(), 1);
        }
    }

    #[test]
    fn history_query_preserves_source_recent_clip_order_after_direct_edit() {
        let mut s = demo();
        let ids: Vec<_> = s.project.items.keys().copied().take(2).collect();
        assert_eq!(ids.len(), 2);
        s.edit("Set source state", |_, state| {
            state.source_item = Some(ids[0]);
            state.source_history = vec![ids[1], ids[0]];
            Ok(())
        })
        .unwrap();
        let before = s.state.source_history.clone();
        for params in [json!({}), json!({"detail": true}), json!({"limit": 0})] {
            let _ = s.execute("history.list", params);
            assert_eq!(s.state.source_history, before);
        }
        s.execute("source.open", json!({"item": ids[0].0})).unwrap();
        assert_eq!(s.state.source_history, vec![ids[0], ids[1]]);
    }

    #[test]
    fn read_only_query_preserves_project_revision_journal_and_history() {
        let mut s = demo();
        change(&mut s, "Trim", 1);
        let project = s.project.clone();
        let revision = s.revision;
        let journal = s.journal.clone();
        let before = detail(&mut s);
        for _ in 0..3 {
            assert_eq!(detail(&mut s), before);
        }
        assert!(Arc::ptr_eq(&s.project, &project));
        assert_eq!(s.revision, revision);
        assert_eq!(s.journal, journal);
    }

    #[test]
    fn unchanged_nan_auto_points_are_not_false_positive_clip_changes() {
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        s.edit("Rename", |p, _| {
            p.item_mut(sequence).unwrap().name = "New title".into();
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        assert_eq!(d["details"]["undo"][0]["clipCount"], 0);
        assert_eq!(d["details"]["undo"][0]["itemCount"], 1);
    }

    #[test]
    fn moving_between_tracks_keeps_one_identity() {
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        let id = first_clip(&s);
        s.edit("Move", |p, _| {
            let q = p.sequence_mut(sequence).unwrap();
            let clip = q.video_tracks[0].items.remove(0);
            q.video_tracks[1].items.push(clip);
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        let c = &d["details"]["undo"][0]["clips"][0];
        assert_eq!(c["id"], id.0);
        assert_eq!(c["change"], "changed");
        assert_eq!(c["fields"], json!(["track"]));
        assert_ne!(c["before"]["track"], c["after"]["track"]);
    }

    #[test]
    fn clip_removal_has_before_but_no_after() {
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        s.edit("Delete", |p, _| {
            p.sequence_mut(sequence).unwrap().video_tracks[0].items.remove(0);
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        let c = &d["details"]["undo"][0]["clips"][0];
        assert_eq!(c["change"], "removed");
        assert!(c["before"].is_object());
        assert!(c["after"].is_null());
    }

    #[test]
    fn audio_gain_and_video_options_name_every_changed_field() {
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        s.edit("Clip options", |p, _| {
            let clip = &mut p.sequence_mut(sequence).unwrap().video_tracks[0].items[0];
            clip.gain_db = -6.0;
            clip.hold_filters = true;
            clip.time_interpolation = filmcraft_project::TimeInterpolation::FrameBlending;
            clip.field_options = Some(Default::default());
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        let c = &d["details"]["undo"][0]["clips"][0];
        for name in ["gainDb", "holdFilters", "timeInterpolation", "fieldOptions"] {
            assert!(c["fields"].as_array().unwrap().contains(&json!(name)), "{name}: {c}");
        }
        assert_eq!(c["after"]["gainDb"], -6.0);
        assert_eq!(c["after"]["holdFilters"], true);
    }

    #[test]
    fn hostile_audio_routing_lists_are_bounded_with_omissions() {
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        s.edit("Routing", |p, _| {
            p.sequence_mut(sequence).unwrap().video_tracks[0].items[0].source_channels = vec![1; 1000];
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        let c = &d["details"]["undo"][0]["clips"][0]["after"];
        assert_eq!(c["sourceChannels"].as_array().unwrap().len(), 32);
        assert_eq!(c["sourceChannelsOmitted"], 968);
    }

    #[test]
    fn metadata_only_edit_names_item_without_dumping_metadata() {
        let mut s = demo();
        let id = s.state.active_sequence.unwrap();
        s.edit("Metadata", |p, _| {
            p.item_mut(id).unwrap().metadata.insert("note".into(), "private long text".into());
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        assert_eq!(d["details"]["undo"][0]["items"][0]["id"], id.0);
        assert!(!d.to_string().contains("private long text"));
    }

    #[test]
    fn reverting_cancelled_gesture_drops_its_details() {
        let mut s = demo();
        change(&mut s, "Keep", 10);
        let mark = s.undo_mark();
        let before = detail(&mut s);
        change(&mut s, "Cancel", 100);
        assert_eq!(s.revert_to(mark), 1);
        assert_eq!(detail(&mut s), before);
    }

    #[test]
    fn retained_history_limit_has_correct_latest_before_and_after() {
        let mut s = demo();
        s.history.limit = 2;
        for _ in 0..5 {
            change(&mut s, "Trim", 10);
        }
        let d = detail(&mut s);
        assert_eq!(d["details"]["undo"].as_array().unwrap().len(), 2);
        let c = &d["details"]["undo"][1]["clips"][0];
        assert_eq!(c["after"]["sourceIn"].as_i64().unwrap() - c["before"]["sourceIn"].as_i64().unwrap(), 10);
    }

    #[test]
    fn project_item_addition_and_removal_are_distinguished() {
        let mut s = demo();
        s.edit("Add", |p, _| {
            p.new_sequence("Extra", Default::default(), 1, 1, None);
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        assert_eq!(d["details"]["undo"][0]["items"][0]["change"], "added");
        s.undo().unwrap();
        let r = detail(&mut s);
        assert_eq!(r["details"]["redo"][0]["items"][0]["change"], "added");
    }

    #[test]
    fn effect_edit_identifies_only_the_changed_clip() {
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        s.edit("Effect", |p, _| {
            let clip = &mut p.sequence_mut(sequence).unwrap().video_tracks[0].items[0];
            clip.effects[0].enabled = !clip.effects[0].enabled;
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        assert_eq!(d["details"]["undo"][0]["clipCount"], 1);
        assert_eq!(d["details"]["undo"][0]["clips"][0]["fields"], json!(["effects"]));
    }

    #[test]
    fn bounded_clip_changes_report_omitted_count() {
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        s.edit("Many", |p, _| {
            let template = p.sequence(sequence).unwrap().video_tracks[0].items[0].clone();
            let mut clips = Vec::new();
            for _ in 0..(MAX_CHANGES + 5) {
                let mut clip = template.clone();
                clip.id = ClipId(p.alloc_id());
                clips.push(clip);
            }
            p.sequence_mut(sequence).unwrap().tracks_mut(TrackKind::Video)[0].items.extend(clips);
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        let e = &d["details"]["undo"][0];
        assert_eq!(e["clipCount"], MAX_CHANGES + 5);
        assert_eq!(e["clips"].as_array().unwrap().len(), MAX_CHANGES);
        assert_eq!(e["clipsOmitted"], 5);
    }

    #[test]
    fn no_op_edit_is_explicitly_empty_not_an_invented_command() {
        let mut s = demo();
        s.edit("No-op", |_, _| Ok(())).unwrap();
        let d = detail(&mut s);
        let e = &d["details"]["undo"][0];
        assert_eq!(e["itemCount"], 0);
        assert_eq!(e["clipCount"], 0);
        assert!(e.get("command").is_none());
    }

    #[test]
    fn item_label_change_is_visible_without_clip_noise() {
        let mut s = demo();
        let id = s.state.active_sequence.unwrap();
        s.edit("Label", |p, _| {
            p.item_mut(id).unwrap().label = Label::Mango;
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        assert_eq!(d["details"]["undo"][0]["itemCount"], 1);
        assert_eq!(d["details"]["undo"][0]["clipCount"], 0);
    }

    #[test]
    fn static_effect_parameter_has_before_after_values() {
        use filmcraft_project::{Param, ParamValue};
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        s.edit("Parameter", |p, _| {
            let clip = &mut p.sequence_mut(sequence).unwrap().video_tracks[0].items[0];
            clip.effects[0].params.insert("test_gain".into(), Param::new(ParamValue::Float(0.25)));
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        let e = &d["details"]["undo"][0]["clips"][0]["effects"]["changes"][0];
        assert_eq!(e["parameters"][0]["id"], "test_gain");
        assert!(e["parameters"][0]["before"].is_null());
        assert_eq!(e["parameters"][0]["after"]["value"], 0.25);
        assert_eq!(e["parameters"][0]["after"]["keyframeCount"], 0);
    }

    #[test]
    fn text_and_keyframe_payloads_are_not_echoed() {
        use filmcraft_project::{Param, ParamValue};
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        s.edit("Text", |p, _| {
            let clip = &mut p.sequence_mut(sequence).unwrap().video_tracks[0].items[0];
            clip.effects[0].params.insert("text".into(), Param::new(ParamValue::Text("private script".repeat(500))));
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        assert!(!d.to_string().contains("private script"));
        assert!(d.to_string().contains("structured"));
    }

    #[test]
    fn effect_parameter_changes_are_bounded_and_counted() {
        use filmcraft_project::{Param, ParamValue};
        let mut s = demo();
        let sequence = s.state.active_sequence.unwrap();
        s.edit("Parameters", |p, _| {
            let clip = &mut p.sequence_mut(sequence).unwrap().video_tracks[0].items[0];
            for n in 0..25 {
                clip.effects[0].params.insert(format!("synthetic_{n}"), Param::new(ParamValue::Float(1.0)));
            }
            Ok(())
        })
        .unwrap();
        let d = detail(&mut s);
        let e = &d["details"]["undo"][0]["clips"][0]["effects"]["changes"][0];
        assert_eq!(e["parameters"].as_array().unwrap().len(), 20);
        assert_eq!(e["parametersOmitted"], 5);
    }

    #[test]
    fn unicode_display_hints_truncate_on_character_boundaries() {
        let text = "🦀".repeat(200);
        assert_eq!(short(&text).chars().count(), 163);
        assert!(short(&text).ends_with("..."));
        assert_eq!(short("short"), "short");
    }
}
