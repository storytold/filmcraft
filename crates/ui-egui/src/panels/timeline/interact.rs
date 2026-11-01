use super::clips::paint_hatch;
use super::layout::{Layout, max_scroll, min_pps, new_track_at};
use super::menus::{CLIP_MENU, EditPointTarget, edit_point_menu, gap_menu, marker_menu, multicam_menu, ruler_menu};
use super::snap::{snap, snap_playhead};
use super::{
    Align2, ClipId, Color32, CursorIcon, Drag, EdgeKind, FilmcraftApp, Grab, HashMap, Hit, MarkerId, MarqueeAnchor, Pos2, Pressed, Rect, Sense, Sequence,
    Stroke, StrokeKind, Tick, Tokens, Tool, TrackId, TrackItem, TrackKind, Value, edge_geometry, grab_at, hit, json, marker_at, marker_under_pointer, pos2,
    selection_trim_kind, vec2,
};

pub(super) fn preview_drag(
    app: &FilmcraftApp,
    seq: &Sequence,
    out: &mut HashMap<ClipId, (Tick, Tick, Option<TrackId>)>,
    edited: &mut HashMap<ClipId, TrackItem>,
) {
    // the item as the edge move leaves it, and its place in the preview
    let mut moved = |it: &TrackItem, edge: filmcraft_edit::Edge, delta: Tick| {
        let mut e = it.clone();
        filmcraft_edit::move_edge(&mut e, edge, delta, true);
        out.insert(e.id, (e.start, e.duration, None));
        edited.insert(e.id, e);
    };
    let Some(d) = &app.tl.drag else { return };
    match d {
        Drag::Move { new_track: Some(_), .. } => {}
        Drag::Move { clips, offset, track_delta, .. } => {
            for c in clips {
                if let Some((tid, it)) = seq.find_item(*c) {
                    let dest = shift_track(seq, tid, *track_delta);
                    out.insert(*c, ((it.start + *offset).max(Tick::ZERO), it.duration, dest));
                }
            }
        }
        Drag::Remix { clip, delta, .. } => {
            if let Some((_, it)) = seq.find_item(*clip) {
                // #1424: the preview must carry an edited item like Trim and Roll do. Without it
                // the body shrinks while the waveform still maps through the original item, which
                // squeezes the whole waveform into the new width until the remix commits. The
                // commit keeps start and source_in and only changes duration, so clone and match.
                let mut e = it.clone();
                e.duration = (e.duration + *delta).max(Tick(1));
                out.insert(*clip, (e.start, e.duration, None));
                edited.insert(*clip, e);
            }
        }
        Drag::Trim { clip, edge, delta, linked, .. } => {
            let ids = if *linked { filmcraft_engine::commands::with_links(&app.session, &[*clip]) } else { vec![*clip] };
            for c in ids {
                if let Some((_, it)) = seq.find_item(c) {
                    moved(it, *edge, *delta);
                }
            }
        }
        // a rate stretch keeps the same media in a new length: that one does squeeze
        Drag::Stretch { clip, edge, delta, linked, .. } => {
            let ids = if *linked { filmcraft_engine::commands::with_links(&app.session, &[*clip]) } else { vec![*clip] };
            for c in ids {
                if let Some((_, it)) = seq.find_item(c) {
                    let v = match edge {
                        filmcraft_edit::Edge::In => (it.start + *delta, it.duration - *delta, None),
                        filmcraft_edit::Edge::Out => (it.start, it.duration + *delta, None),
                    };
                    out.insert(c, v);
                }
            }
        }
        Drag::Roll { left, right, delta, .. } => {
            let pairs = filmcraft_engine::commands::linked_roll_pairs(seq, *left, *right, app.session.state.linked_selection);
            for (pl, pr) in pairs {
                if let Some((_, l)) = seq.find_item(pl) {
                    moved(l, filmcraft_edit::Edge::Out, *delta);
                }
                if let Some((_, r)) = seq.find_item(pr) {
                    moved(r, filmcraft_edit::Edge::In, *delta);
                }
            }
        }
        Drag::Slide { clip, delta } => {
            if let Some((_, it)) = seq.find_item(*clip) {
                out.insert(*clip, (it.start + *delta, it.duration, None));
            }
        }
        _ => {}
    }
}

/// The transition a clip drag will leave behind, by the same rule `edit::move_items` applies:
/// the transition travels when every clip it sits on moves — to one track, by the same (clamped)
/// offset — and is dropped when only some of its clips move (a partner that stays behind ends the
/// move, as in Premiere), shifted by that offset. The preview draws carried ones where they land,
/// so a dragged clip's transition follows it instead of lagging behind until the drop, and a cut
/// transition does not ride along on a clip whose partner stays. Returns the transition's
/// `(start, end, destination track)` when the drag carries it.
pub(super) fn move_transition_preview(app: &FilmcraftApp, seq: &Sequence, trn: &filmcraft_project::Transition) -> Option<(Tick, Tick, TrackId)> {
    let Some(Drag::Move { clips, offset, track_delta, .. }) = &app.tl.drag else { return None };
    let endpoints: Vec<ClipId> = [trn.from, trn.to].into_iter().flatten().collect();
    // where each clip it sits on that moves lands (the same track shift as the clip preview), and
    // by how much: clamped like the drop clamps each clip's start
    let lands: Vec<(TrackId, Tick)> = endpoints
        .iter()
        .filter_map(|c| {
            if !clips.contains(c) {
                return None;
            }
            let (tid, it) = seq.find_item(*c)?;
            let dest = shift_track(seq, tid, *track_delta)?;
            Some((dest, (it.start + *offset).max(Tick::ZERO) - it.start))
        })
        .collect();
    // every clip it sits on must move to one track by one offset; one staying behind keeps the
    // transition where it is (and the drop does not carry it either)
    if lands.len() != endpoints.len() {
        return None;
    }
    let &(dest, delta) = lands.first()?;
    lands.iter().skip(1).all(|l| *l == (dest, delta)).then(|| (trn.start + delta, trn.end() + delta, dest))
}

impl MarqueeAnchor {
    pub(super) fn new(layout: &Layout, t: Tick, (v_scroll, a_scroll): (f32, f32), p: Pos2) -> Self {
        let video = p.y < layout.split_y;
        MarqueeAnchor { t, y: p.y, scroll: if video { v_scroll } else { a_scroll }, video }
    }

    /// The box from this corner, wherever it is on screen now, to the pointer at `p`. It follows
    /// the rows as `layout` placed them, not the view's scroll a wheel turn has already changed.
    pub(super) fn rect_to(&self, layout: &Layout, p: Pos2) -> Rect {
        // the video tracks move down as `v_scroll` grows, the audio tracks up as `a_scroll` grows
        let mut y = if self.video { self.y + (layout.v_scroll - self.scroll) } else { self.y - (layout.a_scroll - self.scroll) };
        // video scrolling away under a pointer that is still over the video area must not reach into the audio rows
        if self.video && p.y < layout.split_y {
            y = y.min(layout.split_y - 1.0);
        }
        // nor audio scrolling away under a pointer still below the divider into the video rows
        if !self.video && p.y >= layout.split_y {
            y = y.max(layout.split_y + 1.0);
        }
        Rect::from_two_pos(pos2(layout.x_of(self.t), y), p)
    }
}

/// The clips a marquee rectangle touches, on the tracks it crosses.
fn marquee_clips(seq: &Sequence, layout: &Layout, r: Rect) -> Vec<ClipId> {
    let (a, b) = (layout.tick_at(r.min.x), layout.tick_at(r.max.x));
    layout
        .rows
        .iter()
        .filter(|row| row.rect.intersects(r))
        .filter_map(|row| seq.track(row.track))
        .flat_map(|tr| tr.items.iter().filter(|i| i.start < b && i.end() > a).map(|i| i.id))
        .collect()
}

/// A move previews the same track-lock restrictions as its eventual edit.
fn drag_is_locked(session: &filmcraft_engine::Session, seq: &Sequence, drag: &Drag) -> bool {
    let clips = match drag {
        Drag::Move { clips, track_delta, .. } => return move_is_locked(seq, clips, *track_delta),
        Drag::Trim { clip, .. } | Drag::Stretch { clip, .. } | Drag::Remix { clip, .. } | Drag::Slip { clip, .. } | Drag::Slide { clip, .. } => vec![*clip],
        Drag::Roll { left, right, .. } => vec![*left, *right],
        Drag::Transition { id, .. } => return seq.all_tracks().any(|t| t.locked && t.transitions.iter().any(|tr| tr.id == *id)),
        _ => return false,
    };
    move_is_locked(seq, &filmcraft_engine::commands::with_links(session, &clips), 0)
}

fn move_is_locked(seq: &Sequence, clips: &[ClipId], delta: i32) -> bool {
    clips.iter().any(|clip| {
        seq.find_item(*clip).is_some_and(|(track, _)| {
            seq.track(track).is_some_and(|t| t.locked) || shift_track(seq, track, delta).and_then(|id| seq.track(id)).is_some_and(|t| t.locked)
        })
    })
}

fn shift_track(seq: &Sequence, tid: TrackId, delta: i32) -> Option<TrackId> {
    if delta == 0 {
        return Some(tid);
    }
    for tracks in [&seq.video_tracks, &seq.audio_tracks] {
        if let Some(i) = tracks.iter().position(|t| t.id == tid) {
            let ni = (i as i32 + delta).clamp(0, tracks.len() as i32 - 1) as usize;
            return Some(tracks[ni].id);
        }
    }
    Some(tid)
}

/// What the wheel and the trackpad did this frame.
pub(crate) struct WheelInput {
    /// Scroll in points (wheel lines and pages converted), positive = content moves right / down.
    pub(crate) delta: egui::Vec2,
    /// Pinch factor (1 = none).
    pub(crate) pinch: f32,
    command: bool,
    alt: bool,
    shift: bool,
}

/// The wheel events of this frame, read from the events themselves: egui turns Cmd + wheel into a
/// zoom and Shift + wheel into a sideways scroll before anyone asks, and the Timeline has its own
/// meaning for both. The modifiers are those of the events (a script's wheel carries its own).
pub(crate) fn wheel_input(ctx: &egui::Context) -> WheelInput {
    ctx.input(|i| {
        let mut w = WheelInput { delta: egui::Vec2::ZERO, pinch: 1.0, command: false, alt: false, shift: false };
        for e in &i.events {
            match e {
                egui::Event::MouseWheel { unit, delta, modifiers, .. } => {
                    let points = match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => 40.0,
                        egui::MouseWheelUnit::Page => 400.0,
                    };
                    if delta.x.is_finite() && delta.y.is_finite() {
                        w.delta += *delta * points;
                    }
                    w.command |= modifiers.command || modifiers.mac_cmd || modifiers.ctrl;
                    w.alt |= modifiers.alt;
                    w.shift |= modifiers.shift;
                }
                egui::Event::Zoom(z) if z.is_finite() && *z > 0.0 => w.pinch *= *z,
                _ => {}
            }
        }
        w
    })
}

/// Puts the playhead where the pointer is on the ruler: what a press does before any drag (#1090).
fn scrub_playhead(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, p: Pos2, shift: bool) {
    // A press on the ruler stops playback, but it must not close a scrub stream that is already
    // open: `stop()` drops the output stream, and the grain a frame later has to open the device
    // again (slow on WASAPI, #2633). A press while nothing runs has nothing to stop.
    // The Source monitor runs on its own clock and `stop()` leaves it alone, so it stops first;
    app.stop_source();
    app.stop_for_scrub();
    let mut tt = seq.settings.frame_rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO));
    if shift || app.session.prefs.timeline.snap_playhead {
        tt = snap_playhead(app, seq, layout, tt);
    }
    // a press on a marker selects it: the playhead goes exactly onto it (#695)
    if let Some(m) = marker_at(seq, layout, p) {
        tt = m.max(Tick::ZERO);
    }
    app.session.set_playhead(tt);
}

/// Wheel input over the Timeline, as in Premiere Pro on macOS (checked in 26.5.2): the wheel
/// moves the tracks under the pointer, Cmd + wheel the Timeline sideways, Option + wheel
/// zooms about the pointer.
fn wheel_scroll(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, layout: &Layout, seq: &Sequence) {
    let ctx = ui.ctx().clone();
    // ---- wheel, as in Premiere Pro on macOS (checked in 26.5.2):
    //   wheel            the tracks under the pointer, up and down
    //   Cmd + wheel      the Timeline, sideways
    //   Option + wheel   zoom about the pointer
    // Settings ▸ Timeline ▸ Timeline Mouse Scrolling "Horizontal" swaps the first two. A sideways
    // gesture (trackpad swipe, tilt wheel, Shift + wheel) always scrolls sideways; a pinch zooms.
    if ui.rect_contains_pointer(rect)
        && let Some(p) = ctx.pointer_hover_pos()
    {
        let w = wheel_input(&ctx);
        if (w.pinch - 1.0).abs() > 1e-4 || (w.alt && w.delta.y != 0.0) {
            let f = if (w.pinch - 1.0).abs() > 1e-4 { w.pinch as f64 } else { (1.0_f64 + w.delta.y as f64 * 0.01).clamp(0.5, 2.0) };
            let anchor_t = layout.tick_at(p.x).seconds();
            let v = &mut app.ui.timeline;
            v.target_pps = (v.target_pps * f).clamp(min_pps(seq.duration().seconds(), app.last_timeline_width), 24_000.0);
            app.tl.zoom_anchor = Some((anchor_t, p.x));
        } else if w.delta != egui::Vec2::ZERO {
            let wheel_sideways = (app.session.prefs.timeline.mouse_scrolling == "horizontal") != w.command;
            let sideways = if w.delta.x.abs() > w.delta.y.abs() {
                Some(w.delta.x)
            } else if wheel_sideways || w.shift {
                Some(w.delta.y)
            } else {
                None
            };
            match sideways {
                Some(d) => {
                    let v = &mut app.ui.timeline;
                    let limit = max_scroll(seq.duration().seconds(), layout.content.width(), v.pps);
                    v.target_scroll = (v.target_scroll - d as f64 / v.pps).clamp(0.0, limit);
                    v.scroll = v.target_scroll;
                }
                // up and down: the video tracks above the divider, the audio tracks below it (the
                // lowest video track is at the bottom, so wheeling up brings higher ones in)
                None if p.y < layout.split_y => app.ui.timeline.v_scroll += w.delta.y,
                None => app.ui.timeline.a_scroll -= w.delta.y,
            }
        }
    }
}

/// Cursor feedback: what a press here would grab (#259) — the cursor for it, the trim
/// bracket on an edge, and the razor's preview line.
fn hover_cursor(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, pos: Option<Pos2>) {
    let ctx = ui.ctx().clone();
    let area = Rect::from_min_max(pos2(layout.content.min.x, layout.ruler.min.y), layout.content.max);
    let mods = ctx.input(|i| i.modifiers);
    let tool = app.ui.tool;
    let rate = seq.settings.frame_rate;
    let roll_ripple = app.session.prefs.trim.selection_tool_roll_ripple;
    // ---- cursor feedback: what a press here would grab (#259)
    if app.tl.drag.is_none()
        && let Some(p) = pos
        && area.contains(p)
    {
        let g = grab_at(seq, layout, p, tool, mods, roll_ripple);
        let blocked = tool == Tool::Selection
            && match g.hit() {
                Hit::Clip { clip, .. } => {
                    let clips = if app.session.state.selection.contains(&clip) {
                        app.session.state.selection.clone()
                    } else if mods.alt {
                        vec![clip]
                    } else {
                        filmcraft_engine::commands::with_links(&app.session, &[clip])
                    };
                    move_is_locked(seq, &clips, 0)
                }
                _ => false,
            };
        let cur = if blocked {
            CursorIcon::NotAllowed
        } else {
            match (tool, g) {
                (_, Grab::Edge { .. }) => CursorIcon::ResizeColumn,
                (Tool::Selection, Grab::Other(Hit::Transition { edge: Some(_), .. })) => CursorIcon::ResizeColumn,
                (Tool::Selection, Grab::Other(Hit::Transition { edge: None, .. })) => CursorIcon::Grab,
                (_, Grab::Other(Hit::WorkArea { edge: Some(_) })) => CursorIcon::ResizeColumn,
                (_, Grab::Other(Hit::WorkArea { edge: None })) => CursorIcon::Grab,
                (_, Grab::Volume { .. }) if tool == Tool::Pen || mods.command => CursorIcon::Crosshair,
                (_, Grab::Volume { .. }) => CursorIcon::ResizeVertical,
                (Tool::Razor, Grab::Other(Hit::Clip { .. })) => CursorIcon::Crosshair,
                (Tool::Slip | Tool::Slide, Grab::Other(Hit::Clip { .. })) => CursorIcon::ResizeHorizontal,
                (Tool::Hand, _) => CursorIcon::Grab,
                (Tool::Zoom, _) => CursorIcon::ZoomIn,
                _ => CursorIcon::Default,
            }
        };
        ctx.set_cursor_icon(cur);
        // the edge a press here would grab, and the trim it starts (#259)
        if let Grab::Edge { track, clip, edge, kind } = g {
            crate::panels::trim_monitor::paint_hover_bracket(ui.painter(), seq, layout, track, clip, edge, kind);
        }
        // razor preview line
        if tool == Tool::Razor
            && let Grab::Other(Hit::Clip { track, .. }) = g
            && let Some(row) = layout.rows.iter().find(|r| r.track == track)
        {
            let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x)), &[]);
            let x = layout.x_of(t);
            ui.painter().line_segment([pos2(x, row.rect.min.y), pos2(x, row.rect.max.y)], Stroke::new(1.0, Color32::WHITE));
        }
    }
}

/// The press frame: remember what was under the pointer, jump the playhead on a ruler press,
/// select a marker, and start the drag the press implies. Returns whether `interact` should
/// stop (a press with no pointer position: nothing more can be handled this frame).
fn press_action(app: &mut FilmcraftApp, ctx: &egui::Context, resp: &egui::Response, seq: &Sequence, layout: &Layout) -> bool {
    let tool = app.ui.tool;
    let mods = ctx.input(|i| i.modifiers);
    let roll_ripple = app.session.prefs.trim.selection_tool_roll_ripple;
    // ---- press: act on what was under the pointer when the button went down (#259)
    // (the primary button selects, moves, trims and cuts, the middle one pans; a right-button drag
    // starts nothing)
    let press_began = resp.drag_started_by(egui::PointerButton::Primary) || resp.drag_started_by(egui::PointerButton::Middle);
    if !app.tl.cancelled && (press_began || (resp.clicked() && app.tl.drag.is_none())) {
        let pressed = match app.tl.pressed.take() {
            Some(pr) => pr,
            // a press and release in the same frame is never stored: it happened where egui says
            None => {
                let Some(p) = resp.interact_pointer_pos() else { return true };
                Pressed {
                    pos: p,
                    tick: layout.tick_at(p.x),
                    grab: grab_at(seq, layout, p, tool, mods, roll_ripple),
                    v_scroll: layout.v_scroll,
                    a_scroll: layout.a_scroll,
                }
            }
        };
        let (p, t) = (pressed.pos, pressed.tick);
        let adding = tool == Tool::Pen || mods.command;
        // a plain click on a Volume line selects the clip, as anywhere else on it
        let grab = match pressed.grab {
            Grab::Volume { .. } if resp.clicked() && !adding => Grab::Other(pressed.grab.hit()),
            g => g,
        };
        start_drag(app, ctx, resp, seq, layout, grab, p, t, (pressed.v_scroll, pressed.a_scroll));
    }

    false
}

/// What a press does: the drag it starts (or the tool's click action), the playhead jump a
/// ruler press makes, and the selection a tool change makes first.
#[allow(clippy::too_many_arguments)]
fn start_drag(
    app: &mut FilmcraftApp,
    ctx: &egui::Context,
    resp: &egui::Response,
    seq: &Sequence,
    layout: &Layout,
    grab: Grab,
    p: Pos2,
    t: Tick,
    scroll: (f32, f32),
) {
    let tool = app.ui.tool;
    let mods = ctx.input(|i| i.modifiers);
    let rate = seq.settings.frame_rate;
    let roll_ripple = app.session.prefs.trim.selection_tool_roll_ripple;
    let started = match (tool, grab) {
        // a middle-button drag pans whatever the tool and whatever it starts on, as in the
        // monitors (#824) and DaVinci Resolve (#1490); only a primary press is kept in
        // `pressed`, so the pan starts from where the button went down
        _ if resp.drag_started_by(egui::PointerButton::Middle) => {
            let last = ctx.input(|i| i.pointer.press_origin()).unwrap_or(p);
            let tv = &app.ui.timeline;
            Some(Drag::Pan { last, start_scroll: tv.target_scroll, start_v: tv.v_scroll, audio: last.y >= layout.split_y, start_a: tv.a_scroll })
        }
        (_, Grab::Other(Hit::Ruler)) => Some(Drag::Scrub { initial: app.session.playhead() }),
        (_, Grab::Other(Hit::WorkArea { edge })) => {
            seq.work_area.filter(|_| resp.drag_started()).map(|w| Drag::WorkArea { edge, from: t, start: w.start, end: w.end() })
        }
        (Tool::Hand, _) => {
            let tv = &app.ui.timeline;
            Some(Drag::Pan { last: p, start_scroll: tv.target_scroll, start_v: tv.v_scroll, audio: p.y >= layout.split_y, start_a: tv.a_scroll })
        }
        (Tool::Zoom, _) => {
            if resp.clicked() {
                let f = if mods.alt { 1.0 / 2.0 } else { 2.0 };
                app.ui.timeline.target_pps = (app.ui.timeline.target_pps * f).clamp(min_pps(seq.duration().seconds(), app.last_timeline_width), 24_000.0);
                app.tl.zoom_anchor = Some((t.seconds(), p.x));
            }
            None
        }
        (Tool::Razor, Grab::Other(Hit::Clip { clip, .. })) => {
            if resp.clicked() || resp.drag_started() {
                let tt = snap(app, seq, layout, rate.snap_nearest(t), &[]);
                let r = if mods.shift {
                    app.session.execute("timeline.razor", json!({"time": tt.0}))
                } else {
                    app.session.execute("timeline.razor", json!({"time": tt.0, "clip": clip.0}))
                };
                if let Err(e) = r {
                    app.ui.status = e.to_string();
                }
            }
            None
        }
        // a transition over a cut counts as its track, as the clips under it do
        (Tool::TrackSelectForward | Tool::TrackSelectBackward, Grab::Other(Hit::Clip { track, .. } | Hit::Transition { track, .. } | Hit::Empty { track })) => {
            let fwd = tool == Tool::TrackSelectForward;
            let ids: Vec<u64> = seq
                .all_tracks()
                // a locked track is skipped: its clips cannot move, and they would block the drag
                .filter(|tr| !tr.locked && (!mods.shift || tr.id == track))
                .flat_map(|tr| tr.items.iter())
                .filter(|i| if fwd { i.end() > t } else { i.start < t })
                .map(|i| i.id.0)
                .collect();
            let _ = app.session.execute("timeline.select", json!({"clips": ids}));
            select_track_select_captions(app, seq, t, fwd, mods.shift);
            // as in Premiere Pro, a drag with the tool moves what it just selected
            if resp.drag_started() {
                let clips = app.session.state.selection.clone();
                (!clips.is_empty()).then_some(Drag::Move {
                    clips,
                    grab_tick: t,
                    start_track: track,
                    offset: Tick::ZERO,
                    track_delta: 0,
                    new_track: None,
                    copy: mods.alt,
                })
            } else {
                None
            }
        }
        (Tool::Selection | Tool::Ripple | Tool::Rolling, Grab::Edge { clip, edge, track, .. }) if resp.clicked() => {
            // click an edge = select it as an edit point (trim mode); Shift adds
            let kind = match tool {
                Tool::Rolling => "roll",
                Tool::Ripple => "ripple",
                _ => {
                    let (dist, neighbour) = edge_geometry(seq, layout, track, clip, edge, p.x);
                    selection_trim_kind(roll_ripple, mods.command, mods.shift, dist, neighbour)
                }
            };
            let e = if edge == filmcraft_edit::Edge::In { "in" } else { "out" };
            let r = app.session.execute("trim.selectEditPoint", json!({"clip": clip.0, "edge": e, "kind": kind, "add": mods.shift && !mods.command}));
            if let Err(e) = r {
                app.ui.status = e.to_string();
            }
            None
        }
        (_, Grab::Edge { clip, edge, kind, .. }) => Some(match kind {
            EdgeKind::Trim => Drag::Trim { clip, edge, mode: filmcraft_edit::TrimMode::Regular, delta: Tick::ZERO, from: t, linked: !mods.alt },
            EdgeKind::Ripple => Drag::Trim { clip, edge, mode: filmcraft_edit::TrimMode::Ripple, delta: Tick::ZERO, from: t, linked: !mods.alt },
            EdgeKind::Roll { left, right } => Drag::Roll { left, right, delta: Tick::ZERO, from: t },
            EdgeKind::Stretch => Drag::Stretch { clip, edge, delta: Tick::ZERO, from: t, linked: !mods.alt },
            EdgeKind::Remix => Drag::Remix { clip, delta: Tick::ZERO, from: t },
        }),
        (_, Grab::Volume { clip, .. }) if resp.drag_started() => crate::panels::timeline_volume::start(seq, layout, clip, p).map(Drag::Volume),
        (_, Grab::Volume { clip, .. }) => {
            crate::panels::timeline_volume::add_keyframe(app, seq, layout, clip, p.x);
            None
        }
        (Tool::Slip, Grab::Other(Hit::Clip { clip, .. })) => Some(Drag::Slip { clip, delta: Tick::ZERO }),
        (Tool::Slide, Grab::Other(Hit::Clip { clip, .. })) => Some(Drag::Slide { clip, delta: Tick::ZERO }),
        (_, Grab::Other(Hit::Clip { clip, track, .. })) => {
            // select (shift toggles; alt selects one side of a link)
            let sel = &app.session.state.selection;
            if mods.shift {
                let _ = app.session.execute("timeline.select", json!({"clips": [clip.0], "toggle": true, "linked": !mods.alt}));
            } else if !sel.contains(&clip) || (mods.alt && resp.clicked()) {
                // Alt-click isolates one side; Alt-drag keeps the selected group to copy.
                let _ = app.session.execute("timeline.select", json!({"clips": [clip.0], "linked": !mods.alt}));
            }
            if resp.drag_started() {
                let clips = app.session.state.selection.clone();
                Some(Drag::Move { clips, grab_tick: t, start_track: track, offset: Tick::ZERO, track_delta: 0, new_track: None, copy: mods.alt })
            } else {
                None
            }
        }
        // click a transition to select it (Shift adds / removes); drag an end to change its
        // duration or its middle to slide it over the cut (#430, #224)
        (Tool::Selection, Grab::Other(Hit::Transition { id, edge, .. })) => {
            // a drag of a selected transition (Shift too) moves it rather than toggling it out first
            if !(app.session.state.transition_selection.contains(&id) && resp.drag_started()) {
                let r = app.session.execute("timeline.select", json!({"transitions": [id.0], "toggle": mods.shift}));
                if let Err(e) = r {
                    app.ui.status = e.to_string();
                }
            }
            let span = seq.all_tracks().find_map(|tr| tr.transitions.iter().find(|x| x.id == id)).map(|x| (x.start, x.duration));
            match span {
                Some((start, duration)) if resp.drag_started() => Some(Drag::Transition { id, edge, from: t, start, duration }),
                _ => None,
            }
        }
        (_, Grab::Other(Hit::Transition { .. })) => None,
        // empty space: an empty stretch of a track, or no track at all (below the tracks, between
        // the video and audio tracks, #683)
        (_, Grab::Other(hit @ (Hit::Empty { .. } | Hit::None))) if layout.content.contains(p) => {
            if resp.drag_started() {
                Some(Drag::Marquee {
                    start: MarqueeAnchor::new(layout, t, scroll, p),
                    before: app.session.state.selection.clone(),
                    add: mods.shift || mods.command,
                    individual: mods.alt,
                })
            } else {
                app.session.state.selection.clear();
                app.session.state.edit_points.clear();
                app.session.state.transition_selection.clear();
                // empty time between clips: select the gap, for Delete to close it (#648, #668)
                let gap = match hit {
                    Hit::Empty { track } => app.session.execute("timeline.selectGap", json!({"track": track.0, "time": t.0})).is_ok(),
                    _ => false,
                };
                if !gap {
                    app.session.state.gap_selection = None;
                }
                None
            }
        }
        (_, Grab::Other(Hit::Empty { .. } | Hit::None)) => None,
    };
    let started = started.filter(|drag| {
        let blocked = drag_is_locked(&app.session, seq, drag);
        if blocked {
            ctx.set_cursor_icon(CursorIcon::NotAllowed);
        }
        !blocked
    });
    if let Some(d) = started {
        if matches!(d, Drag::Scrub { .. }) {
            scrub_playhead(app, seq, layout, p, mods.shift);
        }
        if resp.drag_started() {
            app.tl.drag = Some(d);
        }
    }
}

/// Track Select Forward (Backward) also selects the captions that end after (start before) `t`, so
/// the subtitles go with the clips and stay in sync. Shift keeps to the clicked track, which has no
/// captions, and a locked caption track keeps its captions.
fn select_track_select_captions(app: &mut FilmcraftApp, seq: &Sequence, t: Tick, fwd: bool, shift: bool) {
    if shift {
        return;
    }
    let picked = |c: &&filmcraft_project::Caption| if fwd { c.end() > t } else { c.start < t };
    let captions: Vec<u64> = seq.caption_tracks.iter().filter(|tr| !tr.locked).flat_map(|tr| tr.captions.iter()).filter(picked).map(|c| c.id.0).collect();
    let _ = app.session.execute("captions.select", json!({"captions": captions, "add": true}));
}

/// The captions a Track Select drag moves with its clips, in the same undo step. None when the drop
/// copies (Option) or inserts (Cmd): an insert already carries the sync-locked caption tracks.
fn track_select_drag_captions(app: &FilmcraftApp, copying: bool, inserting: bool) -> Vec<u64> {
    if copying || inserting || !matches!(app.ui.tool, Tool::TrackSelectForward | Tool::TrackSelectBackward) {
        return Vec::new();
    }
    app.session.state.caption_selection.iter().map(|c| c.0).collect()
}

/// Escape cancels an ongoing drag or press (#580): the release commits nothing.
fn escape_cancel(app: &mut FilmcraftApp, ctx: &egui::Context) {
    // Escape cancels an ongoing drag or press (#580): the release commits nothing
    if (app.tl.drag.is_some() || app.tl.pressed.is_some()) && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        if let Some(d) = app.tl.drag.take() {
            match d {
                Drag::Scrub { initial } => {
                    app.session.set_playhead(initial);
                }
                Drag::Volume(v) if v.changed() => {
                    let _ = app.session.execute("edit.undo", json!({}));
                }
                Drag::Pan { start_scroll, start_v, start_a, .. } => {
                    let v = &mut app.ui.timeline;
                    v.target_scroll = start_scroll;
                    v.scroll = start_scroll;
                    v.v_scroll = start_v;
                    v.a_scroll = start_a;
                }
                // the live box (#1028) already changed the selection: put back the one the drag began with
                Drag::Marquee { before, .. } => {
                    app.session.state.selection = before;
                }
                _ => {}
            }
        }
        app.tl.pressed = None;
        app.tl.cancelled = true;
    }
}

/// An ongoing drag: recomputes its state under the pointer (snapping, the destination track,
/// the marquee box), refuses it when it would land on locked tracks, and auto-scrolls near
/// the edges.
fn drag_update(app: &mut FilmcraftApp, ui: &mut egui::Ui, resp: &egui::Response, seq: &Sequence, layout: &Layout) {
    let ctx = ui.ctx().clone();
    let rate = seq.settings.frame_rate;
    let mods = ctx.input(|i| i.modifiers);
    // ---- drag
    if !app.tl.cancelled
        && resp.dragged()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(d) = app.tl.drag.clone()
    {
        let t_here = layout.tick_at(p.x);
        let new = match d {
            Drag::Scrub { initial } => {
                let mut tt = rate.snap_nearest(t_here.max(Tick::ZERO));
                // Shift snaps; Settings ▸ Timeline ▸ "Snap playhead in Timeline when Snap is enabled"
                if mods.shift || app.session.prefs.timeline.snap_playhead {
                    tt = snap_playhead(app, seq, layout, tt);
                }
                app.session.set_playhead(tt);
                Some(Drag::Scrub { initial })
            }
            Drag::Pan { last, start_scroll, start_v, audio, start_a } => {
                let dx = p.x - last.x;
                let v = &mut app.ui.timeline;
                v.target_scroll = (v.target_scroll - dx as f64 / v.pps).max(0.0);
                v.scroll = v.target_scroll;
                // the tracks follow the pointer: the video tracks hang from the divider (scrolling
                // brings higher ones down), the audio tracks start at it, as with the wheel
                if audio {
                    v.a_scroll -= p.y - last.y;
                } else {
                    v.v_scroll += p.y - last.y;
                }
                Some(Drag::Pan { last: p, start_scroll, start_v, audio, start_a })
            }
            Drag::Move { clips, grab_tick, start_track, .. } => {
                let raw = rate.snap_nearest(t_here - grab_tick);
                // snap the moved block's start or end
                let first = clips.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| i.start)).min().unwrap_or_default();
                let last = clips.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| i.end())).max().unwrap_or_default();
                let s1 = snap(app, seq, layout, first + raw, &clips) - first;
                let offset = if s1 != raw { s1 } else { snap(app, seq, layout, last + raw, &clips) - last };
                let offset = offset.max(-first);
                let cur_row = layout.row_at(p.y);
                let start_row = layout.rows.iter().find(|r| r.track == start_track);
                let track_delta = match (cur_row, start_row) {
                    (Some(c), Some(s)) if c.kind == s.kind => c.index as i32 - s.index as i32,
                    _ => 0,
                };
                if move_is_locked(seq, &clips, track_delta) {
                    ctx.set_cursor_icon(CursorIcon::NotAllowed);
                    Some(Drag::Move { clips, grab_tick, start_track, offset: Tick::ZERO, track_delta: 0, new_track: None, copy: mods.alt })
                } else {
                    // A linked or multitrack selection gets one destination per source track.
                    let new_track = start_row
                        .filter(|_| clips.iter().all(|c| seq.find_item(*c).is_some_and(|(tid, _)| seq.track(tid).is_some_and(|t| !t.locked))))
                        .and_then(|s| {
                            let (vh, ah) = (app.ui.timeline.video_track_h, app.ui.timeline.audio_track_h);
                            new_track_at(seq, layout, p.y, vh, ah).filter(|(kind, _)| *kind == s.kind)
                        });
                    Some(Drag::Move { clips, grab_tick, start_track, offset, track_delta, new_track, copy: mods.alt })
                }
            }
            Drag::Trim { clip, edge, mode, from, linked, .. } => seq.find_item(clip).map(|(_, it)| {
                let base = if edge == filmcraft_edit::Edge::In { it.start } else { it.end() };
                let target = snap(app, seq, layout, rate.snap_nearest(base + (t_here - from)), &[clip]);
                // stop at the end of the media (or the neighbour), as the trim on release will (#653)
                let delta = filmcraft_engine::commands::trim_delta_linked(&app.session, clip, edge, mode, target - base, linked).unwrap_or(target - base);
                Drag::Trim { clip, edge, mode, delta, from, linked }
            }),
            Drag::Remix { clip, from, .. } => seq.find_item(clip).map(|(_, it)| {
                let target = rate.snap_nearest(it.end() + (t_here - from)).max(it.start + rate.frame_duration());
                Drag::Remix { clip, delta: target - it.end(), from }
            }),
            Drag::Stretch { clip, edge, from, linked, .. } => seq.find_item(clip).map(|(_, it)| {
                let base = if edge == filmcraft_edit::Edge::In { it.start } else { it.end() };
                let target = snap(app, seq, layout, rate.snap_nearest(base + (t_here - from)), &[clip]);
                Drag::Stretch { clip, edge, delta: target - base, from, linked }
            }),
            Drag::Roll { left, right, from, .. } => seq.find_item(left).map(|(_, l)| {
                let target = snap(app, seq, layout, rate.snap_nearest(l.end() + (t_here - from)), &[left, right]);
                // stop where the roll on release stops: the media, a frame left in each clip and linked
                // cuts
                let delta = filmcraft_engine::commands::roll_delta(&app.session, left, right, target - l.end()).unwrap_or(target - l.end());
                Drag::Roll { left, right, delta, from }
            }),
            Drag::Slip { clip, .. } => {
                let start = resp.interact_pointer_pos().map(|_| ()).and(ctx.input(|i| i.pointer.press_origin()));
                let origin = start.map(|o| layout.tick_at(o.x)).unwrap_or(t_here);
                Some(Drag::Slip { clip, delta: -(rate.snap_nearest(t_here - origin)) })
            }
            Drag::Slide { clip, .. } => {
                let origin = ctx.input(|i| i.pointer.press_origin()).map(|o| layout.tick_at(o.x)).unwrap_or(t_here);
                Some(Drag::Slide { clip, delta: rate.snap_nearest(t_here - origin) })
            }
            Drag::Transition { id, edge, from, start, duration } => {
                let delta = rate.snap_nearest(t_here - from);
                let span = seq.all_tracks().find_map(|tr| {
                    tr.transitions.iter().find(|x| x.id == id).and_then(|x| filmcraft_edit::transitions::drag_span(tr, x, edge, delta, rate.frame_duration()))
                });
                let (start, duration) = span.unwrap_or((start, duration));
                Some(Drag::Transition { id, edge, from, start, duration })
            }
            Drag::WorkArea { edge, from, start, end } => {
                // the bar as it was at the press: the sequence keeps it until the release
                let (s0, e0) = seq.work_area.map(|w| (w.start, w.end())).unwrap_or((start, end));
                let fd = rate.frame_duration();
                let delta = rate.snap_nearest(t_here - from);
                let (start, end) = match edge {
                    Some(filmcraft_edit::Edge::In) => (snap(app, seq, layout, rate.snap_nearest(s0 + delta), &[]).min(e0 - fd).max(Tick::ZERO), e0),
                    Some(filmcraft_edit::Edge::Out) => (s0, snap(app, seq, layout, rate.snap_nearest(e0 + delta), &[]).max(s0 + fd)),
                    None => {
                        let d = delta.max(Tick::ZERO - s0);
                        (s0 + d, e0 + d)
                    }
                };
                Some(Drag::WorkArea { edge, from, start, end })
            }
            Drag::Volume(v) => Some(Drag::Volume(crate::panels::timeline_volume::drag(app, ui, seq, layout, v, p.y))),
            Drag::Marquee { start, before, add, individual } => {
                let r = start.rect_to(layout, p);
                ui.painter().rect_filled(r, 0.0, Color32::from_white_alpha(18));
                ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_white_alpha(140)), StrokeKind::Inside);
                // the clips in the box are selected as it grows and shrinks; only the release runs
                // the command, so the drag is one selection change
                let mut sel = if add { before.clone() } else { Vec::new() };
                let clips = marquee_clips(seq, layout, r);
                let clips = if individual { clips } else { filmcraft_engine::commands::with_links(&app.session, &clips) };
                for c in clips {
                    if !sel.contains(&c) {
                        sel.push(c);
                    }
                }
                app.session.state.selection = sel;
                Some(Drag::Marquee { start, before, add, individual })
            }
            other => Some(other),
        };
        app.tl.drag = new.filter(|drag| {
            let blocked = drag_is_locked(&app.session, seq, drag);
            if blocked {
                ctx.set_cursor_icon(CursorIcon::NotAllowed);
            }
            !blocked
        });
        // the trim cursor stays for the whole drag of an edge: the hover feedback above stops once
        // a drag is in progress (#1780)
        if matches!(
            app.tl.drag,
            Some(Drag::Trim { .. } | Drag::Roll { .. } | Drag::Stretch { .. } | Drag::Remix { .. } | Drag::Transition { edge: Some(_), .. })
        ) {
            ctx.set_cursor_icon(CursorIcon::ResizeColumn);
        }
        // auto-scroll when dragging near the edges
        if !matches!(app.tl.drag, Some(Drag::Pan { .. }) | Some(Drag::ZoomBar { .. }) | Some(Drag::Volume(_))) {
            let v = &mut app.ui.timeline;
            if p.x > layout.content.max.x - 20.0 {
                v.target_scroll += 6.0 / v.pps;
                v.scroll = v.target_scroll;
            } else if p.x < layout.content.min.x + 10.0 && v.scroll > 0.0 {
                v.target_scroll = (v.target_scroll - 6.0 / v.pps).max(0.0);
                v.scroll = v.target_scroll;
            }
        }
    }
}

/// Release: commit exactly one command for the drag that just ended, or drop it when the
/// drag moved nothing (or Escape cancelled it).
fn commit_drag(app: &mut FilmcraftApp, ctx: &egui::Context, resp: &egui::Response, seq: &Sequence, layout: &Layout) {
    let mods = ctx.input(|i| i.modifiers);
    // ---- release: commit exactly one command
    if resp.drag_stopped() {
        // a drag Escape cancelled (#580) commits nothing
        let cancelled = std::mem::take(&mut app.tl.cancelled);
        if !cancelled && let Some(d) = app.tl.drag.take() {
            let r = match d {
                Drag::Move { clips, offset, track_delta, new_track, copy, .. } if offset != Tick::ZERO || track_delta != 0 || new_track.is_some() => {
                    // #1104: dropped past the last track, the clips go onto a new one
                    let to = |tid| new_track.map_or_else(|| json!(shift_track(seq, tid, track_delta).unwrap_or(tid).0), |_| json!("new"));
                    let mut seen = std::collections::HashSet::new();
                    let moves: Vec<Value> = clips
                        .iter()
                        .filter(|c| seen.insert(**c))
                        .filter_map(|c| seq.find_item(*c).map(|(tid, it)| json!({"clip": c.0, "track": to(tid), "time": (it.start + offset).max(Tick::ZERO).0})))
                        .collect();
                    // Option held while dropping copies (Premiere: Option-drag duplicates)
                    let copying = copy || mods.alt;
                    let captions = track_select_drag_captions(app, copying, mods.command);
                    Some(app.session.execute("timeline.move", json!({"moves": moves, "insert": mods.command, "linked": false, "copy": copying, "captions": captions})))
                }
                Drag::Trim { clip, edge, mode, delta, linked, .. } if delta != Tick::ZERO => Some(app.session.execute(
                    "timeline.trim",
                    json!({"clip": clip.0, "edge": if edge == filmcraft_edit::Edge::In {"in"} else {"out"}, "mode": if mode == filmcraft_edit::TrimMode::Ripple {"ripple"} else {"regular"}, "delta": delta.0, "linked": linked}),
                )),
                Drag::Stretch { clip, edge, delta, linked, .. } if delta != Tick::ZERO => Some(app.session.execute("timeline.rateStretch", json!({"clip": clip.0, "edge": if edge == filmcraft_edit::Edge::In {"in"} else {"out"}, "delta": delta.0, "linked": linked}))),
                Drag::Remix { clip, delta, .. } if delta != Tick::ZERO => {
                    let d = seq.find_item(clip).map(|(_, it)| it.duration + delta).unwrap_or(Tick::ZERO);
                    Some(app.session.execute("clip.remix", json!({"clip": clip.0, "duration": d.0})))
                }
                Drag::Roll { left, right, delta, .. } if delta != Tick::ZERO => Some(app.session.execute("timeline.roll", json!({"left": left.0, "right": right.0, "delta": delta.0}))),
                Drag::Slip { clip, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.slip", json!({"clip": clip.0, "delta": delta.0}))),
                Drag::Slide { clip, delta } if delta != Tick::ZERO => Some(app.session.execute("timeline.slide", json!({"clip": clip.0, "delta": delta.0}))),
                Drag::Transition { id, start, duration, .. } => {
                    let moved = seq.all_tracks().find_map(|tr| tr.transitions.iter().find(|x| x.id == id)).is_some_and(|x| (x.start, x.duration) != (start, duration));
                    moved.then(|| app.session.execute("sequence.setTransition", json!({"transition": id.0, "start": start.0, "duration": duration.0})))
                }
                Drag::WorkArea { start, end, .. } => {
                    let moved = seq.work_area.is_some_and(|w| (w.start, w.end()) != (start, end));
                    moved.then(|| app.session.execute("sequence.setWorkArea", json!({"start": start.0, "end": end.0})))
                }
                Drag::Marquee { start, before, add, individual } => {
                    app.session.state.selection = before;
                    // egui ends a drag on Escape: that cancels the box, as in the Project panel
                    match resp.interact_pointer_pos() {
                        Some(end) if !ctx.input(|i| i.key_pressed(egui::Key::Escape)) => {
                            let ids: Vec<u64> = marquee_clips(seq, layout, start.rect_to(layout, end)).iter().map(|c| c.0).collect();
                            Some(app.session.execute("timeline.select", json!({"clips": ids, "add": add, "linked": !individual})))
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(Err(e)) = r {
                app.ui.status = e.to_string();
            }
        } else {
            app.tl.drag = None;
        }
    }
}

/// #1887: the rows a Project drop fills, with their automation ids: the picture from the video
/// destination up, the sound from the audio destination down (one track per audio clip the item
/// places), as `timeline.place` lays them out. `row` is the track under the pointer, `new_track`
/// the track the drop adds in the empty space above or below them (drawn for its first lane only).
/// `(vlanes, alanes)` is how many video and audio tracks the drop fills ([`filmcraft_engine::Session::placement_lanes`]).
fn drop_preview_bands(
    seq: &Sequence,
    layout: &Layout,
    (vlanes, alanes): (usize, usize),
    row: Option<&super::layout::Row>,
    new_track: Option<(TrackKind, Rect)>,
) -> Vec<(Rect, String)> {
    // first track index of each kind, `None` = a new track the drop adds
    let dests = match (row, new_track) {
        (Some(row), _) => match row.kind {
            TrackKind::Video => (Some(row.index), Some(if row.index < seq.audio_tracks.len() { row.index } else { 0 })),
            TrackKind::Audio => (Some(if row.index < seq.video_tracks.len() { row.index } else { 0 }), Some(row.index)),
        },
        (None, Some((TrackKind::Video, _))) => (None, (seq.video_tracks.len() < seq.audio_tracks.len()).then_some(seq.video_tracks.len())),
        (None, Some((TrackKind::Audio, _))) => ((seq.audio_tracks.len() < seq.video_tracks.len()).then_some(seq.audio_tracks.len()), None),
        (None, None) => return Vec::new(),
    };
    let mut bands = Vec::new();
    for (kind, first, lanes, letter) in [(TrackKind::Video, dests.0, vlanes, 'V'), (TrackKind::Audio, dests.1, alanes, 'A')] {
        for lane in 0..lanes {
            let band = match first {
                Some(i) => i.checked_add(lane).and_then(|i| layout.rows.iter().find(|r| r.kind == kind && r.index == i)).map(|r| r.rect),
                None => new_track.filter(|(k, _)| *k == kind && lane == 0).map(|(_, band)| band),
            };
            if let Some(band) = band {
                let n = first.map_or(seq.tracks(kind).len(), |i| i.saturating_add(lane)).saturating_add(1);
                bands.push((band, format!("timeline.dropPreview.{letter}{n}")));
            }
        }
    }
    bands
}

/// Project items, graphics templates, effects and OS files dragged over the Timeline: the
/// preview of where they would land while hovering, and the command that places them on
/// release.
fn drop_targets(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout) {
    let ctx = ui.ctx().clone();
    let rate = seq.settings.frame_rate;
    let mods = ctx.input(|i| i.modifiers);
    // ---- drops: project items and effects
    if let Some(item) = crate::panels::dragged_project_item(ui)
        && let Some(p) = ctx.pointer_hover_pos()
        && layout.content.contains(p)
        && app.timeline_drop_target(ui.ctx(), p)
    {
        let source = crate::panels::dragged_source(ui);
        let row = layout.row_at(p.y).cloned().filter(|r| {
            source.is_none_or(|s| match r.kind {
                TrackKind::Video => s.video,
                TrackKind::Audio => s.audio,
            })
        });
        let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
        let still = app.session.prefs.timeline.still_duration(rate);
        let is_still = app.session.project.item(item).and_then(|i| i.as_media()).is_some_and(|m| m.info.kind == filmcraft_media::MediaKind::Still);
        let dur = source
            .map(|s| s.range.duration)
            .unwrap_or_else(|| app.session.project.item(item).map(|i| i.duration()).filter(|d| d.0 > 0 && !is_still).unwrap_or(still));
        // #483: in the empty space above the top video track or below the last audio track, a new track
        let new_track = if row.is_some() { None } else { new_track_at(seq, layout, p.y, app.ui.timeline.video_track_h, app.ui.timeline.audio_track_h) }.filter(
            |(kind, _)| {
                let carries = app.session.project.item(item).is_some_and(|i| if *kind == TrackKind::Video { i.has_video() } else { i.has_audio() });
                carries && source.is_none_or(|s| if *kind == TrackKind::Video { s.video } else { s.audio })
            },
        );
        // #1887: a preview on every track the drop fills, not only the one under the pointer
        let range = source.map_or(filmcraft_time::TimeRange::new(Tick::ZERO, dur), |s| s.range);
        let lanes = app.session.placement_lanes(item, range, source.is_none_or(|s| s.video), source.is_none_or(|s| s.audio));
        for (k, (band, id)) in drop_preview_bands(seq, layout, lanes, row.as_ref(), new_track).iter().enumerate() {
            let r = Rect::from_min_max(pos2(layout.x_of(t), band.min.y + 1.0), pos2(layout.x_of(t + dur), band.max.y - 1.0));
            ui.painter().rect_filled(r, 3.0, Color32::from_white_alpha(40));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
            app.auto.add(id, r, "tracks");
            if mods.command && k == 0 {
                ui.painter().text(r.left_top() + vec2(4.0, -2.0), Align2::LEFT_BOTTOM, tl!("Insert"), Tokens::ui(10.0), Color32::WHITE);
            }
        }
        let target = match (&row, new_track) {
            (Some(row), _) => Some(match row.kind {
                TrackKind::Video => (json!(row.track.0), json!(seq.audio_tracks.get(row.index).or(seq.audio_tracks.first()).map(|t| t.id.0))),
                TrackKind::Audio => (json!(seq.video_tracks.get(row.index).or(seq.video_tracks.first()).map(|t| t.id.0)), json!(row.track.0)),
            }),
            // the other stream goes on the track of the same number, or on a new one as well
            (None, Some((TrackKind::Video, _))) => Some((json!("new"), seq.audio_tracks.get(seq.video_tracks.len()).map_or(json!("new"), |t| json!(t.id.0)))),
            (None, Some((TrackKind::Audio, _))) => Some((seq.video_tracks.get(seq.audio_tracks.len()).map_or(json!("new"), |t| json!(t.id.0)), json!("new"))),
            (None, None) => None,
        };
        if ctx.input(|i| i.pointer.any_released())
            && let Some((vt, at)) = target
        {
            // dragging one of several selected project items places them all, back to back, as one edit
            let items = match source {
                None => crate::panels::dragged_selection(app, item),
                Some(_) => vec![item],
            };
            let mut params = json!({"track": vt, "audioTrack": at, "time": t.0, "insert": mods.command});
            match items.as_slice() {
                [one] => params["item"] = json!(one.0),
                many => params["items"] = json!(many.iter().map(|i| i.0).collect::<Vec<_>>()),
            }
            if let Some(source) = source {
                params["sourceIn"] = json!(source.range.start.0);
                params["duration"] = json!(source.range.duration.0);
                params["video"] = json!(source.video);
                params["audio"] = json!(source.audio);
            }
            if let Err(e) = crate::panels::clip_mismatch::execute(app, "timeline.place", params) {
                app.ui.status = e;
            }
            crate::panels::clear_drag(ui);
        }
    }
    // graphics templates from Essential Graphics ▸ Browse: placed on the video track under the pointer
    if let Some(template) = crate::panels::dragged_template(ui)
        && let Some(p) = ctx.pointer_hover_pos()
        && layout.content.contains(p)
        && app.timeline_drop_target(ui.ctx(), p)
    {
        let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
        let row = layout.row_at(p.y).cloned().filter(|r| r.kind == TrackKind::Video);
        if let Some(row) = &row {
            let r = Rect::from_min_max(pos2(layout.x_of(t), row.rect.min.y + 1.0), pos2(layout.x_of(t) + 60.0, row.rect.max.y - 1.0));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
        }
        if ctx.input(|i| i.pointer.any_released()) {
            let mut q = json!({"template": template, "time": t.0});
            if let Some(row) = row {
                q["track"] = json!(row.index);
            }
            if let Err(e) = app.session.execute("graphics.template.apply", q) {
                app.ui.status = e.to_string();
            }
            crate::panels::clear_drag(ui);
        }
    }
    effect_drop_preview(app, ui, seq, layout);
    // ---- OS file drops imported with "drop imports to the timeline": placed at the drop point, one after another
    if let Some((items, p, viewport)) = app.pending_timeline_drop.clone()
        && viewport == ui.ctx().viewport_id()
        && layout.content.contains(p)
        && app.timeline_drop_target(ui.ctx(), p)
    {
        app.pending_timeline_drop = None;
        let row = layout.row_at(p.y).cloned().or_else(|| layout.rows.iter().find(|r| r.kind == TrackKind::Video).cloned());
        if let Some(row) = row {
            let (vt, at) = match row.kind {
                TrackKind::Video => (Some(row.track.0), seq.audio_tracks.get(row.index).or(seq.audio_tracks.first()).map(|t| t.id.0)),
                TrackKind::Audio => (seq.video_tracks.get(row.index).or(seq.video_tracks.first()).map(|t| t.id.0), Some(row.track.0)),
            };
            let start = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
            // one command, one undo step: the files go end to end from the drop point
            let ids: Vec<u64> = items.iter().map(|i| i.0).collect();
            let params = json!({"items": ids, "track": vt, "audioTrack": at, "time": start.0, "insert": false});
            if let Err(e) = crate::panels::clip_mismatch::execute(app, "timeline.place", params) {
                app.ui.status = e;
            }
        }
    }
    // preview of OS files being dragged over the Timeline: a placeholder where the drop would start
    if let Some((count, p, viewport)) = app.file_drag_hover
        && viewport == ui.ctx().viewport_id()
        && layout.content.contains(p)
        && app.timeline_drop_target(ui.ctx(), p)
    {
        let row = layout.row_at(p.y).cloned().or_else(|| layout.rows.iter().find(|r| r.kind == TrackKind::Video).cloned());
        if let Some(row) = row {
            let t = snap(app, seq, layout, rate.snap_nearest(layout.tick_at(p.x).max(Tick::ZERO)), &[]);
            let x = layout.x_of(t);
            let r = Rect::from_min_max(pos2(x, row.rect.min.y + 1.0), pos2((x + 120.0).min(layout.content.max.x), row.rect.max.y - 1.0));
            ui.painter().rect_filled(r, 3.0, Color32::from_white_alpha(40));
            ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
            app.auto.add("timeline.fileDropPreview", r, "tracks");
            let label = if count == 1 { tl!("1 file").to_string() } else { tlf!("{n} files", n = count) };
            ui.painter().text(r.left_top() + vec2(4.0, 2.0), Align2::LEFT_TOP, label, Tokens::ui(10.0), Color32::WHITE);
        }
    }
}

/// An effect or transition dragged over a clip: previews where it would land (the transition
/// spanning the nearest edge, the effect covering the clip) and applies it on release.
fn effect_drop_preview(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout) {
    let ctx = ui.ctx().clone();
    if let Some(effect) = crate::panels::dragged_effect(ui)
        && let Some(p) = ctx.pointer_hover_pos()
        && layout.content.contains(p)
        && app.timeline_drop_target(ui.ctx(), p)
        && let Hit::Clip { clip, .. } = hit(seq, layout, p)
        && let Some((tid, it)) = seq.find_item(clip)
        && let Some(row) = layout.rows.iter().find(|r| r.track == tid)
    {
        {
            let transition_kind = filmcraft_project::find_effect(&effect).and_then(|d| match d.kind {
                filmcraft_project::EffectKind::VideoTransition => Some(TrackKind::Video),
                filmcraft_project::EffectKind::AudioTransition => Some(TrackKind::Audio),
                _ => None,
            });
            let edge = if p.x - layout.x_of(it.start) < layout.x_of(it.end()) - p.x { "in" } else { "out" };
            let mut params = json!({"effect": effect, "clip": clip.0, "edge": edge});
            if let Some(kind) = transition_kind {
                if let Ok((_, tr)) = filmcraft_engine::commands::preview_transition(&app.session, &params, kind) {
                    // Snap the drop to the cut: Start at Cut, Center at Cut or End at Cut, by
                    // where the pointer is relative to it (beyond half the transition's span).
                    let cut = if edge == "in" { it.start } else { it.end() };
                    let half = tr.duration.mul_ratio(1, 2);
                    let align = if layout.tick_at(p.x) < cut - half {
                        filmcraft_edit::transitions::Alignment::End
                    } else if layout.tick_at(p.x) > cut + half {
                        filmcraft_edit::transitions::Alignment::Start
                    } else {
                        filmcraft_edit::transitions::Alignment::Center
                    };
                    params["align"] = json!(align.id());
                    if let Ok((_, tr)) = filmcraft_engine::commands::preview_transition(&app.session, &params, kind) {
                        let r = Rect::from_min_max(pos2(layout.x_of(tr.start), row.rect.min.y + 1.0), pos2(layout.x_of(tr.end()), row.rect.max.y - 1.0));
                        let painter = ui.painter().with_clip_rect(layout.content);
                        painter.rect_filled(r, 2.0, app.tokens.accent.gamma_multiply(0.35));
                        paint_hatch(&painter, r.intersect(layout.content));
                        painter.rect_stroke(r, 2.0, Stroke::new(2.0, app.tokens.accent), StrokeKind::Inside);
                        let x = layout.x_of(cut);
                        painter.line_segment([pos2(x, r.top()), pos2(x, r.bottom())], Stroke::new(3.0, app.tokens.accent));
                        // Keep the alignment label readable even when the transition is only a few pixels wide.
                        let label = filmcraft_edit::transitions::alignment(&tr, cut, seq.settings.frame_rate.frame_duration()).label();
                        let shown = crate::i18n::t(label);
                        let label_width = painter.layout_no_wrap(shown.to_string(), Tokens::ui(11.0), app.tokens.accent).size().x + 8.0;
                        let label_rect = Rect::from_center_size(pos2(p.x, row.rect.top() + 10.0), vec2(label_width, 15.0));
                        painter.rect_filled(label_rect, 2.0, app.tokens.panel_bg);
                        painter.text(label_rect.center(), Align2::CENTER_CENTER, shown, Tokens::ui(11.0), app.tokens.accent);
                        // Automation stays in English so tests and the control channel match a fixed label.
                        app.auto.add("timeline.transitionDropPreview", r.intersect(layout.content), label);
                    }
                }
            } else {
                let r = Rect::from_min_max(pos2(layout.x_of(it.start), row.rect.min.y), pos2(layout.x_of(it.end()), row.rect.max.y));
                ui.painter().rect_stroke(r, 3.0, Stroke::new(2.0, app.tokens.accent), StrokeKind::Inside);
            }
            if ctx.input(|i| i.pointer.any_released()) {
                let r = if let Some(name) = effect.strip_prefix("preset:") {
                    app.session.execute("presets.apply", json!({"preset": name, "clips": [clip.0]}))
                } else if let Some(name) = effect.strip_prefix(crate::panels::LUMETRI_PRESET_PREFIX) {
                    app.session.execute("lumetri.applyPreset", json!({"name": name, "clips": [clip.0]}))
                } else if transition_kind.is_some() {
                    app.session.execute("effects.apply", params)
                } else {
                    app.session.execute("effects.apply", json!({"effect": effect, "clips": [clip.0]}))
                };
                if let Err(e) = r {
                    app.ui.status = e.to_string();
                } else {
                    let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
                }
                crate::panels::clear_drag(ui);
            }
        }
    }
}

/// Handle Timeline pointer input: remember the grab from the press frame, jump the playhead on a ruler press, select a marker on its press, and reset the
/// cancel flag once the button is released.
pub(super) fn interact(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, rect: Rect) {
    let ctx = ui.ctx().clone();
    let area = Rect::from_min_max(pos2(layout.content.min.x, layout.ruler.min.y), layout.content.max);
    let resp = ui.interact(area, egui::Id::unique("timeline-area"), Sense::click_and_drag());
    let pos = resp.hover_pos().or(resp.interact_pointer_pos());
    let mods = ctx.input(|i| i.modifiers);
    let tool = app.ui.tool;
    let rate = seq.settings.frame_rate;
    let roll_ripple = app.session.prefs.trim.selection_tool_roll_ripple;

    // ---- what the button went down on (#259): kept from the press frame until it comes up
    if ctx.input(|i| !i.pointer.any_down() && !i.pointer.any_released()) {
        app.tl.pressed = None;
        app.tl.cancelled = false;
    }
    if resp.is_pointer_button_down_on()
        && ctx.input(|i| i.pointer.primary_pressed())
        && let Some(p) = ctx.input(|i| i.pointer.press_origin())
    {
        app.tl.cancelled = false;
        let grab = grab_at(seq, layout, p, tool, mods, roll_ripple);
        // the playhead jumps to a press on the ruler at once, not only once the pointer has moved (#1090)
        if matches!(grab, Grab::Other(Hit::Ruler)) {
            scrub_playhead(app, seq, layout, p, mods.shift);
        }
        app.tl.pressed = Some(Pressed { pos: p, tick: layout.tick_at(p.x), grab, v_scroll: layout.v_scroll, a_scroll: layout.a_scroll });
    }

    wheel_scroll(app, ui, rect, layout, seq);

    hover_cursor(app, ui, seq, layout, pos);

    if press_action(app, &ctx, &resp, seq, layout) {
        return;
    }

    escape_cancel(app, &ctx);

    drag_update(app, ui, &resp, seq, layout);

    commit_drag(app, &ctx, &resp, seq, layout);

    // ---- double-click the work area bar: it spans the part of the sequence the ruler shows
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Hit::WorkArea { .. } = hit(seq, layout, p)
    {
        let start = rate.snap_nearest(layout.tick_at(layout.content.min.x).max(Tick::ZERO));
        let end = rate.snap_nearest(layout.tick_at(layout.content.max.x)).min(seq.duration());
        // a ruler scrolled past the end of the sequence leaves the bar as it is
        if end > start
            && let Err(e) = app.session.execute("sequence.setWorkArea", json!({"start": start.0, "end": end.0}))
        {
            app.ui.status = e.to_string();
        }
    }

    // ---- double-click a marker on the ruler: the Edit Marker dialog (#3023)
    if resp.double_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Some(marker) = marker_under_pointer(seq, layout, p)
    {
        super::marker_dialog::open(app, &ctx, marker.id);
    }

    // ---- double-click a transition: Set Transition Duration (Premiere)
    if resp.double_clicked()
        && tool == Tool::Selection
        && let Some(p) = resp.interact_pointer_pos()
        && let Hit::Transition { id, .. } = hit(seq, layout, p)
    {
        crate::panels::transition_controls::open_duration(app, id);
    }

    // ---- double-click a nested sequence clip: open it in its own Timeline tab
    if resp.double_clicked()
        && tool == Tool::Selection
        && let Some(p) = resp.interact_pointer_pos()
        && let Hit::Clip { clip, .. } = hit(seq, layout, p)
        && seq.find_item(clip).is_some_and(|(_, it)| app.session.project.sequence(it.item).is_some())
    {
        let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
        if let Err(e) = app.session.execute("sequence.revealNested", json!({})) {
            app.ui.status = e.to_string();
        }
    }

    // ---- double-click any other clip (not on its edge): open its media in the Source monitor at
    // the matching frame (Premiere); graphic clips have no source to show
    if resp.double_clicked()
        && tool == Tool::Selection
        && let Some(p) = resp.interact_pointer_pos()
        && let Hit::Clip { clip, edge: None, .. } = hit(seq, layout, p)
        && seq
            .find_item(clip)
            .and_then(|(_, it)| app.session.project.item(it.item))
            .is_some_and(|i| !matches!(i.kind, filmcraft_project::ItemKind::Sequence(_) | filmcraft_project::ItemKind::Graphic { .. }))
        && let Err(e) = app.session.execute("source.open", json!({"clip": clip.0}))
    {
        app.ui.status = e.to_string();
    }

    // ---- context menu on clips (right-clicking an unselected clip selects it first); on a clip's
    // edge, the edit point menu instead (right-clicking an unselected edit point selects it first)
    let edit_point_menu_id = egui::Id::unique("timeline.editPointMenu.target");
    // the clip right-clicked, remembered while its menu is open: Unlink leaves only it selected
    let menu_clip_id = egui::Id::unique("timeline.clipMenu.clip");
    // right-clicking empty time between clips selects the gap and opens its menu (#648, #668)
    let gap_menu_id = egui::Id::unique("timeline.gapMenu.open");
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let on_gap = match hit(seq, layout, p) {
            Hit::Empty { track } => app.session.execute("timeline.selectGap", json!({"track": track.0, "time": layout.tick_at(p.x).0})).is_ok(),
            _ => false,
        };
        ctx.data_mut(|d| d.insert_temp(gap_menu_id, on_gap));
        if on_gap {
            // not the edit point menu of a cut right-clicked before
            ctx.data_mut(|d| d.insert_temp::<Option<EditPointTarget>>(edit_point_menu_id, None));
        }
    }
    // on a marker of the ruler, the marker menu instead (right-clicking a marker selects it first)
    let marker_menu_id = egui::Id::unique("timeline.markerMenu.target");
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let marker = marker_under_pointer(seq, layout, p);
        if let Some(m) = marker {
            app.session.set_playhead(m.start.max(Tick::ZERO));
        }
        ctx.data_mut(|d| d.insert_temp(marker_menu_id, marker.map(|m| m.id)));
    }
    // on the ruler away from a marker, the ruler's own menu, not the clip menu of an earlier press
    let ruler_menu_id = egui::Id::unique("timeline.rulerMenu.open");
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
    {
        let on_ruler = layout.ruler.contains(p) && marker_under_pointer(seq, layout, p).is_none();
        ctx.data_mut(|d| d.insert_temp(ruler_menu_id, on_ruler));
        if on_ruler {
            ctx.data_mut(|d| d.insert_temp::<Option<EditPointTarget>>(edit_point_menu_id, None));
            ctx.data_mut(|d| d.insert_temp(gap_menu_id, false));
        }
    }
    if resp.secondary_clicked()
        && let Some(p) = resp.interact_pointer_pos()
        && let Hit::Clip { clip, edge, track } = hit(seq, layout, p)
    {
        ctx.data_mut(|d| d.insert_temp(menu_clip_id, clip));
        let target = edge.and_then(|e| EditPointTarget::at(seq, track, clip, e).map(|t| (t, e)));
        ctx.data_mut(|d| d.insert_temp(edit_point_menu_id, target.map(|(t, _)| t)));
        if let Some((t, e)) = target {
            let on_it = app.session.state.edit_points.iter().any(|ep| (ep.out && Some(ep.clip) == t.left) || (!ep.out && Some(ep.clip) == t.right));
            if !on_it {
                let kind = match tool {
                    Tool::Rolling => "roll",
                    Tool::Ripple => "ripple",
                    _ => {
                        let (dist, neighbour) = edge_geometry(seq, layout, track, clip, e, p.x);
                        selection_trim_kind(app.session.prefs.trim.selection_tool_roll_ripple, false, false, dist, neighbour)
                    }
                };
                let edge = if e == filmcraft_edit::Edge::In { "in" } else { "out" };
                let _ = app.session.execute("trim.selectEditPoint", json!({"clip": clip.0, "edge": edge, "kind": kind}));
            }
        } else if !app.session.state.selection.contains(&clip) {
            let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
        }
    }
    resp.context_menu(|ui| {
        // a long menu scrolls instead of running off the bottom of the screen (#2852)
        let max_height = (ui.ctx().content_rect().height() - 16.0).max(0.0);
        egui::ScrollArea::vertical().id_salt("clip-menu-scroll").max_height(max_height).min_scrolled_height(max_height).show(ui, |ui| {
            ui.set_min_width(220.0);
            if let Some(marker) = ctx.data(|d| d.get_temp::<Option<MarkerId>>(marker_menu_id)).flatten() {
                marker_menu(app, &ctx, ui, marker);
                return;
            }
            if ctx.data(|d| d.get_temp::<bool>(ruler_menu_id)).unwrap_or(false) {
                ruler_menu(app, &ctx, ui);
                return;
            }
            if let Some(target) = ctx.data(|d| d.get_temp::<Option<EditPointTarget>>(edit_point_menu_id)).flatten() {
                edit_point_menu(app, &ctx, ui, seq, target);
                return;
            }
            if ctx.data(|d| d.get_temp::<bool>(gap_menu_id)).unwrap_or(false) {
                gap_menu(app, &ctx, ui);
                return;
            }
            let sel = app.session.state.selection.clone();
            let picked: Vec<&filmcraft_project::TrackItem> = sel.iter().filter_map(|c| seq.find_item(*c).map(|(_, it)| it)).collect();
            let all_enabled = !picked.is_empty() && picked.iter().all(|it| it.enabled);
            let linked = picked.iter().any(|it| it.link.is_some());
            let mut first = true;
            for group in CLIP_MENU {
                if !std::mem::take(&mut first) {
                    ui.separator();
                }
                for &(label, cmd) in *group {
                    if cmd == "edit.label" {
                        ui.add_enabled_ui(!picked.is_empty(), |ui| {
                            let menu = ui.menu_button(crate::i18n::t(label), |ui| {
                                // the user's label names and colours, as in Edit ▸ Label (#786)
                                let labels = app.session.prefs.labels.clone();
                                for l in filmcraft_project::Label::ALL {
                                    if crate::menus::label_choice(ui, &mut app.auto, &labels, l, "timeline.clipMenu.label").clicked() {
                                        if let Err(error) = app.session.execute("edit.label", json!({"label":l.name()})) {
                                            app.ui.status = error.to_string();
                                        }
                                        ui.close();
                                    }
                                }
                            });
                            app.auto.add_response("timeline.clipMenu.edit.label", &menu.response, crate::i18n::t(label));
                        });
                        continue;
                    }
                    if cmd == "clip.multicam" {
                        multicam_menu(app, ui, &picked);
                        continue;
                    }
                    let label = match cmd {
                        "clip.enable" if all_enabled => tl!("✓ Enable"),
                        "clip.link" if linked => tl!("Unlink"),
                        _ => crate::i18n::t(label),
                    };
                    let r = ui.add_enabled(!sel.is_empty() && app.session.is_enabled(cmd), egui::Button::new(label));
                    app.auto.add(&format!("timeline.clipMenu.{cmd}"), r.rect, label);
                    if r.clicked() {
                        match crate::menus::invoke(app, &ctx, cmd, json!({})) {
                            // Unlink (#220): the clips were selected together because they were linked;
                            // keep only the one right-clicked, so it can be dragged away from the others
                            Ok(v) if cmd == "clip.link" && v["linked"] == json!(false) => {
                                if let Some(clip) = ctx.data(|d| d.get_temp::<ClipId>(menu_clip_id))
                                    && app.session.state.selection.contains(&clip)
                                {
                                    let _ = app.session.execute("timeline.select", json!({"clips": [clip.0]}));
                                }
                            }
                            Ok(_) => {}
                            Err(e) => app.ui.status = e,
                        }
                        ui.close();
                    }
                }
            }
        });
    });

    drop_targets(app, ui, seq, layout);

    app.auto.add("timeline.tracks", layout.content, "tracks");
}

#[cfg(test)]
mod marquee_tests {
    use super::*;

    fn layout(v_scroll: f32, a_scroll: f32) -> Layout {
        let content = Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 600.0));
        let ruler = Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 20.0));
        Layout { content, ruler, rows: Vec::new(), pps: 100.0, scroll: 0.0, split_y: 300.0, v_scroll, a_scroll }
    }

    /// Review of #3583: a video corner scrolled down past the divider, with the pointer still over the
    /// video tracks, must not reach into the audio tracks; once the pointer enters them it does.
    #[test]
    fn a_video_corner_scrolled_below_the_divider_stays_out_of_the_audio_tracks() {
        let anchor = MarqueeAnchor::new(&layout(0.0, 0.0), Tick::ZERO, (0.0, 0.0), pos2(50.0, 100.0));
        let l = layout(500.0, 0.0);
        let r = anchor.rect_to(&l, pos2(200.0, 150.0));
        assert!(r.max.y < l.split_y, "the box ends above the divider: {r:?}");
        let r = anchor.rect_to(&l, pos2(200.0, 400.0));
        assert!(r.max.y >= 400.0, "the pointer in the audio tracks reaches them: {r:?}");
    }

    /// Review of #3583: the audio tracks scroll on their own (`a_scroll` moves them up); a corner
    /// pressed among them moves with them, and the video scroll does not move it.
    #[test]
    fn an_audio_corner_moves_with_the_audio_scroll() {
        let anchor = MarqueeAnchor::new(&layout(30.0, 10.0), Tick::ZERO, (30.0, 10.0), pos2(0.0, 420.0));
        let r = anchor.rect_to(&layout(80.0, 60.0), pos2(200.0, 520.0));
        assert_eq!(r, Rect::from_min_max(pos2(0.0, 370.0), pos2(200.0, 520.0)));
        // scrolled up past the divider, with the pointer still below it: it stays out of the video tracks
        let l = layout(0.0, 400.0);
        let r = anchor.rect_to(&l, pos2(200.0, 350.0));
        assert!(r.min.y > l.split_y, "the box starts below the divider: {r:?}");
        let r = anchor.rect_to(&l, pos2(200.0, 100.0));
        assert!(r.min.y <= 100.0, "the pointer in the video tracks reaches them: {r:?}");
    }
}
