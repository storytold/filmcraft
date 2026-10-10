//! Building blocks shared by the Kokoro modules. Tensors are `[1, channels, time]` unless noted;
//! everything runs in f32 on the CPU.

use candle_core::{D, Tensor};

use super::weights::{Weights, err};
use crate::TtsError;

pub(crate) type R<T> = Result<T, TtsError>;

/// 1-D convolution with optional bias.
pub(crate) struct Conv {
    w: Tensor,
    b: Option<Tensor>,
    pad: usize,
    stride: usize,
    dil: usize,
    groups: usize,
}

impl Conv {
    pub(crate) fn load(wt: &Weights, name: &str, pad: usize, stride: usize, dil: usize, groups: usize) -> R<Conv> {
        let w = wt.get(&format!("{name}.weight"))?;
        if w.rank() != 3 {
            return Err(err(format!("{name}: conv weight must be 3-D")));
        }
        let b = wt.get(&format!("{name}.bias")).ok();
        Ok(Conv { w, b, pad, stride, dil, groups })
    }
    /// `pad = dil · (k − 1) / 2` ("same" length for stride 1).
    pub(crate) fn same(wt: &Weights, name: &str, dil: usize) -> R<Conv> {
        let k = wt.get(&format!("{name}.weight"))?.dims().get(2).copied().unwrap_or(1);
        Conv::load(wt, name, dil * (k.saturating_sub(1)) / 2, 1, dil, 1)
    }
    pub(crate) fn fwd(&self, x: &Tensor) -> R<Tensor> {
        let y = x.conv1d(&self.w, self.pad, self.stride, self.dil, self.groups).map_err(err)?;
        add_bias(y, self.b.as_ref())
    }
}

fn add_bias(y: Tensor, b: Option<&Tensor>) -> R<Tensor> {
    match b {
        Some(b) => {
            let c = b.dims1().map_err(err)?;
            y.broadcast_add(&b.reshape((1, c, 1)).map_err(err)?).map_err(err)
        }
        None => Ok(y),
    }
}

/// Transposed 1-D convolution.
pub(crate) struct ConvT {
    w: Tensor,
    b: Option<Tensor>,
    pad: usize,
    out_pad: usize,
    stride: usize,
    groups: usize,
}

impl ConvT {
    pub(crate) fn load(wt: &Weights, name: &str, pad: usize, out_pad: usize, stride: usize, groups: usize) -> R<ConvT> {
        Ok(ConvT { w: wt.get(&format!("{name}.weight"))?, b: wt.get(&format!("{name}.bias")).ok(), pad, out_pad, stride, groups })
    }
    pub(crate) fn fwd(&self, x: &Tensor) -> R<Tensor> {
        let y = x.conv_transpose1d(&self.w, self.pad, self.out_pad, self.stride, 1, self.groups).map_err(err)?;
        add_bias(y, self.b.as_ref())
    }
}

/// Linear layer on the last dimension.
pub(crate) struct Linear {
    w: Tensor,
    b: Option<Tensor>,
}

impl Linear {
    pub(crate) fn load(wt: &Weights, name: &str) -> R<Linear> {
        Ok(Linear { w: wt.get(&format!("{name}.weight"))?.t().map_err(err)?, b: wt.get(&format!("{name}.bias")).ok() })
    }
    pub(crate) fn fwd(&self, x: &Tensor) -> R<Tensor> {
        let y = x.broadcast_matmul(&self.w).map_err(err)?;
        match &self.b {
            Some(b) => y.broadcast_add(b).map_err(err),
            None => Ok(y),
        }
    }
}

pub(crate) fn leaky(x: &Tensor, slope: f64) -> R<Tensor> {
    // max(x, slope · x) for 0 < slope < 1
    x.maximum(&(x * slope).map_err(err)?).map_err(err)
}

/// Normalise over the last dimension (layer norm without affine).
pub(crate) fn norm_last(x: &Tensor, eps: f64) -> R<Tensor> {
    let mean = x.mean_keepdim(D::Minus1).map_err(err)?;
    let xc = x.broadcast_sub(&mean).map_err(err)?;
    let var = xc.sqr().map_err(err)?.mean_keepdim(D::Minus1).map_err(err)?;
    xc.broadcast_div(&(var + eps).map_err(err)?.sqrt().map_err(err)?).map_err(err)
}

/// Layer norm over the last dimension with weight and bias.
pub(crate) struct LayerNorm {
    g: Tensor,
    b: Tensor,
    eps: f64,
}

impl LayerNorm {
    pub(crate) fn load(wt: &Weights, gamma: &str, beta: &str, eps: f64) -> R<LayerNorm> {
        Ok(LayerNorm { g: wt.get(gamma)?, b: wt.get(beta)?, eps })
    }
    pub(crate) fn fwd(&self, x: &Tensor) -> R<Tensor> {
        norm_last(x, self.eps)?.broadcast_mul(&self.g).map_err(err)?.broadcast_add(&self.b).map_err(err)
    }
}

/// Split a style projection `[1, 2C]` into `(1 + gamma, beta)`.
fn style_affine(fc: &Linear, s: &Tensor) -> R<(Tensor, Tensor)> {
    let h = fc.fwd(s)?;
    let c2 = h.dim(1).map_err(err)?;
    let c = c2 / 2;
    let gamma = (h.narrow(1, 0, c).map_err(err)? + 1.0).map_err(err)?;
    let beta = h.narrow(1, c, c).map_err(err)?;
    Ok((gamma, beta))
}

/// Adaptive instance norm: instance-normalise each channel over time, then scale and shift by the
/// style (StyleTTS 2).
pub(crate) struct AdaIn {
    fc: Linear,
}

impl AdaIn {
    pub(crate) fn load(wt: &Weights, name: &str) -> R<AdaIn> {
        Ok(AdaIn { fc: Linear::load(wt, &format!("{name}.fc"))? })
    }
    pub(crate) fn fwd(&self, x: &Tensor, s: &Tensor) -> R<Tensor> {
        let (gamma, beta) = style_affine(&self.fc, s)?;
        let c = gamma.dim(1).map_err(err)?;
        let n = norm_last(x, 1e-5)?;
        n.broadcast_mul(&gamma.reshape((1, c, 1)).map_err(err)?).map_err(err)?.broadcast_add(&beta.reshape((1, c, 1)).map_err(err)?).map_err(err)
    }
}

/// Adaptive layer norm on `[1, T, C]`: layer-normalise over channels, then style scale and shift.
pub(crate) struct AdaLayerNorm {
    fc: Linear,
}

impl AdaLayerNorm {
    pub(crate) fn load(wt: &Weights, name: &str) -> R<AdaLayerNorm> {
        Ok(AdaLayerNorm { fc: Linear::load(wt, &format!("{name}.fc"))? })
    }
    pub(crate) fn fwd(&self, x: &Tensor, s: &Tensor) -> R<Tensor> {
        let (gamma, beta) = style_affine(&self.fc, s)?;
        norm_last(x, 1e-5)?.broadcast_mul(&gamma).map_err(err)?.broadcast_add(&beta).map_err(err)
    }
}

/// Residual block with adaptive instance norm (StyleTTS 2 `AdainResBlk1d`), optionally doubling
/// the time resolution.
pub(crate) struct AdaResBlock {
    norm1: AdaIn,
    norm2: AdaIn,
    conv1: Conv,
    conv2: Conv,
    shortcut: Option<Conv>,
    pool: Option<ConvT>,
}

impl AdaResBlock {
    pub(crate) fn load(wt: &Weights, name: &str, upsample: bool) -> R<AdaResBlock> {
        let shortcut = wt.get(&format!("{name}.conv1x1.weight")).ok().map(|w| Conv { w, b: None, pad: 0, stride: 1, dil: 1, groups: 1 });
        let pool = if upsample {
            let c = wt.get(&format!("{name}.pool.weight"))?.dim(0).map_err(err)?;
            Some(ConvT::load(wt, &format!("{name}.pool"), 1, 1, 2, c)?)
        } else {
            None
        };
        Ok(AdaResBlock {
            norm1: AdaIn::load(wt, &format!("{name}.norm1"))?,
            norm2: AdaIn::load(wt, &format!("{name}.norm2"))?,
            conv1: Conv::same(wt, &format!("{name}.conv1"), 1)?,
            conv2: Conv::same(wt, &format!("{name}.conv2"), 1)?,
            shortcut,
            pool,
        })
    }
    pub(crate) fn upsamples(&self) -> bool {
        self.pool.is_some()
    }
    pub(crate) fn fwd(&self, x: &Tensor, s: &Tensor) -> R<Tensor> {
        let mut r = leaky(&self.norm1.fwd(x, s)?, 0.2)?;
        if let Some(p) = &self.pool {
            r = p.fwd(&r)?;
        }
        r = self.conv1.fwd(&r)?;
        r = leaky(&self.norm2.fwd(&r, s)?, 0.2)?;
        r = self.conv2.fwd(&r)?;
        let mut sc = x.clone();
        if self.pool.is_some() {
            sc = upsample2(&sc)?;
        }
        if let Some(c) = &self.shortcut {
            sc = c.fwd(&sc)?;
        }
        ((r + sc).map_err(err)? / std::f64::consts::SQRT_2).map_err(err)
    }
}

/// Nearest-neighbour ×2 along time.
pub(crate) fn upsample2(x: &Tensor) -> R<Tensor> {
    let (b, c, t) = x.dims3().map_err(err)?;
    x.unsqueeze(3).map_err(err)?.broadcast_as((b, c, t, 2)).map_err(err)?.reshape((b, c, t * 2)).map_err(err)
}

/// Snake activation `x + sin²(αx) / α` with a learned α per channel.
pub(crate) fn snake(x: &Tensor, alpha: &Tensor) -> R<Tensor> {
    let ax = x.broadcast_mul(alpha).map_err(err)?;
    let s2 = ax.sin().map_err(err)?.sqr().map_err(err)?;
    let inv = (alpha.clone() + 1e-9).map_err(err)?.recip().map_err(err)?;
    (x + s2.broadcast_mul(&inv).map_err(err)?).map_err(err)
}

/// Generator residual block (HiFi-GAN "resblock 1" with adaptive instance norm and Snake).
pub(crate) struct SnakeResBlock {
    convs1: Vec<Conv>,
    convs2: Vec<Conv>,
    adain1: Vec<AdaIn>,
    adain2: Vec<AdaIn>,
    alpha1: Vec<Tensor>,
    alpha2: Vec<Tensor>,
}

impl SnakeResBlock {
    pub(crate) fn load(wt: &Weights, name: &str, dilations: [usize; 3]) -> R<SnakeResBlock> {
        let mut b = SnakeResBlock { convs1: vec![], convs2: vec![], adain1: vec![], adain2: vec![], alpha1: vec![], alpha2: vec![] };
        for (j, d) in dilations.iter().enumerate() {
            b.convs1.push(Conv::same(wt, &format!("{name}.convs1.{j}"), *d)?);
            b.convs2.push(Conv::same(wt, &format!("{name}.convs2.{j}"), 1)?);
            b.adain1.push(AdaIn::load(wt, &format!("{name}.adain1.{j}"))?);
            b.adain2.push(AdaIn::load(wt, &format!("{name}.adain2.{j}"))?);
            b.alpha1.push(wt.get(&format!("{name}.alpha1.{j}"))?);
            b.alpha2.push(wt.get(&format!("{name}.alpha2.{j}"))?);
        }
        Ok(b)
    }
    pub(crate) fn fwd(&self, x: &Tensor, s: &Tensor) -> R<Tensor> {
        let mut x = x.clone();
        for j in 0..self.convs1.len() {
            let (Some(c1), Some(c2), Some(n1), Some(n2), Some(a1), Some(a2)) =
                (self.convs1.get(j), self.convs2.get(j), self.adain1.get(j), self.adain2.get(j), self.alpha1.get(j), self.alpha2.get(j))
            else {
                break;
            };
            let mut t = snake(&n1.fwd(&x, s)?, a1)?;
            t = c1.fwd(&t)?;
            t = snake(&n2.fwd(&t, s)?, a2)?;
            t = c2.fwd(&t)?;
            x = (x + t).map_err(err)?;
        }
        Ok(x)
    }
}

/// One LSTM direction (PyTorch gate order i, f, g, o), run in plain loops for speed.
struct LstmDir {
    w_ih: Tensor,
    w_hh: Vec<f32>,
    bias: Vec<f32>,
    hidden: usize,
}

impl LstmDir {
    fn load(wt: &Weights, name: &str, suffix: &str) -> R<LstmDir> {
        let w_ih = wt.get(&format!("{name}.weight_ih_l0{suffix}"))?;
        let w_hh = wt.get(&format!("{name}.weight_hh_l0{suffix}"))?;
        let (g4, hidden) = w_hh.dims2().map_err(err)?;
        if g4 != 4 * hidden {
            return Err(err(format!("{name}: bad LSTM shape")));
        }
        let b_ih: Vec<f32> = wt.get(&format!("{name}.bias_ih_l0{suffix}"))?.to_vec1().map_err(err)?;
        let b_hh: Vec<f32> = wt.get(&format!("{name}.bias_hh_l0{suffix}"))?.to_vec1().map_err(err)?;
        let bias = b_ih.iter().zip(&b_hh).map(|(a, b)| a + b).collect();
        // row-major [4H, H] → transposed [H, 4H] so the recurrent product walks memory forwards
        let hh: Vec<Vec<f32>> = w_hh.to_vec2().map_err(err)?;
        let mut w_t = vec![0f32; hidden * g4];
        for (r, row) in hh.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                if let Some(slot) = w_t.get_mut(c * g4 + r) {
                    *slot = *v;
                }
            }
        }
        Ok(LstmDir { w_ih: w_ih.t().map_err(err)?, w_hh: w_t, bias, hidden })
    }

    /// `x`: `[T, I]` → `[T, H]` (in input order).
    fn run(&self, x: &Tensor, reverse: bool) -> R<Vec<f32>> {
        let (t_len, _) = x.dims2().map_err(err)?;
        let h = self.hidden;
        let g4 = 4 * h;
        let pre: Vec<f32> = x.matmul(&self.w_ih).map_err(err)?.flatten_all().map_err(err)?.to_vec1().map_err(err)?;
        let mut out = vec![0f32; t_len * h];
        let (mut hs, mut cs) = (vec![0f32; h], vec![0f32; h]);
        let mut gates = vec![0f32; g4];
        let sig = |v: f32| 1.0 / (1.0 + (-v).exp());
        for step in 0..t_len {
            let t = if reverse { t_len - 1 - step } else { step };
            let Some(p) = pre.get(t * g4..(t + 1) * g4) else { break };
            for ((g, a), b) in gates.iter_mut().zip(p).zip(&self.bias) {
                *g = a + b;
            }
            for (k, hv) in hs.iter().enumerate() {
                if *hv == 0.0 {
                    continue;
                }
                let Some(row) = self.w_hh.get(k * g4..(k + 1) * g4) else { break };
                for (g, w) in gates.iter_mut().zip(row) {
                    *g += hv * w;
                }
            }
            for j in 0..h {
                let (i, f, gg, o) = (gates[j], gates[h + j], gates[2 * h + j], gates[3 * h + j]);
                let c = sig(f) * cs[j] + sig(i) * gg.tanh();
                cs[j] = c;
                hs[j] = sig(o) * c.tanh();
            }
            if let Some(slot) = out.get_mut(t * h..(t + 1) * h) {
                slot.copy_from_slice(&hs);
            }
        }
        Ok(out)
    }
}

/// Bidirectional single-layer LSTM on `[1, T, I]` → `[1, T, 2H]`.
pub(crate) struct BiLstm {
    fwd: LstmDir,
    bwd: LstmDir,
}

impl BiLstm {
    pub(crate) fn load(wt: &Weights, name: &str) -> R<BiLstm> {
        Ok(BiLstm { fwd: LstmDir::load(wt, name, "")?, bwd: LstmDir::load(wt, name, "_reverse")? })
    }
    pub(crate) fn fwd(&self, x: &Tensor) -> R<Tensor> {
        let x2 = x.squeeze(0).map_err(err)?.contiguous().map_err(err)?;
        let t = x2.dim(0).map_err(err)?;
        let h = self.fwd.hidden;
        let a = self.fwd.run(&x2, false)?;
        let b = self.bwd.run(&x2, true)?;
        let mut out = Vec::with_capacity(t * 2 * h);
        for i in 0..t {
            out.extend_from_slice(a.get(i * h..(i + 1) * h).unwrap_or(&[]));
            out.extend_from_slice(b.get(i * h..(i + 1) * h).unwrap_or(&[]));
        }
        Tensor::from_vec(out, (1, t, 2 * h), x.device()).map_err(err)
    }
}
