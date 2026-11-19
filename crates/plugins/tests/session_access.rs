//! The session reaches the places that need it. A `before` gate makes host calls, a nested
//! `cmd.execute` shows as `Origin::Plugin`, and a native dynamic command gets the real session.

mod common;

use std::sync::{Arc, Mutex};

use common::guests::{EDITS, GATE_EDITS, edits_package, gate_edits_package};
use common::*;
use craft_command::{Call, DynCommand, Enablement, Interceptor, Interest, Origin, Owner, Verdict, Visibility};
use filmcraft_engine::{EngineError, Session};
use serde_json::{Value, json};

fn project_text(session: &Session) -> String {
    String::from_utf8_lossy(&project_bytes(session)).into_owned()
}

#[test]
fn a_before_gate_reaches_the_session_for_its_host_calls() {
    let (mut session, host) = rig_frozen();
    install(&host, &gate_edits_package(&["file.newBin"]));
    assert_active(&host, GATE_EDITS);
    assert!(!project_text(&session).contains("Plug-in A"));

    // The gate runs `cmd.execute file.newBin` from inside `Hooks::before`. It used to get the busy
    // error, because only a command body could reach the session.
    session.execute("file.newBin", json!({"name": "the user's bin"})).expect("the command runs");

    let text = project_text(&session);
    assert!(text.contains("Plug-in A"), "the gate's cmd.execute reached the session");
    assert!(text.contains("the user's bin"), "the user's own command still ran");
}

struct Record {
    interest: Interest,
    seen: Arc<Mutex<Vec<(String, Origin)>>>,
}

impl Interceptor<Session> for Record {
    fn interest(&self) -> &Interest {
        &self.interest
    }

    fn before(&self, _session: &mut Session, call: &Call) -> Verdict {
        self.seen.lock().unwrap().push((call.id.to_string(), call.origin));
        Verdict::Pass
    }
}

#[test]
fn a_nested_cmd_execute_reaches_the_gates_as_origin_plugin() {
    let (mut session, host) = rig_frozen();
    install(&host, &edits_package());
    assert_active(&host, EDITS);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let owner = Owner { plugin_id: Arc::from("test.recorder"), generation: 1 };
    session.hooks.register(&owner, Arc::new(Record { interest: Interest { globs: vec!["*".into()], priority: 0 }, seen: Arc::clone(&seen) }));

    session.execute(&format!("plugin.{EDITS}.ok"), json!({})).expect("the plug-in command runs");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![(format!("plugin.{EDITS}.ok"), Origin::Execute), ("file.newBin".to_string(), Origin::Plugin)],
        "the user's call is Execute and the plug-in's nested call is Plugin"
    );
}

#[test]
fn a_native_dynamic_command_gets_the_real_session_with_the_plugin_host_running() {
    let (mut session, _host) = rig_frozen();
    let probe = DynCommand::<Session, EngineError> {
        id: Arc::from("plugin.test.native.probe"),
        label: Arc::from("Probe"),
        params: Arc::from("{}"),
        shortcut: None,
        menu: Arc::from(Vec::new()),
        owner: Owner { plugin_id: Arc::from("test.native"), generation: 1 },
        enablement: Enablement::Always,
        visibility: Visibility { menu: false, palette: true, shortcut: false },
        journal: false,
        run: Arc::new(|session: &mut Session, _params: &Value| Ok(json!({ "project": session.project.name.clone(), "revision": session.revision }))),
    };
    session.registry.register(probe).expect("registered");
    let answer = session.execute("plugin.test.native.probe", json!({})).expect("probe");
    assert_eq!(answer["project"], json!(session.project.name), "the body saw the open project, not an idle spare");
    assert_eq!(answer["revision"], json!(session.revision));
}
