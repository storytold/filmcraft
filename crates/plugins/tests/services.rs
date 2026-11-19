//! `Services` over a real `Session`: `cmd.execute`, `cmd.list`, `doc.query`, `notify`, the
//! host commands, calls from other threads, and the host-command lock.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::guests::{EDITS, edits_package};
use common::*;
use craft_command::{DynCommand, Enablement, Owner, Visibility};
use craft_plugin_abi::permissions::DocScope;
use craft_plugin_host::services::{CallOutcome, CommandFailure, QueryFailure, Services, UserNotice};
use filmcraft_engine::EngineError;
use filmcraft_engine::Session;
use filmcraft_plugins::services::HOST_PREFIX;
use filmcraft_plugins::{HostConfig, PluginHost};
use serde_json::{Value, json};

fn owner() -> Owner {
    Owner { plugin_id: Arc::from("test.direct"), generation: 1 }
}

#[test]
fn a_guest_doc_query_reads_the_open_project() {
    let (mut session, _host) = rig();
    install(&_host, &edits_package());
    let answer = session.execute("plugin.test.edits.query", json!({})).unwrap();
    assert_eq!(answer["name"], json!(session.project.name));
    assert_eq!(answer["items"], json!(session.project.items.len()));
    assert_eq!(answer["revision"], json!(session.revision));
    assert_eq!(answer["dirty"], json!(session.is_dirty()));
    let seq = &answer["active_sequence"];
    assert!(seq["video_tracks"].as_array().is_some_and(|t| !t.is_empty()), "the demo sequence has video tracks: {seq}");
    let active = session.active_sequence().unwrap();
    assert_eq!(seq["duration_ticks"], json!(active.duration().0));
}

#[test]
fn doc_query_answers_each_scope_and_each_question() {
    let (mut session, host) = rig();
    let services = host.services.clone();
    let ask = |session: &mut Session, scope, q: Value| host.bridge.scoped(session, || services.doc_query(&owner(), scope, &q));
    assert!(ask(&mut session, DocScope::Metadata, json!({"what": "sequence"})).unwrap()["width"].is_u64());
    assert!(ask(&mut session, DocScope::Metadata, json!({"what": "selection"})).unwrap()["clips"].is_array());
    assert_eq!(ask(&mut session, DocScope::Metadata, json!({"what": "nonsense"})), Err(QueryFailure::BadQuery("unknown `what`: nonsense".into())));
    assert_eq!(ask(&mut session, DocScope::Pixels, json!({})), Err(QueryFailure::Unsupported), "no pixel snapshot is served");
}

#[test]
fn a_host_call_outside_a_guest_call_has_no_session_and_says_so() {
    // Outside a guest call the session is borrowed by the caller above us.
    let (_session, host) = rig();
    let r = host.services.doc_query(&owner(), DocScope::Metadata, &json!({}));
    assert_eq!(r, Err(QueryFailure::Unsupported));
    let r = host.services.execute_command(&owner(), "file.newBin", &json!({"name": "x"}));
    assert!(matches!(r, Err(CommandFailure::Failed(ref m)) if m.contains("busy")), "{r:?}");
}

#[test]
fn cmd_list_hides_the_host_commands_and_cmd_execute_refuses_them() {
    let (mut session, host) = rig();
    install(&host, &edits_package());
    let listed = session.execute("plugin.test.edits.list", json!({})).unwrap();
    let ids: Vec<&str> = listed["commands"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"file.newBin"));
    assert!(ids.iter().all(|id| !id.starts_with(HOST_PREFIX)), "a plug-in must not see the install/grant commands");
    assert!(session.registry.resolve("plugin.app.filmcraft.grant").is_some(), "they do exist for the user and the control channel");

    // A plug-in with commands.run tries to switch itself off through the host command. The host
    // refuses before the command is looked up (`plugin_may_name_command`), so the guest sees E_DENIED.
    let answer = session.execute("plugin.test.edits.host-cmd", json!({})).unwrap();
    assert!(answer["error"].as_str().is_some_and(|e| e.contains("E_DENIED")), "{answer}");
    assert_active(&host, EDITS);
}

#[test]
fn host_commands_install_grant_and_remove_a_plugin_by_the_control_surface() {
    let (mut session, host) = rig();
    let dir = std::env::temp_dir().join(format!("filmcraft-plugins-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("edits.wasm");
    std::fs::write(&path, edits_package()).unwrap();

    let r = session.execute("plugin.app.filmcraft.install", json!({"path": path})).unwrap();
    assert_eq!(r["outcome"], json!("needs-grant"), "{r}");
    assert_eq!(r["required"], json!(["commands.run", "doc.read"]));
    assert_eq!(host.manager.info(EDITS).map(|i| i.id), None, "nothing is installed before the answer");
    assert_eq!(host.waiting_prompts(), vec![EDITS.to_string()], "the permission prompt is queued for the window");

    let r = session.execute("plugin.app.filmcraft.grant", json!({"id": EDITS})).unwrap();
    assert_eq!(r["outcome"], json!("loaded"));
    assert_active(&host, EDITS);
    let listed = session.execute("plugin.app.filmcraft.list", json!({})).unwrap();
    let row = listed["plugins"].as_array().unwrap().iter().find(|p| p["id"] == json!(EDITS)).unwrap();
    assert_eq!(row["grants"], json!(["commands.run", "doc.read"]));
    assert!(row["commands"].as_array().unwrap().len() >= 8);

    session.execute("plugin.app.filmcraft.disable", json!({"id": EDITS})).unwrap();
    assert!(session.registry.resolve("plugin.test.edits.ok").is_none(), "disable takes the commands away");
    session.execute("plugin.app.filmcraft.enable", json!({"id": EDITS})).unwrap();
    assert!(session.registry.resolve("plugin.test.edits.ok").is_some());
    session.execute("plugin.app.filmcraft.remove", json!({"id": EDITS})).unwrap();
    assert!(session.registry.resolve("plugin.test.edits.ok").is_none());
    assert!(host.manager.info(EDITS).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn declining_the_prompt_runs_nothing_and_stores_the_plugin_disabled() {
    let (mut session, host) = rig();
    let outcome = host.install_bytes(&edits_package(), craft_plugin_host::store::Source::File).unwrap();
    assert!(matches!(outcome, craft_plugin_host::lifecycle::InstallOutcome::NeedsGrant(_)));
    let r = session.execute("plugin.app.filmcraft.grant", json!({"id": EDITS, "accept": false})).unwrap();
    assert_eq!(r["outcome"], json!("stored-disabled"));
    assert!(session.registry.resolve("plugin.test.edits.ok").is_none());
}

#[test]
fn a_call_from_another_thread_runs_on_the_session_thread_when_pumped() {
    let (mut session, host) = rig();
    let undo_before = session.history.undo.len();
    let services = host.services.clone();
    let worker = std::thread::spawn(move || services.execute_command(&owner(), "file.newBin", &json!({"name": "from a worker"})));
    // The worker waits. Serve it the way the app does, once per frame.
    let mut waited = 0;
    while !worker.is_finished() {
        host.pump(&mut session);
        std::thread::sleep(Duration::from_millis(2));
        waited += 1;
        assert!(waited < 2000, "the worker never finished");
    }
    let result = worker.join().unwrap();
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(session.history.undo.len(), undo_before + 1, "one undo step");
}

#[test]
fn a_call_from_another_thread_gives_up_when_nobody_pumps() {
    let mut session = Session::default();
    let cfg = HostConfig { wait: Duration::from_millis(60), ..HostConfig::default() };
    let host = PluginHost::start(&mut session, cfg).unwrap();
    let before = common::project_bytes(&session);
    let services = host.services.clone();
    let result = std::thread::spawn(move || services.execute_command(&owner(), "file.newBin", &json!({"name": "late"}))).join().unwrap();
    assert!(matches!(&result, Err(CommandFailure::Failed(m)) if m.contains("did not answer")), "{result:?}");
    // The abandoned job is skipped when the app pumps later, so it cannot edit behind the plug-in's back.
    host.pump(&mut session);
    assert_eq!(common::project_bytes(&session), before);
}

#[test]
fn a_worker_scope_that_faults_is_rolled_back_when_nothing_else_touched_the_document() {
    let (mut session, host) = rig();
    let before = common::project_bytes(&session);
    let services = host.services.clone();
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let worker = std::thread::spawn(move || {
        let scope = services.begin_call(&owner(), "hook");
        let r = services.execute_command(&owner(), "file.newBin", &json!({"name": "will be undone"}));
        services.end_call(scope, &CallOutcome::Faulted("trap".into()));
        flag.store(true, Ordering::Release);
        r
    });
    while !done.load(Ordering::Acquire) {
        host.pump(&mut session);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(worker.join().unwrap().is_ok());
    assert_eq!(common::project_bytes(&session), before, "the worker's edit is gone");
}

#[test]
fn a_worker_scope_that_faults_does_not_undo_work_that_is_not_its_own() {
    let (mut session, host) = rig();
    let services = host.services.clone();
    let (edited_tx, edited_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        let scope = services.begin_call(&owner(), "hook");
        let r = services.execute_command(&owner(), "file.newBin", &json!({"name": "worker"}));
        edited_tx.send(()).unwrap();
        go_rx.recv().unwrap();
        services.end_call(scope, &CallOutcome::Faulted("trap".into()));
        r
    });
    while edited_rx.try_recv().is_err() {
        host.pump(&mut session);
        std::thread::sleep(Duration::from_millis(2));
    }
    // The user edits after the worker did.
    session.execute("file.newBin", json!({"name": "the user's own edit"})).unwrap();
    let with_both = common::project_bytes(&session);
    go_tx.send(()).unwrap();
    while !worker.is_finished() {
        host.pump(&mut session);
        std::thread::sleep(Duration::from_millis(2));
    }
    worker.join().unwrap().unwrap();
    assert_eq!(common::project_bytes(&session), with_both, "the user's edit must survive; the rollback is refused");
    host.pump(&mut session);
    let toast = session
        .drain_events()
        .into_iter()
        .find_map(|e| match e {
            filmcraft_engine::Event::Toast { message, .. } if message.contains("left in place") => Some(message),
            _ => None,
        })
        .expect("the user is told the edits were left in place");
    assert!(toast.contains("changed after"), "{toast}");
}

#[test]
fn notices_reach_the_session_on_the_next_pump() {
    let (mut session, host) = rig();
    let _ = session.drain_events();
    host.services.notify(UserNotice { plugin_id: "test.direct".into(), contribution: None, message: "hello user".into() });
    assert!(session.drain_events().iter().all(|e| !matches!(e, filmcraft_engine::Event::Toast { .. })), "nothing is shown before the pump");
    host.pump(&mut session);
    let events = session.drain_events();
    assert!(events.iter().any(|e| matches!(e, filmcraft_engine::Event::Toast { message, error: true } if message == "hello user")), "{events:?}");
}

#[test]
fn a_guest_runs_a_builtin_command_that_changes_the_document() {
    let (mut session, host) = rig();
    install(&host, &edits_package());
    session.execute("file.newBin", json!({"name": "to undo"})).unwrap();
    let before = common::project_bytes(&session);
    session.execute("plugin.test.edits.run-undo", json!({})).unwrap();
    assert_ne!(common::project_bytes(&session), before, "the guest's cmd.execute edit.undo ran");
}

#[test]
fn a_plugin_command_that_runs_itself_stops_at_the_nesting_cap() {
    let (mut session, host) = rig();
    install(&host, &edits_package());
    let result = session.execute("plugin.test.edits.recurse", json!({}));
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(session.exec_depth, 0, "every level unwound");
    host.pump(&mut session);
}

#[test]
fn link_calls_say_so_when_craft_ipc_is_missing() {
    let (_session, host) = rig();
    let r = host.services.link_apps(&owner());
    assert!(matches!(r, Err(craft_plugin_host::services::LinkFailure::Failed(m)) if m.contains("not available")));
}

/// A command that edits twice, the way a compound built-in does. The body gets the real session.
fn register_double(session: &Session) {
    let cmd = DynCommand {
        id: Arc::from("plugin.test.double.run"),
        label: Arc::from("Double"),
        params: Arc::from("{}"),
        shortcut: None,
        menu: Arc::from([]),
        owner: Owner { plugin_id: Arc::from("test.double"), generation: 1 },
        enablement: Enablement::Always,
        visibility: Visibility { menu: false, palette: true, shortcut: false },
        journal: true,
        run: Arc::new(move |s: &mut Session, _p: &Value| {
            s.edit("first half", |p, _| {
                p.add_bin("one", None);
                Ok(())
            })?;
            s.edit("second half", |p, _| {
                p.add_bin("two", None);
                Ok(())
            })?;
            Ok::<Value, EngineError>(json!({"done": true}))
        }),
    };
    session.registry.register(cmd).unwrap();
}

#[test]
fn a_command_that_edits_twice_is_one_undo_step_for_the_plugin() {
    let (mut session, host) = rig();
    register_double(&session);
    let before = common::project_bytes(&session);
    let undo_before = session.history.undo.len();
    let services = host.services.clone();
    let r = host.bridge.scoped(&mut session, || services.execute_command(&owner(), "plugin.test.double.run", &json!({})));
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(session.history.undo.len(), undo_before + 1, "two edits, one step");
    assert_ne!(common::project_bytes(&session), before);
    session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(common::project_bytes(&session), before, "one undo takes back both halves");
}
