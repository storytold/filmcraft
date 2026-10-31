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
/// Most ARPAbet symbols retained for one dictionary pronunciation.
const MAX_PRONUNCIATION_PHONES: usize = 256;
/// Long unknown words are not useful narration and can make suffix fallback recurse repeatedly.
const MAX_LETTER_TO_SOUND_CHARS: usize = 512;
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
            let phones: Vec<&str> = parts.by_ref().take(MAX_PRONUNCIATION_PHONES + 1).collect();
            if phones.is_empty() {
                if had_comment {
                    continue;
                }
                skipped += 1;
                continue;
            }
            if phones.len() > MAX_PRONUNCIATION_PHONES {
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
    let mut out = Vec::new();
    let mut start = 0usize;
    while start < s.len() {
        let rest = &s[start..];
        let Some((limit, _)) = rest.char_indices().nth(MAX_CHUNK_CHARS) else {
            let piece = rest.trim().to_string();
            if !piece.is_empty() {
                out.push(piece);
            }
            break;
        };
        let head = &rest[..limit];
        let cut = head
            .char_indices()
            .rev()
            .find(|&(_, c)| matches!(c, ',' | ';' | ':'))
            .map(|(i, _)| i + 1)
            .or_else(|| head.rfind(' ').map(|i| i + 1))
            .unwrap_or(limit);
        let piece = head[..cut].trim_end();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }
        start += cut;
        while s.get(start..).and_then(|rest| rest.chars().next()).is_some_and(char::is_whitespace) {
            start += s[start..].chars().next().map(char::len_utf8).unwrap_or(0);
        }
    }
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
    let ing_stems: [Option<String>; 2] = [strip_suffix(word, "ing").map(ToString::to_string), strip_suffix(word, "ing").map(|s| format!("{s}e"))];
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
    let letters: Vec<char> = word.chars().filter(char::is_ascii_alphabetic).collect();
    if letters.is_empty() || letters.len() > MAX_LETTER_TO_SOUND_CHARS {
        return None;
    }
    if let Some(word) = ture_sure_word(&letters) {
        return Some(word);
    }
    let (chars, long_last, last_vowel_index) = strip_silent_e(&letters);
    let mut lts = LetterToSound { chars: &chars, out: Vec::new(), vowel_positions: Vec::new(), long_last, last_vowel_index };
    let mut i = 0usize;
    while i < chars.len() {
        match lts.step(i) {
            Step::Next(next) => i = next,
            Step::Stop => break,
        }
    }
    lts.finish()
}

/// -ture / -sure endings (nature, measure): the e belongs to the suffix, so the word is the head's
/// sounds (stressed on its first vowel) plus the suffix, before the silent-e rule applies.
fn ture_sure_word(letters: &[char]) -> Option<String> {
    if letters.len() < 5 {
        return None;
    }
    let tail: String = letters.get(letters.len() - 4..)?.iter().collect();
    if tail != "ture" && tail != "sure" {
        return None;
    }
    let head: String = letters.get(..letters.len() - 4)?.iter().collect();
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
    Some(body)
}

/// Silent final e: drop it and lengthen the last vowel ("cake" → kˈA k). Returns the letters, whether
/// the last vowel is long, and that vowel's index.
fn strip_silent_e(letters: &[char]) -> (Vec<char>, bool, Option<usize>) {
    let mut chars = letters.to_vec();
    let ends_consonant_le = chars.len() >= 3
        && chars.last() == Some(&'e')
        && chars.get(chars.len() - 2) == Some(&'l')
        && chars.get(chars.len() - 3).is_some_and(|c| !is_vowel_letter(*c));
    if !ends_consonant_le && chars.len() >= 3 && chars.last() == Some(&'e') && chars.get(chars.len() - 2).is_some_and(|c| !is_vowel_letter(*c)) {
        chars.pop();
        let last_vowel_index = chars.iter().rposition(|c| is_vowel_letter(*c));
        return (chars, true, last_vowel_index);
    }
    (chars, false, None)
}

/// Where the letter-to-sound scan goes after a rule fired.
enum Step {
    /// Continue at this letter index.
    Next(usize),
    /// The rule consumed the rest of the word.
    Stop,
}

/// Letter-to-sound state: the letters and the sounds produced so far.
struct LetterToSound<'a> {
    chars: &'a [char],
    out: Vec<String>,
    /// Indices into `out` of the vowels pushed.
    vowel_positions: Vec<usize>,
    long_last: bool,
    last_vowel_index: Option<usize>,
}

impl LetterToSound<'_> {
    fn push_all(&mut self, symbols: &[&str]) {
        self.out.extend(symbols.iter().map(|s| (*s).to_string()));
    }

    /// Mark the most recent vowel with primary stress.
    fn stress_last_vowel(&mut self) {
        if let Some(&last) = self.vowel_positions.last()
            && let Some(v) = self.out.get_mut(last)
        {
            *v = format!("ˈ{v}");
        }
    }

    /// Apply the rule for the letters at `i`.
    fn step(&mut self, i: usize) -> Step {
        let chars = self.chars;
        let Some(rest) = chars.get(i..) else { return Step::Stop };
        // Multi-letter rules first (longest match wins).
        if let Some(step) = self.word_rule(i, rest) {
            return step;
        }
        if let Some(step) = self.letter_group(i, rest) {
            return step;
        }
        let Some(&c) = chars.get(i) else { return Step::Stop };
        if is_vowel_letter(c) {
            return Step::Next(i + self.vowel(i, c, rest));
        }
        self.consonant(i, c)
    }

    /// Suffixes and irregular groups: tion, sion, sch, -sten, -gue, -que, ough, gu, initial ps.
    fn word_rule(&mut self, i: usize, rest: &[char]) -> Option<Step> {
        if rest.len() == 4 && rest.iter().copied().eq("tion".chars()) {
            // The vowel before the suffix takes the primary stress.
            self.stress_last_vowel();
            self.push_all(&["ʃ", "ə", "n"]);
            return Some(Step::Stop);
        }
        if rest.len() == 4 && rest.iter().copied().eq("sion".chars()) {
            self.stress_last_vowel();
            self.push_all(&["ʒ", "ə", "n"]);
            return Some(Step::Stop);
        }
        if rest.starts_with(&['s', 'c', 'h']) {
            self.push_all(&["s", "k"]);
            return Some(Step::Next(i + 3));
        }
        if rest == ['s', 't', 'e', 'n'] {
            // listen
            self.push_all(&["s", "ə", "n"]);
            return Some(Step::Stop);
        }
        if rest == ['g', 'u', 'e'] {
            self.push_all(&["ɡ"]);
            return Some(Step::Stop);
        }
        if rest == ['q', 'u', 'e'] {
            self.push_all(&["k"]);
            return Some(Step::Stop);
        }
        if rest.starts_with(&['o', 'u', 'g', 'h']) {
            // A famously irregular group; pick the "thought" vowel, f when final.
            self.push_all(&["ɔ"]);
            if rest == ['o', 'u', 'g', 'h'] {
                self.push_all(&["f"]);
                return Some(Step::Stop);
            }
            if rest.starts_with(&['o', 'u', 'g', 'h', 't']) {
                self.push_all(&["t"]);
                return Some(Step::Next(i + 5));
            }
            return Some(Step::Next(i + 3));
        }
        if rest.starts_with(&['g', 'u']) && rest.len() > 2 {
            // The u is silent before a vowel (guard, guess).
            self.push_all(&["ɡ"]);
            return Some(Step::Next(i + 2));
        }
        if rest.starts_with(&['p', 's']) && i == 0 {
            self.push_all(&["s"]);
            return Some(Step::Next(i + 2));
        }
        None
    }

    /// Three- and two-letter consonant groups (igh, tch, dge, ch, sh, th, ph, ck, qu, wh, gh, ng,
    /// nk, kn, gn, wr, mb).
    fn letter_group(&mut self, i: usize, rest: &[char]) -> Option<Step> {
        let n = self.chars.len();
        let at_start = i == 0;
        let two: String = rest.iter().take(2).collect();
        let three: String = rest.iter().take(3).collect();
        match three.as_str() {
            "igh" => {
                push_vowel(&mut self.out, &mut self.vowel_positions, "I");
                return Some(Step::Next(i + 3));
            }
            "tch" => {
                self.push_all(&["ʧ"]);
                return Some(Step::Next(i + 3));
            }
            "dge" => {
                self.push_all(&["ʤ"]);
                return Some(Step::Next(i + 3));
            }
            _ => {}
        }
        let sounds: &[&str] = match two.as_str() {
            "ch" => &["ʧ"],
            "sh" => &["ʃ"],
            "th" => &["θ"],
            "ph" => &["f"],
            "ck" => &["k"],
            "qu" => &["k", "w"],
            "wh" => &["w"],
            // Word-final → f (laugh, tough); before t → silent (knight's "igh"
            // rule already handled it); otherwise g (ghost).
            "gh" if i + 2 == n => &["f"],
            "gh" if self.chars.get(i + 2).copied() != Some('t') => &["ɡ"],
            "gh" => &[],
            "ng" if i + 2 == n => &["ŋ"],
            "nk" => &["ŋ", "k"],
            "kn" | "gn" if at_start => &["n"],
            "wr" if at_start => &["ɹ"],
            "mb" if i + 2 == n => &["m"],
            _ => return None,
        };
        self.push_all(sounds);
        Some(Step::Next(i + 2))
    }

    /// Push the vowel at `i` (digraph or single letter); returns how many letters it consumed.
    fn vowel(&mut self, i: usize, c: char, rest: &[char]) -> usize {
        let chars = self.chars;
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
            push_vowel(&mut self.out, &mut self.vowel_positions, sym);
            return 2;
        }
        // A doubled consonant after the vowel keeps it short (little → ˈlɪtəl).
        let doubled = chars.get(i + 1) == chars.get(i + 2) && chars.get(i + 1).is_some_and(|c| !is_vowel_letter(*c));
        let long = self.long_last && self.last_vowel_index == Some(i) && !doubled;
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
        push_vowel(&mut self.out, &mut self.vowel_positions, sym);
        1
    }

    /// Push the consonant at `i`.
    fn consonant(&mut self, i: usize, c: char) -> Step {
        let chars = self.chars;
        let n = chars.len();
        let soft = chars.get(i + 1).is_some_and(|n| matches!(n, 'e' | 'i' | 'y'));
        match c {
            'c' => self.push_all(&[if soft { "s" } else { "k" }]),
            'g' => self.push_all(&[if soft { "ʤ" } else { "ɡ" }]),
            'x' => self.push_all(&["k", "s"]),
            'j' => self.push_all(&["ʤ"]),
            'q' => self.push_all(&["k"]),
            'l' if i + 2 == n && chars.get(i + 1) == Some(&'e') && i > 0 && chars.get(i - 1).is_some_and(|c| !is_vowel_letter(*c)) => {
                // "little", "castle": the final "le" is a syllable əl.
                self.push_all(&["ə", "l"]);
                return Step::Next(i + 2);
            }
            'y' if i == 0 => self.push_all(&["j"]),
            'y' if i + 1 == n => self.push_all(&["i"]),
            'y' => self.push_all(&["ɪ"]),
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
                    self.push_all(&[sym]);
                }
            }
        }
        Step::Next(i + 1)
    }

    /// Collapse doubled consonants and stress the first vowel if nothing is stressed yet.
    fn finish(self) -> Option<String> {
        // Doubled consonants collapse ("narration": rr → r).
        let mut collapsed: Vec<String> = Vec::new();
        for sym in self.out {
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
mod tests;
