//! #2633: a press on the Timeline ruler that only moves the playhead must not close the audio
//! output.
//!
//! `scrub_playhead` called `app.stop()` on every press, and `stop()` closes the output stream. The
//! press runs twice in one drag, so the grain the scrub had just started lost its stream two frames
//! later, and the next grain opened the device again: 0.5-1 s on WASAPI, on the UI thread, which
//! froze the playhead at the start of every click and drag. The press now stops only a running
//! transport.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::{AudioOut, FilmcraftApp};
use serde_json::{Value, json};

type Fill = Box<dyn FnMut(&mut [f32], usize) + Send>;

/// The playback output: counts the streams that were closed.
struct Out {
    stops: Arc<AtomicUsize>,
}

impl AudioOut for Out {
    fn start(&mut self, _: Fill) -> Result<u32, String> {
        Ok(48_000)
    }
    fn stop(&mut self) {
        self.stops.fetch_add(1, Ordering::Relaxed);
    }
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn played_frames(&self) -> Option<u64> {
        None
    }
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    stops: Arc<AtomicUsize>,
}

impl Driver {
    /// An empty 3V/3A sequence with scrub audio on, and an output that counts its stops.
    fn empty_sequence() -> Self {
        let mut session = Session::default();
        session.execute("file.newProject", json!({"name": "press"})).expect("project");
        session.execute("file.newSequence", json!({"name": "Press", "video": 3, "audio": 3})).expect("sequence");
        session.prefs.audio.scrub_audio = true;
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        let stops = Arc::new(AtomicUsize::new(0));
        app.audio = Some(Box::new(Out { stops: stops.clone() }));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Self { harness, tx, stops };
        d.frames(4);
        // The first frame applies Settings ▸ Audio Hardware, which closes the output once: these
        // tests count the stops a press causes, not that one.
        d.stops.store(0, Ordering::Relaxed);
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

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn rect(&mut self, id: &str) -> egui::Rect {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
    }

    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }

    /// Press and release on the Timeline ruler without moving the pointer: the press still puts the
    /// playhead where the pointer is, and starts no grain.
    fn click_ruler(&mut self) {
        let r = self.rect("timeline.ruler");
        let pos = egui::pos2(r.min.x + 40.0, r.center().y);
        self.send(egui::Event::PointerMoved(pos));
        self.send(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        self.send(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        self.frames(2);
    }

    fn stops(&self) -> usize {
        self.stops.load(Ordering::Relaxed)
    }

    /// Puts a four-second clip in a new sequence and returns its media item. An empty sequence has
    /// no duration: a press could arrive after the transport had already run into its end and
    /// stopped by itself, and the Source monitor needs an item to open.
    fn with_contents(&mut self) -> u64 {
        let session = &mut self.harness.state_mut().session;
        let r = session.execute("file.newColorMatte", json!({"color": "#3366ff", "width": 144, "height": 108, "seconds": 4.0})).unwrap();
        let item = r["item"].as_u64().unwrap();
        session.execute("file.newSequenceFromClip", json!({"items": [item]})).unwrap();
        self.frames(2);
        item
    }

    /// Runs playback until the preroll is over, so the transport is really moving, not waiting for
    /// its first frames.
    fn until_playing(&mut self) {
        for _ in 0..120 {
            self.frames(1);
            if self.harness.state().playback.preroll.is_none() {
                return;
            }
        }
        panic!("the preroll did not end");
    }
}

/// #2633: with nothing playing there is nothing for the press to stop, so the output stream the
/// scrub is using stays open.
#[test]
fn a_press_on_the_ruler_while_stopped_does_not_close_the_audio_output() {
    let mut d = Driver::empty_sequence();
    // A drag opens the scrub stream; the press that follows must leave it open, or the next grain
    // has to open the device again (the freeze this fixes).
    let r = d.rect("timeline.ruler");
    let from = egui::pos2(r.min.x + 100.0, r.center().y);
    let moved = egui::pos2(r.min.x + 120.0, r.center().y);
    d.send(egui::Event::PointerMoved(from));
    d.send(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    d.send(egui::Event::PointerMoved(moved));
    assert!(d.harness.state().scrub.is_open(), "the drag leaves its stream open");
    d.send(egui::Event::PointerButton { pos: moved, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    assert!(d.harness.state().scrub.is_open(), "the stream lingers after the release");
    let stops = d.stops();
    let before = d.harness.state().session.playhead();
    d.click_ruler();
    assert_ne!(d.harness.state().session.playhead(), before, "the press moved the playhead");
    assert_eq!(d.stops(), stops, "nothing was playing, so the press had nothing to stop");
    assert!(d.harness.state().scrub.is_open(), "the open stream survived the press");
}

/// The press still stops a running transport: that is what `stop()` is for.
#[test]
fn a_press_on_the_ruler_while_playing_stops_the_transport() {
    let mut d = Driver::empty_sequence();
    d.with_contents();
    d.harness.state_mut().play(1.0);
    d.until_playing();
    assert!(d.harness.state().playback.playing, "playback is still running before the press");
    let before = d.stops();
    d.click_ruler();
    assert!(!d.harness.state().playback.playing, "the press stopped playback");
    assert!(d.stops() > before, "the press closed the output the transport was using");
}

/// #2633: `transport_running()` counts the Source monitor, so the press must stop it too; the
/// program `stop()` leaves its clock alone.
#[test]
fn a_press_on_the_ruler_while_the_source_plays_stops_it() {
    let mut d = Driver::empty_sequence();
    let item = d.with_contents();
    d.harness.state_mut().session.execute("source.open", json!({"item": item})).unwrap();
    d.harness.state_mut().play_source().unwrap();
    d.frames(1);
    assert!(d.harness.state().source_playback.clock.playing, "the Source monitor is playing");
    d.click_ruler();
    assert!(!d.harness.state().source_playback.clock.playing, "the press stopped the Source monitor");
}
