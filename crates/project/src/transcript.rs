//! Transcripts of media clips (Text panel ▸ Transcript).
//!
//! A [`Transcript`] belongs to one media item (`Project::transcripts`, keyed by the item id) and
//! lists the spoken [`Word`]s with their **media-time** bounds, so it stays valid however the clip
//! is trimmed, moved or reused: the sequence transcript is derived from the clip transcripts by
//! mapping each word through the track items that show it (`filmcraft_edit::transcript`).
//!
//! Speakers are numbered per transcript (`Word::speaker` indexes [`Transcript::speakers`]); the
//! Text panel shows and renames them by name, so two clips whose speakers share a name read as one
//! speaker in the sequence transcript.

use filmcraft_time::{Tick, TimeRange};
use serde::{Deserialize, Serialize};

/// One transcribed word.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// The word as written, with attached punctuation (`"Hello,"`). No surrounding spaces.
    pub text: String,
    /// Media time of the word's first sample.
    pub start: Tick,
    /// Media time just after the word (exclusive).
    pub end: Tick,
    /// Index into [`Transcript::speakers`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<u32>,
    /// Recogniser confidence 0..1 (1 for hand-made or corrected words).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub confidence: f32,
}

fn one() -> f32 {
    1.0
}
fn is_one(v: &f32) -> bool {
    *v == 1.0
}

impl Word {
    pub fn new(text: impl Into<String>, start: Tick, end: Tick) -> Self {
        Self { text: text.into(), start, end, speaker: None, confidence: 1.0 }
    }
    pub fn range(&self) -> TimeRange {
        TimeRange::from_bounds(self.start, self.end.max(self.start))
    }
    /// Lower-case text without leading/trailing punctuation (`"Um,"` → `"um"`), for search and
    /// filler-word matching.
    pub fn normalized(&self) -> String {
        normalize_word(&self.text)
    }
}

/// Lower-case `s` and trim punctuation and symbols from both ends (apostrophes inside a word stay:
/// `"Don't!"` → `"don't"`).
pub fn normalize_word(s: &str) -> String {
    s.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// A speaker label.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Speaker {
    pub name: String,
}

/// The transcript of one media item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transcript {
    /// ISO 639-1 code of the spoken language (`"en"`).
    pub language: String,
    /// What produced it: a model id (`"whisper-base"`), `"imported"` or `"manual"`.
    pub source: String,
    pub speakers: Vec<Speaker>,
    /// Words in time order, non-overlapping.
    pub words: Vec<Word>,
    /// Where the recording has voice, as `(start, end)` media times measured from the waveform when
    /// it was transcribed (sorted, non-overlapping). The gaps between them are the pauses the Text
    /// panel shows and Delete removes. Empty for transcripts made without the audio (imported, or
    /// from before voice analysis): their pauses are the gaps between words.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub voice: Vec<(Tick, Tick)>,
}

impl Transcript {
    /// The speaker name of `word` (`"Speaker 2"` when the label list is short; `None` when the word
    /// has no speaker).
    pub fn speaker_name(&self, word: &Word) -> Option<String> {
        let i = word.speaker? as usize;
        Some(self.speakers.get(i).map(|s| s.name.clone()).unwrap_or_else(|| format!("Speaker {}", i + 1)))
    }

    /// The text of all words joined by spaces.
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// Indices of the words overlapping the media range.
    pub fn words_in(&self, r: TimeRange) -> std::ops::Range<usize> {
        let a = self.words.partition_point(|w| w.end <= r.start);
        let b = self.words.partition_point(|w| w.start < r.end());
        a..b.max(a)
    }

    /// Sort words by time and make the transcript well formed: empty words dropped, `end >= start`,
    /// no overlaps (a word ends at the next one's start at the latest), speaker indices in range
    /// (missing labels are added as "Speaker N").
    pub fn normalize(&mut self) {
        self.words.retain(|w| !w.text.trim().is_empty());
        for w in &mut self.words {
            w.text = w.text.trim().to_string();
            if w.end < w.start {
                w.end = w.start;
            }
            w.confidence = w.confidence.clamp(0.0, 1.0);
        }
        self.words.sort_by_key(|w| (w.start, w.end));
        for i in 1..self.words.len() {
            let s = self.words[i].start;
            if self.words[i - 1].end > s {
                self.words[i - 1].end = s.max(self.words[i - 1].start);
            }
        }
        // voice spans: positive, sorted, overlapping or touching ones merged
        self.voice.retain(|(a, b)| b > a);
        self.voice.sort_unstable();
        let mut merged: Vec<(Tick, Tick)> = Vec::with_capacity(self.voice.len());
        for (a, b) in std::mem::take(&mut self.voice) {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        self.voice = merged;
        let max = self.words.iter().filter_map(|w| w.speaker).max();
        if let Some(m) = max {
            while self.speakers.len() <= m as usize {
                let n = self.speakers.len() + 1;
                self.speakers.push(Speaker { name: format!("Speaker {n}") });
            }
        }
    }

    /// Validate the invariants [`Transcript::normalize`] establishes.
    pub fn check(&self) -> Result<(), String> {
        for (i, w) in self.words.iter().enumerate() {
            if w.end < w.start {
                return Err(format!("word {i} ({:?}) ends before it starts", w.text));
            }
            if i > 0 && self.words[i - 1].end > w.start {
                return Err(format!("word {i} ({:?}) overlaps the word before it", w.text));
            }
            if let Some(s) = w.speaker
                && s as usize >= self.speakers.len()
            {
                return Err(format!("word {i} has speaker {s}, but there are {} speakers", self.speakers.len()));
            }
        }
        for (i, (a, b)) in self.voice.iter().enumerate() {
            if b <= a {
                return Err(format!("voice span {i} is empty or reversed"));
            }
            if i > 0 && self.voice[i - 1].1 >= *a {
                return Err(format!("voice span {i} overlaps or touches the one before it"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(t: &str, a: i64, b: i64) -> Word {
        Word::new(t, Tick(a), Tick(b))
    }

    #[test]
    fn normalize_sorts_clamps_and_labels() {
        let mut t = Transcript { words: vec![w("b", 10, 30), w(" a ", 0, 15), w("", 40, 50), w("c", 35, 20)], ..Default::default() };
        t.words[0].speaker = Some(1);
        t.normalize();
        assert_eq!(t.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["a", "b", "c"]);
        assert_eq!(t.words[0].end, Tick(10));
        assert_eq!(t.words[2].end, Tick(35));
        assert_eq!(t.speakers.len(), 2);
        assert_eq!(t.speaker_name(&t.words[1]).as_deref(), Some("Speaker 2"));
        t.check().unwrap();
    }

    #[test]
    fn voice_spans_are_sorted_merged_and_optional_in_files() {
        let mut t = Transcript { voice: vec![(Tick(50), Tick(60)), (Tick(0), Tick(10)), (Tick(8), Tick(20)), (Tick(30), Tick(30))], ..Default::default() };
        t.normalize();
        assert_eq!(t.voice, vec![(Tick(0), Tick(20)), (Tick(50), Tick(60))]);
        t.check().unwrap();
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"voice\":[[0,20],[50,60]]"), "{json}");
        // older transcripts have no voice spans and don't write any
        let old: Transcript = serde_json::from_str(r#"{"language":"en","source":"whisper-base","speakers":[],"words":[]}"#).unwrap();
        assert!(old.voice.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("voice"));
        t.voice = vec![(Tick(5), Tick(1))];
        assert!(t.check().is_err());
    }

    #[test]
    fn words_in_range_and_normalized() {
        let t = Transcript { words: vec![w("Um,", 0, 10), w("Don't!", 10, 20), w("go", 25, 30)], ..Default::default() };
        assert_eq!(t.words_in(TimeRange::from_bounds(Tick(5), Tick(24))), 0..2);
        assert_eq!(t.words_in(TimeRange::from_bounds(Tick(20), Tick(25))), 2..2);
        assert_eq!(t.words[0].normalized(), "um");
        assert_eq!(t.words[1].normalized(), "don't");
        assert_eq!(t.text(), "Um, Don't! go");
    }
}
