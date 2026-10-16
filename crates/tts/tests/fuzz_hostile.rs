//! Random scripts and parameters through every built-in voice: an error or audio, never a panic.

use filmcraft_tts::{Params, script, voice, voices};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

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

const PIECES: &[&str] = &[
    "hello",
    "world",
    " ",
    "  ",
    "\n",
    ".",
    ",",
    "?",
    "!",
    "1234",
    "[pause 1s]",
    "[pause 99999999999s]",
    "[pause 0ms]",
    "[pause -5s]",
    "[pause 1.5.5s]",
    "[pause",
    "]",
    "[",
    "[[pause 2s]]",
    "[PAUSE 10 MS]",
    "é",
    "日本語",
    "😀",
    "\u{0}",
    "\u{301}",
    "zzzzzzzzzzzzzzzzzzzz",
    "ngth",
    "th",
    "sh",
    "ch",
    "x",
    "q",
    "'",
    "-",
    "$5",
    "ñ",
];

#[test]
fn random_scripts_and_params_never_panic() {
    let mut rng = Rng(0xabcd_ef01_2345_6789);
    let floats = [0.0, 1.0, -1.0, 0.25, 0.5, 4.0, 2.0, -24.0, 24.0, 12.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MIN_POSITIVE, 1e300];
    let mut bad = Vec::new();
    for i in 0..600 {
        let mut text = String::new();
        for _ in 0..rng.below(40) {
            text.push_str(PIECES[rng.below(PIECES.len())]);
            if rng.below(2) == 0 {
                text.push(' ');
            }
        }
        if i % 50 == 0 {
            text = text.repeat(1 + rng.below(100));
        }
        let params = Params { semitones: floats[rng.below(floats.len())], pace: floats[rng.below(floats.len())] };
        let all = voices();
        let id = all[rng.below(all.len())].id;
        let t = Instant::now();
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = script::parse(&text);
            let _ = filmcraft_tts::check_text(&text);
            if let Ok(v) = voice(id) {
                let _ = v.synthesize(&text, &params);
            }
        }));
        if r.is_err() || t.elapsed().as_secs() > 10 {
            bad.push(format!("{:?} {params:?} {:?}", text.chars().take(50).collect::<String>(), t.elapsed()));
        }
    }
    assert!(bad.is_empty(), "{} failures, e.g. {:?}", bad.len(), bad.first());
}
