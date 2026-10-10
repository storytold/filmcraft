//! Keyboard shortcuts: chords, presets, conflicts, panel scope, undo, files.

use super::*;
use crate::shortcuts::{Binding, Chord, CommandInfo, Platform, Shortcuts};
use serde_json::json;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-shortcuts-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The frontend's commands, as the egui UI registers them (a few are enough here).
fn with_ui_commands(s: &mut Session) {
    s.shortcuts.register_external(vec![
        CommandInfo::new("playback.toggle", "Play/Stop", &[], Some("Space")),
        CommandInfo::new("playback.forward", "Shuttle Right", &[], Some("L")),
        CommandInfo::new("playback.slowForward", "Shuttle Slow Right", &[], None),
        CommandInfo::new("playback.playAround", "Play Around", &[], None),
        CommandInfo::new("tool.razor", "Razor Tool", &[], Some("C")),
        CommandInfo::new("app.keyboardShortcuts", "Keyboard Shortcuts…", &["Edit"], Some("Cmd+Alt+K")),
    ]);
}

#[test]
fn chords_parse_normalise_and_display_per_os() {
    let c = Chord::parse("⌥⇧⌘K").unwrap();
    assert_eq!(c.canonical(), "Cmd+Alt+Shift+K");
    assert_eq!(Chord::parse("shift+opt+command+k").unwrap(), c);
    assert_eq!(c.display(Platform::Mac), "⌥⇧⌘K");
    assert_eq!(c.display(Platform::Windows), "Ctrl+Alt+Shift+K");
    assert_eq!(Chord::parse("Return").unwrap().canonical(), "Enter");
    assert_eq!(Chord::parse("Forward Delete").unwrap().canonical(), "Delete");
    assert_eq!(Chord::parse("Shift+Left").unwrap().display(Platform::Mac), "⇧←");
    // Windows text: Ctrl is the primary modifier
    assert_eq!(Chord::parse_for("Ctrl+Shift+Z", Platform::Windows).unwrap().canonical(), "Cmd+Shift+Z");
    assert_eq!(Chord::parse_for("Win+T", Platform::Windows).unwrap().canonical(), "Ctrl+T");
    // macOS Control folds into Ctrl elsewhere
    assert_eq!(Chord::parse("Ctrl+T").unwrap().effective(Platform::Windows), Chord::parse("Cmd+T").unwrap());
    assert!(Chord::parse("Cmd+").is_err());
    assert!(Chord::parse("Hyper+K").is_err());
}

#[test]
fn defaults_follow_the_registry_and_the_premiere_audit() {
    let mut s = Session::default();
    with_ui_commands(&mut s);
    let sc = &s.shortcuts;
    assert_eq!(sc.preset, "FilmCraft Default");
    assert_eq!(sc.primary("sequence.addEdit").as_deref(), Some("Cmd+K"));
    assert_eq!(sc.primary("playback.toggle").as_deref(), Some("Space"));
    // the audit adopts Premiere defaults for commands that had none
    let audit = sc.audit();
    let has = |c: &str, k: &str, p: Option<&str>| audit.iter().any(|b| b.command == c && b.keys == k && b.panel.as_deref() == p);
    assert!(has("trim.applyDefaultTransition", "Shift+D", None), "{audit:?}");
    assert!(has("playback.slowForward", "Shift+L", None));
    assert!(has("playback.playAround", "Shift+K", None));
    assert!(has("source.open", "Shift+O", Some("Project")));
    assert_eq!(sc.primary("trim.applyDefaultTransition").as_deref(), Some("Shift+D"));
    // no conflicts in any built-in preset (macOS)
    for name in crate::shortcuts::BUILTIN_PRESETS {
        let mut t = Session::default();
        with_ui_commands(&mut t);
        t.shortcuts.load_preset(name).unwrap();
        assert!(t.shortcuts.conflicts(Platform::Mac).is_empty(), "{name}: {:?}", t.shortcuts.conflicts(Platform::Mac));
    }
    // command.list reports the live bindings
    let list = s.execute("command.list", json!({})).unwrap();
    let ae = list.as_array().unwrap().iter().find(|c| c["id"] == "sequence.addEdit").unwrap();
    assert_eq!(ae["shortcut"], json!("Cmd+K"));
}

#[test]
fn assign_reassigns_conflicts_and_undoes() {
    let mut s = Session::default();
    // macOS Control folds into the primary modifier on Linux/Windows, so compare changes
    // against the platform's original conflicts rather than assuming the set starts empty.
    let baseline = s.execute("shortcuts.conflicts", json!({})).unwrap()["conflicts"].clone();
    let r = s.execute("shortcuts.set", json!({"command": "edit.undo", "keys": "Cmd+Shift+K"})).unwrap();
    assert_eq!(r["reassigned"][0]["command"], json!("sequence.addEditAllTracks"), "{r}");
    assert!(s.shortcuts.primary("sequence.addEditAllTracks").is_none());
    assert_eq!(s.shortcuts.primary("edit.undo").as_deref(), Some("Cmd+Shift+K"), "replaces Cmd+Z");
    let hit = s.execute("shortcuts.resolve", json!({"keys": "⇧⌘K"})).unwrap();
    assert_eq!(hit["command"], json!("edit.undo"));
    assert!(s.shortcuts.modified);
    // add a second shortcut, keep a conflict on purpose
    s.execute("shortcuts.set", json!({"command": "edit.undo", "keys": "Cmd+Z", "add": true})).unwrap();
    assert_eq!(s.shortcuts.for_command("edit.undo").iter().filter(|b| b.panel.is_none()).count(), 2);
    let r = s.execute("shortcuts.set", json!({"command": "edit.redo", "keys": "M", "keepConflicts": true})).unwrap();
    assert_eq!(r["conflicts"].as_array().unwrap().len(), baseline.as_array().unwrap().len() + 1, "{r}");
    let c = s.execute("shortcuts.conflicts", json!({"platform": "mac"})).unwrap();
    assert_eq!(c["conflicts"][0]["keys"], json!("M"));
    // undo / redo within the editor
    s.execute("shortcuts.undo", json!({})).unwrap();
    assert_eq!(s.execute("shortcuts.conflicts", json!({})).unwrap()["conflicts"], baseline);
    s.execute("shortcuts.undo", json!({})).unwrap();
    s.execute("shortcuts.undo", json!({})).unwrap();
    assert_eq!(s.shortcuts.primary("edit.undo").as_deref(), Some("Cmd+Z"));
    assert_eq!(s.shortcuts.primary("sequence.addEditAllTracks").as_deref(), Some("Cmd+Shift+K"));
    s.execute("shortcuts.redo", json!({})).unwrap();
    assert_eq!(s.shortcuts.primary("edit.undo").as_deref(), Some("Cmd+Shift+K"));
    // clear
    let r = s.execute("shortcuts.clear", json!({"command": "edit.undo"})).unwrap();
    assert_eq!(r["removed"], json!(2), "Cmd+Shift+K and History ▸ Left");
    assert!(s.shortcuts.for_command("edit.undo").is_empty());
    assert!(s.execute("shortcuts.set", json!({"command": "no.such", "keys": "K"})).is_err());
    assert!(s.execute("shortcuts.set", json!({"command": "edit.undo", "keys": "K", "panel": "Nowhere"})).is_err());
    // editor Cancel restores the set it opened with
    s.shortcuts.begin_editing();
    s.execute("shortcuts.set", json!({"command": "edit.undo", "keys": "U"})).unwrap();
    s.shortcuts.cancel_editing();
    assert!(s.shortcuts.for_command("edit.undo").is_empty());
}

#[test]
fn panel_shortcuts_override_application_ones_with_focus() {
    let s = Session::default();
    let left = Chord::parse("Left").unwrap();
    let p = Platform::Mac;
    assert_eq!(s.shortcuts.resolve(&left, None, p).unwrap().command, "playhead.stepBack");
    assert_eq!(s.shortcuts.resolve(&left, Some("Timeline"), p).unwrap().command, "playhead.stepBack");
    assert_eq!(s.shortcuts.resolve(&left, Some("History"), p).unwrap().command, "edit.undo");
    let bs = Chord::parse("Backspace").unwrap();
    assert_eq!(s.shortcuts.resolve(&bs, Some("Project"), p).unwrap().command, "project.delete");
    assert_eq!(s.shortcuts.resolve(&bs, None, p).unwrap().command, "edit.clear");
    let ov = s.shortcuts.overrides(p);
    assert!(ov.iter().any(|(panel, k, cmd, app)| panel == "History" && k == "Left" && cmd == "edit.undo" && app == "playhead.stepBack"), "{ov:?}");
    // Windows: macOS-Control shortcuts fold into Ctrl and can clash
    let mut t = Session::default();
    t.execute("shortcuts.set", json!({"command": "edit.copy", "keys": "Ctrl+C", "add": true, "keepConflicts": true})).unwrap();
    assert!(t.shortcuts.conflicts(Platform::Mac).is_empty());
    assert!(!t.shortcuts.conflicts(Platform::Windows).is_empty() || t.shortcuts.for_command("edit.copy").len() == 2);
    let k = t.execute("shortcuts.forKey", json!({"key": "C"})).unwrap();
    assert!(k.as_array().unwrap().iter().any(|b| b["command"] == "edit.copy" && b["keys"] == "Ctrl+C"), "{k}");
}

#[test]
fn delete_clears_timeline_clips_and_project_items() {
    // #243: with the Timeline focused, Delete (the forward-delete key, labelled Delete on Windows
    // and Linux keyboards) resolved to Project ▸ Clear and left the selected clips in place.
    let s = Session::default();
    let del = Chord::parse("Delete").unwrap();
    for p in [Platform::Mac, Platform::Windows] {
        assert_eq!(s.shortcuts.resolve(&del, Some("Timeline"), p).unwrap().command, "edit.clear", "{p:?}");
        assert_eq!(s.shortcuts.resolve(&del, Some("Project"), p).unwrap().command, "project.delete", "{p:?}");
        assert_eq!(s.shortcuts.resolve(&del, None, p).unwrap().command, "project.delete", "{p:?}");
    }
}

#[test]
fn compat_presets_map_other_editors() {
    let mut s = Session::default();
    with_ui_commands(&mut s);
    s.execute("shortcuts.loadPreset", json!({"name": "Premiere Pro Compatible"})).unwrap();
    assert_eq!(s.shortcuts.primary("markers.clearIn").as_deref(), Some("Alt+I"));
    assert_eq!(s.shortcuts.primary("edit.clear").as_deref(), Some("Delete"));
    assert!(s.shortcuts.for_command("edit.clear").iter().any(|b| b.panel.as_deref() == Some("Timeline") && b.keys == "Backspace"));
    s.execute("shortcuts.loadPreset", json!({"name": "Final Cut Pro Compatible"})).unwrap();
    assert_eq!(s.shortcuts.primary("source.insert").as_deref(), Some("W"));
    assert_eq!(s.shortcuts.primary("tool.razor").as_deref(), Some("B"));
    assert!(s.shortcuts.primary("trim.rippleNext").is_none(), "W was taken by Insert");
    s.execute("shortcuts.loadPreset", json!({"name": "Avid Media Composer Compatible"})).unwrap();
    assert_eq!(s.shortcuts.primary("sequence.lift").as_deref(), Some("Z"));
    assert_eq!(s.shortcuts.for_command("markers.markIn").len(), 2, "I and E");
    assert!(!s.shortcuts.modified);
    assert!(s.execute("shortcuts.loadPreset", json!({"name": "Nope"})).is_err());
    s.execute("shortcuts.loadPreset", json!({"name": "FilmCraft Default"})).unwrap();
    assert_eq!(s.shortcuts.primary("source.insert").as_deref(), Some(","));
}

#[test]
fn custom_presets_persist_export_and_import() {
    let dir = temp_dir("presets");
    let mut s = Session::default();
    s.shortcuts.set_dir(&dir);
    s.execute("shortcuts.set", json!({"command": "sequence.addEdit", "keys": "Cmd+Shift+B"})).unwrap();
    let r = s.execute("shortcuts.savePreset", json!({"name": "My Keys"})).unwrap();
    assert!(std::path::Path::new(r["path"].as_str().unwrap()).exists());
    let p = s.execute("shortcuts.presets", json!({})).unwrap();
    assert_eq!(p["active"], json!("My Keys"));
    assert_eq!(p["custom"], json!(["My Keys"]));
    assert!(s.execute("shortcuts.savePreset", json!({"name": "FilmCraft Default"})).is_err());
    // the active set survives a restart
    let mut t = Shortcuts::new();
    t.set_dir(&dir);
    assert_eq!(t.preset, "My Keys");
    assert_eq!(t.primary("sequence.addEdit").as_deref(), Some("Cmd+Shift+B"));
    // switch away and back
    s.execute("shortcuts.loadPreset", json!({"name": "FilmCraft Default"})).unwrap();
    assert_eq!(s.shortcuts.primary("sequence.addEdit").as_deref(), Some("Cmd+K"));
    s.execute("shortcuts.loadPreset", json!({"name": "My Keys"})).unwrap();
    assert_eq!(s.shortcuts.primary("sequence.addEdit").as_deref(), Some("Cmd+Shift+B"));
    // export, then import a Windows-notation file
    let out = dir.join("export.json");
    s.execute("shortcuts.export", json!({"path": out.to_string_lossy()})).unwrap();
    let f: crate::shortcuts::PresetFile = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert!(f.bindings.contains(&Binding { command: "sequence.addEdit".into(), keys: "Cmd+Shift+B".into(), panel: None }));
    let win = dir.join("win.json");
    std::fs::write(
        &win,
        serde_json::to_vec(&json!({"format": "filmcraft-keyboard-shortcuts", "version": 1, "name": "From Windows", "platform": "windows",
            "bindings": [{"command": "edit.undo", "keys": "Ctrl+Z"}, {"command": "sequence.addEdit", "keys": "Ctrl+Alt+K"}, {"command": "no.such", "keys": "K"}]}))
        .unwrap(),
    )
    .unwrap();
    let r = s.execute("shortcuts.import", json!({"path": win.to_string_lossy()})).unwrap();
    assert_eq!(r["name"], json!("From Windows"));
    assert_eq!(s.shortcuts.primary("edit.undo").as_deref(), Some("Cmd+Z"));
    assert_eq!(s.shortcuts.primary("sequence.addEdit").as_deref(), Some("Cmd+Alt+K"));
    assert_eq!(s.shortcuts.bindings.len(), 2, "unknown commands dropped");
    s.execute("shortcuts.deletePreset", json!({"name": "My Keys"})).unwrap();
    assert!(s.execute("shortcuts.deletePreset", json!({"name": "FilmCraft Default"})).is_err());
    assert_eq!(s.execute("shortcuts.presets", json!({})).unwrap()["custom"], json!(["From Windows"]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_searches_labels_and_keys() {
    let s = Session::default();
    let r = s.shortcuts.commands();
    assert!(r.iter().any(|c| c.id == "trim.forward"));
    assert!(!r.iter().any(|c| c.id == "timeline.move"), "commands with required params are not bindable");
    // a parameter doc holds parameters only (each optional or with a default): a result
    // description in it would take the command out of this list
    for id in ["clip.replaceFromBin", "clip.replaceFromSource", "clip.replaceFromSourceMatchFrame"] {
        assert!(r.iter().any(|c| c.id == id), "{id} has no required parameter, so it can be bound");
    }
    let mut s = s;
    let v = s.execute("shortcuts.list", json!({"query": "ripple delete"})).unwrap();
    assert!(v.as_array().unwrap().iter().any(|c| c["id"] == "edit.rippleDelete"), "{v}");
    let v = s.execute("shortcuts.list", json!({"query": "cmd+k", "platform": "windows"})).unwrap();
    assert!(v.as_array().unwrap().iter().any(|c| c["id"] == "sequence.addEdit"), "{v}");
    let v = s.execute("shortcuts.list", json!({"panel": "History", "assigned": true})).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 2, "{v}");
}

#[test]
fn default_keymaps_have_no_conflicts_on_any_platform() {
    // The Premiere audit used to check keys on macOS only, so off macOS `⌃9`/`⌘9`, `⌃⇧M`/`⌘⇧M` and
    // `⌃T`/`⌘T` ended up on the same Ctrl key; CI only ran on macOS, so nobody saw it.
    use crate::shortcuts::Platform;
    let s = Session::default();
    for p in [Platform::Mac, Platform::Windows, Platform::Linux] {
        let mut sc = s.shortcuts.clone();
        sc.bindings = sc.builtin_for(crate::shortcuts::DEFAULT_PRESET, p).unwrap();
        assert!(sc.conflicts(p).is_empty(), "{p:?}: {:?}", sc.conflicts(p));
    }
    // macOS keeps every Premiere key it had
    let mac = s.shortcuts.builtin_for(crate::shortcuts::DEFAULT_PRESET, Platform::Mac).unwrap();
    for (cmd, keys) in [("multicam.cutToCamera9", "Ctrl+9"), ("markers.addRange", "Ctrl+Shift+M"), ("graphics.newText", "Cmd+T")] {
        assert!(mac.iter().any(|b| b.command == cmd && b.keys == keys), "{cmd} {keys} on macOS");
    }
}
