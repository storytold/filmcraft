//! English pronunciation: spoken words → Kokoro phoneme symbols (task M7.11).
//!
//! [`phonemize`] normalizes a script ([`crate::normalize`]) and turns each word into
//! Kokoro's phoneme symbols: pronunciation overrides first, then a CMUdict-format
//! [`Lexicon`] (downloaded with the voices at run time, never committed), then
//! morphology on dictionary stems (plural/possessive `-s`, `-ed`, `-ing`, `-er`,
//! `-ly`, `un-`, `re-`), then letter-to-sound rules written from scratch.
//!
//! Output alphabet: only the characters in [`SYMBOLS`] — the ARPAbet→Kokoro
//! mapping below, the stress marks `ˈ` (primary) and `ˌ` (secondary), spaces
//! between words, and a small punctuation set. Claude's Kokoro code checks
//! against [`SYMBOLS`], so nothing outside it may ever be emitted.
//!
//! ARPAbet → Kokoro (American English): AA→ɑ, AE→æ, AH0→ə, AH1/2→ʌ, AO→ɔ, AW→W,
//! AY→I, EH→ɛ, ER0→ɚ, ER1/2→ɜɹ, EY→A, IH→ɪ, IY→i, OW→O, OY→Y, UH→ʊ, UW→u,
//! B→b, CH→ʧ, D→d, DH→ð, F→f, G→ɡ (U+0261), HH→h, JH→ʤ, K→k, L→l, M→m, N→n,
//! NG→ŋ, P→p, R→ɹ, S→s, SH→ʃ, T→t, TH→θ, V→v, W→w, Y→j, Z→z, ZH→ʒ.
//! `A I O W Y` are Kokoro's single-symbol diphthongs (eɪ aɪ oʊ aʊ ɔɪ).

use crate::normalize::{self, Lang, NormalizeError, Token};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Maximum characters in one [`Chunk::Phonemes`]; longer sentences are split.
pub const MAX_CHUNK_CHARS: usize = 400;
/// Maximum dictionary input size (16 MiB).
const MAX_DICT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum number of dictionary entries.
const MAX_DICT_ENTRIES: usize = 500_000;
/// Maximum number of overrides honoured (excess ignored, not an error).
const MAX_OVERRIDES: usize = 1_000;
/// Maximum length of one override value (longer ones ignored).
const MAX_OVERRIDE_LEN: usize = 200;

/// Every character [`phonemize`] may ever emit: the phoneme symbols, the two
/// stress marks, one space, and the kept punctuation. Claude's vocabulary check
/// is exactly this set.
pub const SYMBOLS: &str = "ˈˌ ɑæəʌɔɛɚɜɪʊiunAIOWYbʧdðfɡhʤklmŋpɹsʃtθvwjzʒ,.!?;:—…()\"";

/// Punctuation the normalizer may emit that is kept in phoneme output; anything
/// else (apostrophes, stray symbols) is dropped.
const KEPT_PUNCT: [char; 11] = [',', '.', '!', '?', ';', ':', '—', '…', '(', ')', '"'];

/// A pronunciation dictionary in CMUdict format.
#[derive(Clone, Debug, Default)]
pub struct Lexicon {
    /// Uppercase word → its pronunciations in ARPAbet (first is used for lookup).
    entries: BTreeMap<String, Vec<String>>,
    /// Lines skipped because they were malformed.
    skipped: usize,
}

/// Dictionary parse errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LexError {
    /// The input exceeds the 16 MiB limit.
    #[error("the pronunciation dictionary exceeds the 16 MiB limit")]
    TooLarge,
    /// The dictionary has more than 500 000 entries.
    #[error("the pronunciation dictionary has more than 500 000 entries")]
    TooManyEntries,
}

impl Lexicon {
    /// An empty dictionary (everything falls through to letter-to-sound rules).
    pub fn empty() -> Lexicon {
        Lexicon::default()
    }

    /// Parse CMUdict's text format: `WORD  P1 P2 …`, alternates as `WORD(2)`,
    /// `;;;` comment lines and ` #` trailing comments. Malformed lines are
    /// skipped and counted by [`Lexicon::skipped`], never an error. Refuses
    /// input over 16 MiB or with more than 500 000 entries ([`LexError`]).
    pub fn parse_cmudict(bytes: &[u8]) -> Result<Lexicon, LexError> {
        if bytes.len() > MAX_DICT_BYTES {
            return Err(LexError::TooLarge);
        }
        let text = String::from_utf8_lossy(bytes);
        let mut entries: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut skipped = 0usize;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(";;;") {
                continue;
            }
            // A trailing ` #` comment; words themselves never contain ` #`.
            let had_comment = line.contains(" #");
            let code = match line.find(" #") {
                Some(i) => line.get(..i).unwrap_or(line),
                None => line,
            };
            let code = code.trim();
            if code.is_empty() {
                continue;
            }
            let mut parts = code.split_whitespace();
            let Some(head) = parts.next() else {
                skipped += 1;
                continue;
            };
            let phones: Vec<&str> = parts.collect();
            if phones.is_empty() {
                if had_comment {
                    continue;
                }
                skipped += 1;
                continue;
            }
            // `WORD(2)` alternates fold into the same key.
            let key = match (head.find('('), head.rfind(')')) {
                (Some(open), Some(close)) if close > open + 1 && head.get(open + 1..close).is_some_and(|d| d.chars().all(|c| c.is_ascii_digit())) => {
                    head.get(..open).unwrap_or(head)
                }
                (Some(_), _) => {
                    skipped += 1;
                    continue;
                }
                (None, _) => head,
            };
            if phones.iter().any(|p| !is_arpabet_token(p)) {
                skipped += 1;
                continue;
            }
            if !entries.contains_key(key) && entries.len() >= MAX_DICT_ENTRIES {
                return Err(LexError::TooManyEntries);
            }
            let key = key.to_ascii_uppercase();
            entries.entry(key).or_default().push(phones.join(" "));
        }
        Ok(Lexicon { entries, skipped })
    }

    /// Number of distinct words in the dictionary.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Number of malformed lines skipped during parsing.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// Whether the dictionary is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All pronunciations of a word (already uppercase), first one first.
    fn pronunciations(&self, word_upper: &str) -> Option<&Vec<String>> {
        self.entries.get(word_upper)
    }
}

/// One ARPAbet token: a known symbol with an optional stress digit.
fn is_arpabet_token(token: &str) -> bool {
    let (letters, digits) = token.split_at(token.find(|c: char| c.is_ascii_digit()).unwrap_or(token.len()));
    if !matches!(digits, "" | "0" | "1" | "2") {
        return false;
    }
    matches!(
        letters,
        "AA" | "AE"
            | "AH"
            | "AO"
            | "AW"
            | "AY"
            | "B"
            | "CH"
            | "D"
            | "DH"
            | "EH"
            | "ER"
            | "EY"
            | "F"
            | "G"
            | "HH"
            | "IH"
            | "IY"
            | "JH"
            | "K"
            | "L"
            | "M"
            | "N"
            | "NG"
            | "OW"
            | "OY"
            | "P"
            | "R"
            | "S"
            | "SH"
            | "T"
            | "TH"
            | "UH"
            | "UW"
            | "V"
            | "W"
            | "Y"
            | "Z"
            | "ZH"
    )
}

/// The user's pronunciation list: word (case-insensitive) → how it should sound.
/// The value is either a respelling in plain English ("tee dee ay"), which goes
/// through the same dictionary + rules, or raw Kokoro phonemes between slashes
/// ("/tˌidˌiˈA/").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Overrides(pub BTreeMap<String, String>);

/// A piece of the phonemized script.
#[derive(Clone, Debug, PartialEq)]
pub enum Chunk {
    /// Kokoro phonemes for a stretch of speech, at most [`MAX_CHUNK_CHARS`] chars.
    Phonemes(String),
    /// A narration pause, straight from the normalizer's `[pause …]` markers.
    Pause {
        /// Pause length in milliseconds.
        millis: u32,
    },
}

/// Script → chunks: normalize, then phonemize each sentence (one
/// [`Chunk::Phonemes`] per sentence, split when over [`MAX_CHUNK_CHARS`]).
/// Never crashes; hostile input degrades (AGENTS.md §0).
pub fn phonemize(text: &str, lang: Lang, lex: &Lexicon, overrides: &Overrides) -> Result<Vec<Chunk>, NormalizeError> {
    let tokens = normalize::normalize(text, lang)?;
    let overrides = bounded_overrides(overrides);
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut sentence: Vec<Token> = Vec::new();
    for token in tokens {
        match token {
            Token::SentenceEnd => {
                // The sentence terminator attaches to the last word as a period.
                sentence.push(Token::Punct('.'));
                flush_sentence(&mut sentence, &mut chunks, lex, &overrides, lang);
            }
            Token::SentenceEndWith(c) => {
                // `?` and `!` shape Kokoro's intonation; keep them.
                sentence.push(Token::Punct(c));
                flush_sentence(&mut sentence, &mut chunks, lex, &overrides, lang);
            }
            Token::Pause { millis } => {
                flush_sentence(&mut sentence, &mut chunks, lex, &overrides, lang);
                chunks.push(Chunk::Pause { millis });
            }
            other => sentence.push(other),
        }
    }
    flush_sentence(&mut sentence, &mut chunks, lex, &overrides, lang);
    Ok(chunks)
}

/// The bounded view of the user's overrides: keys lowercased, at most
/// [`MAX_OVERRIDES`] entries of at most [`MAX_OVERRIDE_LEN`] chars — excess is
/// ignored, not an error.
fn bounded_overrides(overrides: &Overrides) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (word, value) in overrides.0.iter() {
        if out.len() >= MAX_OVERRIDES {
            break;
        }
        if value.chars().count() > MAX_OVERRIDE_LEN {
            continue;
        }
        out.insert(word.to_lowercase(), value.clone());
    }
    out
}

/// Phonemize one buffered sentence into zero or more `Phonemes` chunks.
fn flush_sentence(sentence: &mut Vec<Token>, chunks: &mut Vec<Chunk>, lex: &Lexicon, overrides: &BTreeMap<String, String>, lang: Lang) {
    let tokens = std::mem::take(sentence);
    let mut out = String::new();
    for token in &tokens {
        match token {
            Token::Word(w) => {
                let Some(p) = word_phonemes(w, lex, Some(overrides), lang) else { continue };
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&p);
            }
            Token::Punct(c) => {
                if KEPT_PUNCT.contains(c) {
                    out.push(*c);
                }
            }
            Token::Pause { .. } | Token::SentenceEnd | Token::SentenceEndWith(_) => {}
        }
    }
    for piece in split_long_chunk(out) {
        if !piece.is_empty() {
            chunks.push(Chunk::Phonemes(piece));
        }
    }
}

/// Split a phoneme string longer than [`MAX_CHUNK_CHARS`] at the last `,` `;` `:`
/// before the limit, else at the last space, else hard-split at the limit.
fn split_long_chunk(s: String) -> Vec<String> {
    if s.chars().count() <= MAX_CHUNK_CHARS {
        return vec![s];
    }
    let limit = s.char_indices().map(|(i, _)| i).nth(MAX_CHUNK_CHARS).unwrap_or(s.len());
    let head_boundary = s.get(..limit).and_then(|head| {
        head.char_indices().rev().find(|&(_, c)| matches!(c, ',' | ';' | ':')).map(|(i, _)| i + 1).or_else(|| head.rfind(' ').map(|i| i + 1)).or(Some(limit))
    });
    let Some(cut) = head_boundary else { return vec![s] };
    let head = s.get(..cut).unwrap_or("").trim_end().to_string();
    let tail = s.get(cut..).unwrap_or("").trim_start().to_string();
    let mut out = vec![head];
    out.extend(split_long_chunk(tail));
    out.retain(|p| !p.is_empty());
    out
}

/// Whether `c` may appear in raw-phoneme override values.
fn is_output_char(c: char) -> bool {
    SYMBOLS.contains(c)
}

/// Phonemes for one word: overrides → dictionary → morphology → letter-to-sound.
/// `overrides` is `None` while expanding a respelling, so overrides can't loop.
fn word_phonemes(word: &str, lex: &Lexicon, overrides: Option<&BTreeMap<String, String>>, lang: Lang) -> Option<String> {
    let lower = word.to_lowercase();
    if lower.is_empty() {
        return None;
    }
    if let Some(map) = overrides
        && let Some(value) = map.get(&lower)
    {
        if value.starts_with('/') {
            // A raw-phoneme override; an invalid one is ignored entirely.
            if let Some(raw) = raw_override(value) {
                return Some(raw);
            }
        } else {
            // A respelling: plain English words through dictionary + rules.
            let tokens = normalize::normalize(value, lang).ok()?;
            let mut out = String::new();
            for token in tokens {
                if let Token::Word(w) = token
                    && let Some(p) = word_phonemes(&w, lex, None, Lang::EnUs)
                {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&p);
                }
            }
            if !out.is_empty() {
                return Some(out);
            }
        }
    }
    // Spelled-out single letters use the letter's name; a lowercase single
    // letter that is a function word (a, i) stays on the normal path.
    if is_single_letter(word) {
        return Some(letter_phonemes(word, lex));
    }
    // Hyphenated words: each part goes through the same pipeline.
    if word.contains('-') && !word.contains(char::is_numeric) {
        let mut out = String::new();
        for part in word.split('-') {
            if let Some(p) = word_phonemes(part, lex, None, lang) {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&p);
            }
        }
        if !out.is_empty() {
            return Some(out);
        }
    }
    let upper = lower.to_uppercase();
    if let Some(pron) = lex.pronunciations(&upper)
        && let Some(first) = pron.first()
    {
        return Some(arpabet_to_kokoro(first, is_function_word(&lower)));
    }
    if let Some(p) = morphology(&lower, lex) {
        return Some(p);
    }
    letter_to_sound(&lower)
}

/// A raw-phoneme override `/…/`: strip the slashes and accept it only when every
/// character is in [`SYMBOLS`]; otherwise fall through to the respelling path.
fn raw_override(value: &str) -> Option<String> {
    let inner = value.strip_prefix('/')?.strip_suffix('/')?;
    if inner.is_empty() || !inner.chars().all(is_output_char) {
        return None;
    }
    Some(inner.to_string())
}

fn is_single_letter(word: &str) -> bool {
    let mut chars = word.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_alphabetic())
        && !(word.chars().all(char::is_lowercase) && is_function_word(&word.to_lowercase()))
}

/// English letter names in ARPAbet (public knowledge, not copied from any
/// project); used when the dictionary has no entry for the letter.
const LETTER_NAMES: [(&str, &str); 26] = [
    ("A", "EY1"),
    ("B", "B IY1"),
    ("C", "S IY1"),
    ("D", "D IY1"),
    ("E", "IY1"),
    ("F", "EH F1"),
    ("G", "JH IY1"),
    ("H", "EY CH1"),
    ("I", "AY1"),
    ("J", "JH EY1"),
    ("K", "K EY1"),
    ("L", "EH L1"),
    ("M", "EH M1"),
    ("N", "EH N1"),
    ("O", "OW1"),
    ("P", "P IY1"),
    ("Q", "K Y UW1"),
    ("R", "AA R1"),
    ("S", "EH S1"),
    ("T", "T IY1"),
    ("U", "Y UW1"),
    ("V", "V IY1"),
    ("W", "D AH1 B AH0 L Y UW1"),
    ("X", "EH K S1"),
    ("Y", "W AY1"),
    ("Z", "Z IY1"),
];

/// Phonemes for a spelled-out single uppercase letter: prefer the dictionary's
/// entry that matches the letter name (CMUdict lists "A" as the article first,
/// the letter name second), else the first entry, else the built-in name.
fn letter_phonemes(word: &str, lex: &Lexicon) -> String {
    let upper = word.to_uppercase();
    let letter_name = LETTER_NAMES.iter().find(|(l, _)| *l == upper).map(|(_, p)| *p);
    if let Some(pron) = lex.pronunciations(&upper) {
        if let Some(name) = letter_name
            && let Some(matched) = pron.iter().find(|p| p.as_str() == name)
        {
            return arpabet_to_kokoro(matched, false);
        }
        if let Some(first) = pron.first() {
            return arpabet_to_kokoro(first, false);
        }
    }
    if let Some(name) = letter_name {
        return arpabet_to_kokoro(name, false);
    }
    String::new()
}

/// Single-syllable function words carry no stress mark.
fn is_function_word(lower: &str) -> bool {
    matches!(
        lower,
        "a" | "an"
            | "the"
            | "of"
            | "to"
            | "and"
            | "in"
            | "is"
            | "it"
            | "for"
            | "on"
            | "with"
            | "at"
            | "by"
            | "as"
            | "be"
            | "or"
            | "but"
            | "from"
            | "that"
            | "this"
    )
}

/// One ARPAbet phoneme with its stress digit (if any).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Phone {
    symbol: String,
    stress: Option<u8>,
}

/// Split an ARPAbet pronunciation into phonemes.
fn parse_arpabet(pron: &str) -> Vec<Phone> {
    pron.split_whitespace()
        .map(|token| {
            let cut = token.find(|c: char| c.is_ascii_digit()).unwrap_or(token.len());
            let symbol = match token.get(..cut) {
                Some(s) => s,
                None => token,
            };
            let stress = token.get(cut..).and_then(|d| d.parse::<u8>().ok());
            Phone { symbol: symbol.to_string(), stress }
        })
        .collect()
}

/// Kokoro symbol for an ARPAbet phoneme (stress handled by the caller).
fn kokoro_symbol(symbol: &str, stress: Option<u8>) -> &'static str {
    match (symbol, stress) {
        ("AH", Some(0)) => "ə",
        ("ER", Some(0)) => "ɚ",
        ("AA", _) => "ɑ",
        ("AE", _) => "æ",
        ("AH", _) => "ʌ",
        ("AO", _) => "ɔ",
        ("AW", _) => "W",
        ("AY", _) => "I",
        ("EH", _) => "ɛ",
        ("ER", _) => "ɜɹ",
        ("EY", _) => "A",
        ("IH", _) => "ɪ",
        ("IY", _) => "i",
        ("OW", _) => "O",
        ("OY", _) => "Y",
        ("UH", _) => "ʊ",
        ("UW", _) => "u",
        ("B", _) => "b",
        ("CH", _) => "ʧ",
        ("D", _) => "d",
        ("DH", _) => "ð",
        ("F", _) => "f",
        ("G", _) => "ɡ",
        ("HH", _) => "h",
        ("JH", _) => "ʤ",
        ("K", _) => "k",
        ("L", _) => "l",
        ("M", _) => "m",
        ("N", _) => "n",
        ("NG", _) => "ŋ",
        ("P", _) => "p",
        ("R", _) => "ɹ",
        ("S", _) => "s",
        ("SH", _) => "ʃ",
        ("T", _) => "t",
        ("TH", _) => "θ",
        ("V", _) => "v",
        ("W", _) => "w",
        ("Y", _) => "j",
        ("Z", _) => "z",
        ("ZH", _) => "ʒ",
        _ => "",
    }
}

/// Whether an ARPAbet symbol is a vowel (carries stress).
fn is_vowel_symbol(symbol: &str) -> bool {
    matches!(symbol, "AA" | "AE" | "AH" | "AO" | "AW" | "AY" | "EH" | "ER" | "EY" | "IH" | "IY" | "OW" | "OY" | "UH" | "UW")
}

/// ARPAbet → Kokoro symbols, with `ˈ`/`ˌ` immediately before the stressed vowel
/// symbol (or no marks for function words).
fn arpabet_to_kokoro(pron: &str, strip_stress: bool) -> String {
    let mut out = String::new();
    for phone in parse_arpabet(pron) {
        let symbol = kokoro_symbol(&phone.symbol, phone.stress);
        if symbol.is_empty() {
            continue;
        }
        if !strip_stress && is_vowel_symbol(&phone.symbol) {
            match phone.stress {
                Some(1) => out.push('ˈ'),
                Some(2) => out.push('ˌ'),
                _ => {}
            }
        }
        out.push_str(symbol);
    }
    out
}

// ------------------------------------------------------------------ morphology

/// Suffix/prefix morphology on dictionary stems: plural/possessive `-s`/`-'s`
/// with voicing (s / z / ɪz), `-ed` (t / d / ɪd), `-ing`, `-er`, `-ly`, and the
/// prefixes `un-`, `re-`. Only fires when the stem is in the dictionary.
fn morphology(word: &str, lex: &Lexicon) -> Option<String> {
    // Possessive / plural.
    for stem in [strip_suffix(word, "'s"), strip_suffix(word, "s"), strip_suffix(word, "es")].into_iter().flatten() {
        if let Some(pron) = lex.pronunciations(&stem.to_uppercase())
            && let Some(first) = pron.first()
            && let Some(last) = parse_arpabet(first).last()
        {
            let s = plural_s(last);
            return Some(join_phonemes(&parse_arpabet(first), &[s]));
        }
    }
    // -ed.
    for stem in [strip_suffix(word, "ed"), strip_suffix(word, "d")].into_iter().flatten() {
        if let Some(pron) = lex.pronunciations(&stem.to_uppercase())
            && let Some(first) = pron.first()
            && let Some(last) = parse_arpabet(first).last()
        {
            let ed = past_ed(last);
            return Some(join_phonemes(&parse_arpabet(first), &[ed]));
        }
    }
    // -ing (stem may need its silent e back: making → make).
    let ing_stems: [Option<String>; 2] = [strip_suffix(word, "ing").map(|s| s.to_string()), strip_suffix(word, "ing").map(|s| format!("{s}e"))];
    for stem in ing_stems.into_iter().flatten() {
        let stem = stem.as_str();
        if let Some(pron) = lex.pronunciations(&stem.to_uppercase())
            && let Some(first) = pron.first()
        {
            return Some(join_phonemes(&parse_arpabet(first), &["IH", "NG"]));
        }
    }
    // -er, -ly.
    for (suffix, added) in [("er", ["ER0"].as_slice()), ("ly", ["L", "IY0"].as_slice())] {
        if let Some(stem) = strip_suffix(word, suffix)
            && let Some(pron) = lex.pronunciations(&stem.to_uppercase())
            && let Some(first) = pron.first()
        {
            return Some(join_phonemes(&parse_arpabet(first), added));
        }
    }
    // Prefixes un-, re-.
    for (prefix, added) in [("un", ["AH0", "N"]), ("re", ["R", "IY0"])] {
        if let Some(stem) = word.strip_prefix(prefix)
            && !stem.is_empty()
            && let Some(pron) = lex.pronunciations(&stem.to_uppercase())
            && let Some(first) = pron.first()
        {
            let mut phones: Vec<Phone> = added.iter().flat_map(|s| parse_arpabet(s)).collect();
            phones.extend(parse_arpabet(first));
            return Some(join_phonemes(&phones, &[]));
        }
    }
    None
}

/// Strip `suffix` (lowercase) from the end of a lowercase word.
fn strip_suffix<'a>(word: &'a str, suffix: &str) -> Option<&'a str> {
    let stem = word.strip_suffix(suffix)?;
    if stem.is_empty() {
        return None;
    }
    Some(stem)
}

/// Join phonemes into Kokoro symbols; the suffix phonemes get no stress marks
/// (they are unstressed in these endings).
fn join_phonemes(stem: &[Phone], suffix: &[&str]) -> String {
    let mut out = arpabet_to_kokoro_phones(stem, false);
    for group in suffix {
        for phone in parse_arpabet(group) {
            out.push_str(kokoro_symbol(&phone.symbol, phone.stress));
        }
    }
    out
}

/// ARPAbet phones → Kokoro with stress marks.
fn arpabet_to_kokoro_phones(phones: &[Phone], strip_stress: bool) -> String {
    let pron = phones
        .iter()
        .map(|p| match p.stress {
            Some(s) => format!("{}{s}", p.symbol),
            None => p.symbol.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    arpabet_to_kokoro(&pron, strip_stress)
}

/// The `-s`/`-'s` allomorph in ARPAbet: ɪz after sibilants, s after voiceless
/// sounds, z elsewhere.
fn plural_s(last: &Phone) -> &'static str {
    match last.symbol.as_str() {
        "S" | "Z" | "SH" | "ZH" | "CH" | "JH" => "IH0 Z",
        "P" | "T" | "K" | "F" | "TH" => "S",
        _ => "Z",
    }
}

/// The `-ed` allomorph: ɪd after t/d, t after voiceless sounds, d elsewhere.
fn past_ed(last: &Phone) -> &'static str {
    match last.symbol.as_str() {
        "T" | "D" => "IH0 D",
        "P" | "K" | "F" | "TH" | "S" | "SH" | "CH" => "T",
        _ => "D",
    }
}

// -------------------------------------------------------- letter-to-sound rules

/// Letter-to-sound rules for words the dictionary doesn't have, written from
/// scratch. Silent final e, vowel digraphs, `tion`/`sion`, `ph`/`gh`/`ck`/`qu`,
/// doubled consonants, soft c/g, `y` as vowel; primary stress on the first
/// syllable (on the vowel before -tion/-sion). Always pronounceable.
fn letter_to_sound(word: &str) -> Option<String> {
    let letters: Vec<char> = word.chars().filter(|c| c.is_ascii_alphabetic()).collect();
    if letters.is_empty() {
        return None;
    }
    // -ture / -sure endings (nature, measure): the e belongs to the suffix, so it
    // is handled before the silent-e rule.
    if letters.len() >= 5 {
        let tail_chars = letters.get(letters.len() - 4..)?;
        let tail: String = tail_chars.iter().collect();
        if tail == "ture" || tail == "sure" {
            let head_chars = letters.get(..letters.len() - 4)?;
            let head: String = head_chars.iter().collect();
            let mut body = letter_to_sound(&head).unwrap_or_default();
            if !body.is_empty() && !body.contains('ˈ') {
                // mark the head's first vowel
                if let Some(pos) = body.chars().collect::<Vec<_>>().iter().position(|c| is_vowel_kokoro(&c.to_string())) {
                    let mut v: Vec<char> = body.chars().collect();
                    v.insert(pos, 'ˈ');
                    body = v.into_iter().collect();
                }
            }
            body.push_str(if tail == "ture" { "ʧɚ" } else { "ʒɚ" });
            return Some(body);
        }
    }
    // Silent final e: drop it and lengthen the last vowel ("cake" → kˈA k).
    let mut chars = letters.clone();
    let mut long_last = false;
    let mut last_vowel_index: Option<usize> = None;
    let ends_consonant_le = chars.len() >= 3
        && chars.last() == Some(&'e')
        && chars.get(chars.len() - 2) == Some(&'l')
        && chars.get(chars.len() - 3).is_some_and(|c| !is_vowel_letter(*c));
    if !ends_consonant_le && chars.len() >= 3 && chars.last() == Some(&'e') && chars.get(chars.len() - 2).is_some_and(|c| !is_vowel_letter(*c)) {
        chars.pop();
        long_last = true;
        last_vowel_index = chars.iter().rposition(|c| is_vowel_letter(*c));
    }
    let n = chars.len();
    let mut out: Vec<String> = Vec::new();
    let mut vowel_positions: Vec<usize> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let Some(rest) = chars.get(i..) else { break };
        let rest_str: String = rest.iter().collect();
        let at_start = i == 0;
        // Multi-letter rules first (longest match wins).
        if rest_str.ends_with("tion") && rest_str.len() == 4 {
            // The vowel before the suffix takes the primary stress.
            if let Some(&last) = vowel_positions.last()
                && let Some(v) = out.get_mut(last)
            {
                *v = format!("ˈ{v}");
            }
            out.push("ʃ".into());
            out.push("ə".into());
            out.push("n".into());
            break;
        }
        if rest_str.ends_with("sion") && rest_str.len() == 4 {
            if let Some(&last) = vowel_positions.last()
                && let Some(v) = out.get_mut(last)
            {
                *v = format!("ˈ{v}");
            }
            out.push("ʒ".into());
            out.push("ə".into());
            out.push("n".into());
            break;
        }
        if rest_str.starts_with("sch") {
            out.push("s".into());
            out.push("k".into());
            i += 3;
            continue;
        }
        if rest_str == "sten" {
            // listen
            out.push("s".into());
            out.push("ə".into());
            out.push("n".into());
            break;
        }
        if rest_str == "gue" {
            out.push("ɡ".into());
            break;
        }
        if rest_str == "que" {
            out.push("k".into());
            break;
        }
        if rest_str.starts_with("ough") {
            // A famously irregular group; pick the "thought" vowel, f when final.
            out.push("ɔ".into());
            if rest_str == "ough" {
                out.push("f".into());
                break;
            }
            if rest_str.starts_with("ought") {
                out.push("t".into());
                i += 5;
                continue;
            }
            i += 3;
            continue;
        }
        if rest_str.starts_with("gu") && rest_str.len() > 2 {
            // The u is silent before a vowel (guard, guess).
            out.push("ɡ".into());
            i += 2;
            continue;
        }
        if rest_str.starts_with("ps") && at_start {
            out.push("s".into());
            i += 2;
            continue;
        }
        let two: String = rest.iter().take(2).collect();
        let three: String = rest.iter().take(3).collect();
        match three.as_str() {
            "igh" => {
                push_vowel(&mut out, &mut vowel_positions, "I");
                i += 3;
                continue;
            }
            "tch" => {
                out.push("ʧ".into());
                i += 3;
                continue;
            }
            "dge" => {
                out.push("ʤ".into());
                i += 3;
                continue;
            }
            _ => {}
        }
        match two.as_str() {
            "ch" => {
                out.push("ʧ".into());
                i += 2;
                continue;
            }
            "sh" => {
                out.push("ʃ".into());
                i += 2;
                continue;
            }
            "th" => {
                out.push("θ".into());
                i += 2;
                continue;
            }
            "ph" => {
                out.push("f".into());
                i += 2;
                continue;
            }
            "ck" => {
                out.push("k".into());
                i += 2;
                continue;
            }
            "qu" => {
                out.push("k".into());
                out.push("w".into());
                i += 2;
                continue;
            }
            "wh" => {
                out.push("w".into());
                i += 2;
                continue;
            }
            "gh" => {
                // Word-final → f (laugh, tough); before t → silent (knight's "igh"
                // rule already handled it); otherwise g (ghost).
                if i + 2 == n {
                    out.push("f".into());
                } else if chars.get(i + 2).copied() != Some('t') {
                    out.push("ɡ".into());
                }
                i += 2;
                continue;
            }
            "ng" if i + 2 == n => {
                out.push("ŋ".into());
                i += 2;
                continue;
            }
            "nk" => {
                out.push("ŋ".into());
                out.push("k".into());
                i += 2;
                continue;
            }
            "kn" if at_start => {
                out.push("n".into());
                i += 2;
                continue;
            }
            "gn" if at_start => {
                out.push("n".into());
                i += 2;
                continue;
            }
            "wr" if at_start => {
                out.push("ɹ".into());
                i += 2;
                continue;
            }
            "mb" if i + 2 == n => {
                out.push("m".into());
                i += 2;
                continue;
            }
            _ => {}
        }
        let Some(c) = chars.get(i) else { break };
        let c = *c;
        if is_vowel_letter(c) {
            // Vowel digraphs.
            let digraph: String = rest.iter().take(2).collect();
            let digraph_symbol = match digraph.as_str() {
                "ee" | "ea" | "ie" => Some("i"),
                "oo" | "ue" | "ui" => Some("u"),
                "ou" => Some("W"),
                "ow" => Some("O"),
                "oi" | "oy" => Some("Y"),
                "ai" | "ay" | "ei" => Some("A"),
                "au" | "aw" => Some("ɔ"),
                "oa" | "oe" => Some("O"),
                _ => None,
            };
            if let Some(sym) = digraph_symbol.filter(|_| rest.len() >= 2) {
                push_vowel(&mut out, &mut vowel_positions, sym);
                i += 2;
                continue;
            }
            // A doubled consonant after the vowel keeps it short (little → ˈlɪtəl).
            let doubled = chars.get(i + 1) == chars.get(i + 2) && chars.get(i + 1).is_some_and(|c| !is_vowel_letter(*c));
            let long = long_last && last_vowel_index == Some(i) && !doubled;
            let sym = match (c, long) {
                ('a', true) => "A",
                ('e', true) => "i",
                ('i', true) => "I",
                ('o', true) => "O",
                ('u', true) => "u",
                ('a', _) => "æ",
                ('e', _) => "ɛ",
                ('i', _) => "ɪ",
                ('o', _) => "ɑ",
                ('u', _) => "ʌ",
                ('y', _) => "ɪ",
                _ => "ə",
            };
            push_vowel(&mut out, &mut vowel_positions, sym);
            i += 1;
            continue;
        }
        // Consonants.
        match c {
            'c' => {
                let soft = chars.get(i + 1).is_some_and(|n| matches!(n, 'e' | 'i' | 'y'));
                out.push(if soft { "s".into() } else { "k".into() });
            }
            'g' => {
                let soft = chars.get(i + 1).is_some_and(|n| matches!(n, 'e' | 'i' | 'y'));
                out.push(if soft { "ʤ".into() } else { "ɡ".into() });
            }
            'x' => {
                out.push("k".into());
                out.push("s".into());
            }
            'j' => out.push("ʤ".into()),
            'q' => {
                out.push("k".into());
            }
            'l' if i + 2 == n && chars.get(i + 1) == Some(&'e') && i > 0 && chars.get(i - 1).is_some_and(|c| !is_vowel_letter(*c)) => {
                // "little", "castle": the final "le" is a syllable əl.
                out.push("ə".into());
                out.push("l".into());
                i += 2;
                continue;
            }
            'y' if at_start => out.push("j".into()),
            'y' if i + 1 == n => out.push("i".into()),
            'y' => out.push("ɪ".into()),
            other => {
                let sym = match other {
                    'b' => "b",
                    'd' => "d",
                    'f' => "f",
                    'h' => "h",
                    'k' => "k",
                    'l' => "l",
                    'm' => "m",
                    'n' => "n",
                    'p' => "p",
                    'r' => "ɹ",
                    's' => "s",
                    't' => "t",
                    'v' => "v",
                    'w' => "w",
                    'z' => "z",
                    _ => "",
                };
                if !sym.is_empty() {
                    out.push(sym.into());
                }
            }
        }
        i += 1;
    }
    // Doubled consonants collapse ("narration": rr → r).
    let mut collapsed: Vec<String> = Vec::new();
    for sym in out {
        if collapsed.last() == Some(&sym) && sym.chars().count() == 1 && !is_vowel_kokoro(&sym) {
            continue;
        }
        collapsed.push(sym);
    }
    if collapsed.is_empty() {
        return None;
    }
    // Primary stress on the first vowel if not already marked.
    if !collapsed.iter().any(|s| s.contains('ˈ') || s.contains('ˌ'))
        && let Some(pos) = collapsed.iter().position(|s| is_vowel_kokoro(s))
        && let Some(v) = collapsed.get_mut(pos)
    {
        *v = format!("ˈ{v}");
    }
    Some(collapsed.join(""))
}

fn push_vowel(out: &mut Vec<String>, positions: &mut Vec<usize>, sym: &str) {
    positions.push(out.len());
    out.push(sym.to_string());
}

fn is_vowel_letter(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

fn is_vowel_kokoro(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some('ˈ') | Some('ˌ') => {
            matches!(chars.next(), Some('ɑ' | 'æ' | 'ə' | 'ʌ' | 'ɔ' | 'ɛ' | 'ɚ' | 'ɜ' | 'ɪ' | 'ʊ' | 'i' | 'u' | 'A' | 'I' | 'O' | 'W' | 'Y'))
        }
        Some(c) => matches!(c, 'ɑ' | 'æ' | 'ə' | 'ʌ' | 'ɔ' | 'ɛ' | 'ɚ' | 'ɜ' | 'ɪ' | 'ʊ' | 'i' | 'u' | 'A' | 'I' | 'O' | 'W' | 'Y'),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small CMUdict-format dictionary typed for these tests (55 entries,
    /// 53 distinct words).
    const TEST_DICT: &str = r#";;; small test dictionary
HELLO  HH AH0 L OW1
WORLD  W ER1 L D
READ  R IY1 D
READ(2)  R EH D
THE  DH AH0
OF  AH0 V
TO  T UW1
AND  AE1 N D
IN  IH0 N
IS  IH0 Z
IT  IH0 T
FOR  F AO1 R
ON  AA1 N
WITH  W IH0 TH
AT  AE1 T
BY  B AY1
AS  AE1 Z
BE  B IY1
OR  AO1 R
BUT  B AH1 T
FROM  F R AH0 M
THAT  DH AE1 T
THIS  DH IH0 S
AN  AE1 N
A  AH0
A(2)  EY1
I  AY1
T  T IY1
D  D IY1
CAT  K AE1 T
DOG  D AO1 G
WISH  W IH1 SH
PLAY  P L EY1
MAKE  M EY1 K
SLOW  S L OW1
DO  D UW1
FIVE  F AY1 V
CENT  S EH1 N T
THREE  TH R IY1
DOLLAR  D AA1 L ER0
FIFTY  F IH1 F T IY0
ZERO  Z IH1 R OW
ONE  W AH1 N
FOUR  F AO1 R
THIRTY  T ER1 T IY0
SEVEN  S EH1 V AH0 N
TWENTY  T W EH1 N T IY0
NINETY  N AY1 N T IY0
NINETEEN  N AY1 N T IY1 N
TWO  T UW1
NINE  N AY1 N
OCTOBER  AA0 K T OW1 B ER0
CALL  K AO1 L
OFF  AO1 F
PERCENT  P ER0 S EH1 N T
"#;

    pub(crate) fn test_lex() -> Lexicon {
        Lexicon::parse_cmudict(TEST_DICT.as_bytes()).unwrap()
    }

    /// Flatten chunks: phonemes as-is, pauses as `[pause N]`.
    fn flat(chunks: &[Chunk]) -> String {
        let mut s = String::new();
        for c in chunks {
            match c {
                Chunk::Phonemes(p) => {
                    if !s.is_empty() {
                        s.push(' ');
                    }
                    s.push_str(p);
                }
                Chunk::Pause { millis } => {
                    if !s.is_empty() {
                        s.push(' ');
                    }
                    s.push_str(&format!("[pause {millis}]"));
                }
            }
        }
        s
    }

    fn speak(text: &str) -> String {
        flat(&phonemize(text, Lang::EnUs, &test_lex(), &Overrides::default()).unwrap())
    }

    #[test]
    fn parses_cmudict() {
        let lex = test_lex();
        assert_eq!(lex.len(), 53);
        assert_eq!(lex.skipped(), 0);
        // Alternates fold into one key; lookup uses the first pronunciation.
        assert_eq!(lex.pronunciations("READ").map(Vec::as_slice), Some(&["R IY1 D".to_string(), "R EH D".to_string()][..]));
        assert_eq!(lex.pronunciations("A").map(|v| v.len()), Some(2));
        assert!(lex.pronunciations("MISSING").is_none());
        assert!(Lexicon::parse_cmudict(b"").unwrap().is_empty());
    }

    #[test]
    fn skips_malformed_lines_and_counts_them() {
        let input =
            b";;; a comment\n\nGOOD  AE1\nBADLINE\nONLYWORD\nWORD  XX1 YY2\nBAD(  AE1\nWORD(2)  AA1\nWORD  AE1 # trailing comment\nTRAIL  AE1 T # note\n";
        let lex = Lexicon::parse_cmudict(input).unwrap();
        assert_eq!(lex.len(), 3); // GOOD, WORD (two pronunciations), TRAIL
        assert_eq!(lex.skipped(), 4); // BADLINE, ONLYWORD, WORD XX1 YY2, BAD(
        assert_eq!(lex.pronunciations("WORD").map(|v| v.first().map(String::as_str)), Some(Some("AA1")));
        // The trailing comment is stripped, not part of the pronunciation.
        assert_eq!(lex.pronunciations("TRAIL").map(|v| v.first().map(String::as_str)), Some(Some("AE1 T")));
    }

    #[test]
    fn refuses_oversized_dictionaries() {
        let oversized = vec![b'a'; MAX_DICT_BYTES + 1];
        assert!(matches!(Lexicon::parse_cmudict(&oversized), Err(LexError::TooLarge)));
        let mut big = String::new();
        for i in 0..MAX_DICT_ENTRIES + 1 {
            big.push_str(&format!("W{i}  AE1\n"));
        }
        let big = big.into_bytes();
        assert!(matches!(Lexicon::parse_cmudict(&big), Err(LexError::TooManyEntries)));
    }

    #[test]
    fn table_cases() {
        let cases: [(&str, &str); 46] = [
            // Dictionary words, stress placement, function words.
            ("Hello world", "həlˈO wˈɜɹld"),
            ("hello", "həlˈO"),
            ("the world", "ðə wˈɜɹld"),
            ("world", "wˈɜɹld"),
            ("read", "ɹˈid"),
            ("I read", "ˈI ɹˈid"),
            ("T D A", "tˈi dˈi ˈA"),
            ("NASA", "nˈæsæ"),
            ("a cat", "ə kˈæt"),
            ("an apple", "æn ˈæpəl"),
            ("It is on", "ɪt ɪz ɑn"),
            // Numbers from the normalizer.
            ("$3.50", "θɹˈi dˈɑlɚz ænd fˈɪfti sˈɛnts"),
            ("in 1999.", "ɪn nˈIntˈin nˈInti nˈIn."),
            ("5 cats", "fˈIv kˈæts"),
            ("Call 555-0134.", "kˈɔl fˈIv fˈIv fˈIv zˈɪɹO wˈʌn θɹˈi fˈɔɹ."),
            ("At 3:30.", "æt θɹˈi tˈɜɹti."),
            ("Up 20%.", "ˈʌp twˈɛnti pɚsˈɛnt."),
            ("On 10/7/2026.", "ɑn ɑktˈObɚ sˈɛvɛnθ twˈɛnti twˈɛnti sˈɪks."),
            // Morphology on dictionary stems.
            ("worlds", "wˈɜɹldz"),
            ("hellos", "həlˈOz"),
            ("read's", "ɹˈidz"),
            ("cats", "kˈæts"),
            ("dogs", "dˈɔɡz"),
            ("wishes", "wˈɪʃɪz"),
            ("played", "plˈAd"),
            ("making", "mˈAkɪŋ"),
            ("slower", "slˈOɚ"),
            ("slowly", "slˈOli"),
            ("undo", "əndˈu"),
            ("redo", "ɹidˈu"),
            ("cents", "sˈɛnts"),
            ("fives", "fˈIvz"),
            // Letter-to-sound for unknown words.
            ("kokoro", "kˈɑkɑɹɑ"),
            ("filmcraft", "fˈɪlmkɹæft"),
            ("knight", "nˈIt"),
            // Punctuation attachment, kept and dropped.
            ("hello, world.", "həlˈO, wˈɜɹld."),
            ("hello $ world", "həlˈO wˈɜɹld"),
            ("end.", "ˈɛnd."),
            ("Wow!", "wˈO!"),
            ("Really?", "ɹˈilɪ?"),
            ("a: b; c", "ə: bˈi; sˈi"),
            ("Tom & Jerry", "tˈɑm ænd ʤˈɛɹɪ"),
            ("Version 2. Next.", "vˈɛɹʒən tˈu. nˈɛkst."),
            ("It was 3.5 percent.", "ɪt wˈæs θɹˈi pˈYnt fˈIv pɚsˈɛnt."),
            // The review's two sentences, end to end.
            ("Dr. Smith lives on Main St. in 1999.", "dˈɑktɑɹ smˈɪθ lˈɪvɛs ɑn mˈAn stɹˈit ɪn nˈIntˈin nˈInti nˈIn."),
            (
                "The TDA project for NASA costs $1,234,567.",
                "ðə tˈi dˈi ˈA pɹˈɑʤɛkt fɔɹ nˈæsæ kˈɑsts wˈʌn mˈɪlɪɑn tˈu hˈʌndɹɛd tˈɜɹti fˈɔɹ θˈWsænd fˈIv hˈʌndɹɛd sˈɪkstɪ sˈɛvən dˈɑlɚz.",
            ),
        ];
        for (input, expected) in &cases {
            assert_eq!(&speak(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn overrides_both_forms_and_caps() {
        // Raw Kokoro phonemes and a respelling.
        let ov = Overrides([("kokoro".into(), "/kˈOkOɹO/".into()), ("tda".into(), "tee dee ay".into())].into_iter().collect());
        assert_eq!(speak_with(&ov, "kokoro"), "kˈOkOɹO");
        assert_eq!(speak_with(&ov, "TDA"), "tˈi dˈi ˈA");
        // Case-insensitive keys.
        assert_eq!(speak_with(&ov, "KOKORO"), "kˈOkOɹO");
        // A raw value with a character outside SYMBOLS falls back to respelling.
        let bad = Overrides([("kokoro".into(), "/kəmˈpuːtər/".into())].into_iter().collect());
        assert_eq!(speak_with(&bad, "kokoro"), "kˈɑkɑɹɑ");
    }

    fn speak_with(ov: &Overrides, text: &str) -> String {
        flat(&phonemize(text, Lang::EnUs, &test_lex(), ov).unwrap())
    }

    #[test]
    fn letter_to_sound_cases() {
        let cases: [(&str, &str); 36] = [
            ("filmcraft", "fˈɪlmkɹæft"),
            ("narration", "næɹˈæʃən"),
            ("kokoro", "kˈɑkɑɹɑ"),
            ("phone", "fˈOn"),
            ("knight", "nˈIt"),
            ("cake", "kˈAk"),
            ("quickly", "kwˈɪklɪ"),
            ("thought", "θˈɔt"),
            ("ghost", "ɡˈɑst"),
            ("chemistry", "ʧˈɛmɪstɹɪ"),
            ("giant", "ʤˈɪænt"),
            ("gym", "ʤˈɪm"),
            ("xylophone", "ksˈɪlɑfOn"),
            ("little", "lˈɪtəl"),
            ("ocean", "ˈɑsin"),
            ("python", "pˈɪθɑn"),
            ("rhythm", "ɹhˈɪθm"),
            ("island", "ˈɪslænd"),
            ("listen", "lˈɪsən"),
            ("castle", "kˈæstəl"),
            ("answer", "ˈænswɛɹ"),
            ("autumn", "ˈɔtʌmn"),
            ("conscious", "kˈɑnsɪWs"),
            ("schedule", "skˈɛdul"),
            ("nature", "nˈæʧɚ"),
            ("measure", "mˈiʒɚ"),
            ("vision", "vˈɪʒən"),
            ("education", "ɛdʌkˈæʃən"),
            ("example", "ˈɛksæmpəl"),
            ("exhibit", "ˈɛkshɪbɪt"),
            ("hour", "hˈWɹ"),
            ("honest", "hˈɑnɛst"),
            ("wrong", "ɹˈɑŋ"),
            ("wren", "ɹˈɛn"),
            ("gnome", "nˈOm"),
            ("psalm", "sˈælm"),
        ];
        for (word, expected) in &cases {
            assert_eq!(&letter_to_sound(word).unwrap(), expected, "word {word}");
        }
        // More, checked for pronounceability and non-emptiness rather than exact form.
        for w in ["lamb", "climb", "song", "bank", "laugh", "tough", "cough", "plague", "guard", "business", "colonel", "smile", "bridge", "wrist", "knock"] {
            let out = letter_to_sound(w).unwrap();
            assert!(!out.is_empty(), "{w} produced nothing");
            assert!(out.chars().all(|c| SYMBOLS.contains(c)), "{w} → {out:?}");
            println!("LTS {w} -> {out}");
        }
    }

    #[test]
    fn chunks_are_bounded_and_split_at_punctuation() {
        // A long sentence with many commas: splits at commas, each chunk ≤ 400.
        let sentence = "one, two, three, ".repeat(200);
        let chunks = phonemize(&sentence, Lang::EnUs, &test_lex(), &Overrides::default()).unwrap();
        assert!(chunks.len() > 1, "expected a split, got {} chunk(s)", chunks.len());
        for c in &chunks {
            let Chunk::Phonemes(p) = c else { panic!("not phonemes") };
            assert!(p.chars().count() <= MAX_CHUNK_CHARS, "chunk of {} chars", p.chars().count());
        }
        // The split happened at commas: no chunk ends mid-word.
        for c in &chunks[..chunks.len() - 1] {
            let Chunk::Phonemes(p) = c else { continue };
            assert!(
                p.ends_with(',') || p.ends_with(';') || p.ends_with(':') || p.ends_with('w') || p.ends_with("ʌn") || p.ends_with("tˈu"),
                "bad split: {p:?}"
            );
        }
    }

    #[test]
    fn every_output_char_is_in_symbols() {
        let inputs = [
            "Call 555-0134 by 3:30 p.m. on Oct. 7th, it's $3.50 (20% off)",
            "Hello world. [pause 1s] T D A & NASA, really?!",
            "1,234,567 dollars; 1999 was … fun — (honest)!",
            "Version 2. Next. Up 20%. At 3:30. On 10/7/2026.",
            "",
            "   ",
            "$$$$",
            "[pause",
            "[pause x]",
            "日本語",
            "emoji 👍👍👍",
            "العربية",
            "\u{0301}",
            "\0",
            "3:",
            "1,,2",
            ".....",
            "[[[[",
            "-",
            "$",
            "3:305",
            "5-",
            "999999999999999999999999",
            "007",
            "12345678901234",
            "filmcraft narration kokoro",
            "Dr. Smith lives on Main St. in 1999.",
            "The TDA project for NASA costs $1,234,567.",
        ];
        for input in &inputs {
            let Ok(chunks) = phonemize(input, Lang::EnUs, &test_lex(), &Overrides::default()) else { continue };
            for c in chunks {
                let Chunk::Phonemes(p) = c else { continue };
                for ch in p.chars() {
                    assert!(SYMBOLS.contains(ch), "{input:?} produced {p:?} — bad char {ch:?} U+{:04X}", ch as u32);
                }
            }
        }
    }

    #[test]
    fn hostile_inputs_do_not_panic() {
        let ov_big = Overrides((0..1001).map(|i| (format!("w{i}"), "/ɑ/".to_string())).collect());
        let ov_long = Overrides([("x".into(), "a".repeat(201))].into_iter().collect());
        let huge = "a".repeat(2 * 1024 * 1024);
        let long_word = "a".repeat(10_000);
        let default = Overrides::default();
        let hostile: Vec<(&str, &Overrides)> = vec![
            ("", &default),
            ("   \t\n ", &default),
            (huge.as_str(), &default),
            (long_word.as_str(), &default),
            ("$$$$", &default),
            ("日本語のテキスト", &default),
            ("emoji 👍👍👍", &default),
            ("العربية", &default),
            ("\u{0301}", &default),
            ("\0", &default),
            ("hello", &ov_big),
            ("hello", &ov_long),
            ("[pause 999999999999s]", &default),
        ];
        for (input, ov) in &hostile {
            let result = std::panic::catch_unwind(|| phonemize(input, Lang::EnUs, &test_lex(), ov));
            assert!(result.is_ok(), "panicked on {input:?}");
        }
        // The oversized input errors instead of degrading.
        assert!(phonemize(&huge, Lang::EnUs, &test_lex(), &default).is_err());
    }

    #[test]
    fn overrides_are_capped_not_errors() {
        let mut map = BTreeMap::new();
        for i in 0..1100 {
            map.insert(format!("w{i}"), "/ɑ/".to_string());
        }
        map.insert("hello".to_string(), "/oʊ/".to_string()); // beyond 1000? keys sorted: w0.. so "hello" is after w-entries → dropped
        let ov = Overrides(map);
        // 1001st+ entries ignored silently; no panic, no error.
        let out = phonemize("hello", Lang::EnUs, &test_lex(), &ov).unwrap();
        assert!(!out.is_empty());
    }

    /// Measured accuracy against a real CMUdict, when the environment provides one
    /// (FILMCRAFT_CMUDICT=/path/to/cmudict.dict). Holds out every 50th entry,
    /// rebuilds the lexicon without them, and reports letter-to-sound word
    /// accuracy and phoneme error rate on the held-out words.
    #[test]
    fn measured_accuracy_against_cmudict() {
        let Ok(path) = std::env::var("FILMCRAFT_CMUDICT") else {
            eprintln!("measured_accuracy_against_cmudict: FILMCRAFT_CMUDICT not set; skipping");
            return;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            eprintln!("measured_accuracy_against_cmudict: cannot read {path}; skipping");
            return;
        };
        let full = Lexicon::parse_cmudict(&bytes).unwrap();
        // Hold out every 50th entry.
        let mut kept_text = String::new();
        let mut held_out: Vec<(String, String)> = Vec::new();
        for (i, (word, prons)) in full.entries.iter().enumerate() {
            if i % 50 == 49 {
                if let Some(first) = prons.first() {
                    held_out.push((word.clone(), first.clone()));
                }
                continue;
            }
            for pron in prons {
                kept_text.push_str(&format!("{word}  {pron}\n"));
            }
        }
        let lex = Lexicon::parse_cmudict(kept_text.as_bytes()).unwrap();
        let _ = &lex;
        let mut words_hit = 0usize;
        let mut phoneme_total = 0usize;
        let mut phoneme_errors = 0usize;
        for (word, truth) in &held_out {
            let Some(guess) = letter_to_sound(&word.to_lowercase()) else { continue };
            let expected = arpabet_to_kokoro(truth, false);
            let g: Vec<char> = guess.chars().collect();
            let e: Vec<char> = expected.chars().collect();
            if g == e {
                words_hit += 1;
            }
            phoneme_total += e.len().max(1);
            phoneme_errors += edit_distance(&g, &e);
        }
        let n = held_out.len().max(1);
        let accuracy = words_hit as f64 / n as f64;
        let per = phoneme_errors as f64 / phoneme_total as f64;
        println!("held out {} words: word accuracy {:.1}%, phoneme error rate {:.1}%", words_hit, accuracy * 100.0, per * 100.0);
        assert!(!held_out.is_empty());
    }

    /// Levenshtein distance over phoneme characters.
    fn edit_distance(a: &[char], b: &[char]) -> usize {
        let mut prev: Vec<usize> = (0..=b.len()).collect();
        for i in 1..=a.len() {
            let mut cur = vec![i; b.len() + 1];
            for j in 1..=b.len() {
                let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
                cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
            }
            prev = cur;
        }
        prev[b.len()]
    }
}
