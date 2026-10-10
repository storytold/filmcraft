//! Tests of the M3.11 File / Edit / Clip commands (`project_tools`).

use filmcraft_media::DemoScene;
use filmcraft_project::{ItemId, ItemKind};
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;
use crate::media_test_util::{make_movie, session_with, tmp_dir};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn item_named(s: &Session, name: &str) -> ItemId {
    s.project.items.values().find(|i| i.name == name).map(|i| i.id).unwrap_or_else(|| panic!("no item {name}"))
}

fn menu_ids(path: &[&str]) -> Vec<&'static str> {
    crate::commands::command_specs().iter().filter(|c| c.menu == path).map(|c| c.id).collect()
}

fn pos(ids: &[&str], id: &str) -> usize {
    ids.iter().position(|x| *x == id).unwrap_or_else(|| panic!("{id} not in {ids:?}"))
}

fn assert_order(ids: &[&str], order: &[&str]) {
    for w in order.windows(2) {
        assert!(pos(ids, w[0]) < pos(ids, w[1]), "{} before {}: {ids:?}", w[0], w[1]);
    }
}

#[test]
fn file_edit_clip_menu_order_and_shortcuts_match_premiere() {
    assert_order(&menu_ids(&["File", "New"]), &["file.newBin", "file.newBinFromSelection", "file.newSearchBin", "file.newOfflineFile"]);
    assert_order(
        &menu_ids(&["File"]),
        &[
            "file.open",
            "file.close",
            "file.closeProject",
            "file.closeAllProjects",
            "file.closeAllOtherProjects",
            "file.save",
            "file.saveCopy",
            "file.saveAsTemplate",
            "file.saveAll",
            "file.revert",
            "media.makeOffline",
            "file.importFromMediaBrowser",
            "file.import",
            "file.projectManager",
        ],
    );
    assert_order(&menu_ids(&["File", "Export"]), &["file.exportEdl", "file.exportSelectionProject", "file.exportAle", "file.exportOtio", "file.exportFcp7Xml"]);
    assert_eq!(menu_ids(&["File", "Get Media File Properties for"]), vec!["file.mediaPropertiesFile", "file.mediaProperties"]);
    assert_eq!(menu_ids(&["File", "Project Settings"]), vec!["file.projectSettings.general", "file.projectSettings.scratchDisks", "project.ingestSettings"]);
    assert_order(&menu_ids(&["Edit"]), &["edit.deselectAll", "edit.find", "edit.findNext", "edit.consolidateDuplicates", "edit.editOriginal"]);
    let clip: Vec<&str> = crate::commands::command_specs().iter().filter(|c| c.menu.first() == Some(&"Clip")).map(|c| c.id).collect();
    assert_order(
        &clip,
        &[
            "clip.editSubclip",
            "clip.editOffline",
            "clip.sourceSettings",
            "clip.speedDuration",
            "clip.sceneEditDetection",
            "clip.replaceFromBin",
            "clip.restoreCaptionsFromSource",
            "clip.updateMetadata",
            "clip.generateAudioWaveform",
            "clip.automateToSequence",
        ],
    );
    for (id, label, sc) in [
        ("file.close", "Close", Some("Cmd+W")),
        ("file.importFromMediaBrowser", "Import from Media Browser", Some("Cmd+Alt+I")),
        ("file.mediaProperties", "Selection…", Some("Cmd+Shift+H")),
        ("file.exportSelectionProject", "Selection as FilmCraft Project…", None),
        ("file.exportAle", "Avid Log Exchange…", None),
        ("edit.find", "Find…", Some("Cmd+F")),
        ("edit.editOriginal", "Edit Original", Some("Cmd+E")),
        ("clip.sceneEditDetection", "Scene Edit Detection…", None),
        ("clip.automateToSequence", "Automate to Sequence…", None),
    ] {
        let c = crate::commands::find(id).unwrap();
        assert_eq!((c.label, c.shortcut), (label, sc), "{id}");
    }
    // the default shortcut set binds them (no conflicts with existing keys)
    let s = Session::default();
    assert_eq!(s.shortcuts.primary("edit.find").as_deref(), Some("Cmd+F"));
    assert_eq!(s.shortcuts.primary("edit.editOriginal").as_deref(), Some("Cmd+E"));
}

#[test]
fn search_bins_update_live() {
    let mut s = demo();
    let r = s.execute("file.newSearchBin", json!({"column": "Name", "operator": "contains", "text": "ocean"})).unwrap();
    let bin = r["bin"].as_u64().unwrap();
    assert_eq!(r["name"], "ocean");
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    assert_eq!(r["items"], json!([ocean.0]));
    // renaming another item to match adds it to the bin
    let dunes = item_named(&s, "Desert_Dunes.mp4");
    s.execute("clip.rename", json!({"item": dunes.0, "name": "Ocean Dunes"})).unwrap();
    let r = s.execute("project.searchBinItems", json!({"bin": bin})).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 2);
    // edit the query, rename, delete; all undoable
    s.execute("project.editSearchBin", json!({"bin": bin, "name": "Movies", "column": "Media Type", "operator": "matches", "text": "movie"})).unwrap();
    let r = s.execute("project.searchBinItems", json!({"bin": bin})).unwrap();
    assert_eq!(r["name"], "Movies");
    assert_eq!(r["items"].as_array().unwrap().len(), 6, "the six demo scenes: {r}");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.search_bins[0].query.rows[0].text, "ocean");
    s.execute("project.deleteSearchBin", json!({"bin": bin})).unwrap();
    assert!(s.project.search_bins.is_empty());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.search_bins.len(), 1);
    // saved with the project
    let back = filmcraft_format::decode(&filmcraft_format::encode(&s.project, false)).unwrap().project;
    assert_eq!(back.search_bins, s.project.search_bins);
    assert!(s.execute("file.newSearchBin", json!({"text": ""})).is_err());
    assert!(s.execute("file.newSearchBin", json!({"text": "x", "operator": "near"})).is_err());
}

#[test]
fn find_and_find_next_in_the_project_and_timeline() {
    let mut s = demo();
    assert!(!s.is_enabled("edit.findNext"));
    let r = s.execute("edit.find", json!({"column": "Name", "operator": "endsWith", "text": ".mp4"})).unwrap();
    let n = r["matches"].as_u64().unwrap();
    assert_eq!(n, 5, "{r}");
    let first = r["item"].as_u64().unwrap();
    assert_eq!(s.state.project_selection, vec![ItemId(first)]);
    let r2 = s.execute("edit.findNext", json!({})).unwrap();
    assert_ne!(r2["item"], r["item"]);
    for _ in 0..(n - 1) {
        s.execute("edit.findNext", json!({})).unwrap();
    }
    assert_eq!(s.state.project_selection, vec![ItemId(first)], "Find Next wraps around");
    // two rows, match any
    let r = s
        .execute(
            "edit.find",
            json!({"rows": [{"column": "Name", "operator": "beginsWith", "text": "Neon"}, {"column": "Label", "operator": "matches", "text": "Caribbean"}], "matchAll": false}),
        )
        .unwrap();
    assert_eq!(r["matches"], 2);
    assert!(s.execute("edit.find", json!({"text": "zzz"})).is_err());
    // timeline: clip names, then markers
    let r = s.execute("edit.find", json!({"scope": "timeline", "text": "city"})).unwrap();
    let c = filmcraft_project::ClipId(r["clip"].as_u64().unwrap());
    assert_eq!(s.state.selection, vec![c]);
    assert_eq!(s.playhead(), s.active_sequence().unwrap().find_item(c).unwrap().1.start);
    assert_eq!(r["matches"], 2, "video and audio clip: {r}");
    s.execute("markers.add", json!({"seconds": 3.0, "name": "Pickup line"})).unwrap();
    let r = s.execute("edit.find", json!({"scope": "timeline", "text": "pickup", "in": "markers"})).unwrap();
    assert!(r.get("marker").is_some());
    assert_eq!(s.playhead(), s.sequence_rate().snap(Tick::from_seconds_f64(3.0)));
}

#[test]
fn close_commands_and_templates() {
    let mut s = demo();
    let seq = s.state.active_sequence.unwrap();
    s.execute("file.close", json!({})).unwrap();
    assert!(!s.state.open_sequences.contains(&seq));
    assert!(!s.is_enabled("file.closeAllOtherProjects"));
    assert!(s.execute("file.closeAllOtherProjects", json!({})).is_err());
    // template round trip
    let dir = tmp_dir("templates");
    let path = dir.join("Doc Template.fcproj").to_string_lossy().to_string();
    let r = s.execute("file.saveAsTemplate", json!({"path": path, "name": "Doc Template"})).unwrap();
    assert_eq!(r["path"], path);
    let n = s.project.items.len();
    s.execute("file.newBin", json!({"name": "Unsaved"})).unwrap();
    assert!(s.execute("file.closeAllProjects", json!({})).is_err(), "dirty: needs force");
    s.execute("file.closeAllProjects", json!({"force": true})).unwrap();
    assert!(s.project.items.is_empty());
    s.execute("file.newProjectFromTemplate", json!({"template": path, "name": "Ep 2"})).unwrap();
    assert!(s.project.items.values().any(|i| i.name == "Main Edit"));
    assert_eq!(s.project.items.len(), n);
    assert_eq!(s.project.name, "Ep 2");
    assert!(s.path.is_none() && !s.is_dirty());
}

#[test]
fn export_selection_project_and_ale() {
    let mut s = demo();
    let main = s.state.active_sequence.unwrap();
    s.execute("project.select", json!({"items": [main.0]})).unwrap();
    let dir = tmp_dir("export-sel");
    let path = dir.join("Main Only.fcproj").to_string_lossy().to_string();
    let r = s.execute("file.exportSelectionProject", json!({"path": path})).unwrap();
    let p = filmcraft_format::decode(&std::fs::read(&path).unwrap()).unwrap().project;
    assert_eq!(p.name, "Main Only");
    assert!(p.item(main).is_some());
    // the six scenes used by Main Edit come along; Rough Cut, the score, bars and leader don't
    for n in ["Ocean_Sunset.mp4", "Neon_Loop.mov", "Misty_Forest.mp4"] {
        assert!(p.items.values().any(|i| i.name == n), "{n}");
    }
    for n in ["Rough Cut", "Bars and Tone", "Color Matte"] {
        assert!(!p.items.values().any(|i| i.name == n), "{n}");
    }
    assert_eq!(r["items"].as_array().unwrap().len(), p.items.len());
    // every bin left has something in it, and every kept item is still in a bin
    let mut listed = Vec::new();
    p.root.all_items(&mut listed);
    assert!(p.items.keys().all(|k| listed.contains(k) || matches!(p.item(*k).unwrap().kind, ItemKind::Graphic { .. })));
    // ALE of the selection, or of the whole project
    let ale = dir.join("log.ale").to_string_lossy().to_string();
    s.execute("project.select", json!({"items": []})).unwrap();
    let r = s.execute("file.exportAle", json!({"path": ale})).unwrap();
    let doc = filmcraft_interchange::ale::parse(&std::fs::read_to_string(&ale).unwrap()).unwrap();
    assert_eq!(r["clips"].as_u64().unwrap() as usize, doc.rows.len());
    assert!(doc.rows.len() >= 9, "{}", doc.rows.len());
    assert_eq!(doc.heading("FPS"), Some("23.976"));
    assert!(doc.rows.iter().any(|r| r[0] == "Ambient_Score.wav" && r[1] == "A1A2"));
}

#[test]
fn export_selection_project_of_a_bin_exports_its_contents() {
    let mut s = demo();
    let (footage_bin, bin_items) = s
        .project
        .root
        .children
        .iter()
        .find_map(|c| match c {
            filmcraft_project::BinEntry::Bin(b) if b.name == "Footage" => {
                let mut items = Vec::new();
                b.all_items(&mut items);
                Some((b.id.0, items))
            }
            _ => None,
        })
        .expect("Footage bin");
    assert_eq!(bin_items.len(), 6);

    // Selecting the bin (its id, not its items) must export the bin's contents, not an empty project.
    s.execute("project.select", json!({"items": [footage_bin]})).unwrap();
    let dir = tmp_dir("export-bin");
    let path = dir.join("Footage.fcproj").to_string_lossy().to_string();
    let r = s.execute("file.exportSelectionProject", json!({"path": path})).unwrap();
    let p = filmcraft_format::decode(&std::fs::read(&path).unwrap()).unwrap().project;
    assert_eq!(r["items"].as_array().unwrap().len(), p.items.len(), "reported items match the saved project");
    let reported: std::collections::BTreeSet<ItemId> = r["items"].as_array().unwrap().iter().map(|v| ItemId(v.as_u64().unwrap())).collect();
    assert_eq!(reported, bin_items.iter().copied().collect(), "the bin's items are the exported items");
    for n in ["Ocean_Sunset.mp4", "Neon_Loop.mov", "Misty_Forest.mp4"] {
        assert!(p.items.values().any(|i| i.name == n), "{n}");
    }
    assert!(!p.items.values().any(|i| i.name == "Ambient_Score.wav"), "the Audio bin stayed behind");
    let mut listed = Vec::new();
    p.root.all_items(&mut listed);
    assert!(p.items.keys().all(|k| listed.contains(k) || matches!(p.item(*k).unwrap().kind, ItemKind::Graphic { .. })));

    // A mixed bin + item request must not report the bin id, which names no item in the output.
    let explicit = dir.join("Mixed.fcproj").to_string_lossy().to_string();
    let r = s.execute("file.exportSelectionProject", json!({"items": [footage_bin, bin_items[0].0], "path": explicit})).unwrap();
    let p = filmcraft_format::decode(&std::fs::read(&explicit).unwrap()).unwrap().project;
    for v in r["items"].as_array().unwrap() {
        let id = ItemId(v.as_u64().unwrap());
        assert!(p.item(id).is_some(), "reported item {id:?} is in the output");
    }

    // An empty bin has nothing to export: an actionable error, no misleading empty file.
    let empty_bin = s.execute("file.newBin", json!({"name": "Empty"})).unwrap()["bin"].as_u64().unwrap();
    s.execute("project.select", json!({"items": [empty_bin]})).unwrap();
    let empty_path = dir.join("Empty.fcproj").to_string_lossy().to_string();
    assert!(s.execute("file.exportSelectionProject", json!({"path": empty_path})).is_err());
    assert!(!std::path::Path::new(&empty_path).exists(), "no empty project written");
}

#[test]
fn media_properties_settings_and_scratch_disks() {
    let dir = tmp_dir("props");
    let path = dir.join("clip.mp4");
    make_movie(&path, DemoScene::Forest, 64, 36, 12);
    let (mut s, items, _) = session_with(&[&path]);
    s.execute("project.select", json!({"items": [items[0].0]})).unwrap();
    let r = s.execute("file.mediaProperties", json!({})).unwrap();
    let v = &r[0];
    assert_eq!(v["video"]["width"], 64);
    assert_eq!(v["video"]["codec"].as_str().map(|c| c.to_ascii_lowercase().contains("264") || c.contains("avc")), Some(true), "{v}");
    assert_eq!(v["audio"]["sampleRate"], 48000);
    assert_eq!(v["path"], path.to_string_lossy().as_ref());
    let f = s.execute("file.mediaPropertiesFile", json!({"path": path.to_string_lossy()})).unwrap();
    assert_eq!(f["video"]["height"], 36);
    assert!(s.execute("file.mediaPropertiesFile", json!({"path": dir.join("missing.mp4").to_string_lossy()})).is_err());
    // Project Settings ▸ General
    let g = s.execute("file.projectSettings.general", json!({})).unwrap();
    assert_eq!(g["titleSafe"], json!([20.0, 20.0]));
    let n = s.history.undo.len();
    s.execute(
        "file.projectSettings.general",
        json!({"renderer": "software", "videoDisplay": "frames", "audioDisplay": "milliseconds", "titleSafe": [15, 12], "captureFormat": "HDV"}),
    )
    .unwrap();
    let st = &s.project.settings;
    assert_eq!(st.renderer, crate::project_tools::RENDERER_SOFTWARE);
    assert_eq!(st.video_display, filmcraft_time::TimeDisplay::Frames);
    assert!(!st.audio_display_samples);
    assert_eq!((st.title_safe, st.capture_format.as_str()), ((15.0, 12.0), "HDV"));
    assert_eq!(s.history.undo.len(), n + 1);
    assert!(s.execute("file.projectSettings.general", json!({"renderer": "quantum"})).is_err());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.settings.renderer, crate::project_tools::RENDERER_GPU);
    // Scratch Disks: previews follow the Video Previews folder once the project is saved
    let proj = dir.join("Show.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": proj})).unwrap();
    assert_eq!(crate::project_tools::previews_dir(&s).unwrap(), dir.join("FilmCraft Previews").join("Show"));
    let scratch = dir.join("scratch").to_string_lossy().to_string();
    let r = s.execute("file.projectSettings.scratchDisks", json!({"videoPreviews": scratch, "autoSave": scratch})).unwrap();
    assert_eq!(crate::project_tools::previews_dir(&s).unwrap(), dir.join("scratch").join("Show"));
    assert_eq!(r["videoPreviews"]["path"], dir.join("scratch").join("Show").to_string_lossy().as_ref());
    assert_eq!(r["autoSave"]["path"], scratch);
    assert_eq!(s.previews.dir(), Some(dir.join("scratch").join("Show")));
    s.execute("file.projectSettings.scratchDisks", json!({"videoPreviews": null})).unwrap();
    assert!(s.project.settings.scratch.video_previews.is_none());
    assert!(s.execute("file.projectSettings.scratchDisks", json!({"captured": 5})).is_err());
}

#[test]
fn edit_original_update_metadata_and_clip_items() {
    let dir = tmp_dir("orig");
    let path = dir.join("take.mp4");
    make_movie(&path, DemoScene::Aurora, 64, 36, 8);
    let (mut s, items, _) = session_with(&[&path]);
    s.execute("project.select", json!({"items": [items[0].0]})).unwrap();
    let r = s.execute("edit.editOriginal", json!({})).unwrap();
    assert_eq!(r["paths"], json!([path.to_string_lossy()]));
    // generator media has no original file
    let mut d = demo();
    let ocean = item_named(&d, "Ocean_Sunset.mp4");
    d.execute("project.select", json!({"items": [ocean.0]})).unwrap();
    assert!(!d.is_enabled("edit.editOriginal"));
    assert!(d.is_enabled("clip.sourceSettings"));
    let ss = d.execute("clip.sourceSettings", json!({})).unwrap();
    assert!(ss["message"].as_str().unwrap().contains("no source settings"));
    assert!(!d.is_enabled("clip.restoreCaptionsFromSource"));
    let w = d.execute("clip.generateAudioWaveform", json!({})).unwrap();
    assert_eq!(w["items"], json!([ocean.0]));
    // Update Metadata writes an XMP sidecar with the log fields
    let mut p = (*s.project).clone();
    let it = p.item_mut(items[0]).unwrap();
    it.metadata.insert("Scene".into(), "12A".into());
    it.metadata.insert("Log Note".into(), "best <take>".into());
    s.project = std::sync::Arc::new(p);
    let r = s.execute("clip.updateMetadata", json!({})).unwrap();
    let side = dir.join("take.xmp");
    assert_eq!(r["written"][0]["path"], side.to_string_lossy().as_ref());
    let x = std::fs::read_to_string(&side).unwrap();
    assert!(x.contains("<xmpDM:scene>12A</xmpDM:scene>") && x.contains("<xmpDM:logComment>best &lt;take&gt;</xmpDM:logComment>"), "{x}");
    assert!(x.contains("<rdf:li xml:lang=\"x-default\">take.mp4</rdf:li>"));
    // a sidecar from another application is left alone
    std::fs::write(&side, "<x:xmpmeta>someone else's</x:xmpmeta>").unwrap();
    let r = s.execute("clip.updateMetadata", json!({})).unwrap();
    assert!(r["written"].as_array().unwrap().is_empty() && r["skipped"].as_array().unwrap().len() == 1);
    assert!(std::fs::read_to_string(&side).unwrap().contains("someone else"));
}

#[test]
fn edit_offline_sets_the_log_fields() {
    let mut s = demo();
    let r = s.execute("file.newOfflineFile", json!({"name": "Missing Take", "seconds": 4.0})).unwrap();
    let id = ItemId(r["item"].as_u64().unwrap());
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    s.execute("project.select", json!({"items": [ocean.0]})).unwrap();
    assert!(!s.is_enabled("clip.editOffline"), "online clip");
    s.execute("project.select", json!({"items": [id.0]})).unwrap();
    assert!(s.is_enabled("clip.editOffline"));
    s.execute("clip.editOffline", json!({"mediaName": "Take 7", "tapeName": "A007", "scene": "4", "shot": "2", "logNote": "reshoot", "description": "wide"}))
        .unwrap();
    let it = s.project.item(id).unwrap();
    assert_eq!(it.name, "Take 7");
    assert_eq!(it.metadata.get("Tape Name").map(String::as_str), Some("A007"));
    assert_eq!(it.metadata.get("Log Note").map(String::as_str), Some("reshoot"));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.item(id).unwrap().name, "Missing Take");
}

#[test]
fn automate_to_sequence_places_clips_with_overlap_and_transitions() {
    let mut s = demo();
    s.execute("file.newSequence", json!({"name": "Auto", "fps": 23.976})).unwrap();
    let rate = s.sequence_rate();
    let names = ["Misty_Forest.mp4", "Ocean_Sunset.mp4", "Aurora_Timelapse.mp4"];
    let ids: Vec<ItemId> = names.iter().map(|n| item_named(&s, n)).collect();
    // short In/Out ranges: 3 s each
    for id in &ids {
        s.execute("project.setMarks", json!({"item": id.0, "in": 0, "out": rate.tick_of(71).0})).unwrap();
    }
    s.execute("project.select", json!({"items": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    assert!(s.is_enabled("clip.automateToSequence"));
    let n_undo = s.history.undo.len();
    let r = s.execute("clip.automateToSequence", json!({"ordering": "selection", "method": "overwrite", "overlapFrames": 12})).unwrap();
    assert_eq!(s.history.undo.len(), n_undo + 1, "one undo step");
    assert_eq!(r["placed"], 3);
    let q = s.active_sequence().unwrap();
    let v1 = &q.video_tracks[0].items;
    assert_eq!(v1.len(), 3);
    // selection order; each clip starts 12 frames before the previous one ends
    assert_eq!(v1.iter().map(|i| i.item).collect::<Vec<_>>(), ids);
    assert_eq!(v1[0].start, Tick::ZERO);
    assert_eq!(v1[1].start, rate.tick_of(72 - 12));
    assert_eq!(v1[2].start, rate.tick_of(2 * (72 - 12)));
    assert_eq!(r["transitions"], 4, "a video and an audio transition at each of the two cuts: {r}");
    assert_eq!(q.video_tracks[0].transitions.len(), 2);
    assert!(q.video_tracks[0].transitions.iter().all(|t| t.duration == rate.tick_of(12)));
    // sort order (bin order): Ocean, Aurora (bin order of the demo), Forest
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active_sequence().unwrap().video_tracks[0].items.is_empty());
    s.execute(
        "clip.automateToSequence",
        json!({"ordering": "sort", "overlapFrames": 0, "videoTransition": false, "audioTransition": false, "ignoreAudio": true}),
    )
    .unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items.iter().map(|i| i.item).collect::<Vec<_>>(), vec![ids[1], ids[2], ids[0]]);
    assert!(q.audio_tracks.iter().all(|t| t.items.is_empty()));
    assert!(q.video_tracks[0].transitions.is_empty());
    // insert at the playhead pushes what is there
    let end = q.video_tracks[0].items[2].end();
    s.set_playhead(Tick::ZERO);
    s.execute("project.select", json!({"items": [ids[0].0]})).unwrap();
    s.execute("clip.automateToSequence", json!({"method": "insert", "ignoreAudio": true})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items.len(), 4);
    assert_eq!(q.video_tracks[0].items[0].item, ids[0]);
    assert_eq!(q.video_tracks[0].items.last().unwrap().end(), end + rate.tick_of(72));
    // at unnumbered markers
    s.execute("file.newSequence", json!({"name": "Markers", "fps": 23.976})).unwrap();
    s.execute("markers.add", json!({"seconds": 10.0})).unwrap();
    s.execute("markers.add", json!({"seconds": 20.0})).unwrap();
    s.execute("project.select", json!({"items": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    let r = s.execute("clip.automateToSequence", json!({"placement": "unnumberedMarkers", "method": "overwrite", "ordering": "selection"})).unwrap();
    assert_eq!(r["placed"], 2, "two markers, two clips");
    let q = s.active_sequence().unwrap();
    let starts: Vec<Tick> = q.video_tracks[0].items.iter().map(|i| i.start).collect();
    assert_eq!(starts, vec![rate.snap(Tick::from_seconds_f64(10.0)), rate.snap(Tick::from_seconds_f64(20.0))]);
    // disabled without a Project panel selection
    s.execute("project.select", json!({"items": []})).unwrap();
    assert!(!s.is_enabled("clip.automateToSequence"));
}

#[test]
fn system_report_lists_the_build() {
    let s = &mut Session::default();
    let r = s.execute("help.systemReport", json!({})).unwrap();
    assert_eq!(r["os"], std::env::consts::OS);
    assert!(r["cpuThreads"].as_u64().unwrap() >= 1);
    assert!(r["decoders"].as_array().unwrap().iter().any(|d| d.as_str().unwrap().contains("H.264")));
    assert_eq!(r["exportFormats"].as_array().unwrap().len(), filmcraft_export::Format::ALL.len());
}

#[test]
fn clear_deletes_a_bin_with_everything_in_it() {
    let mut s = demo();
    let before = s.project.items.len();
    let outer = s.execute("file.newBin", json!({"name": "Old footage"})).unwrap()["bin"].as_u64().unwrap();
    let inner = s.execute("file.newBin", json!({"name": "Inside", "parent": outer})).unwrap()["bin"].as_u64().unwrap();
    let empty = s.execute("file.newBin", json!({"name": "Empty"})).unwrap()["bin"].as_u64().unwrap();
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    s.execute("project.moveToBin", json!({"items": [ocean.0], "bin": inner})).unwrap();
    let dunes = item_named(&s, "Desert_Dunes.mp4");
    let select = |s: &mut Session| s.execute("project.select", json!({"items": [dunes.0]})).unwrap();
    select(&mut s);
    // an empty bin, by its id: gone, nothing else touched
    let r = s.execute("project.delete", json!({"items": [empty]})).unwrap();
    assert_eq!(r, json!({"items": 0, "bins": 1}));
    assert!(s.project.root.find_bin(filmcraft_project::BinId(empty)).is_none());
    assert_eq!(s.project.items.len(), before);
    // a bin holding a sub-bin holding a clip: all three go
    select(&mut s);
    let r = s.execute("project.delete", json!({"items": [outer]})).unwrap();
    assert_eq!(r, json!({"items": 1, "bins": 1}));
    assert!(s.project.root.find_bin(filmcraft_project::BinId(outer)).is_none());
    assert!(s.project.root.find_bin(filmcraft_project::BinId(inner)).is_none());
    assert!(s.project.item(ocean).is_none());
    assert_eq!(s.project.items.len(), before - 1);
    // undo puts the bins and the clip back where they were
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.root.find_bin(filmcraft_project::BinId(inner)).is_some());
    assert_eq!(s.project.root.parent_of(ocean), Some(filmcraft_project::BinId(inner)));
    // the clip, its bin and the bin around it all named: each counted once
    select(&mut s);
    let r = s.execute("project.delete", json!({"items": [ocean.0, inner, outer]})).unwrap();
    assert_eq!(r, json!({"items": 1, "bins": 1}));
    assert_eq!(s.project.items.len(), before - 1);
    s.execute("edit.undo", json!({})).unwrap();
    // the project's own top bin is never cleared
    let root = s.project.root.id.0;
    select(&mut s);
    s.execute("project.delete", json!({"items": [root]})).unwrap();
    assert_eq!(s.project.items.len(), before);
}
