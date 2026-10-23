//! NVIDIA Parakeet TDT speech recognition in pure Rust (candle, CPU).
//!
//! Parakeet TDT 0.6B v3 is a FastConformer encoder with a Token-and-Duration Transducer decoder:
//! every step predicts a token *and* how many 80 ms encoder frames it covers, so each emitted
//! token comes with its own frame index. Word times therefore come straight from the decoder (no
//! attention alignment), which is what makes it a good fit for word-by-word captions. It writes
//! punctuation and capitals itself and recognises 25 European languages without being told which.
//!
//! Transcription:
//!
//! 1. Clips longer than [`MAX_CHUNK_S`] are cut into chunks at the quietest 10 ms frame of the last
//!    [`CUT_SEARCH_S`] seconds of each chunk, so cuts land in pauses. Attention is global within a
//!    chunk (memory grows with the square of its length).
//! 2. Each chunk's log-mel features ([`features`]) are encoded ([`model::Encoder`]).
//! 3. **Greedy TDT decoding**: at frame `t` the joint network scores tokens and durations; a
//!    non-blank token is emitted at `t` and fed to the LSTM prediction network; `t` advances by the
//!    predicted duration (at least one frame after a blank, and after
//!    `max_symbols_per_step` tokens on one frame). Each chunk starts from a fresh prediction
//!    network.
//! 4. SentencePiece pieces are joined into words at `▁`; a word spans its first token's frame to
//!    its last token's frame plus that token's duration.
//! 5. Word bounds are tightened past silent frames ([`crate::vad`]), and speakers labelled
//!    ([`crate::diarize`]) when asked.

mod features;
mod model;

use std::path::Path;

use candle_core::Tensor;
use filmcraft_project::{Transcript, Word};

use crate::{Options, ProgressFn, SpeechError, Transcriber, sample_tick};
use model::{Config, Model};

/// Longest chunk encoded at once, seconds.
pub const MAX_CHUNK_S: usize = 90;
/// A chunk is cut at the quietest point of its last this-many seconds.
pub const CUT_SEARCH_S: usize = 30;
/// Clips up to this long are encoded whole.
const WHOLE_S: usize = 120;
/// Audio samples per encoder frame (8 × 10 ms).
const SAMPLES_PER_FRAME: usize = 8 * features::HOP;

fn merr(e: candle_core::Error) -> SpeechError {
    SpeechError::Model(e.to_string())
}

/// SentencePiece vocabulary: id → piece.
struct Vocab {
    pieces: Vec<String>,
    /// special tokens (`<unk>`, `<|…|>`) that are never text
    special: Vec<bool>,
}

impl Vocab {
    fn from_json(s: &str, size: usize) -> Result<Self, SpeechError> {
        let v: serde_json::Value = serde_json::from_str(s).map_err(|e| SpeechError::Model(format!("tokenizer.json: {e}")))?;
        let mut pieces = vec![String::new(); size];
        let mut special = vec![false; size];
        let vocab = v["model"]["vocab"].as_object().ok_or_else(|| SpeechError::Model("tokenizer.json: no model.vocab".into()))?;
        for (p, id) in vocab {
            if let Some(slot) = id.as_u64().and_then(|i| pieces.get_mut(i as usize)) {
                *slot = p.clone();
            }
        }
        for a in v["added_tokens"].as_array().into_iter().flatten() {
            if let Some(i) = a["id"].as_u64().map(|i| i as usize)
                && let Some(slot) = special.get_mut(i)
            {
                *slot = a["special"].as_bool().unwrap_or(true);
            }
        }
        for (p, s) in pieces.iter().zip(special.iter_mut()) {
            // the blank (not in the tokenizer) and control tokens are never text
            if p.is_empty() || p.starts_with("<|") || p == "<unk>" || p == "<pad>" {
                *s = true;
            }
        }
        Ok(Self { pieces, special })
    }

    /// `(starts a word, text)` of a token; `None` for special tokens. Byte-fallback pieces
    /// (`<0x41>`) come back as `Err(byte)`.
    fn piece(&self, id: u32) -> Option<(bool, Result<String, u8>)> {
        let i = id as usize;
        if self.special.get(i).copied().unwrap_or(true) {
            return None;
        }
        let p = self.pieces.get(i)?;
        if let Some(hex) = p.strip_prefix("<0x").and_then(|h| h.strip_suffix('>')) {
            return u8::from_str_radix(hex, 16).ok().map(|b| (false, Err(b)));
        }
        let start = p.starts_with('\u{2581}');
        Some((start, Ok(p.replace('\u{2581}', " ").trim_start().to_string())))
    }
}

/// An emitted token: id, global encoder frame, duration in frames.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Emit {
    id: u32,
    frame: usize,
    dur: usize,
}

pub struct Parakeet {
    id: String,
    model: Model,
    vocab: Vocab,
}

impl Parakeet {
    /// Load a model directory (`config.json`, `tokenizer.json`, `model.safetensors`).
    pub fn load(dir: &Path, id: &str) -> Result<Self, SpeechError> {
        let read = |n: &str| std::fs::read(dir.join(n)).map_err(|e| SpeechError::Model(format!("{}: {e}", dir.join(n).display())));
        let cfg: Config = serde_json::from_slice(&read("config.json")?).map_err(|e| SpeechError::Model(format!("config.json: {e}")))?;
        if cfg.blank_token_id as usize >= cfg.vocab_size || cfg.durations.is_empty() || cfg.encoder_config.num_attention_heads == 0 {
            return Err(SpeechError::Model("config.json: inconsistent blank token, durations or heads".into()));
        }
        let vocab = Vocab::from_json(&String::from_utf8_lossy(&read("tokenizer.json")?), cfg.vocab_size)?;
        let tensors = crate::safetensors::load_f32(&dir.join("model.safetensors"), &candle_core::Device::Cpu)?;
        let model = Model::load(cfg, tensors).map_err(merr)?;
        Ok(Self { id: id.into(), model, vocab })
    }

    /// Chunk boundaries (sample indices) for `audio`.
    fn chunks(audio: &[f32]) -> Vec<(usize, usize)> {
        let sr = crate::SAMPLE_RATE as usize;
        if audio.len() <= WHOLE_S * sr {
            return vec![(0, audio.len())];
        }
        let db = crate::vad::frame_db(audio);
        let fs = sr / 100; // samples per 10 ms level frame
        let mut out = Vec::new();
        let mut start = 0;
        while audio.len() - start > MAX_CHUNK_S * sr {
            let (lo, hi) = ((start + (MAX_CHUNK_S - CUT_SEARCH_S) * sr) / fs, (start + MAX_CHUNK_S * sr) / fs);
            let quiet = (lo..hi.min(db.len())).min_by(|a, b| db[*a].total_cmp(&db[*b])).unwrap_or(hi);
            // cut on an encoder-frame boundary so frame times stay exact
            let cut = ((quiet * fs + fs / 2) / SAMPLES_PER_FRAME * SAMPLES_PER_FRAME).max(start + SAMPLES_PER_FRAME);
            out.push((start, cut));
            start = cut;
        }
        out.push((start, audio.len()));
        out
    }

    /// Greedy TDT decoding of one chunk's encoder frames (`t × j`, row-major).
    fn decode(&self, enc: &[f32], j: usize, frame0: usize, state: &mut model::State, g: &mut Vec<f32>, out: &mut Vec<Emit>) {
        let cfg = &self.model.cfg;
        let (vocab, blank) = (cfg.vocab_size, cfg.blank_token_id);
        let frames = enc.len() / j.max(1);
        let (mut t, mut on_frame) = (0usize, 0usize);
        while t < frames {
            let Some(e) = enc.get(t * j..(t + 1) * j) else { break };
            let logits = self.model.decoder.joint(e, g);
            let tok = model::argmax(logits.get(..vocab).unwrap_or_default()) as u32;
            let mut dur = cfg.durations.get(model::argmax(logits.get(vocab..).unwrap_or_default())).copied().unwrap_or(1);
            if tok != blank {
                out.push(Emit { id: tok, frame: frame0 + t, dur });
                *g = self.model.decoder.step(tok, state);
                on_frame += 1;
            } else if dur == 0 {
                dur = 1;
            }
            if dur == 0 && on_frame >= cfg.max_symbols_per_step {
                dur = 1;
            }
            if dur > 0 {
                on_frame = 0;
            }
            t += dur;
        }
    }
}

/// Group emitted tokens into timed words.
fn group_words(vocab: &Vocab, emits: &[Emit], total_frames: usize) -> Vec<Word> {
    struct Pending {
        bytes: Vec<u8>,
        first: usize,
        last_end: usize,
    }
    let mut words: Vec<Word> = Vec::new();
    let mut cur: Option<Pending> = None;
    let flush = |p: Option<Pending>, words: &mut Vec<Word>| {
        if let Some(p) = p {
            let text = String::from_utf8_lossy(&p.bytes).trim().to_string();
            if !text.is_empty() {
                let end = p.last_end.max(p.first + 1).min(total_frames.max(p.first + 1));
                words.push(Word::new(text, sample_tick((p.first * SAMPLES_PER_FRAME) as i64), sample_tick((end * SAMPLES_PER_FRAME) as i64)));
            }
        }
    };
    for e in emits {
        let Some((starts, text)) = vocab.piece(e.id) else { continue };
        if starts || cur.is_none() {
            flush(cur.take(), &mut words);
            cur = Some(Pending { bytes: Vec::new(), first: e.frame, last_end: e.frame });
        }
        if let Some(p) = cur.as_mut() {
            match text {
                Ok(s) => p.bytes.extend_from_slice(s.as_bytes()),
                Err(b) => p.bytes.push(b),
            }
            p.last_end = p.last_end.max(e.frame + e.dur.max(1));
        }
    }
    flush(cur.take(), &mut words);
    // words never overlap the next one
    for i in 1..words.len() {
        let next = words[i].start;
        if words[i - 1].end > next {
            words[i - 1].end = next.max(words[i - 1].start);
        }
    }
    words
}

/// A best guess at the language of `text` from common short words (Parakeet v3 does not report
/// the language it heard); English when nothing matches.
fn guess_language(text: &str) -> &'static str {
    const LANGS: &[(&str, &[&str])] = &[
        ("en", &["the", "and", "is", "you", "that", "of", "to", "it", "what", "this"]),
        ("es", &["el", "la", "que", "y", "los", "es", "por", "una", "con", "pero"]),
        ("fr", &["le", "la", "et", "les", "est", "que", "une", "pas", "je", "vous"]),
        ("de", &["der", "die", "und", "das", "ist", "nicht", "ich", "ein", "sie", "zu"]),
        ("it", &["il", "che", "di", "non", "è", "un", "per", "sono", "una", "gli"]),
        ("pt", &["o", "que", "não", "de", "um", "uma", "é", "com", "os", "você"]),
        ("nl", &["de", "het", "een", "en", "niet", "ik", "dat", "is", "van", "je"]),
        ("ru", &["и", "в", "не", "что", "на", "я", "это", "он", "с", "как"]),
        ("pl", &["i", "nie", "się", "to", "że", "w", "jest", "na", "jak", "co"]),
        ("uk", &["і", "не", "що", "це", "в", "на", "я", "як", "та", "з"]),
    ];
    let words: Vec<String> = text.split_whitespace().map(filmcraft_project::transcript::normalize_word).collect();
    LANGS
        .iter()
        .map(|(code, common)| (*code, words.iter().filter(|w| common.contains(&w.as_str())).count()))
        .fold(("en", 0), |best, c| if c.1 > best.1 { c } else { best })
        .0
}

impl Transcriber for Parakeet {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        if !progress(0.0, "Analysing audio") {
            return Err(SpeechError::Cancelled);
        }
        let trace = std::env::var_os("FILMCRAFT_SPEECH_TRACE").is_some();
        let clock = std::time::Instant::now();
        let chunks = Self::chunks(audio);
        let total = audio.len().max(1) as f32;
        let dec = &self.model.decoder;
        let mut emits = Vec::new();
        for (a, b) in chunks {
            let Some(chunk) = audio.get(a..b) else { continue };
            let (frames, feats) = features::features(chunk, self.model.n_mels());
            // the encoder needs at least a few frames to survive 8× subsampling
            if frames < 9 {
                continue;
            }
            let x = Tensor::from_vec(feats, (1, frames, self.model.n_mels()), &self.model.device).map_err(merr)?;
            let (f0, span) = (a as f32 / total, (b - a) as f32 / total);
            let mut cancelled = false;
            let enc = self
                .model
                .encoder
                .forward(&x, &mut |f| {
                    let p = f0 + span * f * 0.9;
                    cancelled = !progress(p * 0.95, &format!("Transcribing {:.0}%", p * 100.0));
                    !cancelled
                })
                .map_err(merr)?;
            let Some(enc) = enc else { return Err(SpeechError::Cancelled) };
            let (t, j) = enc.dims2().map_err(merr)?;
            let enc: Vec<f32> = enc.flatten_all().and_then(|e| e.to_vec1()).map_err(merr)?;
            if trace {
                eprintln!("chunk {:.1}s–{:.1}s: {t} frames encoded at {:.2}s", a as f64 / 16e3, b as f64 / 16e3, clock.elapsed().as_secs_f64());
            }
            // a fresh prediction network per chunk: carried across a cut, its state can leave the
            // decoder emitting blanks for seconds
            let mut state = dec.initial_state();
            let mut g = dec.step(self.model.cfg.blank_token_id, &mut state);
            self.decode(&enc, j, a / SAMPLES_PER_FRAME, &mut state, &mut g, &mut emits);
            if trace {
                eprintln!("  decoded at {:.2}s", clock.elapsed().as_secs_f64());
            }
            if !progress((f0 + span) * 0.95, "Transcribing") {
                return Err(SpeechError::Cancelled);
            }
        }
        let total_frames = audio.len().div_ceil(SAMPLES_PER_FRAME);
        let mut t = Transcript { source: self.id.clone(), words: group_words(&self.vocab, &emits, total_frames), ..Default::default() };
        t.language = opts.language.clone().filter(|l| !l.is_empty() && l != "auto").unwrap_or_else(|| guess_language(&t.text()).to_string());
        crate::vad::tighten_words(audio, &mut t.words);
        if opts.diarize && !t.words.is_empty() {
            if !progress(0.96, "Identifying speakers") {
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

    fn vocab() -> Vocab {
        let json = r#"{"model":{"vocab":{"<unk>":0,"<|pnc|>":1,"▁Hel":2,"lo":3,",":4,"▁wor":5,"ld":6,".":7,"<0xC3>":8,"<0xA9>":9,"▁caf":10}},
            "added_tokens":[{"id":0,"content":"<unk>","special":true},{"id":1,"content":"<|pnc|>","special":true}]}"#;
        Vocab::from_json(json, 12).unwrap()
    }

    #[test]
    fn pieces_join_into_timed_words() {
        let v = vocab();
        assert!(v.piece(1).is_none());
        assert!(v.piece(11).is_none(), "the blank is not text");
        let e = |id, frame, dur| Emit { id, frame, dur };
        let emits = [e(1, 0, 0), e(2, 2, 1), e(3, 3, 2), e(4, 5, 0), e(5, 7, 1), e(6, 8, 1), e(7, 9, 1), e(10, 12, 1), e(8, 13, 0), e(9, 13, 1)];
        let words = group_words(&v, &emits, 100);
        let text: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(text, ["Hello,", "world.", "café"]);
        let f = |n: i64| sample_tick(n * SAMPLES_PER_FRAME as i64);
        assert_eq!((words[0].start, words[0].end), (f(2), f(6)));
        assert_eq!((words[1].start, words[1].end), (f(7), f(10)));
        assert_eq!((words[2].start, words[2].end), (f(12), f(14)));
    }

    #[test]
    fn long_clips_are_cut_in_pauses() {
        let sr = 16_000usize;
        // 200 s of tone with a silent gap at 75 s
        let mut a: Vec<f32> = (0..200 * sr).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        for v in &mut a[75 * sr..75 * sr + sr / 2] {
            *v = 0.0;
        }
        let c = Parakeet::chunks(&a);
        assert_eq!(c.first().map(|c| c.0), Some(0));
        assert_eq!(c.last().map(|c| c.1), Some(a.len()));
        assert!(c.windows(2).all(|w| w[0].1 == w[1].0));
        assert!(c.iter().all(|(s, e)| e - s <= MAX_CHUNK_S * sr && s % SAMPLES_PER_FRAME == 0));
        let cut = c[0].1 as f32 / sr as f32;
        assert!((74.9..75.5).contains(&cut), "cut at {cut}");
        assert_eq!(Parakeet::chunks(&a[..60 * sr]), vec![(0, 60 * sr)]);
        assert_eq!(Parakeet::chunks(&[]), vec![(0, 0)]);
    }

    #[test]
    fn language_guess() {
        assert_eq!(guess_language("So what is the plan for this one?"), "en");
        assert_eq!(guess_language("Je ne sais pas, mais vous et les autres"), "fr");
        assert_eq!(guess_language("Ich weiß nicht, und das ist gut"), "de");
        assert_eq!(guess_language(""), "en");
    }
}
