//! WAT guests. Each is a real craft-abi 1 module with a bump allocator and `craft_invoke`.

use serde_json::{Value, json};

use super::{manifest, package};

pub const EDITS: &str = "test.edits";
pub const VETO: &str = "test.veto";

/// The WAT text escaped for a data segment.
fn data(s: &str) -> String {
    s.bytes().map(|b| format!("\\{b:02x}")).collect()
}

/// A guest whose entries run `file.newBin` (and more). The commands are contributions 0 to 4.
///
/// | Entry | Does | Then |
/// |---|---|---|
/// | 0 `ok` | one edit | answers `{"result":{"ok":true}}` |
/// | 1 `trap` | one edit | `unreachable` |
/// | 2 `error` | one edit | answers `{"error":"nope"}` |
/// | 3 `two` | two edits | answers ok |
/// | 4 `query` | `doc.query {"what":"project"}` | answers `{"result": <the query answer>}` |
/// | 5 `run_undo` | `cmd.execute edit.undo` | answers ok |
/// | 6 `list` | `cmd.list` | answers `{"result": <the list>}` |
/// | 7 `host_cmd` | `cmd.execute plugin.app.filmcraft.disable {"id":"test.edits"}` | answers `{"result": <the execute answer>}` |
/// | 8 `recurse` | `cmd.execute plugin.test.edits.recurse` (itself) | answers ok |
pub fn edits_wat() -> String {
    let edit_a = data(r#"{"id":"file.newBin","params":{"name":"Plug-in A"}}"#);
    let edit_b = data(r#"{"id":"file.newBin","params":{"name":"Plug-in B"}}"#);
    let ok = data(r#"{"result":{"ok":true}}"#);
    let err = data(r#"{"error":"nope"}"#);
    let query = data(r#"{"what":"project"}"#);
    let undo = data(r#"{"id":"edit.undo"}"#);
    let host_cmd = data(r#"{"id":"plugin.app.filmcraft.disable","params":{"id":"test.edits"}}"#);
    let prefix = data(r#"{"result":"#);
    let recurse = data(r#"{"id":"plugin.test.edits.recurse"}"#);
    let denied = data(r#"{"result":{"error":"E_DENIED: the host refused the call"}}"#);
    format!(
        r#"(module
  (import "craft_host" "call" (func $host (param i32 i32 i32 i32 i32) (result i64)))
  (memory (export "memory") 4 16)
  (global $heap (mut i32) (i32.const 110000))
  (data (i32.const 64) "{edit_a}")
  (data (i32.const 192) "{edit_b}")
  (data (i32.const 320) "{ok}")
  (data (i32.const 384) "{err}")
  (data (i32.const 448) "{query}")
  (data (i32.const 512) "{undo}")
  (data (i32.const 576) "{host_cmd}")
  (data (i32.const 704) "{prefix}")
  (data (i32.const 768) "{recurse}")
  (data (i32.const 832) "{denied}")
  (func $alloc (param $len i32) (param $align i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
    (if (i32.gt_u (global.get $heap) (i32.const 250000)) (then (global.set $heap (local.get $p)) (return (i32.const 0))))
    (local.get $p))
  (func $lit (param $src i32) (param $len i32) (result i64)
    (local $out i32)
    (local.set $out (call $alloc (local.get $len) (i32.const 1)))
    (memory.copy (local.get $out) (local.get $src) (local.get $len))
    (i64.or (i64.shl (i64.extend_i32_u (local.get $len)) (i64.const 32)) (i64.extend_i32_u (local.get $out))))
  (func (export "craft_abi") (result i32) i32.const 1)
  (export "craft_alloc" (func $alloc))
  (func (export "craft_free") (param $p i32) (param $len i32) (param $align i32)
    (if (i32.eq (i32.add (local.get $p) (local.get $len)) (global.get $heap)) (then (global.set $heap (local.get $p)))))
  ;; one host call with the request at $p, answer into 2048..
  (func $exec (param $p i32) (param $len i32)
    (drop (call $host (i32.const 0x11) (local.get $p) (local.get $len) (i32.const 2048) (i32.const 1024))))
  ;; {{"result": <host answer at 1034, n bytes>}} built at 1024
  (func $wrap (param $op i32) (param $in i32) (param $inlen i32) (result i64)
    (local $r i64) (local $n i32)
    (memory.copy (i32.const 1024) (i32.const 704) (i32.const 10))
    (local.set $r (call $host (local.get $op) (local.get $in) (local.get $inlen) (i32.const 1034) (i32.const 100000)))
    ;; a refused call (status -1, E_DENIED) answers an error, not an empty result
    (if (i32.eq (i32.wrap_i64 (local.get $r)) (i32.const -1)) (then (return (call $lit (i32.const 832) (i32.const {ldenied})))))
    (local.set $n (i32.wrap_i64 (i64.shr_u (local.get $r) (i64.const 32))))
    (i32.store8 (i32.add (i32.const 1034) (local.get $n)) (i32.const 125))
    (i64.or (i64.shl (i64.extend_i32_u (i32.add (local.get $n) (i32.const 11))) (i64.const 32)) (i64.const 1024)))
  (func (export "craft_invoke") (param $e i32) (param $rp i32) (param $rl i32) (param $bp i32) (param $bl i32) (result i64)
    (if (i32.eq (local.get $e) (i32.const 0)) (then (call $exec (i32.const 64) (i32.const {la})) (return (call $lit (i32.const 320) (i32.const {lok})))))
    (if (i32.eq (local.get $e) (i32.const 1)) (then (call $exec (i32.const 64) (i32.const {la})) unreachable))
    (if (i32.eq (local.get $e) (i32.const 2)) (then (call $exec (i32.const 64) (i32.const {la})) (return (call $lit (i32.const 384) (i32.const {lerr})))))
    (if (i32.eq (local.get $e) (i32.const 3)) (then (call $exec (i32.const 64) (i32.const {la})) (call $exec (i32.const 192) (i32.const {lb})) (return (call $lit (i32.const 320) (i32.const {lok})))))
    (if (i32.eq (local.get $e) (i32.const 4)) (then (return (call $wrap (i32.const 0x20) (i32.const 448) (i32.const {lq})))))
    (if (i32.eq (local.get $e) (i32.const 5)) (then (call $exec (i32.const 512) (i32.const {lu})) (return (call $lit (i32.const 320) (i32.const {lok})))))
    (if (i32.eq (local.get $e) (i32.const 6)) (then (return (call $wrap (i32.const 0x10) (i32.const 448) (i32.const 0)))))
    (if (i32.eq (local.get $e) (i32.const 7)) (then (return (call $wrap (i32.const 0x11) (i32.const 576) (i32.const {lh})))))
    (if (i32.eq (local.get $e) (i32.const 8)) (then (call $exec (i32.const 768) (i32.const {lr})) (return (call $lit (i32.const 320) (i32.const {lok})))))
    (i64.const -1))
)"#,
        la = r#"{"id":"file.newBin","params":{"name":"Plug-in A"}}"#.len(),
        lb = r#"{"id":"file.newBin","params":{"name":"Plug-in B"}}"#.len(),
        lok = r#"{"result":{"ok":true}}"#.len(),
        lerr = r#"{"error":"nope"}"#.len(),
        lq = r#"{"what":"project"}"#.len(),
        lu = r#"{"id":"edit.undo"}"#.len(),
        lh = r#"{"id":"plugin.app.filmcraft.disable","params":{"id":"test.edits"}}"#.len(),
        lr = r#"{"id":"plugin.test.edits.recurse"}"#.len(),
        ldenied = r#"{"result":{"error":"E_DENIED: the host refused the call"}}"#.len(),
    )
}

pub fn edits_package() -> Vec<u8> {
    edits_package_version("1.0.0", "")
}

/// The `edits` package at `version`, with `label_suffix` added to each command label. Installing a
/// higher version over an installed one is a hot reload.
pub fn edits_package_version(version: &str, label_suffix: &str) -> Vec<u8> {
    let c = |id: &str, entry: u32| json!({ "kind": "command", "id": id, "entry": entry, "label": format!("{id}{label_suffix}"), "enabled": "always" });
    let contributions =
        json!([c("ok", 0), c("trap", 1), c("error", 2), c("two", 3), c("query", 4), c("run-undo", 5), c("list", 6), c("host-cmd", 7), c("recurse", 8)]);
    let mut m = manifest(EDITS, &["cmd.execute", "cmd.list", "doc.query"], &["commands.run", "doc.read"], contributions);
    m["version"] = json!(version);
    package(&edits_wat(), &m)
}

/// A `before` hook that vetoes every id in `globs` with a reason.
pub fn veto_package(globs: &[&str]) -> Vec<u8> {
    let reason = r#"{"veto":"vetoed by the test plug-in"}"#;
    let wat = format!(
        r#"(module
  (memory (export "memory") 1 4)
  (global $heap (mut i32) (i32.const 4096))
  (data (i32.const 64) "{}")
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
    (global.set $heap (i32.const 4096))
    (local.set $out (call $alloc (i32.const {len}) (i32.const 1)))
    (memory.copy (local.get $out) (i32.const 64) (i32.const {len}))
    (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.extend_i32_u (local.get $out))))
)"#,
        data(reason),
        len = reason.len()
    );
    let contributions: Value = json!([{ "kind": "hook", "id": "veto", "entry": 0, "phase": "before", "match": globs, "instance": "session" }]);
    package(&wat, &manifest(VETO, &[], &["commands.intercept"], contributions))
}

/// The `edits` guest as a `before` hook on `globs`. Its entry 0 runs `cmd.execute file.newBin`
/// ("Plug-in A") and then passes, so the hook only edits when its host call reaches the session.
pub fn gate_edits_package(globs: &[&str]) -> Vec<u8> {
    let contributions: Value = json!([{ "kind": "hook", "id": "gate", "entry": 0, "phase": "before", "match": globs, "instance": "session" }]);
    package(&edits_wat(), &manifest(GATE_EDITS, &["cmd.execute"], &["commands.intercept", "commands.run"], contributions))
}

pub const GATE_EDITS: &str = "test.gate-edits";
