//! The Trim Monitor's two pictures at a cut inside one take: the outgoing clip's last frame and the
//! incoming clip's first are frames of the same media item at the same size. Each picture's
//! newest request used to replace the other's waiting or running decode (one "view" per target
//! and size), so while the monitor kept asking for both they cancelled each other until one
//! decode happened to finish within a refresh. Each picture now has its own view. The real
//! `FilmcraftApp` under `egui_kittest`.

use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ItemId;
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    _tx: Sender<ControlRequest>,
}

impl Driver {
    fn new(session: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(100_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, _tx: tx };
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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        let v = self.harness.state_mut().session.execute(command, params.clone()).unwrap_or_else(|e| panic!("{command} {params}: {e}"));
        self.frames(1);
        v
    }

    /// The media frames the Trim Monitor's outgoing and incoming pictures should show now.
    fn wanted(&self) -> (i64, i64) {
        let app = self.harness.state();
        let info = filmcraft_engine::trim::monitor_info(&app.session);
        let frame = |side: &Value| {
            let item = ItemId(side["item"].as_u64().expect("item"));
            let rate = app.session.project.item(item).expect("item").frame_rate();
            rate.frame_at(Tick(side["mediaTime"].as_i64().expect("mediaTime")))
        };
        (frame(&info["outgoing"]), frame(&info["incoming"]))
    }

    fn shows(&self, tex: &str, frame: i64) -> bool {
        self.harness.state().texture_key(tex).is_some_and(|k| k.frame == frame)
    }

    /// Step frames, as the monitor redraws while it waits, until both pictures show the frames
    /// due; how long that took, or `None` after `limit`.
    fn settle(&mut self, limit: Duration) -> Option<Duration> {
        let (out, inc) = self.wanted();
        let t0 = Instant::now();
        while t0.elapsed() < limit {
            if self.shows("trim-outgoing", out) && self.shows("trim-incoming", inc) {
                return Some(t0.elapsed());
            }
            self.frames(1);
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }
}

/// The demo's first V1 clip cut in two with the razor: a cut inside one take, its roll edit point
/// selected so the Program panel shows the Trim Monitor.
fn demo_through_edit() -> Driver {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let seq = session.active_sequence().expect("sequence");
    let it = seq.video_tracks[0].items.first().expect("a V1 clip").clone();
    let mid = it.start + Tick((it.end().0 - it.start.0) / 2);
    let mut d = Driver::new(session);
    d.exec("timeline.razor", json!({"time": mid.0, "track": "V1"}));
    let seq = d.harness.state().session.active_sequence().expect("sequence").clone();
    let a = seq.video_tracks[0].items.iter().find(|c| c.end() == mid).expect("the clip before the cut").id;
    d.exec("trim.selectEditPoint", json!({"clip": a.0, "edge": "out", "kind": "roll"}));
    d.frames(2);
    d
}

/// Frame jobs given up so far for a newer request: cancelled while running or dropped while waiting.
fn given_up(d: &Driver) -> u64 {
    let s = d.harness.state().frames.stats();
    s.cancelled + s.replaced
}

#[test]
fn both_pictures_of_a_cut_inside_one_take_arrive_and_follow_a_roll() {
    let mut d = demo_through_edit();
    let (out, inc) = d.wanted();
    assert_eq!(inc, out + 1, "a through edit: the incoming picture is the outgoing one's next frame");
    assert!(d.settle(Duration::from_secs(20)).is_some(), "both pictures show their frames ({out}, {inc})");
    // Nothing moved, so no decode was worth cancelling. With one view per item and size, each
    // picture's request dropped or cancelled the other's decode: they still arrived, but late.
    assert_eq!(given_up(&d), 0, "the two pictures gave up each other's decode");
    for _ in 0..6 {
        d.exec("trim.forward", json!({}));
    }
    let (out2, _) = d.wanted();
    assert_eq!(out2, out + 6, "rolled 6 frames");
    assert!(d.settle(Duration::from_secs(20)).is_some(), "both pictures follow the roll");
}

/// A 20 s 3840×2160 25 fps movie in `codec` (ffmpeg's libx264 / libx265), a keyframe every `gop`
/// frames, made once in `target/fixtures/ui-egui`; `None` without ffmpeg.
fn movie_4k(codec: &str, gop: u32) -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/ui-egui");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("4k_{}_g{gop}.mp4", if codec == "libx265" { "hevc" } else { "h264" }));
    if !path.exists() {
        let ffmpeg = std::env::var_os("FILMCRAFT_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
        let ok = std::process::Command::new(ffmpeg)
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=3840x2160:rate=25:duration=20"])
            .args(["-c:v", codec, "-pix_fmt", "yuv420p", "-g", &gop.to_string(), "-preset", "veryfast"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return None;
        }
    }
    Some(path)
}

/// How long the Trim Monitor takes to show both frames of a cut inside one 4K take, when the edit
/// point is selected and after a burst of roll steps (one per refresh, as holding a trim key
/// does), and how many frame jobs were started, and cancelled or dropped before starting, meanwhile. Prints the numbers:
/// `cargo test --release -p filmcraft-ui-egui --test trim_monitor_frames_ui perf_ -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_trim_monitor_4k_through_edit() {
    for (codec, gop) in [("libx264", 250), ("libx265", 250), ("libx264", 25), ("libx265", 25)] {
        let Some(path) = movie_4k(codec, gop) else {
            eprintln!("skipped: no ffmpeg to make the 4K fixture");
            return;
        };
        let mut session = Session::default();
        session.execute("file.newProject", json!({"name": "Trim 4K"})).unwrap();
        let r = session.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
        let item = r["items"][0].as_u64().or_else(|| r["item"].as_u64()).unwrap_or_else(|| panic!("import: {r}"));
        session.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 0, "width": 3840, "height": 2160})).unwrap();
        let fr = session.sequence_rate().tick_of(1).0;
        // one take cut at media frame 187 (deep in a 250-frame GOP): 4 s either side
        let a = session.execute("timeline.place", json!({"item": item, "track": "V1", "time": 0, "sourceIn": fr * 87, "duration": fr * 100})).unwrap();
        session.execute("timeline.place", json!({"item": item, "track": "V1", "time": fr * 100, "sourceIn": fr * 187, "duration": fr * 100})).unwrap();
        let a = a["clips"][0].as_u64().unwrap_or_else(|| panic!("place: {a}"));
        let mut d = Driver::new(session);
        let stats = |d: &Driver| {
            let s = d.harness.state().frames.stats();
            (s.jobs, s.cancelled + s.replaced)
        };
        let limit = Duration::from_secs(30);
        let (j0, c0) = stats(&d);
        d.exec("trim.selectEditPoint", json!({"clip": a, "edge": "out", "kind": "roll"}));
        let select = d.settle(limit);
        let (j1, c1) = stats(&d);
        for _ in 0..12 {
            d.exec("trim.forward", json!({}));
        }
        let roll = d.settle(limit);
        let (j2, c2) = stats(&d);
        let ms = |t: Option<Duration>| t.map_or_else(|| format!("not within {} s", limit.as_secs()), |t| format!("{:.0} ms", t.as_secs_f64() * 1000.0));
        eprintln!(
            "{codec} 4K GOP {gop}: both frames on screen {} after selecting the edit point ({} jobs, {} given up); {} after 12 roll steps ({} jobs, {} given up)",
            ms(select),
            j1 - j0,
            c1 - c0,
            ms(roll),
            j2 - j1,
            c2 - c1,
        );
    }
}
