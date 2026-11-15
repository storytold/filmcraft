//! Streaming sample-rate conversion for playback, when the output device runs at another rate
//! than the sequence (a 44.1 kHz device playing a 48 kHz sequence).
//!
//! Every output sample's source position comes from its absolute output frame, as an exact
//! rational (`frame × from / to`), and is linearly interpolated, so the output is identical however
//! the stream is cut into blocks: no phase jump at block boundaries. Source frames are pulled in
//! contiguous, non-overlapping spans, so whatever produces them (the mixer, with its effect state)
//! sees one continuous stream. Output lags the source by one source sample (≈21 µs at 48 kHz),
//! which is what lets each block interpolate without reading ahead.

/// Converts a planar stream from `from` Hz to `to` Hz, block by block.
#[derive(Debug, Clone)]
pub struct StreamResampler {
    from: u32,
    to: u32,
    /// Source frames `[held_start, held_start + held[c].len())` kept from the last pull.
    held_start: i64,
    held: Vec<Vec<f32>>,
    /// The output frame the next block is expected to start at (contiguous playback).
    next_out: Option<i64>,
}

/// Maximum source frames fetched in a single resampler pull.
pub const MAX_RESAMPLE_SOURCE_SPAN: usize = 384_000 * 10;

impl StreamResampler {
    /// Rates of zero are treated as 1 Hz rather than dividing by zero.
    pub fn new(from: u32, to: u32) -> Self {
        Self { from: from.max(1), to: to.max(1), held_start: 0, held: Vec::new(), next_out: None }
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.from, self.to)
    }

    /// The source frame whose sample lands at (or just before) output frame `out`, and the
    /// remainder of the exact position in units of `1 / to` source frames.
    fn source_index(&self, out: i64) -> (i64, i64) {
        let p = out as i128 * self.from as i128;
        let to = self.to as i128;
        (p.div_euclid(to) as i64, p.rem_euclid(to) as i64)
    }

    /// Output frames `[out, out + n)`, planar. `pull(start, frames)` returns source frames
    /// `[start, start + frames)`, planar (missing samples read as silence). A block that does not
    /// continue the previous one (a seek) starts the stream afresh.
    pub fn process(&mut self, out: i64, n: usize, mut pull: impl FnMut(i64, usize) -> Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        if n == 0 {
            return vec![Vec::new(); self.held.len().max(1)];
        }
        let (first, first_rem) = self.source_index(out);
        let (last, _) = self.source_index(out.saturating_add(n as i64 - 1));
        // interpolation reads source frames first - 1 ..= last
        if self.next_out != Some(out) || self.held.is_empty() {
            self.held_start = first - 1;
            self.held.clear();
        }
        let held_end = self.held_start + self.held.first().map_or(0, Vec::len) as i64;
        if last + 1 > held_end {
            let want = ((last + 1 - held_end) as usize).min(MAX_RESAMPLE_SOURCE_SPAN);
            let got = pull(held_end, want);
            if self.held.is_empty() {
                self.held = vec![Vec::new(); got.len().max(1)];
            }
            for (c, h) in self.held.iter_mut().enumerate() {
                let src = got.get(c).map(Vec::as_slice).unwrap_or_default();
                h.extend((0..want).map(|i| src.get(i).copied().unwrap_or(0.0)));
            }
        }
        let mut dst = vec![vec![0.0f32; n]; self.held.len()];
        let to = self.to as i64;
        let (step, rem_step) = ((self.from / self.to) as i64, (self.from % self.to) as i64);
        let (mut s, mut rem) = (first, first_rem);
        for i in 0..n {
            let frac = (rem as f64 / to as f64) as f32;
            // held starts at first - 1, so the earlier frame is always held
            let k = usize::try_from(s - self.held_start).unwrap_or(0);
            for (d, h) in dst.iter_mut().zip(&self.held) {
                let a = k.checked_sub(1).and_then(|j| h.get(j)).copied().unwrap_or(0.0);
                let b = h.get(k).copied().unwrap_or(0.0);
                if let Some(x) = d.get_mut(i) {
                    *x = a + (b - a) * frac;
                }
            }
            s = s.wrapping_add(step);
            rem += rem_step;
            if rem >= to {
                rem -= to;
                s = s.wrapping_add(1);
            }
        }
        // keep what the next block can still read: from its first frame - 1 on
        let (next_first, _) = self.source_index(out.saturating_add(n as i64));
        let drop = (next_first - 1 - self.held_start).clamp(0, self.held.first().map_or(0, Vec::len) as i64) as usize;
        for h in &mut self.held {
            h.drain(..drop);
        }
        self.held_start += drop as i64;
        self.next_out = Some(out.saturating_add(n as i64));
        dst
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine at `hz`, sampled at `rate`, from absolute frame `start`; logs every pull.
    fn sine(hz: f64, rate: u32, pulls: &mut Vec<(i64, usize)>) -> impl FnMut(i64, usize) -> Vec<Vec<f32>> + '_ {
        move |start, frames| {
            pulls.push((start, frames));
            let ch: Vec<f32> = (0..frames).map(|i| (2.0 * std::f64::consts::PI * hz * (start + i as i64) as f64 / rate as f64).sin() as f32).collect();
            vec![ch.clone(), ch]
        }
    }

    fn convert(from: u32, to: u32, blocks: &[usize], total: usize) -> (Vec<f32>, Vec<(i64, usize)>) {
        let mut r = StreamResampler::new(from, to);
        let mut pulls = Vec::new();
        let mut out = Vec::new();
        let mut pos = 0i64;
        let mut k = 0;
        while (pos as usize) < total {
            let n = blocks[k % blocks.len()].min(total - pos as usize);
            let b = r.process(pos, n, sine(1000.0, from, &mut pulls));
            assert_eq!(b.len(), 2);
            assert!(b.iter().all(|c| c.len() == n));
            out.extend_from_slice(&b[0]);
            pos += n as i64;
            k += 1;
        }
        (out, pulls)
    }

    #[test]
    fn output_is_the_same_however_the_stream_is_cut() {
        for (from, to) in [(48_000, 44_100), (44_100, 48_000), (48_000, 96_000), (96_000, 48_000)] {
            let (whole, _) = convert(from, to, &[20_000], 20_000);
            for blocks in [&[512][..], &[333], &[1, 7, 512, 64]] {
                let (cut, _) = convert(from, to, blocks, 20_000);
                assert_eq!(cut, whole, "{from}→{to} in blocks {blocks:?}");
            }
        }
    }

    /// `process` as it was with one i128 division per output sample.
    fn reference_process(r: &mut StreamResampler, out: i64, n: usize, mut pull: impl FnMut(i64, usize) -> Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        let (from, to) = (r.from as i128, r.to as i128);
        let index = |out: i64| {
            let p = out as i128 * from;
            (p.div_euclid(to) as i64, (p.rem_euclid(to) as f64 / to as f64) as f32)
        };
        if n == 0 {
            return vec![Vec::new(); r.held.len().max(1)];
        }
        let (first, _) = index(out);
        let (last, _) = index(out.saturating_add(n as i64 - 1));
        if r.next_out != Some(out) || r.held.is_empty() {
            r.held_start = first - 1;
            r.held.clear();
        }
        let held_end = r.held_start + r.held.first().map_or(0, Vec::len) as i64;
        if last + 1 > held_end {
            let want = ((last + 1 - held_end) as usize).min(MAX_RESAMPLE_SOURCE_SPAN);
            let got = pull(held_end, want);
            if r.held.is_empty() {
                r.held = vec![Vec::new(); got.len().max(1)];
            }
            for (c, h) in r.held.iter_mut().enumerate() {
                let src = got.get(c).map(Vec::as_slice).unwrap_or_default();
                h.extend((0..want).map(|i| src.get(i).copied().unwrap_or(0.0)));
            }
        }
        let mut dst = vec![vec![0.0f32; n]; r.held.len()];
        for (i, frame) in (out..).take(n).enumerate() {
            let (s, frac) = index(frame);
            let k = usize::try_from(s - r.held_start).unwrap_or(0);
            for (d, h) in dst.iter_mut().zip(&r.held) {
                let a = k.checked_sub(1).and_then(|j| h.get(j)).copied().unwrap_or(0.0);
                let b = h.get(k).copied().unwrap_or(0.0);
                if let Some(x) = d.get_mut(i) {
                    *x = a + (b - a) * frac;
                }
            }
        }
        let (next_first, _) = index(out.saturating_add(n as i64));
        let drop = (next_first - 1 - r.held_start).clamp(0, r.held.first().map_or(0, Vec::len) as i64) as usize;
        for h in &mut r.held {
            h.drain(..drop);
        }
        r.held_start += drop as i64;
        r.next_out = Some(out.saturating_add(n as i64));
        dst
    }

    #[test]
    fn stepped_source_positions_are_bit_identical_to_per_sample_division() {
        let bits = |b: &[Vec<f32>]| b.iter().map(|c| c.iter().map(|v| v.to_bits()).collect::<Vec<_>>()).collect::<Vec<_>>();
        for (from, to) in
            [(48_000, 44_100), (44_100, 48_000), (96_000, 48_000), (48_000, 48_000), (48_000, 96_000), (44_100, 22_050), (7, 384_000), (176_400, 1_001)]
        {
            let (mut new, mut old) = (StreamResampler::new(from, to), StreamResampler::new(from, to));
            let mut k = 0;
            for start in [0i64, -5_000, 1_000_000_007, -3, i64::MAX / 2_000_000] {
                let mut pos = start;
                for _ in 0..12 {
                    let n = [1, 7, 333, 512, 64][k % 5];
                    k += 1;
                    let (mut pa, mut pb) = (Vec::new(), Vec::new());
                    let a = new.process(pos, n, sine(440.0, from, &mut pa));
                    let b = reference_process(&mut old, pos, n, sine(440.0, from, &mut pb));
                    assert_eq!(bits(&a), bits(&b), "{from}→{to} at {pos} + {n}");
                    assert_eq!(pa, pb);
                    pos += n as i64;
                }
            }
        }
    }

    #[test]
    fn a_tone_converts_to_the_same_tone() {
        // 1 kHz at 48 kHz played on a 44.1 kHz device: within linear interpolation's error of the
        // ideal tone, one source sample late
        let (from, to) = (48_000u32, 44_100u32);
        let (out, _) = convert(from, to, &[512], 20_000);
        for (o, x) in out.iter().enumerate().skip(2) {
            let t = o as f64 / to as f64 - 1.0 / from as f64;
            let want = (2.0 * std::f64::consts::PI * 1000.0 * t).sin() as f32;
            assert!((x - want).abs() < 3e-3, "frame {o}: {x} vs {want}");
        }
    }

    #[test]
    fn the_source_is_pulled_as_one_continuous_stream() {
        for (from, to) in [(48_000, 44_100), (44_100, 48_000)] {
            let (_, pulls) = convert(from, to, &[512, 333], 20_000);
            for w in pulls.windows(2) {
                assert_eq!(w[0].0 + w[0].1 as i64, w[1].0, "{from}→{to}: {:?} then {:?}", w[0], w[1]);
            }
        }
    }

    #[test]
    fn a_seek_starts_afresh_and_hostile_input_stays_finite() {
        let mut r = StreamResampler::new(48_000, 44_100);
        let mut pulls = Vec::new();
        let _ = r.process(0, 512, sine(1000.0, 48_000, &mut pulls));
        let _ = r.process(1_000_000, 512, sine(1000.0, 48_000, &mut pulls));
        assert_eq!(pulls.last().map(|p| p.0), Some(1_000_000 * 48_000 / 44_100 - 1));
        // a mixer returning too few channels and samples, and zero rates
        let mut r = StreamResampler::new(0, 0);
        let b = r.process(5, 64, |_, _| vec![vec![1.0; 3]]);
        assert_eq!(b.len(), 1);
        assert!(b[0].iter().all(|x| x.is_finite()));
        assert!(r.process(0, 0, |_, _| Vec::new()).iter().all(Vec::is_empty));
    }
}
