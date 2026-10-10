//! Headless UI tests of the Text panel's Transcript tab (Premiere's text-based editing, see
//! `docs/transcripts.md` § Premiere parity): Transcribe with its options, the background job with
//! progress and Cancel, clicking and selecting words and pauses, Delete, the search filters with
//! "Delete all", and Transcript View Options. A fake recogniser stands in for a model, so no
//! weights are needed.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu. The ignored test
//! `transcript_panel_screenshots` always renders and writes `transcript-*.png` (the Text panel and
//! its dialogs) to that directory, by default `target/transcript-screenshots`.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::Transcript;
use filmcraft_speech::{FixedTranscriber, Options, ProgressFn, SpeechError, Transcriber};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

type Gate = Arc<(Mutex<u32>, Condvar)>;

/// A slow recogniser: reports progress in `steps` steps, each released by the test.
struct Stepped {
    inner: FixedTranscriber,
    steps: u32,
    gate: Gate,
}

impl Transcriber for Stepped {
    fn id(&self) -> String {
        "stepped".into()
    }
    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        let (lock, cv) = &*self.gate;
        for k in 1..=self.steps {
            let mut released = lock.lock().unwrap();
            while *released < k {
                let (g, timeout) = cv.wait_timeout(released, std::time::Duration::from_secs(30)).unwrap();
                released = g;
                if timeout.timed_out() {
                    return Err(SpeechError::Model("never released".into()));
                }
            }
            drop(released);
            if !progress(k as f32 / self.steps as f32, "step") {
                return Err(SpeechError::Cancelled);
            }
        }
        self.inner.transcribe(audio, opts, &mut |_, _| true)
    }
}

fn release(gate: &Gate, n: u32) {
    *gate.0.lock().unwrap() = n;
    gate.1.notify_all();
}

/// The words the fake recogniser hears in the first A1 clip (seconds from its source In):
/// "Hello um there friend." — a 1.1 s pause — "So uh we start."
const WORDS: [(&str, f64, f64); 8] = [
    ("Hello", 0.5, 0.9),
    ("um", 1.0, 1.3),
    ("there", 1.4, 1.8),
    ("friend.", 1.9, 2.3),
    ("So", 3.4, 3.6),
    ("uh", 3.8, 4.0),
    ("we", 4.1, 4.3),
    ("start.", 4.4, 4.9),
];

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo(snapshots: Option<std::path::PathBuf>) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        // frames 1/60 s apart, so two clicks of `ui.click {"count": 2}` are a double-click
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).with_step_dt(1.0 / 60.0);
        if snapshots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d
    }

    /// The demo project with the Text panel's Transcript tab in front.
    fn transcript_tab() -> Self {
        Self::with_tab(std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from))
    }

    fn with_tab(snapshots: Option<std::path::PathBuf>) -> Self {
        let mut d = Self::demo(snapshots);
        d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
        d.frames(2);
        d.click("text.tab.Transcript");
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
        self.frames(3);
    }

    /// A double-click (after a second without clicks, so it can't count as a triple-click).
    fn double_click(&mut self, id: &str) {
        self.frames(60);
        self.click_with(id, json!({"count": 2}));
    }

    fn click_with(&mut self, id: &str, extra: Value) {
        let mut p = json!({"id": id});
        if let (Some(o), Some(e)) = (p.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        self.ok("ui.click", p);
        self.frames(3);
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn label(&mut self, id: &str) -> String {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == id).and_then(|e| e["label"].as_str()).unwrap_or_default().to_string()
    }

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn app_mut(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// The fake recogniser, its transcript fitting the first A1 clip's media.
    fn install_recogniser(&mut self, steps: u32) -> Gate {
        let a = self.app().session.active_sequence().unwrap().audio_tracks[0].items[0].clone();
        let tk = |s: f64| a.source_in + filmcraft_time::Tick::from_seconds_f64(s);
        let mut t = Transcript { language: "en".into(), ..Default::default() };
        for (w, s, e) in WORDS {
            t.words.push(filmcraft_project::Word::new(w, tk(s), tk(e)));
        }
        let gate: Gate = Arc::default();
        let inner = FixedTranscriber { transcript: t, id: "stepped".into() };
        self.app_mut().session.transcriber = Some(Arc::new(Stepped { inner, steps, gate: gate.clone() }));
        gate
    }

    /// A transcript brought by `transcript.set` (as an agent without a speech model would).
    fn set_transcript(&mut self) {
        let a = self.app().session.active_sequence().unwrap().audio_tracks[0].items[0].clone();
        let tk = |s: f64| (a.source_in + filmcraft_time::Tick::from_seconds_f64(s)).0;
        let words: Vec<Value> = WORDS.iter().map(|(w, s, e)| json!({"text": w, "start": tk(*s), "end": tk(*e)})).collect();
        self.exec("transcript.set", json!({"item": a.item.0, "transcript": {"language": "en", "words": words}}));
        self.frames(3);
    }

    fn words(&mut self) -> Vec<String> {
        let r = self.exec("transcript.inspect", json!({}));
        r["words"].as_array().unwrap().iter().filter_map(|w| w["text"].as_str().map(str::to_string)).collect()
    }

    /// Step frames until `done` (the UI keeps running while a job works).
    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        for _ in 0..2000 {
            if done(self) {
                return;
            }
            self.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("timed out waiting for {what}");
    }

    /// Offscreen render cropped to the Text panel (with its tabs), or to a dialog window (`area`:
    /// the window's egui id).
    fn snapshot(&mut self, name: &str, area: Option<egui::Id>) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        let crop = match area.and_then(|id| self.harness.ctx.memory(|m| m.area_rect(id))) {
            Some(r) => [r.min.x - 8.0, r.min.y - 8.0, r.max.x + 8.0, r.max.y + 8.0],
            None => {
                let v = self.ok("ui.elements", json!({"prefix": "panel.Text"}));
                let r = &v[0]["rect"];
                let (x, y, w, h) = (r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32);
                [x, y - 30.0, x + w, y + h]
            }
        };
        self.frames(2);
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        let ppp = img.width() as f32 / 1600.0;
        let x0 = (crop[0] * ppp).max(0.0) as u32;
        let y0 = (crop[1] * ppp).max(0.0) as u32;
        let x1 = ((crop[2] * ppp) as u32).min(img.width());
        let y1 = ((crop[3] * ppp) as u32).min(img.height());
        let img = if x1 > x0 && y1 > y0 { image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image() } else { img };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("transcript-{name}.png"));
        img.save(&path).unwrap();
        eprintln!("snapshot: {}", path.display());
    }
}

#[test]
fn transcribe_runs_in_the_background_with_progress_and_cancel() {
    let mut d = Driver::transcript_tab();
    let gate = d.install_recogniser(3);
    assert_eq!(d.ids("text.transcript.generate"), ["text.transcript.generate"], "the empty tab offers Transcribe");
    // Transcribe opens the options: language, speech model, audio analysis; speakers stay hidden
    d.click("text.transcript.generate");
    for id in ["transcribe.language", "transcribe.analysis.dialogue", "transcribe.analysis.track", "transcribe.track", "transcribe.ok", "transcribe.cancel"] {
        assert_eq!(d.ids(id).first().map(String::as_str), Some(id), "{id}");
    }
    assert!(d.ids("transcribe.diarize").is_empty(), "speaker labelling is hidden");
    d.ok("ui.set", json!({"menuDialog": {"language": "en"}}));
    d.click("transcribe.ok");
    assert!(d.app().ui.extras.dialog.is_none(), "the dialog closed");
    // the job runs while the UI keeps drawing: progress and Cancel in the tab
    d.until("the progress row", |d| !d.ids("text.transcript.progress").is_empty());
    assert!(d.label("text.transcript.progress").starts_with("Transcribing"), "{}", d.label("text.transcript.progress"));
    assert_eq!(d.exec("transcript.status", json!({}))["running"], true);
    let p0 = d.exec("transcript.status", json!({}))["progress"].as_f64().unwrap();
    release(&gate, 1);
    d.until("progress", |d| d.exec("transcript.status", json!({}))["progress"].as_f64().unwrap_or(0.0) > p0 + 0.01);
    d.click("text.transcript.cancel");
    release(&gate, 3);
    d.until("the job to stop", |d| d.app().session.transcribe_jobs.is_empty());
    d.frames(3);
    assert!(d.app().session.project.transcripts.is_empty(), "Cancel changes nothing");
    assert_eq!(d.ids("text.transcript.generate").len(), 1, "back to the empty tab");

    // again, to the end: one undo step adds the transcripts of every clip on A1 (the fake
    // recogniser hears the same words in each)
    let gate = d.install_recogniser(2);
    let undo = d.app().session.history.undo.len();
    d.click("text.transcript.generate");
    d.ok("ui.set", json!({"menuDialog": {"track": "A1"}}));
    d.click("transcribe.ok");
    release(&gate, 2);
    d.until("the job", |d| d.app().session.transcribe_jobs.is_empty());
    d.frames(3);
    let first = d.app().session.active_sequence().unwrap().audio_tracks[0].items[0].item;
    assert_eq!(d.app().session.project.transcripts[&first].words.len(), WORDS.len());
    assert_eq!(d.app().session.history.undo.len(), undo + 1, "one undo step");
    assert_eq!(d.app().session.history.undo.last().unwrap().0, "Transcribe");
    assert!(d.ids("text.transcript.progress").is_empty());
    assert!(d.ids("text.transcript.word.").len() >= WORDS.len());
    assert_eq!(d.words()[..4], ["Hello", "um", "there", "friend."]);
}

#[test]
fn a_missing_model_is_confirmed_with_size_and_licence() {
    let mut d = Driver::transcript_tab();
    d.install_recogniser(1);
    d.click("text.transcript.generate");
    // as in a build with a speech model that is not downloaded yet
    let dlg = d.app_mut().ui.extras.dialog.as_mut().expect("Transcribe dialog");
    dlg.info["recogniser"] = Value::Null;
    dlg.info["models"] = json!([{"id": "whisper-base", "name": "Whisper base (multilingual)", "size": 290_000_000u64, "installed": false,
        "license": "MIT (OpenAI Whisper weights)", "source": "https://huggingface.co/openai/whisper-base", "description": "74 M parameters."}]);
    dlg.params["model"] = json!("whisper-base");
    d.frames(2);
    assert_eq!(d.ids("transcribe.model"), ["transcribe.model"], "the speech model picker");
    d.click("transcribe.ok");
    assert_eq!(d.app().ui.extras.dialog.as_ref().unwrap().params["confirmDownload"], true, "asks before downloading");
    assert_eq!(d.label("transcribe.ok"), "Download and transcribe");
    assert!(d.label("transcribe.download.size").contains("290 MB"), "{}", d.label("transcribe.download.size"));
    assert!(d.label("transcribe.download.license").contains("MIT"), "{}", d.label("transcribe.download.license"));
    assert!(d.ids("transcribe.download.attribution").is_empty(), "MIT needs no credit line");
    d.click("transcribe.cancel");
    assert!(d.app().ui.extras.dialog.is_none());
    assert!(d.app().session.transcribe_jobs.is_empty(), "nothing started");
}

/// Parakeet is CC-BY-4.0: the download confirmation credits NVIDIA with the licence and source.
#[test]
fn a_cc_by_model_is_credited_before_download() {
    let mut d = Driver::transcript_tab();
    d.install_recogniser(1);
    d.click("text.transcript.generate");
    let m = filmcraft_speech::models::find("parakeet-tdt-0.6b-v3").expect("in the catalogue");
    let dlg = d.app_mut().ui.extras.dialog.as_mut().expect("Transcribe dialog");
    dlg.info["recogniser"] = Value::Null;
    dlg.info["models"] = json!([{"id": m.id, "name": m.name, "size": m.size(), "installed": false, "license": m.license,
        "attribution": m.attribution(), "source": m.source}]);
    dlg.params["model"] = json!(m.id);
    // the longer model name widens the dialog: let it settle before clicking
    d.frames(8);
    d.click("transcribe.ok");
    assert_eq!(d.label("transcribe.ok"), "Download and transcribe");
    let credit = d.label("transcribe.download.attribution");
    assert!(credit.contains("NVIDIA") && credit.contains("CC-BY-4.0") && credit.contains("huggingface.co/nvidia"), "{credit}");
    d.click("transcribe.cancel");
}

#[test]
fn clicking_selecting_pauses_and_delete_work_like_premiere() {
    let mut d = Driver::transcript_tab();
    d.set_transcript();
    assert_eq!(d.ids("text.transcript.word.").len(), WORDS.len());
    // the 1.1 s pause after "friend." is a "[...]" marker; the shorter gaps are not
    assert_eq!(d.ids("text.transcript.pause."), ["text.transcript.pause.3"]);
    let start = |d: &mut Driver, i: usize| d.exec("transcript.inspect", json!({}))["words"][i]["start"].as_i64().unwrap();
    let frame = d.app().session.sequence_rate().frame_duration().0;

    // a plain click moves the playhead to the word, selects nothing and marks nothing
    d.click("text.transcript.word.2");
    let w2 = start(&mut d, 2);
    assert!((d.app().session.playhead().0 - w2).abs() < frame, "the playhead at the word's frame");
    assert_eq!(d.app().ui.transcript_sel, None);
    assert_eq!(d.app().session.active_sequence().unwrap().mark_in, None);
    // Shift+click selects from the cursor and marks In/Out
    d.click_with("text.transcript.word.3", json!({"modifiers": {"shift": true}}));
    assert_eq!(d.app().ui.transcript_sel, Some((2, 3)));
    let q = d.app().session.active_sequence().unwrap().clone();
    assert!(q.mark_in.is_some() && q.mark_out.is_some());
    // a plain click clears the selection and the In/Out it set
    d.click("text.transcript.word.0");
    assert_eq!(d.app().ui.transcript_sel, None);
    assert_eq!(d.app().session.active_sequence().unwrap().mark_in, None);
    // a double-click selects one word
    d.double_click("text.transcript.word.6");
    assert_eq!(d.app().ui.transcript_sel, Some((6, 6)));

    // a click on the pause selects it; Backspace in the Text panel extracts it
    let before = d.app().session.active_sequence().unwrap().duration();
    d.click("text.transcript.pause.3");
    assert_eq!(d.app().ui.transcript.pause, Some(3));
    assert!(d.app().session.active_sequence().unwrap().mark_in.is_some());
    d.ok("ui.set", json!({"focused": "Text"}));
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.frames(3);
    let after = d.app().session.active_sequence().unwrap().duration();
    assert!(after < before - filmcraft_time::Tick::from_seconds_f64(1.0), "the pause is gone: {before:?} → {after:?}");
    assert!(d.ids("text.transcript.pause.").is_empty());
    assert_eq!(d.words().len(), WORDS.len(), "no word was cut");
    d.exec("edit.undo", json!({}));

    // the selected words: Extract from the toolbar removes them, Delete (forward) too
    d.double_click("text.transcript.word.4");
    d.click("text.transcript.extract");
    assert!(!d.words().contains(&"So".to_string()));
    d.double_click("text.transcript.word.0");
    d.ok("ui.set", json!({"focused": "Text"}));
    d.ok("ui.key", json!({"key": "Delete"}));
    d.frames(3);
    assert_eq!(d.words()[0], "um");
}

#[test]
fn filler_and_pause_filters_delete_all_in_one_step() {
    let mut d = Driver::transcript_tab();
    d.set_transcript();
    // Filter ▸ Filler words: "1/2 results", ∨ moves to the next and selects it
    d.click("text.transcript.filter");
    assert_eq!(d.ids("text.transcript.filter.").len(), 4, "Text, Filler words, Pauses, Search settings…");
    d.click("text.transcript.filter.fillers");
    assert_eq!(d.label("text.transcript.count"), "1/2 results");
    d.click("text.transcript.next");
    assert_eq!(d.label("text.transcript.count"), "2/2 results");
    assert_eq!(d.app().ui.transcript_sel, Some((5, 5)), "the match is selected");
    // Delete ▸ Extract ▸ Delete all: both fillers, one undo step
    d.click("text.transcript.delete");
    assert_eq!(d.ids("text.transcript.deleteMode.").len(), 2);
    let undo = d.app().session.history.undo.len();
    d.click("text.transcript.deleteAll");
    assert_eq!(d.words(), ["Hello", "there", "friend.", "So", "we", "start."]);
    assert_eq!(d.app().session.history.undo.len(), undo + 1);
    assert_eq!(d.app().session.history.undo.last().unwrap().0, "Delete All Filler Words");
    assert_eq!(d.label("text.transcript.count"), "no results");
    // Filter ▸ Pauses, Lift ▸ Delete: the current pause leaves a gap, the sequence keeps its length
    d.click("text.transcript.filter");
    d.click("text.transcript.filter.pauses");
    assert_eq!(d.label("text.transcript.count"), "1/1 results");
    let before = d.app().session.active_sequence().unwrap().duration();
    d.click("text.transcript.delete");
    d.click("text.transcript.deleteMode.lift");
    d.click("text.transcript.deleteOne");
    assert_eq!(d.app().session.active_sequence().unwrap().duration(), before);
    // text search: the counter follows the query
    d.click("text.transcript.filter");
    d.click("text.transcript.filter.text");
    d.ok("ui.set", json!({"transcriptSearch": "st"}));
    d.frames(3);
    assert_eq!(d.label("text.transcript.count"), "1/1 results", "\"start.\"");
}

#[test]
fn view_options_and_the_more_menu() {
    let mut d = Driver::transcript_tab();
    d.set_transcript();
    d.click("text.transcript.more");
    assert_eq!(d.ids("text.transcript.more.").len(), 4);
    d.click("text.transcript.more.viewOptions");
    for id in ["fillerWords", "pauses", "minPauseLength", "wholeWords", "matchCase", "save", "cancel"] {
        assert_eq!(d.ids(&format!("transcriptViewOptions.{id}")).len(), 1, "{id}");
    }
    // a shorter minimum pause length shows more pauses; Save keeps it in the preferences
    let mut draft = d.app().ui.transcript.view_options.clone().unwrap();
    assert_eq!(draft.min_pause_length, 0.75, "Premiere's default");
    draft.min_pause_length = 0.15;
    d.ok("ui.set", json!({"transcript": {"viewOptions": serde_json::to_value(&draft).unwrap()}}));
    d.click("transcriptViewOptions.save");
    assert_eq!(d.app().session.prefs.transcript.min_pause_length, 0.15);
    assert!(d.app().ui.transcript.view_options.is_none());
    assert_eq!(d.ids("text.transcript.pause.").len(), 2, "{:?}", d.ids("text.transcript.pause."));
    // { } off: a selection marks no In/Out
    d.click("text.transcript.autoInOut");
    assert!(!d.app().session.prefs.transcript.auto_in_out);
    d.double_click("text.transcript.word.1");
    assert_eq!(d.app().ui.transcript_sel, Some((1, 1)));
    assert_eq!(d.app().session.active_sequence().unwrap().mark_in, None);
    // the filler words are marked; with Filler words off they are not
    let marked = d.exec("transcript.inspect", json!({}))["words"][1]["filler"].clone();
    assert_eq!(marked, true);
}

/// Renders the Text panel's states to PNGs; look at them after UI changes.
#[test]
#[ignore]
fn transcript_panel_screenshots() {
    let dir = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/transcript-screenshots"));
    let mut d = Driver::with_tab(Some(dir));
    let gate = d.install_recogniser(4);
    let transcribe = egui::Id::new(("menu-dialog", "transcribe"));
    d.snapshot("empty", None);
    d.click("text.transcript.generate");
    d.snapshot("transcribe-dialog", Some(transcribe));
    // the download confirmation of a missing model
    let saved = d.app().ui.extras.dialog.clone();
    if let Some(dlg) = d.app_mut().ui.extras.dialog.as_mut() {
        dlg.info["recogniser"] = Value::Null;
        dlg.info["models"] = json!([{"id": "whisper-base", "name": "Whisper base (multilingual)", "size": 290_403_936u64, "installed": false,
            "license": "MIT (OpenAI Whisper weights); Hugging Face conversion: Apache-2.0", "source": "https://huggingface.co/openai/whisper-base",
            "description": "74 M parameters. The default: a good balance of speed and accuracy."}]);
        dlg.params["model"] = json!("whisper-base");
    }
    d.frames(2);
    d.snapshot("model-picker", Some(transcribe));
    d.click("transcribe.ok");
    d.snapshot("download-confirmation", Some(transcribe));
    d.app_mut().ui.extras.dialog = saved;
    d.frames(2);
    d.ok("ui.set", json!({"menuDialog": {"track": "A1"}}));
    d.click("transcribe.ok");
    release(&gate, 2);
    d.until("progress", |d| d.exec("transcript.status", json!({}))["progress"].as_f64().unwrap_or(0.0) > 0.1);
    d.snapshot("transcribing", None);
    release(&gate, 4);
    d.until("the job", |d| d.app().session.transcribe_jobs.is_empty());
    d.frames(3);
    // the playhead on "there"
    let t = d.exec("transcript.inspect", json!({}))["words"][2]["start"].as_i64().unwrap();
    d.app_mut().session.set_playhead(filmcraft_time::Tick(t + 1000));
    d.frames(3);
    d.snapshot("transcript", None);
    d.double_click("text.transcript.word.4");
    d.click_with("text.transcript.word.6", json!({"modifiers": {"shift": true}}));
    d.snapshot("selection", None);
    d.click("text.transcript.pause.3");
    d.snapshot("pause-selected", None);
    d.click("text.transcript.filter");
    d.snapshot("filter-menu", None);
    d.click("text.transcript.filter.fillers");
    d.click("text.transcript.delete");
    d.snapshot("filler-filter-delete-row", None);
    d.click("text.transcript.filter");
    d.click("text.transcript.filter.pauses");
    d.snapshot("pause-filter", None);
    d.click("text.transcript.filter");
    d.click("text.transcript.filter.text");
    d.ok("ui.set", json!({"transcriptSearch": "the"}));
    d.frames(3);
    d.snapshot("search", None);
    d.click("text.transcript.more");
    d.snapshot("more-menu", None);
    d.click("text.transcript.more.viewOptions");
    d.snapshot("view-options", Some(egui::Id::new("transcript-view-options")));
}
