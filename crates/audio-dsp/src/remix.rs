//! Music remix: retime a piece of music to a target duration by cutting it at musically similar
//! beat boundaries (Clip ▸ Remix).
//!
//! ```text
//! mono mix ─► STFT (≈46 ms Hann, ¼ hop) ─► log-magnitude spectral flux = onset envelope
//!          ─► tempo: autocorrelation of the envelope, 50–200 BPM, log-Gaussian prior at 120 BPM
//!          ─► beats: dynamic programming (onset strength + penalty on beat-to-beat deviation
//!             from the period, squared log ratio), backtracked from the strongest end
//!          ─► per beat: 12-bin chroma and 12 mel cepstra (26 mel bands, DCT-II, c0 dropped)
//!          ─► beat self-similarity S(i, j) = 0.6 · cos(chroma) + 0.4 · (1 + cos(z-scored cepstra)) / 2
//! plan:  output = piece₀ ⧺ piece₁ ⧺ … ⧺ piece_k; each joint leaves the source at a beat boundary
//!        b_i and continues at another beat boundary b_j, chosen where the context of the jump
//!        (the beats before and after) is most similar: J(i, j) = mean of S along the diagonals
//!        through (i, j) over `CONTEXT` beats on both sides.
//! render: pieces concatenated with an equal-power crossfade centred on every joint.
//! ```
//!
//! **Parameters** (Premiere's Remix "Segments" and "Variations" sliders; our own semantics):
//!
//! | parameter | range | default | meaning |
//! |---|---|---|---|
//! | segments | 0 … 100 | 50 | number of joints: 0–33 → 1, 34–67 → 2, 68–100 → 3 (more when a long extension needs them) |
//! | variations | 0 … 100 | 50 | how far from the evenly spaced positions a joint may move to find a better match: ±(1 + 7·v/100) beats |
//!
//! The first piece starts at the beginning of the source and the last ends at its end (intro and
//! outro are kept). The plan length is within half a beat of the target whenever the beat grid
//! allows it, always within one beat. Everything is deterministic: the same audio, target and
//! parameters give the same plan.
//!
//! Positions are in samples of the analysed signal (`i64`).

use crate::fft::Fft;

/// Beats of context on each side of a joint that the similarity looks at.
pub const CONTEXT: usize = 4;
/// Shortest piece, in beats (no stutter edits).
pub const MIN_PIECE_BEATS: usize = 4;
/// Crossfade at each joint (seconds).
pub const XFADE_S: f64 = 0.02;
/// Long enough for a complete music cue while bounding the audio decoded and analysed by Remix.
pub const MAX_ANALYSIS_SECONDS: u64 = 10 * 60;
/// A dense similarity matrix is useful for planning, but must remain a bounded allocation.
const MAX_SIMILARITY_CELLS: usize = 4_000_000;
/// Remix analysis supports ordinary production and high-resolution audio rates, not arbitrary
/// integers supplied through a public library API.
pub const MAX_ANALYSIS_SAMPLE_RATE: u32 = 384_000;

/// One piece of the output: `len` samples read from the source at `src`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub src: i64,
    pub len: i64,
}

/// A remix: pieces played one after the other, crossfaded over `xfade` samples at the joints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub pieces: Vec<Piece>,
    pub xfade: i64,
}

impl Plan {
    /// The plan that plays the source unchanged.
    pub fn identity(len: i64, xfade: i64) -> Plan {
        Plan { pieces: vec![Piece { src: 0, len }], xfade }
    }
    /// Output length (samples).
    pub fn len(&self) -> i64 {
        self.pieces.iter().map(|p| p.len).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Output position of each joint (the start of every piece after the first).
    pub fn joints(&self) -> Vec<i64> {
        let mut out = Vec::new();
        let mut d = 0;
        for p in &self.pieces[..self.pieces.len().saturating_sub(1)] {
            d += p.len;
            out.push(d);
        }
        out
    }
    /// The source positions each joint cuts at: (end of the outgoing piece, start of the incoming one).
    pub fn cuts(&self) -> Vec<(i64, i64)> {
        self.pieces.windows(2).map(|w| (w[0].src + w[0].len, w[1].src)).collect()
    }
}

/// Remix sliders (see the module documentation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub segments: f64,
    pub variations: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params { segments: 50.0, variations: 50.0 }
    }
}

/// What the planner needs to know about the music.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    pub sample_rate: u32,
    /// Source length (samples).
    pub len: i64,
    /// Beat positions (samples), increasing.
    pub beats: Vec<i64>,
    /// Beat period (samples).
    pub period: f64,
    /// Tempo (beats per minute).
    pub bpm: f64,
    /// Beat self-similarity, `beats.len()²`, row-major; beat `i` is `[beats[i], beats[i + 1])`
    /// (the last runs to the end).
    pub sim: Vec<f32>,
}

impl Analysis {
    pub fn sim(&self, i: usize, j: usize) -> f32 {
        let m = self.beats.len();
        self.sim[i * m + j]
    }

    /// How well a jump from beat boundary `i` (the source is left there) to beat boundary `j`
    /// (playback continues there) fits: mean similarity of the beats after `i` and after `j`, and
    /// of the beats before them, over [`CONTEXT`] beats. 0 … 1.
    pub fn jump_score(&self, i: usize, j: usize) -> f32 {
        let m = self.beats.len();
        let (mut s, mut n) = (0f32, 0f32);
        for k in 0..CONTEXT {
            if i + k < m && j + k < m {
                s += self.sim(i + k, j + k);
                n += 1.0;
            }
            if i > k && j > k {
                s += self.sim(i - 1 - k, j - 1 - k);
                n += 1.0;
            }
        }
        if n > 0.0 { s / n } else { 0.0 }
    }
}

/// Why music cannot be remixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemixError {
    /// The source sample rate is zero or above the supported analysis limit.
    InvalidSampleRate,
    /// The source would exceed the bounded Remix analysis duration.
    TooLong,
    /// The detected beat grid would need too much similarity storage or work.
    TooComplex,
    /// Not enough rhythmic content to find a beat (silence, noise, speech).
    NoBeat,
    /// The source has fewer beats than a remix needs.
    TooShort,
    /// The target is shorter than the intro and outro the remix keeps.
    TargetTooShort,
}

impl std::fmt::Display for RemixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RemixError::InvalidSampleRate => "the audio sample rate is not supported for remix analysis",
            RemixError::TooLong => "the music is too long to remix",
            RemixError::TooComplex => "the music has too many beats to remix",
            RemixError::NoBeat => "no beat found in the audio",
            RemixError::TooShort => "the music is too short to remix",
            RemixError::TargetTooShort => "the target duration is too short for this music",
        })
    }
}

impl std::error::Error for RemixError {}

// ------------------------------------------------------------------------------------- analysis

fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos() as f32).collect()
}

/// Magnitude spectra of `x` with frame `n` and `hop` (frames start at `t·hop`).
fn stft_mag(x: &[f32], n: usize, hop: usize) -> Vec<Vec<f32>> {
    let fft = Fft::new(n);
    let w = hann(n);
    let frames = if x.len() >= n { (x.len() - n) / hop + 1 } else { 1 };
    let mut re = vec![0f32; n];
    let mut im = vec![0f32; n];
    let mut out = Vec::with_capacity(frames);
    for t in 0..frames {
        let s = t * hop;
        for i in 0..n {
            re[i] = x.get(s + i).copied().unwrap_or(0.0) * w[i];
            im[i] = 0.0;
        }
        fft.forward(&mut re, &mut im);
        out.push((0..n / 2 + 1).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect());
    }
    out
}

/// STFT frame for the onset envelope: the power of two nearest to 46 ms (1024 at 22.05 kHz, 2048
/// at 44.1/48 kHz).
fn onset_frame(sr: u32) -> usize {
    // `onset_envelope` is public on its own, so it must remain bounded even outside `analyze`.
    let n = (sr.min(MAX_ANALYSIS_SAMPLE_RATE) as f64 * 0.046).max(256.0);
    1usize << n.log2().round() as u32
}

/// Below this normalised autocorrelation at the beat period there is no usable pulse.
const MIN_PULSE: f64 = 0.1;

/// Onset strength per hop: half-wave rectified log-magnitude spectral flux, normalised to unit
/// standard deviation. Returns (envelope, frame size, hop).
pub fn onset_envelope(x: &[f32], sr: u32) -> (Vec<f32>, usize, usize) {
    let n = onset_frame(sr);
    let hop = n / 4;
    let mags = stft_mag(x, n, hop);
    let mut env = vec![0f32; mags.len()];
    let lg: Vec<Vec<f32>> = mags.iter().map(|m| m.iter().map(|v| (1.0 + 1000.0 * v).ln()).collect()).collect();
    for t in 1..lg.len() {
        env[t] = lg[t].iter().zip(&lg[t - 1]).map(|(a, b)| (a - b).max(0.0)).sum();
    }
    let mean = env.iter().sum::<f32>() / env.len().max(1) as f32;
    let sd = (env.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / env.len().max(1) as f32).sqrt();
    if sd > 0.0 {
        env.iter_mut().for_each(|v| *v /= sd);
    }
    (env, n, hop)
}

/// Beat period in envelope frames from the autocorrelation (50–200 BPM, prior centred at 120 BPM).
fn tempo(env: &[f32], frame_rate: f64) -> Option<f64> {
    let mean = env.iter().sum::<f32>() / env.len().max(1) as f32;
    let e: Vec<f32> = env.iter().map(|v| v - mean).collect();
    let lo = (frame_rate * 60.0 / 200.0).floor().max(2.0) as usize;
    let hi = ((frame_rate * 60.0 / 50.0).ceil() as usize).min(e.len() / 2);
    if hi <= lo + 2 {
        return None;
    }
    let ac = |lag: usize| -> f64 { e.iter().zip(&e[lag..]).map(|(a, b)| (*a as f64) * (*b as f64)).sum::<f64>() / (e.len() - lag) as f64 };
    let raw: Vec<f64> = (lo - 1..=hi + 1).map(ac).collect();
    let weighted = |k: usize| {
        let lag = (lo - 1 + k) as f64;
        let bpm = 60.0 * frame_rate / lag;
        raw[k] * (-0.5 * ((bpm / 120.0).log2() / 1.0).powi(2)).exp()
    };
    let (mut best, mut bk) = (f64::MIN, 0usize);
    for k in 1..raw.len() - 1 {
        let w = weighted(k);
        if w > best {
            best = w;
            bk = k;
        }
    }
    if best <= 0.0 || raw[bk] <= MIN_PULSE * ac(0) {
        return None;
    }
    // parabolic refinement on the raw autocorrelation
    let (a, b, c) = (raw[bk - 1], raw[bk], raw[bk + 1]);
    let den = a - 2.0 * b + c;
    let d = if den < 0.0 { (0.5 * (a - c) / den).clamp(-0.5, 0.5) } else { 0.0 };
    Some((lo - 1 + bk) as f64 + d)
}

/// Dynamic-programming beat tracker: frame indices of beats.
fn track_beats(env: &[f32], period: f64) -> Vec<usize> {
    let n = env.len();
    let mut score = vec![0f64; n];
    let mut back = vec![usize::MAX; n];
    let tight = 100.0;
    let lo = (period * 0.5).round().max(1.0) as usize;
    let hi = (period * 2.0).round() as usize;
    for t in 0..n {
        let mut best = 0.0f64;
        let mut arg = usize::MAX;
        if t >= lo {
            for prev in t.saturating_sub(hi)..=t - lo {
                let r = ((t - prev) as f64 / period).ln();
                let v = score[prev] - tight * r * r;
                if arg == usize::MAX || v > best {
                    best = v;
                    arg = prev;
                }
            }
        }
        score[t] = env[t] as f64 + if arg == usize::MAX { 0.0 } else { best.max(0.0) };
        back[t] = if arg != usize::MAX && best > 0.0 { arg } else { usize::MAX };
    }
    // strongest end within the last period
    let tail = n.saturating_sub(period.ceil() as usize + 1);
    let mut t = (tail..n).max_by(|&a, &b| score[a].total_cmp(&score[b]).then(b.cmp(&a))).unwrap_or(0);
    let mut beats = vec![t];
    while back[t] != usize::MAX {
        t = back[t];
        beats.push(t);
    }
    beats.reverse();
    beats
}

fn hz_to_mel(f: f64) -> f64 {
    2595.0 * (1.0 + f / 700.0).log10()
}

fn mel_to_hz(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2595.0) - 1.0)
}

/// Mel bands of the cepstra.
const BANDS: usize = 26;

/// Triangular mel filterbank (26 bands, 30 Hz … min(8 kHz, Nyquist)) for an `n`-point STFT: per
/// band, the bins it covers with their weights, in increasing bin order. Every other bin has
/// weight 0 in that band.
fn mel_weights(sr: u32, n: usize) -> Vec<Vec<(usize, f64)>> {
    let hz = |k: usize| k as f64 * sr as f64 / n as f64;
    let fmax = (sr as f64 / 2.0).min(8000.0);
    let (m0, m1) = (hz_to_mel(30.0), hz_to_mel(fmax));
    let edges: Vec<f64> = (0..BANDS + 2).map(|i| mel_to_hz(m0 + (m1 - m0) * i as f64 / (BANDS + 1) as f64)).collect();
    (0..BANDS)
        .map(|b| {
            let (l, cn, r) = (edges[b], edges[b + 1], edges[b + 2]);
            (0..n / 2 + 1)
                .filter_map(|k| {
                    let f = hz(k);
                    if f > l && f < cn {
                        Some((k, (f - l) / (cn - l)))
                    } else if f >= cn && f < r {
                        Some((k, (r - f) / (r - cn)))
                    } else {
                        Some((k, (r - f) / (r - cn)))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect()
}

/// Mel band energies of one magnitude spectrum.
fn mel_bands(L: &[Vec<(usize, f64)>], mag: &[f32]) -> [f64; BANDS] {
    let mut band = [0f64; BANDS];
    for (e, weights) in band.iter_mut().zip(L) {
        for &(k, w) in weights {
            let v = mag[k];
            *e += w * (v as f64) * (v as f64);
        }
    }
    band
}
}
