//! Follow Speech (#5266): during a voice-over take, recognise the narrator's speech as it is
//! recorded and keep the prompter where the narrator is.
//!
//! The option is a voice-over preference (`audio.voiceover.settings {"followSpeech": true}`, or
//! `audio.voiceover.followSpeech` to turn it on and off), off by default. While a take records with
//! it on, [`crate::voiceover::pump`] reads the input as it arrives, converts it to 16 kHz and feeds
//! a [`StreamingRecognizer`]: the rolling-window Whisper recogniser of `filmcraft_speech::stream`
//! with the model the transcription preference names, or one a host or test installed
//! ([`RecognizerFactory`]). The committed words drive the prompter, are shown on its card, and are
//! kept as the take's transcript. `audio.voiceover.followState` reports what it is doing.
//!
//! **Normalising** ([`tokens`]). Script and heard words are split into tokens the same way:
//! letters and digits (any script, `char::is_alphanumeric`) with combining marks kept, lower case
//! (`char::to_lowercase`), everything else a separator; apostrophes inside a word are dropped
//! (`don't` = `dont`). In writing systems without spaces between words (Han, Hiragana, Katakana,
//! Thai, Lao, Khmer, Myanmar) each character is a token. For English, whole numbers below 100
//! become words (`42` = `forty two`), as recognisers write them either way. No Unicode
//! normalisation (NFC) is applied: a script typed with decomposed accents does not match.
//!
//! **Script** ([`ScriptTracker`]). The place is a token index in the section's script followed by
//! the next section's (so reading on into the next section moves the prompter there). Each heard
//! token, the last [`MATCH_WINDOW`] heard tokens are aligned (Smith-Waterman: match +2 per token
//! weight, a near match +1, a mismatch −1, a word missing on either side −2; a token of four or more characters in a spaced
//! script weighs 2, others 1; a near match is an edit distance of 1, or 2 from 8 characters, between
//! words of 4 or more characters) against the script from [`LOOK_BACK`] tokens before the place to
//! [`LOOK_AHEAD`] after it, ending on the last heard token. It moves forward when that alignment
//! matches at least [`MIN_MATCHED`] of the window's tokens with a weight of at least
//! [`MIN_EVIDENCE`], and the heard token before the last matches a script token just before the end
//! (so a move rests on the latest words, not on one new word and older ones); of equal scores the
//! nearest end ahead of the place wins, so a phrase repeated later in the script is not jumped to.
//! An end up to two tokens behind the place is the same place (the words arrive late). It moves backward (a deliberate re-read) only when the
//! last [`BACK_MATCHED`] heard tokens match the script exactly and consecutively. After
//! [`LOST_AFTER`] heard tokens without a move it is lost: the place holds, and the whole document
//! is searched with the backward rule until a clear match finds the narrator again.
//!
//! **Talking points** ([`PointTracker`]). A point's distinctive words are its tokens of four or more
//! characters (any token in writing systems without spaces) that no other point of the two sections
//! uses and that are not common English function words. A point is heard when
//! [`POINT_EVIDENCE`] different distinctive words of it (all of them when it has fewer; a point with
//! a single one needs it to be at least 6 characters long) are among the last [`POINT_WINDOW`] heard
//! tokens; going back to an earlier point needs one more. Without such evidence the clock and the
//! manual commands drive, as without this option.
//!
//! **Driving the prompter.** For script text the prompter is *held* at the tracked place: its prompt
//! time is the script time of the place and does not run with the playhead (a held [`Anchor`],
//! logged as a `follow` entry with `rateAfter` 0). It moves when the place is [`STEP_CHARS`]
//! characters from where it is held, so the reading line moves a line at a time, not word by word,
//! and holds while the narrator pauses, ad-libs or is lost. At the end of the script it is released
//! to run with the playhead again. For talking points a heard point moves the prompter to the
//! point's start, keeping its rate, like Next. Until the first match the clock drives as usual.
//! It never moves back to where the narrator is when the clock had carried it further (it holds there
//! until they catch up), except for a re-read. Each automatic move is a `follow` entry in the take's
//! log, so the log keeps its replay rule.
//!
//! **Manual commands win.** Next, Previous, Return to Playhead, Faster, Slower and Reset Rate put the
//! prompter where they say, release a hold, and restart tracking from the new prompt time. For
//! [`MANUAL_DEFER_SECONDS`] of playhead time after one, the tracker listens but does not move the
//! prompter, and words spoken before the command (still arriving, late) are not followed.
//!
//! **Transcript.** Committed words are kept with their take times (from the record point, as the
//! take log): returned as `transcript` by `audio.voiceover.stop` and by `audio.voiceover.followState`
//! (`lastTranscript`). Storing it on the recorded clip needs a project schema change and is left for
//! a follow-up: it would go in `Project::transcripts` under the recording's media item, as
//! `transcript.generate` stores one.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use filmcraft_project::{Marker, MarkerId, Sequence, Word};
use filmcraft_speech::Transcriber;
use filmcraft_speech::resample::Resampler;
use filmcraft_speech::stream::{RollingConfig, RollingRecognizer, StreamStatus};
pub use filmcraft_speech::stream::{ScriptedRecognizer, ScriptedWord, StreamingRecognizer};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::Serialize;
use serde_json::{Value, json};

use crate::Session;
use crate::prompter::{self, PointState};

// ------------------------------------------------------------------------------------- tokens

/// One token of normalised text, with where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub norm: String,
    /// 2 for a word of four or more characters in a spaced script, else 1.
    pub weight: u8,
    /// Character offsets of the token in its text: `[start, end)`.
    pub start: usize,
    pub end: usize,
}

/// Characters of writing systems that do not put spaces between words: each is a token.
fn unspaced(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F // Han, kana
        | 0x0E00..=0x0EFF // Thai, Lao
        | 0x1000..=0x109F // Myanmar
        | 0x1780..=0x17FF // Khmer
    )
}

fn combining(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];

/// English words for a whole number below 100.
fn number_words(n: usize) -> Vec<&'static str> {
    match n {
        0..=19 => vec![ONES[n]],
        _ if n.is_multiple_of(10) => vec![TENS[n / 10]],
        _ => vec![TENS[n / 10], ONES[n % 10]],
    }
}

/// Split `text` into normalised tokens (see the module docs). `language` (ISO 639-1) only changes
/// how numbers are read.
pub fn tokens(text: &str, language: Option<&str>) -> Vec<Token> {
    let english = language.is_none_or(|l| l == "en");
    let mut out = Vec::new();
    let mut word = String::new();
    let mut start = 0;
    let mut prev_alnum = false;
    for (i, c) in text.chars().enumerate() {
        if out.len() > MAX_TOKENS {
            break;
        }
        if unspaced(c) && c.is_alphanumeric() {
            finish(&mut out, &mut word, start, i, english);
            word.extend(c.to_lowercase());
            finish(&mut out, &mut word, i, i + 1, english);
            prev_alnum = false;
        } else if c.is_alphanumeric() || (combining(c) && !word.is_empty()) {
            if word.is_empty() {
                start = i;
            }
            word.extend(c.to_lowercase());
            prev_alnum = true;
        } else if matches!(c, '\'' | '’') && prev_alnum {
            // inside a word: dropped
        } else {
            finish(&mut out, &mut word, start, i, english);
            prev_alnum = false;
        }
    }
    finish(&mut out, &mut word, start, text.chars().count(), english);
    out.truncate(MAX_TOKENS);
    out
}

/// End the word being read (if any) as one token, or as number words.
fn finish(out: &mut Vec<Token>, word: &mut String, start: usize, end: usize, english: bool) {
    if word.is_empty() {
        return;
    }
    let w = std::mem::take(word);
    if english && w.len() <= 2 && w.chars().all(|c| c.is_ascii_digit()) {
        for n in number_words(w.parse().unwrap_or(0)) {
            out.push(Token { norm: n.to_owned(), weight: if n.len() >= 4 { 2 } else { 1 }, start, end });
        }
        return;
    }
    let chars = w.chars().filter(|c| !combining(*c)).count();
    out.push(Token { norm: w, weight: if chars >= 4 { 2 } else { 1 }, start, end });
}

/// Tokens read from one text (a script of [`prompter::MAX_COMMENT_BYTES`] has fewer).
const MAX_TOKENS: usize = 70_000;

/// Edit distance, giving up past `limit`.
fn distance_within(a: &str, b: &str, limit: usize) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > limit || a.len() > 64 || b.len() > 64 {
        return false;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j - 1] + usize::from(a[i - 1] != b[j - 1])).min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        prev = cur;
    }
    prev[b.len()] <= limit
}

/// How well a heard token matches a script token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Similar {
    Exact,
    Near,
    No,
}

fn similar(heard: &str, script: &str) -> Similar {
    if heard == script {
        return Similar::Exact;
    }
    let n = heard.chars().count().min(script.chars().count());
    if n >= 4 && distance_within(heard, script, if n >= 8 { 2 } else { 1 }) { Similar::Near } else { Similar::No }
}

// ------------------------------------------------------------------------------------- script

/// Heard tokens aligned at a time.
pub const MATCH_WINDOW: usize = 4;
/// Of those, matched for a forward move.
pub const MIN_MATCHED: usize = 3;
/// Weight of the matched tokens for a forward move.
pub const MIN_EVIDENCE: u32 = 5;
/// Heard tokens matching the script exactly and consecutively for a backward move or to recover.
pub const BACK_MATCHED: usize = 4;
/// Script tokens behind the place searched.
pub const LOOK_BACK: usize = 15;
/// Script tokens ahead of the place searched.
pub const LOOK_AHEAD: usize = 40;
/// Heard tokens in a row without a move before the narrator is lost.
pub const LOST_AFTER: usize = 6;
/// A place this many tokens behind the current one is the same place, not a move back.
const SAME_PLACE: usize = 2;
/// The alignment's cost of a word said but not in the script, or in the script but not said.
const GAP: i32 = 2;

/// Where the tracker is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Track {
    /// No match yet since the start or a manual command.
    Listening,
    Following,
    /// The recent words match nothing nearby; the place holds.
    Lost,
}

/// What one heard token did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heard {
    Nothing,
    /// The place moved; `back` for a re-read.
    Moved {
        place: usize,
        back: bool,
    },
    /// The narrator was lost (the place holds).
    Lost,
}

/// The narrator's place in a script (see the module docs). Pure: tokens in, place out.
#[derive(Clone, Debug)]
pub struct ScriptTracker {
    script: Vec<Token>,
    /// Tokens read: the next one to be said is `script[place]`.
    place: usize,
    heard: VecDeque<String>,
    misses: usize,
    state: Track,
}

/// The best alignment of the heard window ending on script token `end`.
#[derive(Clone, Copy, Debug)]
struct Alignment {
    end: usize,
    score: i32,
    matched: usize,
    weight: u32,
}

impl ScriptTracker {
    pub fn new(script: Vec<Token>, place: usize) -> Self {
        let place = place.min(script.len());
        Self { script, place, heard: VecDeque::new(), misses: 0, state: Track::Listening }
    }
    pub fn place(&self) -> usize {
        self.place
    }
    pub fn state(&self) -> Track {
        self.state
    }
    pub fn len(&self) -> usize {
        self.script.len()
    }
    pub fn is_empty(&self) -> bool {
        self.script.is_empty()
    }
    pub fn token(&self, i: usize) -> Option<&Token> {
        self.script.get(i)
    }

    /// Hear one token.
    pub fn hear(&mut self, token: &str) -> Heard {
        if self.script.is_empty() || token.is_empty() {
            return Heard::Nothing;
        }
        self.heard.push_back(token.to_owned());
        if self.heard.len() > MATCH_WINDOW.max(BACK_MATCHED) {
            self.heard.pop_front();
        }
        let lost = self.state == Track::Lost;
        let (lo, hi) = if lost { (0, self.script.len()) } else { (self.place.saturating_sub(LOOK_BACK), (self.place + LOOK_AHEAD).min(self.script.len())) };
        let window: Vec<&str> = self.heard.iter().rev().take(MATCH_WINDOW).rev().map(String::as_str).collect();
        let place = self.place;
        // where an alignment leaves the narrator, relative to the place now
        let back = |a: &Alignment| a.end + 1 + SAME_PLACE < place;
        let best = self
            .align(&window, lo, hi)
            .into_iter()
            .filter(|a| self.previous_heard_nearby(&window, a.end))
            .filter(|a| if lost || back(a) { self.exact_run(a.end) >= BACK_MATCHED } else { a.matched >= MIN_MATCHED && a.weight >= MIN_EVIDENCE })
            // the best score; of equals the nearest end ahead of the place, then the nearest behind
            .max_by_key(|a| (a.score, !back(a), std::cmp::Reverse(a.end.abs_diff(place))));
        match best {
            Some(a) => {
                self.misses = 0;
                self.state = Track::Following;
                let to = a.end + 1;
                // a match just behind the place is the same place (the recogniser's words lag)
                if to == place || (!lost && !back(&a) && to < place) {
                    return Heard::Nothing;
                }
                self.place = to;
                Heard::Moved { place: to, back: to < place }
            }
            None => {
                self.misses += 1;
                if self.state == Track::Following && self.misses >= LOST_AFTER {
                    self.state = Track::Lost;
                    return Heard::Lost;
                }
                Heard::Nothing
            }
        }
    }

    /// The heard token before the last one matches a script token just before `end`, so a move
    /// rests on the latest words, not on one word and older ones.
    fn previous_heard_nearby(&self, window: &[&str], end: usize) -> bool {
        let Some(prev) = window.len().checked_sub(2).and_then(|i| window.get(i)) else { return false };
        (end.saturating_sub(3)..end).any(|i| self.script.get(i).is_some_and(|t| similar(prev, &t.norm) != Similar::No))
    }

    /// The last heard tokens equal the script tokens up to `end`, one for one: how many.
    fn exact_run(&self, end: usize) -> usize {
        self.heard.iter().rev().zip((0..=end).rev()).take_while(|(h, i)| self.script.get(*i).is_some_and(|t| &t.norm == *h)).count()
    }

    /// Local alignments of `heard` against `script[lo..hi]` that end with the last heard token
    /// matched to a script token.
    fn align(&self, heard: &[&str], lo: usize, hi: usize) -> Vec<Alignment> {
        let seg = self.script.get(lo..hi).unwrap_or_default();
        let (n, m) = (heard.len(), seg.len());
        if n == 0 || m == 0 {
            return Vec::new();
        }
        // (score, matched, weight) per cell; row i = heard tokens taken, column j = script tokens
        let mut prev = vec![(0i32, 0usize, 0u32); m + 1];
        let mut out = Vec::new();
        for (i, h) in heard.iter().enumerate() {
            let mut cur = vec![(0i32, 0usize, 0u32); m + 1];
            for j in 1..=m {
                let t = &seg[j - 1];
                let sim = similar(h, &t.norm);
                let w = u32::from(t.weight);
                let (ds, dm, dw) = prev[j - 1];
                let diag = match sim {
                    Similar::Exact => (ds + 2 * w as i32, dm + 1, dw + w),
                    Similar::Near => (ds + w as i32, dm + 1, dw + w),
                    Similar::No => (ds - 1, dm, dw),
                };
                let up = (prev[j].0 - GAP, prev[j].1, prev[j].2);
                let left = (cur[j - 1].0 - GAP, cur[j - 1].1, cur[j - 1].2);
                let mut best = diag;
                for c in [up, left] {
                    if c.0 > best.0 {
                        best = c;
                    }
                }
                cur[j] = if best.0 > 0 { best } else { (0, 0, 0) };
                if i + 1 == n && sim != Similar::No && cur[j] == diag {
                    out.push(Alignment { end: lo + j - 1, score: diag.0, matched: diag.1, weight: diag.2 });
                }
            }
            prev = cur;
        }
        out
    }

    /// A manual command moved the prompter: continue from `place`.
    pub fn restart(&mut self, place: usize) {
        self.place = place.min(self.script.len());
        self.heard.clear();
        self.misses = 0;
        self.state = Track::Listening;
    }
}

// ------------------------------------------------------------------------------------- points

/// Heard tokens a point's words are looked for in.
pub const POINT_WINDOW: usize = 12;
/// Different distinctive words of a point that make it heard.
pub const POINT_EVIDENCE: usize = 2;
/// A point with one distinctive word needs it this long.
const SINGLE_WORD_CHARS: usize = 6;

/// English function words of four or more letters that say nothing about a point.
const COMMON_EN: &[&str] = &[
    "about", "above", "after", "again", "also", "because", "been", "before", "being", "between", "both", "could", "does", "doing", "down", "each", "even",
    "every", "from", "have", "having", "here", "into", "just", "like", "many", "more", "most", "much", "must", "never", "only", "other", "over", "really",
    "same", "should", "some", "such", "than", "that", "their", "them", "then", "there", "these", "they", "thing", "things", "this", "those", "through", "very",
    "want", "well", "were", "what", "when", "where", "which", "while", "will", "with", "would", "your", "going", "know", "make", "think",
];

/// Which talking point the narrator is saying (see the module docs).
#[derive(Clone, Debug)]
pub struct PointTracker {
    /// Distinctive words of each point.
    words: Vec<Vec<String>>,
    heard: VecDeque<String>,
}

impl PointTracker {
    /// From the points' texts, in order.
    pub fn new(points: &[&str], language: Option<&str>) -> Self {
        let english = language.is_none_or(|l| l == "en");
        let toks: Vec<Vec<Token>> = points.iter().map(|p| tokens(p, language)).collect();
        let words = toks
            .iter()
            .enumerate()
            .map(|(i, ts)| {
                let mut w: Vec<String> = ts
                    .iter()
                    .filter(|t| t.weight >= 2 || t.norm.chars().any(unspaced))
                    .filter(|t| !(english && COMMON_EN.contains(&t.norm.as_str())))
                    .filter(|t| !toks.iter().enumerate().any(|(k, o)| k != i && o.iter().any(|x| x.norm == t.norm)))
                    .map(|t| t.norm.clone())
                    .collect();
                w.sort();
                w.dedup();
                if w.len() == 1 && w[0].chars().count() < SINGLE_WORD_CHARS {
                    w.clear();
                }
                w
            })
            .collect();
        Self { words, heard: VecDeque::new() }
    }

    /// Distinctive words of point `i` (empty: it cannot be heard).
    pub fn distinctive(&self, i: usize) -> &[String] {
        self.words.get(i).map_or(&[], Vec::as_slice)
    }

    /// Hear one token; returns the point heard, if one now has the evidence. `current` is the point
    /// shown now: an earlier one needs one more word.
    pub fn hear(&mut self, token: &str, current: Option<usize>) -> Option<usize> {
        self.heard.push_back(token.to_owned());
        if self.heard.len() > POINT_WINDOW {
            self.heard.pop_front();
        }
        let found = |i: usize| {
            let w = &self.words[i];
            let hits = w.iter().filter(|x| self.heard.iter().any(|h| similar(h, x) != Similar::No)).count();
            let need = POINT_EVIDENCE.min(w.len()) + usize::from(current.is_some_and(|c| i < c));
            !w.is_empty()
                && hits >= need.min(w.len().max(1))
                && hits >= 1
                && (hits >= need || (w.len() < need && hits == w.len() && current.is_none_or(|c| i >= c)))
        };
        // the heard word must belong to the point, so an old word does not keep re-triggering it
        let heard_now = |i: usize| self.words[i].iter().any(|x| similar(token, x) != Similar::No);
        let candidates: Vec<usize> = (0..self.words.len()).filter(|&i| Some(i) != current && heard_now(i) && found(i)).collect();
        candidates.into_iter().min_by_key(|&i| current.map_or(i, |c| if i > c { i - c } else { (c - i) + self.words.len() }))
    }

    pub fn restart(&mut self) {
        self.heard.clear();
    }
}

// ------------------------------------------------------------------------------------- document

/// Where a script token is: in which section and at which character of its script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct At {
    marker: MarkerId,
    offset: usize,
}

/// What is followed: the section at the prompt time and the next one after it.
struct Doc {
    /// The section the document starts with.
    base: MarkerId,
    script: ScriptTracker,
    /// Where each script token is.
    at: Vec<At>,
    /// The talking points: (section, index in its points, start time).
    points: Vec<(MarkerId, usize, Tick)>,
    point_tracker: PointTracker,
}

fn marker(seq: &Sequence, id: MarkerId) -> Option<&Marker> {
    seq.markers.iter().find(|m| m.id == id)
}

impl Doc {
    /// The document for the section shown at `prompt`, with the place at `prompt`. None outside
    /// sections.
    fn at(seq: &Sequence, prompt: Tick, language: Option<&str>) -> Option<Doc> {
        let p = prompter::prompt_at(seq, prompt);
        let section = p.section.as_ref()?;
        let mut ids = vec![section.marker];
        ids.extend(p.next.as_ref().map(|n| n.marker).filter(|m| *m != section.marker));
        let mut script = Vec::new();
        let mut at = Vec::new();
        let mut points = Vec::new();
        let mut texts: Vec<String> = Vec::new();
        let mut place = 0;
        for id in ids {
            let Some(m) = marker(seq, id) else { continue };
            let shown = prompter::prompt_at(seq, m.start.max(if id == section.marker { prompt } else { m.start }));
            let Some(s) = shown.section.filter(|s| s.marker == id) else { continue };
            if let Some(sc) = &s.script {
                let toks = tokens(&sc.text, language);
                if id == section.marker {
                    place = script.len() + toks.iter().take_while(|t| t.end <= sc.offset).count();
                }
                at.extend(toks.iter().map(|t| At { marker: id, offset: t.start }));
                script.extend(toks);
            }
            for (i, pt) in s.points.iter().enumerate() {
                points.push((id, i, pt.start));
                texts.push(pt.text.clone());
            }
        }
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        Some(Doc { base: section.marker, script: ScriptTracker::new(script, place), at, points, point_tracker: PointTracker::new(&refs, language) })
    }
}

// ------------------------------------------------------------------------------------- live state

/// How far the place moves before the prompter follows it, in script characters: about a third of
/// a line on the card, so the reading line moves a line at a time.
pub const STEP_CHARS: usize = 24;
/// After a manual command, the tracker does not move the prompter for this long (playhead time).
pub const MANUAL_DEFER_SECONDS: f64 = 3.0;
/// Words kept in a take's transcript; later ones are counted as dropped.
pub const MAX_TRANSCRIPT_WORDS: usize = 100_000;
/// Recent words shown on the card and reported by the query.
pub const RECENT_WORDS: usize = 12;

/// Makes a fresh recogniser.
pub type MakeRecognizer = Arc<dyn Fn() -> Box<dyn StreamingRecognizer> + Send + Sync>;

/// Makes the recogniser for a take: installed by hosts and tests (any build).
#[derive(Clone)]
pub struct RecognizerFactory {
    pub id: String,
    pub make: MakeRecognizer,
}

impl RecognizerFactory {
    /// A scripted recogniser that hears `words` in every take (tests and agents, in any build).
    pub fn scripted(words: Vec<ScriptedWord>) -> Self {
        Self { id: "scripted".into(), make: Arc::new(move || Box::new(ScriptedRecognizer::new("scripted", words.clone())) as Box<dyn StreamingRecognizer>) }
    }
}

/// What the option is doing (`audio.voiceover.followState` `state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FollowState {
    /// The option is off.
    Off,
    /// It is on but cannot work in this build or without a model: see `reason`.
    Unavailable,
    /// On and able; no take is recording.
    Ready,
    /// A take is recording and the model is loading.
    Starting,
    /// Hearing words, not yet matched to the text (or there is no text to follow).
    Listening,
    Following,
    /// The recent words match nothing nearby: the prompter holds.
    Lost,
    /// The recogniser stopped working during the take: see `reason`.
    Failed,
}

/// A word of the take's transcript: times from the take's record point.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeWord {
    pub text: String,
    pub start: Tick,
    pub end: Tick,
}

/// The transcript of a take.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeTranscript {
    pub model: String,
    pub language: Option<String>,
    pub words: Vec<TakeWord>,
    pub dropped: u64,
}

/// Following during a take: owned by `VoiceOver::follow` while the take records.
pub struct Live {
    recognizer: Box<dyn StreamingRecognizer>,
    resampler: Resampler,
    input_rate: u32,
    /// Captured samples already fed to the recogniser.
    pub(crate) fed: usize,
    capture_start: Tick,
    record_start: Tick,
    language: Option<String>,
    doc: Option<Doc>,
    /// The document was made for this sequence time's section and language.
    doc_language: Option<String>,
    /// Playhead before which the tracker may not move the prompter (after a manual command).
    defer_until: Tick,
    /// Words spoken before this playhead are not followed (they predate a manual command).
    ignore_before: Tick,
    /// A talking point was heard since the document was made.
    followed_point: bool,
    transcript: TakeTranscript,
    recent: VecDeque<String>,
    tentative: Vec<String>,
    /// The state the card shows changed since the UI last asked (for its announcement).
    pub(crate) changed: bool,
}

impl Live {
    fn new(recognizer: Box<dyn StreamingRecognizer>, input_rate: u32, capture_start: Tick, record_start: Tick) -> Self {
        let model = recognizer.id();
        Self {
            recognizer,
            resampler: Resampler::new(input_rate),
            input_rate,
            fed: 0,
            capture_start,
            record_start,
            language: None,
            doc: None,
            doc_language: None,
            defer_until: Tick::ZERO,
            ignore_before: Tick::ZERO,
            followed_point: false,
            transcript: TakeTranscript { model, ..Default::default() },
            recent: VecDeque::new(),
            tentative: Vec::new(),
            changed: false,
        }
    }

    /// Feed captured samples (mono, the input's rate).
    pub(crate) fn feed(&mut self, samples: &[f32]) {
        let out = self.resampler.push(samples);
        self.recognizer.push(&out);
        self.fed += samples.len();
    }

    fn track(&self) -> Option<Track> {
        self.doc.as_ref().map(|d| d.script.state())
    }

    fn state(&self) -> FollowState {
        match self.recognizer.status() {
            StreamStatus::Starting => FollowState::Starting,
            StreamStatus::Failed(_) => FollowState::Failed,
            _ => match self.track() {
                Some(Track::Following) => FollowState::Following,
                Some(Track::Lost) => FollowState::Lost,
                _ if self.followed_point => FollowState::Following,
                _ => FollowState::Listening,
            },
        }
    }
}

/// Why following cannot run, or the recogniser it would use.
pub fn availability(s: &Session) -> std::result::Result<String, String> {
    if let Some(f) = &s.voiceover.recognizer {
        return Ok(f.id.clone());
    }
    if let Some(t) = &s.transcriber {
        return Ok(t.id());
    }
    let model = s.prefs.media_analysis.whisper_model.clone();
    if !filmcraft_speech::available() {
        return Err("speech recognition is not in this build (it was built without the `whisper` feature)".into());
    }
    let m = filmcraft_speech::models::find(&model).ok_or_else(|| format!("unknown speech model `{model}` in Settings > Media Analysis & Transcription"))?;
    let dir = crate::transcript::models_dir().ok_or("there is no data directory for speech models")?;
    if !filmcraft_speech::models::installed(&dir, m) {
        return Err(format!(
            "the speech model `{model}` is not installed: download it with the transcript.downloadModel command (Transcribe… in the Text panel offers it)"
        ));
    }
    Ok(model)
}

/// The recogniser for a take, or why there is none.
fn recognizer(s: &Session) -> std::result::Result<Box<dyn StreamingRecognizer>, String> {
    if let Some(f) = &s.voiceover.recognizer {
        return Ok((f.make)());
    }
    let model = availability(s)?;
    let ma = &s.prefs.media_analysis;
    let config = RollingConfig { language: (!ma.language_auto_detect).then(|| ma.default_language.clone()).filter(|l| !l.is_empty()), ..Default::default() };
    let cache = Arc::clone(&s.voiceover.model_cache);
    let installed = s.transcriber.clone();
    let id = model.clone();
    let load = Box::new(move || -> std::result::Result<Arc<dyn Transcriber>, filmcraft_speech::SpeechError> {
        if let Some(t) = installed {
            return Ok(t);
        }
        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((m, t)) = c.as_ref()
            && *m == id
        {
            return Ok(Arc::clone(t));
        }
        let dir = crate::transcript::models_dir().ok_or_else(|| filmcraft_speech::SpeechError::Unavailable("no data directory".into()))?;
        let t = filmcraft_speech::load(&dir, &id)?;
        *c = Some((id.clone(), Arc::clone(&t)));
        Ok(t)
    });
    Ok(Box::new(RollingRecognizer::new(model, load, config)))
}

/// Loaded models kept between takes (the model named by the preference).
pub type ModelCache = Arc<Mutex<Option<(String, Arc<dyn Transcriber>)>>>;

/// A take started (or its capture restarted): start following when the option is on and can run.
pub(crate) fn begin(s: &mut Session) {
    end(s);
    let Some(rec) = s.voiceover.rec.clone() else { return };
    if !s.prefs.voice_over.follow_speech {
        return;
    }
    match recognizer(s) {
        Ok(r) => s.voiceover.follow = Some(Live::new(r, rec.format.sample_rate, rec.capture_start, rec.record_start)),
        Err(e) => s.voiceover.follow_error = Some(e),
    }
}

/// The take ended: stop the recogniser; returns the take's transcript (null when not following).
pub(crate) fn end(s: &mut Session) -> Value {
    s.voiceover.follow_error = None;
    let Some(mut live) = s.voiceover.follow.take() else { return Value::Null };
    live.recognizer.stop();
    let v = serde_json::to_value(&live.transcript).unwrap_or(Value::Null);
    s.voiceover.last_transcript = Some(live.transcript);
    v
}

/// Take in what the recogniser committed and move the prompter (called by `voiceover::pump`).
pub(crate) fn update(s: &mut Session) {
    let Some(mut live) = s.voiceover.follow.take() else { return };
    let before = (live.state(), live.recent.len(), live.tentative.clone());
    let u = live.recognizer.poll();
    if u.language.is_some() && u.language != live.language {
        live.language = u.language.clone();
        live.transcript.language = u.language;
    }
    live.tentative = u.tentative.iter().map(|w| w.text.clone()).collect();
    for w in &u.committed {
        take_word(s, &mut live, w);
    }
    // the section followed is the one the prompter shows
    sync_doc(s, &mut live, false);
    let after = (live.state(), live.recent.len(), live.tentative.clone());
    live.changed |= before != after;
    s.voiceover.follow = Some(live);
}

/// Make the document again when the prompter shows another section or the language changed.
fn sync_doc(s: &Session, live: &mut Live, force: bool) {
    let Some(seq) = s.active_sequence() else { return };
    let prompt = prompter::prompt_time(s);
    let shown = prompter::section_at(seq, prompt).map(|m| m.id);
    let stale = force || live.doc_language != live.language || live.doc.as_ref().map(|d| d.base) != shown;
    if stale {
        live.doc = Doc::at(seq, prompt, live.language.as_deref());
        live.doc_language = live.language.clone();
        live.followed_point = false;
    }
}

fn take_word(s: &mut Session, live: &mut Live, w: &Word) {
    let spoken = live.capture_start.saturating_add(w.start);
    let spoken_end = live.capture_start.saturating_add(w.end);
    if live.transcript.words.len() < MAX_TRANSCRIPT_WORDS {
        live.transcript.words.push(TakeWord {
            text: w.text.clone(),
            start: spoken.saturating_sub(live.record_start),
            end: spoken_end.saturating_sub(live.record_start),
        });
    } else {
        live.transcript.dropped += 1;
    }
    live.recent.push_back(w.text.clone());
    if live.recent.len() > RECENT_WORDS {
        live.recent.pop_front();
    }
    if spoken_end < live.ignore_before {
        return;
    }
    sync_doc(s, live, false);
    let language = live.language.clone();
    for t in tokens(&w.text, language.as_deref()) {
        hear(s, live, &t.norm);
    }
}

/// One heard token: track it and move the prompter when the place calls for it.
fn hear(s: &mut Session, live: &mut Live, token: &str) {
    let Some(doc) = live.doc.as_mut() else { return };
    let deferring = s.playhead() < live.defer_until;
    if !doc.script.is_empty() {
        let was = doc.script.state();
        let heard = doc.script.hear(token);
        if heard == Heard::Lost || (was != doc.script.state()) {
            live.changed = true;
        }
        if deferring || doc.script.state() != Track::Following {
            return;
        }
        let place = doc.script.place();
        if place >= doc.script.len() {
            // read to the end: run with the playhead again
            if prompter::held(s) {
                prompter::follow_release(s);
            }
            return;
        }
        let (m, c) = char_at(doc, place);
        let Some(seq) = s.active_sequence() else { return };
        let now = prompter::prompt_time(s);
        let shown = prompter::prompt_at(seq, now).section;
        let Some(target) = marker(seq, m).map(|mk| prompter::script_time(mk, c)) else { return };
        let reread = matches!(heard, Heard::Moved { back: true, .. });
        if let Some(cur) = shown.as_ref().filter(|x| x.marker == m).and_then(|x| x.script.as_ref()).map(|x| x.offset) {
            if c < cur && !reread {
                // the narrator is behind where the clock carried the prompter: wait for them there
                if !prompter::held(s) {
                    prompter::follow_hold(s, now);
                }
                return;
            }
            if c >= cur && c - cur < STEP_CHARS && prompter::held(s) {
                return;
            }
        }
        prompter::follow_hold(s, target);
        return;
    }
    // talking points
    let Some(seq) = s.active_sequence() else { return };
    let now = prompter::prompt_at(seq, prompter::prompt_time(s));
    let current = now.section.as_ref().and_then(|sec| {
        let cur = sec.points.iter().position(|p| p.state == PointState::Current)?;
        doc.points.iter().position(|(m, i, _)| *m == sec.marker && *i == cur)
    });
    if let Some(i) = doc.point_tracker.hear(token, current)
        && !deferring
        && let Some(&(_, _, start)) = doc.points.get(i)
    {
        live.followed_point = true;
        live.changed = true;
        prompter::follow_move(s, start);
    }
}

/// The section and script character of script token `place` (the end of the script for the end).
fn char_at(doc: &Doc, place: usize) -> (MarkerId, usize) {
    match doc.at.get(place) {
        Some(a) => (a.marker, a.offset),
        None => doc.at.last().map_or((doc.base, 0), |a| (a.marker, a.offset + doc.script.token(place.saturating_sub(1)).map_or(0, |t| t.end - t.start))),
    }
}

/// A manual command moved the prompter or changed its rate: defer and continue from there.
pub(crate) fn manual(s: &mut Session) {
    let Some(mut live) = s.voiceover.follow.take() else { return };
    let playhead = s.playhead();
    live.defer_until = playhead.saturating_add(Tick::from_seconds_f64(MANUAL_DEFER_SECONDS));
    live.ignore_before = playhead;
    sync_doc(s, &mut live, true);
    live.changed = true;
    s.voiceover.follow = Some(live);
}

/// The audio was discarded (`audio.voiceover.sync`): start the stream again.
pub(crate) fn restart(s: &mut Session) {
    let Some(rec) = s.voiceover.rec.clone() else { return };
    if let Some(live) = s.voiceover.follow.as_mut() {
        live.recognizer.reset();
        live.resampler = Resampler::new(live.input_rate);
        live.fed = 0;
        live.capture_start = rec.capture_start;
        live.record_start = rec.record_start;
        live.transcript.words.clear();
        live.transcript.dropped = 0;
        live.recent.clear();
        live.tentative.clear();
        live.doc = None;
    }
}

/// What the card shows: the state, the last committed words and the tentative ones. None when the
/// option is off or no take is recording.
#[derive(Clone, Debug, PartialEq)]
pub struct CardState {
    pub state: FollowState,
    pub committed: Vec<String>,
    pub tentative: Vec<String>,
    /// Why it is unavailable or failed.
    pub reason: Option<String>,
}

/// The card's view of following, while a take records with the option on.
pub fn card(s: &Session) -> Option<CardState> {
    if !s.voiceover.recording() || !s.prefs.voice_over.follow_speech {
        return None;
    }
    Some(match &s.voiceover.follow {
        Some(l) => CardState {
            state: l.state(),
            committed: l.recent.iter().cloned().collect(),
            tentative: l.tentative.clone(),
            reason: match l.recognizer.status() {
                StreamStatus::Failed(e) => Some(e),
                _ => None,
            },
        },
        None => CardState {
            state: FollowState::Unavailable,
            committed: Vec::new(),
            tentative: Vec::new(),
            reason: s.voiceover.follow_error.clone().or_else(|| availability(s).err()),
        },
    })
}

/// `audio.voiceover.followState`.
pub fn state_json(s: &Session) -> Value {
    let on = s.prefs.voice_over.follow_speech;
    let available = availability(s);
    let (state, reason) = match (&s.voiceover.follow, on, &available) {
        (_, false, _) => (FollowState::Off, available.as_ref().err().cloned()),
        (Some(l), true, _) => {
            let st = l.state();
            let reason = match l.recognizer.status() {
                StreamStatus::Failed(e) => Some(e),
                _ => None,
            };
            (st, reason)
        }
        (None, true, Err(e)) => (FollowState::Unavailable, Some(e.clone())),
        (None, true, Ok(_)) if s.voiceover.recording() => (FollowState::Unavailable, s.voiceover.follow_error.clone()),
        (None, true, Ok(_)) => (FollowState::Ready, None),
    };
    let live = s.voiceover.follow.as_ref();
    json!({
        "enabled": on,
        "available": available.is_ok(),
        "state": state,
        "reason": reason,
        "model": live.map(|l| l.transcript.model.clone()).or_else(|| available.as_ref().ok().cloned()),
        "language": live.and_then(|l| l.language.clone()),
        "recording": s.voiceover.recording(),
        "committed": live.map(|l| l.recent.iter().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "tentative": live.map(|l| l.tentative.clone()).unwrap_or_default(),
        "words": live.map_or(0, |l| l.transcript.words.len()),
        "place": live.and_then(|l| l.doc.as_ref()).filter(|d| !d.script.is_empty()).map(|d| {
            let (m, offset) = char_at(d, d.script.place());
            json!({"marker": m, "offset": offset})
        }),
        "stats": live.map(|l| l.recognizer.stats()),
        "transcript": live.map(|l| &l.transcript),
        "lastTranscript": s.voiceover.last_transcript,
    })
}

/// Seconds as ticks (for tests and callers that think in seconds).
pub fn seconds(x: f64) -> Tick {
    Tick((x * TICKS_PER_SECOND as f64) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "Sales grew every quarter this year, and the second half carried most of it. The chart on screen shows each region, \
        starting with the north. Every region grew, but the north grew fastest because of the new stores we opened in spring.";

    fn norms(text: &str) -> Vec<String> {
        tokens(text, Some("en")).into_iter().map(|t| t.norm).collect()
    }

    fn tracker(text: &str) -> ScriptTracker {
        ScriptTracker::new(tokens(text, Some("en")), 0)
    }

    /// Hear every token; the places reached, and whether it was ever lost.
    fn read(t: &mut ScriptTracker, heard: &[String]) -> (Vec<usize>, bool) {
        let mut places = Vec::new();
        let mut lost = false;
        for h in heard {
            match t.hear(h) {
                Heard::Moved { place, .. } => places.push(place),
                Heard::Lost => lost = true,
                Heard::Nothing => {}
            }
        }
        (places, lost)
    }

    /// A deterministic generator for the property tests.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    #[test]
    fn script_and_speech_are_normalised_the_same_way_in_any_writing_system() {
        assert_eq!(norms("Don't STOP, it's 42!"), ["dont", "stop", "its", "forty", "two"]);
        assert_eq!(norms("don’t stop its forty-two"), ["dont", "stop", "its", "forty", "two"], "typographic apostrophe, hyphen");
        assert_eq!(tokens("42 Grüße", Some("de")).iter().map(|t| t.norm.as_str()).collect::<Vec<_>>(), ["42", "grüße"], "numbers stay digits outside English");
        assert_eq!(norms("ΚΑΛΗΜΕΡΑ κόσμε"), ["καλημερα", "κόσμε"]);
        assert_eq!(norms("你好，世界"), ["你", "好", "世", "界"], "a character a token without spaces");
        assert_eq!(norms("東京へ行きます"), ["東", "京", "へ", "行", "き", "ま", "す"]);
        let t = tokens("Hello, world", None);
        assert_eq!((t[1].start, t[1].end), (7, 12), "tokens know their characters in the text");
    }

    #[test]
    fn a_clean_read_follows_to_the_end_without_ever_going_back() {
        let mut t = tracker(SCRIPT);
        let (places, lost) = read(&mut t, &norms(SCRIPT));
        assert!(!lost && t.state() == Track::Following);
        assert!(places.windows(2).all(|w| w[0] < w[1]), "only forward: {places:?}");
        assert_eq!(t.place(), t.len(), "at the end of the script");
        assert!(places.first().is_some_and(|p| *p <= MIN_MATCHED + 1), "it picks the narrator up after a few words");
    }

    #[test]
    fn a_read_with_each_kind_of_error_still_follows() {
        let words = norms(SCRIPT);
        let edits: [(&str, Box<dyn Fn(usize, &String) -> Vec<String>>); 4] = [
            ("substituted", Box::new(|i, w| vec![if i % 6 == 5 { "banana".into() } else { w.clone() }])),
            ("dropped", Box::new(|i, w| if i % 7 == 6 { vec![] } else { vec![w.clone()] })),
            ("added", Box::new(|i, w| if i % 6 == 5 { vec!["um".into(), w.clone()] } else { vec![w.clone()] })),
            ("misrecognised", Box::new(|i, w| vec![if i % 5 == 4 && w.len() >= 5 { format!("{}{}", &w[..w.len() - 1], "z") } else { w.clone() }])),
        ];
        for (what, edit) in edits {
            let heard: Vec<String> = words.iter().enumerate().flat_map(|(i, w)| edit(i, w)).collect();
            let mut t = tracker(SCRIPT);
            let (places, lost) = read(&mut t, &heard);
            assert!(!lost, "{what}: never lost");
            assert!(places.windows(2).all(|w| w[0] < w[1]), "{what}: never back: {places:?}");
            assert!(t.place() + 2 >= t.len(), "{what}: reaches the end: {} of {}", t.place(), t.len());
        }
    }

    #[test]
    fn restarting_a_sentence_goes_back_only_on_a_clear_match_and_then_on() {
        let words = norms(SCRIPT);
        let mut t = tracker(SCRIPT);
        read(&mut t, &words[..20]);
        let at = t.place();
        // three words from earlier are not enough to go back
        read(&mut t, &words[2..5]);
        assert_eq!(t.place(), at, "a few earlier words do not move it back");
        let mut t = tracker(SCRIPT);
        read(&mut t, &words[..30]);
        // the narrator goes back to the sentence starting at word 14 and reads on
        let (places, lost) = read(&mut t, &words[14..]);
        assert!(!lost);
        assert!(places.first().is_some_and(|p| *p < 30 && *p >= 14 + BACK_MATCHED), "a re-read moves back: {places:?}");
        assert_eq!(t.place(), t.len());
    }

    #[test]
    fn an_ad_lib_loses_the_narrator_and_holds_the_place_until_they_return() {
        let words = norms(SCRIPT);
        let mut t = tracker(SCRIPT);
        read(&mut t, &words[..15]);
        let at = t.place();
        let ad_lib = norms("by the way if you want the details they are on the website linked below");
        let (places, lost) = read(&mut t, &ad_lib);
        assert!(lost && t.state() == Track::Lost, "the ad-lib loses it");
        assert!(places.is_empty() && t.place() == at, "and the place holds: {places:?}");
        let (_, lost) = read(&mut t, &words[15..]);
        assert!(!lost && t.state() == Track::Following && t.place() == t.len(), "back on the script, it follows again");
    }

    #[test]
    fn a_phrase_repeated_later_in_the_script_is_not_jumped_to() {
        let text = "We opened new stores in the north this spring. Then the team worked on prices for a month. \
            Later we opened new stores in the north again, this time in the autumn.";
        let words = norms(text);
        let second = words.windows(3).rposition(|w| w == ["we", "opened", "new"]).unwrap();
        let mut t = tracker(text);
        read(&mut t, &words[..8]);
        assert!(t.place() < second, "the first occurrence is followed: {} vs {second}", t.place());
        let (_, lost) = read(&mut t, &words[8..]);
        assert!(!lost && t.place() == t.len());
    }

    #[test]
    fn an_empty_script_and_hostile_input_change_nothing_and_never_panic() {
        let mut t = tracker("");
        assert_eq!(t.hear("hello"), Heard::Nothing);
        assert!(t.is_empty() && t.place() == 0);
        let mut t = tracker(SCRIPT);
        for h in ["", "\u{0}", &"x".repeat(100_000), "🙂", "Ⅻ", "\u{301}"] {
            t.hear(h);
        }
        assert!(t.place() <= t.len());
        assert!(tokens(&"word ".repeat(200_000), None).len() <= MAX_TOKENS);
        assert!(tokens("\u{301}\u{301}abc\u{301}", None).len() == 1, "stray combining marks do not make tokens");
    }

    #[test]
    fn random_reads_stay_in_bounds_and_only_go_back_on_an_exact_run() {
        let words = norms(SCRIPT);
        let vocab = norms("the a north region grew sales chart stores spring um and of it");
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for case in 0..300 {
            let mut t = tracker(SCRIPT);
            let mut heard: Vec<String> = Vec::new();
            for _ in 0..rng.below(80) {
                match rng.below(4) {
                    0 => heard.push(vocab[rng.below(vocab.len())].clone()),
                    _ => {
                        let at = rng.below(words.len());
                        heard.extend(words[at..(at + rng.below(6)).min(words.len())].iter().cloned());
                    }
                }
            }
            let mut last = t.place();
            for (i, h) in heard.iter().enumerate() {
                if let Heard::Moved { place, back } = t.hear(h) {
                    assert!(place <= t.len(), "case {case}");
                    assert_eq!(back, place < last, "case {case}");
                    if back {
                        let run = heard[..=i].iter().rev().zip(words[..place].iter().rev()).take_while(|(a, b)| a == b).count();
                        assert!(run >= BACK_MATCHED, "case {case}: back to {place} after only {run} exact words");
                    }
                    last = place;
                }
            }
        }
    }

    #[test]
    fn a_talking_point_is_heard_by_its_distinctive_words_and_not_by_common_ones() {
        let points = ["Say hello to the viewers", "Introduce the quarterly numbers", "Show the regional chart", "Thank the team"];
        let mut p = PointTracker::new(&points, Some("en"));
        assert!(!p.distinctive(2).contains(&"show".to_string()) || p.distinctive(2).contains(&"regional".to_string()));
        assert!(!p.distinctive(3).iter().any(|w| w == "the"), "short words are not distinctive: {:?}", p.distinctive(3));
        let hear = |p: &mut PointTracker, text: &str, current: Option<usize>| norms(text).iter().filter_map(|t| p.hear(t, current)).last();
        assert_eq!(hear(&mut p, "so this is the thing with the team that we have", Some(1)), None, "common words never trigger a point");
        assert_eq!(hear(&mut p, "now let me show you the regional chart", Some(1)), Some(2), "two of its words do");
        p.restart();
        assert_eq!(hear(&mut p, "back to the quarterly results", Some(2)), None, "going back needs more");
        assert_eq!(hear(&mut p, "the quarterly numbers once more, introduce them again", Some(2)), Some(1));
    }
}
