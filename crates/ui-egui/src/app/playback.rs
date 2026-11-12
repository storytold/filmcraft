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
        self.frames.play_starts();
        self.playback.speed = speed;
        self.playback.anchor_tick = self.session.playhead();
        self.playback.anchor_time = -1.0; // set when the preroll ends
        self.playback.smooth_playhead = None;
        self.playback.preroll = Some(-1.0);
        self.playback.preroll_ready = false;
        self.playback.preroll_pending = false;
        self.playback.preroll_revision = None;
        // The scrub output gives its device back first (#2633): playback's stream opens below, and
        // waiting for the scrub stream to close belongs here too, not at the moment the clock starts.
        self.release_scrub_output();
        if let Some(a) = self.audio.as_mut() {
            a.stop();
            // the device opens now, while the preroll decodes and nothing is due on screen
            a.prepare();
        }
        self.playback.audio_clock = false;
        self.playback.audio_retry_at = 0.0;
    }

    /// Start the clock (and audio) once the first frames are ready or the preroll timed out.
    fn end_preroll(&mut self, now: f64) {
        self.playback.preroll = None;
        if let Some(g) = self.gpu.as_mut() {
            g.compositor.wake_done();
        }
        self.playback.anchor_time = now;
        self.playback.anchor_tick = self.session.playhead();
        self.playback.audio_seen = (0, now);
        let t0 = web_time::Instant::now();
        self.start_audio(now);
        let ms = t0.elapsed().as_secs_f64() * 1e3;
        if ms > 4.0 {
            log::info!("audio start took {ms:.1} ms on the UI thread");
        }
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
        self.release_scrub_output();
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
        if let Some(out) = self.scrub_out.as_mut() {
            out.stop();
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

    /// Give the scrub output's device back before playback opens its own stream (#2633). The scrub
    /// stream lingers for a few seconds after the last grain, and a device that takes one stream
    /// at a time (a raw ALSA `hw:` device, an exclusive-mode output) would refuse playback's.
    fn release_scrub_output(&mut self) {
        if let Some(out) = self.scrub_out.as_mut() {
            out.release();
            self.scrub.device_stopped();
        }
    }

    pub(super) fn start_audio(&mut self, now: f64) {
        let Some(seq_id) = self.session.state.active_sequence else { return };
        let start_tick = self.session.playhead();
        self.release_scrub_output();
        let Some(a) = self.audio.as_mut() else { return };
        let sr = a.sample_rate();
        // Output frames still follow the accelerated timeline. The pitch preference selects
        // either the original rate conversion or pitch-corrected program audio.
        let t0 = web_time::Instant::now();
        let mixed = shuttle::mix(&self.session, seq_id, sr, self.playback.speed);
        let mix_ms = t0.elapsed().as_secs_f64() * 1e3;
        let Some((mix_rate, mix)) = mixed else {
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
        let t1 = web_time::Instant::now();
        let started = a.start(fill);
        let start_ms = t1.elapsed().as_secs_f64() * 1e3;
        if mix_ms + start_ms > 4.0 {
            log::info!("audio: mixer built in {mix_ms:.1} ms, stream started in {start_ms:.1} ms");
        }
        match started {
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

    /// The smoothed playback clock at `clock`: the last one moved on by the wall time since,
    /// plus a tenth of how far that is from the clock, at most [`SMOOTH_STEP_S`] a pass. It never
    /// steps back against the direction of play; a jump (loop, seek) more than a quarter second
    /// away is taken at once. The Timeline draws it, and the frame shown follows it too
    /// ([`Self::advance_playback`]): the device clock advances in buffer-sized steps (10 ms at
    /// 512 frames) and stands still while a late callback catches up, and the frame due by it,
    /// sampled at the display's refresh, was now and then skipped or held.
    fn smooth_playhead(&self, clock: Tick, now: f64) -> Tick {
        let Some((drawn, at)) = self.playback.smooth_playhead else { return clock };
        let speed = self.playback.speed;
        let predicted = drawn.seconds() + (now - at).max(0.0) * speed;
        let error = clock.seconds() - predicted;
        if !predicted.is_finite() || error.abs() > 0.25 {
            return clock;
        }
        // (capped: a pass that catches up more than that moves the clock past a whole 50 fps
        // frame in one 60 Hz refresh, and the frame is skipped)
        let next = predicted + (error * 0.1).clamp(-SMOOTH_STEP_S, SMOOTH_STEP_S);
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
            let timeout = if self.playback.preroll_pending { frames::PREROLL_MAX_S } else { frames::PREROLL_TIMEOUT_S };
            // ... and the GPU's clocks are up, or had their time ([`frames::WAKE_MAX_S`])
            let gpu_awake = self.gpu.as_ref().is_none_or(|g| g.compositor.wake_warm()) || now - since >= frames::WAKE_MAX_S;
            if (self.playback.preroll_ready && gpu_awake) || now - since >= timeout {
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
            let smooth = self.smooth_playhead(t, now);
            self.session.set_playhead(self.no_skip(smooth, rate));
            self.playback.smooth_playhead = Some((smooth, now));
        }
        ctx.request_repaint();
    }

    /// The playhead to show for the smoothed clock at `clock`: the clock, except that when a
    /// pass finds it two frames past the frame on screen (the pass came a refresh late: the
    /// desktop compositor held the previous frame, or the device clock jumped; or the frame due
    /// at the last pass was not ready and the one before stood in) the frame in between is
    /// shown first, one refresh late. Playing 50 fps at 60 Hz every sixth refresh repeats a
    /// frame, so the picture is back on the clock within a few refreshes; shown late is
    /// invisible where skipped is a hitch. Three or more frames on, the display cannot catch up
    /// a frame at a time: the clock wins and the frames are dropped. Forward play up to normal
    /// speed only; the first pass of a play (nothing shown yet) and a clock that is not moving
    /// forward show the clock's frame.
    fn no_skip(&self, clock: Tick, rate: filmcraft_time::FrameRate) -> Tick {
        if !(0.0 < self.playback.speed && self.playback.speed <= 1.0) || self.playback.smooth_playhead.is_none() {
            return clock;
        }
        // the frame on screen (the Program monitor's last picture, a stand-in included), else
        // the one the last pass asked for
        let on_screen = self.program_shown.map_or_else(|| rate.frame_at(self.session.playhead()), |k| k.frame);
        let due = rate.frame_at(clock);
        if due == on_screen.saturating_add(2) { rate.tick_of(on_screen.saturating_add(1)) } else { clock }
    }
}

/// The most the smoothed playback clock catches up with (or waits for) the device clock in one
/// pass ([`FilmcraftApp::smooth_playhead`]): 3 ms, so that a 60 Hz pass never moves it a whole
/// 50 fps frame (20 ms) on.
const SMOOTH_STEP_S: f64 = 0.003;

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

    /// The frame shown follows the smoothed clock. A device clock that stands still for a tenth
    /// of a second (its callback waited for a core) and then catches up at once used to hold the
    /// picture and then skip a frame or two; the picture now goes on at the clock's rate and no
    /// frame is skipped.
    #[test]
    fn a_stalled_device_clock_skips_no_frame() {
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
        let rate = app.session.sequence_rate();
        let mut frames = Vec::new();
        for pass in 0..180 {
            let now = pass as f64 / 60.0;
            // the counter stands still from 1.0 s to 1.1 s, then catches up at once
            let counted = if (1.0..1.1).contains(&now) { 1.0 } else { now };
            played.store((counted * 48_000.0 / 512.0).floor() as u64 * 512, std::sync::atomic::Ordering::Relaxed);
            app.set_now_for_test(now);
            app.advance_playback(&ctx);
            frames.push(rate.frame_at(app.session.playhead()));
        }
        for w in frames.windows(2) {
            assert!((0..=1).contains(&(w[1] - w[0])), "the frame shown went {} -> {}", w[0], w[1]);
        }
        let last = frames.last().copied().unwrap();
        let at_end = rate.frame_at(Tick::from_seconds_f64(179.0 / 60.0));
        assert!((at_end - 2..=at_end).contains(&last), "ended on frame {last} with the clock on {at_end}");
    }

    /// A pass that comes a refresh late (the desktop compositor held the previous frame) finds
    /// the clock two frames on. The frame in between is shown first, one refresh late, and the
    /// picture is back on the clock at the next refresh; the frame is not skipped. Three frames
    /// on at once, the clock wins.
    #[test]
    fn a_late_pass_shows_the_frame_it_would_skip_first() {
        let mut app = playing_at(0.0);
        let rate = app.session.sequence_rate();
        let frame = rate.frame_duration().seconds();
        let ctx = egui::Context::default();
        app.set_now_for_test(frame * 2.5);
        app.advance_playback(&ctx);
        assert_eq!(rate.frame_at(app.session.playhead()), 1, "frame 1 is shown late rather than never");
        assert_eq!(rate.frame_at(app.shown_playhead()), 2, "the drawn playhead follows the clock");
        app.set_now_for_test(frame * 2.9);
        app.advance_playback(&ctx);
        assert_eq!(rate.frame_at(app.session.playhead()), 2, "back on the clock at the next refresh");
        app.set_now_for_test(frame * 6.5);
        app.advance_playback(&ctx);
        assert_eq!(rate.frame_at(app.session.playhead()), 6, "three frames on at once: the clock wins");
    }

    /// The frame due at the last pass was not ready and the monitor drew the one before it
    /// again: the next pass shows it first, one refresh late, instead of moving on to the
    /// frame now due and never showing it.
    #[test]
    fn a_frame_that_stood_in_for_is_still_shown_before_the_next() {
        use crate::frames::{FrameKey, Target};
        let mut app = playing_at(0.0);
        let rate = app.session.sequence_rate();
        let frame = rate.frame_duration().seconds();
        let ctx = egui::Context::default();
        app.set_now_for_test(frame * 1.5);
        app.advance_playback(&ctx);
        assert_eq!(rate.frame_at(app.session.playhead()), 1);
        // the monitor drew frame 0 again: frame 1 was not ready
        let seq = app.session.state.active_sequence.unwrap();
        let drawn = FrameKey { target: Target::Sequence(seq), frame: 0, size: 1000, revision: 0, draft: false };
        app.program_shown = Some(drawn);
        app.set_now_for_test(frame * 2.5);
        app.advance_playback(&ctx);
        assert_eq!(rate.frame_at(app.session.playhead()), 1, "frame 1 is shown before frame 2");
        app.program_shown = Some(FrameKey { frame: 1, ..drawn });
        app.set_now_for_test(frame * 2.9);
        app.advance_playback(&ctx);
        assert_eq!(rate.frame_at(app.session.playhead()), 2, "back on the clock");
    }
}
