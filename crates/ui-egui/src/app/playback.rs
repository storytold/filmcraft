#[cfg(not(target_arch = "wasm32"))]
use super::play_ahead;
use super::{AUDIO_RETRY_S, AUDIO_STALL_S, FilmcraftApp, Tick, frames, json, panels, shuttle};
#[cfg(test)]
use filmcraft_engine::Session;

impl FilmcraftApp {
    // ---------------------------------------------------------------- playback

    pub fn toggle_play(&mut self, speed: f64) {
        if self.playback.playing {
            self.stop();
        } else {
            self.play(speed);
        }
    }

    pub fn play(&mut self, speed: f64) {
        self.stop_source();
        if self.session.active_sequence().is_none() {
            return;
        }
        // restart from the end → from the start (Settings ▸ Timeline ▸ "At playback end, return to
        // beginning when restarting playback")
        let dur = self.session.active_sequence().map(|q| q.duration()).unwrap_or_default();
        // (not while a voice-over take starts: it was put exactly where the take must begin, e.g. on empty timeline past the end)
        if speed > 0.0
            && self.session.prefs.timeline.return_to_beginning
            && !self.session.voiceover.recording()
            && self.session.playhead() >= dur - self.session.sequence_rate().frame_duration()
        {
            self.session.set_playhead(Tick::ZERO);
        }
        // A loop restart keeps counting into the same meter.
        if !self.playback.playing || self.playback.speed != speed {
            self.playback.meter.start(speed);
        }
        self.playback.playing = true;
        self.playback.speed = speed;
        self.playback.anchor_tick = self.session.playhead();
        self.playback.anchor_time = -1.0; // set when the preroll ends
        self.playback.smooth_playhead = None;
        self.playback.preroll = Some(-1.0);
        self.playback.preroll_ready = false;
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.playback.audio_clock = false;
        self.playback.audio_retry_at = 0.0;
    }

    /// Start the clock (and audio) once the first frames are ready or the preroll timed out.
    fn end_preroll(&mut self, now: f64) {
        self.playback.preroll = None;
        self.playback.anchor_time = now;
        self.playback.anchor_tick = self.session.playhead();
        self.playback.audio_seen = (0, now);
        self.start_audio(now);
        // Audio Track Mixer: an automation pass runs while playing forward in real time
        if (self.playback.speed - 1.0).abs() < 1e-9 && !self.session.mixrec.active() {
            let t = self.session.playhead();
            let _ = self.session.execute("mixer.recordStart", json!({"time": t.0}));
        }
        // Multi-Camera view: playing records live cuts (keys 1–9 / clicking angles)
        panels::multicam::on_play(self);
        // voice-over: the capture starts with the audio clock
        panels::voiceover::on_play(self);
    }

    /// Play the last Text to Speech preview (`Session::tts_preview`) on the audio output, stopping
    /// timeline playback first. Starting playback again takes the output back.
    pub fn play_tts_preview(&mut self) -> Result<f64, String> {
        let audio = self.session.tts_preview.clone().ok_or("nothing to preview")?;
        if self.playback.playing {
            self.stop();
        }
        let a = self.audio.as_mut().ok_or("no audio output (check Settings > Audio Hardware)")?;
        let sr = a.sample_rate().max(1);
        a.stop();
        let step = f64::from(audio.sample_rate.max(1)) / f64::from(sr);
        let seconds = audio.seconds();
        let mut pos = 0.0f64;
        let fill = Box::new(move |buf: &mut [f32], ch: usize| {
            for frame in buf.chunks_mut(ch.max(1)) {
                let i = pos as usize;
                let f = (pos - i as f64) as f32;
                let x0 = audio.samples.get(i).copied().unwrap_or(0.0);
                let x1 = audio.samples.get(i + 1).copied().unwrap_or(0.0);
                frame.fill(x0 + (x1 - x0) * f);
                pos += step;
            }
        });
        a.start(fill)?;
        self.playback.audio_clock = false;
        self.playback.tts_preview = true;
        Ok(seconds)
    }

    /// Whether [`FilmcraftApp::stop`] has a transport to stop: Program or Source playback, a
    /// preroll, or a recording that runs one. A press that only moves the playhead uses this so it
    /// does not close an audio stream that is already open (#2633): dropping the output stream
    /// makes the next scrub grain open the device again, and on WASAPI that open is slow.
    pub fn transport_running(&self) -> bool {
        self.playback.playing
            // A panel that panicked clears `playing` but leaves the output playing, and the clock
            // stays on: the press still has that stream to stop.
            || self.playback.audio_clock
            || self.source_playback.clock.playing
            || self.playback.preroll.is_some()
            || self.session.mixrec.active()
            || self.session.mcrec.active()
            || self.session.voiceover.recording()
    }

    pub fn stop(&mut self) {
        self.playback.playing = false;
        self.playback.tts_preview = false;
        self.playback.stop_at = None;
        self.playback.rest_at = None;
        self.playback.preroll = None;
        self.playback.meter.finish();
        self.frames.stop_prefetch();
        if let Some(a) = self.audio.as_mut() {
            a.stop();
        }
        self.scrub.device_stopped();
        self.playback.audio_clock = false;
        self.playback.audio_retry_at = 0.0;
        if self.session.mixrec.active() {
            let t = self.session.playhead();
            if let Err(e) = self.session.execute("mixer.recordStop", json!({"time": t.0})) {
                self.ui.status = e.to_string();
            }
        }
        panels::multicam::on_stop(self);
        panels::voiceover::on_stop(self);
    }

    /// [`stop`](Self::stop) for a press or drag that moves the playhead (a scrub bar, a ruler).
    /// With nothing playing or recording there is nothing to stop, and stopping the output would
    /// close the scrub stream (see `scrub.rs`) that the drag keeps open, once per move. A take
    /// that records while stopped and a Text to Speech preview still end here, as they do in `stop`.
    pub(crate) fn stop_for_scrub(&mut self) {
        if self.transport_running() || self.playback.tts_preview {
            self.stop();
        }
    }

    pub(super) fn start_audio(&mut self, now: f64) {
        let Some(seq_id) = self.session.state.active_sequence else { return };
        let start_tick = self.session.playhead();
        let Some(a) = self.audio.as_mut() else { return };
        let sr = a.sample_rate();
        // Output frames still follow the accelerated timeline. The pitch preference selects
        // either the original rate conversion or pitch-corrected program audio.
        let Some((mix_rate, mix)) = shuttle::mix(&self.session, seq_id, sr, self.playback.speed) else {
            a.stop();
            self.playback.audio_retry_at = 0.0;
            return;
        };
        let cursor = start_tick.to_units_floor(i64::from(mix_rate));
        // the old stream stops before the new mixer resets the underrun counters
        a.stop();
        // Desktop: mix ahead on a thread so the device callback never waits on decoding.
        #[cfg(not(target_arch = "wasm32"))]
        let fill = play_ahead::spawn(mix, cursor, sr, a.channels(), self.playback.audio_stats.clone());
        #[cfg(target_arch = "wasm32")]
        let fill = {
            let (mut mix, mut cursor) = (mix, cursor);
            Box::new(move |buf: &mut [f32], ch: usize| {
                mix(cursor, buf, ch);
                cursor += (buf.len() / ch.max(1)) as i64;
            })
        };
        match a.start(fill) {
            Ok(_) => {
                self.playback.audio_clock = true;
                self.playback.audio_retry_at = 0.0;
                if let Some(note) = self.audio.as_ref().and_then(|audio| audio.note()) {
                    self.ui.status = note;
                }
            }
            Err(e) => {
                log::warn!("audio output unavailable: {e}");
                self.playback.audio_clock = false;
                self.playback.audio_retry_at = now + AUDIO_RETRY_S;
            }
        }
    }

    /// Where the Timeline draws the playhead: the smoothed playback clock while playing, else the
    /// session playhead.
    pub(crate) fn shown_playhead(&self) -> Tick {
        match self.playback.smooth_playhead {
            Some((t, _)) if self.playback.playing => t,
            _ => self.session.playhead(),
        }
    }

    /// The drawn playhead for a playback clock at `clock`: the last drawn one moved on by the wall
    /// time since, plus a tenth of how far that is from the clock. It never steps back against
    /// the direction of play; a jump (loop, seek) more than a quarter second away is taken at once.
    fn smooth_playhead(&self, clock: Tick, now: f64) -> Tick {
        let Some((drawn, at)) = self.playback.smooth_playhead else { return clock };
        let speed = self.playback.speed;
        let predicted = drawn.seconds() + (now - at).max(0.0) * speed;
        let error = clock.seconds() - predicted;
        if !predicted.is_finite() || error.abs() > 0.25 {
            return clock;
        }
        let next = predicted + error * 0.1;
        let next = if speed >= 0.0 { next.max(drawn.seconds()) } else { next.min(drawn.seconds()) };
        Tick::from_seconds_f64(next)
    }

    /// Advance the Program playhead only while playing; owns preroll completion, audio-clock anchoring, output retry and the fallback to wall time when audio
    /// is lost or stalls.
    pub(super) fn advance_playback(&mut self, ctx: &egui::Context) {
        if !self.playback.playing {
            self.playback.smooth_playhead = None;
            return;
        }
        let now = self.now();
        if let Some(since) = self.playback.preroll {
            let since = if since < 0.0 { now } else { since };
            self.playback.preroll = Some(since);
            if self.playback.preroll_ready || now - since >= frames::PREROLL_TIMEOUT_S {
                self.end_preroll(now);
            } else {
                ctx.request_repaint();
                return;
            }
        }
        if self.playback.anchor_time < 0.0 {
            self.playback.anchor_time = now;
        }
        if !self.playback.audio_clock && self.playback.audio_retry_at > 0.0 && now >= self.playback.audio_retry_at {
            // The output failed mid-play: open it again where the wall clock got to (what
            // pause/play does by hand), so a recovering device brings the sound back.
            self.playback.audio_retry_at = now + AUDIO_RETRY_S;
            self.playback.anchor_tick = self.session.playhead();
            self.playback.anchor_time = now;
            self.playback.audio_seen = (0, now);
            self.start_audio(now);
        }
        let rate = self.session.sequence_rate();
        if self.playback.audio_clock
            && let Some(audio) = self.audio.as_mut()
        {
            audio.recover();
        }
        let reading = if self.playback.audio_clock { self.audio.as_ref().and_then(|a| a.played_frames().map(|f| (f, a.sample_rate()))) } else { None };
        if self.playback.audio_clock && reading.is_none() {
            // A device error relinquishes its clock immediately. Resume from the displayed
            // frame, rather than jumping to the old wall-clock anchor after buffered playback.
            self.playback.anchor_tick = self.session.playhead();
            self.playback.anchor_time = now;
            self.playback.audio_clock = false;
            self.playback.audio_retry_at = now + AUDIO_RETRY_S;
            if let Some(audio) = self.audio.as_mut() {
                audio.stop();
            }
            log::warn!("audio output lost its playback clock; playing without sound");
            self.ui.status = tl!("Audio output failed: playing without sound (check Settings > Audio Hardware)").into();
        }
        if let Some((f, sr)) = reading {
            if f != self.playback.audio_seen.0 {
                self.playback.audio_seen = (f, now);
            } else if now - self.playback.audio_seen.1 >= AUDIO_STALL_S {
                // the device stopped consuming samples: continue from where the audio got to on
                // the wall clock, without sound
                let played = if sr > 0 { f as f64 / sr as f64 } else { 0.0 };
                self.playback.anchor_tick += Tick::from_seconds_f64(played * self.playback.speed);
                self.playback.anchor_time = now;
                self.playback.audio_clock = false;
                if let Some(a) = self.audio.as_mut() {
                    a.stop();
                }
                log::warn!("audio output stalled (no samples consumed for {AUDIO_STALL_S} s); playing without sound");
                self.ui.status = tl!("Audio output is not responding: playing without sound (check Settings > Audio Hardware)").into();
            }
        }
        let elapsed = match reading {
            Some((f, sr)) if self.playback.audio_clock && sr > 0 => f as f64 / sr as f64,
            _ => now - self.playback.anchor_time,
        };
        let t = self.playback.anchor_tick + Tick::from_seconds_f64(elapsed * self.playback.speed);
        let seq = self.session.active_sequence();
        let dur = seq.map(|q| q.duration()).unwrap_or_default();
        let (lo, hi) = if self.playback.looping {
            (seq.and_then(|q| q.mark_in).unwrap_or(Tick::ZERO), seq.and_then(|q| q.mark_out).map(|o| o + rate.frame_duration()).unwrap_or(dur))
        } else {
            (Tick::ZERO, dur)
        };
        if let Some(end) = self.playback.stop_at.filter(|e| t >= *e && self.playback.speed > 0.0 && !self.playback.looping) {
            self.session.set_playhead(self.playback.rest_at.unwrap_or(end));
            self.stop();
        } else if t >= hi && self.playback.speed > 0.0 && (self.playback.looping || !self.session.voiceover.recording()) {
            // a voice-over take keeps playing (and recording) into the empty timeline past the end
            if self.playback.looping {
                self.session.set_playhead(lo);
                self.play(self.playback.speed);
            } else {
                self.session.set_playhead(hi - rate.frame_duration());
                self.stop();
            }
        } else if t <= Tick::ZERO && self.playback.speed < 0.0 {
            self.session.set_playhead(Tick::ZERO);
            self.stop();
        } else {
            self.session.set_playhead(t);
            self.playback.smooth_playhead = Some((self.smooth_playhead(t, now), now));
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;

    /// The egui clock starts at the integration's beginning, which can precede `clock_start`.
    /// Applying the captured offset to host time keeps hidden logic moving instead of freezing at
    /// the last UI time and then jumping when the next UI pass arrives.
    #[test]
    fn now_applies_the_ui_clock_offset_and_keeps_the_test_override_first() {
        let mut app = FilmcraftApp::new(Session::default());
        app.ui_clock = 123.0;
        app.ui_clock_offset = Some(1_000.0);
        assert!(app.now() >= 1_000.0, "the captured offset moves the host clock to egui time");
        app.ui_clock = 2_000.0;
        assert_eq!(app.now(), 2_000.0, "an egui clock ahead of host+offset still wins");
        app.set_now_for_test(7.0);
        assert_eq!(app.now(), 7.0, "the deterministic test override keeps precedence");
    }
}

#[cfg(test)]
mod audio_recovery_tests;

#[cfg(test)]
mod voiceover_playback_tests {
    use super::*;

    /// A voice-over take into an empty timeline past the end keeps playing and recording.
    #[test]
    fn a_voice_over_take_records_into_the_empty_timeline_past_the_end() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        session.execute("audio.voiceover.settings", json!({"prerollSeconds": 0.0})).unwrap();
        session.prefs.timeline.return_to_beginning = true;
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(session);
        app.audio = None;
        let dur = app.session.active_sequence().map(|q| q.duration()).unwrap();
        let from = app.session.sequence_rate().snap(dur + Tick::from_seconds_f64(60.0));
        app.session.set_playhead(from);
        panels::voiceover::toggle(&mut app, None).unwrap();
        // play() keeps the record point instead of returning to the beginning
        assert!(app.playback.playing);
        assert_eq!(app.session.playhead(), from);
        app.end_preroll(0.0);
        // and one second later the take is still playing and recording, not stopped at the end
        app.set_now_for_test(1.0);
        let mut output = ctx.run_ui(egui::RawInput { time: Some(1.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        output.textures_delta.clear();
        assert!(app.playback.playing);
        assert!(app.session.voiceover.recording());
        assert!(app.session.playhead() > from, "{:?} <= {from:?}", app.session.playhead());
        app.session.execute("audio.voiceover.stop", json!({"discard": true})).unwrap();
        app.stop();
    }
}

#[cfg(test)]
mod smooth_playhead_tests {
    use super::*;

    fn playing_at(now: f64) -> FilmcraftApp {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let mut app = FilmcraftApp::new(session);
        app.playback.playing = true;
        app.playback.speed = 1.0;
        app.playback.anchor_tick = Tick::ZERO;
        app.playback.anchor_time = 0.0;
        app.set_now_for_test(now);
        app.advance_playback(&egui::Context::default());
        app
    }

    /// The playhead line follows the playback clock between frame boundaries, so it glides at the
    /// display's refresh rate instead of jumping a whole frame at a time. The session playhead
    /// still names the frame shown.
    #[test]
    fn the_drawn_playhead_moves_between_frames_while_playing() {
        let frame = playing_at(0.0).session.sequence_rate().frame_duration().seconds();
        let mid = frame * 1.5;
        let app = playing_at(mid);
        assert_eq!(app.session.playhead(), app.session.sequence_rate().tick_of(1), "the frame shown");
        let drawn = app.shown_playhead().seconds();
        assert!((drawn - mid).abs() < 1e-6, "drawn at the clock time {mid}, not the frame boundary: {drawn}");
        let later = playing_at(mid + frame * 0.25).shown_playhead().seconds();
        assert!(later > drawn, "it keeps moving inside the same frame: {drawn} -> {later}");
    }

    /// A device that reports its clock once per 1024-sample buffer.
    struct BufferedDevice(std::sync::Arc<std::sync::atomic::AtomicU64>);

    impl crate::AudioOut for BufferedDevice {
        fn sample_rate(&self) -> u32 {
            48_000
        }
        fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
            Ok(48_000)
        }
        fn stop(&mut self) {}
        fn played_frames(&self) -> Option<u64> {
            Some(self.0.load(std::sync::atomic::Ordering::Relaxed))
        }
    }

    /// Played on the audio clock and drawn at 60 Hz, the playhead only ever moves forward, by
    /// about one display frame's worth of time each frame. It used to step back a few points and
    /// then jump ahead every half second or so.
    #[test]
    fn on_the_audio_clock_the_drawn_playhead_never_steps_back() {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let mut app = FilmcraftApp::new(session);
        let played = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        app.audio = Some(Box::new(BufferedDevice(played.clone())));
        app.playback.playing = true;
        app.playback.speed = 1.0;
        app.playback.audio_clock = true;
        app.playback.anchor_tick = Tick::ZERO;
        let ctx = egui::Context::default();
        let mut last: Option<f64> = None;
        for frame in 0..240 {
            let now = frame as f64 / 60.0;
            played.store((now * 48_000.0 / 1024.0).floor() as u64 * 1024, std::sync::atomic::Ordering::Relaxed);
            app.set_now_for_test(now);
            app.advance_playback(&ctx);
            let drawn = app.shown_playhead().seconds();
            if let Some(prev) = last.filter(|_| frame > 30) {
                let step = drawn - prev;
                assert!(step >= 0.0, "frame {frame}: stepped back {:.4} s ({prev:.4} -> {drawn:.4})", -step);
                assert!((0.5 / 60.0..1.5 / 60.0).contains(&step), "frame {frame}: uneven step {step:.4} s");
                assert!((drawn - now).abs() < 0.05, "frame {frame}: drawn {drawn:.4} drifted from the clock {now:.4}");
            }
            last = Some(drawn);
        }
    }

    /// Stopped, the drawn playhead is the session playhead.
    #[test]
    fn stopped_it_is_the_session_playhead() {
        let mut app = playing_at(0.1);
        app.stop();
        assert_eq!(app.shown_playhead(), app.session.playhead());
    }
}
