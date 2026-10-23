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
    /// An absurd rate ratio has already been reported for this stream.
    reported: bool,
}

/// Most source frames one block may pull.
const MAX_PULL: i64 = 1 << 24;
/// Output frames per internal block of a long request. With `MAX_PULL` this means any ratio up to
/// 256:1 (`MAX_PULL / BLOCK`; for instance 192 kHz to 750 Hz) works however long the request is; a
/// shorter request tolerates more. Beyond that the block is silence (see [`StreamResampler::process`]).
const BLOCK: usize = 1 << 16;

impl StreamResampler {
    /// Rates of zero are treated as 1 Hz rather than dividing by zero.
    pub fn new(from: u32, to: u32) -> Self {
        Self { from: from.max(1), to: to.max(1), held_start: 0, held: Vec::new(), next_out: None, reported: false }
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.from, self.to)
    }

    /// The source frame whose sample lands at (or just before) output frame `out`.
    fn source_index(&self, out: i64) -> (i64, f32) {
        let p = out as i128 * self.from as i128;
        let to = self.to as i128;
        let whole = p.div_euclid(to).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
        (whole, (p.rem_euclid(to) as f64 / to as f64) as f32)
    }

    /// Output frames `[out, out + n)`, planar. `pull(start, frames)` returns source frames
    /// `[start, start + frames)`, planar (missing samples read as silence). A block that does not
    /// continue the previous one (a seek) starts the stream afresh.
    ///
    /// Rate ratios up to 256:1 always work. A block that would need more than 2^24 source frames
    /// (corrupt rates such as 4 GHz to 1 Hz) is silence and restarts the stream; this is logged once
    /// per resampler.
    pub fn process(&mut self, out: i64, n: usize, mut pull: impl FnMut(i64, usize) -> Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        // Long requests run in bounded blocks (continuity is kept by `next_out`), so the pull cap
        // in `block` only trips on absurd rate ratios, never on a legitimate long call.
        if n <= BLOCK {
            return self.block(out, n, &mut pull);
        }
        let mut acc: Vec<Vec<f32>> = Vec::new();
        let mut done = 0usize;
        while done < n {
            let len = BLOCK.min(n - done);
            let part = self.block(out.saturating_add(done as i64), len, &mut pull);
            // a silenced block may report fewer channels than a pulled one: pad so every channel has n frames
            acc.resize(acc.len().max(part.len()), vec![0.0; done]);
            for (c, a) in acc.iter_mut().enumerate() {
                a.extend(part.get(c).into_iter().flatten());
                a.resize(done + len, 0.0);
            }
            done += len;
        }
        acc
    }

    fn block(&mut self, out: i64, n: usize, pull: &mut dyn FnMut(i64, usize) -> Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        if n == 0 {
            return vec![Vec::new(); self.held.len().max(1)];
        }
        let (first, _) = self.source_index(out);
        let (last, _) = self.source_index(out.saturating_add(n as i64 - 1));
        // interpolation reads source frames first - 1 ..= last
        if self.next_out != Some(out) || self.held.is_empty() {
            self.held_start = first.saturating_sub(1);
            self.held.clear();
        }
        let held_end = self.held_start.saturating_add(self.held.first().map_or(0, Vec::len) as i64);
        if last.saturating_add(1) > held_end {
            let want = last.saturating_add(1).saturating_sub(held_end);
            if want > MAX_PULL {
                // an absurd rate ratio (corrupt rates): silence rather than gigabytes of pulls
                if !self.reported {
                    self.reported = true;
                    log::warn!("resampler {} Hz -> {} Hz: block needs {want} source frames (limit {MAX_PULL}); outputting silence", self.from, self.to);
                }
                let channels = self.held.len().max(1);
                self.held.clear();
                self.next_out = None;
                return vec![vec![0.0; n]; channels];
            }
            let want = want as usize;
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
        for i in 0..n {
            let (s, frac) = self.source_index(out.saturating_add(i as i64));
            // held starts at first - 1, so the earlier frame is always held
            let k = usize::try_from(s.saturating_sub(self.held_start)).unwrap_or(0);
            for (d, h) in dst.iter_mut().zip(&self.held) {
                let a = k.checked_sub(1).and_then(|j| h.get(j)).copied().unwrap_or(0.0);
                let b = h.get(k).copied().unwrap_or(0.0);
                if let Some(x) = d.get_mut(i) {
                    *x = a + (b - a) * frac;
                }
            }
        }
        // keep what the next block can still read: from its first frame - 1 on
        let (next_first, _) = self.source_index(out.saturating_add(n as i64));
        let drop = next_first.saturating_sub(1).saturating_sub(self.held_start).clamp(0, self.held.first().map_or(0, Vec::len) as i64) as usize;
        for h in &mut self.held {
            h.drain(..drop);
        }
        self.held_start = self.held_start.saturating_add(drop as i64);
        self.next_out = Some(out.saturating_add(n as i64));
        dst
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_downsample_in_one_call_matches_chunked_and_is_not_silence() {
        // 192 kHz -> 8 kHz, well past MAX_PULL source frames in one request
        let n = 1_000_000usize;
        let src = |start: i64, k: usize| vec![(0..k).map(|i| (((start + i as i64) as f32) * 0.001).sin()).collect::<Vec<f32>>()];
        let whole = StreamResampler::new(192_000, 8_000).process(0, n, src);
        assert_eq!(whole[0].len(), n);
        assert!(whole[0].iter().any(|v| v.abs() > 0.5), "silenced");
        let mut r = StreamResampler::new(192_000, 8_000);
        let mut chunked = Vec::new();
        for start in (0..n).step_by(70_000) {
            chunked.extend(r.process(start as i64, 70_000.min(n - start), src).remove(0));
        }
        assert_eq!(whole[0], chunked);
    }

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

    #[test]
    fn a_long_call_keeps_every_channel_when_some_blocks_are_silenced() {
        // 512:1 silences a full block (2^25 source frames) but not a short one
        let stereo = |_: i64, k: usize| vec![vec![0.5; k]; 2];
        let n = BLOCK + 1000;
        // fresh stream: the silenced first block knows no channel count, the short last block pulls two
        let b = StreamResampler::new(512_000, 1_000).process(0, n, stereo);
        assert_eq!(b.len(), 2);
        assert!(b.iter().all(|c| c.len() == n));
        // stereo already established, then two silenced blocks in one call
        let mut r = StreamResampler::new(512_000, 1_000);
        let _ = r.process(0, 10, stereo);
        let b = r.process(10, 2 * BLOCK, stereo);
        assert_eq!(b.len(), 2);
        assert!(b.iter().all(|c| c.len() == 2 * BLOCK));
    }
}
