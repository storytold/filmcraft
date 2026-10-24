//! Headless UI tests of media management: the Link Media dialog opening for a project whose media
//! moved (search a folder, link, the others follow), Project panel offline / proxy badges, the
//! monitors' Toggle Proxies button, Create Proxies and Project Manager dialogs.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write PNGs
//! there (`media-*.png`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let d = std::env::temp_dir().join(format!("filmcraft-ui-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A 24 fps ProRes movie of a demo scene (our own encoder).
fn make_movie(path: &Path, scene: DemoScene, w: u32, h: u32, frames: i64) {
    let rate = FrameRate::FPS_24;
    let dur = rate.tick_of(frames);
    let src = GeneratorSource::new(Generator::Demo(scene), w, h, rate, dur);
    let clip = MediaClip {
        media: MediaRef::Generator(Generator::Demo(scene)),
        info: src.info().clone(),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let mut p = Project::new("fixture");
    let item = p.add_item("scene", Label::Iris, ItemKind::Media(clip), None);
    let seq = p.new_sequence("s", SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 1, 0, None);
    let mut v = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, dur), rate).unwrap();
    for e in &mut v.effects {
        filmcraft_engine::project::resolve_auto_points(e, (w, h), (w, h));
    }
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
    let settings = filmcraft_engine::export::ExportSettings {
        format: filmcraft_engine::export::Format::ProRes,
        path: path.to_string_lossy().into_owned(),
        include_audio: false,
        ..Default::default()
    };
    let shared: filmcraft_media::SharedSource = Arc::new(src);
    let provider = move |id| (id == item).then(|| shared.clone());
    filmcraft_engine::export::export(&Arc::new(p), seq, &settings, &provider, &Default::default()).unwrap();
}

/// `<root>/p.fcproj` with a sequence of `<root>/Media/{Harbour,Night}.mov`; the media folder is
/// then moved to `<root>/Moved/Media`.
fn moved_project(root: &Path) -> String {
    let media = root.join("Media");
    std::fs::create_dir_all(&media).unwrap();
    let files = [media.join("Harbour.mov"), media.join("Night.mov")];
    make_movie(&files[0], DemoScene::OceanSunset, 640, 360, 24);
    make_movie(&files[1], DemoScene::CityNight, 640, 360, 24);
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": files.iter().map(|f| f.to_string_lossy()).collect::<Vec<_>>()})).unwrap();
    let items: Vec<u64> = r["items"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    let rate = FrameRate::FPS_24;
    let mut p = (*s.project).clone();
    let seq = p.new_sequence("Harbour Cut", SequenceSettings { width: 640, height: 360, frame_rate: rate, ..Default::default() }, 1, 1, None);
    let mut at = Tick::ZERO;
    for it in items {
        let it = filmcraft_engine::project::ItemId(it);
        let d = rate.tick_of(24);
        let mut v = p.make_track_item(it, TrackKind::Video, at, TimeRange::new(Tick::ZERO, d), rate).unwrap();
        for e in &mut v.effects {
            filmcraft_engine::project::resolve_auto_points(e, (640, 360), (640, 360));
        }
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(v);
        at += d;
    }
    s.project = Arc::new(p);
    let path = root.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.save", json!({"path": path})).unwrap();
    drop(s); // Close decoder files before moving their directory on Windows.
    std::fs::create_dir_all(root.join("Moved")).unwrap();
    std::fs::rename(&media, root.join("Moved").join("Media")).unwrap();
    path
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<PathBuf>,
}

impl Driver {
    fn new(session: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
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

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(3);
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        img.save(&path).unwrap();
        eprintln!("snapshot: {}", path.display());
    }
}

/// #301: Source transport must step by frame index when frame durations need rounding.
#[test]
fn source_transport_steps_every_frame_at_a_fractional_tick_rate() {
    let rate = FrameRate::new(31, 1);
    let generator = Generator::Demo(DemoScene::OceanSunset);
    let src = GeneratorSource::new(generator.clone(), 32, 18, rate, rate.tick_of(41));
    let mut session = Session::default();
    let item = Arc::make_mut(&mut session.project).add_item(
        "31 fps source",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(generator),
            info: src.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    session.execute("source.open", json!({"item": item.0})).unwrap();
    let mut d = Driver::new(session);
    for frame in 1..=40 {
        d.click("source.transport.src.stepFwd");
        assert_eq!(rate.frame_at(d.app().session.state.source_playhead), frame);
    }
    d.click("source.transport.src.stepFwd");
    assert_eq!(rate.frame_at(d.app().session.state.source_playhead), 40, "stop at the last source frame");
    for frame in (0..40).rev() {
        d.click("source.transport.src.stepBack");
        assert_eq!(rate.frame_at(d.app().session.state.source_playhead), frame);
    }
    d.click("source.transport.src.stepBack");
    assert_eq!(d.app().session.state.source_playhead, Tick::ZERO);
}

/// Source-focused menu/keyboard commands use the same frame grid as the Source toolbar.
#[test]
fn source_menu_steps_use_ordinals_for_explicit_targets_and_focus() {
    let rate = FrameRate::new(31, 1);
    for explicit_source in [true, false] {
        let generator = Generator::Demo(DemoScene::OceanSunset);
        let src = GeneratorSource::new(generator.clone(), 32, 18, rate, rate.tick_of(41));
        let mut session = Session::default();
        session.execute("file.newSequence", json!({"fps": 31, "width": 32, "height": 18})).unwrap();
        session.execute("playhead.set", json!({"frame": 10})).unwrap();
        session.execute("prefs.set", json!({"key": "playback.stepManyFrames", "value": 12})).unwrap();
        let item = Arc::make_mut(&mut session.project).add_item(
            "31 fps menu source",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(generator),
                info: src.info().clone(),
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        session.execute("source.open", json!({"item": item.0})).unwrap();
        let mut d = Driver::new(session);
        d.ok("ui.set", json!({"focused": if explicit_source { "Program" } else { "Source" }}));
        let project = d.app().session.project.clone();
        let history = d.exec("history.list", json!({}));
        let mut source_metadata = d.exec("source.inspect", json!({}));
        source_metadata.as_object_mut().unwrap().remove("playhead");
        let source_params = if explicit_source { json!({"monitor": "source"}) } else { json!({}) };
        for (command, frame) in [
            ("playhead.stepForward", 1),
            ("playhead.stepForward", 2),
            ("playhead.stepBack", 1),
            ("playhead.stepBack", 0),
            ("playhead.stepBack", 0),
            ("playhead.stepForward5", 12),
            ("playhead.stepForward5", 24),
            ("playhead.stepForward5", 36),
            ("playhead.stepForward5", 40),
            ("playhead.stepForward5", 40),
            ("playhead.stepBack5", 28),
            ("playhead.stepBack5", 16),
            ("playhead.stepBack5", 4),
            ("playhead.stepBack5", 0),
            ("playhead.stepBack5", 0),
        ] {
            d.ok("ui.menu.invoke", json!({"id": command, "params": source_params.clone()}));
            let mut source = d.exec("source.inspect", json!({}));
            assert_eq!(source["playhead"], json!(rate.tick_of(frame).0), "{command}, explicit Source target: {explicit_source}");
            source.as_object_mut().unwrap().remove("playhead");
            assert_eq!(source, source_metadata);
            assert_eq!(d.app().session.playhead(), rate.tick_of(10));
            assert!(Arc::ptr_eq(&d.app().session.project, &project), "Source navigation must preserve the document");
            assert_eq!(d.exec("history.list", json!({})), history);
            let state = d.ok("ui.inspect", json!({}));
            assert_eq!(state["playback"]["playing"], json!(false));
            assert_eq!(state["sourcePlayback"]["playing"], json!(false));
        }
        // An explicit Program target takes priority even while Source has focus.
        for (command, frame) in [("playhead.stepForward", 11), ("playhead.stepBack", 10)] {
            d.ok("ui.menu.invoke", json!({"id": command, "params": {"monitor": "program"}}));
            assert_eq!(d.app().session.playhead(), rate.tick_of(frame));
            assert_eq!(d.app().session.state.source_playhead, Tick::ZERO);
            assert!(Arc::ptr_eq(&d.app().session.project, &project));
            assert_eq!(d.exec("history.list", json!({})), history);
        }
    }
}

// Requires the wide Time arithmetic: the accepted Source rate has more than one frame per tick.
fn zero_tick_frame_duration_source() -> (Driver, Tick) {
    let rate = FrameRate::new(i64::MAX, 1000);
    let current = Tick(254_270_016_000_000); // 1001 seconds, beyond the i64 frame-index range.
    assert_eq!(rate.frame_duration(), Tick::ZERO);
    assert_eq!(rate.frame_at(current), i64::MAX);
    assert_eq!(rate.tick_of(i64::MAX), Tick(254_016_000_000_000));
    assert_eq!(rate.snap(current), current, "the composed Time helper must retain the exact cursor");
    let generator = Generator::ColorMatte { color: [0.2, 0.8, 0.2, 1.0] };
    let src = GeneratorSource::new(generator.clone(), 32, 18, rate, Tick(508_032_000_000_000)); // 2000 seconds.
    let mut session = Session::default();
    session.execute("file.newSequence", json!({"fps": 31, "width": 32, "height": 18})).unwrap();
    let item = Arc::make_mut(&mut session.project).add_item(
        "Source with less than one tick per frame",
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(generator),
            info: src.info().clone(),
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    assert_eq!(filmcraft_engine::clip_ops::source_view(&session, item).unwrap().rate, rate);
    session.execute("source.open", json!({"item": item.0})).unwrap();
    let mut d = Driver::new(session);
    d.exec("source.setPlayhead", json!({"time": current.0}));
    assert_eq!(d.app().session.state.source_playhead, current, "the public Source seek must accept the exact starting cursor");
    (d, current)
}

#[test]
fn source_toolbar_steps_preserve_zero_tick_frame_duration_cursor() {
    let (mut d, current) = zero_tick_frame_duration_source();
    for button in ["source.transport.src.stepFwd", "source.transport.src.stepBack"] {
        d.click(button);
        assert_eq!(d.app().session.state.source_playhead, current, "{button} must retain the existing zero-duration no-op");
    }
}

#[test]
fn source_menu_steps_preserve_zero_tick_frame_duration_cursor() {
    for explicit_source in [true, false] {
        let (mut d, current) = zero_tick_frame_duration_source();
        d.ok("ui.set", json!({"focused": if explicit_source { "Program" } else { "Source" }}));
        let params = if explicit_source { json!({"monitor": "source"}) } else { json!({}) };
        for command in ["playhead.stepForward", "playhead.stepBack"] {
            d.ok("ui.menu.invoke", json!({"id": command, "params": params.clone()}));
            assert_eq!(d.app().session.state.source_playhead, current, "{command}, explicit Source target: {explicit_source}");
        }
    }
}

#[test]
fn link_media_large_lists_and_long_paths_keep_actions_on_screen() {
    for (width, height) in [(1600.0, 980.0), (1024.0, 768.0), (800.0, 600.0)] {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let mut p = (*s.project).clone();
        let template = p.items.values().find_map(|i| i.as_media()).unwrap().clone();
        for n in 0..100 {
            let mut media = template.clone();
            media.media = MediaRef::File { path: format!("/missing/{}/clip-{n}.mov", "long-project-directory/".repeat(20)) };
            media.offline = true;
            let id = p.add_item(&format!("Missing clip {n}"), Label::Iris, ItemKind::Media(media), None);
            s.offline.missing.push(id);
        }
        s.project = Arc::new(p);
        let mut app = FilmcraftApp::new(s);
        app.ui.link_media = Some(Default::default());
        let mut builder = Harness::builder().with_size(egui::vec2(width, height)).with_max_steps(10_000);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(PathBuf::from);
        if snapshots.is_some() {
            builder = builder.wgpu();
        }
        let mut h = builder.build_eframe(move |_| app);
        for _ in 0..8 {
            h.step();
        }
        for id in ["linkMedia.locate", "linkMedia.cancel", "linkMedia.offlineAll", "linkMedia.link"] {
            let r = h.state().auto.find(id).unwrap().rect;
            assert!(r[0] >= 0.0 && r[1] >= 0.0 && r[0] + r[2] <= width && r[1] + r[3] <= height, "{id} at {r:?} outside {width}x{height}");
        }
        if let Some(dir) = snapshots {
            std::fs::create_dir_all(&dir).unwrap();
            h.render().unwrap().save(dir.join(format!("media-long-paths-{width}x{height}.png"))).unwrap();
        }
    }
}

#[test]
fn link_media_dialog_relinks_moved_media() {
    let root = tmp_dir("link");
    let path = moved_project(&root);
    let mut s = Session::default();
    s.execute("file.open", json!({"path": path})).unwrap();
    let mut d = Driver::new(s);
    d.frames(6);
    // the dialog opened by itself, listing both clips; the Project panel marks them
    let rows = d.ids("linkMedia.row.");
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(!d.ids("linkMedia.locate").is_empty() && !d.ids("linkMedia.offlineAll").is_empty());
    d.snapshot("media-link-dialog");
    // search the moved folder for the selected clip
    let folder = root.join("Moved").to_string_lossy().into_owned();
    d.app().ui.link_media.as_mut().unwrap().folder = folder;
    d.click("linkMedia.search");
    assert_eq!(d.ids("linkMedia.candidate.").len(), 1);
    d.frames(4);
    d.snapshot("media-link-search");
    d.click("linkMedia.link");
    d.frames(4);
    assert!(d.app().ui.link_media.is_none(), "both clips relinked (the second by folder remap), dialog closed");
    let st = d.exec("media.status", json!({}));
    assert!(st.as_array().unwrap().iter().all(|m| m["status"] == "online"), "{st}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn offline_badges_proxy_toggle_and_dialogs() {
    let root = tmp_dir("badges");
    let path = moved_project(&root);
    let mut s = Session::default();
    s.execute("file.open", json!({"path": path})).unwrap();
    let mut d = Driver::new(s);
    d.frames(4);
    d.click("linkMedia.cancel");
    assert!(d.app().ui.link_media.is_none());
    // the Program monitor shows the offline slate; the project panel marks offline items
    d.exec("playhead.set", json!({"frame": 6}));
    d.frames(30);
    let offline_badges = d.ids("project.item.").into_iter().filter(|i| i.ends_with(".offline")).count();
    assert_eq!(offline_badges, 2, "icon-view offline markers");
    d.snapshot("media-offline-project");
    // relink by folder remap, create proxies through the dialog, toggle them from the monitor
    d.exec("media.autoRelink", json!({"from": root.join("Media").to_string_lossy(), "to": root.join("Moved/Media").to_string_lossy()}));
    let items: Vec<u64> = d.exec("media.status", json!({})).as_array().unwrap().iter().map(|m| m["item"].as_u64().unwrap()).collect();
    d.exec("project.select", json!({"items": items}));
    d.ok("ui.menu.invoke", json!({"id": "media.createProxies"}));
    d.frames(3);
    assert!(d.app().ui.create_proxies.is_some());
    d.snapshot("media-create-proxies");
    d.click("proxies.preset.prores_proxy_quarter");
    d.click("proxies.ok");
    for _ in 0..400 {
        d.frames(1);
        let attached = d.app().session.project.items.values().filter(|i| i.as_media().is_some_and(|m| m.proxy.is_some())).count();
        if attached == 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(d.app().session.project.items.values().filter(|i| i.as_media().is_some_and(|m| m.proxy.is_some())).count(), 2, "proxies attached");
    d.click("program.transport.media.toggleProxies");
    assert!(d.app().session.prefs.media.enable_proxies);
    assert_eq!(d.ids("project.item.").into_iter().filter(|i| i.ends_with(".proxy")).count(), 2, "proxy badges");
    d.frames(20);
    d.snapshot("media-proxies-on");
    // Project Manager dialog: calculate the size estimate
    d.ok("ui.menu.invoke", json!({"id": "file.projectManager"}));
    d.frames(2);
    d.app().ui.project_manager.as_mut().unwrap().destination = root.join("Collected").to_string_lossy().into_owned();
    d.click("pm.calculate");
    let est = d.app().ui.project_manager.as_ref().unwrap().estimate;
    assert!(est.is_some_and(|(a, b, n)| a > 0 && b == a && n == 2), "{est:?}");
    d.snapshot("media-project-manager");
    d.click("pm.ok");
    assert!(d.app().ui.project_manager.is_none());
    let _ = std::fs::remove_dir_all(&root);
}

/// #111: Link Media ▸ Locate… relinks the selected clip to the chosen file instead of importing it,
/// both with a host that returns the path and with one (the web) that runs the hint's command later.
#[test]
fn locate_relinks_instead_of_importing() {
    let root = tmp_dir("locate");
    let path = moved_project(&root);
    let mut s = Session::default();
    s.execute("file.open", json!({"path": path})).unwrap();
    let mut d = Driver::new(s);
    d.frames(6);
    let items = d.app().session.project.items.len();
    let imported = Arc::new(std::sync::Mutex::new(0));
    let n = imported.clone();
    d.app().hooks.pick_files = Some(Box::new(move |_| {
        *n.lock().unwrap() += 1;
        vec![]
    }));
    // an async host: nothing comes back now, the hint says what to run later
    let hints = Arc::new(std::sync::Mutex::new(Vec::new()));
    let h = hints.clone();
    d.app().hooks.pick_file_for_relink = Some(Box::new(move |_, hint| {
        h.lock().unwrap().push(hint);
        None
    }));
    d.click("linkMedia.locate");
    let hint = hints.lock().unwrap().pop().flatten().expect("the picker got a hint");
    assert_eq!(hint.command, "media.relink");
    let item = hint.params["item"].as_u64().expect("the hint names the clip");
    assert!(hint.params["match"].is_object(), "and the dialog's match options: {}", hint.params);
    // what the web host does once the user has chosen
    let file = root.join("Moved/Media/Harbour.mov").to_string_lossy().into_owned();
    let mut p = hint.params.clone();
    p["path"] = json!(file);
    d.exec(&hint.command, p);
    // a host that returns the path relinks the other clip from the dialog itself
    let other = root.join("Moved/Media/Night.mov").to_string_lossy().into_owned();
    d.app().hooks.pick_file_for_relink = Some(Box::new(move |_, _| Some(other.clone())));
    if d.app().ui.link_media.is_some() {
        d.click("linkMedia.locate");
        d.frames(4);
    }
    assert_eq!(*imported.lock().unwrap(), 0, "Locate… never goes through the import picker");
    assert_eq!(d.app().session.project.items.len(), items, "no new project item");
    let st = d.exec("media.status", json!({}));
    assert!(st.as_array().unwrap().iter().all(|m| m["status"] == "online"), "item {item}: {st}");
    let _ = std::fs::remove_dir_all(&root);
}
