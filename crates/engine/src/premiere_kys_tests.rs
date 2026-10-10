//! Premiere Pro `.kys` import: key codes and keyboard layouts, command mapping, the preset it
//! makes, and hostile files.

use serde_json::json;

use crate::Session;
use crate::premiere_kys::{self, COMMANDS, PANEL_COMMANDS};
use crate::shortcuts::{Chord, CommandInfo, KeyLayout, Platform};

const CHAR: u32 = 0x8000_0000;
const KEYPAD: u32 = 0x4000_0000;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-kys-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// One `<item>`: (command, key code, ctrl, alt, shift); code 0 = unbound.
type Item<'a> = (&'a str, u32, bool, bool, bool);

/// A `.kys` document with one context per (name, items).
fn kys(contexts: &[(&str, &[Item])]) -> String {
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<PremiereData Version=\"3\">\n<shortcuts Version=\"5\">\n");
    for (ctx, items) in contexts {
        s.push_str(&format!("<context.{ctx} Version=\"1\">\n"));
        for (i, (cmd, code, c, a, sh)) in items.iter().enumerate() {
            s.push_str(&format!("<item.{i} Version=\"1\">"));
            if *code != 0 {
                s.push_str(&format!(
                    "<virtualkey>{code}</virtualkey><modifier.ctrl>{c}</modifier.ctrl><modifier.alt>{a}</modifier.alt><modifier.shift>{sh}</modifier.shift>"
                ));
            }
            s.push_str(&format!("<commandname>{cmd}</commandname></item.{i}>\n"));
        }
        s.push_str(&format!("</context.{ctx}>\n"));
    }
    s.push_str("<platform>windows</platform>\n</shortcuts>\n</PremiereData>\n");
    s
}

fn ch(c: char) -> u32 {
    CHAR | c as u32
}

/// The frontend's commands that the sample files use (as the egui UI registers them).
fn with_ui_commands(s: &mut Session) {
    s.shortcuts.register_external(vec![
        CommandInfo::new("playback.toggle", "Play/Stop", &[], Some("Space")),
        CommandInfo::new("playback.reverse", "Shuttle Left", &[], Some("J")),
        CommandInfo::new("playback.forward", "Shuttle Right", &[], Some("L")),
        CommandInfo::new("view.zoomIn", "Zoom In", &["View"], Some("=")),
        CommandInfo::new("view.zoomOut", "Zoom Out", &["View"], Some("-")),
        CommandInfo::new("view.zoomToSequence", "Zoom to Sequence", &["View"], Some("\\")),
        CommandInfo::new("tool.razor", "Razor Tool", &[], Some("C")),
        CommandInfo::new("tool.rolling", "Rolling Edit Tool", &[], Some("N")),
        CommandInfo::new("multicam.selectCamera1", "Camera 1", &[], None),
        CommandInfo::new("window.maximizeFrameUnderCursor", "Maximize or Restore Frame Under Cursor", &[], Some("`")),
    ]);
}

#[test]
fn layouts_name_and_label_keys() {
    let de = KeyLayout::De;
    for (c, k) in [('ö', ";"), ('Ä', "'"), ('ü', "["), ('ß', "-"), ('+', "]"), ('#', "\\"), ('<', "IntlBackslash"), ('^', "`"), ('´', "="), ('-', "/")] {
        assert_eq!(de.key_for_char(c), Some(k), "{c}");
        assert_eq!(de.label(k).chars().next().map(|l| l.to_lowercase().to_string()), Some(c.to_lowercase().to_string()), "{k}");
    }
    assert_eq!(de.key_for_char('z'), Some("Z"), "letters stay letters (QWERTZ labels)");
    assert_eq!(de.key_for_char('7'), Some("7"));
    assert_eq!(de.key_for_char('='), None, "Shift+0 on a German keyboard");
    assert_eq!(KeyLayout::Us.key_for_char(';'), Some(";"));
    assert_eq!(KeyLayout::Us.key_for_char('ö'), None);
    let c = Chord::parse("Cmd+Alt+;").unwrap();
    assert_eq!(c.display_in(Platform::Windows, de), "Ctrl+Alt+Ö");
    assert_eq!(c.display_in(Platform::Windows, KeyLayout::Us), "Ctrl+Alt+;");
    assert_eq!(c.display_in(Platform::Mac, de), "⌥⌘Ö");
    assert_eq!(Chord::parse("Shift+<").unwrap().canonical(), "Shift+IntlBackslash");
    assert_eq!(Chord::parse("intlbackslash").unwrap().display_in(Platform::Linux, de), "<");
    assert_eq!(KeyLayout::from_name("Deutsch"), Some(de));
    assert_eq!(KeyLayout::from_name("qwerty-ish"), None);
}

#[test]
fn a_german_file_reads_its_keys_and_commands() {
    let text = kys(&[
        (
            "global",
            &[
                ("cmd.transport.toggleplay", 1, false, false, false),
                ("cmd.sequence.decreaseclipvolume", ch('Ö'), false, false, false),
                ("cmd.zoom.in", ch('+'), false, false, false),
                ("cmd.tlnav.zoomto.sequence", ch('#'), false, false, false),
                ("cmd.transport.shuttle.right", ch('<'), false, false, false),
                ("cmd.edit.rippledelete", 35, false, false, true),
                ("cmd.sequence.extract", ch('ß'), true, true, false),
                ("cmd.multicam.choosenocut.camera1", KEYPAD | ch('1'), false, false, false),
                // unbound: listed on purpose
                ("cmd.tools.04roll", 0, false, false, false),
                // FilmCraft has no such command
                ("cmd.clip.aeify", ch('N'), false, false, false),
                // not a key on a German keyboard
                ("cmd.zoom.out", ch('='), false, false, false),
                // a key code Premiere has but FilmCraft does not know
                ("cmd.tlnav.select.next.clip", 5, false, false, false),
            ],
        ),
        ("timeline", &[("cmd.timeline.ripple.delete", 2, false, true, false), ("cmd.timeline.nudge.left.one", 42, false, true, false)]),
        ("prproduction", &[("cmd.prproductionfolder.openproject", ch('O'), true, false, false)]),
    ]);
    let f = premiere_kys::parse(&text, None).unwrap();
    assert_eq!(f.layout, KeyLayout::De, "Ö in the file");
    assert_eq!(f.platform, Platform::Windows);
    let has = |cmd: &str, keys: &str, panel: Option<&str>| f.entries.iter().any(|e| e.command == cmd && e.keys == keys && e.panel == panel);
    assert!(has("playback.toggle", "Space", None));
    assert!(has("clip.volumeDown", ";", None), "{:?}", f.entries);
    assert!(has("view.zoomIn", "]", None));
    assert!(has("view.zoomToSequence", "\\", None));
    assert!(has("playback.forward", "IntlBackslash", None));
    assert!(has("edit.rippleDelete", "Shift+Delete", None));
    assert!(has("sequence.extract", "Cmd+Alt+-", None), "Ctrl is the primary modifier");
    assert!(has("multicam.selectCamera1", "1", None) && f.entries.iter().any(|e| e.keypad), "keypad keys are their main twins");
    assert!(has("edit.rippleDelete", "Alt+Backspace", Some("Timeline")));
    assert!(has("timeline.nudgeLeft", "Alt+Left", Some("Timeline")));
    assert!(f.listed.iter().any(|l| l == "tool.rolling"), "unbound commands are listed");
    let skipped = |cmd: &str| f.skipped.iter().find(|s| s.command == cmd).map(|s| s.reason.clone());
    assert!(skipped("cmd.clip.aeify").is_some_and(|r| r.contains("After Effects")));
    assert!(skipped("cmd.zoom.out").is_some_and(|r| r.contains("German")));
    assert!(skipped("cmd.tlnav.select.next.clip").is_some());
    assert!(skipped("cmd.prproductionfolder.openproject").is_some());
    assert!(f.reserved.iter().any(|(p, k)| p.is_none() && k == "N"), "N stays free for the command FilmCraft lacks");
    // the same file read as US English: Ö is no US key
    let us = premiere_kys::parse(&text, Some(KeyLayout::Us)).unwrap();
    assert!(us.skipped.iter().any(|s| s.command == "cmd.sequence.decreaseclipvolume"));
}

#[test]
fn import_makes_an_active_preset_from_filmcraft_default() {
    let dir = temp_dir("import");
    let mut s = Session::default();
    with_ui_commands(&mut s);
    s.shortcuts.set_dir(&dir);
    let text = kys(&[(
        "global",
        &[
            ("cmd.zoom.in", ch('+'), false, false, false),
            ("cmd.sequence.decreaseclipvolume", ch('Ö'), false, false, false),
            ("cmd.transport.shuttle.left", ch('J'), false, false, false),
            // Premiere's own key for Zoom to Sequence is unbound here: so is FilmCraft's
            ("cmd.tlnav.zoomto.sequence", 0, false, false, false),
            ("cmd.edit.undo", ch('4'), false, false, false),
            ("cmd.edit.undo", ch('Z'), true, false, false),
            ("cmd.multicam.choosenocut.camera1", ch('1'), false, false, false),
            // the keypad twin of 4 cannot have camera 4: 4 is Undo
            ("cmd.multicam.choosenocut.camera4", KEYPAD | ch('4'), false, false, false),
            // C is the Razor Tool in FilmCraft Default; here it is a command FilmCraft lacks
            ("cmd.tools.25rangeselection", ch('C'), false, false, false),
            ("cmd.tools.01pointer", ch('V'), false, false, false),
        ],
    )]);
    let path = dir.join("My Premiere Keys.kys");
    std::fs::write(&path, &text).unwrap();
    let r = s.execute("shortcuts.import", json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(r["name"], json!("My Premiere Keys"));
    assert_eq!(r["premiere"], json!(true));
    assert_eq!(r["layout"], json!("de"));
    assert_eq!(s.prefs.general.keyboard_layout, "de", "labels follow the file's keyboard");
    let sc = &s.shortcuts;
    assert_eq!(sc.preset, "My Premiere Keys");
    assert!(!sc.modified);
    assert_eq!(sc.primary("view.zoomIn").as_deref(), Some("]"));
    assert_eq!(sc.primary("clip.volumeDown").as_deref(), Some(";"));
    assert!(sc.primary("view.zoomToSequence").is_none(), "listed without a key: unbound");
    assert!(sc.for_command("edit.undo").iter().any(|b| b.keys == "4") && sc.for_command("edit.undo").iter().any(|b| b.keys == "Cmd+Z"));
    assert_eq!(sc.primary("multicam.selectCamera1").as_deref(), Some("1"));
    assert!(sc.primary("multicam.selectCamera4").is_none());
    assert!(r["skipped"].as_array().unwrap().iter().any(|x| x["command"] == "cmd.multicam.choosenocut.camera4"), "{r}");
    assert!(sc.primary("tool.razor").is_none(), "C was given to a command FilmCraft lacks");
    // commands Premiere does not have keep FilmCraft's defaults
    assert_eq!(sc.primary("sequence.addEdit").as_deref(), Some("Cmd+K"));
    assert!(sc.conflicts(Platform::current()).is_empty(), "{:?}", sc.conflicts(Platform::current()));
    // ⌘; shows as Ctrl+Ö / ⌘Ö now
    let l = s.execute("shortcuts.get", json!({"command": "clip.volumeDown", "platform": "windows"})).unwrap();
    assert_eq!(l["shortcuts"][0]["display"], json!("Ö"));
    // saved as a custom preset: loads again after switching away
    assert_eq!(s.execute("shortcuts.presets", json!({})).unwrap()["custom"], json!(["My Premiere Keys"]));
    s.execute("shortcuts.loadPreset", json!({"name": "FilmCraft Default"})).unwrap();
    assert_eq!(s.shortcuts.primary("view.zoomIn").as_deref(), Some("="));
    s.execute("shortcuts.loadPreset", json!({"name": "My Premiere Keys"})).unwrap();
    assert_eq!(s.shortcuts.primary("view.zoomIn").as_deref(), Some("]"));
    // a FilmCraft preset file still imports as before
    let out = dir.join("export.json");
    s.execute("shortcuts.export", json!({"path": out.to_string_lossy()})).unwrap();
    let r = s.execute("shortcuts.import", json!({"path": out.to_string_lossy()})).unwrap();
    assert!(r.get("premiere").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_mapped_command_exists() {
    // UI commands are checked against the frontend's list in the egui tests
    let ui_prefixes =
        ["tool.", "playback.", "view.", "window.", "mode.", "app.", "help.", "panel.", "projectPanel.", "textPanel.", "effectControls.", "multicam.toggleView"];
    let ui_ids = ["multicam.recordToggle", "timeline.playheadToCursor", "mixer.meterInputOnly", "mixer.showHideTracks", "graphics.beginTextEditing"];
    let ui_ids2 = ["timeline.expandAllTracks", "timeline.minimizeAllTracks", "timeline.nextScreen", "timeline.prevScreen"];
    let ui_ids3 = ["timeline.increaseVideoHeight", "timeline.decreaseVideoHeight", "timeline.increaseAudioHeight", "timeline.decreaseAudioHeight"];
    let s = Session::default();
    let bindable = s.shortcuts.commands();
    let mut bad = Vec::new();
    for (premiere, id) in COMMANDS.iter().map(|(p, i)| (*p, *i)).chain(PANEL_COMMANDS.iter().map(|(_, p, i)| (*p, *i))) {
        let ui = ui_prefixes.iter().any(|p| id.starts_with(p)) || ui_ids.iter().chain(&ui_ids2).chain(&ui_ids3).any(|u| *u == id);
        if ui && crate::commands::find(id).is_none() {
            continue;
        }
        // keyboard-only engine commands (no menu, a result to return) are known without being
        // listed for the editor, as the Premiere preset has them
        if !bindable.iter().any(|c| c.id == id) && crate::commands::find(id).is_none() {
            bad.push(format!("{premiere} → {id}"));
        }
    }
    assert!(bad.is_empty(), "not engine commands: {bad:#?}");
    // menu items can have keys, whatever parameters agents may pass them
    for id in ["clip.nest", "clip.rename", "file.exportAaf", "sequence.simplify"] {
        assert!(bindable.iter().any(|c| c.id == id), "{id}");
    }
    let mut seen = std::collections::HashSet::new();
    for (p, _) in COMMANDS {
        assert!(seen.insert(*p), "{p} is mapped twice");
    }
}

#[test]
fn hostile_files_are_errors_not_crashes() {
    let good = kys(&[("global", &[("cmd.transport.toggleplay", 1, false, false, false), ("cmd.edit.undo", ch('Z'), true, false, false)])]);
    for bad in [
        String::new(),
        "not xml at all".into(),
        "<PremiereData>".into(),
        "<Other><shortcuts/></Other>".into(),
        "<!DOCTYPE x [<!ENTITY a \"aaaa\">]><PremiereData/>".into(),
        good.replace("</PremiereData>", ""),
    ] {
        assert!(premiere_kys::parse(&bad, None).is_err(), "{bad:?}");
    }
    // nonsense codes and modifiers are skipped or reported, never trusted
    let odd = kys(&[(
        "global",
        &[
            ("cmd.edit.undo", u32::MAX, true, false, false),
            ("cmd.edit.redo", CHAR | 0xD800, false, false, false),
            ("cmd.edit.cut", 999, false, false, false),
            ("cmd.edit.copy", CHAR | KEYPAD | '*' as u32, false, false, false),
        ],
    )])
    .replace("<virtualkey>4294967295</virtualkey>", "<virtualkey>-7</virtualkey>");
    let f = premiere_kys::parse(&odd, None).unwrap();
    assert!(f.entries.is_empty(), "{:?}", f.entries);
    // mutation fuzz: truncations and byte flips never panic
    let bytes = good.as_bytes();
    for i in (0..bytes.len()).step_by(7) {
        let mut b = bytes.to_vec();
        b.truncate(i);
        let _ = std::panic::catch_unwind(|| premiere_kys::parse(&String::from_utf8_lossy(&b), None)).expect("truncated file panicked");
        let mut b = bytes.to_vec();
        if let Some(x) = b.get_mut(i) {
            *x ^= 0x5a;
        }
        let _ = std::panic::catch_unwind(|| premiere_kys::parse(&String::from_utf8_lossy(&b), None)).expect("flipped byte panicked");
    }
    // the command reports a bad file instead of failing silently
    let dir = temp_dir("hostile");
    let mut s = Session::default();
    s.shortcuts.set_dir(&dir);
    let p = dir.join("broken.kys");
    std::fs::write(&p, "<PremiereData><shortcuts>").unwrap();
    assert!(s.execute("shortcuts.import", json!({"path": p.to_string_lossy()})).is_err());
    assert!(s.execute("shortcuts.import", json!({"path": p.to_string_lossy(), "layout": "klingon"})).is_err());
    assert_eq!(s.shortcuts.preset, "FilmCraft Default", "nothing changed");
    let _ = std::fs::remove_dir_all(&dir);
}
