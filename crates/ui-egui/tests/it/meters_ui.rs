//! The Audio Meters panel, driven headless through the control channel: what it shows as it gets
//! narrower (the loudness readout and the dB scale give way, the bars stay), and that it moves
//! while the playhead is scrubbed with audio scrubbing on.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use craft_ui::docking::Node;
use craft_ui::layout::SplitSize;
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::dock::PanelKind;
use filmcraft_ui_egui::{AudioOut, FilmcraftApp};
use serde_json::{Value, json};

type Fill = Box<dyn FnMut(&mut [f32], usize) + Send>;

/// An output that keeps the callback it was started with, for the test to pull samples from.
struct Out {
    fill: Arc<Mutex<Option<Fill>>>,
    starts: Arc<AtomicUsize>,
}

impl AudioOut for Out {
    fn start(&mut self, fill: Fill) -> Result<u32, String> {
        *self.fill.lock().unwrap() = Some(fill);
        self.starts.fetch_add(1, Ordering::Relaxed);
        Ok(48_000)
    }
    fn stop(&mut self) {
        *self.fill.lock().unwrap() = None;
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
    fill: Arc<Mutex<Option<Fill>>>,
    /// How many times the output was started.
    starts: Arc<AtomicUsize>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.prefs.audio.scrub_audio = true;
        let (tx, rx) = channel();
        let fill = Arc::new(Mutex::new(None));
        let starts = Arc::new(AtomicUsize::new(0));
        let mut app = FilmcraftApp::new(session).with_control(rx);
        app.audio = Some(Box::new(Out { fill: fill.clone(), starts: starts.clone() }));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, fill, starts };
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
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn element(&mut self, id: &str) -> Option<Value> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == id).cloned()
    }
    fn label(&mut self, id: &str) -> String {
        self.element(id).and_then(|e| e["label"].as_str().map(str::to_string)).unwrap_or_default()
    }
    /// Give the Audio Meters panel `width` points (it sits in a split beside the Timeline).
    fn meters_width(&mut self, width: f32) {
        fn set(n: &mut Node<PanelKind>, width: f32) -> bool {
            match n {
                Node::Split { size, second, .. } if matches!(&**second, Node::Tabs { panels, .. } if panels.as_slice() == [PanelKind::AudioMeters]) => {
                    *size = SplitSize::FixedSecond(width);
                    true
                }
                Node::Split { first, second, .. } => set(first, width) || set(second, width),
                _ => false,
            }
        }
        assert!(self.harness.state_mut().ui.dock.root.as_mut().is_some_and(|root| set(root, width)));
        self.frames(3);
    }
    /// The level the left bar shows, in dB.
    fn left_db(&mut self) -> f32 {
        let label = self.label("audioMeters.channel.L");
        label.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or_else(|| panic!("label {label:?}"))
    }
}

/// As the panel narrows the loudness readout goes and then the dB scale, while the bars stay; at
/// the default width it shows all of them, the scale every 3 dB.
#[test]
fn a_narrow_meter_panel_drops_the_readout_and_then_the_scale() {
    let mut d = Driver::demo();
    assert!(d.element("audioMeters.loudness").is_some(), "the readout at the default width");
    assert_eq!(d.label("audioMeters.scale").split(' ').count(), 20, "0 to -57 every 3 dB");
    d.meters_width(60.0);
    assert!(d.element("audioMeters.loudness").is_none(), "no room for the readout");
    assert!(d.element("audioMeters.scale").is_some());
    d.meters_width(44.0);
    assert!(d.element("audioMeters.scale").is_none(), "no room for the scale");
    for name in ["L", "R"] {
        let bar = d.element(&format!("audioMeters.channel.{name}")).unwrap();
        assert!(bar["rect"][2].as_f64().unwrap() >= 5.0, "the {name} bar stays readable: {bar}");
    }
    d.meters_width(116.0);
    assert!(d.element("audioMeters.loudness").is_some());
}

/// Dragging the playhead along the ruler with audio scrubbing on plays grains of the sequence, and
/// the meters show them, as they do in playback.
#[test]
fn the_meters_move_while_scrubbing() {
    let mut d = Driver::demo();
    assert_eq!(d.left_db(), -90.0, "silent before");
    let ruler = d.element("timeline.ruler").expect("the Timeline ruler");
    let r = &ruler["rect"];
    let (x, y, w) = (r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32 + r[3].as_f64().unwrap() as f32 / 2.0, r[2].as_f64().unwrap() as f32);
    let (from, to) = (egui::pos2(x + w * 0.2, y), egui::pos2(x + w * 0.3, y));
    d.harness.input_mut().events.push(egui::Event::PointerMoved(from));
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    d.frames(1);
    d.harness.input_mut().events.push(egui::Event::PointerMoved(to));
    // the playhead moves in this frame; the next one hears it
    d.frames(2);
    // the device asks for the grain
    {
        let mut fill = d.fill.lock().unwrap();
        let f = fill.as_mut().expect("scrubbing opened the output");
        let mut buf = vec![0.0; 4096 * 2];
        f(&mut buf, 2);
        assert!(buf.iter().any(|s| *s != 0.0), "the grain has sound");
    }
    d.frames(2);
    let db = d.left_db();
    assert!(db > -60.0, "the meter shows the grain: {db} dB");
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    d.frames(1);
}

/// A drag along a scrub bar moves the playhead on every frame; the output stream is opened once
/// for the whole drag and only re-aimed, as the module promises, not stopped and reopened per
/// frame (which made a monitor's scrub bar stutter while its timecode scrubbed smoothly).
#[test]
fn dragging_a_scrub_bar_keeps_one_output_stream_open() {
    for id in ["program.scrubBar", "timeline.ruler"] {
        let mut d = Driver::demo();
        let bar = d.element(id).unwrap_or_else(|| panic!("{id}"));
        let r = &bar["rect"];
        let (x, y, w) = (r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32 + r[3].as_f64().unwrap() as f32 / 2.0, r[2].as_f64().unwrap() as f32);
        let at = |f: f32| egui::pos2(x + w * f, y);
        d.harness.input_mut().events.push(egui::Event::PointerMoved(at(0.2)));
        d.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: at(0.2),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        d.frames(2);
        let before = d.starts.load(Ordering::Relaxed);
        for i in 1..=8 {
            d.harness.input_mut().events.push(egui::Event::PointerMoved(at(0.2 + 0.03 * i as f32)));
            d.frames(2);
        }
        {
            let mut fill = d.fill.lock().unwrap();
            let f = fill.as_mut().unwrap_or_else(|| panic!("{id}: the output is open during the drag"));
            let mut buf = vec![0.0; 4096 * 2];
            f(&mut buf, 2);
            assert!(buf.iter().any(|s| *s != 0.0), "{id}: the drag plays a grain");
        }
        d.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: at(0.44),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        d.frames(1);
        let starts = d.starts.load(Ordering::Relaxed);
        assert!(starts >= 1, "{id}: the drag opened the output");
        assert!(starts - before.min(starts) <= 1, "{id}: the output was restarted {} times during the drag", starts - before.min(starts));
    }
}

/// A take can record while playback is stopped (`audio.voiceover.start` is a command like any
/// other); scrubbing a bar ends it, as every other way of stopping does.
#[test]
fn a_scrub_ends_a_take_that_records_while_stopped() {
    for id in ["program.scrubBar", "timeline.ruler"] {
        let mut d = Driver::demo();
        d.ok("engine.execute", json!({"command": "audio.voiceover.start", "params": {"track": "A1", "time": 0, "preroll": 0}}));
        d.frames(2);
        assert!(d.harness.state().session.voiceover.recording(), "{id}: the take records");
        assert_eq!(d.ok("ui.inspect", json!({}))["playback"]["playing"], json!(false), "{id}: nothing plays");
        let bar = d.element(id).unwrap_or_else(|| panic!("{id}"));
        let r = &bar["rect"];
        let (x, y, w) = (r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32 + r[3].as_f64().unwrap() as f32 / 2.0, r[2].as_f64().unwrap() as f32);
        let at = |f: f32| egui::pos2(x + w * f, y);
        d.harness.input_mut().events.push(egui::Event::PointerMoved(at(0.2)));
        d.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: at(0.2),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        d.frames(2);
        d.harness.input_mut().events.push(egui::Event::PointerMoved(at(0.3)));
        d.frames(2);
        assert!(!d.harness.state().session.voiceover.recording(), "{id}: the scrub ended the take: {}", d.harness.state().ui.status);
        d.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: at(0.3),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        d.frames(1);
    }
}
