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
    let input = b";;; a comment\n\nGOOD  AE1\nBADLINE\nONLYWORD\nWORD  XX1 YY2\nBAD(  AE1\nWORD(2)  AA1\nWORD  AE1 # trailing comment\nTRAIL  AE1 T # note\n";
    let lex = Lexicon::parse_cmudict(input).unwrap();
    assert_eq!(lex.len(), 3); // GOOD, WORD (two pronunciations), TRAIL
    assert_eq!(lex.skipped(), 4); // BADLINE, ONLYWORD, WORD XX1 YY2, BAD(
    assert_eq!(lex.pronunciations("WORD").map(|v| v.first().map(String::as_str)), Some(Some("AA1")));
    // The trailing comment is stripped, not part of the pronunciation.
    assert_eq!(lex.pronunciations("TRAIL").map(|v| v.first().map(String::as_str)), Some(Some("AE1 T")));
}

#[test]
fn skips_an_unreasonably_long_dictionary_pronunciation() {
    let mut input = String::from("GOOD  AE1\nTOO_LONG");
    for _ in 0..=MAX_PRONUNCIATION_PHONES {
        input.push_str(" AE1");
    }
    input.push('\n');
    let lex = Lexicon::parse_cmudict(input.as_bytes()).unwrap();
    assert_eq!(lex.len(), 1);
    assert_eq!(lex.skipped(), 1);
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
        assert!(p.ends_with(',') || p.ends_with(';') || p.ends_with(':') || p.ends_with('w') || p.ends_with("ʌn") || p.ends_with("tˈu"), "bad split: {p:?}");
    }
}

#[test]
fn chunk_splitting_is_iterative_for_many_chunks() {
    let input = "a".repeat(MAX_CHUNK_CHARS * 2_000);
    let chunks = split_long_chunk(input);
    assert_eq!(chunks.len(), 2_000);
    assert!(chunks.iter().all(|chunk| chunk.len() == MAX_CHUNK_CHARS));
}

#[test]
fn letter_to_sound_refuses_an_absurdly_long_unknown_word() {
    assert!(letter_to_sound(&"ture".repeat(MAX_LETTER_TO_SOUND_CHARS)).is_none());
}

#[test]
fn am_reads_the_letter_name_not_the_article() {
    for text in ["At 9:00 a.m.", "At 9:00am"] {
        let chunks = phonemize(text, Lang::EnUs, &test_lex(), &Overrides::default()).unwrap();
        let joined = format!("{chunks:?}");
        assert!(joined.contains("ˈA"), "{text}: {joined}");
        assert!(!joined.contains('ə'), "{text}: {joined}");
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
