//! Headless UI tests of File ▸ Export ▸ Captions… (#3365): the settings dialog picks the format,
//! the host's save panel is asked for only that format's filter, extension and name, and
//! `captions.export` writes it. Driven by automation id through the control channel. With
//! `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the UI with wgpu and writes PNGs there.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// What the save panel was asked for: filter label, extensions, suggested name.
type Asked = Arc<Mutex<Vec<(String, Vec<String>, String)>>>;

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    asked: Asked,
    /// The name the user types in the save panel next (else the suggested one is kept).
    typed: Arc<Mutex<Option<String>>>,
    shots: Option<std::path::PathBuf>,
}

impl Driver {
    fn new(session: Session, save_to: std::path::PathBuf) -> Self {
        let (tx, rx) = channel();
        let asked = Asked::default();
        let log = asked.clone();
        let typed = Arc::<Mutex<Option<String>>>::default();
        let answer = typed.clone();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        app.hooks.pick_save_as = Some(Box::new(move |filter, exts, suggested| {
            log.lock().unwrap().push((filter.to_string(), exts.iter().map(|e| e.to_string()).collect(), suggested.to_string()));
            let name = answer.lock().unwrap().take().unwrap_or_else(|| suggested.to_string());
            Some(save_to.join(name).to_string_lossy().into_owned())
        }));
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, asked, typed, shots };
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

    fn elements(&mut self, prefix: &str) -> Vec<(String, String)> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().map(|e| (e["id"].as_str().unwrap().to_string(), e["label"].as_str().unwrap_or_default().to_string())).collect()
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        self.elements(prefix).into_iter().map(|(id, _)| id).collect()
    }

    /// The format the dialog's File Format menu shows.
    fn shown_format(&mut self) -> String {
        let els = self.elements("captionExport.format");
        els.into_iter().find(|(id, _)| id == "captionExport.format").map(|(_, label)| label).expect("the File Format menu")
    }

    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(4);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("caption-export-{name}.png"))).unwrap();
    }

    /// Export WebVTT through the dialog, typing `name` in the save panel.
    fn export_vtt_as(&mut self, name: &str) {
        *self.typed.lock().unwrap() = Some(name.to_string());
        self.ok("ui.menu.invoke", json!({"id": "captions.export", "params": {"format": "vtt"}}));
        self.frames(3);
        self.ok("ui.click", json!({"id": "captionExport.ok"}));
        self.frames(4);
    }

    fn status(&mut self) -> String {
        self.ok("ui.inspect", json!({}))["ui"]["status"].as_str().unwrap_or_default().to_string()
    }

    fn asked(&self) -> Vec<(String, Vec<String>, String)> {
        self.asked.lock().unwrap().clone()
    }
}

/// A sequence "Main Edit" with one caption at one second.
fn session() -> Session {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "Main Edit", "audio": 1, "video": 1})).unwrap();
    s.execute("captions.add", json!({"text": "Hello captions", "seconds": 1.0})).unwrap();
    s
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-capx-ui-{name}-{}", std::process::id()));
    // start empty: files a failed run left under a reused process id would change what the tests see
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn caption_export_dialog_chooses_the_format() {
    let dir = tmp("format");
    let mut d = Driver::new(session(), dir.clone());
    let items = d.ok("ui.menu.list", json!({}));
    let item = items.as_array().unwrap().iter().find(|i| i["id"] == "captions.export").expect("captions.export in the menus");
    assert_eq!(item["path"], json!(["File", "Export"]));

    // the menu opens the settings, not the save panel
    let r = d.ok("ui.menu.invoke", json!({"id": "captions.export"}));
    assert_eq!(r["dialog"], "captionExport");
    d.frames(3);
    assert!(d.asked().is_empty(), "no save panel before OK");
    let ids = d.ids("captionExport.");
    for id in ["format", "ok", "cancel"] {
        assert!(ids.contains(&format!("captionExport.{id}")), "captionExport.{id} missing: {ids:?}");
    }
    assert_eq!(d.shown_format(), "SubRip (SRT)");
    d.shot("dialog");

    // every format captions.export writes is offered
    d.ok("ui.click", json!({"id": "captionExport.format"}));
    d.frames(3);
    let options = d.ids("captionExport.format.option.");
    d.shot("formats");
    for ext in ["srt", "vtt", "scc", "mcc", "stl", "ttml", "dfxp"] {
        assert!(options.contains(&format!("captionExport.format.option.{ext}")), "{ext} missing: {options:?}");
    }

    // WebVTT: the save panel offers only .vtt and suggests the sequence's name with it
    d.ok("ui.click", json!({"id": "captionExport.format.option.vtt"}));
    d.frames(2);
    assert_eq!(d.shown_format(), "WebVTT");
    d.ok("ui.click", json!({"id": "captionExport.ok"}));
    d.frames(4);
    assert!(d.ids("captionExport.").is_empty(), "the dialog closes");
    assert_eq!(d.asked(), [("WebVTT".to_string(), vec!["vtt".to_string()], "Main_Edit.vtt".to_string())]);
    let vtt = std::fs::read_to_string(dir.join("Main_Edit.vtt")).expect("Main_Edit.vtt written");
    assert!(vtt.starts_with("WEBVTT") && vtt.contains("Hello captions"), "{vtt}");

    // the choice is remembered, in the UI state agents read; Cancel asks for no file
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["caption_export_format"], "vtt");
    d.ok("ui.menu.invoke", json!({"id": "captions.export"}));
    d.frames(3);
    assert_eq!(d.shown_format(), "WebVTT");
    d.ok("ui.click", json!({"id": "captionExport.cancel"}));
    d.frames(3);
    assert!(d.ids("captionExport.").is_empty(), "Cancel closes the dialog");
    assert_eq!(d.asked().len(), 1, "Cancel opens no save panel");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn caption_export_keeps_the_command_parameters() {
    let dir = tmp("params");
    let mut d = Driver::new(session(), dir.clone());

    // a `format` is preselected; the other parameters (here non-drop-frame timecode) reach the export
    d.ok("ui.menu.invoke", json!({"id": "captions.export", "params": {"format": "scc", "dropFrame": false}}));
    d.frames(3);
    assert_eq!(d.shown_format(), "Scenarist SCC (CEA-608)");
    d.ok("ui.click", json!({"id": "captionExport.ok"}));
    d.frames(4);
    assert_eq!(d.asked().last().map(|a| a.2.clone()), Some("Main_Edit.scc".to_string()));
    let scc = std::fs::read_to_string(dir.join("Main_Edit.scc")).expect("Main_Edit.scc written");
    assert!(scc.starts_with("Scenarist_SCC") && !scc.contains(';'), "non-drop-frame timecode: {scc}");

    // with a path the command exports directly (agents); drop-frame stays the default
    let p = dir.join("direct.scc").to_string_lossy().into_owned();
    let v = d.ok("ui.menu.invoke", json!({"id": "captions.export", "params": {"path": p}}));
    assert!(v.get("dialog").is_none(), "{v}");
    assert!(std::fs::read_to_string(&p).expect("direct.scc written").contains(';'), "drop-frame timecode");
    assert_eq!(d.asked().len(), 1, "no save panel for a path");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn caption_export_survives_parameters_that_are_not_an_object() {
    let dir = tmp("hostile");
    let mut d = Driver::new(session(), dir.clone());
    for (i, params) in [json!("oops"), json!([1, 2]), json!(7)].into_iter().enumerate() {
        let name = format!("hostile-{i}.srt");
        *d.typed.lock().unwrap() = Some(name.clone());
        d.ok("ui.menu.invoke", json!({"id": "captions.export", "params": params}));
        d.frames(3);
        d.ok("ui.click", json!({"id": "captionExport.ok"}));
        d.frames(4);
        assert!(dir.join(&name).exists(), "export {i} did not write {name}");
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn caption_export_writes_the_file_the_save_panel_named_and_never_replaces_one_unasked() {
    let dir = tmp("names");
    let mut d = Driver::new(session(), dir.clone());
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();

    // typed without the extension: it is added
    d.export_vtt_as("subs");
    assert!(read("subs.vtt").starts_with("WEBVTT"), "subs.vtt written as WebVTT");
    assert!(!dir.join("subs").exists());

    // typed with another caption format's extension: the file the save panel asked about, in that
    // format, offered first the next time
    d.export_vtt_as("subs.srt");
    let srt = read("subs.srt");
    assert!(srt.starts_with("1\n") && srt.contains(" --> ") && !srt.contains("WEBVTT"), "subs.srt written as SubRip: {srt}");
    assert!(!dir.join("subs.srt.vtt").exists());
    assert!(d.status().contains("as SubRip (SRT)"), "{}", d.status());
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["caption_export_format"], "srt");

    // the extension would name a file the save panel never asked about: it is left alone
    std::fs::write(dir.join("keep.vtt"), "precious").unwrap();
    d.export_vtt_as("keep");
    assert_eq!(read("keep.vtt"), "precious", "not overwritten");
    assert!(d.status().contains("keep.vtt already exists"), "{}", d.status());

    // chosen in the save panel by its full name (the panel asked): it is replaced
    d.export_vtt_as("keep.vtt");
    assert!(read("keep.vtt").starts_with("WEBVTT"), "replaced");
    let _ = std::fs::remove_dir_all(dir);
}
