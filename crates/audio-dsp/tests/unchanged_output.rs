//! The hostile-input hardening must not change what ordinary input produces. The pre-hardening
//! `StreamResampler` and `remix::render` are kept here verbatim (from before the saturating
//! arithmetic, the block splitting and the pull cap) and compared bit for bit with the current
//! ones on normal 44.1 / 48 / 96 kHz material, including seeks and one call longer than the
//! internal block size.

use filmcraft_audio_dsp::remix::{self, Params, Piece, Plan};
use filmcraft_audio_dsp::resample::StreamResampler;

mod old {
    use filmcraft_audio_dsp::remix::Plan;

    pub struct Resampler {
        from: u32,
        to: u32,
        held_start: i64,
        held: Vec<Vec<f32>>,
        next_out: Option<i64>,
    }

    impl Resampler {
        pub fn new(from: u32, to: u32) -> Self {
            Self { from: from.max(1), to: to.max(1), held_start: 0, held: Vec::new(), next_out: None }
        }

        fn source_index(&self, out: i64) -> (i64, f32) {
            let p = out as i128 * self.from as i128;
            let to = self.to as i128;
            (p.div_euclid(to) as i64, (p.rem_euclid(to) as f64 / to as f64) as f32)
        }

        pub fn process(&mut self, out: i64, n: usize, mut pull: impl FnMut(i64, usize) -> Vec<Vec<f32>>) -> Vec<Vec<f32>> {
            if n == 0 {
                return vec![Vec::new(); self.held.len().max(1)];
            }
            let (first, _) = self.source_index(out);
            let (last, _) = self.source_index(out.saturating_add(n as i64 - 1));
            if self.next_out != Some(out) || self.held.is_empty() {
                self.held_start = first - 1;
                self.held.clear();
            }
            let held_end = self.held_start + self.held.first().map_or(0, Vec::len) as i64;
            if last + 1 > held_end {
                let want = (last + 1 - held_end) as usize;
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
            for (i, frame) in (out..).take(n).enumerate() {
                let (s, frac) = self.source_index(frame);
                let k = usize::try_from(s - self.held_start).unwrap_or(0);
                for (d, h) in dst.iter_mut().zip(&self.held) {
                    let a = k.checked_sub(1).and_then(|j| h.get(j)).copied().unwrap_or(0.0);
                    let b = h.get(k).copied().unwrap_or(0.0);
                    if let Some(x) = d.get_mut(i) {
                        *x = a + (b - a) * frac;
                    }
                }
            }
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

    pub fn render(plan: &Plan, out0: i64, n: usize, channels: usize, read: &mut dyn FnMut(i64, usize) -> Vec<Vec<f32>>) -> Vec<Vec<f32>> {
        let mut out = vec![vec![0f32; n]; channels];
        let x = plan.xfade.max(0);
        let h0 = x / 2;
        let h1 = x - h0;
        let end = out0 + n as i64;
        let last = plan.pieces.len().saturating_sub(1);
        let mut d = 0i64;
        for (pi, p) in plan.pieces.iter().enumerate() {
            let (ds, de) = (d, d + p.len);
            d = de;
            let fade_in = pi > 0 && x > 0;
            let fade_out = pi < last && x > 0;
            let s = if fade_in { ds - h0 } else { ds };
            let e = if fade_out { de + h1 } else { de };
            let (a, z) = (s.max(out0), e.min(end));
            if z <= a {
                continue;
            }
            let len = (z - a) as usize;
            let buf = read(p.src + (a - ds), len);
            for i in 0..len {
                let o = a + i as i64;
                let mut g = 1.0f64;
                if fade_in && o < ds + h1 {
                    let u = (o - (ds - h0)) as f64 + 0.5;
                    g *= (u / x as f64 * std::f64::consts::FRAC_PI_2).sin();
                }
                if fade_out && o >= de - h0 {
                    let u = (o - (de - h0)) as f64 + 0.5;
                    g *= (u / x as f64 * std::f64::consts::FRAC_PI_2).cos();
                }
                let g = g as f32;
                let k = (o - out0) as usize;
                for (c, dst) in out.iter_mut().enumerate() {
                    let src = buf.get(c.min(buf.len().saturating_sub(1))).and_then(|ch| ch.get(i)).copied().unwrap_or(0.0);
                    dst[k] += src * g;
                }
            }
        }
        out
    }
}

/// Planar source frames `[start, start + frames)` of `music`, silence outside it.
fn read(music: &[Vec<f32>], start: i64, frames: usize) -> Vec<Vec<f32>> {
    music.iter().map(|ch| (0..frames).map(|i| usize::try_from(start + i as i64).ok().and_then(|j| ch.get(j)).copied().unwrap_or(0.0)).collect()).collect()
}

fn bits(x: &[Vec<f32>]) -> Vec<Vec<u32>> {
    x.iter().map(|c| c.iter().map(|v| v.to_bits()).collect()).collect()
}

#[test]
fn resampler_output_is_unchanged_for_normal_rates() {
    let (music, _) = remix::test_music(48_000, 120.0, 6, 7);
    for (from, to) in [(48_000, 44_100), (44_100, 48_000), (48_000, 48_000), (96_000, 8_000), (8_000, 48_000), (22_050, 48_000)] {
        let mut new = StreamResampler::new(from, to);
        let mut old = old::Resampler::new(from, to);
        // contiguous blocks of mixed sizes, a seek, then one call longer than the internal block size
        let mut out = 0i64;
        let mut plan: Vec<(i64, usize)> = Vec::new();
        for n in [1usize, 480, 1000, 4410, 7, 0, 2048] {
            plan.push((out, n));
            out += n as i64;
        }
        plan.push((100_000, 3000)); // seek
        plan.push((103_000, 150_000)); // one long call, past 65 536 output frames
        plan.push((253_000, 17));
        let mut signal = false;
        for (start, n) in plan {
            let a = new.process(start, n, |s, k| read(&music, s, k));
            let b = old.process(start, n, |s, k| read(&music, s, k));
            assert_eq!(bits(&a), bits(&b), "{from}->{to} at {start} for {n}");
            signal |= a.iter().flatten().any(|v| v.abs() > 1e-3);
        }
        assert!(signal, "{from}->{to}: the comparison only saw silence");
    }
}

#[test]
fn remix_render_is_unchanged() {
    let (music, _) = remix::test_music(48_000, 120.0, 8, 3);
    let len = music[0].len() as i64;
    let refs: Vec<&[f32]> = music.iter().map(Vec::as_slice).collect();
    let analysis = remix::analyze(&refs, 48_000).expect("analysis of the test music");
    let remixed = remix::plan(&analysis, len * 3 / 4, Params::default()).expect("plan");
    let cut = Plan { pieces: vec![Piece { src: 1000, len: 30_000 }, Piece { src: 90_000, len: 40_000 }, Piece { src: 5_000, len: 20_000 }], xfade: 2_049 };
    for plan in [Plan::identity(len, 2048), remixed, cut] {
        let total = plan.len();
        for (out0, n) in [(0i64, total as usize), (0, 5000), (4_999, 70_001), (total / 2, 9000), (total - 100, 400), (-500, 1000)] {
            let a = remix::render(&plan, out0, n, 2, &mut |s, k| read(&music, s, k));
            let b = old::render(&plan, out0, n, 2, &mut |s, k| read(&music, s, k));
            assert_eq!(bits(&a), bits(&b), "render {out0} +{n}");
        }
    }
}
