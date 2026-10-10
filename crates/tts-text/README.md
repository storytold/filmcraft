# filmcraft-tts-text

Text-to-speech text front end for FilmCraft (L2, phases P2a + P2b). Takes a
narration script and returns plain spoken words, structured as tokens: `Word`,
`Pause` (from `[pause 1s]` markers), `SentenceEnd` and `Punct`
(`normalize`), and turns those words into Kokoro phoneme symbols (`phonemize`).

```rust
use filmcraft_tts_text::{normalize, Lang, Token};

let tokens = normalize("Call 555-0134 by 3:30 p.m. on Oct. 7th, it's $3.50 (20% off)", Lang::EnUs)?;
// → Call | five five five zero one three four | by | three thirty p m | on |
//   October seventh | , | it's | three dollars and fifty cents | ( | twenty percent off | )
```

English only (`Lang::EnUs`, `Lang::EnGb`), per the amended brief. Handles:
cardinals to 999 999 999 999, negatives, decimals, ordinals, money (`$`, `€`,
`£`), percent, years (1100–2099), clock times, numeric and abbreviated dates,
phone-like digit groups, common abbreviations (Mr., Mrs., Dr., St., Mt., vs.,
etc., e.g., i.e., a.m., p.m.), spelled acronyms, `&`/`+`/`@`/`#`/`/`, and pause
markers. Never crashes: >1 MiB input is `NormalizeError::TooLong`, oversized or
leading-zero numbers are read digit by digit, malformed pause markers become
plain text, and all slicing is boundary-safe with `get()`.

## Rules where behaviour is a judgement call

- **Cardinals have no "and"**: "1,234" → "one thousand two hundred thirty-four".
  British "one thousand two hundred **and** thirty-four" is not implemented.
- **Decimals read digit by digit** after "point" ("3.14" → "three point one four").
- **Years**: a standalone 4-digit number in 1100–2099: 1100–1999 →
  century + remainder ("nineteen ninety-nine", "nineteen oh five",
  "nineteen hundred"); 2000–2009 → "two thousand five"; 2010–2099 →
  "twenty twenty-six". 2100+ reads as an ordinary cardinal.
- **St.** is "saint" before a capitalised name, "street" otherwise; **Dr.** is
  always "doctor" (drive/dr. disambiguation is out of scope, as the brief allows).
- **Acronyms**: unpronounceable all-caps words of 2–5 letters are spelled letter
  by letter ("TDA" → T D A, "FBI" → F B I): a leading consonant cluster of 2+ or
  an interior run of 3+ consonants makes an acronym unpronounceable, as does
  having no vowels at all. Pronounceable ones (NASA, UNESCO and a small list)
  stay words. Single letters are Word tokens that G2P should read as letter names.
- **a.m./p.m.** become the letter words "a m"/"p m". Lowercase "pm" without dots
  is spelled; lowercase "am" is left alone (it's the English verb).
- **Fractions**: 1/2 → "one half"; other denominators ≤ 12 → "N ordinal(s)"
  ("three quarters"); anything else → "N over M". Dates need a 4-digit year —
  "10/7" alone reads as a fraction.
- **Digit groups**: hyphen-joined digits read digit by digit when any group has
  a leading zero or there are 7+ digits in total ("555-0134"); otherwise two
  small groups read "A to B" ("2-3" → "two to three").
- **Pause markers**: `[pause <n>s|ms]`, capped at 10 s; malformed markers
  ([pause], [pause x], unclosed) stay plain text — never an error.
- **Money**: "$3.50" → "three dollars and fifty cents" (€ → euros/cents, £ →
  pounds/pence); a lone `$`/`€`/`£` with no number stays a punctuation token.
- **Sentence ends**: runs of `.!?` are one `SentenceEnd`; abbreviation and
  number dots are consumed by their rules and never end a sentence.

## Limitations

- English only (amended brief). Spanish was cut from this crate's scope.
- No "and" in British cardinals; no British-ism beyond date order and £.
- Pronunciation is not attempted here — this crate stops at spoken words.
- Time hours 0–12 read as cardinals ("0:15" → "zero fifteen"), not "twelve".
- Ordinal suffixes are validated but "1st"/"1 th"-style spacing is not accepted.
- "may" is never treated as a month abbreviation (it's a word); other dotted
  month abbreviations (Jan.–Dec.) are.

## References

None (rules written from scratch). Clean-room: no TTS/normalizer source was read.


# P2b — English pronunciation (`phonemize`)

```rust
pub fn phonemize(text: &str, lang: Lang, lex: &Lexicon, overrides: &Overrides)
    -> Result<Vec<Chunk>, NormalizeError>;
pub enum Chunk { Phonemes(String), Pause { millis: u32 } }
pub const MAX_CHUNK_CHARS: usize = 400;
pub const SYMBOLS: &str; // every character phonemize may emit
```

`phonemize` normalizes the script, then phonemizes each sentence into one
`Chunk::Phonemes` (split at the last `,`/`;`/`:` before 400 chars, else at the
last space, else hard-split). Normalizer pauses become `Chunk::Pause`. One space
between words; punctuation from the normalizer attaches to the preceding word
with no space (`həlˈO, wˈɜɹld.`); a sentence terminator becomes a `.`.

## The ARPAbet → Kokoro table (American English)

AA→ɑ, AE→æ, AH0→ə, AH1/2→ʌ, AO→ɔ, AW→W, AY→I, EH→ɛ, ER0→ɚ, ER1/2→ɜɹ, EY→A,
IH→ɪ, IY→i, OW→O, OY→Y, UH→ʊ, UW→u; B→b, CH→ʧ, D→d, DH→ð, F→f, G→ɡ (U+0261,
not ASCII g), HH→h, JH→ʤ, K→k, L→l, M→m, N→n, NG→ŋ, P→p, R→ɹ, S→s, SH→ʃ, T→t,
TH→θ, V→v, W→w, Y→j, Z→z, ZH→ʒ. `A I O W Y` are Kokoro's single-symbol
diphthongs (eɪ aɪ oʊ aʊ ɔɪ).

## Stress

`ˈ` (primary) / `ˌ` (secondary) go immediately before the stressed vowel symbol,
not before the syllable: HELLO (HH AH0 L OW1) → `həlˈO`. Single-syllable
function words (a, an, the, of, to, and, in, is, it, for, on, with, at, by, as,
be, or, but, from, that, this) get no stress mark. Spelled-out single letters
use the letter's name from the dictionary (T → `tˈi`; CMUdict lists "A" as the
article first and the letter name as an alternate, and the letter-name alternate
is preferred).

## Lookup order

1. Overrides (case-insensitive; ≤ 1 000 entries of ≤ 200 chars honoured, excess
   ignored). The value is either raw Kokoro phonemes between slashes
   (`/tˌidˌiˈA/` — accepted only if every character is in `SYMBOLS`, otherwise
   the override is ignored) or a plain-English respelling ("tee dee ay") that
   goes through the same dictionary + rules.
2. Dictionary (first pronunciation). The dictionary is CMUdict in its text
   format, parsed by `Lexicon::parse_cmudict` (alternates `WORD(2)`, `;;;`
   comments, `#` trailing comments; malformed lines skipped and counted by
   `skipped()`; input over 16 MiB or 500 000 entries is `LexError`).
3. Morphology on a dictionary stem: plural/possessive `-s`/`-'s` with voicing
   (s / z / ɪz), `-ed` (t / d / ɪd), `-ing` (stem may regain its silent e:
   making → make), `-er`, `-ly`, and the prefixes `un-`, `re-`.
4. Hyphenated words: each part goes through the same pipeline.
5. Letter-to-sound rules.

## Letter-to-sound rules (written from scratch, clean-room)

A left-to-right scanner with longest-match rules: `tion`/`sion` (stress on the
vowel before the suffix), `ture`/`sure`, `ough` (→ ɔ; +f word-final; "ought" →
ɔt), `igh`, `tch`, `dge`, `ch`, `sh`, `th`, `ph`, `ck`, `qu`, `wh`, `gh` (g at
the start, f word-final, silent before t), `ng` final, `nk`, initial `kn`/`gn`
(silent k/g), `wr`, final `mb`, `gu` (silent u), word-initial `ps`, `sch`,
`sten` (listen), final consonant+`le` → əl, vowel digraphs (ee/ea/ie→i, oo/ue/ui→u,
ou→W, ow→O, oi/oy→Y, ai/ay/ei→A, au/aw→ɔ, oa/oe→O), silent final e with
lengthening of the last vowel (cake → `kˈAk`), doubled consonants keeping the
vowel short (little → `lˈɪtəl`), soft c/g before e/i/y, `x` → ks, final y → i.
Primary stress lands on the first syllable unless a suffix rule moves it.
Every word with letters produces pronounceable output — never empty.

## Measured accuracy

Held-out measurement against the real CMUdict (`cmusphinx/cmudict`, commit
`74790861f652b15e4ac49015a90074ad62a27690`, SHA-256
`81917843c7f44ce2b094ac63873c2c7a4cf802040792c455ba3ca406891c3d22`): every 50th
entry held out, lexicon rebuilt without them, letter-to-sound output compared
with the dictionary pronunciation. On 2026-10-08, machine: mjcamacho's Linux
workstation (rustc 1.99.0, release build): **229 held-out words, 9.1 % exact
word accuracy, 37.9 % phoneme error rate** (Levenshtein over phoneme symbols).
The test runs whenever `FILMCRAFT_CMUDICT` points at a CMUdict file and prints
the numbers; it asserts only that it runs. CMUdict is downloaded with the voices
at run time, never committed (BSD-style licence, attributed in the download
dialog).

## Limitations

- Homographs get their first CMUdict pronunciation only: "read" is always
  /ɹiːd/, never /ɹɛd/; live/live, lead/lead, bow/bow are wrong half the time.
  Overrides are the mitigation.
- Names, jargon and irregular spellings are the letter-to-sound rules' weak
  spot (colonel, yacht, place names). Measured accuracy above is the honest
  number.
- No flap-T or connected-speech effects ("water" is /ˈwɔtɚ/-style from the
  dictionary, rules give ˈwɔtɚ only if the dictionary has it).
- `ow` is read /oʊ/ everywhere (so "wow" → wˈO), `ough` has one reading plus
  word-final f; "ia"/"ea" ambiguities resolved to one value each.
- The sentence terminator always becomes `.` (the normalizer collapses `!`/`?`
  into `SentenceEnd`).
- English only.

## References

None (rules written from scratch). Clean-room: no G2P/TTS project source was
read. Allowed sources used: CMUdict's own data and licence, published IPA
charts, and Wikipedia's ARPAbet and English phonology pages.
