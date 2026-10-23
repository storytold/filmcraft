//! Headless UI tests of the Slip Monitor: with the Slip tool (Y), dragging a clip turns the
//! Program monitor into the four-up slip view (previous clip's Out, the new In and Out, the next
//! clip's In) that follows the pointer frame by frame, shows the decoded frames, and commits on
//! release exactly what it showed; Escape drops it. The real `FilmcraftApp` under `egui_kittest`
//! with the demo project, the pointer moving a few pixels per frame as a mouse does.

use std::sync::mpsc::{Sender, channel};

use egui::{Pos2, pos2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, ItemId, TrackItem};
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::panels::timeline::Drag;
use filmcraft_ui_egui::state::Tool;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project, snapping off so drags land exactly where the pointer goes; frames 1/60 s
    /// apart, as on a display.
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.execute("sequence.snap", json!({"on": false})).expect("snapping off");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d.ok("ui.elements", json!({"prefix": "timeline.clip."}));
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

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn app_mut(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }

    fn key(&mut self, key: egui::Key) {
        self.send(egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
        self.send(egui::Event::Key { key, physical_key: None, pressed: false, repeat: false, modifiers: Default::default() });
    }

    fn press(&mut self, at: Pos2) {
        self.send(egui::Event::PointerMoved(at));
        self.send(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    }

    fn move_to(&mut self, at: Pos2) {
        self.send(egui::Event::PointerMoved(at));
    }

    fn release(&mut self, at: Pos2) {
        self.send(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(2);
    }

    /// The Slip Monitor's elements (id → label) of the frame before the last.
    fn slip_elements(&self) -> Vec<(String, String)> {
        let v = serde_json::to_value(self.app().auto.query("slipMonitor.")).unwrap_or_default();
        v.as_array().into_iter().flatten().map(|e| (e["id"].as_str().unwrap_or("").to_string(), e["label"].as_str().unwrap_or("").to_string())).collect()
    }

    /// The preview the Slip drag in progress shows.
    fn slip_preview(&self) -> Value {
        match &self.app().tl.drag {
            Some(Drag::Slip { preview: Some(p), .. }) => (**p).clone(),
            other => panic!("no slip preview: {other:?}"),
        }
    }

    fn clip(&self, id: ClipId) -> TrackItem {
        self.app().session.active_sequence().expect("sequence").find_item(id).expect("clip").1.clone()
    }

    /// A V1 clip with clips touching both its edges and linked sound, and a point inside it.
    fn middle_clip(&mut self) -> (ClipId, Pos2) {
        let seq = self.app().session.active_sequence().expect("sequence").clone();
        let items = &seq.video_tracks[0].items;
        let it = items
            .iter()
            .find(|c| c.link.is_some() && items.iter().any(|p| p.end() == c.start) && items.iter().any(|n| n.start == c.end()))
            .expect("a V1 clip between two others")
            .clone();
        let id = format!("timeline.clip.{}", it.id.0);
        let els = self.ok("ui.elements", json!({"prefix": id}));
        let el = els.as_array().expect("elements").iter().find(|e| e["id"] == id.as_str()).unwrap_or_else(|| panic!("no {id}: {els}")).clone();
        let r: Vec<f32> = el["rect"].as_array().expect("rect").iter().map(|x| x.as_f64().expect("number") as f32).collect();
        // the clip's middle, above the lower band where a transition takes the press
        (it.id, pos2(r[0] + r[2] / 2.0, r[1] + 8.0))
    }

    /// The media frame a side shows, on its item's own frame grid.
    fn media_frame(&self, side: &Value) -> i64 {
        let item = ItemId(side["item"].as_u64().expect("item"));
        let rate = self.app().session.project.item(item).expect("item").frame_rate();
        rate.frame_at(Tick(side["mediaTime"].as_i64().expect("mediaTime")))
    }

    /// Step frames (the button still held) until the named picture shows `frame`, for up to 20 s.
    fn wait_for_texture(&mut self, tex: &str, frame: i64) -> bool {
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_secs(20) {
            if self.app().texture_key(tex).is_some_and(|k| k.frame == frame) {
                return true;
            }
            self.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        false
    }

    /// Render the window to `<target tmp>/ui-screenshots/<name>.png` (needs a GPU).
    fn screenshot(&mut self, name: &str) -> std::path::PathBuf {
        self.frames(2);
        let img = self.harness.render().expect("render");
        let png = filmcraft_ui_egui::control::encode_png(img.as_raw(), img.width(), img.height()).expect("png");
        let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ui-screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        std::fs::write(&path, png).unwrap();
        eprintln!("screenshot: {}", path.display());
        path
    }
}

fn ids(els: &[(String, String)]) -> Vec<&str> {
    els.iter().map(|(id, _)| id.as_str()).collect()
}

#[test]
fn dragging_with_the_slip_tool_shows_the_four_up_and_follows_the_pointer() {
    let mut d = Driver::demo();
    let (clip, at) = d.middle_clip();
    let before = d.clip(clip);
    let partners = filmcraft_engine::commands::with_links(&d.app().session, &[clip]);
    let partners_before: Vec<TrackItem> = partners.iter().map(|c| d.clip(*c)).collect();
    let undo0 = d.app().session.history.undo.len();
    d.key(egui::Key::Y);
    assert_eq!(d.app().ui.tool, Tool::Slip, "Y picks the Slip tool");
    assert!(d.slip_elements().is_empty(), "no Slip Monitor before the drag");

    d.press(at);
    // past egui's drag threshold: the four-up is up at once (the Program monitor is drawn before
    // the Timeline, and the automation ids are the last frame's)
    d.move_to(at + egui::vec2(8.0, 0.0));
    assert!(matches!(d.app().tl.drag, Some(Drag::Slip { preview: Some(_), .. })), "the slip drag started");
    d.frames(2);
    let els = d.slip_elements();
    for id in [
        "slipMonitor.previous",
        "slipMonitor.in",
        "slipMonitor.out",
        "slipMonitor.next",
        "slipMonitor.inTimecode",
        "slipMonitor.outTimecode",
        "slipMonitor.offset",
    ] {
        assert!(ids(&els).contains(&id), "{id} on screen: {els:?}");
    }
    let first = d.slip_preview();
    let (prev0, next0) = (first["previous"].clone(), first["next"].clone());
    assert!(prev0.is_object() && next0.is_object(), "the clip has neighbours: {first}");

    // a few pixels at a time to the right, then back past the start to the left: the In and Out
    // frames and the counters follow every step; the neighbours never change
    let mut last_in = first["in"]["mediaTime"].as_i64().unwrap();
    let mut seen = std::collections::HashSet::new();
    let path: Vec<f32> = (1..=12).map(|i| 8.0 + 4.0 * i as f32).chain((1..=30).map(|i| 56.0 - 4.0 * i as f32)).collect();
    for (step, dx) in path.iter().enumerate() {
        d.move_to(at + egui::vec2(*dx, 0.0));
        let p = d.slip_preview();
        let in_t = p["in"]["mediaTime"].as_i64().unwrap();
        let out_t = p["out"]["mediaTime"].as_i64().unwrap();
        // Out − In is the clip's length: the slip moves both by one amount
        assert_eq!(out_t - in_t, first["out"]["mediaTime"].as_i64().unwrap() - first["in"]["mediaTime"].as_i64().unwrap(), "step {step}");
        if step < 12 {
            assert!(in_t <= last_in, "dragging right slips to earlier media (step {step})");
        } else {
            assert!(in_t >= last_in, "dragging left slips to later media (step {step})");
        }
        last_in = in_t;
        seen.insert(in_t);
        assert_eq!(p["previous"], prev0, "the previous clip's Out frame stays");
        assert_eq!(p["next"], next0, "the next clip's In frame stays");
        // the counters show it on the next frame (the one the Timeline asks for when it changes),
        // and the automation ids read back are the frame before the last
        d.frames(2);
        let els = d.slip_elements();
        let label = |id: &str| els.iter().find(|(i, _)| i == id).map(|(_, l)| l.clone()).unwrap_or_default();
        assert_eq!(label("slipMonitor.inTimecode"), p["in"]["sourceTimecode"].as_str().unwrap());
        assert_eq!(label("slipMonitor.outTimecode"), p["out"]["sourceTimecode"].as_str().unwrap());
        let n = p["offsetFrames"].as_i64().unwrap();
        assert!(label("slipMonitor.offset").contains(&if n > 0 { format!("+{n}") } else { n.to_string() }), "{}", label("slipMonitor.offset"));
        // nothing is edited while dragging
        assert_eq!(d.clip(clip), before);
        assert_eq!(d.app().session.history.undo.len(), undo0);
    }
    assert!(seen.len() > 10, "the In frame moved with the pointer: {seen:?}");

    // the pictures are decoded frames of the new In and Out
    let p = d.slip_preview();
    let (fin, fout) = (d.media_frame(&p["in"]), d.media_frame(&p["out"]));
    assert!(d.wait_for_texture("slip-in", fin), "the In picture shows media frame {fin}: {:?}", d.app().texture_key("slip-in"));
    assert!(d.wait_for_texture("slip-out", fout), "the Out picture shows media frame {fout}: {:?}", d.app().texture_key("slip-out"));
    assert!(d.wait_for_texture("slip-previous", d.media_frame(&prev0)));
    assert!(d.wait_for_texture("slip-next", d.media_frame(&next0)));

    // release: one Slip, exactly what the monitor showed, picture and sound together; the
    // Program monitor is back
    let end = at + egui::vec2(*path.last().unwrap(), 0.0);
    d.release(end);
    assert!(d.slip_elements().is_empty(), "the Slip Monitor is gone after the release");
    assert!(d.app().tl.drag.is_none());
    assert_eq!(d.app().session.history.undo.len(), undo0 + 1, "one undo step");
    for c in p["clips"].as_array().unwrap() {
        let it = d.clip(ClipId(c["clip"].as_u64().unwrap()));
        assert_eq!(it.source_in.0, c["sourceIn"].as_i64().unwrap(), "committed as previewed");
    }
    let after = d.clip(clip);
    assert_eq!((after.start, after.duration), (before.start, before.duration), "the clip keeps its place and length");
    assert_ne!(after.source_in, before.source_in);
    let shift = after.source_in - before.source_in;
    for (c, was) in partners.iter().zip(&partners_before) {
        assert_eq!(d.clip(*c).source_in - was.source_in, shift, "linked clips slip together");
    }

    // undo and redo
    d.ok("engine.execute", json!({"command": "edit.undo", "params": {}}));
    assert_eq!(d.clip(clip), before);
    d.ok("engine.execute", json!({"command": "edit.redo", "params": {}}));
    assert_eq!(d.clip(clip), after);
}

#[test]
fn escape_drops_the_slip_and_its_monitor() {
    let mut d = Driver::demo();
    let (clip, at) = d.middle_clip();
    let before = d.clip(clip);
    let project = d.app().session.project.clone();
    let undo0 = d.app().session.history.undo.len();
    d.key(egui::Key::Y);
    d.press(at);
    for i in 1..=10 {
        d.move_to(at + egui::vec2(6.0 * i as f32, 0.0));
    }
    assert!(!d.slip_elements().is_empty(), "the Slip Monitor is up");
    assert_ne!(d.slip_preview()["offsetFrames"], json!(0));
    d.key(egui::Key::Escape);
    d.frames(1);
    assert!(d.app().tl.drag.is_none(), "Escape ended the drag");
    assert!(d.slip_elements().is_empty(), "and closed the Slip Monitor");
    d.release(at + egui::vec2(60.0, 0.0));
    assert_eq!(d.clip(clip), before, "nothing slipped");
    assert_eq!(*d.app().session.project, *project);
    assert_eq!(d.app().session.history.undo.len(), undo0, "no undo step");
}

#[test]
fn a_slip_far_past_the_media_stops_at_its_end_and_commits_that() {
    let mut d = Driver::demo();
    let (clip, at) = d.middle_clip();
    d.key(egui::Key::Y);
    d.press(at);
    // far to the left: later and later media, past what the file has
    for i in 1..=40 {
        d.move_to(at - egui::vec2(25.0 * i as f32, 0.0));
    }
    let p = d.slip_preview();
    assert_eq!(p["atLimit"], json!(true), "held at the media's end: {p}");
    assert!(d.slip_elements().iter().any(|(id, l)| id == "slipMonitor.offset" && l.contains("limit")));
    d.release(at - egui::vec2(1000.0, 0.0));
    for c in p["clips"].as_array().unwrap() {
        assert_eq!(d.clip(ClipId(c["clip"].as_u64().unwrap())).source_in.0, c["sourceIn"].as_i64().unwrap());
    }
    let _ = clip;
}

#[test]
fn a_fast_slip_keeps_one_decode_per_picture_waiting() {
    let mut d = Driver::demo();
    let (_, at) = d.middle_clip();
    d.key(egui::Key::Y);
    d.press(at);
    // big jumps every frame: each refresh asks for new In and Out frames
    let mut most = 0;
    for i in 1..=60 {
        let dx = if i % 2 == 0 { 15.0 * i as f32 } else { -15.0 * i as f32 };
        d.move_to(at + egui::vec2(dx.clamp(-600.0, 600.0), 0.0));
        most = most.max(d.app().frames.queue_len());
    }
    // the newest position replaces the ones left behind in each picture's view: a handful of
    // jobs wait, never one per refresh
    assert!(most <= 8, "at most a few frame jobs queued while dragging fast, saw {most}");
    d.release(at);
}

#[test]
fn slip_and_trim_tools_still_edit_after_a_slip() {
    // Trim Monitor drags and ordinary trims keep working once a slip has run (the pictures share
    // `trim_monitor::picture`)
    let mut d = Driver::demo();
    let (clip, at) = d.middle_clip();
    d.key(egui::Key::Y);
    d.press(at);
    for i in 1..=6 {
        d.move_to(at + egui::vec2(5.0 * i as f32, 0.0));
    }
    d.release(at + egui::vec2(30.0, 0.0));
    d.key(egui::Key::V);
    assert_eq!(d.app().ui.tool, Tool::Selection);
    let before = d.clip(clip);
    d.ok("engine.execute", json!({"command": "timeline.trim", "params": {"clip": clip.0, "edge": "out", "deltaFrames": -3}}));
    assert_eq!(d.clip(clip).duration, before.duration - d.app().session.sequence_rate().tick_of(3));
    d.ok("engine.execute", json!({"command": "trim.selectEditPoint", "params": {"clip": clip.0, "edge": "out", "kind": "roll"}}));
    d.frames(3);
    let els = serde_json::to_value(d.app().auto.query("trimMonitor.")).unwrap_or_default();
    assert!(els.as_array().is_some_and(|a| a.iter().any(|e| e["id"] == "trimMonitor.outgoing")), "the Trim Monitor still shows");
}

/// Screenshots of the Slip Monitor mid-drag, for looking at (needs a GPU):
/// `cargo test -p filmcraft-ui-egui --test slip_monitor_ui -- --ignored --nocapture`
#[test]
#[ignore]
fn slip_monitor_screenshots() {
    let mut d = Driver::demo();
    let (_, at) = d.middle_clip();
    d.screenshot("slip-before");
    d.key(egui::Key::Y);
    d.press(at);
    for (i, dx) in [40.0, -80.0].into_iter().enumerate() {
        for s in 1..=10 {
            d.move_to(at + egui::vec2(dx * s as f32 / 10.0, 0.0));
        }
        let p = d.slip_preview();
        let (fin, fout) = (d.media_frame(&p["in"]), d.media_frame(&p["out"]));
        d.wait_for_texture("slip-in", fin);
        d.wait_for_texture("slip-out", fout);
        d.screenshot(&format!("slip-dragging-{i}"));
    }
    d.release(at + egui::vec2(-80.0, 0.0));
    d.screenshot("slip-released");
}

/// A 20 s 3840×2160 25 fps movie with sound in `codec` (ffmpeg's libx264 / libx265), a keyframe
/// every `gop` frames, made once in `target/fixtures/ui-egui`; `None` without ffmpeg.
fn movie_4k(codec: &str, gop: u32) -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/ui-egui");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("slip_4k_{}_g{gop}.mp4", if codec == "libx265" { "hevc" } else { "h264" }));
    if !path.exists() {
        let ffmpeg = std::env::var_os("FILMCRAFT_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
        let ok = std::process::Command::new(ffmpeg)
            .args([
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=3840x2160:rate=25:duration=20",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=20",
            ])
            .args(["-c:v", codec, "-pix_fmt", "yuv420p", "-g", &gop.to_string(), "-preset", "veryfast", "-c:a", "aac", "-shortest"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return None;
        }
    }
    Some(path)
}

/// How the Slip Monitor keeps up with long-GOP 4K H.264 and HEVC: the UI frame time while the
/// pointer slips a clip fast (decoding is off the UI thread, so it stays a display refresh),
/// the frame jobs waiting, and how long after the pointer stops the exact In and Out frames are
/// on screen. Prints the numbers:
/// `cargo test --release -p filmcraft-ui-egui --test slip_monitor_ui perf_ -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_slip_monitor_4k_long_gop() {
    for (codec, gop) in [("libx264", 250), ("libx265", 250), ("libx264", 25), ("libx265", 25)] {
        let Some(path) = movie_4k(codec, gop) else {
            eprintln!("skipped: no ffmpeg to make the 4K fixture");
            return;
        };
        let mut session = Session::default();
        session.execute("file.newProject", json!({"name": "Slip 4K"})).unwrap();
        let r = session.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
        let item = r["items"][0].as_u64().or_else(|| r["item"].as_u64()).unwrap_or_else(|| panic!("import: {r}"));
        session.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 1, "width": 3840, "height": 2160})).unwrap();
        session.execute("sequence.snap", json!({"on": false})).unwrap();
        let fr = session.sequence_rate().tick_of(1).0;
        // three 4 s clips cut from the take, with handles on both sides of the middle one
        for (i, src) in [(0, 0), (1, 200), (2, 400)] {
            session
                .execute(
                    "timeline.place",
                    json!({"item": item, "track": "V1", "audioTrack": "A1", "time": fr * 100 * i, "sourceIn": fr * (src + 20), "duration": fr * 100}),
                )
                .unwrap();
        }
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(100_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d.ok("ui.set", json!({"timeline": {"fit": true}}));
        d.frames(10);
        let (clip, at) = d.middle_clip();
        // baseline: the Program monitor showing a frame deep in a GOP, as a seek does
        let it = d.clip(clip);
        let seek = it.start + Tick(fr * 87);
        let want = d.app().session.sequence_rate().frame_at(seek);
        d.app_mut().session.set_playhead(seek);
        let t0 = std::time::Instant::now();
        while d.app().program_picture().map(|k| k.frame) != Some(want) && t0.elapsed() < std::time::Duration::from_secs(20) {
            d.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let program = t0.elapsed().as_secs_f64() * 1000.0;
        d.key(egui::Key::Y);
        d.press(at);
        let mut steps = Vec::new();
        let mut most_queued = 0;
        for i in 1..=120 {
            let dx = ((i as f32) * 0.37).sin() * 220.0;
            let t0 = std::time::Instant::now();
            d.move_to(at + egui::vec2(dx, 0.0));
            steps.push(t0.elapsed().as_secs_f64() * 1000.0);
            most_queued = most_queued.max(d.app().frames.queue_len());
        }
        steps.sort_by(f64::total_cmp);
        let p = d.slip_preview();
        let (fin, fout) = (d.media_frame(&p["in"]), d.media_frame(&p["out"]));
        let t0 = std::time::Instant::now();
        let shown = d.wait_for_texture("slip-in", fin) && d.wait_for_texture("slip-out", fout);
        let settle = t0.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "{codec} 4K GOP {gop}: Program monitor seek {program:.0} ms; UI frame while slipping median {:.1} ms, p95 {:.1} ms, max {:.1} ms; at most {most_queued} frame jobs queued; exact In+Out on screen {settle:.0} ms after the pointer stopped ({})",
            steps[steps.len() / 2],
            steps[steps.len() * 95 / 100],
            steps[steps.len() - 1],
            if shown { "shown" } else { "NOT shown" }
        );
        assert!(shown, "{codec}: the exact frames arrive once the pointer rests");
        d.release(at);
    }
}
