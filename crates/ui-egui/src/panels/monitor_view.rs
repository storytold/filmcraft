//! Monitor view options (the View menu and the monitors' wrench menus): Playback / Paused
//! Resolution, High Quality Playback, Display Mode (Composite Video, Alpha, Red, Green, Blue,
//! Multi-Camera, Audio Waveform, Comparison View, Video and Audio Waveform Split), Magnification
//! (Fit, 10%…1600%, scroll or Hand-tool panning), rulers, guides (dragged out of the rulers, Add
//! Guide…, Clear Guides, Lock Guides, templates saved in the user preferences) and Snap in Program
//! Monitor (graphic drags snap to guides, the frame edges and centre).
//!
//! Everything is [`MonitorView`] state (serde: `ui.inspect`, `ui.set {"program": {…}}`) changed
//! through the `view.*` UI commands, which [`route`] handles for `menus::invoke`. Commands act on
//! `params.monitor` ("program" / "source"), else on the focused monitor (Program by default).
//!
//! Automation ids: `<monitor>.zoom`, `<monitor>.zoom.<fit|10|…|1600>`,
//! `<monitor>.settings.<command suffix>` (wrench menu, e.g. `program.settings.display.alpha`),
//! `<monitor>.ruler.top`, `<monitor>.ruler.left`, `<monitor>.guide.<n>`,
//! `program.compare.<prev|next|set>`, `source.waveform`, and in the dialogs `guides.add.*`,
//! `guides.save.*`, `guides.manage.*`.

use egui::{Align2, Color32, Rect, Sense, Stroke, Vec2, pos2, vec2};
use filmcraft_project::ItemId;
use filmcraft_time::{Tick, TimeDisplay, format_time};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::frames::Rgba;
use crate::panels::monitor::Which;
use crate::state::{DisplayMode, Guide, GuideDialog, MonitorView, PlaybackRes, Tool};
use crate::theme::Tokens;

/// Magnification levels (View ▸ Magnification and the monitor zoom menu).
pub const ZOOMS: [(&str, f32); 10] =
    [("10", 0.1), ("25", 0.25), ("50", 0.5), ("75", 0.75), ("100", 1.0), ("150", 1.5), ("200", 2.0), ("400", 4.0), ("800", 8.0), ("1600", 16.0)];
/// Ruler thickness (points).
pub const RULER: f32 = 16.0;
const GUIDE_COLOR: Color32 = Color32::from_rgb(0x3d, 0xa5, 0xff);
const SNAP_COLOR: Color32 = Color32::from_rgb(0xff, 0x4f, 0xd8);
/// Snap distance (points).
pub const SNAP_PX: f32 = 6.0;

const RES_NAMES: [(&str, PlaybackRes); 5] = [
    ("full", PlaybackRes::Full),
    ("half", PlaybackRes::Half),
    ("quarter", PlaybackRes::Quarter),
    ("eighth", PlaybackRes::Eighth),
    ("sixteenth", PlaybackRes::Sixteenth),
];

const DISPLAY_NAMES: [(&str, DisplayMode); 8] = [
    ("composite", DisplayMode::Composite),
    ("alpha", DisplayMode::Alpha),
    ("red", DisplayMode::Red),
    ("green", DisplayMode::Green),
    ("blue", DisplayMode::Blue),
    ("audioWaveform", DisplayMode::AudioWaveform),
    ("comparison", DisplayMode::Comparison),
    ("videoAndWaveform", DisplayMode::VideoAndWaveform),
];

pub fn prefix(w: Which) -> &'static str {
    if w == Which::Program { "program" } else { "source" }
}

pub fn view(app: &FilmcraftApp, w: Which) -> &MonitorView {
    if w == Which::Program { &app.ui.program } else { &app.ui.source }
}

pub fn view_mut(app: &mut FilmcraftApp, w: Which) -> &mut MonitorView {
    if w == Which::Program { &mut app.ui.program } else { &mut app.ui.source }
}

/// The monitor a `view.*` command acts on: `params.monitor`, else the focused monitor.
fn which_of(app: &FilmcraftApp, params: &Value) -> Which {
    match params.get("monitor").and_then(Value::as_str) {
        Some("source") => Which::Source,
        Some("program") => Which::Program,
        _ if app.ui.focused == PanelKind::Source => Which::Source,
        _ => Which::Program,
    }
}

fn toggled(cur: bool, params: &Value) -> bool {
    params.get("enabled").and_then(Value::as_bool).unwrap_or(!cur)
}

/// Frame size of a monitor's picture (sequence or source item), if it shows video.
fn frame_size(app: &FilmcraftApp, w: Which) -> Option<(u32, u32)> {
    match w {
        Which::Program => app.session.active_sequence().map(|q| (q.settings.width, q.settings.height)),
        Which::Source => {
            let pi = app.session.project.item(app.session.state.source_item?)?;
            match &pi.kind {
                filmcraft_project::ItemKind::Media(m) => m.info.video.as_ref().map(|v| (v.width, v.height)),
                filmcraft_project::ItemKind::Sequence(s) => Some((s.settings.width, s.settings.height)),
                _ => Some((1920, 1080)),
            }
        }
    }
}

/// Width over height of one pixel of a monitor's picture: the active sequence's pixel aspect
/// ratio (Program) or the Source item's (Interpret Footage first). Monitors show the picture at
/// its display aspect, so a 1440 x 1080 frame with 4:3 pixels is drawn 16:9.
pub fn pixel_aspect(app: &FilmcraftApp, w: Which) -> f64 {
    let par = match w {
        Which::Program => app.session.active_sequence().map(|q| q.settings.par),
        Which::Source => app.session.state.source_item.and_then(|i| app.session.project.source_par(i)),
    };
    par.map_or(1.0, filmcraft_project::par_ratio)
}

/// Handle a `view.*` command (None for other ids).
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let rest = id.strip_prefix("view.")?;
    let w = which_of(app, params);
    let pfx = prefix(w);
    if let Some(r) = rest.strip_prefix("playbackRes.") {
        let (_, res) = RES_NAMES.iter().find(|(n, _)| *n == r)?;
        view_mut(app, w).res = *res;
        return Some(Ok(json!({"monitor": pfx, "playbackRes": res.label()})));
    }
    if let Some(r) = rest.strip_prefix("pausedRes.") {
        let (_, res) = RES_NAMES.iter().find(|(n, _)| *n == r)?;
        view_mut(app, w).paused_res = *res;
        return Some(Ok(json!({"monitor": pfx, "pausedRes": res.label()})));
    }
    if let Some(d) = rest.strip_prefix("display.") {
        return Some(set_display(app, w, d));
    }
    if let Some(z) = rest.strip_prefix("magnification.") {
        let zoom = if z == "fit" { None } else { Some(ZOOMS.iter().find(|(n, _)| *n == z)?.1) };
        let v = view_mut(app, w);
        v.zoom = zoom;
        v.pan = [0.0, 0.0];
        return Some(Ok(json!({"monitor": pfx, "zoom": zoom})));
    }
    let r = match rest {
        "highQualityPlayback" => {
            let v = view_mut(app, w);
            v.high_quality = toggled(v.high_quality, params);
            Ok(json!({"monitor": pfx, "highQuality": v.high_quality}))
        }
        "safeMargins" => {
            let v = view_mut(app, w);
            v.safe_margins = toggled(v.safe_margins, params);
            Ok(json!({"monitor": pfx, "safeMargins": v.safe_margins}))
        }
        "showRulers" => {
            let v = view_mut(app, w);
            v.show_rulers = toggled(v.show_rulers, params);
            Ok(json!({"monitor": pfx, "showRulers": v.show_rulers}))
        }
        "showGuides" => {
            let v = view_mut(app, w);
            v.show_guides = toggled(v.show_guides, params);
            Ok(json!({"monitor": pfx, "showGuides": v.show_guides}))
        }
        "lockGuides" => {
            let v = view_mut(app, w);
            v.lock_guides = toggled(v.lock_guides, params);
            Ok(json!({"monitor": pfx, "lockGuides": v.lock_guides}))
        }
        "snapInProgramMonitor" => {
            let v = &mut app.ui.program;
            v.snap = toggled(v.snap, params);
            Ok(json!({"snap": v.snap}))
        }
        "clearGuides" => {
            let v = view_mut(app, w);
            let n = v.guides.len();
            v.guides.clear();
            Ok(json!({"monitor": pfx, "cleared": n}))
        }
        "addGuide" => match params.get("position").and_then(Value::as_f64) {
            Some(pos) => {
                let vertical = params.get("orientation").and_then(Value::as_str) != Some("horizontal");
                let v = view_mut(app, w);
                v.guides.push(Guide { vertical, position: pos });
                v.show_guides = true;
                Ok(json!({"monitor": pfx, "guides": v.guides.len()}))
            }
            None => {
                let mid = frame_size(app, w).map_or(960.0, |f| (f.0 / 2) as f64);
                app.ui.guide_dialog = Some(GuideDialog::Add { vertical: true, position: mid, source: w == Which::Source });
                Ok(json!({"dialog": "addGuide"}))
            }
        },
        "guideTemplates.save" => match params.get("name").and_then(Value::as_str) {
            Some(name) => save_template(app, w, name),
            None => {
                if view(app, w).guides.is_empty() {
                    return Some(Err("there are no guides to save".into()));
                }
                app.ui.guide_dialog = Some(GuideDialog::SaveTemplate { name: tl!("Guides").into(), source: w == Which::Source });
                Ok(json!({"dialog": "saveGuides"}))
            }
        },
        "guideTemplates.manage" => {
            app.ui.guide_dialog = Some(GuideDialog::Manage { selected: None, source: w == Which::Source });
            Ok(json!({"dialog": "manageGuides", "templates": app.session.prefs.guides.templates.iter().map(|t| t.name.clone()).collect::<Vec<_>>()}))
        }
        "guideTemplates.apply" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            match app.session.prefs.guides.templates.iter().find(|t| t.name == name) {
                Some(t) => {
                    let g = t.guides.clone();
                    let v = view_mut(app, w);
                    v.guides = g;
                    v.show_guides = true;
                    Ok(json!({"monitor": pfx, "guides": v.guides.len()}))
                }
                None => Err(format!("no guide template `{name}`")),
            }
        }
        "guideTemplates.delete" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let mut next = app.session.prefs.clone();
            let before = next.guides.templates.len();
            next.guides.templates.retain(|t| t.name != name);
            if next.guides.templates.len() == before {
                Err(format!("no guide template `{name}`"))
            } else {
                app.session.set_prefs(next).map(|_| json!({"deleted": name})).map_err(|e| format!("saving preferences: {e}"))
            }
        }
        "compare.setReference" => {
            let t = params
                .get("time")
                .and_then(Value::as_i64)
                .map(Tick)
                .or_else(|| params.get("seconds").and_then(Value::as_f64).map(Tick::from_seconds_f64))
                .unwrap_or_else(|| app.session.playhead());
            app.ui.program.compare_ref = Some(t.0);
            Ok(json!({"reference": t.0}))
        }
        _ => return None,
    };
    Some(r)
}

fn set_display(app: &mut FilmcraftApp, w: Which, d: &str) -> Result<Value, String> {
    if d == "multicam" {
        app.ui.program.multicam = true;
        return Ok(json!({"monitor": "program", "display": "multicam"}));
    }
    let (_, mode) = DISPLAY_NAMES.iter().find(|(n, _)| *n == d).ok_or_else(|| format!("unknown display mode `{d}`"))?;
    // Audio Waveform and the split view are Source Monitor modes, Comparison is a Program mode.
    let w = match mode {
        DisplayMode::AudioWaveform | DisplayMode::VideoAndWaveform => Which::Source,
        DisplayMode::Comparison => Which::Program,
        _ => w,
    };
    let ph = app.session.playhead();
    let v = view_mut(app, w);
    v.display = *mode;
    if w == Which::Program {
        v.multicam = false;
    }
    if *mode == DisplayMode::Comparison && v.compare_ref.is_none() {
        v.compare_ref = Some(ph.0);
    }
    Ok(json!({"monitor": prefix(w), "display": d}))
}

fn save_template(app: &mut FilmcraftApp, w: Which, name: &str) -> Result<Value, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("the template needs a name".into());
    }
    let guides = view(app, w).guides.clone();
    let mut next = app.session.prefs.clone();
    next.guides.templates.retain(|t| t.name != name);
    next.guides.templates.push(filmcraft_engine::autosave::GuideTemplate { name: name.to_string(), guides });
    app.session.set_prefs(next).map_err(|e| format!("saving preferences: {e}"))?;
    Ok(json!({"saved": name}))
}

/// Menu checkmark state of a `view.*` command (None = not a toggle / radio item).
pub fn checked(app: &FilmcraftApp, id: &str) -> Option<bool> {
    let rest = id.strip_prefix("view.")?;
    let w = which_of(app, &Value::Null);
    let v = view(app, w);
    if let Some(r) = rest.strip_prefix("playbackRes.") {
        return RES_NAMES.iter().find(|(n, _)| *n == r).map(|(_, x)| *x == v.res);
    }
    if let Some(r) = rest.strip_prefix("pausedRes.") {
        return RES_NAMES.iter().find(|(n, _)| *n == r).map(|(_, x)| *x == v.paused_res);
    }
    if let Some(d) = rest.strip_prefix("display.") {
        if d == "multicam" {
            return Some(app.ui.program.multicam);
        }
        let (_, mode) = DISPLAY_NAMES.iter().find(|(n, _)| *n == d)?;
        let v = match mode {
            DisplayMode::AudioWaveform | DisplayMode::VideoAndWaveform => &app.ui.source,
            DisplayMode::Comparison => &app.ui.program,
            _ => v,
        };
        return Some(v.display_mode() == Some(*mode));
    }
    if let Some(z) = rest.strip_prefix("magnification.") {
        return Some(if z == "fit" { v.zoom.is_none() } else { ZOOMS.iter().find(|(n, _)| *n == z).is_some_and(|(_, f)| v.zoom == Some(*f)) });
    }
    match rest {
        "highQualityPlayback" => Some(v.high_quality),
        "safeMargins" => Some(v.safe_margins),
        "showRulers" => Some(v.show_rulers),
        "showGuides" => Some(v.show_guides),
        "lockGuides" => Some(v.lock_guides),
        "snapInProgramMonitor" => Some(app.ui.program.snap),
        _ => None,
    }
}

/// Menu enablement of a `view.*` command.
pub fn enabled(app: &FilmcraftApp, id: &str) -> bool {
    let v = view(app, which_of(app, &Value::Null));
    match id {
        "view.lockGuides" | "view.clearGuides" | "view.guideTemplates.save" => !v.guides.is_empty(),
        _ => true,
    }
}

/// A channel of a frame as greyscale (Alpha, Red, Green, Blue display modes); colour channels are
/// shown premultiplied, so transparent areas are black.
pub fn channel_view(img: &Rgba, mode: DisplayMode) -> Rgba {
    let mut px = Vec::with_capacity(img.px.len());
    for p in img.px.as_chunks::<4>().0 {
        let a = p[3] as u32;
        let v = match mode {
            DisplayMode::Alpha => p[3],
            DisplayMode::Red => (p[0] as u32 * a / 255) as u8,
            DisplayMode::Green => (p[1] as u32 * a / 255) as u8,
            DisplayMode::Blue => (p[2] as u32 * a / 255) as u8,
            _ => {
                px.extend_from_slice(p);
                continue;
            }
        };
        px.extend_from_slice(&[v, v, v, 255]);
    }
    Rgba { w: img.w, h: img.h, px }
}

/// The on-screen picture rect: fitted, or magnified about the area centre plus the (clamped) pan.
pub fn picture_rect(area: Rect, fw: f32, fh: f32, v: &MonitorView, ppp: f32) -> Rect {
    match v.zoom {
        None => crate::panels::monitor::fit(area, fw, fh),
        Some(z) => {
            let size = vec2(fw * z / ppp.max(0.1), fh * z / ppp.max(0.1));
            let max = ((size - area.size()) / 2.0).max(Vec2::ZERO);
            let pan = vec2(v.pan[0].clamp(-max.x, max.x), v.pan[1].clamp(-max.y, max.y));
            Rect::from_center_size(area.center() + pan, size)
        }
    }
}

/// Pan a magnified picture: scroll over it, or drag with the Hand tool.
pub fn pan_input(app: &mut FilmcraftApp, ui: &mut egui::Ui, w: Which, area: Rect, pic: Rect) {
    if view(app, w).zoom.is_none() {
        return;
    }
    let mut d = Vec2::ZERO;
    if ui.rect_contains_pointer(area) {
        d += ui.input(|i| i.smooth_scroll_delta);
    }
    if app.ui.tool == Tool::Hand {
        let resp = ui.interact(area, egui::Id::new((prefix(w), "pan")), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
        }
        d += resp.drag_delta();
    }
    if d != Vec2::ZERO {
        let max = ((pic.size() - area.size()) / 2.0).max(Vec2::ZERO);
        let v = view_mut(app, w);
        v.pan = [(v.pan[0] + d.x).clamp(-max.x, max.x), (v.pan[1] + d.y).clamp(-max.y, max.y)];
    }
}

/// Snap one axis: the moving item's (start, centre, end) against `targets`; returns
/// (delta, target) of the closest pair within `thr`.
pub fn snap_axis(edges: [f32; 3], targets: &[f32], thr: f32) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    for e in edges {
        for &t in targets {
            let d = t - e;
            if d.abs() <= thr && best.is_none_or(|(b, _)| d.abs() < b.abs()) {
                best = Some((d, t));
            }
        }
    }
    best
}

/// Snap in Program Monitor: adjust a drag offset so the moved box's edges or centre meet the
/// frame edges / centre or a guide. Returns the offset and the lines snapped to (vertical?, coord).
pub fn snap_move(app: &FilmcraftApp, pic: Rect, frame: (u32, u32), moving: Rect, off: Vec2) -> (Vec2, Vec<(bool, f32)>) {
    let v = &app.ui.program;
    if !v.snap {
        return (off, Vec::new());
    }
    let (kx, ky) = (pic.width() / frame.0.max(1) as f32, pic.height() / frame.1.max(1) as f32);
    let mut tx = vec![pic.min.x, pic.center().x, pic.max.x];
    let mut ty = vec![pic.min.y, pic.center().y, pic.max.y];
    if v.show_guides {
        for g in &v.guides {
            if g.vertical {
                tx.push(pic.min.x + g.position as f32 * kx);
            } else {
                ty.push(pic.min.y + g.position as f32 * ky);
            }
        }
    }
    let m = moving.translate(off);
    let mut off = off;
    let mut lines = Vec::new();
    if let Some((d, t)) = snap_axis([m.min.x, m.center().x, m.max.x], &tx, SNAP_PX) {
        off.x += d;
        lines.push((true, t));
    }
    if let Some((d, t)) = snap_axis([m.min.y, m.center().y, m.max.y], &ty, SNAP_PX) {
        off.y += d;
        lines.push((false, t));
    }
    (off, lines)
}

pub fn draw_snap_lines(p: &egui::Painter, pic: Rect, lines: &[(bool, f32)]) {
    for &(vertical, c) in lines {
        let seg = if vertical { [pos2(c, pic.min.y), pos2(c, pic.max.y)] } else { [pos2(pic.min.x, c), pos2(pic.max.x, c)] };
        p.line_segment(seg, Stroke::new(1.0, SNAP_COLOR));
    }
}

fn nice_step(px_per_unit: f32) -> f32 {
    [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0].into_iter().find(|s| s * px_per_unit >= 50.0).unwrap_or(5000.0)
}

#[derive(Clone, Copy)]
struct NewGuide {
    vertical: bool,
}

/// Rulers (frame pixels) along the top and left of the picture area; drag out of a ruler to add a
/// guide (top ruler → horizontal guide, left ruler → vertical guide).
#[allow(clippy::too_many_arguments)]
pub fn rulers(app: &mut FilmcraftApp, ui: &mut egui::Ui, w: Which, top: Rect, left: Rect, area: Rect, pic: Rect, frame: (u32, u32)) {
    let t = app.tokens;
    let pfx = prefix(w);
    let p = ui.painter().clone();
    for r in [top, left, Rect::from_min_max(left.min - vec2(0.0, RULER), top.min + vec2(0.0, RULER))] {
        p.rect_filled(r, 0.0, t.tl_ruler_bg);
    }
    let (kx, ky) = (pic.width() / frame.0.max(1) as f32, pic.height() / frame.1.max(1) as f32);
    for (r, vertical, k, o) in [(top, false, kx, pic.min.x), (left, true, ky, pic.min.y)] {
        let cp = p.with_clip_rect(r);
        let step = nice_step(k);
        let (lo, hi) = if vertical { (r.min.y, r.max.y) } else { (r.min.x, r.max.x) };
        let first = ((lo - o) / k / step * 5.0).floor() as i64;
        let last = ((hi - o) / k / step * 5.0).ceil() as i64;
        for i in first..=last {
            let v = i as f32 * step / 5.0;
            let s = o + v * k;
            let major = i % 5 == 0;
            let len = if major { RULER * 0.6 } else { RULER * 0.25 };
            if vertical {
                cp.line_segment([pos2(r.max.x - len, s), pos2(r.max.x, s)], Stroke::new(1.0, t.tl_ruler_tick));
                if major {
                    cp.text(pos2(r.min.x + 1.0, s + 2.0), Align2::LEFT_TOP, format!("{}", v as i64), Tokens::ui(8.0), t.tl_ruler_text);
                }
            } else {
                cp.line_segment([pos2(s, r.max.y - len), pos2(s, r.max.y)], Stroke::new(1.0, t.tl_ruler_tick));
                if major {
                    cp.text(pos2(s + 2.0, r.min.y + 1.0), Align2::LEFT_TOP, format!("{}", v as i64), Tokens::ui(8.0), t.tl_ruler_text);
                }
            }
        }
    }
    let drag_id = egui::Id::new((pfx, "new-guide"));
    for (r, name, vertical) in [(top, "top", false), (left, "left", true)] {
        let resp = ui.interact(r, egui::Id::new((pfx, "ruler", name)), Sense::drag());
        app.auto.add(&format!("{pfx}.ruler.{name}"), r, if vertical { "left ruler" } else { "top ruler" });
        if resp.hovered() {
            ui.ctx().set_cursor_icon(if vertical { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
        }
        if resp.drag_started() {
            ui.data_mut(|d| d.insert_temp(drag_id, NewGuide { vertical }));
        }
        let Some(ng) = ui.data(|d| d.get_temp::<NewGuide>(drag_id)).filter(|g| g.vertical == vertical) else { continue };
        let Some(pos) = resp.interact_pointer_pos().or(ui.ctx().pointer_latest_pos()) else { continue };
        if area.contains(pos) {
            let seg = if ng.vertical { [pos2(pos.x, area.min.y), pos2(pos.x, area.max.y)] } else { [pos2(area.min.x, pos.y), pos2(area.max.x, pos.y)] };
            ui.painter().with_clip_rect(area).line_segment(seg, Stroke::new(1.0, GUIDE_COLOR));
        }
        if resp.drag_stopped() {
            ui.data_mut(|d| d.remove::<NewGuide>(drag_id));
            if area.contains(pos) {
                let position = if ng.vertical { ((pos.x - pic.min.x) / kx).round() } else { ((pos.y - pic.min.y) / ky).round() } as f64;
                let v = view_mut(app, w);
                v.guides.push(Guide { vertical: ng.vertical, position });
                v.show_guides = true;
            }
        }
    }
}

/// Draw the guides; drag an unlocked guide to move it, or back onto a ruler (out of the picture
/// area) to remove it.
pub fn guides(app: &mut FilmcraftApp, ui: &mut egui::Ui, w: Which, area: Rect, pic: Rect, frame: (u32, u32)) {
    let v = view(app, w).clone();
    if !v.show_guides || v.guides.is_empty() {
        return;
    }
    let pfx = prefix(w);
    let (kx, ky) = (pic.width() / frame.0.max(1) as f32, pic.height() / frame.1.max(1) as f32);
    let p = ui.painter().with_clip_rect(area);
    let mut remove = None;
    for (i, g) in v.guides.iter().enumerate() {
        let c = if g.vertical { pic.min.x + g.position as f32 * kx } else { pic.min.y + g.position as f32 * ky };
        let (seg, hit) = if g.vertical {
            ([pos2(c, area.min.y), pos2(c, area.max.y)], Rect::from_min_max(pos2(c - 3.0, area.min.y), pos2(c + 3.0, area.max.y)))
        } else {
            ([pos2(area.min.x, c), pos2(area.max.x, c)], Rect::from_min_max(pos2(area.min.x, c - 3.0), pos2(area.max.x, c + 3.0)))
        };
        p.line_segment(seg, Stroke::new(1.0, GUIDE_COLOR));
        let hit = hit.intersect(area);
        app.auto.add(&format!("{pfx}.guide.{i}"), hit, if g.vertical { "vertical guide" } else { "horizontal guide" });
        if v.lock_guides {
            continue;
        }
        let resp = ui.interact(hit, egui::Id::new((pfx, "guide", i)), Sense::drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(if g.vertical { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
        }
        if let Some(pos) = resp.interact_pointer_pos()
            && (resp.dragged() || resp.drag_stopped())
        {
            if !area.contains(pos) {
                // outside the picture area: dropping here removes the guide
                if resp.drag_stopped() {
                    remove = Some(i);
                }
            } else {
                let position = if g.vertical { ((pos.x - pic.min.x) / kx).round() } else { ((pos.y - pic.min.y) / ky).round() } as f64;
                view_mut(app, w).guides[i].position = position;
            }
        }
    }
    if let Some(i) = remove {
        view_mut(app, w).guides.remove(i);
    }
}

/// The Source Monitor's audio waveform (Audio Waveform display mode, audio-only clips and the
/// split view): click to move the source playhead.
#[allow(clippy::too_many_arguments)]
pub fn waveform(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    area: Rect,
    item: ItemId,
    time: Tick,
    duration: Tick,
    mark_in: Option<Tick>,
    mark_out: Option<Tick>,
) {
    let t = app.tokens;
    let p = ui.painter().with_clip_rect(area);
    p.rect_filled(area, 0.0, t.monitor_bg);
    let dur = duration.0.max(1) as f64;
    let xof = |tk: Tick| area.min.x + ((tk.0 as f64 / dur) as f32).clamp(0.0, 1.0) * area.width();
    if mark_in.is_some() || mark_out.is_some() {
        let (a, b) = (xof(mark_in.unwrap_or(Tick::ZERO)), xof(mark_out.unwrap_or(duration)));
        p.rect_filled(Rect::from_min_max(pos2(a, area.min.y), pos2(b, area.max.y)), 0.0, Color32::from_white_alpha(14));
    }
    let mid = area.center().y;
    p.line_segment([pos2(area.min.x, mid), pos2(area.max.x, mid)], Stroke::new(1.0, Color32::from_white_alpha(30)));
    match crate::panels::timeline::request_peaks(app, item, 0) {
        Some(peaks) if !peaks.is_empty() => {
            let half = area.height() * 0.45;
            let spp = 256.0 * TICKS_PER_SAMPLE_48K;
            let mut mesh = egui::Mesh::default();
            let col = Color32::from_rgb(0x4f, 0xc3, 0x7a);
            for x in (area.min.x.floor() as i32)..(area.max.x.ceil() as i32) {
                let f0 = (x as f32 - area.min.x) / area.width();
                let f1 = (x as f32 + 1.0 - area.min.x) / area.width();
                let s0 = (dur * f0 as f64 / spp) as usize;
                let s1 = ((dur * f1 as f64 / spp) as usize).max(s0 + 1);
                let (lo, hi) = peaks.iter().skip(s0).take(s1 - s0).fold((0f32, 0f32), |(l, h), (a, b)| (l.min(*a), h.max(*b)));
                if hi - lo > 0.002 {
                    mesh.add_colored_rect(
                        Rect::from_min_max(pos2(x as f32, mid - hi.min(1.0) * half), pos2(x as f32 + 1.0, mid - lo.max(-1.0) * half + 1.0)),
                        col,
                    );
                }
            }
            p.add(mesh);
        }
        Some(_) => {
            p.text(area.center(), Align2::CENTER_CENTER, tl!("(no audio)"), Tokens::ui(12.0), t.text_dim);
        }
        None => {
            p.text(area.center(), Align2::CENTER_CENTER, tl!("Generating waveform…"), Tokens::ui(12.0), t.text_dim);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    let x = xof(time);
    p.line_segment([pos2(x, area.min.y), pos2(x, area.max.y)], Stroke::new(1.0, t.playhead));
    let resp = ui.interact(area, egui::Id::new("source-waveform"), Sense::click_and_drag());
    app.auto.add("source.waveform", area, "audio waveform");
    // Horizontal waveform drags scrub; dragging vertically lifts the marked audio to the timeline.
    let delta = resp.drag_delta();
    if resp.dragged() && delta.y.abs() > delta.x.abs().max(6.0) && crate::panels::dragged_source(ui).is_none() {
        crate::panels::source_drag::begin(app, ui, false, true);
    }
    if (resp.clicked() || resp.dragged())
        && crate::panels::dragged_source(ui).is_none()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let f = ((pos.x - area.min.x) / area.width()).clamp(0.0, 1.0) as f64;
        let _ = app.session.execute("source.setPlayhead", json!({"time": (f * dur) as i64}));
    }
}

/// Ticks per 48 kHz sample (the waveform peaks are 256-sample min/max pairs at 48 kHz).
const TICKS_PER_SAMPLE_48K: f64 = filmcraft_time::TICKS_PER_SECOND as f64 / 48_000.0;

/// Comparison View: the reference side's label, timecode and step / set buttons.
pub fn compare_bar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, rate: filmcraft_time::FrameRate, drop_frame: bool) {
    let t = app.tokens;
    let rf = Tick(app.ui.program.compare_ref.unwrap_or(app.session.playhead().0));
    let tc = format_time(rf, rate, drop_frame, TimeDisplay::Timecode, 48000);
    ui.painter().text(pos2(bar.min.x + 4.0, bar.center().y), Align2::LEFT_CENTER, tlf!("Reference  {tc}", tc), Tokens::ui(11.0), t.text_dim);
    let mut x = bar.max.x;
    for (key, label, tip) in [
        ("set", tl!("Set"), tl!("Set the reference to the playhead")),
        ("next", "▶", tl!("Reference: next frame")),
        ("prev", "◀", tl!("Reference: previous frame")),
    ] {
        let r = Rect::from_min_max(pos2(x - 34.0, bar.min.y + 1.0), pos2(x - 2.0, bar.max.y - 1.0));
        x -= 34.0;
        let resp = ui.interact(r, egui::Id::new(("compare", key)), Sense::click()).on_hover_text(tip);
        ui.painter().rect_filled(r, 3.0, if resp.hovered() { t.hover } else { t.field_bg });
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(10.0), t.text);
        app.auto.add(&format!("program.compare.{key}"), r, tip);
        if resp.clicked() {
            let nt = match key {
                "set" => app.session.playhead(),
                "next" => rf + rate.frame_duration(),
                _ => (rf - rate.frame_duration()).max(Tick::ZERO),
            };
            app.ui.program.compare_ref = Some(nt.0);
        }
    }
}

type Picks = Vec<(String, Rect, String, bool)>;

fn pick(ui: &mut egui::Ui, out: &mut Picks, key: &str, label: &str, on: bool) {
    let r = ui.selectable_label(on, label);
    out.push((key.to_string(), r.rect, label.to_string(), r.clicked()));
}

/// The view items of a monitor's wrench menu (display modes, resolutions, rulers, guides, snap).
pub fn wrench_items(app: &mut FilmcraftApp, ui: &mut egui::Ui, w: Which) {
    let v = view(app, w).clone();
    let mode = v.display_mode();
    let mut out: Picks = Vec::new();
    pick(ui, &mut out, "display.composite", tl!("Composite Video"), mode == Some(DisplayMode::Composite));
    ui.menu_button(tl!("RGBA Channels"), |ui| {
        for (k, l, m) in [
            ("alpha", tl!("Alpha"), DisplayMode::Alpha),
            ("red", tl!("Red"), DisplayMode::Red),
            ("green", tl!("Green"), DisplayMode::Green),
            ("blue", tl!("Blue"), DisplayMode::Blue),
        ] {
            pick(ui, &mut out, &format!("display.{k}"), l, mode == Some(m));
        }
    });
    if w == Which::Program {
        pick(ui, &mut out, "display.multicam", tl!("Multi-Camera"), v.multicam);
        pick(ui, &mut out, "display.comparison", tl!("Comparison View"), mode == Some(DisplayMode::Comparison));
    } else {
        pick(ui, &mut out, "display.audioWaveform", tl!("Audio Waveform"), mode == Some(DisplayMode::AudioWaveform));
        pick(ui, &mut out, "display.videoAndWaveform", tl!("Video and Audio Waveform Split"), mode == Some(DisplayMode::VideoAndWaveform));
    }
    ui.separator();
    ui.menu_button(tl!("Playback Resolution"), |ui| {
        for (k, r) in RES_NAMES {
            pick(ui, &mut out, &format!("playbackRes.{k}"), crate::i18n::t(r.label()), v.res == r);
        }
    });
    ui.menu_button(tl!("Paused Resolution"), |ui| {
        for (k, r) in RES_NAMES {
            pick(ui, &mut out, &format!("pausedRes.{k}"), crate::i18n::t(r.label()), v.paused_res == r);
        }
    });
    pick(ui, &mut out, "highQualityPlayback", tl!("High Quality Playback"), v.high_quality);
    ui.separator();
    pick(ui, &mut out, "showRulers", tl!("Show Rulers"), v.show_rulers);
    pick(ui, &mut out, "showGuides", tl!("Show Guides"), v.show_guides);
    if !v.guides.is_empty() {
        pick(ui, &mut out, "lockGuides", tl!("Lock Guides"), v.lock_guides);
        pick(ui, &mut out, "clearGuides", tl!("Clear Guides"), false);
    }
    if w == Which::Program {
        pick(ui, &mut out, "snapInProgramMonitor", tl!("Snap in Program Monitor"), v.snap);
    }
    ui.separator();
    let pfx = prefix(w);
    for (key, r, label, clicked) in out {
        app.auto.add(&format!("{pfx}.settings.{key}"), r, &label);
        if clicked && let Some(Err(e)) = route(app, &format!("view.{key}"), &json!({"monitor": pfx})) {
            app.ui.status = e;
        }
    }
}

/// The magnification popup of a monitor's zoom dropdown.
pub fn zoom_menu(app: &mut FilmcraftApp, resp: &egui::Response, w: Which) {
    let zoom = view(app, w).zoom;
    let mut out: Picks = Vec::new();
    egui::Popup::menu(resp).show(|ui| {
        ui.set_min_width(80.0);
        pick(ui, &mut out, "fit", tl!("Fit"), zoom.is_none());
        ui.separator();
        for (k, z) in ZOOMS {
            pick(ui, &mut out, k, &format!("{k}%"), zoom == Some(z));
        }
    });
    let pfx = prefix(w);
    for (key, r, label, clicked) in out {
        app.auto.add(&format!("{pfx}.zoom.{key}"), r, &label);
        if clicked {
            let _ = route(app, &format!("view.magnification.{key}"), &json!({"monitor": pfx}));
        }
    }
}

type Elems = Vec<(String, Rect, String)>;

fn push(e: &mut Elems, id: &str, r: &egui::Response, label: &str) {
    e.push((id.to_string(), r.rect, label.to_string()));
}

/// Add Guide… / Save Guides as Template… / Manage Guides… dialogs.
pub fn dialogs(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.guide_dialog.clone() else { return };
    let mut elems: Elems = Vec::new();
    let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let mut act: Option<(String, Value)> = None;
    let templates: Vec<String> = app.session.prefs.guides.templates.iter().map(|t| t.name.clone()).collect();
    let (title, shown) = match &d {
        GuideDialog::Add { .. } => ("Add Guide", tl!("Add Guide")),
        GuideDialog::SaveTemplate { .. } => ("Save Guides as Template", tl!("Save Guides as Template")),
        GuideDialog::Manage { .. } => ("Manage Guides", tl!("Manage Guides")),
    };
    egui::Window::new(shown).id(egui::Id::new(title)).collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(
        ctx,
        |ui| match &mut d {
            GuideDialog::Add { vertical, position, source } => {
                let m = if *source { "source" } else { "program" };
                ui.horizontal(|ui| {
                    ui.label(tl!("Orientation:"));
                    let r = ui.radio(*vertical, tl!("Vertical"));
                    push(&mut elems, "guides.add.vertical", &r, "Vertical");
                    if r.clicked() {
                        *vertical = true;
                    }
                    let r = ui.radio(!*vertical, tl!("Horizontal"));
                    push(&mut elems, "guides.add.horizontal", &r, "Horizontal");
                    if r.clicked() {
                        *vertical = false;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(tl!("Position:"));
                    let r = ui.add(egui::DragValue::new(position).speed(1.0).suffix(" px"));
                    push(&mut elems, "guides.add.position", &r, "Position");
                });
                ui.horizontal(|ui| {
                    let r = ui.button(tl!("Cancel"));
                    push(&mut elems, "guides.add.cancel", &r, "Cancel");
                    close |= r.clicked();
                    let r = ui.button(tl!("OK"));
                    push(&mut elems, "guides.add.ok", &r, "OK");
                    if r.clicked() {
                        act = Some((
                            "view.addGuide".into(),
                            json!({"monitor": m, "orientation": if *vertical { "vertical" } else { "horizontal" }, "position": *position}),
                        ));
                    }
                });
            }
            GuideDialog::SaveTemplate { name, source } => {
                let m = if *source { "source" } else { "program" };
                ui.horizontal(|ui| {
                    ui.label(tl!("Name:"));
                    let r = ui.text_edit_singleline(name);
                    push(&mut elems, "guides.save.name", &r, "Name");
                });
                ui.horizontal(|ui| {
                    let r = ui.button(tl!("Cancel"));
                    push(&mut elems, "guides.save.cancel", &r, "Cancel");
                    close |= r.clicked();
                    let r = ui.button(tl!("OK"));
                    push(&mut elems, "guides.save.ok", &r, "OK");
                    if r.clicked() {
                        act = Some(("view.guideTemplates.save".into(), json!({"monitor": m, "name": name.clone()})));
                    }
                });
            }
            GuideDialog::Manage { selected, source } => {
                let m = if *source { "source" } else { "program" };
                if templates.is_empty() {
                    ui.label(tl!("No saved guide templates."));
                }
                for (i, n) in templates.iter().enumerate() {
                    let r = ui.selectable_label(*selected == Some(i), n);
                    push(&mut elems, &format!("guides.manage.row.{i}"), &r, n);
                    if r.clicked() {
                        *selected = Some(i);
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let sel = selected.and_then(|i| templates.get(i)).cloned();
                    let r = ui.add_enabled(sel.is_some(), egui::Button::new(tl!("Apply")));
                    push(&mut elems, "guides.manage.apply", &r, "Apply");
                    if r.clicked()
                        && let Some(n) = &sel
                    {
                        act = Some(("view.guideTemplates.apply".into(), json!({"monitor": m, "name": n})));
                    }
                    let r = ui.add_enabled(sel.is_some(), egui::Button::new(tl!("Delete")));
                    push(&mut elems, "guides.manage.delete", &r, "Delete");
                    if r.clicked()
                        && let Some(n) = &sel
                    {
                        act = Some(("view.guideTemplates.delete".into(), json!({"name": n})));
                        *selected = None;
                    }
                    let r = ui.button(tl!("Close"));
                    push(&mut elems, "guides.manage.close", &r, "Close");
                    close |= r.clicked();
                });
            }
        },
    );
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    let keep_open = matches!(d, GuideDialog::Manage { .. });
    app.ui.guide_dialog = Some(d);
    if let Some((id, p)) = act {
        match route(app, &id, &p) {
            Some(Err(e)) => app.ui.status = e,
            _ => close |= !keep_open || id == "view.guideTemplates.apply",
        }
    }
    if close {
        app.ui.guide_dialog = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_axis_picks_the_closest_target_within_reach() {
        assert_eq!(snap_axis([10.0, 20.0, 30.0], &[0.0, 33.0, 100.0], 6.0), Some((3.0, 33.0)));
        assert_eq!(snap_axis([10.0, 20.0, 30.0], &[50.0], 6.0), None);
        assert_eq!(snap_axis([10.0, 20.0, 30.0], &[8.0, 21.0], 6.0), Some((1.0, 21.0)));
    }

    #[test]
    fn channel_view_greyscales() {
        let img = Rgba { w: 2, h: 1, px: vec![200, 100, 50, 255, 255, 255, 255, 0] };
        assert_eq!(channel_view(&img, DisplayMode::Red).px, vec![200, 200, 200, 255, 0, 0, 0, 255]);
        assert_eq!(channel_view(&img, DisplayMode::Green).px[..4], [100, 100, 100, 255]);
        assert_eq!(channel_view(&img, DisplayMode::Alpha).px, vec![255, 255, 255, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn picture_rect_fits_or_magnifies_with_clamped_pan() {
        let area = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0));
        let mut v = MonitorView::default();
        let r = picture_rect(area, 1920.0, 1080.0, &v, 1.0);
        assert!((r.width() - 400.0).abs() < 1e-3);
        v.zoom = Some(1.0);
        v.pan = [10_000.0, 0.0];
        let r = picture_rect(area, 1920.0, 1080.0, &v, 2.0);
        assert!((r.width() - 960.0).abs() < 1e-3, "100% = one frame pixel per physical pixel");
        assert!((r.min.x - 0.0).abs() < 1e-3, "pan clamped so the picture's left edge stays at the area's");
        v.zoom = Some(0.1);
        let r = picture_rect(area, 1920.0, 1080.0, &v, 1.0);
        assert!((r.center().x - 200.0).abs() < 1e-3, "a small picture stays centred");
    }

    #[test]
    fn effective_resolution_follows_paused_and_high_quality() {
        let mut v = MonitorView::default();
        assert_eq!(v.effective_res(false), PlaybackRes::Full);
        assert_eq!(v.effective_res(true), PlaybackRes::Half);
        v.high_quality = true;
        assert_eq!(v.effective_res(true), PlaybackRes::Full);
        v.paused_res = PlaybackRes::Quarter;
        assert_eq!(v.effective_res(true), PlaybackRes::Half);
        assert_eq!(v.effective_res(false), PlaybackRes::Quarter);
    }
}
