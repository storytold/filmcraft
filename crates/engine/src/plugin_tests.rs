//! Plug-in runs against a real session, with modules assembled from WebAssembly text (RFC-0046).

use serde_json::{Value, json};

use crate::Session;
use crate::plugin::craft_plugin::bundle::Bundle;

/// A module that calls `calls` (command, params JSON) in order. A refused or failed call fails the
/// run when `strict`; otherwise its message becomes the output and the run goes on. `tail` is WAT
/// run after the calls (a trap, an endless loop), before returning 0.
fn module(calls: &[(&str, &str)], strict: bool, tail: &str) -> Vec<u8> {
    let mut data = String::new();
    let mut body = String::new();
    let mut at = 0usize;
    for (command, params) in calls {
        let (c, p) = (at, at + command.len());
        at = p + params.len();
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        data.push_str(&format!(r#"(data (i32.const {c}) "{}") (data (i32.const {p}) "{}")"#, esc(command), esc(params)));
        let on_error = if strict {
            "(return (i32.const 1))".to_string()
        } else {
            "(call $set_output (i32.const 8192) (call $reply_read (i32.const 8192) (call $reply_len)))".to_string()
        };
        body.push_str(&format!(
            "(if (i32.ne (call $call (i32.const {c}) (i32.const {}) (i32.const {p}) (i32.const {})) (i32.const 0)) (then {on_error}))\n",
            command.len(),
            params.len()
        ));
    }
    let wat = format!(
        r#"(module
          (import "craft" "call" (func $call (param i32 i32 i32 i32) (result i32)))
          (import "craft" "reply_len" (func $reply_len (result i32)))
          (import "craft" "reply_read" (func $reply_read (param i32 i32) (result i32)))
          (import "craft" "set_output" (func $set_output (param i32 i32)))
          (memory (export "memory") 1)
          {data}
          (func (export "craft_run") (param $n i32) (result i32) {body} {tail} (i32.const 0)))"#
    );
    wat::parse_str(&wat).unwrap_or_else(|e| panic!("{e}\n{wat}"))
}

/// The `.craftplugin` of plug-in `id` for FilmCraft, asking for `access`.
fn bundle(id: &str, access: &str, wasm: Vec<u8>) -> Vec<u8> {
    let manifest = json!({
        "id": id, "name": "Test Plug-in", "version": "1.0.0", "main": "plugin.wasm",
        "apps": {"filmcraft": {"api": 6}},
        "permissions": {"access": access},
        "commands": [{"id": "go", "label": "Go"}]
    });
    Bundle { manifest: manifest.to_string(), wasm, assets: vec![] }.encode()
}

fn base64(bytes: &[u8]) -> String {
    filmcraft_project::gtemplate::base64_encode(bytes)
}

/// The demo project with plug-in `id` installed for the session.
fn with_plugin(id: &str, access: &str, wasm: Vec<u8>) -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let r = s.execute("plugin.install", json!({"data": base64(&bundle(id, access, wasm)), "name": "test.craftplugin"})).unwrap();
    assert_eq!(r["installed"], true);
    assert_eq!(r["stored"], false, "without a store, a plug-in is kept for the session");
    s
}

fn video_tracks(s: &Session) -> usize {
    s.active_sequence().unwrap().video_tracks.len()
}

fn undo_labels(s: &Session) -> Vec<String> {
    s.history.undo.iter().map(|u| u.0.clone()).collect()
}

const ADD_TRACK: (&str, &str) = ("sequence.addTracks", r#"{"video":1,"audio":0}"#);

#[test]
fn a_write_plugin_runs_real_commands_as_one_undo_step_named_after_it() {
    let mut s = with_plugin("io.example.tracks", "write", module(&[ADD_TRACK, ADD_TRACK], true, ""));
    let (count, steps) = (video_tracks(&s), undo_labels(&s).len());

    let r = s.execute("plugin.run", json!({"id": "io.example.tracks"})).unwrap();

    assert_eq!(r["calls"], 2);
    assert_eq!(video_tracks(&s), count + 2);
    let labels = undo_labels(&s);
    assert_eq!(labels.len(), steps + 1, "two commands, one undo step");
    assert_eq!(labels.last().map(String::as_str), Some("Test Plug-in"));
    assert_eq!(s.history.undo.last().and_then(|u| u.2.as_deref()), Some("plugin.run"), "the History panel shows the run's command");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(video_tracks(&s), count, "one undo takes the whole run back");
    assert_eq!(undo_labels(&s).len(), steps, "the project's own steps are still there");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(video_tracks(&s), count + 2);
}

#[test]
fn a_run_that_changes_nothing_records_no_step_and_keeps_redo() {
    let mut s = with_plugin("io.example.lister", "write", module(&[("state.inspect", "{}")], true, ""));
    s.execute("sequence.addTracks", json!({"video": 1})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let steps = undo_labels(&s).len();

    s.execute("plugin.run", json!({"id": "io.example.lister"})).unwrap();

    assert_eq!(undo_labels(&s).len(), steps);
    assert!(s.history.can_redo(), "redo survives a run that edited nothing");
}

#[test]
fn a_read_plugin_that_edits_is_put_back_and_refused() {
    let mut s = with_plugin("io.example.reader", "read", module(&[("state.inspect", "{}"), ADD_TRACK], false, ""));
    let (project, steps, dirty) = (s.project.clone(), undo_labels(&s).len(), s.is_dirty());

    let r = s.execute("plugin.run", json!({"id": "io.example.reader"})).unwrap();

    let message = r["output"].as_str().unwrap();
    assert!(message.contains("may only read it"), "{message}");
    assert_eq!(*s.project, *project, "the edit is put back");
    assert_eq!(undo_labels(&s).len(), steps, "nothing to undo");
    assert!(!s.history.can_redo());
    assert_eq!(s.is_dirty(), dirty, "the project isn't marked changed by the run");
}

#[test]
fn refused_commands_and_file_paths_are_refused() {
    for (call, why) in [
        (("file.open", r#"{"path":"/etc/hosts"}"#), "isn't available to plugins"),
        (("file.newSequence", r#"{"name":"x"}"#), "isn't available to plugins"),
        (("file.exportMedia", r#"{"path":"/tmp/x.mov"}"#), "isn't available to plugins"),
        (("edit.undo", "{}"), "isn't available to plugins"),
        (("history.clear", "{}"), "isn't available to plugins"),
        (("prefs.set", r#"{"key":"autoSave.enabled","value":false}"#), "isn't available to plugins"),
        (("export.queue.start", "{}"), "isn't available to plugins"),
        (("media.relink", "{}"), "isn't available to plugins"),
        (("media.createProxies", "{}"), "isn't available to plugins"),
        (("mediaBrowser.list", "{}"), "isn't available to plugins"),
        (("edit.copy", "{}"), "isn't available to plugins"),
        (("tts.render", "{}"), "isn't available to plugins"),
        (("sequence.renderInToOut", "{}"), "isn't available to plugins"),
        (("audio.voiceover.start", "{}"), "isn't available to plugins"),
        (("shortcuts.import", "{}"), "isn't available to plugins"),
        (("plugin.list", "{}"), "isn't available to plugins"),
        (("captions.import", r#"{"path":"/etc/hosts"}"#), "isn't available to plugins"),
        (("sequence.addTracks", r#"{"video":1,"dir":"/tmp"}"#), "can't name files"),
        (("timeline.place", r#"{"paths":["/etc/hosts"]}"#), "can't name files"),
    ] {
        let mut s = with_plugin("io.example.sneaky", "write", module(&[call], false, ""));
        let (project, steps) = (s.project.clone(), undo_labels(&s).len());
        let r = s.execute("plugin.run", json!({"id": "io.example.sneaky"})).unwrap();
        let message = r["output"].as_str().unwrap_or_default();
        assert!(message.contains(why), "{}: {message}", call.0);
        assert_eq!(*s.project, *project, "{}", call.0);
        assert_eq!(undo_labels(&s).len(), steps, "{}", call.0);
    }
}

#[test]
fn listing_commands_stay_open_and_mask_paths_are_not_files() {
    let m = manifest("write");
    for allowed in ["presets.list", "media.status", "tts.voices", "masks.add", "sequence.addTracks"] {
        assert!(crate::plugin::refusal(&m, allowed, &json!({})).is_none(), "{allowed}");
    }
    assert!(crate::plugin::refusal(&m, "masks.add", &json!({"path": [[0, 0], [10, 0], [10, 10]]})).is_none(), "a mask's path is points");
    assert!(crate::plugin::refusal(&m, "masks.add", &json!({"path": "/etc/hosts"})).is_some());
    assert!(crate::plugin::refusal(&m, "metadata.set", &json!({"fields": {"nested": {"folder": "/tmp"}}})).is_some(), "at any depth");
}

fn manifest(access: &str) -> crate::plugin::craft_plugin::Manifest {
    let v =
        json!({"id": "io.example.m", "name": "M", "version": "1.0.0", "main": "p.wasm", "apps": {"filmcraft": {"api": 6}}, "permissions": {"access": access}});
    crate::plugin::craft_plugin::Manifest::from_value(&v).unwrap()
}

#[test]
fn a_command_outside_the_manifest_allowlist_is_refused() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let manifest = json!({"id": "io.example.narrow", "name": "Narrow", "version": "1.0.0", "main": "p.wasm",
        "apps": {"filmcraft": {"api": 6}}, "permissions": {"access": "write", "commands": ["markers.*"]}});
    let bytes = Bundle { manifest: manifest.to_string(), wasm: module(&[ADD_TRACK], false, ""), assets: vec![] }.encode();
    s.execute("plugin.install", json!({"data": base64(&bytes)})).unwrap();
    let count = video_tracks(&s);

    let r = s.execute("plugin.run", json!({"id": "io.example.narrow"})).unwrap();

    assert!(r["output"].as_str().unwrap().contains("asked permission for"), "{r}");
    assert_eq!(video_tracks(&s), count);
}

#[test]
fn a_trapping_or_endless_plugin_leaves_the_project_unchanged() {
    for (tail, fuel) in [("unreachable", None), ("(loop $l (br $l))", Some(200_000))] {
        let mut s = with_plugin("io.example.crash", "write", module(&[ADD_TRACK, ("edit.selectAll", "{}"), ADD_TRACK], true, tail));
        s.execute("sequence.addTracks", json!({"video": 1})).unwrap();
        let (project, steps, dirty) = (s.project.clone(), undo_labels(&s), s.is_dirty());
        let selection = s.state.selection.clone();
        let mut params = json!({"id": "io.example.crash"});
        if let Some(f) = fuel {
            params["fuel"] = json!(f);
        }

        let e = s.execute("plugin.run", params).unwrap_err().to_string();

        assert!(e.contains("Test Plug-in"), "{e}");
        assert_eq!(*s.project, *project, "{tail}: the edits before the failure are rolled back");
        assert_eq!(undo_labels(&s), steps);
        assert!(!s.history.can_redo());
        assert_eq!(s.state.selection, selection, "the selection is put back");
        assert_eq!(s.is_dirty(), dirty);
    }
}

#[test]
fn a_move_that_lists_a_clip_twice_is_refused_and_changes_nothing() {
    let probe = with_plugin("io.example.probe", "write", module(&[], true, ""));
    let q = probe.active_sequence().unwrap();
    let (track, clip) = (q.video_tracks[0].id, q.video_tracks[0].items[0].clone());
    let moves =
        json!({"moves": [{"clip": clip.id.0, "track": track.0, "time": clip.start.0}, {"clip": clip.id.0, "track": track.0, "time": clip.start.0 + 1000}]})
            .to_string();
    let mut s = with_plugin("io.example.mover", "write", module(&[ADD_TRACK, ("timeline.move", &moves)], true, ""));
    let (project, steps) = (s.project.clone(), undo_labels(&s).len());

    assert!(s.execute("plugin.run", json!({"id": "io.example.mover"})).is_err());

    assert_eq!(*s.project, *project, "the track added before the refused move is rolled back too");
    assert_eq!(undo_labels(&s).len(), steps);
}

#[test]
fn a_call_that_changes_the_settings_is_put_back_and_refused() {
    let mut s = with_plugin("io.example.prefs", "write", module(&[("sequence.selectionFollowsPlayhead", r#"{"on":true}"#)], false, ""));
    let prefs = s.prefs.clone();

    let r = s.execute("plugin.run", json!({"id": "io.example.prefs"})).unwrap();

    assert!(r["output"].as_str().unwrap().contains("changes FilmCraft's settings"), "{r}");
    assert_eq!(s.prefs, prefs);
    assert!(!s.state.selection_follows_playhead);
}

#[test]
fn a_plugin_without_a_command_or_installed_does_not_run() {
    let mut s = with_plugin("io.example.tracks", "write", module(&[ADD_TRACK], true, ""));
    assert!(s.execute("plugin.run", json!({"id": "io.example.nothing"})).unwrap_err().to_string().contains("no plug-in"));
    assert!(s.execute("plugin.run", json!({"id": "io.example.tracks", "command": "other"})).is_err());
    assert!(s.execute("plugin.run", json!({})).is_err());
}

#[test]
fn settings_are_bounded_and_kept_per_plugin() {
    let mut all = Default::default();
    crate::plugin::set_setting(&mut all, "a", "k", Some("{\"x\": 1}")).unwrap();
    assert_eq!(all["a"]["k"], json!({"x": 1}));
    assert!(crate::plugin::set_setting(&mut all, "a", &"k".repeat(129), Some("1")).is_err());
    assert!(crate::plugin::set_setting(&mut all, "a", "big", Some(&format!("\"{}\"", "x".repeat(64 * 1024)))).is_err());
    assert!(crate::plugin::set_setting(&mut all, "a", "bad", Some("not json")).is_err());
    crate::plugin::set_setting(&mut all, "a", "k", None).unwrap();
    assert!(!all.contains_key("a"));
}

#[test]
fn hostile_plugin_preferences_are_dropped_not_the_rest() {
    let v = json!({"autoSave": {"enabled": false}, "pluginData": {
        "settings": {"ok": {"k": 1, "": 2}, "bad": "x", "big": {"b": "x".repeat(70 * 1024)}},
        "catalogs": ["https://example.com/index.json", 3, "https://raw.githubusercontent.com/o/r/main/index.json"]
    }});
    let p = crate::autosave::Preferences::from_value(v);
    assert!(!p.auto_save.enabled, "the rest of the preferences read");
    assert_eq!(p.plugin_data.settings.len(), 1, "{:?}", p.plugin_data.settings);
    assert_eq!(p.plugin_data.settings["ok"].len(), 1);
    assert_eq!(p.plugin_data.catalogs, ["https://raw.githubusercontent.com/o/r/main/index.json"]);
    let p = crate::autosave::Preferences::from_value(json!({"pluginData": [1, 2]}));
    assert_eq!(p.plugin_data, Default::default());
}

#[test]
fn install_review_list_and_remove_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::default();
    s.plugins.open_store(dir.path());
    let bytes = bundle("io.example.stored", "write", module(&[], true, ""));
    let data = base64(&bytes);

    // The confirmation: described, nothing written
    let review = s.execute("plugin.install", json!({"data": data, "review": true})).unwrap();
    assert_eq!(review["installed"], false);
    assert_eq!(review["permissions"]["access"], "write");
    assert_eq!(review["kind"], "command");
    assert_eq!(review["sha256"], crate::plugin::craft_plugin::sha256_hex(&bytes));
    assert!(s.plugins.list().is_empty());
    assert!(std::fs::read_dir(dir.path()).map(|mut d| d.next().is_none()).unwrap_or(true), "nothing is written before the install");

    // Another bundle than the one reviewed isn't installed
    let e = s.execute("plugin.install", json!({"data": data, "sha256": "0".repeat(64)})).unwrap_err();
    assert!(e.to_string().contains("reviewed"), "{e}");
    s.execute("plugin.install", json!({"data": data, "review": true})).unwrap();
    let r = s.execute("plugin.install", json!({"data": data, "sha256": review["sha256"]})).unwrap();
    assert_eq!((r["installed"].clone(), r["stored"].clone()), (json!(true), json!(true)));

    // A new session reads it back from the folder
    let mut again = Session::default();
    again.plugins.open_store(dir.path());
    let list = again.execute("plugin.list", json!({})).unwrap();
    assert_eq!(list["plugins"][0]["id"], "io.example.stored");
    assert_eq!(list["plugins"][0]["source"]["type"], "file");
    assert_eq!(list["stored"], true);

    again.execute("plugin.remove", json!({"id": "io.example.stored"})).unwrap();
    assert_eq!(again.execute("plugin.list", json!({})).unwrap()["plugins"], json!([]));
    let mut third = Session::default();
    third.plugins.open_store(dir.path());
    assert!(third.plugins.list().is_empty(), "removed from the folder");
}

#[test]
fn the_desktop_store_opens_with_the_data_directory_and_settings_persist() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::default();
    s.start_autosave(crate::autosave::AutosaveConfig::new(dir.path())).unwrap();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let wat = r#"(module
      (import "craft" "setting_set" (func $set (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 0) "count") (data (i32.const 16) "3")
      (func (export "craft_run") (param i32) (result i32) (drop (call $set (i32.const 0) (i32.const 5) (i32.const 16) (i32.const 1))) (i32.const 0)))"#;
    let bytes = bundle("io.example.counter", "read", wat::parse_str(wat).unwrap());
    s.execute("plugin.install", json!({"data": base64(&bytes)})).unwrap();
    s.execute("plugin.run", json!({"id": "io.example.counter"})).unwrap();
    s.shutdown();

    let mut again = Session::default();
    again.start_autosave(crate::autosave::AutosaveConfig::new(dir.path())).unwrap();
    assert!(again.plugins.get("io.example.counter").is_some_and(|p| p.stored), "kept in the data directory's Plug-ins folder");
    assert_eq!(again.prefs.plugin_data.settings["io.example.counter"]["count"], json!(3), "its settings are kept with the preferences");
    again.shutdown();
}

#[test]
fn plugins_for_other_apps_or_not_bundles_are_refused() {
    let mut s = Session::default();
    let manifest = json!({"id": "io.example.other", "name": "Other", "version": "1.0.0", "main": "p.wasm", "apps": {"figcraft": {"api": 6}}});
    let bytes = Bundle { manifest: manifest.to_string(), wasm: module(&[], true, ""), assets: vec![] }.encode();
    let e = s.execute("plugin.install", json!({"data": base64(&bytes)})).unwrap_err().to_string();
    assert!(e.contains("figcraft"), "{e}");
    assert!(s.execute("plugin.install", json!({"data": base64(b"not a bundle")})).is_err());
    assert!(s.execute("plugin.install", json!({})).is_err());
    assert!(s.execute("plugin.install", json!({"data": 7, "path": ["x"]})).is_err(), "hostile params are an error");
}

#[test]
fn a_local_catalog_installs_its_package() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = bundle("io.example.listed", "write", module(&[], true, ""));
    let file = dir.path().join("listed.craftplugin");
    std::fs::write(&file, &bytes).unwrap();
    let url = |p: &std::path::Path| format!("file://{}", p.to_string_lossy().replace('\\', "/"));
    let index = json!({"schema": 1, "packages": [{"id": "io.example.listed", "name": "Listed", "repo": "https://github.com/example/listed",
        "versions": [{"version": "1.0.0", "apps": {"filmcraft": {"api": 6}}, "url": url(&file),
            "sha256": crate::plugin::craft_plugin::sha256_hex(&bytes), "size": bytes.len()}]}]});
    let catalog = dir.path().join("index.json");
    std::fs::write(&catalog, index.to_string()).unwrap();
    let mut s = Session::default();

    s.execute("plugin.catalog.add", json!({"url": url(&catalog)})).unwrap();
    let listed = s.execute("plugin.catalog.list", json!({})).unwrap();
    let r = s.execute("plugin.install", json!({"catalog": url(&catalog), "id": "io.example.listed"})).unwrap();

    assert_eq!(listed["packages"][0]["id"], "io.example.listed", "{listed}");
    assert_eq!(r["inCatalog"], true);
    assert_eq!(r["source"]["type"], "catalog");
    assert_eq!(s.execute("plugin.update", json!({})).unwrap()["updates"], json!([]), "up to date");
    assert!(s.execute("plugin.catalog.add", json!({"url": "https://example.com/index.json"})).is_err(), "GitHub's hosts only");
}

#[test]
fn a_worker_download_is_offered_then_installed_without_downloading_again() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = bundle("io.example.fetched", "read", module(&[], true, ""));
    let file = dir.path().join("fetched.craftplugin");
    std::fs::write(&file, &bytes).unwrap();
    let url = format!("file://{}", file.to_string_lossy().replace('\\', "/"));
    let index = json!({"schema": 1, "packages": [{"id": "io.example.fetched", "name": "Fetched", "repo": "https://github.com/example/fetched",
        "versions": [{"version": "1.0.0", "apps": {"filmcraft": {"api": 6}}, "url": url, "sha256": crate::plugin::craft_plugin::sha256_hex(&bytes), "size": bytes.len()}]}]});
    let catalog = dir.path().join("index.json");
    std::fs::write(&catalog, index.to_string()).unwrap();
    let catalog = format!("file://{}", catalog.to_string_lossy().replace('\\', "/"));
    let mut s = Session::default();
    let request = json!({"catalog": catalog, "id": "io.example.fetched"});

    // What the UI's worker does, then the confirmation and Install
    let c = s.plugins.fetcher().with(|f| crate::plugin::download(f, &[], &request)).unwrap();
    std::fs::remove_file(&file).unwrap();
    let info = crate::plugin::offer(&mut s, &request, c).unwrap();
    let mut install: Value = request.clone();
    install["sha256"] = info["sha256"].clone();
    let r = s.execute("plugin.install", install).unwrap();

    assert_eq!(r["installed"], true, "the reviewed bundle is installed; the file it came from is gone");
    assert!(s.plugins.get("io.example.fetched").is_some());
}

#[test]
fn a_repository_install_without_network_says_so() {
    let mut s = Session::default();
    let e = s.execute("plugin.install", json!({"repo": "example/tools"})).unwrap_err().to_string();
    assert!(e.contains("no network access"), "{e}");
}
