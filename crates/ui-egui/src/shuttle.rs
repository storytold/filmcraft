//! Pitch-preserving shuttle playback. Pitch is corrected before changing the sample rate,
//! so faster playback does not first discard the upper part of the audible spectrum.

use filmcraft_audio_dsp::{AudioEffect, effects::PitchShifter, resample::StreamResampler};
use filmcraft_engine::Session;
use filmcraft_project::ItemId;

type Mixer = Box<dyn FnMut(i64, &mut [f32], usize) + Send>;
const BLOCK: usize = 1024;
const MAX_CHANNELS: usize = 64;
// Two shifters have at most 16384 frames of latency; leave room for priming and interpolation.
const FRAME_MARGIN: i128 = 65_536;

/// Returns `(mix_rate, mixer)` for shuttle playback at `speed`, or `None` if `speed` is outside
/// the supported range (0.5×–4×, forward or reverse) or the device rate is zero.
///
/// * **Forward** (`speed > 0`): the mixer renders at a reduced sample rate so the device plays
///   back faster; pitch correction is applied when the preference is set.
/// * **Reverse** (`speed < 0`): the device runs at `device_rate`; a `reverse_wrap` closure reads
///   blocks forward from the timeline, flips their frame order, and resamples to the output buffer.
pub(crate) fn mix(session: &Session, seq_id: ItemId, device_rate: u32, speed: f64) -> Option<(u32, Mixer)> {
    let rate = super::shuttle_mix_rate(device_rate, speed)?;
    if speed < 0.0 {
        // Reverse playback: device runs at `rate` == `device_rate`; the mixer reads blocks
        // backward from the timeline and flips them so the device hears the audio in reverse.
        let inner = super::playback_mix(session, seq_id, rate);
        return Some((rate, Box::new(reverse_wrap(inner, speed.abs()))));
    }
    if !session.prefs.audio.maintain_pitch_shuttling || rate == device_rate {
        return Some((rate, Box::new(super::playback_mix(session, seq_id, rate))));
    }
    // Slow playback needs headroom for the upward pitch correction before rate conversion.
    let internal_rate = if rate > device_rate { device_rate.checked_mul(2)? } else { device_rate };
    let semitones = (12.0 * (f64::from(rate) / f64::from(device_rate)).log2()) as f32;
    Some((rate, Box::new(corrected(super::playback_mix(session, seq_id, internal_rate), internal_rate, rate, semitones))))
}

/// Wraps a forward mixer so the device hears it playing backward.
///
/// The `fill` closure from `play_ahead` calls this with a *device-frame cursor* that starts at the
/// current timeline position and increases monotonically (device always consumes frames in order).
/// We record the cursor on the first call (`start`) and derive the timeline read position from it:
///
///   timeline_end = start − (cursor − start) × abs_speed
///
/// So as device time advances, the timeline retreats. Each block reads `in_frames` forward-in-time
/// ending at `timeline_end`, then reverses the frame order before writing to `buf`.
///
/// At 1× reverse: block N reads [timeline_end − N, timeline_end], flips → N device frames reversed.
/// At 2× reverse: block N reads 2N frames, flips, resamples to N device frames.
fn reverse_wrap(
    mut mix: impl FnMut(i64, &mut [f32], usize) + Send + 'static,
    abs_speed: f64,
) -> impl FnMut(i64, &mut [f32], usize) + Send {
    let mut start: Option<i64> = None;
    move |cursor: i64, buf: &mut [f32], ch: usize| {
        if ch == 0 || buf.is_empty() || !buf.len().is_multiple_of(ch) {
            buf.fill(0.0);
            return;
        }
        let out_frames = buf.len() / ch;
        // Record the cursor at the moment J is first pressed (= timeline position at that instant).
        // Reset if cursor jumps backward (seek or restart creates a fresh closure, but be safe).
        let s = match start {
            Some(s) if cursor >= s => s,
            _ => { start = Some(cursor); cursor }
        };
        // Map increasing device cursor → decreasing timeline position.
        let device_elapsed = cursor.saturating_sub(s);
        let timeline_retreat = (device_elapsed as f64 * abs_speed).round() as i64;
        let timeline_end = s.saturating_sub(timeline_retreat);
        if timeline_end <= 0 {
            // Reached or passed the start of the timeline.
            buf.fill(0.0);
            return;
        }
        // Read `in_frames` forward-in-time ending at `timeline_end`, then reverse them.
        let in_frames = ((out_frames as f64 * abs_speed).ceil() as usize).max(1);
        let read_start = timeline_end.saturating_sub(in_frames as i64).max(0);
        let actual_in = (timeline_end - read_start).max(0) as usize;
        if actual_in == 0 {
            buf.fill(0.0);
            return;
        }
        let mut fwd = vec![0.0f32; actual_in * ch];
        mix(read_start, &mut fwd, ch);
        // Reverse the frame order of `fwd` in-place.
        for i in 0..(actual_in / 2) {
            let j = actual_in - 1 - i;
            for c in 0..ch {
                fwd.swap(i * ch + c, j * ch + c);
            }
        }
        // Resample the available reversed frames at the requested speed, then pad with silence.
        // Near timeline zero actual_in < in_frames; stretching partial frames across all
        // out_frames would slow the audio. Instead output only the proportional number of
        // frames and fill the remainder with silence.
        let available_out = ((actual_in as f64 / abs_speed).round() as usize).min(out_frames);
        if available_out == 0 {
            buf.fill(0.0);
            return;
        }
        if actual_in == available_out {
            buf[..available_out * ch].copy_from_slice(&fwd);
        } else {
            let scale = (actual_in - 1).max(1) as f64 / available_out.max(1) as f64;
            for i in 0..available_out {
                let src = i as f64 * scale;
                let lo = src as usize;
                let hi = (lo + 1).min(actual_in - 1);
                let t = (src - lo as f64) as f32;
                for c in 0..ch {
                    let s0 = fwd[lo * ch + c];
                    let s1 = fwd[hi * ch + c];
                    buf[i * ch + c] = s0 + (s1 - s0) * t;
                }
            }
        }
        buf[available_out * ch..].fill(0.0);
    }
}

fn corrected(mix: impl FnMut(i64, &mut [f32], usize) + Send + 'static, from: u32, to: u32, semitones: f32) -> impl FnMut(i64, &mut [f32], usize) + Send {
    let mut source = PitchedStream { mix: Box::new(mix), rate: from, semitones, channels: 0, stages: Vec::new(), latency: 0, next: None };
    let mut resampler = StreamResampler::new(from, to);
    move |cursor, buf: &mut [f32], ch| {
        if ch == 0 || ch > MAX_CHANNELS || !buf.len().is_multiple_of(ch) {
            buf.fill(0.0);
            return;
        }
        let n = buf.len() / ch;
        if n == 0 {
            return;
        }
        // StreamResampler uses i64 source coordinates. Reject extremes before its interpolation
        // and before the pitch stream's pre-roll/lookahead arithmetic can overflow.
        let first = (i128::from(cursor) * i128::from(from)).div_euclid(i128::from(to.max(1)));
        let last = ((i128::from(cursor) + n as i128) * i128::from(from)).div_euclid(i128::from(to.max(1)));
        if first < i128::from(i64::MIN) + FRAME_MARGIN || last > i128::from(i64::MAX) - FRAME_MARGIN || i128::from(cursor) + n as i128 > i128::from(i64::MAX) {
            buf.fill(0.0);
            source.next = None;
            resampler = StreamResampler::new(from, to);
            return;
        }
        if source.channels != ch {
            source.configure(ch);
            resampler = StreamResampler::new(from, to);
        }
        for (b, chunk) in buf.chunks_mut(BLOCK * ch).enumerate() {
            let start = cursor + (b * BLOCK) as i64;
            let planar = resampler.process(start, chunk.len() / ch, |s, frames| source.pull(s, frames));
            for (i, frame) in chunk.chunks_exact_mut(ch).enumerate() {
                for (sample, plane) in frame.iter_mut().zip(&planar) {
                    *sample = plane.get(i).copied().filter(|x| x.is_finite()).unwrap_or(0.0);
                }
            }
        }
    }
}

struct PitchedStream {
    mix: Mixer,
    rate: u32,
    semitones: f32,
    channels: usize,
    stages: Vec<PitchShifter>,
    latency: usize,
    // Expected next source output frame; mixer input is latency frames ahead of it.
    next: Option<i64>,
}

impl PitchedStream {
    fn configure(&mut self, channels: usize) {
        self.channels = channels;
        self.stages.clear();
        let mut remaining = self.semitones;
        // The existing effect supports ±12 semitones. Two stages cover 4x (-24 semitones).
        for _ in 0..2 {
            let shift = remaining.clamp(-12.0, 12.0);
            let mut stage = PitchShifter::new(self.rate as f32, channels);
            stage.set_param("semitones", shift);
            stage.reset();
            self.stages.push(stage);
            remaining -= shift;
            if remaining.abs() < 1e-4 {
                break;
            }
        }
        self.latency = self.stages.iter().map(AudioEffect::latency).sum();
        self.next = None;
    }

    fn process(&mut self, start: i64, n: usize) -> Vec<Vec<f32>> {
        let mut interleaved = vec![0.0; n * self.channels];
        (self.mix)(start, &mut interleaved, self.channels);
        let mut planar = vec![vec![0.0; n]; self.channels];
        for (i, frame) in interleaved.chunks_exact(self.channels).enumerate() {
            for (plane, sample) in planar.iter_mut().zip(frame) {
                plane[i] = if sample.is_finite() { *sample } else { 0.0 };
            }
        }
        let mut lanes: Vec<_> = planar.iter_mut().map(Vec::as_mut_slice).collect();
        for stage in &mut self.stages {
            stage.process(&mut lanes);
        }
        planar
    }

    fn pull(&mut self, start: i64, n: usize) -> Vec<Vec<f32>> {
        if self.next != Some(start) {
            for stage in &mut self.stages {
                stage.reset();
            }
            // Prime from earlier timeline audio and discard the delay. First returned audio is
            // aligned with `start`, rather than shifting sound behind the accelerated picture.
            let pre = self.latency * 2;
            let Some(mut input) = start.checked_sub(pre as i64) else { return vec![vec![0.0; n]; self.channels] };
            let mut left = pre + self.latency;
            while left > 0 {
                let count = left.min(BLOCK);
                self.process(input, count);
                input += count as i64;
                left -= count;
            }
        }
        let mut result = vec![Vec::with_capacity(n); self.channels];
        let mut done = 0;
        while done < n {
            let count = (n - done).min(BLOCK);
            let block = self.process(start + self.latency as i64 + done as i64, count);
            for (out, plane) in result.iter_mut().zip(block) {
                out.extend(plane);
            }
            done += count;
        }
        self.next = Some(start + n as i64);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_mix(rate: u32, tones: Vec<f64>, burst: bool) -> impl FnMut(i64, &mut [f32], usize) + Send {
        move |start, buf: &mut [f32], ch| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let time = (start + i as i64) as f64 / f64::from(rate);
                for (sample, hz) in frame.iter_mut().zip(&tones) {
                    let on = !burst || (0.5..0.6).contains(&time);
                    *sample = if on { (0.3 * (std::f64::consts::TAU * hz * time).sin()) as f32 } else { 0.0 };
                }
            }
        }
    }

    fn amplitude(samples: &[f32], hz: f64, sr: u32) -> f64 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, x) in samples.iter().enumerate() {
            let phase = std::f64::consts::TAU * hz * i as f64 / f64::from(sr);
            re += f64::from(*x) * phase.cos();
            im += f64::from(*x) * phase.sin();
        }
        2.0 * re.hypot(im) / samples.len() as f64
    }

    fn rates(sr: u32, speed: f64) -> (u32, u32, f32) {
        let to = super::super::shuttle_mix_rate(sr, speed).unwrap();
        let from = if speed < 1.0 { sr * 2 } else { sr };
        (from, to, (12.0 * (f64::from(to) / f64::from(sr)).log2()) as f32)
    }

    #[test]
    fn original_tones_survive_shuttling_including_high_frequencies() {
        for sr in [44_100, 48_000, 96_000] {
            for speed in [0.5, 0.75, 1.5, 2.0, 4.0] {
                let (from, to, semitones) = rates(sr, speed);
                let tones = vec![440.0, 660.0, 6300.0, 9000.0, 18_000.0, 1000.0];
                let mut mix = corrected(tone_mix(from, tones.clone(), false), from, to, semitones);
                let mut buf = vec![0.0; 16_000 * tones.len()];
                mix(i64::from(to), &mut buf, tones.len());
                for (ch, hz) in tones.iter().enumerate() {
                    let samples: Vec<f32> = buf.chunks_exact(tones.len()).map(|f| f[ch]).collect();
                    let level = amplitude(&samples, *hz, sr);
                    assert!(level > 0.1, "{sr} Hz, {speed}x, channel {ch}: {hz} Hz level={level}");
                    if hz * speed < f64::from(sr) / 2.0 {
                        assert!(amplitude(&samples, hz * speed, sr) < level * 0.15, "uncorrected pitch at {speed}x: {hz} Hz");
                    }
                }
            }
        }
    }

    #[test]
    fn block_boundaries_and_seeks_do_not_change_the_output() {
        let (sr, speed) = (48_000, 4.0);
        let (from, to, semitones) = rates(sr, speed);
        let make = || corrected(tone_mix(from, vec![440.0, 660.0], false), from, to, semitones);
        let mut whole_mix = make();
        let mut whole = vec![0.0; 24_000];
        whole_mix(i64::from(to), &mut whole, 2);
        let mut cut_mix = make();
        let mut cut = Vec::new();
        let mut n = 0;
        for size in [1, 7, 333, 1024].into_iter().cycle() {
            let frames = size.min(12_000 - n);
            let mut block = vec![0.0; frames * 2];
            cut_mix(i64::from(to) + n as i64, &mut block, 2);
            cut.extend(block);
            n += frames;
            if n == 12_000 {
                break;
            }
        }
        assert_eq!(cut, whole);
        let mut sought = vec![0.0; 4096];
        cut_mix(0, &mut sought, 2);
        let mut fresh = vec![0.0; 4096];
        make()(0, &mut fresh, 2);
        assert_eq!(sought, fresh, "a backward seek must reset the pitch history");
    }

    #[test]
    fn pitch_latency_is_compensated_against_the_timeline() {
        for speed in [0.5, 2.0, 4.0] {
            let sr = 48_000;
            let (from, to, semitones) = rates(sr, speed);
            let mut mix = corrected(tone_mix(from, vec![440.0], true), from, to, semitones);
            let mut buf = vec![0.0; (f64::from(sr) * 1.4) as usize];
            mix(0, &mut buf, 1);
            let energy: f64 = buf.iter().map(|x| f64::from(*x).powi(2)).sum();
            let center: f64 = buf.iter().enumerate().map(|(i, x)| i as f64 / f64::from(sr) * f64::from(*x).powi(2)).sum::<f64>() / energy;
            let expected = 0.55 / speed;
            assert!(energy > 1.0);
            assert!((center - expected).abs() < 0.025 / speed, "{speed}x: burst at {center}s, expected {expected}s");
            assert!(buf.iter().take((0.4 / speed * f64::from(sr)) as usize).all(|x| x.abs() < 1e-6), "audio must not arrive early");
        }
    }

    #[test]
    fn hostile_coordinates_and_channel_counts_produce_silence() {
        let (from, to, semitones) = rates(48_000, 4.0);
        let mut mix = corrected(tone_mix(from, vec![440.0, 660.0], false), from, to, semitones);
        for (cursor, ch, len) in [(i64::MIN, 2, 2048), (i64::MAX, 2, 2048), (0, 0, 2048), (0, 65, 65), (0, 2, 3)] {
            let mut buf = vec![1.0; len];
            mix(cursor, &mut buf, ch);
            assert!(buf.iter().all(|x| *x == 0.0));
        }
        mix(0, &mut [], 2);
        let mut buf = vec![0.0; 2048];
        mix(i64::from(to), &mut buf, 2);
        assert!(buf.iter().all(|x| x.is_finite()));
        assert!(buf.iter().any(|x| x.abs() > 0.1));
    }

    #[test]
    fn reverse_wrap_produces_audio_and_is_finite() {
        let sr = 48_000u32;
        let tones = vec![440.0, 660.0];
        let inner = tone_mix(sr, tones, false);
        let mut rev = reverse_wrap(inner, 1.0);
        // At cursor=48000 (1 second in), there should be reversed audio.
        let mut buf = vec![0.0f32; 2048];
        rev(sr as i64, &mut buf, 2);
        assert!(buf.iter().all(|x| x.is_finite()), "reverse output must be finite");
        assert!(buf.iter().any(|x| x.abs() > 0.01), "reverse output must have signal");
    }

    #[test]
    fn reverse_wrap_at_start_of_timeline_produces_silence() {
        let sr = 48_000u32;
        let inner = tone_mix(sr, vec![440.0], false);
        let mut rev = reverse_wrap(inner, 1.0);
        // cursor=0: timeline_end=0, so we've hit the start — must output silence.
        let mut buf = vec![1.0f32; 512];
        rev(0, &mut buf, 1);
        assert!(buf.iter().all(|x| *x == 0.0), "must be silent at timeline start");
    }

    #[test]
    fn reverse_wrap_2x_speed_produces_finite_output() {
        let sr = 48_000u32;
        let inner = tone_mix(sr, vec![440.0, 660.0], false);
        let mut rev = reverse_wrap(inner, 2.0);
        let mut buf = vec![0.0f32; 2048];
        rev(sr as i64 * 2, &mut buf, 2);
        assert!(buf.iter().all(|x| x.is_finite()), "2x reverse must be finite");
    }

    #[test]
    fn reverse_wrap_blocks_sequence_backward_on_timeline() {
        // Verify that successive calls retreat on the timeline rather than advancing.
        // Use a ramp signal so different timeline positions produce measurably different output.
        let sr = 48_000u32;
        let block = 1024usize;
        // Ramp: sample value = frame index / sr (unique per position, easy to sum-check).
        let ramp_mix = |start: i64, buf: &mut [f32], ch: usize| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                let v = (start + i as i64) as f32 / sr as f32;
                frame.iter_mut().for_each(|s| *s = v);
            }
        };
        let start_cursor: i64 = sr as i64 * 10; // 10 seconds in
        let mut rev = reverse_wrap(ramp_mix, 1.0);
        // First call: timeline reads [start-block, start], reversed → first sample ≈ start/sr.
        let mut buf1 = vec![0.0f32; block];
        rev(start_cursor, &mut buf1, 1);
        // Second call: cursor advances by block; timeline retreats by block.
        let mut buf2 = vec![0.0f32; block];
        rev(start_cursor + block as i64, &mut buf2, 1);
        // buf1 first sample should be higher (later in timeline) than buf2 first sample.
        assert!(buf1[0] > buf2[0], "reverse must retreat on the timeline: buf1[0]={} buf2[0]={}", buf1[0], buf2[0]);
        // Adjacent: buf1 ends where buf2 began (within float rounding).
        assert!((buf1[block - 1] - buf2[0]).abs() < 1e-4, "blocks must be adjacent on the timeline");
    }
}
