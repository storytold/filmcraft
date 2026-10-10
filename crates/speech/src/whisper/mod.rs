//! Whisper speech recognition in pure Rust on the CPU ([`crate::nn`] kernels).
//!
//! 1. The whole clip's log-mel spectrogram is computed once ([`crate::mel`]), padded with 30 s of
//!    silence (whose frames give the padding value of short windows).
//! 2. The clip is cut into regions of about 90 s at its longest pauses ([`plan`]). Inside a region
//!    the procedure the model was published with runs: 30-second windows, each starting where the
//!    previous one's last complete segment ended. The regions are independent, so the current
//!    window of every region is encoded and then all of them are decoded **together**: every
//!    decoding step reads the decoder weights once for the whole batch. Each window's
//!    cross-attention keys/values are projected once. Silence of 1.5 s or more at the start of a
//!    window is skipped.
//! 3. **Language**: unless given, the decoder logits after `<|startoftranscript|>` on the first
//!    window are compared over the language tokens and the most likely language is used.
//! 4. **Decoding** is greedy with timestamp tokens: the first token must be a timestamp (≤ 1 s),
//!    timestamps come in pairs and never go backwards, timestamps win whenever their summed
//!    probability beats the best text token, and special tokens are suppressed. Windows whose
//!    `<|nospeech|>` probability exceeds 0.6 while the text is improbable are skipped. A loop
//!    guard stops a window that keeps repeating itself; such a window (or one that runs out of
//!    tokens) is decoded again with seeded sampling at temperature 0.2, 0.4, … 1.0.
//! 5. A window whose text ends in an unfinished segment keeps only the complete segments; its
//!    region's next window starts at the end of the last one. A window still reads the 30 s of
//!    audio after its start, past its region's end; words that start (after trimming silence)
//!    beyond the region's end are left to the next region.
//! 6. **Word timestamps**: the window's text tokens are run once more through the decoder
//!    (`<|notimestamps|>` prompt; layers after the last alignment head and the output projection
//!    are skipped) and the cross-attention logits of the model's alignment heads
//!    (`generation_config.json`; default: every head of the second half of the decoder) are
//!    softmaxed over the window's audio frames, standardised per token, median-filtered (width 7)
//!    and averaged; dynamic time warping through the negated matrix gives each token's start frame
//!    (20 ms resolution). Tokens are grouped into words at spaces, punctuation joins its word.
//! 7. Word bounds are tightened past silent frames ([`crate::vad`]), so pauses stay pauses.
//!
//! Optional speaker labelling runs afterwards ([`crate::diarize`]).
//!
//! Everything runs on a dedicated rayon pool ([`default_threads`]); progress is reported per
//! window and the progress callback can cancel between decoding steps.

mod align;
mod model;
pub mod plan;
pub mod tokenizer;

use std::path::Path;
use std::time::{Duration, Instant};

use filmcraft_project::{Transcript, Word};
use rayon::prelude::*;

use crate::{Options, ProgressFn, SpeechError, Transcriber, sample_tick};
use model::{Config, Model, N_CTX, N_FRAMES, Row, Scratch, Weights};
use tokenizer::Tokenizer;

/// Mel frames per timestamp step (0.02 s).
const FRAMES_PER_TS: usize = 2;
const SAMPLES_PER_FRAME: usize = crate::mel::HOP;
const MAX_TOKENS: usize = 224;
/// Memory for the decoding state of one batch of windows.
const BATCH_BYTES: usize = 1536 << 20;
/// Most windows decoded together.
const MAX_BATCH: usize = 16;
const TICKS_PER_SAMPLE_I64: i64 = crate::TICKS_PER_SAMPLE;

pub struct Whisper {
    id: String,
    model: Model,
    tok: Tokenizer,
    alignment_heads: Vec<(usize, usize)>,
    suppress: Vec<u32>,
    begin_suppress: Vec<u32>,
    pool: rayon::ThreadPool,
}

/// Threads used for inference: `FILMCRAFT_SPEECH_THREADS` if set, else two thirds of the logical
/// CPUs (all of them on machines with fewer than 6). Leaving some cores free keeps the editor
/// responsive and is also faster: matrix products split evenly over threads, so a thread that
/// shares its core (SMT), runs on an efficiency core or is preempted holds everyone up.
pub fn default_threads() -> usize {
    if let Some(n) = std::env::var("FILMCRAFT_SPEECH_THREADS").ok().and_then(|s| s.trim().parse::<usize>().ok()).filter(|&n| n > 0) {
        return n.min(1024);
    }
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    if n >= 6 { n * 2 / 3 } else { n }
}

fn merr(e: impl std::fmt::Display) -> SpeechError {
    SpeechError::Model(e.to_string())
}

/// A window: mel frames `start..end` (at most 30 s); frames past `end` are silence.
#[derive(Clone, Copy, Debug)]
struct Chunk {
    start: usize,
    end: usize,
}

/// The decoded tokens of one window.
struct Decoded {
    /// sampled tokens (no prompt, no end-of-text)
    toks: Vec<u32>,
    avg_lp: f32,
    no_speech: f32,
    /// ended in a loop or ran out of tokens instead of reaching `<|endoftext|>`
    failed: bool,
}

/// Sampling temperatures: greedy first, then the fallbacks for a window that loops.
const TEMPERATURES: [f32; 6] = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];

/// A seeded xorshift generator (reproducible sampling).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    /// Uniform in [0, 1).
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Draw a token from `softmax(logits / t)` with the uniform number `u` in [0, 1).
fn sample(logits: &[f32], t: f32, u: f32) -> u32 {
    let m = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !m.is_finite() || t <= 0.0 {
        return argmax(logits);
    }
    let w: Vec<f32> = logits.iter().map(|&v| ((v - m) / t).exp()).collect();
    let mut u = u * w.iter().sum::<f32>();
    for (i, &p) in w.iter().enumerate() {
        if u < p && p > 0.0 {
            return i as u32;
        }
        u -= p;
    }
    argmax(logits)
}

#[derive(Default)]
struct Timing {
    mel: Duration,
    encode: Duration,
    decode: Duration,
    align: Duration,
    windows: usize,
    steps: usize,
    tokens: usize,
    fallbacks: usize,
}

impl Whisper {
    /// Load a model directory (`config.json`, `generation_config.json`, `tokenizer.json`,
    /// `model.safetensors`).
    pub fn load(dir: &Path, id: &str) -> Result<Self, SpeechError> {
        let read = |n: &str| std::fs::read(dir.join(n)).map_err(|e| SpeechError::Model(format!("{}: {e}", dir.join(n).display())));
        let mut weights = Weights::open(dir)?;
        let (cfg, gen_cfg) = match weights {
            Weights::Safetensors(_) => {
                let cfg: Config = serde_json::from_slice(&read("config.json")?).map_err(|e| SpeechError::Model(format!("config.json: {e}")))?;
                (cfg, serde_json::from_slice(&read("generation_config.json")?).unwrap_or_default())
            }
            // a faster-whisper conversion: its config.json holds the decoding options
            Weights::Ct2(_) => {
                let c: serde_json::Value = serde_json::from_slice(&read("config.json")?).unwrap_or_default();
                let g = serde_json::json!({"alignment_heads": c["alignment_heads"], "suppress_tokens": c["suppress_ids"], "begin_suppress_tokens": c["suppress_ids_begin"]});
                (weights.ct2_config()?, g)
            }
        };
        cfg.validate()?;
        let tok = Tokenizer::from_json(&String::from_utf8_lossy(&read("tokenizer.json")?))?;
        let vocab = cfg.vocab_size as u32;
        if [tok.eot, tok.sot, tok.transcribe, tok.no_timestamps, tok.timestamp_begin].iter().any(|&t| t >= vocab) {
            return Err(SpeechError::Model("tokenizer.json does not match the model's vocabulary".into()));
        }
        let alignment_heads = gen_cfg["alignment_heads"]
            .as_array()
            .map(|a| a.iter().filter_map(|p| Some((p.get(0)?.as_u64()? as usize, p.get(1)?.as_u64()? as usize))).collect::<Vec<_>>())
            .map(|v| v.into_iter().filter(|&(l, h)| l < cfg.decoder_layers && h < cfg.decoder_attention_heads).collect::<Vec<_>>())
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
        let pool = rayon::ThreadPoolBuilder::new().num_threads(default_threads()).thread_name(|i| format!("speech-{i}")).build().map_err(merr)?;
        let model = pool.install(|| Model::load(cfg, &mut weights))?;
        Ok(Self { id: id.to_string(), model, tok, alignment_heads, suppress, begin_suppress, pool })
    }

    pub fn languages(&self) -> Vec<String> {
        self.tok.languages.iter().map(|l| l.0.clone()).collect()
    }

    /// Windows decoded together: as many as fit [`BATCH_BYTES`] (at most [`MAX_BATCH`];
    /// `FILMCRAFT_SPEECH_BATCH` overrides).
    fn batch_size(&self) -> usize {
        if let Some(n) = std::env::var("FILMCRAFT_SPEECH_BATCH").ok().and_then(|s| s.trim().parse::<usize>().ok()).filter(|&n| n > 0) {
            return n.min(64);
        }
        (BATCH_BYTES / self.model.row_bytes().max(1)).clamp(1, MAX_BATCH)
    }

    /// The `n_mels × N_FRAMES` input of window `c`; frames past its end hold `pad` (silence).
    fn window(mel: &[f32], n_mels: usize, frames: usize, c: Chunk, pad: f32) -> Vec<f32> {
        let mut w = vec![pad; n_mels * N_FRAMES];
        let len = c.end.saturating_sub(c.start).min(N_FRAMES);
        for m in 0..n_mels {
            let src = mel.get(m * frames + c.start..m * frames + c.start + len);
            if let (Some(dst), Some(src)) = (w.get_mut(m * N_FRAMES..m * N_FRAMES + len), src) {
                dst.copy_from_slice(src);
            }
        }
        w
    }

    fn detect_language(&self, row: &mut Row, s: &mut Scratch) -> Result<String, SpeechError> {
        let logits = self.pool.install(|| self.model.step(&mut [row], &[self.tok.sot], 0, true, s))?.unwrap_or_default();
        let score = |id: u32| logits.get(id as usize).copied().unwrap_or(f32::NEG_INFINITY);
        let best = self.tok.languages.iter().max_by(|a, b| score(a.1).total_cmp(&score(b.1))).map(|x| x.0.clone());
        Ok(best.unwrap_or_else(|| "en".into()))
    }

    /// Apply the timestamp rules and suppression to `logits` for the next token after `seq`
    /// (sampled tokens of this window, prompt excluded).
    fn constrain(&self, logits: &mut [f32], seq: &[u32], max_ts: Option<u32>) {
        let tb = (self.tok.timestamp_begin as usize).min(logits.len());
        let eot = (self.tok.eot as usize).min(logits.len());
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
        let penult_ts = seq.len() < 2 || seq.get(seq.len() - 2).is_some_and(|&t| self.tok.is_timestamp(t));
        if last_ts {
            if penult_ts {
                logits[tb..].iter_mut().for_each(|v| *v = ninf);
            } else {
                logits[..eot].iter_mut().for_each(|v| *v = ninf);
            }
        }
        if let Some(&last) = seq.iter().rev().find(|&&t| self.tok.is_timestamp(t)) {
            let lim = (if last_ts && !penult_ts { last } else { last + 1 } as usize).clamp(tb, logits.len());
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

    /// Greedy decoding of a batch of windows in lockstep. `poll` is called between steps; `false`
    /// cancels.
    /// Row `i` is sampled at temperature `temps[i]` (0 = greedy) with a generator seeded by
    /// `seeds[i]`.
    #[allow(clippy::too_many_arguments)]
    fn decode(
        &self,
        rows: &mut [Row],
        prompt: &[u32],
        temps: &[f32],
        seeds: &[u64],
        s: &mut Scratch,
        t: &mut Timing,
        poll: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<Decoded>, SpeechError> {
        let n = rows.len();
        let vocab = self.model.cfg.vocab_size;
        let mut out: Vec<Decoded> = (0..n).map(|_| Decoded { toks: Vec::new(), avg_lp: 0.0, no_speech: 0.0, failed: true }).collect();
        let mut rngs: Vec<Rng> = (0..n).map(|i| Rng::new(seeds.get(i).copied().unwrap_or(0))).collect();
        let mut sum_lp = vec![0f32; n];
        let mut cur = Vec::new();
        for (p, &tok) in prompt.iter().enumerate() {
            let want = p == 0 || p + 1 == prompt.len();
            let mut refs: Vec<&mut Row> = rows.iter_mut().collect();
            let logits = self.pool.install(|| self.model.step(&mut refs, &vec![tok; n], p, want, s))?;
            if let Some(l) = logits {
                if p == 0
                    && let Some(ns) = self.tok.no_speech
                {
                    for (i, o) in out.iter_mut().enumerate() {
                        o.no_speech = l.get(i * vocab..(i + 1) * vocab).map(|r| softmax_at(r, ns as usize)).unwrap_or(0.0);
                    }
                }
                cur = l;
            }
        }
        let mut active: Vec<usize> = (0..n).collect();
        let max_steps = MAX_TOKENS.min(self.model.cfg.max_target_positions / 2);
        for step in 0..max_steps {
            let draws: Vec<f32> = active.iter().map(|&r| rngs[r].next()).collect();
            let picks: Vec<(u32, f32)> = self.pool.install(|| {
                active
                    .par_iter()
                    .zip(&draws)
                    .enumerate()
                    .map(|(ai, (&r, &u))| {
                        let l = cur.get(ai * vocab..(ai + 1) * vocab).unwrap_or_default();
                        let lp = crate::nn::log_softmax(l);
                        let mut c = l.to_vec();
                        self.constrain(&mut c, &out[r].toks, Some(50));
                        let temp = temps.get(r).copied().unwrap_or(0.0);
                        let next = if temp > 0.0 { sample(&c, temp, u) } else { argmax(&c) };
                        (next, lp.get(next as usize).copied().unwrap_or(f32::NEG_INFINITY))
                    })
                    .collect()
            });
            let mut next_active = Vec::with_capacity(active.len());
            let mut tokens = Vec::with_capacity(active.len());
            for (&r, (next, lp)) in active.iter().zip(picks) {
                sum_lp[r] += lp;
                if next == self.tok.eot {
                    out[r].failed = false;
                    continue;
                }
                out[r].toks.push(next);
                t.tokens += 1;
                if repeating(&out[r].toks, self.tok.timestamp_begin) {
                    continue;
                }
                next_active.push(r);
                tokens.push(next);
            }
            active = next_active;
            if active.is_empty() || step + 1 == max_steps {
                break;
            }
            if !poll(step) {
                return Err(SpeechError::Cancelled);
            }
            let mut refs: Vec<&mut Row> = rows.iter_mut().enumerate().filter(|(i, _)| active.contains(i)).map(|(_, r)| r).collect();
            cur = self.pool.install(|| self.model.step(&mut refs, &tokens, prompt.len() + step, true, s))?.unwrap_or_default();
            t.steps += 1;
        }
        for (o, lp) in out.iter_mut().zip(sum_lp) {
            o.avg_lp = lp / (o.toks.len() + 1) as f32;
        }
        Ok(out)
    }

    fn run(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        let n_mels = self.model.cfg.num_mel_bins;
        let content_frames = audio.len() / SAMPLES_PER_FRAME;
        if !progress(0.0, "Analysing audio") {
            return Err(SpeechError::Cancelled);
        }
        let trace = std::env::var_os("FILMCRAFT_SPEECH_TRACE").is_some();
        let mut tm = Timing::default();
        let clock = Instant::now();
        let mut padded = audio.to_vec();
        padded.extend(std::iter::repeat_n(0.0, N_FRAMES * SAMPLES_PER_FRAME));
        let (frames, mel) = self.pool.install(|| crate::mel::log_mel(&padded, n_mels));
        drop(padded);
        // the last frame lies in the appended silence
        let pad = mel.get(frames.saturating_sub(1)).copied().unwrap_or(0.0);
        tm.mel = clock.elapsed();
        let mut language = opts.language.clone().filter(|l| !l.is_empty() && l != "auto");
        if language.as_deref().is_some_and(|l| self.tok.multilingual() && self.tok.language_token(l).is_none()) {
            return Err(SpeechError::Model(format!("the model does not know the language `{}`", language.unwrap_or_default())));
        }
        let db = crate::vad::frame_db(audio);
        let th = crate::vad::threshold(&db);
        let silent: Vec<bool> = db.iter().map(|&v| v < th).collect();
        let content = content_frames.min(frames);
        // independent regions decoded side by side (one, without silence skipping, for the
        // classic procedure: `FILMCRAFT_SPEECH_SEQUENTIAL`, for diagnostics)
        let sequential = std::env::var_os("FILMCRAFT_SPEECH_SEQUENTIAL").is_some();
        let max_batch = self.batch_size();
        let n_regions = if sequential { 1 } else { content.div_ceil(plan::REGION).clamp(1, max_batch) };
        let mut streams: Vec<(usize, usize)> = plan::regions(content, &silent, &db, n_regions);
        let starts: Vec<usize> = streams.iter().map(|s| s.0).collect();
        // temperature fallback level of every region's current window
        let mut attempt = vec![0usize; streams.len()];
        let total = content.max(1) as f32;
        let mut shown = 0f32;
        let mut report = |done: f32, progress: &mut dyn FnMut(f32, &str) -> bool| -> bool {
            let f = (0.02 + 0.93 * (done / total).clamp(0.0, 1.0)).max(shown);
            shown = f;
            progress(f, &format!("Transcribing {:.0}%", f * 100.0))
        };
        let mut s = Scratch::default();
        let mut words: Vec<(usize, Vec<Word>)> = Vec::new();
        loop {
            // the next window of every unfinished region
            let mut batch: Vec<(usize, Chunk)> = Vec::new();
            for (i, st) in streams.iter_mut().enumerate() {
                if !sequential {
                    st.0 = plan::skip_silence(st.0, st.1, &silent);
                }
                if st.0 < st.1 && batch.len() < max_batch {
                    // like the sequential procedure, a window holds the next 30 s of audio, even
                    // past its region's end (the words found there belong to the next region)
                    batch.push((i, Chunk { start: st.0, end: (st.0 + N_FRAMES).min(content) }));
                }
            }
            if batch.is_empty() {
                break;
            }
            // frames already behind the regions' seek points
            let base: f32 = streams.iter().zip(&starts).map(|(st, &s0)| st.0.min(st.1).saturating_sub(s0) as f32).sum();
            let batch_frames: f32 = batch.iter().map(|b| (b.1.end - b.1.start) as f32).sum();
            // encode
            let mut rows = Vec::with_capacity(batch.len());
            for (k, &(_, c)) in batch.iter().enumerate() {
                if !report(base + 0.45 * batch_frames * k as f32 / batch.len() as f32, progress) {
                    return Err(SpeechError::Cancelled);
                }
                let t0 = Instant::now();
                let win = Self::window(&mel, n_mels, frames, c, pad);
                let xa = self.pool.install(|| self.model.encode(&win, &mut s))?;
                rows.push(self.pool.install(|| self.model.row(&xa))?);
                tm.encode += t0.elapsed();
                tm.windows += 1;
            }
            // prompt
            let mut prompt = vec![self.tok.sot];
            if self.tok.multilingual() {
                if language.is_none()
                    && let Some(r0) = rows.first_mut()
                {
                    language = Some(self.detect_language(r0, &mut s)?);
                }
                let lang = language.as_deref().unwrap_or("en");
                prompt.push(self.tok.language_token(lang).unwrap_or(self.tok.sot + 1));
                prompt.push(self.tok.transcribe);
            }
            // decode
            let t0 = Instant::now();
            let temps: Vec<f32> = batch.iter().map(|b| TEMPERATURES.get(attempt[b.0]).copied().unwrap_or(1.0)).collect();
            let seeds: Vec<u64> = batch.iter().map(|b| (b.1.start as u64) << 8 | attempt[b.0] as u64).collect();
            let decoded = {
                let mut poll = |step: usize| report(base + batch_frames * (0.45 + 0.4 * (step as f32 / 150.0).min(1.0)), progress);
                self.decode(&mut rows, &prompt, &temps, &seeds, &mut s, &mut tm, &mut poll)?
            };
            tm.decode += t0.elapsed();
            // segments, word times, where each region continues
            let t0 = Instant::now();
            for ((&(si, c), row), d) in batch.iter().zip(&rows).zip(decoded) {
                let seg_frames = c.end - c.start;
                // a window that loops is decoded again at the next temperature
                if d.failed && attempt[si] + 1 < TEMPERATURES.len() {
                    attempt[si] += 1;
                    tm.fallbacks += 1;
                    continue;
                }
                attempt[si] = 0;
                let toks = d.toks;
                let skip = d.no_speech > 0.6 && d.avg_lp < -1.0;
                let is_ts: Vec<bool> = toks.iter().map(|&t| self.tok.is_timestamp(t)).collect();
                let single_ending = is_ts.len() >= 2 && !is_ts[is_ts.len() - 2] && is_ts[is_ts.len() - 1];
                let last_pair = (1..is_ts.len()).rev().find(|&i| is_ts[i] && is_ts[i - 1]);
                let advance = match last_pair {
                    Some(i) if !single_ending => ((toks[i - 1] - self.tok.timestamp_begin) as usize * FRAMES_PER_TS).clamp(1, seg_frames),
                    _ => seg_frames,
                };
                if let Some(st) = streams.get_mut(si) {
                    st.0 = c.start + advance;
                }
                let upto = match last_pair {
                    Some(i) if !single_ending => i,
                    _ => toks.len(),
                };
                let text: Vec<u32> = toks[..upto].iter().copied().filter(|&t| t < self.tok.eot).collect();
                if !skip && !text.is_empty() {
                    let mut aprompt = prompt.clone();
                    aprompt.push(self.tok.no_timestamps);
                    let mut tokens = aprompt.clone();
                    tokens.extend_from_slice(&text);
                    tokens.push(self.tok.eot);
                    let qk = self.pool.install(|| self.model.cross_logits(row, &tokens, &self.alignment_heads, &mut s))?;
                    let times = align::align(&qk, tokens.len(), N_CTX, aprompt.len() - 1, text.len(), seg_frames);
                    let offset = c.start * SAMPLES_PER_FRAME;
                    let limit = (c.start + advance) * SAMPLES_PER_FRAME;
                    let region_end = sample_tick(streams.get(si).map_or(i64::MAX / TICKS_PER_SAMPLE_I64, |st| (st.1 * SAMPLES_PER_FRAME) as i64));
                    let mut ws = Vec::new();
                    for (wtext, r) in tokenizer::group_words(&self.tok, &text) {
                        let (Some(s0), Some(e0)) = (times.get(r.start), r.end.checked_sub(1).and_then(|i| times.get(i))) else { continue };
                        let a = (offset + s0.0 * SAMPLES_PER_FRAME).min(limit);
                        let b = (offset + e0.1 * SAMPLES_PER_FRAME).min(limit).max(a);
                        ws.push(Word::new(wtext, sample_tick(a as i64), sample_tick(b as i64)));
                    }
                    // words past the region's end belong to the next region; decide by where the
                    // word's sound starts (alignment tends to start a word inside the pause before it)
                    crate::vad::tighten_words_db(&db, th, &mut ws);
                    ws.retain(|w| w.start < region_end);
                    words.push((c.start, ws));
                }
            }
            tm.align += t0.elapsed();
        }
        if trace {
            eprintln!(
                "speech: {} windows ({} fallbacks), {} steps, {} tokens, {} threads; mel {:.2}s encode {:.2}s decode {:.2}s align {:.2}s",
                tm.windows,
                tm.fallbacks,
                tm.steps,
                tm.tokens,
                self.pool.current_num_threads(),
                tm.mel.as_secs_f64(),
                tm.encode.as_secs_f64(),
                tm.decode.as_secs_f64(),
                tm.align.as_secs_f64()
            );
        }
        words.sort_by_key(|w| w.0);
        let words = words.into_iter().flat_map(|w| w.1).collect();
        let mut t = Transcript { language: language.unwrap_or_else(|| "en".into()), source: self.id.clone(), speakers: Vec::new(), words };
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

fn softmax_at(x: &[f32], i: usize) -> f32 {
    let m = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let s: f32 = x.iter().map(|v| (v - m).exp()).sum();
    x.get(i).map(|v| (v - m).exp() / s).unwrap_or(0.0)
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
        // the kernels check every shape; this is the last line of defence for the never-crash rule
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(audio, opts, progress)))
            .unwrap_or_else(|_| Err(SpeechError::Model("speech recognition failed (internal error)".into())))
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

    #[test]
    fn default_thread_count_leaves_headroom() {
        let n = default_threads();
        assert!(n >= 1);
    }
    #[test]
    fn sampling_is_reproducible_and_follows_the_distribution() {
        let logits = [0.0f32, 2.0, f32::NEG_INFINITY, 1.0];
        assert_eq!(sample(&logits, 0.0, 0.5), 1);
        let mut rng = Rng::new(7);
        let mut counts = [0usize; 4];
        for _ in 0..4000 {
            counts[sample(&logits, 1.0, rng.next()) as usize] += 1;
        }
        assert_eq!(counts[2], 0);
        assert!(counts[1] > counts[3] && counts[3] > counts[0], "{counts:?}");
        let (mut a, mut b) = (Rng::new(3), Rng::new(3));
        assert!((0..10).all(|_| a.next() == b.next()));
        assert_eq!(sample(&[f32::NEG_INFINITY; 3], 0.5, 0.3), 0);
    }

    /// A tiny Whisper with random weights, written to a temporary model directory.
    struct Tiny {
        dir: std::path::PathBuf,
    }

    impl Drop for Tiny {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn tiny_model(tag: &str, config: serde_json::Value) -> Tiny {
        let dir = std::env::temp_dir().join(format!("filmcraft-whisper-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let get = |k: &str| config[k].as_u64().unwrap() as usize;
        let (d, nm, vocab) = (get("d_model"), get("num_mel_bins"), get("vocab_size"));
        let (el, dl, src, tgt) = (get("encoder_layers"), get("decoder_layers"), get("max_source_positions"), get("max_target_positions"));
        let mut tensors: Vec<(String, Vec<usize>)> = vec![
            ("model.encoder.conv1.weight".into(), vec![d, nm, 3]),
            ("model.encoder.conv1.bias".into(), vec![d]),
            ("model.encoder.conv2.weight".into(), vec![d, d, 3]),
            ("model.encoder.conv2.bias".into(), vec![d]),
            ("model.encoder.embed_positions.weight".into(), vec![src, d]),
            ("model.encoder.layer_norm.weight".into(), vec![d]),
            ("model.encoder.layer_norm.bias".into(), vec![d]),
            ("model.decoder.embed_tokens.weight".into(), vec![vocab, d]),
            ("model.decoder.embed_positions.weight".into(), vec![tgt, d]),
            ("model.decoder.layer_norm.weight".into(), vec![d]),
            ("model.decoder.layer_norm.bias".into(), vec![d]),
        ];
        let attn = |t: &mut Vec<(String, Vec<usize>)>, p: &str| {
            for x in ["q_proj", "k_proj", "v_proj", "out_proj"] {
                t.push((format!("{p}.{x}.weight"), vec![d, d]));
                if x != "k_proj" {
                    t.push((format!("{p}.{x}.bias"), vec![d]));
                }
            }
        };
        let norm = |t: &mut Vec<(String, Vec<usize>)>, p: &str| {
            t.push((format!("{p}.weight"), vec![d]));
            t.push((format!("{p}.bias"), vec![d]));
        };
        let mlp = |t: &mut Vec<(String, Vec<usize>)>, p: &str| {
            t.push((format!("{p}.fc1.weight"), vec![4 * d, d]));
            t.push((format!("{p}.fc1.bias"), vec![4 * d]));
            t.push((format!("{p}.fc2.weight"), vec![d, 4 * d]));
            t.push((format!("{p}.fc2.bias"), vec![d]));
        };
        for i in 0..el {
            let p = format!("model.encoder.layers.{i}");
            attn(&mut tensors, &format!("{p}.self_attn"));
            norm(&mut tensors, &format!("{p}.self_attn_layer_norm"));
            norm(&mut tensors, &format!("{p}.final_layer_norm"));
            mlp(&mut tensors, &p);
        }
        for i in 0..dl {
            let p = format!("model.decoder.layers.{i}");
            attn(&mut tensors, &format!("{p}.self_attn"));
            attn(&mut tensors, &format!("{p}.encoder_attn"));
            for n in ["self_attn_layer_norm", "encoder_attn_layer_norm", "final_layer_norm"] {
                norm(&mut tensors, &format!("{p}.{n}"));
            }
            mlp(&mut tensors, &p);
        }
        // F16 weights (the large models' storage format), small random values
        let mut header = serde_json::Map::new();
        let mut data = Vec::new();
        let mut seed = 0x1234_5678u32;
        for (name, shape) in &tensors {
            let n: usize = shape.iter().product();
            let begin = data.len();
            for _ in 0..n {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                // a value within ±0.25 as an f16 bit pattern (sign, exponent 10..13, random mantissa)
                let h = ((seed >> 16) as u16 & 0x83ff) | (((10 + (seed >> 8) % 4) as u16) << 10);
                data.extend_from_slice(&h.to_le_bytes());
            }
            header.insert(name.clone(), serde_json::json!({"dtype": "F16", "shape": shape, "data_offsets": [begin, data.len()]}));
        }
        let h = serde_json::to_vec(&header).unwrap();
        let mut file = (h.len() as u64).to_le_bytes().to_vec();
        file.extend(h);
        file.extend(data);
        std::fs::write(dir.join("model.safetensors"), file).unwrap();
        std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
        std::fs::write(dir.join("generation_config.json"), r#"{"alignment_heads": [[1, 0], [1, 1], [9, 9]], "suppress_tokens": [1, 2]}"#).unwrap();
        // tokenizer: printable ASCII and " a".." z" as tokens, then the special tokens
        let mut vocab_map = serde_json::Map::new();
        for (i, c) in ('!'..='~').enumerate() {
            vocab_map.insert(c.to_string(), i.into());
        }
        for (i, c) in ('a'..='z').enumerate() {
            vocab_map.insert(format!("\u{120}{c}"), (94 + i).into());
        }
        let mut added = Vec::new();
        let mut id = 120;
        for sp in ["<|endoftext|>", "<|startoftranscript|>", "<|en|>", "<|de|>", "<|translate|>", "<|transcribe|>", "<|nospeech|>", "<|notimestamps|>"] {
            added.push(serde_json::json!({"id": id, "content": sp}));
            id += 1;
        }
        for t in 0..=1500 {
            added.push(serde_json::json!({"id": id, "content": format!("<|{:.2}|>", t as f64 * 0.02)}));
            id += 1;
        }
        assert_eq!(id, vocab);
        let tok = serde_json::json!({"model": {"vocab": vocab_map}, "added_tokens": added});
        std::fs::write(dir.join("tokenizer.json"), tok.to_string()).unwrap();
        Tiny { dir }
    }

    fn tiny_config() -> serde_json::Value {
        serde_json::json!({
            "num_mel_bins": 80, "d_model": 16, "encoder_layers": 1, "encoder_attention_heads": 2,
            "decoder_layers": 2, "decoder_attention_heads": 2, "max_source_positions": 1500,
            "max_target_positions": 448, "vocab_size": 1629
        })
    }

    #[test]
    fn a_random_tiny_model_runs_end_to_end() {
        let m = tiny_model("e2e", tiny_config());
        let w = Whisper::load(&m.dir, "tiny-random").unwrap();
        assert_eq!(w.languages(), vec!["en".to_string(), "de".to_string()]);
        // 50 s of tones with a pause: windows in more than one region
        let audio: Vec<f32> = (0..16_000 * 50)
            .map(|i| if (20 * 16_000..22 * 16_000).contains(&i) { 0.0 } else { (i as f32 * 0.37).sin() * 0.3 * ((i / 800) % 3) as f32 })
            .collect();
        let mut seen = Vec::new();
        let t = w
            .transcribe(&audio, &Options { language: None, diarize: true, max_speakers: 3 }, &mut |f, _| {
                seen.push(f);
                true
            })
            .unwrap();
        t.check().unwrap();
        let end = sample_tick(audio.len() as i64);
        assert!(t.words.iter().all(|x| x.end <= end));
        assert!(seen.windows(2).all(|p| p[0] <= p[1]), "progress goes backwards: {seen:?}");
        assert_eq!(seen.last(), Some(&1.0));
        assert!(seen.len() > 4, "progress is reported per window and step");
        // cancelling while transcribing
        let mut calls = 0;
        let r = w.transcribe(&audio, &Options::default(), &mut |_, _| {
            calls += 1;
            calls < 3
        });
        assert_eq!(r, Err(SpeechError::Cancelled));
        // an unknown language, empty and very short inputs
        assert!(w.transcribe(&audio, &Options { language: Some("xx".into()), ..Default::default() }, &mut |_, _| true).is_err());
        assert!(w.transcribe(&[], &Options::default(), &mut |_, _| true).unwrap().words.is_empty());
        assert!(w.transcribe(&[0.5; 100], &Options::default(), &mut |_, _| true).is_ok());
    }

    #[test]
    fn hostile_model_directories_are_errors() {
        let mut bad = tiny_config();
        bad["decoder_attention_heads"] = 3.into(); // does not divide d_model
        let m = tiny_model("heads", bad);
        assert!(Whisper::load(&m.dir, "x").is_err());
        let mut bad = tiny_config();
        bad["max_source_positions"] = 100.into();
        let m = tiny_model("src", bad);
        assert!(Whisper::load(&m.dir, "x").is_err());
        // weights shaped for another configuration
        let m = tiny_model("shape", tiny_config());
        let mut other = tiny_config();
        other["d_model"] = 32.into();
        std::fs::write(m.dir.join("config.json"), other.to_string()).unwrap();
        assert!(Whisper::load(&m.dir, "x").is_err());
        // a tokenizer whose special tokens lie outside the vocabulary, a truncated weight file,
        // a broken config
        let m = tiny_model("tok", tiny_config());
        let mut small = tiny_config();
        small["vocab_size"] = 200.into();
        std::fs::write(m.dir.join("config.json"), small.to_string()).unwrap();
        assert!(Whisper::load(&m.dir, "x").is_err());
        let m = tiny_model("trunc", tiny_config());
        let p = m.dir.join("model.safetensors");
        let b = std::fs::read(&p).unwrap();
        std::fs::write(&p, &b[..b.len() / 2]).unwrap();
        assert!(Whisper::load(&m.dir, "x").is_err());
        std::fs::write(m.dir.join("config.json"), "{").unwrap();
        assert!(Whisper::load(&m.dir, "x").is_err());
    }
}
