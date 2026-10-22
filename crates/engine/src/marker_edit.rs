//! Explicit marker ownership and atomic single/bulk edits.
use crate::{
    EngineError, Result, Session,
    commands::{bad, marker_author_p, marker_color_p},
};
use filmcraft_project::{ItemId, Marker, MarkerId, Project};
use filmcraft_time::{FrameRate, Tick};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn list(project: &mut Project, owner: ItemId) -> Result<&mut Vec<Marker>> {
    match &mut project.item_mut(owner).ok_or_else(|| EngineError::Other("Marker owner no longer exists".into()))?.kind {
        filmcraft_project::ItemKind::Media(m) => Ok(&mut m.markers),
        filmcraft_project::ItemKind::Sequence(q) => Ok(&mut std::sync::Arc::make_mut(q).markers),
        _ => Err(EngineError::Other("This item cannot own markers".into())),
    }
}

fn item(p: &Value, key: &str, fallback: Option<ItemId>, id: &str) -> Result<ItemId> {
    match p.get(key) {
        Some(v) => v.as_u64().map(ItemId).ok_or_else(|| bad(id, "owner must be an item id")),
        None => fallback.ok_or_else(|| bad(id, "no marker owner is open")),
    }
}
fn frames(p: &Value, key: &str, rate: FrameRate, id: &str) -> Result<Option<Tick>> {
    p.get(key)
        .map(|v| {
            let f = v.as_i64().ok_or_else(|| bad(id, "frame values must be integers"))?;
            if f < 0 || f > rate.frame_at(Tick::MAX) {
                return Err(bad(id, "frame value is outside the supported time range"));
            }
            Ok(rate.tick_of(f))
        })
        .transpose()
}

pub(crate) fn run(s: &mut Session, p: &Value, delete: bool) -> Result<Value> {
    let id = if delete { "markers.delete" } else { "markers.edit" };
    let (owner, rate, lower, upper, visible) = match p.get("target") {
        None | Some(Value::String(_)) if p.get("target").is_none_or(|v| v == "program") => {
            let owner = item(p, "sequence", s.state.active_sequence, id)?;
            let q = s.project.sequence(owner).ok_or_else(|| bad(id, "no such sequence"))?;
            (owner, q.settings.frame_rate, Tick::ZERO, Tick::MAX, None)
        }
        Some(Value::String(t)) if t == "source" => {
            let source = item(p, "item", s.state.source_item, id)?;
            let v = crate::clip_ops::source_view(s, source).ok_or_else(|| bad(id, "no such source"))?;
            (v.media, v.rate, v.start, v.end, Some(v.markers.iter().map(|m| m.id).collect::<BTreeSet<_>>()))
        }
        _ => return Err(bad(id, "target must be program or source")),
    };
    let ids: BTreeSet<MarkerId> = if let Some(v) = p.get("markers") {
        if p.get("marker").is_some() {
            return Err(bad(id, "use marker or markers, not both"));
        }
        let a = v.as_array().filter(|a| !a.is_empty() && a.len() <= 100_000).ok_or_else(|| bad(id, "markers must contain 1 to 100000 ids"))?;
        a.iter().map(|v| v.as_u64().map(MarkerId).ok_or_else(|| bad(id, "invalid marker id"))).collect::<Result<_>>()?
    } else {
        BTreeSet::from([MarkerId(p.get("marker").and_then(Value::as_u64).ok_or_else(|| bad(id, "need marker id"))?)])
    };
    let time = match p.get("time") {
        Some(v) => {
            if p.get("frame").is_some() {
                return Err(bad(id, "use time or frame, not both"));
            }
            Some(Tick(v.as_i64().ok_or_else(|| bad(id, "time must be integer ticks"))?))
        }
        None => frames(p, "frame", rate, id)?,
    };
    let duration = frames(p, "durationFrames", rate, id)?;
    if ids.len() > 1 && (time.is_some() || duration.is_some()) {
        return Err(bad(id, "time edits require a single marker"));
    }
    let color = marker_color_p(p, id)?;
    let author = marker_author_p(p, id)?;
    let text = |key| -> Result<Option<String>> {
        p.get(key)
            .map(|v| v.as_str().filter(|v| v.len() <= 65536).map(str::to_owned).ok_or_else(|| bad(id, "marker text must be a string of at most 65536 bytes")))
            .transpose()
    };
    let name = text("name")?;
    let comment = text("comment")?;
    s.edit(if delete { "Delete Markers" } else { "Edit Markers" }, |project, _| {
        let markers = list(project, owner)?;
        let present: BTreeSet<_> = markers.iter().map(|m| m.id).collect();
        if !ids.is_subset(&present) {
            return Err(bad(id, "no such marker in this owner"));
        }
        if visible.as_ref().is_some_and(|v| !ids.is_subset(v)) {
            return Err(bad(id, "marker is outside the source view"));
        }
        for m in markers.iter().filter(|m| ids.contains(&m.id)) {
            if !delete && (time.is_some() || duration.is_some()) {
                let start = time.unwrap_or(m.start);
                let dur = duration.unwrap_or(m.duration);
                let end = start.0.checked_add(dur.0).ok_or_else(|| bad(id, "marker end overflows"))?;
                if start < lower || start >= upper || dur < Tick::ZERO || end > upper.0 {
                    return Err(bad(id, "marker range is outside the source or supported time range"));
                }
            }
        }
        if delete {
            markers.retain(|m| !ids.contains(&m.id));
        } else {
            for m in markers.iter_mut().filter(|m| ids.contains(&m.id)) {
                if let Some(v) = &name {
                    m.name.clone_from(v);
                }
                if let Some(v) = &comment {
                    m.comment.clone_from(v);
                }
                if let Some(v) = &author {
                    m.author.clone_from(v);
                }
                if let Some((preset, custom)) = color {
                    if custom.is_none() {
                        m.color = preset;
                    }
                    m.custom_color = custom;
                }
                if let Some(v) = time {
                    m.start = v;
                }
                if let Some(v) = duration {
                    m.duration = v;
                }
            }
            markers.sort_by_key(|m| m.start);
        }
        Ok(())
    })?;
    Ok(json!({"count":ids.len()}))
}
