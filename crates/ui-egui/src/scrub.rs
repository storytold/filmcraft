//! Audio during scrubbing (#211): Settings ▸ Audio ▸ "Play audio while scrubbing in Source and
//! Program Monitors" (`audio.scrubAudio`, Shift+S).
//!
//! While the playhead moves with a mouse button down (the Timeline ruler, a monitor's scrub bar,
//! a dragged timecode) and playback is stopped, a short grain of the sequence's sound plays from
//! the new position. One output stream stays open for the whole scrub: each move only re-aims it,
//! so the device is not reopened every frame. The stream closes when the button is released and
//! the grain has played, when the setting is off, or when playback starts.

use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_time::Tick;

use crate::FilmcraftApp;

/// Length of the grain played after each move.
pub const GRAIN_S: f64 = 0.08;
/// Fade in / out at the grain's ends, so it doesn't click.
const RAMP_S: f64 = 0.004;
/// How long the stream stays open after the last move once the button is up.
const LINGER_S: f64 = 0.25;

/// What the audio callback plays next: `left` of `total` frames from `cursor`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Grain {
    cursor: i64,
    left: i64,
    total: i64,
}

#[derive(Default)]
pub struct ScrubAudio {
    grain: Arc<Mutex<Grain>>,
    /// The scrub stream is open.
    open: bool,
    last_tick: Option<Tick>,
    last_move: f64,
}

impl ScrubAudio {
    pub fn is_open(&self) -> bool {
        self.open
    }
}

impl FilmcraftApp {
    /// Once a frame: play a grain when the playhead moved under a held mouse button.
    pub(crate) fn scrub_audio(&mut self, ctx: &egui::Context) {
        let (now, down) = ctx.input(|i| (i.time, i.pointer.any_down()));
        if self.scrub_tick(now, down) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(LINGER_S));
        }
    }

    /// [`FilmcraftApp::scrub_audio`] with the clock and the button state given. Returns whether
    /// the scrub stream is still open.
    pub fn scrub_tick(&mut self, now: f64, down: bool) -> bool {
        let ph = self.session.playhead();
        let prev = self.scrub.last_tick.replace(ph);
        if self.playback.playing || self.source_playback.clock.playing {
            // playback (Program or Source) owns the device: it stopped the scrub stream when it
            // started, and the scrub must not stop it when the button is released
            self.scrub.open = false;
            return false;
        }
        let on = self.session.prefs.audio.scrub_audio;
        if on && down && prev.is_some_and(|p| p != ph) {
            self.scrub_grain(ph, now);
        }
        if self.scrub.open && (!on || (!down && now - self.scrub.last_move >= LINGER_S)) {
            if let Some(a) = self.audio.as_mut() {
                a.stop();
            }
            self.scrub.open = false;
        }
        self.scrub.open
    }

    /// Aim the scrub stream at `at` (opening it if needed) and play one grain.
    fn scrub_grain(&mut self, at: Tick, now: f64) {
        let Some(seq_id) = self.session.state.active_sequence else { return };
        let Some(a) = self.audio.as_ref() else { return };
        let sr = a.sample_rate();
        if sr == 0 {
            return;
        }
        let total = ((GRAIN_S * sr as f64) as i64).max(1);
        {
            let mut g = self.scrub.grain.lock().unwrap_or_else(PoisonError::into_inner);
            *g = Grain { cursor: at.to_units_floor(sr as i64), left: total, total };
        }
        self.scrub.last_move = now;
        if self.scrub.open {
            return;
        }
        let mix = crate::playback_mix(&self.session, seq_id, sr);
        let fill = grain_fill(mix, self.scrub.grain.clone(), ((RAMP_S * sr as f64) as i64).max(1));
        let Some(a) = self.audio.as_mut() else { return };
        a.stop();
        match a.start(fill) {
            Ok(_) => self.scrub.open = true,
            Err(e) => log::warn!("audio output unavailable for scrubbing: {e}"),
        }
    }
}

/// The scrub stream's callback: the rest of the current grain (faded at its ends), then silence.
fn grain_fill(mut mix: impl FnMut(i64, &mut [f32], usize) + Send + 'static, grain: Arc<Mutex<Grain>>, ramp: i64) -> Box<dyn FnMut(&mut [f32], usize) + Send> {
    Box::new(move |buf: &mut [f32], ch: usize| {
        let ch = ch.max(1);
        let n = (buf.len() / ch) as i64;
        let (g, take) = {
            let mut g = grain.lock().unwrap_or_else(PoisonError::into_inner);
            let now = *g;
            let take = g.left.clamp(0, n);
            g.cursor = g.cursor.saturating_add(take);
            g.left -= take;
            (now, take)
        };
        buf.fill(0.0);
        let Some(part) = buf.get_mut(..(take as usize).saturating_mul(ch)) else { return };
        if part.is_empty() {
            return;
        }
        mix(g.cursor, part, ch);
        let done = g.total - g.left;
        for (i, frame) in part.chunks_mut(ch).enumerate() {
            let pos = done + i as i64;
            let gain = (pos.min(g.total - 1 - pos) as f32 / ramp as f32).clamp(0.0, 1.0);
            frame.iter_mut().for_each(|s| *s *= gain);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(fill: &mut Box<dyn FnMut(&mut [f32], usize) + Send>, frames: usize) -> Vec<f32> {
        let mut buf = vec![9.0; frames * 2];
        fill(&mut buf, 2);
        buf
    }

    #[test]
    fn a_grain_plays_once_from_its_cursor_then_silence() {
        let grain = Arc::new(Mutex::new(Grain { cursor: 1000, left: 300, total: 300 }));
        let starts = Arc::new(Mutex::new(Vec::new()));
        let seen = starts.clone();
        let mix = move |c: i64, b: &mut [f32], _: usize| {
            seen.lock().unwrap().push(c);
            b.fill(1.0);
        };
        let mut fill = grain_fill(mix, grain.clone(), 10);
        let a = run(&mut fill, 256);
        let b = run(&mut fill, 256);
        let c = run(&mut fill, 256);
        assert_eq!(*starts.lock().unwrap(), vec![1000, 1256], "continues where it left off");
        assert_eq!(a[0], 0.0, "fades in");
        assert_eq!(a[200], 1.0);
        assert!(b[..44 * 2].iter().all(|s| *s < 1.0 + f32::EPSILON) && b[43 * 2 + 1] < 0.2, "fades out at its end");
        assert!(b[44 * 2..].iter().all(|s| *s == 0.0), "then silence");
        assert!(c.iter().all(|s| *s == 0.0));
        // a new move re-aims it
        *grain.lock().unwrap() = Grain { cursor: 5000, left: 300, total: 300 };
        run(&mut fill, 256);
        assert_eq!(starts.lock().unwrap().last(), Some(&5000));
    }

    type Fill = Box<dyn FnMut(&mut [f32], usize) + Send>;

    /// An output that keeps the callback it was started with.
    struct Out {
        fill: Arc<Mutex<Option<Fill>>>,
    }

    impl crate::AudioOut for Out {
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

    fn app(scrub_audio: bool) -> (FilmcraftApp, Arc<Mutex<Option<Fill>>>) {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        s.prefs.audio.scrub_audio = scrub_audio;
        let mut app = FilmcraftApp::new(s);
        let fill = Arc::new(Mutex::new(None));
        app.audio = Some(Box::new(Out { fill: fill.clone() }));
        (app, fill)
    }

    fn loudness(fill: &Arc<Mutex<Option<Fill>>>) -> f32 {
        let mut g = fill.lock().unwrap();
        let Some(f) = g.as_mut() else { return 0.0 };
        let mut buf = vec![0.0; 4096 * 2]; // more than a grain
        f(&mut buf, 2);
        buf.iter().map(|s| s.abs()).sum()
    }

    /// #211: moving the playhead with the button down plays the sequence's sound at the new spot;
    /// releasing the button closes the stream.
    #[test]
    fn scrubbing_plays_a_grain_and_stops_after_release() {
        let (mut app, fill) = app(true);
        let rate = app.session.sequence_rate();
        app.session.set_playhead(rate.tick_of(48));
        assert!(!app.scrub_tick(0.0, true), "pressing alone plays nothing");
        app.session.set_playhead(rate.tick_of(60));
        assert!(app.scrub_tick(0.02, true), "a move opens the stream");
        assert!(loudness(&fill) > 0.0, "the demo's music is heard");
        assert_eq!(loudness(&fill), 0.0, "one grain, then silence");
        app.session.set_playhead(rate.tick_of(72));
        assert!(app.scrub_tick(0.04, true));
        assert!(loudness(&fill) > 0.0, "the next move plays again on the same stream");
        assert!(app.scrub_tick(0.05, false), "lingers briefly after release");
        assert!(!app.scrub_tick(0.05 + LINGER_S, false), "then closes");
        assert!(fill.lock().unwrap().is_none());
    }

    #[test]
    fn nothing_plays_with_the_setting_off_or_while_playing() {
        let (mut app, fill) = app(false);
        let rate = app.session.sequence_rate();
        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(!app.scrub_tick(0.02, true));
        assert!(fill.lock().unwrap().is_none());

        let (mut app, _) = self::app(true);
        app.playback.playing = true;
        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(!app.scrub_tick(0.02, true), "playback owns the device");
    }

    /// Source monitor playback (added on main next to this feature) owns the device too: a scrub
    /// that was still lingering when Source playback started must not stop its sound on release.
    #[test]
    fn source_playback_owns_the_device() {
        let (mut app, fill) = app(true);
        let rate = app.session.sequence_rate();
        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(app.scrub_tick(0.02, true), "a grain opened the scrub stream");
        // Source playback starts (it replaces the stream with its own)
        app.source_playback.clock.playing = true;
        let source_fill: Fill = Box::new(|buf: &mut [f32], _| buf.fill(0.5));
        *fill.lock().unwrap() = Some(source_fill);
        assert!(!app.scrub_tick(0.05 + LINGER_S, false));
        assert!(fill.lock().unwrap().is_some(), "the Source playback stream is still playing");
        assert!(!app.scrub.is_open());
    }
}
