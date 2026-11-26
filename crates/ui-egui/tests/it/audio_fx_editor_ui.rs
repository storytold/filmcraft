//! Headless UI tests of the Clip / Track Fx Editor windows (Parametric Equalizer, Graphic
//! Equalizer, Multiband Compressor, Dynamics): opened from Effect Controls ("Custom Setup ▸
//! Edit…") and from a mixer effect slot, driven by automation id and by pointer drags.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also write PNGs of the windows (`fx-editor-*.png`).

use crate::support::{Drive, Launch, builder, demo_session, driver, pointer_drag_by, save_snapshot, with_snapshots};
use serde_json::{Value, json};

driver!(Driver);

impl Driver {
    fn demo() -> Self {
        Self::open(demo_session(), with_snapshots(builder()))
    }
    /// Press on `id`, move by (dx, dy) over several frames, release.
    fn drag(&mut self, id: &str, dx: f32, dy: f32, steps: usize) {
        let r = self.element(id).unwrap_or_else(|| panic!("no element {id}"));
        pointer_drag_by(self, egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0), dx, dy, steps, 3);
    }

    fn snapshot(&mut self, name: &str) {
        save_snapshot(self, 2, &format!("{name}.png"));
    }

    fn undo_len(&mut self) -> usize {
        self.exec("history.list", json!({}))["undo"].as_array().unwrap().len()
    }

    /// Audio clip on A1 (id) with `effect` applied and selected; Effect Controls showing it.
    fn audio_clip_with(&mut self, effect: &str) -> u64 {
        self.ok("ui.set", json!({"workspace": "Effects"}));
        let seq = self.exec("sequence.inspect", json!({}));
        let clip = &seq["audio"][0]["items"][0];
        let (id, start) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap());
        self.exec("playhead.set", json!({"time": start}));
        // only the audio clip (Effect Controls prefers a linked video clip)
        self.exec("sequence.linkedSelection", json!({"on": false}));
        self.exec("timeline.select", json!({"clips": [id]}));
        self.exec("effects.apply", json!({"clips": [id], "effect": effect}));
        self.frames(3);
        // fold the fixed effects so the applied one's rows are on screen
        for fx in ["volume", "channel_volume", "panner"] {
            self.click(&format!("effectControls.effect.{fx}.twirl"));
        }
        id
    }

    fn clip_param(&mut self, clip: u64, effect: &str, param: &str) -> Value {
        let seq = self.exec("sequence.inspect", json!({}));
        let items = seq["audio"][0]["items"].as_array().unwrap().clone();
        let it = items.iter().find(|i| i["clip"] == clip).unwrap().clone();
        let fx = it["effects"].as_array().unwrap().iter().find(|e| e["effect"] == effect).unwrap().clone();
        fx["params"][param]["value"].clone()
    }
}

/// A parameter value from `sequence.inspect` (`"Float(3.5)"`, `"Bool(true)"`) or
/// `mixer.inspect` (plain JSON).
fn num(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim_start_matches("Float(").trim_start_matches("Choice(").trim_end_matches(')').parse().ok()))
        .unwrap_or(f64::NAN)
}

fn truthy(v: &Value) -> bool {
    v.as_bool().unwrap_or(false) || v.as_str() == Some("Bool(true)")
}

#[test]
fn parametric_eq_editor_from_effect_controls() {
    let mut d = Driver::demo();
    let clip = d.audio_clip_with("parametric_eq");
    d.click("effectControls.effect.parametric_eq.edit");
    d.frames(3);
    let ids = d.ids("fxEditor.parametric_eq.");
    for id in ["plot", "node.hp", "node.low", "node.mid", "node.lp", "toggle.hp_on", "param.master_gain", "close"] {
        assert!(ids.iter().any(|i| *i == format!("fxEditor.parametric_eq.{id}")), "{id} missing from {ids:?}");
    }
    // band toggle by id
    d.click("fxEditor.parametric_eq.toggle.hp_on");
    assert!(truthy(&d.clip_param(clip, "parametric_eq", "hp_on")));
    // dragging band 3 up boosts it — one undo step for the whole drag
    let undo0 = d.undo_len();
    d.drag("fxEditor.parametric_eq.node.mid", 0.0, -60.0, 6);
    let g = num(&d.clip_param(clip, "parametric_eq", "mid_gain"));
    assert!(g > 3.0, "mid gain after drag: {g}");
    assert_eq!(d.undo_len(), undo0 + 2, "frequency + gain committed on release");
    d.snapshot("fx-editor-parametric-eq");
    d.click("fxEditor.parametric_eq.close");
    assert!(d.ids("fxEditor.parametric_eq.").is_empty(), "window closed");
}

#[test]
fn graphic_eq_and_dynamics_editors() {
    let mut d = Driver::demo();
    let clip = d.audio_clip_with("graphic_eq_30");
    d.click("effectControls.effect.graphic_eq_30.edit");
    d.frames(3);
    let ids = d.ids("fxEditor.graphic_eq_30.param.");
    assert_eq!(ids.iter().filter(|i| i.starts_with("fxEditor.graphic_eq_30.param.b")).count(), 30, "{ids:?}");
    d.drag("fxEditor.graphic_eq_30.param.b17", 0.0, -40.0, 5);
    let g = num(&d.clip_param(clip, "graphic_eq_30", "b17"));
    assert!(g > 3.0, "1 kHz band after drag: {g}");
    d.click("fxEditor.graphic_eq_30.reset");
    assert_eq!(num(&d.clip_param(clip, "graphic_eq_30", "b17")), 0.0);
    d.snapshot("fx-editor-graphic-eq-30");
    d.click("fxEditor.graphic_eq_30.close");
    // Dynamics: the transfer curve and section toggles
    d.exec("effects.apply", json!({"clips": [clip], "effect": "dynamics_rack"}));
    d.frames(3);
    d.click("effectControls.effect.graphic_eq_30.twirl");
    d.click("effectControls.effect.dynamics_rack.edit");
    d.frames(3);
    assert!(d.element("fxEditor.dynamics_rack.curve").is_some());
    d.click("fxEditor.dynamics_rack.toggle.lim_on");
    assert!(truthy(&d.clip_param(clip, "dynamics_rack", "lim_on")));
    d.snapshot("fx-editor-dynamics");
}

#[test]
fn track_fx_editor_from_the_mixer_slot() {
    let mut d = Driver::demo();
    d.exec("mixer.addInsert", json!({"strip": "A1", "effect": "multiband_compressor"}));
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.frames(3);
    d.click("mixer.showEffects");
    d.click("mixer.A1.fx.0");
    d.click("mixer.A1.fx.0.edit");
    d.frames(3);
    let ids = d.ids("fxEditor.multiband_compressor.");
    for id in ["xo1", "xo2", "xo3", "curve.1", "curve.4", "toggle.b2_solo", "param.b3_threshold", "toggle.lim_on"] {
        assert!(ids.iter().any(|i| *i == format!("fxEditor.multiband_compressor.{id}")), "{id} missing from {ids:?}");
    }
    d.click("fxEditor.multiband_compressor.toggle.b2_solo");
    let m = d.exec("mixer.inspect", json!({}));
    let a1 = m["strips"].as_array().unwrap().iter().find(|s| s["ref"] == "A1").unwrap().clone();
    assert!(truthy(&a1["inserts"][0]["params"]["b2_solo"]), "solo set on the insert: {a1}");
    // dragging the mid crossover to the right raises it
    d.drag("fxEditor.multiband_compressor.xo2", 60.0, 0.0, 5);
    let m = d.exec("mixer.inspect", json!({}));
    let a1 = m["strips"].as_array().unwrap().iter().find(|s| s["ref"] == "A1").unwrap().clone();
    let xo2 = num(&a1["inserts"][0]["params"]["xo2"]);
    assert!(xo2 > 2200.0, "crossover after drag: {xo2} ({a1})");
    // #2404 review: a graphical editor's edits go through the lane too: with the crossover's lane
    // automated and read, the drag writes a keyframe at the playhead instead of a static value the
    // automation overrides
    d.exec("mixer.setKeyframe", json!({"strip": "A1", "lane": "fx.0.xo2", "time": 0, "value": 2000.0}));
    d.exec("mixer.setStrip", json!({"strip": "A1", "mode": "Read"}));
    d.frames(3);
    let at_playhead = |d: &mut Driver| {
        let s = &d.harness.state().session;
        s.active_sequence().unwrap().audio_tracks[0].lane_value("fx.0.xo2", s.playhead())
    };
    assert!((at_playhead(&mut d) - 2000.0).abs() < 1e-6);
    d.drag("fxEditor.multiband_compressor.xo2", 60.0, 0.0, 5);
    assert!(at_playhead(&mut d) > 2000.0, "the automation at the playhead follows the drag: {}", at_playhead(&mut d));
    d.snapshot("fx-editor-multiband");
}

/// #1758: an insert without a graphical editor (Highpass) still opens the Track Fx Editor, with a
/// control per parameter that edits the insert.
#[test]
fn any_mixer_insert_can_be_edited() {
    let mut d = Driver::demo();
    d.exec("mixer.addInsert", json!({"strip": "A1", "effect": "highpass"}));
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.frames(3);
    d.click("mixer.showEffects");
    d.click("mixer.A1.fx.0");
    d.click("mixer.A1.fx.0.edit");
    d.frames(3);
    let ids = d.ids("fxEditor.highpass.");
    assert!(ids.iter().any(|i| i == "fxEditor.highpass.param.cutoff"), "{ids:?}");
    let cutoff = |d: &mut Driver| {
        let m = d.exec("mixer.inspect", json!({}));
        let a1 = m["strips"].as_array().unwrap().iter().find(|s| s["ref"] == "A1").unwrap().clone();
        num(&a1["inserts"][0]["params"]["cutoff"])
    };
    let before = cutoff(&mut d);
    d.drag("fxEditor.highpass.param.cutoff", 80.0, 0.0, 5);
    let after = cutoff(&mut d);
    assert!(after > before, "cutoff {before} -> {after}");
    // an automated lane read by the strip: the edit writes a keyframe at the playhead, so it is heard
    d.exec("mixer.setKeyframe", json!({"strip": "A1", "lane": "fx.0.cutoff", "time": 0, "value": 100.0}));
    d.exec("mixer.setStrip", json!({"strip": "A1", "mode": "Read"}));
    d.frames(3);
    let at_playhead = |d: &mut Driver| {
        let s = &d.harness.state().session;
        s.active_sequence().unwrap().audio_tracks[0].lane_value("fx.0.cutoff", s.playhead())
    };
    assert!((at_playhead(&mut d) - 100.0).abs() < 1e-6);
    d.drag("fxEditor.highpass.param.cutoff", 60.0, 0.0, 5);
    assert!(at_playhead(&mut d) > 100.0, "the automation at the playhead follows the edit: {}", at_playhead(&mut d));
    d.click("fxEditor.highpass.close");
    assert!(d.ids("fxEditor.highpass.").is_empty(), "window closed");
}
