//! Nonmodal Speed Editor diagnostics. Input observation precedes editing/focus guards,
//! so unsupported buttons and suppressed input remain visible without changing a project.

use std::{collections::VecDeque, io::Cursor, time::Duration};

use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::gestures::Taps;
use super::{Button, Event};
use crate::{FilmcraftApp, menus};
use filmcraft_engine::settings::{SPEED_EDITOR_SENSITIVITY_MAX, SPEED_EDITOR_SENSITIVITY_MIN};

const BACKGROUND: &[u8] = include_bytes!("../../../../assets/devices/speed-editor.webp");
const PHOTO_SIZE: egui::Vec2 = vec2(1280.0, 818.0);
const DIAL_CENTRE: egui::Vec2 = vec2(1086.0, 615.0);
const DIAL_RADIUS: f32 = 172.0;
const HISTORY_LIMIT: usize = 64;
const FLASH_SECONDS: f64 = 0.6;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowState {
    pub open: bool,
    pub monitor_only: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self { open: false, monitor_only: true }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pulse {
    pub button: Button,
    pub until: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WheelReading {
    pub mode: u8,
    pub value: i32,
    pub time: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub number: u64,
    pub time: f64,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct GestureReading {
    pub button: Button,
    pub action: &'static str,
    pub time: f64,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Telemetry {
    pub inputs: u64,
    pub button_changes: u64,
    pub wheel_events: u64,
    pub double_taps: u64,
    pub last_gesture: Option<GestureReading>,
    pub pressed: Vec<Button>,
    pub pulses: Vec<Pulse>,
    pub last_wheel: Option<WheelReading>,
    pub wheel_position: i64,
    pub history: VecDeque<Entry>,
    #[serde(skip)]
    taps: Taps,
}

impl Telemetry {
    pub fn record(&mut self, event: &Event, now: f64) {
        if matches!(event, Event::Buttons(buttons) | Event::TimedButtons { buttons, .. } if buttons.len() > 6) {
            return;
        }
        let now = event.observed_at(if now.is_finite() { now.max(0.0) } else { 0.0 });
        self.inputs = self.inputs.saturating_add(1);
        self.pulses.retain(|pulse| pulse.until > now);
        match event {
            Event::Connection { connected, detail } => {
                self.taps.clear();
                if !connected {
                    self.pressed.clear();
                    self.pulses.clear();
                }
                self.log(now, format!("{}: {}", if *connected { "Connected" } else { "Disconnected" }, detail.chars().take(512).collect::<String>()));
            }
            Event::Buttons(buttons) | Event::TimedButtons { buttons, .. } => {
                let mut current = Vec::with_capacity(6);
                for button in buttons {
                    if !current.contains(button) {
                        current.push(*button);
                    }
                }
                let previous = std::mem::replace(&mut self.pressed, current.clone());
                for button in previous.iter().filter(|button| !current.contains(button)) {
                    self.taps.release(*button);
                    self.button_change(*button, false, now);
                }
                if current.len() > 1 {
                    self.taps.clear();
                }
                for button in current.iter().filter(|button| !previous.contains(button)) {
                    self.button_change(*button, true, now);
                    if current.len() == 1
                        && self.taps.press(*button, now)
                        && let Some(action) = button.secondary_label()
                    {
                        self.double_taps = self.double_taps.saturating_add(1);
                        self.last_gesture = Some(GestureReading { button: *button, action, time: now });
                        self.log(now, format!("{} double tap → {action}", key(*button).map_or("Unknown key", |key| key.label)));
                    }
                }
            }
            Event::Wheel { mode, value } => {
                if *value != 0 {
                    self.taps.clear();
                }
                self.wheel_events = self.wheel_events.saturating_add(1);
                self.wheel_position = if matches!(mode, 0 | 2) { self.wheel_position.saturating_add(i64::from(*value)) } else { i64::from(*value) };
                self.last_wheel = Some(WheelReading { mode: *mode, value: *value, time: now });
                self.log(now, format!("Dial {value:+} ({})", wheel_mode(*mode)));
            }
            Event::Battery { charging, level } => self.log(now, format!("Battery {}%{}", level.min(&100), if *charging { " · charging" } else { "" })),
        }
    }

    fn button_change(&mut self, button: Button, pressed: bool, now: f64) {
        self.button_changes = self.button_changes.saturating_add(1);
        if let Some(pulse) = self.pulses.iter_mut().find(|pulse| pulse.button == button) {
            pulse.until = now + FLASH_SECONDS;
        } else {
            self.pulses.push(Pulse { button, until: now + FLASH_SECONDS });
        }
        self.log(now, format!("{} {}", key(button).map_or("Unknown key", |key| key.label), if pressed { "pressed" } else { "released" }));
    }

    fn log(&mut self, now: f64, message: String) {
        while self.history.len() >= HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history.push_back(Entry { number: self.inputs, time: now, message });
    }

    pub fn highlighted(&self, button: Button, now: f64) -> bool {
        self.pressed.contains(&button) || self.pulses.iter().any(|pulse| pulse.button == button && pulse.until > now)
    }

    fn clear(&mut self) {
        let pressed = std::mem::take(&mut self.pressed);
        *self = Self { pressed, ..Default::default() };
    }
}

fn wheel_mode(mode: u8) -> &'static str {
    match mode {
        0 | 2 => "relative",
        1 => "absolute",
        3 => "centred shuttle",
        _ => "unknown mode",
    }
}

/// Hit/overlay rectangles in the contributor photograph's 1280 × 818 coordinate space.
struct Key {
    button: Button,
    id: &'static str,
    label: &'static str,
    bounds: [f32; 4],
}

macro_rules! key {
    ($button:ident, $id:literal, $label:literal, $x:literal, $y:literal, $w:literal, $h:literal) => {
        Key { button: Button::$button, id: $id, label: $label, bounds: [$x, $y, $w, $h] }
    };
}

const KEYS: &[Key] = &[
    key!(SmartInsert, "smartInsert", "SMART INSERT", 62., 117., 97., 96.),
    key!(Append, "append", "APPEND", 164., 117., 97., 96.),
    key!(RippleOverwrite, "rippleOverwrite", "RIPPLE OVERWRITE", 267., 117., 95., 96.),
    key!(CloseUp, "closeUp", "CLOSE UP", 62., 219., 97., 96.),
    key!(PlaceOnTop, "placeOnTop", "PLACE ON TOP", 164., 219., 97., 96.),
    key!(SourceOverwrite, "sourceOverwrite", "SOURCE OVERWRITE", 267., 219., 95., 96.),
    key!(In, "in", "IN", 66., 362., 148., 93.),
    key!(Out, "out", "OUT", 219., 362., 144., 93.),
    key!(TrimIn, "trimIn", "TRIM IN", 68., 464., 96., 94.),
    key!(TrimOut, "trimOut", "TRIM OUT", 169., 464., 96., 94.),
    key!(Roll, "roll", "ROLL", 270., 464., 94., 94.),
    key!(SlipSource, "slipSource", "SLIP SOURCE", 69., 564., 95., 94.),
    key!(SlipDestination, "slipDestination", "SLIP DESTINATION", 170., 564., 95., 94.),
    key!(TransitionDuration, "transitionDuration", "TRANSITION DURATION", 271., 564., 93., 94.),
    key!(Cut, "cut", "CUT", 71., 665., 94., 89.),
    key!(Dissolve, "dissolve", "DISSOLVE", 172., 665., 93., 89.),
    key!(SmoothCut, "smoothCut", "SMOOTH CUT", 273., 665., 92., 89.),
    key!(Escape, "escape", "ESC", 440., 116., 97., 97.),
    key!(SyncBin, "syncBin", "SYNC BIN", 542., 116., 96., 97.),
    key!(AudioLevel, "audioLevel", "AUDIO LEVEL", 644., 116., 96., 97.),
    key!(FullView, "fullView", "FULL VIEW", 747., 116., 95., 97.),
    key!(Transition, "transition", "TRANSITION", 440., 219., 97., 96.),
    key!(Split, "split", "SPLIT", 542., 219., 96., 96.),
    key!(Snap, "snap", "SNAP", 644., 219., 96., 96.),
    key!(RippleDelete, "rippleDelete", "RIPPLE DELETE", 747., 219., 95., 96.),
    key!(Cam7, "cam7", "CAM 7", 444., 361., 94., 94.),
    key!(Cam8, "cam8", "CAM 8", 546., 361., 93., 94.),
    key!(Cam9, "cam9", "CAM 9", 647., 361., 92., 94.),
    key!(LiveOverwrite, "liveOverwrite", "LIVE OVERWRITE", 747., 361., 94., 94.),
    key!(Cam4, "cam4", "CAM 4", 444., 461., 94., 95.),
    key!(Cam5, "cam5", "CAM 5", 546., 461., 93., 95.),
    key!(Cam6, "cam6", "CAM 6", 647., 461., 92., 95.),
    key!(VideoOnly, "videoOnly", "VIDEO ONLY", 747., 461., 94., 95.),
    key!(Cam1, "cam1", "CAM 1", 445., 563., 94., 95.),
    key!(Cam2, "cam2", "CAM 2", 546., 563., 93., 95.),
    key!(Cam3, "cam3", "CAM 3", 647., 563., 92., 95.),
    key!(AudioOnly, "audioOnly", "AUDIO ONLY", 747., 563., 94., 95.),
    key!(StopPlay, "stopPlay", "STOP / PLAY", 445., 665., 395., 89.),
    key!(Source, "source", "SOURCE", 923., 117., 145., 96.),
    key!(Timeline, "timeline", "TIMELINE", 1073., 117., 146., 96.),
    key!(Shuttle, "shuttle", "SHUTTLE", 923., 260., 94., 95.),
    key!(Jog, "jog", "JOG", 1021., 260., 95., 95.),
    key!(Scroll, "scroll", "SCROLL", 1121., 260., 96., 95.),
];

fn key(button: Button) -> Option<&'static Key> {
    KEYS.iter().find(|key| key.button == button)
}

fn texture(ctx: &egui::Context) -> Result<TextureHandle, String> {
    let id = egui::Id::unique("speed-editor-background");
    if let Some(result) = ctx.data(|data| data.get_temp::<Result<TextureHandle, String>>(id)) {
        return result;
    }
    let decoded = (|| {
        let mut reader = image::ImageReader::new(Cursor::new(BACKGROUND)).with_guessed_format().map_err(|e| e.to_string())?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(2048);
        limits.max_image_height = Some(2048);
        limits.max_alloc = Some(32 * 1024 * 1024);
        reader.limits(limits);
        let image = reader.decode().map_err(|e| format!("Could not decode Speed Editor background: {e}"))?.to_rgba8();
        let width = usize::try_from(image.width()).map_err(|e| e.to_string())?;
        let height = usize::try_from(image.height()).map_err(|e| e.to_string())?;
        if width == 0 || height == 0 {
            return Err("Speed Editor background has empty dimensions".into());
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], image.as_raw());
        Ok(ctx.load_texture("speed-editor-background", image, egui::TextureOptions::LINEAR))
    })();
    ctx.data_mut(|data| data.insert_temp(id, decoded.clone()));
    decoded
}

fn board(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let width = ui.available_width().clamp(1.0, 1100.0);
    let (area, _) = ui.allocate_exact_size(vec2(width, width * PHOTO_SIZE.y / PHOTO_SIZE.x), Sense::hover());
    match texture(ui.ctx()) {
        Ok(texture) => {
            ui.painter().image(texture.id(), area, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        }
        Err(e) => {
            ui.painter().rect_filled(area, 8.0, app.tokens.panel_bg);
            ui.painter().text(area.center(), egui::Align2::CENTER_CENTER, e, egui::FontId::proportional(13.0), app.tokens.text);
        }
    }
    app.auto.add("speedEditor.debugger.board", area, "Speed Editor board");
    let scale = width / PHOTO_SIZE.x;
    let outline_radius = 7.0 * scale;
    let now = app.now();
    let telemetry = &app.speed_editor.telemetry;
    for key in KEYS {
        let [x, y, w, h] = key.bounds;
        let rect = Rect::from_min_size(area.min + vec2(x, y) * scale, vec2(w, h) * scale);
        let held = telemetry.pressed.contains(&key.button);
        if telemetry.highlighted(key.button, now) {
            let double = telemetry.last_gesture.as_ref().is_some_and(|gesture| gesture.button == key.button && now < gesture.time + FLASH_SECONDS);
            let color = if double {
                Color32::from_rgb(202, 151, 255)
            } else if held {
                Color32::from_rgb(75, 235, 162)
            } else {
                Color32::from_rgb(255, 194, 82)
            };
            ui.painter().rect_filled(rect, outline_radius, color.gamma_multiply(0.25));
            ui.painter().rect_stroke(rect, outline_radius, Stroke::new(2.5, color), StrokeKind::Inside);
        }
        let response = ui.interact(rect, egui::Id::unique(("speed-editor-key", key.id)), Sense::hover());
        let secondary = key.button.secondary_label().map_or(String::new(), |label| format!("\nDouble tap: {label}"));
        response.on_hover_text(format!("{} · {}{secondary}", key.label, if held { "pressed" } else { "released" }));
        app.auto.add(&format!("speedEditor.debugger.button.{}", key.id), rect, key.label);
    }
    let centre = area.min + DIAL_CENTRE * scale;
    let radius = DIAL_RADIUS * scale;
    let wheel = Rect::from_center_size(centre, vec2(radius * 2.0, radius * 2.0));
    app.auto.add("speedEditor.debugger.wheel", wheel, "Speed Editor dial");
    let moving = telemetry.last_wheel.as_ref().is_some_and(|reading| now < reading.time + FLASH_SECONDS);
    if moving {
        let color = Color32::from_rgb(87, 199, 255);
        ui.painter().circle_stroke(centre, radius, Stroke::new(3.0, color));
        let angle = telemetry.wheel_position.rem_euclid(8192) as f32 / 8192.0 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        let tip = centre + vec2(angle.cos(), angle.sin()) * radius * 0.8;
        ui.painter().line_segment([centre, tip], Stroke::new(2.0, color));
        ui.painter().circle_filled(tip, outline_radius * 1.25, color);
    }
    if moving || telemetry.pulses.iter().any(|pulse| pulse.until > now) {
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    }
}

/// An ordinary floating egui window, rather than a modal dialog: it can observe board
/// activity while the timeline and monitors keep drawing and receiving playback updates.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if !app.ui.speed_editor_debugger.open {
        return;
    }
    let mut open = true;
    let shown = egui::Window::new("DaVinci Speed Editor")
        .id(egui::Id::unique("speed-editor-debugger"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(860.0)
        .default_height((ctx.content_rect().height() - 120.0).max(300.0))
        .min_width(420.0)
        .default_pos(pos2((ctx.content_rect().center().x - 430.0).max(8.0), ctx.content_rect().top() + 48.0))
        .vscroll(true)
        .max_height((ctx.content_rect().height() - 80.0).max(200.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let connected = app.speed_editor.connected;
                let color = if connected { Color32::from_rgb(75, 235, 162) } else { Color32::from_rgb(255, 161, 92) };
                let status = ui.label(RichText::new(if connected { tl!("Connected") } else { tl!("Disconnected") }).color(color).strong());
                app.auto.add_response("speedEditor.debugger.connection", &status, if connected { "Connected" } else { "Disconnected" });
                ui.label(if app.speed_editor.detail.is_empty() { "Waiting for USB device discovery" } else { &app.speed_editor.detail });
                if let Some(level) = app.speed_editor.battery {
                    ui.label(format!("{level}%{}", if app.speed_editor.charging { " · charging" } else { "" }));
                }
            });
            #[cfg(target_os = "linux")]
            if !app.speed_editor.connected {
                let hint = tl!("Device not detected? Linux udev rules may need adjustment. Check read/write access for USB device 1edb:da0e.");
                let response = ui.add(egui::Label::new(RichText::new(hint).small().weak()).wrap());
                app.auto.add_response("speedEditor.debugger.udevHint", &response, hint);
            }
            ui.horizontal_wrapped(|ui| {
                let mut monitor = app.ui.speed_editor_debugger.monitor_only;
                let response = ui.checkbox(&mut monitor, tl!("Monitor only"));
                app.auto.add_response("speedEditor.debugger.monitorOnly", &response, "Monitor only");
                if response.changed() {
                    invoke(app, ctx, "devices.speedEditor", json!({"monitorOnly":monitor}));
                }
                ui.label(if monitor { tl!("Inputs are shown without applying edits.") } else { tl!("Live editing bindings enabled.") });
                let clear = ui.button(tl!("Clear history"));
                app.auto.add_response("speedEditor.debugger.clear", &clear, "Clear history");
                if clear.clicked() {
                    invoke(app, ctx, "speedEditor.debugger.clear", json!({}));
                }
                let close = ui.button(tl!("Close"));
                app.auto.add_response("speedEditor.debugger.close", &close, "Close Speed Editor debugger");
                if close.clicked() {
                    invoke(app, ctx, "devices.speedEditor", json!({"open":false}));
                }
            });
            ui.horizontal_wrapped(|ui| {
                let mut sensitivity = app.session.prefs.general.speed_editor_sensitivity.clamp(SPEED_EDITOR_SENSITIVITY_MIN, SPEED_EDITOR_SENSITIVITY_MAX);
                let response = ui
                    .add(
                        egui::Slider::new(&mut sensitivity, SPEED_EDITOR_SENSITIVITY_MIN..=SPEED_EDITOR_SENSITIVITY_MAX)
                            .logarithmic(true)
                            .suffix("%")
                            .text(tl!("Knob sensitivity")),
                    )
                    .on_hover_text(tl!("Applies to Jog, Scroll, Shuttle and wheel trims. 100% keeps the original speed."));
                app.auto.add_response("speedEditor.debugger.sensitivity", &response, "Knob sensitivity");
                if response.changed() {
                    invoke(app, ctx, "devices.speedEditor", json!({"sensitivity":sensitivity}));
                }
                ui.label(tl!("Lower values slow the dial."));
            });
            ui.separator();
            board(app, ui);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tl!("Green: held")).color(Color32::from_rgb(75, 235, 162)));
                ui.label(RichText::new(tl!("Amber: recent press or release")).color(Color32::from_rgb(255, 194, 82)));
                ui.label(RichText::new(tl!("Blue: dial movement")).color(Color32::from_rgb(87, 199, 255)));
                ui.label(RichText::new(tl!("Purple: double tap")).color(Color32::from_rgb(202, 151, 255)));
            });
            let telemetry = &app.speed_editor.telemetry;
            ui.label(format!(
                "Inputs: {}   Key changes: {}   Dial events: {}   Double taps: {}",
                telemetry.inputs, telemetry.button_changes, telemetry.wheel_events, telemetry.double_taps
            ));
            let gesture = telemetry.last_gesture.as_ref().map_or_else(
                || "Double tap: none".into(),
                |gesture| format!("Double tap: {} → {}", key(gesture.button).map_or("Unknown key", |key| key.label), gesture.action),
            );
            let response = ui.monospace(&gesture);
            app.auto.add_response("speedEditor.debugger.lastGesture", &response, &gesture);
            let wheel = telemetry
                .last_wheel
                .as_ref()
                .map_or_else(|| "Dial: no movement received".to_string(), |reading| format!("Dial: {:+} · {}", reading.value, wheel_mode(reading.mode)));
            let response = ui.monospace(&wheel);
            app.auto.add_response("speedEditor.debugger.lastWheel", &response, &wheel);
            ui.separator();
            let log = egui::ScrollArea::vertical().id_salt("speed-editor-events").max_height(115.0).stick_to_bottom(true).show(ui, |ui| {
                for entry in &app.speed_editor.telemetry.history {
                    ui.label(RichText::new(format!("#{:04}  {:8.2}s  {}", entry.number, entry.time, entry.message)).monospace().size(11.5));
                }
                if app.speed_editor.telemetry.history.is_empty() {
                    ui.label(tl!("Press a key or turn the dial to see input events."));
                }
            });
            app.auto.add("speedEditor.debugger.history", log.inner_rect, "Recent Speed Editor events");
        });
    if let Some(shown) = shown {
        app.auto.add("speedEditor.debugger.window", shown.response.rect, "DaVinci Speed Editor");
    }
    if !open {
        invoke(app, ctx, "devices.speedEditor", json!({"open":false}));
    }
}

fn invoke(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: Value) {
    if let Err(e) = menus::invoke(app, ctx, id, params) {
        app.status(e);
    }
}

pub(super) fn route(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == "speedEditor.debugger.clear" {
        if !params.is_object() {
            return Some(Err("Speed Editor history parameters must be an object".into()));
        }
        app.speed_editor.telemetry.clear();
        return Some(Ok(Value::Null));
    }
    if id != "devices.speedEditor" {
        return None;
    }
    Some((|| {
        if !params.is_object() {
            return Err("Speed Editor window parameters must be an object".into());
        }
        let flag = |key| params.get(key).map(|value| value.as_bool().ok_or_else(|| format!("`{key}` must be a boolean"))).transpose();
        let open = flag("open")?.unwrap_or(true);
        let monitor = flag("monitorOnly")?.unwrap_or(app.ui.speed_editor_debugger.monitor_only);
        let sensitivity = params
            .get("sensitivity")
            .map(|value| {
                value
                    .as_u64()
                    .filter(|value| (u64::from(SPEED_EDITOR_SENSITIVITY_MIN)..=u64::from(SPEED_EDITOR_SENSITIVITY_MAX)).contains(value))
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or("sensitivity must be a whole percentage between 1 and 200")
            })
            .transpose()?;
        if let Some(sensitivity) = sensitivity {
            app.session.execute("prefs.set", json!({"key":"general.speedEditorSensitivity", "value":sensitivity})).map_err(|e| e.to_string())?;
        }
        if (open && monitor) != (app.ui.speed_editor_debugger.open && app.ui.speed_editor_debugger.monitor_only) {
            app.cancel_speed_editor_gestures();
        }
        if open && monitor {
            app.reset_speed_editor(ctx);
            app.speed_editor.blocked_until_release |= !app.speed_editor.held.is_empty();
        }
        app.ui.speed_editor_debugger = WindowState { open, monitor_only: monitor };
        ctx.request_repaint();
        Ok(json!({"open":open, "monitorOnly":monitor, "sensitivity":app.session.prefs.general.speed_editor_sensitivity}))
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[test]
    fn keys_cover_every_physical_button_once_and_stay_on_the_photograph() {
        let dimensions = image::ImageReader::new(Cursor::new(BACKGROUND)).with_guessed_format().unwrap().into_dimensions().unwrap();
        let photo_size = vec2(dimensions.0 as f32, dimensions.1 as f32);
        assert_eq!(photo_size, PHOTO_SIZE, "overlay coordinates must match the bundled photograph");
        let buttons: Vec<_> = (0..=255).filter_map(Button::from_code).collect();
        assert_eq!(KEYS.len(), buttons.len());
        for button in buttons {
            assert_eq!(KEYS.iter().filter(|key| key.button == button).count(), 1);
        }
        for key in KEYS {
            let [x, y, w, h] = key.bounds;
            assert!(x >= 0.0 && y >= 0.0 && w > 0.0 && h > 0.0 && x + w <= PHOTO_SIZE.x && y + h <= PHOTO_SIZE.y);
        }
        let dial = Rect::from_center_size(pos2(DIAL_CENTRE.x, DIAL_CENTRE.y), vec2(DIAL_RADIUS * 2.0, DIAL_RADIUS * 2.0));
        assert!(Rect::from_min_size(Pos2::ZERO, photo_size).contains_rect(dial));
    }

    #[test]
    fn held_keys_flash_on_release_and_disconnect_clears_highlights() {
        let mut telemetry = Telemetry::default();
        telemetry.record(&Event::Buttons(vec![Button::Split]), 1.0);
        assert!(telemetry.highlighted(Button::Split, 10.0), "a held key stays lit beyond its pulse");
        telemetry.record(&Event::Buttons(vec![]), 10.0);
        assert!(telemetry.highlighted(Button::Split, 10.5), "quick release remains visible");
        assert!(!telemetry.highlighted(Button::Split, 10.7));
        telemetry.record(&Event::Buttons(vec![Button::In, Button::Out]), 11.0);
        telemetry.record(&Event::Connection { connected: false, detail: "Unplugged".into() }, 11.1);
        assert!(telemetry.pressed.is_empty());
        assert!(telemetry.pulses.is_empty());
    }

    #[test]
    fn trace_is_bounded_deduplicates_keys_and_saturates_wheel_counts() {
        let mut telemetry = Telemetry::default();
        telemetry.record(&Event::Buttons(vec![Button::Split, Button::Split]), 1.0);
        telemetry.record(&Event::Buttons(vec![Button::Split]), 2.0);
        assert_eq!(telemetry.button_changes, 1);
        for n in 0..200 {
            telemetry.record(&Event::Wheel { mode: 0, value: i32::MAX }, f64::from(n));
        }
        assert_eq!(telemetry.history.len(), HISTORY_LIMIT);
        telemetry.wheel_position = i64::MAX;
        telemetry.record(&Event::Wheel { mode: 0, value: i32::MAX }, f64::NAN);
        assert_eq!(telemetry.wheel_position, i64::MAX);
        telemetry.record(&Event::Wheel { mode: 3, value: -2048 }, 202.0);
        assert_eq!(telemetry.wheel_position, -2048);
        assert!(telemetry.history.iter().all(|entry| entry.time.is_finite()));
        telemetry.clear();
        assert!(telemetry.history.is_empty());
        assert_eq!(telemetry.inputs, 0);
        assert_eq!(telemetry.pressed, vec![Button::Split], "clearing history must not fake a key release");
    }

    #[test]
    fn unfocused_physical_input_is_still_observed_without_editing() {
        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        session.execute("playhead.set", json!({"frame":24})).unwrap();
        let mut app = FilmcraftApp::new(session);
        let before = app.session.project.to_json();
        let pending = Rc::new(RefCell::new(vec![Event::Buttons(vec![Button::Split]), Event::Wheel { mode: 0, value: 128 }]));
        let input = pending.clone();
        app.hooks.speed_editor = Some(Box::new(move |_| input.borrow_mut().drain(..).collect()));
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput { focused: false, ..Default::default() }, |ui| app.poll_speed_editor(ui.ctx())).textures_delta.clear();
        assert_eq!(app.speed_editor.telemetry.inputs, 2);
        assert!(app.speed_editor.telemetry.pressed.contains(&Button::Split));
        assert_eq!(app.speed_editor.telemetry.last_wheel.as_ref().map(|wheel| wheel.value), Some(128));
        assert_eq!(app.session.project.to_json(), before);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 24);
    }

    #[test]
    fn hostile_window_flags_do_not_change_window_or_project() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        for params in [
            json!(null),
            json!([]),
            json!({"open":1}),
            json!({"monitorOnly":"yes"}),
            json!({"open":true,"monitorOnly":null}),
            json!({"sensitivity":0}),
            json!({"sensitivity":201}),
            json!({"sensitivity":-1}),
            json!({"sensitivity":1.5}),
            json!({"sensitivity":null}),
            json!({"sensitivity":"25"}),
            json!({"sensitivity":u64::MAX}),
            json!({"open":true,"sensitivity":25,"monitorOnly":null}),
        ] {
            assert!(menus::invoke(&mut app, &ctx, "devices.speedEditor", params).is_err());
            assert!(!app.ui.speed_editor_debugger.open);
            assert!(app.ui.speed_editor_debugger.monitor_only);
            assert_eq!(app.session.prefs.general.speed_editor_sensitivity, 100);
        }
    }
}
