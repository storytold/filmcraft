//! Programmatic control of the running app (agents, tests, MCP bridge).
//!
//! Transport-agnostic: a transport thread (TCP in the desktop app) sends [`ControlRequest`]s; the
//! UI thread handles them between frames and replies with `{"ok":…, "result"|"error":…}`.
//!
//! Methods:
//! - `engine.execute {command, params}` / `ui.menu.invoke {id}`: run any engine or UI command
//! - `engine.commands`: list engine commands; `ui.menu.list`: menu tree with enablement
//! - `ui.inspect`: UI state + every registered widget (`elements`: id, label, rect)
//! - `ui.elements {prefix?}`: registered widgets (optionally filtered by id prefix)
//! - `ui.set {tool?, workspace?, mode?, theme?, focused?, playbackRes?, timeline?:{pps,scroll}}`
//! - `ui.panel.show {panel}` / `ui.panel.close {panel}`
//! - `ui.panel.move {panel, target, zone}` / `ui.panel.undock {panel}` / `ui.panel.dock {panel}`
//! - `ui.click {id | x,y, button?, count?, modifiers?}` / `ui.move {x,y}` / `ui.scroll {x,y,dx,dy}`
//! - `ui.drag {from:{id|x,y}, to:{id|x,y}, steps?, modifiers?}`: synthetic press-move-release
//! - `ui.key {key, command?, shift?, alt?, ctrl?}` / `ui.type {text}`
//! - `ui.timeline.hit {x, y}`: what the timeline shows at a point (track, clip, edge, time)
//! - `ui.timeline.locate {clip, edge?}`: screen point of a clip body/edge (for drags)
//! - `ui.playback {action: play|stop|toggle, speed?}`
//! - `ui.screenshot {path?, panel?}`: PNG of the window (or one panel)
//! - `ui.resize {width, height}` / `ui.focus` / `app.quit`

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::state::{Mode, PlaybackRes, Tool};

/// Prefix marking errors that may resolve after another frame.
pub const RETRY: &str = "\u{1}";

pub struct ControlRequest {
    pub method: String,
    pub params: Value,
    pub reply: Sender<Value>,
}

impl ControlRequest {
    pub fn new(method: impl Into<String>, params: Value) -> (Self, std::sync::mpsc::Receiver<Value>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Self { method: method.into(), params, reply: tx }, rx)
    }
}

pub enum Outcome {
    Done(Value),
    /// Try again on a later frame (e.g. the element is not on screen yet).
    Retry(String),
    /// Reply once queued synthetic input has been processed.
    AfterInput,
    Screenshot {
        path: Option<String>,
        crop: Option<[f32; 4]>,
    },
}

fn ok(v: Value) -> Outcome {
    Outcome::Done(json!({"ok": true, "result": v}))
}
fn err(e: impl std::fmt::Display) -> Outcome {
    Outcome::Done(json!({"ok": false, "error": e.to_string()}))
}

fn modifiers(p: &Value) -> egui::Modifiers {
    let m = p.get("modifiers").unwrap_or(p);
    let b = |k: &str| m.get(k).and_then(Value::as_bool).unwrap_or(false);
    egui::Modifiers { alt: b("alt"), ctrl: b("ctrl"), shift: b("shift"), mac_cmd: cfg!(target_os = "macos") && b("command"), command: b("command") }
}

/// Resolve a point from `{id}` (element centre) or `{x, y}`.
fn point(app: &FilmcraftApp, p: &Value) -> Result<egui::Pos2, String> {
    if let Some(id) = p.get("id").and_then(Value::as_str) {
        let e = app.auto.find(id).ok_or_else(|| format!("{RETRY}no element `{id}` (see ui.elements)"))?;
        let fx = p.get("fx").and_then(Value::as_f64).unwrap_or(0.5) as f32;
        let fy = p.get("fy").and_then(Value::as_f64).unwrap_or(0.5) as f32;
        return Ok(egui::pos2(e.rect[0] + e.rect[2] * fx, e.rect[1] + e.rect[3] * fy));
    }
    let x = p.get("x").and_then(Value::as_f64).ok_or("need `id` or `x`,`y`")?;
    let y = p.get("y").and_then(Value::as_f64).ok_or("need `y`")?;
    Ok(egui::pos2(x as f32, y as f32))
}

pub fn handle(app: &mut FilmcraftApp, ctx: &egui::Context, req: &ControlRequest) -> Outcome {
    let p = &req.params;
    let s = |k: &str| p.get(k).and_then(Value::as_str);
    match req.method.as_str() {
        "engine.execute" | "ui.menu.invoke" => {
            let Some(id) = s("command").or(s("id")) else { return err("missing `command`") };
            let params = p.get("params").cloned().unwrap_or(json!({}));
            match crate::menus::invoke(app, ctx, id, params) {
                Ok(v) => ok(v),
                Err(e) => err(e),
            }
        }
        "engine.commands" => match app.session.execute("command.list", json!({})) {
            Ok(v) => ok(v),
            Err(e) => err(e),
        },
        "ui.menu.list" => ok(serde_json::to_value(crate::menus::menu_items(app)).unwrap_or_default()),
        "ui.inspect" => ok(inspect(app, ctx)),
        "perf.stats" => ok(crate::perf::stats(app)),
        "ui.elements" => {
            if app.timeline_still < 2 {
                // rects of timeline elements are still moving (e.g. the fit after opening a sequence)
                return Outcome::Retry("the timeline is still zooming".into());
            }
            let prefix = s("prefix").unwrap_or("");
            ok(serde_json::to_value(app.auto.query(prefix)).unwrap_or_default())
        }
        "ui.set" => {
            if let Some(t) = s("tool") {
                match Tool::from_name(t) {
                    Some(t) => app.ui.tool = t,
                    None => return err(format!("unknown tool `{t}`")),
                }
            }
            if let Some(w) = s("workspace") {
                match crate::dock::find(&app.workspaces, w) {
                    Some(w) => app.set_workspace(&w),
                    None => return err(format!("unknown workspace `{w}`")),
                }
            }
            if let Some(m) = s("mode") {
                app.ui.mode = match m.to_ascii_lowercase().as_str() {
                    "import" => Mode::Import,
                    "export" => Mode::Export,
                    _ => Mode::Edit,
                };
            }
            if let Some(th) = s("theme") {
                match crate::theme::ThemeKind::from_name(th) {
                    Some(k) => crate::panels::settings::set_theme(app, ctx, k),
                    None => return err("unknown theme (dark, medium, light)"),
                }
            }
            // the open Settings dialog: page and draft values (`{"page": id, "values": {key: value}}`)
            if let Some(st) = p.get("settings")
                && let Err(e) = crate::panels::settings::patch(app, st)
            {
                return err(e);
            }
            if let Some(f) = s("focused").and_then(PanelKind::from_name) {
                app.ui.focused = f;
            }
            if let Some(r) = s("playbackRes") {
                app.ui.program.res = PlaybackRes::ALL
                    .into_iter()
                    .find(|x| x.label().eq_ignore_ascii_case(r) || format!("{x:?}").eq_ignore_ascii_case(r))
                    .unwrap_or(app.ui.program.res);
            }
            if let Some(tl) = p.get("timeline") {
                if let Some(v) = tl.get("pps").and_then(Value::as_f64) {
                    app.ui.timeline.target_pps = v;
                }
                if let Some(v) = tl.get("scroll").and_then(Value::as_f64) {
                    app.ui.timeline.target_scroll = v;
                }
                if tl.get("fit").and_then(Value::as_bool) == Some(true) {
                    app.ui.timeline.fit_pending = true;
                }
                if let Some(v) = tl.get("videoTrackHeight").and_then(Value::as_f64) {
                    app.ui.timeline.video_track_h = v as f32;
                }
                if let Some(v) = tl.get("audioTrackHeight").and_then(Value::as_f64) {
                    app.ui.timeline.audio_track_h = v as f32;
                }
            }
            if let Some(q) = s("effectsSearch") {
                app.ui.effects_search = q.to_string();
            }
            if let Some(v) = p.get("safeMargins").and_then(Value::as_bool) {
                app.ui.program.safe_margins = v;
            }
            // Monitor view state (serde fields of `MonitorView`), merged into the current state.
            for k in ["program", "source"] {
                let Some(patch) = p.get(k).and_then(Value::as_object) else { continue };
                let mv = if k == "program" { &mut app.ui.program } else { &mut app.ui.source };
                let mut cur = serde_json::to_value(&*mv).unwrap_or_default();
                if let Some(o) = cur.as_object_mut() {
                    for (f, v) in patch {
                        o.insert(f.clone(), v.clone());
                    }
                }
                match serde_json::from_value(cur) {
                    Ok(m) => *mv = m,
                    Err(e) => return err(format!("`{k}`: {e}")),
                }
            }
            // Export mode state (`panels::export_mode::ExportUi`), deep-merged; `"preset": name`
            // applies that preset's settings first
            if let Some(patch) = p.get("export").filter(|v| v.is_object()) {
                if let Some(name) = patch.get("preset").and_then(Value::as_str)
                    && name != crate::panels::export_mode::CUSTOM
                    && !crate::panels::export_mode::apply_preset(app, name)
                {
                    return err(format!("no export preset named `{name}`"));
                }
                let mut cur = serde_json::to_value(&app.ui.export).unwrap_or_default();
                merge(&mut cur, patch);
                match serde_json::from_value(cur) {
                    Ok(e) => app.ui.export = e,
                    Err(e) => return err(format!("`export`: {e}")),
                }
            }
            // panel settings (`panels::panel_state`): {"scopes": {...}, "timecode": {...}, …}, merged
            if let Some(patch) = p.get("panels") {
                let mut cur = serde_json::to_value(&app.ui.panels).unwrap_or_default();
                merge(&mut cur, patch);
                match serde_json::from_value(cur) {
                    Ok(v) => app.ui.panels = v,
                    Err(e) => return err(format!("`panels`: {e}")),
                }
            }
            // Project panel / Media Browser view state (`panels::project`, `panels::media_browser`), merged
            for (k, which) in [("projectPanel", 0), ("mediaBrowser", 1)] {
                let Some(patch) = p.get(k) else { continue };
                let mut cur =
                    if which == 0 { serde_json::to_value(&app.ui.project_panel) } else { serde_json::to_value(&app.ui.media_browser) }.unwrap_or_default();
                merge(&mut cur, patch);
                let r = if which == 0 {
                    serde_json::from_value(cur).map(|v| app.ui.project_panel = v)
                } else {
                    serde_json::from_value(cur).map(|v| app.ui.media_browser = v)
                };
                if let Err(e) = r {
                    return err(format!("`{k}`: {e}"));
                }
            }
            // Essential Graphics ▸ Browse and the template / font dialogs (`panels::graphics_templates`), merged
            if let Some(patch) = p.get("gfxTemplates") {
                let mut cur = serde_json::to_value(&app.ui.gfx_templates).unwrap_or_default();
                merge(&mut cur, patch);
                match serde_json::from_value(cur) {
                    Ok(v) => app.ui.gfx_templates = v,
                    Err(e) => return err(format!("`gfxTemplates`: {e}")),
                }
            }
            // the Type tool's text editing state: {"clip", "layer", "caret", "anchor"} (byte offsets) or null
            if let Some(v) = p.get("gfxEdit") {
                match serde_json::from_value(v.clone()) {
                    Ok(e) => app.ui.gfx_edit = e,
                    Err(e) => return err(format!("`gfxEdit`: {e}")),
                }
            }
            // fields of the open Edit / Clip / File dialog (`panels::clip_dialogs`)
            if let Some(m) = p.get("clipDialog").and_then(Value::as_object) {
                let Some(d) = app.ui.clip_dialog.as_mut() else { return err("no clip dialog is open") };
                for (k, v) in m {
                    d.params[k.as_str()] = v.clone();
                }
            }
            // fields of the open M3.11 menu dialog (`panels::menu_dialogs`)
            if let Some(m) = p.get("menuDialog").and_then(Value::as_object) {
                let Some(d) = app.ui.extras.dialog.as_mut() else { return err("no menu dialog is open") };
                for (k, v) in m {
                    d.params[k.as_str()] = v.clone();
                }
            }
            ok(Value::Null)
        }
        "ui.panel.show" | "ui.panel.close" => {
            let Some(panel) = s("panel").and_then(PanelKind::from_name) else { return err("unknown `panel`") };
            if req.method == "ui.panel.show" {
                app.show_panel(panel);
            } else {
                app.ui.dock.close(panel);
                app.ui.floating.retain(|f| f.panel != panel);
            }
            ok(Value::Null)
        }
        "ui.panel.undock" | "ui.panel.dock" => {
            let Some(panel) = s("panel").and_then(PanelKind::from_name) else { return err("unknown `panel`") };
            if req.method == "ui.panel.undock" {
                match app.undock_panel(panel) {
                    Ok(()) => ok(Value::Null),
                    Err(e) => err(e),
                }
            } else {
                app.dock_panel(panel);
                ok(Value::Null)
            }
        }
        "ui.panel.move" => {
            let (Some(panel), Some(target)) = (s("panel").and_then(PanelKind::from_name), s("target").and_then(PanelKind::from_name)) else {
                return err("need `panel` and `target`");
            };
            let Some(zone) = s("zone").and_then(crate::dock::DropZone::from_name) else { return err("`zone` is center, left, right, top or bottom") };
            if app.ui.dock.move_panel(panel, target, zone) {
                app.ui.dock.activate(panel);
                ok(Value::Null)
            } else {
                err("cannot move that panel there")
            }
        }
        "ui.click" | "ui.move" => {
            let pos = match point(app, p) {
                Ok(p) => p,
                Err(e) if e.starts_with(RETRY) => return Outcome::Retry(e.trim_start_matches(RETRY).to_string()),
                Err(e) => return err(e),
            };
            let m = modifiers(p);
            app.synthetic.push(egui::Event::PointerMoved(pos));
            if req.method == "ui.click" {
                let button = match s("button") {
                    Some("right") => egui::PointerButton::Secondary,
                    Some("middle") => egui::PointerButton::Middle,
                    _ => egui::PointerButton::Primary,
                };
                let count = p.get("count").and_then(Value::as_u64).unwrap_or(1);
                for _ in 0..count {
                    app.synthetic.push(egui::Event::PointerButton { pos, button, pressed: true, modifiers: m });
                    app.synthetic.push(egui::Event::PointerButton { pos, button, pressed: false, modifiers: m });
                }
            }
            Outcome::AfterInput
        }
        "ui.drag" => {
            let (Some(from), Some(to)) = (p.get("from"), p.get("to")) else { return err("need `from` and `to`") };
            let (a, b) = match (point(app, from), point(app, to)) {
                (Ok(a), Ok(b)) => (a, b),
                (Err(e), _) | (_, Err(e)) if e.starts_with(RETRY) => return Outcome::Retry(e.trim_start_matches(RETRY).to_string()),
                (Err(e), _) | (_, Err(e)) => return err(e),
            };
            let steps = p.get("steps").and_then(Value::as_u64).unwrap_or(12).max(2);
            let m = modifiers(p);
            app.synthetic.push(egui::Event::PointerMoved(a));
            app.synthetic.push(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
            for i in 1..=steps {
                let f = i as f32 / steps as f32;
                app.synthetic.push(egui::Event::PointerMoved(a + (b - a) * f));
            }
            app.synthetic.push(egui::Event::PointerButton { pos: b, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
            Outcome::AfterInput
        }
        "ui.scroll" => {
            let pos = match point(app, p) {
                Ok(p) => p,
                Err(e) => return err(e),
            };
            let dx = p.get("dx").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            let dy = p.get("dy").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            app.synthetic.push(egui::Event::PointerMoved(pos));
            app.synthetic.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(dx, dy),
                modifiers: modifiers(p),
                phase: egui::TouchPhase::Move,
            });
            app.synthetic.push(egui::Event::PointerButton {
                pos: egui::pos2(-100.0, -100.0),
                button: egui::PointerButton::Extra1,
                pressed: false,
                modifiers: Default::default(),
            });
            Outcome::AfterInput
        }
        "ui.key" => {
            let Some(name) = s("key") else { return err("missing `key`") };
            let Some((mut m, key)) = crate::menus::parse_shortcut(name) else { return err(format!("unknown key `{name}`")) };
            let extra = modifiers(p);
            m |= extra;
            app.synthetic.push(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: m });
            app.synthetic.push(egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: m });
            Outcome::AfterInput
        }
        "ui.type" => {
            let Some(text) = s("text") else { return err("missing `text`") };
            app.synthetic.push(egui::Event::Text(text.to_string()));
            app.synthetic.push(egui::Event::Key { key: egui::Key::F35, physical_key: None, pressed: false, repeat: false, modifiers: Default::default() });
            Outcome::AfterInput
        }
        "ui.timeline.hit" => {
            let pos = match point(app, p) {
                Ok(p) => p,
                Err(e) => return err(e),
            };
            ok(crate::panels::timeline::hit_json(app, pos))
        }
        "ui.timeline.locate" => {
            let Some(c) = p.get("clip").and_then(Value::as_u64) else { return err("need `clip`") };
            match crate::panels::timeline::locate(app, c, s("edge")) {
                Some((x, y)) => ok(json!({"x": x, "y": y})),
                None => err("clip not visible in the timeline"),
            }
        }
        "ui.playback" => {
            match s("action").unwrap_or("toggle") {
                "play" => app.play(p.get("speed").and_then(Value::as_f64).unwrap_or(1.0)),
                "stop" => app.stop(),
                _ => app.toggle_play(1.0),
            }
            ok(json!({"playing": app.playback.playing, "playhead": app.session.playhead().0}))
        }
        "ui.screenshot" => {
            let crop = s("panel").and_then(PanelKind::from_name).and_then(|pk| app.auto.find(&format!("panel.{}", pk.id())).map(|e| e.rect));
            let crop = crop.or_else(|| s("id").and_then(|id| app.auto.find(id).map(|e| e.rect)));
            // Bring the window on screen so a frame is presented, without taking keyboard focus.
            app.raise_for_control(ctx);
            Outcome::Screenshot { path: s("path").map(str::to_string), crop }
        }
        "ui.resize" => {
            let w = p.get("width").and_then(Value::as_f64).unwrap_or(1600.0) as f32;
            let h = p.get("height").and_then(Value::as_f64).unwrap_or(1000.0) as f32;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
            ok(Value::Null)
        }
        "ui.focus" => {
            // Explicit request: activate the app and take keyboard focus.
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ok(Value::Null)
        }
        "app.quit" => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ok(Value::Null)
        }
        m => err(format!("unknown method `{m}`")),
    }
}

/// Merge `patch` into `v` (objects recursively; other values replace).
fn merge(v: &mut Value, patch: &Value) {
    match (v, patch) {
        (Value::Object(o), Value::Object(p)) => {
            for (k, pv) in p {
                match o.get_mut(k) {
                    Some(cur) => merge(cur, pv),
                    None => {
                        o.insert(k.clone(), pv.clone());
                    }
                }
            }
        }
        (v, p) => *v = p.clone(),
    }
}

pub fn inspect(app: &FilmcraftApp, ctx: &egui::Context) -> Value {
    let size = ctx.content_rect().size();
    json!({
        "window": [size.x, size.y],
        "pixelsPerPoint": ctx.pixels_per_point(),
        "fps": app.fps,
        "ui": app.ui,
        "playback": {"playing": app.playback.playing, "speed": app.playback.speed, "loop": app.playback.looping, "audioClock": app.playback.audio_clock, "dropped": app.playback.meter.counts().1, "shown": app.playback.meter.counts().0, "preroll": app.playback.preroll.is_some()},
        "playhead": app.session.playhead().0,
        "activeSequence": app.session.state.active_sequence.map(|i| i.0),
        "selection": app.session.state.selection.iter().map(|c| c.0).collect::<Vec<_>>(),
        "elements": app.auto.previous.len(),
        "dialog": app.dialog.map(|d| format!("{d:?}")),
    })
}

/// Save a screenshot (optionally cropped to a rect in points) as PNG; returns the reply JSON.
pub fn save_screenshot(ctx: &egui::Context, image: &egui::ColorImage, path: Option<&str>, crop: Option<[f32; 4]>) -> Value {
    let ppp = ctx.pixels_per_point();
    let [w, h] = image.size;
    let (x0, y0, cw, ch) = match crop {
        Some([x, y, cw, ch]) => {
            let x0 = ((x * ppp) as usize).min(w);
            let y0 = ((y * ppp) as usize).min(h);
            (x0, y0, ((cw * ppp) as usize).min(w - x0), ((ch * ppp) as usize).min(h - y0))
        }
        None => (0, 0, w, h),
    };
    let mut rgba = Vec::with_capacity(cw * ch * 4);
    for y in y0..y0 + ch {
        for x in x0..x0 + cw {
            let c = image.pixels[y * w + x];
            rgba.extend_from_slice(&[c.r(), c.g(), c.b(), 255]);
        }
    }
    if cfg!(target_arch = "wasm32") {
        // No filesystem on the web: the PNG comes back in the reply.
        return match encode_png(&rgba, cw as u32, ch as u32) {
            Ok(png) => json!({"ok": true, "result": {"pngBase64": base64(&png), "width": cw, "height": ch}}),
            Err(e) => json!({"ok": false, "error": e}),
        };
    }
    let path = path.map(str::to_string).unwrap_or_else(|| filmcraft_engine::temp_dir().join("filmcraft-screenshot.png").to_string_lossy().to_string());
    match encode_png(&rgba, cw as u32, ch as u32) {
        Ok(png) => match std::fs::write(&path, png) {
            Ok(()) => json!({"ok": true, "result": {"path": path, "width": cw, "height": ch}}),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        },
        Err(e) => json!({"ok": false, "error": e}),
    }
}

/// Standard base64 (RFC 4648, with padding).
pub fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(A[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Minimal PNG encoder (stored deflate blocks, no compression dependency).
pub fn encode_png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    fn chunk(out: &mut Vec<u8>, ty: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = Vec::with_capacity(4 + data.len());
        c.extend_from_slice(ty);
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc(&c).to_be_bytes());
    }
    fn crc(d: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in d {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        !c
    }
    let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * w as usize * 4..(y + 1) * w as usize * 4]);
    }
    // zlib stream with stored blocks
    let mut z = vec![0x78, 0x01];
    for (i, block) in raw.chunks(65535).enumerate() {
        let last = (i + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

#[cfg(test)]
mod base64_tests {
    #[test]
    fn rfc4648_vectors() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(super::base64(i.as_bytes()), o);
        }
    }
}
