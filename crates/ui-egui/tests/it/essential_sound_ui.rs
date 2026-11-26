//! Headless UI test of the Essential Sound panel: the real `FilmcraftApp` under `egui_kittest`,
//! driven by automation id and by frame-by-frame pointer drags.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `essential-sound-*.png` there; without it no GPU is needed.

use crate::support::{Drive, Launch, builder, demo_session, driver, with_snapshots};
use serde_json::{Value, json};

driver!(Driver);

impl Driver {
    fn demo() -> Self {
        Self::open(demo_session(), with_snapshots(builder()))
    }
    /// Press on `id` at fraction `fx` of its width, move to fraction `tx` over several frames, release.
    fn drag_x(&mut self, id: &str, fx: f32, tx: f32, steps: usize) {
        let r = self.element(id).unwrap_or_else(|| panic!("no element {id}"));
        let mut p = egui::pos2(r[0] + r[2] * fx, r[1] + r[3] / 2.0);
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(p));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        for _ in 0..steps {
            p.x += r[2] * (tx - fx) / steps as f32;
            push(self, egui::Event::PointerMoved(p));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(2);
    }
}

fn es(d: &mut Driver, clip: u64) -> Value {
    d.exec("essentialSound.inspect", json!({"clips": [clip]}))["clips"][0].clone()
}

#[test]
fn essential_sound_panel_by_id_and_by_drag() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.frames(3);
    let seq = d.exec("sequence.inspect", json!({}));
    let a1 = seq["audio"][0]["items"][0]["clip"].as_u64().unwrap();
    let music = seq["audio"][1]["items"][0]["clip"].as_u64().unwrap();
    // select a clip → the type buttons
    d.exec("timeline.select", json!({"clips": [a1]}));
    d.frames(3);
    let ids = d.ids("essentialSound.");
    for id in [
        "essentialSound.tab.Edit",
        "essentialSound.tab.Browse",
        "essentialSound.type.Dialogue",
        "essentialSound.type.Music",
        "essentialSound.type.SFX",
        "essentialSound.type.Ambience",
    ] {
        assert!(ids.iter().any(|i| i == id), "{id} missing from {ids:?}");
    }
    d.snapshot("essential-sound-untyped", Some("essentialSound."));
    d.click("essentialSound.type.Dialogue");
    assert_eq!(es(&mut d, a1)["type"], "Dialogue");
    d.frames(2);
    let ids = d.ids("essentialSound.");
    for id in [
        "essentialSound.clearType",
        "essentialSound.preset",
        "essentialSound.preset.save",
        "essentialSound.section.Loudness",
        "essentialSound.section.Repair.toggle",
        "essentialSound.section.Clarity",
        "essentialSound.section.Creative",
        "essentialSound.repair.noise.on",
        "essentialSound.repair.noise.amount",
        "essentialSound.repair.humHz.1",
        "essentialSound.clarity.eqPreset",
        "essentialSound.creative.reverbPreset",
        "essentialSound.volume.on",
        "essentialSound.volume.levelDb",
        "essentialSound.mute",
    ] {
        assert!(ids.iter().any(|i| i == id), "{id} missing from {ids:?}");
    }
    // switch a repair slot on, drag its slider: one undo step
    d.click("essentialSound.repair.noise.on");
    let c = es(&mut d, a1);
    assert_eq!(c["settings"]["repair"]["noise"]["on"], true);
    assert!(c["effects"].as_array().unwrap().iter().any(|e| e["effect"] == "denoise"));
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    d.drag_x("essentialSound.repair.noise.amount", 0.5, 0.9, 6);
    let amt = es(&mut d, a1)["settings"]["repair"]["noise"]["amount"].as_f64().unwrap();
    assert!(amt > 8.0, "slider dragged to {amt}");
    assert_eq!(d.exec("history.list", json!({}))["undo"].as_array().unwrap().len(), undo0 + 1, "a drag is one undo step");
    // the section switch bypasses the section's effects
    d.click("essentialSound.section.Repair.toggle");
    let c = es(&mut d, a1);
    assert_eq!(c["settings"]["repair"]["enabled"], false);
    assert!(c["effects"].as_array().unwrap().iter().all(|e| e["enabled"] == false));
    d.click("essentialSound.section.Repair.toggle");
    // Loudness twirl + Auto-Match
    d.click("essentialSound.section.Loudness");
    d.click("essentialSound.autoMatch");
    let c = es(&mut d, a1);
    assert!(c["settings"]["loudness"]["target_lufs"].is_number(), "auto-matched: {c}");
    // a preset
    d.exec("essentialSound.applyPreset", json!({"preset": "Podcast Voice"}));
    d.frames(3);
    d.snapshot("essential-sound-dialogue", Some("essentialSound."));
    // footer: clip volume and mute
    d.click("essentialSound.volume.on");
    d.click("essentialSound.mute");
    let c = es(&mut d, a1);
    assert_eq!(c["settings"]["volume"]["on"], true);
    assert_eq!(c["settings"]["mute"], true);
    d.click("essentialSound.mute");
    // music: Ducking section and Generate Keyframes
    d.exec("timeline.select", json!({"clips": [music]}));
    d.frames(2);
    d.click("essentialSound.type.Music");
    d.frames(2);
    let ids = d.ids("essentialSound.");
    for id in [
        "essentialSound.section.Ducking.toggle",
        "essentialSound.ducking.against.Dialogue",
        "essentialSound.ducking.sensitivity",
        "essentialSound.generateDucking",
    ] {
        assert!(ids.iter().any(|i| i == id), "{id} missing from {ids:?}");
    }
    d.click("essentialSound.section.Ducking.toggle");
    d.click("essentialSound.generateDucking");
    assert_eq!(es(&mut d, music)["settings"]["ducking"]["enabled"], true);
    d.snapshot("essential-sound-music", Some("essentialSound."));
    // Browse: presets per type; a click applies
    d.click("essentialSound.tab.Browse");
    d.frames(2);
    let ids = d.ids("essentialSound.browse.");
    assert!(ids.iter().any(|i| i == "essentialSound.browse.Music.Duck Deep Under Dialogue"), "{ids:?}");
    d.click("essentialSound.browse.Music.Duck Deep Under Dialogue");
    assert_eq!(es(&mut d, music)["settings"]["preset"], "Duck Deep Under Dialogue");
    d.snapshot("essential-sound-browse", Some("essentialSound."));
    d.click("essentialSound.tab.Edit");
    d.snapshot("essential-sound-audio-workspace", None);
}
