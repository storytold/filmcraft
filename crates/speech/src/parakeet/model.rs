//! The Parakeet TDT network on candle (CPU), written from the published architecture —
//! D. Rekesh et al., "Fast Conformer with Linearly Scalable Attention for Efficient Speech
//! Recognition" (2023); H. Xu et al., "Efficient Sequence Transduction by Jointly Predicting Tokens
//! and Durations" (TDT, 2023); Z. Dai et al., "Transformer-XL" (2019) for the relative-position
//! attention — and the tensor names and configuration of NVIDIA's Hugging Face safetensors
//! release (`nvidia/parakeet-tdt-0.6b-v3`, CC-BY-4.0).
//!
//! **Encoder (FastConformer).** 8× subsampling: a 3×3 stride-2 convolution (1 → 256 channels) and
//! ReLU, then twice a depthwise 3×3 stride-2 convolution, a pointwise convolution and ReLU; the
//! 256 channels × 16 remaining mel bins are flattened and projected to 1024. Then 24 Conformer
//! blocks: ½·feed-forward, relative-position multi-head self-attention (8 heads, content and
//! position biases, sinusoidal relative embeddings for offsets T−1 … −(T−1)), the convolution
//! module (pointwise → GLU → depthwise k = 9 → batch norm → SiLU → pointwise) and ½·feed-forward,
//! each pre-LayerNorm with a residual, then a final LayerNorm. One output frame is 80 ms.
//!
//! **Decoder (prediction network).** Token embedding, 2-layer LSTM (640), linear projection.
//! **Joint.** `ReLU(enc_proj(frame) + dec) → linear` to 8193 token logits (8192 = blank) and 5
//! duration logits (0–4 frames). The decoder and joint run per step on plain `f32` slices (they
//! are small, and candle's per-op overhead would dominate).

use std::collections::HashMap;

use candle_core::{DType, Device, Module, Result, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, LayerNorm, Linear, VarBuilder};

/// `config.json` fields we use.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    pub encoder_config: EncoderConfig,
    pub decoder_hidden_size: usize,
    pub num_decoder_layers: usize,
    pub vocab_size: usize,
    pub blank_token_id: u32,
    pub durations: Vec<usize>,
    #[serde(default = "max_symbols")]
    pub max_symbols_per_step: usize,
}

fn max_symbols() -> usize {
    10
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct EncoderConfig {
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub num_attention_heads: usize,
    pub num_hidden_layers: usize,
    pub num_mel_bins: usize,
    pub conv_kernel_size: usize,
    pub subsampling_conv_channels: usize,
    pub subsampling_factor: usize,
    #[serde(default)]
    pub scale_input: bool,
}

fn ln(d: usize, vb: VarBuilder) -> Result<LayerNorm> {
    candle_nn::layer_norm(d, 1e-5, vb)
}

struct FeedForward {
    l1: Linear,
    l2: Linear,
}

impl FeedForward {
    fn new(c: &EncoderConfig, vb: VarBuilder) -> Result<Self> {
        Ok(Self {
            l1: candle_nn::linear_no_bias(c.hidden_size, c.intermediate_size, vb.pp("linear1"))?,
            l2: candle_nn::linear_no_bias(c.intermediate_size, c.hidden_size, vb.pp("linear2"))?,
        })
    }
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        self.l2.forward(&self.l1.forward(x)?.silu()?)
    }
}

struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    pos: Linear,
    /// (1, heads, 1, head_dim)
    bias_u: Tensor,
    bias_v: Tensor,
    heads: usize,
}

impl Attention {
    fn new(c: &EncoderConfig, vb: VarBuilder) -> Result<Self> {
        let d = c.hidden_size;
        let h = c.num_attention_heads.max(1);
        Ok(Self {
            q: candle_nn::linear_no_bias(d, d, vb.pp("q_proj"))?,
            k: candle_nn::linear_no_bias(d, d, vb.pp("k_proj"))?,
            v: candle_nn::linear_no_bias(d, d, vb.pp("v_proj"))?,
            o: candle_nn::linear_no_bias(d, d, vb.pp("o_proj"))?,
            pos: candle_nn::linear_no_bias(d, d, vb.pp("relative_k_proj"))?,
            bias_u: vb.get((h, d / h), "bias_u")?.reshape((1, h, 1, d / h))?,
            bias_v: vb.get((h, d / h), "bias_v")?.reshape((1, h, 1, d / h))?,
            heads: h,
        })
    }

    /// `x`: (1, T, d); `pos`: (1, 2T − 1, d) relative embeddings for offsets T−1 … −(T−1).
    fn forward(&self, x: &Tensor, pos: &Tensor) -> Result<Tensor> {
        let (b, t, d) = x.dims3()?;
        let (h, dh) = (self.heads, d / self.heads);
        let split = |y: Tensor| -> Result<Tensor> { y.reshape((b, t, h, dh))?.transpose(1, 2)?.contiguous() };
        let q = split(self.q.forward(x)?)?;
        let k = split(self.k.forward(x)?)?;
        let v = split(self.v.forward(x)?)?;
        let np = pos.dim(1)?;
        // (1, h, dh, 2T−1)
        let p = self.pos.forward(pos)?.reshape((b, np, h, dh))?.permute((0, 2, 3, 1))?.contiguous()?;
        let scale = (dh as f64).powf(-0.5);
        let ac = q.broadcast_add(&self.bias_u)?.matmul(&k.transpose(2, 3)?.contiguous()?)?;
        let bd = q.broadcast_add(&self.bias_v)?.matmul(&p)?;
        // relative shift: row i, column j ← column j + T − 1 − i
        let bd = bd.pad_with_zeros(3, 1, 0)?.reshape((b, h, np + 1, t))?.narrow(2, 1, np)?.contiguous()?.reshape((b, h, t, np))?.narrow(3, 0, t)?;
        let scores = ((ac + bd)? * scale)?;
        let w = candle_nn::ops::softmax_last_dim(&scores)?;
        let y = w.matmul(&v)?.transpose(1, 2)?.contiguous()?.reshape((b, t, d))?;
        self.o.forward(&y)
    }
}

struct ConvModule {
    pw1: Linear,
    pw2: Linear,
    /// depthwise kernel (k, d), with the batch norm folded in
    dw: Tensor,
    /// batch-norm shift (1, d) after folding
    shift: Tensor,
    k: usize,
}

impl ConvModule {
    fn new(c: &EncoderConfig, vb: VarBuilder) -> Result<Self> {
        let d = c.hidden_size;
        let k = c.conv_kernel_size.max(1);
        let pw1 = Linear::new(vb.get((2 * d, d, 1), "pointwise_conv1.weight")?.reshape((2 * d, d))?, None);
        let pw2 = Linear::new(vb.get((d, d, 1), "pointwise_conv2.weight")?.reshape((d, d))?, None);
        let dw = vb.get((d, 1, k), "depthwise_conv.weight")?.reshape((d, k))?;
        let n = vb.pp("norm");
        let (g, beta, mean, var) = (n.get(d, "weight")?, n.get(d, "bias")?, n.get(d, "running_mean")?, n.get(d, "running_var")?);
        // BN(y) = (y − mean)·g/√(var+ε) + beta = y·s + (beta − mean·s)
        let s = g.div(&(var + 1e-5)?.sqrt()?)?;
        let shift = beta.sub(&mean.mul(&s)?)?.reshape((1, d))?;
        let dw = dw.broadcast_mul(&s.reshape((d, 1))?)?.t()?.contiguous()?;
        Ok(Self { pw1, pw2, dw, shift, k })
    }

    /// `x`: (1, T, d).
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let (_, t, d) = x.dims3()?;
        let y = self.pw1.forward(x)?.squeeze(0)?;
        let a = y.narrow(1, 0, d)?;
        let g = y.narrow(1, d, d)?;
        let y = a.mul(&candle_nn::ops::sigmoid(&g)?)?;
        let pad = (self.k - 1) / 2;
        let yp = y.pad_with_zeros(0, pad, self.k - 1 - pad)?;
        let mut acc = self.shift.broadcast_as((t, d))?.contiguous()?;
        for i in 0..self.k {
            acc = (acc + yp.narrow(0, i, t)?.broadcast_mul(&self.dw.narrow(0, i, 1)?)?)?;
        }
        self.pw2.forward(&acc.silu()?.unsqueeze(0)?)
    }
}

struct Block {
    ff1: FeedForward,
    attn: Attention,
    conv: ConvModule,
    ff2: FeedForward,
    n_ff1: LayerNorm,
    n_att: LayerNorm,
    n_conv: LayerNorm,
    n_ff2: LayerNorm,
    n_out: LayerNorm,
}

impl Block {
    fn new(c: &EncoderConfig, vb: VarBuilder) -> Result<Self> {
        let d = c.hidden_size;
        Ok(Self {
            ff1: FeedForward::new(c, vb.pp("feed_forward1"))?,
            attn: Attention::new(c, vb.pp("self_attn"))?,
            conv: ConvModule::new(c, vb.pp("conv"))?,
            ff2: FeedForward::new(c, vb.pp("feed_forward2"))?,
            n_ff1: ln(d, vb.pp("norm_feed_forward1"))?,
            n_att: ln(d, vb.pp("norm_self_att"))?,
            n_conv: ln(d, vb.pp("norm_conv"))?,
            n_ff2: ln(d, vb.pp("norm_feed_forward2"))?,
            n_out: ln(d, vb.pp("norm_out"))?,
        })
    }

    fn forward(&self, x: &Tensor, pos: &Tensor) -> Result<Tensor> {
        let x = (x + (self.ff1.forward(&self.n_ff1.forward(x)?)? * 0.5)?)?;
        let x = (&x + self.attn.forward(&self.n_att.forward(&x)?, pos)?)?;
        let x = (&x + self.conv.forward(&self.n_conv.forward(&x)?)?)?;
        let x = (&x + (self.ff2.forward(&self.n_ff2.forward(&x)?)? * 0.5)?)?;
        self.n_out.forward(&x)
    }
}

pub struct Encoder {
    /// (conv, is_pointwise_or_first) in order; ReLU after the first conv and after every pointwise one
    sub: Vec<(Conv2d, bool)>,
    sub_out: Linear,
    blocks: Vec<Block>,
    /// encoder → joint projection
    proj: Linear,
    hidden: usize,
    scale: f64,
}

impl Encoder {
    fn new(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        let c = &cfg.encoder_config;
        let ch = c.subsampling_conv_channels;
        let s = vb.pp("encoder.subsampling.layers");
        let stride2 = |groups| Conv2dConfig { padding: 1, stride: 2, dilation: 1, groups, cudnn_fwd_algo: None };
        let pw = Conv2dConfig::default();
        let mut sub = vec![(candle_nn::conv2d(1, ch, 3, stride2(1), s.pp("0"))?, true)];
        let stages = c.subsampling_factor.max(2).ilog2() as usize;
        // layer indices: 0 conv, 1 ReLU, then per stage depthwise, pointwise, ReLU
        for i in 1..stages {
            let base = 2 + (i - 1) * 3;
            sub.push((candle_nn::conv2d(ch, ch, 3, stride2(ch), s.pp(base.to_string()))?, false));
            sub.push((candle_nn::conv2d(ch, ch, 1, pw, s.pp((base + 1).to_string()))?, true));
        }
        let freq = c.num_mel_bins >> stages;
        let sub_out = candle_nn::linear(ch * freq, c.hidden_size, vb.pp("encoder.subsampling.linear"))?;
        let blocks = (0..c.num_hidden_layers).map(|i| Block::new(c, vb.pp(format!("encoder.layers.{i}")))).collect::<Result<Vec<_>>>()?;
        let proj = candle_nn::linear(c.hidden_size, cfg.decoder_hidden_size, vb.pp("encoder_projector"))?;
        let scale = if c.scale_input { (c.hidden_size as f64).sqrt() } else { 1.0 };
        Ok(Self { sub, sub_out, blocks, proj, hidden: c.hidden_size, scale })
    }

    /// Sinusoidal relative position embeddings (1, 2T − 1, d) for offsets T−1 … −(T−1),
    /// interleaved sin, cos.
    fn positions(&self, t: usize, dev: &Device) -> Result<Tensor> {
        let d = self.hidden;
        let n = 2 * t - 1;
        let mut v = vec![0f32; n * d];
        for (r, row) in v.chunks_mut(d).enumerate() {
            let p = (t as f64 - 1.0) - r as f64;
            for i in 0..d / 2 {
                let a = p / 10000f64.powf(2.0 * i as f64 / d as f64);
                row[2 * i] = a.sin() as f32;
                row[2 * i + 1] = a.cos() as f32;
            }
        }
        Tensor::from_vec(v, (1, n, d), dev)
    }

    /// `feats`: (1, frames, mels) → joint-space encoder frames (T, joint).
    pub fn forward(&self, feats: &Tensor, progress: &mut dyn FnMut(f32) -> bool) -> Result<Option<Tensor>> {
        let mut x = feats.unsqueeze(1)?;
        for (conv, relu) in &self.sub {
            x = conv.forward(&x)?;
            if *relu {
                x = x.relu()?;
            }
        }
        let (b, c, t, f) = x.dims4()?;
        let x = x.transpose(1, 2)?.contiguous()?.reshape((b, t, c * f))?;
        let mut x = (self.sub_out.forward(&x)? * self.scale)?;
        let pos = self.positions(t, x.device())?;
        let n = self.blocks.len().max(1);
        for (i, blk) in self.blocks.iter().enumerate() {
            x = blk.forward(&x, &pos)?;
            if !progress((i + 1) as f32 / n as f32) {
                return Ok(None);
            }
        }
        Ok(Some(self.proj.forward(&x)?.squeeze(0)?))
    }
}

/// Row-major matrix for the per-step parts.
struct Mat {
    rows: usize,
    cols: usize,
    w: Vec<f32>,
}

impl Mat {
    fn from(t: &Tensor) -> Result<Self> {
        let (rows, cols) = t.dims2()?;
        Ok(Self { rows, cols, w: t.flatten_all()?.to_vec1()? })
    }
    /// `out[r] += W[r]·x`
    fn mul_add(&self, x: &[f32], out: &mut [f32]) {
        for (row, o) in self.w.chunks_exact(self.cols).zip(out.iter_mut()).take(self.rows) {
            *o += dot(row, x);
        }
    }
}

/// Dot product with independent accumulators (vectorises).
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum::<f32>() + ra.iter().zip(rb).map(|(x, y)| x * y).sum::<f32>()
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

struct LstmLayer {
    w_ih: Mat,
    w_hh: Mat,
    bias: Vec<f32>,
}

/// LSTM state: (h, c) per layer.
#[derive(Clone)]
pub struct State {
    h: Vec<Vec<f32>>,
    c: Vec<Vec<f32>>,
}

pub struct Decoder {
    embed: Vec<f32>,
    hidden: usize,
    vocab: usize,
    layers: Vec<LstmLayer>,
    proj: Mat,
    proj_b: Vec<f32>,
    head: Mat,
    head_b: Vec<f32>,
}

impl Decoder {
    fn new(cfg: &Config, vb: &VarBuilder) -> Result<Self> {
        let hd = cfg.decoder_hidden_size;
        let d = vb.pp("decoder");
        let embed = d.get((cfg.vocab_size, hd), "embedding.weight")?.flatten_all()?.to_vec1()?;
        let mut layers = Vec::new();
        for l in 0..cfg.num_decoder_layers {
            let g = |n: &str| d.pp("lstm").get(4 * hd, &format!("{n}_l{l}"));
            let bias: Vec<f32> = (g("bias_ih")? + g("bias_hh")?)?.to_vec1()?;
            layers.push(LstmLayer {
                w_ih: Mat::from(&d.pp("lstm").get((4 * hd, hd), &format!("weight_ih_l{l}"))?)?,
                w_hh: Mat::from(&d.pp("lstm").get((4 * hd, hd), &format!("weight_hh_l{l}"))?)?,
                bias,
            });
        }
        let outs = cfg.vocab_size + cfg.durations.len();
        Ok(Self {
            embed,
            hidden: hd,
            vocab: cfg.vocab_size,
            layers,
            proj: Mat::from(&d.get((hd, hd), "decoder_projector.weight")?)?,
            proj_b: d.get(hd, "decoder_projector.bias")?.to_vec1()?,
            head: Mat::from(&vb.get((outs, hd), "joint.head.weight")?)?,
            head_b: vb.get(outs, "joint.head.bias")?.to_vec1()?,
        })
    }

    pub fn initial_state(&self) -> State {
        State { h: vec![vec![0.0; self.hidden]; self.layers.len()], c: vec![vec![0.0; self.hidden]; self.layers.len()] }
    }

    /// Feed `token`: returns the projected decoder output and updates `state`.
    pub fn step(&self, token: u32, state: &mut State) -> Vec<f32> {
        let hd = self.hidden;
        let tok = (token as usize).min(self.vocab.saturating_sub(1));
        let mut x: Vec<f32> = self.embed.get(tok * hd..(tok + 1) * hd).map(<[f32]>::to_vec).unwrap_or_else(|| vec![0.0; hd]);
        for (l, layer) in self.layers.iter().enumerate() {
            let (Some(h), Some(c)) = (state.h.get_mut(l), state.c.get_mut(l)) else { break };
            let mut g = layer.bias.clone();
            layer.w_ih.mul_add(&x, &mut g);
            layer.w_hh.mul_add(h, &mut g);
            // gate order: input, forget, cell, output
            for j in 0..hd {
                let (i, f, gg, o) = (sigmoid(g[j]), sigmoid(g[hd + j]), g[2 * hd + j].tanh(), sigmoid(g[3 * hd + j]));
                c[j] = f * c[j] + i * gg;
                h[j] = o * c[j].tanh();
            }
            x.clone_from(h);
        }
        let mut out = self.proj_b.clone();
        self.proj.mul_add(&x, &mut out);
        out
    }

    /// Joint logits for an encoder frame and a decoder output: `vocab` token logits then the
    /// duration logits.
    pub fn joint(&self, enc: &[f32], dec: &[f32]) -> Vec<f32> {
        let z: Vec<f32> = enc.iter().zip(dec).map(|(a, b)| (a + b).max(0.0)).collect();
        let mut out = self.head_b.clone();
        self.head.mul_add(&z, &mut out);
        out
    }
}

pub struct Model {
    pub cfg: Config,
    pub encoder: Encoder,
    pub decoder: Decoder,
    pub device: Device,
}

impl Model {
    pub fn load(cfg: Config, tensors: HashMap<String, Tensor>) -> Result<Self> {
        let device = Device::Cpu;
        let vb = VarBuilder::from_tensors(tensors, DType::F32, &device);
        let encoder = Encoder::new(&cfg, vb.clone())?;
        let decoder = Decoder::new(&cfg, &vb)?;
        Ok(Self { cfg, encoder, decoder, device })
    }

    pub fn n_mels(&self) -> usize {
        self.cfg.encoder_config.num_mel_bins
    }
}

/// Index of the largest value (0 when empty).
pub fn argmax(x: &[f32]) -> usize {
    x.iter().enumerate().fold((0, f32::NEG_INFINITY), |(bi, bv), (i, &v)| if v > bv { (i, v) } else { (bi, bv) }).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_handles_remainders() {
        let a: Vec<f32> = (0..13).map(|i| i as f32).collect();
        assert_eq!(dot(&a, &a), (0..13).map(|i| (i * i) as f32).sum::<f32>());
    }

    #[test]
    fn argmax_picks_the_largest() {
        assert_eq!(argmax(&[0.1, 3.0, -1.0, 2.9]), 1);
        assert_eq!(argmax(&[]), 0);
    }
}
