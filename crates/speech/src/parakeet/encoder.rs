//! The FastConformer encoder (Rekesh et al. 2023, "Fast Conformer with Linearly Scalable Attention
//! for Efficient Speech Recognition"; Gulati et al. 2020, "Conformer"), written against candle from
//! the papers and NeMo's Apache-2.0 module descriptions, reading NeMo's state-dict names.
//!
//! - **Subsampling** (`dw_striding`, 8×): a 3×3 stride-2 convolution (1 → C channels) and ReLU, then
//!   twice a depthwise 3×3 stride-2 convolution, a pointwise 1×1 convolution and ReLU, all over
//!   (time, mel); the (C × mel/8) features of each 80 ms frame are projected to `d_model`.
//! - **Blocks** (×24): macaron feed-forward (½ residual, SiLU), relative-position multi-head
//!   self-attention (Transformer-XL: learned biases `u`, `v`, sinusoidal relative positions with a
//!   per-layer projection, the "relative shift"), the convolution module (pointwise to 2·d, GLU,
//!   depthwise k = 9, batch norm, SiLU, pointwise), a second ½ feed-forward and a final layer norm.
//!   Every sub-block is pre-norm with a residual connection.
//!
//! Inference only: dropout is the identity and batch norm uses its running statistics, folded into
//! the depthwise convolution at load time. One clip (batch 1) without padding, full attention.

use candle_core::{DType, Device, Module, Result, Tensor};
use candle_nn::LayerNorm;

use super::ops;

/// A linear layer: weight `(out, in)` as stored, optional bias.
pub struct Linear {
    w: Tensor,
    b: Option<Tensor>,
}

impl Linear {
    pub fn new(w: Tensor, b: Option<Tensor>) -> Result<Self> {
        Ok(Self { w, b })
    }

    /// `x`: `(n, in)` → `(n, out)`.
    pub fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let y = x.matmul(&self.w.t()?)?;
        match &self.b {
            Some(b) => y.broadcast_add(b),
            None => Ok(y),
        }
    }
}

pub struct FeedForward {
    pub norm: LayerNorm,
    pub l1: Linear,
    pub l2: Linear,
}

impl FeedForward {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        self.l2.forward(&ops::silu(&self.l1.forward(&self.norm.forward(x)?)?)?)
    }
}

pub struct Attention {
    pub norm: LayerNorm,
    pub q: Linear,
    pub k: Linear,
    pub v: Linear,
    pub out: Linear,
    pub pos: Linear,
    /// `(heads, d_k)`
    pub bias_u: Tensor,
    pub bias_v: Tensor,
    pub heads: usize,
}

impl Attention {
    /// `x`: `(t, d)`; `pos`: `(2t − 1, d)` sinusoidal relative positions `t−1 … −(t−1)`.
    fn forward(&self, x: &Tensor, pos: &Tensor) -> Result<Tensor> {
        let x = self.norm.forward(x)?;
        let (t, d) = x.dims2()?;
        let h = self.heads;
        let dk = d / h;
        let split = |y: Tensor, n: usize| y.reshape((n, h, dk));
        let q = split(self.q.forward(&x)?, t)?;
        let k = split(self.k.forward(&x)?, t)?.transpose(0, 1)?.contiguous()?;
        let v = split(self.v.forward(&x)?, t)?.transpose(0, 1)?.contiguous()?;
        let p = split(self.pos.forward(pos)?, 2 * t - 1)?.transpose(0, 1)?.contiguous()?;
        let qu = q.broadcast_add(&self.bias_u)?.transpose(0, 1)?.contiguous()?;
        let qv = q.broadcast_add(&self.bias_v)?.transpose(0, 1)?.contiguous()?;
        // content term (h, t, t) and position term (h, t, 2t−1)
        let ac = qu.matmul(&k.t()?)?;
        let bd = qv.matmul(&p.t()?)?;
        let scores = ops::rel_scores(&ac, &bd, 1.0 / (dk as f32).sqrt())?;
        let w = candle_nn::ops::softmax_last_dim(&scores)?;
        let o = w.matmul(&v)?.transpose(0, 1)?.contiguous()?.reshape((t, d))?;
        self.out.forward(&o)
    }
}

pub struct ConvModule {
    pub norm: LayerNorm,
    pub pw1: Linear,
    /// Depthwise convolution with batch norm folded in, then SiLU.
    pub dw: ops::DepthwiseSilu,
    pub pw2: Linear,
}

impl ConvModule {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let x = ops::glu(&self.pw1.forward(&self.norm.forward(x)?)?)?;
        self.pw2.forward(&x.apply_op1_no_bwd(&self.dw)?)
    }
}

pub struct Layer {
    pub ff1: FeedForward,
    pub att: Attention,
    pub conv: ConvModule,
    pub ff2: FeedForward,
    pub norm_out: LayerNorm,
}

impl Layer {
    fn forward(&self, x: &Tensor, pos: &Tensor) -> Result<Tensor> {
        let x = (x + (self.ff1.forward(x)? * 0.5)?)?;
        let x = (&x + self.att.forward(&x, pos)?)?;
        let x = (&x + self.conv.forward(&x)?)?;
        let x = (&x + (self.ff2.forward(&x)? * 0.5)?)?;
        self.norm_out.forward(&x)
    }
}

/// The `dw_striding` subsampling stack.
pub struct Subsampling {
    /// `(C, 1, 3, 3)` and `(C)`
    pub conv0: (Tensor, Tensor),
    /// depthwise `(C, 1, 3, 3)` + bias, pointwise `(C, C)` + bias `(C, 1)`
    pub stages: Vec<(Tensor, Tensor, Tensor, Tensor)>,
    pub out: Linear,
    pub channels: usize,
}

impl Subsampling {
    /// `mel`: `(t, n_mels)` → `(t', d_model)`.
    fn forward(&self, mel: &Tensor) -> Result<Tensor> {
        let (t, f) = mel.dims2()?;
        let c = self.channels;
        let x = mel.reshape((1, 1, t, f))?;
        let x = x.conv2d(&self.conv0.0, 1, 2, 1, 1)?.broadcast_add(&self.conv0.1.reshape((1, c, 1, 1))?)?.relu()?;
        let mut x = x;
        for (dw, db, pw, pb) in &self.stages {
            let y = depthwise3x3s2(&x, dw)?.broadcast_add(&db.reshape((1, c, 1, 1))?)?;
            let (_, _, ht, wf) = y.dims4()?;
            // pointwise over channels: W (c, c) · Y (c, ht·wf)
            x = pw.matmul(&y.reshape((c, ht * wf))?)?.broadcast_add(pb)?.reshape((1, c, ht, wf))?.relu()?;
        }
        let (_, _, ht, wf) = x.dims4()?;
        let x = x.squeeze(0)?.transpose(0, 1)?.contiguous()?.reshape((ht, c * wf))?;
        self.out.forward(&x)
    }
}

/// Depthwise 3×3 convolution, stride 2, padding 1, over `(1, c, h, w)`; kernel `(c, 1, 3, 3)`.
fn depthwise3x3s2(x: &Tensor, k: &Tensor) -> Result<Tensor> {
    use rayon::prelude::*;
    let (_, c, h, w) = x.dims4()?;
    let (ho, wo) = ((h - 1) / 2 + 1, (w - 1) / 2 + 1);
    let xs = x.flatten_all()?.to_vec1::<f32>()?;
    let ks = k.flatten_all()?.to_vec1::<f32>()?;
    if xs.len() != c * h * w || ks.len() != c * 9 {
        candle_core::bail!("depthwise convolution: unexpected shapes");
    }
    let mut out = vec![0f32; c * ho * wo];
    out.par_chunks_mut(ho * wo).enumerate().for_each(|(ch, o)| {
        let src = &xs[ch * h * w..(ch + 1) * h * w];
        let kk = &ks[ch * 9..ch * 9 + 9];
        for i in 0..ho {
            for j in 0..wo {
                let mut s = 0f32;
                for di in 0..3 {
                    let y = (2 * i + di) as isize - 1;
                    if y < 0 || y >= h as isize {
                        continue;
                    }
                    let row = &src[y as usize * w..y as usize * w + w];
                    for dj in 0..3 {
                        let xx = (2 * j + dj) as isize - 1;
                        if xx >= 0 && xx < w as isize {
                            s += kk[di * 3 + dj] * row[xx as usize];
                        }
                    }
                }
                o[i * wo + j] = s;
            }
        }
    });
    Tensor::from_vec(out, (1, c, ho, wo), x.device())
}

/// Frames after the 8× subsampling of `t` mel frames (three k = 3, s = 2, p = 1 convolutions).
pub fn subsampled_len(t: usize) -> usize {
    (0..3).fold(t, |n, _| if n == 0 { 0 } else { (n - 1) / 2 + 1 })
}

/// Sinusoidal relative position table `(2t − 1, d)` for positions `t−1, …, −(t−1)`.
pub fn rel_positions(t: usize, d: usize, dev: &Device) -> Result<Tensor> {
    let n = 2 * t - 1;
    let mut pe = vec![0f32; n * d];
    for (r, row) in pe.chunks_mut(d).enumerate() {
        let pos = (t as f32 - 1.0) - r as f32;
        for i in 0..d / 2 {
            let div = (-((2 * i) as f32) * (10_000f32.ln() / d as f32)).exp();
            row[2 * i] = (pos * div).sin();
            row[2 * i + 1] = (pos * div).cos();
        }
    }
    Tensor::from_vec(pe, (n, d), dev)
}

pub struct Encoder {
    pub sub: Subsampling,
    pub layers: Vec<Layer>,
    pub d_model: usize,
    /// Multiply the subsampled features by √d (`xscaling`).
    pub xscale: Option<f64>,
}

impl Encoder {
    /// `mel`: `n_mels × frames` row-major (the front end's layout) → `(frames', d_model)`.
    /// `on_layer(i, n)` runs before layer `i` of `n`; returning `false` cancels.
    pub fn forward(&self, mel: &[f32], n_mels: usize, frames: usize, dev: &Device, on_layer: &mut dyn FnMut(usize, usize) -> bool) -> Result<Tensor> {
        let m = Tensor::from_slice(mel, (n_mels, frames), dev)?.t()?.contiguous()?;
        let mut x = self.sub.forward(&m)?;
        if let Some(s) = self.xscale {
            x = (x * s)?;
        }
        let t = x.dim(0)?;
        let pos = rel_positions(t, self.d_model, dev)?;
        let n = self.layers.len();
        for (i, l) in self.layers.iter().enumerate() {
            if !on_layer(i, n) {
                candle_core::bail!("cancelled");
            }
            x = l.forward(&x, &pos)?;
        }
        x.to_dtype(DType::F32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsampled_lengths() {
        assert_eq!(subsampled_len(1043), 131);
        assert_eq!(subsampled_len(8), 1);
        assert_eq!(subsampled_len(9), 2);
        assert_eq!(subsampled_len(0), 0);
    }

    #[test]
    fn depthwise_matches_candle_grouped_conv() {
        let dev = Device::Cpu;
        let x = Tensor::from_vec((0..2 * 7 * 5).map(|i| (i as f32 * 0.37).sin()).collect::<Vec<_>>(), (1, 2, 7, 5), &dev).unwrap();
        let k = Tensor::from_vec((0..18).map(|i| (i as f32 * 0.11).cos()).collect::<Vec<_>>(), (2, 1, 3, 3), &dev).unwrap();
        let a = depthwise3x3s2(&x, &k).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap();
        let b = x.conv2d(&k, 1, 2, 1, 2).unwrap().flatten_all().unwrap().to_vec1::<f32>().unwrap();
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-5);
        }
    }
}
