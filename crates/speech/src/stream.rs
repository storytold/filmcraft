//! Live recognition: words while the audio is still arriving.
//!
//! [`StreamingRecognizer`] is what a caller feeds audio to (mono 16 kHz, in blocks of any size)
//! and polls for words. Word times count from the first sample pushed. Two implementations:
//!
//! - [`ScriptedRecognizer`] emits given words when the audio pushed reaches given times: tests and
//!   agents use it to drive everything above the recogniser without a model, in every build.
//! - [`RollingRecognizer`] runs any [`Transcriber`] (the Whisper models) over a rolling window of
//!   the most recent audio on a worker thread and keeps the words consecutive passes agree on.
//!   A recogniser built for streaming can implement the same trait later.
//!
//! **Rolling window.** A pass is due every [`RollingConfig::hop`] of audio. It transcribes the
//! audio from just before the last committed word (at most [`RollingConfig::window`] back) to now.
//! One pass runs at a time; when a pass takes longer than the hop, the passes that fell due
//! meanwhile are skipped, not queued (counted in [`StreamStats::skipped`]), and the next pass
//! takes the newest audio. The buffer holds at most [`MAX_BUFFER_SECONDS`], whatever the take's
//! length.
//!
//! **Agreement** ([`Agreement`]). Each pass yields a hypothesis: words with times. Words that end
//! before the last committed word (less a tolerance) or repeat the committed tail are dropped. Of
//! the rest, the longest prefix equal (case and punctuation aside) to what the previous pass left
//! uncommitted is committed; what follows is tentative. Committed words are never retracted and
//! never go back in time. A hypothesis that changes from pass to pass commits nothing.
//!
//! **Silence.** Speech models invent words over silence and noise. Each 10 ms frame of the window
//! is voiced when it is louder than both [`ABS_FLOOR_DB`] and the window's own noise floor (its
//! 10th-percentile level) plus [`ABOVE_FLOOR_DB`]. A pass runs only when the window holds at least
//! [`MIN_VOICED_FRAMES`] voiced frames, and when the new audio has some or tentative words are
//! waiting (that last pass over a finished phrase is what commits its end). A silent window drops
//! the tentative words. A word is kept only when voiced frames lie under it, give or take 100 ms.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use filmcraft_project::Word;
use filmcraft_project::transcript::normalize_word;
use filmcraft_time::Tick;
use serde::Serialize;

use crate::{Options, SAMPLE_RATE, SpeechError, Transcriber, sample_tick};

/// What a recogniser is doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "state", content = "reason")]
pub enum StreamStatus {
    /// Loading its model.
    Starting,
    /// Taking audio and producing words.
    Listening,
    /// It cannot work (the model failed to load, the worker could not start): the reason.
    Failed(String),
    /// [`StreamingRecognizer::stop`] was called.
    Stopped,
}

/// What [`StreamingRecognizer::poll`] returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamUpdate {
    /// Words committed since the last poll, in order. They are final.
    pub committed: Vec<Word>,
    /// The words heard after the committed ones that are not final yet (replaces the last poll's).
    pub tentative: Vec<Word>,
    /// The language being recognised, once known (ISO 639-1).
    pub language: Option<String>,
}

/// Counters for checking that a recogniser keeps up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStats {
    /// Passes run.
    pub passes: u64,
    /// Passes that fell due while one was still running and were skipped.
    pub skipped: u64,
    /// Passes not run because the window held no speech.
    pub silent: u64,
    /// Seconds of audio pushed.
    pub audio_seconds: f64,
    /// Wall-clock seconds spent in passes, and the longest pass.
    pub busy_seconds: f64,
    pub longest_pass_seconds: f64,
    /// Committed words not collected by `poll` before the queue filled, dropped oldest first.
    pub dropped_words: u64,
}

/// A recogniser fed while the audio is being captured.
pub trait StreamingRecognizer: Send {
    /// The model or recogniser id (`"whisper-base"`, `"scripted"`).
    fn id(&self) -> String;
    /// Take more audio: mono, [`SAMPLE_RATE`]. Never blocks for longer than a lock.
    fn push(&mut self, audio: &[f32]);
    /// The words committed since the last call and the current tentative ones.
    fn poll(&mut self) -> StreamUpdate;
    fn status(&self) -> StreamStatus;
    fn stats(&self) -> StreamStats {
        StreamStats::default()
    }
    /// Forget all audio and words: the stream starts again at time zero.
    fn reset(&mut self);
    /// Stop taking audio and producing words. Returns at once; a pass in progress is abandoned.
    fn stop(&mut self);
}

// ------------------------------------------------------------------------------------- scripted

/// One word of a [`ScriptedRecognizer`]: the word (times from the first sample), when it shows as
/// tentative and when it is committed (audio pushed so far, same clock).
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptedWord {
    pub word: Word,
    pub tentative_at: Option<Tick>,
    pub commit_at: Tick,
}

impl ScriptedWord {
    /// `text` spoken from `start` to `end` seconds, committed `lag` seconds after it ends.
    pub fn spoken(text: &str, start: f64, end: f64, lag: f64) -> Self {
        let tick = crate::seconds_tick;
        Self { word: Word::new(text, tick(start), tick(end)), tentative_at: Some(tick(end)), commit_at: tick(end + lag) }
    }
}

/// Emits given words at given times of the audio pushed: no model, deterministic.
pub struct ScriptedRecognizer {
    id: String,
    words: Vec<ScriptedWord>,
    next: usize,
    pushed: i64,
    stopped: bool,
    language: Option<String>,
}

impl ScriptedRecognizer {
    pub fn new(id: impl Into<String>, words: Vec<ScriptedWord>) -> Self {
        Self { id: id.into(), words, next: 0, pushed: 0, stopped: false, language: Some("en".into()) }
    }

    /// `text`'s words read one every `seconds_per_word` from `start`, each committed `lag` later.
    pub fn reading(text: &str, start: f64, seconds_per_word: f64, lag: f64) -> Vec<ScriptedWord> {
        text.split_whitespace()
            .enumerate()
            .map(|(i, w)| {
                let a = start + i as f64 * seconds_per_word;
                ScriptedWord::spoken(w, a, a + seconds_per_word * 0.8, lag)
            })
            .collect()
    }

    fn now(&self) -> Tick {
        sample_tick(self.pushed)
    }
}

impl StreamingRecognizer for ScriptedRecognizer {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn push(&mut self, audio: &[f32]) {
        if !self.stopped {
            self.pushed = self.pushed.saturating_add(audio.len() as i64);
        }
    }
    fn poll(&mut self) -> StreamUpdate {
        let now = self.now();
        let mut committed = Vec::new();
        while let Some(w) = self.words.get(self.next).filter(|w| w.commit_at <= now && !self.stopped) {
            committed.push(w.word.clone());
            self.next += 1;
        }
        let tentative = self.words[self.next.min(self.words.len())..]
            .iter()
            .take_while(|w| w.tentative_at.is_some_and(|t| t <= now) && !self.stopped)
            .map(|w| w.word.clone())
            .collect();
        StreamUpdate { committed, tentative, language: self.language.clone() }
    }
    fn status(&self) -> StreamStatus {
        if self.stopped { StreamStatus::Stopped } else { StreamStatus::Listening }
    }
    fn reset(&mut self) {
        self.next = 0;
        self.pushed = 0;
    }
    fn stop(&mut self) {
        self.stopped = true;
    }
}

// ------------------------------------------------------------------------------------- agreement

/// Hypothesis words looked at per pass (a pass over a few seconds has far fewer).
pub const MAX_HYPOTHESIS_WORDS: usize = 400;
/// Characters kept of one word.
const MAX_WORD_CHARS: usize = 64;
/// A hypothesis word ending this long before the last committed word's end is already behind us.
const OVERLAP_TOLERANCE_SECONDS: f64 = 0.25;
/// Committed words remembered to recognise a pass repeating them.
const TAIL_WORDS: usize = 8;

/// The local-agreement rule over consecutive passes (see the module docs). Pure: no audio, no
/// threads; times are absolute (from the first sample of the stream).
#[derive(Clone, Debug, Default)]
pub struct Agreement {
    /// End of the last committed word.
    committed_end: Tick,
    /// Normalised text of the last committed words.
    tail: VecDeque<String>,
    /// What the previous pass left uncommitted.
    pending: Vec<Word>,
}

impl Agreement {
    pub fn new() -> Self {
        Self::default()
    }

    /// End of the last committed word (zero before any).
    pub fn committed_end(&self) -> Tick {
        self.committed_end
    }

    /// The words the last pass left tentative.
    pub fn tentative(&self) -> &[Word] {
        &self.pending
    }

    /// Nothing is being said: drop the tentative words (they are not confirmed by silence).
    pub fn clear_tentative(&mut self) {
        self.pending.clear();
    }

    /// Take one pass's hypothesis; returns the words it commits. [`Agreement::tentative`] then
    /// holds the rest.
    pub fn pass(&mut self, hypothesis: &[Word]) -> Vec<Word> {
        let fresh = self.fresh(hypothesis);
        let agreed = fresh.iter().zip(&self.pending).take_while(|(a, b)| normalize_word(&a.text) == normalize_word(&b.text)).count();
        let mut committed = Vec::with_capacity(agreed);
        for w in fresh.iter().take(agreed) {
            let start = w.start.max(self.committed_end);
            let end = w.end.max(start);
            let mut w = Word::new(w.text.clone(), start, end);
            w.confidence = 1.0;
            self.committed_end = end;
            self.tail.push_back(normalize_word(&w.text));
            if self.tail.len() > TAIL_WORDS {
                self.tail.pop_front();
            }
            committed.push(w);
        }
        self.pending = fresh.into_iter().skip(agreed).collect();
        committed
    }

    /// The hypothesis words after what is committed: cleaned, in time order, without the ones
    /// already behind the commit point or repeating the committed tail.
    fn fresh(&self, hypothesis: &[Word]) -> Vec<Word> {
        let tolerance = crate::seconds_tick(OVERLAP_TOLERANCE_SECONDS);
        let mut out: Vec<Word> = Vec::new();
        for w in hypothesis.iter().take(MAX_HYPOTHESIS_WORDS) {
            let text: String = w.text.trim().chars().take(MAX_WORD_CHARS).collect();
            if normalize_word(&text).is_empty() || w.start < Tick::ZERO {
                continue;
            }
            // words never go back in time
            if out.last().is_some_and(|p| w.start < p.start) {
                continue;
            }
            if w.end.max(w.start) <= self.committed_end.saturating_sub(tolerance) {
                continue;
            }
            out.push(Word::new(text, w.start, w.end.max(w.start)));
        }
        // a pass that starts by repeating the last committed words
        let norms: Vec<String> = out.iter().take(TAIL_WORDS).map(|w| normalize_word(&w.text)).collect();
        let tail: Vec<&String> = self.tail.iter().collect();
        let repeat = (1..=norms.len().min(tail.len())).rev().find(|&k| norms[..k].iter().zip(&tail[tail.len() - k..]).all(|(a, b)| a == *b)).unwrap_or(0);
        out.drain(..repeat);
        out
    }
}

// ------------------------------------------------------------------------------------- silence

/// The quietest level a voiced frame can have (dBFS, 10 ms RMS).
pub const ABS_FLOOR_DB: f32 = -50.0;
/// How far above the window's noise floor a voiced frame is (dB).
pub const ABOVE_FLOOR_DB: f32 = 10.0;
/// Voiced frames (10 ms) a window needs before a pass is run over it.
pub const MIN_VOICED_FRAMES: usize = 15;
/// Voiced frames a word needs under it to be kept.
const MIN_WORD_VOICED_FRAMES: usize = 3;
/// Samples per voicing frame.
const FRAME: usize = (SAMPLE_RATE / 100) as usize;

/// Which 10 ms frames of `audio` hold voice (see the module docs).
pub fn voiced_frames(audio: &[f32]) -> Vec<bool> {
    let db = crate::vad::frame_db(audio);
    let mut sorted: Vec<f32> = db.iter().copied().filter(|v| v.is_finite()).collect();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted.get(sorted.len() / 10).copied().unwrap_or(-120.0);
    let threshold = ABS_FLOOR_DB.max(floor + ABOVE_FLOOR_DB);
    db.iter().map(|v| *v > threshold).collect()
}

/// Whether a word (times relative to the first sample of `voiced`) has voice under it.
fn word_is_voiced(voiced: &[bool], w: &Word) -> bool {
    let frame = crate::TICKS_PER_SAMPLE * FRAME as i64;
    let margin = 10; // 100 ms
    let a = (w.start.0 / frame).saturating_sub(margin).max(0) as usize;
    let b = ((w.end.0 / frame).saturating_add(margin + 1)).max(0) as usize;
    voiced.get(a.min(voiced.len())..b.min(voiced.len())).is_some_and(|f| f.iter().filter(|v| **v).count() >= MIN_WORD_VOICED_FRAMES)
}

// ------------------------------------------------------------------------------------- rolling

/// Audio the rolling recogniser keeps (seconds): more than any window.
pub const MAX_BUFFER_SECONDS: f64 = 30.0;
/// Committed words held for `poll` (older ones are dropped and counted).
const MAX_QUEUED_WORDS: usize = 10_000;

/// Window and hop of a [`RollingRecognizer`], in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct RollingConfig {
    /// The longest stretch of audio one pass transcribes.
    pub window: f64,
    /// A pass falls due every this much new audio.
    pub hop: f64,
    /// Audio before the last committed word included in a pass, as context.
    pub context: f64,
    /// ISO 639-1 code; None: detected by the first pass that hears words, then kept.
    pub language: Option<String>,
}

impl Default for RollingConfig {
    /// A window of up to 10 seconds and a pass every 1.5 seconds. A pass covers the audio since
    /// just before the last committed word, so it is usually 3 to 5 seconds long; the 10-second
    /// bound is reached only when passes keep disagreeing (a 6-second bound cut phrases off before
    /// they were committed in the measurements recorded in the crate README). With Whisper tiny on
    /// a laptop CPU a pass takes under a second, inside the hop; base takes longer than the hop on
    /// the same machine, so it skips passes and its words arrive later.
    fn default() -> Self {
        Self { window: 10.0, hop: 1.5, context: 1.0, language: None }
    }
}

/// Loads the transcriber on the worker thread, so a take never waits for a model on the UI thread.
pub type Loader = Box<dyn FnOnce() -> Result<Arc<dyn Transcriber>, SpeechError> + Send>;

struct Inner {
    /// The most recent audio, from absolute sample `start`.
    audio: VecDeque<f32>,
    start: i64,
    /// Samples pushed in all.
    total: i64,
    /// `total` at which the next pass falls due, and the samples between passes.
    due: i64,
    hop: i64,
    /// Bumped by `reset`: a pass that started before it is thrown away.
    generation: u64,
    committed: VecDeque<Word>,
    tentative: Vec<Word>,
    language: Option<String>,
    status: StreamStatus,
    stats: StreamStats,
    stop: bool,
    running: bool,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A [`Transcriber`] over a rolling window on a worker thread (see the module docs).
pub struct RollingRecognizer {
    id: String,
    shared: Arc<Shared>,
}

impl RollingRecognizer {
    /// Start the worker; it loads the model with `load` first ([`StreamStatus::Starting`]).
    pub fn new(id: impl Into<String>, load: Loader, config: RollingConfig) -> Self {
        let hop = (config.hop.clamp(0.1, 30.0) * f64::from(SAMPLE_RATE)) as i64;
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                audio: VecDeque::new(),
                start: 0,
                total: 0,
                due: hop,
                hop,
                generation: 0,
                committed: VecDeque::new(),
                tentative: Vec::new(),
                language: config.language.clone(),
                status: StreamStatus::Starting,
                stats: StreamStats::default(),
                stop: false,
                running: true,
            }),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new().name("speech-follow".into()).spawn(move || run(&worker, load, &config));
        if let Err(e) = spawned {
            let mut g = shared.lock();
            g.status = StreamStatus::Failed(format!("the speech worker could not start: {e}"));
            g.running = false;
        }
        Self { id: id.into(), shared }
    }

    /// Whether the worker thread is still alive (it ends soon after [`StreamingRecognizer::stop`]).
    pub fn worker_running(&self) -> bool {
        self.shared.lock().running
    }
}

impl Drop for RollingRecognizer {
    fn drop(&mut self) {
        self.stop();
    }
}

impl StreamingRecognizer for RollingRecognizer {
    fn id(&self) -> String {
        self.id.clone()
    }

    fn push(&mut self, audio: &[f32]) {
        let mut g = self.shared.lock();
        if g.stop {
            return;
        }
        g.audio.extend(audio.iter().map(|v| if v.is_finite() { *v } else { 0.0 }));
        g.total = g.total.saturating_add(audio.len() as i64);
        g.stats.audio_seconds = g.total as f64 / f64::from(SAMPLE_RATE);
        let cap = (MAX_BUFFER_SECONDS * f64::from(SAMPLE_RATE)) as usize;
        if g.audio.len() > cap {
            let over = g.audio.len() - cap;
            g.audio.drain(..over);
            g.start += over as i64;
        }
        drop(g);
        self.shared.wake.notify_all();
    }

    fn poll(&mut self) -> StreamUpdate {
        let mut g = self.shared.lock();
        StreamUpdate { committed: g.committed.drain(..).collect(), tentative: g.tentative.clone(), language: g.language.clone() }
    }

    fn status(&self) -> StreamStatus {
        self.shared.lock().status.clone()
    }

    fn stats(&self) -> StreamStats {
        self.shared.lock().stats
    }

    fn reset(&mut self) {
        let mut g = self.shared.lock();
        g.audio.clear();
        g.start = 0;
        g.total = 0;
        g.due = g.hop;
        g.generation += 1;
        g.committed.clear();
        g.tentative.clear();
        drop(g);
        self.shared.wake.notify_all();
    }

    fn stop(&mut self) {
        let mut g = self.shared.lock();
        g.stop = true;
        if !matches!(g.status, StreamStatus::Failed(_)) {
            g.status = StreamStatus::Stopped;
        }
        g.tentative.clear();
        drop(g);
        self.shared.wake.notify_all();
    }
}

/// One pass's input, copied out of the lock.
struct Job {
    generation: u64,
    /// Absolute sample of `audio[0]`.
    start: i64,
    audio: Vec<f32>,
    /// Where the audio not heard by the previous pass starts, relative to `start`.
    fresh_from: usize,
    language: Option<String>,
}

fn run(shared: &Shared, load: Loader, config: &RollingConfig) {
    let sr = f64::from(SAMPLE_RATE);
    let hop = (config.hop.clamp(0.1, 30.0) * sr) as i64;
    let window = (config.window.clamp(1.0, MAX_BUFFER_SECONDS) * sr) as i64;
    let context = (config.context.clamp(0.0, 5.0) * sr) as i64;
    let model = match load() {
        Ok(m) => m,
        Err(e) => {
            let mut g = shared.lock();
            g.status = StreamStatus::Failed(e.to_string());
            g.running = false;
            return;
        }
    };
    {
        let mut g = shared.lock();
        if g.stop {
            g.running = false;
            return;
        }
        g.status = StreamStatus::Listening;
    }
    let mut agreement = Agreement::new();
    let mut generation = 0;
    let mut last_end = 0i64;
    loop {
        let job = {
            let mut g = shared.lock();
            while !g.stop && g.total < g.due {
                g = shared.wake.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            if g.stop {
                break;
            }
            if g.generation != generation {
                generation = g.generation;
                agreement = Agreement::new();
                last_end = 0;
            }
            let end = g.total;
            let committed = agreement.committed_end().0 / crate::TICKS_PER_SAMPLE;
            let from = (end - window).max(committed - context).max(g.start).min(end);
            let audio: Vec<f32> = g.audio.range((from - g.start) as usize..).copied().collect();
            let fresh_from = (last_end - from).clamp(0, audio.len() as i64) as usize;
            last_end = end;
            Job { generation, start: from, audio, fresh_from, language: g.language.clone() }
        };
        let began = Instant::now();
        let outcome = pass(shared, model.as_ref(), &mut agreement, &job);
        let took = began.elapsed().as_secs_f64();
        let mut g = shared.lock();
        if g.stop {
            break;
        }
        // passes that fell due while this one ran are skipped, not queued
        let missed = ((g.total - g.due) / hop).max(0);
        g.due = g.due.saturating_add(hop.saturating_mul(missed + 1));
        g.stats.skipped += missed as u64;
        if g.generation != job.generation {
            continue;
        }
        match outcome {
            Pass::Silent => {
                g.stats.silent += 1;
                g.tentative.clear();
            }
            Pass::Ran { committed, language } => {
                g.stats.passes += 1;
                g.stats.busy_seconds += took;
                g.stats.longest_pass_seconds = g.stats.longest_pass_seconds.max(took);
                if g.language.is_none() {
                    g.language = language;
                }
                for w in committed {
                    if g.committed.len() >= MAX_QUEUED_WORDS {
                        g.committed.pop_front();
                        g.stats.dropped_words += 1;
                    }
                    g.committed.push_back(w);
                }
                g.tentative = agreement.tentative().to_vec();
            }
            Pass::Failed(e) => {
                g.status = StreamStatus::Failed(e);
                break;
            }
        }
    }
    shared.lock().running = false;
}

enum Pass {
    Silent,
    Ran { committed: Vec<Word>, language: Option<String> },
    Failed(String),
}

fn pass(shared: &Shared, model: &dyn Transcriber, agreement: &mut Agreement, job: &Job) -> Pass {
    let voiced = voiced_frames(&job.audio);
    let total = voiced.iter().filter(|v| **v).count();
    let fresh = voiced.get(job.fresh_from / FRAME..).map_or(0, |f| f.iter().filter(|v| **v).count());
    if total < MIN_VOICED_FRAMES || (fresh == 0 && agreement.tentative().is_empty()) {
        agreement.clear_tentative();
        return Pass::Silent;
    }
    let opts = Options { language: job.language.clone(), diarize: false, max_speakers: 1 };
    let result = model.transcribe(&job.audio, &opts, &mut |_, _| {
        let g = shared.lock();
        !g.stop && g.generation == job.generation
    });
    match result {
        Ok(t) => {
            let offset = sample_tick(job.start);
            let hypothesis: Vec<Word> = t
                .words
                .iter()
                .filter(|w| word_is_voiced(&voiced, w))
                .map(|w| Word::new(w.text.clone(), w.start.saturating_add(offset), w.end.saturating_add(offset)))
                .collect();
            let committed = agreement.pass(&hypothesis);
            Pass::Ran { committed, language: (!t.language.is_empty()).then_some(t.language) }
        }
        Err(SpeechError::Cancelled) => Pass::Silent,
        Err(e) => Pass::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seconds_tick as s;

    fn w(text: &str, a: f64, b: f64) -> Word {
        Word::new(text, s(a), s(b))
    }

    fn words(spec: &str, from: f64) -> Vec<Word> {
        spec.split_whitespace().enumerate().map(|(i, t)| w(t, from + i as f64 * 0.4, from + i as f64 * 0.4 + 0.3)).collect()
    }

    fn texts(ws: &[Word]) -> Vec<String> {
        ws.iter().map(|w| w.text.clone()).collect()
    }

    #[test]
    fn words_commit_once_two_passes_agree_and_the_rest_stays_tentative() {
        let mut a = Agreement::new();
        assert!(a.pass(&words("the quick brown", 0.0)).is_empty(), "one pass alone commits nothing");
        assert_eq!(texts(a.tentative()), ["the", "quick", "brown"]);
        let c = a.pass(&words("The quick, brown fox jumps", 0.0));
        assert_eq!(texts(&c), ["The", "quick,", "brown"], "the agreed prefix commits, case and punctuation aside");
        assert_eq!(texts(a.tentative()), ["fox", "jumps"]);
        let c = a.pass(&words("the quick brown fox leaps over", 0.0));
        assert_eq!(texts(&c), ["fox"], "the committed words are not repeated, and only agreement commits");
        assert_eq!(texts(a.tentative()), ["leaps", "over"]);
    }

    #[test]
    fn committed_words_are_never_retracted_or_moved_back_in_time() {
        let mut a = Agreement::new();
        a.pass(&words("one two three", 0.0));
        let first = a.pass(&words("one two three four", 0.0));
        assert_eq!(first.len(), 3);
        let end = a.committed_end();
        // a later pass rewrites the past and puts its words earlier
        let c = a.pass(&words("won too tree four five", 0.0));
        assert!(c.iter().all(|w| w.start >= end), "no committed word starts before the commit point: {c:?}");
        assert!(a.committed_end() >= end, "the commit point never goes back");
    }

    #[test]
    fn hypotheses_that_disagree_every_pass_commit_nothing() {
        let mut a = Agreement::new();
        let noise = ["la la la", "thank you", "bye", "music", "you you", "so", "the end"];
        let committed: usize = noise.iter().map(|h| a.pass(&words(h, 1.0)).len()).sum();
        assert_eq!(committed, 0, "invented words that change from pass to pass never become final");
    }

    #[test]
    fn hostile_hypotheses_are_bounded_and_cleaned() {
        let mut a = Agreement::new();
        let mut hostile = vec![
            w("", 0.0, 1.0),
            w("  ...  ", 0.0, 1.0),
            Word::new("neg", Tick(-5), Tick(-1)),
            w("backwards", 2.0, 1.0),
            w(&"x".repeat(10_000), 3.0, 3.5),
            w("early", 0.5, 0.6),
        ];
        hostile.extend((0..5_000).map(|i| w("spam", 4.0 + i as f64 * 0.01, 4.0 + i as f64 * 0.01)));
        a.pass(&hostile);
        let c = a.pass(&hostile);
        assert!(c.len() <= MAX_HYPOTHESIS_WORDS, "at most {MAX_HYPOTHESIS_WORDS} words a pass: {}", c.len());
        assert!(c.iter().all(|w| !normalize_word(&w.text).is_empty() && w.end >= w.start && w.start >= Tick::ZERO));
        assert!(c.iter().all(|w| w.text.chars().count() <= MAX_WORD_CHARS));
        assert!(c.windows(2).all(|p| p[0].start <= p[1].start), "in time order");
        assert!(!c.iter().any(|w| w.text == "early"), "a word earlier than the one before it is dropped");
    }

    #[test]
    fn a_window_that_starts_inside_the_committed_words_does_not_repeat_them() {
        let mut a = Agreement::new();
        a.pass(&words("alpha beta gamma delta", 0.0));
        let c = a.pass(&words("alpha beta gamma delta", 0.0));
        assert_eq!(c.len(), 4);
        // the next window starts a second earlier than the last committed word; its times drift
        let c = a.pass(&[w("gamma", 0.85, 1.05), w("delta", 1.25, 1.4), w("epsilon", 1.7, 2.0)]);
        assert!(c.is_empty() && texts(a.tentative()) == ["epsilon"], "the repeated tail is skipped: {c:?} {:?}", a.tentative());
    }

    #[test]
    fn silence_and_steady_noise_have_no_voiced_frames_and_speech_like_bursts_do() {
        assert!(!voiced_frames(&vec![0.0; 16_000]).contains(&true), "digital silence");
        // white noise at a steady level: its own floor
        let mut x = 0x1234_5678u32;
        let noise: Vec<f32> = (0..32_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 - 0.5) * 0.2
            })
            .collect();
        let v = voiced_frames(&noise);
        assert!(v.iter().filter(|b| **b).count() < MIN_VOICED_FRAMES, "steady noise is not speech");
        // bursts of tone over the same noise: syllables
        let bursts: Vec<f32> = noise.iter().enumerate().map(|(i, n)| n + if (i / 3_200) % 2 == 0 { 0.4 * ((i as f32) * 0.2).sin() } else { 0.0 }).collect();
        assert!(voiced_frames(&bursts).iter().filter(|b| **b).count() >= MIN_VOICED_FRAMES);
    }

    #[test]
    fn scripted_words_arrive_as_the_audio_reaches_them() {
        let mut r = ScriptedRecognizer::new("scripted", ScriptedRecognizer::reading("hello there world", 0.0, 1.0, 0.5));
        r.push(&vec![0.0; 16_000]);
        let u = r.poll();
        assert!(u.committed.is_empty() && texts(&u.tentative) == ["hello"], "{u:?}");
        r.push(&vec![0.0; 24_000]);
        assert_eq!(texts(&r.poll().committed), ["hello", "there"]);
        r.stop();
        r.push(&vec![0.0; 160_000]);
        assert!(r.poll().committed.is_empty(), "nothing after stop");
    }

    /// A transcriber that answers each pass with the next of `passes` (times relative to the
    /// window it is given, as Whisper's are), after `delay`.
    struct Passes {
        passes: Mutex<VecDeque<Vec<Word>>>,
        delay: std::time::Duration,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Transcriber for Passes {
        fn id(&self) -> String {
            "passes".into()
        }
        fn transcribe(&self, _audio: &[f32], _: &Options, progress: crate::ProgressFn) -> Result<filmcraft_project::Transcript, SpeechError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(self.delay);
            if !progress(0.5, "") {
                return Err(SpeechError::Cancelled);
            }
            let words = self.passes.lock().unwrap().pop_front().unwrap_or_default();
            Ok(filmcraft_project::Transcript { language: "en".into(), source: "passes".into(), speakers: Vec::new(), words })
        }
    }

    fn speech(seconds: f64) -> Vec<f32> {
        (0..(seconds * 16_000.0) as usize).map(|i| if (i / 1_600) % 2 == 0 { 0.3 * ((i as f32) * 0.15).sin() } else { 0.0 }).collect()
    }

    fn wait_for(r: &mut RollingRecognizer, until: impl Fn(&StreamUpdate, &RollingRecognizer) -> bool) -> Vec<Word> {
        let mut all = Vec::new();
        for _ in 0..500 {
            let u = r.poll();
            all.extend(u.committed.clone());
            if until(&u, r) {
                return all;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out; got {all:?}");
    }

    fn rolling(passes: Vec<Vec<Word>>, delay_ms: u64, config: RollingConfig) -> (RollingRecognizer, Arc<std::sync::atomic::AtomicUsize>) {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let t: Arc<dyn Transcriber> =
            Arc::new(Passes { passes: Mutex::new(passes.into()), delay: std::time::Duration::from_millis(delay_ms), calls: calls.clone() });
        (RollingRecognizer::new("passes", Box::new(move || Ok(t)), config), calls)
    }

    #[test]
    fn the_rolling_recogniser_commits_agreed_words_from_its_worker() {
        let config = RollingConfig { window: 6.0, hop: 1.0, context: 1.0, language: Some("en".into()) };
        let (mut r, _) = rolling(vec![words("one two", 0.1), words("one two three", 0.1), words("two three four", 0.5)], 0, config);
        wait_for(&mut r, |_, r| r.status() == StreamStatus::Listening);
        r.push(&speech(1.0));
        let mut got = wait_for(&mut r, |_, r| r.stats().passes >= 1);
        r.push(&speech(1.0));
        got.extend(wait_for(&mut r, |_, r| r.stats().passes >= 2));
        assert_eq!(texts(&got), ["one", "two"]);
        r.stop();
        wait_for(&mut r, |_, r| !r.worker_running());
    }

    #[test]
    fn silence_runs_no_pass_and_commits_nothing() {
        let config = RollingConfig { hop: 0.5, ..Default::default() };
        let (mut r, calls) = rolling(vec![words("thank you", 0.0); 10], 0, config);
        r.push(&vec![0.0; 16_000 * 4]);
        let got = wait_for(&mut r, |_, r| r.stats().silent >= 1);
        assert!(got.is_empty() && calls.load(std::sync::atomic::Ordering::SeqCst) == 0, "silence never reaches the model");
    }

    #[test]
    fn a_slow_model_skips_passes_instead_of_queueing_them_and_stops_promptly() {
        let config = RollingConfig { hop: 0.25, ..Default::default() };
        let (mut r, calls) = rolling(vec![words("a b c", 0.0); 100], 300, config);
        wait_for(&mut r, |_, r| r.status() == StreamStatus::Listening);
        for _ in 0..8 {
            r.push(&speech(0.25));
        }
        wait_for(&mut r, |_, r| r.stats().skipped > 0);
        let before = calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(before < 8, "fewer passes than hops: {before}");
        let stopped = Instant::now();
        r.stop();
        wait_for(&mut r, |_, r| !r.worker_running());
        assert!(stopped.elapsed() < std::time::Duration::from_secs(2), "stop does not wait for queued work");
        r.push(&speech(5.0));
        assert!(r.poll().committed.is_empty() && r.status() == StreamStatus::Stopped);
    }

    #[test]
    fn a_model_that_fails_to_load_says_why() {
        let mut r = RollingRecognizer::new("x", Box::new(|| Err(SpeechError::NotInstalled("whisper-base".into()))), RollingConfig::default());
        wait_for(&mut r, |_, r| matches!(r.status(), StreamStatus::Failed(_)));
        assert!(matches!(r.status(), StreamStatus::Failed(m) if m.contains("whisper-base")));
    }

    #[test]
    fn the_buffer_stays_bounded_for_any_take_length() {
        let config = RollingConfig { hop: 1000.0, ..Default::default() };
        let (mut r, _) = rolling(Vec::new(), 0, config);
        for _ in 0..120 {
            r.push(&vec![0.0; 16_000]);
        }
        let held = r.shared.lock().audio.len();
        assert!(held as f64 <= MAX_BUFFER_SECONDS * 16_000.0, "{held}");
    }
}
