//! #2633: a playhead scrub opens the scrub output once, and never in the frame that moves the
//! playhead.
//!
//! `scrub_playhead` used to call `app.stop()` on every press, which closed the output stream the
//! scrub grain had just opened (twice per drag: the press path runs twice). Opening a WASAPI stream
//! from a cold audio engine costs 0.5-1 s on this machine, on the UI thread, so the playhead froze
//! at the start of every click and drag. The scrub now plays on its own output (`scrub_out`), whose
//! implementation opens the stream on a thread that owns it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use crate::support::{Drive, build, builder, driver, egui_rect, start};
use filmcraft_engine::Session;
use filmcraft_ui_egui::scrub::ScrubOut;
use filmcraft_ui_egui::{AudioOut, FilmcraftApp};
use serde_json::json;

type Fill = Box<dyn FnMut(&mut [f32], usize) + Send>;

/// What the scrub output saw.
#[derive(Default)]
struct ScrubCounts {
    /// Streams asked for (what the platform layer opens on its thread).
    plays: AtomicUsize,
    /// Streams closed.
    stops: AtomicUsize,
    open: AtomicBool,
}

/// The scrub output: returns at once, like the real one does.
struct ScrubDevice {
    counts: Arc<ScrubCounts>,
    fill: Arc<Mutex<Option<Fill>>>,
}

impl ScrubOut for ScrubDevice {
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn play(&mut self, fill: Fill) {
        if !self.counts.open.swap(true, Ordering::Relaxed) {
            self.counts.plays.fetch_add(1, Ordering::Relaxed);
        }
        *self.fill.lock().unwrap() = Some(fill);
    }
    fn stop(&mut self) {
        if self.counts.open.swap(false, Ordering::Relaxed) {
            self.counts.stops.fetch_add(1, Ordering::Relaxed);
        }
        *self.fill.lock().unwrap() = None;
    }
}

/// The playback output: the scrub must leave it alone.
#[derive(Default)]
struct PlaybackCounts {
    starts: AtomicUsize,
}

struct Playback {
    counts: Arc<PlaybackCounts>,
}

impl AudioOut for Playback {
    fn start(&mut self, _: Fill) -> Result<u32, String> {
        self.counts.starts.fetch_add(1, Ordering::Relaxed);
        Ok(48_000)
    }
    fn stop(&mut self) {}
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn played_frames(&self) -> Option<u64> {
        None
    }
}

driver! {
    Driver {
        scrub: Arc<ScrubCounts>,
        playback: Arc<PlaybackCounts>,
    }
}

impl Driver {
    /// An empty sequence (three video and three audio tracks) with scrub audio on: the output is
    /// opened by the scrub alone, whatever the timeline holds.
    fn empty_sequence() -> Self {
        let mut session = Session::default();
        session.execute("file.newProject", json!({"name": "scrub"})).expect("project");
        session.execute("file.newSequence", json!({"name": "Scrub", "video": 3, "audio": 3})).expect("sequence");
        session.prefs.audio.scrub_audio = true;
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        let scrub = Arc::new(ScrubCounts::default());
        let playback = Arc::new(PlaybackCounts::default());
        app.scrub_out = Some(Box::new(ScrubDevice { counts: scrub.clone(), fill: Arc::new(Mutex::new(None)) }));
        app.audio = Some(Box::new(Playback { counts: playback.clone() }));
        // A real frame's worth of time: the default step would run the 0.25 s linger out at once.
        let harness = build(builder().with_step_dt(1.0 / 60.0), app);
        start(Self { harness, tx, scrub, playback })
    }

    fn rect(&mut self, id: &str) -> egui::Rect {
        egui_rect(self.expect_rect(id))
    }

    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }

    /// Press on the Timeline ruler, drag across it one pointer move per frame, release.
    fn drag_ruler(&mut self, steps: usize) {
        let r = self.rect("timeline.ruler");
        let y = r.center().y;
        let (from, to) = (egui::pos2(r.min.x + 40.0, y), egui::pos2(r.max.x - 40.0, y));
        self.send(egui::Event::PointerMoved(from));
        self.send(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        for k in 1..=steps {
            self.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / steps as f32)));
        }
        self.send(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    }

    fn plays(&self) -> usize {
        self.scrub.plays.load(Ordering::Relaxed)
    }

    fn stops(&self) -> usize {
        self.scrub.stops.load(Ordering::Relaxed)
    }

    fn playback_starts(&self) -> usize {
        self.playback.starts.load(Ordering::Relaxed)
    }
}

/// #2633: one drag asks for the scrub output once, and never opens the playback output.
#[test]
fn a_playhead_drag_opens_the_scrub_output_once() {
    let mut d = Driver::empty_sequence();
    d.drag_ruler(12);
    assert_eq!(d.plays(), 1, "the first grain asked for the output");
    assert_eq!(d.stops(), 0, "no press of the drag closed the stream it opened");
    assert_eq!(d.playback_starts(), 0, "scrubbing does not touch the playback output");

    // A second drag while the grain still lingers reuses the same stream.
    d.drag_ruler(12);
    assert_eq!(d.plays(), 1, "the lingering stream is reused");
    assert_eq!(d.stops(), 0);

    // Release plus the linger closes it once.
    d.frames(30);
    assert_eq!(d.stops(), 1, "the stream closes after the grain lingers out");
}

/// A press that only moves the playhead still stops playback: that is what `stop()` is for.
#[test]
fn a_press_on_the_ruler_while_playing_stops_the_transport() {
    let mut d = Driver::empty_sequence();
    d.harness.state_mut().playback.playing = true;
    d.harness.state_mut().playback.preroll = None;
    assert!(d.harness.state().transport_running());
    d.drag_ruler(4);
    assert!(!d.harness.state().playback.playing, "the press stopped playback");
}
