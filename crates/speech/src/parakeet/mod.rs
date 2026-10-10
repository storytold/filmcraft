//! NVIDIA Parakeet TDT speech recognition in pure Rust (candle, CPU), read from the model's
//! `.nemo` archive ([`crate::nemo`]).
//!
//! The model is a FastConformer encoder ([`encoder`]: 8× subsampling to 80 ms frames, 24
//! conformer blocks with relative-position attention) and a Token-and-Duration Transducer decoder
//! ([`decoder`]: LSTM prediction network, joint network predicting a token and how many frames it
//! covers), decoded greedily. The front end ([`features`]) is NeMo's normalised log-mel spectrogram.
//!
//! Transcription ([`Parakeet::recognise`], then [`Transcriber::transcribe`]):
//!
//! 1. **Pieces.** Clips up to [`MAX_PIECE_SECONDS`] are encoded in one pass (full attention, as
//!    the model was trained). Longer audio is cut in pauses ([`pieces`]): from the previous cut,
//!    the longest pause (≥ 150 ms below the clip's speech threshold, [`crate::vad`]) between
//!    [`MIN_PIECE_SECONDS`] and [`MAX_PIECE_SECONDS`] later becomes the next cut, the latest of
//!    equally long ones. Pieces cut in pauses do not overlap: no word can straddle the cut. Where
//!    nobody pauses for that long, the quietest 250 ms is cut and both neighbours are encoded
//!    with [`MARGIN_SECONDS`] of extra context; a word belongs to the piece whose cuts enclose
//!    its midpoint. Either way no word is lost or doubled at a join, and times stay exact: they
//!    are sample offsets from the piece's first sample.
//! 2. **Dead air.** Digital or noise-gated silence (10 ms frames below [`DEAD_AIR_DB`]) longer
//!    than 300 ms is shortened to 300 ms before encoding, and token times are mapped back to the
//!    clip. Long stretches of exact zeros otherwise throw the encoder off (whole sentences go
//!    missing, NeMo's own inference included); the decoder also restarts its prediction network
//!    after 2 s without a token ([`decoder::RESET_AFTER_FRAMES`]).
//! 3. **Words.** SentencePiece tokens starting with `▁` begin a word; punctuation joins its word.
//!    A word starts at its first token's frame and ends at its last token's frame plus that
//!    token's predicted duration (80 ms frames). Its confidence is the geometric mean of its
//!    token probabilities. English filler words (um, uh) are mostly kept (75 % of those in AMI
//!    meeting speech); German äh/ähm are in v3's vocabulary but were not seen in testing.
//! 4. Word bounds are tightened past silent frames ([`crate::vad`]), and optional speaker labelling
//!    runs afterwards ([`crate::diarize`]).
//!
//! Parakeet TDT 0.6B v3 recognises 25 European languages without being told which; v2 is English
//! only. The transcript's language is the one requested, else a guess from frequent words.

pub mod decoder;
pub mod encoder;
pub mod features;
mod ops;

use std::path::Path;

use candle_core::{Device, Tensor};
use candle_nn::LayerNorm;
use filmcraft_project::{Transcript, Word};

use crate::nemo::{Nemo, spm};
use crate::{Options, ProgressFn, SpeechError, Transcriber, sample_tick};
use decoder::{LstmLayer, Mat, Tdt, Token};
use encoder::{Attention, ConvModule, Encoder, FeedForward, Layer, Linear, Subsampling};
use features::Frontend;

/// Clips up to this long are encoded in one pass; longer audio is cut into pieces this long at most.
pub const MAX_PIECE_SECONDS: f64 = 60.0;
/// The shortest piece a cut leaves (except the last).
pub const MIN_PIECE_SECONDS: f64 = 15.0;
/// A pause is at least this many 10 ms frames below the speech threshold.
const MIN_PAUSE_FRAMES: usize = 15;
/// Context encoded on each side of a cut that is not in a pause.
pub const MARGIN_SECONDS: f64 = 3.0;
/// Dead air: 10 ms frames quieter than this (digital or noise-gated silence). Longer runs are
/// shortened before encoding (see the module docs).
pub const DEAD_AIR_DB: f32 = -80.0;

/// The languages of Parakeet TDT 0.6B v3 (model card).
pub const V3_LANGUAGES: &[&str] =
    &["bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt", "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk"];

fn merr(e: candle_core::Error) -> SpeechError {
    SpeechError::Model(e.to_string())
}

fn bad(msg: impl Into<String>) -> SpeechError {
    SpeechError::Model(msg.into())
}

/// Settings read from `model_config.yaml` (only the architecture implemented here is accepted).
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub n_fft: usize,
    pub win_length: usize,
    pub hop: usize,
    pub n_mels: usize,
    pub preemph: Option<f32>,
    pub log_guard: f32,
    pub d_model: usize,
    pub n_layers: usize,
    pub n_heads: usize,
    pub conv_channels: usize,
    pub conv_kernel: usize,
    pub ff_expansion: usize,
    pub xscaling: bool,
    pub pred_hidden: usize,
    pub pred_layers: usize,
    pub vocab_size: usize,
    pub joint_hidden: usize,
    pub durations: Vec<usize>,
    pub max_symbols: usize,
    /// Archive member holding the SentencePiece model.
    pub tokenizer: String,
}

impl Config {
    pub fn from_yaml(y: &crate::nemo::yaml::Yaml) -> Result<Self, SpeechError> {
        let unsupported = |what: &str| SpeechError::Unavailable(format!("this Parakeet model uses an unsupported setting ({what})"));
        let want = |path: &str, ok: &[&str], default: &str| -> Result<(), SpeechError> {
            let v = y.str(path).unwrap_or(default);
            if ok.contains(&v) { Ok(()) } else { Err(unsupported(&format!("{path}: {v}"))) }
        };
        let num = |path: &str, default: Option<i64>, max: i64| -> Result<usize, SpeechError> {
            let v = y.int(path).or(default).ok_or_else(|| bad(format!("model_config.yaml: {path} is missing")))?;
            if v <= 0 || v > max {
                return Err(bad(format!("model_config.yaml: {path} = {v} is out of range")));
            }
            Ok(v as usize)
        };
        if y.int("preprocessor.sample_rate").unwrap_or(16_000) != 16_000 {
            return Err(unsupported("sample rate"));
        }
        want("preprocessor.window", &["hann"], "hann")?;
        want("preprocessor.normalize", &["per_feature"], "per_feature")?;
        want("preprocessor.log_zero_guard_type", &["add"], "add")?;
        want("preprocessor.mel_norm", &["slaney"], "slaney")?;
        if y.bool("preprocessor.log") == Some(false) || y.int("preprocessor.frame_splicing").unwrap_or(1) != 1 || y.bool("preprocessor.exact_pad") == Some(true)
        {
            return Err(unsupported("preprocessor"));
        }
        if y.float("preprocessor.mag_power").unwrap_or(2.0) != 2.0
            || y.float("preprocessor.lowfreq").unwrap_or(0.0) != 0.0
            || y.float("preprocessor.highfreq").is_some_and(|f| f != 8000.0)
        {
            return Err(unsupported("spectrogram"));
        }
        let secs = |path: &str, d: f64| y.float(path).unwrap_or(d);
        let win_length = (secs("preprocessor.window_size", 0.025) * 16_000.0).round();
        let hop = (secs("preprocessor.window_stride", 0.01) * 16_000.0).round();
        if !(16.0..=4096.0).contains(&win_length) || !(1.0..=4096.0).contains(&hop) {
            return Err(unsupported("window"));
        }
        let (win_length, hop) = (win_length as usize, hop as usize);
        let n_fft = match y.int("preprocessor.n_fft") {
            Some(n) if n > 0 && n <= 1 << 16 && (n as usize).is_power_of_two() && n as usize >= win_length => n as usize,
            Some(_) => return Err(unsupported("n_fft")),
            None => win_length.next_power_of_two(),
        };
        // absent: NeMo's default 0.97; `null`: none
        let preemph = match y.raw("preprocessor.preemph") {
            None => Some(0.97),
            Some(_) => y.float("preprocessor.preemph").map(|v| v as f32),
        };
        let log_guard = y.float("preprocessor.log_zero_guard_value").unwrap_or(2f64.powi(-24)) as f32;
        want("encoder.subsampling", &["dw_striding"], "dw_striding")?;
        want("encoder.self_attention_model", &["rel_pos"], "rel_pos")?;
        want("encoder.conv_norm_type", &["batch_norm"], "batch_norm")?;
        want("encoder.att_context_style", &["regular"], "regular")?;
        if num("encoder.subsampling_factor", Some(8), 64)? != 8 || y.bool("encoder.causal_downsampling") == Some(true) {
            return Err(unsupported("subsampling"));
        }
        if y.int_list("encoder.att_context_size").is_some_and(|a| a.iter().any(|&v| v != -1)) {
            return Err(unsupported("limited attention context"));
        }
        if y.str("encoder.reduction").is_some() {
            return Err(unsupported("encoder reduction"));
        }
        want("joint.jointnet.activation", &["relu"], "relu")?;
        if y.bool("decoder.blank_as_pad") == Some(false) {
            return Err(unsupported("decoder without blank padding"));
        }
        let durations = y
            .int_list("model_defaults.tdt_durations")
            .or_else(|| y.int_list("decoding.durations"))
            .or_else(|| y.int_list("loss.tdt_kwargs.durations"))
            .ok_or_else(|| unsupported("not a TDT model (no durations)"))?;
        if durations.is_empty() || durations.len() > 64 || durations.iter().any(|&d| !(0..=64).contains(&d)) {
            return Err(unsupported("durations"));
        }
        let tokenizer = y.str("tokenizer.model_path").ok_or_else(|| bad("model_config.yaml: tokenizer.model_path is missing"))?;
        Ok(Self {
            n_fft,
            win_length,
            hop,
            n_mels: num("preprocessor.features", Some(80), 1024)?,
            preemph,
            log_guard,
            d_model: num("encoder.d_model", None, 8192)?,
            n_layers: num("encoder.n_layers", None, 256)?,
            n_heads: num("encoder.n_heads", None, 256)?,
            conv_channels: num("encoder.subsampling_conv_channels", Some(256), 8192)?,
            conv_kernel: match num("encoder.conv_kernel_size", Some(9), 255)? {
                k if k % 2 == 1 => k,
                _ => return Err(unsupported("even convolution kernel")),
            },
            ff_expansion: num("encoder.ff_expansion_factor", Some(4), 16)?,
            xscaling: y.bool("encoder.xscaling").unwrap_or(true),
            pred_hidden: num("decoder.prednet.pred_hidden", None, 8192)?,
            pred_layers: num("decoder.prednet.pred_rnn_layers", Some(1), 16)?,
            vocab_size: num("decoder.vocab_size", None, 1 << 20)?,
            joint_hidden: num("joint.jointnet.joint_hidden", None, 8192)?,
            durations: durations.iter().map(|&d| d as usize).collect(),
            max_symbols: num("decoding.greedy.max_symbols", Some(10), 1000)?,
            tokenizer: tokenizer.strip_prefix("nemo:").unwrap_or(tokenizer).to_string(),
        })
    }
}

/// A loaded Parakeet TDT model.
pub struct Parakeet {
    id: String,
    cfg: Config,
    frontend: Frontend,
    encoder: Encoder,
    /// joint `enc` projection
    enc_proj: Linear,
    tdt: Tdt,
    pieces: Vec<spm::Piece>,
    device: Device,
}

/// Reads tensors from the archive by name with shape checks.
struct Weights<'a> {
    nemo: &'a Nemo,
    file: std::fs::File,
    dev: Device,
}

impl Weights<'_> {
    fn vec(&mut self, name: &str, shape: &[usize]) -> Result<Vec<f32>, SpeechError> {
        let (s, v) = self.nemo.read_f32(&mut self.file, name)?;
        // size-1 dimensions may be squeezed (conv kernels stored as (out, in, 1))
        let squeeze = |s: &[usize]| s.iter().copied().filter(|&d| d != 1).collect::<Vec<_>>();
        if squeeze(&s) != squeeze(shape) {
            return Err(bad(format!("tensor {name} has shape {s:?}, expected {shape:?}")));
        }
        Ok(v)
    }
    fn has(&self, name: &str) -> bool {
        self.nemo.tensor(name).is_some()
    }
    fn t(&mut self, name: &str, shape: &[usize]) -> Result<Tensor, SpeechError> {
        let v = self.vec(name, shape)?;
        Tensor::from_vec(v, shape, &self.dev).map_err(merr)
    }
    fn opt(&mut self, name: &str, shape: &[usize]) -> Result<Option<Tensor>, SpeechError> {
        if self.has(name) { self.t(name, shape).map(Some) } else { Ok(None) }
    }
    fn linear(&mut self, p: &str, out: usize, inp: usize) -> Result<Linear, SpeechError> {
        let w = self.t(&format!("{p}.weight"), &[out, inp])?;
        let b = self.opt(&format!("{p}.bias"), &[out])?;
        Linear::new(w, b).map_err(merr)
    }
    fn norm(&mut self, p: &str, d: usize) -> Result<LayerNorm, SpeechError> {
        Ok(LayerNorm::new(self.t(&format!("{p}.weight"), &[d])?, self.t(&format!("{p}.bias"), &[d])?, 1e-5))
    }
    fn mat(&mut self, name: &str, rows: usize, cols: usize) -> Result<Mat, SpeechError> {
        let v = self.vec(name, &[rows, cols])?;
        Mat::new(rows, cols, v).ok_or_else(|| bad(format!("tensor {name}: bad size")))
    }
}

impl Parakeet {
    /// Load the `.nemo` archive in `dir` (the first `*.nemo` file) as catalogue model `id`.
    pub fn load(dir: &Path, id: &str) -> Result<Self, SpeechError> {
        let path = std::fs::read_dir(dir)
            .map_err(|e| bad(format!("{}: {e}", dir.display())))?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "nemo"))
            .ok_or_else(|| bad(format!("no .nemo file in {}", dir.display())))?;
        Self::load_file(&path, id)
    }

    /// Load a `.nemo` archive.
    pub fn load_file(path: &Path, id: &str) -> Result<Self, SpeechError> {
        let trace = Trace::new();
        let nemo = Nemo::open(path)?;
        trace.log("index archive");
        let cfg = Config::from_yaml(&nemo.config)?;
        let pieces = spm::pieces(&nemo.read_member(&cfg.tokenizer)?)?;
        if pieces.len() != cfg.vocab_size {
            return Err(bad(format!("the tokenizer has {} pieces, the model {}", pieces.len(), cfg.vocab_size)));
        }
        let dev = Device::Cpu;
        let mut w = Weights { file: nemo.file()?, nemo: &nemo, dev: dev.clone() };
        let c = &cfg;
        let d = c.d_model;
        if d % c.n_heads != 0 || d % 2 != 0 {
            return Err(bad("d_model is not divisible by the number of heads"));
        }
        // front end: the checkpoint's filterbank and window when present
        let bins = c.n_fft / 2 + 1;
        let filters = if w.has("preprocessor.featurizer.fb") {
            w.vec("preprocessor.featurizer.fb", &[1, c.n_mels, bins])?
        } else {
            features::mel_filters(16_000.0, c.n_fft, c.n_mels, 0.0, 8_000.0)
        };
        let window =
            if w.has("preprocessor.featurizer.window") { w.vec("preprocessor.featurizer.window", &[c.win_length])? } else { features::hann(c.win_length) };
        let frontend = Frontend::new(c.n_fft, c.hop, &window, filters, c.n_mels, c.preemph, c.log_guard).ok_or_else(|| bad("front-end settings"))?;
        // subsampling
        let ch = c.conv_channels;
        let freq = encoder::subsampled_len(c.n_mels);
        let p = "encoder.pre_encode";
        let conv0 = (w.t(&format!("{p}.conv.0.weight"), &[ch, 1, 3, 3])?, w.t(&format!("{p}.conv.0.bias"), &[ch])?);
        let mut stages = Vec::new();
        for (dw, pw) in [(2, 3), (5, 6)] {
            let dw_w = w.t(&format!("{p}.conv.{dw}.weight"), &[ch, 1, 3, 3])?;
            let dw_b = w.t(&format!("{p}.conv.{dw}.bias"), &[ch])?;
            let pw_w = w.t(&format!("{p}.conv.{pw}.weight"), &[ch, ch, 1, 1])?.reshape((ch, ch)).map_err(merr)?;
            let pw_b = match w.opt(&format!("{p}.conv.{pw}.bias"), &[ch])? {
                Some(b) => b.reshape((ch, 1)).map_err(merr)?,
                None => Tensor::zeros((ch, 1), candle_core::DType::F32, &dev).map_err(merr)?,
            };
            stages.push((dw_w, dw_b, pw_w, pw_b));
        }
        let sub = Subsampling { conv0, stages, out: w.linear(&format!("{p}.out"), d, ch * freq)?, channels: ch };
        // conformer layers
        let ff = c.ff_expansion * d;
        let dk = d / c.n_heads;
        let k = c.conv_kernel;
        let mut layers = Vec::with_capacity(c.n_layers);
        for i in 0..c.n_layers {
            let p = format!("encoder.layers.{i}");
            let ffn = |w: &mut Weights, n: &str| -> Result<FeedForward, SpeechError> {
                Ok(FeedForward {
                    norm: w.norm(&format!("{p}.norm_{n}"), d)?,
                    l1: w.linear(&format!("{p}.{n}.linear1"), ff, d)?,
                    l2: w.linear(&format!("{p}.{n}.linear2"), d, ff)?,
                })
            };
            let ff1 = ffn(&mut w, "feed_forward1")?;
            let a = format!("{p}.self_attn");
            let att = Attention {
                norm: w.norm(&format!("{p}.norm_self_att"), d)?,
                q: w.linear(&format!("{a}.linear_q"), d, d)?,
                k: w.linear(&format!("{a}.linear_k"), d, d)?,
                v: w.linear(&format!("{a}.linear_v"), d, d)?,
                out: w.linear(&format!("{a}.linear_out"), d, d)?,
                pos: w.linear(&format!("{a}.linear_pos"), d, d)?,
                bias_u: w.t(&format!("{a}.pos_bias_u"), &[c.n_heads, dk])?,
                bias_v: w.t(&format!("{a}.pos_bias_v"), &[c.n_heads, dk])?,
                heads: c.n_heads,
            };
            let cv = format!("{p}.conv");
            let pw1 = w.t(&format!("{cv}.pointwise_conv1.weight"), &[2 * d, d, 1])?.reshape((2 * d, d)).map_err(merr)?;
            let pw1 = Linear::new(pw1, w.opt(&format!("{cv}.pointwise_conv1.bias"), &[2 * d])?).map_err(merr)?;
            let pw2 = w.t(&format!("{cv}.pointwise_conv2.weight"), &[d, d, 1])?.reshape((d, d)).map_err(merr)?;
            let pw2 = Linear::new(pw2, w.opt(&format!("{cv}.pointwise_conv2.bias"), &[d])?).map_err(merr)?;
            // fold batch norm into the depthwise kernel
            let dw = w.vec(&format!("{cv}.depthwise_conv.weight"), &[d, 1, k])?;
            let dw_b = if w.has(&format!("{cv}.depthwise_conv.bias")) { w.vec(&format!("{cv}.depthwise_conv.bias"), &[d])? } else { vec![0.0; d] };
            let bn = |w: &mut Weights, n: &str| w.vec(&format!("{cv}.batch_norm.{n}"), &[d]);
            let (gamma, beta, mean, var) = (bn(&mut w, "weight")?, bn(&mut w, "bias")?, bn(&mut w, "running_mean")?, bn(&mut w, "running_var")?);
            let mut kt = vec![0f32; k * d];
            let mut kb = vec![0f32; d];
            for ci in 0..d {
                let s = gamma[ci] / (var[ci] + 1e-5).max(0.0).sqrt().max(1e-12);
                for j in 0..k {
                    kt[j * d + ci] = dw[ci * k + j] * s;
                }
                kb[ci] = (dw_b[ci] - mean[ci]) * s + beta[ci];
            }
            let conv = ConvModule { norm: w.norm(&format!("{p}.norm_conv"), d)?, pw1, dw: ops::DepthwiseSilu { kernel: kt, bias: kb, taps: k }, pw2 };
            let ff2 = ffn(&mut w, "feed_forward2")?;
            layers.push(Layer { ff1, att, conv, ff2, norm_out: w.norm(&format!("{p}.norm_out"), d)? });
        }
        let encoder = Encoder { sub, layers, d_model: d, xscale: c.xscaling.then(|| (d as f64).sqrt()) };
        // transducer
        let (h, j, v) = (c.pred_hidden, c.joint_hidden, c.vocab_size);
        let embed = w.t("decoder.prediction.embed.weight", &[v + 1, h])?;
        let mut lstm = Vec::new();
        for l in 0..c.pred_layers {
            let r = "decoder.prediction.dec_rnn.lstm";
            let b_ih = w.vec(&format!("{r}.bias_ih_l{l}"), &[4 * h])?;
            let b_hh = w.vec(&format!("{r}.bias_hh_l{l}"), &[4 * h])?;
            lstm.push(LstmLayer {
                w_ih: w.mat(&format!("{r}.weight_ih_l{l}"), 4 * h, h)?,
                w_hh: w.mat(&format!("{r}.weight_hh_l{l}"), 4 * h, h)?,
                b: b_ih.iter().zip(&b_hh).map(|(a, b)| a + b).collect(),
            });
        }
        // fold the embedding into the first layer: E · W_ih0ᵀ + b_ih0 + b_hh0, one row per token
        let (w_ih0, b0) = lstm.first().map(|l| (l.w_ih.w.clone(), l.b.clone())).ok_or_else(|| bad("the prediction network has no layers"))?;
        let w_ih0 = Tensor::from_vec(w_ih0, (4 * h, h), &dev).map_err(merr)?;
        let b0 = Tensor::from_vec(b0, 4 * h, &dev).map_err(merr)?;
        let table = embed
            .matmul(&w_ih0.t().map_err(merr)?)
            .and_then(|t| t.broadcast_add(&b0))
            .and_then(|t| t.flatten_all())
            .and_then(|t| t.to_vec1::<f32>())
            .map_err(merr)?;
        let embed_gates = Mat::new(v + 1, 4 * h, table).ok_or_else(|| bad("embedding table size"))?;
        let n_out = v + 1 + c.durations.len();
        let tdt = Tdt {
            embed_gates,
            lstm,
            hidden: h,
            pred: w.mat("joint.pred.weight", j, h)?,
            pred_b: w.vec("joint.pred.bias", &[j])?,
            out: w.mat("joint.joint_net.2.weight", n_out, j)?,
            out_b: w.vec("joint.joint_net.2.bias", &[n_out])?,
            blank: v,
            durations: c.durations.clone(),
            max_symbols: c.max_symbols,
            reset_after: Some(decoder::RESET_AFTER_FRAMES),
        };
        let enc_proj = w.linear("joint.enc", j, d)?;
        trace.log("weights");
        Ok(Self { id: id.to_string(), cfg, frontend, encoder, enc_proj, tdt, pieces, device: dev })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Whether the model is the multilingual v3 (English only otherwise).
    pub fn multilingual(&self) -> bool {
        self.cfg.vocab_size > 4096
    }

    /// Samples per encoder frame (80 ms).
    pub fn samples_per_frame(&self) -> usize {
        self.cfg.hop * 8
    }

    /// The normalised log-mel features of `audio`: `(frames, n_mels × frames)`.
    pub fn features(&self, audio: &[f32]) -> (usize, Vec<f32>) {
        self.frontend.features(audio)
    }

    /// The encoder output of `audio`: `(frames, frames × d_model)` (parity checks, tools).
    pub fn encode(&self, audio: &[f32]) -> Result<(usize, Vec<f32>), SpeechError> {
        let (frames, mel) = self.frontend.features(audio);
        if frames < 8 {
            return Ok((0, Vec::new()));
        }
        let enc = self.encoder.forward(&mel, self.cfg.n_mels, frames, &self.device, &mut |_, _| true).map_err(merr)?;
        let t = enc.dim(0).map_err(merr)?;
        Ok((t, enc.flatten_all().map_err(merr)?.to_vec1::<f32>().map_err(merr)?))
    }

    /// The text of token ids.
    pub fn detokenize(&self, ids: &[u32]) -> String {
        spm::decode(&self.pieces, ids)
    }

    /// The tokens of one piece of audio (frames relative to its first sample). `on_step(f)`
    /// reports progress through the piece (0..1) and returns `false` to cancel.
    pub fn tokens(&self, audio: &[f32], on_step: &mut dyn FnMut(f32) -> bool) -> Result<Vec<Token>, SpeechError> {
        let trace = Trace::new();
        let (frames, mel) = self.frontend.features(audio);
        if frames < 8 {
            return Ok(Vec::new());
        }
        trace.log("features");
        let enc = self
            .encoder
            .forward(&mel, self.cfg.n_mels, frames, &self.device, &mut |i, n| {
                if i == 1 {
                    trace.log("subsampling + layer 0");
                }
                on_step(0.95 * i as f32 / n.max(1) as f32)
            })
            .map_err(|e| if e.to_string().contains("cancelled") { SpeechError::Cancelled } else { merr(e) })?;
        trace.log("encoder");
        let t = enc.dim(0).map_err(merr)?;
        let proj = self.enc_proj.forward(&enc).map_err(merr)?.flatten_all().map_err(merr)?.to_vec1::<f32>().map_err(merr)?;
        let toks = self.tdt.decode(&proj, t, &mut || !on_step(0.97)).ok_or(SpeechError::Cancelled);
        trace.log(&format!("decode ({t} frames)"));
        toks
    }

    /// The words of `audio` with the model's own times (no tightening), long audio in pieces
    /// ([`pieces`]). `progress(f)` gets the fraction done and returns `false` to cancel.
    pub fn recognise(&self, audio: &[f32], progress: &mut dyn FnMut(f32) -> bool) -> Result<Vec<Word>, SpeechError> {
        let total = audio.len().max(1) as f32;
        let mut words: Vec<Word> = Vec::new();
        for p in pieces(audio) {
            let Some(piece) = audio.get(p.start..p.end) else { continue };
            let (base, span) = (p.keep.start as f32 / total, p.keep.len() as f32 / total);
            let (piece, spans) = compact(piece, DEAD_AIR_DB);
            let map = |n: usize| {
                let i = spans.partition_point(|s| s.1 <= n).saturating_sub(1);
                spans.get(i).map_or(n, |s| s.0 + (n - s.1).min(s.2)) + p.start
            };
            let mut cancelled = false;
            let toks = self.tokens(&piece, &mut |f| {
                let ok = progress(base + span * f);
                cancelled |= !ok;
                ok
            });
            let toks = match toks {
                Err(SpeechError::Cancelled) => return Err(SpeechError::Cancelled),
                other => other?,
            };
            if cancelled {
                return Err(SpeechError::Cancelled);
            }
            for (w, mid) in self.words(&toks, &map) {
                if p.keep.contains(&mid) || (mid >= audio.len() && p.keep.end == audio.len()) {
                    words.push(w);
                }
            }
        }
        Ok(words)
    }

    /// Words of `tokens` before any tightening, each with its midpoint in samples; `at` maps a
    /// sample position in the encoded piece to the clip's.
    pub fn words(&self, tokens: &[Token], at: &dyn Fn(usize) -> usize) -> Vec<(Word, usize)> {
        let spf = self.samples_per_frame();
        let mut groups: Vec<Vec<&Token>> = Vec::new();
        for t in tokens {
            let Some(p) = self.pieces.get(t.id as usize) else { continue };
            if !p.is_text() {
                continue;
            }
            if p.text.starts_with(spm::SPACE) || groups.is_empty() {
                groups.push(vec![t]);
            } else if let Some(g) = groups.last_mut() {
                g.push(t);
            }
        }
        let mut out: Vec<(Word, usize)> = Vec::new();
        let mut prefix = String::new();
        for g in groups {
            let ids: Vec<u32> = g.iter().map(|t| t.id).collect();
            let text = spm::decode(&self.pieces, &ids).trim().to_string();
            if text.is_empty() {
                continue;
            }
            let (Some(first), Some(last)) = (g.first(), g.last()) else { continue };
            // punctuation on its own: opening marks join the next word, the rest the previous one
            if !text.chars().any(char::is_alphanumeric) {
                if text.chars().all(|c| "¿¡«„“‚‘([{\"'".contains(c)) {
                    prefix.push_str(&text);
                } else if let Some((w, _)) = out.last_mut() {
                    w.text.push_str(&text);
                    w.end = w.end.max(sample_tick(at((last.frame + last.duration) * spf) as i64));
                }
                continue;
            }
            let a = at(first.frame * spf);
            let b = at((last.frame + last.duration) * spf).max(a + spf / 2);
            let conf = (g.iter().map(|t| f64::from(t.prob.max(1e-6)).ln()).sum::<f64>() / g.len() as f64).exp() as f32;
            let mut w = Word::new(format!("{prefix}{text}"), sample_tick(a as i64), sample_tick(b as i64));
            w.confidence = conf;
            prefix.clear();
            out.push((w, (a + b) / 2));
        }
        out
    }
}

/// Shorten runs of dead air (10 ms frames below `db` dBFS) longer than 300 ms to 300 ms. Returns the
/// shortened audio and its spans `(source start, compacted start, length)`.
fn compact(audio: &[f32], db: f32) -> (Vec<f32>, Vec<(usize, usize, usize)>) {
    const FRAME: usize = 160;
    const KEEP: usize = 15;
    let levels = crate::vad::frame_db(audio);
    let mut out = Vec::with_capacity(audio.len());
    let mut spans = Vec::new();
    let mut src = 0usize;
    let mut i = 0usize;
    while i < levels.len() {
        if levels[i] >= db {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < levels.len() && levels[i] < db {
            i += 1;
        }
        if i - run_start > 2 * KEEP {
            let cut_a = ((run_start + KEEP) * FRAME).min(audio.len());
            let cut_b = ((i - KEEP) * FRAME).min(audio.len());
            spans.push((src, out.len(), cut_a - src));
            out.extend_from_slice(&audio[src..cut_a]);
            src = cut_b;
        }
    }
    spans.push((src, out.len(), audio.len() - src));
    out.extend_from_slice(&audio[src..]);
    (out, spans)
}

/// Timing printed to stderr with `FILMCRAFT_SPEECH_TRACE` set.
struct Trace(Option<std::time::Instant>);

impl Trace {
    fn new() -> Self {
        Self(std::env::var_os("FILMCRAFT_SPEECH_TRACE").map(|_| std::time::Instant::now()))
    }
    fn log(&self, what: &str) {
        if let Some(t) = self.0 {
            eprintln!("parakeet: {what} {:.3}s", t.elapsed().as_secs_f64());
        }
    }
}

/// One piece of long audio: encoded over `start..end` (samples), owning the words whose midpoint
/// lies in `keep`.
#[derive(Clone, Debug, PartialEq)]
pub struct Piece {
    pub start: usize,
    pub end: usize,
    pub keep: std::ops::Range<usize>,
}

/// Split audio into pieces of at most [`MAX_PIECE_SECONDS`], cut in pauses (see the module docs).
pub fn pieces(audio: &[f32]) -> Vec<Piece> {
    let sr = crate::SAMPLE_RATE as usize;
    let frame = sr / 100;
    let (max_s, min_s) = (MAX_PIECE_SECONDS, MIN_PIECE_SECONDS);
    let whole = vec![Piece { start: 0, end: audio.len(), keep: 0..audio.len() }];
    if audio.len() as f64 <= max_s * sr as f64 {
        return whole;
    }
    let db = crate::vad::frame_db(audio);
    let th = crate::vad::threshold(&db);
    let total = db.len();
    // pauses: runs of quiet frames, as (centre frame, length)
    let mut pauses: Vec<(usize, usize)> = Vec::new();
    let mut run = 0usize;
    for (i, v) in db.iter().chain(std::iter::once(&f32::INFINITY)).enumerate() {
        if *v < th {
            run += 1;
        } else {
            if run >= MIN_PAUSE_FRAMES {
                pauses.push((i - run + run / 2, run));
            }
            run = 0;
        }
    }
    // 250 ms moving average of the levels, for cuts where nobody pauses
    let w = 25usize;
    let mut sm = vec![0f32; total];
    let mut acc = 0f32;
    for i in 0..total {
        acc += db[i];
        if i >= w {
            acc -= db[i - w];
        }
        sm[i] = acc / (i + 1).min(w) as f32;
    }
    let (lo, hi) = ((min_s * 100.0) as usize, (max_s * 100.0) as usize);
    // (cut frame, in a pause)
    let mut cuts: Vec<(usize, bool)> = Vec::new();
    let mut prev = 0usize;
    while total - prev > hi {
        let (a, b) = (prev + lo, prev + hi);
        let best = pauses.iter().filter(|(c, _)| (a..b).contains(c)).max_by_key(|(c, len)| ((*len).min(100), *c));
        let cut = match best {
            Some(&(c, _)) => (c, true),
            None => {
                let a = prev + hi * 3 / 4;
                let q = (a..b).min_by(|&x, &y| sm[x].total_cmp(&sm[y])).unwrap_or(b);
                (q.saturating_sub(w / 2).max(prev + 1), false)
            }
        };
        cuts.push(cut);
        prev = cut.0;
    }
    let margin = (MARGIN_SECONDS * sr as f64) as usize;
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut start = (0usize, true);
    for end in cuts.into_iter().map(|(c, p)| ((c * frame).min(audio.len()), p)).chain(std::iter::once((audio.len(), true))) {
        let a = if start.1 { start.0 } else { start.0.saturating_sub(margin) };
        let b = if end.1 { end.0 } else { (end.0 + margin).min(audio.len()) };
        out.push(Piece { start: a, end: b, keep: start.0..end.0 });
        start = end;
    }
    out
}

/// A guess at the language of a transcript from frequent function words (v3 languages only).
pub fn guess_language(text: &str) -> Option<&'static str> {
    const WORDS: &[(&str, &[&str])] = &[
        ("en", &["the", "and", "of", "to", "is", "that", "it", "you", "was", "for", "with", "this"]),
        ("de", &["der", "die", "und", "das", "ist", "nicht", "ich", "ein", "zu", "mit", "sie", "auch"]),
        ("fr", &["le", "la", "et", "les", "des", "est", "une", "que", "pas", "dans", "je", "pour"]),
        ("es", &["el", "la", "que", "y", "los", "en", "es", "por", "una", "con", "para", "las"]),
        ("it", &["il", "che", "di", "e", "la", "per", "non", "una", "sono", "con", "della", "gli"]),
        ("nl", &["de", "het", "een", "en", "van", "is", "niet", "dat", "ik", "op", "zijn", "met"]),
        ("pt", &["o", "que", "de", "não", "uma", "os", "do", "da", "em", "para", "com", "é"]),
        ("pl", &["i", "w", "nie", "się", "na", "że", "jest", "to", "z", "do", "jak", "ale"]),
        ("sv", &["och", "att", "det", "är", "som", "en", "på", "för", "med", "inte", "jag", "av"]),
        ("da", &["og", "at", "det", "er", "en", "til", "på", "som", "med", "ikke", "jeg", "af"]),
        ("cs", &["a", "je", "se", "na", "v", "že", "to", "ne", "jsem", "do", "jak", "ale"]),
        ("ru", &["и", "в", "не", "на", "что", "я", "с", "он", "это", "как", "но", "по"]),
        ("uk", &["і", "в", "не", "на", "що", "я", "з", "це", "як", "та", "але", "й"]),
        ("fi", &["ja", "on", "ei", "että", "se", "oli", "mutta", "kun", "niin", "hän", "kuin", "myös"]),
        ("hu", &["a", "az", "és", "hogy", "nem", "is", "egy", "van", "meg", "de", "csak", "már"]),
        ("ro", &["și", "în", "de", "nu", "la", "că", "o", "cu", "pe", "este", "un", "să"]),
        ("el", &["και", "το", "να", "η", "της", "είναι", "του", "με", "την", "δεν", "που", "για"]),
    ];
    let words: Vec<String> = text.split_whitespace().map(filmcraft_project::transcript::normalize_word).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return None;
    }
    let score = |list: &[&str]| words.iter().filter(|w| list.contains(&w.as_str())).count();
    let mut scores: Vec<(&'static str, usize)> = WORDS.iter().map(|(l, list)| (*l, score(list))).collect();
    scores.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    let (lang, best) = *scores.first()?;
    let second = scores.get(1).map_or(0, |s| s.1);
    // at least two hits, one in twenty words, and a clear winner
    (best >= 2 && best * 20 >= words.len().min(200) && best > second + second / 2).then_some(lang)
}

impl Transcriber for Parakeet {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        let language = opts.language.clone().filter(|l| !l.is_empty() && l != "auto");
        if let Some(l) = &language {
            let ok = if self.multilingual() { V3_LANGUAGES.contains(&l.as_str()) } else { l == "en" };
            if !ok {
                return Err(SpeechError::Model(format!("{} does not recognise the language `{l}`", self.id)));
            }
        }
        if !progress(0.0, "Analysing audio") {
            return Err(SpeechError::Cancelled);
        }
        let words = self.recognise(audio, &mut |f| progress(f * 0.95, &format!("Transcribing {:.0}%", f * 100.0)))?;
        let text: String = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        let language = language.unwrap_or_else(|| if self.multilingual() { guess_language(&text).unwrap_or("en").to_string() } else { "en".to_string() });
        let mut t = Transcript { language, source: self.id.clone(), speakers: Vec::new(), words };
        // a word ends at the next one's start at the latest
        t.normalize();
        crate::vad::tighten_words(audio, &mut t.words);
        if opts.diarize && !t.words.is_empty() {
            if !progress(0.96, "Labelling speakers") {
                return Err(SpeechError::Cancelled);
            }
            let p = crate::diarize::Params { max_speakers: opts.max_speakers, ..Default::default() };
            crate::diarize::diarize(audio, &mut t, &p);
        }
        progress(1.0, "Done");
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_reads_parakeet_yaml_and_refuses_other_architectures() {
        let yaml = "\
tokenizer:
  model_path: nemo:abc_tokenizer.model
preprocessor:
  window_size: 0.025
  window_stride: 0.01
  window: hann
  features: 128
  n_fft: 512
  normalize: per_feature
  dither: 1.0e-05
encoder:
  d_model: 1024
  n_layers: 24
  n_heads: 8
  subsampling: dw_striding
  subsampling_factor: 8
  subsampling_conv_channels: 256
  self_attention_model: rel_pos
  att_context_size:
  - -1
  - -1
  xscaling: false
  conv_kernel_size: 9
  conv_norm_type: batch_norm
decoder:
  blank_as_pad: true
  prednet:
    pred_hidden: 640
    pred_rnn_layers: 2
  vocab_size: 8192
joint:
  jointnet:
    joint_hidden: 640
    activation: relu
decoding:
  model_type: tdt
  durations:
  - 0
  - 1
  - 2
  - 3
  - 4
";
        let c = Config::from_yaml(&crate::nemo::yaml::Yaml::parse(yaml).unwrap()).unwrap();
        assert_eq!((c.n_fft, c.win_length, c.hop, c.n_mels), (512, 400, 160, 128));
        assert_eq!(c.durations, vec![0, 1, 2, 3, 4]);
        assert_eq!(c.tokenizer, "abc_tokenizer.model");
        assert_eq!(c.max_symbols, 10);
        assert!(!c.xscaling);
        for (from, to) in [
            ("rel_pos\n", "abs_pos\n"),
            ("  - -1\n  - -1", "  - 128\n  - 128"),
            ("hann", "hamming"),
            ("  - 4\n", "  - 4000\n"),
            ("d_model: 1024", "d_model: -3"),
        ] {
            let y = crate::nemo::yaml::Yaml::parse(&yaml.replacen(from, to, 1)).unwrap();
            assert!(Config::from_yaml(&y).is_err(), "{to}");
        }
    }

    #[test]
    fn long_audio_is_cut_at_pauses() {
        assert_eq!(pieces(&vec![0.1; 16_000 * 55]), vec![Piece { start: 0, end: 880_000, keep: 0..880_000 }]);
        // 200 s of "speech" (a tone with 120 ms dips to a noise floor every second) with 600 ms
        // pauses at 30, 47 and 101 s: the cut takes the latest of equally long pauses
        let mut seed = 1u32;
        let mut a: Vec<f32> = (0..16_000 * 200)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = 0.001 * ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5);
                if i % 16_000 < 1_920 { noise } else { 0.3 * (i as f32 * 0.05).sin() + noise }
            })
            .collect();
        for p in [30.0, 47.0, 101.0] {
            let s = (p * 16_000.0) as usize;
            a[s..s + 9_600].iter_mut().for_each(|v| *v *= 0.001);
        }
        let plan = pieces(&a);
        let secs = |n: usize| n as f64 / 16_000.0;
        assert!((secs(plan[0].keep.end) - 47.3).abs() < 0.1, "{plan:?}");
        assert!((secs(plan[1].keep.end) - 101.3).abs() < 0.1, "{plan:?}");
        // pause cuts carry no overlap; pieces tile the audio and stay short
        assert_eq!((plan[0].start, plan[0].end), (0, plan[0].keep.end));
        assert_eq!((plan[1].start, plan[1].end), (plan[1].keep.start, plan[1].keep.end));
        assert_eq!(plan.last().unwrap().keep.end, a.len());
        for w in plan.windows(2) {
            assert_eq!(w[0].keep.end, w[1].keep.start);
        }
        for p in &plan {
            assert!(secs(p.keep.len()) <= MAX_PIECE_SECONDS + 1e-9);
            assert!(p.start <= p.keep.start && p.keep.end <= p.end && secs(p.end - p.start) <= MAX_PIECE_SECONDS + 2.0 * MARGIN_SECONDS + 1e-9);
        }
        // where nobody pauses, the cut overlaps its neighbours by the margin
        assert!(plan.iter().any(|p| p.start < p.keep.start), "{plan:?}");
    }

    #[test]
    fn dead_air_is_shortened_and_times_map_back() {
        // 1 s tone, 2 s digital silence, 1 s tone
        let tone = |n: usize| (0..n).map(|i| 0.3 * (i as f32 * 0.07).sin()).collect::<Vec<f32>>();
        let mut a = tone(16_000);
        a.extend(vec![0.0; 32_000]);
        a.extend(tone(16_000));
        let (c, spans) = compact(&a, DEAD_AIR_DB);
        assert_eq!(c.len(), 16_000 + 4_800 + 16_000);
        assert_eq!(spans.len(), 2);
        // a sample of the second tone maps back to its place in the source
        let back = |n: usize| {
            let i = spans.partition_point(|s| s.1 <= n).saturating_sub(1);
            spans[i].0 + (n - spans[i].1)
        };
        assert_eq!(back(16_000 + 4_800 + 100), 16_000 + 32_000 + 100);
        assert_eq!(&c[16_000 + 4_800..], &a[48_000..]);
        assert_eq!(back(500), 500);
        // nothing to shorten: unchanged
        let (c, spans) = compact(&tone(8_000), DEAD_AIR_DB);
        assert_eq!((c.len(), spans), (8_000, vec![(0, 0, 8_000)]));
    }

    #[test]
    fn guesses_languages() {
        assert_eq!(guess_language("This is the house that Jack built and it was nice"), Some("en"));
        assert_eq!(guess_language("Ich weiß nicht, ob das die richtige Antwort ist, und sie auch nicht."), Some("de"));
        assert_eq!(guess_language(""), None);
        // too little to tell ("to" is English and Czech)
        assert_eq!(guess_language("anger, pain. Painful to hear."), None);
    }
}
