//! Premiere's gap editing and sequence end, in the headless app: a click on the empty space
//! between two clips selects the gap (a light box), Delete / Backspace close it; playback that
//! reaches the end parks the playhead flush with the end of the last clip.
//!
//! `cargo test -p filmcraft-ui-egui --test gap_ui -- --ignored screenshots` renders the states to
//! PNGs for visual review (needs a GPU).

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project with its Desert_Dunes clip cleared from V1/A1 (a gap on both) and Sync Lock
    /// off on A2, whose music runs across the gap (with it on, Premiere refuses to close the gap).
    fn with_gap(render: bool) -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if render {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        let dunes = d.clip("Desert_Dunes.mp4", 0);
        d.exec("timeline.select", json!({"clips": [dunes]}));
        d.exec("edit.clear", json!({}));
        d.exec("timeline.setTrack", json!({"track": "A2", "syncLock": false}));
        d.ok("ui.set", json!({"focused": "Timeline"}));
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

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    /// The id of the clip named `name` on video track `v` (0 = V1).
    fn clip(&mut self, name: &str, v: usize) -> u64 {
        let seq = self.exec("sequence.inspect", json!({}));
        seq["video"][v]["items"].as_array().unwrap().iter().find(|i| i["name"] == name).and_then(|i| i["clip"].as_u64()).unwrap()
    }

    fn start(&mut self, clip: u64) -> i64 {
        let seq = self.exec("sequence.inspect", json!({}));
        seq["video"]
            .as_array()
            .unwrap()
            .iter()
            .chain(seq["audio"].as_array().unwrap())
            .flat_map(|t| t["items"].as_array().unwrap().clone())
            .find(|i| i["clip"].as_u64() == Some(clip))
            .and_then(|i| i["start"].as_i64())
            .unwrap()
    }

    /// Screen point of time `t` on the row of `track` (the Timeline as last laid out).
    fn point(&self, track: &str, t: Tick) -> (f32, f32) {
        let layout = self.app().tl.layout.clone().expect("the Timeline is laid out");
        let seq = self.app().session.active_sequence().unwrap();
        let (kind, n) = (track.as_bytes()[0], track[1..].parse::<usize>().unwrap() - 1);
        let id = if kind == b'V' { seq.video_tracks[n].id } else { seq.audio_tracks[n].id };
        let row = layout.rows.iter().find(|r| r.track == id).expect("row on screen");
        (layout.x_of(t), row.rect.center().y)
    }

    fn click(&mut self, (x, y): (f32, f32)) {
        self.ok("ui.click", json!({"x": x, "y": y}));
        self.frames(3);
    }

    fn key(&mut self, key: &str) {
        self.ok("ui.key", json!({"key": key}));
        self.frames(3);
    }

    fn gap(&mut self) -> Value {
        self.exec("sequence.inspect", json!({}))["gap"].clone()
    }

    /// Render the Timeline panel to `<tmp>/gap-screenshots/<name>.png`.
    fn screenshot(&mut self, name: &str) -> std::path::PathBuf {
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            self.frames(1);
        }
        let img = self.harness.render().expect("render");
        let r = self.ok("ui.elements", json!({"prefix": "panel.Timeline"}));
        let rect = r.as_array().unwrap().iter().find(|e| e["id"] == "panel.Timeline").map(|e| e["rect"].clone()).unwrap();
        let ppp = img.width() as f32 / 1600.0;
        let [x, y, w, h] = [0, 1, 2, 3].map(|i| (rect[i].as_f64().unwrap() as f32 * ppp) as u32);
        let crop = image::imageops::crop_imm(&img, x, y, w.min(img.width() - x), h.min(img.height() - y)).to_image();
        let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gap-screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        crop.save(&path).unwrap();
        eprintln!("screenshot: {}", path.display());
        path
    }
}

#[test]
fn clicking_a_gap_selects_it_and_delete_closes_it() {
    let mut d = Driver::with_gap(false);
    let rate = d.app().session.sequence_rate();
    let misty = d.clip("Misty_Forest.mp4", 0);
    let gap_start = Tick::from_seconds_f64(13.388375);
    let before = d.start(misty);
    assert!(before > rate.snap(gap_start).0 + rate.tick_of(24).0, "a gap precedes Misty_Forest on V1");
    // click in the middle of the gap on V1
    let mid = Tick((gap_start.0 + before) / 2);
    d.click(d.point("V1", mid));
    let gap = d.gap();
    assert_eq!(gap["end"].as_i64(), Some(before), "the gap ends where Misty_Forest starts: {gap}");
    assert!(d.app().session.state.selection.is_empty(), "no clip selected");
    // the light box is drawn over it
    let ids = d.ok("ui.elements", json!({"prefix": "timeline.gap"}));
    assert_eq!(ids.as_array().map(Vec::len), Some(1), "{ids}");
    // Delete closes it: Misty_Forest moves up to the end of City_Night_Drive
    d.key("Delete");
    let after = d.start(misty);
    assert_eq!(after, gap["start"].as_i64().unwrap(), "closed up");
    assert!(d.gap().is_null());
    assert!(d.ok("ui.elements", json!({"prefix": "timeline.gap"})).as_array().is_none_or(|a| a.is_empty()));
    // undo, then the same with Backspace
    d.key("Cmd+Z");
    assert_eq!(d.start(misty), before);
    d.click(d.point("A1", mid));
    assert!(!d.gap().is_null(), "the gap on A1 selects too");
    d.key("Backspace");
    assert_eq!(d.start(misty), after);
    // past the last clip there is no gap: a click there only deselects
    d.key("Cmd+Z");
    let end = d.app().session.active_sequence().unwrap().duration();
    d.click(d.point("V1", end + Tick::from_seconds_f64(1.0)));
    assert!(d.gap().is_null());
    // dragging from a gap still draws a marquee (and selects no gap)
    let (x, y) = d.point("V1", mid);
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 200.0, "y": y + 30.0}}));
    d.frames(3);
    assert!(d.gap().is_null());
}

#[test]
fn playback_parks_the_playhead_at_the_end_of_the_last_clip() {
    let mut d = Driver::with_gap(false);
    let end = d.app().session.sequence_end();
    d.exec("playhead.set", json!({"time": end.0 - Tick::from_seconds_f64(0.25).0}));
    d.ok("ui.playback", json!({"action": "play"}));
    for _ in 0..3000 {
        d.frames(1);
        if !d.app().playback.playing {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!d.app().playback.playing, "playback stopped at the end");
    assert_eq!(d.app().session.playhead(), end, "flush with the end of the last clip, not on its last frame");
    // End goes to the same place
    d.exec("playhead.start", json!({}));
    d.key("End");
    assert_eq!(d.app().session.playhead(), end);
}

/// Renders the gap and sequence-end states for visual review (needs a GPU).
#[test]
#[ignore]
fn screenshots() {
    let mut d = Driver::with_gap(true);
    let misty = d.clip("Misty_Forest.mp4", 0);
    let before = d.start(misty);
    let gap_start = Tick::from_seconds_f64(13.388375);
    d.screenshot("1-gap-before-click");
    d.click(d.point("V1", Tick((gap_start.0 + before) / 2)));
    d.screenshot("2-gap-selected");
    d.key("Delete");
    d.screenshot("3-gap-closed");
    d.key("End");
    d.screenshot("4-playhead-at-end");
    // play into the end, then zoom in on the playhead: one frame early would show as a gap
    // between the playhead line and the end of the last clip
    let end = d.app().session.sequence_end();
    d.exec("playhead.set", json!({"time": end.0 - Tick::from_seconds_f64(0.5).0}));
    d.ok("ui.playback", json!({"action": "play"}));
    for _ in 0..3000 {
        d.frames(1);
        if !d.app().playback.playing {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(d.app().session.playhead(), end);
    for _ in 0..9 {
        d.key("=");
    }
    d.screenshot("5-playback-end-zoomed");
}
