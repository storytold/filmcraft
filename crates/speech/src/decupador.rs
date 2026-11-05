//! The Decupador Inteligente transcriber: speech-to-text through the studio's own pipeline
//! (`src/transcriber.py` of the decupador project: faster-whisper with word timestamps and
//! per-word probabilities), instead of the built-in candle Whisper.
//!
//! The decupador lives outside this repository. [`Config`] says where (`home`), which Python runs
//! it and which model size to use; the engine reads it from `<data dir>/decupador.json`
//! (`FILMCRAFT_DECUPADOR_HOME` overrides `home`). The audio goes to the decupador as a 16 kHz mono
//! WAV in a temporary folder, and its `transcript.json` comes back as a [`Transcript`]. The word
//! times are the decupador's own (they are what its cut plans are written against), so nothing
//! here re-aligns or tightens them.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use filmcraft_project::{Speaker, Transcript, Word};
use serde::Deserialize;

use crate::{Options, ProgressFn, SAMPLE_RATE, SpeechError, Transcriber, seconds_tick};

/// Model id stored in the transcripts this transcriber makes (`decupador-small`, …).
pub const ID_PREFIX: &str = "decupador";

/// The bridge run by Python: imports the decupador's own transcriber and writes its
/// `transcript.json` into the output folder.
const BRIDGE: &str = r#"import sys
from pathlib import Path
import json
home, wav, out, model, language, options = sys.argv[1:7]
sys.path.insert(0, home)
from src.transcriber import transcribe_audio
opts = json.loads(options) or None
try:
    print(transcribe_audio(Path(wav), Path(out), model_size=model, language=language, force=True, whisper_options=opts))
except TypeError:
    # a decupador without `whisper_options`
    print(transcribe_audio(Path(wav), Path(out), model_size=model, language=language, force=True))
"#;

/// Reads a script (PDF, Markdown or text) with the decupador's own reader and splits it into
/// blocks (`src/multitake.py`: Markdown headings, else paragraphs); prints them as JSON.
const SCRIPT_BRIDGE: &str = r#"import sys, json
from pathlib import Path
home, path = sys.argv[1:3]
sys.path.insert(0, home)
from src.multitake import read_script_text, split_script_blocks
print(json.dumps([{"title": b["titulo"], "text": b["texto"]} for b in split_script_blocks(read_script_text(Path(path)))], ensure_ascii=False))
"#;

/// Where the decupador is and how to run it (`decupador.json`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Config {
    /// The decupador project folder (the one with `src/transcriber.py` and `models/`).
    pub home: PathBuf,
    /// The Python interpreter that has `faster-whisper` installed.
    #[serde(default = "default_python")]
    pub python: String,
    /// faster-whisper model size (`small`: the decupador's default).
    #[serde(default = "default_model")]
    pub model: String,
    /// Language when the request names none.
    #[serde(default = "default_language")]
    pub language: String,
    /// Extra faster-whisper options (`whisperOptions`). `{"vad_filter": true,
    /// "condition_on_previous_text": false}` keeps long sessions from looping on one word and
    /// then skipping minutes of speech.
    #[serde(default, rename = "whisperOptions")]
    pub whisper_options: serde_json::Value,
}

fn default_python() -> String {
    "python3".into()
}
fn default_model() -> String {
    "small".into()
}
fn default_language() -> String {
    "pt".into()
}

impl Config {
    /// Read `path` (`decupador.json`); `FILMCRAFT_DECUPADOR_HOME` overrides `home` (and alone is
    /// enough). `None` when neither says where the decupador is, or it has no transcriber.
    pub fn load(path: &Path) -> Option<Config> {
        let mut cfg = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Config>(&t).ok());
        if let Some(home) = std::env::var_os("FILMCRAFT_DECUPADOR_HOME").filter(|h| !h.is_empty()) {
            let c = cfg.get_or_insert_with(|| Config {
                home: PathBuf::new(),
                python: default_python(),
                model: default_model(),
                language: default_language(),
                whisper_options: serde_json::Value::Null,
            });
            c.home = PathBuf::from(home);
        }
        cfg.filter(|c| c.home.join("src").join("transcriber.py").is_file())
    }
}

/// Speech-to-text through the decupador's faster-whisper pipeline.
pub struct Decupador {
    pub config: Config,
}

impl Decupador {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

/// `transcript.json` as the decupador writes it (only what is read here).
#[derive(Deserialize)]
struct Output {
    #[serde(default)]
    language: String,
    #[serde(default)]
    segments: Vec<Segment>,
}

#[derive(Deserialize)]
struct Segment {
    #[serde(default)]
    words: Vec<OutWord>,
}

#[derive(Deserialize)]
struct OutWord {
    word: String,
    start: f64,
    end: f64,
    #[serde(default)]
    prob: Option<f32>,
}

/// Write mono 16 kHz samples as a 16-bit PCM WAV.
fn write_wav(path: &Path, audio: &[f32]) -> std::io::Result<()> {
    let data_len = (audio.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + audio.len() * 2);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    b.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in audio {
        b.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes());
    }
    std::fs::File::create(path)?.write_all(&b)
}

/// The decupador's output as a FilmCraft transcript (one speaker: the decupador labels none).
fn to_transcript(out: Output, fallback_language: &str, source: String) -> Transcript {
    let mut words = Vec::new();
    for w in out.segments.into_iter().flat_map(|s| s.words) {
        let text = w.word.trim();
        if text.is_empty() || !w.start.is_finite() || !w.end.is_finite() {
            continue;
        }
        let start = seconds_tick(w.start.max(0.0));
        let end = seconds_tick(w.end.max(w.start)).max(start + crate::sample_tick(1));
        let mut word = Word::new(text, start, end);
        word.speaker = Some(0);
        word.confidence = w.prob.unwrap_or(1.0).clamp(0.0, 1.0);
        words.push(word);
    }
    let language = if out.language.is_empty() { fallback_language.to_string() } else { out.language };
    Transcript { language, source, speakers: vec![Speaker { name: "Speaker 1".into() }], words }
}

impl Decupador {
    /// The blocks of a script file, read by the decupador (`(title, text)`, in order).
    pub fn read_script(&self, path: &Path) -> Result<Vec<(String, String)>, SpeechError> {
        let out = Command::new(&self.config.python)
            .arg("-c")
            .arg(SCRIPT_BRIDGE)
            .arg(&self.config.home)
            .arg(path)
            .current_dir(&self.config.home)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| SpeechError::Unavailable(format!("could not run the decupador with `{}`: {e}", self.config.python)))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().rev().take(4).collect();
            return Err(SpeechError::Model(format!("the decupador could not read the script: {}", tail.into_iter().rev().collect::<Vec<_>>().join("\n"))));
        }
        #[derive(Deserialize)]
        struct Block {
            title: String,
            text: String,
        }
        let blocks: Vec<Block> = serde_json::from_slice(&out.stdout).map_err(|e| SpeechError::Model(format!("script blocks: {e}")))?;
        Ok(blocks.into_iter().map(|b| (b.title, b.text)).collect())
    }
}

impl Transcriber for Decupador {
    fn id(&self) -> String {
        format!("{ID_PREFIX}-{}", self.config.model)
    }

    fn transcribe(&self, audio: &[f32], opts: &Options, progress: ProgressFn) -> Result<Transcript, SpeechError> {
        if !progress(0.0, "Preparing audio for the decupador") {
            return Err(SpeechError::Cancelled);
        }
        static RUN: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("filmcraft-decupador-{}-{}", std::process::id(), RUN.fetch_add(1, Ordering::Relaxed)));
        let out_dir = dir.join("out");
        std::fs::create_dir_all(&out_dir)?;
        let result = (|| {
            let wav = dir.join("audio.wav");
            write_wav(&wav, audio)?;
            let script = dir.join("bridge.py");
            std::fs::write(&script, BRIDGE)?;
            let language = opts.language.clone().unwrap_or_else(|| self.config.language.clone());
            let mut child = Command::new(&self.config.python)
                .arg(&script)
                .arg(&self.config.home)
                .arg(&wav)
                .arg(&out_dir)
                .arg(&self.config.model)
                .arg(&language)
                .arg(if self.config.whisper_options.is_object() { self.config.whisper_options.to_string() } else { "{}".into() })
                .current_dir(&self.config.home)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| SpeechError::Unavailable(format!("could not run the decupador with `{}`: {e}", self.config.python)))?;
            // faster-whisper `small` on a laptop CPU runs at roughly 4× real time: the estimate only
            // drives the progress bar.
            let audio_secs = audio.len() as f64 / f64::from(SAMPLE_RATE);
            let estimate = (audio_secs / 4.0).max(5.0);
            let started = Instant::now();
            let status = loop {
                if let Some(st) = child.try_wait()? {
                    break st;
                }
                let f = (started.elapsed().as_secs_f64() / estimate).min(0.95) as f32;
                if !progress(0.05 + f * 0.9, "Transcribing with the decupador") {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SpeechError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(200));
            };
            if !status.success() {
                let mut err = String::new();
                if let Some(mut e) = child.stderr.take() {
                    let _ = std::io::Read::read_to_string(&mut e, &mut err);
                }
                let tail: String = err.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                return Err(SpeechError::Model(format!("the decupador transcriber failed ({status}): {tail}")));
            }
            let text = std::fs::read_to_string(out_dir.join("transcript.json"))?;
            let out: Output = serde_json::from_str(&text).map_err(|e| SpeechError::Model(format!("transcript.json: {e}")))?;
            Ok(to_transcript(out, &language, self.id()))
        })();
        let _ = std::fs::remove_dir_all(&dir);
        let t = result?;
        progress(1.0, "Done");
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_becomes_a_transcript() {
        let out: Output = serde_json::from_str(
            r#"{"language":"pt","segments":[{"words":[{"word":" Luz,","start":3.5,"end":3.9,"prob":0.8},{"word":"ação!","start":4.18,"end":4.92,"prob":0.99},{"word":"  ","start":5.0,"end":5.1}]}]}"#,
        )
        .unwrap();
        let t = to_transcript(out, "en", "decupador-small".into());
        assert_eq!(t.language, "pt");
        assert_eq!(t.source, "decupador-small");
        assert_eq!(t.words.len(), 2);
        assert_eq!(t.words[0].text, "Luz,");
        assert_eq!(t.words[1].start, seconds_tick(4.18));
        assert_eq!(t.words[1].end, seconds_tick(4.92));
        assert!((t.words[0].confidence - 0.8).abs() < 1e-6);
        assert_eq!(t.words[0].speaker, Some(0));
    }

    #[test]
    fn zero_length_words_get_one_sample() {
        let out: Output = serde_json::from_str(r#"{"segments":[{"words":[{"word":"é","start":1.0,"end":1.0}]}]}"#).unwrap();
        let t = to_transcript(out, "pt", "x".into());
        assert_eq!(t.language, "pt");
        assert!(t.words[0].end > t.words[0].start);
    }

    #[test]
    fn wav_header_is_well_formed() {
        let dir = std::env::temp_dir().join(format!("fc-decupador-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.wav");
        write_wav(&p, &[0.0, 1.0, -1.0]).unwrap();
        let b = std::fs::read(&p).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(b.len(), 44 + 6);
        assert_eq!(i16::from_le_bytes([b[46], b[47]]), 32767);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_needs_the_transcriber() {
        let dir = std::env::temp_dir().join(format!("fc-decupador-cfg-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("home/src")).unwrap();
        let cfg = dir.join("decupador.json");
        std::fs::write(&cfg, format!(r#"{{"home": {:?}}}"#, dir.join("home"))).unwrap();
        assert_eq!(Config::load(&cfg), None);
        std::fs::write(dir.join("home/src/transcriber.py"), "").unwrap();
        let c = Config::load(&cfg).unwrap();
        assert_eq!((c.python.as_str(), c.model.as_str(), c.language.as_str()), ("python3", "small", "pt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
