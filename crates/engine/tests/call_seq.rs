//! `Call::seq` names one gated call. A hook that pairs a `before` record with an `after` record by
//! `seq` must never see two calls of one session share a value, whichever entry point made them.

#![allow(clippy::disallowed_methods, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use craft_command::{Call, DynCommand, Enablement, Interceptor, Interest, Owner, Verdict, Visibility};
use filmcraft_engine::{Session, UiOutcome};
use serde_json::{Value, json};

/// Records the `seq` of every call it is asked about.
struct SeqLog {
    interest: Interest,
    seen: Mutex<Vec<(String, u64)>>,
}

impl Interceptor<Session> for SeqLog {
    fn interest(&self) -> &Interest {
        &self.interest
    }

    fn before(&self, _session: &mut Session, call: &Call) -> Verdict {
        self.seen.lock().unwrap_or_else(|err| err.into_inner()).push((call.id.to_string(), call.seq));
        Verdict::Pass
    }
}

fn session_with_log() -> (Session, Arc<SeqLog>) {
    let session = Session::default();
    let owner = Owner { plugin_id: Arc::from("seq"), generation: 1 };
    let cmd = DynCommand {
        id: Arc::from("plugin.seq.noop"),
        label: Arc::from("Noop"),
        params: Arc::from("{}"),
        shortcut: None,
        menu: Arc::from([Arc::from("Plugin")]),
        owner: owner.clone(),
        enablement: Enablement::Always,
        visibility: Visibility { menu: false, palette: false, shortcut: false },
        journal: false,
        run: Arc::new(|_session, _params| Ok(Value::Null)),
    };
    session.registry.register(cmd).unwrap();
    let log = Arc::new(SeqLog { interest: Interest { globs: vec!["*".into()], priority: 0 }, seen: Mutex::new(Vec::new()) });
    session.hooks.register(&owner, log.clone());
    (session, log)
}

#[test]
fn execute_and_open_ui_never_share_a_seq() {
    let (mut session, log) = session_with_log();
    session.execute("plugin.seq.noop", json!({})).unwrap();
    let gate = session.open_ui("ui.noop", &json!({})).unwrap();
    session.close_ui(gate, UiOutcome::Ok);
    session.execute("plugin.seq.noop", json!({})).unwrap();
    let gate = session.open_ui("ui.noop", &json!({})).unwrap();
    session.close_ui(gate, UiOutcome::Ok);
    session.execute("plugin.seq.noop", json!({})).unwrap();

    let seen = log.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 5, "every call reached the hook: {seen:?}");
    let mut seqs: Vec<u64> = seen.iter().map(|(_, seq)| *seq).collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), 5, "a seq repeated: {seen:?}");
    let in_order: Vec<u64> = seen.iter().map(|(_, seq)| *seq).collect();
    assert!(in_order.windows(2).all(|pair| pair[0] < pair[1]), "seq must go up with each call: {in_order:?}");
}
