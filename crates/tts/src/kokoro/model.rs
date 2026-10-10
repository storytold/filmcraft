//! The Kokoro-82M forward pass (inference only, batch of one).
//!
//! 1. **Text** – phoneme ids go through an ALBERT encoder (one shared layer applied 12 times) and
//!    a linear map to 512 channels (`bert`, `bert_encoder`), and separately through an embedding,
//!    three convolutions and a bidirectional LSTM (`text_encoder`).
//! 2. **Prosody** – the duration encoder (LSTMs with style-adaptive layer norm), a duration head
//!    (50 bins, sigmoid, summed), then pitch (F0) and energy (N) curves at twice the frame rate from
//!    adaptive residual blocks (`predictor`).
//! 3. **Decoder** – text features expanded to frames by the durations, combined with F0 and N in
//!    adaptive residual blocks (`decoder.encode`, `decoder.decode`).
//! 4. **Vocoder** – iSTFTNet: a harmonic-plus-noise source from F0 (9 harmonics), two upsampling
//!    stages (×10, ×6) with source injection and Snake residual blocks, then magnitude and phase for
//!    an inverse STFT (n_fft 20, hop 5) at 24 kHz.
//!
//! Style: a voice pack row of 256 values; the first 128 condition the decoder, the last 128 the
//! prosody predictor.

use candle_core::{D, Device, Tensor};

use super::layers::*;
use super::weights::{Weights, err};

const HEADS: usize = 12;
const LAYERS: usize = 12;
pub const SAMPLE_RATE: u32 = 24_000;
/// Output samples per predicted duration frame (2 F0 frames × 60 upsampling × hop 5).
pub(crate) const SAMPLES_PER_FRAME: usize = 600;

struct Albert {
    word: Tensor,
    pos: Tensor,
    ttype: Tensor,
    emb_ln: LayerNorm,
    map_in: Linear,
    q: Linear,
    k: Linear,
    v: Linear,
    dense: Linear,
    attn_ln: LayerNorm,
    ffn: Linear,
    ffn_out: Linear,
    full_ln: LayerNorm,
}

impl Albert {
    fn load(w: &Weights) -> R<Albert> {
        let l = "bert.encoder.albert_layer_groups.0.albert_layers.0";
        Ok(Albert {
            word: w.shaped("bert.embeddings.word_embeddings.weight", &[178, 128])?,
            pos: w.shaped("bert.embeddings.position_embeddings.weight", &[512, 128])?,
            ttype: w.shaped("bert.embeddings.token_type_embeddings.weight", &[2, 128])?,
            emb_ln: LayerNorm::load(w, "bert.embeddings.LayerNorm.weight", "bert.embeddings.LayerNorm.bias", 1e-12)?,
            map_in: Linear::load(w, "bert.encoder.embedding_hidden_mapping_in")?,
            q: Linear::load(w, &format!("{l}.attention.query"))?,
            k: Linear::load(w, &format!("{l}.attention.key"))?,
            v: Linear::load(w, &format!("{l}.attention.value"))?,
            dense: Linear::load(w, &format!("{l}.attention.dense"))?,
            attn_ln: LayerNorm::load(w, &format!("{l}.attention.LayerNorm.weight"), &format!("{l}.attention.LayerNorm.bias"), 1e-12)?,
            ffn: Linear::load(w, &format!("{l}.ffn"))?,
            ffn_out: Linear::load(w, &format!("{l}.ffn_output"))?,
            full_ln: LayerNorm::load(w, &format!("{l}.full_layer_layer_norm.weight"), &format!("{l}.full_layer_layer_norm.bias"), 1e-12)?,
        })
    }

    /// `ids`: `[T]` → `[1, T, 768]`.
    fn fwd(&self, ids: &Tensor) -> R<Tensor> {
        let t = ids.dim(0).map_err(err)?;
        let e = self.word.index_select(ids, 0).map_err(err)?;
        let e = (e + self.pos.narrow(0, 0, t).map_err(err)?).map_err(err)?;
        let e = e.broadcast_add(&self.ttype.narrow(0, 0, 1).map_err(err)?).map_err(err)?;
        let mut x = self.map_in.fwd(&self.emb_ln.fwd(&e)?)?.unsqueeze(0).map_err(err)?; // [1, T, 768]
        let hd = 768 / HEADS;
        let scale = 1.0 / (hd as f64).sqrt();
        let split = |y: Tensor| -> R<Tensor> { y.reshape((1, t, HEADS, hd)).map_err(err)?.transpose(1, 2).map_err(err)?.contiguous().map_err(err) };
        for _ in 0..LAYERS {
            let q = split(self.q.fwd(&x)?)?;
            let k = split(self.k.fwd(&x)?)?;
            let v = split(self.v.fwd(&x)?)?;
            let att = (q.matmul(&k.t().map_err(err)?).map_err(err)? * scale).map_err(err)?;
            let att = candle_nn::ops::softmax_last_dim(&att).map_err(err)?;
            let ctx = att.matmul(&v).map_err(err)?.transpose(1, 2).map_err(err)?.reshape((1, t, 768)).map_err(err)?;
            let a = self.attn_ln.fwd(&(x + self.dense.fwd(&ctx)?).map_err(err)?)?;
            let f = self.ffn_out.fwd(&self.ffn.fwd(&a)?.gelu().map_err(err)?)?;
            x = self.full_ln.fwd(&(f + a).map_err(err)?)?;
        }
        Ok(x)
    }
}

struct TextEncoder {
    emb: Tensor,
    convs: Vec<(Conv, LayerNorm)>,
    lstm: BiLstm,
}

impl TextEncoder {
    fn load(w: &Weights) -> R<TextEncoder> {
        let mut convs = Vec::new();
        for i in 0..3 {
            let c = Conv::same(w, &format!("text_encoder.cnn.{i}.0"), 1)?;
            let n = LayerNorm::load(w, &format!("text_encoder.cnn.{i}.1.gamma"), &format!("text_encoder.cnn.{i}.1.beta"), 1e-5)?;
            convs.push((c, n));
        }
        Ok(TextEncoder { emb: w.shaped("text_encoder.embedding.weight", &[178, 512])?, convs, lstm: BiLstm::load(w, "text_encoder.lstm")? })
    }
    /// `[T]` → `[1, 512, T]`.
    fn fwd(&self, ids: &Tensor) -> R<Tensor> {
        let mut x = self.emb.index_select(ids, 0).map_err(err)?.t().map_err(err)?.unsqueeze(0).map_err(err)?; // [1, 512, T]
        for (c, n) in &self.convs {
            x = c.fwd(&x)?;
            // layer norm over channels
            x = n.fwd(&x.transpose(1, 2).map_err(err)?)?.transpose(1, 2).map_err(err)?;
            x = leaky(&x, 0.2)?;
        }
        let y = self.lstm.fwd(&x.transpose(1, 2).map_err(err)?.contiguous().map_err(err)?)?; // [1, T, 512]
        y.transpose(1, 2).map_err(err)
    }
}

struct Predictor {
    dur_lstms: Vec<BiLstm>,
    dur_norms: Vec<AdaLayerNorm>,
    lstm: BiLstm,
    dur_proj: Linear,
    shared: BiLstm,
    f0: Vec<AdaResBlock>,
    n: Vec<AdaResBlock>,
    f0_proj: Conv,
    n_proj: Conv,
}

impl Predictor {
    fn load(w: &Weights) -> R<Predictor> {
        let mut dur_lstms = Vec::new();
        let mut dur_norms = Vec::new();
        for i in 0..3 {
            dur_lstms.push(BiLstm::load(w, &format!("predictor.text_encoder.lstms.{}", 2 * i))?);
            dur_norms.push(AdaLayerNorm::load(w, &format!("predictor.text_encoder.lstms.{}", 2 * i + 1))?);
        }
        let blocks = |k: &str| -> R<Vec<AdaResBlock>> { (0..3).map(|i| AdaResBlock::load(w, &format!("predictor.{k}.{i}"), i == 1)).collect() };
        Ok(Predictor {
            dur_lstms,
            dur_norms,
            lstm: BiLstm::load(w, "predictor.lstm")?,
            dur_proj: Linear::load(w, "predictor.duration_proj.linear_layer")?,
            shared: BiLstm::load(w, "predictor.shared")?,
            f0: blocks("F0")?,
            n: blocks("N")?,
            f0_proj: Conv::load(w, "predictor.F0_proj", 0, 1, 1, 1)?,
            n_proj: Conv::load(w, "predictor.N_proj", 0, 1, 1, 1)?,
        })
    }

    /// Duration encoder: `d_en` `[1, 512, T]`, style `[1, 128]` → `[1, T, 640]`.
    fn encode(&self, d_en: &Tensor, s: &Tensor) -> R<Tensor> {
        let t = d_en.dim(2).map_err(err)?;
        let s_t = s.unsqueeze(1).map_err(err)?.broadcast_as((1, t, 128)).map_err(err)?.contiguous().map_err(err)?;
        let mut x = Tensor::cat(&[&d_en.transpose(1, 2).map_err(err)?.contiguous().map_err(err)?, &s_t], 2).map_err(err)?;
        for (l, n) in self.dur_lstms.iter().zip(&self.dur_norms) {
            let y = n.fwd(&l.fwd(&x)?, s)?;
            x = Tensor::cat(&[&y, &s_t], 2).map_err(err)?;
        }
        Ok(x)
    }

    /// Frames per input token, at `speed`.
    fn durations(&self, d: &Tensor, speed: f64) -> R<Vec<usize>> {
        let x = self.lstm.fwd(d)?;
        let logits = self.dur_proj.fwd(&x)?;
        let dur = candle_nn::ops::sigmoid(&logits).map_err(err)?.sum(D::Minus1).map_err(err)?.squeeze(0).map_err(err)?;
        let v: Vec<f32> = dur.to_vec1().map_err(err)?;
        Ok(v.iter().map(|d| ((f64::from(*d) / speed).round().clamp(1.0, 200.0)) as usize).collect())
    }

    /// F0 and N curves `[1, 1, 2F]` from frame features `en` `[1, 640, F]`.
    fn f0_n(&self, en: &Tensor, s: &Tensor) -> R<(Tensor, Tensor)> {
        let x = self.shared.fwd(&en.transpose(1, 2).map_err(err)?.contiguous().map_err(err)?)?.transpose(1, 2).map_err(err)?.contiguous().map_err(err)?;
        let mut f = x.clone();
        for b in &self.f0 {
            f = b.fwd(&f, s)?;
        }
        let mut n = x;
        for b in &self.n {
            n = b.fwd(&n, s)?;
        }
        Ok((self.f0_proj.fwd(&f)?, self.n_proj.fwd(&n)?))
    }
}

struct Generator {
    l_linear: Linear,
    noise_convs: Vec<Conv>,
    noise_res: Vec<SnakeResBlock>,
    ups: Vec<ConvT>,
    resblocks: Vec<SnakeResBlock>,
    conv_post: Conv,
}

impl Generator {
    fn load(w: &Weights) -> R<Generator> {
        let g = "decoder.generator";
        let dil = [1, 3, 5];
        Ok(Generator {
            l_linear: Linear::load(w, &format!("{g}.m_source.l_linear"))?,
            noise_convs: vec![Conv::load(w, &format!("{g}.noise_convs.0"), 3, 6, 1, 1)?, Conv::load(w, &format!("{g}.noise_convs.1"), 0, 1, 1, 1)?],
            noise_res: vec![SnakeResBlock::load(w, &format!("{g}.noise_res.0"), dil)?, SnakeResBlock::load(w, &format!("{g}.noise_res.1"), dil)?],
            ups: vec![ConvT::load(w, &format!("{g}.ups.0"), 5, 0, 10, 1)?, ConvT::load(w, &format!("{g}.ups.1"), 3, 0, 6, 1)?],
            resblocks: (0..6).map(|i| SnakeResBlock::load(w, &format!("{g}.resblocks.{i}"), dil)).collect::<R<_>>()?,
            conv_post: Conv::same(w, &format!("{g}.conv_post"), 1)?,
        })
    }

    /// `x` `[1, 512, 2F]`, style `[1, 128]`, F0 per half-frame → samples.
    fn fwd(&self, x: &Tensor, s: &Tensor, f0: &[f32], seed: u64) -> R<Vec<f32>> {
        let dev = x.device();
        let source = harmonic_source(f0, &self.l_linear, seed, dev)?;
        let (mag, phase) = super::dsp::stft(&source);
        let frames = mag.len() / super::dsp::BINS;
        let mut har = mag;
        har.extend_from_slice(&phase);
        let har = Tensor::from_vec(har, (1, 2 * super::dsp::BINS, frames), dev).map_err(err)?;
        let mut x = x.clone();
        for i in 0..2 {
            let (Some(nc), Some(nr), Some(up)) = (self.noise_convs.get(i), self.noise_res.get(i), self.ups.get(i)) else { break };
            x = leaky(&x, 0.1)?;
            let xs = nr.fwd(&nc.fwd(&har)?, s)?;
            x = up.fwd(&x)?;
            if i == 1 {
                // reflection pad one frame on the left
                let first = x.narrow(2, 1, 1).map_err(err)?;
                x = Tensor::cat(&[&first, &x], 2).map_err(err)?;
            }
            let t = x.dim(2).map_err(err)?.min(xs.dim(2).map_err(err)?);
            x = (x.narrow(2, 0, t).map_err(err)? + xs.narrow(2, 0, t).map_err(err)?).map_err(err)?;
            let mut acc: Option<Tensor> = None;
            for j in 0..3 {
                let Some(rb) = self.resblocks.get(i * 3 + j) else { break };
                let y = rb.fwd(&x, s)?;
                acc = Some(match acc {
                    Some(a) => (a + y).map_err(err)?,
                    None => y,
                });
            }
            x = (acc.ok_or_else(|| err("missing resblocks"))? / 3.0).map_err(err)?;
        }
        x = leaky(&x, 0.01)?;
        let y = self.conv_post.fwd(&x)?.squeeze(0).map_err(err)?; // [22, T]
        let bins = super::dsp::BINS;
        let mag: Vec<f32> = y.narrow(0, 0, bins).map_err(err)?.exp().map_err(err)?.flatten_all().map_err(err)?.to_vec1().map_err(err)?;
        let ph: Vec<f32> = y.narrow(0, bins, bins).map_err(err)?.sin().map_err(err)?.flatten_all().map_err(err)?.to_vec1().map_err(err)?;
        Ok(super::dsp::istft(&mag, &ph))
    }
}

/// Neural source filter: 9 sine harmonics of F0 (voiced) or noise (unvoiced), mixed to one
/// channel by a learned linear layer and tanh. F0 is given per half-frame and held for 300 samples.
fn harmonic_source(f0: &[f32], l_linear: &Linear, seed: u64, dev: &Device) -> R<Vec<f32>> {
    const HARM: usize = 9;
    const HOLD: usize = SAMPLES_PER_FRAME / 2;
    let n = f0.len() * HOLD;
    let mut rng = super::dsp::Rng::new(seed);
    // random start phase for the overtones, none for the fundamental
    let start: Vec<f64> = (0..HARM).map(|h| if h == 0 { 0.0 } else { rng.uniform() }).collect();
    let mut phase = start;
    let mut sines = vec![0f32; n * HARM];
    for (i, row) in sines.as_chunks_mut::<HARM>().0.iter_mut().enumerate() {
        let f = f64::from(f0.get(i / HOLD).copied().unwrap_or(0.0)).max(0.0);
        let voiced = f > 10.0;
        let noise_amp = if voiced { 0.003 } else { 0.1 / 3.0 };
        for (h, out) in row.iter_mut().enumerate() {
            phase[h] = (phase[h] + f * (h + 1) as f64 / f64::from(SAMPLE_RATE)).fract();
            let sine = if voiced { 0.1 * (std::f64::consts::TAU * phase[h]).sin() } else { 0.0 };
            *out = (sine + noise_amp * rng.normal()) as f32;
        }
    }
    let t = Tensor::from_vec(sines, (n, HARM), dev).map_err(err)?;
    let y = l_linear.fwd(&t)?.tanh().map_err(err)?.flatten_all().map_err(err)?;
    y.to_vec1().map_err(err)
}

struct Decoder {
    f0_conv: Conv,
    n_conv: Conv,
    encode: AdaResBlock,
    decode: Vec<AdaResBlock>,
    asr_res: Conv,
    vocoder: Generator,
}

impl Decoder {
    fn load(w: &Weights) -> R<Decoder> {
        Ok(Decoder {
            f0_conv: Conv::load(w, "decoder.F0_conv", 1, 2, 1, 1)?,
            n_conv: Conv::load(w, "decoder.N_conv", 1, 2, 1, 1)?,
            encode: AdaResBlock::load(w, "decoder.encode", false)?,
            decode: (0..4).map(|i| AdaResBlock::load(w, &format!("decoder.decode.{i}"), i == 3)).collect::<R<_>>()?,
            asr_res: Conv::load(w, "decoder.asr_res.0", 0, 1, 1, 1)?,
            vocoder: Generator::load(w)?,
        })
    }

    fn fwd(&self, asr: &Tensor, f0: &Tensor, n: &Tensor, s: &Tensor, seed: u64) -> R<Vec<f32>> {
        let f0_curve: Vec<f32> = f0.flatten_all().map_err(err)?.to_vec1().map_err(err)?;
        let f = self.f0_conv.fwd(f0)?;
        let nn = self.n_conv.fwd(n)?;
        let frames = asr.dim(2).map_err(err)?.min(f.dim(2).map_err(err)?).min(nn.dim(2).map_err(err)?);
        let asr = asr.narrow(2, 0, frames).map_err(err)?;
        let f = f.narrow(2, 0, frames).map_err(err)?;
        let nn = nn.narrow(2, 0, frames).map_err(err)?;
        let mut x = self.encode.fwd(&Tensor::cat(&[&asr, &f, &nn], 1).map_err(err)?, s)?;
        let res = self.asr_res.fwd(&asr)?;
        let mut concat = true;
        for b in &self.decode {
            if concat {
                x = Tensor::cat(&[&x, &res, &f, &nn], 1).map_err(err)?;
            }
            x = b.fwd(&x, s)?;
            if b.upsamples() {
                concat = false;
            }
        }
        self.vocoder.fwd(&x, s, &f0_curve, seed)
    }
}

/// The loaded model.
pub(crate) struct Model {
    albert: Albert,
    bert_enc: Linear,
    text: TextEncoder,
    pred: Predictor,
    dec: Decoder,
    dev: Device,
}

impl Model {
    pub(crate) fn load(w: &Weights) -> R<Model> {
        Ok(Model {
            albert: Albert::load(w)?,
            bert_enc: Linear::load(w, "bert_encoder")?,
            text: TextEncoder::load(w)?,
            pred: Predictor::load(w)?,
            dec: Decoder::load(w)?,
            dev: Device::Cpu,
        })
    }

    pub(crate) fn device(&self) -> &Device {
        &self.dev
    }

    /// Phoneme ids (without the boundary tokens; at most 510) and a 256-value style → samples.
    pub(crate) fn forward(&self, ids: &[u32], style: &Tensor, speed: f64, seed: u64) -> R<Vec<f32>> {
        let mut padded = Vec::with_capacity(ids.len() + 2);
        padded.push(0u32);
        padded.extend_from_slice(ids);
        padded.push(0);
        let t = padded.len();
        let ids_t = Tensor::from_vec(padded, t, &self.dev).map_err(err)?;
        let s_dec = style.narrow(1, 0, 128).map_err(err)?;
        let s_pro = style.narrow(1, 128, 128).map_err(err)?;
        let bert = self.albert.fwd(&ids_t)?;
        let d_en = self.bert_enc.fwd(&bert)?.transpose(1, 2).map_err(err)?.contiguous().map_err(err)?; // [1, 512, T]
        let d = self.pred.encode(&d_en, &s_pro)?; // [1, T, 640]
        let dur = self.pred.durations(&d, speed)?;
        let frames: usize = dur.iter().sum();
        // alignment [T, F]: token i covers its run of frames
        let mut aln = vec![0f32; t * frames];
        let mut at = 0usize;
        for (i, n) in dur.iter().enumerate() {
            for k in at..at + n {
                if let Some(v) = aln.get_mut(i * frames + k) {
                    *v = 1.0;
                }
            }
            at += n;
        }
        let aln = Tensor::from_vec(aln, (1, t, frames), &self.dev).map_err(err)?;
        let en = d.transpose(1, 2).map_err(err)?.contiguous().map_err(err)?.matmul(&aln).map_err(err)?; // [1, 640, F]
        let (f0, n) = self.pred.f0_n(&en, &s_pro)?;
        let t_en = self.text.fwd(&ids_t)?; // [1, 512, T]
        let asr = t_en.matmul(&aln).map_err(err)?; // [1, 512, F]
        self.dec.fwd(&asr, &f0, &n, &s_dec, seed)
    }
}
