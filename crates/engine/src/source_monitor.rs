//! Source range arithmetic and Source-targeted marker commands. Edits use the session undo path.

use crate::clip_ops::source_view;
use crate::commands::bad;
use crate::{EngineError, Result, Session};
use filmcraft_project::{ItemId, Label, Marker, MarkerId, MarkerKind, Project};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeEdge {
    In,
    Out,
    Move,
}

/// Adjust one boundary or translate the span, constrained to the source and source frame grid.
pub fn adjust_range(bounds: TimeRange, range: TimeRange, rate: FrameRate, edge: RangeEdge, delta: Tick) -> Result<TimeRange> {
    let error = || EngineError::Other("invalid Source range or frame rate".into());
    let end = bounds.start.0.checked_add(bounds.duration.0).ok_or_else(error)?;
    let out = range.start.0.checked_add(range.duration.0).ok_or_else(error)?;
    if bounds.start.0 < 0 || bounds.duration.0 <= 0 || range.duration.0 <= 0 || range.start < bounds.start || out > end || rate.num <= 0 || rate.den <= 0 {
        return Err(error());
    }
    let scale = i128::from(TICKS_PER_SECOND).checked_mul(i128::from(rate.den)).ok_or_else(error)?;
    let fd = scale / i128::from(rate.num);
    if fd <= 0 || fd > i128::from(i64::MAX) {
        return Err(error());
    }
    let floor = |t: i128| -> Result<i128> {
        let f = t.checked_mul(i128::from(rate.num)).ok_or_else(error)?.div_euclid(scale);
        Ok(f.checked_mul(scale).ok_or_else(error)?.div_euclid(i128::from(rate.num)))
    };
    let nearest = |t: i128| -> Result<i128> {
        let a = floor(t)?;
        let f = t.checked_mul(i128::from(rate.num)).ok_or_else(error)?.div_euclid(scale);
        let b = f.checked_add(1).and_then(|f| f.checked_mul(scale)).ok_or_else(error)?.div_euclid(i128::from(rate.num));
        Ok(if t - a <= b - t { a } else { b })
    };
    let (lo, hi, start, stop, delta) = (i128::from(bounds.start.0), i128::from(end), i128::from(range.start.0), i128::from(out), i128::from(delta.0));
    let (a, b) = match edge {
        RangeEdge::In => {
            let max = floor((stop - fd.min(hi - lo)).max(lo))?.max(lo);
            if max < lo {
                return Err(error());
            }
            (nearest((start + delta).clamp(lo, max))?.clamp(lo, max), stop)
        }
        RangeEdge::Out => {
            let last = floor((hi - fd).max(lo))?.max(lo);
            if start > last {
                return Err(error());
            }
            let o = nearest((stop - fd + delta).clamp(start, last))?.clamp(start, last);
            (start, (o + fd).min(hi))
        }
        RangeEdge::Move => {
            let min = -floor(-(lo - start))?;
            let max = floor(hi - stop)?;
            if min > max {
                return Err(error());
            }
            let d = nearest(delta.clamp(min, max))?.clamp(min, max);
            (start + d, stop + d)
        }
    };
    let a = i64::try_from(a).map_err(|_| error())?;
    let b = i64::try_from(b).map_err(|_| error())?;
    Ok(TimeRange::from_bounds(Tick(a), Tick(b)))
}

pub fn source_command(id: &str) -> bool {
    id.starts_with("markers.markSplit")
        || id.starts_with("markers.goToSplit")
        || matches!(
            id,
            "markers.markIn"
                | "markers.markOut"
                | "markers.goToIn"
                | "markers.goToOut"
                | "markers.clearIn"
                | "markers.clearOut"
                | "markers.clearInOut"
                | "markers.add"
                | "markers.goNext"
                | "markers.goPrev"
                | "markers.clearCurrent"
                | "markers.clearAll"
                | "markers.edit"
        )
}

/// Most clips the Source monitor's list of recent clips keeps.
pub const SOURCE_HISTORY_MAX: usize = 20;

/// Keep the Source monitor's recent clips in step with the clip it shows: the open clip moves to
/// the front, items no longer in the project drop out.
pub(crate) fn track_history(s: &mut Session) {
    let current = s.state.source_item;
    let project = &s.project;
    let history = &mut s.state.source_history;
    history.retain(|i| project.item(*i).is_some());
    if let Some(cur) = current.filter(|i| project.item(*i).is_some())
        && history.first() != Some(&cur)
    {
        history.retain(|i| *i != cur);
        history.insert(0, cur);
    }
    history.truncate(SOURCE_HISTORY_MAX);
}

/// Enablement of Close / Close All: a clip is open in the Source monitor.
pub(crate) fn has_source_clip(s: &Session) -> std::result::Result<(), String> {
    s.state.source_item.map(|_| ()).ok_or_else(|| "no clip is open in the Source monitor".into())
}

/// Source panel ▸ Close (`all`: Close All). The open clip leaves the Source monitor and its list
/// of recent clips, and the next most recent one opens in its place, as in Premiere; Close All
/// empties the list and the monitor.
pub(crate) fn close(s: &mut Session, all: bool) -> Result<Value> {
    if all {
        s.state.source_history.clear();
    } else if let Some(cur) = s.state.source_item {
        s.state.source_history.retain(|i| *i != cur);
    }
    let next = s.state.source_history.iter().copied().find(|i| s.project.item(*i).is_some());
    match next {
        Some(i) => {
            s.execute("source.open", json!({"item": i.0}))?;
        }
        None => {
            s.state.source_item = None;
            s.state.source_playhead = Tick::ZERO;
        }
    }
    Ok(json!({"item": next.map(|i| i.0)}))
}

fn marker_list(p: &mut Project, item: ItemId) -> Result<&mut Vec<Marker>> {
    let it = p.item_mut(item).ok_or_else(|| EngineError::Other("Source item is unavailable".into()))?;
    match &mut it.kind {
        filmcraft_project::ItemKind::Media(m) => Ok(&mut m.markers),
        filmcraft_project::ItemKind::Sequence(q) => Ok(&mut std::sync::Arc::make_mut(q).markers),
        _ => Err(EngineError::Other("Source marker editing requires media or a sequence".into())),
    }
}

/// `None` leaves the existing Program command implementation in charge.
pub(crate) fn route(s: &mut Session, id: &str, p: &Value) -> Result<Option<Value>> {
    match p.get("target") {
        None => return Ok(None),
        Some(Value::String(t)) if t == "program" => return Ok(None),
        Some(Value::String(t)) if t == "source" => {}
        _ => return Err(bad(id, "target must be program or source")),
    }
    let item = s.state.source_item.ok_or_else(|| bad(id, "no Source clip is open"))?;
    let view = source_view(s, item).ok_or_else(|| bad(id, "Source item is unavailable"))?;
    let cursor = s.state.source_playhead;
    match id {
        "markers.goToIn" | "markers.goToOut" => {
            let time = if id.ends_with("In") {
                view.mark_in.unwrap_or(view.start)
            } else {
                view.mark_out.unwrap_or(Tick(view.end.0.saturating_sub(view.rate.frame_duration().0)).max(view.start))
            };
            s.execute("source.setPlayhead", json!({"time":time.0}))?;
        }
        "markers.clearIn" | "markers.clearOut" | "markers.clearInOut" => {
            let mut marks = json!({"item":item.0});
            if id != "markers.clearOut" {
                marks["in"] = Value::Null;
            }
            if id != "markers.clearIn" {
                marks["out"] = Value::Null;
            }
            s.execute("project.setMarks", marks)?;
        }
        "markers.goNext" | "markers.goPrev" => {
            let time = if id.ends_with("Next") {
                view.markers.iter().map(|m| m.start).filter(|t| *t > cursor).min()
            } else {
                view.markers.iter().map(|m| m.start).filter(|t| *t < cursor).max()
            };
            if let Some(t) = time {
                s.execute("source.setPlayhead", json!({"time":t.0}))?;
            }
        }
        "markers.add" => {
            let time = match p.get("time") {
                None => cursor,
                Some(v) => Tick(v.as_i64().ok_or_else(|| bad(id, "time must be integer ticks"))?),
            };
            if time < view.start || time >= view.end {
                return Err(bad(id, "marker time is outside the Source clip"));
            }
            let duration = duration(p, &view, id)?.min(Tick(view.end.0.saturating_sub(time.0)));
            let name = text(p, "name", id)?.unwrap_or_default().to_string();
            let comment = text(p, "comment", id)?.unwrap_or_default().to_string();
            let color = color(p, id)?.unwrap_or(Label::Green);
            let root = view.media;
            let marker = s.edit("Add Source Marker", |pr, _| {
                if pr.next_id == u64::MAX {
                    return Err(bad(id, "project ids are exhausted"));
                }
                let marker = MarkerId(pr.alloc_id());
                let list = marker_list(pr, root)?;
                list.push(Marker { id: marker, start: time, duration, name, comment, color, kind: MarkerKind::Comment });
                list.sort_by_key(|m| m.start);
                Ok(marker)
            })?;
            return Ok(Some(json!({"marker":marker.0})));
        }
        "markers.clearCurrent" | "markers.clearAll" => {
            s.edit("Clear Source Markers", |pr, _| {
                let list = marker_list(pr, view.media)?;
                if id.ends_with("All") {
                    list.clear();
                } else {
                    list.retain(|m| m.start != cursor);
                }
                Ok(())
            })?;
        }
        "markers.edit" => {
            let marker = MarkerId(p.get("marker").and_then(Value::as_u64).ok_or_else(|| bad(id, "need marker id"))?);
            let name = text(p, "name", id)?.map(str::to_string);
            let comment = text(p, "comment", id)?.map(str::to_string);
            let color = color(p, id)?;
            let duration = if p.get("durationFrames").is_some() { Some(duration(p, &view, id)?) } else { None };
            s.edit("Edit Source Marker", |pr, _| {
                let m = marker_list(pr, view.media)?.iter_mut().find(|m| m.id == marker).ok_or_else(|| bad(id, "no such Source marker"))?;
                if let Some(name) = name {
                    m.name = name;
                }
                if let Some(comment) = comment {
                    m.comment = comment;
                }
                if let Some(color) = color {
                    m.color = color;
                }
                if let Some(d) = duration {
                    m.duration = d.min(Tick(view.end.0.saturating_sub(m.start.0)));
                }
                Ok(())
            })?;
        }
        _ => return Err(bad(id, "unsupported Source command")),
    }
    Ok(Some(Value::Null))
}

fn text<'a>(p: &'a Value, key: &str, id: &str) -> Result<Option<&'a str>> {
    p.get(key).map(|v| v.as_str().ok_or_else(|| bad(id, format!("{key} must be text")))).transpose()
}
fn color(p: &Value, id: &str) -> Result<Option<Label>> {
    text(p, "color", id)?.map(|t| Label::from_name(t).ok_or_else(|| bad(id, "unknown marker color"))).transpose()
}
fn duration(p: &Value, view: &crate::clip_ops::SourceView, id: &str) -> Result<Tick> {
    let Some(v) = p.get("durationFrames") else { return Ok(Tick::ZERO) };
    let f = v.as_i64().ok_or_else(|| bad(id, "durationFrames must be an integer"))?;
    let span = view.end.0.saturating_sub(view.start.0).max(0);
    if f < 0 || f > view.rate.frame_at(Tick(span)) {
        return Err(bad(id, "marker duration is outside the Source clip"));
    }
    Ok(view.rate.tick_of(f))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Session {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.execute("source.open", json!({"item":5})).unwrap();
        s
    }
    #[test]
    fn ranges_snap_and_clamp_without_crossing_or_changing_move_duration() {
        for rate in FrameRate::COMMON {
            let bounds = TimeRange::from_bounds(Tick::ZERO, rate.tick_of(240));
            let range = TimeRange::from_bounds(rate.tick_of(48), rate.tick_of(96));
            for delta in [Tick(i64::MIN), -rate.tick_of(200), -rate.tick_of(8), Tick::ZERO, rate.tick_of(8), rate.tick_of(200), Tick(i64::MAX)] {
                for edge in [RangeEdge::In, RangeEdge::Out, RangeEdge::Move] {
                    let r = adjust_range(bounds, range, rate, edge, delta).unwrap();
                    assert!(r.start >= bounds.start && r.end() <= bounds.end() && r.duration >= rate.frame_duration());
                    if edge == RangeEdge::Move {
                        assert_eq!(r.duration, range.duration);
                    }
                    if edge == RangeEdge::In {
                        assert_eq!(r.end(), range.end());
                    }
                    if edge == RangeEdge::Out {
                        assert_eq!(r.start, range.start);
                    }
                }
            }
        }
    }
    #[test]
    fn hostile_ranges_and_rates_return_errors_without_panicking() {
        let bounds = TimeRange::new(Tick::ZERO, Tick::from_seconds_f64(10.0));
        let range = TimeRange::new(Tick::ZERO, Tick::from_seconds_f64(1.0));
        for rate in [FrameRate { num: 0, den: 0 }, FrameRate { num: -1, den: 1 }, FrameRate { num: 1, den: i64::MAX }, FrameRate { num: i64::MAX, den: 1 }] {
            assert!(adjust_range(bounds, range, rate, RangeEdge::Move, Tick(i64::MAX)).is_err());
        }
        for r in [
            TimeRange::new(Tick(i64::MAX), Tick(2)),
            TimeRange::new(Tick(-1), Tick(1)),
            TimeRange::new(Tick::ZERO, Tick(-1)),
            TimeRange::new(Tick::ZERO, Tick::ZERO),
        ] {
            assert!(adjust_range(bounds, r, FrameRate::FPS_24, RangeEdge::In, Tick(i64::MIN)).is_err());
        }
        let small = TimeRange::new(Tick::ZERO, Tick(1));
        assert_eq!(adjust_range(small, small, FrameRate::FPS_24, RangeEdge::Out, Tick(i64::MAX)).unwrap(), small);
    }
    #[test]
    fn source_markers_are_undoable_and_independent_of_sequence_markers() {
        let mut s = fixture();
        let cursor = source_view(&s, ItemId(5)).unwrap().rate.tick_of(48);
        s.execute("source.setPlayhead", json!({"time":cursor.0})).unwrap();
        let before = s.project.to_json();
        let qbefore = s.active_sequence().unwrap().clone();
        let marker = s.execute("markers.add", json!({"target":"source","name":"Cue"})).unwrap()["marker"].as_u64().unwrap();
        let v = source_view(&s, ItemId(5)).unwrap();
        assert_eq!(v.markers.len(), 1);
        assert_eq!(v.markers[0].start, cursor);
        assert_eq!(s.active_sequence().unwrap(), &qbefore);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.to_json(), before);
        s.execute("edit.redo", json!({})).unwrap();
        s.execute("markers.edit", json!({"target":"source","marker":marker,"name":"Changed"})).unwrap();
        assert_eq!(source_view(&s, ItemId(5)).unwrap().markers[0].name, "Changed");
        s.execute("markers.clearCurrent", json!({"target":"source"})).unwrap();
        assert!(source_view(&s, ItemId(5)).unwrap().markers.is_empty());
    }
    /// #313: the Source monitor remembers the clips opened in it, most recent first; Close shows
    /// the next most recent one and Close All empties the monitor.
    #[test]
    fn source_clips_are_remembered_and_closed_in_turn() {
        let mut s = fixture();
        let a = ItemId(5);
        let mut others: Vec<ItemId> = s.project.items.values().filter(|i| i.as_media().is_some() && i.id != a).map(|i| i.id).collect();
        others.sort();
        let [b, c] = [others[0], others[1]];
        for i in [b, c, a] {
            s.execute("source.open", json!({"item": i.0})).unwrap();
        }
        assert_eq!(s.state.source_history, [a, c, b]);
        assert_eq!(s.execute("source.close", json!({})).unwrap()["item"], c.0);
        assert_eq!((s.state.source_item, s.state.source_history.clone()), (Some(c), vec![c, b]));
        // an item deleted from the project leaves the list
        s.execute("project.delete", json!({"items": [b.0]})).unwrap();
        assert!(s.project.item(b).is_none());
        assert!(!s.state.source_history.contains(&b));
        s.execute("source.closeAll", json!({})).unwrap();
        assert_eq!((s.state.source_item, s.state.source_history.len()), (None, 0));
        assert!(s.execute("source.close", json!({})).is_err(), "nothing left to close");
    }
    #[test]
    fn invalid_marker_parameters_do_not_edit_the_project() {
        let mut s = fixture();
        let before = s.project.to_json();
        for p in [
            json!({"target":true}),
            json!({"target":"unknown"}),
            json!({"target":"source","time":i64::MAX}),
            json!({"target":"source","time":"bad"}),
            json!({"target":"source","durationFrames":i64::MAX}),
            json!({"target":"source","durationFrames":-1}),
            json!({"target":"source","name":23}),
            json!({"target":"source","color":"unknown"}),
        ] {
            assert!(s.execute("markers.add", p).is_err());
            assert_eq!(s.project.to_json(), before);
        }
    }
}
