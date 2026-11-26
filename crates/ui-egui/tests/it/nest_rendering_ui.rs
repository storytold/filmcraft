//! Headless UI test of how nested sequences are shown in the Timeline: the waveform on a nest's
//! sound clip.

use crate::support::{Drive, Launch, driver};
use filmcraft_engine::project::ItemId;
use serde_json::json;

driver!(Driver);

impl Driver {
    /// The cached waveform peaks of `item`, once `ready` accepts them (they are made in the
    /// background; drawing the Timeline asks for them), for at most two minutes of wall-clock time.
    fn peaks_when(&mut self, item: u64, ready: impl Fn(&[(f32, f32)]) -> bool) -> Vec<(f32, f32)> {
        let t0 = std::time::Instant::now();
        while t0.elapsed().as_secs() < 120 {
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
    let mut d = Driver::demo();
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
