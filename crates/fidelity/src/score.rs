//! Scores: how far one render is from another.
//!
//! Video is compared as 8-bit sRGB frames over black: PSNR over the RGB channels (the usual
//! codec metric; ∞ when identical), the per-pixel maximum channel difference (mean, 99th
//! percentile, maximum) and the perceptual colour difference CIEDE2000 (ΔE₀₀, Sharma et al.
//! 2005; mean, 95th percentile and maximum over the pixels). Audio is compared sample by sample
//! after the best alignment within a search window is found (a render that starts a few samples
//! late is reported as a lag, not as noise): SNR of the reference over the difference, the
//! largest sample error and the RMS error, per channel and overall.
//!
//! Every function here is pure and sized by its inputs: frames of different sizes are an error,
//! never a panic, and a request to compare nothing scores nothing.

use serde::{Deserialize, Serialize};

/// An 8-bit RGBA frame, row-major, no padding (alpha is ignored by the scores).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ScoreError {
    #[error("frame is {w}×{h} but has {len} bytes (need {need})")]
    FrameSize { w: u32, h: u32, len: usize, need: usize },
    #[error("frames differ in size: {0}×{1} vs {2}×{3}")]
    SizeMismatch(u32, u32, u32, u32),
    #[error("nothing to compare: {0}")]
    Empty(&'static str),
}

impl Frame {
    pub fn new(w: u32, h: u32, rgba: Vec<u8>) -> Result<Self, ScoreError> {
        let need =
            (w as usize).checked_mul(h as usize).and_then(|n| n.checked_mul(4)).ok_or(ScoreError::FrameSize { w, h, len: rgba.len(), need: usize::MAX })?;
        if rgba.len() != need {
            return Err(ScoreError::FrameSize { w, h, len: rgba.len(), need });
        }
        Ok(Self { w, h, rgba })
    }

    pub fn pixels(&self) -> impl Iterator<Item = &[u8; 4]> {
        self.rgba.as_chunks::<4>().0.iter()
    }
}

/// Difference between two frames.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameScore {
    /// PSNR over R, G, B in dB; `None` when the frames are identical (infinite).
    pub psnr_db: Option<f64>,
    /// Sum of squared RGB differences (so scores of several frames can be pooled).
    pub sse: f64,
    /// Mean of the per-pixel maximum channel difference.
    pub mean_abs: f64,
    /// 99th percentile of the per-pixel maximum channel difference.
    pub p99_abs: u8,
    pub max_abs: u8,
    pub de2000_mean: f64,
    pub de2000_p95: f64,
    pub de2000_max: f64,
    pub pixels: usize,
}

/// The reference level (8-bit full scale) used by [`psnr`].
const PEAK: f64 = 255.0;

/// PSNR in dB of a mean squared error, `None` for zero error.
pub fn psnr(mse: f64) -> Option<f64> {
    if mse <= 0.0 || !mse.is_finite() {
        return None;
    }
    Some(10.0 * (PEAK * PEAK / mse).log10())
}

/// Compare two frames of the same size.
pub fn frame_score(ours: &Frame, reference: &Frame) -> Result<FrameScore, ScoreError> {
    if (ours.w, ours.h) != (reference.w, reference.h) {
        return Err(ScoreError::SizeMismatch(ours.w, ours.h, reference.w, reference.h));
    }
    let pixels = ours.w as usize * ours.h as usize;
    if pixels == 0 {
        return Err(ScoreError::Empty("frame has no pixels"));
    }
    let mut sse = 0.0f64;
    let mut hist = [0usize; 256];
    let mut abs_sum = 0u64;
    let mut max_abs = 0u8;
    let mut de: Vec<f64> = Vec::with_capacity(pixels);
    let mut de_sum = 0.0f64;
    let mut de_max = 0.0f64;
    for (a, b) in ours.pixels().zip(reference.pixels()) {
        let mut px_max = 0u8;
        for k in 0..3 {
            let d = i32::from(a[k]) - i32::from(b[k]);
            sse += f64::from(d * d);
            px_max = px_max.max(d.unsigned_abs().min(255) as u8);
        }
        if let Some(h) = hist.get_mut(px_max as usize) {
            *h += 1;
        }
        abs_sum += u64::from(px_max);
        max_abs = max_abs.max(px_max);
        let e = if a[..3] == b[..3] { 0.0 } else { ciede2000(srgb8_to_lab([a[0], a[1], a[2]]), srgb8_to_lab([b[0], b[1], b[2]])) };
        de.push(e);
        de_sum += e;
        de_max = de_max.max(e);
    }
    let mse = sse / (pixels as f64 * 3.0);
    de.sort_by(|x, y| x.total_cmp(y));
    Ok(FrameScore {
        psnr_db: psnr(mse),
        sse,
        mean_abs: abs_sum as f64 / pixels as f64,
        p99_abs: percentile_u8(&hist, pixels, 0.99),
        max_abs,
        de2000_mean: de_sum / pixels as f64,
        de2000_p95: percentile_sorted(&de, 0.95),
        de2000_max: de_max,
        pixels,
    })
}

/// The smallest value whose cumulative count reaches `q` of `n` samples.
fn percentile_u8(hist: &[usize; 256], n: usize, q: f64) -> u8 {
    let target = ((n as f64 * q).ceil() as usize).clamp(1, n.max(1));
    let mut acc = 0usize;
    for (v, c) in hist.iter().enumerate() {
        acc += c;
        if acc >= target {
            return v.min(255) as u8;
        }
    }
    255
}

/// The `q` quantile of an ascending slice (nearest rank).
fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() as f64 * q).ceil() as usize).clamp(1, sorted.len());
    sorted.get(rank - 1).copied().unwrap_or(0.0)
}

/// Video scores pooled over the compared frames of a case.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoScore {
    pub frames_ours: usize,
    pub frames_reference: usize,
    /// Frames actually compared (the shorter side).
    pub frames_compared: usize,
    /// PSNR of the pooled squared error over every compared pixel; `None` when identical.
    pub psnr_db: Option<f64>,
    /// The worst single frame (index, PSNR); `None` when every frame is identical.
    pub worst_frame: Option<(usize, f64)>,
    pub mean_abs: f64,
    pub p99_abs: u8,
    pub max_abs: u8,
    pub de2000_mean: f64,
    pub de2000_p95: f64,
    pub de2000_max: f64,
    pub per_frame: Vec<FrameScore>,
}

/// Compare frame sequences pairwise (the shorter one decides how many).
pub fn video_score(ours: &[Frame], reference: &[Frame]) -> Result<VideoScore, ScoreError> {
    let n = ours.len().min(reference.len());
    if n == 0 {
        return Err(ScoreError::Empty("no frames on one side"));
    }
    let mut per_frame = Vec::with_capacity(n);
    for (a, b) in ours.iter().zip(reference.iter()) {
        per_frame.push(frame_score(a, b)?);
    }
    let pixels: usize = per_frame.iter().map(|f| f.pixels).sum();
    let sse: f64 = per_frame.iter().map(|f| f.sse).sum();
    let mse = sse / (pixels.max(1) as f64 * 3.0);
    let worst_frame = per_frame.iter().enumerate().filter_map(|(i, f)| f.psnr_db.map(|p| (i, p))).min_by(|a, b| a.1.total_cmp(&b.1));
    // the pooled 99th percentile is approximated by the worst per-frame p99
    let p99_abs = per_frame.iter().map(|f| f.p99_abs).max().unwrap_or(0);
    let p95 = per_frame.iter().map(|f| f.de2000_p95).fold(0.0, f64::max);
    Ok(VideoScore {
        frames_ours: ours.len(),
        frames_reference: reference.len(),
        frames_compared: n,
        psnr_db: psnr(mse),
        worst_frame,
        mean_abs: per_frame.iter().map(|f| f.mean_abs * f.pixels as f64).sum::<f64>() / pixels.max(1) as f64,
        p99_abs,
        max_abs: per_frame.iter().map(|f| f.max_abs).max().unwrap_or(0),
        de2000_mean: per_frame.iter().map(|f| f.de2000_mean * f.pixels as f64).sum::<f64>() / pixels.max(1) as f64,
        de2000_p95: p95,
        de2000_max: per_frame.iter().map(|f| f.de2000_max).fold(0.0, f64::max),
        per_frame,
    })
}

// ---- colour difference ----

/// sRGB electro-optical transfer function (IEC 61966-2-1).
fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// CIE L*a*b* (D65 white, 2° observer) of an 8-bit sRGB pixel.
pub fn srgb8_to_lab(rgb: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(|c| srgb_to_linear(f64::from(c) / 255.0));
    // sRGB / BT.709 primaries to XYZ (D65)
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;
    xyz_to_lab([x, y, z])
}

/// CIE XYZ (D65) to L*a*b*.
pub fn xyz_to_lab(xyz: [f64; 3]) -> [f64; 3] {
    const WHITE: [f64; 3] = [0.950_47, 1.0, 1.088_83];
    const DELTA: f64 = 6.0 / 29.0;
    let f = |t: f64| if t > DELTA * DELTA * DELTA { t.cbrt() } else { t / (3.0 * DELTA * DELTA) + 4.0 / 29.0 };
    let [fx, fy, fz] = [f(xyz[0] / WHITE[0]), f(xyz[1] / WHITE[1]), f(xyz[2] / WHITE[2])];
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// CIEDE2000 colour difference between two L*a*b* colours (Sharma, Wu & Dalal 2005).
pub fn ciede2000(lab1: [f64; 3], lab2: [f64; 3]) -> f64 {
    let [l1, a1, b1] = lab1;
    let [l2, a2, b2] = lab2;
    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let c_bar = (c1 + c2) / 2.0;
    let c_bar7 = c_bar.powi(7);
    let g = 0.5 * (1.0 - (c_bar7 / (c_bar7 + 25f64.powi(7))).sqrt());
    let a1p = (1.0 + g) * a1;
    let a2p = (1.0 + g) * a2;
    let c1p = a1p.hypot(b1);
    let c2p = a2p.hypot(b2);
    let hue = |a: f64, b: f64| if a == 0.0 && b == 0.0 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    let h1p = hue(a1p, b1);
    let h2p = hue(a2p, b2);
    let dl = l2 - l1;
    let dc = c2p - c1p;
    let dh = if c1p * c2p == 0.0 {
        0.0
    } else {
        let d = h2p - h1p;
        if d > 180.0 {
            d - 360.0
        } else if d < -180.0 {
            d + 360.0
        } else {
            d
        }
    };
    let dh_big = 2.0 * (c1p * c2p).sqrt() * (dh / 2.0).to_radians().sin();
    let l_bar = (l1 + l2) / 2.0;
    let cp_bar = (c1p + c2p) / 2.0;
    let h_bar = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (h_bar - 30.0).to_radians().cos() + 0.24 * (2.0 * h_bar).to_radians().cos() + 0.32 * (3.0 * h_bar + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_bar - 63.0).to_radians().cos();
    let d_theta = 30.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp();
    let cp_bar7 = cp_bar.powi(7);
    let rc = 2.0 * (cp_bar7 / (cp_bar7 + 25f64.powi(7))).sqrt();
    let l50 = (l_bar - 50.0).powi(2);
    let sl = 1.0 + 0.015 * l50 / (20.0 + l50).sqrt();
    let sc = 1.0 + 0.045 * cp_bar;
    let sh = 1.0 + 0.015 * cp_bar * t;
    let rt = -(2.0 * d_theta).to_radians().sin() * rc;
    let (tl, tc, th) = (dl / sl, dc / sc, dh_big / sh);
    (tl * tl + tc * tc + th * th + rt * tc * th).max(0.0).sqrt()
}

// ---- audio ----

/// Sample-level difference between two audio signals.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioScore {
    pub channels_ours: usize,
    pub channels_reference: usize,
    pub frames_ours: usize,
    pub frames_reference: usize,
    /// Frames compared after alignment (the overlap).
    pub frames_compared: usize,
    /// Reference length minus ours, in samples.
    pub length_delta: i64,
    /// Alignment found: reference sample `i + lag` is compared with our sample `i`.
    pub lag: i64,
    /// Largest lag that was searched.
    pub max_lag: usize,
    /// Reference power over error power in dB, over every compared channel; `None` when the
    /// compared samples are identical.
    pub snr_db: Option<f64>,
    pub per_channel_snr_db: Vec<Option<f64>>,
    pub max_abs: f32,
    pub rms_error: f64,
    pub peak_ours: f32,
    pub peak_reference: f32,
}

/// In-place iterative radix-2 complex FFT (`re`/`im` of equal power-of-two length).
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len().min(im.len());
    if n < 2 || !n.is_power_of_two() {
        return;
    }
    // bit-reversal permutation
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = if inverse { std::f64::consts::TAU / len as f64 } else { -std::f64::consts::TAU / len as f64 };
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let (i, j) = (start + k, start + k + len / 2);
                let (Some(&ur), Some(&ui), Some(&vr), Some(&vi)) = (re.get(i), im.get(i), re.get(j), im.get(j)) else { return };
                let (tr, ti) = (vr * cr - vi * ci, vr * ci + vi * cr);
                if let Some(v) = re.get_mut(i) {
                    *v = ur + tr;
                }
                if let Some(v) = im.get_mut(i) {
                    *v = ui + ti;
                }
                if let Some(v) = re.get_mut(j) {
                    *v = ur - tr;
                }
                if let Some(v) = im.get_mut(j) {
                    *v = ui - ti;
                }
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
    if inverse {
        let s = 1.0 / n as f64;
        re.iter_mut().for_each(|v| *v *= s);
        im.iter_mut().for_each(|v| *v *= s);
    }
}

/// Cross-correlation `r(lag) = Σ_i a[i]·b[i + lag]` for every lag, by FFT (linear, no
/// wrap-around). Returns the values and the lag of the first one (`-(a.len() - 1)`).
fn cross_correlation(a: &[f64], b: &[f64]) -> (Vec<f64>, i64) {
    let la = a.len();
    let lb = b.len();
    let n = (la + lb).saturating_sub(1).max(2).next_power_of_two();
    let (mut ar, mut ai) = (vec![0.0f64; n], vec![0.0f64; n]);
    let (mut br, mut bi) = (vec![0.0f64; n], vec![0.0f64; n]);
    ar.iter_mut().zip(a).for_each(|(o, &v)| *o = v);
    br.iter_mut().zip(b).for_each(|(o, &v)| *o = v);
    fft(&mut ar, &mut ai, false);
    fft(&mut br, &mut bi, false);
    // conj(A) · B
    for k in 0..n {
        let (Some(&xr), Some(&xi), Some(&yr), Some(&yi)) = (ar.get(k), ai.get(k), br.get(k), bi.get(k)) else { break };
        if let (Some(o), Some(p)) = (br.get_mut(k), bi.get_mut(k)) {
            *o = xr * yr + xi * yi;
            *p = xr * yi - xi * yr;
        }
    }
    fft(&mut br, &mut bi, true);
    // index k = positive lag k (k < lb); index n - m = negative lag -m (m < la)
    let first = -(la as i64).saturating_sub(1);
    let out: Vec<f64> =
        (first..lb as i64).map(|lag| if lag >= 0 { br.get(lag as usize) } else { br.get(n - (-lag) as usize) }.copied().unwrap_or(0.0)).collect();
    (out, first)
}

/// The lag (in samples, `|lag| ≤ max_lag`) at which `reference[i + lag]` best matches `ours[i]`
/// by least squared error with silence beyond either end, which is the lag of maximum
/// cross-correlation. Energy that only one side has at a given time counts as error, so a
/// periodic signal (a tone) aligns on its lead-in and envelope, not on an arbitrary cycle. Ties
/// go to the smaller |lag|.
pub fn best_lag(ours: &[f32], reference: &[f32], max_lag: usize) -> i64 {
    if ours.is_empty() || reference.is_empty() || max_lag == 0 {
        return 0;
    }
    let a: Vec<f64> = ours.iter().map(|&v| f64::from(v)).collect();
    let b: Vec<f64> = reference.iter().map(|&v| f64::from(v)).collect();
    let (r, first) = cross_correlation(&a, &b);
    let scale = r.iter().fold(0.0f64, |m, v| m.max(v.abs())).max(1e-300);
    let eps = scale * 1e-9;
    let max_lag = max_lag as i64;
    let mut best = (0i64, f64::NEG_INFINITY);
    for (i, &v) in r.iter().enumerate() {
        let lag = first + i as i64;
        if lag.abs() > max_lag {
            continue;
        }
        if v > best.1 + eps || (v >= best.1 - eps && lag.abs() < best.0.abs()) {
            best = (lag, v);
        }
    }
    best.0
}

/// Compare two planar signals (one `Vec` per channel). The lag is found on the first channel
/// (or the channel sum when there are several) and applied to all; channels beyond the shorter
/// side's count are ignored.
pub fn audio_score(ours: &[Vec<f32>], reference: &[Vec<f32>], max_lag: usize) -> Result<AudioScore, ScoreError> {
    let channels = ours.len().min(reference.len());
    if channels == 0 {
        return Err(ScoreError::Empty("no audio channels on one side"));
    }
    let frames_ours = ours.iter().map(Vec::len).min().unwrap_or(0);
    let frames_reference = reference.iter().map(Vec::len).min().unwrap_or(0);
    if frames_ours == 0 || frames_reference == 0 {
        return Err(ScoreError::Empty("no audio samples on one side"));
    }
    let sum = |chs: &[Vec<f32>], n: usize| -> Vec<f32> {
        let mut s = vec![0.0f32; n];
        for ch in chs.iter().take(channels) {
            for (o, v) in s.iter_mut().zip(ch.iter()) {
                *o += v;
            }
        }
        s
    };
    let lag = if channels == 1 {
        best_lag(ours.first().map(Vec::as_slice).unwrap_or_default(), reference.first().map(Vec::as_slice).unwrap_or_default(), max_lag)
    } else {
        best_lag(&sum(ours, frames_ours), &sum(reference, frames_reference), max_lag)
    };
    let start_o = (-lag).max(0) as usize;
    let start_r = lag.max(0) as usize;
    let n = frames_ours.saturating_sub(start_o).min(frames_reference.saturating_sub(start_r));
    if n == 0 {
        return Err(ScoreError::Empty("signals do not overlap at the found lag"));
    }
    let (mut sig, mut err, mut max_abs) = (0.0f64, 0.0f64, 0.0f32);
    let mut per_channel = Vec::with_capacity(channels);
    let (mut peak_o, mut peak_r) = (0.0f32, 0.0f32);
    for (o, r) in ours.iter().zip(reference.iter()).take(channels) {
        let (mut s, mut e) = (0.0f64, 0.0f64);
        for (x, y) in o.iter().skip(start_o).take(n).zip(r.iter().skip(start_r).take(n)) {
            let d = f64::from(*y) - f64::from(*x);
            s += f64::from(*y) * f64::from(*y);
            e += d * d;
            max_abs = max_abs.max(d.abs() as f32);
            peak_o = peak_o.max(x.abs());
            peak_r = peak_r.max(y.abs());
        }
        per_channel.push(snr(s, e));
        sig += s;
        err += e;
    }
    Ok(AudioScore {
        channels_ours: ours.len(),
        channels_reference: reference.len(),
        frames_ours,
        frames_reference,
        frames_compared: n,
        length_delta: frames_reference as i64 - frames_ours as i64,
        lag,
        max_lag,
        snr_db: snr(sig, err),
        per_channel_snr_db: per_channel,
        max_abs,
        rms_error: (err / (n as f64 * channels as f64)).sqrt(),
        peak_ours: peak_o,
        peak_reference: peak_r,
    })
}

/// Signal-to-noise ratio in dB from signal and error energies; `None` for zero error.
fn snr(signal: f64, error: f64) -> Option<f64> {
    if error <= 0.0 {
        return None;
    }
    Some(10.0 * (signal.max(1e-20) / error).log10())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32, rgb: [u8; 3]) -> Frame {
        Frame::new(w, h, (0..w * h).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect()).unwrap()
    }

    #[test]
    fn identical_frames_are_perfect() {
        let a = flat(8, 4, [10, 200, 30]);
        let s = frame_score(&a, &a).unwrap();
        assert_eq!(s.psnr_db, None);
        assert_eq!((s.max_abs, s.p99_abs), (0, 0));
        assert_eq!(s.de2000_mean, 0.0);
        assert_eq!(s.pixels, 32);
    }

    #[test]
    fn psnr_of_a_known_error() {
        // every channel off by 2: MSE 4 → 10·log10(255²/4) = 42.11 dB
        let s = frame_score(&flat(4, 4, [100, 100, 100]), &flat(4, 4, [102, 98, 102])).unwrap();
        let p = s.psnr_db.unwrap();
        assert!((p - 42.1102).abs() < 1e-3, "{p}");
        assert_eq!((s.max_abs, s.p99_abs), (2, 2));
        assert_eq!(s.mean_abs, 2.0);
        assert_eq!(psnr(0.0), None);
        assert_eq!(psnr(f64::NAN), None);
    }

    #[test]
    fn percentiles_follow_the_outliers() {
        let mut px: Vec<u8> = (0..100).flat_map(|_| [50, 50, 50, 255]).collect();
        // one pixel far off
        px[0] = 250;
        let a = Frame::new(10, 10, px).unwrap();
        let s = frame_score(&a, &flat(10, 10, [50, 50, 50])).unwrap();
        assert_eq!(s.max_abs, 200);
        assert_eq!(s.p99_abs, 0, "one in a hundred is above the 99th percentile");
        assert!(s.de2000_max > 10.0 && s.de2000_p95 == 0.0, "{s:?}");
    }

    #[test]
    fn size_mismatch_and_bad_buffers_are_errors() {
        assert_eq!(frame_score(&flat(2, 2, [0; 3]), &flat(2, 3, [0; 3])), Err(ScoreError::SizeMismatch(2, 2, 2, 3)));
        assert!(matches!(Frame::new(2, 2, vec![0; 15]), Err(ScoreError::FrameSize { .. })));
        assert!(Frame::new(u32::MAX, u32::MAX, vec![]).is_err());
        assert_eq!(video_score(&[], &[flat(1, 1, [0; 3])]), Err(ScoreError::Empty("no frames on one side")));
    }

    #[test]
    fn lab_of_white_and_black() {
        let w = srgb8_to_lab([255, 255, 255]);
        assert!((w[0] - 100.0).abs() < 0.01 && w[1].abs() < 0.01 && w[2].abs() < 0.01, "{w:?}");
        let k = srgb8_to_lab([0, 0, 0]);
        assert_eq!(k, [0.0, 0.0, 0.0]);
        let r = srgb8_to_lab([255, 0, 0]);
        assert!((r[0] - 53.24).abs() < 0.1 && (r[1] - 80.09).abs() < 0.2 && (r[2] - 67.20).abs() < 0.2, "{r:?}");
    }

    /// Pairs from the CIEDE2000 test data of Sharma, Wu & Dalal (2005), table 1.
    #[test]
    fn ciede2000_reference_pairs() {
        let pairs: [([f64; 3], [f64; 3], f64); 5] = [
            ([50.0, 2.6772, -79.7751], [50.0, 0.0, -82.7485], 2.0425),
            ([50.0, 3.1571, -77.2803], [50.0, 0.0, -82.7485], 2.8615),
            ([50.0, 2.8361, -74.0200], [50.0, 0.0, -82.7485], 3.4412),
            ([50.0, 0.0, 0.0], [50.0, -1.0, 2.0], 2.3669),
            ([50.0, 2.5, 0.0], [73.0, 25.0, -18.0], 27.1492),
        ];
        for (a, b, want) in pairs {
            let got = ciede2000(a, b);
            assert!((got - want).abs() < 1e-4, "{a:?} vs {b:?}: got {got}, want {want}");
            assert!((ciede2000(b, a) - want).abs() < 1e-4, "symmetric");
        }
        assert_eq!(ciede2000([50.0, 10.0, -20.0], [50.0, 10.0, -20.0]), 0.0);
        assert!((ciede2000([100.0, 0.0, 0.0], [0.0, 0.0, 0.0]) - 100.0).abs() < 1e-9, "white vs black");
    }

    #[test]
    fn video_score_pools_frames_and_finds_the_worst() {
        let base = flat(4, 4, [120, 120, 120]);
        let ours = vec![base.clone(), base.clone(), base.clone()];
        let reference = vec![base.clone(), flat(4, 4, [124, 120, 120]), flat(4, 4, [121, 121, 121]), base.clone()];
        let s = video_score(&ours, &reference).unwrap();
        assert_eq!((s.frames_ours, s.frames_reference, s.frames_compared), (3, 4, 3));
        assert_eq!(s.worst_frame.map(|(i, _)| i), Some(1));
        assert_eq!(s.max_abs, 4);
        // pooled MSE = (16 + 3) / 9 per channel-sample over 3 frames → (16·16 + 3·16) / (48·3)
        let mse = (16.0 * 16.0 + 3.0 * 16.0) / (48.0 * 3.0);
        assert!((s.psnr_db.unwrap() - 10.0 * (255.0f64 * 255.0 / mse).log10()).abs() < 1e-9);
        assert_eq!(s.per_frame.len(), 3);
        let same = video_score(&ours, &ours).unwrap();
        assert_eq!((same.psnr_db, same.worst_frame), (None, None));
    }

    #[test]
    fn fft_cross_correlation_matches_brute_force() {
        // a small deterministic pseudo-random pair, odd lengths (so padding and both lag signs matter)
        let mut seed = 12345u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) - 0.5
        };
        let a: Vec<f64> = (0..37).map(|_| next()).collect();
        let b: Vec<f64> = (0..53).map(|_| next()).collect();
        let (r, first) = cross_correlation(&a, &b);
        assert_eq!(first, -36);
        assert_eq!(r.len(), 36 + 53);
        for (i, got) in r.iter().enumerate() {
            let lag = first + i as i64;
            let want: f64 = (0..a.len() as i64).filter_map(|k| Some(a[k as usize] * *b.get(usize::try_from(k + lag).ok()?)?)).sum();
            assert!((got - want).abs() < 1e-9, "lag {lag}: got {got}, want {want}");
        }
        // degenerate sizes are harmless
        let mut re = vec![1.0, 2.0, 3.0];
        let mut im = vec![0.0; 3];
        fft(&mut re, &mut im, false);
        assert_eq!(re, vec![1.0, 2.0, 3.0], "non-power-of-two input is left alone");
        assert_eq!(cross_correlation(&[], &[1.0]).0.len(), 0, "nothing correlates with nothing");
    }

    fn tone(n: usize, hz: f64, amp: f32, phase: usize) -> Vec<f32> {
        (0..n).map(|i| amp * ((i + phase) as f64 * hz * std::f64::consts::TAU / 48_000.0).sin() as f32).collect()
    }

    #[test]
    fn identical_audio_is_perfect_at_lag_zero() {
        let a = vec![tone(4800, 440.0, 0.5, 0), tone(4800, 440.0, 0.25, 7)];
        let s = audio_score(&a, &a, 200).unwrap();
        assert_eq!((s.lag, s.snr_db, s.max_abs), (0, None, 0.0));
        assert_eq!(s.per_channel_snr_db, vec![None, None]);
        assert_eq!((s.frames_compared, s.length_delta), (4800, 0));
    }

    #[test]
    fn a_delayed_copy_is_found_at_its_lag() {
        // the reference starts 37 samples of silence later than ours
        let ours = tone(9600, 300.0, 0.4, 0);
        let mut reference = vec![0.0f32; 37];
        reference.extend_from_slice(&ours);
        let s = audio_score(std::slice::from_ref(&ours), std::slice::from_ref(&reference), 100).unwrap();
        assert_eq!(s.lag, 37, "{s:?}");
        assert_eq!(s.snr_db, None);
        assert_eq!(s.length_delta, 37);
        // and the other way round (reference early)
        let s = audio_score(&[reference], &[ours], 100).unwrap();
        assert_eq!(s.lag, -37);
        assert_eq!(s.snr_db, None);
    }

    #[test]
    fn large_lags_are_found_through_the_coarse_search() {
        let ours = tone(48_000, 997.0, 0.4, 0);
        let mut reference = vec![0.0f32; 1234];
        reference.extend_from_slice(&ours);
        assert_eq!(best_lag(&ours, &reference, 2000), 1234);
        assert_eq!(best_lag(&ours, &reference, 0), 0, "no search, no lag");
        assert_eq!(best_lag(&[], &reference, 2000), 0);
    }

    #[test]
    fn snr_of_known_noise() {
        let ours = tone(4800, 1000.0, 0.5, 0);
        // reference = ours + a 0.005 DC offset: SNR = 10·log10(mean(ours²)/0.005²) = 10·log10(0.125/2.5e-5) ≈ 36.99 dB
        let reference: Vec<f32> = ours.iter().map(|v| v + 0.005).collect();
        let s = audio_score(&[ours], &[reference], 10).unwrap();
        assert_eq!(s.lag, 0);
        let snr = s.snr_db.unwrap();
        assert!((snr - 36.99).abs() < 0.05, "{snr}");
        assert!((s.max_abs - 0.005).abs() < 1e-6);
        assert!((s.rms_error - 0.005).abs() < 1e-6);
        assert!((s.peak_reference - 0.505).abs() < 1e-3);
    }

    #[test]
    fn silence_and_empty_inputs() {
        let z = vec![vec![0.0f32; 100]];
        let s = audio_score(&z, &z, 10).unwrap();
        assert_eq!((s.lag, s.snr_db), (0, None));
        assert!(audio_score(&[], &z, 10).is_err());
        assert!(audio_score(&[vec![]], &z, 10).is_err());
        // silence against a tone: finite, poor SNR, no panic
        let s = audio_score(&z, &[tone(100, 440.0, 0.5, 0)], 10).unwrap();
        assert!(s.snr_db.is_some_and(|v| v.is_finite()));
    }
}
