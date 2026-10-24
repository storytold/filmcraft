//! The Audio Meters panel, driven headless through the control channel: what it shows as it gets
//! narrower (the loudness readout and the dB scale give way, the bars stay), and that it moves
//! while the playhead is scrubbed with audio scrubbing on.

use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::dock::{DockNode, PanelKind, SplitSize};
use filmcraft_ui_egui::{AudioOut, FilmcraftApp};
use serde_json::{Value, json};

type Fill = Box<dyn FnMut(&mut [f32], usize) + Send>;

/// An output that keeps the callback it was started with, for the test to pull samples from.
struct Out {
    fill: Arc<Mutex<Option<Fill>>>,
}

impl AudioOut for Out {
    fn start(&mut self, fill: Fill) -> Result<u32, String> {
        *self.fill.lock().unwrap() = Some(fill);
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
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.prefs.audio.scrub_audio = true;
        let (tx, rx) = channel();
        let fill = Arc::new(Mutex::new(None));
        let mut app = FilmcraftApp::new(session).with_control(rx);
        app.audio = Some(Box::new(Out { fill: fill.clone() }));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, fill };
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
    /// Give the Audio Meters panel `width` points (it sits in a fixed-width split beside the
    /// Timeline).
    fn meters_width(&mut self, width: f32) {
        fn set(n: &mut DockNode, width: f32) -> bool {
            match n {
                DockNode::Split { size, b, .. } if matches!(&**b, DockNode::Tabs { panels, .. } if panels.as_slice() == [PanelKind::AudioMeters]) => {
                    *size = SplitSize::FixedB(width);
                    true
                }
                DockNode::Split { a, b, .. } => set(a, width) || set(b, width),
                DockNode::Tabs { .. } => false,
            }
        }
        assert!(set(&mut self.harness.state_mut().ui.dock, width));
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
