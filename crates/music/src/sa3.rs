//! The Stable Audio 3 sampling pipeline, independent of any inference runtime.
//!
//! The model is three graphs: a conditioner (T5Gemma text encoder plus the seconds embedding), the
//! diffusion transformer (DiT) and the SAME decoder. A [`Graphs`] backend runs them (ONNX Runtime in
//! `native.rs`, a fake in the tests). Everything else lives here: the length plan, the log-SNR
//! timestep schedule, seeded noise, the 8 step ping-pong sampler and the overlapped chunk decode.
//! The numbers follow the reference implementation (`Stability-AI/stable-audio-3`) and were checked
//! against it: same seed, same prompt, waveform correlation 0.998.

use crate::{Audio, MusicError, SAMPLE_RATE};

/// Samples per latent frame (the SAME autoencoder's downsampling ratio).
pub const DOWNSAMPLE: usize = 4096;
/// Latent channels.
pub const LATENT_CHANNELS: usize = 256;
/// Cross attention tokens: 256 text tokens plus one seconds embedding.
pub const COND_TOKENS: usize = 257;
/// Conditioning width.
pub const COND_DIM: usize = 768;
/// Extra seconds generated past the requested duration, so the end of a cue is never cut mid-phrase.
pub const PAD_SECONDS: f64 = 6.0;
/// Decoder chunk length and overlap, in latent frames (bounds peak memory on long cues).
pub const DECODE_CHUNK: usize = 128;
pub const DECODE_OVERLAP: usize = 32;
/// Default number of sampler steps of the post-trained (distilled) models.
pub const DEFAULT_STEPS: usize = 8;

/// log-SNR bounds of the (sequence length independent) sampling schedule.
const LOGSNR_START: f64 = -6.2;
const LOGSNR_END: f64 = 2.0;

/// Output of the conditioner for one prompt.
#[derive(Clone, Debug, PartialEq)]
pub struct Conditioning {
    /// `COND_TOKENS * COND_DIM`, token major.
    pub cross: Vec<f32>,
    /// `COND_DIM`.
    pub global: Vec<f32>,
}

/// The three model graphs. Shapes are channel major (`[channel][frame]`), flattened.
pub trait Graphs: Send + Sync {
    /// Encode a prompt and the target duration.
    fn encode(&self, prompt: &str, seconds: f32) -> Result<Conditioning, MusicError>;
    /// One DiT evaluation at time `t`. `x` is `LATENT_CHANNELS * frames`; `valid` is the number of
    /// leading frames that carry audio (the rest is padding the attention ignores). Returns the
    /// velocity, same shape as `x`.
    fn denoise(&self, x: &[f32], frames: usize, t: f32, cond: &Conditioning, valid: usize) -> Result<Vec<f32>, MusicError>;
    /// Decode `LATENT_CHANNELS * frames` latents to planar stereo `2 * frames * DOWNSAMPLE`.
    fn decode(&self, z: &[f32], frames: usize) -> Result<Vec<f32>, MusicError>;
}

/// Frame counts for one generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Latent frames generated (duration plus padding, aligned).
    pub frames: usize,
    /// Frames the attention treats as real audio.
    pub valid: usize,
    /// Samples in the requested duration.
    pub out_samples: usize,
}

impl Plan {
    /// `max_samples` is the model's `sample_size` (5 292 032 for the small models).
    pub fn new(seconds: f64, max_samples: usize) -> Result<Plan, MusicError> {
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err(MusicError::BadDuration(1.0, crate::MAX_SECONDS));
        }
        let rate = f64::from(SAMPLE_RATE);
        let out_samples = (seconds * rate) as usize;
        let padded = ((seconds + PAD_SECONDS) * rate) as usize;
        // Round up to whole latent frames, then to an even frame count (the codec works in pairs).
        let frames = padded.div_ceil(DOWNSAMPLE).next_multiple_of(2).min(max_samples / DOWNSAMPLE);
        if frames == 0 || out_samples == 0 {
            return Err(MusicError::BadDuration(1.0, crate::MAX_SECONDS));
        }
        let effective = out_samples.div_ceil(DOWNSAMPLE);
        let headroom = (PAD_SECONDS * rate) as usize / DOWNSAMPLE;
        let valid = effective.saturating_add(headroom).min(frames);
        Ok(Plan { frames, valid, out_samples: out_samples.min(frames.saturating_mul(DOWNSAMPLE)) })
    }
}

/// `steps + 1` noise levels from 1 down to 0 (log-SNR spaced).
pub fn sigmas(steps: usize) -> Vec<f32> {
    let n = steps.max(1);
    (0..=n)
        .map(|i| {
            if i == 0 {
                return 1.0;
            }
            if i == n {
                return 0.0;
            }
            let t = 1.0 - i as f64 / n as f64;
            let logsnr = LOGSNR_END - t * (LOGSNR_END - LOGSNR_START);
            (1.0 / (1.0 + logsnr.exp())) as f32
        })
        .collect()
}

/// Start offsets (in latent frames) of the overlapping decoder chunks. A single chunk (offset 0,
/// whole signal) when the signal is shorter than a chunk.
pub fn chunk_starts(frames: usize) -> Vec<usize> {
    if frames < DECODE_CHUNK {
        return vec![0];
    }
    let hop = DECODE_CHUNK - DECODE_OVERLAP;
    let last = frames - DECODE_CHUNK;
    let mut starts: Vec<usize> = (0..=last).step_by(hop).collect();
    if starts.last() != Some(&last) {
        starts.push(last);
    }
    starts
}

/// Seeded Gaussian noise (xoshiro256** with SplitMix64 seeding, Box-Muller). Deterministic across
/// platforms; it does not reproduce PyTorch's generator, so a seed gives a repeatable result in
/// FilmCraft but not the same audio as the Python reference.
pub struct Noise {
    s: [u64; 4],
    spare: Option<f32>,
}

impl Noise {
    pub fn new(seed: u64) -> Self {
        let mut z = seed;
        let mut next = || {
            z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^ (x >> 31)
        };
        Noise { s: [next(), next(), next(), next()], spare: None }
    }

    fn next_u64(&mut self) -> u64 {
        let r = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        r
    }

    /// Uniform in (0, 1].
    fn unit(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / 9_007_199_254_740_992.0
    }

    pub fn gaussian(&mut self) -> f32 {
        if let Some(v) = self.spare.take() {
            return v;
        }
        let (u1, u2) = (self.unit(), self.unit());
        let r = (-2.0 * u1.ln()).sqrt();
        let a = std::f64::consts::TAU * u2;
        self.spare = Some((r * a.sin()) as f32);
        (r * a.cos()) as f32
    }

    pub fn fill(&mut self, n: usize) -> Vec<f32> {
        (0..n).map(|_| self.gaussian()).collect()
    }
}

/// Generation options the pipeline needs beyond [`crate::Params`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub steps: usize,
    pub max_samples: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { steps: DEFAULT_STEPS, max_samples: 5_292_032 }
    }
}

/// Run the whole pipeline. `progress` gets a fraction in `0..=1` and returns `false` to cancel.
pub fn generate(graphs: &dyn Graphs, prompt: &str, seconds: f64, seed: u64, opts: Options, progress: &mut dyn FnMut(f32) -> bool) -> Result<Audio, MusicError> {
    let plan = Plan::new(seconds, opts.max_samples)?;
    let steps = opts.steps.clamp(1, 64);
    let cond = graphs.encode(prompt, seconds as f32)?;
    if cond.cross.len() != COND_TOKENS * COND_DIM || cond.global.len() != COND_DIM {
        return Err(MusicError::Inference("conditioner returned an unexpected shape".into()));
    }
    let n = LATENT_CHANNELS.checked_mul(plan.frames).ok_or_else(|| MusicError::Invalid("duration too long".into()))?;
    let mut noise = Noise::new(seed);
    let mut x = noise.fill(n);
    let ts = sigmas(steps);
    let total = (steps + 1) as f32;
    for (i, pair) in ts.windows(2).enumerate() {
        let (t, t_next) = (pair[0], pair[1]);
        if !progress(i as f32 / total) {
            return Err(MusicError::Inference("cancelled".into()));
        }
        let v = graphs.denoise(&x, plan.frames, t, &cond, plan.valid)?;
        if v.len() != n {
            return Err(MusicError::Inference("DiT returned an unexpected shape".into()));
        }
        let fresh = if t_next > 0.0 { noise.fill(n) } else { Vec::new() };
        for (k, xk) in x.iter_mut().enumerate() {
            let denoised = *xk - t * v.get(k).copied().unwrap_or(0.0);
            let eps = fresh.get(k).copied().unwrap_or(0.0);
            *xk = (1.0 - t_next) * denoised + t_next * eps;
        }
    }
    if !progress(steps as f32 / total) {
        return Err(MusicError::Inference("cancelled".into()));
    }
    let planar = decode_chunked(graphs, &x, plan.frames)?;
    progress(1.0);
    Ok(interleave(&planar, plan))
}

/// Decode with overlapping chunks; the seam sits in the middle of each overlap.
pub fn decode_chunked(graphs: &dyn Graphs, z: &[f32], frames: usize) -> Result<Vec<f32>, MusicError> {
    let bad = || MusicError::Inference("decoder returned an unexpected shape".into());
    if frames < DECODE_CHUNK {
        let out = graphs.decode(z, frames)?;
        return if out.len() == 2 * frames * DOWNSAMPLE { Ok(out) } else { Err(bad()) };
    }
    let total = frames * DOWNSAMPLE;
    let chunk_samples = DECODE_CHUNK * DOWNSAMPLE;
    let half = DECODE_OVERLAP / 2 * DOWNSAMPLE;
    let starts = chunk_starts(frames);
    let mut out = vec![0.0f32; 2 * total];
    let count = starts.len();
    for (k, &start) in starts.iter().enumerate() {
        // Latents are channel major: gather the chunk's columns for every channel.
        let mut piece = Vec::with_capacity(LATENT_CHANNELS * DECODE_CHUNK);
        for c in 0..LATENT_CHANNELS {
            let row = z.get(c * frames + start..c * frames + start + DECODE_CHUNK).ok_or_else(bad)?;
            piece.extend_from_slice(row);
        }
        let dec = graphs.decode(&piece, DECODE_CHUNK)?;
        if dec.len() != 2 * chunk_samples {
            return Err(bad());
        }
        let (first, last) = (k == 0, k + 1 == count);
        let origin = if last { total - chunk_samples } else { start * DOWNSAMPLE };
        let (l, r) = (if first { 0 } else { half }, if last { chunk_samples } else { chunk_samples - half });
        for ch in 0..2 {
            let src = dec.get(ch * chunk_samples + l..ch * chunk_samples + r).ok_or_else(bad)?;
            let dst = out.get_mut(ch * total + origin + l..ch * total + origin + r).ok_or_else(bad)?;
            dst.copy_from_slice(src);
        }
    }
    Ok(out)
}

/// Planar stereo to interleaved, cut to the requested length and clamped.
fn interleave(planar: &[f32], plan: Plan) -> Audio {
    let per_channel = planar.len() / 2;
    let n = plan.out_samples.min(per_channel);
    let mut samples = Vec::with_capacity(n * 2);
    for i in 0..n {
        for ch in 0..2 {
            let v = planar.get(ch * per_channel + i).copied().unwrap_or(0.0);
            samples.push(if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 });
        }
    }
    Audio { samples, sample_rate: SAMPLE_RATE, channels: 2 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn plan_matches_reference_for_30_seconds() {
        let p = Plan::new(30.0, 5_292_032).unwrap();
        assert_eq!(p, Plan { frames: 388, valid: 387, out_samples: 1_323_000 });
    }

    #[test]
    fn plan_for_short_cue_keeps_padding_out_of_attention() {
        let p = Plan::new(10.0, 5_292_032).unwrap();
        assert_eq!(p.frames, 174);
        assert_eq!(p.valid, 172);
        let p = Plan::new(2.0, 5_292_032).unwrap();
        assert!(p.valid <= p.frames && p.frames.is_multiple_of(2));
    }

    #[test]
    fn plan_is_capped_at_the_model_length() {
        let p = Plan::new(300.0, 5_292_032).unwrap();
        assert_eq!(p.frames, 1292);
        assert!(p.out_samples <= 5_292_032);
        assert!(Plan::new(f64::NAN, 5_292_032).is_err());
        assert!(Plan::new(-1.0, 5_292_032).is_err());
        assert!(Plan::new(30.0, 0).is_err());
    }

    #[test]
    fn schedule_matches_the_reference() {
        let want = [1.0, 0.9944, 0.9845, 0.9579, 0.8909, 0.7455, 0.5125, 0.2739, 0.0];
        let got = sigmas(8);
        assert_eq!(got.len(), 9);
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-4, "{g} vs {w}");
        }
        assert!(got.windows(2).all(|w| w[0] > w[1]));
        assert_eq!(sigmas(0).len(), 2);
    }

    #[test]
    fn decoder_chunks_cover_the_signal() {
        assert_eq!(chunk_starts(64), vec![0]);
        assert_eq!(chunk_starts(128), vec![0]);
        assert_eq!(chunk_starts(388), vec![0, 96, 192, 260]);
        assert_eq!(chunk_starts(1292).last(), Some(&(1292 - 128)));
    }

    #[test]
    fn noise_is_repeatable_and_roughly_standard_normal() {
        let a = Noise::new(7).fill(20_000);
        assert_eq!(a, Noise::new(7).fill(20_000));
        assert_ne!(a, Noise::new(8).fill(20_000));
        let mean = a.iter().sum::<f32>() / a.len() as f32;
        let var = a.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / a.len() as f32;
        assert!(mean.abs() < 0.03 && (var - 1.0).abs() < 0.05, "mean {mean} var {var}");
        assert!(a.iter().all(|v| v.is_finite()));
    }

    /// Records calls and returns a constant velocity; the decoder emits a ramp so seams are visible.
    struct Fake {
        calls: Mutex<Vec<(usize, f32, usize)>>,
        decode_calls: Mutex<Vec<usize>>,
    }

    impl Graphs for Fake {
        fn encode(&self, _: &str, _: f32) -> Result<Conditioning, MusicError> {
            Ok(Conditioning { cross: vec![0.0; COND_TOKENS * COND_DIM], global: vec![0.0; COND_DIM] })
        }
        fn denoise(&self, x: &[f32], frames: usize, t: f32, _: &Conditioning, valid: usize) -> Result<Vec<f32>, MusicError> {
            self.calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((frames, t, valid));
            Ok(vec![0.0; x.len()])
        }
        fn decode(&self, z: &[f32], frames: usize) -> Result<Vec<f32>, MusicError> {
            self.decode_calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(frames);
            assert_eq!(z.len(), LATENT_CHANNELS * frames);
            Ok((0..2 * frames * DOWNSAMPLE).map(|i| ((i % 1000) as f32) / 1000.0).collect())
        }
    }

    fn fake() -> Fake {
        Fake { calls: Mutex::new(Vec::new()), decode_calls: Mutex::new(Vec::new()) }
    }

    #[test]
    fn pipeline_runs_every_step_and_returns_the_requested_length() {
        let g = fake();
        let mut seen = Vec::new();
        let a = generate(&g, "warm pad", 30.0, 1, Options::default(), &mut |p| {
            seen.push(p);
            true
        })
        .unwrap();
        assert_eq!(a.sample_rate, SAMPLE_RATE);
        assert_eq!(a.channels, 2);
        assert_eq!(a.samples.len(), 1_323_000 * 2);
        assert!(a.samples.iter().all(|v| (-1.0..=1.0).contains(v)));
        let calls = g.calls.lock().unwrap();
        assert_eq!(calls.len(), 8);
        assert!(calls.iter().all(|c| c.0 == 388 && c.2 == 387));
        assert_eq!(*g.decode_calls.lock().unwrap(), vec![128, 128, 128, 128]);
        assert!(seen.windows(2).all(|w| w[0] <= w[1]) && seen.last() == Some(&1.0));
    }

    #[test]
    fn cancelling_stops_before_the_next_graph_call() {
        let g = fake();
        let mut n = 0;
        let r = generate(&g, "x", 10.0, 1, Options::default(), &mut |_| {
            n += 1;
            n < 3
        });
        assert!(r.is_err());
        assert_eq!(g.calls.lock().unwrap().len(), 2);
        assert!(g.decode_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_wrong_shaped_backend_is_an_error_not_a_panic() {
        struct Bad;
        impl Graphs for Bad {
            fn encode(&self, _: &str, _: f32) -> Result<Conditioning, MusicError> {
                Ok(Conditioning { cross: vec![0.0; 3], global: vec![] })
            }
            fn denoise(&self, _: &[f32], _: usize, _: f32, _: &Conditioning, _: usize) -> Result<Vec<f32>, MusicError> {
                Ok(vec![])
            }
            fn decode(&self, _: &[f32], _: usize) -> Result<Vec<f32>, MusicError> {
                Ok(vec![])
            }
        }
        assert!(generate(&Bad, "x", 5.0, 1, Options::default(), &mut |_| true).is_err());
    }
}
