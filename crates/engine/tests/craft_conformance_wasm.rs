//! Conformance T1 to T6 with the veto-all gate in wasm.
//!
//! `craft_conformance.rs` vetoes from a Rust `Interceptor`. Here the veto decision comes from a
//! `before` hook of a real wasm plug-in, installed through `filmcraft-plugins` (the real
//! `PluginHost`, the real wasmi engine, the real permission flow). A Rust recorder still sits in
//! front of it, only to count which ids reach the gate chain and which outcome `after` sees; it
//! never vetoes. If the wasm gate misses a command, the veto does not happen and T1 fails.
//!
//! Both time sources are injected. The command layer and the plug-in host measure gate time against
//! the wall clock, and a conformant host fails open when a gate overruns (see the
//! craft-plugin-abi and craft-command docs). A busy machine preempts the thread for more than
//! 2 ms, so a wall clock turns this test into a load meter. `TestClock` is frozen for the
//! conformance run and steps 3 ms per read for the slow-gate test.
//!
//! This file lives in `filmcraft-engine` because the command seam is the engine's. It needs
//! `filmcraft-plugins` as a dev-dependency, which depends back on the engine; cargo allows that
//! for tests.

#![allow(clippy::disallowed_methods, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use craft_command::conformance::{ChangeCause, CommandHost, OutcomeClass};
use craft_command::{Call, CallInfo, DynCommand, Enablement, Interceptor, Interest, Outcome, Owner, Verdict, Visibility};
use craft_plugin_host::lifecycle::InstallOutcome;
use craft_plugin_host::store::Source;
use filmcraft_engine::project::Project;
use filmcraft_engine::{EngineError, Session, UiOutcome};
use filmcraft_plugins::{HostConfig, PluginHost};
use serde_json::{Value, json};

const VETO: &str = "conformance.veto-all";

/// One time source for both layers. Every read adds `step_us` microseconds, so 0 is frozen.
struct TestClock {
    now_us: AtomicU64,
    step_us: u64,
}

impl TestClock {
    fn new(step_us: u64) -> Arc<TestClock> {
        Arc::new(TestClock { now_us: AtomicU64::new(1_000_000), step_us })
    }

    fn read_us(&self) -> u64 {
        self.now_us.fetch_add(self.step_us, Ordering::Relaxed)
    }
}

impl craft_command::Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.read_us() / 1000
    }
}

impl craft_plugin_host::engine::Clock for TestClock {
    fn now(&self) -> Duration {
        Duration::from_micros(self.read_us())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn leb(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let b = (n & 0x7F) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

/// The veto-all plug-in: one `before` hook matching `*`, answering `{"veto": ...}` from a session
/// instance (no instantiation per call).
fn veto_all_package() -> Vec<u8> {
    let reason = r#"{"veto":"conformance"}"#;
    let escaped: String = reason.bytes().map(|b| format!("\\{b:02x}")).collect();
    let wat = format!(
        r#"(module
  (memory (export "memory") 1 4)
  (global $heap (mut i32) (i32.const 4096))
  (data (i32.const 64) "{escaped}")
  (func $alloc (param $len i32) (param $align i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
    (if (i32.gt_u (global.get $heap) (i32.const 60000)) (then (global.set $heap (local.get $p)) (return (i32.const 0))))
    (local.get $p))
  (func (export "craft_abi") (result i32) i32.const 1)
  (export "craft_alloc" (func $alloc))
  (func (export "craft_free") (param $p i32) (param $len i32) (param $align i32)
    (if (i32.eq (i32.add (local.get $p) (local.get $len)) (global.get $heap)) (then (global.set $heap (local.get $p)))))
  (func (export "craft_invoke") (param $e i32) (param $rp i32) (param $rl i32) (param $bp i32) (param $bl i32) (result i64)
    (local $out i32)
    ;; The guest keeps no state, so every call starts from an empty heap. The host allocated the
    ;; request above the base and frees it before the answer, which a bump heap cannot reclaim.
    (global.set $heap (i32.const 4096))
    (local.set $out (call $alloc (i32.const {len}) (i32.const 1)))
    (memory.copy (local.get $out) (i32.const 64) (i32.const {len}))
    (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.extend_i32_u (local.get $out))))
)"#,
        len = reason.len()
    );
    let manifest = json!({
        "schema": 1, "id": VETO, "name": "Veto all", "version": "1.0.0",
        "abi": { "craft": 1 },
        "requires": { "ops": [] },
        "permissions": { "required": ["commands.intercept"], "optional": [] },
        "limits": { "memory_mb": 4, "stack_kb": 128 },
        "contributions": [{ "kind": "hook", "id": "veto", "entry": 0, "phase": "before", "match": ["*"], "instance": "session" }]
    });
    let mut wasm = wat::parse_str(&wat).unwrap();
    let name = "craft.manifest";
    let mut body = Vec::new();
    leb(name.len(), &mut body);
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(&serde_json::to_vec(&manifest).unwrap());
    wasm.push(0);
    leb(body.len(), &mut wasm);
    wasm.extend_from_slice(&body);
    wasm
}

/// Records, never vetoes. Sits before the plug-in gate (lower priority number).
struct Recorder {
    interest: Interest,
    before: Mutex<Vec<String>>,
    after: Mutex<Vec<OutcomeClass>>,
    seen_vetoable: AtomicBool,
}

impl Interceptor<Session> for Recorder {
    fn interest(&self) -> &Interest {
        &self.interest
    }

    fn before(&self, _session: &mut Session, call: &Call) -> Verdict {
        lock(&self.before).push(call.id.to_string());
        self.seen_vetoable.store(true, Ordering::Relaxed);
        Verdict::Pass
    }

    fn after(&self, _call: &CallInfo, outcome: &Outcome) {
        let class = match outcome {
            Outcome::Ok => OutcomeClass::Success,
            Outcome::Err | Outcome::Panicked => OutcomeClass::Failure,
            Outcome::Vetoed => OutcomeClass::Vetoed,
            Outcome::Disabled => OutcomeClass::Disabled,
        };
        lock(&self.after).push(class);
    }
}

struct PassGate {
    interest: Interest,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Interceptor<Session> for PassGate {
    fn interest(&self) -> &Interest {
        &self.interest
    }

    fn before(&self, _session: &mut Session, call: &Call) -> Verdict {
        lock(&self.seen).push(call.id.to_string());
        Verdict::Pass
    }
}

struct Host {
    session: Session,
    plugins: Arc<PluginHost>,
    recorder: Arc<Recorder>,
    /// Ids the probe plug-in's gate saw.
    gate_seen: Arc<Mutex<Vec<String>>>,
    sub: u64,
}

impl Host {
    fn map_result(result: Result<Value, EngineError>) -> OutcomeClass {
        match result {
            Ok(_) => OutcomeClass::Success,
            Err(EngineError::Disabled(_, _)) => OutcomeClass::Disabled,
            Err(EngineError::Command(craft_command::CommandError::Vetoed { .. })) => OutcomeClass::Vetoed,
            Err(_) => OutcomeClass::Failure,
        }
    }

    fn keep_outer_after(&self, start: usize) {
        let mut after = lock(&self.recorder.after);
        if after.len() > start + 1 {
            let outer = after.pop().unwrap_or(OutcomeClass::Failure);
            after.truncate(start);
            after.push(outer);
        }
    }
}

impl Host {
    fn with_clock(clock: Arc<TestClock>) -> Self {
        let mut session = Session::default();
        session.clock = clock.clone();
        let plugins = PluginHost::start(&mut session, HostConfig { clock: Some(clock), ..HostConfig::default() }).unwrap();
        let recorder = Arc::new(Recorder {
            interest: Interest { globs: vec!["*".into()], priority: -1000 },
            before: Mutex::new(Vec::new()),
            after: Mutex::new(Vec::new()),
            seen_vetoable: AtomicBool::new(false),
        });
        let owner = Owner { plugin_id: Arc::from("conformance"), generation: 1 };
        session.hooks.register(&owner, recorder.clone());
        let sub = session.hooks.change_bus::<Arc<Project>>().subscribe();
        // The plug-in asks for `commands.intercept`; accept it as the prompt's Install button would.
        let outcome = plugins.install_bytes(&veto_all_package(), Source::File).unwrap();
        let InstallOutcome::NeedsGrant(request) = outcome else { panic!("the veto plug-in must ask for permission first: {outcome:?}") };
        plugins.answer(&request.plugin_id, true, request.optional).unwrap();
        // Off until a check asks for it.
        plugins.manager.disable(VETO).unwrap();
        Host { session, plugins, recorder, gate_seen: Arc::default(), sub }
    }
}

impl CommandHost for Host {
    fn fresh() -> Self {
        Host::with_clock(TestClock::new(0))
    }

    fn command_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        self.session.registry.for_each(|view| ids.push(view.id.to_string()));
        ids
    }

    fn ui_command_ids(&self) -> Vec<String> {
        filmcraft_engine::ui_ids::ui_command_ids().iter().filter(|id| self.session.registry.resolve(id).is_none()).map(|id| (*id).to_string()).collect()
    }

    fn execute(&mut self, id: &str, params: &Value) -> OutcomeClass {
        let start = lock(&self.recorder.after).len();
        let class = Self::map_result(self.session.execute(id, params.clone()));
        self.keep_outer_after(start);
        class
    }

    fn invoke_ui(&mut self, id: &str, params: &Value) -> OutcomeClass {
        let start = lock(&self.recorder.after).len();
        let class = match self.session.open_ui(id, params) {
            Err(_) => OutcomeClass::Vetoed,
            Ok(gate) => {
                self.session.close_ui(gate, UiOutcome::Ok);
                OutcomeClass::Success
            }
        };
        self.keep_outer_after(start);
        class
    }

    fn fingerprint(&self) -> u64 {
        fnv(&serde_json::to_vec(&**self.session.project).unwrap_or_default())
    }

    fn revision(&self) -> u64 {
        self.session.revision
    }

    fn set_veto_all(&mut self, enabled: bool) {
        if enabled {
            self.plugins.manager.enable(VETO).unwrap();
        } else {
            self.plugins.manager.disable(VETO).unwrap();
        }
    }

    fn take_plugin_gate_ids(&mut self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.gate_seen))
    }

    fn take_before_ids(&mut self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.recorder.before))
    }

    fn take_after(&mut self) -> Vec<OutcomeClass> {
        std::mem::take(&mut *lock(&self.recorder.after))
    }

    fn schema(&self, id: &str) -> Option<Value> {
        self.session.registry.schema(id).map(|value| (*value).clone())
    }

    fn script_len(&self) -> usize {
        1
    }

    fn run_script_step(&mut self, index: usize) -> u64 {
        let _ = self.session.hooks.change_bus::<Arc<Project>>().drain(self.sub);
        let before = self.session.revision;
        let result = self.session.execute("file.newProject", json!({"name": format!("Step {index}")}));
        let count = self.session.hooks.change_bus::<Arc<Project>>().drain(self.sub).len() as u64;
        assert!(result.is_ok(), "file.newProject failed: {result:?}");
        assert!(self.session.revision > before, "file.newProject did not bump");
        count
    }

    fn write_without_bump(&mut self) {
        let _ = self.session.project.write();
    }

    fn flush(&mut self) -> Vec<ChangeCause> {
        let bus = self.session.hooks.change_bus::<Arc<Project>>();
        let id = bus.subscribe();
        self.session.hooks.flush(&mut self.session.project, self.session.revision);
        bus.drain(id)
            .into_iter()
            .map(|change| if change.cause == craft_command::Cause::Untracked { ChangeCause::Untracked } else { ChangeCause::Other })
            .collect()
    }

    fn prepare_unregister(&mut self) -> (String, String) {
        let owner = Owner { plugin_id: Arc::from("probe"), generation: 7 };
        let id = "plugin.probe.ping";
        let cmd = DynCommand {
            id: Arc::from(id),
            label: Arc::from("Ping"),
            params: Arc::from("{}"),
            shortcut: None,
            menu: Arc::from([Arc::from("Plugin")]),
            owner: owner.clone(),
            enablement: Enablement::Always,
            visibility: Visibility { menu: true, palette: true, shortcut: true },
            journal: false,
            run: Arc::new(|_session, _params| Ok(Value::Null)),
        };
        self.session.registry.register(cmd).unwrap_or_else(|err| panic!("register probe: {err}"));
        let gate = Arc::new(PassGate { interest: Interest { globs: vec!["*".into()], priority: 100 }, seen: Arc::clone(&self.gate_seen) });
        self.session.hooks.register(&owner, gate);
        let bound = self.session.execute("shortcuts.set", json!({"command": id, "keys": "Cmd+Alt+Shift+F12", "keepConflicts": true}));
        assert!(bound.is_ok(), "shortcuts.set rejected the plugin command: {bound:?}");
        (owner.plugin_id.to_string(), id.to_string())
    }

    fn unregister_owner(&mut self, owner: &str) {
        let owner = Owner { plugin_id: Arc::from(owner), generation: 7 };
        self.session.registry.unregister_owner(&owner);
        self.session.hooks.unregister_owner(&owner);
    }

    fn resolves(&self, id: &str) -> bool {
        self.session.registry.resolve(id).is_some()
    }

    fn menu_lists(&self, id: &str) -> bool {
        self.session.menu_command_rows().iter().any(|(command_id, _, _)| command_id == id)
    }

    fn user_shortcut_kept(&self, id: &str) -> bool {
        self.session.shortcuts.primary(id).as_deref() == Some("Cmd+Alt+Shift+F12")
    }
}

/// Every command reaches the gate, and the gate is the wasm one.
#[test]
fn craft_conformance_t1_through_t6_with_a_wasm_veto_gate() {
    craft_command::conformance::run_all::<Host>().unwrap_or_else(|err| panic!("{err}"));
}

/// The wasm gate is what vetoes. With the plug-in off, the Rust recorder sees every call and
/// nothing is vetoed, so a pass of T1 cannot come from the recorder.
#[test]
fn without_the_plugin_enabled_nothing_is_vetoed() {
    let mut host = Host::fresh();
    host.set_veto_all(false);
    let class = host.execute("file.newBin", &json!({"name": "x"}));
    assert_eq!(class, OutcomeClass::Success);
    assert!(host.recorder.seen_vetoable.load(Ordering::Relaxed), "the recorder saw the call");
    host.set_veto_all(true);
    let class = host.execute("file.newBin", &json!({"name": "y"}));
    assert_eq!(class, OutcomeClass::Vetoed, "the wasm gate vetoes once the plug-in is on");
}

/// Spec: a gate that overruns 2 ms is dropped and the command runs (fail open). Same gate, same
/// command, only the clock differs from the test above.
#[test]
fn a_wasm_gate_over_its_deadline_fails_open() {
    let mut host = Host::with_clock(TestClock::new(3000));
    host.set_veto_all(true);
    let class = host.execute("file.newBin", &json!({"name": "slow"}));
    assert_eq!(class, OutcomeClass::Success, "a gate that took 3 ms must not veto");
}
