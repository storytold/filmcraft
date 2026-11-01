//! Headless UI test of the Text panel's Transcript tab: the Transcribe button, the word view,
//! clicking words (In/Out from the selection) and extracting the selected text.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `transcript-*.png` there; without it no GPU is needed.

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
        let mut b = Harness::builder().with_step_dt(1.0 / 60.0).with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
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

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("transcript-{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

#[test]
fn transcript_tab_selects_and_extracts_words() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Transcript"}));
    d.frames(3);
    assert_eq!(d.ids("text.transcript.generate"), vec!["text.transcript.generate".to_string()], "empty state offers Transcribe");

    // bring a transcript for the first A1 clip's media (as an agent without a speech model would)
    let mut probe = Session::default();
    probe.execute("file.openDemoProject", json!({})).unwrap();
    let a = probe.active_sequence().unwrap().audio_tracks[0].items[0].clone();
    let tk = |s: f64| a.source_in.0 + (s * filmcraft_time::TICKS_PER_SECOND as f64) as i64;
    let words: Vec<Value> = ["Hello", "um", "there", "friend."]
        .iter()
        .enumerate()
        .map(|(i, w)| json!({"text": w, "start": tk(0.5 + i as f64 * 0.5), "end": tk(0.9 + i as f64 * 0.5), "speaker": 0}))
        .collect();
    d.ok("engine.execute", json!({"command": "transcript.set", "params": {"item": a.item.0, "transcript": {"language": "en", "words": words}}}));
    d.frames(3);
    let ids = d.ids("text.transcript.word.");
    assert_eq!(ids.len(), 4, "{ids:?}");
    d.snapshot("words");

    d.ok("ui.click", json!({"id": "text.transcript.word.1"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.extract"}));
    d.frames(3);
    let r = d.ok("engine.execute", json!({"command": "transcript.inspect", "params": {}}));
    let left: Vec<&str> = r["words"].as_array().unwrap().iter().filter_map(|w| w["text"].as_str()).collect();
    assert_eq!(left, ["Hello", "there", "friend."]);
    assert_eq!(d.ids("text.transcript.word.").len(), 3);

    d.ok("engine.execute", json!({"command": "source.open", "params": {"item": a.item.0}}));
    d.frames(3);
    d.ok("ui.panel.show", json!({"panel": "Text"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.view.source"}));
    d.frames(3);
    d.snapshot("source-before");
    assert_eq!(
        d.ids("text.transcript.word.").len(),
        4,
        "source still contains extracted words; source={:?}, view={:?}",
        d.harness.state().session.state.source_item,
        d.harness.state().ui.transcript_source
    );
    d.frames(40);
    d.ok("ui.click", json!({"id": "text.transcript.word.0"}));
    d.ok("ui.click", json!({"id": "text.transcript.word.0"}));
    d.frames(3);
    d.snapshot("correction");
    assert_eq!(d.ids("text.transcript.correction").len(), 3, "editor state: {:?}", d.harness.state().ui.transcript_edit);
    d.ok("ui.click", json!({"id": "text.transcript.correction"}));
    d.ok("ui.key", json!({"key": "A", "command": true}));
    d.ok("ui.type", json!({"text": "Howdy"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.transcript.correction.apply"}));
    d.frames(3);
    let source = d.ok("engine.execute", json!({"command": "transcript.source", "params": {}}));
    assert_eq!(source["words"][0]["text"], "Howdy");
    d.snapshot("source");
}

#[test]
fn speaker_menu_renames_assigns_and_starts_background_identification() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.ok("ui.click", json!({"id": "text.tab.Transcript"}));
    let item = d.harness.state().session.active_sequence().unwrap().audio_tracks[0].items[0].item;
    d.ok(
        "engine.execute",
        json!({"command": "transcript.set", "params": {"item": item.0, "transcript": {"words": [
            {"text": "Hello", "start": 0, "end": filmcraft_time::TICKS_PER_SECOND, "speaker": 0},
            {"text": "again", "start": filmcraft_time::TICKS_PER_SECOND, "end": 2 * filmcraft_time::TICKS_PER_SECOND, "speaker": 0}
        ]}}}),
    );
    d.ok("engine.execute", json!({"command": "source.open", "params": {"item": item.0}}));
    d.frames(3);
    d.ok("ui.panel.show", json!({"panel": "Text"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.view.source"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.paragraph.0"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.speaker.0.name"}));
    d.ok("ui.type", json!({"text": "Host"}));
    d.frames(2);
    d.snapshot("speaker-menu");
    d.ok("ui.click", json!({"id": "text.transcript.speaker.0.rename"}));
    d.frames(3);
    assert_eq!(d.harness.state().session.project.transcripts[&item].speakers[0].name, "Host");
    d.ok("ui.click", json!({"id": "text.transcript.paragraph.0"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.speaker.0.name"}));
    d.ok("ui.type", json!({"text": "Guest"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.transcript.speaker.0.new"}));
    d.frames(3);
    assert!(d.harness.state().session.project.transcripts[&item].words.iter().all(|w| w.speaker == Some(1)));
    d.ok("ui.click", json!({"id": "text.transcript.paragraph.0"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "text.transcript.speaker.0.clear"}));
    d.frames(3);
    assert!(d.harness.state().session.project.transcripts[&item].words.iter().all(|w| w.speaker.is_none()));
    d.ok("ui.click", json!({"id": "text.transcript.speakers"}));
    d.frames(3);
    d.snapshot("speaker-identification");
    assert!(!d.ids("speakers.bounds").is_empty());
    d.ok("ui.click", json!({"id":"speakers.detect"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "speakers.identify"}));
    for _ in 0..2000 {
        if d.harness.state().session.transcribe_jobs.is_empty() {
            break;
        }
        d.frames(1);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(d.harness.state().session.transcribe_jobs.is_empty());
    assert!(d.harness.state().session.jobs.last().unwrap().label.starts_with("Identify Speakers"));
    assert!(d.harness.state().session.project.transcripts[&item].words.iter().all(|w| w.speaker == Some(0)));
    d.snapshot("speakers-identified");
}

#[test]
fn correction_drafts_reject_changed_words_and_do_not_follow_context_switches() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace":"Captions and Graphics"}));
    let item = d.harness.state().session.active_sequence().unwrap().audio_tracks[0].items[0].item;
    let tr = json!({"words":[{"text":"Original","start":0,"end":1000}]});
    d.ok("engine.execute", json!({"command":"transcript.set","params":{"item":item.0,"transcript":tr}}));
    d.ok("engine.execute", json!({"command":"source.open","params":{"item":item.0}}));
    d.ok("ui.panel.show", json!({"panel":"Text"}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"text.tab.Transcript"}));
    d.ok("ui.click", json!({"id":"text.transcript.view.source"}));
    d.frames(40);
    d.ok("ui.click", json!({"id":"text.transcript.word.0"}));
    d.ok("ui.click", json!({"id":"text.transcript.word.0"}));
    d.frames(3);
    assert!(d.harness.state().ui.transcript_edit.is_some(), "double click opens the correction");
    d.ok("ui.click", json!({"id":"text.transcript.correction"}));
    d.ok("ui.key", json!({"key":"A","command":true}));
    d.ok("ui.type", json!({"text":"Draft"}));
    assert_eq!(d.harness.state().ui.transcript_edit.as_ref().unwrap().text, "Draft");
    d.ok("engine.execute", json!({"command":"markers.markIn","params":{"target":"program","time":100}}));
    d.frames(3);
    assert_eq!(d.harness.state().ui.transcript_edit.as_ref().unwrap().text, "Draft", "unrelated marks must preserve the edit draft");
    assert!(serde_json::to_value(&d.harness.state().ui).unwrap().get("transcript_edit").is_none(), "drafts must not survive restoration");
    d.ok("engine.execute", json!({"command":"transcript.set","params":{"item":item.0,"transcript":{"words":[{"text":"Original","start":1,"end":1000}]}}}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"text.transcript.correction.apply"}));
    let draft = d.harness.state().ui.transcript_edit.as_ref().unwrap();
    assert_eq!(draft.text, "Draft");
    assert!(draft.error.contains("changed"));
    assert_eq!(d.harness.state().session.project.transcripts[&item].words[0].text, "Original");
    d.snapshot("stale-correction");
    d.ok("ui.click", json!({"id":"text.transcript.correction.cancel"}));
    d.frames(40);
    d.ok("ui.click", json!({"id":"text.transcript.word.0"}));
    d.ok("ui.click", json!({"id":"text.transcript.word.0"}));
    d.frames(3);
    assert!(d.harness.state().ui.transcript_edit.is_some());
    d.harness.state_mut().session.state.source_item = None;
    d.frames(3);
    assert!(d.harness.state().ui.transcript_edit.is_none());
}

fn speaker_manager_fixture() -> (Driver, filmcraft_project::ItemId, filmcraft_project::ItemId) {
    let mut d = Driver::demo();
    let clips = d.harness.state().session.active_sequence().unwrap().audio_tracks[0].items[..2].to_vec();
    for clip in &clips {
        let start = clip.source_in.0;
        d.ok(
            "engine.execute",
            json!({"command":"transcript.set","params":{"item":clip.item.0,"transcript":{"words":[
                {"text":"Hello","start":start+filmcraft_time::TICKS_PER_SECOND/5,"end":start+filmcraft_time::TICKS_PER_SECOND/2,"speaker":0},
                {"text":"reply","start":start+filmcraft_time::TICKS_PER_SECOND,"end":start+2*filmcraft_time::TICKS_PER_SECOND,"speaker":1}
            ]}}}),
        );
    }
    d.ok("ui.set", json!({"workspace":"Captions and Graphics"}));
    d.ok("ui.click", json!({"id":"text.tab.Transcript"}));
    d.frames(3);
    (d, clips[0].item, clips[1].item)
}

fn rename_field(d: &mut Driver, item: filmcraft_project::ItemId, index: usize, name: &str) {
    d.ok("ui.click", json!({"id":format!("speakers.{}.{}.name",item.0,index)}));
    d.ok("ui.key", json!({"key":"A","command":true}));
    d.ok("ui.type", json!({"text":name}));
    d.frames(2);
}

#[test]
fn speaker_manager_batch_rename_preview_and_cancel_preserve_source_identities() {
    let (mut d, a, b) = speaker_manager_fixture();
    let before = d.harness.state().session.project.clone();
    let undo = d.harness.state().session.history.undo.len();
    d.ok("ui.click", json!({"id":"text.transcript.speakers"}));
    d.frames(3);
    assert_eq!(d.ids(&format!("speakers.{}.", a.0)).len(), 4);
    assert_eq!(d.ids(&format!("speakers.{}.", b.0)).len(), 4);
    rename_field(&mut d, a, 0, "Alice");
    rename_field(&mut d, b, 0, "Bob");
    d.snapshot("speaker-manager-batch");
    d.ok("ui.click", json!({"id":"speakers.detect"}));
    assert!(d.ids("speakers.limit").is_empty(), "pending name drafts cannot be replaced by detection");
    d.harness.state_mut().source_playback.clock.looping = true;
    d.ok("ui.click", json!({"id":format!("speakers.{}.0.preview",a.0)}));
    d.frames(2);
    assert_eq!(d.harness.state().session.state.source_item, Some(a));
    assert!(d.harness.state().source_playback.clock.stop_at.is_some());
    assert_eq!(d.harness.state().session.project, before);
    d.ok("ui.click", json!({"id":"speakers.apply"}));
    d.frames(3);
    assert!(d.harness.state().ui.speaker_dialog.is_none());
    assert!(!d.harness.state().source_playback.clock.playing);
    assert!(d.harness.state().source_playback.clock.looping, "preview restores the old loop preference");
    assert_eq!(d.harness.state().session.history.undo.len(), undo + 1);
    assert_eq!(d.harness.state().session.project.transcripts[&a].speakers[0].name, "Alice");
    assert_eq!(d.harness.state().session.project.transcripts[&b].speakers[0].name, "Bob");
    for id in [a, b] {
        assert_eq!(d.harness.state().session.project.transcripts[&id].words, before.transcripts[&id].words);
    }
    let after = d.harness.state().session.project.clone();
    d.ok("ui.panel.show", json!({"panel":"Text"}));
    d.frames(3);
    d.ok("engine.execute", json!({"command":"edit.undo"}));
    assert_eq!(d.harness.state().session.project, before);
    d.ok("engine.execute", json!({"command":"edit.redo"}));
    assert_eq!(d.harness.state().session.project, after);
    d.ok("ui.click", json!({"id":"text.transcript.speakers"}));
    rename_field(&mut d, a, 0, "Discard");
    d.ok("ui.click", json!({"id":"speakers.cancel"}));
    assert_eq!(d.harness.state().session.project, after);
}

#[test]
fn speaker_manager_rejects_stale_drafts_and_closes_on_source_or_project_change() {
    let (mut d, a, b) = speaker_manager_fixture();
    d.ok("engine.execute", json!({"command":"textPanel.showSourceTranscript","params":{"item":a.0}}));
    d.ok("ui.click", json!({"id":"text.transcript.speakers"}));
    rename_field(&mut d, a, 0, "Draft");
    d.ok("engine.execute", json!({"command":"transcript.correctWord","params":{"item":a.0,"index":0,"text":"Changed"}}));
    d.frames(3);
    let current = d.harness.state().session.project.clone();
    d.ok("ui.click", json!({"id":"speakers.apply"}));
    assert_eq!(d.harness.state().session.project, current);
    assert!(d.harness.state().ui.speaker_dialog.is_some());
    d.snapshot("speaker-manager-stale");
    d.ok("engine.execute", json!({"command":"source.open","params":{"item":b.0}}));
    d.frames(3);
    assert!(d.harness.state().ui.speaker_dialog.is_none());
    d.ok("engine.execute", json!({"command":"textPanel.manageSpeakers"}));
    d.frames(3);
    assert!(d.harness.state().ui.speaker_dialog.is_some());
    d.ok("engine.execute", json!({"command":"file.openDemoProject"}));
    d.frames(3);
    assert!(d.harness.state().ui.speaker_dialog.is_none());
}

#[test]
fn speaker_manager_empty_state_detection_can_be_cancelled_without_a_job() {
    let (mut d, a, _) = speaker_manager_fixture();
    d.ok("engine.execute", json!({"command":"transcript.set","params":{"item":a.0,"transcript":{"words":[{"text":"Unassigned","start":0,"end":1000}]}}}));
    d.ok("engine.execute", json!({"command":"textPanel.showSourceTranscript","params":{"item":a.0}}));
    let before = d.harness.state().session.project.clone();
    d.ok("ui.click", json!({"id":"text.transcript.speakers"}));
    d.frames(3);
    assert!(d.ids(&format!("speakers.{}.", a.0)).is_empty());
    d.snapshot("speaker-manager-empty");
    d.ok("ui.click", json!({"id":"speakers.detect"}));
    d.frames(3);
    assert_eq!(d.ids("speakers.limit").len(), 1);
    d.snapshot("speaker-manager-detection");
    d.ok("ui.click", json!({"id":"speakers.back"}));
    d.frames(3);
    assert!(d.ids("speakers.limit").is_empty());
    d.ok("ui.click", json!({"id":"speakers.cancel"}));
    assert_eq!(d.harness.state().session.project, before);
    assert!(d.harness.state().session.jobs.is_empty());
}
