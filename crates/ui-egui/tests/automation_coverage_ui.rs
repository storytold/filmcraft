//! Every control on screen has an automation id an agent can click: `ui.coverage` reports nothing
//! uncovered in any workspace, with any panel shown (a clip selected, so the clip panels show their
//! controls), or in a dialog. Panels name their own controls (`effectControls.motion.position.x`);
//! standard egui widgets they leave unnamed get `auto.…` ids from the accessibility tree. The real
//! `FilmcraftApp` under `egui_kittest`, driven over the control channel.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::dock::{PanelKind, WORKSPACES};
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project, its sequence open, the first picture clip selected.
    fn demo() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
        s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(6);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks and keys enter through the input hook
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params);
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    /// Controls `ui.coverage` finds without an id, described for a failure message.
    fn gaps(&mut self, state: &str) -> Vec<String> {
        self.frames(4);
        let c = self.ok("ui.coverage", json!({}));
        assert!(c["interactive"].as_u64().unwrap_or(0) > 10, "{state}: nothing interactive on screen? {c}");
        c["uncovered"].as_array().cloned().unwrap_or_default().iter().map(|u| format!("{state}: {u}")).collect()
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        self.frames(2);
        self.ok("ui.elements", json!({"prefix": prefix}))
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|e| e["id"].as_str().map(String::from))
            .collect()
    }
}

#[test]
fn every_control_in_every_workspace_and_panel_has_an_id() {
    let mut d = Driver::demo();
    let mut gaps = Vec::new();
    for w in WORKSPACES {
        d.ok("ui.set", json!({"workspace": w}));
        gaps.extend(d.gaps(&format!("workspace {w}")));
    }
    d.ok("ui.set", json!({"workspace": "Editing"}));
    for p in PanelKind::ALL {
        d.ok("ui.panel.show", json!({"panel": p.title()}));
        gaps.extend(d.gaps(&format!("panel {}", p.title())));
    }
    assert!(gaps.is_empty(), "{} controls an agent can reach only by coordinates:\n{}", gaps.len(), gaps.join("\n"));
}

/// The clip panels name each value after its effect and parameter, the ids `effects.setParam`
/// uses, so an agent can find the field it wants without a screenshot.
#[test]
fn clip_panels_name_values_after_effect_and_parameter() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Effect Controls"}));
    let ec = d.ids("effectControls.");
    for want in [
        "effectControls.motion.position.x",
        "effectControls.motion.position.y",
        "effectControls.motion.scale.value",
        "effectControls.effect.motion.enabled",
        "effectControls.effect.motion.reset",
    ] {
        assert!(ec.iter().any(|i| i == want), "missing {want} in {ec:?}");
    }
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    let props = d.ids("properties.");
    for want in ["properties.section.Transform", "properties.motion.position.x", "properties.motion.scale.value", "properties.opacity.opacity.value"] {
        assert!(props.iter().any(|i| i == want), "missing {want} in {props:?}");
    }
    for want in ["app.titleBar", "app.statusBar", "timeline.track.V1.header"] {
        assert!(!d.ids(want).is_empty(), "missing {want}");
    }
}

/// An id named from the accessibility tree is clickable like any other: clicking the History
/// panel's first row goes back to the opened project.
#[test]
fn named_rows_are_clickable() {
    let mut d = Driver::demo();
    d.ok("engine.execute", json!({"command": "sequence.addEdit", "params": {"seconds": 1}}));
    d.ok("ui.panel.show", json!({"panel": "History"}));
    assert!(!d.ids("history.step.").is_empty(), "the edit is listed");
    let undo = d.harness.state().session.history.undo.len();
    assert!(undo > 0);
    d.ok("ui.click", json!({"id": "history.open"}));
    d.frames(3);
    assert_eq!(d.harness.state().session.history.undo.len(), 0, "clicking Open undoes every step");
}

/// A History list longer than its panel offers only the rows in view, each where it shows: a
/// click on any offered id lands on that row.
#[test]
fn only_visible_history_rows_are_offered() {
    let mut d = Driver::demo();
    for n in 0..40 {
        d.ok("engine.execute", json!({"command": "sequence.addEdit", "params": {"seconds": 0.5 + f64::from(n) * 0.1}}));
    }
    d.ok("ui.panel.show", json!({"panel": "History"}));
    d.frames(4);
    let panel = d.ok("ui.elements", json!({"prefix": "panel.History"})).as_array().cloned().unwrap_or_default();
    let p = panel.iter().find(|e| e["id"] == "panel.History").expect("the History panel is shown")["rect"].clone();
    let (px, py, pw, ph) = (p[0].as_f64().unwrap(), p[1].as_f64().unwrap(), p[2].as_f64().unwrap(), p[3].as_f64().unwrap());
    let rows = d.ok("ui.elements", json!({"prefix": "history."})).as_array().cloned().unwrap_or_default();
    assert!(!rows.is_empty() && rows.len() < 41, "only some of the 41 rows fit: {}", rows.len());
    for r in rows {
        let (x, y, w, h) = (r["rect"][0].as_f64().unwrap(), r["rect"][1].as_f64().unwrap(), r["rect"][2].as_f64().unwrap(), r["rect"][3].as_f64().unwrap());
        assert!(x >= px - 1.0 && y >= py - 1.0 && x + w <= px + pw + 1.0 && y + h <= py + ph + 1.0, "{} lies outside the panel: {r}", r["id"]);
    }
}

/// A dialog's controls all have ids; the standard ones are named after their labels, scoped to the
/// dialog (its title bar, which only moves it, counts as background).
#[test]
fn dialog_controls_have_ids() {
    let mut d = Driver::demo();
    let mut gaps = Vec::new();
    for command in ["file.newSequence", "sequence.settings", "clip.audioGain", "sequence.addTracks", "sequence.deleteTracks"] {
        d.ok("ui.menu.invoke", json!({"id": command}));
        d.frames(3);
        assert!(!d.ok("ui.inspect", json!({}))["dialog"].is_null(), "{command} opens its dialog");
        gaps.extend(d.gaps(command));
        if command == "sequence.settings" {
            assert!(!d.ids("auto.dialog.sequence-settings.").is_empty(), "the dialog's controls are named for it");
        }
        for _ in 0..2 {
            d.ok("ui.key", json!({"key": "Escape"}));
            d.frames(2);
        }
    }
    assert!(gaps.is_empty(), "{}", gaps.join("\n"));
}
