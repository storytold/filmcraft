//! French normalization: script → spoken French words.
//!
//! Numbers follow French counting (`soixante et onze`, `quatre-vingts`, plural `cents`
//! / `vingts` only when final, `mille` invariable), read as separate words so the
//! pronunciation step sees `quatre vingts`. Spaces, non-breaking spaces and dots group
//! thousands (`1 000`, `1.000`); a comma marks decimals (`3,5` → `trois virgule cinq`).
//! Also handled: negative numbers, ordinals (`1er`, `1re`, `2e`, `3ème`, `XXIe` is left
//! alone), percent, euros and dollars with cents, clock times (`14h30`, `14 h`), common
//! abbreviations (`M.`, `Mme`, `Dr`, `etc.`, `n°`), spelled acronyms and the symbols
//! `& + @ # /`. Pause markers and sentence ends are the English normalizer's.
//!
//! Rules written from scratch from French spelling and counting conventions (clean-room).

use super::{MAX_CARDINAL_DIGITS, NormalizeError, Token, parse_pause, peek};

/// Normalize a French script (the caller has checked the input size).
pub(super) fn normalize(text: &str) -> Result<Vec<Token>, NormalizeError> {
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
                    pos += 1;
                    out.push(Token::Punct('['));
                }
            },
            '-' | '−' if peek(rest, c.len_utf8()).is_some_and(|d| d.is_ascii_digit()) && !after_word(text, pos) => {
                pos += c.len_utf8();
                out.push(word("moins"));
                let used = number(text.get(pos..).unwrap_or(""), &mut out);
                pos += used.max(1);
            }
            _ if c.is_ascii_digit() => {
                let used = number(rest, &mut out);
                pos += used.max(1);
            }
            '.' | '!' | '?' | '…' => {
                let mut question = false;
                let mut bang = false;
                while let Some(t) = peek(text.get(pos..).unwrap_or(""), 0).filter(|t| matches!(t, '.' | '!' | '?' | '…')) {
                    question |= t == '?';
                    bang |= t == '!';
                    pos += t.len_utf8();
                }
                let end = if question {
                    Token::SentenceEndWith('?')
                } else if bang {
                    Token::SentenceEndWith('!')
                } else {
                    Token::SentenceEnd
                };
                if !matches!(out.last(), Some(Token::SentenceEnd | Token::SentenceEndWith(_))) {
                    out.push(end);
                }
            }
            _ if c.is_alphabetic() => {
                let used = word_run(rest, &mut out);
                pos += used.max(c.len_utf8());
            }
            '&' => {
                pos += 1;
                out.push(word("et"));
            }
            '+' => {
                pos += 1;
                out.push(word("plus"));
            }
            '@' => {
                pos += 1;
                out.push(word("arobase"));
            }
            '#' => {
                pos += 1;
                out.push(word("dièse"));
            }
            '/' => {
                pos += 1;
                out.push(word("sur"));
            }
            '%' => {
                pos += 1;
                out.push(word("pour"));
                out.push(word("cent"));
            }
            '€' => {
                pos += c.len_utf8();
                out.push(word("euros"));
            }
            '«' | '»' | '“' | '”' => {
                pos += c.len_utf8();
                out.push(Token::Punct('"'));
            }
            _ => {
                pos += c.len_utf8();
                out.push(Token::Punct(c));
            }
        }
    }
    Ok(out)
}

fn word(w: &str) -> Token {
    Token::Word(w.to_string())
}

fn push_all(out: &mut Vec<Token>, words: &[&str]) {
    out.extend(words.iter().map(|w| word(w)));
}

/// The character before byte `pos` is a letter or digit (a hyphen there joins, it is no minus sign).
fn after_word(text: &str, pos: usize) -> bool {
    text.get(..pos).and_then(|b| b.chars().next_back()).is_some_and(char::is_alphanumeric)
}

// ---------------------------------------------------------------- numbers

/// Thousands separators accepted between groups of three digits.
fn is_group_sep(c: char) -> bool {
    matches!(c, ' ' | '\u{a0}' | '\u{202f}' | '.')
}

/// Read the digits starting `s` (with thousands separators), returning the digit string and
/// bytes used.
fn integer_part(s: &str) -> (String, usize) {
    let mut digits = String::new();
    let mut used = 0usize;
    let mut group = 0usize;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            digits.push(c);
            group += 1;
            used = i + 1;
        } else if is_group_sep(c) && group > 0 && group <= 3 && digits.len() < 40 {
            // a separator counts only when exactly three digits follow it, then a non-digit
            let after = s.get(i + c.len_utf8()..).unwrap_or("");
            let three = after.chars().take(3).filter(char::is_ascii_digit).count() == 3;
            let fourth = after.chars().nth(3).is_some_and(|d| d.is_ascii_digit());
            if three && !fourth {
                group = 0;
                continue;
            }
            break;
        } else {
            break;
        }
    }
    (digits, used)
}

/// A number at the start of `s` with whatever follows it (decimals, %, €, ordinal suffix,
/// clock time): pushes its words, returns the bytes used.
fn number(s: &str, out: &mut Vec<Token>) -> usize {
    // clock time: 14h30, 14 h 30, 14h
    if let Some(used) = clock_time(s, out) {
        return used;
    }
    let (int, mut used) = integer_part(s);
    if int.is_empty() {
        return 0;
    }
    let rest = s.get(used..).unwrap_or("");
    // ordinals: 1er 1re 1ère 2e 2ème 2nd(e)
    if let Some((suffix_len, feminine, first)) = ordinal_suffix(rest)
        && let Some(n) = parse_small(&int)
    {
        out.push(Token::Word(ordinal(n, feminine && first)));
        return used + suffix_len;
    }
    // decimals: 3,5 (a comma directly followed by a digit)
    let mut decimals: Option<String> = None;
    if rest.starts_with(',') && rest.get(1..).and_then(|r| r.chars().next()).is_some_and(|d| d.is_ascii_digit()) {
        let frac: String = rest.get(1..).unwrap_or("").chars().take_while(char::is_ascii_digit).collect();
        used += 1 + frac.len();
        decimals = Some(frac);
    }
    let rest = s.get(used..).unwrap_or("");
    let trimmed = rest.trim_start_matches([' ', '\u{a0}', '\u{202f}']);
    let gap = rest.len() - trimmed.len();
    // currency after the amount: 3,50 € / 3 € 50 / 10 $
    if let Some(cur) = trimmed.chars().next().filter(|c| matches!(c, '€' | '$')) {
        let (unit, units) = if cur == '€' { ("euro", "euros") } else { ("dollar", "dollars") };
        push_cardinal(&int, out);
        out.push(word(if int.trim_start_matches('0') == "1" { unit } else { units }));
        if let Some(frac) = decimals.filter(|f| !f.trim_start_matches('0').is_empty()) {
            let cents = if frac.len() == 1 { format!("{frac}0") } else { frac.chars().take(2).collect() };
            push_cardinal(&cents, out);
        }
        return used + gap + cur.len_utf8();
    }
    push_cardinal(&int, out);
    if let Some(frac) = decimals {
        out.push(word("virgule"));
        // "3,05" reads zéro cinq; otherwise the decimals as one number when short
        if frac.starts_with('0') || frac.len() > 3 {
            for d in frac.chars() {
                out.push(word(digit_word(d)));
            }
        } else {
            push_cardinal(&frac, out);
        }
    }
    if trimmed.starts_with('%') {
        push_all(out, &["pour", "cent"]);
        return used + gap + 1;
    }
    used
}

/// `14h30`, `14 h 30`, `9h`: hours (0-24) and minutes (00-59).
fn clock_time(s: &str, out: &mut Vec<Token>) -> Option<usize> {
    let hours: String = s.chars().take_while(char::is_ascii_digit).collect();
    if hours.is_empty() || hours.len() > 2 {
        return None;
    }
    let mut used = hours.len();
    let rest = s.get(used..)?;
    let after_space = rest.trim_start_matches(' ');
    let space = rest.len() - after_space.len();
    if !after_space.starts_with('h') || space > 1 {
        return None;
    }
    // the h must not start a word ("3 heures", "2 hommes")
    if after_space.get(1..).and_then(|r| r.chars().next()).is_some_and(char::is_alphabetic) {
        return None;
    }
    used += space + 1;
    let h: u32 = hours.parse().ok()?;
    if h > 24 {
        return None;
    }
    let rest = s.get(used..)?;
    let after_space = rest.trim_start_matches(' ');
    let space2 = rest.len() - after_space.len();
    let minutes: String = after_space.chars().take_while(char::is_ascii_digit).collect();
    push_cardinal_n(u64::from(h), out, true);
    out.push(word(if h <= 1 { "heure" } else { "heures" }));
    if minutes.len() == 2 && space2 <= 1 {
        let m: u32 = minutes.parse().ok()?;
        if m < 60 {
            if m > 0 {
                push_cardinal_n(u64::from(m), out, true);
            }
            used += space2 + 2;
        }
    }
    Some(used)
}

fn parse_small(digits: &str) -> Option<u64> {
    if digits.len() > MAX_CARDINAL_DIGITS {
        return None;
    }
    digits.parse().ok()
}

/// Ordinal suffix after the digits: (bytes, feminine, is "first").
fn ordinal_suffix(rest: &str) -> Option<(usize, bool, bool)> {
    let end = |len: usize| !rest.get(len..).and_then(|r| r.chars().next()).is_some_and(char::is_alphanumeric);
    for (suffix, feminine, first) in [
        ("ère", true, true),
        ("ière", true, false),
        ("ème", false, false),
        ("nde", true, false),
        ("er", false, true),
        ("re", true, true),
        ("nd", false, false),
        ("e", false, false),
    ] {
        if rest.starts_with(suffix) && end(suffix.len()) {
            return Some((suffix.len(), feminine, first));
        }
    }
    None
}

/// Ordinal word: premier / première, second(e) is read deuxième, otherwise cardinal + ième.
fn ordinal(n: u64, feminine_first: bool) -> String {
    if n == 1 {
        return if feminine_first { "première".into() } else { "premier".into() };
    }
    let words = cardinal_words(n, true);
    let last = words.last().cloned().unwrap_or_default();
    let stem = match last.as_str() {
        "un" => "unième".to_string(),
        "cinq" => "cinquième".to_string(),
        "neuf" => "neuvième".to_string(),
        "cents" | "vingts" => format!("{}ième", last.trim_end_matches('s')),
        w => format!("{}ième", w.strip_suffix('e').unwrap_or(w)),
    };
    let mut all = words;
    all.pop();
    all.push(stem);
    all.join(" ")
}

fn digit_word(d: char) -> &'static str {
    match d {
        '0' => "zéro",
        '1' => "un",
        '2' => "deux",
        '3' => "trois",
        '4' => "quatre",
        '5' => "cinq",
        '6' => "six",
        '7' => "sept",
        '8' => "huit",
        _ => "neuf",
    }
}

/// Push a digit string as a cardinal; very long numbers and ones with a leading zero
/// (`06 12`, codes) are read digit by digit.
fn push_cardinal(digits: &str, out: &mut Vec<Token>) {
    match parse_small(digits).filter(|_| !(digits.len() > 1 && digits.starts_with('0'))) {
        Some(n) => push_cardinal_n(n, out, true),
        None => {
            for d in digits.chars() {
                out.push(word(digit_word(d)));
            }
        }
    }
}

fn push_cardinal_n(n: u64, out: &mut Vec<Token>, final_plural: bool) {
    for w in cardinal_words(n, final_plural) {
        out.push(Token::Word(w));
    }
}

const UNITS: [&str; 17] =
    ["zéro", "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf", "dix", "onze", "douze", "treize", "quatorze", "quinze", "seize"];
const TENS: [&str; 7] = ["", "dix", "vingt", "trente", "quarante", "cinquante", "soixante"];

/// French cardinal as separate words (`quatre-vingt-dix-sept` → quatre vingt dix sept).
/// `final_plural`: the number ends the phrase, so `quatre-vingts` / `deux cents` take their s.
pub(super) fn cardinal_words(n: u64, final_plural: bool) -> Vec<String> {
    if n == 0 {
        return vec!["zéro".into()];
    }
    let mut out: Vec<String> = Vec::new();
    let scales: [(u64, &str, &str); 3] = [(1_000_000_000, "milliard", "milliards"), (1_000_000, "million", "millions"), (1_000, "mille", "mille")];
    let mut rest = n;
    for (scale, one, many) in scales {
        let q = rest / scale;
        rest %= scale;
        if q == 0 {
            continue;
        }
        if scale == 1_000 {
            if q > 1 {
                below_thousand(q, false, &mut out);
            }
            out.push("mille".into());
        } else {
            below_thousand(q, false, &mut out);
            out.push(if q > 1 { many.into() } else { one.into() });
        }
    }
    if rest > 0 {
        below_thousand(rest, final_plural, &mut out);
    }
    out
}

fn below_thousand(n: u64, final_plural: bool, out: &mut Vec<String>) {
    let hundreds = n / 100;
    let rest = n % 100;
    if hundreds > 0 {
        if hundreds > 1 {
            out.push(UNITS.get(hundreds as usize).copied().unwrap_or("").into());
        }
        out.push(if hundreds > 1 && rest == 0 && final_plural { "cents".into() } else { "cent".into() });
    }
    if rest > 0 {
        below_hundred(rest, final_plural, out);
    }
}

fn below_hundred(n: u64, final_plural: bool, out: &mut Vec<String>) {
    if n <= 16 {
        out.push(UNITS.get(n as usize).copied().unwrap_or("").into());
        return;
    }
    let (tens, unit) = (n / 10, n % 10);
    match tens {
        1 => {
            // 17-19: dix sept
            out.push("dix".into());
            out.push(UNITS.get(unit as usize).copied().unwrap_or("").into());
        }
        7 => {
            // 70-79: soixante dix / soixante et onze / soixante douze
            out.push("soixante".into());
            if unit == 1 {
                out.push("et".into());
            }
            below_hundred(10 + unit, final_plural, out);
        }
        8 | 9 => {
            out.push("quatre".into());
            let plain = n == 80;
            out.push(if plain && final_plural { "vingts".into() } else { "vingt".into() });
            let r = n - 80;
            if r > 0 {
                below_hundred(r, final_plural, out);
            }
        }
        _ => {
            out.push(TENS.get(tens as usize).copied().unwrap_or("").into());
            if unit == 1 {
                out.push("et".into());
            }
            if unit > 0 {
                out.push(UNITS.get(unit as usize).copied().unwrap_or("").into());
            }
        }
    }
}

// ---------------------------------------------------------------- words

/// Letters (with accents), apostrophes and inner hyphens; abbreviations and acronyms.
fn word_run(s: &str, out: &mut Vec<Token>) -> usize {
    let mut end = 0usize;
    let mut prev_hyphen = false;
    for (i, c) in s.char_indices() {
        let inner = (c == '\'' || c == '’' || c == '-') && end > 0 && s.get(i + c.len_utf8()..).and_then(|r| r.chars().next()).is_some_and(char::is_alphabetic);
        if c.is_alphabetic() || (inner && !prev_hyphen) {
            prev_hyphen = c == '-';
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    let Some(w) = s.get(..end) else { return 0 };
    let w = w.replace('’', "'");
    let next = s.get(end..).and_then(|r| r.chars().next());
    // dotted abbreviations: M. Mme. etc. Dr. St.
    if next == Some('.') {
        let expanded: Option<&[&str]> = match w.as_str() {
            "M" => Some(&["monsieur"]),
            "MM" => Some(&["messieurs"]),
            "etc" => Some(&["et", "cetera"]),
            "Dr" => Some(&["docteur"]),
            "Pr" => Some(&["professeur"]),
            "St" => Some(&["saint"]),
            "Ste" => Some(&["sainte"]),
            "av" => Some(&["avenue"]),
            "bd" => Some(&["boulevard"]),
            "p" => Some(&["page"]),
            "env" => Some(&["environ"]),
            "cf" => Some(&["confer"]),
            "ex" => Some(&["exemple"]),
            _ => None,
        };
        if let Some(words) = expanded {
            push_all(out, words);
            return end + 1;
        }
    }
    let plain: Option<&[&str]> = match w.as_str() {
        "Mme" => Some(&["madame"]),
        "Mmes" => Some(&["mesdames"]),
        "Mlle" => Some(&["mademoiselle"]),
        "Dr" => Some(&["docteur"]),
        "km" => Some(&["kilomètres"]),
        "kg" => Some(&["kilos"]),
        "cm" => Some(&["centimètres"]),
        "mm" => Some(&["millimètres"]),
        "h" => Some(&["heures"]),
        "min" => Some(&["minutes"]),
        _ => None,
    };
    if let Some(words) = plain {
        push_all(out, words);
        return end;
    }
    if (w == "n" || w == "N") && next == Some('°') {
        out.push(word("numéro"));
        return end + '°'.len_utf8();
    }
    if is_spelled_acronym(&w) {
        for c in w.chars() {
            out.push(Token::Word(c.to_string()));
        }
        return end;
    }
    // hyphenated words go through as separate words (arc-en-ciel → arc en ciel), but a
    // pronoun after a verb (dit-il, va-t-on) keeps its euphonic t
    for part in w.split('-').filter(|p| !p.is_empty()) {
        out.push(word(part));
    }
    end
}

/// An all-caps word of 2-6 letters read letter by letter (SNCF, TGV); pronounceable
/// ones with a vowel pattern (ONU, UNESCO, OTAN) and a few known words stay words.
fn is_spelled_acronym(w: &str) -> bool {
    let n = w.chars().count();
    if !(2..=6).contains(&n) || !w.chars().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    if matches!(w, "ONU" | "OTAN" | "UNESCO" | "UNICEF" | "NASA" | "OVNI" | "SIDA" | "CAC" | "PIB" | "OK") {
        return w == "PIB";
    }
    let vowel = |c: char| matches!(c, 'A' | 'E' | 'I' | 'O' | 'U' | 'Y');
    let letters: Vec<char> = w.chars().collect();
    // no vowel, or two consonants in a row at the start, or three anywhere: spell it
    !letters.iter().any(|&c| vowel(c))
        || letters.get(..2).is_some_and(|p| p.iter().all(|&c| !vowel(c)))
        || letters.windows(3).any(|t| t.iter().all(|&c| !vowel(c)))
        || n <= 3
}
