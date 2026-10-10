//! Headless UI test of the Essential Sound panel: the real `FilmcraftApp` under `egui_kittest`,
//! driven by automation id and by frame-by-frame pointer drags.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `essential-sound-*.png` there; without it no GPU is needed.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
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
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn element(&mut self, id: &str) -> Option<[f32; 4]> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array()?.iter().find(|e| e["id"] == id).map(|e| {
            let r = &e["rect"];
            [r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32]
        })
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
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

    /// Offscreen render of the window (or the bounding box of the elements under `prefix`).
    fn snapshot(&mut self, name: &str, prefix: Option<&str>) {
        let Some(dir) = self.snapshots.clone() else { return };
        let crop = prefix.map(|p| {
            let v = self.ok("ui.elements", json!({"prefix": p}));
            let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for e in v.as_array().unwrap() {
                let r = &e["rect"];
                let (x, y, w, h) = (r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32);
                bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x + w), bb[3].max(y + h)];
            }
            bb
        });
        self.frames(2);
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        let ppp = img.width() as f32 / 1600.0;
        let img = match crop {
            Some(bb) => {
                let x0 = ((bb[0] - 24.0) * ppp).max(0.0) as u32;
                let y0 = ((bb[1] - 30.0) * ppp).max(0.0) as u32;
                let x1 = (((bb[2] + 16.0) * ppp) as u32).min(img.width());
                let y1 = (((bb[3] + 46.0) * ppp) as u32).min(img.height());
                image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image()
            }
            None => img,
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        img.save(&path).unwrap();
        eprintln!("snapshot: {}", path.display());
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
    d.drag_x("essentialSound.repair.noise.amount.track", 0.5, 0.9, 6);
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
