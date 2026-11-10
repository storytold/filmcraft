//! Subframe prediction (RFC 9639 §9.2.5–9.2.6) and residual coding (§9.2.7): the fixed polynomial
//! predictors, linear prediction from the windowed autocorrelation (Levinson–Durbin), and
//! partitioned Rice codes. Everything here only measures and chooses; [`crate::frame`] writes.

/// Residuals larger than this are not Rice coded (a 5-bit parameter tops out at 30).
const MAX_RESIDUAL: i64 = (1 << 30) - 1;

/// A residual coded with partitioned Rice codes.
#[derive(Clone, Debug, PartialEq)]
pub struct Residual {
    /// Rice parameter per partition; `1 << order` partitions.
    pub params: Vec<u8>,
    pub partition_order: u8,
    /// Total bits of the residual section, coding method included.
    pub bits: u64,
    /// The folded residual after the warm-up samples.
    pub folded: Vec<u32>,
}

impl Residual {
    /// The 5-bit parameter method is needed when any parameter passes 14 (15 is the escape code).
    pub fn wide(&self) -> bool {
        self.params.iter().any(|&k| k > 14)
    }
}

/// How one channel of one block is coded.
#[derive(Clone, Debug, PartialEq)]
pub enum Subframe {
    Constant(i32),
    Verbatim,
    Fixed { order: u8, residual: Residual },
    Lpc { coefs: Vec<i32>, precision: u8, shift: u8, residual: Residual },
}

/// Folded residual: 0, -1, 1, -2, 2 … → 0, 1, 2, 3, 4 ….
fn fold(r: i64) -> u64 {
    if r >= 0 { (r as u64) << 1 } else { ((-r - 1) as u64) << 1 | 1 }
}

/// Bits of `n` folded values summing to `sum` with Rice parameter `k`: unary quotient, stop bit,
/// `k` low bits. The quotient total is approximated by `sum >> k`, exact up to the per-value
/// carries; [`rice_bits_exact`] gives the true cost for the chosen parameter.
fn rice_estimate(sum: u64, n: u64, k: u32) -> u64 {
    (sum >> k) + n * (u64::from(k) + 1)
}

fn rice_bits_exact(u: &[u32], k: u32) -> u64 {
    u.iter().map(|&v| u64::from(v >> k)).sum::<u64>() + u.len() as u64 * (u64::from(k) + 1)
}

/// The parameter for a partition of `n` folded values summing to `sum`.
fn param_for(sum: u64, n: u64) -> u32 {
    if n == 0 {
        return 0;
    }
    let mean = sum / n;
    let guess = if mean == 0 { 0 } else { 63 - mean.leading_zeros() };
    let mut best = (0u32, u64::MAX);
    for k in guess.saturating_sub(1)..=(guess + 1).min(30) {
        let b = rice_estimate(sum, n, k);
        if b < best.1 {
            best = (k, b);
        }
    }
    best.0
}

/// One partition while choosing the partition order.
#[derive(Clone, Copy)]
struct Part {
    sum: u64,
    n: u64,
    k: u32,
    bits: u64,
}

/// Choose the partition order and parameters for the residual of a block of `block` samples whose
/// first `order` samples are warm-up (`u` holds the folded residual after them).
///
/// The partition sums of the finest usable order are added pairwise for the coarser orders, and a
/// partition whose two halves chose the same parameter reuses their exact bit counts.
fn code_residual(u: Vec<u32>, block: usize, order: usize, max_partition_order: u8) -> Residual {
    let mut top = None;
    for po in 0..=max_partition_order {
        let parts = 1usize << po;
        if !block.is_multiple_of(parts) || block / parts <= order {
            break;
        }
        top = Some(po);
    }
    let Some(top) = top else { return Residual { params: vec![0], partition_order: 0, bits: u64::MAX, folded: u } };
    let len = block >> top;
    let mut finest = Vec::with_capacity(1 << top);
    let mut start = 0usize;
    for p in 0..1usize << top {
        let n = if p == 0 { len - order } else { len };
        let chunk = u.get(start..start + n).unwrap_or_default();
        let sum: u64 = chunk.iter().map(|&v| u64::from(v)).sum();
        let k = param_for(sum, n as u64);
        finest.push(Part { sum, n: n as u64, k, bits: rice_bits_exact(chunk, k) });
        start += n;
    }
    let mut levels = vec![finest];
    for po in (0..top).rev() {
        let len = block >> po;
        let Some(finer) = levels.last() else { break };
        let mut level = Vec::with_capacity(finer.len() / 2);
        let mut start = 0usize;
        for (p, &[a, b]) in finer.as_chunks::<2>().0.iter().enumerate() {
            // a and b: the two halves of partition p, one order finer
            let (sum, n) = (a.sum + b.sum, a.n + b.n);
            let k = param_for(sum, n);
            let bits = if a.k == k && b.k == k { a.bits + b.bits } else { rice_bits_exact(u.get(start..start + n as usize).unwrap_or_default(), k) };
            level.push(Part { sum, n, k, bits });
            start += if p == 0 { len - order } else { len };
        }
        levels.push(level);
    }
    levels.reverse();
    let mut best: Option<(usize, u64)> = None;
    for (po, level) in levels.iter().enumerate() {
        let wide = level.iter().any(|p| p.k > 14);
        let bits = level.iter().map(|p| p.bits).sum::<u64>() + 2 + 4 + level.len() as u64 * if wide { 5 } else { 4 };
        if best.is_none_or(|b| bits < b.1) {
            best = Some((po, bits));
        }
    }
    let (po, bits) = best.unwrap_or((0, u64::MAX));
    let params = levels.get(po).map_or_else(Vec::new, |l| l.iter().map(|p| p.k as u8).collect());
    Residual { params, partition_order: po as u8, bits, folded: u }
}

/// The folded residual of an order-`N` integer predictor (`c[0]` weighs the previous sample), or
/// None when a residual does not fit. Monomorphised per order so the dot product is unrolled.
fn predict_fold<const N: usize>(x: &[i32], c: &[i32], shift: u8) -> Option<Vec<u32>> {
    let c: [i32; N] = c.try_into().ok()?;
    let mut out = Vec::with_capacity(x.len().saturating_sub(N));
    for w in x.windows(N + 1) {
        let mut p = 0i64;
        for j in 0..N {
            p += i64::from(c[j]) * i64::from(w[N - 1 - j]);
        }
        let r = i64::from(w[N]) - (p >> shift);
        if r.abs() > MAX_RESIDUAL {
            return None;
        }
        out.push(fold(r) as u32);
    }
    Some(out)
}

fn predict_fold_any(x: &[i32], c: &[i32], shift: u8) -> Option<Vec<u32>> {
    let mut out = Vec::with_capacity(x.len().saturating_sub(c.len()));
    for w in x.windows(c.len() + 1) {
        let p: i64 = c.iter().zip(w.iter().rev().skip(1)).map(|(&c, &s)| i64::from(c) * i64::from(s)).sum();
        let r = i64::from(*w.last()?) - (p >> shift);
        if r.abs() > MAX_RESIDUAL {
            return None;
        }
        out.push(fold(r) as u32);
    }
    Some(out)
}

/// The folded residual of linear prediction with `c` (any order), or None when it does not fit.
pub fn lpc_fold(x: &[i32], c: &[i32], shift: u8) -> Option<Vec<u32>> {
    match c.len() {
        0 => predict_fold::<0>(x, c, shift),
        1 => predict_fold::<1>(x, c, shift),
        2 => predict_fold::<2>(x, c, shift),
        3 => predict_fold::<3>(x, c, shift),
        4 => predict_fold::<4>(x, c, shift),
        5 => predict_fold::<5>(x, c, shift),
        6 => predict_fold::<6>(x, c, shift),
        7 => predict_fold::<7>(x, c, shift),
        8 => predict_fold::<8>(x, c, shift),
        9 => predict_fold::<9>(x, c, shift),
        10 => predict_fold::<10>(x, c, shift),
        11 => predict_fold::<11>(x, c, shift),
        12 => predict_fold::<12>(x, c, shift),
        _ => predict_fold_any(x, c, shift),
    }
}

/// The fixed polynomial predictors of RFC 9639 §9.2.5 as integer coefficients.
const FIXED: [&[i32]; 5] = [&[], &[1], &[2, -1], &[3, -3, 1], &[4, -6, 4, -1]];

/// The folded residual of fixed predictor `order` (0–4), or None when it does not fit.
pub fn fixed_fold(x: &[i32], order: usize) -> Option<Vec<u32>> {
    lpc_fold(x, FIXED.get(order)?, 0)
}

/// Tukey(0.5) window: flat in the middle half, raised-cosine tapers.
pub fn window(n: usize) -> Vec<f64> {
    let taper = (n / 4).max(1);
    (0..n)
        .map(|i| {
            let edge = i.min(n - 1 - i);
            if edge >= taper { 1.0 } else { 0.5 * (1.0 - (std::f64::consts::PI * edge as f64 / taper as f64).cos()) }
        })
        .collect()
}

/// Linear prediction coefficients of every order 1..=`max` (Levinson–Durbin on the windowed
/// autocorrelation); empty for a silent or degenerate block.
fn lpc_orders(x: &[i32], max: usize, w: &[f64]) -> Vec<Vec<f64>> {
    let n = x.len();
    if n <= max || w.len() != n {
        return Vec::new();
    }
    let s: Vec<f64> = x.iter().zip(w).map(|(&v, &w)| f64::from(v) * w).collect();
    let r: Vec<f64> = (0..=max).map(|lag| s.iter().zip(s.iter().skip(lag)).map(|(a, b)| a * b).sum()).collect();
    let Some(&r0) = r.first() else { return Vec::new() };
    if r0 <= 0.0 {
        return Vec::new();
    }
    let mut err = r0 * (1.0 + 1e-9);
    let mut a: Vec<f64> = Vec::new();
    let mut all = Vec::with_capacity(max);
    for i in 0..max {
        let ri = r.get(i + 1).copied().unwrap_or(0.0);
        let acc: f64 = a.iter().enumerate().map(|(j, &aj)| aj * r.get(i - j).copied().unwrap_or(0.0)).sum();
        let k = (ri - acc) / err;
        if !k.is_finite() {
            break;
        }
        let prev = a.clone();
        a.push(k);
        for j in 0..i {
            if let (Some(aj), Some(&p)) = (a.get_mut(j), prev.get(i - 1 - j)) {
                *aj -= k * p;
            }
        }
        err *= 1.0 - k * k;
        all.push(a.clone());
        if err <= 0.0 {
            break;
        }
    }
    all
}

/// Quantise coefficients to `precision`-bit integers with a right shift (0–15), carrying the
/// rounding error forward so the quantised filter stays close to the real one.
fn quantise(coefs: &[f64], precision: u8) -> Option<(Vec<i32>, u8)> {
    let cmax = coefs.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    if !(cmax.is_finite() && cmax > 0.0) {
        return None;
    }
    let lim = (1i64 << (precision - 1)) - 1;
    // the largest shift that keeps the biggest coefficient inside the precision
    let log = cmax.log2().floor() as i32 + 1;
    let shift = (i32::from(precision) - 1 - log).clamp(0, 15);
    let scale = f64::from(1u32 << shift);
    let mut carry = 0.0;
    let mut q = Vec::with_capacity(coefs.len());
    for &c in coefs {
        carry += c * scale;
        let v = (carry.round() as i64).clamp(-lim - 1, lim);
        carry -= v as f64;
        q.push(v as i32);
    }
    Some((q, shift as u8))
}

/// Encoding effort, from the compression level.
#[derive(Clone, Copy, Debug)]
pub struct Effort {
    pub max_lpc_order: usize,
    pub max_partition_order: u8,
    pub stereo: bool,
}

impl Effort {
    pub fn level(level: u8) -> Effort {
        let (max_lpc_order, max_partition_order, stereo) = match level {
            0 => (0, 3, false),
            1 | 2 => (0, 3, true),
            3 => (6, 4, false),
            4 => (8, 4, true),
            5 => (8, 5, true),
            6 => (8, 6, true),
            _ => (12, 6, true),
        };
        Effort { max_lpc_order, max_partition_order, stereo }
    }
}

/// The cheapest coding of one channel of `bps`-bit samples, and its size in bits.
pub fn choose(x: &[i32], bps: u8, effort: Effort, win: &[f64]) -> (Subframe, u64) {
    let n = x.len();
    let b = u64::from(bps);
    let head = 8; // zero bit, type, wasted-bits flag
    let first = x.first().copied().unwrap_or(0);
    if x.iter().all(|&v| v == first) {
        return (Subframe::Constant(first), head + b);
    }
    let mut best = (Subframe::Verbatim, head + n as u64 * b);
    let mut consider = |sub: Subframe, bits: u64| {
        if bits < best.1 {
            best = (sub, bits);
        }
    };
    for order in 0..=4usize.min(n.saturating_sub(1)) {
        let Some(r) = fixed_fold(x, order) else { continue };
        let residual = code_residual(r, n, order, effort.max_partition_order);
        let bits = head + order as u64 * b + residual.bits;
        consider(Subframe::Fixed { order: order as u8, residual }, bits);
    }
    // 15-bit coefficients, fewer for short blocks (their side information weighs more)
    let precision: u8 = if n <= 1152 { 12 } else { 15 };
    for coefs in lpc_orders(x, effort.max_lpc_order, win) {
        let Some((q, shift)) = quantise(&coefs, precision) else { continue };
        let order = q.len();
        let Some(r) = lpc_fold(x, &q, shift) else { continue };
        let residual = code_residual(r, n, order, effort.max_partition_order);
        let bits = head + order as u64 * (b + u64::from(precision)) + 4 + 5 + residual.bits;
        consider(Subframe::Lpc { coefs: q, precision, shift, residual }, bits);
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding() {
        assert_eq!([0, -1, 1, -2, 2].map(fold), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn predictable_signals_beat_verbatim() {
        // a ramp is exactly predicted by the order-2 fixed predictor
        let ramp: Vec<i32> = (0..4096).map(|i| i * 3 - 6000).collect();
        let (sub, bits) = choose(&ramp, 16, Effort::level(5), &window(ramp.len()));
        assert!(matches!(sub, Subframe::Fixed { order: 2, .. } | Subframe::Lpc { .. }), "{sub:?}");
        assert!(bits < 4096 * 2, "{bits}");
        // a sine is well predicted by LPC
        let sine: Vec<i32> = (0..4096).map(|i| ((i as f64 * 0.05).sin() * 20000.0) as i32).collect();
        let (_, bits) = choose(&sine, 16, Effort::level(5), &window(sine.len()));
        assert!(bits < 4096 * 6, "{bits}");
        assert_eq!(choose(&[7; 100], 16, Effort::level(5), &window(100)), (Subframe::Constant(7), 24));
    }

    #[test]
    fn lpc_residual_inverts() {
        let x: Vec<i32> = (0..500).map(|i| ((i as f64 * 0.1).sin() * 1000.0) as i32).collect();
        let coefs = lpc_orders(&x, 4, &window(x.len())).pop().unwrap();
        let (q, shift) = quantise(&coefs, 15).unwrap();
        let r: Vec<i64> = lpc_fold(&x, &q, shift).unwrap().iter().map(|&u| if u & 1 == 0 { i64::from(u >> 1) } else { -i64::from(u >> 1) - 1 }).collect();
        // rebuild the signal from the warm-up and the residual, as a decoder would
        let mut y: Vec<i32> = x[..q.len()].to_vec();
        for (i, &res) in r.iter().enumerate() {
            let p: i64 = q.iter().zip(y[i..i + q.len()].iter().rev()).map(|(&c, &s)| i64::from(c) * i64::from(s)).sum();
            y.push((res + (p >> shift)) as i32);
        }
        assert_eq!(y, x);
    }

    #[test]
    fn noise_and_extremes_do_not_panic() {
        let mut seed = 1u32;
        let noise: Vec<i32> = (0..4096)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as i32 - (1 << 23)
            })
            .collect();
        let (_, bits) = choose(&noise, 24, Effort::level(8), &window(noise.len()));
        assert!(bits <= 8 + 4096 * 24);
        let square: Vec<i32> = (0..4096).map(|i| if i % 2 == 0 { (1 << 23) - 1 } else { -(1 << 23) }).collect();
        choose(&square, 24, Effort::level(8), &window(square.len()));
        choose(&[], 16, Effort::level(5), &window(0));
        choose(&[1], 16, Effort::level(5), &window(1));
        choose(&[1, 2], 16, Effort::level(8), &window(2));
    }
}
