//! `Session::execute` skips cause, gates and depth bookkeeping when `Hooks::is_idle`,
//! and takes the full path as soon as a gate, observer subscriber or change subscriber exists.
//!
//! The probe command reads `Hooks::depth` and `Hooks::current_cause` from inside its body. The idle
//! path never calls `enter` or `with_cause`, so the probe sees depth 0 and `Untracked`. The full
//! path shows depth 1 and `Cause::Command`.

#![allow(clippy::disallowed_methods)]

use std::sync::{Arc, Mutex};

use craft_command::{Call, Cause, DynCommand, Enablement, Interceptor, Interest, Owner, Verdict, Visibility};
use filmcraft_engine::Session;
use filmcraft_engine::project::Project;
use serde_json::{Value, json};

const PROBE: &str = "plugin.idle.probe";

type Seen = Arc<Mutex<Vec<(u32, Cause)>>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

/// A session with a journaled probe command. The probe pushes `(depth, cause)` into the returned log.
fn probe_session() -> (Session, Seen) {
    let session = Session::default();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let cmd = DynCommand {
        id: Arc::from(PROBE),
        label: Arc::from("Probe"),
        params: Arc::from("{}"),
        shortcut: None,
        menu: Arc::from([Arc::from("Plugin")]),
        owner: Owner { plugin_id: Arc::from("idle"), generation: 1 },
        enablement: Enablement::Always,
        visibility: Visibility { menu: true, palette: true, shortcut: true },
        journal: true,
        run: Arc::new(move |session: &mut Session, _params: &Value| {
            lock(&log).push((session.hooks.depth(), session.hooks.current_cause()));
            let _ = session.project_mut();
            Ok(Value::Null)
        }),
    };
    session.registry.register(cmd).unwrap_or_else(|err| panic!("register probe: {err}"));
    (session, seen)
}

struct SeqGate {
    interest: Interest,
    seen: Mutex<Vec<(String, u64)>>,
}

impl SeqGate {
    fn new() -> Arc<Self> {
        Arc::new(Self { interest: Interest { globs: vec![PROBE.into()], priority: 0 }, seen: Mutex::new(Vec::new()) })
    }
}

impl Interceptor<Session> for SeqGate {
    fn interest(&self) -> &Interest {
        &self.interest
    }

    fn before(&self, _session: &mut Session, call: &Call) -> Verdict {
        lock(&self.seen).push((call.id.to_string(), call.seq));
        Verdict::Pass
    }
}

fn owner() -> Owner {
    Owner { plugin_id: Arc::from("gate"), generation: 1 }
}

#[test]
fn idle_call_keeps_no_hook_state() {
    let (mut s, seen) = probe_session();
    assert!(s.hooks.is_idle());
    s.execute(PROBE, json!({})).unwrap();
    let seen = lock(&seen);
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, 0, "idle call must not enter the depth counter");
    assert!(matches!(seen[0].1, Cause::Untracked), "idle call must not push a cause: {:?}", seen[0].1);
}

#[test]
fn idle_call_still_journals_and_advances_seq() {
    let (mut s, _seen) = probe_session();
    s.execute(PROBE, json!({"n": 1})).unwrap();
    s.execute(PROBE, json!({"n": 2})).unwrap();
    assert_eq!(s.journal.iter().filter(|(id, _)| id == PROBE).count(), 2, "idle calls are journaled");
    // A gate added now must see seq 2: the two idle calls consumed seq 0 and 1.
    let gate = SeqGate::new();
    s.hooks.register(&owner(), gate.clone());
    s.execute(PROBE, json!({})).unwrap();
    assert_eq!(*lock(&gate.seen), vec![(PROBE.to_string(), 2)]);
}

#[test]
fn gate_sees_calls_and_the_full_path_runs() {
    let (mut s, seen) = probe_session();
    let gate = SeqGate::new();
    s.hooks.register(&owner(), gate.clone());
    assert!(!s.hooks.is_idle());
    s.execute(PROBE, json!({})).unwrap();
    s.execute(PROBE, json!({})).unwrap();
    assert_eq!(*lock(&gate.seen), vec![(PROBE.to_string(), 0), (PROBE.to_string(), 1)]);
    let seen = lock(&seen);
    assert_eq!(seen[0].0, 1, "gated call enters the depth counter");
    assert!(matches!(&seen[0].1, Cause::Command { id, .. } if &**id == PROBE));
}

#[test]
fn unregistering_the_last_gate_returns_to_the_fast_path() {
    let (mut s, seen) = probe_session();
    s.hooks.register(&owner(), SeqGate::new());
    s.execute(PROBE, json!({})).unwrap();
    s.hooks.unregister_owner(&owner());
    assert!(s.hooks.is_idle());
    s.execute(PROBE, json!({})).unwrap();
    let seen = lock(&seen);
    assert_eq!(seen[0].0, 1);
    assert_eq!(seen[1].0, 0);
}

#[test]
fn change_subscriber_takes_the_full_path_and_receives_the_change() {
    let (mut s, seen) = probe_session();
    let bus = s.hooks.change_bus::<Arc<Project>>();
    let sub = bus.subscribe();
    assert!(!s.hooks.is_idle());
    s.execute(PROBE, json!({})).unwrap();
    assert_eq!(lock(&seen)[0].0, 1, "a change subscriber must disable the fast path");
    // The probe wrote the project, so the flush at depth 0 reaches the subscriber.
    assert_eq!(bus.drain(sub).len(), 1);
    bus.unsubscribe(sub);
    assert!(s.hooks.is_idle());
    s.execute(PROBE, json!({})).unwrap();
    assert_eq!(bus.drain(sub).len(), 0);
}

#[test]
fn observer_subscriber_takes_the_full_path() {
    let (mut s, seen) = probe_session();
    let sub = s.hooks.observer_queue().subscribe(None);
    assert!(!s.hooks.is_idle());
    s.execute(PROBE, json!({})).unwrap();
    s.hooks.observer_queue().unsubscribe(sub);
    assert!(s.hooks.is_idle());
    s.execute(PROBE, json!({})).unwrap();
    let seen = lock(&seen);
    assert_eq!((seen[0].0, seen[1].0), (1, 0));
}
