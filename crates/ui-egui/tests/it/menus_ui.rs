//! Headless UI tests of the M3.11 menu long tail: Find, Create Search Bin (and its Project panel
//! rows), Project Settings, Scene Edit Detection, Simplify Sequence, Automate to Sequence,
//! Normalize Mix Track, Dynamic Audio Waveforms, Reveal Log Files and the System Compatibility
//! Report, all driven by automation id through the control channel.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::ItemId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(s).with_control(rx);
        // never hand paths to the real OS opener from a test
        app.hooks.open_path = Some(Box::new(|_, _| Ok(())));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn menu(&mut self, id: &str) -> Value {
        let r = self.ok("ui.menu.invoke", json!({"id": id}));
        self.frames(3);
        r
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn item_named(&mut self, name: &str) -> u64 {
        self.app().session.project.items.values().find(|i| i.name == name).unwrap().id.0
    }

    fn dialog_open(&mut self) -> bool {
        self.app().ui.extras.dialog.is_some()
    }
}

#[test]
fn menu_items_toggles_and_help() {
    let mut d = Driver::new();
    let menus = d.ok("ui.menu.list", json!({}));
    let find = |id: &str| menus.as_array().unwrap().iter().find(|m| m["id"] == id).cloned().unwrap_or_else(|| panic!("{id} not in menus"));
    assert_eq!(find("view.dynamicAudioWaveforms")["checked"], true);
    assert_eq!(find("help.systemCompatibilityReport")["path"], json!(["Help"]));
    assert_eq!(find("file.mediaProperties")["path"], json!(["File", "Get Media File Properties for"]));
    assert_eq!(find("clip.sceneEditDetection")["label"], "Scene Edit Detection…");
    // Dynamic Audio Waveforms toggles
    d.menu("view.dynamicAudioWaveforms");
    assert!(!d.app().ui.extras.dynamic_waveforms);
    let menus = d.ok("ui.menu.list", json!({}));
    assert_eq!(menus.as_array().unwrap().iter().find(|m| m["id"] == "view.dynamicAudioWaveforms").unwrap()["checked"], false);
    // System Compatibility Report
    assert_eq!(d.menu("help.systemCompatibilityReport")["dialog"], "systemReport");
    assert!(d.has("systemReport.ok"));
    d.click("systemReport.ok");
    assert!(!d.dialog_open());
    // Reveal Log Files writes this session's log and reveals it
    let r = d.menu("help.revealLogFiles");
    let path = r["path"].as_str().unwrap().to_string();
    assert!(std::fs::read_to_string(&path).unwrap().contains("\"os\""));
    assert_eq!(d.app().ui.extras.opened.last(), Some(&path));
    let _ = std::fs::remove_file(&path);
    // Import from Media Browser needs a Media Browser selection
    assert_eq!(d.call("ui.menu.invoke", json!({"id": "file.importFromMediaBrowser"}))["ok"], false);
}

/// #2623: the engine's on/off commands in the Sequence and Markers menus show their state, so
/// choosing one visibly turns it on or off.
#[test]
fn engine_toggles_show_their_checkmark() {
    let mut d = Driver::new();
    let checked = |d: &mut Driver, id: &str| {
        let menus = d.ok("ui.menu.list", json!({}));
        menus.as_array().unwrap().iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("{id} not in menus"))["checked"].clone()
    };
    for (id, on) in [
        ("sequence.snap", true),
        ("sequence.linkedSelection", true),
        ("sequence.selectionFollowsPlayhead", false),
        ("sequence.showThroughEdits", false),
        ("markers.rippleSequenceMarkers", false),
        ("markers.copyPasteIncludesSequenceMarkers", false),
    ] {
        assert_eq!(checked(&mut d, id), json!(on), "{id} before");
        d.menu(id);
        assert_eq!(checked(&mut d, id), json!(!on), "{id} after choosing it");
    }
    assert!(d.app().session.state.ripple_sequence_markers);
    d.menu("markers.rippleSequenceMarkers");
    assert_eq!(checked(&mut d, "markers.rippleSequenceMarkers"), json!(false));
    assert!(!d.app().session.state.ripple_sequence_markers);
}

#[test]
fn import_image_sequence_from_the_file_menu_and_media_browser() {
    let dir = std::env::temp_dir().join(format!("fc-ui-imgseq-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let paths: Vec<String> = (1..=5)
        .map(|n| {
            let p = dir.join(format!("plate_{n:03}.png"));
            image::RgbaImage::from_pixel(16, 9, image::Rgba([n as u8 * 40, 0, 0, 255])).save(&p).unwrap();
            p.to_string_lossy().to_string()
        })
        .collect();
    let mut d = Driver::new();
    let menus = d.ok("ui.menu.list", json!({}));
    let item = menus.as_array().unwrap().iter().find(|m| m["id"] == "file.importImageSequence").cloned().expect("File ▸ Import Image Sequence…");
    assert_eq!(item["path"], json!(["File"]));
    // File ▸ Import Image Sequence… with the first frame chosen
    let r = d.ok("ui.menu.invoke", json!({"id": "file.importImageSequence", "params": {"path": paths[0]}}));
    assert_eq!(r["imageSequences"][0]["frames"], 5);
    let id = ItemId(r["items"][0].as_u64().unwrap());
    let kind = d.app().session.project.item(id).unwrap().as_media().unwrap().info.kind;
    assert_eq!(kind, filmcraft_media::MediaKind::ImageSequence);
    // Media Browser ▸ Import as Image Sequence (the selection, from frame 3)
    d.exec("mediaBrowser.select", json!({"paths": [paths[2]]}));
    let r = d.ok("ui.menu.invoke", json!({"id": "file.importFromMediaBrowser", "params": {"imageSequence": true}}));
    assert_eq!(r["imageSequences"][0]["frames"], 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn find_dialog_and_search_bins() {
    let mut d = Driver::new();
    d.ok("ui.set", json!({"focused": "Project"}));
    assert_eq!(d.menu("edit.find")["dialog"], "find");
    for id in
        ["find.scope.project", "find.row.0.column", "find.row.0.operator", "find.row.0.text", "find.row.1.text", "find.match.any", "find.ok", "find.cancel"]
    {
        assert!(d.has(id), "{id}");
    }
    d.ok(
        "ui.set",
        json!({"menuDialog": {"rows": [{"column": "Name", "operator": "beginsWith", "text": "Neon"}, {"column": "Name", "operator": "contains", "text": ""}]}}),
    );
    d.click("find.ok");
    let neon = d.item_named("Neon_Loop.mov");
    assert_eq!(d.app().session.state.project_selection, vec![ItemId(neon)]);
    assert!(d.dialog_open(), "Find stays open until Done");
    d.click("find.cancel");
    assert!(!d.dialog_open());
    // timeline Find from the Timeline panel
    d.ok("ui.set", json!({"focused": "Timeline"}));
    d.menu("edit.find");
    assert_eq!(d.app().ui.extras.dialog.as_ref().unwrap().params["scope"], "timeline");
    d.ok("ui.set", json!({"menuDialog": {"rows": [{"column": "Name", "operator": "contains", "text": "forest"}]}}));
    d.click("find.ok");
    assert_eq!(d.app().session.state.selection.len(), 1);
    d.click("find.cancel");

    // Create Search Bin, shown in the Project panel's list view with its live contents
    d.exec("project.view.set", json!({"view": "list"}));
    d.ok("ui.panel.show", json!({"panel": "Project"}));
    d.menu("file.newSearchBin");
    assert!(d.has("searchBin.text") && d.has("searchBin.column"));
    d.ok("ui.set", json!({"menuDialog": {"text": "ocean", "name": "Ocean shots"}}));
    d.click("searchBin.ok");
    let bin = d.app().session.project.search_bins[0].id.0;
    d.frames(3);
    assert!(d.has(&format!("project.searchBin.{bin}")));
    d.click(&format!("project.searchBin.{bin}"));
    let ocean = d.item_named("Ocean_Sunset.mp4");
    assert!(d.app().ui.expanded_bins.contains(&bin));
    let items = d.ok("ui.elements", json!({"prefix": format!("project.item.{ocean}")}));
    assert!(!items.as_array().unwrap().is_empty(), "listed under the expanded search bin: {items}");
}

#[test]
fn project_settings_dialog() {
    let mut d = Driver::new();
    assert_eq!(d.menu("file.projectSettings.scratchDisks")["dialog"], "projectSettings");
    assert_eq!(d.app().ui.extras.dialog.as_ref().unwrap().params["tab"], "scratchDisks");
    for id in ["projectSettings.scratch.captured", "projectSettings.scratch.videoPreviews.browse", "projectSettings.scratch.autoSave.same"] {
        assert!(d.has(id), "{id}");
    }
    d.click("projectSettings.tab.general");
    for id in ["projectSettings.renderer", "projectSettings.videoDisplay", "projectSettings.titleSafe.horizontal", "projectSettings.captureFormat"] {
        assert!(d.has(id), "{id}");
    }
    d.ok(
        "ui.set",
        json!({"menuDialog": {"titleSafe": [15.0, 12.0], "renderer": filmcraft_engine::project_tools::RENDERER_SOFTWARE, "captured": "/tmp/fc-captured"}}),
    );
    d.click("projectSettings.tab.ingest");
    assert!(d.has("projectSettings.ingest.enabled"));
    d.click("projectSettings.ok");
    assert!(!d.dialog_open());
    let st = d.app().session.project.settings.clone();
    assert_eq!(st.title_safe, (15.0, 12.0));
    assert_eq!(st.renderer, filmcraft_engine::project_tools::RENDERER_SOFTWARE);
    assert_eq!(st.scratch.captured.as_deref(), Some("/tmp/fc-captured"));
    assert!(filmcraft_ui_egui::panels::menu_dialogs::software_renderer(d.app()));
}

#[test]
fn clip_and_sequence_dialogs() {
    let mut d = Driver::new();
    // Scene Edit Detection needs a selected video clip
    assert_eq!(d.call("ui.menu.invoke", json!({"id": "clip.sceneEditDetection"}))["ok"], false);
    let c = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.exec("timeline.select", json!({"clips": [c]}));
    d.menu("clip.sceneEditDetection");
    for id in ["sceneDetect.applyCuts", "sceneDetect.createSubclips", "sceneDetect.generateMarkers", "sceneDetect.sensitivity", "sceneDetect.ok"] {
        assert!(d.has(id), "{id}");
    }
    d.click("sceneDetect.cancel");
    assert!(!d.dialog_open());
    // Normalize Mix Track
    d.menu("sequence.normalizeMixTrack");
    d.ok("ui.set", json!({"menuDialog": {"db": -6.0}}));
    d.click("normalizeMix.ok");
    assert!(!d.dialog_open());
    assert!(d.app().session.active_sequence().unwrap().master_volume_db != 0.0);
    // Simplify Sequence
    let n = d.app().session.project.items.len();
    d.menu("sequence.simplify");
    assert!(d.has("simplify.keep.audio") && d.has("simplify.closeGaps"));
    d.click("simplify.closeGaps");
    d.click("simplify.ok");
    assert_eq!(d.app().session.project.items.len(), n + 1);
    let active = d.app().session.state.active_sequence.unwrap();
    assert_eq!(d.app().session.project.item(active).unwrap().name, "Main Edit (Simplified)");
    // Automate to Sequence into a new sequence
    d.exec("file.newSequence", json!({"name": "Auto"}));
    let ids: Vec<u64> = ["Ocean_Sunset.mp4", "Misty_Forest.mp4"].iter().map(|n| d.item_named(n)).collect();
    d.exec("project.select", json!({"items": ids}));
    assert_eq!(d.menu("clip.automateToSequence")["dialog"], "automate");
    d.click("automate.method.overwrite");
    d.click("automate.ordering.selection");
    d.click("automate.ok");
    assert!(!d.dialog_open());
    let q = d.app().session.active_sequence().unwrap().clone();
    assert_eq!(q.video_tracks[0].items.iter().map(|i| i.item.0).collect::<Vec<_>>(), ids);
    // Get Media File Properties for ▸ Selection shows the probe info
    d.exec("project.select", json!({"items": [ids[0]]}));
    assert_eq!(d.menu("file.mediaProperties")["dialog"], "properties");
    assert_eq!(d.app().ui.extras.dialog.as_ref().unwrap().info[0]["video"]["width"], 1920);
    d.click("properties.ok");
    assert!(!d.dialog_open());
}

/// #29: File ▸ New ▸ Color Matte… asks for the color (and name, size, duration), and an existing
/// matte's color can be changed afterwards (double-click in the Project panel, or
/// `project.matteColor`), which used to leave every matte stuck at its first, dark grey color.
#[test]
fn color_matte_dialog_picks_and_changes_the_color() {
    let mut d = Driver::new();
    let matte_color = |d: &mut Driver, id: u64| match &d.app().session.project.item(ItemId(id)).unwrap().kind {
        filmcraft_engine::project::ItemKind::Media(m) => match &m.media {
            filmcraft_engine::project::MediaRef::Generator(filmcraft_media::Generator::ColorMatte { color }) => filmcraft_color::to_hex(*color),
            g => panic!("not a matte: {g:?}"),
        },
        _ => panic!("not media"),
    };
    assert_eq!(d.menu("file.newColorMatte")["dialog"], "colorMatte");
    for id in ["colorMatte.color", "colorMatte.name", "colorMatte.width", "colorMatte.height", "colorMatte.seconds", "colorMatte.ok"] {
        assert!(d.has(id), "{id}");
    }
    d.ok("ui.set", json!({"menuDialog": {"color": "#ff8000", "name": "Orange"}}));
    d.click("colorMatte.ok");
    assert!(!d.dialog_open());
    let orange = d.item_named("Orange");
    assert_eq!(matte_color(&mut d, orange), "#ff8000");
    // change it: the dialog starts from the current color
    d.exec("project.select", json!({"items": [orange]}));
    assert_eq!(d.menu("project.matteColor")["dialog"], "matteColor");
    assert_eq!(d.app().ui.extras.dialog.as_ref().unwrap().params["color"], "#ff8000");
    assert!(d.has("matteColor.color"));
    d.ok("ui.set", json!({"menuDialog": {"color": "#2040c0"}}));
    d.click("matteColor.ok");
    assert_eq!(matte_color(&mut d, orange), "#2040c0");
    // without a Color Matte selected there is nothing to change
    let seq = d.app().session.state.active_sequence.unwrap().0;
    d.exec("project.select", json!({"items": [seq]}));
    assert_eq!(d.call("ui.menu.invoke", json!({"id": "project.matteColor"}))["ok"], json!(false));
    assert!(!d.dialog_open());
}

#[test]
fn reveal_in_finder_opens_an_imported_clip() {
    let dir = std::env::temp_dir().join(format!("fc-ui-reveal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.png");
    image::RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255])).save(&path).unwrap();
    let mut d = Driver::new();
    let item = d.exec("file.import", json!({"paths": [path.to_string_lossy()]}))["items"][0].as_u64().unwrap();
    assert!(!d.app().session.is_enabled("file.reveal"));
    d.exec("project.select", json!({"items": [item]}));
    assert!(d.app().session.is_enabled("file.reveal"));
    let r = d.ok("ui.menu.invoke", json!({"id": "file.reveal"}));
    assert_eq!(r["paths"][0], path.to_string_lossy().as_ref());
    assert_eq!(d.app().ui.extras.opened.last().map(String::as_str), Some(path.to_string_lossy().as_ref()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A project saved for the first time is listed at the top of Home's Recent list and of File ▸
/// Open Recent, both of which reopen it; Clear Recent Files empties the list (#1461).
#[test]
fn saved_projects_are_listed_in_home_and_open_recent() {
    let dir = std::env::temp_dir().join(format!("fc-ui-recent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("first cut.fcproj").to_string_lossy().to_string();
    let b = dir.join("second cut.fcproj").to_string_lossy().to_string();
    let mut d = Driver::new();
    d.exec("file.saveAs", json!({"path": a}));
    d.exec("file.newProject", json!({"name": "Second"}));
    d.exec("file.saveAs", json!({"path": b}));
    let menus = d.ok("ui.menu.list", json!({}));
    let recent: Vec<&Value> = menus.as_array().unwrap().iter().filter(|m| m["path"] == json!(["File", "Open Recent"])).collect();
    let labels: Vec<&str> = recent.iter().map(|m| m["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["second cut.fcproj", "first cut.fcproj", "Clear Recent Files"]);
    assert_eq!(recent[0]["id"], "file.openRecent.0");
    // File ▸ Open Recent reopens the older one, which moves to the top
    d.menu("file.openRecent.1");
    assert_eq!(d.app().session.path.as_deref(), Some(a.as_str()));
    assert_eq!(d.app().session.prefs.general.recent_projects, [a.clone(), b.clone()]);
    // Home lists them too, newest first, and a click opens one
    d.click("header.home");
    let rows = d.ok("ui.elements", json!({"prefix": "import.recent."}));
    let rows: Vec<&str> = rows.as_array().unwrap().iter().map(|e| e["label"].as_str().unwrap()).collect();
    assert_eq!(rows, ["first cut.fcproj", "second cut.fcproj"]);
    d.click("import.recent.1");
    assert_eq!(d.app().session.path.as_deref(), Some(b.as_str()));
    // Clear Recent Files empties both lists
    d.menu("file.clearRecent");
    assert!(d.app().session.prefs.general.recent_projects.is_empty());
    let menus = d.ok("ui.menu.list", json!({}));
    let clear = menus.as_array().unwrap().iter().find(|m| m["id"] == "file.clearRecent").cloned().expect("Clear Recent Files");
    assert_eq!(clear["enabled"], false);
    assert!(!menus.as_array().unwrap().iter().any(|m| m["id"].as_str().is_some_and(|id| id.starts_with("file.openRecent."))));
    let _ = std::fs::remove_dir_all(&dir);
}
