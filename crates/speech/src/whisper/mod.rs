//! Whisper speech recognition in pure Rust (candle, CPU).
//!
//! Transcription follows the procedure described with the model (sequential 30-second windows):
//!
//! 1. The whole clip's log-mel spectrogram is computed once ([`crate::mel`]), padded with 30 s of
//!    silence. A window of 3000 frames starting at `seek` is encoded.
//! 2. **Language**: unless given, the first window's decoder logits after `<|startoftranscript|>`
//!    are compared over the language tokens and the most likely language is used.
//! 3. **Decoding** is greedy with timestamp tokens: the first token must be a timestamp (≤ 1 s),
//!    timestamps come in pairs and never go backwards, timestamps win whenever their summed
//!    probability beats the best text token, and special tokens are suppressed. Windows whose
//!    `<|nospeech|>` probability exceeds 0.6 while the text is improbable are skipped. A loop
//!    guard stops a window that keeps repeating itself.
//! 4. `seek` advances to the last complete timestamp pair (or the whole window).
//! 5. **Word timestamps**: the window's text tokens are run once more through the decoder
//!    (`<|notimestamps|>` prompt) and the cross-attention logits of the model's alignment heads
//!    (`generation_config.json`; default: every head of the second half of the decoder) are
//!    softmaxed over the window's audio frames, standardised per token, median-filtered (width 7)
//!    and averaged; dynamic time warping through the negated matrix gives each token's start frame
//!    (20 ms resolution). Tokens are grouped into words at spaces, punctuation joins its word.
//!
//! 6. Word bounds are tightened past silent frames ([`crate::vad`]), so pauses stay pauses.
//!
//! Optional speaker labelling runs afterwards ([`crate::diarize`]).

mod align;
mod model;
pub mod tokenizer;

use std::path::Path;
use std::sync::Mutex;

use candle_core::Tensor;
use filmcraft_project::{Transcript, Word};

use crate::{Options, ProgressFn, SpeechError, Transcriber, sample_tick};
use model::{Config, Model};
use tokenizer::Tokenizer;

/// Mel frames per window (30 s).
const N_FRAMES: usize = 3000;
/// Mel frames per timestamp step (0.02 s).
const FRAMES_PER_TS: usize = 2;
const SAMPLES_PER_FRAME: usize = crate::mel::HOP;
const MAX_TOKENS: usize = 224;

fn merr(e: candle_core::Error) -> SpeechError {
    SpeechError::Model(e.to_string())
}

pub struct Whisper {
    id: String,
    model: Mutex<Model>,
    tok: Tokenizer,
    alignment_heads: Vec<(usize, usize)>,
    suppress: Vec<u32>,
    begin_suppress: Vec<u32>,
}

impl Whisper {
    /// Load a model directory (`config.json`, `generation_config.json`, `tokenizer.json`,
    /// `model.safetensors`).
    pub fn load(dir: &Path, id: &str) -> Result<Self, SpeechError> {
        Self::load_with(dir, id, candle_core::Device::Cpu, candle_core::DType::F32)
    }

    /// [`Whisper::load`] onto a chosen candle device (the CPU, or the Apple GPU with the `metal`
    /// feature) with a chosen float type (F32 on the CPU; F16 halves memory on the GPU).
    pub fn load_with(dir: &Path, id: &str, device: candle_core::Device, dtype: candle_core::DType) -> Result<Self, SpeechError> {
        let read = |n: &str| std::fs::read(dir.join(n)).map_err(|e| SpeechError::Model(format!("{}: {e}", dir.join(n).display())));
        let cfg: Config = serde_json::from_slice(&read("config.json")?).map_err(|e| SpeechError::Model(format!("config.json: {e}")))?;
        let gen_cfg: serde_json::Value = serde_json::from_slice(&read("generation_config.json")?).unwrap_or_default();
        let tok = Tokenizer::from_json(&String::from_utf8_lossy(&read("tokenizer.json")?))?;
        let weights = read("model.safetensors")?;
        let alignment_heads = gen_cfg["alignment_heads"]
            .as_array()
            .map(|a| a.iter().filter_map(|p| Some((p[0].as_u64()? as usize, p[1].as_u64()? as usize))).collect::<Vec<_>>())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| (cfg.decoder_layers / 2..cfg.decoder_layers).flat_map(|l| (0..cfg.decoder_attention_heads).map(move |h| (l, h))).collect());
        let mut suppress: Vec<u32> =
            gen_cfg["suppress_tokens"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64().map(|v| v as u32)).collect()).unwrap_or_default();
        suppress.extend(tok.non_text_specials());
        suppress.sort_unstable();
        suppress.dedup();
        let begin_suppress = gen_cfg["begin_suppress_tokens"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_u64().map(|v| v as u32)).collect())
            .unwrap_or_else(|| vec![220, tok.eot]);
        let model = Model::load_on(cfg, weights, device, dtype).map_err(merr)?;
        Ok(Self { id: id.to_string(), model: Mutex::new(model), tok, alignment_heads, suppress, begin_suppress })
    }

    pub fn languages(&self) -> Vec<String> {
        self.tok.languages.iter().map(|l| l.0.clone()).collect()
    }

    fn window(mel: &[f32], n_mels: usize, frames: usize, seek: usize) -> Vec<f32> {
        let mut w = vec![0f32; n_mels * N_FRAMES];
        for m in 0..n_mels {
            let end = (seek + N_FRAMES).min(frames);
            if seek < end {
                w[m * N_FRAMES..m * N_FRAMES + end - seek].copy_from_slice(&mel[m * frames + seek..m * frames + end]);
            }
        }
        w
    }

    fn detect_language(&self, m: &mut Model, xa: &Tensor) -> Result<String, SpeechError> {
        m.decoder.reset();
        let (logits, _) = m.decoder.forward(&[self.tok.sot], xa, false, None).map_err(merr)?;
        let l = Model::last_logits(&logits).map_err(merr)?;
        let best = self.tok.languages.iter().max_by(|a, b| l[a.1 as usize].total_cmp(&l[b.1 as usize])).map(|x| x.0.clone());
        Ok(best.unwrap_or_else(|| "en".into()))
    }

    /// Apply the timestamp rules and suppression to `logits` for the next token after `seq`
    /// (sampled tokens of this window, prompt excluded).
    fn constrain(&self, logits: &mut [f32], seq: &[u32], max_ts: Option<u32>) {
        let tb = self.tok.timestamp_begin as usize;
        let ninf = f32::NEG_INFINITY;
        for &s in &self.suppress {
            if let Some(v) = logits.get_mut(s as usize) {
                *v = ninf;
            }
        }
        if seq.is_empty() {
            for &s in &self.begin_suppress {
                if let Some(v) = logits.get_mut(s as usize) {
                    *v = ninf;
                }
            }
        }
        let last_ts = seq.last().is_some_and(|&t| self.tok.is_timestamp(t));
        let penult_ts = seq.len() < 2 || self.tok.is_timestamp(seq[seq.len() - 2]);
        if last_ts {
            if penult_ts {
                logits[tb..].iter_mut().for_each(|v| *v = ninf);
            } else {
                logits[..self.tok.eot as usize].iter_mut().for_each(|v| *v = ninf);
            }
        }
        if let Some(&last) = seq.iter().rev().find(|&&t| self.tok.is_timestamp(t)) {
            let lim = (if last_ts && !penult_ts { last } else { last + 1 } as usize).min(logits.len());
            logits[tb..lim].iter_mut().for_each(|v| *v = ninf);
        }
        if seq.is_empty() {
            logits[..tb].iter_mut().for_each(|v| *v = ninf);
            if let Some(m) = max_ts {
                let cut = (tb + m as usize + 1).min(logits.len());
                logits[cut..].iter_mut().for_each(|v| *v = ninf);
            }
        }
        // timestamps win when their total probability beats every text token
        let max = logits.iter().copied().fold(ninf, f32::max);
        if max.is_finite() {
            let lse = |s: &[f32]| -> f32 {
                let m = s.iter().copied().fold(ninf, f32::max);
                if !m.is_finite() {
                    return ninf;
                }
                m + s.iter().map(|v| (v - m).exp()).sum::<f32>().ln()
            };
            let ts = lse(&logits[tb..]);
            let best_text = logits[..tb].iter().copied().fold(ninf, f32::max);
            if ts > best_text {
                logits[..tb].iter_mut().for_each(|v| *v = ninf);
            }
        }
    }

    /// Greedy decode of one window. Returns sampled tokens (no prompt, no end-of-text), the mean
    /// log-probability and the no-speech probability.
    fn decode_window(&self, m: &mut Model, xa: &Tensor, prompt: &[u32]) -> Result<(Vec<u32>, f32, f32), SpeechError> {
        m.decoder.reset();
        let mut seq: Vec<u32> = Vec::new();
        let (logits, _) = m.decoder.forward(prompt, xa, true, None).map_err(merr)?;
        // no-speech probability from the start-of-transcript position
        let sot_at = prompt.iter().position(|&t| t == self.tok.sot).unwrap_or(0);
        let first =
            logits.get(0).and_then(|l| l.get(sot_at)).and_then(|l| l.to_dtype(candle_core::DType::F32)).and_then(|l| l.to_vec1::<f32>()).map_err(merr)?;
        let no_speech = self.tok.no_speech.map(|ns| softmax_at(&first, ns as usize)).unwrap_or(0.0);
        let mut cur = Model::last_logits(&logits).map_err(merr)?;
        let mut sum_lp = 0f32;
        // stay inside the decoder's position table
        let budget = MAX_TOKENS.min(m.cfg.max_target_positions / 2).min(m.cfg.max_target_positions.saturating_sub(prompt.len() + 1));
        for _ in 0..budget {
            let lp = log_softmax(&cur);
            self.constrain(&mut cur, &seq, Some(50));
            let next = argmax(&cur);
            sum_lp += lp[next as usize];
            if next == self.tok.eot {
                break;
            }
            seq.push(next);
            if repeating(&seq, self.tok.timestamp_begin) {
                break;
            }
            let (logits, _) = m.decoder.forward(&[next], xa, true, None).map_err(merr)?;
            cur = Model::last_logits(&logits).map_err(merr)?;
        }
        let avg = sum_lp / (seq.len() + 1) as f32;
        Ok((seq, avg, no_speech))
    }
}

fn softmax_at(x: &[f32], i: usize) -> f32 {
    let m = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let s: f32 = x.iter().map(|v| (v - m).exp()).sum();
    x.get(i).map(|v| (v - m).exp() / s).unwrap_or(0.0)
}

fn log_softmax(x: &[f32]) -> Vec<f32> {
    let m = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let lse = m + x.iter().map(|v| (v - m).exp()).sum::<f32>().ln();
    x.iter().map(|v| v - lse).collect()
}

fn argmax(x: &[f32]) -> u32 {
    let mut best = 0;
    for (i, v) in x.iter().enumerate() {
        if *v > x[best] {
            best = i;
        }
    }
    best as u32
}

/// The tail of `seq` (text tokens) repeats a short pattern many times: a decoding loop.
fn repeating(seq: &[u32], tb: u32) -> bool {
    let text: Vec<u32> = seq.iter().copied().filter(|&t| t < tb).collect();
    for n in 1..=8 {
        let reps = if n == 1 { 12 } else { 5 };
        if text.len() < n * reps {
            continue;
        }
        let tail = &text[text.len() - n * reps..];
        if (1..reps).all(|r| tail[r * n..(r + 1) * n] == tail[..n]) {
            return true;
        }
    }
    false
}

impl Transcriber for Whisper {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        let mut m = self.model.lock().unwrap_or_else(|e| e.into_inner());
        let n_mels = m.cfg.num_mel_bins;
        let content_frames = audio.len() / SAMPLES_PER_FRAME;
        let mut padded = audio.to_vec();
        padded.extend(std::iter::repeat_n(0.0, N_FRAMES * SAMPLES_PER_FRAME));
        if !progress(0.0, "Analysing audio") {
            return Err(SpeechError::Cancelled);
        }
        let trace = std::env::var_os("FILMCRAFT_SPEECH_TRACE").is_some();
        let clock = std::time::Instant::now();
        let (frames, mel) = crate::mel::log_mel(&padded, n_mels);
        if trace {
            eprintln!("mel {:.3}s", clock.elapsed().as_secs_f64());
        }
        let mut language = opts.language.clone().filter(|l| !l.is_empty() && l != "auto");
        if language.as_deref().is_some_and(|l| self.tok.multilingual() && self.tok.language_token(l).is_none()) {
            return Err(SpeechError::Model(format!("the model does not know the language `{}`", language.unwrap_or_default())));
        }
        let mut words: Vec<Word> = Vec::new();
        let mut seek = 0usize;
        while seek < content_frames {
            let frac = seek as f32 / content_frames.max(1) as f32;
            if !progress(frac * 0.95, &format!("Transcribing {:.0}%", frac * 100.0)) {
                return Err(SpeechError::Cancelled);
            }
            let seg_frames = N_FRAMES.min(content_frames - seek);
            let win = Self::window(&mel, n_mels, frames, seek);
            let x = Tensor::from_vec(win, (1, n_mels, N_FRAMES), &m.device).and_then(|x| x.to_dtype(m.dtype)).map_err(merr)?;
            let xa = m.encoder.forward(&x).map_err(merr)?;
            if trace {
                eprintln!("encode {:.3}s", clock.elapsed().as_secs_f64());
            }
            let mut prompt = vec![self.tok.sot];
            if self.tok.multilingual() {
                if language.is_none() {
                    language = Some(self.detect_language(&mut m, &xa)?);
                }
                let lang = language.as_deref().unwrap_or("en");
                prompt.push(self.tok.language_token(lang).unwrap_or(self.tok.sot + 1));
                prompt.push(self.tok.transcribe);
            }
            let (toks, avg_lp, no_speech) = self.decode_window(&mut m, &xa, &prompt)?;
            if trace {
                eprintln!("decode {} tokens {:.3}s", toks.len(), clock.elapsed().as_secs_f64());
            }
            let skip = no_speech > 0.6 && avg_lp < -1.0;
            // where the next window starts
            let is_ts: Vec<bool> = toks.iter().map(|&t| self.tok.is_timestamp(t)).collect();
            let single_ending = is_ts.len() >= 2 && !is_ts[is_ts.len() - 2] && is_ts[is_ts.len() - 1];
            let last_pair = (1..is_ts.len()).rev().find(|&i| is_ts[i] && is_ts[i - 1]);
            let advance = match last_pair {
                Some(i) if !single_ending => ((toks[i - 1] - self.tok.timestamp_begin) as usize * FRAMES_PER_TS).clamp(1, seg_frames),
                _ => seg_frames,
            };
            let text: Vec<u32> = toks.iter().copied().filter(|&t| t < self.tok.eot).collect();
            if !skip && !text.is_empty() {
                // the text up to the last complete segment (what `advance` covers)
                let upto = match last_pair {
                    Some(i) if !single_ending => i,
                    _ => toks.len(),
                };
                let text: Vec<u32> = toks[..upto].iter().copied().filter(|&t| t < self.tok.eot).collect();
                let mut aprompt = prompt.clone();
                aprompt.push(self.tok.no_timestamps);
                let times = align::align(&mut m, &xa, &aprompt, &text, self.tok.eot, &self.alignment_heads, seg_frames).map_err(merr)?;
                let offset = seek * SAMPLES_PER_FRAME;
                let limit = (seek + advance) * SAMPLES_PER_FRAME;
                for (wtext, r) in tokenizer::group_words(&self.tok, &text) {
                    let (s, e) = (times[r.start].0, times[r.end - 1].1);
                    let a = (offset + s * SAMPLES_PER_FRAME).min(limit);
                    let b = (offset + e * SAMPLES_PER_FRAME).min(limit).max(a);
                    words.push(Word::new(wtext, sample_tick(a as i64), sample_tick(b as i64)));
                }
            }
            if trace {
                eprintln!("align {:.3}s", clock.elapsed().as_secs_f64());
            }
            seek += advance;
        }
        let mut t = Transcript { language: language.unwrap_or_else(|| "en".into()), source: self.id.clone(), speakers: Vec::new(), words, voice: Vec::new() };
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
    fn repetition_guard() {
        let tb = 1000;
        assert!(repeating(&[5; 12], tb));
        assert!(!repeating(&[5, 5, 5, 6, 7, 8], tb));
        let mut s = Vec::new();
        for _ in 0..5 {
            s.extend([1, 2, 3]);
        }
        assert!(repeating(&s, tb));
        assert!(!repeating(&[1, 2, 3, 4, 5, 6, 7], tb));
    }
}
