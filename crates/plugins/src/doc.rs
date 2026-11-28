//! `doc.query`: read-only answers about the open project.
//!
//! The query is `{"what": "project" | "sequence" | "selection"}`, default `project`. The scope is
//! `metadata`. `pixels` is not served: FilmCraft has no pixel snapshot API for a plug-in yet, so
//! the call ends as `E_UNSUPPORTED` rather than a made-up frame.

use craft_plugin_abi::permissions::DocScope;
use craft_plugin_host::services::QueryFailure;
use filmcraft_engine::Session;
use filmcraft_engine::project::{Sequence, Track};
use serde_json::{Value, json};

/// Answer `query` from the session. Does not change it.
pub fn query(s: &Session, scope: DocScope, query: &Value) -> Result<Value, QueryFailure> {
    if scope != DocScope::Metadata {
        return Err(QueryFailure::Unsupported);
    }
    match query.get("what").and_then(Value::as_str).unwrap_or("project") {
        "project" => Ok(project(s)),
        "sequence" => Ok(sequence(s)),
        "selection" => Ok(selection(s)),
        other => Err(QueryFailure::BadQuery(format!("unknown `what`: {other}"))),
    }
}

fn track_json(t: &Track) -> Value {
    json!({ "id": t.id.0, "name": t.name, "kind": format!("{:?}", t.kind), "clips": t.items.len(), "locked": t.locked })
}

fn sequence_json(q: &Sequence, id: u64, name: &str) -> Value {
    json!({
        "id": id,
        "name": name,
        "width": q.settings.width,
        "height": q.settings.height,
        "frame_rate": { "num": q.settings.frame_rate.num, "den": q.settings.frame_rate.den },
        "duration_ticks": q.duration().0,
        "video_tracks": q.video_tracks.iter().map(track_json).collect::<Vec<_>>(),
        "audio_tracks": q.audio_tracks.iter().map(track_json).collect::<Vec<_>>(),
        "markers": q.markers.len(),
    })
}

fn project(s: &Session) -> Value {
    let active = s.state.active_sequence.and_then(|id| s.project.items.get(&id).map(|item| (id, item)));
    json!({
        "name": s.project.name,
        "path": s.path,
        "revision": s.revision,
        "dirty": s.is_dirty(),
        "items": s.project.items.len(),
        "active_sequence": active.and_then(|(id, item)| item.as_sequence().map(|q| sequence_json(q, id.0, &item.name))),
        "can_undo": s.history.can_undo(),
        "can_redo": s.history.can_redo(),
    })
}

fn sequence(s: &Session) -> Value {
    match s.state.active_sequence.and_then(|id| s.project.items.get(&id).map(|item| (id, item))) {
        Some((id, item)) => item.as_sequence().map_or(Value::Null, |q| sequence_json(q, id.0, &item.name)),
        None => Value::Null,
    }
}

fn selection(s: &Session) -> Value {
    let clips: Vec<Value> = s
        .active_sequence()
        .map(|q| {
            s.state
                .selection
                .iter()
                .filter_map(|id| {
                    q.find_item(*id)
                        .map(|(track, item)| json!({ "id": id.0, "name": item.name, "track": track.0, "start": item.start.0, "duration": item.duration.0 }))
                })
                .collect()
        })
        .unwrap_or_default();
    json!({ "clips": clips, "playhead_ticks": s.playhead().0 })
}
