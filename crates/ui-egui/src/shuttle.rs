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

pub(crate) fn mix(session: &Session, seq_id: ItemId, device_rate: u32, speed: f64) -> Option<(u32, Mixer)> {
    let rate = super::shuttle_mix_rate(device_rate, speed)?;
    if speed < 0.0 {
        // Reverse plays the same sped-up mix backwards, without pitch correction.
        return Some((rate, Box::new(reversed(super::playback_mix(session, seq_id, rate)))));
    }
    // The pitch shifter only handles a limited range; beyond it the rate change is heard.
    if !session.prefs.audio.maintain_pitch_shuttling || rate == device_rate || !(0.5..=4.0).contains(&speed) {
        return Some((rate, Box::new(super::playback_mix(session, seq_id, rate))));
    }
    // Slow playback needs headroom for the upward pitch correction before rate conversion.
    let internal_rate = if rate > device_rate { device_rate.checked_mul(2)? } else { device_rate };
    let semitones = (12.0 * (f64::from(rate) / f64::from(device_rate)).log2()) as f32;
    Some((rate, Box::new(corrected(super::playback_mix(session, seq_id, internal_rate), internal_rate, rate, semitones))))
}

/// Plays `mix` backwards: the first cursor it is asked for is the start, and later blocks walk
/// back from there. Each block is rendered forwards and flipped; samples before zero are silent.
fn reversed(mut mix: impl FnMut(i64, &mut [f32], usize) + Send + 'static) -> impl FnMut(i64, &mut [f32], usize) + Send {
    let mut anchor: Option<i64> = None;
    let mut block: Vec<f32> = Vec::new();
    move |cursor, buf: &mut [f32], ch| {
        buf.fill(0.0);
        if ch == 0 || ch > MAX_CHANNELS || !buf.len().is_multiple_of(ch) {
            return;
        }
        let n = (buf.len() / ch) as i64;
        let start = *anchor.get_or_insert(cursor);
        // forward index of the block's last / first frame in the source
        let Some(last) = start.checked_mul(2).and_then(|x| x.checked_sub(cursor)) else { return };
        let Some(first) = last.checked_sub(n - 1) else { return };
        let skip = first.saturating_neg().clamp(0, n);
        if skip >= n {
            return;
        }
        block.clear();
        block.resize(buf.len(), 0.0);
        if let Some(part) = block.get_mut((skip as usize) * ch..) {
            mix(first.max(0), part, ch);
        }
        for (dst, src) in buf.chunks_exact_mut(ch).zip(block.chunks_exact(ch).rev()) {
            dst.copy_from_slice(src);
        }
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
    #[test]
    fn reversed_plays_blocks_backwards_from_the_start() {
        // the source is a ramp: sample k has the value k
        let ramp = |start: i64, buf: &mut [f32], ch: usize| {
            for (i, frame) in buf.chunks_exact_mut(ch).enumerate() {
                frame.fill((start + i as i64) as f32);
            }
        };
        let mut rev = super::reversed(move |c, b: &mut [f32], ch| ramp(c, b, ch));
        let mut a = vec![0.0; 8];
        rev(10, &mut a, 2);
        assert_eq!(a, [10.0, 10.0, 9.0, 9.0, 8.0, 8.0, 7.0, 7.0]);
        rev(14, &mut a, 2); // the next block continues backwards
        assert_eq!(a, [6.0, 6.0, 5.0, 5.0, 4.0, 4.0, 3.0, 3.0]);
        rev(18, &mut a, 2); // runs past the start of the timeline: silence there
        assert_eq!(a, [2.0, 2.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0]);
        rev(i64::MAX, &mut a, 2); // hostile cursor: silence, no panic
        assert!(a.iter().all(|x| *x == 0.0));
    }

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
}
