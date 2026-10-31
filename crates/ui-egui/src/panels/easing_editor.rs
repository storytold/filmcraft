//! Keyframe Easing editor: right-click a keyframe ▸ Easing Editor…. It edits the segment from that
//! keyframe to the next one in two ways, chosen at the top of the window:
//!
//! - **Simple**: a cubic-bezier curve with two draggable handles (the start's outgoing handle and
//!   the end's incoming handle) and one-click presets (Linear, Ease In, Ease Out, Ease In-Out,
//!   Strong, Hold).
//! - **Advanced**: the interpolation type and the two influences as numbers. The value and
//!   velocity graphs in Effect Controls remain the place for full per-handle control.
//!
//! Changes apply when a handle is released, a preset is clicked or Apply is pressed, so one gesture
//! is one undo step per edited keyframe (the interpolation and each influence are separate edits).
//!
//! Automation ids: `easing.mode.simple`, `easing.mode.advanced`, `easing.preset.<id>`,
//! `easing.handle.out`, `easing.handle.in`, `easing.interp`, `easing.outInfluence`,
//! `easing.inInfluence`, `easing.apply`, `easing.close`.

use egui::{Align2, Color32, Pos2, Rect, Sense, Shape, Stroke, pos2, vec2};
use filmcraft_project::{Interpolation, Keyframe};
use serde_json::json;

use crate::FilmcraftApp;
use crate::state::{EasingDraft, KeyframeRef};
use crate::theme::Tokens;

/// Influence limits the engine clamps to.
const MIN_INFLUENCE: f64 = 0.01;
const PLOT: f32 = 300.0;

struct Preset {
    id: &'static str,
    label: &'static str,
    interp: &'static str,
    out: f64,
    inn: f64,
}

/// Flat-handle presets; the numbers are CSS-style `cubic-bezier(out, 0, 1 - in, 1)` easings.
const PRESETS: [Preset; 6] = [
    Preset { id: "linear", label: "Linear", interp: "linear", out: 1.0 / 3.0, inn: 1.0 / 3.0 },
    Preset { id: "easeIn", label: "Ease In", interp: "bezier", out: 0.42, inn: MIN_INFLUENCE },
    Preset { id: "easeOut", label: "Ease Out", interp: "bezier", out: MIN_INFLUENCE, inn: 0.42 },
    Preset { id: "easeInOut", label: "Ease In-Out", interp: "bezier", out: 0.42, inn: 0.42 },
    Preset { id: "strong", label: "Strong", interp: "bezier", out: 0.7, inn: 0.7 },
    Preset { id: "hold", label: "Hold", interp: "hold", out: 1.0 / 3.0, inn: 1.0 / 3.0 },
];

/// (`effects.setInterpolation` name, label) in the order of the keyframe menu.
const INTERPS: [(&str, &str); 7] = [
    ("linear", "Linear"),
    ("bezier", "Bezier"),
    ("autoBezier", "Auto Bezier"),
    ("continuousBezier", "Continuous Bezier"),
    ("hold", "Hold"),
    ("easeIn", "Ease In"),
    ("easeOut", "Ease Out"),
];

fn interp_name(i: Interpolation) -> &'static str {
    match i {
        Interpolation::Linear => "linear",
        Interpolation::Bezier => "bezier",
        Interpolation::AutoBezier => "autoBezier",
        Interpolation::ContinuousBezier => "continuousBezier",
        Interpolation::Hold => "hold",
        Interpolation::EaseIn => "easeIn",
        Interpolation::EaseOut => "easeOut",
    }
}

/// The editor's starting state for the segment `key` → `next`.
pub fn draft_for(target: KeyframeRef, key: &Keyframe, next: &Keyframe) -> EasingDraft {
    EasingDraft {
        target,
        next_time: next.time,
        interp: interp_name(key.interp).to_string(),
        out_influence: key.out_influence.clamp(MIN_INFLUENCE, 1.0),
        in_influence: next.in_influence.clamp(MIN_INFLUENCE, 1.0),
        advanced: false,
    }
}

/// Points (x, y), both 0..=1, of the easing curve between the two keyframes.
fn curve(interp: &str, out: f64, inn: f64) -> Vec<(f64, f64)> {
    match interp {
        "linear" => vec![(0.0, 0.0), (1.0, 1.0)],
        "hold" => vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
        _ => {
            let (x1, x2) = (out.clamp(MIN_INFLUENCE, 1.0), 1.0 - inn.clamp(MIN_INFLUENCE, 1.0));
            (0..=48)
                .map(|i| {
                    let s = f64::from(i) / 48.0;
                    let (a, b, c) = (3.0 * (1.0 - s) * (1.0 - s) * s, 3.0 * (1.0 - s) * s * s, s * s * s);
                    // y1 = 0 and y2 = 1: the handles are flat
                    (a * x1 + b * x2 + c, b + c)
                })
                .collect()
        }
    }
}

/// Whether the draft is the preset (within a small tolerance).
fn is_preset(d: &EasingDraft, p: &Preset) -> bool {
    if d.interp != p.interp {
        return false;
    }
    p.interp != "bezier" || ((d.out_influence - p.out).abs() < 0.015 && (d.in_influence - p.inn).abs() < 0.015)
}

fn is_ease(interp: &str) -> bool {
    !matches!(interp, "linear" | "hold")
}

/// Dragging a handle of a segment that is not eased (linear): it becomes a bezier, and the other
/// handle starts where it was drawn (a third), not at a stale influence kept from an older edit.
fn leave_flat(d: &mut EasingDraft, dragged: usize) {
    if is_ease(&d.interp) {
        return;
    }
    d.interp = "bezier".into();
    if dragged == 0 {
        d.in_influence = 1.0 / 3.0;
    } else {
        d.out_influence = 1.0 / 3.0;
    }
}

/// Send the draft to the project: the interpolation on the start keyframe, its outgoing influence,
/// and the incoming influence on the end keyframe.
fn apply(app: &mut FilmcraftApp, d: &EasingDraft) {
    let base = |time: filmcraft_time::Tick| {
        let mut v = json!({"clip": d.target.clip, "effect": d.target.effect, "param": d.target.param, "mediaTime": time.0});
        if let Some(mask) = d.target.mask {
            v["mask"] = json!(mask);
        }
        v
    };
    let mut interp = base(d.target.time);
    interp["interpolation"] = json!(d.interp);
    let mut out = base(d.target.time);
    out["outInfluence"] = json!(d.out_influence.clamp(MIN_INFLUENCE, 1.0));
    let mut inn = base(d.next_time);
    inn["inInfluence"] = json!(d.in_influence.clamp(MIN_INFLUENCE, 1.0));
    for (cmd, params) in [("effects.setInterpolation", interp), ("effects.setKeyframe", out), ("effects.setKeyframe", inn)] {
        if let Err(e) = app.session.execute(cmd, params) {
            app.ui.status = e.to_string();
            return;
        }
    }
}

/// Draw the Easing editor window while `ui.easing` is open.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.easing.take() else { return };
    let mut open = true;
    egui::Window::new(tl!("Keyframe Easing"))
        .id(egui::Id::new("easing-editor"))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (advanced, id, label) in [(false, "easing.mode.simple", tl!("Simple")), (true, "easing.mode.advanced", tl!("Advanced"))] {
                    let r = ui.selectable_label(d.advanced == advanced, label);
                    app.auto.add(id, r.rect, label);
                    if r.clicked() {
                        d.advanced = advanced;
                    }
                }
            });
            ui.separator();
            if d.advanced {
                advanced_page(app, ui, &mut d);
            } else {
                simple_page(app, ui, &mut d);
            }
        });
    if crate::widgets::escape_closes(ctx) {
        open = false;
    }
    if open {
        app.ui.easing = Some(d);
    }
}

fn advanced_page(app: &mut FilmcraftApp, ui: &mut egui::Ui, d: &mut EasingDraft) {
    let current = INTERPS.iter().find(|(k, _)| *k == d.interp).map_or("Linear", |(_, l)| *l);
    ui.horizontal(|ui| {
        ui.label(tl!("Interpolation"));
        let combo = egui::ComboBox::from_id_salt("easing-interp").selected_text(crate::i18n::t(current)).show_ui(ui, |ui| {
            for (key, label) in INTERPS {
                ui.selectable_value(&mut d.interp, key.to_string(), crate::i18n::t(label));
            }
        });
        app.auto.add("easing.interp", combo.response.rect, tl!("Interpolation"));
    });
    for (id, label, value) in [
        ("easing.outInfluence", tl!("Outgoing influence"), &mut d.out_influence),
        ("easing.inInfluence", tl!("Incoming influence"), &mut d.in_influence),
    ] {
        ui.horizontal(|ui| {
            ui.label(label);
            let mut pct = *value * 100.0;
            let r = ui.add(egui::DragValue::new(&mut pct).range(1.0..=100.0).suffix("%").speed(0.5));
            app.auto.add(id, r.rect, label);
            if r.changed() {
                *value = (pct / 100.0).clamp(MIN_INFLUENCE, 1.0);
            }
        });
    }
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Open the value graph in Effect Controls for full handle control.")).color(app.tokens.text_dim));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let apply_btn = ui.button(tl!("Apply"));
        app.auto.add("easing.apply", apply_btn.rect, tl!("Apply"));
        if apply_btn.clicked() {
            apply(app, d);
        }
    });
}

fn simple_page(app: &mut FilmcraftApp, ui: &mut egui::Ui, d: &mut EasingDraft) {
    let t = app.tokens;
    let (area, _) = ui.allocate_exact_size(vec2(PLOT + 40.0 + 2.0 * 76.0 + 12.0, PLOT + 56.0), Sense::hover());
    let plot = Rect::from_min_size(area.min + vec2(20.0, 20.0), vec2(PLOT, PLOT));
    let p = ui.painter().clone();
    let to_screen = |x: f64, y: f64| pos2(plot.min.x + x as f32 * plot.width(), plot.max.y - y as f32 * plot.height());

    // grid
    p.rect_filled(plot.expand(14.0), 6.0, t.app_bg);
    for i in 0..=10 {
        let f = i as f32 / 10.0;
        let c = if i == 0 || i == 10 { t.text_faint } else { t.separator };
        p.line_segment([pos2(plot.min.x + f * plot.width(), plot.min.y), pos2(plot.min.x + f * plot.width(), plot.max.y)], Stroke::new(1.0, c));
        p.line_segment([pos2(plot.min.x, plot.min.y + f * plot.height()), pos2(plot.max.x, plot.min.y + f * plot.height())], Stroke::new(1.0, c));
    }

    // curve
    let pts: Vec<Pos2> = curve(&d.interp, d.out_influence, d.in_influence).into_iter().map(|(x, y)| to_screen(x, y)).collect();
    p.add(Shape::line(pts, Stroke::new(3.0, t.accent)));
    p.circle_filled(to_screen(0.0, 0.0), 5.0, t.accent);
    p.circle_filled(to_screen(1.0, 1.0), 5.0, t.accent);

    // handles: the start's outgoing one on the bottom edge, the end's incoming one on the top edge
    let ease = is_ease(&d.interp);
    // (id, anchor, flat handle position, where it sits while the segment is not eased)
    let handles = [
        ("easing.handle.out", to_screen(0.0, 0.0), to_screen(d.out_influence, 0.0), to_screen(1.0 / 3.0, 0.0)),
        ("easing.handle.in", to_screen(1.0, 1.0), to_screen(1.0 - d.in_influence, 1.0), to_screen(2.0 / 3.0, 1.0)),
    ];
    let mut released = false;
    for (n, (id, anchor, flat, idle)) in handles.into_iter().enumerate() {
        if d.interp == "hold" {
            break;
        }
        let at = if ease { flat } else { idle };
        let r = Rect::from_center_size(at, vec2(22.0, 22.0));
        let resp = ui.interact(r, egui::Id::new(("easing-handle", n)), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        app.auto.add(id, r, tl!("Easing handle"));
        if let Some(pos) = resp.interact_pointer_pos().filter(|_| resp.dragged()) {
            let fx = f64::from(((pos.x - plot.min.x) / plot.width()).clamp(0.0, 1.0));
            if n == 0 {
                d.out_influence = fx.clamp(MIN_INFLUENCE, 1.0);
            } else {
                d.in_influence = (1.0 - fx).clamp(MIN_INFLUENCE, 1.0);
            }
            leave_flat(d, n);
        }
        released |= resp.drag_stopped();
        let dim = if ease { Color32::WHITE } else { t.text_faint };
        p.line_segment([anchor, at], Stroke::new(3.0, dim));
        p.circle_filled(at, if resp.hovered() || resp.dragged() { 10.0 } else { 8.0 }, dim);
    }
    if released {
        apply(app, d);
    }

    // caption under the plot
    let name = PRESETS.iter().find(|p| is_preset(d, p)).map_or(tl!("Custom"), |p| crate::i18n::t(p.label));
    p.text(pos2(plot.center().x, plot.max.y + 32.0), Align2::CENTER_CENTER, name.to_uppercase(), Tokens::semibold(12.0), t.text_dim);

    // presets
    let mut chosen = None;
    for (i, preset) in PRESETS.iter().enumerate() {
        let col = (i % 2) as f32;
        let row = (i / 2) as f32;
        let r = Rect::from_min_size(pos2(plot.max.x + 34.0 + col * 76.0, plot.min.y - 6.0 + row * 100.0), vec2(68.0, 92.0));
        let on = is_preset(d, preset);
        let resp = ui.interact(r, egui::Id::new(("easing-preset", preset.id)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        app.auto.add(&format!("easing.preset.{}", preset.id), r, preset.label);
        if on || resp.hovered() {
            p.rect_filled(r, 8.0, if on { t.app_bg } else { t.hover });
        }
        let icon = Rect::from_min_size(r.min + vec2(12.0, 8.0), vec2(44.0, 44.0));
        p.rect_stroke(icon, 0.0, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
        let ink = if on { t.accent } else { t.text };
        let pts: Vec<Pos2> = curve(preset.interp, preset.out, preset.inn)
            .into_iter()
            .map(|(x, y)| pos2(icon.min.x + x as f32 * icon.width(), icon.max.y - y as f32 * icon.height()))
            .collect();
        p.add(Shape::line(pts, Stroke::new(2.0, ink)));
        p.text(pos2(r.center().x, r.max.y - 16.0), Align2::CENTER_CENTER, crate::i18n::t(preset.label), Tokens::ui(11.0), if on { t.text } else { t.text_dim });
        if resp.clicked() {
            chosen = Some(preset);
        }
    }
    if let Some(preset) = chosen {
        d.interp = preset.interp.to_string();
        d.out_influence = preset.out;
        d.in_influence = preset.inn;
        apply(app, d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(interp: &str, out: f64, inn: f64) -> EasingDraft {
        EasingDraft {
            target: KeyframeRef { clip: 1, effect: 0, param: "opacity".into(), mask: None, time: filmcraft_time::Tick(0) },
            next_time: filmcraft_time::Tick(100),
            interp: interp.into(),
            out_influence: out,
            in_influence: inn,
            advanced: false,
        }
    }

    #[test]
    fn curve_runs_from_corner_to_corner_and_stays_in_range() {
        for (interp, out, inn) in [("bezier", 0.42, 0.42), ("bezier", 0.01, 1.0), ("linear", 0.3, 0.3), ("hold", 0.3, 0.3)] {
            let c = curve(interp, out, inn);
            assert_eq!(c.first().copied(), Some((0.0, 0.0)), "{interp}");
            let last = c.last().copied().unwrap_or_default();
            assert!((last.0 - 1.0).abs() < 1e-9 && (last.1 - 1.0).abs() < 1e-9, "{interp}: {last:?}");
            assert!(c.iter().all(|(x, y)| (0.0..=1.0).contains(x) && (0.0..=1.0 + 1e-9).contains(y)), "{interp}");
        }
    }

    #[test]
    fn symmetric_ease_passes_through_the_middle() {
        let c = curve("bezier", 0.42, 0.42);
        let mid = c[24];
        assert!((mid.0 - 0.5).abs() < 1e-9 && (mid.1 - 0.5).abs() < 1e-9, "{mid:?}");
    }

    #[test]
    fn presets_are_recognised_and_custom_shapes_are_not() {
        for p in &PRESETS {
            assert!(is_preset(&draft(p.interp, p.out, p.inn), p), "{}", p.id);
        }
        assert!(!PRESETS.iter().any(|p| is_preset(&draft("bezier", 0.2, 0.8), p)));
        assert!(!is_preset(&draft("hold", 0.3, 0.3), &PRESETS[0]));
    }

    #[test]
    fn dragging_a_linear_handle_keeps_the_other_one_where_it_was_drawn() {
        let mut d = draft("linear", 0.9, 0.8);
        leave_flat(&mut d, 0);
        assert_eq!((d.interp.as_str(), d.in_influence), ("bezier", 1.0 / 3.0));
        let mut d = draft("bezier", 0.9, 0.8);
        leave_flat(&mut d, 0);
        assert_eq!(d.in_influence, 0.8);
    }

    #[test]
    fn draft_reads_both_keyframes_and_clamps() {
        let mut a = Keyframe::new(filmcraft_time::Tick(0), filmcraft_project::ParamValue::Float(0.0));
        let mut b = Keyframe::new(filmcraft_time::Tick(100), filmcraft_project::ParamValue::Float(1.0));
        a.interp = Interpolation::Bezier;
        a.out_influence = 0.0;
        b.in_influence = 5.0;
        let d = draft_for(draft("linear", 0.0, 0.0).target, &a, &b);
        assert_eq!((d.interp.as_str(), d.next_time.0), ("bezier", 100));
        assert_eq!((d.out_influence, d.in_influence), (MIN_INFLUENCE, 1.0));
    }
}
