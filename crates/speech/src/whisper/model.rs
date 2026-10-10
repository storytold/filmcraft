//! The Whisper network (encoder–decoder transformer), written from the published architecture
//! (Radford et al. 2022, "Robust Speech Recognition via Large-Scale Weak Supervision", and OpenAI's
//! MIT-licensed model description), reading the tensor names of the Hugging Face safetensors
//! release, on the CPU kernels of [`crate::nn`].
//!
//! Encoder: two 1-D convolutions (k = 3, the second with stride 2) with GELU, a position table,
//! pre-LayerNorm self-attention blocks, final LayerNorm. Decoder: token and position embeddings,
//! pre-LayerNorm blocks of causal self-attention, cross-attention to the audio and an MLP, final
//! LayerNorm, logits tied to the token embedding.
//!
//! Decoding runs any number of independent windows ("rows") in lockstep: each row keeps its own
//! self-attention cache and its audio's cross-attention keys/values (projected once per window),
//! and every weight matrix is read from memory once per step for all rows.

use std::path::Path;

use crate::SpeechError;
use crate::ct2::Ct2;
use crate::nn::{self, Linear, Norm, Store};
use crate::safetensors::{Raw, SafeTensors};

type Result<T> = std::result::Result<T, SpeechError>;

/// Mel frames per window (30 s).
pub const N_FRAMES: usize = 3000;
/// Encoder positions per window.
pub const N_CTX: usize = N_FRAMES / 2;

/// `config.json` fields we use.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    pub num_mel_bins: usize,
    pub d_model: usize,
    pub encoder_layers: usize,
    pub encoder_attention_heads: usize,
    pub decoder_layers: usize,
    pub decoder_attention_heads: usize,
    pub max_source_positions: usize,
    pub max_target_positions: usize,
    pub vocab_size: usize,
}

impl Config {
    /// Reject configurations this implementation cannot run (or that would allocate absurdly).
    pub fn validate(&self) -> Result<()> {
        let bad = |m: &str| Err(SpeechError::Model(format!("config.json: {m}")));
        if !(1..=512).contains(&self.num_mel_bins) {
            return bad("num_mel_bins out of range");
        }
        if !(1..=8192).contains(&self.d_model) || !(1..=128).contains(&self.encoder_layers) || !(1..=128).contains(&self.decoder_layers) {
            return bad("model size out of range");
        }
        for h in [self.encoder_attention_heads, self.decoder_attention_heads] {
            if h == 0 || h > 256 || !self.d_model.is_multiple_of(h) {
                return bad("attention heads must divide d_model");
            }
        }
        if self.max_source_positions < N_CTX || self.max_source_positions > 100_000 {
            return bad("max_source_positions must be at least 1500");
        }
        if !(8..=100_000).contains(&self.max_target_positions) || !(1..=1_000_000).contains(&self.vocab_size) {
            return bad("max_target_positions / vocab_size out of range");
        }
        Ok(())
    }
}

struct EncLayer {
    ln1: Norm,
    qkv: Linear,
    out: Linear,
    ln2: Norm,
    fc1: Linear,
    fc2: Linear,
}

struct DecLayer {
    ln1: Norm,
    qkv: Linear,
    out: Linear,
    ln2: Norm,
    xq: Linear,
    /// cross-attention key and value projections stacked (k has no bias)
    xkv: Linear,
    xout: Linear,
    ln3: Norm,
    fc1: Linear,
    fc2: Linear,
}

pub struct Model {
    pub cfg: Config,
    /// conv1 as a product: (d, 3·n_mels), columns ordered (tap, mel)
    conv1: Linear,
    /// conv2: (d, 3·d), columns ordered (tap, channel)
    conv2: Linear,
    enc_pos: Vec<f32>,
    enc: Vec<EncLayer>,
    enc_ln: Norm,
    /// token embedding (vocab, d), also the output projection
    emb: Linear,
    dec_pos: Vec<f32>,
    dec: Vec<DecLayer>,
    dec_ln: Norm,
}

/// Per-window decoding state: the cross-attention keys/values of the window's audio and the
/// self-attention cache.
pub struct Row {
    /// per layer: (N_CTX, 2d) = [k | v] per audio position
    cross: Vec<Vec<f32>>,
    /// per layer: (max positions, 2d) = [k | v] per token position
    cache: Vec<Vec<f32>>,
}

/// Reusable buffers of the encoder and decoder.
#[derive(Default)]
pub struct Scratch {
    a: Vec<f32>,
    b: Vec<f32>,
    c: Vec<f32>,
    x: Vec<f32>,
    h: Vec<f32>,
    scores: Vec<f32>,
}

/// Model weights: a Hugging Face `model.safetensors` or a CTranslate2 `model.bin`.
pub enum Weights {
    Safetensors(SafeTensors),
    Ct2(Ct2),
}

impl Weights {
    pub fn open(dir: &Path) -> Result<Self> {
        let st = dir.join("model.safetensors");
        if st.is_file() {
            return Ok(Weights::Safetensors(SafeTensors::open(&st)?));
        }
        let bin = dir.join("model.bin");
        if bin.is_file() {
            return Ok(Weights::Ct2(Ct2::open(&bin)?));
        }
        Err(SpeechError::Model(format!("{}: no model.safetensors or model.bin", dir.display())))
    }

    /// Tensor `name` (Hugging Face naming, without the `model.` prefix) of shape `shape`, as
    /// stored (half precision stays half precision).
    fn read_raw(&mut self, name: &str, shape: &[usize], d: usize) -> Result<Raw> {
        match self {
            Weights::Safetensors(st) => {
                let pre = if st.get("model.encoder.conv1.weight").is_some() { "model." } else { "" };
                st.read_raw(&format!("{pre}{name}"), shape)
            }
            Weights::Ct2(m) => {
                let (var, rows) = ct2_name(name, d).ok_or_else(|| SpeechError::Model(format!("CTranslate2 model: no counterpart for {name}")))?;
                let rows = rows.unwrap_or(0..shape.first().copied().unwrap_or(1));
                if Some(rows.len()) != shape.first().copied() {
                    return Err(SpeechError::Model(format!("CTranslate2 model: {name}: shape {shape:?}")));
                }
                m.read_rows_raw(&var, rows, shape.get(1..).unwrap_or_default())
            }
        }
    }

    /// The configuration of a CTranslate2 Whisper model, from its tensor shapes and options.
    pub fn ct2_config(&mut self) -> Result<Config> {
        let Weights::Ct2(m) = self else { return Err(SpeechError::Model("not a CTranslate2 model".into())) };
        let bad = |w: &str| SpeechError::Model(format!("CTranslate2 model: {w}"));
        if !m.spec.contains("Whisper") {
            return Err(bad(&format!("a {} model, not Whisper", m.spec)));
        }
        if m.scalar("decoder/activation").is_some_and(|a| a != 3) || m.scalar("decoder/alibi").is_some_and(|a| a != 0) {
            return Err(bad("unexpected decoder options"));
        }
        let shape = |m: &Ct2, n: &str| m.vars.get(n).map(|v| v.shape.clone()).ok_or_else(|| bad(&format!("missing {n}")));
        let emb = shape(m, "decoder/embeddings/weight")?;
        let conv = shape(m, "encoder/conv1/weight")?;
        let (Some(&vocab), Some(&d), Some(&n_mels)) = (emb.first(), emb.get(1), conv.get(1)) else { return Err(bad("embedding shapes")) };
        let layers = |m: &Ct2, side: &str| (0..).take_while(|i| m.vars.contains_key(&format!("{side}/layer_{i}/ffn/linear_0/weight"))).count();
        let heads = |m: &mut Ct2, side: &str| m.scalar(&format!("{side}/num_heads")).and_then(|h| usize::try_from(h).ok()).unwrap_or(d / 64);
        let cfg = Config {
            num_mel_bins: n_mels,
            d_model: d,
            encoder_layers: layers(m, "encoder"),
            decoder_layers: layers(m, "decoder"),
            encoder_attention_heads: heads(m, "encoder"),
            decoder_attention_heads: heads(m, "decoder"),
            max_source_positions: shape(m, "encoder/position_encodings/encodings")?.first().copied().unwrap_or(0),
            max_target_positions: shape(m, "decoder/position_encodings/encodings")?.first().copied().unwrap_or(0),
            vocab_size: vocab,
        };
        cfg.validate()?;
        Ok(cfg)
    }
}

/// The CTranslate2 variable (and its rows, for the fused projections) that holds the Hugging Face
/// tensor `name`.
fn ct2_name(name: &str, d: usize) -> Option<(String, Option<std::ops::Range<usize>>)> {
    let whole = |s: &str| Some((s.to_string(), None));
    match name {
        "encoder.embed_positions.weight" => return whole("encoder/position_encodings/encodings"),
        "decoder.embed_positions.weight" => return whole("decoder/position_encodings/encodings"),
        "decoder.embed_tokens.weight" => return whole("decoder/embeddings/weight"),
        _ => {}
    }
    let parts: Vec<&str> = name.split('.').collect();
    let norm = |p: &str| if p == "weight" { "gamma" } else { "beta" };
    match parts.as_slice() {
        [side, conv @ ("conv1" | "conv2"), param] => Some((format!("{side}/{conv}/{param}"), None)),
        [side, "layer_norm", param] => Some((format!("{side}/layer_norm/{}", norm(param)), None)),
        [side, "layers", i, rest @ ..] => {
            let (sub, rows): (String, Option<std::ops::Range<usize>>) = match rest {
                ["self_attn", proj, param] => {
                    let rows = match *proj {
                        "q_proj" => Some(0..d),
                        "k_proj" => Some(d..2 * d),
                        "v_proj" => Some(2 * d..3 * d),
                        _ => None,
                    };
                    let lin = if *proj == "out_proj" { "linear_1" } else { "linear_0" };
                    (format!("self_attention/{lin}/{param}"), rows)
                }
                ["encoder_attn", proj, param] => {
                    let (lin, rows) = match *proj {
                        "q_proj" => ("linear_0", None),
                        "k_proj" => ("linear_1", Some(0..d)),
                        "v_proj" => ("linear_1", Some(d..2 * d)),
                        _ => ("linear_2", None),
                    };
                    (format!("attention/{lin}/{param}"), rows)
                }
                ["self_attn_layer_norm", param] => (format!("self_attention/layer_norm/{}", norm(param)), None),
                ["encoder_attn_layer_norm", param] => (format!("attention/layer_norm/{}", norm(param)), None),
                ["final_layer_norm", param] => (format!("ffn/layer_norm/{}", norm(param)), None),
                ["fc1", param] => (format!("ffn/linear_0/{param}"), None),
                ["fc2", param] => (format!("ffn/linear_1/{param}"), None),
                _ => return None,
            };
            Some((format!("{side}/layer_{i}/{sub}"), rows))
        }
        _ => None,
    }
}

impl Model {
    pub fn load(cfg: Config, w: &mut Weights) -> Result<Self> {
        cfg.validate()?;
        let d = cfg.d_model;
        // the encoder's products run on f32; the decoder's weights stay as stored (see `Store`)
        let mut t = |name: &str, shape: &[usize]| w.read_raw(name, shape, d);
        let f32s = |r: Raw| -> Vec<f32> {
            match r {
                Raw::F32(v) => v,
                Raw::F16(h) => h.into_iter().map(nn::h2f).collect(),
            }
        };
        let norm = |t: &mut dyn FnMut(&str, &[usize]) -> Result<Raw>, p: &str| -> Result<Norm> {
            Ok(Norm { w: f32s(t(&format!("{p}.weight"), &[d])?), b: f32s(t(&format!("{p}.bias"), &[d])?) })
        };
        let lin = |t: &mut dyn FnMut(&str, &[usize]) -> Result<Raw>, p: &str, n: usize, k: usize, bias: bool, half: bool| -> Result<Linear> {
            let w = match t(&format!("{p}.weight"), &[n, k])? {
                Raw::F16(h) if half => Store::F16(h),
                r => Store::F32(f32s(r)),
            };
            let b = if bias { Some(f32s(t(&format!("{p}.bias"), &[n])?)) } else { None };
            Linear::with_store(w, b, n, k)
        };
        let attn = |t: &mut dyn FnMut(&str, &[usize]) -> Result<Raw>, p: &str, half: bool| -> Result<(Linear, Linear, Linear, Linear)> {
            Ok((
                lin(t, &format!("{p}.q_proj"), d, d, true, half)?,
                lin(t, &format!("{p}.k_proj"), d, d, false, half)?,
                lin(t, &format!("{p}.v_proj"), d, d, true, half)?,
                lin(t, &format!("{p}.out_proj"), d, d, true, half)?,
            ))
        };
        let nm = cfg.num_mel_bins;
        let conv = |t: &mut dyn FnMut(&str, &[usize]) -> Result<Raw>, p: &str, cin: usize| -> Result<Linear> {
            let w = f32s(t(&format!("{p}.weight"), &[d, cin, 3])?);
            // (out, in, tap) → (out, tap, in)
            let mut wp = vec![0f32; w.len()];
            for o in 0..d {
                for c in 0..cin {
                    for k in 0..3 {
                        wp[o * 3 * cin + k * cin + c] = w[o * cin * 3 + c * 3 + k];
                    }
                }
            }
            Linear::new(wp, Some(f32s(t(&format!("{p}.bias"), &[d])?)), d, 3 * cin)
        };
        let conv1 = conv(&mut t, "encoder.conv1", nm)?;
        let conv2 = conv(&mut t, "encoder.conv2", d)?;
        let enc_pos_all = f32s(t("encoder.embed_positions.weight", &[cfg.max_source_positions, d])?);
        let enc_pos = enc_pos_all.get(..N_CTX * d).map(<[f32]>::to_vec).unwrap_or_default();
        let mut enc = Vec::with_capacity(cfg.encoder_layers);
        for i in 0..cfg.encoder_layers {
            let p = format!("encoder.layers.{i}");
            let (q, k, v, out) = attn(&mut t, &format!("{p}.self_attn"), false)?;
            enc.push(EncLayer {
                ln1: norm(&mut t, &format!("{p}.self_attn_layer_norm"))?,
                qkv: Linear::stack(vec![q, k, v])?,
                out,
                ln2: norm(&mut t, &format!("{p}.final_layer_norm"))?,
                fc1: lin(&mut t, &format!("{p}.fc1"), 4 * d, d, true, false)?,
                fc2: lin(&mut t, &format!("{p}.fc2"), d, 4 * d, true, false)?,
            });
        }
        let enc_ln = norm(&mut t, "encoder.layer_norm")?;
        let emb = match t("decoder.embed_tokens.weight", &[cfg.vocab_size, d])? {
            Raw::F16(h) => Linear::with_store(Store::F16(h), None, cfg.vocab_size, d)?,
            Raw::F32(v) => Linear::new(v, None, cfg.vocab_size, d)?,
        };
        let dec_pos = f32s(t("decoder.embed_positions.weight", &[cfg.max_target_positions, d])?);
        let mut dec = Vec::with_capacity(cfg.decoder_layers);
        for i in 0..cfg.decoder_layers {
            let p = format!("decoder.layers.{i}");
            let (q, k, v, out) = attn(&mut t, &format!("{p}.self_attn"), true)?;
            let (xq, xk, xv, xout) = attn(&mut t, &format!("{p}.encoder_attn"), true)?;
            dec.push(DecLayer {
                ln1: norm(&mut t, &format!("{p}.self_attn_layer_norm"))?,
                qkv: Linear::stack(vec![q, k, v])?,
                out,
                ln2: norm(&mut t, &format!("{p}.encoder_attn_layer_norm"))?,
                xq,
                xkv: Linear::stack(vec![xk, xv])?,
                xout,
                ln3: norm(&mut t, &format!("{p}.final_layer_norm"))?,
                fc1: lin(&mut t, &format!("{p}.fc1"), 4 * d, d, true, true)?,
                fc2: lin(&mut t, &format!("{p}.fc2"), d, 4 * d, true, true)?,
            });
        }
        let dec_ln = norm(&mut t, "decoder.layer_norm")?;
        Ok(Self { cfg, conv1, conv2, enc_pos, enc, enc_ln, emb, dec_pos, dec, dec_ln })
    }

    fn d(&self) -> usize {
        self.cfg.d_model
    }

    /// Bytes of cross-attention state one decoding row needs (for sizing batches).
    pub fn row_bytes(&self) -> usize {
        self.cfg.decoder_layers * (N_CTX + self.cfg.max_target_positions.min(256)) * 2 * self.d() * 4
    }

    /// Encode one window: `mel` is `n_mels × N_FRAMES` (row-major by mel band). Returns the audio
    /// features, `N_CTX × d`.
    pub fn encode(&self, mel: &[f32], s: &mut Scratch) -> Result<Vec<f32>> {
        let (d, nm) = (self.d(), self.cfg.num_mel_bins);
        if mel.len() != nm * N_FRAMES {
            return Err(SpeechError::Model("mel window size".into()));
        }
        // conv1 (padding 1): im2col rows [x(t-1) | x(t) | x(t+1)], x(t) = the mel column at t
        s.a.clear();
        s.a.resize(N_FRAMES * 3 * nm, 0.0);
        {
            use rayon::prelude::*;
            s.a.par_chunks_mut(3 * nm).enumerate().for_each(|(t, row)| {
                for k in 0..3 {
                    let Some(src) = (t + k).checked_sub(1).filter(|&u| u < N_FRAMES) else { continue };
                    for c in 0..nm {
                        row[k * nm + c] = mel[c * N_FRAMES + src];
                    }
                }
            });
        }
        nn::linear_gelu(&mut s.b, &s.a, N_FRAMES, &self.conv1)?;
        // conv2 (stride 2, padding 1): rows [y(2t-1) | y(2t) | y(2t+1)]
        s.a.clear();
        s.a.resize(N_CTX * 3 * d, 0.0);
        {
            use rayon::prelude::*;
            let y = &s.b;
            s.a.par_chunks_mut(3 * d).enumerate().for_each(|(t, row)| {
                for k in 0..3 {
                    let Some(src) = (2 * t + k).checked_sub(1).filter(|&u| u < N_FRAMES) else { continue };
                    row[k * d..(k + 1) * d].copy_from_slice(&y[src * d..(src + 1) * d]);
                }
            });
        }
        nn::linear_gelu(&mut s.x, &s.a, N_CTX, &self.conv2)?;
        nn::add(&mut s.x, &self.enc_pos);
        let heads = self.cfg.encoder_attention_heads;
        let dh = d / heads;
        let scale = (dh as f32).powf(-0.5);
        for l in &self.enc {
            nn::layer_norm(&mut s.h, &s.x, &l.ln1);
            nn::linear(&mut s.a, &s.h, N_CTX, &l.qkv)?;
            s.c.resize(N_CTX * d, 0.0);
            let qkv = &s.a;
            nn::attend_many(&mut s.c, d, qkv, 3 * d, &qkv[d..], 3 * d, &qkv[2 * d..], 3 * d, N_CTX, N_CTX, heads, dh, scale, false, &mut s.scores)?;
            nn::linear_add(&mut s.x, &s.c, N_CTX, &l.out)?;
            nn::layer_norm(&mut s.h, &s.x, &l.ln2);
            nn::linear_gelu(&mut s.a, &s.h, N_CTX, &l.fc1)?;
            nn::linear_add(&mut s.x, &s.a, N_CTX, &l.fc2)?;
        }
        let mut out = Vec::new();
        nn::layer_norm(&mut out, &s.x, &self.enc_ln);
        Ok(out)
    }

    /// A decoding row for the audio features `xa` (`N_CTX × d`): the cross-attention keys/values
    /// of every decoder layer, projected once.
    pub fn row(&self, xa: &[f32]) -> Result<Row> {
        let mut cross = Vec::with_capacity(self.dec.len());
        for l in &self.dec {
            let mut kv = Vec::new();
            nn::linear(&mut kv, xa, N_CTX, &l.xkv)?;
            cross.push(kv);
        }
        Ok(Row { cross, cache: vec![Vec::new(); self.dec.len()] })
    }

    fn embed(&self, x: &mut Vec<f32>, tokens: &[u32], pos0: usize, same_pos: bool) -> Result<()> {
        let d = self.d();
        x.clear();
        let mut buf = Vec::new();
        for (i, &t) in tokens.iter().enumerate() {
            let p = if same_pos { pos0 } else { pos0 + i };
            let e = self.emb.w.slice(t as usize * d..(t as usize + 1) * d, &mut buf).ok_or_else(|| SpeechError::Model(format!("token {t} out of range")))?;
            let pe = self.dec_pos.get(p * d..(p + 1) * d).ok_or_else(|| SpeechError::Model("decoder position out of range".into()))?;
            x.extend(e.iter().zip(pe).map(|(a, b)| a + b));
        }
        Ok(())
    }

    /// One decoding step for `rows` (all at position `pos`): feed `tokens[i]` to row `i`, append
    /// to the rows' caches, and (with `logits`) return the next-token logits, `rows × vocab`.
    pub fn step(&self, rows: &mut [&mut Row], tokens: &[u32], pos: usize, logits: bool, s: &mut Scratch) -> Result<Option<Vec<f32>>> {
        let (d, m) = (self.d(), rows.len());
        if tokens.len() != m {
            return Err(SpeechError::Model("decoder batch size".into()));
        }
        let heads = self.cfg.decoder_attention_heads;
        let dh = d / heads;
        let scale = (dh as f32).powf(-0.5);
        let max_pos = self.cfg.max_target_positions;
        if pos >= max_pos {
            return Err(SpeechError::Model("decoder sequence too long".into()));
        }
        self.embed(&mut s.x, tokens, pos, true)?;
        for (li, l) in self.dec.iter().enumerate() {
            nn::layer_norm(&mut s.h, &s.x, &l.ln1);
            s.a.resize(m * 3 * d, 0.0);
            nn::linear_small(&mut s.a, &s.h, m, &l.qkv, false)?;
            for (i, r) in rows.iter_mut().enumerate() {
                let c = r.cache.get_mut(li).ok_or_else(|| SpeechError::Model("decoder cache".into()))?;
                if c.len() < (pos + 1) * 2 * d {
                    c.resize((pos + 1) * 2 * d, 0.0);
                }
                c[pos * 2 * d..(pos + 1) * 2 * d].copy_from_slice(&s.a[i * 3 * d + d..(i + 1) * 3 * d]);
            }
            s.c.resize(m * d, 0.0);
            {
                let kv: Vec<&[f32]> = rows.iter().map(|r| r.cache[li].as_slice()).collect();
                nn::attend_one(&mut s.c, &s.a, 3 * d, &kv, 2 * d, 0, d, pos + 1, heads, dh, scale)?;
            }
            nn::linear_small(&mut s.x, &s.c, m, &l.out, true)?;
            nn::layer_norm(&mut s.h, &s.x, &l.ln2);
            s.a.resize(m * d, 0.0);
            nn::linear_small(&mut s.a, &s.h, m, &l.xq, false)?;
            {
                let kv: Vec<&[f32]> = rows.iter().map(|r| r.cross[li].as_slice()).collect();
                nn::attend_one(&mut s.c, &s.a, d, &kv, 2 * d, 0, d, N_CTX, heads, dh, scale)?;
            }
            nn::linear_small(&mut s.x, &s.c, m, &l.xout, true)?;
            nn::layer_norm(&mut s.h, &s.x, &l.ln3);
            s.a.resize(m * 4 * d, 0.0);
            nn::linear_small(&mut s.a, &s.h, m, &l.fc1, false)?;
            nn::gelu_all(&mut s.a);
            nn::linear_small(&mut s.x, &s.a, m, &l.fc2, true)?;
        }
        if !logits {
            return Ok(None);
        }
        nn::layer_norm(&mut s.h, &s.x, &self.dec_ln);
        let mut out = vec![0f32; m * self.cfg.vocab_size];
        nn::linear_small(&mut out, &s.h, m, &self.emb, false)?;
        Ok(Some(out))
    }

    /// Word alignment pass: run `tokens` (positions 0..) through the decoder against `row`'s audio
    /// and return the scaled cross-attention logits (`tokens × N_CTX`) of the given (layer, head)
    /// pairs, in order. Layers after the last one needed are skipped, and so are the logits.
    pub fn cross_logits(&self, row: &Row, tokens: &[u32], heads_wanted: &[(usize, usize)], s: &mut Scratch) -> Result<Vec<Vec<f32>>> {
        let (d, t) = (self.d(), tokens.len());
        let heads = self.cfg.decoder_attention_heads;
        let dh = d / heads;
        let scale = (dh as f32).powf(-0.5);
        if t > self.cfg.max_target_positions {
            return Err(SpeechError::Model("decoder sequence too long".into()));
        }
        let Some(last) = heads_wanted.iter().map(|x| x.0).max() else { return Ok(Vec::new()) };
        let mut out: Vec<Option<Vec<f32>>> = vec![None; heads_wanted.len()];
        self.embed(&mut s.x, tokens, 0, false)?;
        for (li, l) in self.dec.iter().enumerate().take(last + 1) {
            nn::layer_norm(&mut s.h, &s.x, &l.ln1);
            nn::linear(&mut s.a, &s.h, t, &l.qkv)?;
            s.c.resize(t * d, 0.0);
            let qkv = &s.a;
            nn::attend_many(&mut s.c, d, qkv, 3 * d, &qkv[d..], 3 * d, &qkv[2 * d..], 3 * d, t, t, heads, dh, scale, true, &mut s.scores)?;
            nn::linear_add(&mut s.x, &s.c, t, &l.out)?;
            nn::layer_norm(&mut s.h, &s.x, &l.ln2);
            nn::linear(&mut s.a, &s.h, t, &l.xq)?;
            let cross = &row.cross[li];
            for (slot, &(hl, hh)) in out.iter_mut().zip(heads_wanted) {
                if hl == li && hh < heads {
                    *slot = Some(nn::head_logits(&s.a, d, cross, 2 * d, t, N_CTX, hh, dh, scale)?);
                }
            }
            if li == last {
                break;
            }
            nn::attend_many(&mut s.c, d, &s.a, d, cross, 2 * d, &cross[d..], 2 * d, t, N_CTX, heads, dh, scale, false, &mut s.scores)?;
            nn::linear_add(&mut s.x, &s.c, t, &l.xout)?;
            nn::layer_norm(&mut s.h, &s.x, &l.ln3);
            nn::linear_gelu(&mut s.b, &s.h, t, &l.fc1)?;
            nn::linear_add(&mut s.x, &s.b, t, &l.fc2)?;
        }
        Ok(out.into_iter().flatten().collect())
    }
}
