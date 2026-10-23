//! Random scripts, lexicons and override lists must degrade, never panic or run away.

use filmcraft_tts_text::{Lang, Lexicon, Overrides, normalize, phonemize};
use std::collections::BTreeMap;
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
    "0",
    "1",
    "9",
    "99999999999999999999999999999",
    "-",
    "+",
    ".",
    ",",
    ":",
    "/",
    "%",
    "$",
    "£",
    "€",
    "#",
    "&",
    " ",
    "  ",
    "\n",
    "\t",
    "1st",
    "22nd",
    "3rd",
    "1/2",
    "3/0",
    "0/0",
    "12:34",
    "99:99",
    "25:61:61",
    "1999",
    "2024-02-30",
    "13/32/2024",
    "$1,000,000.50",
    "1,2,3",
    "1.2.3.4",
    "Dr.",
    "St.",
    "Mr.",
    "U.S.A.",
    "NASA",
    "[pause 5000]",
    "[pause 99999999999]",
    "[pause -1]",
    "[pause",
    "]",
    "[",
    "[pause 0]",
    "é",
    "ß",
    "日本",
    "😀",
    "\u{0}",
    "\u{200d}",
    "\u{301}",
    "AAAA",
    "x",
    "the",
    "a",
    "I",
    "...",
    "?!",
    "\"",
    "'",
    "(",
    ")",
    "—",
    "1e9",
    "0x1F",
    "1000000000000",
    "٣",
    "１２３",
];

fn script(rng: &mut Rng) -> String {
    let mut s = String::new();
    for _ in 0..rng.below(60) {
        s.push_str(PIECES[rng.below(PIECES.len())]);
        if rng.below(3) != 0 {
            s.push(' ');
        }
    }
    if rng.below(20) == 0 {
        s = s.repeat(1 + rng.below(200));
    }
    if rng.below(20) == 0 {
        s = (0..rng.below(300)).map(|_| char::from_u32(rng.next() as u32 % 0x11_0000).unwrap_or('?')).collect();
    }
    s
}

#[test]
fn hostile_scripts_never_panic() {
    let mut rng = Rng(0xfeed_face_cafe_beef);
    let lex = Lexicon::empty();
    let dict =
        Lexicon::parse_cmudict(b"HELLO  HH AH0 L OW1\nWORLD  W ER1 L D\n;;; comment\nBAD\n\xff\xfe\nTHE(2) DH AH0\n").unwrap_or_else(|_| Lexicon::empty());
    let mut failures = Vec::new();
    for i in 0..4000 {
        let text = script(&mut rng);
        let mut ov = BTreeMap::new();
        if i % 3 == 0 {
            ov.insert(script(&mut rng), script(&mut rng));
            ov.insert("the".into(), "/ðə/".into());
            ov.insert("x".into(), "/".into());
        }
        let ov = Overrides(ov);
        let lang = if i % 2 == 0 { Lang::EnUs } else { Lang::EnGb };
        let t = Instant::now();
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = normalize(&text, lang);
            let _ = phonemize(&text, lang, &lex, &ov);
            let _ = phonemize(&text, lang, &dict, &ov);
        }));
        if r.is_err() {
            failures.push(format!("{:?} ({:?})", text.chars().take(60).collect::<String>(), t.elapsed()));
        }
    }
    assert!(failures.is_empty(), "{} failures, e.g. {:?}", failures.len(), failures.first());
}

#[test]
fn hostile_dictionaries_never_panic() {
    let mut rng = Rng(0x1234_dead_beef_5678);
    for _ in 0..500 {
        let n = rng.below(2000);
        let bytes: Vec<u8> = (0..n)
            .map(|_| match rng.below(6) {
                0 => b'\n',
                1 => b' ',
                2 => b'(',
                3 => rng.next() as u8,
                _ => b'A' + (rng.next() % 26) as u8,
            })
            .collect();
        let r = catch_unwind(AssertUnwindSafe(|| {
            let _ = Lexicon::parse_cmudict(&bytes);
        }));
        assert!(r.is_ok(), "parse_cmudict panicked on {:?}", &bytes[..bytes.len().min(40)]);
    }
}
