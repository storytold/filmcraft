//! Text normalization: script → spoken words (see the crate docs).

/// Language a script is normalized for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    /// American English: month/day dates, "dollars and cents".
    EnUs,
    /// British English: day/month dates, "pounds and pence".
    EnGb,
}

/// A piece of a normalized script.
#[derive(Clone, Debug, PartialEq)]
pub enum Token {
    /// A word to hand to G2P. Single uppercase letters are meant to be read as letter
    /// names (spelled acronyms, a.m./p.m.); digit groups come through as digit words.
    Word(String),
    /// A narration pause, from `[pause 1s]` / `[pause 500ms]` / `[pause 1.5s]`.
    Pause {
        /// Pause length in milliseconds, capped at 10 s.
        millis: u32,
    },
    /// A sentence boundary ending in a period (never after an abbreviation or
    /// inside a number).
    SentenceEnd,
    /// A sentence boundary with an explicit terminator (`?` or `!`), which Kokoro
    /// reads for intonation. Added in M7.11; `SentenceEnd` keeps meaning `.`.
    SentenceEndWith(char),
    /// Punctuation kept for prosody downstream.
    Punct(char),
}

/// Normalization error.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NormalizeError {
    /// The script exceeds the 1 MiB input limit.
    #[error("the script exceeds the 1 MiB normalization limit")]
    TooLong,
}

/// Maximum accepted input size (1 MiB).
const MAX_INPUT: usize = 1_048_576;
/// Maximum pause length (10 s).
const MAX_PAUSE_MS: u32 = 10_000;
/// Numbers with more than this many digits are read digit by digit (never overflow).
const MAX_CARDINAL_DIGITS: usize = 12;
/// A malformed pause marker is only inspected within this bounded prefix.
const MAX_PAUSE_MARKER_BYTES: usize = 64;

/// Normalize `text` into spoken-word tokens for `lang`.
///
/// Never panics on any input: hostile input yields an `Err` or degraded (but valid)
/// tokens, never a crash. Malformed pause markers become plain text rather than an
/// error. Deterministic: the same input and language always give the same tokens.
pub fn normalize(text: &str, lang: Lang) -> Result<Vec<Token>, NormalizeError> {
    if text.len() > MAX_INPUT {
        return Err(NormalizeError::TooLong);
    }
    let mut out: Vec<Token> = Vec::new();
    let mut pos = 0usize;
    while pos < text.len() {
        if !text.is_char_boundary(pos) {
            pos += 1;
            continue;
        }
        let rest = text.get(pos..).unwrap_or("");
        let Some(c) = rest.chars().next() else { break };
        match c {
            _ if c.is_whitespace() => pos += c.len_utf8(),
            '[' => match parse_pause(rest) {
                Some((millis, used)) => {
                    pos += used;
                    out.push(Token::Pause { millis });
                }
                None => {
                    pos += c.len_utf8();
                    out.push(Token::Punct(c));
                }
            },
            '$' | '€' | '£' => match parse_money(rest, lang) {
                Some((words, used)) => {
                    pos += used;
                    push_words(&mut out, &words);
                }
                None => {
                    pos += c.len_utf8();
                    out.push(Token::Punct(c));
                }
            },
            '-' if peek(rest, 1).is_some_and(|c| c.is_ascii_digit()) => {
                pos += 1;
                if let Some((tokens, used)) = parse_number_cluster(rest.get(1..).unwrap_or(""), lang) {
                    pos += used;
                    out.push(Token::Word("minus".into()));
                    out.extend(tokens);
                } else {
                    out.push(Token::Punct(c));
                }
            }
            '-' if matches!(peek(rest, 1), Some('$' | '€' | '£'))
                && !text
                    .get(..pos)
                    .and_then(|before| before.chars().next_back())
                    .is_some_and(|b| b.is_alphanumeric() || matches!(b, '$' | '€' | '£' | '-')) =>
            {
                // A negative amount ("-$5"): keep the sign so a loss is not read as a gain.
                // A hyphen right after a word or amount ("$10-$20") is a range, not a sign.
                pos += 1;
                match parse_money(rest.get(1..).unwrap_or(""), lang) {
                    Some((words, used)) => {
                        pos += used;
                        out.push(Token::Word("minus".into()));
                        push_words(&mut out, &words);
                    }
                    None => out.push(Token::Punct(c)),
                }
            }
            _ if c.is_ascii_digit() => match parse_number_cluster(rest, lang) {
                Some((tokens, used)) => {
                    pos += used;
                    out.extend(tokens);
                }
                None => {
                    pos += c.len_utf8();
                    out.push(Token::Punct(c));
                }
            },
            '.' if peek(rest, 1).is_some_and(|c| c.is_ascii_digit()) => {
                // A decimal with no integer part: ".5" → "point five".
                pos += 1;
                out.push(Token::Word("point".into()));
                let (frac, used) = digit_run_as_words(rest.get(1..).unwrap_or(""));
                pos += used;
                push_words(&mut out, &frac);
            }
            '.' | '!' | '?' => {
                // A run of terminators is one sentence end; the strongest one wins
                // (`?` over `!` over `.`) so "Really?!" keeps its intonation mark.
                // Abbreviations and numbers consume their own '.' earlier, so a
                // leftover one ends a sentence.
                let mut question = false;
                let mut bang = false;
                while matches!(peek(text.get(pos..).unwrap_or(""), 0), Some('.' | '!' | '?')) {
                    match peek(text.get(pos..).unwrap_or(""), 0) {
                        Some('?') => question = true,
                        Some('!') => bang = true,
                        _ => {}
                    }
                    pos += 1;
                }
                let terminator = if question {
                    Token::SentenceEndWith('?')
                } else if bang {
                    Token::SentenceEndWith('!')
                } else {
                    Token::SentenceEnd
                };
                if !matches!(out.last(), Some(Token::SentenceEnd) | Some(Token::SentenceEndWith(_))) {
                    out.push(terminator);
                }
            }
            // Apostrophes can occur inside a word, but an apostrophe-only run must not repeatedly
            // scan the whole remaining script looking for a word.
            _ if c.is_alphabetic() => match parse_wordish(rest, lang) {
                Some((tokens, used)) => {
                    pos += used;
                    out.extend(tokens);
                }
                None => {
                    pos += c.len_utf8();
                    out.push(Token::Punct(c));
                }
            },
            '&' | '+' | '@' | '#' | '/' => {
                // Symbols read as words. Slashes in dates and fractions are consumed
                // by their parsers, so a leftover one is a real "slash".
                pos += c.len_utf8();
                out.push(Token::Word(symbol_word(c).into()));
            }
            _ => {
                pos += c.len_utf8();
                out.push(Token::Punct(c));
            }
        }
    }
    Ok(out)
}

fn push_words(out: &mut Vec<Token>, words: &[String]) {
    for w in words {
        out.push(Token::Word(w.clone()));
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c == '\'' || c == '’'
}

/// The char at byte offset `i` in `s`, if `i` is a char boundary inside `s`.
fn peek(s: &str, i: usize) -> Option<char> {
    s.get(i..).and_then(|r| r.chars().next())
}

// ---------------------------------------------------------------- pause markers

/// `[pause 1s]`, `[pause 500ms]`, `[pause 1.5s]` → a capped pause. Malformed markers
/// return `None` and fall through to plain text (never an error).
fn parse_pause(rest: &str) -> Option<(u32, usize)> {
    // search bytes: the 64-byte cut may fall inside a multi-byte character that follows the marker
    let close = rest.as_bytes().iter().take(MAX_PAUSE_MARKER_BYTES).position(|&b| b == b']')?;
    let inner = rest.get(1..close)?.trim();
    let value = inner.strip_prefix("pause")?;
    let value = value.trim_start();
    if value.is_empty() {
        return None;
    }
    let mut saw_digit = false;
    let mut saw_dot = false;
    let mut num_end = 0usize;
    for (i, ch) in value.char_indices() {
        if ch.is_ascii_digit() {
            saw_digit = true;
            num_end = i + ch.len_utf8();
        } else if ch == '.' && saw_digit && !saw_dot {
            saw_dot = true;
            num_end = i + ch.len_utf8();
        } else {
            num_end = i;
            break;
        }
    }
    if !saw_digit {
        return None;
    }
    let number = value.get(..num_end)?;
    let unit = value.get(num_end..)?.trim();
    let millis = pause_millis(number, unit)?;
    Some((millis, close + 1))
}

/// `<number>` in seconds or milliseconds, capped at [`MAX_PAUSE_MS`]. Overflow and
/// absurd values cap instead of erroring.
fn pause_millis(number: &str, unit: &str) -> Option<u32> {
    let unit_ms: u64 = match unit {
        "s" => 1000,
        "ms" => 1,
        _ => return None,
    };
    let mut whole = 0u64;
    let mut frac_digits: Vec<u32> = Vec::new();
    let mut after_dot = false;
    let mut saw_digit = false;
    for ch in number.chars() {
        match ch {
            '.' if !after_dot => after_dot = true,
            '0'..='9' => {
                saw_digit = true;
                if after_dot {
                    if frac_digits.len() < 3 {
                        frac_digits.push(ch.to_digit(10)?);
                    }
                } else {
                    whole = whole.checked_mul(10)?.checked_add(u64::from(ch.to_digit(10)?))?;
                    if whole > u64::from(MAX_PAUSE_MS) {
                        return Some(MAX_PAUSE_MS);
                    }
                }
            }
            _ => return None,
        }
    }
    if !saw_digit {
        return None;
    }
    let mut millis = whole.checked_mul(unit_ms)?;
    for (i, d) in frac_digits.iter().enumerate() {
        let scale = unit_ms.checked_div(10u64.checked_pow(i as u32 + 1)?)?;
        if scale == 0 {
            break;
        }
        millis = millis.checked_add(u64::from(*d) * scale)?;
    }
    u32::try_from(millis).ok().map(|ms| ms.min(MAX_PAUSE_MS))
}

// ----------------------------------------------------------------------- money

/// Parses a currency amount (the symbol is `rest`'s first char) into spoken words.
fn parse_money(rest: &str, lang: Lang) -> Option<(Vec<String>, usize)> {
    let symbol = peek(rest, 0)?;
    let skip = symbol.len_utf8();
    let (value, digits, used, minor, had_minor) = parse_amount(rest.get(skip..)?, lang)?;
    if digits > MAX_CARDINAL_DIGITS || has_leading_zero(&value, digits) {
        // Oversized amounts: every digit, unit-free, no overflow.
        let all = rest.get(skip..skip + used)?.chars().filter(char::is_ascii_digit).collect::<String>();
        let (words, _) = digit_run_as_words(&all);
        return Some((words, used + skip));
    }
    let (unit, sub) = money_units(symbol, lang);
    let mut words = english_cardinal(value);
    push_unit(&mut words, unit, value == 1);
    if had_minor && minor > 0 {
        words.push("and".into());
        let mut cents = english_cardinal(u64::from(minor));
        push_unit(&mut cents, sub, minor == 1);
        words.extend(cents);
    }
    Some((words, used + skip))
}

fn money_units(symbol: char, lang: Lang) -> (&'static str, &'static str) {
    match (lang, symbol) {
        (_, '$') => ("dollars", "cents"),
        (_, '£') => ("pounds", "pence"),
        (_, _) => ("euros", "cents"),
    }
}

fn push_unit(words: &mut Vec<String>, unit: &str, singular: bool) {
    if singular {
        words.push(unit_singular(unit).into());
    } else {
        words.push(unit.into());
    }
}

fn unit_singular(unit: &str) -> &str {
    match unit {
        "dollars" => "dollar",
        "euros" => "euro",
        "pounds" => "pound",
        "cents" => "cent",
        "pence" => "penny",
        _ => unit,
    }
}

/// Parses `<int>[.<frac>]` honouring thousands grouping with commas. Returns the major
/// value, its digit count, bytes consumed, the minor value and whether one followed.
fn parse_amount(rest: &str, _lang: Lang) -> Option<(u64, usize, usize, u32, bool)> {
    let mut int_digits = String::new();
    let mut frac_digits = String::new();
    let mut used = 0usize;
    let mut after_decimal = false;
    for (i, ch) in rest.char_indices() {
        if ch.is_ascii_digit() {
            if after_decimal {
                if frac_digits.len() < 9 {
                    frac_digits.push(ch);
                }
            } else if int_digits.len() > MAX_CARDINAL_DIGITS {
                break;
            } else {
                int_digits.push(ch);
            }
            used = i + 1;
        } else if ch == ',' && !int_digits.is_empty() && !after_decimal {
            if !grouping_followed_by_3(rest, i) {
                break;
            }
        } else if ch == '.' && !after_decimal && !int_digits.is_empty() {
            after_decimal = true;
            used = i;
        } else {
            break;
        }
    }
    if int_digits.is_empty() {
        return None;
    }
    let major = int_digits.parse::<u64>().ok()?;
    let had_minor = after_decimal && !frac_digits.is_empty();
    // One fractional digit denotes tenths of a major unit (e.g. $1.5 = $1.50),
    // not five minor units. Digits beyond the second are sub-cent precision and are
    // dropped, so the minor part never exceeds 99 (`$3.459` reads as 45 cents).
    let minor = if had_minor {
        let value = frac_digits.get(..2).unwrap_or(&frac_digits).parse::<u32>().ok()?;
        if frac_digits.len() == 1 { value * 10 } else { value }
    } else {
        0
    };
    Some((major, int_digits.len(), used, minor, had_minor))
}

/// A grouping mark is valid only when exactly three digits follow it.
fn grouping_followed_by_3(rest: &str, sep_index: usize) -> bool {
    let after = rest.get(sep_index + 1..).unwrap_or("");
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    let boundary = after.get(digits.len()..).and_then(|r| r.chars().next()).is_none_or(|c| !c.is_ascii_digit());
    digits.len() == 3 && boundary
}

fn has_leading_zero(value: &u64, digits: usize) -> bool {
    digits > 1 && *value < 10u64.pow(digits as u32 - 1)
}

// ---------------------------------------------------------------------- numbers

/// Everything that starts with a digit: time, date, fraction, cardinal/year,
/// percent, ordinal, decimal, digit group.
fn parse_number_cluster(rest: &str, lang: Lang) -> Option<(Vec<Token>, usize)> {
    if let Some(r) = parse_time(rest) {
        return Some(r);
    }
    if let Some(r) = parse_date(rest, lang) {
        return Some(r);
    }
    if let Some(r) = parse_fraction(rest) {
        return Some(r);
    }
    parse_number_core(rest, lang)
}

/// `3:30`, `15:05`, `03:30`, with an optional attached `am`/`pm`.
fn parse_time(rest: &str) -> Option<(Vec<Token>, usize)> {
    let hour_end = digit_run(rest, 0, 2)?;
    if peek(rest, hour_end) != Some(':') {
        return None;
    }
    let minute_start = hour_end + 1;
    let minute_end = digit_run(rest, minute_start, 2)?;
    if minute_end - minute_start != 2 {
        return None;
    }
    if peek(rest, minute_end).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let hour = rest.get(..hour_end)?.parse::<u32>().ok()?;
    let minute = rest.get(minute_start..minute_end)?.parse::<u32>().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    let mut used = minute_end;
    let mut tokens = time_words(hour, minute);
    let after = rest.get(used..)?;
    for (lit, letter_pair) in [("am", ['a', 'm']), ("pm", ['p', 'm'])] {
        if after.get(..2).is_some_and(|s| s.eq_ignore_ascii_case(lit)) {
            for letter in letter_pair {
                // Uppercase "A" so it is read as a letter name, not the article.
                let letter = if letter == 'a' { 'A' } else { letter };
                tokens.push(Token::Word(letter.into()));
            }
            used += 2;
            break;
        }
    }
    Some((tokens, used))
}

/// End byte offset of a digit run starting at `start` (up to `max` digits). `None`
/// when there is no digit at `start`.
fn digit_run(s: &str, start: usize, max: usize) -> Option<usize> {
    let sub = s.get(start..)?;
    let end = sub.char_indices().take(max).take_while(|&(_, c)| c.is_ascii_digit()).map(|(i, c)| i + c.len_utf8()).last()?;
    Some(start + end)
}

fn time_words(h: u32, m: u32) -> Vec<Token> {
    let mut out = vec![Token::Word(english_cardinal(u64::from(h)).join(" "))];
    if m == 0 {
        // On the hour: the hour alone ("nine p m"), no minutes spoken.
    } else if m < 10 {
        out.push(Token::Word("oh".into()));
        out.push(Token::Word(english_digit_word(digit_char(m)).into()));
    } else {
        out.push(Token::Word(english_cardinal(u64::from(m)).join(" ")));
    }
    out
}

fn digit_char(d: u32) -> char {
    char::from_digit(d.min(9), 10).unwrap_or('0')
}

/// `10/7/2026` (EnUs month/day) and `7/10/2026` (EnGb day/month) with a 4-digit year.
fn parse_date(rest: &str, lang: Lang) -> Option<(Vec<Token>, usize)> {
    let (a, a_end) = read_u64(rest, 0, 2)?;
    if peek(rest, a_end) != Some('/') {
        return None;
    }
    let (b, b_end) = read_u64(rest, a_end + 1, 2)?;
    if peek(rest, b_end) != Some('/') || !peek(rest, b_end + 1).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let (year, year_end) = read_u64(rest, b_end + 1, 4)?;
    if year_end - (b_end + 1) != 4 || peek(rest, year_end).is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let (month, day) = match lang {
        Lang::EnUs => (a, b),
        Lang::EnGb => (b, a),
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut tokens = date_words(month, day, lang);
    tokens.extend(year_words(year, lang));
    Some((tokens, year_end))
}

fn read_u64(s: &str, start: usize, max: usize) -> Option<(u64, usize)> {
    let end = digit_run(s, start, max)?;
    if end == start {
        return None;
    }
    let digits = s.get(start..end)?;
    if digits.len() > 19 {
        return None;
    }
    digits.parse::<u64>().ok().map(|v| (v, end))
}

fn date_words(month: u64, day: u64, lang: Lang) -> Vec<Token> {
    let mut out = Vec::new();
    let month_name = english_month(month).unwrap_or("month");
    match lang {
        Lang::EnUs => {
            out.push(Token::Word(month_name.into()));
            out.push(Token::Word(english_ordinal(day)));
        }
        Lang::EnGb => {
            out.push(Token::Word("the".into()));
            out.push(Token::Word(english_ordinal(day)));
            out.push(Token::Word("of".into()));
            out.push(Token::Word(month_name.into()));
        }
    }
    out
}

fn english_month(month: u64) -> Option<&'static str> {
    Some(match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => return None,
    })
}

const MAX_FRACTION_DIGITS: usize = 3;

/// `1/2` → "one half"; other small denominators read "N ordinal(s)", anything
/// bigger reads "N over M".
fn parse_fraction(rest: &str) -> Option<(Vec<Token>, usize)> {
    // Operands of up to three digits: a longer run (a year, as in the `7/2026` left after an
    // impossible date's `13/`) is not a fraction.
    let (n, n_end) = read_u64(rest, 0, MAX_FRACTION_DIGITS)?;
    if peek(rest, n_end) != Some('/') || n == 0 {
        return None;
    }
    let (d, d_end) = read_u64(rest, n_end + 1, MAX_FRACTION_DIGITS)?;
    if d == 0 || peek(rest, d_end).is_some_and(|c| c.is_ascii_alphanumeric() || c == '/') {
        return None;
    }
    let mut words = english_cardinal(n);
    if d > 12 {
        // "7/32" → "seven over thirty-two".
        words.push("over".into());
        words.extend(english_cardinal(d));
        return Some((words.into_iter().map(Token::Word).collect(), d_end));
    }
    match d {
        2 => words.push(if n > 1 { "halves".into() } else { "half".into() }),
        4 => words.push(if n > 1 { "quarters".into() } else { "quarter".into() }),
        _ => {
            let mut denom = english_ordinal(d);
            if n > 1 {
                denom.push('s');
            }
            words.push(denom);
        }
    }
    Some((words.into_iter().map(Token::Word).collect(), d_end))
}

/// Integer (grouped), then the continuations: `%`, ordinal, decimal, digit group —
/// or a plain cardinal / year.
fn parse_number_core(rest: &str, lang: Lang) -> Option<(Vec<Token>, usize)> {
    let mut digits = String::new();
    let mut used = 0usize;
    let mut overflowed = false;
    let mut grouped = false;
    for (i, ch) in rest.char_indices() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            used = i + 1;
            if digits.len() > 19 {
                overflowed = true;
                break;
            }
        } else if ch == ',' && !digits.is_empty() {
            if grouping_followed_by_3(rest, i) {
                // Grouping is visual only; drop the mark.
                used = i + 1;
                grouped = true;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    if digits.is_empty() {
        return None;
    }
    if overflowed || digits.len() > MAX_CARDINAL_DIGITS {
        let words: Vec<Token> = digits.chars().map(|c| Token::Word(english_digit_word(c).into())).collect();
        return Some((words, used));
    }
    let value: u64 = digits.parse().ok()?;
    if let Some((tokens, u)) = parse_continuation(rest, used, value, &digits) {
        return Some((tokens, u));
    }
    if has_leading_zero_str(&digits) {
        let words: Vec<Token> = digits.chars().map(|c| Token::Word(english_digit_word(c).into())).collect();
        return Some((words, used));
    }
    // A year is a bare, ungrouped 4-digit number standing alone.
    if !grouped && digits.len() == 4 && (1100..=2099).contains(&value) && year_is_standalone(rest, used) {
        return Some((year_words(value, lang), used));
    }
    Some((english_cardinal(value).into_iter().map(Token::Word).collect(), used))
}

fn has_leading_zero_str(digits: &str) -> bool {
    digits.len() > 1 && digits.starts_with('0')
}

/// `%`, an ordinal suffix (`st`/`nd`/`rd`/`th`), a decimal fraction or `-`-joined
/// digit groups.
/// Percentage sign attached to a number, or separated from it by whitespace.
fn percent_suffix_len(rest: &str) -> Option<usize> {
    let suffix = rest.trim_start_matches(char::is_whitespace);
    suffix.strip_prefix('%').map(|_| rest.len() - suffix.len() + 1)
}

/// Retain digit-by-digit reading of zero-prefixed integer portions, even when
/// followed by a percent sign or decimal fraction.
fn spoken_integer(value: u64, digits: &str) -> Vec<String> {
    if has_leading_zero_str(digits) { digits.chars().map(|c| english_digit_word(c).into()).collect() } else { english_cardinal(value) }
}

fn parse_continuation(rest: &str, used: usize, value: u64, digits: &str) -> Option<(Vec<Token>, usize)> {
    let after = rest.get(used..)?;
    if let Some(suffix_len) = percent_suffix_len(after) {
        let mut words = spoken_integer(value, digits);
        words.push("percent".into());
        return Some((words.into_iter().map(Token::Word).collect(), used + suffix_len));
    }
    match peek(after, 0) {
        Some('s' | 'S' | 't' | 'T' | 'n' | 'N' | 'd' | 'D' | 'r' | 'R') => {
            let suffix = after.get(..2)?;
            if is_ordinal_suffix(suffix) && !peek(after, 2).is_some_and(char::is_alphabetic) {
                return Some((vec![Token::Word(english_ordinal(value))], used + 2));
            }
            None
        }
        Some('.') if peek(after, 1).is_some_and(|c| c.is_ascii_digit()) => {
            let mut words = spoken_integer(value, digits);
            words.push("point".into());
            let (frac, frac_used) = digit_run_as_words(after.get(1..)?);
            if frac.is_empty() {
                return None;
            }
            words.extend(frac);
            let number_end = used + 1 + frac_used;
            if let Some(suffix_len) = rest.get(number_end..).and_then(percent_suffix_len) {
                words.push("percent".into());
                return Some((words.into_iter().map(Token::Word).collect(), number_end + suffix_len));
            }
            Some((words.into_iter().map(Token::Word).collect(), number_end))
        }
        Some('-') => parse_digit_groups(rest, used),
        _ => None,
    }
}

/// `555-0134` → digit words (any group with a leading zero, or 7+ digits in total);
/// otherwise `A-B` with two small groups reads "A to B".
fn parse_digit_groups(rest: &str, used: usize) -> Option<(Vec<Token>, usize)> {
    let after = rest.get(used..)?.strip_prefix('-')?;
    let mut groups: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut end = used + 1;
    for (i, ch) in after.char_indices() {
        if ch.is_ascii_digit() {
            current.push(ch);
            end = used + 1 + i + 1;
        } else if ch == '-' && !current.is_empty() {
            groups.push(std::mem::take(&mut current));
            end = used + 1 + i + 1;
        } else {
            break;
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    if groups.iter().any(String::is_empty) {
        return None;
    }
    if groups.len() == 1 {
        let group = groups.first()?;
        let leading_zero = has_leading_zero_str(rest.get(..used)?) || group.starts_with('0');
        // Total digits across the whole cluster: the integer before the dash plus this group.
        let total = rest.get(..used).map_or(0, |head| head.chars().filter(char::is_ascii_digit).count()) + group.len();
        if !leading_zero && total < 7 {
            // "2-3" → "two to three".
            let b = group.parse::<u64>().ok()?;
            let a = rest.get(..used)?.parse::<u64>().ok()?;
            let mut words = english_cardinal(a);
            words.push("to".into());
            words.extend(english_cardinal(b));
            return Some((words.into_iter().map(Token::Word).collect(), end));
        }
        let mut words: Vec<Token> = Vec::new();
        // The integer before the dash read digit by digit too.
        if let Some(head) = rest.get(..used) {
            for ch in head.chars() {
                if ch.is_ascii_digit() {
                    words.push(Token::Word(english_digit_word(ch).into()));
                }
            }
        }
        for ch in group.chars() {
            words.push(Token::Word(english_digit_word(ch).into()));
        }
        return Some((words, end));
    }
    let total: usize = groups.iter().map(String::len).sum::<usize>() + rest.get(..used).map_or(0, |head| head.chars().filter(char::is_ascii_digit).count());
    let leading_zero = has_leading_zero_str(rest.get(..used)?) || groups.iter().any(|g| g.starts_with('0'));
    if total >= 7 || leading_zero {
        let mut words: Vec<Token> = Vec::new();
        if let Some(head) = rest.get(..used) {
            for ch in head.chars() {
                if ch.is_ascii_digit() {
                    words.push(Token::Word(english_digit_word(ch).into()));
                }
            }
        }
        for g in &groups {
            for ch in g.chars() {
                words.push(Token::Word(english_digit_word(ch).into()));
            }
        }
        return Some((words, end));
    }
    // For shorter non-phone-like groups, retain every group in order. Previously
    // three original groups ("5-10-15") dropped the middle one by reading only
    // the first and last endpoints.
    if groups.iter().all(|g| g.len() <= 3) {
        let a = rest.get(..used)?.parse::<u64>().ok()?;
        let mut words = english_cardinal(a);
        for group in &groups {
            let n = group.parse::<u64>().ok()?;
            words.push("to".into());
            words.extend(english_cardinal(n));
        }
        return Some((words.into_iter().map(Token::Word).collect(), end));
    }
    None
}

fn is_ordinal_suffix(suffix: &str) -> bool {
    matches!(suffix.to_ascii_lowercase().as_str(), "st" | "nd" | "rd" | "th")
}

/// A 4-digit number reads as a year when nothing alphanumeric follows. Any
/// separator that continues a number (decimal point with digits, grouping comma
/// with three digits, time colon, date slash) has already been consumed by its
/// own parser before this check, so a leftover `.`/`,`/`:`/`/` — or a sentence
/// terminator or closing bracket — is fine.
fn year_is_standalone(rest: &str, used: usize) -> bool {
    !peek(rest, used).is_some_and(char::is_alphanumeric)
}

/// A digit run read digit by digit.
fn digit_run_as_words(s: &str) -> (Vec<String>, usize) {
    let mut words = Vec::new();
    let mut used = 0usize;
    for (i, ch) in s.char_indices() {
        if !ch.is_ascii_digit() {
            break;
        }
        words.push(english_digit_word(ch).into());
        used = i + 1;
    }
    (words, used)
}

fn english_digit_word(c: char) -> &'static str {
    match c {
        '0' => "zero",
        '1' => "one",
        '2' => "two",
        '3' => "three",
        '4' => "four",
        '5' => "five",
        '6' => "six",
        '7' => "seven",
        '8' => "eight",
        '9' => "nine",
        _ => "",
    }
}

// ------------------------------------------------------------- cardinal numbers

/// English cardinal words for values up to 999 999 999 999 (callers cap the digit
/// count before calling). No "and" (British style is a documented limitation).
fn english_cardinal(value: u64) -> Vec<String> {
    if value == 0 {
        return vec!["zero".into()];
    }
    let scales = ["", "thousand", "million", "billion", "trillion"];
    let mut groups: Vec<u64> = Vec::new();
    let mut n = value;
    while n > 0 {
        groups.push(n % 1000);
        n /= 1000;
    }
    let mut words: Vec<String> = Vec::new();
    for (i, g) in groups.iter().enumerate().rev() {
        if *g == 0 {
            continue;
        }
        words.extend(english_below_thousand(*g));
        if i > 0
            && let Some(scale) = scales.get(i)
        {
            words.push((*scale).into());
        }
    }
    words
}

fn english_below_thousand(n: u64) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let hundreds = n / 100;
    let rest = n % 100;
    if hundreds > 0 {
        words.push(english_digit_word(digit_char(hundreds as u32 % 10)).into());
        words.push("hundred".into());
    }
    if rest > 0 {
        words.extend(english_below_hundred(rest));
    }
    words
}

fn english_below_hundred(n: u64) -> Vec<String> {
    const UNITS: [&str; 20] = [
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
    if n < 20 {
        return vec![UNITS.get(n as usize).copied().unwrap_or("").into()];
    }
    let tens = TENS.get((n / 10) as usize).copied().unwrap_or("");
    let units = n % 10;
    if units == 0 { vec![tens.into()] } else { vec![format!("{tens}-{}", UNITS.get(units as usize).copied().unwrap_or(""))] }
}

/// English ordinal: "7th" → "seventh", "21st" → "twenty-first", "100th" → "one hundredth".
fn english_ordinal(value: u64) -> String {
    let mut words = english_cardinal(value);
    let Some(last) = words.pop() else { return "zeroth".into() };
    let stem = words.join(" ");
    if matches!(last.as_str(), "hundred" | "thousand" | "million" | "billion" | "trillion") {
        // The scale word takes the ordinal form: "one hundred" → "one hundredth".
        return if stem.is_empty() { format!("{last}th") } else { format!("{stem} {last}th") };
    }
    let (prefix, tail) = match last.rsplit_once('-') {
        Some((p, t)) => (format!("{p}-"), t),
        None => (String::new(), last.as_str()),
    };
    let tail = match tail {
        "one" => "first",
        "two" => "second",
        "three" => "third",
        "five" => "fifth",
        "eight" => "eighth",
        "nine" => "ninth",
        "twelve" => "twelfth",
        t if t.ends_with('y') => {
            let root = t.get(..t.len() - 1).unwrap_or(t);
            return attach(stem, &format!("{prefix}{root}ieth"));
        }
        t => return attach(stem, &format!("{prefix}{t}th")),
    };
    attach(stem, &format!("{prefix}{tail}"))
}

fn attach(stem: String, s: &str) -> String {
    if stem.is_empty() { s.to_string() } else { format!("{stem} {s}") }
}

/// English year readings: 1999 → "nineteen ninety-nine", 1905 → "nineteen oh five",
/// 1900 → "nineteen hundred", 2005 → "two thousand five", 2026 → "twenty twenty-six".
fn year_words(year: u64, _lang: Lang) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    if (2000..=2009).contains(&year) {
        out.push(Token::Word("two".into()));
        out.push(Token::Word("thousand".into()));
        if !year.is_multiple_of(10) {
            out.push(Token::Word(english_digit_word(digit_char((year % 10) as u32)).into()));
        }
        return out;
    }
    let century = year / 100;
    let rest = year % 100;
    for w in english_cardinal(century) {
        out.push(Token::Word(w));
    }
    if rest == 0 {
        out.push(Token::Word("hundred".into()));
    } else if rest < 10 {
        out.push(Token::Word("oh".into()));
        out.push(Token::Word(english_digit_word(digit_char(rest as u32)).into()));
    } else {
        for w in english_cardinal(rest) {
            out.push(Token::Word(w));
        }
    }
    out
}

// ----------------------------------------------------------------- words, etc.

/// Words, abbreviations, months, acronyms, a.m./p.m. and e.g./i.e.
fn parse_wordish(rest: &str, _lang: Lang) -> Option<(Vec<Token>, usize)> {
    let end = rest.char_indices().find_map(|(i, c)| (!is_word_char(c)).then_some(i)).unwrap_or(rest.len());
    let collected = rest.get(..end)?;
    let word = collected.trim_matches(|c| c == '\'' || c == '’');
    if word.is_empty() {
        return None;
    }
    let used = word.len();
    let lower = word.to_ascii_lowercase();

    // a.m. / p.m. — a single letter, a dot, "m", and a dot or boundary.
    if word.len() == 1 && matches!(lower.as_str(), "a" | "p") && peek(rest, used) == Some('.') && matches!(peek(rest, used + 1), Some('m') | Some('M')) {
        let after_m = used + 2;
        if peek(rest, after_m).is_none_or(|c| !c.is_alphanumeric()) {
            let mut tokens = vec![Token::Word(if lower == "a" { "A".into() } else { lower.clone() })];
            tokens.push(Token::Word("m".into()));
            let consumed = if peek(rest, after_m) == Some('.') { after_m + 1 } else { after_m };
            return Some((tokens, consumed));
        }
    }
    // e.g. / i.e. — a single letter, a dot, a letter, a dot.
    if word.len() == 1
        && matches!(lower.as_str(), "e" | "i")
        && peek(rest, used) == Some('.')
        && let Some(second) = peek(rest, used + 1)
    {
        let pair = format!("{lower}{second}");
        if matches!(pair.as_str(), "eg" | "ie") && peek(rest, used + 2) == Some('.') {
            let expansion = if pair == "eg" { "for example" } else { "that is" };
            return Some((vec![Token::Word(expansion.into())], used + 3));
        }
    }
    // Dotted abbreviations: Mr. Dr. St. Mt. vs. etc., and months (Oct. 7th).
    if let Some((tokens, consumed)) = dotted_abbreviation(&lower, rest, used) {
        return Some((tokens, consumed));
    }
    // Standalone "pm" (no dots) is spelled; lowercase "am" is a real word, so it stays.
    if lower == "pm" {
        return Some((vec![Token::Word("p".into()), Token::Word("m".into())], used));
    }
    // All-caps 2–5 letters with no vowels are spelled letter by letter (TDA, NBC);
    // pronounceable ones (with vowels, e.g. NASA) stay words.
    if is_spellable_acronym(word) {
        let tokens = word.chars().map(|c| Token::Word(c.to_string())).collect();
        return Some((tokens, used));
    }
    Some((vec![Token::Word(word.into())], used))
}

/// Expansions for `Mr.` etc.; the dot is consumed so it never ends a sentence.
/// `St.` picks "saint" before a capitalised name and "street" otherwise.
fn dotted_abbreviation(lower: &str, rest: &str, used: usize) -> Option<(Vec<Token>, usize)> {
    if peek(rest, used) != Some('.') {
        return None;
    }
    let expansion: Vec<Token> = match lower {
        "mr" => vec![Token::Word("mister".into())],
        "mrs" => vec![Token::Word("missus".into())],
        "dr" => vec![Token::Word("doctor".into())],
        "mt" => vec![Token::Word("mount".into())],
        "st" => {
            let mut i = used + 1;
            while peek(rest, i).is_some_and(char::is_whitespace) {
                i += 1;
            }
            let capitalised = peek(rest, i).is_some_and(char::is_uppercase);
            vec![Token::Word(if capitalised { "saint".into() } else { "street".into() })]
        }
        "vs" => vec![Token::Word("versus".into())],
        "etc" => vec![Token::Word("et cetera".into())],
        _ => {
            let month = match lower {
                "jan" => "January",
                "feb" => "February",
                "mar" => "March",
                "apr" => "April",
                "jun" => "June",
                "jul" => "July",
                "aug" => "August",
                "sep" | "sept" => "September",
                "oct" => "October",
                "nov" => "November",
                "dec" => "December",
                _ => return None,
            };
            vec![Token::Word(month.into())]
        }
    };
    Some((expansion, used + 1))
}

/// All-caps, 2–5 ASCII letters that are not pronounceable → spell it out.
/// "Pronounceable" here: no consonant cluster of 3+, and no leading cluster of 2+
/// before the first vowel (TDA, NBC, FBI, HTML are spelled; NASA, UNESCO stay words).
/// A small allowlist of known pronounceable acronyms is kept as words regardless.
fn is_spellable_acronym(word: &str) -> bool {
    const KEEP_AS_WORDS: [&str; 8] = ["NASA", "UNESCO", "NATO", "UNICEF", "OPEC", "FIFA", "RADAR", "SONAR"];
    let letters: Vec<char> = word.chars().collect();
    if letters.len() < 2 || letters.len() > 5 || !letters.iter().all(char::is_ascii_uppercase) {
        return false;
    }
    if KEEP_AS_WORDS.contains(&word) {
        return false;
    }
    if !letters.iter().any(|c| is_vowel(*c)) {
        return true;
    }
    // Leading consonant cluster of 2+ → hard to pronounce → spell.
    let mut leading_consonants = 0usize;
    for c in &letters {
        if is_vowel(*c) {
            break;
        }
        leading_consonants += 1;
    }
    if leading_consonants >= 2 {
        return true;
    }
    // Any interior run of 3+ consonants → spell.
    let mut run = 0usize;
    for c in &letters {
        if is_vowel(*c) {
            run = 0;
        } else {
            run += 1;
            if run >= 3 {
                return true;
            }
        }
    }
    false
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'A' | 'E' | 'I' | 'O' | 'U')
}

fn symbol_word(c: char) -> &'static str {
    match c {
        '&' => "and",
        '+' => "plus",
        '@' => "at",
        '#' => "number",
        '/' => "slash",
        _ => "",
    }
}

#[cfg(test)]
mod tests;
