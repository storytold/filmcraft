//! Edit in EffectCraft from the UI: the timeline clip menu, the key, and the message for a failure.
//! The header has no button for it (it did nothing the user could see when a call was refused).
//!
//! `link.editIn` is installed with a recording stand-in for the Craft Link crate, which this crate
//! does not depend on. The tests check what the command saw: the selection when it ran.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use filmcraft_engine::Session;
use filmcraft_engine::project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::native_dialogs::FileDialog;
use serde_json::{Value, json};

/// The selection at each run of the stand-in.
static RUNS: Mutex<Vec<Vec<u64>>> = Mutex::new(Vec::new());

/// The stand-in records into one list, so the tests take turns.
static TURN: Mutex<()> = Mutex::new(());

fn recording_edit_in(s: &mut Session, _params: &Value) -> filmcraft_engine::Result<Value> {
    let mut selection: Vec<u64> = s.state.selection.iter().map(|c| c.0).collect();
    selection.sort_unstable();
    RUNS.lock().unwrap().push(selection);
    Ok(json!({"ran": true}))
}

fn runs() -> Vec<Vec<u64>> {
    RUNS.lock().unwrap().clone()
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    _turn: MutexGuard<'static, ()>,
}

/// Paths the Save As stand-in was asked about, and what it answers next.
static SAVE_ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SAVE_ANSWER: Mutex<Option<String>> = Mutex::new(None);

impl Driver {
    /// A demo project that was never saved, with a Save As stand-in that answers `SAVE_ANSWER`.
    fn unsaved() -> Self {
        let mut d = Driver::new();
        d.app().session.path = None;
        SAVE_ASKED.lock().unwrap().clear();
        *SAVE_ANSWER.lock().unwrap() = None;
        d.app().hooks.pick_save = Some(Box::new(|name: &str| {
            SAVE_ASKED.lock().unwrap().push(name.to_string());
            SAVE_ANSWER.lock().unwrap().clone()
        }));
        d
    }

    fn new() -> Self {
        let turn = TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        filmcraft_engine::commands::set_link_edit_in(recording_edit_in);
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        // the command needs a saved project
        s.path = Some("/tmp/link-edit-in-ui.fcproj".into());
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, _turn: turn };
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

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn select(&mut self, clips: &[ClipId]) {
        let ids: Vec<u64> = clips.iter().map(|c| c.0).collect();
        self.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": ids}}));
    }

    /// Two unlinked-or-not video clips on V1.
    fn two_video_clips(&mut self) -> (ClipId, ClipId) {
        let seq = self.app().session.active_sequence().unwrap().clone();
        let v = &seq.video_tracks[0].items;
        (v[0].id, v[1].id)
    }
}

#[test]
fn header_has_no_edit_in_button() {
    let mut d = Driver::new();
    let found = d.ok("ui.elements", json!({"prefix": "header."}));
    assert!(found.as_array().unwrap().iter().any(|e| e["id"] == "header.quickExport"), "the header is laid out: {found}");
    assert!(!found.as_array().unwrap().iter().any(|e| e["id"] == "header.linkEditIn"), "no header button: {found}");
}

#[test]
fn clip_menu_runs_edit_in_for_the_right_clicked_clip() {
    let mut d = Driver::new();
    let (first, second) = d.two_video_clips();
    d.select(&[first]);
    let before = runs().len();
    // the right-click lands on a clip that is not selected: it becomes the selection first
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", second.0), "button": "right"}));
    d.frames(3);
    let menu = d.ok("ui.elements", json!({"prefix": "timeline.clipMenu."}));
    assert!(menu.as_array().unwrap().iter().any(|e| e["id"] == "timeline.clipMenu.link.editIn"), "entry in the clip menu: {menu}");
    d.ok("ui.click", json!({"id": "timeline.clipMenu.link.editIn"}));
    d.frames(3);
    let all = runs();
    assert_eq!(all.len(), before + 1, "link.editIn ran once");
    let seen = all.last().unwrap();
    assert!(seen.contains(&second.0), "the clicked clip was selected when it ran: {seen:?}");
    assert!(!seen.contains(&first.0), "the old selection was replaced: {seen:?}");
}

#[test]
fn clip_menu_entry_shows_the_current_key() {
    let mut d = Driver::new();
    let (_, second) = d.two_video_clips();
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", second.0), "button": "right"}));
    d.frames(3);
    let shown = filmcraft_ui_egui::menus::shortcut_text("Cmd+Alt+Shift+E");
    d.harness.step();
    assert!(d.harness.query_by_label_contains(&shown).is_some(), "the menu shows {shown}");
}

#[test]
fn the_key_runs_edit_in_and_follows_a_rebinding() {
    let mut d = Driver::new();
    let (first, _) = d.two_video_clips();
    d.select(&[first]);
    let before = runs().len();
    d.ok("ui.key", json!({"key": "Cmd+Alt+Shift+E"}));
    d.frames(3);
    assert_eq!(runs().len(), before + 1, "the default key runs link.editIn");
    // rebind through the same command the shortcut editor uses
    d.ok("engine.execute", json!({"command": "shortcuts.set", "params": {"command": "link.editIn", "keys": "Cmd+Shift+F9"}}));
    d.frames(3);
    d.ok("ui.key", json!({"key": "Cmd+Alt+Shift+E"}));
    d.frames(3);
    assert_eq!(runs().len(), before + 1, "the old key does nothing");
    d.ok("ui.key", json!({"key": "Cmd+Shift+F9"}));
    d.frames(3);
    assert_eq!(runs().len(), before + 2, "the new key runs it");
    let listed = d.ok("engine.execute", json!({"command": "shortcuts.list", "params": {"query": "Edit in EffectCraft"}}));
    assert!(listed.to_string().contains("link.editIn"), "the shortcut editor lists it: {listed}");
}

#[test]
fn a_refused_call_shows_a_status_line() {
    let mut d = Driver::new();
    d.select(&[]);
    let before = runs().len();
    d.ok("ui.key", json!({"key": "Cmd+Alt+Shift+E"}));
    d.frames(3);
    assert_eq!(runs().len(), before, "nothing ran with no clip selected");
    let app = d.app();
    assert!(app.ui.status.contains("select a video clip"), "status: {:?}", app.ui.status);
}

fn saves_asked() -> usize {
    SAVE_ASKED.lock().unwrap().len()
}

fn save_target(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("fc-edit-in-save-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name).to_string_lossy().into_owned()
}

#[test]
fn an_unsaved_project_saves_first_and_then_edits_in_the_same_selection() {
    let mut d = Driver::unsaved();
    let (first, _) = d.two_video_clips();
    d.select(&[first]);
    // Picking a clip also picks the clips linked to it.
    let mut chosen: Vec<u64> = d.app().session.state.selection.iter().map(|c| c.0).collect();
    chosen.sort_unstable();
    assert!(chosen.contains(&first.0));
    let target = save_target("saved-first.fcproj");
    *SAVE_ANSWER.lock().unwrap() = Some(target.clone());
    let before = runs().len();
    d.ok("ui.key", json!({"key": "Cmd+Alt+Shift+E"}));
    d.frames(3);
    assert_eq!(saves_asked(), 1, "Save As was offered once");
    assert_eq!(d.app().session.path.as_deref(), Some(target.as_str()), "the project was saved where the user chose");
    assert!(std::path::Path::new(&target).exists(), "the file is on disk");
    let all = runs();
    assert_eq!(all.len(), before + 1, "link.editIn ran after the save");
    assert_eq!(all.last().unwrap(), &chosen, "on the selection the user made");
    assert!(!d.app().ui.status.contains("EffectCraft:"), "a good run is quiet");
}

/// Save As dialogs a non-blocking host was asked to show, with the sender that answers each.
type Asked = Arc<Mutex<Vec<Sender<Vec<String>>>>>;

/// An unsaved project on a host that shows dialogs without blocking: Save As returns before the user
/// answers, and the test answers through the sender in the returned list.
fn unsaved_on_a_non_blocking_host() -> (Driver, Asked) {
    let mut d = Driver::unsaved();
    let asked: Asked = Arc::default();
    let log = Arc::clone(&asked);
    d.app().hooks.run_dialog = Some(Box::new(move |dialog: FileDialog| -> Receiver<Vec<String>> {
        assert!(matches!(dialog, FileDialog::SaveProject { .. }), "only Save As is expected: {dialog:?}");
        let (tx, rx) = channel();
        log.lock().unwrap().push(tx);
        rx
    }));
    (d, asked)
}

#[test]
fn a_non_blocking_save_as_runs_edit_in_after_the_answer_on_the_selection_it_started_with() {
    let (mut d, asked) = unsaved_on_a_non_blocking_host();
    let (first, second) = d.two_video_clips();
    d.select(&[first]);
    let mut chosen: Vec<u64> = d.app().session.state.selection.iter().map(|c| c.0).collect();
    chosen.sort_unstable();
    let target = save_target("saved-later.fcproj");
    let before = runs().len();

    d.ok("ui.key", json!({"key": "Cmd+Alt+Shift+E"}));
    d.frames(3);
    // The dialog is open. The command has not run and has not been called cancelled.
    assert_eq!(asked.lock().unwrap().len(), 1, "Save As was offered once");
    assert_eq!(runs().len(), before, "nothing runs before the answer");
    assert!(d.app().session.path.is_none(), "still unsaved");
    assert_ne!(d.app().ui.status, "Edit in EffectCraft cancelled", "the dialog is not answered yet");

    // The user clicks another clip while the dialog is open, then picks a file.
    d.select(&[second]);
    let answer = asked.lock().unwrap().pop().unwrap();
    answer.send(vec![target.clone()]).unwrap();
    d.frames(3);

    assert_eq!(d.app().session.path.as_deref(), Some(target.as_str()), "the project was saved where the user chose");
    let all = runs();
    assert_eq!(all.len(), before + 1, "link.editIn ran after the answer");
    assert_eq!(all.last().unwrap(), &chosen, "on the selection it started with");
    assert_ne!(d.app().ui.status, "Edit in EffectCraft cancelled");
}

#[test]
fn cancelling_a_non_blocking_save_as_cancels_edit_in_quietly() {
    let (mut d, asked) = unsaved_on_a_non_blocking_host();
    let (first, _) = d.two_video_clips();
    d.select(&[first]);
    let before = runs().len();
    d.ok("ui.key", json!({"key": "Cmd+Alt+Shift+E"}));
    d.frames(3);
    assert_ne!(d.app().ui.status, "Edit in EffectCraft cancelled", "the dialog is not answered yet");
    asked.lock().unwrap().pop().unwrap().send(Vec::new()).unwrap();
    d.frames(3);
    assert_eq!(runs().len(), before, "nothing ran");
    assert!(d.app().session.path.is_none(), "still unsaved");
    assert_eq!(d.app().ui.status, "Edit in EffectCraft cancelled");
}

#[test]
fn cancelling_save_as_cancels_edit_in_quietly() {
    let mut d = Driver::unsaved();
    let (first, _) = d.two_video_clips();
    d.select(&[first]);
    let before = runs().len();
    d.ok("ui.key", json!({"key": "Cmd+Alt+Shift+E"}));
    d.frames(3);
    assert_eq!(saves_asked(), 1);
    assert_eq!(runs().len(), before, "nothing ran");
    assert!(d.app().session.path.is_none(), "still unsaved");
    let app = d.app();
    assert_eq!(app.ui.status, "Edit in EffectCraft cancelled");
}

#[test]
fn an_unsaved_project_with_nothing_selected_does_not_ask_where_to_save() {
    let mut d = Driver::unsaved();
    d.select(&[]);
    let before = runs().len();
    d.ok("ui.key", json!({"key": "Cmd+Alt+Shift+E"}));
    d.frames(3);
    assert_eq!(saves_asked(), 0, "the refusal comes first");
    assert_eq!(runs().len(), before);
    assert!(d.app().ui.status.contains("select a video clip"), "status: {:?}", d.app().ui.status);
}

#[test]
fn the_edit_in_entry_is_enabled_in_an_unsaved_project() {
    let mut d = Driver::unsaved();
    let (first, _) = d.two_video_clips();
    d.select(&[first]);
    assert!(d.app().session.is_enabled("link.editIn"));
}

/// Hover a greyed entry of the Clip menu and read the tooltip.
#[test]
fn a_greyed_menu_entry_says_why_on_hover() {
    let mut d = Driver::unsaved();
    d.select(&[]);
    d.harness.ctx.all_styles_mut(|style| style.interaction.tooltip_delay = 0.0);
    d.ok("ui.click", json!({"id": "menu.Clip"}));
    d.frames(3);
    d.ok("ui.move", json!({"id": "menu.link.editIn"}));
    d.frames(6);
    d.harness.step();
    assert!(d.harness.query_by_label_contains("select a video clip in the timeline").is_some(), "the reason shows on hover");
}

#[test]
fn menu_list_carries_the_reason_of_a_greyed_entry() {
    let mut d = Driver::unsaved();
    d.select(&[]);
    let listed = d.ok("ui.menu.list", json!({}));
    let rows = listed.as_array().cloned().or_else(|| listed["items"].as_array().cloned()).unwrap_or_default();
    let row = rows.iter().find(|r| r["id"] == "link.editIn").unwrap_or_else(|| panic!("no link.editIn row in {listed}"));
    assert_eq!(row["enabled"], json!(false));
    assert_eq!(row["reason"], json!("select a video clip in the timeline"));
    let enabled = rows.iter().find(|r| r["id"] == "file.save").expect("file.save row");
    assert!(enabled.get("reason").is_none(), "an enabled entry has no reason: {enabled}");
}

#[test]
fn a_greyed_clip_menu_entry_says_why_on_hover() {
    let mut d = Driver::unsaved();
    let (first, _) = d.two_video_clips();
    d.select(&[first]);
    // Some entry of the clip menu refuses a plain video clip. Take the first one that does.
    let (cmd, why) = ["clip.ungroup", "clip.group", "clip.mergeClips", "clip.synchronize", "sequence.revealNested", "clip.replaceFromSource"]
        .into_iter()
        .find_map(|cmd| d.app().session.enabled_state(cmd).err().map(|why| (cmd, why)))
        .expect("an entry that refuses a plain video clip");
    d.harness.ctx.all_styles_mut(|style| style.interaction.tooltip_delay = 0.0);
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", first.0), "button": "right"}));
    d.frames(3);
    d.ok("ui.move", json!({"id": format!("timeline.clipMenu.{cmd}")}));
    d.frames(6);
    d.harness.step();
    assert!(d.harness.query_by_label_contains(&why).is_some(), "{cmd}: the reason {why:?} shows on hover");
}
