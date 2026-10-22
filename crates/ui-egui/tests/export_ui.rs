//! Headless UI tests of Export mode (M6.5): the settings column and summary, the Preset Manager
//! (search, favourites, save, apply), the export queue panel (send, reorder, cancel, start, retry),
//! the Export button and the header's Quick Export popup.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `export-*.png` there; without it no GPU is needed.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<PathBuf>,
    dir: PathBuf,
}

impl Driver {
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/export-tests").join(format!("ui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.export_presets.set_dir(&dir.join("data"));
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots, dir };
        d.frames(4);
        d
    }

    fn path(&self, name: &str) -> String {
        self.dir.join(name).to_string_lossy().to_string()
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
        self.frames(3);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn queue(&mut self) -> Vec<Value> {
        self.exec("export.queue.list", json!({}))["items"].as_array().unwrap().clone()
    }

    /// Step frames until `done` holds (the app advances the queue and jobs every frame).
    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let t0 = std::time::Instant::now();
        while !done(self) {
            assert!(t0.elapsed().as_secs() < 300, "timed out waiting for {what}");
            self.frames(2);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn export_mode_settings_and_summary() {
    let mut d = Driver::new("settings");
    d.ok("ui.set", json!({"mode": "export"}));
    d.frames(4);
    for id in [
        "export.fileName",
        "export.location",
        "export.preset",
        "export.preset.more",
        "export.format",
        "export.section.video",
        "export.section.audio",
        "export.section.multiplexer",
        "export.video.matchSize",
        "export.video.bitrateMode",
        "export.video.hardwareEncoding",
        "export.video.maxQuality",
        "export.audio.sampleRate",
        "export.range",
        "export.scaling",
        "export.summary",
        "export.estimate",
        "export.sendToQueue",
        "export.button",
        "export.queue.start",
    ] {
        assert!(d.has(id), "{id}");
    }
    // the default preset and the file name from the sequence
    let st = d.ok("ui.inspect", json!({}))["ui"]["export"].clone();
    assert_eq!(st["preset"], filmcraft_engine::export::presets::DEFAULT_PRESET);
    assert!(!st["fileName"].as_str().unwrap().is_empty());
    // a 4K preset shows in the summary
    d.ok("ui.set", json!({"export": {"preset": "YouTube 2160p 4K Ultra HD"}}));
    d.frames(3);
    let sum = d.ok("ui.elements", json!({"prefix": "export.summary"}))[0]["label"].as_str().unwrap().to_string();
    assert!(sum.contains("3840x2160"), "{sum}");
    // editing a setting turns the preset into Custom
    d.click("export.video.maxQuality");
    // collapse Video and Audio so Effects is on screen
    d.click("export.section.video");
    d.click("export.section.audio");
    d.click("export.section.effects");
    assert_eq!(d.app().ui.export.preset, "Custom");
    assert!(d.app().ui.export.settings.max_render_quality);
    // Effects section widgets
    assert!(d.has("export.effects.loudness") && d.has("export.effects.timecodeOverlay"));
    d.click("export.effects.loudness");
    assert!(d.app().ui.export.settings.effects.loudness.enabled);
    assert!(d.has("export.effects.loudness.target"));
    d.snapshot("export-mode");
}

#[test]
fn the_status_bar_and_the_queue_show_the_time_left() {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use filmcraft_engine::Job;
    use filmcraft_engine::export_tools::QueueStatus;
    let mut d = Driver::new("eta");
    d.ok("ui.set", json!({"mode": "export"}));
    d.frames(3);
    d.exec(
        "export.queue.add",
        json!({"preset": "Waveform Audio 48 kHz 16-bit", "path": d.path("eta.wav"), "range": "custom", "startSeconds": 0.0, "endSeconds": 0.25}),
    );
    // a job at 25 %, doing 50 units a second: 750 left, 15 s (readings from the near future, because
    // the app reads the real clock, which is then behind them)
    let job = Job { id: 900, label: "Exporting".into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(1000, Ordering::Relaxed);
    let t0 = web_time::Instant::now() + Duration::from_secs(60);
    for i in 0..=50u64 {
        job.progress.done.store(i * 5, Ordering::Relaxed);
        job.progress.eta_at(t0 + Duration::from_millis(i * 100));
    }
    d.app().session.jobs.push(job);
    {
        let item = &mut d.app().session.export_queue.items[0];
        item.status = QueueStatus::Encoding;
        item.job = Some(900);
    }
    d.frames(4);
    // the status bar says it next to the percentage, and agents read it from the same label
    let label = d.ok("ui.elements", json!({"prefix": "status.job.progress"}))[0]["label"].as_str().unwrap().to_string();
    assert_eq!(label, "25% · 15 s left");
    // the queue item has it too
    let item = d.queue().into_iter().next().unwrap();
    assert!((item["etaSeconds"].as_f64().unwrap() - 15.0).abs() < 0.5, "{item}");
    assert_eq!(item["status"], "encoding");
    d.snapshot("export-eta");
}

#[test]
fn h265_shows_the_h264_family_controls_without_the_h264_only_ones() {
    use filmcraft_engine::export::{ExportSettings, Format};
    let mut d = Driver::new("h265");
    d.ok("ui.set", json!({"mode": "export"}));
    d.frames(4);
    // H.264, the default: profile, level and the hardware checkbox
    for id in ["export.video.profile", "export.video.level", "export.video.hardwareEncoding", "export.video.bitrateMode"] {
        assert!(d.has(id), "H.264: {id}");
    }
    d.app().ui.export.settings = ExportSettings { format: Format::Hevc, ..Default::default() };
    d.frames(4);
    for id in [
        "export.video.bitrateMode",
        "export.video.target",
        "export.video.max",
        "export.video.keyframeOn",
        "export.section.multiplexer",
        "export.section.audio",
        "export.audio.sampleRate",
    ] {
        assert!(d.has(id), "H.265: {id}");
    }
    // Main is its only profile, the level is the encoder's choice, and hardware is the only encoder
    for id in ["export.video.profile", "export.video.level", "export.video.hardwareEncoding"] {
        assert!(!d.has(id), "H.265 has no {id}");
    }
    let sum = d.ok("ui.elements", json!({"prefix": "export.summary"}))[0]["label"].as_str().unwrap().to_string();
    assert!(sum.contains("HEVC Main") && sum.contains("Target 20.00 Mbps"), "{sum}");
    d.snapshot("export-h265");
}

#[test]
fn preset_manager_search_favourite_save_apply() {
    let mut d = Driver::new("manager");
    d.ok("ui.set", json!({"mode": "export"}));
    d.frames(3);
    d.click("export.preset.more");
    assert!(d.app().ui.export.manager.is_some(), "Preset Manager opens");
    for id in ["presetManager.search", "presetManager.favoritesOnly", "presetManager.ok", "presetManager.cancel", "presetManager.import", "presetManager.save"]
    {
        assert!(d.has(id), "{id}");
    }
    // search
    d.ok("ui.set", json!({"export": {"manager": {"query": "prores"}}}));
    d.frames(3);
    assert!(d.has("presetManager.item.Apple ProRes 422 LT"));
    assert!(!d.has("presetManager.item.YouTube 1080p Full HD"), "filtered out");
    // favourite with the star
    d.click("presetManager.favorite.Apple ProRes 422 LT");
    assert!(d.app().session.export_presets.is_favorite("Apple ProRes 422 LT"));
    d.ok("ui.set", json!({"export": {"manager": {"query": "", "favoritesOnly": true}}}));
    d.frames(3);
    assert!(d.has("presetManager.item.Apple ProRes 422 LT"));
    assert!(!d.has("presetManager.item.Apple ProRes 422 HQ"));
    d.snapshot("export-preset-manager");
    // save the current settings as a user preset
    d.ok("ui.set", json!({"export": {"manager": {"favoritesOnly": false, "saveName": "My Review"}}}));
    d.frames(2);
    d.click("presetManager.save");
    assert!(d.app().session.export_presets.find("My Review").is_some());
    assert!(d.dir.join("data/export-presets.json").exists(), "persisted in the data directory");
    // select + OK applies
    d.ok("ui.set", json!({"export": {"manager": {"query": "422 LT"}}}));
    d.frames(2);
    d.click("presetManager.item.Apple ProRes 422 LT");
    d.click("presetManager.ok");
    assert!(d.app().ui.export.manager.is_none(), "closed on OK");
    assert_eq!(d.app().ui.export.preset, "Apple ProRes 422 LT");
    assert_eq!(d.app().ui.export.settings.format, filmcraft_engine::export::Format::ProRes);
    assert_eq!(d.app().ui.export.settings.prores_profile, "lt");
    // delete the user preset through the manager
    d.click("export.preset.more");
    d.ok("ui.set", json!({"export": {"manager": {"selected": "My Review"}}}));
    d.frames(2);
    d.click("presetManager.delete");
    assert!(d.app().session.export_presets.find("My Review").is_none());
    d.click("presetManager.cancel");
    assert!(d.app().ui.export.manager.is_none());
}

#[test]
fn queue_panel_send_reorder_cancel_start_retry() {
    let mut d = Driver::new("queue");
    d.ok("ui.set", json!({"mode": "export"}));
    let loc = d.dir.to_string_lossy().to_string();
    let set = |d: &mut Driver, name: &str| {
        d.ok(
            "ui.set",
            json!({"export": {"preset": "Waveform Audio 48 kHz 16-bit", "location": loc, "fileName": name, "range": "custom", "customStart": 0.0, "customEnd": 0.25}}),
        );
        d.frames(2);
        d.click("export.sendToQueue");
    };
    set(&mut d, "q1");
    set(&mut d, "q2");
    set(&mut d, "q3");
    let ids: Vec<u64> = d.queue().iter().map(|i| i["id"].as_u64().unwrap()).collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(d.queue()[0]["preset"], "Waveform Audio 48 kHz 16-bit");
    d.frames(2);
    // reorder: the first one down, the last one up
    d.click(&format!("export.queue.item.{}.down", ids[0]));
    d.click(&format!("export.queue.item.{}.up", ids[2]));
    let order: Vec<u64> = d.queue().iter().map(|i| i["id"].as_u64().unwrap()).collect();
    assert_eq!(order, [ids[1], ids[2], ids[0]]);
    // cancel the second item before starting
    d.click(&format!("export.queue.item.{}.cancel", ids[2]));
    assert_eq!(d.queue().iter().find(|i| i["id"] == ids[2]).unwrap()["status"], "cancelled");
    d.snapshot("export-queue");
    d.click("export.queue.start");
    d.wait_until("the queue", |d| !d.app().session.export_queue.is_active());
    let st: Vec<(u64, String)> = d.queue().iter().map(|i| (i["id"].as_u64().unwrap(), i["status"].as_str().unwrap().to_string())).collect();
    assert_eq!(st, [(ids[1], "done".into()), (ids[2], "cancelled".into()), (ids[0], "done".into())]);
    assert!(Path::new(&d.path("q1.wav")).exists() && Path::new(&d.path("q2.wav")).exists());
    assert!(!Path::new(&d.path("q3.wav")).exists());
    // retry the cancelled one from its row
    d.frames(2);
    d.click(&format!("export.queue.item.{}.retry", ids[2]));
    d.wait_until("the retry", |d| !d.app().session.export_queue.is_active());
    assert_eq!(d.queue().iter().find(|i| i["id"] == ids[2]).unwrap()["status"], "done");
    assert!(Path::new(&d.path("q3.wav")).exists());
    // clear finished
    d.frames(2);
    d.click("export.queue.clear");
    assert!(d.queue().is_empty());
}

#[test]
fn export_button_and_quick_export_popup() {
    let mut d = Driver::new("quick");
    d.ok("ui.set", json!({"mode": "export"}));
    let loc = d.dir.to_string_lossy().to_string();
    d.ok("ui.set", json!({"export": {"preset": "Waveform Audio 48 kHz 24-bit", "location": loc, "fileName": "button", "range": "custom", "customStart": 0.0, "customEnd": 0.5}}));
    d.frames(2);
    d.click("export.button");
    let out = d.path("button.wav");
    d.wait_until("the export", |_| std::fs::metadata(&out).map(|m| m.len() >= 44 + 24_000 * 6).unwrap_or(false));
    // Quick Export: header button → popup with presets → Export
    d.exec("markers.markIn", json!({"seconds": 0.0}));
    d.exec("markers.markOut", json!({"seconds": 0.2}));
    d.click("header.quickExport");
    assert!(d.app().ui.export.quick_open);
    for id in ["quickExport.path", "quickExport.go", "quickExport.preset.Match Source – Adaptive High Bitrate", "quickExport.preset.YouTube 1080p Full HD"] {
        assert!(d.has(id), "{id}");
    }
    d.snapshot("export-quick");
    d.click("quickExport.preset.Match Source – Adaptive Low Bitrate");
    d.frames(2);
    let quick = d.path("quick.mp4");
    d.ok("ui.set", json!({"export": {"quickPath": quick}}));
    d.frames(2);
    d.click("quickExport.go");
    assert!(!d.app().ui.export.quick_open, "closes on Export");
    d.wait_until("quick export", |d| d.app().session.jobs.iter().all(|j| j.progress.finished.load(std::sync::atomic::Ordering::Relaxed)));
    assert!(Path::new(&quick).exists(), "{quick}");
    assert_eq!(d.app().session.export_queue.quick_preset.as_deref(), Some("Match Source – Adaptive Low Bitrate"));
    // the popup closes with its Close button too
    d.click("header.quickExport");
    assert!(d.app().ui.export.quick_open);
    d.click("quickExport.close");
    assert!(!d.app().ui.export.quick_open);
}

#[test]
fn quick_export_file_name_can_be_typed_and_the_folder_link_opens_the_save_dialog() {
    use std::sync::{Arc, Mutex};
    let mut d = Driver::new("quick-location");
    d.exec("markers.markIn", json!({"seconds": 0.0}));
    d.exec("markers.markOut", json!({"seconds": 0.2}));
    d.click("header.quickExport");
    d.ok("ui.set", json!({"export": {"quickPreset": "Waveform Audio 48 kHz 16-bit"}}));
    d.frames(2);
    let start = PathBuf::from(&d.app().ui.export.quick_path);
    assert_eq!(start.extension().unwrap(), "wav", "{start:?}");
    // emptying the file name to retype it keeps what is typed instead of refilling the old name
    d.click("quickExport.path");
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.frames(2);
    d.ok("ui.type", json!({"text": "renamed"}));
    d.frames(2);
    let typed = PathBuf::from(&d.app().ui.export.quick_path);
    assert_eq!(typed, start.parent().unwrap().join("renamed.wav"));
    // the folder link asks the save dialog, seeded with the whole path; cancelling changes nothing
    let asked: Arc<Mutex<Vec<(Vec<String>, String)>>> = Arc::default();
    let answer: Arc<Mutex<Option<String>>> = Arc::default();
    let (log, reply) = (asked.clone(), answer.clone());
    d.app().hooks.pick_save_as = Some(Box::new(move |_, exts, suggested| {
        log.lock().unwrap().push((exts.iter().map(|e| e.to_string()).collect(), suggested.to_string()));
        reply.lock().unwrap().clone()
    }));
    d.click("quickExport.location");
    assert_eq!(asked.lock().unwrap().clone(), [(vec!["wav".to_string()], typed.to_string_lossy().to_string())]);
    assert_eq!(PathBuf::from(&d.app().ui.export.quick_path), typed);
    assert!(d.app().ui.export.quick_open, "the popup stays open");
    // a chosen path is applied and shown; another format's extension follows the preset
    *answer.lock().unwrap() = Some(d.path("picked.mp4"));
    d.click("quickExport.location");
    let picked = d.path("picked.wav");
    assert_eq!(d.app().ui.export.quick_path, picked);
    let shown = d.ok("ui.elements", json!({"prefix": "quickExport.location"}))[0]["label"].as_str().unwrap().to_string();
    assert_eq!(shown, d.dir.to_string_lossy());
    d.snapshot("export-quick-location");
    d.click("quickExport.go");
    d.wait_until("quick export", |d| d.app().session.jobs.iter().all(|j| j.progress.finished.load(std::sync::atomic::Ordering::Relaxed)));
    assert!(Path::new(&d.path("picked.wav")).exists(), "{}", d.app().ui.status);
}

#[test]
fn export_mode_file_name_can_be_emptied_and_retyped() {
    let mut d = Driver::new("file-name");
    d.ok("ui.set", json!({"mode": "export"}));
    d.frames(3);
    d.click("export.fileName");
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.frames(2);
    d.ok("ui.type", json!({"text": "Final"}));
    d.frames(2);
    assert_eq!(d.app().ui.export.file_name, "Final");
}

/// Sample Size lists every size (16, 24, 32 bit and 32 bit float) the way Audio Format lists every
/// codec: what the chosen audio format and container cannot store is shown greyed out and cannot
/// be picked, and a size that stops being available (float after switching to FLAC) becomes
/// 32-bit integer.
#[test]
fn sample_size_lists_every_size_and_greys_out_what_the_format_cannot_store() {
    use filmcraft_engine::export::{AudioCodec, ExportSettings, Format, SampleFormat};
    let mut d = Driver::new("sample-size");
    d.ok("ui.set", json!({"mode": "export"}));
    d.frames(4);
    // collapse Video so the Audio section sits under the format
    d.click("export.section.video");
    d.app().ui.export.settings = ExportSettings { format: Format::Wav, ..Default::default() };
    d.frames(4);
    let sizes = ["16 bit", "24 bit", "32 bit", "32 bit float"];
    let open_sizes = |d: &mut Driver| {
        if !d.has("export.audio.bits.option.0") {
            d.click("export.audio.bits");
        }
        for (i, label) in sizes.iter().enumerate() {
            let id = format!("export.audio.bits.option.{i}");
            let e = d.ok("ui.elements", json!({"prefix": id}));
            assert_eq!(e.as_array().and_then(|a| a.iter().find(|e| e["id"] == json!(id))).map(|e| e["label"].clone()), Some(json!(label)), "{id}");
        }
    };

    // WAV: all four; pick 32 bit float
    open_sizes(&mut d);
    d.snapshot("export-sample-size-wav");
    d.click("export.audio.bits.option.3");
    assert_eq!(d.app().ui.export.settings.audio.sample_format(), SampleFormat::Float32);

    // FLAC: float is greyed out, so the float choice becomes 32-bit integer and clicking float
    // changes nothing
    d.app().ui.export.settings.format = Format::Flac;
    d.frames(4);
    assert_eq!(d.app().ui.export.settings.audio_codec(), AudioCodec::Flac);
    assert_eq!(d.app().ui.export.settings.audio.sample_format(), SampleFormat::Int32);
    open_sizes(&mut d);
    d.snapshot("export-sample-size-flac");
    d.click("export.audio.bits.option.3");
    assert_eq!(d.app().ui.export.settings.audio.sample_format(), SampleFormat::Int32, "32 bit float is not available for FLAC");
    if d.has("export.audio.bits.option.0") {
        d.click("export.audio.bits");
    }
    d.click("export.audio.bits");
    d.click("export.audio.bits.option.1");
    assert_eq!(d.app().ui.export.settings.audio.sample_format(), SampleFormat::Int24);

    // MXF: integer PCM only
    d.app().ui.export.settings = ExportSettings { format: Format::MxfOp1a, ..Default::default() };
    d.frames(4);
    open_sizes(&mut d);
    d.snapshot("export-sample-size-mxf");
    d.click("export.audio.bits.option.3");
    assert!(!d.app().ui.export.settings.audio.float, "32 bit float is not available for MXF");
    if d.has("export.audio.bits.option.0") {
        d.click("export.audio.bits");
    }

    // QuickTime PCM (ProRes): all four again
    d.app().ui.export.settings = ExportSettings { format: Format::ProRes, ..Default::default() };
    d.frames(4);
    open_sizes(&mut d);
    d.click("export.audio.bits.option.3");
    assert_eq!(d.app().ui.export.settings.audio.sample_format(), SampleFormat::Float32);
    d.snapshot("export-sample-size-prores");
}

#[test]
fn range_defaults_to_in_out_and_entire_source_asks_first() {
    let mut d = Driver::new("entire");
    d.ok("ui.set", json!({"mode": "export"}));
    d.frames(2);
    assert_eq!(d.app().ui.export.range, "entire", "no marks: the whole sequence");
    // setting marks switches Range to Source In/Out; clearing them switches it back
    d.exec("markers.markIn", json!({"seconds": 0.0}));
    d.exec("markers.markOut", json!({"seconds": 0.2}));
    d.frames(2);
    assert_eq!(d.app().ui.export.range, "inOut");
    d.exec("markers.clearInOut", json!({}));
    d.frames(2);
    assert_eq!(d.app().ui.export.range, "entire");
    d.exec("markers.markIn", json!({"seconds": 0.0}));
    d.exec("markers.markOut", json!({"seconds": 0.2}));
    d.frames(2);
    // a hand-picked Entire Source stays, but Send to Queue asks first
    let loc = d.dir.to_string_lossy().to_string();
    d.ok("ui.set", json!({"export": {"preset": "Waveform Audio 48 kHz 16-bit", "location": loc, "fileName": "entire", "range": "entire"}}));
    d.frames(2);
    assert_eq!(d.app().ui.export.range, "entire");
    d.click("export.sendToQueue");
    assert!(d.app().ui.export.confirm_entire.is_some());
    assert!(d.queue().is_empty(), "nothing queued before answering");
    for id in ["export.entireWarning.useInOut", "export.entireWarning.entire", "export.entireWarning.cancel", "export.entireWarning.dontShow"] {
        assert!(d.has(id), "{id}");
    }
    d.snapshot("export-entire-warning");
    d.click("export.entireWarning.cancel");
    assert!(d.app().ui.export.confirm_entire.is_none());
    assert!(d.queue().is_empty());
    // Use Source In/Out queues the marked range
    d.click("export.sendToQueue");
    d.click("export.entireWarning.useInOut");
    assert_eq!(d.app().ui.export.range, "inOut");
    let q = d.queue();
    assert_eq!(q.len(), 1);
    // Don't show again + Queue Entire Sequence: no more asking
    d.ok("ui.set", json!({"export": {"range": "entire"}}));
    d.frames(2);
    d.click("export.sendToQueue");
    d.click("export.entireWarning.dontShow");
    d.click("export.entireWarning.entire");
    assert!(!d.app().session.prefs.general.show_export_range_warning);
    assert_eq!(d.queue().len(), 2);
    d.click("export.sendToQueue");
    assert!(d.app().ui.export.confirm_entire.is_none());
    assert_eq!(d.queue().len(), 3);
}
