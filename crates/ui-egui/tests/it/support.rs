//! The shared harness of the scripted UI tests: the real `FilmcraftApp` runs headless under
//! `egui_kittest` and is driven through the JSON control channel (`docs/control-protocol.md`).
//!
//! Each test file keeps its own `Driver` (its fields and the helpers that are genuinely its own),
//! declared with [`driver!`], which implements [`Drive`] for it; the stepping, request and
//! element-lookup code lives here once. Frames are stepped as eframe does: synthetic input queued by
//! `ui.click`/`ui.key` moves into the next frame's `RawInput` through the app's own
//! `raw_input_hook`.
//!
//! Also included by the separate test binaries (`#[path = "it/support.rs"] mod support;`), which
//! use only part of it.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use egui_kittest::{Harness, HarnessBuilder};
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// The harness every scripted test drives.
pub type AppHarness = Harness<'static, FilmcraftApp>;

/// A fresh session with the demo project open.
pub fn demo_session() -> Session {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    session
}

/// The usual harness: a 1600×980 window that may take up to 10 000 steps to settle.
pub fn builder() -> HarnessBuilder<FilmcraftApp> {
    Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000)
}

/// `FILMCRAFT_UI_SNAPSHOT_DIR`: where tests write PNGs of the window (off by default).
pub fn snapshot_dir() -> Option<PathBuf> {
    std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(PathBuf::from)
}

/// `FILMCRAFT_UI_SHOTS`: where tests write screenshots that must render (off by default).
pub fn shots_dir() -> Option<PathBuf> {
    std::env::var_os("FILMCRAFT_UI_SHOTS").map(PathBuf::from)
}

/// `b` with a wgpu renderer when [`snapshot_dir`] is set.
pub fn with_snapshots(b: HarnessBuilder<FilmcraftApp>) -> HarnessBuilder<FilmcraftApp> {
    if snapshot_dir().is_some() { b.wgpu() } else { b }
}

/// `b` with a wgpu renderer at 1 pixel per point when [`shots_dir`] is set.
pub fn with_shots(b: HarnessBuilder<FilmcraftApp>) -> HarnessBuilder<FilmcraftApp> {
    if shots_dir().is_some() { b.wgpu().with_pixels_per_point(1.0) } else { b }
}

/// Build the harness around `app`, with the headless framework (theme, fonts) installed.
pub fn build(b: HarnessBuilder<FilmcraftApp>, app: FilmcraftApp) -> AppHarness {
    b.build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    })
}

/// The app on `session`, listening on a control channel; returns the harness and the sender.
pub fn launch(session: Session, b: HarnessBuilder<FilmcraftApp>) -> (AppHarness, Sender<ControlRequest>) {
    let (tx, rx) = channel();
    (build(b, FilmcraftApp::new(session).with_control(rx)), tx)
}

/// Run the first frames (theme and fonts installed) of a freshly built driver.
pub fn start<D: Drive>(mut d: D) -> D {
    d.frames(4);
    d
}

/// One frame, feeding queued synthetic input like eframe's `raw_input_hook`; `edit` sees the raw
/// input first.
pub fn step(h: &mut AppHarness, edit: impl FnOnce(&mut egui::RawInput)) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    edit(&mut raw);
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    h.step();
}

/// [`step`] with a repaint requested: direct engine edits in a harness do not wake the event loop.
pub fn step_repaint(h: &mut AppHarness) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint();
    h.step();
}

/// The `[x, y, w, h]` rect of a `ui.elements` entry.
pub fn rect_of(element: &Value) -> [f64; 4] {
    let r = &element["rect"];
    [0, 1, 2, 3].map(|k| r[k].as_f64().unwrap_or_else(|| panic!("no rect in {element}")))
}

/// `[x, y, w, h]` as `f32`.
pub fn f32x4(r: [f64; 4]) -> [f32; 4] {
    r.map(|v| v as f32)
}

/// `[x, y, w, h]` as an egui rect.
pub fn egui_rect(r: [f64; 4]) -> egui::Rect {
    let [x, y, w, h] = f32x4(r);
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
}

/// Render the harness into `dir/file` (creating `dir`), or say why it was skipped.
pub fn save_render(h: &mut AppHarness, dir: &Path, file: &str) {
    match h.render() {
        Ok(img) => {
            std::fs::create_dir_all(dir).unwrap();
            let path = dir.join(file);
            img.save(&path).unwrap();
            eprintln!("snapshot: {}", path.display());
        }
        Err(e) => eprintln!("snapshot {file} skipped: {e}"),
    }
}

/// When [`snapshot_dir`] is set: run `settle` frames, then render the window into `file` there.
pub fn save_snapshot<D: Drive>(d: &mut D, settle: usize, file: &str) {
    let Some(dir) = snapshot_dir() else { return };
    d.frames(settle);
    save_render(d.parts().0, &dir, file);
}

/// When [`shots_dir`] is set: run `settle` frames, then render the window into `file` there (the
/// renderer must work).
pub fn save_shot<D: Drive>(d: &mut D, settle: usize, file: &str) {
    let Some(dir) = shots_dir() else { return };
    d.frames(settle);
    let img = d.parts().0.render().expect("wgpu render");
    std::fs::create_dir_all(&dir).unwrap();
    img.save(dir.join(file)).unwrap();
}

/// When [`snapshot_dir`] is set: snapshot into `<name>.png` there, cropped (with a margin for the
/// panel header) to the bounding box of the elements under `prefix` when given.
pub fn save_cropped<D: Drive>(d: &mut D, name: &str, prefix: Option<&str>) {
    let Some(dir) = snapshot_dir() else { return };
    let crop = prefix.map(|p| {
        let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for e in d.elements(p) {
            let [x, y, w, h] = f32x4(rect_of(&e));
            bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x + w), bb[3].max(y + h)];
        }
        bb
    });
    d.frames(2);
    let img = match d.parts().0.render() {
        Ok(i) => i,
        Err(e) => {
            eprintln!("snapshot {name} skipped: {e}");
            return;
        }
    };
    let ppp = img.width() as f32 / 1600.0;
    let img = match crop {
        Some(bb) => {
            let x0 = ((bb[0] - 24.0) * ppp).max(0.0) as u32;
            let y0 = ((bb[1] - 30.0) * ppp).max(0.0) as u32;
            let x1 = (((bb[2] + 16.0) * ppp) as u32).min(img.width());
            let y1 = (((bb[3] + 46.0) * ppp) as u32).min(img.height());
            image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image()
        }
        None => img,
    };
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    img.save(&path).unwrap();
    eprintln!("snapshot: {}", path.display());
}

/// When [`shots_dir`] is set: let the background frame workers finish (40 frames 25 ms apart),
/// then render the window into `file` there.
pub fn save_settled_shot<D: Drive>(d: &mut D, file: &str) {
    if shots_dir().is_none() {
        return;
    }
    for _ in 0..40 {
        d.frames(1);
        std::thread::sleep(Duration::from_millis(25));
    }
    save_shot(d, 0, file);
}

/// Render the window to `<target tmp>/ui-screenshots/<name>.png` (needs a GPU).
pub fn screenshot<D: Drive>(d: &mut D, name: &str) -> PathBuf {
    d.frames(2);
    let img = d.parts().0.render().expect("render");
    let png = filmcraft_ui_egui::control::encode_png(img.as_raw(), img.width(), img.height()).expect("png");
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ui-screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    std::fs::write(&path, png).unwrap();
    eprintln!("screenshot: {}", path.display());
    path
}

/// Press at `at`, move by `(dx, dy)` in `steps` increments (a frame each), release, and run
/// `settle` frames.
pub fn pointer_drag_by<D: Drive>(d: &mut D, at: egui::Pos2, dx: f32, dy: f32, steps: usize, settle: usize) {
    let mut p = at;
    let push = |d: &mut D, e: egui::Event| d.parts().0.input_mut().events.push(e);
    push(d, egui::Event::PointerMoved(p));
    d.frames(1);
    push(d, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    d.frames(1);
    for _ in 0..steps {
        p.x += dx / steps as f32;
        p.y += dy / steps as f32;
        push(d, egui::Event::PointerMoved(p));
        d.frames(1);
    }
    push(d, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(settle);
}

/// Press at `from`, move to `to` in `steps` even steps (a frame each), release, and run 2 frames.
pub fn pointer_drag<D: Drive>(d: &mut D, from: egui::Pos2, to: egui::Pos2, steps: usize) {
    let push = |d: &mut D, e: egui::Event| d.parts().0.input_mut().events.push(e);
    push(d, egui::Event::PointerMoved(from));
    d.frames(1);
    push(d, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    d.frames(1);
    for i in 1..=steps {
        push(d, egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
        d.frames(1);
    }
    push(d, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);
}

/// Every text shape drawn in the last frame.
pub fn painted_texts(h: &AppHarness) -> Vec<egui::epaint::TextShape> {
    fn walk(s: &egui::Shape, out: &mut Vec<egui::epaint::TextShape>) {
        match s {
            egui::Shape::Text(t) => out.push(t.clone()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    h.output().shapes.iter().for_each(|c| walk(&c.shape, &mut out));
    out
}

/// The demo's Footage bin and its media items.
pub fn demo_footage(app: &FilmcraftApp) -> (u64, Vec<u64>) {
    let bin = app
        .session
        .project
        .root
        .children
        .iter()
        .find_map(|e| match e {
            filmcraft_project::BinEntry::Bin(b) if b.name == "Footage" => Some(b.clone()),
            _ => None,
        })
        .expect("Footage bin");
    let items = bin.children.iter().filter_map(|e| if let filmcraft_project::BinEntry::Item(i) = e { Some(i.0) } else { None }).collect();
    (bin.id.0, items)
}

/// The first `"Movie"` item anywhere in a `project.inspect`-style JSON tree.
pub fn first_media_item(v: &Value) -> Option<u64> {
    match v {
        Value::Object(m) => {
            if let Some(id) = m.get("item").and_then(Value::as_u64)
                && m.get("type").and_then(Value::as_str).is_some_and(|t| t == "Movie")
            {
                return Some(id);
            }
            m.values().find_map(first_media_item)
        }
        Value::Array(a) => a.iter().find_map(first_media_item),
        _ => None,
    }
}

/// Scrolls Effect Controls with the wheel over its parameter list until element `id` sits in the
/// panel's upper part, where the user sees it: next's dock gives a panel a shorter body than the
/// old dock did. Stops when the element no longer moves (the list is at its end). `rect` looks up
/// an element's `[x, y, w, h]`.
pub fn reveal_in_effect_controls<D: Drive>(d: &mut D, id: &str, rect: impl Fn(&mut D, &str) -> [f64; 4]) {
    let p = rect(d, "panel.EffectControls");
    let mut last = None;
    for _ in 0..30 {
        let e = rect(d, id);
        if e[1] + e[3] <= p[1] + p[3] * 0.6 || last == Some(e[1]) {
            return;
        }
        last = Some(e[1]);
        d.ok("ui.scroll", json!({"x": p[0] + 40.0, "y": p[1] + p[3] / 2.0, "dy": -40.0}));
        d.frames(3);
    }
}

/// Scrolls the panel that holds element `id` until the element sits in the panel's upper part,
/// where the user sees it: next's dock gives a panel a shorter body than the old dock did.
/// Stops when the element no longer moves (the panel is at the end of its content).
pub fn reveal_in_panel<D: Drive>(d: &mut D, id: &str) {
    let mut last = None;
    for _ in 0..30 {
        let e = d.expect_rect(id);
        let cx = e[0] + e[2] / 2.0;
        let panels = d.ok("ui.elements", json!({"prefix": "panel."}));
        let panel = panels
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["id"].as_str().is_some_and(|pid| pid.matches('.').count() == 1))
            .filter_map(|p| {
                let r: Vec<f64> = p["rect"].as_array()?.iter().filter_map(Value::as_f64).collect();
                (r.len() == 4).then(|| [r[0], r[1], r[2], r[3]])
            })
            .find(|r| cx >= r[0] && cx <= r[0] + r[2] && e[1] >= r[1] && e[1] <= r[1] + r[3] + 40.0);
        let Some(p) = panel else { return };
        if e[1] + e[3] <= p[1] + p[3] * 0.6 || last == Some(e[1]) {
            return;
        }
        last = Some(e[1]);
        d.ok("ui.scroll", json!({"x": cx, "y": p[1] + p[3] / 2.0, "dy": -40.0}));
        d.frames(3);
    }
}

/// A test driver: the harness plus the control channel into the app. Override [`Drive::frames`]
/// to adjust each frame's input or collect its output; every request goes through it.
pub trait Drive {
    /// The harness and the sender of the app's control channel.
    fn parts(&mut self) -> (&mut AppHarness, &Sender<ControlRequest>);

    /// Run `n` frames.
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            step(self.parts().0, |_| {});
        }
    }

    /// The app under test.
    fn app(&mut self) -> &mut FilmcraftApp {
        self.parts().0.state_mut()
    }

    /// Runs after a successful [`Drive::ok`] reply, before it returns (some files let the UI
    /// settle here).
    fn after_ok(&mut self) {}

    /// Send a control request and run frames until it is answered.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.parts().1.send(req).expect("control channel");
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    /// `call` that must succeed; returns `result`.
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        self.after_ok();
        v["result"].clone()
    }

    /// Run an engine command through the control channel; returns its result.
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    /// Frames [`Drive::click`] runs after the click.
    const CLICK_SETTLE: usize = 2;

    /// Click the element `id` (`ui.click`), then let the UI settle.
    fn click(&mut self, id: &str) {
        self.click_settle(id, Self::CLICK_SETTLE);
    }

    /// Invoke the menu item `id` (`ui.menu.invoke`) and run 3 frames; returns its result.
    fn menu(&mut self, id: &str) -> Value {
        self.menu_settle(id, 3)
    }

    /// The `[x, y, w, h]` rect of the element with exactly this id, as `f32`.
    fn element(&mut self, id: &str) -> Option<[f32; 4]> {
        self.element_rect(id).map(f32x4)
    }

    /// Snapshot the window, or the elements under `prefix`, when [`snapshot_dir`] is set.
    fn snapshot(&mut self, name: &str, prefix: Option<&str>)
    where
        Self: Sized,
    {
        save_cropped(self, name, prefix);
    }

    /// Run frames (5 ms apart) until the element `id` is registered, for at most two minutes.
    fn wait_for(&mut self, id: &str) {
        let t0 = Instant::now();
        while t0.elapsed().as_secs() < 120 {
            if self.has(id) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
            self.frames(1);
        }
        panic!("{id} never appeared: {:?}", self.ids(id.split('.').next().unwrap()));
    }

    /// Click the element `id` (`ui.click`), then run `settle` frames.
    fn click_settle(&mut self, id: &str, settle: usize) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(settle);
    }

    /// Invoke the menu item `id` (`ui.menu.invoke`), then run `settle` frames; returns its result.
    fn menu_settle(&mut self, id: &str, settle: usize) -> Value {
        let r = self.ok("ui.menu.invoke", json!({"id": id}));
        self.frames(settle);
        r
    }

    /// Run frames until `done` holds, for at most two minutes of wall-clock time.
    fn wait_until(&mut self, what: &str, done: impl FnMut(&mut Self) -> bool)
    where
        Self: Sized,
    {
        self.wait_until_for(what, Duration::from_secs(120), 1, done);
    }

    /// Run `frames` frames at a time until `done` holds, for at most `limit` of wall-clock time.
    fn wait_until_for(&mut self, what: &str, limit: Duration, frames: usize, mut done: impl FnMut(&mut Self) -> bool)
    where
        Self: Sized,
    {
        let t0 = Instant::now();
        while !done(self) {
            assert!(t0.elapsed() < limit, "timed out waiting for {what}");
            self.frames(frames);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The `ui.elements` entries whose id starts with `prefix`.
    fn elements(&mut self, prefix: &str) -> Vec<Value> {
        self.ok("ui.elements", json!({"prefix": prefix})).as_array().expect("elements").clone()
    }

    /// The `ui.elements` entry with exactly this id.
    fn find_element(&mut self, id: &str) -> Option<Value> {
        self.elements(id).into_iter().find(|e| e["id"] == id)
    }

    /// The `[x, y, w, h]` rect of the element with exactly this id.
    fn element_rect(&mut self, id: &str) -> Option<[f64; 4]> {
        self.find_element(id).map(|e| rect_of(&e))
    }

    /// [`Drive::element_rect`] of an element that must exist.
    fn expect_rect(&mut self, id: &str) -> [f64; 4] {
        self.element_rect(id).unwrap_or_else(|| panic!("no element {id}"))
    }

    /// The ids of the elements whose id starts with `prefix`.
    fn ids(&mut self, prefix: &str) -> Vec<String> {
        self.elements(prefix).iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    /// Whether an element with exactly this id is registered.
    fn has(&mut self, id: &str) -> bool {
        self.elements(id).iter().any(|e| e["id"] == id)
    }
}

/// A driver made of nothing but the harness and the control channel (see [`driver!`]).
pub trait Launch: Drive + Sized {
    /// The driver around a freshly built harness and its control channel.
    fn from_parts(harness: AppHarness, tx: Sender<ControlRequest>) -> Self;

    /// The app on `session`, built from `b`, after its first frames.
    fn open(session: Session, b: HarnessBuilder<FilmcraftApp>) -> Self {
        let (harness, tx) = launch(session, b);
        start(Self::from_parts(harness, tx))
    }

    /// The demo project in the usual window ([`builder`]).
    fn demo() -> Self {
        Self::open(demo_session(), builder())
    }

    /// The demo project with snapping off, so drags land where the pointer goes; frames 1/60 s
    /// apart, as on a display.
    fn demo_snap_off() -> Self {
        let mut session = demo_session();
        session.execute("sequence.snap", json!({"on": false})).expect("snapping off");
        let mut d = Self::open(session, builder().with_step_dt(1.0 / 60.0));
        d.ok("ui.elements", json!({"prefix": "timeline.clip."}));
        d
    }
}

/// `impl Drive for $driver` over its `harness` and `tx` fields; further trait items follow after a
/// comma.
macro_rules! impl_drive {
    ($driver:ty $(, $($item:tt)*)?) => {
        impl $crate::support::Drive for $driver {
            fn parts(
                &mut self,
            ) -> (&mut $crate::support::AppHarness, &::std::sync::mpsc::Sender<::filmcraft_ui_egui::control::ControlRequest>) {
                (&mut self.harness, &self.tx)
            }
            $($($item)*)?
        }
    };
}
pub(crate) use impl_drive;

/// A file's `Driver`: `[vis] struct $driver` with the `harness` and `tx` fields, then any fields
/// in braces, and its [`Drive`] impl (and [`Launch`] when it has no other fields); trait items
/// (overrides of `frames`, `after_ok` or `CLICK_SETTLE`) follow after a semicolon.
macro_rules! driver {
    ($(#[$meta:meta])* $vis:vis $driver:ident $(; $($item:tt)*)?) => {
        $(#[$meta])*
        $vis struct $driver {
            $vis harness: $crate::support::AppHarness,
            $vis tx: ::std::sync::mpsc::Sender<::filmcraft_ui_egui::control::ControlRequest>,
        }
        $crate::support::impl_drive!($driver $(, $($item)*)?);
        impl $crate::support::Launch for $driver {
            fn from_parts(
                harness: $crate::support::AppHarness,
                tx: ::std::sync::mpsc::Sender<::filmcraft_ui_egui::control::ControlRequest>,
            ) -> Self {
                Self { harness, tx }
            }
        }
    };
    ($(#[$meta:meta])* $vis:vis $driver:ident { $($(#[$fmeta:meta])* $field:ident : $ty:ty),* $(,)? } $(; $($item:tt)*)?) => {
        $(#[$meta])*
        $vis struct $driver {
            $vis harness: $crate::support::AppHarness,
            $vis tx: ::std::sync::mpsc::Sender<::filmcraft_ui_egui::control::ControlRequest>,
            $($(#[$fmeta])* $vis $field: $ty,)*
        }
        $crate::support::impl_drive!($driver $(, $($item)*)?);
    };
}
pub(crate) use driver;
