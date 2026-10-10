//! Headless UI tests of colour features: the Lumetri Color panel's LUT menus and section
//! switches, Interpret Footage ▸ Color Management and the sequence colour settings, driven by
//! automation id through the control channel.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu and write `color-*.png`.

#![allow(dead_code)]

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

/// Select the first clip on V1 and park the playhead on it.
fn select_first_clip(d: &mut Driver) -> u64 {
    let seq = d.exec("sequence.inspect", json!({}));
    let first = seq["video"][0]["items"][0].clone();
    let clip = first["clip"].as_u64().unwrap_or_else(|| panic!("no clip: {seq}"));
    d.exec("timeline.select", json!({"clips": [clip]}));
    clip
}

fn lumetri(d: &mut Driver, clip: u64) -> Value {
    let seq = d.exec("sequence.inspect", json!({}));
    let items = seq["video"][0]["items"].as_array().unwrap().clone();
    let it = items.into_iter().find(|i| i["clip"] == clip).unwrap();
    it["effects"].as_array().unwrap().iter().find(|e| e["effect"] == "lumetri").cloned().unwrap_or(Value::Null)
}

#[test]
fn lumetri_lut_menus_and_section_switches() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.exec("lumetri.setInputLut", json!({"lut": "builtin:slog3-sgamut3cine-to-rec709"}));
    d.frames(3);
    let ids = d.ids("lumetri.");
    for id in ["lumetri.input_lut", "lumetri.switch.Basic Correction", "lumetri.switch.Creative", "lumetri.switch.Vignette"] {
        assert!(ids.iter().any(|i| i == id), "{id} missing from {ids:?}");
    }
    d.click("lumetri.section.Creative");
    assert!(d.ids("lumetri.").iter().any(|i| i == "lumetri.look_lut"));
    d.snapshot("color-lumetri-panel", None);
    // clicking a section switch bypasses the section (an undoable engine command)
    d.click("lumetri.switch.Creative");
    let e = lumetri(&mut d, clip);
    assert_eq!(e["params"]["creative_on"]["value"], json!("Bool(false)"), "{e}");
    d.exec("edit.undo", json!({}));
    let e = lumetri(&mut d, clip);
    assert_eq!(e["params"]["creative_on"]["value"], json!("Bool(true)"), "{e}");
}

#[test]
fn interpret_footage_and_sequence_color_dialogs_and_hdr_scopes() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    let seq = d.exec("sequence.inspect", json!({}));
    let item = seq["video"][0]["items"].as_array().unwrap().iter().find(|i| i["clip"] == clip).unwrap()["item"].clone();
    // Clip ▸ Modify ▸ Interpret Footage… opens the dialog (no params)
    let r = d.ok("ui.menu.invoke", json!({"id": "clip.interpretFootage"}));
    assert_eq!(r["dialog"], "interpretFootage");
    d.frames(3);
    d.click("colorDialog.space.slog3-sgamut3cine");
    d.snapshot("color-interpret-footage", None);
    d.click("colorDialog.ok");
    let info = d.exec("media.colorInfo", json!({"item": item}));
    assert_eq!(info["override"], "slog3-sgamut3cine", "{info}");
    // Sequence ▸ Color Management…: Rec. 2100 PQ
    let r = d.ok("ui.menu.invoke", json!({"id": "sequence.colorSettings"}));
    assert_eq!(r["dialog"], "sequenceColor");
    d.frames(3);
    d.click("colorDialog.working.rec2100-pq");
    d.snapshot("color-sequence-settings", None);
    d.click("colorDialog.ok");
    let st = d.exec("sequence.colorSettings", json!({"autoToneMap": true}));
    assert_eq!(st["workingSpace"], "rec2100-pq");
    // the scopes switch to the HDR waveform (cd/m², PQ scale)
    d.ok("ui.panel.show", json!({"panel": "LumetriScopes"}));
    d.frames(6);
    assert!(d.ids("scopes.").iter().any(|i| i == "scopes.hdrWaveform"), "{:?}", d.ids("scopes."));
    d.snapshot("color-hdr-scopes", None);
    // undo the working space and the interpretation
    d.exec("edit.undo", json!({}));
    d.exec("edit.undo", json!({}));
    let info = d.exec("media.colorInfo", json!({"item": item}));
    assert_eq!(info["override"], Value::Null, "{info}");
}

#[test]
fn apply_match_from_the_color_wheels_section() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.exec("effects.apply", json!({"clips": [clip], "effect": "lumetri"}));
    d.frames(2);
    d.click("lumetri.section.Color Wheels & Match");
    for id in ["lumetri.match.reference", "lumetri.match.faceDetection", "lumetri.match.apply"] {
        assert!(d.ids("lumetri.match").iter().any(|i| i == id), "{id} missing");
    }
    // reference: a frame of the last V1 clip (another shot), typed into the field
    let seq = d.exec("sequence.inspect", json!({}));
    let last = seq["video"][0]["items"].as_array().unwrap().last().unwrap().clone();
    let frame = last["startFrame"].as_i64().unwrap() + last["durationFrames"].as_i64().unwrap() / 2;
    let r = d.exec("lumetri.applyMatch", json!({"referenceFrame": frame}));
    assert!(r["distanceAfter"].as_f64().unwrap() < r["distanceBefore"].as_f64().unwrap(), "{r}");
    d.frames(4);
    d.snapshot("color-apply-match", None);
    let e = lumetri(&mut d, clip);
    assert!(e["params"]["wheel_highlights"]["value"].as_str().unwrap() != "Vec2(Vec2 { x: 0.0, y: 0.0 })", "{e}");
    // the button runs the same command (here with the default reference inside the clip: refused, reported in the status line)
    d.click("lumetri.match.apply");
    let ui = d.ok("ui.inspect", json!({}));
    assert!(ui.to_string().contains("reference frame is inside"), "{ui}");
}

impl Driver {
    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

#[test]
fn hdr_lumetri_controls_hsl_refine_and_comparison_view() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.exec("effects.apply", json!({"clips": [clip], "effect": "lumetri"}));
    d.frames(3);
    // SDR sequence: no HDR controls
    assert!(d.ids("lumetri.param.hdr_white").is_empty());
    // a Rec. 2100 PQ sequence: Basic Correction ▸ HDR White / HDR Specular, Curves ▸ HDR Range
    d.exec("sequence.colorSettings", json!({"workingSpace": "rec2100-pq"}));
    d.frames(3);
    for id in ["lumetri.param.hdr_white", "lumetri.param.hdr_specular"] {
        assert!(!d.ids(id).is_empty(), "{id} missing: {:?}", d.ids("lumetri."));
    }
    d.click("lumetri.section.Curves");
    assert!(!d.ids("lumetri.param.curves_hdr_range").is_empty());
    d.snapshot("color-lumetri-hdr", None);
    // clicking on the HDR White slider sets it (an undoable engine edit)
    let r = d.element("lumetri.param.hdr_white").unwrap();
    d.ok("ui.click", json!({"x": r[0] + 112.0 + 4.0, "y": r[1] + r[3] / 2.0}));
    d.frames(3);
    let e = lumetri(&mut d, clip);
    assert_ne!(e["params"]["hdr_white"]["value"], json!("Float(1000.0)"), "{e}");
    // HSL Secondary ▸ Refine (fold Basic Correction and Curves so the section is on screen)
    d.click("lumetri.section.Curves");
    d.click("lumetri.section.Basic Correction");
    d.click("lumetri.section.HSL Secondary");
    for id in ["lumetri.param.hsl_denoise", "lumetri.param.hsl_blur"] {
        assert!(!d.ids(id).is_empty(), "{id} missing");
    }
    d.snapshot("color-lumetri-hsl-refine", None);
    // Color Match ▸ Comparison View toggles the Program monitor's Comparison View; Apply Match
    // then uses its reference frame
    d.click("lumetri.section.Color Wheels & Match");
    d.click("lumetri.match.comparisonView");
    assert_eq!(d.app().ui.program.display_mode(), Some(filmcraft_ui_egui::state::DisplayMode::Comparison));
    assert!(!d.ids("program.compare.reference").is_empty());
    let seq = d.exec("sequence.inspect", json!({}));
    let last = seq["video"][0]["items"].as_array().unwrap().last().unwrap().clone();
    let frame = last["startFrame"].as_i64().unwrap() + last["durationFrames"].as_i64().unwrap() / 2;
    let rate = d.app().session.sequence_rate();
    d.app().ui.program.compare_ref = Some(rate.tick_of(frame).0);
    d.frames(3);
    d.snapshot("color-comparison-view", None);
    d.click("lumetri.match.apply");
    let e = lumetri(&mut d, clip);
    assert!(e["params"]["wheel_highlights"]["value"].as_str().unwrap() != "Vec2(Vec2 { x: 0.0, y: 0.0 })", "matched to the comparison reference: {e}");
    d.click("lumetri.match.comparisonView");
    assert_ne!(d.app().ui.program.display_mode(), Some(filmcraft_ui_egui::state::DisplayMode::Comparison));
}

#[test]
fn lumetri_presets_in_the_effects_panel() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.frames(3);
    let clip = select_first_clip(&mut d);
    d.click("effects.folder.Lumetri Presets");
    d.click("effects.folder.Lumetri Presets/Monochrome");
    let rows = d.ids("effects.lumetriPreset.");
    assert!(rows.iter().any(|r| r == "effects.lumetriPreset.Neutral Mono"), "{rows:?}");
    // double-click a preset row: Lumetri Color configured as the preset on the selected clip
    d.ok("ui.click", json!({"id": "effects.lumetriPreset.Neutral Mono", "button": "right"}));
    d.frames(3);
    d.click("effects.presetMenu.apply");
    let e = lumetri(&mut d, clip);
    assert_eq!(e["params"]["saturation"]["value"], json!("Float(0.0)"), "{e}");
    d.exec("edit.undo", json!({}));
    // maximized (wide) panel: the folder's thumbnail grid next to the tree
    d.app().ui.keys.maximized = Some(filmcraft_ui_egui::dock::PanelKind::Effects);
    d.frames(4);
    assert!(!d.ids("effects.presetGrid").is_empty(), "no grid: {:?}", d.ids("effects."));
    let cells = d.ids("effects.presetGrid.");
    assert!(cells.iter().any(|c| c == "effects.presetGrid.Sepia Tone"), "{cells:?}");
    d.click("effects.folder.Lumetri Presets/Cinematic");
    let cells = d.ids("effects.presetGrid.");
    assert!(cells.iter().any(|c| c == "effects.presetGrid.Golden Dusk"), "{cells:?}");
    d.snapshot("color-lumetri-presets-grid", None);
    d.ok("ui.click", json!({"id": "effects.presetGrid.Golden Dusk", "button": "right"}));
    d.frames(3);
    d.click("effects.presetMenu.apply");
    let e = lumetri(&mut d, clip);
    assert_eq!(e["params"]["temperature"]["value"], json!("Float(35.0)"), "{e}");
}

#[test]
fn lumetri_presets_drag_onto_a_timeline_clip() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.frames(3);
    // the first clip is selected; the preset is dropped on the second, which gets it alone
    let selected = select_first_clip(&mut d);
    let seq = d.exec("sequence.inspect", json!({}));
    let target = seq["video"][0]["items"][1]["clip"].as_u64().unwrap_or_else(|| panic!("no second clip: {seq}"));
    d.click("effects.folder.Lumetri Presets");
    d.click("effects.folder.Lumetri Presets/Monochrome");
    d.frames(2);
    assert!(d.element(&format!("timeline.clip.{target}")).is_some(), "target clip not on screen");
    d.ok("ui.drag", json!({"from": {"id": "effects.lumetriPreset.Neutral Mono"}, "to": {"id": format!("timeline.clip.{target}")}, "steps": 8}));
    d.frames(3);
    let e = lumetri(&mut d, target);
    assert_eq!(e["params"]["saturation"]["value"], json!("Float(0.0)"), "dropped preset on the target clip: {e}");
    assert_eq!(lumetri(&mut d, selected), Value::Null, "the selected clip it was not dropped on is untouched");
    // one undo step removes it again
    d.exec("edit.undo", json!({}));
    assert_eq!(lumetri(&mut d, target), Value::Null);
}
