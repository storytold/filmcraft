use filmcraft_project::{Track, Transition};

use super::layout::Layout;
use super::{ClipId, FilmcraftApp, Grab, Hit, Pos2, SNAP_PX, Sequence, Tick, Value, grab_at, json};

/// Snap `t` to nearby candidates (edits, playhead, markers, in/out). Returns snapped tick.
pub(in super::super) fn snap(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick, exclude: &[ClipId]) -> Tick {
    snap_to(app, seq, layout, t, exclude, true)
}

/// Where a dragged playhead lands: [`snap`], but not onto itself, or it would stick where it is
/// and only move in jumps of `SNAP_PX` (#164).
pub(super) fn snap_playhead(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick) -> Tick {
    snap_to(app, seq, layout, t, &[], false)
}

fn snap_to(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick, exclude: &[ClipId], to_playhead: bool) -> Tick {
    match targets(app, seq, layout, t, exclude, to_playhead).first() {
        Some(&(_, c)) => {
            app.tl.snap_x = Some(layout.x_of(c));
            c
        }
        None => t,
    }
}

/// The candidates [`snap_to`] can snap `t` to, within `SNAP_PX` and nearest first (in candidate
/// order on a tie, the one it takes): none with snapping off.
fn targets(app: &FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick, exclude: &[ClipId], to_playhead: bool) -> Vec<(f32, Tick)> {
    if !app.session.state.snapping {
        return Vec::new();
    }
    let mut cands: Vec<Tick> = Vec::with_capacity(64);
    for tr in seq.all_tracks() {
        for it in &tr.items {
            if exclude.contains(&it.id) {
                continue;
            }
            cands.push(it.start);
            cands.push(it.end());
        }
    }
    if to_playhead {
        cands.push(app.session.playhead());
    }
    cands.extend(seq.markers.iter().map(|m| m.start));
    cands.extend(seq.mark_in);
    cands.extend(seq.mark_out);
    let x = layout.x_of(t);
    let mut near: Vec<(f32, Tick)> = cands.into_iter().map(|c| ((layout.x_of(c) - x).abs(), c)).filter(|(d, _)| *d < SNAP_PX).collect();
    // stable: ties keep candidate order
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    near
}

/// Where transition `tr` on `track` goes (start, duration) when its `edge` (both ends when
/// `None`) is dragged by `delta` (#4328): on the nearest of [`snap`]'s targets for the end that
/// moves (for a move, either end's) that [`filmcraft_edit::transitions::drag_span`] lets it reach;
/// a nearer one it can't reach is passed over, and with none it moves by `delta` as far as it can.
/// Transitions are not snap targets themselves; an end on a clip edge or the playhead holds there
/// as a clip edge does.
pub(super) fn snap_transition(
    app: &mut FilmcraftApp,
    seq: &Sequence,
    layout: &Layout,
    track: &Track,
    tr: &Transition,
    edge: Option<filmcraft_edit::Edge>,
    delta: Tick,
) -> Option<(Tick, Tick)> {
    let span = |d: Tick| filmcraft_edit::transitions::drag_span(track, tr, edge, d, seq.settings.frame_rate.frame_duration());
    // how far the dragged end (the start, for a move) goes
    let moved = |(s, d): (Tick, Tick)| if edge == Some(filmcraft_edit::Edge::Out) { s + d - tr.end() } else { s - tr.start };
    let ends = match edge {
        Some(filmcraft_edit::Edge::In) => vec![tr.start],
        Some(filmcraft_edit::Edge::Out) => vec![tr.end()],
        None => vec![tr.start, tr.end()],
    };
    // (distance, move, target) of each end's targets, nearest first (the start's on a tie)
    let mut snaps: Vec<(f32, Tick, Tick)> =
        ends.into_iter().flat_map(|base| targets(app, seq, layout, base + delta, &[], true).into_iter().map(move |(px, c)| (px, c - base, c))).collect();
    snaps.sort_by(|a, b| a.0.total_cmp(&b.0));
    match snaps.into_iter().find_map(|(_, d, c)| span(d).filter(|s| moved(*s) == d).map(|s| (s, c))) {
        Some((s, c)) => {
            app.tl.snap_x = Some(layout.x_of(c));
            Some(s)
        }
        None => span(delta),
    }
}

/// Where a dragged caption edge lands (#2192): [`snap`], or the nearest edge of a caption not in
/// `exclude` (the captions being dragged) when that is closer.
pub(in super::super) fn snap_caption(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, t: Tick, exclude: &[ClipId]) -> Tick {
    if !app.session.state.snapping {
        return t;
    }
    let x = layout.x_of(t);
    let caption = seq
        .caption_tracks
        .iter()
        .flat_map(|tr| &tr.captions)
        .filter(|c| !exclude.contains(&c.id))
        .flat_map(|c| [c.start, c.end()])
        .map(|c| ((layout.x_of(c) - x).abs(), c))
        .filter(|(d, _)| *d < SNAP_PX)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    // `snap` sets the indicator only when it snaps: that tells a snap onto `t` itself from none
    let prev = app.tl.snap_x.take();
    let clip = snap(app, seq, layout, t, &[]);
    let clip_d = app.tl.snap_x.map(|sx| (sx - x).abs());
    match (caption, clip_d) {
        (Some((d, c)), cd) if cd.is_none_or(|cd| d < cd) => {
            app.tl.snap_x = Some(layout.x_of(c));
            c
        }
        (_, Some(_)) => clip,
        _ => {
            app.tl.snap_x = prev;
            t
        }
    }
}

/// What the Timeline shows at `pos` for the control channel's `ui.timeline.hit`, with `kind`: the
/// trim a press of the current tool with `mods` starts there (null when it would not trim).
pub fn hit_json(app: &FilmcraftApp, pos: Pos2, mods: egui::Modifiers) -> Value {
    let (Some(layout), Some(seq)) = (app.tl.layout.as_ref(), app.session.active_sequence()) else { return json!({"hit": "none"}) };
    let t = layout.tick_at(pos.x);
    let grab = grab_at(seq, layout, pos, app.ui.tool, mods, app.session.prefs.trim.selection_tool_roll_ripple);
    let kind = match grab {
        Grab::Edge { kind, .. } => json!(kind.name()),
        Grab::Volume { .. } => json!("volume"),
        Grab::Other(_) => Value::Null,
    };
    match grab.hit() {
        Hit::Ruler => json!({"hit": "ruler", "time": t.0}),
        Hit::WorkArea { edge } => json!({"hit": "workArea", "edge": edge.map(|e| format!("{e:?}")), "time": t.0}),
        Hit::Clip { track, clip, edge } => {
            json!({"hit": "clip", "track": track.0, "clip": clip.0, "edge": edge.map(|e| format!("{e:?}")), "kind": kind, "time": t.0})
        }
        Hit::Transition { track, id, edge } => json!({"hit": "transition", "track": track.0, "transition": id.0, "edge": edge.map(|e| format!("{e:?}"))}),
        Hit::Empty { track } => json!({"hit": "empty", "track": track.0, "time": t.0}),
        Hit::None => json!({"hit": "none"}),
    }
}

/// Screen point of a clip (centre, or its in/out edge).
pub fn locate(app: &FilmcraftApp, clip: u64, edge: Option<&str>) -> Option<(f32, f32)> {
    let layout = app.tl.layout.as_ref()?;
    let seq = app.session.active_sequence()?;
    let (tid, it) = seq.find_item(ClipId(clip))?;
    let row = layout.rows.iter().find(|r| r.track == tid)?;
    let y = row.rect.center().y;
    let x = match edge {
        Some("in") => layout.x_of(it.start) + 2.0,
        Some("out") => layout.x_of(it.end()) - 2.0,
        _ => (layout.x_of(it.start) + layout.x_of(it.end())) / 2.0,
    };
    Some((x, y))
}
