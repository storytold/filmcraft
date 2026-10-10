//! Clip / Track Fx Editor windows: graphical editors for the audio effects that need one
//! (Parametric Equalizer curve, Graphic Equalizer 10/20/30 bands, Multiband Compressor,
//! Dynamics). Opened from Effect Controls ("Custom Setup ▸ Edit…") and from a track's effect
//! slot in the Audio Track Mixer.
//!
//! Curves are computed by the real DSP (`render::audio_fx::configured` →
//! `AudioEffect::response_db` / `transfer_db`), so what is drawn is what is heard. Drags preview
//! locally and commit one `effects.setParam` / `mixer.setInsert` on release (one undo step).
//!
//! Automation ids (`<fx>` = effect id): `fxEditor.<fx>.plot`, `fxEditor.<fx>.node.<band>` (EQ
//! nodes: hp, low, b1, b2, mid, b4, b5, high, lp), `fxEditor.<fx>.toggle.<param>` (on/off,
//! solo, bypass…), `fxEditor.<fx>.param.<param>` (sliders), `fxEditor.<fx>.xo1…3` (crossover
//! handles), `fxEditor.<fx>.reset`, `fxEditor.<fx>.close`.

use std::collections::HashMap;

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::{ClipId, EffectInstance, ParamKind, ParamValue, TrackId};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::theme::Tokens;

/// What an editor window edits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FxTarget {
    /// Clip effect `index` of clip `clip` (Clip Fx Editor).
    Clip { clip: u64, index: usize },
    /// Insert `slot` of mixer strip `strip` (track id; Track Fx Editor).
    Insert { strip: u64, slot: usize },
}

/// Effects that have a graphical editor.
pub fn has_editor(effect: &str) -> bool {
    matches!(effect, "parametric_eq" | "graphic_eq" | "graphic_eq_20" | "graphic_eq_30" | "multiband_compressor" | "dynamics_rack")
}

/// Open (or raise) the editor for a target.
pub fn open(app: &mut FilmcraftApp, target: FxTarget) {
    if !app.ui.audio_fx_editors.contains(&target) {
        app.ui.audio_fx_editors.push(target);
    }
}

/// The edited effect instance (current project state), its media time at the playhead, and a
/// display label.
fn resolve(app: &FilmcraftApp, target: &FxTarget) -> Option<(EffectInstance, Tick, String)> {
    let seq = app.session.active_sequence()?;
    let ph = app.session.playhead();
    match target {
        FxTarget::Clip { clip, index } => {
            let (tid, it) = seq.find_item(ClipId(*clip))?;
            let e = it.effects.get(*index)?.clone();
            let track = seq.track(tid).map(|t| t.name.clone()).unwrap_or_default();
            let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
            Some((e, mt, format!("{track}, {}", it.name)))
        }
        FxTarget::Insert { strip, slot } => {
            let tr = seq.strip(TrackId(*strip))?;
            let e = tr.effects.get(*slot)?.clone();
            Some((e, ph, tr.name.clone()))
        }
    }
}

fn key(target: &FxTarget) -> String {
    match target {
        FxTarget::Clip { clip, index } => format!("clip-{clip}-{index}"),
        FxTarget::Insert { strip, slot } => format!("insert-{strip}-{slot}"),
    }
}

/// Command that sets one parameter on the target.
fn set_cmd(target: &FxTarget, pid: &str, v: Value) -> (String, Value) {
    match target {
        FxTarget::Clip { clip, index } => ("effects.setParam".into(), json!({"clip": clip, "effect": index, "param": pid, "value": v})),
        FxTarget::Insert { strip, slot } => ("mixer.setInsert".into(), json!({"strip": strip, "slot": slot, "params": {pid: v}})),
    }
}

/// Per-window editing context.
struct Ed<'a> {
    t: Tokens,
    fx: &'a str,
    target: &'a FxTarget,
    inst: EffectInstance,
    mt: Tick,
    /// Uncommitted values (live drags), by parameter id.
    drafts: HashMap<String, f64>,
    acts: Vec<(String, Value)>,
    autos: Vec<(String, Rect, String)>,
}

impl Ed<'_> {
    fn v(&self, pid: &str) -> f64 {
        self.drafts.get(pid).copied().unwrap_or_else(|| self.inst.f64_at(pid, self.mt))
    }
    fn on(&self, pid: &str) -> bool {
        self.v(pid) >= 0.5
    }
    fn range(&self, pid: &str) -> (f64, f64, &'static str) {
        match self.inst.def().and_then(|d| d.param(pid)).map(|p| &p.kind) {
            Some(ParamKind::Float { min, max, unit, .. }) => (*min, *max, unit),
            _ => (0.0, 1.0, ""),
        }
    }
    fn commit(&mut self, pid: &str, v: f64) {
        let is_bool = matches!(self.inst.def().and_then(|d| d.param(pid)).map(|p| &p.kind), Some(ParamKind::Bool));
        let val = if is_bool { json!(v >= 0.5) } else { json!(v) };
        self.acts.push(set_cmd(self.target, pid, val));
        // keep showing the new value until the project catches up this frame
        self.drafts.insert(pid.to_string(), v);
    }
    fn auto(&mut self, id: String, r: Rect, label: &str) {
        self.autos.push((id, r, label.to_string()));
    }
    /// The effect as currently edited (drafts applied), for curve evaluation.
    fn preview(&self) -> EffectInstance {
        let mut e = self.inst.clone();
        for (k, v) in &self.drafts {
            if let Some(p) = e.params.get_mut(k) {
                p.keyframes.clear();
                p.value = match p.value {
                    ParamValue::Bool(_) => ParamValue::Bool(*v >= 0.5),
                    ParamValue::Choice(_) => ParamValue::Choice(*v as u32),
                    _ => ParamValue::Float(*v),
                };
            } else if let Some(d) = e.def().and_then(|d| d.param(k)) {
                let mut p = filmcraft_project::Param::new(d.default.clone());
                p.value = ParamValue::Float(*v);
                e.params.insert(k.clone(), p);
            }
        }
        e
    }
    /// A labelled slider; drags preview, release commits.
    fn slider(&mut self, ui: &mut egui::Ui, pid: &str, label: &str, log: bool) {
        let (min, max, unit) = self.range(pid);
        let mut v = self.v(pid);
        let r = ui.add(egui::Slider::new(&mut v, min..=max).text(format!("{label} {unit}").trim().to_string()).logarithmic(log && min > 0.0).max_decimals(2));
        self.auto(format!("fxEditor.{}.param.{pid}", self.fx), r.rect, label);
        if r.dragged() {
            self.drafts.insert(pid.to_string(), v);
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            self.commit(pid, v);
        }
    }
    /// A toggle button for a Bool parameter.
    fn toggle(&mut self, ui: &mut egui::Ui, pid: &str, label: &str) {
        let on = self.on(pid);
        let r = ui.selectable_label(on, label);
        self.auto(format!("fxEditor.{}.toggle.{pid}", self.fx), r.rect, label);
        if r.clicked() {
            self.commit(pid, if on { 0.0 } else { 1.0 });
        }
    }
}

/// Show every open editor window.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let targets = app.ui.audio_fx_editors.clone();
    let mut close = Vec::new();
    for target in targets {
        let Some((inst, mt, place)) = resolve(app, &target) else {
            close.push(target);
            continue;
        };
        if !has_editor(&inst.effect) {
            close.push(target);
            continue;
        }
        let name = inst.def().map_or(tl!("Effect"), |d| crate::i18n::t(d.name));
        let kind = if matches!(target, FxTarget::Clip { .. }) { tl!("Clip Fx Editor") } else { tl!("Track Fx Editor") };
        let title = format!("{kind} - {name}: {place}");
        let k = key(&target);
        let drafts_id = egui::Id::new(("fx-editor-drafts", &k));
        let drafts: HashMap<String, f64> = ctx.data(|d| d.get_temp(drafts_id)).unwrap_or_default();
        let fx_id = inst.effect.clone();
        let mut ed = Ed { t: app.tokens, fx: &fx_id, target: &target, inst, mt, drafts, acts: Vec::new(), autos: Vec::new() };
        let mut open = true;
        let mut closed = false;
        let w = egui::Window::new(title).id(egui::Id::new(("fx-editor", &k))).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            match ed.fx {
                "parametric_eq" => parametric(ui, &mut ed),
                "graphic_eq" | "graphic_eq_20" | "graphic_eq_30" => graphic(ui, &mut ed),
                "multiband_compressor" => multiband(ui, &mut ed),
                _ => dynamics(ui, &mut ed),
            }
            ui.separator();
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Close"));
                ed.auto(format!("fxEditor.{}.close", ed.fx), r.rect, "Close");
                if r.clicked() {
                    closed = true;
                }
            });
        });
        // drafts that were committed are dropped once the project shows them
        let committed: Vec<String> = ed
            .acts
            .iter()
            .filter_map(|(_, p)| {
                p.get("param")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| p.get("params").and_then(Value::as_object).and_then(|o| o.keys().next().cloned()))
            })
            .collect();
        let mut keep = ed.drafts.clone();
        for c in &committed {
            keep.remove(c);
        }
        ctx.data_mut(|d| d.insert_temp(drafts_id, keep));
        if let Some(w) = w {
            ed.auto(format!("fxEditor.{}.windowClose", ed.fx), crate::widgets::window_close_rect(ctx, w.response.rect, None), "Close window");
        }
        for (id, r, label) in std::mem::take(&mut ed.autos) {
            app.auto.add(&id, r, &label);
        }
        for (cmd, p) in std::mem::take(&mut ed.acts) {
            if let Err(e) = app.session.execute(&cmd, p) {
                app.ui.status = e.to_string();
            }
        }
        if !open || closed {
            close.push(target);
        }
    }
    app.ui.audio_fx_editors.retain(|t| !close.contains(t));
}

// ------------------------------------------------------------------------------- plotting

const F_MIN: f64 = 20.0;
const F_MAX: f64 = 20000.0;

fn fx_of(x: f32, r: Rect) -> f64 {
    let u = ((x - r.min.x) / r.width()).clamp(0.0, 1.0) as f64;
    F_MIN * (F_MAX / F_MIN).powf(u)
}
fn x_of(f: f64, r: Rect) -> f32 {
    r.min.x + ((f.clamp(F_MIN, F_MAX) / F_MIN).ln() / (F_MAX / F_MIN).ln()) as f32 * r.width()
}
fn y_of(db: f64, r: Rect, range: f64) -> f32 {
    r.center().y - (db.clamp(-range, range) / range) as f32 * r.height() * 0.5
}
fn db_of(y: f32, r: Rect, range: f64) -> f64 {
    ((r.center().y - y) / (r.height() * 0.5)) as f64 * range
}

/// Frequency-response plot with a log-frequency grid; returns the plot rect and response.
fn response_plot(ui: &mut egui::Ui, ed: &mut Ed, size: egui::Vec2, range: f64) -> (Rect, egui::Response) {
    let (r, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let p = ui.painter_at(r);
    let t = &ed.t;
    p.rect_filled(r, 2.0, ed.t.plot_bg);
    for f in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
        let x = x_of(f, r);
        p.line_segment([pos2(x, r.min.y), pos2(x, r.max.y)], Stroke::new(1.0, ed.t.plot_grid));
        let label = if f >= 1000.0 { format!("{}k", f / 1000.0) } else { format!("{f}") };
        p.text(pos2(x + 2.0, r.max.y - 2.0), Align2::LEFT_BOTTOM, label, Tokens::ui(9.0), t.text_faint);
    }
    let step = if range > 30.0 { 12.0 } else { 6.0 };
    let mut d = -range;
    while d <= range + 1e-9 {
        let y = y_of(d, r, range);
        let col = if d.abs() < 1e-9 { ed.t.plot_axis } else { ed.t.plot_grid };
        p.line_segment([pos2(r.min.x, y), pos2(r.max.x, y)], Stroke::new(1.0, col));
        p.text(pos2(r.min.x + 2.0, y - 1.0), Align2::LEFT_BOTTOM, format!("{d:+.0}"), Tokens::ui(9.0), t.text_faint);
        d += step;
    }
    // curve from the DSP
    if let Some(dsp) = filmcraft_render::audio_fx::configured(&ed.preview(), ed.mt, 48000) {
        let pts: Vec<Pos2> = (0..=240)
            .filter_map(|i| {
                let x = r.min.x + r.width() * i as f32 / 240.0;
                dsp.response_db(fx_of(x, r)).map(|db| pos2(x, y_of(db, r, range)))
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.5, Color32::from_rgb(0x4d, 0xa3, 0xff))));
    }
    ed.auto(format!("fxEditor.{}.plot", ed.fx), r, "Response");
    (r, resp)
}

// ------------------------------------------------------------------------- parametric EQ

/// (node label, parameter prefix, has gain)
const PEQ_NODES: [(&str, &str, bool); 9] = [
    ("HP", "hp", false),
    ("L", "low", true),
    ("1", "b1", true),
    ("2", "b2", true),
    ("3", "mid", true),
    ("4", "b4", true),
    ("5", "b5", true),
    ("H", "high", true),
    ("LP", "lp", false),
];

fn parametric(ui: &mut egui::Ui, ed: &mut Ed) {
    let range = 30.0;
    let (r, _) = response_plot(ui, ed, vec2(620.0, 260.0), range);
    for (label, pre, gain) in PEQ_NODES {
        let (on_id, f_id, g_id, q_id) = (format!("{pre}_on"), format!("{pre}_freq"), format!("{pre}_gain"), format!("{pre}_q"));
        let f = ed.v(&f_id);
        let g = if gain { ed.v(&g_id) } else { 0.0 };
        let c = pos2(x_of(f, r), y_of(g, r, range));
        let nr = Rect::from_center_size(c, vec2(16.0, 16.0));
        let id = egui::Id::new(("peq-node", ed.target.clone(), pre));
        let resp = ui.interact(nr, id, Sense::click_and_drag());
        let on = ed.on(&on_id);
        let col = if on { ed.t.eq_node } else { ed.t.eq_node_off };
        ui.painter().circle_stroke(c, 7.0, Stroke::new(1.5, col));
        ui.painter().text(c, Align2::CENTER_CENTER, label, Tokens::ui(8.5), col);
        ed.auto(format!("fxEditor.{}.node.{pre}", ed.fx), nr, label);
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            ed.drafts.insert(f_id.clone(), fx_of(pos.x, r).round());
            if gain {
                let (lo, hi, _) = ed.range(&g_id);
                ed.drafts.insert(g_id.clone(), (db_of(pos.y, r, range) * 10.0).round().clamp(lo * 10.0, hi * 10.0) / 10.0);
            }
        }
        if resp.drag_stopped() {
            let fv = ed.v(&f_id);
            ed.commit(&f_id, fv);
            if gain {
                let gv = ed.v(&g_id);
                ed.commit(&g_id, gv);
            }
        }
        // scroll over a node changes its Q
        if gain && resp.hovered() {
            let s = ui.input(|i| i.smooth_scroll_delta.y);
            if s.abs() > 0.5 {
                let (lo, hi, _) = ed.range(&q_id);
                let q = (ed.v(&q_id) * if s > 0.0 { 1.1 } else { 1.0 / 1.1 }).clamp(lo, hi);
                ed.commit(&q_id, (q * 100.0).round() / 100.0);
            }
        }
        if resp.double_clicked() {
            ed.commit(&on_id, if on { 0.0 } else { 1.0 });
        }
    }
    ui.horizontal(|ui| {
        for (label, pre, _) in PEQ_NODES {
            ed.toggle(ui, &format!("{pre}_on"), label);
        }
    });
    ed.slider(ui, "master_gain", tl!("Master Gain"), false);
}

// --------------------------------------------------------------------------- graphic EQ

fn graphic(ui: &mut egui::Ui, ed: &mut Ed) {
    let n = match ed.fx {
        "graphic_eq" => 10,
        "graphic_eq_20" => 20,
        _ => 30,
    };
    let labels: &[&str] = match n {
        10 => &filmcraft_project::effect::GEQ10_LABELS,
        20 => &filmcraft_project::effect::GEQ20_LABELS,
        _ => &filmcraft_project::effect::GEQ30_LABELS,
    };
    let w = (n as f32 * 26.0).max(420.0);
    response_plot(ui, ed, vec2(w, 140.0), 24.0);
    ui.add_space(4.0);
    let colw = w / n as f32;
    let (area, _) = ui.allocate_exact_size(vec2(w, 170.0), Sense::hover());
    for (i, label) in labels.iter().enumerate() {
        let pid = format!("b{}", i + 1);
        let x = area.min.x + colw * (i as f32 + 0.5);
        let track = Rect::from_center_size(pos2(x, area.min.y + 70.0), vec2(6.0, 130.0));
        ui.painter().rect_filled(track, 2.0, ed.t.eq_track);
        ui.painter().line_segment([pos2(x - 5.0, track.center().y), pos2(x + 5.0, track.center().y)], Stroke::new(1.0, ed.t.eq_tick));
        let v = ed.v(&pid);
        let ky = track.center().y - (v / 24.0) as f32 * track.height() * 0.5;
        let knob = Rect::from_center_size(pos2(x, ky), vec2(colw.min(18.0), 8.0));
        let id = egui::Id::new(("geq-band", ed.target.clone(), i));
        let resp = ui.interact(knob.union(track).expand(3.0), id, Sense::click_and_drag());
        ui.painter().rect_filled(knob, 2.0, if resp.hovered() || resp.dragged() { ed.t.eq_knob_active } else { ed.t.eq_knob });
        ui.painter().text(pos2(x, area.max.y - 22.0), Align2::CENTER_CENTER, format!("{v:+.0}"), Tokens::ui(8.5), ed.t.text_dim);
        let short = label.replace(" Hz", "").replace(" kHz", "k");
        ui.painter().text(pos2(x, area.max.y - 8.0), Align2::CENTER_CENTER, short, Tokens::ui(8.5), ed.t.text_faint);
        ed.auto(format!("fxEditor.{}.param.{pid}", ed.fx), track.union(knob), label);
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let db = (((track.center().y - pos.y) / (track.height() * 0.5)) as f64 * 24.0).clamp(-24.0, 24.0);
            ed.drafts.insert(pid.clone(), (db * 2.0).round() / 2.0);
        }
        if resp.drag_stopped() {
            let nv = ed.v(&pid);
            ed.commit(&pid, nv);
        }
        if resp.double_clicked() {
            ed.commit(&pid, 0.0);
        }
    }
    ui.horizontal(|ui| {
        ed.slider(ui, "gain", tl!("Master Gain"), false);
        let r = ui.button(tl!("Reset"));
        ed.auto(format!("fxEditor.{}.reset", ed.fx), r.rect, "Reset");
        if r.clicked() {
            for i in 0..n {
                let pid = format!("b{}", i + 1);
                if ed.v(&pid) != 0.0 {
                    ed.commit(&pid, 0.0);
                }
            }
        }
    });
}

// --------------------------------------------------------------------- transfer curves

fn transfer_plot(ui: &mut egui::Ui, ed: &mut Ed, size: f32, band: usize, auto: String) {
    let (r, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    let p = ui.painter_at(r);
    p.rect_filled(r, 2.0, ed.t.plot_bg);
    let lo = -60.0;
    let map = |x: f64, y: f64| pos2(r.min.x + ((x - lo) / -lo) as f32 * r.width(), r.max.y - (((y - lo) / -lo).clamp(-0.05, 1.05)) as f32 * r.height());
    for d in [-48.0, -36.0, -24.0, -12.0] {
        p.line_segment([map(d, lo), map(d, 0.0)], Stroke::new(1.0, ed.t.plot_grid));
        p.line_segment([map(lo, d), map(0.0, d)], Stroke::new(1.0, ed.t.plot_grid));
    }
    p.line_segment([map(lo, lo), map(0.0, 0.0)], Stroke::new(1.0, ed.t.plot_axis));
    if let Some(dsp) = filmcraft_render::audio_fx::configured(&ed.preview(), ed.mt, 48000) {
        let pts: Vec<Pos2> = (0..=120)
            .filter_map(|i| {
                let x = lo + -lo * i as f64 / 120.0;
                dsp.transfer_db(band, x as f32).map(|y| map(x, y as f64))
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.5, Color32::from_rgb(0xff, 0xb3, 0x47))));
    }
    p.text(r.left_top() + vec2(3.0, 2.0), Align2::LEFT_TOP, tl!("out"), Tokens::ui(8.5), ed.t.text_faint);
    p.text(r.right_bottom() - vec2(3.0, 2.0), Align2::RIGHT_BOTTOM, tl!("in"), Tokens::ui(8.5), ed.t.text_faint);
    ed.auto(auto, r, "Transfer curve");
}

// ------------------------------------------------------------------- multiband compressor

fn multiband(ui: &mut egui::Ui, ed: &mut Ed) {
    // Spectrum strip with draggable crossover handles.
    let (r, _) = ui.allocate_exact_size(vec2(640.0, 70.0), Sense::hover());
    ui.painter().rect_filled(r, 2.0, ed.t.plot_bg);
    let cols =
        [Color32::from_rgb(0x3d, 0x6e, 0xb4), Color32::from_rgb(0x3d, 0xa0, 0x6e), Color32::from_rgb(0xb4, 0x9a, 0x3d), Color32::from_rgb(0xb4, 0x4f, 0x3d)];
    let xo = [ed.v("xo1"), ed.v("xo2"), ed.v("xo3")];
    let edges = [r.min.x, x_of(xo[0], r), x_of(xo[1], r), x_of(xo[2], r), r.max.x];
    for b in 0..4 {
        let br = Rect::from_min_max(pos2(edges[b], r.min.y), pos2(edges[b + 1], r.max.y));
        ui.painter().rect_filled(br.shrink2(vec2(0.0, 8.0)), 0.0, cols[b].gamma_multiply(0.35));
        ui.painter().text(br.center(), Align2::CENTER_CENTER, tlf!("Band {n}", n = b + 1), Tokens::ui(10.0), ed.t.text);
    }
    for (k, pid) in ["xo1", "xo2", "xo3"].iter().enumerate() {
        let x = edges[k + 1];
        let hr = Rect::from_center_size(pos2(x, r.center().y), vec2(10.0, r.height()));
        let resp = ui.interact(hr, egui::Id::new(("mb-xo", ed.target.clone(), k)), Sense::drag());
        ui.painter()
            .line_segment([pos2(x, r.min.y), pos2(x, r.max.y)], Stroke::new(if resp.hovered() || resp.dragged() { 2.5 } else { 1.5 }, ed.t.crossover_handle));
        ui.painter().text(pos2(x + 3.0, r.min.y + 2.0), Align2::LEFT_TOP, format!("{:.0} Hz", xo[k]), Tokens::ui(9.0), ed.t.text_dim);
        ed.auto(format!("fxEditor.{}.{pid}", ed.fx), hr, "Crossover");
        if resp.dragged()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let (lo, hi, _) = ed.range(pid);
            ed.drafts.insert(pid.to_string(), fx_of(pos.x, r).clamp(lo, hi).round());
        }
        if resp.drag_stopped() {
            let v = ed.v(pid);
            ed.commit(pid, v);
        }
    }
    ui.add_space(4.0);
    ui.horizontal_top(|ui| {
        for b in 1..=4 {
            ui.vertical(|ui| {
                ui.set_width(150.0);
                ui.label(egui::RichText::new(tlf!("Band {n}", n = b)).strong());
                transfer_plot(ui, ed, 110.0, b - 1, format!("fxEditor.{}.curve.{b}", ed.fx));
                ui.horizontal(|ui| {
                    ed.toggle(ui, &format!("b{b}_solo"), tl!("Solo"));
                    ed.toggle(ui, &format!("b{b}_bypass"), tl!("Bypass"));
                });
                ed.slider(ui, &format!("b{b}_threshold"), tl!("Thr"), false);
                ed.slider(ui, &format!("b{b}_ratio"), tl!("Ratio"), false);
                ed.slider(ui, &format!("b{b}_attack"), tl!("Att"), true);
                ed.slider(ui, &format!("b{b}_release"), tl!("Rel"), true);
                ed.slider(ui, &format!("b{b}_gain"), tl!("Gain"), false);
            });
        }
    });
    ui.separator();
    ui.horizontal(|ui| {
        ed.slider(ui, "output", tl!("Output Gain"), false);
        ed.toggle(ui, "lim_on", tl!("Limiter"));
        ed.toggle(ui, "link", tl!("Link Channels"));
    });
    ui.horizontal(|ui| {
        ed.slider(ui, "lim_threshold", tl!("Limiter Threshold"), false);
        ed.slider(ui, "lim_release", tl!("Limiter Release"), true);
    });
}

// ------------------------------------------------------------------------------- dynamics

fn dynamics(ui: &mut egui::Ui, ed: &mut Ed) {
    ui.horizontal_top(|ui| {
        transfer_plot(ui, ed, 220.0, 0, format!("fxEditor.{}.curve", ed.fx));
        ui.vertical(|ui| {
            ui.set_width(330.0);
            ed.toggle(ui, "gate_on", tl!("Auto Gate"));
            ed.slider(ui, "gate_threshold", tl!("Threshold"), false);
            ed.slider(ui, "gate_attack", tl!("Attack"), true);
            ed.slider(ui, "gate_release", tl!("Release"), true);
            ed.slider(ui, "gate_hold", tl!("Hold"), false);
            ui.separator();
            ed.toggle(ui, "comp_on", tl!("Compressor"));
            ed.slider(ui, "comp_threshold", tl!("Threshold"), false);
            ed.slider(ui, "comp_ratio", tl!("Ratio"), false);
            ed.slider(ui, "comp_attack", tl!("Attack"), true);
            ed.slider(ui, "comp_release", tl!("Release"), true);
            ui.horizontal(|ui| {
                ed.toggle(ui, "comp_auto", tl!("Auto Makeup"));
            });
            ed.slider(ui, "comp_makeup", tl!("Makeup"), false);
        });
        ui.vertical(|ui| {
            ui.set_width(330.0);
            ed.toggle(ui, "exp_on", tl!("Expander"));
            ed.slider(ui, "exp_threshold", tl!("Threshold"), false);
            ed.slider(ui, "exp_ratio", tl!("Ratio"), false);
            ui.separator();
            ed.toggle(ui, "lim_on", tl!("Limiter"));
            ed.slider(ui, "lim_threshold", tl!("Threshold"), false);
            ed.slider(ui, "lim_release", tl!("Release"), true);
            ed.toggle(ui, "soft_clip", tl!("Soft Clip"));
            ui.separator();
            ed.slider(ui, "output", tl!("Output Gain"), false);
        });
    });
}
