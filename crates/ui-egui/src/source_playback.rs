//! Source-file auditioning. Playback is presentation state: it never edits the project.

use std::sync::{Arc, Mutex};

use filmcraft_engine::clip_ops::source_view;
use filmcraft_project::{ItemId, ItemKind};
use filmcraft_render::SourceProvider;
use filmcraft_time::Tick;
use serde_json::json;

use crate::{AUDIO_RETRY_S, AUDIO_STALL_S, FilmcraftApp, Playback, shuttle};

#[derive(Default)]
pub struct SourcePlayback {
    pub clock: Playback,
    item: Option<ItemId>,
    shown: Tick,
    range_start: Option<Tick>,
    failure: Arc<Mutex<Option<String>>>,
}

/// Apply Source media's selected channels before auditioning on a stereo device. Unlike an
/// assigned mixer bus, eight media channels alone do not establish their speaker positions.
fn audition_stereo(
    mut sound: filmcraft_frame::AudioBuffer,
    channels: &[u16],
    speaker_mask: Option<u32>,
    explicit_71: bool,
    mixdown: filmcraft_audio_dsp::channels::Mixdown,
) -> filmcraft_frame::AudioBuffer {
    if !channels.is_empty() {
        sound.channels = channels.iter().filter_map(|c| sound.channels.get(usize::from(*c)).cloned()).collect();
    }
    // A decoder mask describes its original ordering; arbitrary Adaptive channel picks do
    // not inherit that ordering. Only an explicit 7.1 interpretation reassigns the speakers.
    let identity = channels.is_empty() || channels.iter().copied().eq(0..8);
    let known_71 = explicit_71 || identity && speaker_mask == Some(filmcraft_media::SPEAKER_MASK_7_1);
    if sound.channels.len() == 8 && !known_71 {
        sound.channels.truncate(2);
    }
    filmcraft_render::audio::to_layout(sound, filmcraft_audio_dsp::channels::Layout::Stereo, mixdown)
}

impl FilmcraftApp {
    pub fn toggle_source_play(&mut self) -> Result<(), String> {
        if self.source_playback.clock.playing {
            self.stop_source();
            Ok(())
        } else {
            self.play_source()
        }
    }

    pub fn play_source(&mut self) -> Result<(), String> {
        self.start_source_playback(None, None, 1.0)
    }

    /// J / K / L in the Source monitor: play at `speed` × real time (negative = backward) from the
    /// Source playhead. Sound plays at normal forward speed only, as in the Program monitor.
    pub fn shuttle_source(&mut self, speed: f64) -> Result<(), String> {
        if !speed.is_finite() || speed == 0.0 {
            return Err("Source shuttle speed must be a non-zero number".into());
        }
        self.start_source_playback(None, None, speed.clamp(-8.0, 8.0))
    }

    pub fn play_source_range(&mut self, from_playhead: bool, preroll: bool) -> Result<(), String> {
        let item = self.session.state.source_item.ok_or("Open a Source clip first")?;
        let view = source_view(&self.session, item).ok_or("Source item is unavailable")?;
        let range = view.selected_range();
        if range.duration.0 <= 0 {
            return Err("Source marked range is empty".into());
        }
        let seconds = |v: f64| Tick::from_seconds_f64(if v.is_finite() { v.clamp(0.0, 3600.0) } else { 0.0 });
        let start = if from_playhead {
            self.session.state.source_playhead
        } else if preroll {
            Tick(range.start.0.saturating_sub(seconds(self.session.prefs.playback.preroll_seconds).0)).max(view.start)
        } else {
            range.start
        };
        let stop =
            if preroll { Tick(range.end().0.saturating_add(seconds(self.session.prefs.playback.postroll_seconds).0)).min(view.end) } else { range.end() };
        if start >= stop {
            return Err("Source playhead is at or beyond the Out point".into());
        }
        self.start_source_playback(Some(start), Some(stop), 1.0)
    }

    fn start_source_playback(&mut self, start: Option<Tick>, stop: Option<Tick>, speed: f64) -> Result<(), String> {
        let item = self.session.state.source_item.ok_or("Open a media file in the Source monitor first")?;
        let view = source_view(&self.session, item).ok_or("Source item is unavailable")?;
        let pi = self.session.project.item(view.media).ok_or("Source playback currently supports media files, subclips and sequences")?;
        match &pi.kind {
            ItemKind::Media(media) => {
                if media.offline {
                    return Err("Source media is offline: use Link Media first".into());
                }
            }
            ItemKind::Sequence(_) => {}
            _ => return Err("Source playback currently supports media files, subclips and sequences".into()),
        }
        if view.end <= view.start {
            return Err("Source clip has no playable duration".into());
        }
        let looping = self.source_playback.clock.looping;
        let range = view.selected_range();
        if looping && range.duration.0 <= 0 {
            return Err("Source marked range is empty".into());
        }
        let first = if looping { range.start } else { view.start };
        let end = stop.unwrap_or(if looping { range.end() } else { view.end });
        self.stop();
        self.stop_source();
        let mut time = start.unwrap_or(self.session.state.source_playhead);
        // forward from the end (or before a looped range) starts over; backward runs from where it is
        if start.is_none() && speed > 0.0 && (time < first || time >= view.rate.snap(Tick(end.0.saturating_sub(view.rate.frame_duration().0))).max(first)) {
            time = first;
        }
        self.session.execute("source.setPlayhead", json!({"time": time.0})).map_err(|e| e.to_string())?;
        let time = self.session.state.source_playhead;
        self.source_playback.range_start = start;
        self.source_playback.item = Some(item);
        self.source_playback.shown = time;
        self.source_playback.clock = Playback { playing: true, speed, anchor_time: -1.0, anchor_tick: time, looping, stop_at: stop, ..Default::default() };
        self.source_playback.clock.meter.start(speed);
        self.start_source_audio(0.0);
        Ok(())
    }

    pub fn stop_source(&mut self) {
        if !self.source_playback.clock.playing {
            return;
        }
        self.source_playback.clock.playing = false;
        self.source_playback.clock.audio_clock = false;
        self.source_playback.clock.meter.finish();
        self.frames.stop_prefetch();
        if let Some(audio) = self.audio.as_mut() {
            audio.stop();
        }
    }

    fn start_source_audio(&mut self, now: f64) {
        self.source_playback.clock.audio_clock = false;
        if (self.source_playback.clock.speed - 1.0).abs() > 1e-9 {
            if let Some(audio) = self.audio.as_mut() {
                audio.stop();
            }
            self.source_playback.clock.audio_retry_at = 0.0;
            return;
        }
        let Some(item) = self.source_playback.item else { return };
        let Some(view) = source_view(&self.session, item) else { return };
        let Some(audio) = self.audio.as_mut() else { return };

        let sr = audio.sample_rate();
        if sr == 0 || audio.channels() == 0 {
            self.ui.status = "Source audio output has an invalid sample rate or channel count".into();
            return;
        }
        let Some(pi) = self.session.project.item(view.media) else { return };
        let fill: Box<dyn FnMut(&mut [f32], usize) + Send> = match &pi.kind {
            ItemKind::Media(media) => {
                if !media.info.has_audio() {
                    audio.stop();
                    self.source_playback.clock.audio_retry_at = 0.0;
                    return;
                }
                let project = self.session.project.clone();
                let provider = self.session.media.provider(project, self.session.services.clone());
                let Some(source) = provider.source(view.media) else {
                    self.ui.status = "Source audio is unavailable: check Link Media".into();
                    return;
                };
                let speaker_mask = provider.audio_channel_mask(view.media, 0);
                let explicit_71 = media.interpret.audio_channels.as_ref().is_some_and(|mapping| mapping.format == filmcraft_project::AudioChannels::Surround71);
                let channels = media.interpret.audio_channels.as_ref().and_then(|m| m.clips.first()).cloned().unwrap_or_default();
                let map = [self.session.prefs.audio_hardware.map_left, self.session.prefs.audio_hardware.map_right];
                let mixdown = filmcraft_audio_dsp::channels::Mixdown::from_id(&self.session.prefs.audio.mixdown_type).unwrap_or_default();
                // A seek gets its own failure slot: a late callback from the old stream cannot stop it.
                self.source_playback.failure = Arc::new(Mutex::new(None));
                let failure = self.source_playback.failure.clone();
                let end = self
                    .source_playback
                    .clock
                    .stop_at
                    .unwrap_or_else(|| if self.source_playback.clock.looping { view.selected_range().end() } else { view.end })
                    .to_units_floor(i64::from(sr));
                let mix = move |cursor: i64, buf: &mut [f32], ch: usize| {
                    buf.fill(0.0);
                    if ch == 0 || cursor >= end {
                        return;
                    }
                    let frames = (buf.len() / ch).min(usize::try_from(end.saturating_sub(cursor)).unwrap_or(0));
                    match source.audio(cursor, frames, sr) {
                        Ok(sound) => {
                            let sound = audition_stereo(sound, &channels, speaker_mask, explicit_71, mixdown);
                            let Some(left) = sound.channels.first() else { return };
                            let right = sound.channels.get(1).unwrap_or(left);
                            filmcraft_engine::settings::map_output(left, right, buf, ch, map);
                        }
                        Err(e) => {
                            *failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format!("Source audio decoding failed: {e}"));
                        }
                    }
                };

                let cursor = self.session.state.source_playhead.to_units_floor(i64::from(sr));

                audio.stop();
                #[cfg(not(target_arch = "wasm32"))]
                let fill = crate::play_ahead::spawn(mix, cursor, sr, audio.channels(), self.source_playback.clock.audio_stats.clone());
                #[cfg(target_arch = "wasm32")]
                let fill = {
                    let (mix, mut cursor) = (mix, cursor);
                    Box::new(move |buf: &mut [f32], ch: usize| {
                        mix(cursor, buf, ch);
                        cursor = cursor.saturating_add(i64::try_from(buf.len() / ch.max(1)).unwrap_or(0));
                    })
                };
                fill
            }
            ItemKind::Sequence(seq) => {
                if seq.audio_tracks.iter().all(|t| t.items.is_empty()) {
                    audio.stop();
                    self.source_playback.clock.audio_retry_at = 0.0;
                    return;
                }
                let Some((mix_rate, mix)) = shuttle::mix(&self.session, item, sr, self.source_playback.clock.speed) else {
                    audio.stop();
                    self.source_playback.clock.audio_retry_at = 0.0;
                    return;
                };
                let cursor = self.session.state.source_playhead.to_units_floor(i64::from(mix_rate));

                audio.stop();
                #[cfg(not(target_arch = "wasm32"))]
                let fill = crate::play_ahead::spawn(mix, cursor, sr, audio.channels(), self.source_playback.clock.audio_stats.clone());
                #[cfg(target_arch = "wasm32")]
                let fill = {
                    let (mut mix, mut cursor) = (mix, cursor);
                    Box::new(move |buf: &mut [f32], ch: usize| {
                        mix(cursor, buf, ch);
                        cursor = cursor.saturating_add(i64::try_from(buf.len() / ch.max(1)).unwrap_or(0));
                    })
                };
                fill
            }
            _ => return,
        };
        match audio.start(fill) {
            Ok(_) => {
                self.source_playback.clock.audio_clock = true;
                self.source_playback.clock.audio_retry_at = 0.0;
            }
            Err(e) => {
                self.ui.status = format!("Source audio output failed: {e}");
                self.source_playback.clock.audio_retry_at = now + AUDIO_RETRY_S;
            }
        }
    }

    /// Advance Source audition playback while playing; stop on a missing or unavailable item or a failure, and re-anchor when the Source playhead is moved
    /// externally.
    pub(crate) fn advance_source_playback(&mut self, ctx: &egui::Context) {
        if !self.source_playback.clock.playing {
            return;
        }
        let item = self.source_playback.item;
        let Some(view) = item.filter(|i| Some(*i) == self.session.state.source_item).and_then(|i| source_view(&self.session, i)) else {
            self.stop_source();
            return;
        };
        let error = self.source_playback.failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(error) = error {
            self.stop_source();
            self.ui.status = error;
            return;
        }
        let now = self.now();
        let time = self.session.state.source_playhead;
        if time != self.source_playback.shown {
            self.source_playback.clock.anchor_tick = time;
            self.source_playback.clock.anchor_time = now;
            self.source_playback.clock.audio_seen = (0, now);
            self.start_source_audio(now);
        }
        if !self.source_playback.clock.audio_clock && self.source_playback.clock.audio_retry_at > 0.0 && now >= self.source_playback.clock.audio_retry_at {
            // The output failed mid-play: open it again where the wall clock got to, so a
            // recovering device brings the sound back without a pause/play.
            self.source_playback.clock.audio_retry_at = now + AUDIO_RETRY_S;
            self.source_playback.clock.anchor_tick = time;
            self.source_playback.clock.anchor_time = now;
            self.source_playback.clock.audio_seen = (0, now);
            self.start_source_audio(now);
        }
        if self.source_playback.clock.audio_clock
            && let Some(audio) = self.audio.as_mut()
        {
            audio.recover();
        }
        let clock = &mut self.source_playback.clock;
        if clock.anchor_time < 0.0 {
            clock.anchor_time = now;
            clock.audio_seen = (0, now);
        }
        let reading = if clock.audio_clock { self.audio.as_ref().and_then(|a| a.played_frames().map(|n| (n, a.sample_rate()))) } else { None };
        if let Some((frames, _)) = reading
            && frames != clock.audio_seen.0
        {
            clock.audio_seen = (frames, now);
        }
        if clock.audio_clock && (reading.is_none() || now - clock.audio_seen.1 >= AUDIO_STALL_S) {
            clock.anchor_tick = time;
            clock.anchor_time = now;
            clock.audio_clock = false;
            // A dead stream is reopened on a timer; a stalled one keeps the wall clock (#136).
            if reading.is_none() {
                clock.audio_retry_at = now + AUDIO_RETRY_S;
            }
            if let Some(audio) = self.audio.as_mut() {
                audio.stop();
            }
            self.ui.status = "Source audio output stopped responding: playing without sound (check Settings > Audio Hardware)".into();
        }
        let elapsed = match reading {
            Some((frames, sr)) if clock.audio_clock && sr > 0 => frames as f64 / f64::from(sr),
            _ => (now - clock.anchor_time).max(0.0),
        };
        let next = Tick(clock.anchor_tick.0.saturating_add(Tick::from_seconds_f64(elapsed * clock.speed).0));
        if clock.speed < 0.0 {
            // backward: stops on the first frame (as the Program monitor stops at the start)
            let ended = next <= view.start;
            match self.session.execute("source.setPlayhead", json!({"time": next.max(view.start).0})) {
                Ok(_) => self.source_playback.shown = self.session.state.source_playhead,
                Err(e) => {
                    self.stop_source();
                    self.ui.status = e.to_string();
                    return;
                }
            }
            if ended {
                self.stop_source();
            }
            ctx.request_repaint();
            return;
        }
        let range = view.selected_range();
        let end = clock.stop_at.unwrap_or_else(|| if clock.looping { range.end() } else { view.end }).min(view.end);
        let ended = next >= end;
        if ended && clock.looping {
            let start = self.source_playback.range_start.unwrap_or(range.start).max(view.start);
            if let Err(e) = self.session.execute("source.setPlayhead", json!({"time":start.0})) {
                self.stop_source();
                self.ui.status = e.to_string();
                return;
            }
            let time = self.session.state.source_playhead;
            self.source_playback.shown = time;
            self.source_playback.clock.anchor_tick = time;
            self.source_playback.clock.anchor_time = now;
            self.source_playback.clock.audio_seen = (0, now);
            self.start_source_audio(now);
            ctx.request_repaint();
            return;
        }
        let next = if ended { Tick(end.0.saturating_sub(view.rate.frame_duration().0)).max(view.start) } else { next.max(view.start) };
        match self.session.execute("source.setPlayhead", json!({"time": next.0})) {
            Ok(_) => self.source_playback.shown = self.session.state.source_playhead,
            Err(e) => {
                self.stop_source();
                self.ui.status = e.to_string();
                return;
            }
        }
        if ended {
            self.stop_source();
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_audio_dsp::channels::Mixdown;
    use filmcraft_frame::AudioBuffer;
    use filmcraft_media::SPEAKER_MASK_7_1;

    fn discrete_source() -> AudioBuffer {
        AudioBuffer { sample_rate: 48_000, channels: (1..=8).map(|n| vec![n as f32 * 0.05; 2]).collect() }
    }

    #[test]
    fn source_audition_does_not_infer_unknown_or_wide_eight_channel_speakers() {
        for mask in [None, Some(0xff)] {
            let sound = audition_stereo(discrete_source(), &[], mask, false, Mixdown::FrontRear);
            assert_eq!(sound.channels, vec![vec![0.05; 2], vec![0.1; 2]], "mask {mask:?}");
            let selected = audition_stereo(discrete_source(), &[4, 5], mask, false, Mixdown::FrontRear);
            assert_eq!(selected.channels, vec![vec![0.25; 2], vec![0.3; 2]], "selected pair, mask {mask:?}");
        }
    }

    #[test]
    fn source_audition_folds_proven_or_manually_assigned_seven_one() {
        // PoolProvider supplies this same mask for a decoder's proven layout and the user's
        // explicit 7.1 interpretation. Generic assigned-bus conversion remains unchanged.
        for channels in [vec![], (0..8).collect()] {
            let sound = audition_stereo(discrete_source(), &channels, Some(SPEAKER_MASK_7_1), false, Mixdown::FrontRear);
            let expected = filmcraft_render::audio::to_layout(discrete_source(), filmcraft_audio_dsp::channels::Layout::Stereo, Mixdown::FrontRear);
            assert_eq!(sound.channels, expected.channels);
            assert!(sound.channels[0][0] > 0.05 && sound.channels[1][0] > 0.1);
        }
    }

    #[test]
    fn arbitrary_eight_channel_selection_does_not_inherit_a_decoder_speaker_mask() {
        let channels = [1, 0, 2, 3, 4, 5, 6, 7];
        let adaptive = audition_stereo(discrete_source(), &channels, Some(SPEAKER_MASK_7_1), false, Mixdown::FrontRear);
        assert_eq!(adaptive.channels, vec![vec![0.1; 2], vec![0.05; 2]]);
        let assigned = audition_stereo(discrete_source(), &channels, None, true, Mixdown::FrontRear);
        let mut selected = discrete_source();
        selected.channels.swap(0, 1);
        let expected = filmcraft_render::audio::to_layout(selected, filmcraft_audio_dsp::channels::Layout::Stereo, Mixdown::FrontRear);
        assert_eq!(assigned.channels, expected.channels, "explicit 7.1 assigns speaker meaning after selection");
    }
}
