//! CPU kernels for transformer inference (feature `whisper`).
//!
//! Large matrix products go through [faer](https://github.com/sarah-quinones/faer-rs) (pure Rust,
//! runtime SIMD dispatch, MIT) with explicit parallelism: a big product is split over the current
//! rayon pool, while attention runs every (head, block of query rows) as its own sequential task.
//! Row kernels (layer norm, softmax, erf-GELU, bias) and the decoder's thin products (a handful of
//! rows against a large weight matrix, which is bound by memory bandwidth) are plain safe Rust
//! written with fixed-width lane accumulators, so the compiler vectorises them, and run on rayon.
//!
//! Every matrix is row-major `f32` with an explicit row stride ("leading dimension"). Shapes are
//! checked before faer sees them; a mismatch is an error, never a panic.

use faer::linalg::matmul::matmul;
use faer::{Accum, MatMut, MatRef, Par};
use rayon::prelude::*;

use crate::SpeechError;

type Result<T> = std::result::Result<T, SpeechError>;

/// Elements below which row kernels run on the calling thread.
const PAR_MIN: usize = 1 << 14;
/// Query rows per attention task.
const ATT_BLOCK: usize = 128;
/// Lanes of the vectorised reductions.
const L: usize = 16;

fn shape_err(what: &str) -> SpeechError {
    SpeechError::Model(format!("internal shape mismatch ({what})"))
}

/// The parallelism for a large product: every thread of the current rayon pool.
pub fn par() -> Par {
    Par::rayon(0)
}

fn fits(len: usize, rows: usize, cols: usize, ld: usize) -> bool {
    rows == 0 || cols == 0 || (ld >= cols && (rows - 1).checked_mul(ld).and_then(|x| x.checked_add(cols)).is_some_and(|need| need <= len))
}

/// A checked row-major view of `rows × cols` with row stride `ld`.
pub fn mat(s: &[f32], rows: usize, cols: usize, ld: usize) -> Result<MatRef<'_, f32>> {
    if !fits(s.len(), rows, cols, ld) {
        return Err(shape_err("view"));
    }
    Ok(MatRef::from_row_major_slice_with_stride(s, rows, cols, ld.max(cols)))
}

/// A checked mutable row-major view.
///
/// Built as the transpose of a column-major view: faer 0.23.2's
/// `MatMut::from_row_major_slice_with_stride_mut` swaps the two strides (its bounds check is for
/// the row-major layout, the view it returns is column-major), so it reads and writes outside the
/// slice. `strided_views_stay_in_bounds` below guards this.
pub fn mat_mut(s: &mut [f32], rows: usize, cols: usize, ld: usize) -> Result<MatMut<'_, f32>> {
    if !fits(s.len(), rows, cols, ld) {
        return Err(shape_err("view"));
    }
    Ok(MatMut::from_column_major_slice_with_stride_mut(s, cols, rows, ld.max(cols)).transpose_mut())
}

/// `c = alpha · a · bᵀ` (or `c += …` with `acc`), `a`: m × k, `b`: n × k (the layout of a
/// PyTorch `Linear` weight), `c`: m × n.
#[allow(clippy::too_many_arguments)]
pub fn gemm_nt(
    c: &mut [f32],
    ldc: usize,
    a: &[f32],
    lda: usize,
    b: &[f32],
    ldb: usize,
    m: usize,
    n: usize,
    k: usize,
    alpha: f32,
    acc: bool,
    par: Par,
) -> Result<()> {
    if m == 0 || n == 0 {
        return Ok(());
    }
    let (a, b, c) = (mat(a, m, k, lda)?, mat(b, n, k, ldb)?, mat_mut(c, m, n, ldc)?);
    matmul(c, if acc { Accum::Add } else { Accum::Replace }, a, b.transpose(), alpha, par);
    Ok(())
}

/// `y = x · wᵀ + bias` for a whole activation matrix (`x`: m × k contiguous, `w`: n × k), into
/// `y` (resized to m × n). Thin products (few rows) use [`linear_small`].
pub fn linear(y: &mut Vec<f32>, x: &[f32], m: usize, w: &Linear) -> Result<()> {
    y.resize(m * w.n, 0.0);
    if m <= SMALL_ROWS {
        return linear_small(y, x, m, w, false);
    }
    let mut buf = Vec::new();
    gemm_nt(y, w.n, x, w.k, w.w.f32(&mut buf), w.k, m, w.n, w.k, 1.0, false, par())?;
    add_bias(y, &w.b);
    Ok(())
}

/// `y = gelu(x · wᵀ + bias)`.
pub fn linear_gelu(y: &mut Vec<f32>, x: &[f32], m: usize, w: &Linear) -> Result<()> {
    y.resize(m * w.n, 0.0);
    if m <= SMALL_ROWS {
        linear_small(y, x, m, w, false)?;
        gelu_all(y);
        return Ok(());
    }
    let mut buf = Vec::new();
    gemm_nt(y, w.n, x, w.k, w.w.f32(&mut buf), w.k, m, w.n, w.k, 1.0, false, par())?;
    bias_gelu(y, &w.b);
    Ok(())
}

/// `y += x · wᵀ + bias` (residual connection), `y`: m × n.
pub fn linear_add(y: &mut [f32], x: &[f32], m: usize, w: &Linear) -> Result<()> {
    if m <= SMALL_ROWS {
        return linear_small(y, x, m, w, true);
    }
    let mut buf = Vec::new();
    gemm_nt(y, w.n, x, w.k, w.w.f32(&mut buf), w.k, m, w.n, w.k, 1.0, true, par())?;
    add_bias(y, &w.b);
    Ok(())
}

/// Rows up to which [`linear_small`] beats a blocked product.
pub const SMALL_ROWS: usize = 16;

/// Weight storage: `f32`, or the IEEE half-precision bit patterns a model was published with.
/// Decoding reads every decoder weight once per step, so keeping half-precision weights halves
/// that memory traffic; they are converted to `f32` exactly as they are used ([`h2f`]).
#[derive(Clone, Debug)]
pub enum Store {
    F32(Vec<f32>),
    F16(Vec<u16>),
}

impl Default for Store {
    fn default() -> Self {
        Store::F32(Vec::new())
    }
}

impl Store {
    pub fn len(&self) -> usize {
        match self {
            Store::F32(v) => v.len(),
            Store::F16(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// All weights as `f32`: borrowed, or converted into `buf`.
    pub fn f32<'a>(&'a self, buf: &'a mut Vec<f32>) -> &'a [f32] {
        match self {
            Store::F32(v) => v,
            Store::F16(h) => {
                buf.clear();
                buf.resize(h.len(), 0.0);
                buf.par_chunks_mut(1 << 16).zip(h.par_chunks(1 << 16)).for_each(|(o, i)| o.iter_mut().zip(i).for_each(|(o, &i)| *o = h2f(i)));
                buf
            }
        }
    }

    /// Elements `range` as `f32` (borrowed, or converted into `buf`); `None` when out of range.
    pub fn slice<'a>(&'a self, range: std::ops::Range<usize>, buf: &'a mut Vec<f32>) -> Option<&'a [f32]> {
        match self {
            Store::F32(v) => v.get(range),
            Store::F16(h) => {
                let h = h.get(range)?;
                buf.clear();
                buf.extend(h.iter().map(|&x| h2f(x)));
                Some(buf)
            }
        }
    }
}

/// Half precision → `f32`, branch-free (so loops over it vectorise) and exact for every finite
/// value, subnormals included: the exponent and mantissa are moved into place and re-biased by a
/// multiplication with 2¹¹². (Infinities and NaNs, which no weight file should contain, come out
/// as large finite values.)
#[inline(always)]
pub fn h2f(h: u16) -> f32 {
    let h = h as u32;
    let v = f32::from_bits((h & 0x7fff) << 13) * f32::from_bits(0x7780_0000);
    f32::from_bits(v.to_bits() | ((h & 0x8000) << 16))
}

/// A linear layer: weight `n × k` (row-major, PyTorch layout) and bias `n` (zeros if absent).
#[derive(Clone, Debug, Default)]
pub struct Linear {
    pub w: Store,
    pub b: Vec<f32>,
    pub n: usize,
    pub k: usize,
}

impl Linear {
    pub fn new(w: Vec<f32>, b: Option<Vec<f32>>, n: usize, k: usize) -> Result<Self> {
        Self::with_store(Store::F32(w), b, n, k)
    }

    pub fn with_store(w: Store, b: Option<Vec<f32>>, n: usize, k: usize) -> Result<Self> {
        let b = b.unwrap_or_else(|| vec![0.0; n]);
        if w.len() != n.checked_mul(k).ok_or_else(|| shape_err("linear"))? || b.len() != n {
            return Err(shape_err("linear weight"));
        }
        Ok(Self { w, b, n, k })
    }

    /// Stack layers with the same input width into one (`[a; b; …]`): one product instead of several.
    pub fn stack(parts: Vec<Linear>) -> Result<Self> {
        let k = parts.first().map(|p| p.k).unwrap_or(0);
        if parts.iter().any(|p| p.k != k) {
            return Err(shape_err("stacked linear"));
        }
        let n = parts.iter().map(|p| p.n).sum();
        let mut b = Vec::with_capacity(n);
        let w = if parts.iter().all(|p| matches!(p.w, Store::F16(_))) {
            let mut w = Vec::with_capacity(n * k);
            for p in parts {
                if let Store::F16(h) = p.w {
                    w.extend(h);
                }
                b.extend(p.b);
            }
            Store::F16(w)
        } else {
            let mut w = Vec::with_capacity(n * k);
            for p in parts {
                let mut buf = Vec::new();
                w.extend_from_slice(p.w.f32(&mut buf));
                b.extend(p.b);
            }
            Store::F32(w)
        };
        Ok(Self { w, b, n, k })
    }
}

/// `y (m × n) = x (m × k) · wᵀ + b` (or `y +=` with `acc`) for a few rows: every weight row is
/// streamed from memory once and dotted with all `m` input rows (decoding is bound by memory
/// bandwidth, and a blocked product would leave most threads idle).
pub fn linear_small(y: &mut [f32], x: &[f32], m: usize, w: &Linear, acc: bool) -> Result<()> {
    let (n, k) = (w.n, w.k);
    if y.len() < m * n || x.len() < m * k || w.w.len() < n * k || w.b.len() < n {
        return Err(shape_err("linear_small"));
    }
    if m == 0 || n == 0 {
        return Ok(());
    }
    let threads = rayon::current_num_threads().max(1);
    let block = (n / (threads * 4)).clamp(8, 512);
    // every task covers a block of weight rows (= output columns) for all m input rows and fills
    // a local m × block buffer (writing into y directly would make neighbouring tasks share cache
    // lines); the blocks are copied into y afterwards
    let blocks: Vec<Vec<f32>> = (0..n.div_ceil(block))
        .into_par_iter()
        .map(|bi| {
            let r0 = bi * block;
            let cols = block.min(n - r0);
            let mut out = vec![0f32; m * cols];
            let mut row = vec![0f32; if matches!(w.w, Store::F16(_)) { k } else { 0 }];
            for j in 0..cols {
                let r = r0 + j;
                let wr: &[f32] = match &w.w {
                    Store::F32(f) => &f[r * k..(r + 1) * k],
                    Store::F16(h) => {
                        row.iter_mut().zip(&h[r * k..(r + 1) * k]).for_each(|(o, &x)| *o = h2f(x));
                        &row
                    }
                };
                let b = w.b[r];
                for i in 0..m {
                    out[i * cols + j] = dot(&x[i * k..(i + 1) * k], wr) + b;
                }
            }
            out
        })
        .collect();
    let write = |(i, yr): (usize, &mut [f32])| {
        let mut r0 = 0;
        for blk in &blocks {
            let cols = blk.len() / m;
            let (dst, src) = (&mut yr[r0..r0 + cols], &blk[i * cols..(i + 1) * cols]);
            if acc {
                dst.iter_mut().zip(src).for_each(|(d, s)| *d += s);
            } else {
                dst.copy_from_slice(src);
            }
            r0 += cols;
        }
    };
    if m * n >= PAR_MIN {
        y[..m * n].par_chunks_mut(n).enumerate().for_each(write);
    } else {
        y[..m * n].chunks_mut(n).enumerate().for_each(write);
    }
    Ok(())
}

/// Dot product with lane accumulators (vectorises without reassociating a single sum).
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let (ac, at) = a[..n].as_chunks::<L>();
    let (bc, bt) = b[..n].as_chunks::<L>();
    let mut acc = [0f32; L];
    for (x, y) in ac.iter().zip(bc) {
        for l in 0..L {
            acc[l] += x[l] * y[l];
        }
    }
    let tail: f32 = at.iter().zip(bt).map(|(x, y)| x * y).sum();
    lanes_sum(&acc) + tail
}

#[inline]
fn lanes_sum(acc: &[f32; L]) -> f32 {
    let mut s = *acc;
    let mut w = L / 2;
    while w > 0 {
        for l in 0..w {
            s[l] += s[l + w];
        }
        w /= 2;
    }
    s[0]
}

fn sum(x: &[f32]) -> f32 {
    let (c, t) = x.as_chunks::<L>();
    let mut acc = [0f32; L];
    for v in c {
        for l in 0..L {
            acc[l] += v[l];
        }
    }
    lanes_sum(&acc) + t.iter().sum::<f32>()
}

fn max(x: &[f32]) -> f32 {
    let (c, t) = x.as_chunks::<L>();
    let mut acc = [f32::NEG_INFINITY; L];
    for v in c {
        for l in 0..L {
            acc[l] = if v[l] > acc[l] { v[l] } else { acc[l] };
        }
    }
    acc.iter().chain(t).copied().fold(f32::NEG_INFINITY, f32::max)
}

/// `e^x` with Cody–Waite range reduction to [−ln2/2, ln2/2] and a degree-6 polynomial (about one
/// ulp); 2ⁿ is built from the exponent bits. Branch-free, so loops over it vectorise. Underflows
/// (and −∞) give 0; NaN stays NaN.
#[inline(always)]
pub fn exp(x: f32) -> f32 {
    let xc = x.clamp(-87.3, 88.0);
    let n = (xc * std::f32::consts::LOG2_E + 12_582_912.0) - 12_582_912.0; // round to nearest
    let r = xc - n * 0.693_359_4 + n * 2.121_944_4e-4;
    let p = 1.987_569_1e-4f32;
    let p = p * r + 1.398_199_9e-3;
    let p = p * r + 8.333_452e-3;
    let p = p * r + 4.166_579_6e-2;
    let p = p * r + 1.666_666_5e-1;
    let p = p * r + 0.5;
    let y = p * r * r + r + 1.0;
    let e = f32::from_bits(((n as i32 + 127) as u32) << 23);
    if x < -87.3 { 0.0 } else { y * e }
}

/// The error function (Abramowitz & Stegun 7.1.26, |error| ≤ 1.5·10⁻⁷).
#[inline(always)]
pub fn erf(x: f32) -> f32 {
    let a = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * a);
    let poly = ((((1.061_405_4 * t - 1.453_152_1) * t + 1.421_413_8) * t - 0.284_496_74) * t + 0.254_829_6) * t;
    (1.0 - poly * exp(-a * a)).copysign(x)
}

/// GELU with the exact (erf) form Whisper was trained with.
#[inline(always)]
pub fn gelu(x: f32) -> f32 {
    0.5 * x * (1.0 + erf(x * std::f32::consts::FRAC_1_SQRT_2))
}

fn rows_mut(x: &mut [f32], cols: usize, f: impl Fn(&mut [f32]) + Sync + Send) {
    if cols == 0 {
        return;
    }
    if x.len() >= PAR_MIN {
        x.par_chunks_mut(cols).with_min_len((PAR_MIN / 4 / cols).max(1)).for_each(f);
    } else {
        x.chunks_mut(cols).for_each(f);
    }
}

/// `x[r] += bias` for every row.
pub fn add_bias(x: &mut [f32], bias: &[f32]) {
    rows_mut(x, bias.len(), |r| r.iter_mut().zip(bias).for_each(|(v, b)| *v += b));
}

/// `x[r] = gelu(x[r] + bias)` for every row.
pub fn bias_gelu(x: &mut [f32], bias: &[f32]) {
    rows_mut(x, bias.len(), |r| r.iter_mut().zip(bias).for_each(|(v, b)| *v = gelu(*v + b)));
}

/// `x = gelu(x)` elementwise.
pub fn gelu_all(x: &mut [f32]) {
    if x.len() >= PAR_MIN {
        x.par_chunks_mut(PAR_MIN / 4).for_each(|c| c.iter_mut().for_each(|v| *v = gelu(*v)));
    } else {
        x.iter_mut().for_each(|v| *v = gelu(*v));
    }
}

/// `x += y` elementwise.
pub fn add(x: &mut [f32], y: &[f32]) {
    if x.len() >= PAR_MIN {
        x.par_chunks_mut(PAR_MIN).zip(y.par_chunks(PAR_MIN)).for_each(|(a, b)| a.iter_mut().zip(b).for_each(|(a, b)| *a += b));
    } else {
        x.iter_mut().zip(y).for_each(|(a, b)| *a += b);
    }
}

/// Softmax of one row in place (an all −∞ row becomes all zeros).
pub fn softmax(r: &mut [f32]) {
    let m = max(r);
    if m == f32::NEG_INFINITY {
        r.fill(0.0);
        return;
    }
    r.iter_mut().for_each(|v| *v = exp(*v - m));
    let s = sum(r);
    let inv = if s > 0.0 { 1.0 / s } else { 0.0 };
    r.iter_mut().for_each(|v| *v *= inv);
}

/// Softmax of every `cols`-wide row.
pub fn softmax_rows(x: &mut [f32], cols: usize) {
    rows_mut(x, cols, softmax);
}

/// `log softmax` of a row.
pub fn log_softmax(x: &[f32]) -> Vec<f32> {
    let m = max(x);
    if !m.is_finite() {
        return vec![f32::NEG_INFINITY; x.len()];
    }
    let s: f32 = {
        let e: Vec<f32> = x.iter().map(|v| exp(*v - m)).collect();
        sum(&e)
    };
    let lse = m + s.ln();
    x.iter().map(|v| v - lse).collect()
}

/// A layer norm's parameters.
#[derive(Clone, Debug, Default)]
pub struct Norm {
    pub w: Vec<f32>,
    pub b: Vec<f32>,
}

/// Layer norm (ε = 1e-5) of every row of `x` into `y` (resized to `x`'s size).
pub fn layer_norm(y: &mut Vec<f32>, x: &[f32], p: &Norm) {
    let d = p.w.len();
    y.resize(x.len(), 0.0);
    if d == 0 || p.b.len() != d {
        return;
    }
    let f = |(yr, xr): (&mut [f32], &[f32])| {
        let mean = sum(xr) / d as f32;
        let (c, t) = xr.as_chunks::<L>();
        let mut acc = [0f32; L];
        for v in c {
            for l in 0..L {
                let z = v[l] - mean;
                acc[l] += z * z;
            }
        }
        let var = (lanes_sum(&acc) + t.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>()) / d as f32;
        let inv = 1.0 / (var + 1e-5).sqrt();
        for (((o, v), w), b) in yr.iter_mut().zip(xr).zip(&p.w).zip(&p.b) {
            *o = (v - mean) * inv * w + b;
        }
    };
    if x.len() >= PAR_MIN {
        y.par_chunks_mut(d).zip(x.par_chunks(d)).with_min_len((PAR_MIN / 4 / d).max(1)).for_each(f);
    } else {
        y.chunks_mut(d).zip(x.chunks(d)).for_each(f);
    }
}

/// Attention with one query per row (decoding). Row `r` attends over the first `len` positions of
/// `kv[r]` (`ld` floats per position, keys at `k_off`, values at `v_off`, head `h` at `h·dh`):
/// `out[r, h] = softmax(scale · q[r, h] · K[r, h]ᵀ) · V[r, h]`. Every (row, head) is a task.
#[allow(clippy::too_many_arguments)]
pub fn attend_one(
    out: &mut [f32],
    q: &[f32],
    ldq: usize,
    kv: &[&[f32]],
    ld: usize,
    k_off: usize,
    v_off: usize,
    len: usize,
    heads: usize,
    dh: usize,
    scale: f32,
) -> Result<()> {
    let d = heads * dh;
    let rows = kv.len();
    if dh == 0 || rows == 0 {
        return Ok(());
    }
    let ok = out.len() >= rows * d
        && fits(q.len(), rows, d, ldq)
        && k_off + d <= ld
        && v_off + d <= ld
        && kv.iter().all(|s| len.checked_mul(ld).is_some_and(|n| n <= s.len()));
    if !ok {
        return Err(shape_err("attend_one"));
    }
    out[..rows * d].par_chunks_mut(dh).enumerate().for_each(|(i, o)| {
        let (r, h) = (i / heads, i % heads);
        let qh = &q[r * ldq + h * dh..r * ldq + (h + 1) * dh];
        let src = kv[r];
        let mut s: Vec<f32> = (0..len).map(|j| dot(qh, &src[j * ld + k_off + h * dh..j * ld + k_off + (h + 1) * dh]) * scale).collect();
        softmax(&mut s);
        o.fill(0.0);
        for (j, p) in s.iter().enumerate() {
            let v = &src[j * ld + v_off + h * dh..j * ld + v_off + (h + 1) * dh];
            o.iter_mut().zip(v).for_each(|(a, b)| *a += p * b);
        }
    });
    Ok(())
}

/// Multi-head attention for `tq` queries over `tk` keys (`q`: tq × ldq, `k`/`v`: tk × ldk/ldv,
/// head `h` in columns `h·dh..`), into `out` (tq × ldo). `causal`: query `i` sees keys `0..=i`.
/// Every (head, block of query rows) is a sequential task; `scratch` holds their score blocks.
#[allow(clippy::too_many_arguments)]
pub fn attend_many(
    out: &mut [f32],
    ldo: usize,
    q: &[f32],
    ldq: usize,
    k: &[f32],
    ldk: usize,
    v: &[f32],
    ldv: usize,
    tq: usize,
    tk: usize,
    heads: usize,
    dh: usize,
    scale: f32,
    causal: bool,
    scratch: &mut Vec<f32>,
) -> Result<()> {
    let d = heads * dh;
    if tq == 0 || tk == 0 || d == 0 {
        return Ok(());
    }
    if !(fits(q.len(), tq, d, ldq) && fits(k.len(), tk, d, ldk) && fits(v.len(), tk, d, ldv)) {
        return Err(shape_err("attend_many"));
    }
    let nb = tq.div_ceil(ATT_BLOCK);
    let block = ATT_BLOCK.min(tq);
    scratch.resize(heads * nb * block * tk, 0.0);
    // disjoint output views: (block, head)
    let mut views = Vec::with_capacity(nb * heads);
    let mut rest = mat_mut(out, tq, d, ldo)?;
    for b in 0..nb {
        let rows = block.min(tq - b * block);
        let (top, bottom) = rest.split_at_row_mut(rows);
        rest = bottom;
        let mut row = top;
        for h in 0..heads {
            let (head, right) = row.split_at_col_mut(dh);
            views.push((b * block, h, head));
            row = right;
        }
    }
    views.into_par_iter().zip(scratch.par_chunks_mut(block * tk)).try_for_each(|((i0, h, o), s)| -> Result<()> {
        let rows = o.nrows();
        let qv = mat(q.get(i0 * ldq + h * dh..).unwrap_or_default(), rows, dh, ldq)?;
        let kv = mat(k.get(h * dh..).unwrap_or_default(), tk, dh, ldk)?;
        let vv = mat(v.get(h * dh..).unwrap_or_default(), tk, dh, ldv)?;
        let s = &mut s[..rows * tk];
        matmul(mat_mut(s, rows, tk, tk)?, Accum::Replace, qv, kv.transpose(), scale, Par::Seq);
        for (i, r) in s.chunks_mut(tk).enumerate() {
            if causal {
                r.iter_mut().skip(i0 + i + 1).for_each(|x| *x = f32::NEG_INFINITY);
            }
            softmax(r);
        }
        matmul(o, Accum::Replace, mat(s, rows, tk, tk)?, vv, 1.0, Par::Seq);
        Ok(())
    })
}

/// The scaled attention logits `scale · Q_h · K_hᵀ` (tq × tk) of one head (word alignment).
#[allow(clippy::too_many_arguments)]
pub fn head_logits(q: &[f32], ldq: usize, k: &[f32], ldk: usize, tq: usize, tk: usize, h: usize, dh: usize, scale: f32) -> Result<Vec<f32>> {
    let mut s = vec![0f32; tq * tk];
    let qv = mat(q.get(h * dh..).unwrap_or_default(), tq, dh, ldq)?;
    let kv = mat(k.get(h * dh..).unwrap_or_default(), tk, dh, ldk)?;
    matmul(mat_mut(&mut s, tq, tk, tk)?, Accum::Replace, qv, kv.transpose(), scale, par());
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_nt(a: &[f32], b: &[f32], m: usize, n: usize, k: usize) -> Vec<f32> {
        let mut c = vec![0f32; m * n];
        for i in 0..m {
            for j in 0..n {
                c[i * n + j] = (0..k).map(|p| a[i * k + p] as f64 * b[j * k + p] as f64).sum::<f64>() as f32;
            }
        }
        c
    }

    fn data(n: usize, seed: u32) -> Vec<f32> {
        (0..n).map(|i| (((i as u32).wrapping_mul(2_654_435_761).wrapping_add(seed) >> 8) % 2001) as f32 / 1000.0 - 1.0).collect()
    }

    #[test]
    fn exp_and_erf_are_accurate() {
        for i in -8_700..=8_800 {
            let x = i as f32 * 0.01;
            let (got, want) = (exp(x), (x as f64).exp());
            assert!(((got as f64 - want) / want).abs() < 3e-7, "exp({x}) = {got}, want {want}");
        }
        assert_eq!(exp(f32::NEG_INFINITY), 0.0);
        assert_eq!(exp(-100.0), 0.0);
        assert!(exp(f32::NAN).is_nan());
        // erf against its series / known values
        for (x, want) in
            [(0.0f32, 0.0f64), (0.5, 0.520_499_877_813_046_5), (1.0, 0.842_700_792_949_714_9), (2.0, 0.995_322_265_018_952_7), (-1.5, -0.966_105_146_475_310_7)]
        {
            assert!((erf(x) as f64 - want).abs() < 3e-7, "erf({x})");
        }
        assert!((gelu(1.0) - 0.841_344_7).abs() < 1e-6);
        assert!(gelu(-10.0).abs() < 1e-6 && gelu(10.0) == 10.0);
    }

    #[test]
    fn products_match_a_naive_reference() {
        let (m, n, k) = (37, 29, 70);
        let (a, b) = (data(m * k, 1), data(n * k, 2));
        let want = naive_nt(&a, &b, m, n, k);
        let mut c = vec![0f32; m * n];
        gemm_nt(&mut c, n, &a, k, &b, k, m, n, k, 1.0, false, Par::Seq).unwrap();
        assert!(c.iter().zip(&want).all(|(x, y)| (x - y).abs() < 1e-4));
        // thin products, with bias and accumulation
        let lin = Linear::new(b.clone(), Some(data(n, 3)), n, k).unwrap();
        for rows in [1, 3, 16] {
            let mut y = vec![1f32; rows * n];
            linear_small(&mut y, &a, rows, &lin, true).unwrap();
            for i in 0..rows {
                for j in 0..n {
                    let w = want[i * n + j] + lin.b[j] + 1.0;
                    assert!((y[i * n + j] - w).abs() < 1e-4, "{rows} rows ({i}, {j})");
                }
            }
        }
        let mut y = Vec::new();
        linear(&mut y, &a, m, &lin).unwrap();
        assert!((y[5 * n + 7] - want[5 * n + 7] - lin.b[7]).abs() < 1e-4);
        // shape errors, not panics
        assert!(gemm_nt(&mut c, n, &a, k, &b, k, m + 1, n, k, 1.0, false, Par::Seq).is_err());
        assert!(linear_small(&mut c, &a, m + 1, &lin, false).is_err());
    }

    #[test]
    fn attention_matches_a_naive_reference() {
        let (heads, dh, tq, tk) = (3, 8, 5, 140);
        let d = heads * dh;
        let (q, k, v) = (data(tq * d, 4), data(tk * d, 5), data(tk * d, 6));
        let scale = 0.3;
        let naive = |causal: bool| {
            let mut o = vec![0f32; tq * d];
            for h in 0..heads {
                for i in 0..tq {
                    let mut s: Vec<f64> =
                        (0..tk).map(|j| (0..dh).map(|p| (q[i * d + h * dh + p] * k[j * d + h * dh + p]) as f64).sum::<f64>() * scale as f64).collect();
                    if causal {
                        s.iter_mut().skip(i + 1).for_each(|x| *x = f64::NEG_INFINITY);
                    }
                    let m = s.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                    let e: Vec<f64> = s.iter().map(|x| (x - m).exp()).collect();
                    let z: f64 = e.iter().sum();
                    for p in 0..dh {
                        o[i * d + h * dh + p] = (0..tk).map(|j| e[j] / z * v[j * d + h * dh + p] as f64).sum::<f64>() as f32;
                    }
                }
            }
            o
        };
        for causal in [false, true] {
            let mut out = vec![0f32; tq * d];
            attend_many(&mut out, d, &q, d, &k, d, &v, d, tq, tk, heads, dh, scale, causal, &mut Vec::new()).unwrap();
            let want = naive(causal);
            assert!(out.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-5), "causal {causal}");
        }
        // one query per row over interleaved [k | v] positions
        let kvbuf: Vec<f32> = (0..tk).flat_map(|j| k[j * d..(j + 1) * d].iter().chain(&v[j * d..(j + 1) * d]).copied().collect::<Vec<_>>()).collect();
        let mut out = vec![0f32; d];
        attend_one(&mut out, &q[2 * d..3 * d], d, &[&kvbuf], 2 * d, 0, d, tk, heads, dh, scale).unwrap();
        let want = naive(false);
        assert!(out.iter().zip(&want[2 * d..3 * d]).all(|(a, b)| (a - b).abs() < 1e-5));
        assert!(attend_one(&mut out, &q, d, &[&kvbuf[..10]], 2 * d, 0, d, tk, heads, dh, scale).is_err());
    }

    #[test]
    fn half_precision_conversion_is_exact_for_finite_values() {
        for h in 0..=u16::MAX {
            let want = crate::safetensors::f16_to_f32(h);
            if want.is_finite() {
                assert_eq!(h2f(h).to_bits(), want.to_bits(), "{h:#06x}");
            } else {
                assert!(!h2f(h).is_nan());
            }
        }
        // a half-precision layer gives exactly the f32 layer's results
        let (n, k) = (9, 37);
        let h: Vec<u16> = (0..n * k).map(|i| (i as u16).wrapping_mul(2_654) & 0xbbff).collect();
        let f: Vec<f32> = h.iter().map(|&x| h2f(x)).collect();
        let (l16, l32) = (Linear::with_store(Store::F16(h), None, n, k).unwrap(), Linear::new(f, None, n, k).unwrap());
        let x = data(3 * k, 9);
        let (mut a, mut b) = (vec![0f32; 3 * n], vec![0f32; 3 * n]);
        linear_small(&mut a, &x, 3, &l16, false).unwrap();
        linear_small(&mut b, &x, 3, &l32, false).unwrap();
        assert_eq!(a, b);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        linear(&mut a, &data(40 * k, 3), 40, &l16).unwrap();
        linear(&mut b, &data(40 * k, 3), 40, &l32).unwrap();
        assert_eq!(a, b);
        let st = Linear::stack(vec![l16.clone(), l16]).unwrap();
        assert!(matches!(st.w, Store::F16(_)) && st.n == 2 * n);
    }

    #[test]
    fn strided_views_stay_in_bounds() {
        // a 3 × 2 product written into the middle columns of a 3 × 5 buffer (row stride 5)
        let a = [1.0f32, 2.0, 3.0, 4.0, 5.0, 6.0];
        let b = [1.0f32, 0.0, 0.0, 1.0];
        let mut c = vec![9f32; 15];
        gemm_nt(&mut c[1..], 5, &a, 2, &b, 2, 3, 2, 2, 1.0, false, Par::Seq).unwrap();
        assert_eq!(c, vec![9.0, 1.0, 2.0, 9.0, 9.0, 9.0, 3.0, 4.0, 9.0, 9.0, 9.0, 5.0, 6.0, 9.0, 9.0]);
        let mut v = vec![0f32; 7];
        let mut m = mat_mut(&mut v, 2, 2, 5).unwrap();
        m[(1, 1)] = 1.0;
        assert_eq!(v[6], 1.0);
        assert!(mat_mut(&mut v, 2, 3, 5).is_err());
    }

    #[test]
    fn row_kernels() {
        let mut r = vec![1.0, 2.0, 3.0, f32::NEG_INFINITY];
        softmax(&mut r);
        assert!((r.iter().sum::<f32>() - 1.0).abs() < 1e-6 && r[3] == 0.0 && r[2] > r[1]);
        let mut z = vec![f32::NEG_INFINITY; 3];
        softmax(&mut z);
        assert_eq!(z, vec![0.0; 3]);
        let p = Norm { w: vec![1.0; 4], b: vec![0.5; 4] };
        let mut y = Vec::new();
        layer_norm(&mut y, &[1.0, 2.0, 3.0, 4.0, 2.0, 2.0, 2.0, 2.0], &p);
        assert!((y[0] - (0.5 - 1.5 / 1.25f32.sqrt())).abs() < 1e-4 && (y[4] - 0.5).abs() < 1e-6);
        let ls = log_softmax(&[0.0, 0.0]);
        assert!((ls[0] - 0.5f32.ln()).abs() < 1e-6);
    }
}
