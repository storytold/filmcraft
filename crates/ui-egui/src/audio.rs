use super::*;

/// How long (s) the audio clock may stand still during playback before the wall clock takes over.
/// An output stream can open and then never call back (ALSA with a busy or misconfigured device,
/// #136); playback must not freeze on it. Devices that are slow to start (Bluetooth) stay well
/// under this.
pub const AUDIO_STALL_S: f64 = 1.0;

/// How often (s) playback re-opens the output after its stream or device failed, while playing
/// goes on silently on the wall clock. Reopening is what pause/play does by hand; doing it on a
/// timer brings the sound back when the device recovers (#2503).
pub const AUDIO_RETRY_S: f64 = 2.0;

/// Effective sample rate for shuttle playback. The audio device still runs at `device_rate`,
/// so at 2x its output samples represent twice the timeline duration. Reverse and
/// extreme shuttle speeds remain silent rather than asking the mixer for invalid rates.
pub(crate) fn shuttle_mix_rate(device_rate: u32, speed: f64) -> Option<u32> {
    if device_rate == 0 || !speed.is_finite() || !(0.5..=4.0).contains(&speed) {
        return None;
    }
    let rate = (f64::from(device_rate) / speed).round();
    if !(1.0..=f64::from(u32::MAX)).contains(&rate) {
        return None;
    }
    Some(rate as u32)
}

/// The program mix that playback plays: `mix(device_frame, interleaved, channels)` renders the
/// active sequence at device rate `sr` from `device_frame`, with the live project snapshot (edits
/// made while playing are heard), Output Mapping, the 5.1 mixdown and voice-over cues.
pub fn playback_mix(session: &Session, seq_id: filmcraft_project::ItemId, sr: u32) -> impl FnMut(i64, &mut [f32], usize) + Send + 'static {
    let project = session.project.clone();
    let provider = session.media.provider(project.clone(), session.services.clone());
    let previews = session.previews.clone();
    // Settings ▸ Audio Hardware ▸ Output Mapping
    let map = [session.prefs.audio_hardware.map_left, session.prefs.audio_hardware.map_right];
    // Preferences ▸ Audio ▸ 5.1 Mixdown Type: how a 5.1 Mix plays on a stereo device
    let mixdown = filmcraft_audio_dsp::channels::Mixdown::from_id(&session.prefs.audio.mixdown_type).unwrap_or_default();
    let cues = panels::voiceover::cues(session, sr);
    previews.live.publish_project(project.clone());
    let mut resampler = None;
    move |cursor: i64, buf: &mut [f32], ch: usize| {
        // the newest project snapshot: mixer moves and other edits are heard while playing
        let project = previews.live.project().filter(|p| p.sequence(seq_id).is_some()).unwrap_or_else(|| project.clone());
        let Some(seq) = project.sequence(seq_id) else {
            buf.fill(0.0);
            return;
        };
        let n = buf.len() / ch.max(1);
        let seq_sr = seq.settings.sample_rate;
        // a 5.1 Mix plays as six channels (L, R, C, LFE, Ls, Rs) on a device with at least six
        use filmcraft_audio_dsp::channels::Layout;
        let layout = if ch >= 6 && seq.settings.audio_master == filmcraft_project::AudioChannels::Surround51 { Layout::Surround51 } else { Layout::Stereo };
        let mix = if seq_sr == sr {
            previews.mix_layout(&project, seq_id, cursor, n, &provider, layout, mixdown)
        } else {
            // Mix at the sequence rate as one continuous stream, interpolated to the device rate
            // (started afresh when the rate or the channel layout changes while playing).
            let key = (seq_sr, layout);
            if resampler.as_ref().is_none_or(|(k, _)| *k != key) {
                resampler = Some((key, filmcraft_audio_dsp::resample::StreamResampler::new(seq_sr, sr)));
            }
            let Some((_, r)) = resampler.as_mut() else {
                buf.fill(0.0);
                return;
            };
            let channels = r.process(cursor, n, |s0, m| previews.mix_layout(&project, seq_id, s0, m, &provider, layout, mixdown).channels);
            filmcraft_frame::AudioBuffer { sample_rate: sr, channels }
        };
        if layout == Layout::Surround51 {
            buf.fill(0.0);
            for (i, frame) in buf.chunks_mut(ch).enumerate() {
                for (c, x) in frame.iter_mut().take(6).enumerate() {
                    *x = mix.channels[c][i];
                }
            }
        } else {
            filmcraft_engine::settings::map_output(&mix.channels[0], &mix.channels[1.min(mix.channels.len() - 1)], buf, ch, map);
        }
        panels::voiceover::mix_cues(buf, ch, cursor, &cues);
    }
}
