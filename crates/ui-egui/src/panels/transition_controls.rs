//! A selected transition (#430): its Effect Controls (Duration, Alignment, Reverse and its own
//! settings, as Premiere's Change transition settings / Align transitions / Change transition
//! duration pages describe them) and the Set Transition Duration dialog a double-click on it in
//! the Timeline opens. Every change is one `sequence.setTransition`; dragging a value is one undo
//! step.
//!
//! Right of the settings, as in Premiere, is the transition's own timeline (#577): a time ruler over
//! the cut, the outgoing clip (A) above, the incoming clip (B) below and the transition between
//! them. Dragging the transition's middle slides it over the cut; dragging one of its ends changes
//! its duration from that end, the other staying put (the Timeline's rules,
//! [`trn::drag_span`]); each drag is one undo step. Clicking or dragging the ruler moves the
//! playhead. The divider between the settings and the timeline is the clip view's.
//!
//! Not here yet: the A/B preview thumbnails with their Start / End sliders, Show Actual Sources,
//! and a rolling edit by dragging clip A or B in the transition's timeline.
//!
//! Automation ids: `effectControls.transition.duration`, `.alignment` (+ `.alignment.option.<center|
//! start|end>` while open), `.reverse`, `.reset`, `.param.<id>` (a point's are `.param.<id>.x` /
//! `.y`; a list's entries `.param.<id>.option.<n>` while open); its timeline's
//! `effectControls.transition.timeline`, `.timeline.ruler`, `.timeline.a` / `.timeline.b` (the
//! clips, labelled with their names), `.timeline.span` (the transition) with its ends
//! `.timeline.span.in` / `.timeline.span.out`, `.timeline.playhead`; `effectControls.divider`;
//! the dialog's `transitionDuration.value`, `.ok`, `.cancel`.

use egui::{Align2, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};
use filmcraft_edit::Edge;
use filmcraft_edit::transitions::{self as trn, Alignment};
use filmcraft_project::{ParamKind, ParamValue, Transition, TransitionId};
use filmcraft_time::{FrameRate, Tick};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::TransitionDurationDraft;
use crate::theme::Tokens;

type Elems = Vec<(String, Rect, String)>;

// the same sizes as the clip view's (`effect_controls.rs`), so the two look alike
const MIN_LIST_W: f32 = 260.0;
const MIN_LANE_W: f32 = 60.0;
const RULER_H: f32 = 24.0;
const BAR_H: f32 = 18.0;
/// How far into the transition from either end a press grabs that end (the Timeline's width).
const EDGE_PX: f32 = 7.0;

/// The selected transition, when it still exists, with its alignment and how many frames it may
/// last at most (the clips it joins).
fn lookup(app: &FilmcraftApp, id: TransitionId) -> Option<(Transition, Alignment, i64)> {
    let seq = app.session.active_sequence()?;
    let rate = seq.settings.frame_rate.sane();
    seq.all_tracks().find_map(|tr| {
        let x = tr.transitions.iter().find(|x| x.id == id)?;
        let (lo, cut, hi) = trn::bounds(tr, x)?;
        let max = rate.frame_at(hi - lo).max(1);
        Some((x.clone(), trn::alignment(x, cut, rate.frame_duration()), max))
    })
}

/// The first selected transition that still exists.
pub fn selected(app: &FilmcraftApp) -> Option<TransitionId> {
    app.session.state.transition_selection.iter().copied().find(|id| lookup(app, *id).is_some())
}

fn timecode(frames: f64, rate: FrameRate, drop_frame: bool) -> String {
    filmcraft_time::format_timecode_frames(frames.round() as i64, rate, drop_frame)
}

/// A duration field in frames that reads and takes timecode (or a plain frame count).
fn duration_field(ui: &mut egui::Ui, frames: &mut i64, max: i64, rate: FrameRate, drop_frame: bool) -> egui::Response {
    ui.add(egui::DragValue::new(frames).range(1..=max.max(1)).speed(0.25).custom_formatter(move |v, _| timecode(v, rate, drop_frame)).custom_parser(move |s| {
        match s.trim().parse::<i64>() {
            Ok(n) => Some(n as f64),
            Err(_) => filmcraft_time::parse_timecode(s, rate, drop_frame, 0).ok().map(|n| n as f64),
        }
    }))
}

/// Effect Controls for a selected transition. `rect` is the panel.
pub fn effect_controls(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, id: TransitionId) {
    let t = app.tokens;
    let Some((x, align, max_frames)) = lookup(app, id) else {
        crate::dock::placeholder(ui, rect, &t, tl!("(no clip selected)"));
        return;
    };
    let Some(seq) = app.session.active_sequence() else { return };
    let (rate, drop_frame) = (seq.settings.frame_rate.sane(), seq.settings.drop_frame);
    let (fw, fh) = (f64::from(seq.settings.width), f64::from(seq.settings.height));
    let seq_name = app.session.state.active_sequence.and_then(|i| app.session.project.item(i)).map(|i| i.name.clone()).unwrap_or_default();
    let def = x.effect.def();
    let name = def.map_or(x.effect.effect.as_str(), |d| d.name).to_string();
    let one_sided = x.from.is_none() || x.to.is_none();
    let mut changes: Vec<Value> = Vec::new();
    let mut elems: Elems = Vec::new();
    // the settings on the left, the transition's timeline right of the divider (the clip view's
    // divider and width, #643); a panel too narrow for both shows the settings only
    let max_list = (rect.width() - MIN_LANE_W - 10.0).max(MIN_LIST_W);
    let list_w = if app.ui.effect_controls_split > 0.0 { app.ui.effect_controls_split } else { (rect.width() * 0.58).max(260.0) };
    let list_w = list_w.clamp(MIN_LIST_W, max_list);
    let split = rect.min.x + list_w;
    let lane = Rect::from_min_max(pos2(split + 4.0, rect.min.y + 4.0), pos2(rect.max.x - 6.0, rect.max.y - 6.0));
    let has_lane = lane.width() >= MIN_LANE_W / 2.0 && lane.height() >= RULER_H + 3.0 * BAR_H;
    let list = if has_lane { Rect::from_min_max(rect.min, pos2(split, rect.max.y)) } else { rect };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink2(vec2(10.0, 6.0))).id_salt(("tr-ec", id.0)));
    child.set_clip_rect(list.intersect(ui.clip_rect()));
    child.label(RichText::new(format!("{seq_name} · {name}")).color(t.text_dim));
    child.add_space(4.0);
    // the line under each row, as for a clip's effects (#640): halfway into the row spacing
    let row_line = |ui: &egui::Ui| {
        let y = ui.cursor().top() - 4.0;
        ui.painter().line_segment([pos2(list.min.x, y), pos2(list.max.x, y)], Stroke::new(1.0, t.separator));
    };
    egui::ScrollArea::vertical().id_salt(("tr-ec-scroll", id.0)).auto_shrink([false, false]).show(&mut child, |ui| {
        egui::Grid::new(("tr-ec-grid", id.0)).num_columns(2).spacing([14.0, 8.0]).show(ui, |ui| {
            ui.label(tl!("Duration"));
            let mut frames = rate.frame_at(x.duration).max(1);
            let r = duration_field(ui, &mut frames, max_frames, rate, drop_frame);
            elems.push(("effectControls.transition.duration".into(), r.rect, timecode(frames as f64, rate, drop_frame)));
            if r.changed() {
                changes.push(json!({"frames": frames, "merge": true, "begin": r.drag_started() || !r.dragged()}));
            }
            ui.end_row();
            row_line(ui);

            ui.label(tl!("Alignment"));
            let shown = align.label();
            let r = ui
                .add_enabled_ui(!one_sided, |ui| {
                    egui::ComboBox::from_id_salt(("tr-align", id.0))
                        .selected_text(shown)
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for a in Alignment::MENU {
                                let o = ui.selectable_label(a == align, a.label());
                                if o.clicked() && a != align {
                                    changes.push(json!({"align": a.id()}));
                                }
                                elems.push((format!("effectControls.transition.alignment.option.{}", a.id()), o.rect, a.label().to_string()));
                            }
                        })
                        .response
                })
                .inner;
            elems.push(("effectControls.transition.alignment".into(), r.rect, shown.to_string()));
            ui.end_row();
            row_line(ui);

            ui.label(tl!("Reverse"));
            let mut rev = x.reverse;
            let r = ui.checkbox(&mut rev, "");
            elems.push(("effectControls.transition.reverse".into(), r.rect, rev.to_string()));
            if r.changed() {
                changes.push(json!({"reverse": rev}));
            }
            ui.end_row();
            row_line(ui);

            for pd in def.map(|d| d.params.as_slice()).unwrap_or(&[]) {
                let value = x.effect.params.get(pd.id).map_or(&pd.default, |p| &p.value);
                ui.label(crate::i18n::t(pd.label));
                if let Some(v) = param_widget(ui, &mut elems, pd.id, &pd.kind, value, (fw, fh)) {
                    changes.push(v);
                }
                ui.end_row();
                row_line(ui);
            }
        });
        ui.add_space(8.0);
        let r = ui.button(tl!("Reset"));
        elems.push(("effectControls.transition.reset".into(), r.rect, "Reset".into()));
        if r.clicked() {
            changes.push(json!({"reset": true}));
        }
    });
    if has_lane {
        if let Some(c) = timeline(app, ui, lane, id, &x, &mut elems) {
            changes.push(c);
        }
        let divider = Rect::from_min_max(pos2(split - 2.0, rect.min.y), pos2(split + 4.0, rect.max.y));
        let dresp = ui.interact(divider, egui::Id::new("tr-ec-divider"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if dresp.dragged() {
            app.ui.effect_controls_split = (list_w + dresp.drag_delta().x).clamp(MIN_LIST_W, max_list);
        }
        let dcol = if dresp.hovered() || dresp.dragged() { t.accent } else { t.separator };
        ui.painter().line_segment([pos2(split + 1.5, rect.min.y), pos2(split + 1.5, rect.max.y)], Stroke::new(1.0, dcol));
        elems.push(("effectControls.divider".into(), divider, "divider".into()));
    }
    for (eid, r, l) in elems {
        app.auto.add(&eid, r, &l);
    }
    for mut c in changes {
        c["transition"] = json!(id.0);
        if let Err(e) = app.session.execute("sequence.setTransition", c) {
            app.ui.status = e.to_string();
        }
    }
}

/// A drag in the transition's timeline: the end it grabbed (`None`: the middle), where the
/// transition was when it began, and the stretch of time the lane showed then, so the drag is
/// measured against a ruler that holds still while the transition changes under it. `begun` once
/// its first change went through: the rest merge into that undo step.
#[derive(Clone, Copy, Debug)]
struct LaneDrag {
    edge: Option<Edge>,
    start: Tick,
    duration: Tick,
    view: (Tick, Tick),
    from_x: f32,
    begun: bool,
}

/// The stretch of time the transition's timeline shows: its cut with one and a half times the
/// transition's duration either side, room for it wherever it sits over the cut.
fn view(x: &Transition, cut: Tick, frame: Tick) -> (Tick, Tick) {
    let half = x.duration.max(frame).max(Tick(1)).mul_ratio(3, 2);
    (cut - half, cut + half)
}

/// Where `tk` falls on `lane`, which shows `view`.
fn x_of(lane: Rect, view: (Tick, Tick), tk: Tick) -> f32 {
    let span = (view.1 - view.0).0.max(1) as f64;
    let f = ((tk - view.0).0 as f64 / span).clamp(-100.0, 100.0) as f32;
    // far outside the view stays a finite number of lane widths away
    lane.min.x + f * lane.width()
}

/// The time under `px` on `lane`, which shows `view`.
fn tick_at(lane: Rect, view: (Tick, Tick), px: f32) -> Tick {
    let span = (view.1 - view.0).0.max(1) as f64;
    // `as` saturates (and a NaN is 0), so a hostile width can't overflow
    view.0 + Tick((f64::from((px - lane.min.x) / lane.width().max(1.0)) * span) as i64)
}

/// Ticks per `px` points of `lane` showing `view`, as a time delta.
fn ticks_for(lane: Rect, view: (Tick, Tick), px: f32) -> Tick {
    let span = (view.1 - view.0).0.max(1) as f64;
    Tick((f64::from(px / lane.width().max(1.0)) * span) as i64)
}

/// The selected transition's own timeline, right of its settings: ruler, clip A, the transition,
/// clip B, the cut and the playhead. Returns the change a drag of the transition makes this frame,
/// as `sequence.setTransition` params (without the id); moves the playhead itself.
fn timeline(app: &mut FilmcraftApp, ui: &mut egui::Ui, lane: Rect, id: TransitionId, x: &Transition, elems: &mut Elems) -> Option<Value> {
    let t = app.tokens;
    let seq = app.session.active_sequence()?;
    let (rate, drop_frame) = (seq.settings.frame_rate.sane(), seq.settings.drop_frame);
    let frame = rate.frame_duration();
    let track = seq.all_tracks().find(|tr| tr.transitions.iter().any(|y| y.id == id))?;
    let (_, cut, _) = trn::bounds(track, x)?;
    let clip_a = x.from.and_then(|c| track.item(c)).map(|it| (it.start, it.end(), it.name.clone()));
    let clip_b = x.to.and_then(|c| track.item(c)).map(|it| (it.start, it.end(), it.name.clone()));
    let mem = egui::Id::new(("tr-ec-lane-drag", id.0));
    let mut drag: Option<LaneDrag> = ui.data(|d| d.get_temp(mem));
    let v = drag.map_or_else(|| view(x, cut, frame), |d| d.view);
    let painter = ui.painter().with_clip_rect(lane.intersect(ui.clip_rect()));
    painter.rect_filled(lane, 0.0, t.tl_bg);
    let ruler = Rect::from_min_max(lane.min, pos2(lane.max.x, lane.min.y + RULER_H));
    paint_ruler(&painter, ruler, v, rate, drop_frame, &t);
    // the rows: A on top, the transition between, B under it, as Premiere stacks them
    let row = |i: f32| {
        let top = ruler.max.y + 4.0 + i * (BAR_H + 2.0);
        Rect::from_min_max(pos2(lane.min.x, top), pos2(lane.max.x, top + BAR_H))
    };
    let (row_a, row_x, row_b) = (row(0.0), row(1.0), row(2.0));
    for ((clip, r), key) in [(&clip_a, row_a), (&clip_b, row_b)].into_iter().zip(["a", "b"]) {
        let Some((s, e, name)) = clip else { continue };
        let bar = Rect::from_min_max(pos2(x_of(lane, v, *s).max(lane.min.x), r.min.y), pos2(x_of(lane, v, *e).min(lane.max.x), r.max.y));
        if bar.width() <= 0.0 {
            continue;
        }
        painter.rect_filled(bar, 2.0, t.clip_bar_bg);
        painter.with_clip_rect(bar.intersect(lane)).text(pos2(bar.min.x + 4.0, bar.center().y), Align2::LEFT_CENTER, name, Tokens::ui(10.0), t.text);
        elems.push((format!("effectControls.transition.timeline.{key}"), bar, name.clone()));
    }
    // the transition, outlined as selected (it is), with the Timeline's end zones
    let (x0, x1) = (x_of(lane, v, x.start), x_of(lane, v, x.end()));
    let span = Rect::from_min_max(pos2(x0, row_x.min.y), pos2(x1.max(x0 + 1.0), row_x.max.y));
    painter.rect_filled(span, 0.0, egui::Color32::from_black_alpha(90));
    painter.rect_stroke(span, 0.0, Stroke::new(2.0, t.accent), egui::StrokeKind::Inside);
    let name = x.effect.def().map_or(x.effect.effect.as_str(), |d| d.name);
    if span.width() > 30.0 {
        painter.with_clip_rect(span.shrink(2.0).intersect(lane)).text(
            pos2(span.min.x + 5.0, span.center().y),
            Align2::LEFT_CENTER,
            crate::i18n::t(name),
            Tokens::ui(10.0),
            t.text,
        );
    }
    let zone = EDGE_PX.min(span.width() / 3.0);
    elems.push(("effectControls.transition.timeline.span".into(), span, name.to_string()));
    elems.push(("effectControls.transition.timeline.span.in".into(), Rect::from_min_max(span.min, pos2(span.min.x + zone, span.max.y)), "start".into()));
    elems.push(("effectControls.transition.timeline.span.out".into(), Rect::from_min_max(pos2(span.max.x - zone, span.min.y), span.max), "end".into()));
    // the cut, through all three rows
    let cx = x_of(lane, v, cut);
    painter.line_segment([pos2(cx, row_a.min.y), pos2(cx, row_b.max.y)], Stroke::new(1.0, t.text_dim));
    // the playhead, while it is in view
    let ph = app.session.playhead();
    if ph >= v.0 && ph <= v.1 {
        let px = x_of(lane, v, ph);
        let (top, tip) = (ruler.max.y - 13.0, ruler.max.y);
        let head = vec![pos2(px - 5.0, top), pos2(px + 5.0, top), pos2(px + 5.0, tip - 5.0), pos2(px, tip), pos2(px - 5.0, tip - 5.0)];
        painter.add(egui::Shape::convex_polygon(head, t.playhead, Stroke::NONE));
        painter.line_segment([pos2(px, tip), pos2(px, lane.max.y)], Stroke::new(1.0, t.playhead));
        elems.push(("effectControls.transition.timeline.playhead".into(), Rect::from_min_max(pos2(px - 5.0, top), pos2(px + 5.0, tip)), "playhead".into()));
    }
    elems.push(("effectControls.transition.timeline".into(), lane, "transition timeline".into()));
    elems.push(("effectControls.transition.timeline.ruler".into(), ruler, "time ruler".into()));
    // the ruler moves the playhead (once the sequence is no longer borrowed, below)
    let mut seek = None;
    let rresp = ui.interact(ruler, egui::Id::new(("tr-ec-ruler", id.0)), Sense::click_and_drag());
    if (rresp.dragged() || rresp.clicked())
        && let Some(pos) = rresp.interact_pointer_pos()
    {
        seek = Some(rate.snap_nearest(tick_at(lane, v, pos.x.clamp(lane.min.x, lane.max.x))).max(Tick::ZERO));
    }
    // the transition: its middle slides it, its ends change its duration
    let grab = |px: f32| -> Option<Edge> {
        if px < span.min.x + zone {
            Some(Edge::In)
        } else if px > span.max.x - zone {
            Some(Edge::Out)
        } else {
            None
        }
    };
    let sresp = ui.interact(span, egui::Id::new(("tr-ec-span", id.0)), Sense::click_and_drag());
    if let Some(h) = sresp.hover_pos() {
        ui.ctx().set_cursor_icon(if grab(h.x).is_some() { egui::CursorIcon::ResizeColumn } else { egui::CursorIcon::Grab });
    }
    if sresp.drag_started() {
        let from = ui.input(|i| i.pointer.press_origin()).or(sresp.interact_pointer_pos()).map_or(span.center().x, |p: Pos2| p.x);
        drag = Some(LaneDrag { edge: grab(from), start: x.start, duration: x.duration, view: v, from_x: from, begun: false });
    }
    let mut change = None;
    if sresp.dragged()
        && let Some(d) = drag.as_mut()
        && let Some(pos) = sresp.interact_pointer_pos()
    {
        let delta = rate.snap_nearest(ticks_for(lane, d.view, pos.x - d.from_x));
        let was = Transition { start: d.start, duration: d.duration, ..x.clone() };
        if let Some((st, du)) = trn::drag_span(track, &was, d.edge, delta, frame)
            && (st, du) != (x.start, x.duration)
        {
            change = Some(json!({"start": st.0, "duration": du.0, "merge": true, "begin": !d.begun}));
            d.begun = true;
        }
    }
    // released, or a drag that lost its transition (deleted, deselected) while it went on
    if !sresp.dragged() && !sresp.drag_started() {
        drag = None;
    }
    if let Some(tk) = seek {
        app.stop();
        app.session.set_playhead(tk);
    }
    ui.data_mut(|m| match drag {
        Some(d) => {
            m.insert_temp(mem, d);
        }
        None => m.remove::<LaneDrag>(mem),
    });
    change
}

/// The timeline's ruler: ticks and sequence timecode across `view` (the clip view's spacing).
fn paint_ruler(p: &egui::Painter, ruler: Rect, view: (Tick, Tick), rate: FrameRate, drop_frame: bool, t: &Tokens) {
    let p = p.with_clip_rect(ruler.intersect(p.clip_rect()));
    let span = (view.1 - view.0).0.max(1) as f64;
    let frame_px = f64::from(ruler.width()) * rate.frame_duration().0.max(1) as f64 / span;
    let base = rate.timecode_base();
    // frames, then seconds (a rate's timecode base) multiplied up; saturating, as a damaged rate may be huge
    let steps: Vec<i64> = [1, 2, 5, 10, base / 2, base].into_iter().chain([2, 5, 10, 30, 60, 300, 600, 3600].map(|m| base.saturating_mul(m))).collect();
    // labels far enough apart to read; the small ticks divide the labelled ones evenly
    let label_step = steps.iter().copied().find(|s| *s > 0 && *s as f64 * frame_px >= 80.0);
    let minor = steps.iter().copied().find(|s| *s > 0 && *s as f64 * frame_px >= 8.0 && label_step.is_none_or(|l| l % s == 0));
    let (f0, f1) = (rate.frame_at(view.0), rate.frame_at(view.1));
    let base_y = ruler.max.y - 1.0;
    for (step, h, labelled) in [(minor, 3.0, false), (label_step, 7.0, true)] {
        let Some(step) = step else { continue };
        let mut f = f0.div_euclid(step).saturating_mul(step);
        // a step is at least 8 px wide, so this covers any ruler; the cap is for damaged numbers
        for _ in 0..4096 {
            if f > f1 {
                break;
            }
            let x = x_of(ruler, view, rate.tick_of(f));
            p.line_segment([pos2(x, base_y - h), pos2(x, base_y)], Stroke::new(1.0, t.tl_ruler_tick));
            if labelled && f >= 0 {
                let label = filmcraft_time::format_time(rate.tick_of(f), rate, drop_frame, filmcraft_time::TimeDisplay::Timecode, 48000);
                p.text(pos2(x, ruler.min.y + 7.0), Align2::CENTER_CENTER, label, Tokens::ui(10.0), t.tl_ruler_text);
            }
            f = f.saturating_add(step);
        }
    }
}

/// One of a transition's own settings. Returns the change, as `sequence.setTransition` params.
fn param_widget(ui: &mut egui::Ui, elems: &mut Elems, pid: &str, kind: &ParamKind, value: &ParamValue, frame: (f64, f64)) -> Option<Value> {
    let eid = format!("effectControls.transition.param.{pid}");
    let drag = |r: &egui::Response| json!({"merge": true, "begin": r.drag_started() || !r.dragged()});
    match (kind, value) {
        (ParamKind::Float { min, max, soft_min, soft_max, unit, decimals }, ParamValue::Float(v)) => {
            let mut v = *v;
            let speed = ((soft_max - soft_min) / 200.0).max(0.001);
            let r = ui.add(egui::DragValue::new(&mut v).range(*min..=*max).speed(speed).max_decimals(usize::from(*decimals)).suffix(*unit));
            elems.push((eid, r.rect, format!("{v}")));
            r.changed().then(|| {
                let mut c = drag(&r);
                c["params"] = json!({ pid: v });
                c
            })
        }
        (ParamKind::Angle, ParamValue::Float(v)) => {
            let mut v = *v;
            let r = ui.add(egui::DragValue::new(&mut v).speed(0.5).suffix("°"));
            elems.push((eid, r.rect, format!("{v}")));
            r.changed().then(|| {
                let mut c = drag(&r);
                c["params"] = json!({ pid: v });
                c
            })
        }
        (ParamKind::Bool, ParamValue::Bool(b)) => {
            let mut b = *b;
            let r = ui.checkbox(&mut b, "");
            elems.push((eid, r.rect, b.to_string()));
            r.changed().then(|| json!({"params": { pid: b }}))
        }
        (ParamKind::Choice(opts), ParamValue::Choice(c)) => {
            let shown = opts.get(*c as usize).copied().unwrap_or("");
            let mut picked = None;
            let mut options = Vec::new();
            let r = egui::ComboBox::from_id_salt(("tr-param", pid.to_string())).selected_text(crate::i18n::t(shown)).show_ui(ui, |ui| {
                for (i, o) in opts.iter().enumerate() {
                    let r = ui.selectable_label(i == *c as usize, crate::i18n::t(o));
                    if r.clicked() && i != *c as usize {
                        picked = Some(i);
                    }
                    options.push((format!("{eid}.option.{i}"), r.rect, o.to_string()));
                }
            });
            elems.extend(options);
            elems.push((eid, r.response.rect, shown.to_string()));
            picked.map(|i| json!({"params": { pid: i }}))
        }
        (ParamKind::Color, ParamValue::Color(c)) => {
            let mut c = *c;
            let r = ui.color_edit_button_rgba_unmultiplied(&mut c);
            elems.push((eid, r.rect, format!("{c:?}")));
            r.changed().then(|| {
                let mut v = drag(&r);
                v["params"] = json!({ pid: [c[0], c[1], c[2], c[3]] });
                v
            })
        }
        (ParamKind::Point, ParamValue::Vec2(p)) => {
            // an automatic point (NaN) is the frame's centre until it is moved
            let (mut px, mut py) = (if p.x.is_nan() { frame.0 / 2.0 } else { p.x }, if p.y.is_nan() { frame.1 / 2.0 } else { p.y });
            let mut out = None;
            ui.horizontal(|ui| {
                let rx = ui.add(egui::DragValue::new(&mut px).speed(1.0).prefix("x "));
                let ry = ui.add(egui::DragValue::new(&mut py).speed(1.0).prefix("y "));
                elems.push((format!("{eid}.x"), rx.rect, format!("{px}")));
                elems.push((format!("{eid}.y"), ry.rect, format!("{py}")));
                for r in [&rx, &ry] {
                    if r.changed() {
                        let mut v = drag(r);
                        v["params"] = json!({ pid: [px, py] });
                        out = Some(v);
                    }
                }
            });
            out
        }
        (_, v) => {
            let r = ui.label(RichText::new(format!("{v:?}")).weak());
            elems.push((eid, r.rect, String::new()));
            None
        }
    }
}

/// Double-click a transition in the Timeline: Set Transition Duration.
pub fn open_duration(app: &mut FilmcraftApp, id: TransitionId) {
    let Some((x, ..)) = lookup(app, id) else { return };
    let rate = app.session.active_sequence().map(|q| q.settings.frame_rate.sane()).unwrap_or_default();
    app.ui.transition_duration = TransitionDurationDraft { transition: id.0, frames: rate.frame_at(x.duration).max(1) };
    let _ = app.session.execute("timeline.select", json!({"transitions": [id.0]}));
    app.dialog = Some(crate::Dialog::TransitionDuration);
}

/// The Set Transition Duration dialog. Returns whether it stays open.
pub fn duration_dialog(app: &mut FilmcraftApp, ctx: &egui::Context) -> bool {
    let mut d = app.ui.transition_duration.clone();
    let id = TransitionId(d.transition);
    let Some((_, _, max_frames)) = lookup(app, id) else { return false };
    let Some(seq) = app.session.active_sequence() else { return false };
    let (rate, drop_frame) = (seq.settings.frame_rate.sane(), seq.settings.drop_frame);
    let accent = app.tokens.accent;
    let (mut keep, mut apply) = (true, false);
    let mut elems: Elems = Vec::new();
    egui::Window::new(tl!("Set Transition Duration"))
        .collapsible(false)
        .resizable(false)
        .default_width(260.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(tl!("Duration:"));
                let r = duration_field(ui, &mut d.frames, max_frames, rate, drop_frame);
                elems.push(("transitionDuration.value".into(), r.rect, timecode(d.frames as f64, rate, drop_frame)));
            });
            ui.add_space(8.0);
            // one row high: a right-to-left layout would otherwise take the window's whole height
            let row = egui::vec2(ui.available_width().max(220.0), ui.spacing().interact_size.y);
            ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let o = ui.add(egui::Button::new(RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(accent));
                elems.push(("transitionDuration.ok".into(), o.rect, "OK".into()));
                apply |= o.clicked();
                let c = ui.button(tl!("Cancel"));
                elems.push(("transitionDuration.cancel".into(), c.rect, "Cancel".into()));
                keep &= !c.clicked();
            });
        });
    for (eid, r, l) in elems {
        app.auto.add(&eid, r, &l);
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        keep = false;
    }
    if ctx.input(|i| i.key_pressed(egui::Key::Enter)) && !ctx.egui_wants_keyboard_input() {
        apply = true;
    }
    if apply {
        let cur = lookup(app, id).map(|(x, ..)| rate.frame_at(x.duration));
        if cur != Some(d.frames)
            && let Err(e) = app.session.execute("sequence.setTransition", json!({"transition": id.0, "frames": d.frames}))
        {
            app.ui.status = e.to_string();
        }
        keep = false;
    }
    app.ui.transition_duration = d;
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_as_timecode() {
        assert_eq!(timecode(24.0, FrameRate::FPS_24, false), "00:00:01:00");
        assert_eq!(timecode(30.0, FrameRate::FPS_29_97, true), "00;00;01;00");
    }
}
