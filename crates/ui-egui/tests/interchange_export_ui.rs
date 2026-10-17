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
            let requested_time = raw.time;
            *self.harness.input_mut() = raw;
            self.harness.step();
            if let Some(time) = requested_time {
                assert_eq!(ctx.input(|i| i.time), time, "the rendered frame uses the requested test clock");
            }
        }
    }

    fn at(&mut self, time: f64) {
        assert!(time >= self.harness.ctx.input(|i| i.time), "the test clock never moves backwards");
        self.harness.input_mut().time = Some(time);
        self.frames(1);
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
    s.execute("file.newSequence", json!({"name": "Mix", "fps": 25, "audio": 2, "video": 1})).unwrap();
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

// Successful export conversion feedback: this fixture uses ordinary supported audio alongside
// a named title and a marker, whose conversion reports are produced independently below.
fn feedback_session() -> Session {
    let mut s = session();
    let item = s.active_sequence().unwrap().audio_tracks[0].items[0].item;
    s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 1.0})).unwrap();
    let clips: Vec<_> = s.active_sequence().unwrap().audio_tracks[0].items.iter().map(|c| c.id).collect();
    s.state.selection = clips.clone();
    let ids: Vec<_> = clips.iter().map(|id| id.0).collect();
    s.execute("clip.audioGain", json!({"clips": ids, "mode": "set", "db": 6.0})).unwrap();
    s.execute("graphics.newText", json!({"text": "Opening", "newClip": true, "seconds": 1.0, "track": 0, "time": 0})).unwrap();
    s.execute("markers.add", json!({"target": "program", "frame": 6, "name": "Review"})).unwrap();
    s
}

fn feedback_driver(mut session: Session, dir: &std::path::Path) -> Driver {
    let original = session.project.clone();
    session.execute("markers.add", json!({"target": "program", "frame": 12, "name": "Temporary history fixture"})).unwrap();
    session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(session.project.as_ref(), original.as_ref(), "the fixture edit is fully undone");
    assert!(!session.history.undo.is_empty() && !session.history.redo.is_empty(), "export must preserve real undo and redo branches");
    session.execute("events.clear", json!({})).unwrap();
    session.log.push(filmcraft_engine::panels::Level::Info, "app", "Existing review note");
    let mut d = Driver::new(session, dir.to_path_buf());
    d.harness.state_mut().ui.status = "Previous review status".into();
    d
}

fn feedback_snapshot(s: &Session) -> Value {
    let undo: Vec<_> = s.history.undo.iter().map(|(label, project)| json!([label, project.as_ref()])).collect();
    let redo: Vec<_> = s.history.redo.iter().map(|(label, project)| json!([label, project.as_ref()])).collect();
    json!({
        "project": s.project.as_ref(), "editor": s.state,
        "undo": undo, "redo": redo, "historyLimit": s.history.limit, "mergeKey": s.history.merge_key,
        "revision": s.revision, "savedRevision": s.saved_revision, "path": s.path,
    })
}

fn feedback_events(d: &mut Driver) -> Vec<Value> {
    d.ok("engine.execute", json!({"command": "events.list", "params": {}}))["entries"].as_array().unwrap().clone()
}

type FeedbackKey = (String, String, String); // level, canonical command source, rendered message

type FeedbackCounts = std::collections::BTreeMap<FeedbackKey, u64>;

fn feedback_key(entry: &Value) -> FeedbackKey {
    (entry["level"].as_str().unwrap().into(), entry["source"].as_str().unwrap().into(), entry["message"].as_str().unwrap().into())
}

fn feedback_added(before: &[Value], after: &[Value]) -> FeedbackCounts {
    let mut counts = FeedbackCounts::new();
    for entry in after {
        *counts.entry(feedback_key(entry)).or_default() += entry["count"].as_u64().unwrap();
    }
    for entry in before {
        let kept = after.iter().find(|now| now["id"] == entry["id"]).expect("preexisting Events entry is retained");
        assert_eq!(feedback_key(kept), feedback_key(entry));
        assert!(kept["count"].as_u64().unwrap() >= entry["count"].as_u64().unwrap());
        *counts.get_mut(&feedback_key(entry)).unwrap() -= entry["count"].as_u64().unwrap();
    }
    counts.retain(|_, count| *count != 0);
    counts
}

fn feedback_messages(report: &filmcraft_interchange::Report) -> Vec<String> {
    report.entries.iter().map(|entry| if entry.count > 1 { format!("{} (×{})", entry.message, entry.count) } else { entry.message.clone() }).collect()
}

fn feedback_expected(report: &filmcraft_interchange::Report, source: &str) -> FeedbackCounts {
    let mut expected = FeedbackCounts::new();
    for (entry, message) in report.entries.iter().zip(feedback_messages(report)) {
        let level = match entry.level {
            filmcraft_interchange::Level::Info => "info",
            filmcraft_interchange::Level::Warning => "warning",
        };
        // Report.count counts affected clips; Event.count counts deliveries of the rendered note.
        *expected.entry((level.into(), source.into(), message)).or_default() += 1;
    }
    expected
}

fn feedback_report(s: &Session, format: filmcraft_interchange::Format) -> (Vec<u8>, filmcraft_interchange::Report) {
    filmcraft_interchange::export(&s.project, s.state.active_sequence.unwrap(), format, &Default::default()).unwrap()
}

fn feedback_status(path: &std::path::Path, notes: bool) -> String {
    let text = if notes { "Exported with conversion notes; see Events: {path}" } else { "Exported {path}" };
    filmcraft_ui_egui::i18n::fmt(filmcraft_ui_egui::i18n::t(text), &[("path", path.to_string_lossy().as_ref())])
}

fn feedback_assert(d: &mut Driver, before: &Value, events: &[Value], report: &filmcraft_interchange::Report, source: &str, path: &std::path::Path) {
    assert_eq!(feedback_snapshot(&d.harness.state().session), *before);
    let after = feedback_events(d);
    assert_eq!(feedback_added(events, &after), feedback_expected(report, source), "exact conversion-event delta; no extra UI summary or duplicate delivery");
    assert_eq!(d.harness.state().ui.status, feedback_status(path, !report.entries.is_empty()));
}

fn feedback_fcp7_completion(notes: bool, cases: &[(&str, &str, bool)]) {
    for &(case, id, direct) in cases {
        let dir = tmp(&format!("feedback-fcp7-{notes}-{case}"));
        let s = if notes { feedback_session() } else { session() };
        let (expected_bytes, report) = feedback_report(&s, filmcraft_interchange::Format::Fcp7Xml);
        if notes {
            assert!(report.entries.iter().any(|entry| entry.level == filmcraft_interchange::Level::Warning && entry.message.contains("Opening")));
            assert!(report.entries.iter().any(|entry| entry.level == filmcraft_interchange::Level::Info && entry.count == 2));
        } else {
            assert!(report.entries.is_empty(), "the clean positive control really has no conversion notes");
        }
        let mut d = feedback_driver(s, &dir);
        let events = feedback_events(&mut d);
        let before = feedback_snapshot(&d.harness.state().session);
        let path = dir.join(if direct { "direct.xml" } else { "Mix.xml" });
        let params = if direct { json!({"path": path.to_string_lossy(), "format": "xml"}) } else { json!({}) };
        let result = d.ok("ui.menu.invoke", json!({"id": id, "params": params}));
        assert_eq!(std::fs::read(&path).unwrap(), expected_bytes, "the export itself succeeds before feedback is checked");
        assert_eq!(result["bytes"].as_u64(), Some(expected_bytes.len() as u64));
        assert_eq!(result["report"], json!(feedback_messages(&report)));
        feedback_assert(&mut d, &before, &events, &report, "file.exportInterchange", &path);
        if !notes {
            assert_eq!(feedback_events(&mut d), events);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn interchange_conversion_feedback_preserves_typed_notes_and_editor_state() {
    feedback_fcp7_completion(true, &[("save", "file.exportFcp7Xml", false), ("alias", "file.exportFcp7Xml", true), ("raw", "file.exportInterchange", true)]);
}

#[test]
fn interchange_dedicated_settings_and_direct_exports_publish_conversion_feedback() {
    for (id, ext, panel, format) in
        [("file.exportAaf", "aaf", "aafExport", filmcraft_interchange::Format::Aaf), ("file.exportOmf", "omf", "omfExport", filmcraft_interchange::Format::Omf)]
    {
        for direct in [false, true] {
            let dir = tmp(&format!("feedback-{ext}-{direct}"));
            let s = feedback_session();
            let (_, report) = feedback_report(&s, format);
            if ext == "aaf" {
                assert!(report.entries.iter().any(|e| e.level == filmcraft_interchange::Level::Warning && e.message.contains("Opening")));
            } else {
                // OMF is audio only: its positive oracle is the marker note, not the video title.
                assert!(report.entries.iter().any(|e| e.level == filmcraft_interchange::Level::Info && e.message == "sequence markers are not written to OMF"));
            }
            let mut d = feedback_driver(s, &dir);
            let events = feedback_events(&mut d);
            let before = feedback_snapshot(&d.harness.state().session);
            let old_status = d.harness.state().ui.status.clone();
            let path = dir.join(if direct { format!("direct.{ext}") } else { format!("Mix.{ext}") });
            let result = if direct {
                Some(d.ok("ui.menu.invoke", json!({"id": id, "params": {"path": path.to_string_lossy(), "audio": "embedded", "mixdownVideo": false}})))
            } else {
                // Cancelling settings must not publish a successful export or disturb an old note.
                d.ok("ui.menu.invoke", json!({"id": id}));
                d.frames(3);
                d.ok("ui.click", json!({"id": format!("{panel}.cancel")}));
                d.frames(3);
                assert_eq!(feedback_snapshot(&d.harness.state().session), before);
                assert_eq!(feedback_events(&mut d), events);
                assert_eq!(d.harness.state().ui.status, old_status);
                assert!(!path.exists());
                d.ok("ui.menu.invoke", json!({"id": id}));
                d.frames(3);
                d.ok("ui.click", json!({"id": format!("{panel}.audio.embedded")}));
                d.ok("ui.click", json!({"id": format!("{panel}.ok")}));
                d.frames(4);
                assert!(d.ids(&format!("{panel}.")).is_empty());
                None
            };
            let bytes = std::fs::read(&path).unwrap();
            assert!(!bytes.is_empty());
            let (imported, extracted, _) = if ext == "aaf" {
                assert!(filmcraft_interchange::aaf::sniff(&bytes));
                filmcraft_interchange::aaf::import(&bytes, &Default::default()).unwrap()
            } else {
                assert!(filmcraft_interchange::omf::sniff(&bytes));
                filmcraft_interchange::omf::import(&bytes, &Default::default()).unwrap()
            };
            assert_eq!(imported.sequences.len(), 1);
            let sequence = imported.project.sequence(imported.sequences[0]).unwrap();
            let mut placements: Vec<_> =
                sequence.audio_tracks.iter().flat_map(|track| &track.items).map(|clip| (clip.range().start, clip.range().duration)).collect();
            placements.sort();
            let second = filmcraft_time::Tick::from_seconds_f64(1.0);
            assert_eq!(placements, vec![(filmcraft_time::Tick::ZERO, second), (second, second)], "both supported audio placements survive");
            assert!(!extracted.is_empty(), "embedded supported audio survives the successful export");
            for media in extracted {
                let (pcm, channels, rate, bits) = filmcraft_interchange::wav::parse_wav(&media.wav).unwrap();
                assert_eq!((channels, rate, bits), (2, 48_000, 16));
                assert_eq!(pcm.len(), 48_000 * 2 * 2, "the embedded essence retains the full one-second stereo source");
                // Gain remains document metadata (renderAudioEffects is false), so the source tone is unchanged.
                for frame in [137usize, 997, 10_007] {
                    let sample = (frame as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
                    let expected = (sample * 32_767.0).round() as i32;
                    for channel in 0..2 {
                        let offset = (frame * 2 + channel) * 2;
                        let actual = i16::from_le_bytes([pcm[offset], pcm[offset + 1]]) as i32;
                        assert!((actual - expected).abs() <= 2, "source tone at frame {frame}, channel {channel}: {actual} versus {expected}");
                    }
                }
            }
            if let Some(result) = result {
                assert_eq!(result["bytes"].as_u64(), Some(bytes.len() as u64));
                assert_eq!(result["report"], json!(feedback_messages(&report)));
                assert_eq!(result["mediaFiles"], json!([]), "embedded audio and no video mixdown write no side files");
            }
            feedback_assert(&mut d, &before, &events, &report, id, &path);
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[test]
fn interchange_clean_exports_keep_the_existing_completion_and_event_log() {
    feedback_fcp7_completion(false, &[("save", "file.exportFcp7Xml", false), ("alias", "file.exportFcp7Xml", true)]);
}

#[test]
fn interchange_pending_and_cancelled_save_publish_nothing_until_successful_completion() {
    use filmcraft_ui_egui::native_dialogs::FileDialog;
    let dir = tmp("feedback-continuation");
    let s = feedback_session();
    let (expected_bytes, report) = feedback_report(&s, filmcraft_interchange::Format::Fcp7Xml);
    let mut d = feedback_driver(s, &dir);
    let (asked_tx, asked_rx) = channel();
    d.harness.state_mut().hooks.run_dialog = Some(Box::new(move |dialog| {
        let (answer, result) = channel();
        asked_tx.send((dialog, answer)).unwrap();
        result
    }));
    let events = feedback_events(&mut d);
    let before = feedback_snapshot(&d.harness.state().session);
    let old_status = d.harness.state().ui.status.clone();
    let path = dir.join("chosen.xml");
    for cancel in [true, false] {
        assert_eq!(d.ok("ui.menu.invoke", json!({"id": "file.exportFcp7Xml"})), json!({"dialog": "open"}));
        let (dialog, answer) = asked_rx.try_recv().unwrap();
        match dialog {
            FileDialog::SaveAs { exts, name, .. } => {
                assert_eq!(exts, vec!["xml".to_string()]);
                assert_eq!(name, "Mix.xml");
            }
            other => panic!("expected the export save picker: {other:?}"),
        }
        assert!(asked_rx.try_recv().is_err());
        d.frames(6);
        assert!(d.harness.state().native_dialog_open());
        assert_eq!(feedback_snapshot(&d.harness.state().session), before);
        assert_eq!(feedback_events(&mut d), events);
        assert_eq!(d.harness.state().ui.status, old_status);
        assert!(!path.exists());
        answer.send(if cancel { vec![] } else { vec![path.to_string_lossy().into_owned()] }).unwrap();
        d.frames(3);
        assert!(!d.harness.state().native_dialog_open());
        if cancel {
            assert_eq!(feedback_snapshot(&d.harness.state().session), before);
            assert_eq!(feedback_events(&mut d), events);
            assert_eq!(d.harness.state().ui.status, old_status);
            assert!(!path.exists());
        } else {
            assert_eq!(std::fs::read(&path).unwrap(), expected_bytes);
            feedback_assert(&mut d, &before, &events, &report, "file.exportInterchange", &path);
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn interchange_failed_final_document_write_publishes_no_successful_conversion_notes() {
    for (id, ext, format) in [
        ("file.exportFcp7Xml", "xml", filmcraft_interchange::Format::Fcp7Xml),
        ("file.exportAaf", "aaf", filmcraft_interchange::Format::Aaf),
        ("file.exportOmf", "omf", filmcraft_interchange::Format::Omf),
    ] {
        let dir = tmp(&format!("feedback-final-write-{ext}"));
        let blocker = dir.join("not-a-directory");
        std::fs::write(&blocker, b"existing file").unwrap();
        let path = blocker.join(format!("failed.{ext}"));
        let s = feedback_session();
        let (bytes, report) = feedback_report(&s, format);
        assert!(!bytes.is_empty() && !report.entries.is_empty(), "conversion succeeds independently before the blocked final write");
        let mut d = feedback_driver(s, &dir);
        let events = feedback_events(&mut d);
        let before = feedback_snapshot(&d.harness.state().session);
        let response = d.call("ui.menu.invoke", json!({"id": id, "params": {"path": path.to_string_lossy(), "audio": "embedded", "mixdownVideo": false}}));
        assert_eq!(response["ok"], false);
        let error = response["error"].as_str().unwrap();
        assert!(!error.is_empty());
        if ext != "xml" {
            // Dedicated exporters prefix final-document write errors with that exact path.
            assert!(error.starts_with(&format!("{}: ", path.display())), "{error}");
        }
        assert_eq!(d.harness.state().ui.status, error);
        assert_ne!(d.harness.state().ui.status, feedback_status(&path, true));
        assert_ne!(d.harness.state().ui.status, feedback_status(&path, false));
        assert_eq!(feedback_snapshot(&d.harness.state().session), before);
        let after = feedback_events(&mut d);
        assert!(
            feedback_added(&events, &after).keys().all(|(level, _, _)| level == "error"),
            "failure may add Error events, never successful conversion notes"
        );
        assert!(!path.exists());
        assert_eq!(std::fs::read(&blocker).unwrap(), b"existing file");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no auxiliary export or leaked sibling temporary file");
        let _ = std::fs::remove_dir_all(dir);
    }
}

// Completion delivery, rather than a changed string, restarts the existing eight-second lifetime.
#[derive(Clone, Copy)]
enum RepeatCompletionRoute {
    Direct,
    Save,
    OmfSettings,
}

fn feedback_repeat_completion(route: RepeatCompletionRoute, notes: bool, different_path: bool) {
    let (name, ext, format, source) = match route {
        RepeatCompletionRoute::Direct => ("direct", "xml", filmcraft_interchange::Format::Fcp7Xml, "file.exportInterchange"),
        RepeatCompletionRoute::Save => ("save", "xml", filmcraft_interchange::Format::Fcp7Xml, "file.exportInterchange"),
        RepeatCompletionRoute::OmfSettings => ("settings", "omf", filmcraft_interchange::Format::Omf, "file.exportOmf"),
    };
    let dir = tmp(&format!("repeat-status-{name}-{notes}-{different_path}"));
    let s = if notes { feedback_session() } else { session() };
    let (expected_bytes, report) = feedback_report(&s, format);
    assert_eq!(!report.entries.is_empty(), notes, "the fixture really supplies the intended report control");
    let mut d = feedback_driver(s, &dir);
    d.harness.state_mut().ui.show_status_bar = true;
    d.at(100.0);
    let before = feedback_snapshot(&d.harness.state().session);
    let mut events = feedback_events(&mut d);
    let mut first_bytes = None;
    let mut last_status = String::new();
    for delivery in 0..2 {
        if delivery == 1 {
            d.at(106.0);
            assert_eq!(d.harness.state().ui.status, last_status, "the original completion is still visible before repeating");
        }
        let path = dir.join(if delivery == 1 && different_path { format!("other.{ext}") } else { format!("Mix.{ext}") });
        let result = match route {
            RepeatCompletionRoute::Direct => Some(d.ok("ui.menu.invoke", json!({"id": "file.exportFcp7Xml", "params": {"path": path.to_string_lossy()}}))),
            RepeatCompletionRoute::Save => Some(d.ok("ui.menu.invoke", json!({"id": "file.exportFcp7Xml"}))),
            RepeatCompletionRoute::OmfSettings => {
                d.ok("ui.menu.invoke", json!({"id": "file.exportOmf"}));
                d.frames(3);
                d.ok("ui.click", json!({"id": "omfExport.audio.embedded"}));
                d.ok("ui.click", json!({"id": "omfExport.ok"}));
                d.frames(4);
                assert!(d.ids("omfExport.").is_empty());
                None
            }
        };
        let bytes = std::fs::read(&path).expect("successful final export before observing its completion lifetime");
        assert!(!bytes.is_empty());
        if ext == "xml" {
            assert_eq!(bytes, expected_bytes, "ordinary successful FCP7 bytes are unchanged");
        } else {
            assert!(filmcraft_interchange::omf::sniff(&bytes));
            let (imported, extracted, _) = filmcraft_interchange::omf::import(&bytes, &Default::default()).unwrap();
            assert_eq!(imported.sequences.len(), 1);
            assert!(!extracted.is_empty(), "the settings export contains supported embedded audio");
        }
        if let Some(result) = result {
            assert_eq!(result["path"], json!(path.to_string_lossy()));
            assert_eq!(result["bytes"], json!(bytes.len()));
            assert_eq!(result["report"], json!(feedback_messages(&report)));
        }
        if let Some(first) = &first_bytes {
            assert_eq!(&bytes, first, "a second completion does not change the exported document");
        } else {
            first_bytes = Some(bytes);
        }
        feedback_assert(&mut d, &before, &events, &report, source, &path);
        events = feedback_events(&mut d);
        let status = feedback_status(&path, notes);
        if delivery == 1 {
            assert_eq!(status == last_status, !different_path, "same-text regression versus changed-text control");
        }
        last_status = status;
    }
    d.at(109.0);
    assert_eq!(d.harness.state().ui.status, last_status, "the second completion survives the first completion's expiry");
    d.at(114.0);
    assert_eq!(d.harness.state().ui.status, last_status, "the existing timer retains text at exactly eight seconds");
    d.at(115.0);
    assert!(d.harness.state().ui.status.is_empty(), "the refreshed completion still expires after eight seconds");
    assert_eq!(feedback_events(&mut d), events, "timer frames never publish extra Events");
    assert_eq!(feedback_snapshot(&d.harness.state().session), before);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn interchange_completion_status_refreshes_same_path_direct_notes() {
    feedback_repeat_completion(RepeatCompletionRoute::Direct, true, false);
}

#[test]
fn interchange_completion_status_refreshes_same_path_save_notes() {
    feedback_repeat_completion(RepeatCompletionRoute::Save, true, false);
}

#[test]
fn interchange_completion_status_refreshes_same_path_settings_notes() {
    feedback_repeat_completion(RepeatCompletionRoute::OmfSettings, true, false);
}

#[test]
fn interchange_completion_status_refreshes_same_path_clean_export() {
    feedback_repeat_completion(RepeatCompletionRoute::Direct, false, false);
}

#[test]
fn interchange_completion_status_changed_path_keeps_existing_lifetime() {
    feedback_repeat_completion(RepeatCompletionRoute::Direct, true, true);
}
