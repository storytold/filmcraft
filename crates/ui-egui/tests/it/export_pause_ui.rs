//! Export is a still preview: entering it ends editing transport without moving either playhead.
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use egui_kittest::Harness;
use filmcraft_engine::{Job, Session};
use filmcraft_time::Tick;
use filmcraft_ui_egui::{AudioOut, FilmcraftApp, control::ControlRequest, dock::PanelKind, menus, state::Mode};
use serde_json::{Value, json};

type Fill = Box<dyn FnMut(&mut [f32], usize) + Send>;
struct Out {
    frames: AtomicU64,
    fill: Arc<Mutex<Option<Fill>>>,
}
impl AudioOut for Out {
    fn start(&mut self, fill: Fill) -> Result<u32, String> {
        self.frames.store(0, Ordering::SeqCst);
        *self.fill.lock().unwrap() = Some(fill);
        Ok(48_000)
    }
    fn stop(&mut self) {
        *self.fill.lock().unwrap() = None;
    }
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn played_frames(&self) -> Option<u64> {
        Some(self.frames.fetch_add(4_800, Ordering::SeqCst))
    }
}
impl filmcraft_ui_egui::scrub::ScrubOut for Out {
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn play(&mut self, fill: Fill) {
        AudioOut::start(self, fill).unwrap();
    }
    fn stop(&mut self) {
        AudioOut::stop(self);
    }
}
struct Driver {
    h: Harness<'static, FilmcraftApp>,
    tx: mpsc::Sender<ControlRequest>,
    fill: Arc<Mutex<Option<Fill>>>,
}
impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let id = s.state.active_sequence.unwrap();
        let q = Arc::make_mut(&mut s.project).sequence_mut(id).unwrap();
        q.settings.width = 32;
        q.settings.height = 18;
        let fill = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::channel();
        let mut app = FilmcraftApp::new(s).with_control(rx);
        app.audio = Some(Box::new(Out { frames: AtomicU64::new(0), fill: fill.clone() }));
        let h = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_step_dt(0.25).build_eframe(move |_cc| app);
        let mut d = Self { h, tx, fill };
        d.frames(2);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.h.ctx.clone();
            let mut raw = std::mem::take(self.h.input_mut());
            eframe::App::raw_input_hook(self.h.state_mut(), &ctx, &mut raw);
            *self.h.input_mut() = raw;
            self.h.step();
        }
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        let (request, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(request).unwrap();
        for _ in 0..200 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn invoke(&mut self, id: &str, params: Value) -> Result<Value, String> {
        let ctx = self.h.ctx.clone();
        menus::invoke(self.h.state_mut(), &ctx, id, params)
    }
    fn enter(&mut self, route: &str) {
        match route {
            "header.mode.export" | "header.notifications" => {
                self.call("ui.click", json!({"id": route}));
            }
            "ui.set" => {
                self.call("ui.set", json!({"mode":"export"}));
            }
            "shortcut" => {
                self.call("ui.key", json!({"key":"Cmd+M"}));
            }
            id => {
                self.invoke(id, json!({})).unwrap();
            }
        }
        assert_eq!(self.h.state().ui.mode, Mode::Export, "{route}");
    }
    fn paused(&self) {
        let app = self.h.state();
        assert!(!app.playback.playing, "Program keeps playing in Export");
        assert!(!app.playback.audio_clock);
        assert!(app.playback.preroll.is_none());
        assert!(!app.source_playback.clock.playing, "Source keeps playing in Export");
        assert!(!app.source_playback.clock.audio_clock);
        assert!(!app.session.trim_play.active());
        assert!(self.fill.lock().unwrap().is_none(), "audio output still owns a playback callback");
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        self.h.state_mut().stop();
        self.h.state_mut().stop_source();
    }
}
const ROUTES: [&str; 6] = ["header.mode.export", "header.notifications", "mode.export", "file.exportMedia", "ui.set", "shortcut"];

#[test]
fn export_entry_routes_stop_program_and_preserve_later_frames() {
    for route in ROUTES {
        let mut d = Driver::new();
        let before = d.h.state().session.project.to_json();
        let history = d.h.state_mut().session.execute("history.list", json!({})).unwrap();
        d.h.state_mut().play(1.0);
        d.frames(12);
        assert!(d.h.state().playback.playing);
        assert!(d.fill.lock().unwrap().is_some(), "exercise running audio");
        d.enter(route);
        d.paused();
        let at = d.h.state().session.playhead();
        let settings = serde_json::to_value(&d.h.state().ui.export.settings).unwrap();
        d.frames(8);
        assert_eq!(d.h.state().session.playhead(), at, "{route}");
        assert_eq!(d.h.state().session.project.to_json(), before);
        assert_eq!(d.h.state_mut().session.execute("history.list", json!({})).unwrap(), history);
        assert_eq!(serde_json::to_value(&d.h.state().ui.export.settings).unwrap(), settings);
        d.invoke("mode.edit", json!({})).unwrap();
        d.frames(2);
        d.paused();
        assert_eq!(d.h.state().session.playhead(), at);
        d.h.state_mut().play(1.0);
        d.frames(12);
        assert!(d.h.state().session.playhead() > at, "explicit resume works");
    }
}

#[test]
fn export_entry_routes_stop_source_output() {
    for route in ROUTES {
        let mut d = Driver::new();
        d.h.state_mut().session.execute("source.open", json!({"item":11})).unwrap();
        d.h.state_mut().ui.focused = PanelKind::Source;
        d.h.state_mut().play_source().unwrap();
        d.frames(4);
        assert!(d.fill.lock().unwrap().is_some());
        d.enter(route);
        d.paused();
        let at = d.h.state().session.state.source_playhead;
        d.frames(8);
        assert_eq!(d.h.state().session.state.source_playhead, at, "{route}");
    }
}

#[test]
fn export_entry_cancels_preroll_without_rewinding() {
    let mut d = Driver::new();
    d.h.state_mut().session.set_playhead(Tick::from_seconds_f64(3.0));
    d.h.state_mut().play(1.0);
    assert!(d.h.state().playback.preroll.is_some());
    let at = d.h.state().session.playhead();
    d.enter("mode.export");
    d.paused();
    assert_eq!(d.h.state().session.playhead(), at);
}

#[test]
fn export_rejects_hidden_transport_before_it_can_seek_or_record() {
    let mut d = Driver::new();
    d.h.state_mut().session.execute("source.open", json!({"item":11})).unwrap();
    d.h.state_mut().session.execute("markers.markIn", json!({"seconds":2})).unwrap();
    d.h.state_mut().session.execute("markers.markOut", json!({"seconds":4})).unwrap();
    d.enter("mode.export");
    let at = d.h.state().session.playhead();
    let source_at = d.h.state().session.state.source_playhead;
    let before = d.h.state().session.project.to_json();
    for id in [
        "playback.toggle",
        "playback.forward",
        "playback.reverse",
        "playback.slowForward",
        "playback.slowReverse",
        "playback.inToOut",
        "playback.inToOutPreroll",
        "playback.toOut",
        "playback.playAround",
        "source.playback.play",
        "source.playback.toggle",
        "trim.shuttle",
        "trim.playAround",
        "voiceover.recordToggle",
        "audio.voiceover.start",
    ] {
        let error = d.invoke(id, json!({})).expect_err("reject hidden transport");
        assert!(error.contains("Export") && error.contains("Edit"), "{id}: {error}");
        d.paused();
        assert_eq!(d.h.state().session.playhead(), at, "{id}");
        assert_eq!(d.h.state().session.state.source_playhead, source_at, "{id}");
        assert!(!d.h.state().session.voiceover.recording());
        assert_eq!(d.h.state().session.project.to_json(), before);
    }
    d.call("ui.playback", json!({"action":"play"}));
    d.paused();
    assert!(d.h.state_mut().play_source().is_err());
    for key in ["Space", "L", "J", "Shift+Space", "Ctrl+Space"] {
        d.call("ui.key", json!({"key":key}));
        d.frames(2);
        d.paused();
        assert_eq!(d.h.state().session.playhead(), at, "{key}");
    }
}

#[test]
fn preview_render_completion_does_not_restart_or_seek_in_export() {
    let mut d = Driver::new();
    let job = Job { id: 950, label: "Rendering previews".into(), progress: Default::default(), result: Default::default() };
    d.h.state_mut().ui.play_after_render = true;
    d.h.state_mut().session.jobs.push(job.clone());
    d.frames(1); // Establish the watched render while still in Edit.
    d.h.state_mut().session.set_playhead(Tick::from_seconds_f64(3.0));
    d.enter("mode.export");
    let at = d.h.state().session.playhead();
    assert!(!job.progress.cancel.load(Ordering::Relaxed), "entering Export must not cancel the job");
    job.progress.finished.store(true, Ordering::Relaxed);
    d.frames(4);
    d.paused();
    assert_eq!(d.h.state().session.playhead(), at);
}

#[test]
fn export_finishes_dynamic_trim_as_one_undo_step() {
    let mut d = Driver::new();
    for track in ["V2", "V3", "A2", "A3"] {
        d.h.state_mut().session.execute("timeline.setTrack", json!({"track":track,"syncLock":false})).unwrap();
    }
    let clip = d.h.state().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    d.h.state_mut().session.execute("trim.selectEditPoint", json!({"clip":clip.0,"edge":"out","kind":"roll"})).unwrap();
    let original = d.h.state().session.project.to_json();
    let undo = d.h.state_mut().session.execute("history.list", json!({})).unwrap()["undo"].as_array().unwrap().len();
    d.h.state_mut().session.execute("trim.shuttle", json!({"direction":"forward","clock":0.0})).unwrap();
    d.h.state_mut().session.execute("trim.tick", json!({"clock":0.25})).unwrap();
    let edited = d.h.state().session.project.to_json();
    assert_ne!(edited, original);
    let at = d.h.state().session.playhead();
    d.enter("mode.export");
    d.paused();
    assert_eq!(d.h.state().session.project.to_json(), edited, "keep the visible trim, do not cancel or advance it");
    let history = d.h.state_mut().session.execute("history.list", json!({})).unwrap();
    assert_eq!(history["undo"].as_array().unwrap().len(), undo + 1);
    d.frames(4);
    assert_eq!(d.h.state().session.playhead(), at);
    assert_eq!(d.h.state().session.project.to_json(), edited);
    d.h.state_mut().session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(d.h.state().session.project.to_json(), original);
}

#[test]
fn export_finishes_voiceover_before_leaving_edit() {
    for elapsed in [0.0, 1.0] {
        let dir = tempfile::tempdir().unwrap();
        let mut d = Driver::new();
        d.h.state_mut().session.execute("file.projectSettings.scratchDisks", json!({"captured":dir.path()})).unwrap();
        d.h.state_mut().session.set_playhead(Tick::ZERO);
        let before = d.h.state().session.project.to_json();
        d.h.state_mut().session.execute("audio.voiceover.start", json!({"track":"A1","time":0,"preroll":0})).unwrap();
        assert!(d.h.state().session.voiceover.recording());
        d.h.state_mut().play(1.0);
        d.h.state_mut().session.set_playhead(Tick::from_seconds_f64(elapsed));
        let at = d.h.state().session.playhead();
        d.enter("mode.export");
        d.paused();
        assert!(!d.h.state().session.voiceover.recording());
        assert_eq!(d.h.state().session.playhead(), at);
        if elapsed > 0.0 {
            assert_ne!(d.h.state().session.project.to_json(), before, "the recorded take must be retained");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "the take is saved");
            d.h.state_mut().session.execute("edit.undo", json!({})).unwrap();
        }
        assert_eq!(d.h.state().session.project.to_json(), before);
    }
}

#[test]
fn completed_speech_preview_cannot_restart_audio_in_export() {
    let mut d = Driver::new();
    d.enter("mode.export");
    // Synthesis requested in Edit can finish after the mode switch.
    let audio = Arc::new(filmcraft_tts::Audio { samples: vec![0.25; 48_000], sample_rate: 48_000 });
    d.h.state_mut().session.tts_preview = Some(audio.clone());
    let error = d.h.state_mut().play_tts_preview().expect_err("do not audition behind Export");
    assert!(error.contains("Export"));
    d.paused();
    assert!(Arc::ptr_eq(d.h.state().session.tts_preview.as_ref().unwrap(), &audio), "keep the completed synthesis for later");
    d.invoke("mode.edit", json!({})).unwrap();
    d.h.state_mut().play_tts_preview().unwrap();
    assert!(d.fill.lock().unwrap().is_some(), "audition remains available in Edit");
    d.enter("mode.export");
    d.paused();
    assert!(!d.h.state().playback.tts_preview);
}

#[test]
fn held_pointer_cannot_reopen_scrub_audio_after_export_entry() {
    for separate_output in [false, true] {
        let mut d = Driver::new();
        if separate_output {
            d.h.state_mut().scrub_out = Some(Box::new(Out { frames: AtomicU64::new(0), fill: d.fill.clone() }));
        }
        d.h.state_mut().session.prefs.audio.scrub_audio = true;
        d.h.state_mut().scrub_tick(10.0, false);
        d.h.state_mut().play(1.0);
        // Playback moved since the preceding UI pass; the pointer is still held as Export opens.
        d.h.state_mut().session.set_playhead(Tick::from_seconds_f64(5.0));
        d.enter("mode.export");
        d.paused();
        assert!(!d.h.state_mut().scrub_tick(10.1, true), "a hidden playhead must not open scrub output");
        d.paused();
        d.invoke("mode.edit", json!({})).unwrap();
        assert!(!d.h.state_mut().scrub_tick(10.2, true), "returning to Edit must not replay a stale move");
        d.h.state_mut().session.set_playhead(Tick::from_seconds_f64(6.0));
        assert!(d.h.state_mut().scrub_tick(10.3, true), "a deliberate Edit scrub still plays");
        assert!(d.fill.lock().unwrap().is_some());
        d.enter("mode.export");
        assert!(!d.h.state_mut().scrub_tick(10.4, true));
        d.paused();
    }
}

fn paints_texture(shape: &egui::Shape, id: egui::TextureId) -> bool {
    match shape {
        egui::Shape::Mesh(mesh) => mesh.texture_id == id && !mesh.indices.is_empty(),
        egui::Shape::Vec(shapes) => shapes.iter().any(|shape| paints_texture(shape, id)),
        _ => false,
    }
}

#[test]
fn export_keeps_painting_the_paused_picture() {
    let mut d = Driver::new();
    d.h.state_mut().play(1.0);
    d.frames(8);
    d.enter("header.mode.export");
    d.paused();
    for _ in 0..200 {
        d.frames(1);
        if d.h.state().texture_existing("export-preview").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (texture, _) = d.h.state().texture_existing("export-preview").expect("Export picture rendered");
    let key = d.h.state().texture_key("export-preview").unwrap();
    for _ in 0..20 {
        d.frames(1);
        assert_eq!(d.h.state().texture_key("export-preview"), Some(key));
        assert!(d.h.output().shapes.iter().any(|shape| paints_texture(&shape.shape, texture)), "cached texture must actually be painted, not just retained");
    }
}
