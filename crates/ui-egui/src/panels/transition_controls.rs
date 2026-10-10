//! A selected transition (#430): its Effect Controls (Duration, Alignment, Reverse and its own
//! settings, as Premiere's Change transition settings / Align transitions / Change transition
//! duration pages describe them) and the Set Transition Duration dialog a double-click on it in
//! the Timeline opens. Every change is one `sequence.setTransition`; dragging a value is one undo
//! step.
//!
//! Not here yet: the A/B preview thumbnails with their Start / End sliders, Show Actual Sources,
//! and dragging the transition in Effect Controls' own time ruler (the Timeline does that).
//!
//! Automation ids: `effectControls.transition.duration`, `.alignment` (+ `.alignment.option.<center|
//! start|end>` while open), `.reverse`, `.reset`, `.param.<id>` (a point's are `.param.<id>.x` /
//! `.y`; a list's entries `.param.<id>.option.<n>` while open); the dialog's
//! `transitionDuration.value`, `.ok`, `.cancel`.

use egui::{Rect, RichText, vec2};
use filmcraft_edit::transitions::{self as trn, Alignment};
use filmcraft_project::{ParamKind, ParamValue, Transition, TransitionId};
use filmcraft_time::FrameRate;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::TransitionDurationDraft;

type Elems = Vec<(String, Rect, String)>;

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
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 6.0))).id_salt(("tr-ec", id.0)));
    child.label(RichText::new(format!("{seq_name} · {name}")).color(t.text_dim));
    child.add_space(4.0);
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

            ui.label(tl!("Reverse"));
            let mut rev = x.reverse;
            let r = ui.checkbox(&mut rev, "");
            elems.push(("effectControls.transition.reverse".into(), r.rect, rev.to_string()));
            if r.changed() {
                changes.push(json!({"reverse": rev}));
            }
            ui.end_row();

            for pd in def.map(|d| d.params.as_slice()).unwrap_or(&[]) {
                let value = x.effect.params.get(pd.id).map_or(&pd.default, |p| &p.value);
                ui.label(crate::i18n::t(pd.label));
                if let Some(v) = param_widget(ui, &mut elems, pd.id, &pd.kind, value, (fw, fh)) {
                    changes.push(v);
                }
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        let r = ui.button(tl!("Reset"));
        elems.push(("effectControls.transition.reset".into(), r.rect, "Reset".into()));
        if r.clicked() {
            changes.push(json!({"reset": true}));
        }
    });
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
    let (mut keep, mut apply) = (true, false);
    let mut elems: Elems = Vec::new();
    crate::dialog_style::Window::new(tl!("Set Transition Duration"))
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
            crate::dialog_style::actions(ui, |ui| {
                let o = ui.add(crate::dialog_style::primary(tl!("OK")));
                elems.push(("transitionDuration.ok".into(), o.rect, "OK".into()));
                apply |= o.clicked();
                let c = ui.add(crate::dialog_style::secondary(tl!("Cancel")));
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
