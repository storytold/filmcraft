//! Hostile audio and transcripts (empty, NaN, extreme times, unordered words) must not panic.

use filmcraft_project::{Speaker, Transcript, Word};
use filmcraft_speech::diarize::{self, Params};
use filmcraft_speech::{downmix, mel, vad, word_error_rate};
use filmcraft_time::Tick;
use std::panic::{AssertUnwindSafe, catch_unwind};

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

const TICKS: [i64; 10] = [0, 1, -1, 5_000_000_000, 254_016_000_000, i64::MAX, i64::MIN, i64::MAX / 2, i64::MIN / 2, 1 << 50];

fn audio(rng: &mut Rng) -> Vec<f32> {
    let n = [0usize, 1, 2, 159, 160, 161, 400, 16_000, 90_000][rng.below(9)];
    let mode = rng.below(4);
    (0..n)
        .map(|i| match mode {
            0 => [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e30, -1e30, 0.0][rng.below(6)],
            1 => (i as f32 * 0.07).sin(),
            2 => 0.0,
            _ => (rng.next() as u32 as f32 / u32::MAX as f32) - 0.5,
        })
        .collect()
}

fn transcript(rng: &mut Rng) -> Transcript {
    let words = (0..rng.below(40))
        .map(|i| {
            let (start, end) = if rng.below(3) == 0 {
                (TICKS[rng.below(10)], TICKS[rng.below(10)])
            } else {
                let s = i as i64 * 3_000_000_000;
                (s, s + 2_500_000_000)
            };
            Word {
                text: "w".repeat(rng.below(5)),
                start: Tick(start),
                end: Tick(end),
                speaker: [None, Some(0), Some(u32::MAX)][rng.below(3)],
                confidence: [0.0, f32::NAN, 1.0][rng.below(3)],
            }
        })
        .collect();
    Transcript { language: "en".into(), source: "fuzz".into(), speakers: vec![Speaker { name: "A".into() }; rng.below(3)], words }
}

#[test]
fn speech_helpers_survive_hostile_input() {
    let mut rng = Rng(0x5eed_0123_4567_89ab);
    let mut failures = Vec::new();
    for _ in 0..150 {
        let a = audio(&mut rng);
        let mut t = transcript(&mut rng);
        let p = Params {
            chunk_gap: Tick(TICKS[rng.below(10)]),
            min_chunk: Tick(TICKS[rng.below(10)]),
            max_chunk: Tick(TICKS[rng.below(10)]),
            threshold: [0.0, f32::NAN, f32::INFINITY, 0.4][rng.below(4)],
            max_speakers: [0usize, 1, 6, usize::MAX][rng.below(4)],
        };
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = mel::log_mel(&a, [0usize, 1, 80, 128][rng.below(4)]);
            let _ = mel::power_spectrum(&a);
            let db = vad::frame_db(&a);
            let _ = vad::threshold(&db);
            let mut words = t.words.clone();
            vad::tighten_words(&a, &mut words);
            let _ = diarize::chunks(&t, &p);
            let _ = diarize::diarize(&a, &mut t, &p);
            let _ = downmix(&[a.clone(), vec![0.5; a.len() / 2], Vec::new()]);
            let _ = downmix(&[]);
            let _ = word_error_rate("", "");
            let _ = word_error_rate("a b c", "");
        }));
        if r.is_err() {
            failures.push(format!("audio len {} words {}", a.len(), t.words.len()));
        }
    }
    assert!(failures.is_empty(), "{} panics, e.g. {:?}", failures.len(), failures.first());
}
