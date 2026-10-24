//! A lost graphics device must not take the app down (AGENTS.md §0). wgpu's device-lost and
//! uncaptured-error callbacks flag the device (`gpu_health`); the app then draws an empty frame
//! (nothing to upload, so egui-wgpu cannot panic for want of a staging buffer) and asks the host
//! to recreate the window, which shows "Graphics device lost; rendering in software. Save your
//! work." and keeps answering the control channel. The desktop host's half (catching a panic
//! that still escapes the paint path, the new window) is covered by
//! `apps/filmcraft/tests/graphics_loss.rs`.

use std::sync::mpsc::{Sender, channel};

use eframe::egui_wgpu::{RenderState, RendererOptions, ScreenDescriptor};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::graphics_notice::{GraphicsNotice, GraphicsRecovery};
use serde_json::{Value, json};

const MESSAGE: &str = "Graphics device lost; rendering in software. Save your work.";

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// An app on the demo project, drawing through `rs` when given one (as the desktop host does).
    fn new(rs: Option<RenderState>) -> Self {
        filmcraft_ui_egui::crash::install(None);
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        if let Some(rs) = rs {
            app.set_wgpu(rs);
        }
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params}: {v}");
        v["result"].clone()
    }

    fn elements(&mut self, prefix: &str) -> Vec<(String, String)> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().map(|e| (e["id"].as_str().unwrap().to_string(), e["label"].as_str().unwrap_or_default().to_string())).collect()
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// What the desktop host does between the lost window and the new one.
    fn recreate_window(&mut self, software: bool) -> String {
        let reason = self.app().take_graphics_restart().expect("the app asked for a new window");
        self.app().detach_window();
        let mode = if software { "software rendering" } else { "default graphics" };
        let info = GraphicsRecovery { reason: reason.clone(), mode: mode.into(), software, at: "2026-10-19 10:00:00".into(), panicked: false };
        self.app().graphics_recovered(GraphicsNotice::device_lost(mode, &reason, software), info);
        self.frames(4);
        reason
    }
}

/// A real wgpu device for the tests that need one, or `None` on a machine without an adapter.
fn render_state() -> Option<RenderState> {
    let made = std::panic::catch_unwind(|| egui_kittest::wgpu::create_render_state(egui_kittest::wgpu::default_wgpu_setup(), RendererOptions::PREDICTABLE));
    match made {
        Ok(rs) => Some(rs),
        Err(_) => {
            eprintln!("SKIPPED: no wgpu adapter on this machine");
            None
        }
    }
}

/// Upload `primitives` through egui-wgpu's renderer on `rs`, as the window's paint does.
fn upload(rs: &RenderState, primitives: &[egui::epaint::ClippedPrimitive]) -> Result<(), String> {
    let mut encoder = rs.device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor { label: Some("test") });
    let screen = ScreenDescriptor { size_in_pixels: [1600, 980], pixels_per_point: 1.0 };
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rs.renderer.write().update_buffers(&rs.device, &rs.queue, &mut encoder, primitives, &screen);
    }));
    ran.map_err(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default())
}

/// The regression: on a device that refuses allocations, the frame the app used to draw panicked
/// inside egui-wgpu ("Failed to create staging buffer for index data"); now the app flags the
/// loss and draws nothing, and that empty frame uploads without a panic.
#[test]
fn a_destroyed_device_gets_an_empty_frame_instead_of_the_staging_buffer_panic() {
    let Some(rs) = render_state() else { return };
    let mut d = Driver::new(Some(rs.clone()));
    assert!(d.app().gpu_device().is_some(), "the app draws through the device");
    assert!(!d.harness.output().shapes.is_empty(), "the demo project draws a window");
    assert!(d.app().graphics_restart_pending().is_none());
    // a frame as the app drew it before the loss, to upload later
    let before = d.harness.ctx.tessellate(d.harness.output().shapes.clone(), 1.0);
    assert!(!before.is_empty());

    let r = d.ok("ui.injectGraphicsFault", json!({"kind": "destroyDevice"}));
    assert_eq!(r["kind"], "destroyDevice");
    d.frames(3);
    assert!(d.app().gpu_health.is_lost(), "the device-lost callback flagged the device");
    let pending = d.app().graphics_restart_pending().map(str::to_string);
    assert!(pending.as_deref().is_some_and(|p| p.contains("Destroyed")), "{pending:?}");
    assert!(d.harness.output().shapes.is_empty(), "a lost device gets an empty frame");
    assert!(d.app().gpu.is_none(), "the compositor let go of the dead device");

    // what the old frame did on that device, and what the empty one does
    let old = upload(&rs, &before).expect_err("the pre-fix frame panics on a device without allocations");
    assert!(old.contains("Failed to create staging buffer"), "{old}");
    upload(&rs, &[]).expect("the empty frame uploads nothing and survives");

    // the host recreates the window; the app shows the message and answers again
    let reason = d.recreate_window(true);
    assert!(reason.contains("Destroyed"), "{reason}");
    let els = d.elements("graphics.notice");
    assert!(els.iter().any(|(id, label)| id == "graphics.notice" && label == MESSAGE), "{els:?}");
    assert!(d.app().graphics_restart_pending().is_none());
    assert!(!d.app().gpu_health.is_lost(), "the new window starts with a healthy device");
    assert!(!d.harness.output().shapes.is_empty(), "the new window draws again");
    assert_eq!(d.ok("ui.inspect", json!({}))["playback"]["playing"], json!(false));
}

/// An out-of-memory error from the uncaptured-error handler is a loss too (the device cannot give
/// egui-wgpu its buffers). The recreated window shows the message, Help ▸ Graphics Recovery…
/// explains it, and the control channel keeps working. Needs no GPU.
#[test]
fn the_recreated_window_shows_the_message_and_keeps_answering() {
    let mut d = Driver::new(None);
    d.ok("engine.execute", json!({"command": "playhead.set", "params": {"seconds": 1}}));
    d.ok("ui.playback", json!({"action": "play"}));
    let r = d.ok("ui.injectGraphicsFault", json!({"kind": "outOfMemory", "reason": "CUDA helpers took the VRAM"}));
    assert_eq!(r["lost"], json!(true));
    d.frames(3);
    let pending = d.app().graphics_restart_pending().map(str::to_string).expect("restart requested");
    assert!(pending.contains("out of GPU memory") && pending.contains("CUDA helpers"), "{pending}");
    assert!(d.harness.output().shapes.is_empty(), "nothing is drawn on the lost device");
    assert!(!d.app().playback.playing, "playback stopped");
    // the host's turn
    let reason = d.recreate_window(true);
    assert_eq!(d.app().ui.status, MESSAGE);
    let els = d.elements("graphics.notice");
    for id in ["graphics.notice", "graphics.notice.save", "graphics.notice.close"] {
        assert!(els.iter().any(|(i, _)| i == id), "{id} in {els:?}");
    }
    assert!(els.iter().any(|(id, label)| id == "graphics.notice" && label == MESSAGE), "{els:?}");
    // Help ▸ Graphics Recovery… tells the story
    let r = d.ok("ui.menu.invoke", json!({"id": "help.graphicsRecovery"}));
    d.frames(2);
    assert_eq!(r["dialog"], "graphicsRecovery");
    assert_eq!(r["report"]["recovered"], json!(true));
    assert_eq!(r["report"]["reason"], json!(reason));
    assert_eq!(r["report"]["software"], json!(true));
    assert!(d.elements("graphicsRecovery").iter().any(|(id, _)| id == "graphicsRecovery.ok"));
    d.ok("ui.click", json!({"id": "graphicsRecovery.ok"}));
    d.frames(3);
    assert!(d.app().ui.extras.dialog.is_none());
    // the notice closes, the session is the same one
    d.ok("ui.click", json!({"id": "graphics.notice.close"}));
    d.frames(3);
    assert!(d.elements("graphics.notice").is_empty());
    assert_eq!(d.ok("ui.inspect", json!({}))["playback"]["playing"], json!(false));
    assert!(d.app().session.active_sequence().is_some(), "the demo sequence is still open");
}

/// Without a loss, Help ▸ Graphics Recovery… says so, and the menu lists it under Help.
#[test]
fn graphics_recovery_without_a_loss_and_the_menu_entry() {
    let mut d = Driver::new(None);
    let menus = d.ok("ui.menu.list", json!({}));
    let item = menus.as_array().unwrap().iter().find(|m| m["id"] == "help.graphicsRecovery").expect("Help ▸ Graphics Recovery…");
    assert_eq!(item["path"], json!(["Help"]));
    let r = d.ok("ui.menu.invoke", json!({"id": "help.graphicsRecovery"}));
    d.frames(2);
    assert_eq!(r["report"]["recovered"], json!(false));
    assert!(r["report"]["logs"].as_str().is_some_and(|p| !p.is_empty()));
    assert!(d.elements("graphicsRecovery").iter().any(|(id, _)| id == "graphicsRecovery.ok"));
}

/// A device lost on a platform without a software adapter (macOS) is recreated on the default
/// device; the message says so instead of promising software rendering.
#[test]
fn the_message_without_a_software_adapter() {
    let mut d = Driver::new(None);
    d.ok("ui.injectGraphicsFault", json!({"kind": "deviceLost"}));
    d.frames(2);
    d.recreate_window(false);
    let els = d.elements("graphics.notice");
    assert!(els.iter().any(|(id, label)| id == "graphics.notice" && label == "Graphics device lost; the window was recreated. Save your work."), "{els:?}");
}

/// The empty-frame path must not need a wgpu device at all (a window without one, the web).
#[test]
fn unknown_fault_kinds_and_no_device_are_errors_not_panics() {
    let mut d = Driver::new(None);
    let v = d.call("ui.injectGraphicsFault", json!({"kind": "destroyDevice"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    let v = d.call("ui.injectGraphicsFault", json!({"kind": "meltdown"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(d.app().graphics_restart_pending().is_none());
    assert!(!d.harness.output().shapes.is_empty());
}
