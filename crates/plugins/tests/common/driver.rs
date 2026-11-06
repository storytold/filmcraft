//! A real `FilmcraftApp` in a kittest harness with a real plug-in host, driven through the
//! control channel the way an agent drives the running app.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_plugins::{HostConfig, PluginHost};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

pub struct Driver {
    pub harness: Harness<'static, FilmcraftApp>,
    pub tx: Sender<ControlRequest>,
    pub host: Arc<PluginHost>,
}

impl Driver {
    pub fn new() -> Self {
        Driver::with_config(HostConfig::default())
    }

    /// Both gate clocks are frozen, so a busy machine cannot trip the 2 ms gate budget.
    pub fn with_config(mut cfg: HostConfig) -> Self {
        let clock = Arc::new(super::Frozen);
        cfg.clock.get_or_insert_with(|| clock.clone());
        let mut session = Session::default();
        session.clock = clock;
        session.execute("file.openDemoProject", json!({})).unwrap();
        let host = PluginHost::start(&mut session, cfg).unwrap();
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        app.hooks.open_path = Some(Box::new(|_, _| Ok(())));
        let per_frame_host = Arc::clone(&host);
        app.hooks.per_frame = Some(Box::new(move |ctx, session| filmcraft_plugins::ui::frame(&per_frame_host, ctx, session)));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, host };
        d.frames(4);
        d
    }

    pub fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    /// The raw reply of a control request.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
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

    pub fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    /// `engine.execute` that must succeed.
    pub fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    pub fn key(&mut self, key: &str, ctrl: bool) {
        self.ok("ui.key", json!({"key": key, "ctrl": ctrl}));
        self.frames(4);
    }

    pub fn session(&self) -> &Session {
        &self.harness.state().session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.harness.state_mut().session
    }

    pub fn bytes(&self) -> Vec<u8> {
        super::project_bytes(self.session())
    }

    /// (project bytes, revision, undo steps, redo steps)
    pub fn doc(&self) -> (Vec<u8>, u64, usize, usize) {
        let s = self.session();
        (self.bytes(), s.revision, s.history.undo.len(), s.history.redo.len())
    }

    /// Toast messages since the last call.
    pub fn notices(&mut self) -> Vec<String> {
        self.frames(2);
        let events = self.session_mut().drain_events();
        events
            .into_iter()
            .filter_map(|e| match e {
                filmcraft_engine::Event::Toast { message, .. } => Some(message),
                _ => None,
            })
            .collect()
    }
}
