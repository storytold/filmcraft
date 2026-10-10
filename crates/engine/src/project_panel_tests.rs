//! Tests of the Project panel commands (`project_panel`): view settings, Metadata Display columns,
//! sorting, view presets, Freeform positions / stacks / arrangements (undoable, saved with the
//! project) and bin renames.

use filmcraft_project::{BinEntry, ItemId, ItemKind};
use serde_json::{Value, json};

use crate::Session;
use crate::project_panel::{self, FREEFORM_POS, FREEFORM_STACK, FontSize, ViewMode};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn media(s: &Session) -> Vec<ItemId> {
    let mut v: Vec<ItemId> = s.project.items.values().filter(|i| i.as_media().is_some()).map(|i| i.id).collect();
    v.sort();
    // the items of the first media item's bin (the demo's Footage bin)
    let bin = s.project.root.parent_of(v[0]);
    v.retain(|i| s.project.root.parent_of(*i) == bin);
    v
}

fn bin_of(s: &Session, i: ItemId) -> u64 {
    s.project.root.parent_of(i).unwrap().0
}

fn names(rows: &Value) -> Vec<String> {
    rows["rows"].as_array().unwrap().iter().filter(|r| r.get("item").is_some()).map(|r| r["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn view_settings_are_preferences() {
    let mut s = demo();
    assert_eq!(s.prefs.project_panel.view.mode, ViewMode::Icon);
    let r = s.execute("project.view.set", json!({"view": "list", "fontSize": "large", "previewArea": true, "iconSize": 1000, "hoverScrub": false})).unwrap();
    assert_eq!(r["view"]["mode"], "list");
    let pp = &s.prefs.project_panel;
    assert_eq!(pp.view.mode, ViewMode::List);
    assert_eq!(pp.view.font_size, FontSize::Large);
    assert!(pp.preview_area && !pp.hover_scrub);
    assert_eq!(pp.view.icon_size, 400.0, "clamped");
    assert!(s.execute("project.view.set", json!({"view": "grid"})).is_err());
    // round-trips through the preferences file format
    let text = serde_json::to_string(&s.prefs).unwrap();
    let back: crate::autosave::Preferences = serde_json::from_str(&text).unwrap();
    assert_eq!(back.project_panel, s.prefs.project_panel);
    // older preference files (no projectPanel key) load with the defaults
    let mut v: Value = serde_json::from_str(&text).unwrap();
    v.as_object_mut().unwrap().remove("projectPanel");
    let old: crate::autosave::Preferences = serde_json::from_value(v).unwrap();
    assert_eq!(old.project_panel, project_panel::ProjectPanelPrefs::default());
}

#[test]
fn metadata_display_chooses_and_orders_columns() {
    let mut s = demo();
    let ids = media(&s);
    s.execute("metadata.set", json!({"item": ids[0].0, "field": "Mood", "value": "calm"})).unwrap();
    let l = s.execute("project.columns.list", json!({})).unwrap();
    let avail: Vec<&str> = l["available"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    for c in ["Name", "Label", "Media Duration", "Video Usage", "Mood"] {
        assert!(avail.contains(&c), "{c} missing from {avail:?}");
    }
    // Freeform bookkeeping keys are not offered
    s.execute("project.freeform.move", json!({"items": [ids[0].0], "x": 40, "y": 40})).unwrap();
    let l = s.execute("project.columns.list", json!({})).unwrap();
    assert!(!l["available"].as_array().unwrap().iter().any(|c| c["name"] == FREEFORM_POS));

    // Name stays first; unknown-case names are canonicalised; widths are kept
    s.execute("project.columns.set", json!({"columns": ["media duration", {"name": "Mood", "width": 90}, "Label"]})).unwrap();
    let cols: Vec<String> = s.prefs.project_panel.view.columns.iter().map(|c| c.name.clone()).collect();
    assert_eq!(cols, ["Name", "Media Duration", "Mood", "Label"]);
    assert_eq!(s.prefs.project_panel.view.columns[2].width, 90.0);
    s.execute("project.columns.resize", json!({"column": "Label", "width": 140})).unwrap();
    assert_eq!(s.prefs.project_panel.view.columns[3].width, 140.0);
    assert!(s.execute("project.columns.resize", json!({"column": "Frame Rate", "width": 140})).is_err(), "not shown");

    // the rows carry the shown columns' text
    let rows = s.execute("project.items", json!({"bin": bin_of(&s, ids[0])})).unwrap();
    let row = rows["rows"].as_array().unwrap().iter().find(|r| r["item"] == json!(ids[0].0)).unwrap();
    assert_eq!(row["cells"]["Mood"], "calm");
    assert!(row["cells"]["Media Duration"].as_str().unwrap().contains(':'));
}

#[test]
fn sorting_by_a_column_and_reversing() {
    let mut s = demo();
    let bin = bin_of(&s, media(&s)[0]);
    let by_name = names(&s.execute("project.items", json!({"bin": bin})).unwrap());
    let mut sorted = by_name.clone();
    sorted.sort_by_key(|n| n.to_lowercase());
    assert!(by_name.len() > 2);
    assert_eq!(by_name, sorted, "Name ascending by default");
    // the same column again reverses
    s.execute("project.sort", json!({"column": "name"})).unwrap();
    assert!(s.prefs.project_panel.view.sort.descending);
    let rev = names(&s.execute("project.items", json!({"bin": bin})).unwrap());
    assert_eq!(rev, sorted.iter().rev().cloned().collect::<Vec<_>>());
    // durations sort numerically, not as text
    s.execute("project.sort", json!({"column": "Media Duration", "descending": false})).unwrap();
    let rows = s.execute("project.items", json!({"bin": bin})).unwrap();
    let durs: Vec<i64> =
        rows["rows"].as_array().unwrap().iter().filter_map(|r| r["item"].as_u64()).map(|i| s.project.item(ItemId(i)).unwrap().duration().0).collect();
    assert!(durs.windows(2).all(|w| w[0] <= w[1]), "{durs:?}");
    assert!(s.execute("project.sort", json!({"column": "Nope"})).is_err());
}

#[test]
fn view_presets_save_restore_and_manage() {
    let mut s = demo();
    assert!(s.execute("project.viewPreset.save", json!({})).is_err(), "no current preset yet");
    assert!(s.execute("project.viewPreset.restore", json!({"slot": 1})).is_err(), "none saved");
    s.execute("project.view.set", json!({"view": "list"})).unwrap();
    s.execute("project.columns.set", json!({"columns": ["Name", "Label"]})).unwrap();
    let r = s.execute("project.viewPreset.saveAs", json!({"name": "Logging"})).unwrap();
    assert_eq!(r["current"], 1);
    assert_eq!(r["presets"][0]["name"], "Logging");
    // change the view, then restore preset 1
    s.execute("project.view.set", json!({"view": "freeform"})).unwrap();
    s.execute("project.columns.set", json!({"columns": ["Name", "Frame Rate", "Video Info"]})).unwrap();
    s.execute("project.viewPreset.saveAs", json!({})).unwrap();
    assert_eq!(s.prefs.project_panel.presets[1].as_ref().unwrap().name, "Project View Preset 2");
    s.execute("project.viewPreset.restore", json!({"slot": 1})).unwrap();
    assert_eq!(s.prefs.project_panel.view.mode, ViewMode::List);
    assert_eq!(s.prefs.project_panel.view.columns.len(), 2);
    // Save Current View Preset overwrites the current slot
    s.execute("project.view.set", json!({"fontSize": "small"})).unwrap();
    s.execute("project.viewPreset.save", json!({})).unwrap();
    assert_eq!(s.prefs.project_panel.presets[0].as_ref().unwrap().settings.font_size, FontSize::Small);
    assert_eq!(s.prefs.project_panel.presets[0].as_ref().unwrap().name, "Logging");
    // Manage: rename and delete
    s.execute("project.viewPreset.rename", json!({"slot": 2, "name": "Grid"})).unwrap();
    assert_eq!(s.prefs.project_panel.presets[1].as_ref().unwrap().name, "Grid");
    s.execute("project.viewPreset.delete", json!({"slot": 1})).unwrap();
    assert!(s.prefs.project_panel.presets[0].is_none());
    assert_eq!(s.prefs.project_panel.current_preset, None);
    assert!(s.execute("project.viewPreset.restore", json!({"slot": 11})).is_err());
    // slots fill up
    for _ in 0..9 {
        s.execute("project.viewPreset.saveAs", json!({})).unwrap();
    }
    assert!(s.execute("project.viewPreset.saveAs", json!({})).is_err(), "all ten used");
}

#[test]
fn view_presets_persist_in_the_preferences_file() {
    let dir = std::env::temp_dir().join(format!("fc-viewpresets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("preferences.json");
    let mut s = demo();
    s.prefs_path = Some(path.clone());
    s.execute("project.columns.set", json!({"columns": ["Name", "Scene", "Shot"]})).unwrap();
    s.execute("project.viewPreset.saveAs", json!({"name": "Scenes"})).unwrap();
    let loaded = crate::autosave::Preferences::load(&path);
    assert_eq!(loaded.project_panel.presets[0].as_ref().unwrap().name, "Scenes");
    assert_eq!(loaded.project_panel.view.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Name", "Scene", "Shot"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn freeform_positions_are_undoable_and_saved_with_the_project() {
    let mut s = demo();
    let ids = media(&s);
    let bin = bin_of(&s, ids[0]);
    let lay = s.execute("project.freeform.layout", json!({"bin": bin, "width": 600})).unwrap();
    let cards = lay["cards"].as_array().unwrap();
    assert!(cards.len() >= ids.len());
    assert!(cards.iter().all(|c| c["placed"] == false), "nothing placed yet: auto grid");
    // moving two cards keeps their offsets
    let a = cards.iter().find(|c| c["item"] == json!(ids[0].0)).unwrap().clone();
    let b = cards.iter().find(|c| c["item"] == json!(ids[1].0)).unwrap().clone();
    let dx = b["x"].as_f64().unwrap() - a["x"].as_f64().unwrap();
    s.execute("project.freeform.move", json!({"items": [ids[0].0, ids[1].0], "x": 300, "y": 200, "width": 600})).unwrap();
    let pos = |s: &Session, i: ItemId| project_panel::parse_pos(s.project.item(i).unwrap().metadata.get(FREEFORM_POS).unwrap()).unwrap();
    assert_eq!((pos(&s, ids[0]).0, pos(&s, ids[0]).1), (300.0, 200.0));
    assert_eq!(pos(&s, ids[1]).0, (300.0 + dx as f32).round());
    // placed cards come first; the rest go on the grid below them
    let lay = s.execute("project.freeform.layout", json!({"bin": bin, "width": 600})).unwrap();
    let unplaced: Vec<&Value> = lay["cards"].as_array().unwrap().iter().filter(|c| c["placed"] == false).collect();
    assert!(unplaced.iter().all(|c| c["y"].as_f64().unwrap() > 200.0));
    // undo puts the card back on the grid
    s.execute("edit.undo", json!({})).unwrap();
    assert!(!s.project.item(ids[0]).unwrap().metadata.contains_key(FREEFORM_POS));
    s.execute("edit.redo", json!({})).unwrap();
    // snapping and Align to Grid
    s.execute("project.freeform.options", json!({"grid": 50, "snap": true})).unwrap();
    s.execute("project.freeform.move", json!({"items": [ids[2].0], "x": 123, "y": 77})).unwrap();
    assert_eq!((pos(&s, ids[2]).0, pos(&s, ids[2]).1), (100.0, 100.0));
    s.execute("project.freeform.options", json!({"snap": false})).unwrap();
    s.execute("project.freeform.move", json!({"items": [ids[2].0], "x": 133, "y": 77})).unwrap();
    s.execute("project.freeform.alignToGrid", json!({"bin": bin})).unwrap();
    assert_eq!((pos(&s, ids[2]).0, pos(&s, ids[2]).1), (150.0, 100.0));
    assert!(ids.iter().all(|i| s.project.item(*i).unwrap().metadata.contains_key(FREEFORM_POS)), "align pins every card");
    // Clip Size
    s.execute("project.freeform.resize", json!({"items": [ids[0].0], "size": 200})).unwrap();
    assert_eq!(pos(&s, ids[0]).2, Some(200.0));
    s.execute("project.freeform.resize", json!({"items": [ids[0].0], "step": 1})).unwrap();
    assert_eq!(pos(&s, ids[0]).2, Some(250.0));
    // saved with the project (item metadata) and back
    let text = serde_json::to_string(&*s.project).unwrap();
    let p2: filmcraft_project::Project = serde_json::from_str(&text).unwrap();
    assert_eq!(p2.item(ids[0]).unwrap().metadata.get(FREEFORM_POS), s.project.item(ids[0]).unwrap().metadata.get(FREEFORM_POS));
    // the Metadata panel doesn't list the bookkeeping keys
    let m = s.execute("metadata.get", json!({"item": ids[0].0})).unwrap();
    assert!(!m["fields"].as_array().unwrap().iter().any(|f| f["name"] == FREEFORM_POS));
    // Reset to Grid
    s.execute("project.freeform.reset", json!({"bin": bin})).unwrap();
    assert!(s.project.items.values().all(|i| !i.metadata.contains_key(FREEFORM_POS)));
}

#[test]
fn freeform_stacks_and_arrangements() {
    let mut s = demo();
    let ids = media(&s);
    assert!(s.execute("project.freeform.stack", json!({"items": [ids[0].0]})).is_err(), "needs two");
    s.execute("project.freeform.move", json!({"items": [ids[0].0], "x": 400, "y": 300})).unwrap();
    s.execute("project.freeform.stack", json!({"items": [ids[0].0, ids[1].0, ids[2].0]})).unwrap();
    let bin = bin_of(&s, ids[0]);
    let lay = s.execute("project.freeform.layout", json!({"bin": bin})).unwrap();
    let card = |i: ItemId| lay["cards"].as_array().unwrap().iter().find(|c| c["item"] == json!(i.0)).unwrap().clone();
    assert_eq!(card(ids[1])["stack"], json!(ids[0].0));
    // stacked cards sit on the first card, offset
    assert!((card(ids[1])["x"].as_f64().unwrap() - 406.0).abs() < 0.01);
    assert!((card(ids[2])["y"].as_f64().unwrap() - 312.0).abs() < 0.01);
    // one undo step
    let n = s.history.undo.len();
    s.execute("project.freeform.unstack", json!({"items": [ids[0].0]})).unwrap();
    assert_eq!(s.history.undo.len(), n + 1);
    assert!(ids[..3].iter().all(|i| !s.project.item(*i).unwrap().metadata.contains_key(FREEFORM_STACK)), "unstacking the first card releases all");
    let lay = s.execute("project.freeform.layout", json!({"bin": bin})).unwrap();
    let xs: Vec<f64> =
        ids[..3].iter().map(|i| lay["cards"].as_array().unwrap().iter().find(|c| c["item"] == json!(i.0)).unwrap()["x"].as_f64().unwrap()).collect();
    assert!(xs[0] < xs[1] && xs[1] < xs[2], "released cards are spread: {xs:?}");

    // arrangements
    s.execute("project.freeform.saveArrangement", json!({"name": "Rough", "bin": bin})).unwrap();
    s.execute("project.freeform.move", json!({"items": [ids[0].0], "x": 20, "y": 20})).unwrap();
    assert_eq!(s.execute("project.freeform.arrangements", json!({"bin": bin})).unwrap()["arrangements"], json!(["Rough"]));
    s.execute("project.freeform.restoreArrangement", json!({"name": "Rough", "bin": bin})).unwrap();
    let x0 = project_panel::parse_pos(s.project.item(ids[0]).unwrap().metadata.get(FREEFORM_POS).unwrap()).unwrap().0;
    assert_eq!(x0, 400.0);
    assert!(s.execute("project.freeform.restoreArrangement", json!({"name": "Nope", "bin": bin})).is_err());
    s.execute("project.freeform.deleteArrangement", json!({"name": "Rough", "bin": bin})).unwrap();
    assert_eq!(s.execute("project.freeform.arrangements", json!({"bin": bin})).unwrap()["arrangements"], json!([]));
}

#[test]
fn bins_rename_nest_and_list() {
    let mut s = demo();
    let b = s.execute("file.newBin", json!({"name": "Footage"})).unwrap()["bin"].as_u64().unwrap();
    let inner = s.execute("file.newBin", json!({"name": "Day 1", "parent": b})).unwrap()["bin"].as_u64().unwrap();
    let ids = media(&s);
    s.execute("project.moveToBin", json!({"items": [ids[0].0], "bin": inner})).unwrap();
    s.execute("project.renameBin", json!({"bin": b, "name": "Camera"})).unwrap();
    let rows = s.execute("project.items", json!({"recursive": true})).unwrap();
    let r = rows["rows"].as_array().unwrap();
    let cam = r.iter().position(|x| x["bin"] == json!(b)).unwrap();
    assert_eq!(r[cam]["name"], "Camera");
    assert_eq!(r[cam + 1]["bin"], json!(inner));
    assert_eq!(r[cam + 2]["item"], json!(ids[0].0));
    assert_eq!(r[cam + 2]["depth"], 2);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.root.find_bin(filmcraft_project::BinId(b)).unwrap().name, "Footage");
    assert!(s.execute("project.renameBin", json!({"bin": b, "name": "  "})).is_err());
    // only the bin's rows without `recursive`
    let rows = s.execute("project.items", json!({"bin": inner})).unwrap();
    assert_eq!(names(&rows).len(), 1);
}

#[test]
fn usage_counts_sequence_uses() {
    let s = demo();
    let used = s
        .project
        .sequences()
        .flat_map(|q| q.as_sequence().into_iter().flat_map(|q| q.all_tracks()).flat_map(|t| t.items.iter().map(|i| i.item)).collect::<Vec<_>>())
        .next()
        .expect("the demo sequence uses an item");
    let (v, a) = project_panel::usage(&s.project, used);
    assert!(v + a > 0);
    let it = s.project.item(used).unwrap();
    let (text, _) = project_panel::cell(&s.project, it, if v > 0 { "Video Usage" } else { "Audio Usage" });
    assert_eq!(text, (if v > 0 { v } else { a }).to_string());
    assert!(!matches!(it.kind, ItemKind::Graphic { .. }));
}

#[test]
fn select_all_selects_every_project_item() {
    let mut s = demo();
    assert!(s.project.items.len() >= 3);
    s.state.project_selection.clear();
    let r = s.execute("project.selectAll", json!({})).unwrap();
    assert_eq!(r["selected"].as_u64(), Some(s.project.items.len() as u64));
    assert_eq!(s.state.project_selection.len(), s.project.items.len());
    assert!(s.project.items.keys().all(|i| s.state.project_selection.contains(i)));
    s.execute("project.deselectAll", json!({})).unwrap();
    assert!(s.state.project_selection.is_empty());
    // the timeline Select All still selects timeline clips and leaves the Project panel alone
    s.execute("project.selectAll", json!({})).unwrap();
    let before = s.state.project_selection.clone();
    s.execute("edit.selectAll", json!({})).unwrap();
    assert!(!s.state.selection.is_empty());
    assert_eq!(s.state.project_selection, before);
}

#[test]
fn select_all_in_a_bin_skips_closed_bins_and_other_bins() {
    // #456: Cmd+A in the Project panel selected every item in the project, closed bins included
    let mut s = demo();
    let outer = s.execute("file.newBin", json!({"name": "Footage A"})).unwrap()["bin"].as_u64().unwrap();
    let inner = s.execute("file.newBin", json!({"name": "Day 1", "parent": outer})).unwrap()["bin"].as_u64().unwrap();
    let ids = media(&s);
    assert!(ids.len() >= 2);
    s.execute("project.moveToBin", json!({"items": [ids[0].0], "bin": outer})).unwrap();
    s.execute("project.moveToBin", json!({"items": [ids[1].0], "bin": inner})).unwrap();
    s.state.project_selection.clear();

    // only the bin's own items: the closed sub-bin's item stays unselected
    let r = s.execute("project.selectAll", json!({"bin": outer})).unwrap();
    assert_eq!(r["selected"].as_u64(), Some(1));
    assert_eq!(s.state.project_selection, vec![ids[0]]);

    // a sub-bin twirled open in List view counts
    s.execute("project.selectAll", json!({"bin": outer, "expanded": [inner]})).unwrap();
    let mut sel = s.state.project_selection.clone();
    sel.sort();
    assert_eq!(sel, vec![ids[0], ids[1]]);

    // the root: its direct items only, nothing from the (closed) bins
    let root = s.project.root.id;
    let mut want: Vec<ItemId> = s
        .project
        .root
        .children
        .iter()
        .filter_map(|e| if let BinEntry::Item(i) = e { Some(*i) } else { None })
        .filter(|i| s.project.item(*i).is_some_and(|it| project_panel::listed(&s.project, it)))
        .collect();
    want.sort();
    s.execute("project.selectAll", json!({"bin": root.0})).unwrap();
    let mut sel = s.state.project_selection.clone();
    sel.sort();
    assert_eq!(sel, want);
    assert!(!sel.contains(&ids[0]) && !sel.contains(&ids[1]));
    assert!(sel.len() < s.project.items.len());

    // an open bin inside a closed one is not shown, so it does not count either
    s.execute("project.selectAll", json!({"bin": root.0, "expanded": [inner]})).unwrap();
    assert!(!s.state.project_selection.contains(&ids[1]));
    // ... but open all the way down, it does
    s.execute("project.selectAll", json!({"bin": root.0, "expanded": [outer, inner]})).unwrap();
    assert!(s.state.project_selection.contains(&ids[0]) && s.state.project_selection.contains(&ids[1]));
}

#[test]
fn select_all_rejects_bad_bins_without_touching_the_selection() {
    let mut s = demo();
    let ids = media(&s);
    let root = s.project.root.id.0;
    s.execute("project.select", json!({"items": [ids[0].0]})).unwrap();
    for p in [
        json!({"bin": 987_654_321u64}),
        json!({"bin": "footage"}),
        json!({"bin": -1}),
        json!({"bin": root, "expanded": "all"}),
        json!({"bin": root, "expanded": ["x"]}),
    ] {
        assert!(s.execute("project.selectAll", p.clone()).is_err(), "{p}");
        assert_eq!(s.state.project_selection, vec![ids[0]], "{p}");
    }
    // unknown ids in `expanded` are simply not open bins
    assert!(s.execute("project.selectAll", json!({"bin": root, "expanded": [987_654_321u64]})).is_ok());
    // `bin: null` is the same as no bin: the whole project
    let r = s.execute("project.selectAll", json!({"bin": null})).unwrap();
    assert_eq!(r["selected"].as_u64(), Some(s.project.items.len() as u64));
}
