//! Fused, multi-threaded CPU kernels for the encoder's element-wise work (candle's own element-wise
//! operations run on one thread, which made them as costly as the matrix products):
//! SiLU, GLU, the depthwise time convolution with bias and SiLU, and the attention scores with the
//! relative shift. All take contiguous `f32` tensors and fail cleanly otherwise.

use candle_core::{CpuStorage, CustomOp1, CustomOp2, Layout, Result, Shape, Tensor};
use rayon::prelude::*;

/// Elements per parallel task for flat element-wise work.
const CHUNK: usize = 1 << 14;

fn slice<'a>(s: &'a CpuStorage, l: &Layout) -> Result<&'a [f32]> {
    let CpuStorage::F32(v) = s else { candle_core::bail!("expected an f32 tensor") };
    let (a, b) = l.contiguous_offsets().ok_or_else(|| candle_core::Error::Msg("expected a contiguous tensor".into()))?;
    v.get(a..b).ok_or_else(|| candle_core::Error::Msg("tensor storage too short".into()))
}

fn dims2(l: &Layout) -> Result<(usize, usize)> {
    match l.shape().dims() {
        &[a, b] => Ok((a, b)),
        d => candle_core::bail!("expected a matrix, got shape {d:?}"),
    }
}

#[inline]
fn silu1(v: f32) -> f32 {
    v / (1.0 + (-v).exp())
}

#[inline]
fn sigmoid(v: f32) -> f32 {
    1.0 / (1.0 + (-v).exp())
}

struct Silu;

impl CustomOp1 for Silu {
    fn name(&self) -> &'static str {
        "parakeet-silu"
    }
    fn cpu_fwd(&self, s: &CpuStorage, l: &Layout) -> Result<(CpuStorage, Shape)> {
        let x = slice(s, l)?;
        let mut out = vec![0f32; x.len()];
        out.par_chunks_mut(CHUNK).zip(x.par_chunks(CHUNK)).for_each(|(o, x)| o.iter_mut().zip(x).for_each(|(o, &v)| *o = silu1(v)));
        Ok((CpuStorage::F32(out), l.shape().clone()))
    }
}

/// `x · sigmoid(x)`.
pub fn silu(x: &Tensor) -> Result<Tensor> {
    x.contiguous()?.apply_op1_no_bwd(&Silu)
}

struct Glu;

impl CustomOp1 for Glu {
    fn name(&self) -> &'static str {
        "parakeet-glu"
    }
    fn cpu_fwd(&self, s: &CpuStorage, l: &Layout) -> Result<(CpuStorage, Shape)> {
        let x = slice(s, l)?;
        let (n, d2) = dims2(l)?;
        if d2 % 2 != 0 {
            candle_core::bail!("GLU needs an even width");
        }
        let d = d2 / 2;
        let mut out = vec![0f32; n * d];
        out.par_chunks_mut(d.max(1)).zip(x.par_chunks(d2.max(1))).for_each(|(o, row)| {
            let (a, b) = row.split_at(d);
            for ((o, &a), &b) in o.iter_mut().zip(a).zip(b) {
                *o = a * sigmoid(b);
            }
        });
        Ok((CpuStorage::F32(out), Shape::from((n, d))))
    }
}

/// Gated linear unit over the last dimension: `(n, 2d)` → `(n, d)`, `a · sigmoid(b)`.
pub fn glu(x: &Tensor) -> Result<Tensor> {
    x.contiguous()?.apply_op1_no_bwd(&Glu)
}

/// Depthwise convolution over time (zero padded, centred), plus bias, then SiLU.
pub struct DepthwiseSilu {
    /// `taps × channels`, tap-major
    pub kernel: Vec<f32>,
    pub bias: Vec<f32>,
    pub taps: usize,
}

impl CustomOp1 for DepthwiseSilu {
    fn name(&self) -> &'static str {
        "parakeet-depthwise-silu"
    }
    fn cpu_fwd(&self, s: &CpuStorage, l: &Layout) -> Result<(CpuStorage, Shape)> {
        let x = slice(s, l)?;
        let (t, d) = dims2(l)?;
        if self.bias.len() != d || self.kernel.len() != self.taps * d || d == 0 {
            candle_core::bail!("depthwise convolution: kernel does not match {d} channels");
        }
        let pad = (self.taps - 1) / 2;
        let mut out = vec![0f32; t * d];
        out.par_chunks_mut(d).enumerate().for_each(|(i, o)| {
            o.copy_from_slice(&self.bias);
            for j in 0..self.taps {
                let Some(src) = (i + j).checked_sub(pad).filter(|&s| s < t) else { continue };
                let (row, k) = (&x[src * d..(src + 1) * d], &self.kernel[j * d..(j + 1) * d]);
                for ((o, &x), &k) in o.iter_mut().zip(row).zip(k) {
                    *o += x * k;
                }
            }
            o.iter_mut().for_each(|v| *v = silu1(*v));
        });
        Ok((CpuStorage::F32(out), l.shape().clone()))
    }
}

struct RelScores {
    scale: f32,
}

impl CustomOp2 for RelScores {
    fn name(&self) -> &'static str {
        "parakeet-rel-scores"
    }
    fn cpu_fwd(&self, s1: &CpuStorage, l1: &Layout, s2: &CpuStorage, l2: &Layout) -> Result<(CpuStorage, Shape)> {
        let (ac, bd) = (slice(s1, l1)?, slice(s2, l2)?);
        let (&[h, t, t2], &[h2, tb, p]) = (l1.shape().dims(), l2.shape().dims()) else { candle_core::bail!("attention scores: expected rank-3 tensors") };
        if t != t2 || h != h2 || t != tb || p + 1 != 2 * t {
            candle_core::bail!("attention scores: shapes do not match");
        }
        let mut out = vec![0f32; h * t * t];
        out.par_chunks_mut(t.max(1)).enumerate().for_each(|(r, o)| {
            let i = r % t;
            let a = &ac[r * t..(r + 1) * t];
            // position term of query i: bd row, shifted so column j holds relative position i − j
            let b = &bd[r * p + (t - 1 - i)..r * p + (t - 1 - i) + t];
            for ((o, &a), &b) in o.iter_mut().zip(a).zip(b) {
                *o = (a + b) * self.scale;
            }
        });
        Ok((CpuStorage::F32(out), Shape::from((h, t, t))))
    }
}

/// Transformer-XL attention scores: `(ac[h][i][j] + bd[h][i][t − 1 − i + j]) · scale` for the
/// content term `ac` `(h, t, t)` and the position term `bd` `(h, t, 2t − 1)`.
pub fn rel_scores(ac: &Tensor, bd: &Tensor, scale: f32) -> Result<Tensor> {
    ac.contiguous()?.apply_op2_no_bwd(&bd.contiguous()?, &RelScores { scale })
}

#[cfg(test)]
mod tests {
    use super::*;
    use candle_core::{D, Device};

    fn close(a: &Tensor, b: &Tensor) {
        let (a, b) = (a.flatten_all().unwrap().to_vec1::<f32>().unwrap(), b.flatten_all().unwrap().to_vec1::<f32>().unwrap());
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-5, "{x} vs {y}");
        }
    }

    fn rand(shape: &[usize]) -> Tensor {
        let n: usize = shape.iter().product();
        Tensor::from_vec((0..n).map(|i| (i * 7919 % 1000) as f32 / 250.0 - 2.0).collect::<Vec<_>>(), shape, &Device::Cpu).unwrap()
    }

    #[test]
    fn kernels_match_candle_reference() {
        let x = rand(&[5, 8]);
        close(&silu(&x).unwrap(), &x.silu().unwrap());
        let reference = (x.narrow(1, 0, 4).unwrap() * candle_nn::ops::sigmoid(&x.narrow(1, 4, 4).unwrap()).unwrap()).unwrap();
        close(&glu(&x).unwrap(), &reference);
        // depthwise: 3 taps over time
        let k = rand(&[3, 8]);
        let b = rand(&[8]);
        let op = DepthwiseSilu { kernel: k.flatten_all().unwrap().to_vec1().unwrap(), bias: b.to_vec1().unwrap(), taps: 3 };
        let xp = x.pad_with_zeros(0, 1, 1).unwrap();
        let mut y = b.unsqueeze(0).unwrap().broadcast_as((5, 8)).unwrap().contiguous().unwrap();
        for j in 0..3 {
            y = (y + xp.narrow(0, j, 5).unwrap().broadcast_mul(&k.get(j).unwrap()).unwrap()).unwrap();
        }
        close(&x.apply_op1_no_bwd(&op).unwrap(), &y.silu().unwrap());
        // relative scores against the pad/reshape formulation
        let (h, t) = (2, 4);
        let ac = rand(&[h, t, t]);
        let bd = rand(&[h, t, 2 * t - 1]);
        let shifted = bd
            .pad_with_zeros(D::Minus1, 1, 0)
            .unwrap()
            .reshape((h, 2 * t, t))
            .unwrap()
            .narrow(1, 1, 2 * t - 1)
            .unwrap()
            .contiguous()
            .unwrap()
            .reshape((h, t, 2 * t - 1))
            .unwrap()
            .narrow(2, 0, t)
            .unwrap();
        close(&rel_scores(&ac, &bd, 0.5).unwrap(), &((ac + shifted).unwrap() * 0.5).unwrap());
        // malformed input is an error, not a panic
        assert!(glu(&rand(&[2, 3])).is_err());
        assert!(rel_scores(&rand(&[2, 4, 4]), &rand(&[2, 4, 6]), 1.0).is_err());
        assert!(rand(&[2, 7]).apply_op1_no_bwd(&op).is_err());
    }
}
