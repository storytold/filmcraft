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
        self.pieces.iter().fold(0i64, |a, p| a.saturating_add(p.len))
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Output position of each joint (the start of every piece after the first).
    pub fn joints(&self) -> Vec<i64> {
        let mut out = Vec::new();
        let mut d = 0;
        for p in &self.pieces[..self.pieces.len().saturating_sub(1)] {
            d = i64::saturating_add(d, p.len);
            out.push(d);
        }
        out
    }
    /// The source positions each joint cuts at: (end of the outgoing piece, start of the incoming one).
    pub fn cuts(&self) -> Vec<(i64, i64)> {
        self.pieces.windows(2).map(|w| (w[0].src.saturating_add(w[0].len), w[1].src)).collect()
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
    // capped so a corrupt sample rate cannot ask for a gigabyte FFT
    let n = (sr as f64 * 0.046).clamp(256.0, 32768.0);
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

/// Per-beat chroma (12) and cepstra (12) from a 4× larger STFT.
fn beat_features(x: &[f32], sr: u32, beats: &[i64]) -> (Vec<[f32; 12]>, Vec<[f32; 12]>) {
    let n = (onset_frame(sr) * 2).max(1024);
    let hop = n / 2;
    let mags = stft_mag(x, n, hop);
    let bins = n / 2 + 1;
    let hz = |k: usize| k as f64 * sr as f64 / n as f64;
    // chroma map
    let pc: Vec<Option<usize>> = (0..bins)
        .map(|k| {
            let f = hz(k);
            (55.0..=5000.0).contains(&f).then(|| (((12.0 * (f / 440.0).log2()).round() as i64 + 9).rem_euclid(12)) as usize)
        })
        .collect();
    // mel filterbank (26 bands, 30 Hz … min(8 kHz, Nyquist))
    const BANDS: usize = 26;
    let fmax = (sr as f64 / 2.0).min(8000.0);
    let (m0, m1) = (hz_to_mel(30.0), hz_to_mel(fmax));
    let edges: Vec<f64> = (0..BANDS + 2).map(|i| mel_to_hz(m0 + (m1 - m0) * i as f64 / (BANDS + 1) as f64)).collect();
    let mut frame_chroma = Vec::with_capacity(mags.len());
    let mut frame_cep = Vec::with_capacity(mags.len());
    for m in &mags {
        let mut c = [0f32; 12];
        for (k, v) in m.iter().enumerate() {
            if let Some(p) = pc[k] {
                c[p] += v * v;
            }
        }
        frame_chroma.push(c);
        let mut band = [0f64; BANDS];
        for (b, e) in band.iter_mut().enumerate() {
            let (l, cn, r) = (edges[b], edges[b + 1], edges[b + 2]);
            for (k, v) in m.iter().enumerate() {
                let f = hz(k);
                let w = if f > l && f < cn {
                    (f - l) / (cn - l)
                } else if f >= cn && f < r {
                    (r - f) / (r - cn)
                } else {
                    0.0
                };
                *e += w * (*v as f64) * (*v as f64);
            }
        }
        let lb: Vec<f64> = band.iter().map(|e| (e + 1e-10).ln()).collect();
        let mut cep = [0f32; 12];
        for (q, c) in cep.iter_mut().enumerate() {
            let q = q + 1;
            *c = lb.iter().enumerate().map(|(b, v)| v * (std::f64::consts::PI * q as f64 * (b as f64 + 0.5) / BANDS as f64).cos()).sum::<f64>() as f32;
        }
        frame_cep.push(cep);
    }
    let mut chroma = Vec::with_capacity(beats.len());
    let mut cep = Vec::with_capacity(beats.len());
    let end = x.len() as i64;
    for (i, &b) in beats.iter().enumerate() {
        let e = beats.get(i + 1).copied().unwrap_or(end);
        let (mut c, mut q, mut cnt) = ([0f32; 12], [0f32; 12], 0f32);
        for (t, (fc, fq)) in frame_chroma.iter().zip(&frame_cep).enumerate() {
            let centre = (t * hop + n / 2) as i64;
            if centre >= b && centre < e {
                for k in 0..12 {
                    c[k] += fc[k];
                    q[k] += fq[k];
                }
                cnt += 1.0;
            }
        }
        if cnt > 0.0 {
            q.iter_mut().for_each(|v| *v /= cnt);
        }
        let norm = c.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            c.iter_mut().for_each(|v| *v /= norm);
        }
        chroma.push(c);
        cep.push(q);
    }
    // z-score the cepstra across beats
    let m = cep.len().max(1) as f32;
    for k in 0..12 {
        let mean = cep.iter().map(|c| c[k]).sum::<f32>() / m;
        let sd = (cep.iter().map(|c| (c[k] - mean).powi(2)).sum::<f32>() / m).sqrt().max(1e-6);
        cep.iter_mut().for_each(|c| c[k] = (c[k] - mean) / sd);
    }
    (chroma, cep)
}

fn cos(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let d: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|v| v * v).sum::<f32>().sqrt();
    let nb = b.iter().map(|v| v * v).sum::<f32>().sqrt();
    if na > 0.0 && nb > 0.0 { d / (na * nb) } else { 0.0 }
}

/// How far [`Analysis::beats`] sit before the attack of each beat (half a crossfade), samples.
pub fn beat_lead(sample_rate: u32) -> i64 {
    (XFADE_S * 0.5 * sample_rate as f64).round() as i64
}

fn local_max(env: &[f32], t: usize, r: usize) -> f32 {
    env[t.saturating_sub(r)..(t + r + 1).min(env.len())].iter().copied().fold(0.0, f32::max)
}

/// Analyse planar audio (channels are summed to mono).
pub fn analyze(channels: &[&[f32]], sample_rate: u32) -> Result<Analysis, RemixError> {
    let len = channels.iter().map(|c| c.len()).max().unwrap_or(0);
    let k = 1.0 / channels.len().max(1) as f32;
    let mono: Vec<f32> = (0..len).map(|i| channels.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum::<f32>() * k).collect();
    let (env, n, hop) = onset_envelope(&mono, sample_rate);
    let frame_rate = sample_rate as f64 / hop as f64;
    let period = tempo(&env, frame_rate).ok_or(RemixError::NoBeat)?;
    let mut frames = track_beats(&env, period);
    // weak beats at the edges (silence before / after the music) are not beats
    let mut strengths: Vec<f32> = frames.iter().map(|&t| local_max(&env, t, 2)).collect();
    strengths.sort_by(f32::total_cmp);
    let median = strengths.get(strengths.len() / 2).copied().unwrap_or(0.0);
    while frames.first().is_some_and(|&t| local_max(&env, t, 2) < 0.3 * median) {
        frames.remove(0);
    }
    while frames.last().is_some_and(|&t| local_max(&env, t, 2) < 0.3 * median) {
        frames.pop();
    }
    // …and edge beats off the grid (the tracker's end search can settle on an off-beat)
    let off_grid = |a: usize, b: usize| ((b - a) as f64 / period - 1.0).abs() > 0.05;
    while frames.len() > 2 && off_grid(frames[frames.len() - 2], frames[frames.len() - 1]) {
        frames.pop();
    }
    while frames.len() > 2 && off_grid(frames[0], frames[1]) {
        frames.remove(0);
    }
    // the flux of frame t peaks as soon as an attack enters the newer window, at about
    // t·hop + n − ¾·hop (measured on generated drums); joints sit half a crossfade earlier so a joint's fade is over
    // when the beat's attack starts
    let mut beats: Vec<i64> =
        frames.iter().map(|&t| (t * hop + n) as i64 - 3 * hop as i64 / 4 - beat_lead(sample_rate)).filter(|&b| b > 0 && b < len as i64).collect();
    beats.dedup();
    if beats.len() < 2 * MIN_PIECE_BEATS + 2 {
        return Err(if beats.len() < 4 { RemixError::NoBeat } else { RemixError::TooShort });
    }
    let (chroma, cep) = beat_features(&mono, sample_rate, &beats);
    let m = beats.len();
    let mut sim = vec![0f32; m * m];
    for i in 0..m {
        for j in 0..m {
            sim[i * m + j] = 0.6 * cos(&chroma[i], &chroma[j]) + 0.4 * (1.0 + cos(&cep[i], &cep[j])) * 0.5;
        }
    }
    let period_s = period * hop as f64;
    Ok(Analysis { sample_rate, len: len as i64, beats, period: period_s, bpm: 60.0 * sample_rate as f64 / period_s, sim })
}

// ------------------------------------------------------------------------------------- planning

/// Number of joints the Segments slider asks for.
pub fn joints_for(segments: f64) -> usize {
    1 + (segments.clamp(0.0, 100.0) / 34.0).floor() as usize
}

/// Plan a remix of the analysed music to `target` samples.
pub fn plan(a: &Analysis, target: i64, params: Params) -> Result<Plan, RemixError> {
    let xfade = (XFADE_S * a.sample_rate as f64).round() as i64;
    let len = a.len.clamp(0, i64::MAX / 4);
    // a corrupt or absurd target must not overflow the arithmetic below or make the joint search run for ages
    let target = target.clamp(i64::MIN / 4, i64::MAX / 4);
    let half = (a.period / 2.0) as i64;
    if (target - len).abs() <= half {
        return Ok(Plan::identity(len, xfade));
    }
    let b = &a.beats;
    let m = b.len();
    if m == 0 {
        return Err(RemixError::NoBeat);
    }
    let min_piece = (MIN_PIECE_BEATS as f64 * a.period) as i64;
    if target < 2 * min_piece {
        return Err(RemixError::TargetTooShort);
    }
    let window = 1 + (7.0 * params.variations.clamp(0.0, 100.0) / 100.0).round() as usize;
    let mut k = joints_for(params.segments).min(((target - len).abs() as f64 / a.period).round().max(1.0) as usize);
    // pieces may not get shorter than the minimum
    while k > 1 && target / (k as i64 + 1) < min_piece {
        k -= 1;
    }
    // a backwards jump can span at most the music between the intro and outro pieces
    if target > len {
        let max_span = (len - 2 * min_piece).max(a.period as i64);
        while ((target - len) as f64 / k as f64) > max_span as f64 * 0.9 && (target / (k as i64 + 2)) >= min_piece && k < m {
            k += 1;
        }
    }
    let nearest = |x: i64| -> usize { b.partition_point(|&v| v < x).min(m - 1) };
    let closest = |x: i64| -> usize {
        let i = nearest(x);
        if i > 0 && (x - b[i - 1]).abs() <= (b[i] - x).abs() { i - 1 } else { i }
    };
    let mut pieces = Vec::new();
    let mut src = 0i64; // start of the current piece in the source
    let mut out = 0i64; // output length so far
    for jn in 0..k {
        let left = (k - jn) as i64; // joints still to place (this one included)
        let out_left = target - out; // output still to make
        let piece = out_left / (left + 1); // even split of what is left
        let src_left = len - src;
        let span = (src_left - out_left) / left; // source skipped (+) or replayed (−) per joint
        let ni = closest(src + piece);
        let last = jn + 1 == k;
        let mut best: Option<(f32, i64, usize, usize)> = None;
        let lo_i = ni.saturating_sub(window);
        for i in lo_i..=(ni + window).min(m - 1) {
            if b[i] - src < min_piece {
                continue;
            }
            let nj = closest(b[i] + span);
            for j in nj.saturating_sub(window)..=(nj + window).min(m - 1) {
                if j == i || len - b[j] < min_piece || (b[j] > b[i]) != (target < len) {
                    continue;
                }
                let new_out = out + (b[i] - src);
                let rest_src = len - b[j];
                // what the remaining joints still have to absorb
                let err = (target - new_out) - rest_src;
                if last {
                    if err.abs() > half.max(1) * 2 {
                        continue;
                    }
                } else {
                    // remaining joints must still be able to fit pieces of the minimum length
                    let rem_out = target - new_out;
                    if rem_out < (left) * min_piece {
                        continue;
                    }
                    // the joints after this one must still each skip (or replay) a beat or more
                    let rem = rest_src - rem_out;
                    if rem.signum() != (len - target).signum() || (rem.abs() as f64) < (left - 1) as f64 * a.period * 0.75 {
                        continue;
                    }
                }
                let score = a.jump_score(i, j);
                // final joint: land as close to the target as possible, then by similarity
                let key_err = if last { err.abs() / half.max(1) } else { 0 };
                let better = match best {
                    None => true,
                    Some((s, e, bi, bj)) => {
                        let bad = key_err.cmp(&e);
                        bad.is_lt() || (bad.is_eq() && (score > s || (score == s && (i, j) < (bi, bj))))
                    }
                };
                if better {
                    best = Some((score, key_err, i, j));
                }
            }
        }
        let Some((_, _, i, j)) = best else {
            return Err(RemixError::TargetTooShort);
        };
        pieces.push(Piece { src, len: b[i] - src });
        out += b[i] - src;
        src = b[j];
    }
    pieces.push(Piece { src, len: len - src });
    Ok(Plan { pieces, xfade })
}

// ------------------------------------------------------------------------------------- render

/// Render output samples `[out0, out0 + n)` of `plan` with `channels` channels. `read(src, len)`
/// returns planar source samples `[src, src + len)` (silence outside the source is fine). Each
/// joint at output position `J` crossfades over `[J − xfade/2, J + xfade/2)` with equal-power
/// gains; the result does not depend on how the output is cut into requests.
pub fn render(plan: &Plan, out0: i64, n: usize, channels: usize, read: &mut dyn FnMut(i64, usize) -> Vec<Vec<f32>>) -> Vec<Vec<f32>> {
    let mut out = vec![vec![0f32; n]; channels];
    let x = plan.xfade.max(0);
    let h0 = x / 2;
    let h1 = x - h0;
    let end = out0.saturating_add(n as i64);
    let last = plan.pieces.len().saturating_sub(1);
    let mut d = 0i64;
    for (pi, p) in plan.pieces.iter().enumerate() {
        let (ds, de) = (d, d.saturating_add(p.len));
        d = de;
        let fade_in = pi > 0 && x > 0;
        let fade_out = pi < last && x > 0;
        let s = if fade_in { ds.saturating_sub(h0) } else { ds };
        let e = if fade_out { de.saturating_add(h1) } else { de };
        let (a, z) = (s.max(out0), e.min(end));
        if z <= a {
            continue;
        }
        let len = (z - a) as usize;
        let buf = read(p.src.saturating_add(a.saturating_sub(ds)), len);
        for i in 0..len {
            let o = a + i as i64;
            let mut g = 1.0f64;
            if fade_in && o < ds.saturating_add(h1) {
                let u = o.saturating_sub(ds.saturating_sub(h0)) as f64 + 0.5;
                g *= (u / x as f64 * std::f64::consts::FRAC_PI_2).sin();
            }
            if fade_out && o >= de.saturating_sub(h0) {
                let u = o.saturating_sub(de.saturating_sub(h0)) as f64 + 0.5;
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

// ------------------------------------------------------------------------------------- serialise

impl Plan {
    /// Text form: `xfade;src,len;src,len;…` (the unit is the caller's).
    pub fn to_text(&self) -> String {
        let mut s = self.xfade.to_string();
        for p in &self.pieces {
            s.push_str(&format!(";{},{}", p.src, p.len));
        }
        s
    }
    pub fn from_text(s: &str) -> Option<Plan> {
        let mut it = s.split(';');
        let xfade = it.next()?.trim().parse().ok()?;
        let mut pieces = Vec::new();
        for p in it {
            let (a, b) = p.split_once(',')?;
            pieces.push(Piece { src: a.trim().parse().ok()?, len: b.trim().parse().ok()? });
        }
        (!pieces.is_empty()).then_some(Plan { pieces, xfade })
    }
}

/// Generated rhythmic test music: drums on every beat, a bass note per beat and a chord pad per
/// bar following `sections` of four-bar progressions. Returns stereo planar audio and the true
/// beat positions (samples). Used by tests here and in the engine.
pub fn test_music(sr: u32, bpm: f64, bars: usize, seed: u32) -> (Vec<Vec<f32>>, Vec<i64>) {
    let beat = sr as f64 * 60.0 / bpm;
    let n = (bars as f64 * 4.0 * beat).round() as usize;
    let mut l = vec![0f32; n];
    let mut rng = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let mut noise = move || {
        rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (rng >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    };
    // chord roots (semitones from A3 = 220 Hz): section A: C Am F G, B: Dm G Em A
    let a_sec = [3, 0, 8, 10];
    let b_sec = [5, 10, 7, 0];
    let form = [a_sec, a_sec, b_sec, a_sec, b_sec, a_sec];
    let minor = |r: i32| matches!(r, 0 | 5 | 7);
    let tau = std::f64::consts::TAU;
    let mut beats = Vec::new();
    for bar in 0..bars {
        let root = form[(bar / 4) % form.len()][bar % 4];
        let third = if minor(root) { 3 } else { 4 };
        let notes = [root, root + third, root + 7];
        let b0 = (bar as f64 * 4.0 * beat).round() as usize;
        let b1 = (((bar + 1) as f64) * 4.0 * beat).round() as usize;
        for (i, s) in l.iter_mut().enumerate().take(b1.min(n)).skip(b0) {
            let t = (i - b0) as f64 / sr as f64;
            let env = (1.0 - (-t * 30.0).exp()) * 0.9f64.powf(t);
            let mut v = 0.0;
            for nt in notes {
                let f = 220.0 * 2f64.powf(nt as f64 / 12.0);
                v += (tau * f * i as f64 / sr as f64).sin() + 0.3 * (tau * 2.0 * f * i as f64 / sr as f64).sin();
            }
            *s += (0.06 * env * v) as f32;
        }
        for q in 0..4 {
            let start = ((bar * 4 + q) as f64 * beat).round() as usize;
            beats.push(start as i64);
            let bass_f = 110.0 * 2f64.powf(root as f64 / 12.0) / if q % 2 == 0 { 1.0 } else { 2.0f64.powf(-7.0 / 12.0) };
            let dur = (beat * 0.9) as usize;
            for k in 0..dur.min(n.saturating_sub(start)) {
                let t = k as f64 / sr as f64;
                let i = start + k;
                // kick (1 and 3) or snare (2 and 4)
                if q % 2 == 0 {
                    let f = 50.0 + 90.0 * (-t * 25.0).exp();
                    l[i] += (0.8 * (-t * 9.0).exp() * (tau * f * t).sin()) as f32;
                } else if t < 0.2 {
                    l[i] += 0.35 * (-t as f32 * 22.0).exp() * noise();
                }
                // bass
                l[i] += (0.15 * (-t * 3.0).exp() * (tau * bass_f * t).sin()) as f32;
            }
            // hats on 8ths
            for h in 0..2 {
                let hs = start + (h as f64 * beat / 2.0) as usize;
                for k in 0..(sr as usize / 25).min(n.saturating_sub(hs)) {
                    let t = k as f32 / sr as f32;
                    l[hs + k] += 0.08 * (-t * 120.0).exp() * noise();
                }
            }
        }
    }
    let r = l.clone();
    (vec![l, r], beats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 22_050;

    fn music(bpm: f64, bars: usize) -> (Vec<Vec<f32>>, Vec<i64>, Analysis) {
        let (m, truth) = test_music(SR, bpm, bars, 7);
        let refs: Vec<&[f32]> = m.iter().map(Vec::as_slice).collect();
        let a = analyze(&refs, SR).expect("analysis");
        (m, truth, a)
    }

    #[test]
    fn finds_tempo_and_beats() {
        for (bpm, sr) in [(100.0, SR), (120.0, SR), (128.0, SR), (120.0, 48_000), (90.0, 44_100)] {
            let (m, truth) = test_music(sr, bpm, 16, 7);
            let refs: Vec<&[f32]> = m.iter().map(Vec::as_slice).collect();
            let a = analyze(&refs, sr).expect("analysis");
            assert!((a.bpm - bpm).abs() < 1.0, "bpm {} vs {bpm}", a.bpm);
            // every detected beat (plus the lead) is within 10 ms of a true beat; at most 2 true beats missed
            let tol = (0.010 * sr as f64) as i64;
            let mut worst = 0;
            for b in a.beats.iter().map(|b| b + beat_lead(sr)) {
                let d = truth.iter().map(|t| (t - b).abs()).min().unwrap();
                if d > tol {
                    eprintln!("beat {b} off by {d} (first true {}, last true {}, len {})", truth[0], truth[truth.len() - 1], a.len);
                }
                worst = worst.max(d);
            }
            assert!(worst <= tol, "worst beat error {} samples ({bpm} BPM)", worst);
            assert!(a.beats.len() + 2 >= truth.len(), "{} beats of {}", a.beats.len(), truth.len());
            eprintln!("{bpm} BPM at {sr} Hz: detected {:.2}, {} beats, worst error {:.1} ms", a.bpm, a.beats.len(), worst as f64 * 1000.0 / sr as f64);
        }
    }

    #[test]
    fn similarity_prefers_same_section() {
        let (_, _, a) = music(120.0, 24);
        // bar 0 (A section, C chord) is closer to bar 4 (A, C) than to bar 8 (B, Dm)
        let first = |bar: usize| a.beats.iter().position(|&b| b >= (bar as f64 * 4.0 * a.period) as i64 - (a.period / 4.0) as i64).unwrap();
        let (i, ja, jb) = (first(1), first(5), first(9));
        assert!(a.jump_score(i, ja) > a.jump_score(i, jb), "{} vs {}", a.jump_score(i, ja), a.jump_score(i, jb));
    }

    #[test]
    fn plans_hit_targets_at_beat_boundaries() {
        let (_, _, a) = music(120.0, 24); // 48 s
        let beat = a.period as i64;
        for secs in [20.0, 30.0, 40.0, 47.0, 60.0, 75.0, 100.0] {
            for segments in [0.0, 50.0, 100.0] {
                for variations in [0.0, 50.0, 100.0] {
                    let target = (secs * SR as f64) as i64;
                    let p = plan(&a, target, Params { segments, variations }).expect("plan");
                    let err = (p.len() - target).abs();
                    assert!(err <= beat, "{secs} s seg {segments} var {variations}: error {err} > beat {beat}");
                    assert_eq!(p.pieces[0].src, 0, "starts at the intro");
                    let lastp = p.pieces.last().unwrap();
                    assert_eq!(lastp.src + lastp.len, a.len, "ends at the outro");
                    for (out_end, in_start) in p.cuts() {
                        assert!(a.beats.contains(&out_end), "cut out at {out_end} is not a beat");
                        assert!(a.beats.contains(&in_start), "cut in at {in_start} is not a beat");
                    }
                    for pc in &p.pieces {
                        assert!(pc.len >= (MIN_PIECE_BEATS as f64 * a.period) as i64 - 2, "piece too short: {pc:?}");
                    }
                    if segments == 50.0 && variations == 50.0 {
                        eprintln!("{secs} s: {} pieces, error {:.1} ms", p.pieces.len(), err as f64 * 1000.0 / SR as f64);
                    }
                }
            }
        }
    }

    #[test]
    fn deterministic_and_parameters_matter() {
        let (_, _, a) = music(120.0, 24);
        let (_, _, a2) = music(120.0, 24);
        assert_eq!(a, a2);
        let t = 30 * SR as i64;
        let p = plan(&a, t, Params::default()).unwrap();
        assert_eq!(p, plan(&a2, t, Params::default()).unwrap());
        let few = plan(&a, t, Params { segments: 0.0, variations: 50.0 }).unwrap();
        let many = plan(&a, t, Params { segments: 100.0, variations: 50.0 }).unwrap();
        assert!(few.pieces.len() < many.pieces.len(), "{} vs {}", few.pieces.len(), many.pieces.len());
        // identity when the target is the source length
        assert_eq!(plan(&a, a.len, Params::default()).unwrap().pieces.len(), 1);
        assert_eq!(plan(&a, SR as i64, Params::default()), Err(RemixError::TargetTooShort));
    }

    #[test]
    fn silence_and_noise_have_no_beat() {
        let z = vec![0f32; SR as usize * 10];
        assert!(analyze(&[&z], SR).is_err());
        let tone: Vec<f32> = (0..SR as usize * 20).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        assert!(analyze(&[&tone], SR).is_err());
        let mut x = 12345u32;
        let noise: Vec<f32> = (0..SR as usize * 20)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect();
        assert!(analyze(&[&noise], SR).is_err());
    }

    #[test]
    fn render_crossfades_without_clicks_and_ignores_block_size() {
        let (m, _, a) = music(120.0, 24);
        let p = plan(&a, 30 * SR as i64, Params::default()).unwrap();
        let mut read = |s: i64, n: usize| -> Vec<Vec<f32>> {
            m.iter().map(|c| (0..n as i64).map(|k| c.get((s + k).max(0) as usize).copied().unwrap_or(0.0)).collect()).collect()
        };
        let whole = render(&p, 0, p.len() as usize, 2, &mut read);
        // block-size independence
        let mut cut = vec![Vec::new(), Vec::new()];
        let mut pos = 0i64;
        let mut step = 1usize;
        while pos < p.len() {
            let n = step.min((p.len() - pos) as usize);
            let b = render(&p, pos, n, 2, &mut read);
            for c in 0..2 {
                cut[c].extend_from_slice(&b[c]);
            }
            pos += n as i64;
            step = step * 7 % 4093 + 1;
        }
        assert_eq!(whole, cut);
        // outside crossfades the output equals the source pieces exactly
        let mut d = 0i64;
        for pc in &p.pieces {
            for k in (p.xfade..pc.len - p.xfade).step_by(97) {
                assert_eq!(whole[0][(d + k) as usize], m[0][(pc.src + k) as usize]);
            }
            d += pc.len;
        }
        // no clicks: the largest sample-to-sample step at a joint is no larger than in the music
        let max_step = |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
        let music_step = max_step(&m[0]);
        for j in p.joints() {
            let s = (j - p.xfade) as usize;
            let e = (j + p.xfade) as usize;
            assert!(max_step(&whole[0][s..e]) <= music_step * 1.05, "click at joint {j}");
        }
        // equal-power gains: in the fade, gain² of both sides sums to 1
        let plan2 = Plan { pieces: vec![Piece { src: 0, len: 100 }, Piece { src: 0, len: 100 }], xfade: 20 };
        let mut ones = |_s: i64, n: usize| vec![vec![1.0f32; n]];
        let mut first = |s: i64, n: usize| vec![(0..n as i64).map(|k| if s + k < 100 + 10 && s + k >= 0 { 1.0 } else { 0.0 }).collect()];
        let _ = render(&plan2, 0, 200, 1, &mut ones);
        let g = render(&Plan { pieces: vec![Piece { src: 0, len: 100 }, Piece { src: 1000, len: 100 }], xfade: 20 }, 85, 30, 1, &mut first);
        for (i, v) in g[0].iter().enumerate() {
            let o = 85 + i as i64;
            if (90..110).contains(&o) {
                let u = (o - 90) as f64 + 0.5;
                let exp = (u / 20.0 * std::f64::consts::FRAC_PI_2).cos() as f32;
                assert!((v - exp).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn text_roundtrip() {
        let p = Plan { pieces: vec![Piece { src: 0, len: 10 }, Piece { src: 50, len: 7 }], xfade: 3 };
        assert_eq!(Plan::from_text(&p.to_text()), Some(p));
        assert_eq!(Plan::from_text("x"), None);
    }
}
