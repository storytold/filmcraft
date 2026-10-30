//! Speed Editor bindings shared by desktop HID input and the control channel.
//! Project changes always pass through the existing undoable engine commands.

use filmcraft_engine::settings::{SPEED_EDITOR_SENSITIVITY_MAX, SPEED_EDITOR_SENSITIVITY_MIN};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{FilmcraftApp, menus};

pub mod debugger;
mod gestures;

/// Physical key identifiers from the Speed Editor HID protocol (bmdse 0.1.0).
macro_rules! buttons {
    ($($name:ident = $value:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub enum Button { $($name = $value),* }
        impl Button {
            pub fn from_code(code: u16) -> Option<Self> {
                match code { $($value => Some(Self::$name),)* _ => None }
            }
        }
    };
}

buttons! {
    SmartInsert = 0x01, Append = 0x02, RippleOverwrite = 0x03, CloseUp = 0x04,
    PlaceOnTop = 0x05, SourceOverwrite = 0x06, In = 0x07, Out = 0x08,
    TrimIn = 0x09, TrimOut = 0x0a, Roll = 0x0b, SlipSource = 0x0c,
    SlipDestination = 0x0d, TransitionDuration = 0x0e, Cut = 0x0f,
    Dissolve = 0x10, SmoothCut = 0x11, Source = 0x1a, Timeline = 0x1b,
    Shuttle = 0x1c, Jog = 0x1d, Scroll = 0x1e, SyncBin = 0x1f,
    Transition = 0x22, VideoOnly = 0x25, AudioOnly = 0x26, RippleDelete = 0x2b,
    AudioLevel = 0x2c, FullView = 0x2d, Snap = 0x2e, Split = 0x2f,
    LiveOverwrite = 0x30, Escape = 0x31, Cam1 = 0x33, Cam2 = 0x34,
    Cam3 = 0x35, Cam4 = 0x36, Cam5 = 0x37, Cam6 = 0x38,
    Cam7 = 0x39, Cam8 = 0x3a, Cam9 = 0x3b, StopPlay = 0x3c,
}

impl Button {
    fn wheel_edit(self) -> bool {
        matches!(
            self,
            Self::TrimIn
                | Self::TrimOut
                | Self::Roll
                | Self::SlipSource
                | Self::SlipDestination
                | Self::Split
                | Self::AudioLevel
                | Self::TransitionDuration
                | Self::CloseUp
                | Self::Snap
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WheelMode {
    #[default]
    Jog,
    Scroll,
    Shuttle,
}

/// Bounded input reports, decoded by the native host. A button report is a snapshot,
/// not a repeat: holding a key must never repeatedly cut or delete clips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Connection {
        connected: bool,
        detail: String,
    },
    Buttons(Vec<Button>),
    /// Native capture time keeps double taps accurate when rendering delays the UI.
    TimedButtons {
        buttons: Vec<Button>,
        at: std::time::Instant,
    },
    Wheel {
        mode: u8,
        value: i32,
    },
    Battery {
        charging: bool,
        level: u8,
    },
}

impl Event {
    fn observed_at(&self, now: f64) -> f64 {
        match self {
            Self::TimedButtons { at, .. } => (now - at.elapsed().as_secs_f64()).max(0.0),
            _ => now,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Feedback {
    pub wheel_mode: u8,
    pub wheel_leds: u8,
    pub button_leds: u32,
}

/// Transient controller state, inspectable with `speedEditor.status`.
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub telemetry: debugger::Telemetry,
    pub connected: bool,
    pub detail: String,
    pub source: bool,
    pub wheel_mode: WheelMode,
    pub video_only: bool,
    pub audio_only: bool,
    pub battery: Option<u8>,
    pub charging: bool,
    held: Vec<Button>,
    remainder: i64,
    remainder_sensitivity: u16,
    shuttling: bool,
    blocked_until_release: bool,
    #[serde(skip)]
    virtual_input: bool,
    #[serde(skip)]
    gestures: gestures::Gestures,
    secondary_held: Option<Button>,
}

impl State {
    pub fn feedback(&self, snapping: bool) -> Feedback {
        let editing = self.held.iter().any(|b| b.wheel_edit());
        Feedback {
            wheel_mode: if self.wheel_mode == WheelMode::Shuttle && !editing { 3 } else { 0 },
            wheel_leds: match self.wheel_mode {
                WheelMode::Jog => 1,
                WheelMode::Shuttle => 2,
                WheelMode::Scroll => 4,
            },
            button_leds: (u32::from(snapping) << 5) | (u32::from(self.video_only) << 13) | (u32::from(self.audio_only) << 17),
        }
    }
}

impl FilmcraftApp {
    pub(crate) fn poll_speed_editor(&mut self, ctx: &egui::Context) {
        // A physical controller never edits a background window, a modal dialog or an
        // automated control-only session. Still consume releases and connection notices.
        let monitoring = self.ui.speed_editor_debugger.open && self.ui.speed_editor_debugger.monitor_only;
        let active = !monitoring && !self.control_input_only && ctx.input(|i| i.focused) && self.dialog.is_none() && self.native_dialog.is_none();
        if !active && !self.speed_editor.virtual_input {
            self.cancel_speed_editor_gestures();
            self.reset_speed_editor(ctx);
            self.speed_editor.blocked_until_release |= !self.speed_editor.held.is_empty();
        }
        if monitoring || self.dialog.is_some() || self.native_dialog.is_some() {
            self.cancel_speed_editor_gestures();
        }
        let events = if let Some(mut poll) = self.hooks.speed_editor.take() {
            let events = poll(self.speed_editor.feedback(self.session.state.snapping));
            self.hooks.speed_editor = Some(poll);
            events
        } else {
            Vec::new()
        };
        for event in events.into_iter().take(256) {
            // Capture raw activity even when editing is suppressed or the app is unfocused.
            self.speed_editor.telemetry.record(&event, self.now());
            let connection = matches!(event, Event::Connection { .. } | Event::Battery { .. });
            if active || connection {
                if active && !connection && self.speed_editor.virtual_input {
                    self.cancel_speed_editor_gestures();
                    self.reset_speed_editor(ctx);
                    self.speed_editor.held.clear();
                    self.speed_editor.virtual_input = false;
                }
                if let Err(e) = self.apply_speed_editor_event(ctx, event) {
                    self.status(format!("Speed Editor: {e}"));
                }
            } else if !self.speed_editor.virtual_input
                && let Event::Buttons(buttons) | Event::TimedButtons { buttons, .. } = event
            {
                // Suppress keys already held when focus returns until they are released.
                self.speed_editor.blocked_until_release = !buttons.is_empty();
                self.speed_editor.held = buttons;
            }
        }
        // Process queued second presses before expiring the first press's timer.
        if !monitoring
            && self.dialog.is_none()
            && self.native_dialog.is_none()
            && let Err(e) = self.flush_speed_editor_press(ctx, false)
        {
            self.status(format!("Speed Editor: {e}"));
        }
    }

    fn reset_speed_editor(&mut self, ctx: &egui::Context) {
        if self.speed_editor.shuttling
            && let Err(e) = self.speed_editor_command(ctx, "playback.stop", json!({}))
        {
            self.status(e);
        }
        if self.speed_editor.held.iter().any(|b| b.wheel_edit())
            && !self.session.state.edit_points.is_empty()
            && let Err(e) = self.speed_editor_command(ctx, "trim.clear", json!({}))
        {
            self.status(e);
        }
        self.speed_editor.shuttling = false;
        self.speed_editor.remainder = 0;
        self.speed_editor.secondary_held = None;
    }

    /// Apply one decoded input report. The control channel uses this exact path too.
    pub fn speed_editor_event(&mut self, ctx: &egui::Context, event: Event) -> Result<(), String> {
        if matches!(&event, Event::Buttons(buttons) | Event::TimedButtons { buttons, .. } if buttons.len() > 6) {
            return Err("a report can hold at most six buttons".into());
        }
        self.speed_editor.telemetry.record(&event, self.now());
        if self.ui.speed_editor_debugger.open
            && self.ui.speed_editor_debugger.monitor_only
            && matches!(event, Event::Buttons(_) | Event::TimedButtons { .. } | Event::Wheel { .. })
        {
            self.cancel_speed_editor_gestures();
            self.reset_speed_editor(ctx);
            if let Event::Buttons(buttons) | Event::TimedButtons { buttons, .. } = event {
                self.speed_editor.blocked_until_release = !buttons.is_empty();
                self.speed_editor.held = buttons;
            }
            return Ok(());
        }
        self.apply_speed_editor_event(ctx, event)
    }

    fn apply_speed_editor_event(&mut self, ctx: &egui::Context, event: Event) -> Result<(), String> {
        let now = event.observed_at(self.now());
        match event {
            Event::Connection { connected, detail } => {
                self.cancel_speed_editor_gestures();
                self.reset_speed_editor(ctx);
                self.speed_editor.held.clear();
                self.speed_editor.virtual_input = false;
                self.speed_editor.blocked_until_release = false;
                if self.speed_editor.detail != detail && (connected || self.speed_editor.connected || !detail.starts_with("No USB Speed Editor")) {
                    self.status(format!("Speed Editor: {detail}"));
                }
                self.speed_editor.connected = connected;
                self.speed_editor.detail = detail;
                if !connected {
                    self.speed_editor.battery = None;
                }
            }
            Event::Battery { charging, level } => {
                self.speed_editor.battery = Some(level.min(100));
                self.speed_editor.charging = charging;
            }
            Event::Buttons(mut buttons) | Event::TimedButtons { mut buttons, .. } => {
                if buttons.len() > 6 {
                    return Err("a report can hold at most six buttons".into());
                }
                let mut seen = Vec::with_capacity(6);
                buttons.retain(|b| {
                    if seen.contains(b) {
                        false
                    } else {
                        seen.push(*b);
                        true
                    }
                });
                if self.speed_editor.blocked_until_release {
                    self.cancel_speed_editor_gestures();
                    self.speed_editor.blocked_until_release = !buttons.is_empty();
                    self.speed_editor.held = buttons;
                    return Ok(());
                }
                let previous = std::mem::replace(&mut self.speed_editor.held, buttons.clone());
                if previous != buttons {
                    self.speed_editor.remainder = 0;
                }
                let mut first_error = None;
                for button in previous.iter().filter(|button| !buttons.contains(button)) {
                    self.speed_editor.gestures.taps.release(*button);
                }
                if buttons.len() > 1 {
                    self.speed_editor.gestures.taps.clear();
                }
                if self.speed_editor.secondary_held.is_some_and(|button| !buttons.contains(&button)) {
                    self.speed_editor.secondary_held = None;
                }
                for button in buttons {
                    if !previous.contains(&button)
                        && let Err(e) = self.speed_editor_key_down(ctx, button, now)
                    {
                        first_error.get_or_insert(e);
                    }
                }
                if previous.iter().any(|b| b.wheel_edit())
                    && !self.speed_editor.held.iter().any(|b| b.wheel_edit())
                    && !self.session.state.edit_points.is_empty()
                {
                    self.speed_editor_command(ctx, "trim.clear", json!({}))?;
                }
                if let Some(e) = first_error {
                    return Err(e);
                }
            }
            Event::Wheel { mode, value } => self.speed_editor_wheel(ctx, mode, value)?,
        }
        Ok(())
    }

    fn speed_editor_command(&mut self, ctx: &egui::Context, id: &str, mut params: Value) -> Result<Value, String> {
        let target = if self.speed_editor.source { "source" } else { "program" };
        let object = params.as_object_mut().ok_or("controller parameters must be an object")?;
        object.insert("monitor".into(), json!(target));
        object.insert("target".into(), json!(target));
        menus::invoke(self, ctx, id, params)
    }

    fn speed_editor_press(&mut self, ctx: &egui::Context, button: Button) -> Result<(), String> {
        use Button::*;
        let id = match button {
            Source | Timeline => {
                self.reset_speed_editor(ctx);
                self.speed_editor.source = button == Source;
                self.stop();
                self.stop_source();
                if button == Source && self.session.state.source_item.is_none() {
                    menus::invoke(self, ctx, "source.open", json!({}))?;
                }
                if button == Source { "window.panel.source" } else { "window.panel.timeline" }
            }
            Jog | Scroll | Shuttle => {
                self.reset_speed_editor(ctx);
                self.speed_editor.wheel_mode = match button {
                    Jog => WheelMode::Jog,
                    Scroll => WheelMode::Scroll,
                    _ => WheelMode::Shuttle,
                };
                return Ok(());
            }
            StopPlay => {
                self.speed_editor.shuttling = false;
                "playback.toggle"
            }
            In => "markers.markIn",
            Out => "markers.markOut",
            Cut | Split => "sequence.addEdit",
            RippleDelete => "edit.rippleDelete",
            Snap => "sequence.snap",
            FullView => "window.maximizeFrame",
            Dissolve => {
                menus::invoke(self, ctx, "sequence.applyVideoTransition", json!({"effect": "cross_dissolve"}))?;
                return Ok(());
            }
            Transition => "sequence.applyVideoTransition",
            TrimIn | TrimOut | Roll => {
                self.reset_speed_editor(ctx);
                self.stop();
                self.stop_source();
                let kind = match button {
                    TrimIn => "rippleIn",
                    TrimOut => "rippleOut",
                    _ => "roll",
                };
                self.speed_editor_command(ctx, "trim.selectNearest", json!({"kind": kind}))?;
                // The keyboard command finds one edge per targeted track. The dial should
                // trim the nearest actual cut and its links, not unrelated distant edges.
                let point = self
                    .session
                    .active_sequence()
                    .and_then(|seq| {
                        self.session
                            .state
                            .edit_points
                            .iter()
                            .filter_map(|point| {
                                let (_, item) = seq.find_item(point.clip)?;
                                let time = if point.out { item.end() } else { item.start };
                                Some((time.0.abs_diff(self.session.playhead().0), *point))
                            })
                            .min_by_key(|(distance, _)| *distance)
                            .map(|(_, point)| point)
                    })
                    .ok_or("No editable cut near the playhead")?;
                self.speed_editor_command(
                    ctx,
                    "trim.selectEditPoint",
                    json!({"clip":point.clip.0, "edge":if point.out {"out"} else {"in"},
                    "kind":if button == Roll {"roll"} else {"ripple"}}),
                )?;
                return Ok(());
            }
            SlipSource | SlipDestination | AudioLevel | TransitionDuration => {
                self.reset_speed_editor(ctx);
                return Ok(());
            }
            VideoOnly | AudioOnly => {
                if button == VideoOnly {
                    self.speed_editor.video_only = !self.speed_editor.video_only;
                    self.speed_editor.audio_only = false;
                } else {
                    self.speed_editor.audio_only = !self.speed_editor.audio_only;
                    self.speed_editor.video_only = false;
                }
                return Ok(());
            }
            SmartInsert | Append | PlaceOnTop => {
                return self.speed_editor_place(ctx, button, false);
            }
            Escape => {
                self.reset_speed_editor(ctx);
                self.stop();
                self.stop_source();
                if !self.session.state.edit_points.is_empty() {
                    self.speed_editor_command(ctx, "trim.clear", json!({}))?;
                }
                return Ok(());
            }
            Cam1 | Cam2 | Cam3 | Cam4 | Cam5 | Cam6 | Cam7 | Cam8 | Cam9 => {
                let n = button as u16 - Cam1 as u16 + 1;
                self.speed_editor_command(ctx, &format!("multicam.selectCamera{n}"), json!({}))?;
                return Ok(());
            }
            LiveOverwrite => "multicam.recordToggle",
            SyncBin => "multicam.toggleView",
            // These Resolve-specific operations have no equivalent in FilmCraft yet.
            _ => return Err(format!("{button:?} is not supported yet")),
        };
        self.speed_editor_command(ctx, id, json!({})).map(|_| ())
    }

    fn speed_editor_place(&mut self, ctx: &egui::Context, button: Button, whole_clip: bool) -> Result<(), String> {
        let item = self.session.state.source_item.ok_or("Open a Source clip first")?;
        let view = filmcraft_engine::clip_ops::source_view(&self.session, item).ok_or("Source clip is unavailable")?;
        let range = if whole_clip { filmcraft_time::TimeRange::new(view.start, view.end - view.start) } else { view.selected_range() };
        let at = if button == Button::Append { self.session.active_sequence().ok_or("Open a sequence first")?.duration() } else { self.session.playhead() };
        let mut params = json!({"item": item.0, "time": at.0, "sourceIn": range.start.0, "duration": range.duration.0,
            "insert": button == Button::SmartInsert, "video": !self.speed_editor.audio_only, "audio": !self.speed_editor.video_only});
        if button == Button::PlaceOnTop && !self.speed_editor.audio_only {
            params["track"] = json!("new");
        }
        menus::invoke(self, ctx, "timeline.place", params).map(|_| ())
    }

    fn speed_editor_wheel(&mut self, ctx: &egui::Context, mode: u8, value: i32) -> Result<(), String> {
        if self.speed_editor.blocked_until_release {
            return Ok(());
        }
        let feedback = self.speed_editor.feedback(self.session.state.snapping);
        // Discard reports still in flight from a previous hardware mode.
        if mode != feedback.wheel_mode && !(feedback.wheel_mode == 0 && mode == 2) {
            return Ok(());
        }
        if value != 0 {
            self.speed_editor.gestures.taps.clear();
            if self
                .speed_editor
                .gestures
                .pending
                .as_ref()
                .is_some_and(|pending| self.speed_editor.held.contains(&pending.button) && pending.button.wheel_edit())
            {
                self.speed_editor.gestures.pending = None;
            } else {
                self.flush_speed_editor_press(ctx, true)?;
            }
        }
        let sensitivity = self.session.prefs.general.speed_editor_sensitivity.clamp(SPEED_EDITOR_SENSITIVITY_MIN, SPEED_EDITOR_SENSITIVITY_MAX);
        if self.speed_editor.remainder_sensitivity != sensitivity {
            self.speed_editor.remainder = 0;
            self.speed_editor.remainder_sensitivity = sensitivity;
        }
        if feedback.wheel_mode == 3 {
            let speed =
                if value.unsigned_abs() <= 128 { 0.0 } else { (f64::from(value.clamp(-4096, 4096)) / 512.0 * f64::from(sensitivity) / 100.0).clamp(-8.0, 8.0) };
            self.speed_editor_command(ctx, "playback.shuttle", json!({"speed": speed}))?;
            self.speed_editor.shuttling = speed != 0.0;
            return Ok(());
        }
        // 8192 units/revolution; 64 frames/revolution at 100%. Accumulate integer
        // raw-units × percent so slow settings retain even one-unit movements.
        // Cap hostile reports to 10 revolutions before applying bounded gain.
        let delta = i64::from(value.clamp(-81920, 81920)).saturating_mul(i64::from(sensitivity));
        let total = self.speed_editor.remainder.saturating_add(delta);
        let frames = total / 12_800;
        self.speed_editor.remainder = total % 12_800;
        if frames == 0 {
            return Ok(());
        }
        self.reset_speed_editor_shuttle(ctx);
        if let Some(edit) = self.speed_editor.held.iter().copied().find(|b| b.wheel_edit()) {
            if self.speed_editor_secondary_wheel(ctx, edit, frames)? {
                return Ok(());
            }
            match edit {
                Button::TrimIn | Button::TrimOut | Button::Roll => {
                    self.speed_editor_command(ctx, "trim.nudge", json!({"frames": frames}))?;
                }
                _ => {
                    let clip = self.session.state.selection.first().ok_or("Select a clip to slip or slide")?.0;
                    let id = if edit == Button::SlipSource { "timeline.slip" } else { "timeline.slide" };
                    self.speed_editor_command(ctx, id, json!({"clip": clip, "deltaFrames": frames}))?;
                }
            }
            return Ok(());
        }
        let source = self.speed_editor.source;
        let rate = if source {
            let item = self.session.state.source_item.ok_or("Open a Source clip first")?;
            filmcraft_engine::clip_ops::source_view(&self.session, item).ok_or("Source clip is unavailable")?.rate
        } else {
            self.session.sequence_rate()
        };
        let frames = if self.speed_editor.wheel_mode == WheelMode::Scroll {
            frames.saturating_mul(rate.frame_at(Tick(filmcraft_time::TICKS_PER_SECOND)).max(1))
        } else {
            frames
        };
        self.stop();
        self.stop_source();
        if source {
            let frame = rate.frame_at(self.session.state.source_playhead).saturating_add(frames).clamp(0, rate.frame_at(Tick::MAX));
            self.speed_editor_command(ctx, "source.setPlayhead", json!({"time": rate.tick_of(frame).0}))?;
        } else {
            self.speed_editor_command(ctx, "playhead.step", json!({"frames": frames}))?;
            // Use the normal grain audio while jogging the timeline.
            self.scrub_tick(self.now(), true);
        }
        Ok(())
    }

    fn reset_speed_editor_shuttle(&mut self, ctx: &egui::Context) {
        if self.speed_editor.shuttling
            && let Err(e) = self.speed_editor_command(ctx, "playback.stop", json!({}))
        {
            self.status(e);
        }
        if self.speed_editor.shuttling {
            self.speed_editor.shuttling = false;
        }
    }
}

/// Automation entry points. Input is either a key edge or a raw wheel report; validation
/// completes before changing held keys, marks, playback or the project.
pub fn route(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if let Some(result) = debugger::route(app, ctx, id, params) {
        return Some(result);
    }
    if id == "speedEditor.status" {
        return Some(serde_json::to_value(&app.speed_editor).map_err(|e| e.to_string()));
    }
    if id != "speedEditor.input" {
        return None;
    }
    Some((|| {
        let event = if let Some(value) = params.get("wheel") {
            if params.get("button").is_some() {
                return Err("provide a button or wheel, not both".into());
            }
            let value = value.as_i64().and_then(|v| i32::try_from(v).ok()).ok_or("wheel must be a signed 32-bit integer")?;
            let mode = params
                .get("mode")
                .map_or(Some(app.speed_editor.feedback(app.session.state.snapping).wheel_mode), |v| {
                    v.as_u64().and_then(|v| u8::try_from(v).ok()).filter(|v| *v <= 3)
                })
                .ok_or("wheel mode must be an integer from 0 to 3")?;
            Event::Wheel { mode, value }
        } else {
            let button: Button = serde_json::from_value(params.get("button").cloned().ok_or("provide button or wheel")?).map_err(|e| e.to_string())?;
            let pressed = params.get("pressed").and_then(Value::as_bool).ok_or("pressed must be a boolean")?;
            let mut held = app.speed_editor.held.clone();
            held.retain(|b| *b != button);
            if pressed {
                held.push(button);
            }
            if held.len() > 6 {
                return Err("at most six buttons can be held".into());
            }
            Event::Buttons(held)
        };
        app.speed_editor.virtual_input = true;
        app.speed_editor_event(ctx, event)?;
        serde_json::to_value(&app.speed_editor).map_err(|e| e.to_string())
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_engine::Session;

    fn app() -> FilmcraftApp {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        FilmcraftApp::new(session)
    }
    fn keys(app: &mut FilmcraftApp, buttons: &[Button]) -> Result<(), String> {
        app.speed_editor_event(&egui::Context::default(), Event::Buttons(buttons.to_vec()))
    }
    fn tap(app: &mut FilmcraftApp, button: Button) {
        keys(app, &[button]).unwrap();
        keys(app, &[]).unwrap();
        app.set_now_for_test(app.now() + gestures::DOUBLE_TAP_SECONDS + 0.01);
        app.flush_speed_editor_press(&egui::Context::default(), false).unwrap();
    }
    fn wheel(app: &mut FilmcraftApp, mode: u8, value: i32) {
        app.speed_editor_event(&egui::Context::default(), Event::Wheel { mode, value }).unwrap();
    }

    #[test]
    fn cuts_are_edges_not_repeats_and_undo_redo_is_exact() {
        let mut app = app();
        let before = app.session.project.to_json();
        app.session.execute("playhead.set", json!({"frame": 24})).unwrap();
        keys(&mut app, &[Button::Cut, Button::Cut]).unwrap();
        let once = app.session.project.to_json();
        assert_ne!(once, before);
        app.session.execute("playhead.set", json!({"frame": 48})).unwrap();
        keys(&mut app, &[Button::Cut]).unwrap();
        assert_eq!(app.session.project.to_json(), once, "holding Split must not cut at the new playhead");
        keys(&mut app, &[]).unwrap();
        tap(&mut app, Button::Cut);
        let twice = app.session.project.to_json();
        assert_ne!(twice, once);
        app.session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.project.to_json(), once);
        app.session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.project.to_json(), before);
        app.session.execute("edit.redo", json!({})).unwrap();
        assert_eq!(app.session.project.to_json(), once);
    }

    #[test]
    fn jog_preserves_fractional_motion_direction_and_bounds_hostile_values() {
        let mut app = app();
        app.session.execute("playhead.set", json!({"frame": 100})).unwrap();
        wheel(&mut app, 0, 65);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 100);
        wheel(&mut app, 0, 63);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 101);
        wheel(&mut app, 0, -64);
        wheel(&mut app, 0, -64);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 100);
        wheel(&mut app, 0, i32::MIN);
        assert_eq!(app.session.playhead(), Tick::ZERO);
        wheel(&mut app, 0, i32::MAX);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 640);
        let before = app.session.playhead();
        wheel(&mut app, 3, i32::MAX);
        assert_eq!(app.session.playhead(), before, "stale reports from another mode are ignored");
    }

    #[test]
    fn sensitivity_scales_both_directions_and_retains_tiny_movements() {
        let mut app = app();
        app.session.execute("prefs.set", json!({"key":"general.speedEditorSensitivity","value":25})).unwrap();
        app.session.execute("playhead.set", json!({"frame":100})).unwrap();
        for _ in 0..3 {
            wheel(&mut app, 0, 128);
        }
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 100);
        wheel(&mut app, 0, 128);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 101);
        wheel(&mut app, 0, -512);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 100);
        app.session.execute("prefs.set", json!({"key":"general.speedEditorSensitivity","value":1})).unwrap();
        wheel(&mut app, 0, 1);
        wheel(&mut app, 0, 12798);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 100);
        wheel(&mut app, 0, 1);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 101);
        app.session.execute("prefs.set", json!({"key":"general.speedEditorSensitivity","value":200})).unwrap();
        wheel(&mut app, 0, 128);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 103);
    }

    #[test]
    fn changing_sensitivity_discards_old_fractional_motion() {
        let mut app = app();
        app.session.execute("prefs.set", json!({"key":"general.speedEditorSensitivity","value":25})).unwrap();
        app.session.execute("playhead.set", json!({"frame":100})).unwrap();
        wheel(&mut app, 0, 256);
        app.session.execute("prefs.set", json!({"key":"general.speedEditorSensitivity","value":100})).unwrap();
        wheel(&mut app, 0, 64);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 100);
        wheel(&mut app, 0, 64);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 101);
    }

    #[test]
    fn sensitivity_applies_to_source_scroll_trim_and_shuttle() {
        let mut app = app();
        app.session.execute("prefs.set", json!({"key":"general.speedEditorSensitivity","value":25})).unwrap();
        app.session.execute("source.open", json!({"item":5})).unwrap();
        tap(&mut app, Button::Source);
        let program = app.session.playhead();
        let rate = filmcraft_engine::clip_ops::source_view(&app.session, filmcraft_project::ItemId(5)).unwrap().rate;
        wheel(&mut app, 0, 512);
        assert_eq!(rate.frame_at(app.session.state.source_playhead), 1);
        tap(&mut app, Button::Scroll);
        wheel(&mut app, 0, 512);
        assert_eq!(rate.frame_at(app.session.state.source_playhead), 1 + rate.frame_at(Tick(filmcraft_time::TICKS_PER_SECOND)));
        assert_eq!(app.session.playhead(), program);
        tap(&mut app, Button::Timeline);
        tap(&mut app, Button::Jog);
        app.session.execute("timeline.setTrack", json!({"track":"A2","syncLock":false})).unwrap();
        app.session.execute("playhead.set", json!({"frame":48})).unwrap();
        tap(&mut app, Button::Split);
        keys(&mut app, &[Button::TrimOut]).unwrap();
        let before = app.session.project.to_json();
        wheel(&mut app, 0, -128);
        assert_eq!(app.session.project.to_json(), before);
        wheel(&mut app, 0, -384);
        assert_ne!(app.session.project.to_json(), before);
        keys(&mut app, &[]).unwrap();
        app.session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.project.to_json(), before);
        tap(&mut app, Button::Shuttle);
        wheel(&mut app, 3, 1024);
        assert_eq!(app.playback.speed, 0.5);
        app.session.execute("prefs.set", json!({"key":"general.speedEditorSensitivity","value":200})).unwrap();
        wheel(&mut app, 3, i32::MAX);
        assert_eq!(app.playback.speed, 8.0);
        wheel(&mut app, 3, i32::MIN);
        assert_eq!(app.playback.speed, -8.0);
        wheel(&mut app, 3, 0);
        assert!(!app.playback.playing);
    }

    #[test]
    fn source_and_timeline_marks_and_jog_are_independent_of_widget_focus() {
        let mut app = app();
        app.session.execute("source.open", json!({"item": 5})).unwrap();
        app.session.execute("playhead.set", json!({"frame": 48})).unwrap();
        let program = app.session.playhead();
        tap(&mut app, Button::Source);
        app.ui.focused = crate::dock::PanelKind::Timeline;
        wheel(&mut app, 0, 128);
        assert_eq!(app.session.playhead(), program);
        assert!(app.session.state.source_playhead > Tick::ZERO);
        tap(&mut app, Button::In);
        let source_marks = app.session.execute("source.inspect", json!({})).unwrap();
        assert_eq!(source_marks["markIn"], json!(app.session.state.source_playhead.0));
        tap(&mut app, Button::Timeline);
        app.ui.focused = crate::dock::PanelKind::Source;
        tap(&mut app, Button::In);
        assert_eq!(app.session.active_sequence().unwrap().mark_in, Some(program));
        wheel(&mut app, 0, 128);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 49);
        tap(&mut app, Button::Out);
        assert_eq!(app.session.active_sequence().unwrap().mark_out, Some(app.session.playhead()));
    }

    #[test]
    fn scroll_is_faster_and_shuttle_centres_and_disconnects_safely() {
        let mut app = app();
        tap(&mut app, Button::Scroll);
        wheel(&mut app, 0, 128);
        assert!(app.session.sequence_rate().frame_at(app.session.playhead()) >= 24);
        tap(&mut app, Button::Shuttle);
        assert_eq!(app.speed_editor.feedback(false).wheel_mode, 3);
        wheel(&mut app, 3, 1024);
        assert!(app.playback.playing);
        assert_eq!(app.playback.speed, 2.0);
        wheel(&mut app, 3, -512);
        assert_eq!(app.playback.speed, -1.0);
        wheel(&mut app, 3, 0);
        assert!(!app.playback.playing);
        wheel(&mut app, 3, 512);
        app.speed_editor_event(&egui::Context::default(), Event::Connection { connected: false, detail: "Unplugged".into() }).unwrap();
        assert!(!app.playback.playing);
        assert!(app.speed_editor.held.is_empty());
        assert_eq!(app.speed_editor.remainder, 0);
    }

    #[test]
    fn shuttle_reports_preserve_preroll_and_running_clock() {
        let mut app = app();
        let ctx = egui::Context::default();
        app.session.execute("file.newSequence", json!({"fps":24,"width":16,"height":16})).unwrap();
        app.session.execute("timeline.place", json!({"item":5,"track":"V1","time":0})).unwrap();
        app.session.execute("playhead.set", json!({"frame":100})).unwrap();
        tap(&mut app, Button::Shuttle);
        wheel(&mut app, 3, 512);
        app.set_now_for_test(1.0);
        app.advance_playback(&ctx);
        assert_eq!(app.playback.preroll, Some(1.0));
        app.playback.preroll_ready = true;
        wheel(&mut app, 3, 1024);
        assert_eq!(app.playback.preroll, Some(1.0), "dial reports must not restart a pending preroll");
        assert!(app.playback.preroll_ready);
        app.advance_playback(&ctx);
        assert!(app.playback.preroll.is_none());
        let rate = app.session.sequence_rate();
        let mut expected = app.session.playhead();
        for (now, previous_speed, value) in [(1.25, 2.0, 512), (1.5, 1.0, 512), (1.75, 1.0, 1024)] {
            app.set_now_for_test(now);
            wheel(&mut app, 3, value);
            assert!(app.playback.preroll.is_none(), "running shuttle must not enter preroll again");
            app.advance_playback(&ctx);
            expected = rate.snap(expected + Tick::from_seconds_f64(0.25 * previous_speed));
            assert_eq!(app.session.playhead(), expected);
        }
        app.set_now_for_test(2.0);
        app.advance_playback(&ctx);
        assert_eq!(app.session.playhead(), expected + rate.tick_of(12));
        wheel(&mut app, 3, -512);
        assert!(app.playback.preroll.is_some(), "direction changes still prepare new frames");
        assert_eq!(app.playback.speed, -1.0);
    }

    #[test]
    fn held_trim_uses_wheel_and_release_exits_trim_mode_with_undo() {
        let mut app = app();
        app.session.execute("timeline.setTrack", json!({"track":"A2", "syncLock":false})).unwrap();
        app.session.execute("playhead.set", json!({"frame": 48})).unwrap();
        tap(&mut app, Button::Split);
        let before = app.session.project.to_json();
        keys(&mut app, &[Button::TrimOut]).unwrap();
        assert!(!app.session.state.edit_points.is_empty());
        wheel(&mut app, 0, -128);
        assert_ne!(app.session.project.to_json(), before);
        keys(&mut app, &[]).unwrap();
        assert!(app.session.state.edit_points.is_empty());
        app.session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.project.to_json(), before);
    }

    #[test]
    fn source_append_honours_marks_and_video_only_and_is_one_undo_step() {
        let mut app = app();
        app.session.execute("source.open", json!({"item": 5})).unwrap();
        tap(&mut app, Button::Source);
        tap(&mut app, Button::In);
        wheel(&mut app, 0, 1280);
        tap(&mut app, Button::Out);
        tap(&mut app, Button::VideoOnly);
        let before = app.session.project.to_json();
        let duration = app.session.active_sequence().unwrap().duration();
        let audio = app.session.active_sequence().unwrap().audio_tracks.clone();
        tap(&mut app, Button::Append);
        assert!(app.session.active_sequence().unwrap().duration() > duration);
        assert_eq!(serde_json::to_value(&app.session.active_sequence().unwrap().audio_tracks).unwrap(), serde_json::to_value(audio).unwrap());
        app.session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.project.to_json(), before);
    }

    #[test]
    fn invalid_automation_parameters_leave_project_and_controller_unchanged() {
        let mut app = app();
        let ctx = egui::Context::default();
        let before = app.session.project.to_json();
        let state = serde_json::to_value(&app.speed_editor).unwrap();
        for params in [
            json!({}),
            json!({"button":"bogus","pressed":true}),
            json!({"button":"split","pressed":1}),
            json!({"wheel":i64::MAX}),
            json!({"wheel":1.5}),
            json!({"wheel":1,"mode":4}),
            json!({"wheel":1,"button":"split"}),
        ] {
            assert!(menus::invoke(&mut app, &ctx, "speedEditor.input", params).is_err());
            assert_eq!(serde_json::to_value(&app.speed_editor).unwrap(), state);
            assert_eq!(app.session.project.to_json(), before);
        }
        for speed in [json!(null), json!("fast"), json!(9), json!(-9)] {
            assert!(menus::invoke(&mut app, &ctx, "playback.shuttle", json!({"speed": speed})).is_err());
            assert!(!app.playback.playing);
        }
        assert!(keys(&mut app, &[Button::SmoothCut]).unwrap_err().contains("not supported"));
        assert_eq!(app.session.project.to_json(), before);
    }

    #[test]
    fn physical_input_pauses_for_focus_and_dialogs_and_requires_release() {
        use std::{cell::RefCell, rc::Rc};
        let mut app = app();
        app.session.execute("playhead.set", json!({"frame":24})).unwrap();
        let before = app.session.project.to_json();
        let pending = Rc::new(RefCell::new(vec![Event::Buttons(vec![Button::Cut])]));
        let input = pending.clone();
        app.hooks.speed_editor = Some(Box::new(move |_| input.borrow_mut().drain(..).collect()));
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput { focused: false, ..Default::default() }, |ui| app.poll_speed_editor(ui.ctx())).textures_delta.clear();
        assert_eq!(app.session.project.to_json(), before);
        pending.borrow_mut().extend([Event::Buttons(vec![Button::Cut]), Event::Wheel { mode: 0, value: 128 }]);
        ctx.run_ui(egui::RawInput { focused: true, ..Default::default() }, |ui| app.poll_speed_editor(ui.ctx())).textures_delta.clear();
        assert_eq!(app.session.project.to_json(), before);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 24);
        pending.borrow_mut().extend([Event::Buttons(vec![]), Event::Buttons(vec![Button::Cut])]);
        ctx.run_ui(egui::RawInput { focused: true, ..Default::default() }, |ui| app.poll_speed_editor(ui.ctx())).textures_delta.clear();
        assert_ne!(app.session.project.to_json(), before);
        let cut = app.session.project.to_json();
        app.dialog = Some(crate::Dialog::About);
        app.session.execute("playhead.set", json!({"frame":48})).unwrap();
        pending.borrow_mut().extend([Event::Buttons(vec![]), Event::Buttons(vec![Button::Cut])]);
        ctx.run_ui(egui::RawInput { focused: true, ..Default::default() }, |ui| app.poll_speed_editor(ui.ctx())).textures_delta.clear();
        assert_eq!(app.session.project.to_json(), cut);
    }

    #[test]
    fn control_channel_can_hold_a_trim_key_across_unfocused_logic_passes() {
        let mut app = app();
        let ctx = egui::Context::default();
        app.session.execute("timeline.setTrack", json!({"track":"A2", "syncLock":false})).unwrap();
        app.session.execute("playhead.set", json!({"frame":48})).unwrap();
        tap(&mut app, Button::Split);
        app.hooks.speed_editor = Some(Box::new(|_| Vec::new()));
        let before = app.session.project.to_json();
        menus::invoke(&mut app, &ctx, "speedEditor.input", json!({"button":"trimOut","pressed":true})).unwrap();
        ctx.run_ui(egui::RawInput { focused: false, ..Default::default() }, |ui| app.poll_speed_editor(ui.ctx())).textures_delta.clear();
        assert!(!app.session.state.edit_points.is_empty());
        menus::invoke(&mut app, &ctx, "speedEditor.input", json!({"wheel":-128})).unwrap();
        assert_ne!(app.session.project.to_json(), before);
        menus::invoke(&mut app, &ctx, "speedEditor.input", json!({"button":"trimOut","pressed":false})).unwrap();
        assert!(app.session.state.edit_points.is_empty());
    }
}
