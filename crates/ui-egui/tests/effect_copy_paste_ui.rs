//! Headless UI tests of copying and pasting effects in Effect Controls (#1680).

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
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

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn clip(&mut self, i: usize) -> filmcraft_engine::project::TrackItem {
        self.app().session.active_sequence().unwrap().video_tracks[0].items[i].clone()
    }

    fn right_click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id, "button": "right"}));
        self.frames(3);
    }

    fn blurriness(&mut self, i: usize) -> Vec<f64> {
        self.clip(i).effects.iter().filter(|e| e.effect == "gaussian_blur").filter_map(|e| e.param("blurriness")?.value.as_f64()).collect()
    }
}

#[test]
fn effects_copy_and_paste_from_the_menu_and_the_keyboard() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let (a, b) = (d.clip(0).id.0, d.clip(2).id.0);
    d.exec("effects.apply", json!({"clips": [a], "effect": "gaussian_blur"}));
    d.exec("effects.setParam", json!({"clip": a, "effect": "gaussian_blur", "param": "blurriness", "value": 30.0}));
    d.exec("timeline.select", json!({"clips": [a]}));
    d.frames(3);
    // fold the fixed effects so the blur's row is in view
    for fx in ["motion", "opacity", "time_remap"] {
        d.click(&format!("effectControls.effect.{fx}.twirl"));
    }

    // the effect's right-click menu: Copy, then Paste on the other clip's Effect Controls
    d.right_click("effectControls.effect.gaussian_blur");
    d.click("effectControls.effect.gaussian_blur.copy");
    d.exec("timeline.select", json!({"clips": [b]}));
    d.frames(3);
    assert!(d.blurriness(2).is_empty());
    let undo = d.app().session.history.undo.len();
    d.right_click("effectControls.effect.motion");
    d.click("effectControls.effect.motion.paste");
    assert_eq!(d.blurriness(2), vec![30.0], "the blur is pasted with its setting");
    assert_eq!(d.app().session.history.undo.len(), undo + 1, "one undo step");

    // Paste Settings puts the copied setting back on an effect of the same kind
    d.exec("effects.setParam", json!({"clip": b, "effect": "gaussian_blur", "param": "blurriness", "value": 5.0}));
    d.frames(2);
    for fx in ["motion", "opacity", "time_remap"] {
        d.click(&format!("effectControls.effect.{fx}.twirl"));
    }
    d.right_click("effectControls.effect.gaussian_blur");
    d.click("effectControls.effect.gaussian_blur.pasteSettings");
    assert_eq!(d.blurriness(2), vec![30.0]);

    // Cmd+C / Cmd+V in Effect Controls copy the selected effect and paste it onto the shown clip,
    // not a clip onto the Timeline
    let clips = d.app().session.active_sequence().unwrap().video_tracks[0].items.len();
    d.click("effectControls.effect.gaussian_blur");
    assert_eq!(d.app().ui.focused, filmcraft_ui_egui::dock::PanelKind::EffectControls);
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Cmd+V"}));
    d.frames(3);
    assert_eq!(d.blurriness(2), vec![30.0, 30.0], "Cmd+V pastes a second blur");
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks[0].items.len(), clips, "no clip is pasted");

    // Cmd+X cuts the selected effect: copied, then gone
    d.click("effectControls.effect.gaussian_blur");
    d.ok("ui.key", json!({"key": "Cmd+X"}));
    d.frames(3);
    assert_eq!(d.blurriness(2), vec![30.0], "Cmd+X removes the selected blur");
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks[0].items.len(), clips, "no clip is cut");

    // with clips copied last, Cmd+V in Effect Controls still pastes no clip (#2471 review)
    d.click(&format!("timeline.clip.{b}"));
    assert_eq!(d.app().ui.focused, filmcraft_ui_egui::dock::PanelKind::Timeline);
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    d.frames(2);
    assert!(!d.app().session.state.clipboard.is_empty(), "the Timeline copied the clip");
    d.click("effectControls.effect.gaussian_blur");
    d.ok("ui.key", json!({"key": "Cmd+V"}));
    d.frames(3);
    assert_eq!(d.app().session.active_sequence().unwrap().video_tracks[0].items.len(), clips, "Effect Controls never pastes clips");
    assert_eq!(d.blurriness(2), vec![30.0], "and nothing else either: the effects copied before are replaced by the clips");

    // with two clips selected, Cmd+V pastes onto the one Effect Controls shows only (#2471 review)
    d.right_click("effectControls.effect.gaussian_blur");
    d.click("effectControls.effect.gaussian_blur.copy");
    // Edit ▸ Paste is enabled for the copied effects with Effect Controls focused, even with an
    // empty clip clipboard (#2471 review)
    d.app().session.state.clipboard.clear();
    let paste = |d: &mut Driver| filmcraft_ui_egui::menus::menu_items(d.app()).into_iter().find(|m| m.id == "edit.paste").unwrap().enabled;
    assert!(paste(&mut d), "Paste is enabled for copied effects");
    d.click(&format!("timeline.clip.{b}"));
    assert!(!paste(&mut d), "the Timeline has no clips to paste");
    d.click("effectControls.effect.gaussian_blur");
    d.exec("timeline.select", json!({"clips": [a, b]}));
    d.frames(3);
    let before = (d.blurriness(0).len(), d.blurriness(2).len());
    d.click("effectControls.effect.motion");
    d.ok("ui.key", json!({"key": "Cmd+V"}));
    d.frames(3);
    let after = (d.blurriness(0).len(), d.blurriness(2).len());
    let gained = (after.0 - before.0) + (after.1 - before.1);
    assert_eq!(gained, 1, "one blur, on one clip: {before:?} → {after:?}");

    // an audio effect copied, a linked video and audio pair shown: Cmd+V goes to the audio clip
    // (#2471 review), not to the video one, which would refuse it
    let (vid, aud, other) = {
        let q = d.app().session.active_sequence().unwrap();
        let v = q.video_tracks[0].items[0].clone();
        let audio: Vec<_> = q.audio_tracks.iter().flat_map(|t| &t.items).cloned().collect();
        let a = audio.iter().find(|c| c.link.is_some() && c.link == v.link).unwrap().id.0;
        let o = audio.iter().find(|c| c.id.0 != a).unwrap().id.0;
        (v.id.0, a, o)
    };
    let level = |d: &mut Driver, c: u64| {
        let q = d.app().session.active_sequence().unwrap();
        q.find_item(filmcraft_project::ClipId(c)).unwrap().1.effect("volume").unwrap().param("level").unwrap().value.as_f64().unwrap()
    };
    d.exec("effects.setParam", json!({"clip": other, "effect": "volume", "param": "level", "value": -6.0}));
    let volume = d
        .app()
        .session
        .active_sequence()
        .unwrap()
        .find_item(filmcraft_project::ClipId(other))
        .unwrap()
        .1
        .effects
        .iter()
        .position(|e| e.effect == "volume")
        .unwrap();
    d.exec("effects.copy", json!({"clip": other, "indices": [volume]}));
    d.exec("timeline.select", json!({"clips": [vid]}));
    d.frames(3);
    assert_ne!(level(&mut d, aud), -6.0);
    d.click("effectControls.effect.motion");
    d.ok("ui.key", json!({"key": "Cmd+V"}));
    d.frames(3);
    assert_eq!(level(&mut d, aud), -6.0, "the shown audio clip takes the copied Volume: {}", d.app().ui.status);
}
