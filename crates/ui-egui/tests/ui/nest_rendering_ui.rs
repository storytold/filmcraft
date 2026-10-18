//! Headless UI test of how nested sequences are shown in the Timeline: the waveform on a nest's
//! sound clip.

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
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks and keys enter through the input hook
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

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

impl Driver {
    /// The cached waveform peaks of `item`, once `ready` accepts them (they are made in the
    /// background; drawing the Timeline asks for them).
    fn peaks_when(&mut self, item: u64, ready: impl Fn(&[(f32, f32)]) -> bool) -> Vec<(f32, f32)> {
        for _ in 0..400 {
            self.frames(1);
            let got = self.app().tl.peaks.lock().unwrap().get(&(ItemId(item), 0)).cloned();
            if let Some(p) = got.filter(|p| ready(p)) {
                return p.to_vec();
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("no waveform for item {item} that passes the check");
    }
}

fn loudest(peaks: &[(f32, f32)]) -> f32 {
    peaks.iter().fold(0.0, |m, (lo, hi)| m.max(lo.abs()).max(hi.abs()))
}

/// Premiere draws a waveform on the sound clip of a nest. Here it is the nested sequence's mix,
/// made in the background and made again when the sequence's sound changes.
#[test]
fn the_sound_clip_of_a_nest_gets_a_waveform_of_its_sequence() {
    let mut d = Driver::new();
    let outer = d.app().session.state.active_sequence.unwrap().0;
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[1].clone();
    d.exec("timeline.select", json!({"clips": [clip.id.0]}));
    let nested = d.exec("clip.nest", json!({"name": "Inner"}))["sequence"].as_u64().unwrap();
    // one peak per 256 samples at 48 kHz, for the nest's whole length, and not silent
    let peaks = d.peaks_when(nested, |p| !p.is_empty());
    let want = (clip.duration.seconds() * 48_000.0 / 256.0) as usize;
    assert!(peaks.len().abs_diff(want) <= 2, "{} peaks for {want}", peaks.len());
    assert!(loudest(&peaks) > 0.01, "the demo footage has sound");
    // turn the clip inside the nest off: the waveform follows
    d.exec("sequence.open", json!({"item": nested}));
    let sound = d.app().session.active_sequence().unwrap().audio_tracks[0].items[0].id.0;
    d.exec("timeline.select", json!({"clips": [sound]}));
    d.exec("clip.enable", json!({}));
    d.exec("sequence.open", json!({"item": outer}));
    let quiet = d.peaks_when(nested, |p| loudest(p) < 0.001);
    assert_eq!(quiet.len(), peaks.len());
}
