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
fn clear_close_sequence_also_clears_file_close() {
    // #773: File ▸ Close (`file.close`, Cmd+W) aliases Close Sequence (`sequence.close`).
    // The editor's Clear sends panel Application against `sequence.close`, which had only a
    // Timeline binding — Cmd+W stayed on `file.close`.
    let mut s = Session::default();
    assert_eq!(s.shortcuts.primary("file.close").as_deref(), Some("Cmd+W"));
    assert_eq!(s.shortcuts.primary("sequence.close").as_deref(), Some("Cmd+W"));
    assert!(s.shortcuts.for_command("sequence.close").iter().any(|b| b.keys == "Cmd+W"));
    let r = s.execute("shortcuts.clear", json!({"command": "sequence.close", "panel": "Application"})).unwrap();
    assert!(r["removed"].as_u64().unwrap() >= 1, "{r}");
    assert!(s.shortcuts.for_command("sequence.close").is_empty(), "{:?}", s.shortcuts.for_command("sequence.close"));
    assert!(s.shortcuts.primary("file.close").is_none());
    let w = Chord::parse("Cmd+W").unwrap();
    assert!(s.shortcuts.resolve(&w, None, Platform::current()).is_none());
    assert!(s.shortcuts.resolve(&w, Some("Timeline"), Platform::current()).is_none());
    s.execute("shortcuts.set", json!({"command": "sequence.addEdit", "keys": "Cmd+W", "add": true})).unwrap();
    assert_eq!(s.shortcuts.resolve(&w, None, Platform::current()).unwrap().command, "sequence.addEdit");
}

#[test]
fn clear_new_bin_frees_cmd_b_in_the_project_panel_too() {
    // #1333: New Bin has Cmd+B application-wide and again in the Project panel. The editor's Clear
    // sends panel Application, so only the first went; the Project panel kept Cmd+B.
    let dir = temp_dir("new-bin");
    let mut s = Session::default();
    s.shortcuts.set_dir(&dir);
    let b = Chord::parse("Cmd+B").unwrap();
    let p = Platform::current();
    assert_eq!(s.shortcuts.resolve(&b, Some("Project"), p).unwrap().command, "file.newBin");
    s.execute("shortcuts.clear", json!({"command": "file.newBin", "panel": "Application"})).unwrap();
    assert!(s.shortcuts.for_command("file.newBin").is_empty(), "{:?}", s.shortcuts.for_command("file.newBin"));
    s.execute("shortcuts.set", json!({"command": "sequence.addEdit", "keys": "Cmd+B"})).unwrap();
    let mut t = Shortcuts::new();
    t.set_dir(&dir);
    for sc in [&s.shortcuts, &t] {
        assert_eq!(sc.resolve(&b, None, p).unwrap().command, "sequence.addEdit");
        assert_eq!(sc.resolve(&b, Some("Project"), p).unwrap().command, "sequence.addEdit");
        assert!(sc.for_command("file.newBin").is_empty());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reassigning_new_bin_moves_its_project_panel_key_too() {
    // #1333: assigning New Bin another key replaced Cmd+B application-wide only.
    let mut s = Session::default();
    s.execute("shortcuts.set", json!({"command": "file.newBin", "keys": "Cmd+Alt+B"})).unwrap();
    let keys: Vec<(&str, Option<&str>)> = s.shortcuts.for_command("file.newBin").iter().map(|b| (b.keys.as_str(), b.panel.as_deref())).collect();
    assert_eq!(keys, [("Cmd+Alt+B", None)]);
    assert!(s.shortcuts.resolve(&Chord::parse("Cmd+B").unwrap(), Some("Project"), Platform::Mac).is_none());
    // a panel key of its own (History ▸ Left for Undo) is not a copy of the application one: it stays
    s.execute("shortcuts.set", json!({"command": "edit.undo", "keys": "Cmd+Alt+Z"})).unwrap();
    assert!(s.shortcuts.for_command("edit.undo").iter().any(|b| b.panel.as_deref() == Some("History") && b.keys == "Left"));
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
    // #2489: a built-in set exported and imported again is listed, under a name of its own
    s.execute("shortcuts.loadPreset", json!({"name": "FilmCraft Default"})).unwrap();
    let again = dir.join("FilmCraft Default.json");
    s.execute("shortcuts.export", json!({"path": again.to_string_lossy()})).unwrap();
    let r = s.execute("shortcuts.import", json!({"path": again.to_string_lossy()})).unwrap();
    assert_eq!(r["name"], json!("FilmCraft Default (Imported)"));
    let presets = s.execute("shortcuts.presets", json!({})).unwrap();
    assert_eq!(presets["active"], json!("FilmCraft Default (Imported)"));
    assert!(presets["custom"].as_array().unwrap().contains(&json!("FilmCraft Default (Imported)")), "{presets}");
    // under another file name it keeps the set's name, not the file's (#2705 review)
    s.execute("shortcuts.loadPreset", json!({"name": "FilmCraft Default"})).unwrap();
    let renamed = dir.join("export.json");
    s.execute("shortcuts.export", json!({"path": renamed.to_string_lossy()})).unwrap();
    assert_eq!(s.execute("shortcuts.import", json!({"path": renamed.to_string_lossy()})).unwrap()["name"], json!("FilmCraft Default (Imported)"));
    // a file past the size limit is refused, not read whole
    let big = dir.join("big.json");
    std::fs::write(&big, vec![b' '; (8 << 20) + 16]).unwrap();
    assert!(s.execute("shortcuts.import", json!({"path": big.to_string_lossy()})).unwrap_err().to_string().contains("too large"));
    // Premiere's own files say why they can't be read
    let kys = dir.join("Mine.kys");
    std::fs::write(&kys, b"<?xml version=\"1.0\"?><shortcuts/>").unwrap();
    let e = s.execute("shortcuts.import", json!({"path": kys.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains(".kys") && e.contains("Premiere Pro Compatible"), "{e}");
    // a FilmCraft export named .kys still imports: the contents decide, not the name (#2705 review)
    let named_kys = dir.join("Exported.kys");
    s.execute("shortcuts.export", json!({"path": named_kys.to_string_lossy()})).unwrap();
    assert!(s.execute("shortcuts.import", json!({"path": named_kys.to_string_lossy()})).is_ok());
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
    assert_eq!(v.as_array().unwrap().len(), 3, "{v}");
}

/// A command run without parameters whose parameters are all optional or defaulted opens its
/// dialog instead of running (ui-egui routes them through clip_dialogs / menu_dialogs /
/// media_dialogs), so it belongs in the shortcut editor. A parameter left marked required takes it
/// out of the list even though the engine defaults it (#2024).
///
/// The engine cannot see ui-egui's dialog tables, so this list is the dialog commands of today.
/// ui-egui's `every_command_with_a_clip_dialog_can_be_bound` and
/// `every_command_with_a_menu_dialog_can_be_bound` walk the registry against `clip_dialogs::meta`
/// and `menu_dialogs::meta` themselves, so a command added to either table later is checked too.
#[test]
fn commands_that_open_dialogs_can_be_bound() {
    let r = Session::default().shortcuts.commands();
    for id in [
        // clip_dialogs::meta
        "edit.pasteAttributes",
        "edit.removeAttributes",
        "file.newOfflineFile",
        "clip.nest",
        "clip.makeSubclip",
        "clip.editSubclip",
        "clip.audioChannels",
        "clip.modifyTimecode",
        "clip.frameHoldOptions",
        "clip.fieldOptions",
        "clip.speedDuration",
        "file.closeProject",
        // menu_dialogs::meta, minus file.mediaProperties and clip.sourceSettings (views, not
        // undoable actions: they stay out of the editor on purpose even though a default key
        // names mediaProperties)
        "edit.find",
        "file.newSearchBin",
        "clip.editOffline",
        "clip.updateMetadata",
        "clip.automateToSequence",
        "clip.sceneEditDetection",
        "sequence.normalizeMixTrack",
        "sequence.simplify",
        "sequence.transcribe",
        "file.saveAsTemplate",
        "markers.addFlashCue",
        "file.newColorMatte",
        // media_dialogs::route, minus the ones that relink a file picked in a native dialog
        // (media.attachProxies, media.reconnectFullRes) and media.linkMedia (a dialog the app
        // opens but does not edit the project, so it is not a bindable action)
        "media.makeOffline",
        "media.createProxies",
    ] {
        assert!(r.iter().any(|c| c.id == id), "{id} opens its dialog when run without parameters, so it can be bound");
    }
    // and the assignment goes through, so the key can be recorded and shown in the editor
    let mut s = Session::default();
    s.execute("shortcuts.set", json!({"command": "clip.nest", "keys": "Cmd+Alt+N"})).unwrap();
    assert_eq!(s.shortcuts.primary("clip.nest").as_deref(), Some("Cmd+Alt+N"));
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
