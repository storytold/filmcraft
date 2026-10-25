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

use crate::{FilmcraftApp, ScrubStart};

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
    /// The scrub stream's asynchronous startup has not completed.
    pending: bool,
    /// The sample rate the open or pending stream's grain and mixer were built for. The device
    /// open can negotiate a different rate; such a stream would play at the wrong speed.
    open_rate: u32,
    last_tick: Option<Tick>,
    last_move: f64,
}

impl ScrubAudio {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The output device was stopped under the stream: the next grain reopens it.
    pub(crate) fn device_stopped(&mut self) {
        self.open = false;
        self.pending = false;
    }
}

impl FilmcraftApp {
    /// Complete a pending scrub startup. A startup that finishes after the drag was released
    /// still plays its grain: the stream starts as the device opens, which on a slow open is
    /// after the release, so the linger is restarted here and the stream closes [`LINGER_S`]
    /// after its adoption instead of being dropped as stale in the same tick. A stream that
    /// opened at a different sample rate than its grain and mixer were built for is not
    /// adopted: it would play at the wrong speed, so it is stopped and the next move reopens
    /// at the device's actual rate.
    fn poll_scrub_start(&mut self, now: f64) {
        if !self.scrub.pending {
            return;
        }
        let Some(audio) = self.audio.as_mut() else { return };
        match audio.poll_start() {
            ScrubStart::Pending => {}
            ScrubStart::Ready => {
                self.scrub.pending = false;
                if audio.sample_rate() == self.scrub.open_rate {
                    self.scrub.open = true;
                    self.scrub.last_move = now;
                } else {
                    log::warn!("the scrub stream opened at {} Hz, not the {} Hz its grain was built for; reopening", audio.sample_rate(), self.scrub.open_rate);
                    audio.stop();
                }
            }
            ScrubStart::Cancelled => {
                // The output was stopped elsewhere while opening (playback took the device,
                // Settings ▸ Audio Hardware changed it): drop the startup without an error —
                // the user caused it — so the next move can open the device again.
                self.scrub.pending = false;
                self.scrub.open = false;
            }
            ScrubStart::Failed(e) => {
                self.scrub.pending = false;
                self.scrub.open = false;
                self.ui.status = tlf!("Audio output unavailable for scrubbing: {e}", e);
                log::warn!("audio output unavailable for scrubbing: {e}");
            }
        }
    }

    /// Once a frame: play a grain when the playhead moved under a held mouse button.
    pub(crate) fn scrub_audio(&mut self, ctx: &egui::Context) {
        let (now, down) = (self.now(), ctx.input(|i| i.pointer.any_down()));
        if self.scrub_tick(now, down) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(LINGER_S));
        } else if self.scrub.pending {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// `FilmcraftApp::scrub_audio` with the clock and the button state given. Returns whether
    /// the scrub stream is still open.
    pub fn scrub_tick(&mut self, now: f64, down: bool) -> bool {
        self.poll_scrub_start(now);
        let ph = self.session.playhead();
        let prev = self.scrub.last_tick.replace(ph);
        if self.playback.playing || self.source_playback.clock.playing {
            // playback (Program or Source) owns the device: it stopped the scrub stream when it
            // started, and the scrub must not stop it when the button is released
            self.scrub.open = false;
            self.scrub.pending = false;
            return false;
        }
        let on = self.session.prefs.audio.scrub_audio;
        if !on && self.scrub.pending {
            if let Some(a) = self.audio.as_mut() {
                a.stop();
            }
            self.scrub.pending = false;
        }
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
        if self.scrub.open || self.scrub.pending {
            return;
        }
        let mix = crate::playback_mix(&self.session, seq_id, sr);
        let fill = grain_fill(mix, self.scrub.grain.clone(), ((RAMP_S * sr as f64) as i64).max(1));
        let Some(a) = self.audio.as_mut() else { return };
        a.stop();
        match a.start_scrub(fill) {
            Ok(_) => {
                self.scrub.pending = true;
                self.scrub.open_rate = sr;
                self.poll_scrub_start(now);
            }
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

    struct DelayedState {
        starts: std::sync::atomic::AtomicUsize,
        stops: std::sync::atomic::AtomicUsize,
        waiting_fill: Mutex<Option<Fill>>,
        active_fill: Mutex<Option<Fill>>,
        completion: Mutex<Option<Result<(), String>>>,
        clock_lost: std::sync::atomic::AtomicBool,
        /// The rate the slow open negotiates (the device may differ from what was asked).
        rate: std::sync::atomic::AtomicU32,
    }

    impl Default for DelayedState {
        fn default() -> Self {
            Self {
                starts: std::sync::atomic::AtomicUsize::new(0),
                stops: std::sync::atomic::AtomicUsize::new(0),
                waiting_fill: Mutex::new(None),
                active_fill: Mutex::new(None),
                completion: Mutex::new(None),
                clock_lost: std::sync::atomic::AtomicBool::new(false),
                rate: std::sync::atomic::AtomicU32::new(48_000),
            }
        }
    }

    struct DelayedOut(Arc<DelayedState>);

    impl crate::AudioOut for DelayedOut {
        fn start(&mut self, fill: Fill) -> Result<u32, String> {
            *self.0.active_fill.lock().unwrap() = Some(fill);
            Ok(48_000)
        }
        fn start_scrub(&mut self, fill: Fill) -> Result<u32, String> {
            self.0.starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.0.waiting_fill.lock().unwrap() = Some(fill);
            *self.0.completion.lock().unwrap() = None;
            Ok(48_000)
        }
        fn poll_start(&mut self) -> ScrubStart {
            if let Some(result) = self.0.completion.lock().unwrap().take() {
                if result.is_ok() {
                    *self.0.active_fill.lock().unwrap() = self.0.waiting_fill.lock().unwrap().take();
                } else {
                    self.0.waiting_fill.lock().unwrap().take();
                }
                return match result {
                    Ok(()) => ScrubStart::Ready,
                    Err(e) => ScrubStart::Failed(e),
                };
            }
            if self.0.waiting_fill.lock().unwrap().is_some() {
                ScrubStart::Pending
            } else {
                // like CpalOut: a queued start that has vanished was cancelled by `stop`
                ScrubStart::Cancelled
            }
        }
        fn stop(&mut self) {
            self.0.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // like CpalOut::stop: a queued start is cancelled — it disappears without a result
            self.0.waiting_fill.lock().unwrap().take();
            self.0.completion.lock().unwrap().take();
            self.0.active_fill.lock().unwrap().take();
        }
        fn sample_rate(&self) -> u32 {
            self.0.rate.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn played_frames(&self) -> Option<u64> {
            (!self.0.clock_lost.load(std::sync::atomic::Ordering::SeqCst)).then_some(1)
        }
    }

    fn loudness_of(fill: &Mutex<Option<Fill>>) -> f32 {
        let mut g = fill.lock().unwrap();
        let Some(f) = g.as_mut() else { return 0.0 };
        let mut buf = vec![0.0; 4096 * 2];
        f(&mut buf, 2);
        buf.iter().map(|s| s.abs()).sum()
    }

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
    fn delayed_start_is_not_cancelled_by_moves_or_release() {
        use std::sync::atomic::Ordering;

        let (mut app, _) = app(true);
        let state = Arc::new(DelayedState::default());
        app.audio = Some(Box::new(DelayedOut(state.clone())));

        let rate = app.session.sequence_rate();
        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(!app.scrub_tick(0.02, true), "a queued startup is not an open stream");
        assert!(!app.scrub.is_open());
        assert!(app.scrub.pending);
        assert!(loudness_of(&state.waiting_fill) > 0.0, "a grain plays while startup is pending");

        app.session.set_playhead(rate.tick_of(72));
        assert!(!app.scrub_tick(0.04, true));
        assert_eq!(state.starts.load(Ordering::SeqCst), 1, "later moves retain the pending request");
        assert_eq!(state.stops.load(Ordering::SeqCst), 1, "later moves do not cancel startup");
        assert!(loudness_of(&state.waiting_fill) > 0.0, "later moves re-aim the pending stream's grain");

        assert!(!app.scrub_tick(0.05, false), "release leaves delayed startup alone");
        assert!(state.waiting_fill.lock().unwrap().is_some());
        assert_eq!(state.stops.load(Ordering::SeqCst), 1, "release does not cancel startup");

        app.session.set_playhead(rate.tick_of(84));
        assert!(!app.scrub_tick(0.06, true));
        *state.completion.lock().unwrap() = Some(Ok(()));
        assert!(app.scrub_tick(0.1, false), "completed startup is open during its linger");
        assert!(app.scrub.is_open());
        assert!(loudness_of(&state.active_fill) > 0.0, "the adopted stream plays the latest grain");
        assert!(!app.scrub_tick(0.1 + LINGER_S + 0.01, false), "the ready stream closes after the linger granted at adoption");
        assert_eq!(state.stops.load(Ordering::SeqCst), 2);
    }

    /// A slow open that completes after the drag was released still plays the drag's grain:
    /// adoption restarts the linger, instead of closing the just-opened stream as stale in the
    /// same tick and leaving the first grain of a short drag inaudible.
    #[test]
    fn a_slow_open_plays_the_released_drag_grain_instead_of_dropping_it() {
        let (mut app, _) = app(true);
        let state = Arc::new(DelayedState::default());
        app.audio = Some(Box::new(DelayedOut(state.clone())));
        let rate = app.session.sequence_rate();

        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(!app.scrub_tick(0.02, true), "the startup is pending");
        assert!(!app.scrub_tick(0.05, false), "release leaves the pending startup alone");

        // the device finishes its open long after the release
        *state.completion.lock().unwrap() = Some(Ok(()));
        assert!(app.scrub_tick(0.5, false), "the adopted stream lingers to play its grain");
        assert!(app.scrub.is_open());
        assert!(loudness_of(&state.active_fill) > 0.0, "the drag's grain is heard after the slow open");
        assert!(!app.scrub_tick(0.5 + LINGER_S, false), "then the stream closes");
    }

    #[test]
    fn delayed_start_failure_reports_error_and_the_next_move_retries() {
        crate::i18n::set_current(crate::i18n::Language::Es);
        let (mut app, _) = app(true);
        let state = Arc::new(DelayedState::default());
        app.audio = Some(Box::new(DelayedOut(state.clone())));
        let rate = app.session.sequence_rate();

        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(!app.scrub_tick(0.02, true));
        *state.completion.lock().unwrap() = Some(Err("startup failed".into()));
        assert!(!app.scrub_tick(0.03, false));
        assert!(!app.scrub.is_open());
        assert!(!app.scrub.pending);
        assert!(app.ui.status.contains("startup failed"));
        assert!(app.ui.status.starts_with("Salida de audio no disponible al arrastrar:"));

        app.session.set_playhead(rate.tick_of(72));
        assert!(!app.scrub_tick(0.04, true));
        assert_eq!(state.starts.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(app.scrub.pending, "a later move retries after a failed startup");
        crate::i18n::set_current(crate::i18n::Language::En);
    }

    /// The output can be stopped elsewhere while a scrub startup is pending — Settings ▸ Audio
    /// Hardware changed, playback took the device. The cancelled startup must not pin the scrub:
    /// it is forgotten (without an error the user cannot act on) and the next move reopens the
    /// device instead of `pending` staying set forever.
    #[test]
    fn a_startup_cancelled_by_an_external_stop_is_forgotten_and_the_next_move_retries() {
        use std::sync::atomic::Ordering;

        let (mut app, _) = app(true);
        let state = Arc::new(DelayedState::default());
        app.audio = Some(Box::new(DelayedOut(state.clone())));
        let rate = app.session.sequence_rate();
        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(!app.scrub_tick(0.02, true));
        assert!(app.scrub.pending, "the scrub startup is pending");

        app.audio.as_mut().unwrap().stop();
        assert!(!app.scrub_tick(0.03, true));
        assert!(!app.scrub.pending, "the cancelled startup no longer pins the scrub");
        assert!(!app.scrub.is_open());
        assert!(!app.ui.status.contains("unavailable"), "a stop the user caused is not an error");

        app.session.set_playhead(rate.tick_of(72));
        assert!(!app.scrub_tick(0.04, true));
        assert_eq!(state.starts.load(Ordering::SeqCst), 2, "the next move opens the device again");
        assert!(app.scrub.pending);
        *state.completion.lock().unwrap() = Some(Ok(()));
        assert!(app.scrub_tick(0.06, true), "the retried startup is adopted");
        assert!(app.scrub.is_open());
    }

    /// The queued open can negotiate a rate other than the one the grain and mixer were built
    /// for (a changed or reconfigured device): that stream would play its grain at the wrong
    /// speed. It is dropped at adoption, and the next move reopens at the device's actual rate.
    #[test]
    fn a_stream_opened_at_a_different_rate_is_dropped_and_the_next_move_reopens() {
        use std::sync::atomic::Ordering;

        let (mut app, _) = app(true);
        let state = Arc::new(DelayedState::default());
        app.audio = Some(Box::new(DelayedOut(state.clone())));
        let rate = app.session.sequence_rate();
        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(!app.scrub_tick(0.02, true));
        assert!(app.scrub.pending);

        // the device completes its slow open at 44.1 kHz, not the 48 kHz the grain was built for
        state.rate.store(44_100, Ordering::SeqCst);
        *state.completion.lock().unwrap() = Some(Ok(()));
        assert!(!app.scrub_tick(0.03, false), "the mis-rate stream is not adopted");
        assert!(!app.scrub.is_open());
        assert!(!app.scrub.pending);
        assert_eq!(state.stops.load(Ordering::SeqCst), 2, "it is stopped at once");

        app.session.set_playhead(rate.tick_of(72));
        assert!(!app.scrub_tick(0.04, true));
        assert_eq!(state.starts.load(Ordering::SeqCst), 2, "the next move reopens the device");
        *state.completion.lock().unwrap() = Some(Ok(()));
        assert!(app.scrub_tick(0.05, true), "the stream opened at the aimed rate is adopted");
        assert!(app.scrub.is_open());
    }

    /// Changing Settings ▸ Audio Hardware stops the output: an open scrub must not keep
    /// re-aiming the dead stream — the next move opens the new device.
    #[test]
    fn changing_audio_hardware_resets_an_open_scrub_and_the_next_move_reopens() {
        use std::sync::atomic::Ordering;

        let (mut app, _) = app(true);
        let state = Arc::new(DelayedState::default());
        app.audio = Some(Box::new(DelayedOut(state.clone())));
        let rate = app.session.sequence_rate();
        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(!app.scrub_tick(0.02, true), "the startup is pending");
        *state.completion.lock().unwrap() = Some(Ok(()));
        assert!(app.scrub_tick(0.03, true));
        assert!(app.scrub.is_open());

        app.session.prefs.audio_hardware.default_output = "another device".into();
        app.apply_prefs(&egui::Context::default());
        assert!(!app.scrub.is_open(), "the stopped stream is not left open");

        app.session.set_playhead(rate.tick_of(72));
        assert!(!app.scrub_tick(0.03, true));
        assert_eq!(state.starts.load(Ordering::SeqCst), 2, "the next move opens the new device");
    }

    #[test]
    fn playback_clock_loss_clears_scrub_and_the_next_move_reopens_output() {
        use std::sync::atomic::Ordering;

        let (mut app, _) = app(true);
        let state = Arc::new(DelayedState::default());
        app.audio = Some(Box::new(DelayedOut(state.clone())));
        let rate = app.session.sequence_rate();
        app.scrub_tick(0.0, true);
        app.session.set_playhead(rate.tick_of(60));
        assert!(!app.scrub_tick(0.02, true));
        *state.completion.lock().unwrap() = Some(Ok(()));
        assert!(app.scrub_tick(0.03, true));
        assert!(app.scrub.is_open());

        app.playback.playing = true;
        app.playback.speed = 1.0;
        app.playback.audio_clock = true;
        app.playback.anchor_time = 0.0;
        state.clock_lost.store(true, Ordering::SeqCst);
        app.advance_playback(&egui::Context::default());
        assert!(!app.playback.audio_clock);
        assert!(!app.scrub.is_open(), "clock loss invalidates the scrub stream");
        assert!(!app.scrub.pending);

        app.playback.playing = false;
        state.clock_lost.store(false, Ordering::SeqCst);
        app.session.set_playhead(rate.tick_of(72));
        assert!(!app.scrub_tick(0.04, true));
        assert_eq!(state.starts.load(Ordering::SeqCst), 2, "the next scrub opens a new device stream");
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
