//! Shared test rig: real `Session`, real `PluginHost`, real wasm guests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use craft_plugin_host::lifecycle::{InstallOutcome, PluginState};
use craft_plugin_host::store::Source;
use filmcraft_engine::Session;
use filmcraft_plugins::{HostConfig, PluginHost};
use serde_json::{Value, json};

pub mod driver;
pub mod guests;

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

/// A wasm module from WAT with a `craft.manifest` custom section appended.
pub fn package(wat_text: &str, manifest: &Value) -> Vec<u8> {
    let mut wasm = wat::parse_str(wat_text).unwrap_or_else(|e| panic!("bad WAT: {e}"));
    let data = serde_json::to_vec(manifest).unwrap();
    let name = "craft.manifest";
    let mut body = Vec::new();
    leb(name.len(), &mut body);
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(&data);
    wasm.push(0);
    leb(body.len(), &mut wasm);
    wasm.extend_from_slice(&body);
    wasm
}

/// The text of `tests/fixtures/<name>.wat`.
pub fn fixture_text(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}.wat", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

pub fn manifest(id: &str, ops: &[&str], required: &[&str], contributions: Value) -> Value {
    json!({
        "schema": 1, "id": id, "name": id, "version": "1.0.0",
        "abi": { "craft": 1 },
        "requires": { "ops": ops },
        "permissions": { "required": required, "optional": [] },
        "limits": { "memory_mb": 4, "stack_kb": 128 },
        "contributions": contributions
    })
}

/// A session with the demo project and a plug-in host in memory.
pub fn rig() -> (Session, Arc<PluginHost>) {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let host = PluginHost::start(&mut session, HostConfig::default()).unwrap();
    (session, host)
}

/// Install `bytes` and accept every permission it asks for, the way the prompt's Install button does.
pub fn install(host: &PluginHost, bytes: &[u8]) -> InstallOutcome {
    match host.install_bytes(bytes, Source::File).unwrap() {
        InstallOutcome::NeedsGrant(req) => {
            let id = req.plugin_id.clone();
            host.answer(&id, true, req.optional).unwrap()
        }
        other => other,
    }
}

pub fn assert_active(host: &PluginHost, id: &str) {
    assert!(matches!(host.manager.state(id), Some(PluginState::Active { .. })), "{id} is {:?}", host.manager.state(id));
}

/// The project as FilmCraft would save it: the bytes the byte-for-byte checks compare.
pub fn project_bytes(session: &Session) -> Vec<u8> {
    filmcraft_format::encode(&session.project, false)
}

/// A clock that never moves, for the command layer's 2 ms gate rule and the plug-in host's deadlines.
/// A test on a busy machine then measures the code and not the load .
pub struct Frozen;

impl craft_command::Clock for Frozen {
    fn now_ms(&self) -> u64 {
        1_000
    }
}

impl craft_plugin_host::engine::Clock for Frozen {
    fn now(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
}

/// [`rig`] with both clocks frozen.
pub fn rig_frozen() -> (Session, Arc<PluginHost>) {
    let clock = Arc::new(Frozen);
    let mut session = Session::default();
    session.clock = clock.clone();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let host = PluginHost::start(&mut session, HostConfig { clock: Some(clock), ..HostConfig::default() }).unwrap();
    (session, host)
}
