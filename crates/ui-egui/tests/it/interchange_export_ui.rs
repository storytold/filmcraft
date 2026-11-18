//! Headless UI tests of File ▸ Export ▸ AAF… / OMF…: the settings dialogs, driven by automation id
//! through the control channel, export through the host's save panel.

use std::sync::Arc;
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
    fn new(session: Session, save_to: std::path::PathBuf) -> Self {
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        app.hooks.pick_save_as = Some(Box::new(move |_, exts, suggested| {
            assert!(suggested.ends_with(&format!(".{}", exts[0])), "{suggested}");
            Some(save_to.join(suggested).to_string_lossy().into_owned())
        }));
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
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
}

/// A sequence "Mix" with one second of a 440 Hz tone on A1.
fn session() -> Session {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Mix", "audio": 2, "video": 1})).unwrap();
    let inter: Vec<f32> = (0..48_000)
        .flat_map(|i| {
            let x = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
            [x, x]
        })
        .collect();
    let bytes: Arc<[u8]> = filmcraft_engine::previews::write_wav_f32(&inter, 48_000).into();
    let item = filmcraft_engine::commands::import_bytes(&mut s, "/tone.wav", bytes, None).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    s
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-ix-ui-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn aaf_and_omf_export_dialogs() {
    let dir = tmp("dialogs");
    let mut d = Driver::new(session(), dir.clone());
    let items = d.ok("ui.menu.list", json!({}));
    let menu: Vec<(String, Vec<String>)> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["id"].as_str().unwrap().to_string(), i["path"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()))
        .collect();
    for id in ["file.exportAaf", "file.exportOmf"] {
        let (_, path) = menu.iter().find(|(i, _)| i == id).unwrap_or_else(|| panic!("{id} missing from the menus"));
        assert_eq!(path, &vec!["File".to_string(), "Export".to_string()]);
    }

    // AAF: the dialog lists Premiere's options; OK asks for the file and exports
    let r = d.ok("ui.menu.invoke", json!({"id": "file.exportAaf"}));
    assert_eq!(r["dialog"], "aafExport");
    d.frames(3);
    let ids = d.ids("aafExport.");
    for id in [
        "mixdownVideo",
        "breakoutToMono",
        "audio.embedded",
        "audio.separate",
        "audio.linked",
        "audioFormat.wav",
        "sampleRate.48000",
        "bitDepth.24",
        "trimAudio",
        "handles",
        "renderAudioEffects",
        "ok",
        "cancel",
    ] {
        assert!(ids.contains(&format!("aafExport.{id}")), "aafExport.{id} missing: {ids:?}");
    }
    d.ok("ui.click", json!({"id": "aafExport.bitDepth.24"}));
    d.ok("ui.click", json!({"id": "aafExport.breakoutToMono"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "aafExport.ok"}));
    d.frames(4);
    assert!(d.ids("aafExport.").is_empty(), "the dialog closes");
    let aaf = std::fs::read(dir.join("Mix.aaf")).expect("Mix.aaf written");
    assert!(filmcraft_interchange::aaf::sniff(&aaf));
    let (imp, extracted, _) = filmcraft_interchange::aaf::import(&aaf, &Default::default()).unwrap();
    assert_eq!(extracted.len(), 2, "embedded, broken out to two mono essences");
    let (_, ch, sr, bits) = filmcraft_interchange::wav::parse_wav(&extracted[0].wav).unwrap();
    assert_eq!((ch, sr, bits), (1, 48_000, 24));
    assert_eq!(imp.sequences.len(), 1);

    // OMF: cancel leaves nothing behind; OK with separate AIFF files
    d.ok("ui.menu.invoke", json!({"id": "file.exportOmf"}));
    d.frames(3);
    let ids = d.ids("omfExport.");
    assert!(ids.contains(&"omfExport.title".to_string()) && !ids.contains(&"omfExport.audio.linked".to_string()), "{ids:?}");
    d.ok("ui.click", json!({"id": "omfExport.cancel"}));
    d.frames(3);
    assert!(!dir.join("Mix.omf").exists());
    d.ok("ui.menu.invoke", json!({"id": "file.exportOmf"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "omfExport.audio.separate"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "omfExport.audioFormat.aiff"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "omfExport.ok"}));
    d.frames(4);
    let omf = std::fs::read(dir.join("Mix.omf")).expect("Mix.omf written");
    assert!(filmcraft_interchange::omf::sniff(&omf));
    let audio_dir = dir.join("Mix Audio Files");
    let aifs: Vec<_> = std::fs::read_dir(&audio_dir).unwrap().filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "aif")).collect();
    assert_eq!(aifs.len(), 1, "one trimmed AIFF");

    // with a path the menu command exports directly (agents)
    let p = dir.join("direct.aaf").to_string_lossy().into_owned();
    let v = d.ok("ui.menu.invoke", json!({"id": "file.exportAaf", "params": {"path": p, "audio": "linked"}}));
    assert!(v.get("dialog").is_none(), "{v}");
    assert!(std::path::Path::new(&p).exists());
    let _ = std::fs::remove_dir_all(dir);
}
