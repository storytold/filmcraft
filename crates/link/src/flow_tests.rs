//! `link.editIn`, `link.focus`, `link.unlink`, `link.refresh` end to end through
//! `Session::execute`, against an in-process `craft_ipc` server that plays EffectCraft.
//!
//! These tests share process globals (the installed host, the static command hook, the prompt
//! slot), so every scenario runs inside the one test function below, in order.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft_ipc::server::{Activate, Activation, ControlSink, FrameOpen, Frames, Server, ServerConfig};
use craft_ipc::{Discovery, DocInfo, Endpoint, InstanceRecord, LinkError, RuntimeDir};
use filmcraft_engine::Session;
use filmcraft_engine::project::{ClipId, ItemId};
use filmcraft_engine::time::FrameRate;
use serde_json::{Value, json};

use crate::fixtures::{TempProject, project_json, video_session};
use crate::host::{Headless, Host, LaunchProgress, Launched, Launcher};
use crate::link_ops::{LINK_KEY, find_link};
use crate::prompt::{self, Prompt};
use crate::ui::{Answer, answer_with, apply, show_with};

/// One egui frame with the link dialog. `Context::run` loads the fonts a window needs.
fn frame(ctx: &egui::Context, session: &mut Session, host: &Host) {
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| show_with(&ui.ctx().clone(), session, host));
    output.textures_delta.clear();
}

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// What the fake EffectCraft answers to `link.open`.
#[derive(Clone)]
enum OpenMode {
    Accept,
    AcceptButLose,
    Refuse,
}

/// What the fake EffectCraft answers to `link.attach`.
#[derive(Clone)]
enum AttachMode {
    /// The comp is bound (or its sidecar opens).
    Reopen,
    /// A restarted EffectCraft whose sidecar is gone.
    CompMissing,
}

struct FakeSink {
    calls: Calls,
    mode: Arc<Mutex<OpenMode>>,
    attach: Arc<Mutex<AttachMode>>,
}

impl ControlSink for FakeSink {
    fn submit(&self, method: String, params: Value) -> Option<Receiver<Value>> {
        self.calls.lock().unwrap().push((method.clone(), params));
        let (tx, rx) = channel();
        let reply = match method.as_str() {
            "link.open" => match self.mode.lock().unwrap().clone() {
                OpenMode::Accept => open_ok(false),
                OpenMode::AcceptButLose => open_ok(true),
                OpenMode::Refuse => json!({"ok": false, "error": {"code": "payload_invalid", "message": "payload_invalid: no timeline in payload"}}),
            },
            "link.close" => json!({}),
            "link.attach" => match self.attach.lock().unwrap().clone() {
                AttachMode::Reopen => json!({"format": {"w": 320, "h": 180, "rate_num": 24, "rate_den": 1, "duration_ticks": 24, "alpha": true}, "epoch": 0}),
                AttachMode::CompMissing => {
                    json!({"ok": false, "error": {"code": "comp_missing", "message": "no link is open and the sidecar project does not exist", "data": {}}})
                }
            },
            other => json!({"ok": false, "error": {"code": "unknown_method", "message": format!("unknown method {other}")}}),
        };
        tx.send(reply).ok()?;
        Some(rx)
    }
}

fn open_ok(lose_something: bool) -> Value {
    let mut result = json!({
        "comp": "comp-1",
        "project": "/tmp/fcl-ec/link.ecproj",
        "format": {"w": 320, "h": 180, "rate_num": 24, "rate_den": 1, "duration_ticks": 24, "alpha": false}
    });
    if lose_something && let Some(map) = result.as_object_mut() {
        map.insert("unsupported".to_string(), json!([{"what": "effect", "name": "Lumetri Color", "kept_as": "none"}]));
    }
    result
}

struct FakeActivate {
    seen: Arc<Mutex<Vec<(Option<String>, Option<u32>, Option<Value>)>>>,
}

impl Activate for FakeActivate {
    fn activate(&self, token: Option<String>, from_pid: Option<u32>, open: Option<Value>) -> Activation {
        self.seen.lock().unwrap().push((token, from_pid, open));
        Activation::Requested
    }
}

struct NoFrames;

impl Frames for NoFrames {
    fn open(&self, _params: &serde_json::Map<String, Value>) -> Result<FrameOpen, LinkError> {
        Err(LinkError::NotReady("no frames in this test".into()))
    }
    fn render(&self, _ticket: &str, _header: &Value) -> Result<(Value, Vec<u8>), LinkError> {
        Err(LinkError::NotReady("no frames in this test".into()))
    }
    fn close_ticket(&self, _ticket: &str) {}
}

/// There is no EffectCraft binary to start.
struct NeverInstalled;

impl Launcher for NeverInstalled {
    fn launch(&self, _runtime: &RuntimeDir, _wait: bool) -> Result<Launched, LinkError> {
        Err(LinkError::NotInstalled { app: "effectcraft".to_string() })
    }
    fn poll(&self, _runtime: &RuntimeDir) -> Result<Launched, LinkError> {
        Err(LinkError::NotReady("not being started".to_string()))
    }
    fn progress(&self) -> Option<LaunchProgress> {
        None
    }
    fn cancel(&self) {}
}

/// A binary exists, was started, and never became ready (an old build on PATH).
struct NeverReady;

impl Launcher for NeverReady {
    fn launch(&self, _runtime: &RuntimeDir, _wait: bool) -> Result<Launched, LinkError> {
        Err(LinkError::NotReady("/old/effectcraft did not become ready within 60 s, so FilmCraft stopped it.".to_string()))
    }
    fn poll(&self, _runtime: &RuntimeDir) -> Result<Launched, LinkError> {
        Err(LinkError::NotReady("not being started".to_string()))
    }
    fn progress(&self) -> Option<LaunchProgress> {
        None
    }
    fn cancel(&self) {}
}

/// What the app being started does next, as the test decides.
enum Start {
    Idle,
    Running,
    Ready(Box<InstanceRecord>),
    Failed(String),
}

/// A launcher whose child the test steers: it starts, runs for as long as the test wants, then is
/// ready, or fails. Counts starts and cancels.
struct SlowStart {
    state: Mutex<Start>,
    started: AtomicUsize,
    cancelled: AtomicUsize,
}

impl SlowStart {
    fn new() -> Arc<Self> {
        Arc::new(Self { state: Mutex::new(Start::Idle), started: AtomicUsize::new(0), cancelled: AtomicUsize::new(0) })
    }

    fn set(&self, next: Start) {
        *self.state.lock().unwrap() = next;
    }

    fn look(&self) -> Result<Launched, LinkError> {
        let mut state = self.state.lock().unwrap();
        match std::mem::replace(&mut *state, Start::Idle) {
            Start::Idle => Err(LinkError::NotReady("not being started".to_string())),
            Start::Running => {
                *state = Start::Running;
                Ok(Launched::Starting)
            }
            Start::Ready(record) => Ok(Launched::Ready(record)),
            Start::Failed(text) => Err(LinkError::NotReady(text)),
        }
    }
}

impl Launcher for Arc<SlowStart> {
    fn launch(&self, _runtime: &RuntimeDir, wait: bool) -> Result<Launched, LinkError> {
        assert!(!wait, "this launcher only plays the window session");
        if matches!(*self.state.lock().unwrap(), Start::Idle) {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.set(Start::Running);
        }
        self.look()
    }
    fn poll(&self, _runtime: &RuntimeDir) -> Result<Launched, LinkError> {
        self.look()
    }
    fn progress(&self) -> Option<LaunchProgress> {
        matches!(*self.state.lock().unwrap(), Start::Running).then_some(LaunchProgress { elapsed: Duration::from_secs(3), ceiling: Duration::from_secs(60) })
    }
    fn cancel(&self) {
        self.cancelled.fetch_add(1, Ordering::SeqCst);
        self.set(Start::Idle);
    }
}

struct FakeEffectCraft {
    server: Server,
    calls: Calls,
    mode: Arc<Mutex<OpenMode>>,
    attach: Arc<Mutex<AttachMode>>,
    activations: Arc<Mutex<Vec<(Option<String>, Option<u32>, Option<Value>)>>>,
}

impl FakeEffectCraft {
    fn start(dir: &RuntimeDir) -> Self {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let mode = Arc::new(Mutex::new(OpenMode::Accept));
        let attach = Arc::new(Mutex::new(AttachMode::Reopen));
        let activations = Arc::new(Mutex::new(Vec::new()));
        let server = Server::start(ServerConfig {
            app: "effectcraft".to_string(),
            app_version: "0.8.3".to_string(),
            instance: None,
            runtime: Some(RuntimeDir::at(dir.path()).unwrap()),
            tcp_port: None,
            sink: Arc::new(FakeSink { calls: Arc::clone(&calls), mode: Arc::clone(&mode), attach: Arc::clone(&attach) }),
            activate: Arc::new(FakeActivate { seen: Arc::clone(&activations) }),
            frames: Arc::new(NoFrames),
        })
        .expect("fake EffectCraft starts");
        Self { server, calls, mode, attach, activations }
    }

    fn calls_to(&self, method: &str) -> Vec<Value> {
        self.calls.lock().unwrap().iter().filter(|(m, _)| m == method).map(|(_, p)| p.clone()).collect()
    }

    fn set_mode(&self, mode: OpenMode) {
        *self.mode.lock().unwrap() = mode;
    }

    fn set_attach(&self, mode: AttachMode) {
        *self.attach.lock().unwrap() = mode;
    }
}

fn fresh_session(project: &TempProject) -> (Session, ClipId) {
    let (mut session, clip) = video_session();
    session.path = Some(project.path.clone());
    crate::commands::install(&mut session);
    (session, clip)
}

fn linked_item(session: &Session) -> (ItemId, crate::record::LinkRecord) {
    let mut found = Vec::new();
    for id in session.project.items.keys() {
        if let Some(record) = crate::link_ops::record_of(&session.project, *id).expect("record parses") {
            found.push((*id, record));
        }
    }
    assert_eq!(found.len(), 1, "expected exactly one linked item");
    found.remove(0)
}

#[test]
fn link_commands_end_to_end() {
    let _exclusive = crate::manager::exclusive();
    let saved = TempProject::new("flow");
    let dir = RuntimeDir::at(format!("/tmp/fcl-{}", std::process::id())).expect("runtime dir");
    let host = Host { backend: Arc::new(Headless), launcher: Arc::new(NeverInstalled), runtime: Some(RuntimeDir::at(dir.path()).unwrap()) };
    crate::host::install(host);

    // ---- 1. EffectCraft is not installed and not running --------------------------------------
    not_installed(&saved);

    // ---- 1b. EffectCraft starts and never becomes ready ---------------------------------------
    start_never_ready(&saved, &dir);

    // ---- 1c. EffectCraft starts slowly: progress dialog, no kill, then the link -----------------
    slow_start(&saved, &dir);

    // ---- 2. EffectCraft runs: link, activate ---------------------------------------------------
    let ec = FakeEffectCraft::start(&dir);
    let (mut session, clip) = fresh_session(&saved);
    let pre_link = project_json(&session);
    let reply = session.execute("link.editIn", json!({})).expect("link.editIn");
    assert!(prompt::peek().is_none(), "a good run left a question behind");
    let link_id = reply["link_id"].as_str().unwrap().to_string();
    assert_eq!(reply["activated"], "requested");
    let opens = ec.calls_to("link.open");
    assert_eq!(opens.len(), 1);
    assert_eq!(opens[0]["link_id"], link_id);
    assert!(opens[0]["payload"]["data"].as_str().unwrap().contains("OTIO_SCHEMA"));
    {
        let seen = ec.activations.lock().unwrap();
        assert_eq!(seen.len(), 1, "app.activate was not sent");
        assert_eq!(seen[0].1, Some(std::process::id()));
        assert_eq!(seen[0].2, Some(json!({"link_id": link_id})));
        assert_eq!(seen[0].0, None, "a headless host cannot mint a token");
    }
    let (media, record) = linked_item(&session);
    assert_eq!(record.link_id, link_id);
    assert_eq!(record.target_comp, "comp-1");
    // The linked item and the nest say what they are, so neither shares the clip's name.
    assert_eq!(session.project.item(media).unwrap().name, "clip-1 (EffectCraft)");
    let nest_item = ItemId(record.nested_sequence.parse().unwrap());
    assert_eq!(session.project.item(nest_item).unwrap().name, "clip-1 (nest)");
    assert_eq!(session.active_sequence().unwrap().video_tracks[0].items[0].name, "clip-1 (EffectCraft)");
    assert!(session.project.item(media).unwrap().metadata.contains_key(LINK_KEY));
    assert_eq!(session.history.undo.len(), 1, "editIn is one undo step");
    let linked_json = project_json(&session);

    // ---- 2b. the same clip again: reopen the link, nest nothing -------------------------------
    reopen_linked_clip(&mut session, &ec, &link_id, &record, &linked_json);

    // ---- 2c. EffectCraft was restarted and lost the sidecar: rebuild from the nest, same link --
    rebuild_lost_sidecar(&mut session, &ec, &link_id, &record);
    let linked_json = project_json(&session);

    // ---- 3. focus: place restore --------------------------------------------------------------
    focus_restores_place(&mut session, &link_id);

    // ---- 4. refresh: epochs -------------------------------------------------------------------
    refresh_epochs(&mut session, &link_id);

    // ---- 5. unlink: one undo step, peer told ---------------------------------------------------
    unlink_is_one_step(&mut session, &ec, &link_id, &record, clip, &pre_link, &linked_json);

    // ---- 6. EffectCraft rejects the payload ---------------------------------------------------
    payload_rejected(&ec, &saved);

    // ---- 7. parts EffectCraft cannot keep: link at once, then keep or undo ---------------------
    parts_effectcraft_cannot_keep(&ec, &saved);

    // ---- 8. two EffectCraft windows: choose ----------------------------------------------------
    two_windows_choose(&ec, &saved, &dir);

    ec.server.shutdown().expect("shutdown");
}

/// 1. EffectCraft is not installed and not running.
fn not_installed(saved: &TempProject) {
    let (mut session, clip) = fresh_session(saved);
    let before = project_json(&session);
    let (undo, revision) = (session.history.undo.len(), session.revision);
    let err = session.execute("link.editIn", json!({})).expect_err("nothing to link to");
    assert!(err.to_string().contains("EffectCraft is not installed"), "{err}");
    match prompt::peek() {
        Some(Prompt::NotInstalled { .. }) => {}
        other => panic!("expected the install prompt, got {other:?}"),
    }
    assert_eq!(prompt::dialog_name().as_deref(), Some("LinkNotInstalled"), "ui.inspect would read null while the prompt shows");
    assert_eq!(project_json(&session), before, "NotInstalled changed the project");
    assert_eq!((session.history.undo.len(), session.revision), (undo, revision));
    assert_eq!(session.state.selection, vec![clip]);
    assert!(!std::path::Path::new(&saved.path).parent().unwrap().join("Project Links").exists());
    prompt::take();
    assert_eq!(prompt::dialog_name(), None, "no prompt, no dialog");
}

/// 1b. EffectCraft starts and never becomes ready.
fn start_never_ready(saved: &TempProject, dir: &RuntimeDir) {
    crate::host::install(Host { backend: Arc::new(Headless), launcher: Arc::new(NeverReady), runtime: Some(RuntimeDir::at(dir.path()).unwrap()) });
    let (mut session, clip) = fresh_session(saved);
    let before = project_json(&session);
    let err = session.execute("link.editIn", json!({})).expect_err("EffectCraft never came up");
    assert!(err.to_string().contains("did not become ready"), "the user must hear why, not a bare timeout: {err}");
    let question = prompt::peek().expect("a failed start must show a dialog");
    let Prompt::LaunchFailed { message } = &question else { panic!("expected the launch dialog, got {question:?}") };
    assert!(message.contains("/old/effectcraft"), "{message}");
    assert_eq!(project_json(&session), before, "a failed start changed the project");
    assert_eq!(session.state.selection, vec![clip]);
    prompt::take();
    crate::host::install(Host { backend: Arc::new(Headless), launcher: Arc::new(NeverInstalled), runtime: Some(RuntimeDir::at(dir.path()).unwrap()) });
}

/// 1c. EffectCraft starts slowly: progress dialog, no kill, then the link.
fn slow_start(saved: &TempProject, dir: &RuntimeDir) {
    let slow = SlowStart::new();
    let host =
        crate::host::install(Host { backend: Arc::new(Headless), launcher: Arc::new(Arc::clone(&slow)), runtime: Some(RuntimeDir::at(dir.path()).unwrap()) });
    let ctx = egui::Context::default();

    // The call returns at once, says why, and leaves the progress dialog.
    let (mut session, clip) = fresh_session(saved);
    let before = project_json(&session);
    let err = session.execute("link.editIn", json!({})).expect_err("EffectCraft is not up yet");
    assert!(err.to_string().contains(crate::edit_in::STARTING_MESSAGE), "{err}");
    assert_eq!(prompt::dialog_name().as_deref(), Some("LinkStarting"));
    assert!(matches!(prompt::peek(), Some(Prompt::Starting { .. })));
    assert_eq!(project_json(&session), before, "the project changed while EffectCraft started");
    assert_eq!(slow.started.load(Ordering::SeqCst), 1);

    // Frames while it runs keep the dialog and start nothing new.
    for _ in 0..3 {
        frame(&ctx, &mut session, &host);
    }
    assert_eq!(prompt::dialog_name().as_deref(), Some("LinkStarting"), "a slow start must keep its dialog");
    assert_eq!(slow.cancelled.load(Ordering::SeqCst), 0, "a healthy slow start was cancelled");
    assert_eq!(project_json(&session), before);
    // Automation that calls again joins the same start.
    session.execute("link.editIn", json!({})).expect_err("still starting");
    assert_eq!(slow.started.load(Ordering::SeqCst), 1, "a second call started a second EffectCraft");

    // EffectCraft comes up. The next frame runs the call that was waiting.
    let up = FakeEffectCraft::start(dir);
    slow.set(Start::Ready(Box::new(up.server.record().clone())));
    frame(&ctx, &mut session, &host);
    assert!(prompt::peek().is_none(), "the dialog stayed after EffectCraft came up: {:?}", prompt::peek());
    assert_ne!(project_json(&session), before, "the call that waited did not run");
    let (_media, record) = linked_item(&session);
    assert_eq!(up.calls_to("link.open").len(), 1);
    assert_eq!(up.calls_to("link.open")[0]["link_id"], record.link_id);
    assert_eq!(session.history.undo.len(), 1);

    // Cancel kills the start and leaves the project alone.
    let (mut cancelled, _clip) = fresh_session(saved);
    slow.set(Start::Idle);
    let before = project_json(&cancelled);
    // EffectCraft is up in this scenario, so close its record first: the launcher is only asked when none runs.
    up.server.shutdown().expect("shutdown");
    cancelled.execute("link.editIn", json!({})).expect_err("starting again");
    let question = prompt::peek().expect("the dialog");
    answer_with(&mut cancelled, &host, &question, Answer::CancelStart);
    assert_eq!(slow.cancelled.load(Ordering::SeqCst), 1, "Cancel did not stop the start");
    assert!(prompt::peek().is_none());
    assert_eq!(project_json(&cancelled), before);

    // A child that exits: the dialog becomes the launch-failed dialog with the reason.
    let (mut failing, clip_failing) = fresh_session(saved);
    let before = project_json(&failing);
    failing.execute("link.editIn", json!({})).expect_err("starting");
    slow.set(Start::Failed("/old/effectcraft stopped at start-up (exit status: 3) and never became ready.".to_string()));
    frame(&ctx, &mut failing, &host);
    let Some(Prompt::LaunchFailed { message }) = prompt::peek() else { panic!("expected the launch-failed dialog, got {:?}", prompt::peek()) };
    assert!(message.contains("exit status: 3"), "{message}");
    assert_eq!(project_json(&failing), before);
    assert_eq!(failing.state.selection, vec![clip_failing]);
    prompt::take();
    let _ = clip;
    crate::host::install(Host { backend: Arc::new(Headless), launcher: Arc::new(NeverInstalled), runtime: Some(RuntimeDir::at(dir.path()).unwrap()) });
}

/// 2b. The same clip again: reopen the link, nest nothing.
fn reopen_linked_clip(session: &mut Session, ec: &FakeEffectCraft, link_id: &str, record: &crate::record::LinkRecord, linked_json: &str) {
    let (item_before, _) = linked_item(session);
    let sequences = |session: &Session| session.project.items.values().filter(|item| item.as_sequence().is_some()).count();
    let (items, seqs) = (session.project.items.len(), sequences(session));
    let (undo, redo, revision, dirty) = (session.history.undo.len(), session.history.redo.len(), session.revision, session.is_dirty());
    // The user clicks the clip that now plays the EffectCraft result.
    let swapped = session.active_sequence().unwrap().video_tracks[0].items[0].clone();
    assert_eq!(swapped.item, item_before, "the clip does not use the linked item");
    session.state.selection = vec![swapped.id];
    let selection = session.state.selection.clone();
    let before_again = project_json(session);
    let activations = ec.activations.lock().unwrap().len();

    let again = session.execute("link.editIn", json!({})).expect("link.editIn on a linked clip reopens the link");
    assert_eq!(again["link_id"], link_id, "the reply names the existing link");
    assert_eq!(again["item"].as_u64(), Some(item_before.0));
    assert_eq!(again["reopened"], true);
    assert!(prompt::peek().is_none());

    assert_eq!(project_json(session), before_again, "a reopen changed the project");
    assert_eq!(project_json(session), linked_json);
    assert_eq!(session.project.items.len(), items, "a reopen added a project item");
    assert_eq!(sequences(session), seqs, "a reopen nested again");
    assert_eq!(linked_item(session).0, item_before, "exactly one linked item, the same one");
    assert_eq!((session.history.undo.len(), session.history.redo.len()), (undo, redo), "a reopen made an undo step");
    assert_eq!((session.revision, session.is_dirty()), (revision, dirty), "a reopen dirtied the project");
    assert_eq!(session.state.selection, selection);

    assert_eq!(ec.calls_to("link.open").len(), 1, "a reopen sent a second link.open");
    let attaches = ec.calls_to("link.attach");
    assert_eq!(attaches.len(), 1, "the reopen call did not reach EffectCraft");
    assert_eq!(attaches[0]["link_id"], link_id);
    assert_eq!(attaches[0]["project"], record.target_project);
    assert_eq!(attaches[0]["comp"], record.target_comp);
    {
        let seen = ec.activations.lock().unwrap();
        assert_eq!(seen.len(), activations + 1, "the reopen did not bring EffectCraft to the front");
        assert_eq!(seen[activations].2, Some(json!({"link_id": link_id})));
    }
    assert!(crate::notice::LinkNotice::from_reply(&again).is_none(), "a reopen must not raise the banner or offer Undo");
}

/// 2c. EffectCraft was restarted and lost the sidecar: rebuild from the nest, same link.
fn rebuild_lost_sidecar(session: &mut Session, ec: &FakeEffectCraft, link_id: &str, record: &crate::record::LinkRecord) {
    ec.set_attach(AttachMode::CompMissing);
    let opens = ec.calls_to("link.open").len();
    let (item_before, _) = linked_item(session);
    let items = session.project.items.len();
    let again = session.execute("link.editIn", json!({})).expect("a missing sidecar falls back to a fresh open");
    assert_eq!(again["link_id"], link_id);
    assert_eq!(again["reopened"], true);
    let reopened = ec.calls_to("link.open");
    assert_eq!(reopened.len(), opens + 1, "the fallback did not send link.open");
    assert_eq!(reopened[opens]["link_id"], link_id, "the fallback must keep the link id");
    assert_eq!(reopened[opens]["result_path"], record.result_path);
    assert!(reopened[opens]["payload"]["data"].as_str().unwrap().contains("OTIO_SCHEMA"));
    assert_eq!(session.project.items.len(), items, "the fallback created an item");
    assert_eq!(linked_item(session).0, item_before);
    assert_eq!(linked_item(session).1.link_id, link_id);
    ec.set_attach(AttachMode::Reopen);
}

/// 3. Focus: place restore.
fn focus_restores_place(session: &mut Session, link_id: &str) {
    session.state.selection.clear();
    let rate = FrameRate::FPS_24;
    let place = json!({"seconds": 0.5, "selection": [{"start_s": 0.0, "end_s": 1.0, "track_index": 0}]});
    let focused = session.execute("plugin.craft.link.focus", json!({"link_id": link_id, "place": place})).expect("link.focus");
    assert_eq!(focused["selected"], 1);
    assert_eq!(session.playhead(), rate.tick_of(12), "0.5 s at 24 fps is frame 12");
    let swapped_clip = session.active_sequence().unwrap().video_tracks[0].items[0].id;
    assert_eq!(session.state.selection, vec![swapped_clip]);
    let err = session.execute("plugin.craft.link.focus", json!({"link_id": "no-such-link"})).expect_err("unknown link");
    assert!(err.to_string().contains("link_unknown"), "{err}");
    let err = session.execute("plugin.craft.link.focus", json!({"link_id": link_id, "place": {"seconds": "not a number"}})).expect_err("bad place");
    assert!(err.to_string().contains("place"), "{err}");
}

/// 4. Refresh: epochs.
fn refresh_epochs(session: &mut Session, link_id: &str) {
    let revision = session.revision;
    let first = session.execute("plugin.craft.link.refresh", json!({"link_id": link_id, "epoch": 3})).expect("refresh");
    assert_eq!((first["epoch"].as_u64(), first["stale"].as_bool()), (Some(3), Some(false)));
    assert!(session.revision > revision, "refresh must bump the view revision so frames refetch");
    let revision = session.revision;
    let again = session.execute("plugin.craft.link.refresh", json!({"link_id": link_id, "epoch": 3})).expect("refresh");
    assert_eq!(again["stale"], true);
    assert_eq!(session.revision, revision, "a stale epoch must not bump the revision");
}

/// 5. Unlink: one undo step, peer told.
fn unlink_is_one_step(
    session: &mut Session,
    ec: &FakeEffectCraft,
    link_id: &str,
    record: &crate::record::LinkRecord,
    clip: ClipId,
    pre_link: &str,
    linked_json: &str,
) {
    let undo = session.history.undo.len();
    let unlinked = session.execute("plugin.craft.link.unlink", json!({})).expect("unlink uses the selection");
    assert_eq!(unlinked["restored"], 1);
    assert_eq!(unlinked["peer_closed"], true, "{unlinked}");
    assert_eq!(session.history.undo.len(), undo + 1, "unlink is one undo step");
    assert!(find_link(&session.project, link_id).unwrap().is_none(), "the linked item is still in the project");
    let closes = ec.calls_to("link.close");
    assert_eq!(closes.len(), 1);
    assert_eq!(closes[0]["link_id"], link_id);
    let nest = ItemId(record.nested_sequence.parse().unwrap());
    let clips: Vec<ItemId> = session.active_sequence().unwrap().video_tracks[0].items.iter().map(|c| c.item).collect();
    assert_eq!(clips, vec![nest], "the clip does not point at the nested sequence again");
    assert!(session.project.sequence(nest).is_some());
    // one undo returns to the linked state, byte for byte
    assert_eq!(session.undo().as_deref(), Some("Unlink from EffectCraft"));
    assert_eq!(project_json(session), linked_json);
    // and one more returns to before the link
    assert_eq!(session.undo().as_deref(), Some("link.editIn"));
    assert_eq!(project_json(session), pre_link, "undoing the link must restore the original clips exactly");
    assert_eq!(session.state.selection, vec![clip], "undoing the link must select the clip again");
    // unlink with nothing linked selected is a typed error
    session.state.selection = vec![clip];
    let err = session.execute("plugin.craft.link.unlink", json!({})).expect_err("nothing linked");
    assert!(err.to_string().contains("linked to EffectCraft"), "{err}");
}

/// 6. EffectCraft rejects the payload.
fn payload_rejected(ec: &FakeEffectCraft, saved: &TempProject) {
    ec.set_mode(OpenMode::Refuse);
    let (mut session, _clip) = fresh_session(saved);
    let before = project_json(&session);
    let err = session.execute("link.editIn", json!({})).expect_err("refused");
    assert!(err.to_string().contains("payload_invalid"), "{err}");
    assert_eq!(project_json(&session), before);
    assert!(prompt::peek().is_none(), "a plain failure is not a question");
}

/// 7. Parts EffectCraft cannot keep: link at once, then keep or undo.
fn parts_effectcraft_cannot_keep(ec: &FakeEffectCraft, saved: &TempProject) {
    ec.set_mode(OpenMode::AcceptButLose);
    let (mut session, _clip) = fresh_session(saved);
    let before = project_json(&session);
    let closes = ec.calls_to("link.close").len();
    let reply = session.execute("link.editIn", json!({})).expect("nest first: no question before the hand-off");
    assert!(prompt::peek().is_none(), "a blocking question was queued");
    assert_ne!(project_json(&session), before, "link.editIn did not link");
    assert_eq!(ec.calls_to("link.close").len(), closes, "the comp was closed although the link goes on");
    let pending = crate::notice::take().expect("the banner is waiting");
    assert_eq!(pending.dropped[0].name, "Lumetri Color");
    assert_eq!(reply["unsupported"][0]["name"], "Lumetri Color");
    let (_media, record) = linked_item(&session);
    assert!(!record.link_id.is_empty());

    // Undo: the project is as before and EffectCraft is told to unlink.
    let said = crate::notice::undo(
        &mut session,
        Some(
            &mut crate::peer::SocketPeer::connect(&crate::host::host(), &crate::peer::PeerChoice { instance: None, holds_project: None }, false).expect("peer"),
        ),
        &pending,
    )
    .expect("undo");
    assert!(said.contains("Undone"), "{said}");
    assert_eq!(project_json(&session), before);
    assert_eq!(ec.calls_to("link.close").len(), closes + 1, "EffectCraft was not told to unlink");
}

/// 8. Two EffectCraft windows: choose.
fn two_windows_choose(ec: &FakeEffectCraft, saved: &TempProject, dir: &RuntimeDir) {
    ec.set_mode(OpenMode::Accept);
    // A second instance. Only its discovery record matters: the chooser appears before any connect.
    let mut other =
        InstanceRecord::new("effectcraft", "0.8.2", 4_000_000, Endpoint::Unix { path: format!("{}/effectcraft-4000000.sock", dir.path().display()) }).unwrap();
    other.docs.push(DocInfo { doc_id: "d".to_string(), title: "Other comp".to_string(), path: None, extra: Default::default() });
    let _second = Discovery::publish(dir, other).expect("second record");
    let (mut session, _clip) = fresh_session(saved);
    let before = project_json(&session);
    let err = session.execute("link.editIn", json!({})).expect_err("two windows");
    assert!(err.to_string().contains("Choose one"), "{err}");
    assert_eq!(project_json(&session), before);
    let question = prompt::take().expect("a question");
    let Prompt::ChooseInstance { choices } = &question else { panic!("{question:?}") };
    assert_eq!(choices.len(), 2);
    let mine = ec.server.record().instance.clone();
    assert!(choices.iter().any(|c| c.instance == mine));
    apply(&mut session, &question, Answer::Choose(mine));
    assert_ne!(project_json(&session), before, "choosing an instance did not link");
    // naming an instance that is not listed fails and changes nothing
    let (mut session, _clip) = fresh_session(saved);
    let before = project_json(&session);
    let err = session.execute("link.editIn", json!({"instance": "ghost"})).expect_err("ghost");
    assert!(err.to_string().contains("has closed"), "{err}");
    assert_eq!(project_json(&session), before);
}

/// A clip whose source has no file renders behind the progress dialog, then links. The dialog runs
/// the same call again when the render ends, and a failed or cancelled render leaves the project alone.
#[test]
fn a_source_with_no_file_prepares_behind_the_dialog() {
    let _exclusive = crate::manager::exclusive();
    let _serial = crate::fixtures::prepare_serial();
    let saved = TempProject::new("flow-prepare");
    let dir = RuntimeDir::at(format!("/tmp/fclp-{}", std::process::id())).expect("runtime dir");
    let up = FakeEffectCraft::start(&dir);
    let host =
        crate::host::install(Host { backend: Arc::new(Headless), launcher: Arc::new(NeverInstalled), runtime: Some(RuntimeDir::at(dir.path()).unwrap()) });
    let cache = std::path::Path::new(&saved.path).with_file_name("cache").to_string_lossy().into_owned();
    let params = json!({ "cache_dir": cache });
    let ctx = egui::Context::default();
    let generator_session = || {
        let (mut session, _seq, _clip) = crate::fixtures::generator_nest(64, 36, 48);
        session.path = Some(saved.path.clone());
        crate::commands::install(&mut session);
        session
    };
    prompt::take();

    // The call returns at once and leaves the dialog. EffectCraft has not been called.
    let mut session = generator_session();
    let before = project_json(&session);
    let err = session.execute("link.editIn", params.clone()).expect_err("a render must run first");
    assert!(err.to_string().contains(crate::edit_in::PREPARING_MESSAGE), "{err}");
    assert_eq!(prompt::dialog_name().as_deref(), Some("LinkStarting"));
    assert!(matches!(prompt::peek(), Some(Prompt::Preparing { .. })));
    assert!(up.calls_to("link.open").is_empty());
    assert_eq!(project_json(&session), before, "the project changed while the clip rendered");

    // Frames run until the render ends. The next frame runs the call again and it links.
    let until = std::time::Instant::now() + Duration::from_secs(120);
    while prompt::peek().is_some() {
        assert!(std::time::Instant::now() < until, "the dialog never finished: {:?}", prompt::peek());
        frame(&ctx, &mut session, &host);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_ne!(project_json(&session), before, "the call that waited did not run");
    let opened = up.calls_to("link.open");
    assert_eq!(opened.len(), 1);
    let otio = opened[0]["payload"]["data"].as_str().expect("payload data");
    assert!(otio.contains("ExternalReference.1") && !otio.contains("GeneratorReference"), "EffectCraft must get a file, not a generator");
    assert_eq!(craft_ipc::payload::missing_media(otio), Ok(Vec::<String>::new()), "every file in the payload opens");
    assert_eq!(session.history.undo.len(), 1);

    // The same source again is a cache hit: the call finishes at once.
    let mut again = generator_session();
    again.execute("link.editIn", params.clone()).expect("cache hit");
    assert!(again.jobs.is_empty(), "a cache hit started a render");
    assert!(prompt::peek().is_none());

    // Cancel stops the render and leaves the project alone. Another source, so there is no cache hit.
    let mut cancelled = generator_session();
    if let Some(filmcraft_engine::project::ItemKind::Media(media)) = cancelled.project_mut().items.values_mut().map(|item| &mut item.kind).next() {
        media.media = filmcraft_engine::project::MediaRef::Generator(filmcraft_media::Generator::BarsAndTone);
    }
    let before = project_json(&cancelled);
    cancelled.execute("link.editIn", params.clone()).expect_err("renders");
    let question = prompt::peek().expect("the dialog");
    answer_with(&mut cancelled, &host, &question, Answer::CancelPrepare);
    assert!(prompt::peek().is_none());
    assert_eq!(project_json(&cancelled), before);
    while crate::prepare::state() != crate::prepare::State::Idle {
        assert!(std::time::Instant::now() < until, "the cancelled render did not end");
        std::thread::sleep(Duration::from_millis(20));
    }

    // A render that cannot store its movie shows why.
    let blocked = std::path::Path::new(&saved.path).with_file_name("blocked");
    std::fs::write(&blocked, b"a file, not a folder").unwrap();
    let mut failing = generator_session();
    let before = project_json(&failing);
    failing.execute("link.editIn", json!({ "cache_dir": blocked.to_string_lossy(), "wait": true })).expect_err("no cache folder");
    let Some(Prompt::PrepareFailed { message }) = prompt::peek() else { panic!("expected the failure dialog, got {:?}", prompt::peek()) };
    assert!(message.contains("link cache"), "{message}");
    assert_eq!(prompt::dialog_name().as_deref(), Some("LinkPrepareFailed"));
    assert_eq!(project_json(&failing), before);
    prompt::take();
    up.server.shutdown().expect("shutdown");
}
