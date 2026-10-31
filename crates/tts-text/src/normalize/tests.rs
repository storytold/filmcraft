use super::*;

/// Flatten tokens the way the tests compare them: words joined by spaces,
/// pauses as `[pause N]`, sentence ends as `|`, punctuation as the raw char.
fn flat(tokens: &[Token]) -> String {
    let mut s = String::new();
    for t in tokens {
        match t {
            Token::Word(w) => {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(w);
            }
            Token::Pause { millis } => {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(&format!("[pause {millis}]"));
            }
            Token::SentenceEnd => {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push('|');
            }
            Token::SentenceEndWith(c) => {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push('|');
                s.push(*c);
            }
            Token::Punct(c) => {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push(*c);
            }
        }
    }
    s
}

fn words(text: &str, lang: Lang) -> String {
    flat(&normalize(text, lang).unwrap())
}

#[test]
fn plain_words_pass_through() {
    assert_eq!(words("Hello world", Lang::EnUs), "Hello world");
    assert_eq!(words("well-known", Lang::EnUs), "well - known");
    assert_eq!(words("it's fine", Lang::EnUs), "it's fine");
}

#[test]
fn cardinals() {
    assert_eq!(words("0", Lang::EnUs), "zero");
    assert_eq!(words("7", Lang::EnUs), "seven");
    assert_eq!(words("13", Lang::EnUs), "thirteen");
    assert_eq!(words("20", Lang::EnUs), "twenty");
    assert_eq!(words("34", Lang::EnUs), "thirty-four");
    assert_eq!(words("100", Lang::EnUs), "one hundred");
    assert_eq!(words("105", Lang::EnUs), "one hundred five");
    assert_eq!(words("999", Lang::EnUs), "nine hundred ninety-nine");
    assert_eq!(words("1,234", Lang::EnUs), "one thousand two hundred thirty-four");
    assert_eq!(words("1,234,567", Lang::EnUs), "one million two hundred thirty-four thousand five hundred sixty-seven");
    assert_eq!(words("1000000", Lang::EnUs), "one million");
    assert_eq!(words("1000000000", Lang::EnUs), "one billion");
    assert_eq!(
        words("999,999,999,999", Lang::EnUs),
        "nine hundred ninety-nine billion nine hundred ninety-nine million nine hundred ninety-nine thousand nine hundred ninety-nine"
    );
    assert_eq!(words("123456789012", Lang::EnUs), "one hundred twenty-three billion four hundred fifty-six million seven hundred eighty-nine thousand twelve");
}

#[test]
fn oversized_and_leading_zero_numbers_go_digit_by_digit() {
    assert_eq!(words("007", Lang::EnUs), "zero zero seven");
    assert_eq!(words("12345678901234", Lang::EnUs), "one two three four five six seven eight nine zero one two three four");
    assert_eq!(words("0.5", Lang::EnUs), "zero point five");
}

#[test]
fn numbers_longer_than_twenty_digits_are_read_fully_digit_by_digit() {
    assert_eq!(
        words("12345678901234567890123", Lang::EnUs),
        "one two three four five six seven eight nine zero one two three four five six seven eight nine zero one two three"
    );
    assert_eq!(
        words("1234567890123456789000123", Lang::EnUs),
        "one two three four five six seven eight nine zero one two three four five six seven eight nine zero zero zero one two three"
    );
}

#[test]
fn negatives_and_decimals() {
    assert_eq!(words("-5", Lang::EnUs), "minus five");
    assert_eq!(words("3.14", Lang::EnUs), "three point one four");
    assert_eq!(words(".5", Lang::EnUs), "point five");
    assert_eq!(words("-2.5", Lang::EnUs), "minus two point five");
}

#[test]
fn ordinals() {
    assert_eq!(words("1st", Lang::EnUs), "first");
    assert_eq!(words("2nd", Lang::EnUs), "second");
    assert_eq!(words("3rd", Lang::EnUs), "third");
    assert_eq!(words("7th", Lang::EnUs), "seventh");
    assert_eq!(words("9th", Lang::EnUs), "ninth");
    assert_eq!(words("12th", Lang::EnUs), "twelfth");
    assert_eq!(words("20th", Lang::EnUs), "twentieth");
    assert_eq!(words("21st", Lang::EnUs), "twenty-first");
    assert_eq!(words("33rd", Lang::EnUs), "thirty-third");
    assert_eq!(words("100th", Lang::EnUs), "one hundredth");
    assert_eq!(words("101st", Lang::EnUs), "one hundred first");
}

#[test]
fn magnitude_is_kept_before_exact_tens_ordinals() {
    for lang in [Lang::EnUs, Lang::EnGb] {
        for (input, expected) in [
            ("20th", "twentieth"),
            ("100th", "one hundredth"),
            ("101st", "one hundred first"),
            ("119th", "one hundred nineteenth"),
            ("120th", "one hundred twentieth"),
            ("130th", "one hundred thirtieth"),
            ("150th", "one hundred fiftieth"),
            ("190th", "one hundred ninetieth"),
            ("1,020th", "one thousand twentieth"),
            ("1120th", "one thousand one hundred twentieth"),
            ("121st", "one hundred twenty-first"),
            ("200th", "two hundredth"),
        ] {
            assert_eq!(words(input, lang), expected, "{input:?} {lang:?}");
        }
    }
}

#[test]
fn money() {
    assert_eq!(words("$3.50", Lang::EnUs), "three dollars and fifty cents");
    assert_eq!(words("$1", Lang::EnUs), "one dollar");
    assert_eq!(words("$1.00", Lang::EnUs), "one dollar");
    assert_eq!(words("$1.5", Lang::EnUs), "one dollar and fifty cents");
    assert_eq!(words("$2.5", Lang::EnUs), "two dollars and fifty cents");
    assert_eq!(words("$0.5", Lang::EnUs), "zero dollars and fifty cents");
    assert_eq!(words("$1.05", Lang::EnUs), "one dollar and five cents");
    assert_eq!(words("The price is $1.5.", Lang::EnUs), words("The price is $1.50.", Lang::EnUs));
    assert_eq!(words("£1.5", Lang::EnGb), "one pound and fifty pence");
    assert_eq!(words("€1.5", Lang::EnUs), "one euro and fifty cents");
    assert_eq!(words("$5.05", Lang::EnUs), "five dollars and five cents");
    assert_eq!(words("$0.75", Lang::EnUs), "zero dollars and seventy-five cents");
    assert_eq!(words("$1,234.56", Lang::EnUs), "one thousand two hundred thirty-four dollars and fifty-six cents");
    assert_eq!(words("€2", Lang::EnUs), "two euros");
    assert_eq!(words("€1", Lang::EnUs), "one euro");
    assert_eq!(words("£2.50", Lang::EnUs), "two pounds and fifty pence");
    assert_eq!(words("£1", Lang::EnUs), "one pound");
    assert_eq!(words("$0.50", Lang::EnUs), "zero dollars and fifty cents");
}

#[test]
fn negative_money_keeps_its_sign() {
    for lang in [Lang::EnUs, Lang::EnGb] {
        assert_eq!(words("-$5", lang), "minus five dollars");
        assert_eq!(words("-€3.50", lang), "minus three euros and fifty cents");
        assert_eq!(words("-£2", lang), "minus two pounds");
        assert_eq!(words("a refund of -$1.05", lang), "a refund of minus one dollar and five cents");
        assert_eq!(words("(-$5)", lang), "( minus five dollars )");
        // A hyphen between two amounts is a range, not a sign.
        assert_eq!(words("$10-$20", lang), "ten dollars - twenty dollars");
        assert_eq!(words("€5-€8", lang), "five euros - eight euros");
        assert_eq!(words("$10.50-$20", lang), "ten dollars and fifty cents - twenty dollars");
        // A hyphen after a word or before a non-amount stays punctuation.
        assert_eq!(words("a-$5", lang), "a - five dollars");
        assert!(!words("-$x", lang).contains("minus"));
    }
}

#[test]
fn money_fraction_beyond_two_digits_is_not_hundreds_of_cents() {
    for lang in [Lang::EnUs, Lang::EnGb] {
        assert_eq!(words("gas is $3.459 a gallon", lang), "gas is three dollars and forty-five cents a gallon");
        assert_eq!(words("$1.999", lang), "one dollar and ninety-nine cents");
        assert_eq!(words("$1.250", lang), "one dollar and twenty-five cents");
        assert_eq!(words("$1.005", lang), "one dollar");
    }
}

#[test]
fn first_digit_group_keeps_leading_zeros() {
    for lang in [Lang::EnUs, Lang::EnGb] {
        for (input, expected) in [
            ("01-2", "zero one two"),
            ("01-2-3", "zero one two three"),
            ("0800-55", "zero eight zero zero five five"),
            ("0800-1-2", "zero eight zero zero one two"),
            ("0-2", "zero to two"),
            ("0-2-3", "zero to two to three"),
            ("2-3", "two to three"),
            ("5-01", "five zero one"),
            ("555-0134", "five five five zero one three four"),
            ("0800-100", "zero eight zero zero one zero zero"),
            ("5-10-15", "five to ten to fifteen"),
            ("zero one two", "zero one two"),
            ("zero eight zero zero five five", "zero eight zero zero five five"),
        ] {
            assert_eq!(words(input, lang), expected, "{lang:?}: {input}");
        }
    }
}

#[test]
fn short_hyphenated_groups_keep_every_number() {
    for lang in [Lang::EnUs, Lang::EnGb] {
        assert_eq!(words("1-2-3", lang), "one to two to three");
        assert_eq!(words("5-10-15", lang), "five to ten to fifteen");
        assert_eq!(words("10-20-30", lang), "ten to twenty to thirty");
        assert_eq!(words("1-2-3-4", lang), "one to two to three to four");
        assert_eq!(words("1-2-3-4-5", lang), "one to two to three to four to five");
        assert_eq!(words("1-2-3-4-5-6", lang), "one to two to three to four to five to six");
        assert_eq!(words("2-3", lang), "two to three");
        assert_eq!(words("10-12", lang), "ten to twelve");
        assert_eq!(words("555-0134", lang), "five five five zero one three four");
        assert_eq!(words("005-010-015", lang), "zero zero five zero one zero zero one five");
    }
}

#[test]
fn percent() {
    assert_eq!(words("20%", Lang::EnUs), "twenty percent");
    assert_eq!(words("100%", Lang::EnUs), "one hundred percent");
    assert_eq!(words("7%", Lang::EnUs), "seven percent");
    for lang in [Lang::EnUs, Lang::EnGb] {
        assert_eq!(words("20.5%", lang), "twenty point five percent");
        assert_eq!(words("0.5%", lang), "zero point five percent");
        assert_eq!(words("50 %", lang), "fifty percent");
        assert_eq!(words("50\t%", lang), "fifty percent");
        assert_eq!(words("50\u{a0}%", lang), "fifty percent");
        assert_eq!(words("100 %", lang), "one hundred percent");
        assert_eq!(words("20.5 %", lang), "twenty point five percent");
        assert_eq!(words("20.5%", lang), words("twenty point five percent", lang));
    }
}

#[test]
fn years() {
    assert_eq!(words("1999", Lang::EnUs), "nineteen ninety-nine");
    assert_eq!(words("2005", Lang::EnUs), "two thousand five");
    assert_eq!(words("2026", Lang::EnUs), "twenty twenty-six");
    assert_eq!(words("1900", Lang::EnUs), "nineteen hundred");
    assert_eq!(words("1905", Lang::EnUs), "nineteen oh five");
    assert_eq!(words("1100", Lang::EnUs), "eleven hundred");
    assert_eq!(words("2000", Lang::EnUs), "two thousand");
    // Not years: outside the range, or part of a bigger number.
    assert_eq!(words("2100", Lang::EnUs), "two thousand one hundred");
    assert_eq!(words("1099", Lang::EnUs), "one thousand ninety-nine");
    assert_eq!(words("19995", Lang::EnUs), "nineteen thousand nine hundred ninety-five");
}

#[test]
fn times() {
    assert_eq!(words("3:30", Lang::EnUs), "three thirty");
    assert_eq!(words("15:05", Lang::EnUs), "fifteen oh five");
    assert_eq!(words("0:15", Lang::EnUs), "zero fifteen");
    assert_eq!(words("03:30", Lang::EnUs), "three thirty");
    assert_eq!(words("23:59", Lang::EnUs), "twenty-three fifty-nine");
    assert_eq!(words("3:30pm", Lang::EnUs), "three thirty p m");
    assert_eq!(words("3:30 p.m.", Lang::EnUs), "three thirty p m");
    assert_eq!(words("9:00 a.m.", Lang::EnUs), "nine A m");
    assert_eq!(words("9:00 PM", Lang::EnUs), "nine p m");
    assert_eq!(words("12:00", Lang::EnUs), "twelve");
}

#[test]
fn dates() {
    assert_eq!(words("10/7/2026", Lang::EnUs), "October seventh twenty twenty-six");
    assert_eq!(words("7/10/2026", Lang::EnGb), "the seventh of October twenty twenty-six");
    assert_eq!(words("Oct. 7th", Lang::EnUs), "October seventh");
    assert_eq!(words("Jan. 1st", Lang::EnUs), "January first");
    assert_eq!(words("Sept. 3rd", Lang::EnUs), "September third");
    assert_eq!(words("May 5th", Lang::EnUs), "May fifth");
    assert_eq!(words("October 7th, 2026", Lang::EnUs), "October seventh , twenty twenty-six");
    assert_eq!(words("9/9/1999", Lang::EnUs), "September ninth nineteen ninety-nine");
    assert_eq!(words("13/7/2026", Lang::EnUs), "thirteen slash seven slash twenty twenty-six");
    assert_eq!(words("10/7/2026.", Lang::EnUs), "October seventh twenty twenty-six |");
}

#[test]
fn digit_groups() {
    assert_eq!(words("555-0134", Lang::EnUs), "five five five zero one three four");
    assert_eq!(words("555-0134-22", Lang::EnUs), "five five five zero one three four two two");
    assert_eq!(words("0800-100", Lang::EnUs), "zero eight zero zero one zero zero");
    assert_eq!(words("2-3", Lang::EnUs), "two to three");
    assert_eq!(words("10-12", Lang::EnUs), "ten to twelve");
}

#[test]
fn abbreviations() {
    assert_eq!(words("Mr. Smith", Lang::EnUs), "mister Smith");
    assert_eq!(words("Mrs. Lee", Lang::EnUs), "missus Lee");
    assert_eq!(words("Dr. Smith", Lang::EnUs), "doctor Smith");
    assert_eq!(words("St. James", Lang::EnUs), "saint James");
    assert_eq!(words("James St.", Lang::EnUs), "James street");
    assert_eq!(words("Mt. Fuji", Lang::EnUs), "mount Fuji");
    assert_eq!(words("vs. foo", Lang::EnUs), "versus foo");
    assert_eq!(words("etc. and", Lang::EnUs), "et cetera and");
    assert_eq!(words("e.g. x", Lang::EnUs), "for example x");
    assert_eq!(words("i.e. y", Lang::EnUs), "that is y");
    // An abbreviation's dot never ends the sentence.
    let tokens = normalize("Call Dr. Smith. Now.", Lang::EnUs).unwrap();
    assert_eq!(flat(&tokens), "Call doctor Smith | Now |");
}

#[test]
fn acronyms() {
    assert_eq!(words("TDA", Lang::EnUs), "T D A");
    assert_eq!(words("NBC", Lang::EnUs), "N B C");
    assert_eq!(words("FBI", Lang::EnUs), "F B I");
    assert_eq!(words("HTML", Lang::EnUs), "H T M L");
    assert_eq!(words("NASA", Lang::EnUs), "NASA");
    assert_eq!(words("UNESCO", Lang::EnUs), "UNESCO");
    assert_eq!(words("laser", Lang::EnUs), "laser");
    assert_eq!(words("A", Lang::EnUs), "A");
}

#[test]
fn symbols() {
    assert_eq!(words("&", Lang::EnUs), "and");
    assert_eq!(words("+", Lang::EnUs), "plus");
    assert_eq!(words("@", Lang::EnUs), "at");
    assert_eq!(words("#", Lang::EnUs), "number");
    assert_eq!(words("/", Lang::EnUs), "slash");
    assert_eq!(words("#3", Lang::EnUs), "number three");
    assert_eq!(words("Tom & Jerry", Lang::EnUs), "Tom and Jerry");
}

#[test]
fn fractions() {
    assert_eq!(words("1/2", Lang::EnUs), "one half");
    assert_eq!(words("3/4", Lang::EnUs), "three quarters");
    assert_eq!(words("2/3", Lang::EnUs), "two thirds");
    assert_eq!(words("1/3", Lang::EnUs), "one third");
    assert_eq!(words("22/7", Lang::EnUs), "twenty-two sevenths");
    assert_eq!(words("5/8", Lang::EnUs), "five eighths");
    assert_eq!(words("7/32", Lang::EnUs), "seven over thirty-two");
}

#[test]
fn fraction_operands_may_have_three_digits() {
    for lang in [Lang::EnUs, Lang::EnGb] {
        for (input, expected) in [
            ("7/100", "seven over one hundred"),
            ("100/7", "one hundred sevenths"),
            ("7/99", "seven over ninety-nine"),
            ("99/7", "ninety-nine sevenths"),
            ("7/0", "seven slash zero"),
            ("1/2", "one half"),
            ("3/4", "three quarters"),
            ("22/7", "twenty-two sevenths"),
        ] {
            assert_eq!(words(input, lang), expected, "{input:?} {lang:?}");
        }
    }
    assert_eq!(words("10/7/2026", Lang::EnUs), "October seventh twenty twenty-six");
    assert_eq!(words("7/10/2026", Lang::EnGb), "the seventh of October twenty twenty-six");
}

#[test]
fn percent_and_decimal_suffixes_keep_leading_zeroes() {
    for lang in [Lang::EnUs, Lang::EnGb] {
        for (input, expected) in [
            ("07", "zero seven"),
            ("007", "zero zero seven"),
            ("07%", "zero seven percent"),
            ("007%", "zero zero seven percent"),
            ("007 %", "zero zero seven percent"),
            ("07.5", "zero seven point five"),
            ("007.5", "zero zero seven point five"),
            ("007.5%", "zero zero seven point five percent"),
            ("20%", "twenty percent"),
            ("0.5", "zero point five"),
            ("0.5%", "zero point five percent"),
        ] {
            assert_eq!(words(input, lang), expected, "{input:?} {lang:?}");
        }
    }
}

#[test]
fn pause_markers() {
    let tokens = normalize("[pause 1s]", Lang::EnUs).unwrap();
    assert_eq!(tokens, vec![Token::Pause { millis: 1000 }]);
    assert_eq!(words("[pause 500ms]", Lang::EnUs), "[pause 500]");
    assert_eq!(words("[pause 1.5s]", Lang::EnUs), "[pause 1500]");
    assert_eq!(words("[pause 10s]", Lang::EnUs), "[pause 10000]");
    assert_eq!(words("[pause 0.25s]", Lang::EnUs), "[pause 250]");
    assert_eq!(words("[pause 11s]", Lang::EnUs), "[pause 10000]");
    assert_eq!(words("[pause 999999999999s]", Lang::EnUs), "[pause 10000]");
    // byte 64 falls inside an `é` after the marker: still a pause
    let tokens = normalize(&format!("[pause 1s]x{}", "é".repeat(40)), Lang::EnUs).unwrap();
    assert_eq!(tokens.first(), Some(&Token::Pause { millis: 1000 }));
    // Malformed markers stay plain text.
    assert_eq!(words("[pause", Lang::EnUs), "[ pause");
    assert_eq!(words("[pause x]", Lang::EnUs), "[ pause x ]");
    assert_eq!(words("[pause 5]", Lang::EnUs), "[ pause five ]");
    assert_eq!(words("[pause]", Lang::EnUs), "[ pause ]");
}

#[test]
fn sentence_ends() {
    let tokens = normalize("Hi. Bye! What?", Lang::EnUs).unwrap();
    assert_eq!(flat(&tokens), "Hi | Bye |! What |?");
    // Runs of terminators are one end; the strongest terminator wins.
    assert_eq!(flat(&normalize("Wow!!!", Lang::EnUs).unwrap()), "Wow |!");
    assert_eq!(flat(&normalize("Really?! Yes.", Lang::EnUs).unwrap()), "Really |? Yes |");
    // The terminator is carried through for intonation (M7.11 fix round).
    assert_eq!(flat(&normalize("Go!", Lang::EnUs).unwrap()), "Go |!");
    assert_eq!(flat(&normalize("Done.", Lang::EnUs).unwrap()), "Done |");
    assert_eq!(flat(&normalize("Is it 1999?", Lang::EnUs).unwrap()), "Is it nineteen ninety-nine |?");
    assert_eq!(flat(&normalize("Call 555-0134!", Lang::EnUs).unwrap()), "Call five five five zero one three four |!");
    assert_eq!(flat(&normalize("Wait... what?", Lang::EnUs).unwrap()), "Wait | what |?");
    assert_eq!(flat(&normalize("No!!", Lang::EnUs).unwrap()), "No |!");
    assert_eq!(flat(&normalize("Are you there?!", Lang::EnUs).unwrap()), "Are you there |?");
    assert_eq!(flat(&normalize("Yes?", Lang::EnGb).unwrap()), "Yes |?");
    // A decimal point is not a sentence end; an abbreviation's dot is consumed.
    assert_eq!(flat(&normalize("It costs 3.14 dollars.", Lang::EnUs).unwrap()), "It costs three point one four dollars |");
    assert_eq!(flat(&normalize("Mr. Smith called.", Lang::EnUs).unwrap()), "mister Smith called |");
    // No sentence end inside "1,234." until the real end.
    assert_eq!(flat(&normalize("Total: 1,234.", Lang::EnUs).unwrap()), "Total : one thousand two hundred thirty-four |");
}

#[test]
fn the_briefs_example_sentence() {
    assert_eq!(
        words("Call 555-0134 by 3:30 p.m. on Oct. 7th, it's $3.50 (20% off)", Lang::EnUs),
        "Call five five five zero one three four by three thirty p m on October seventh , it's three dollars and fifty cents ( twenty percent off )"
    );
}

#[test]
fn punctuation_is_kept() {
    assert_eq!(words("a, b; c", Lang::EnUs), "a , b ; c");
    assert_eq!(words("(x)", Lang::EnUs), "( x )");
    assert_eq!(words("a—b", Lang::EnUs), "a — b");
    assert_eq!(words("end.", Lang::EnUs), "end |");
}

#[test]
fn bad_grouping_falls_back() {
    assert_eq!(words("1,23", Lang::EnUs), "one , twenty-three");
    assert_eq!(words("1,2345", Lang::EnUs), "one , two thousand three hundred forty-five");
}

#[test]
fn malformed_marker_and_apostrophe_runs_make_linear_progress() {
    let brackets = "[".repeat(20_000);
    let apostrophes = "'".repeat(20_000);
    assert_eq!(normalize(&brackets, Lang::EnUs).unwrap().len(), 20_000);
    assert_eq!(normalize(&apostrophes, Lang::EnUs).unwrap().len(), 20_000);
}

#[test]
fn hostile_inputs_do_not_panic() {
    let hostile: Vec<String> = vec![
        String::new(),
        "   \t\n  ".into(),
        "x".repeat(MAX_INPUT) + "x",
        "9".repeat(10_000),
        "$$$$".into(),
        "[pause 999999999999s]".into(),
        "[pause".into(),
        "[pause x]".into(),
        "日本語のテキスト".into(),
        "emoji 👍👍👍".into(),
        "العربية".into(),
        "\u{0301}".into(),
        "\0".into(),
        "3:".into(),
        "1,,2".into(),
        ".....".into(),
        "[[[[".into(),
        "-".into(),
        "$".into(),
        "3:305".into(),
        "5-".into(),
        "999999999999999999999999".into(),
    ];
    for h in &hostile {
        let result = std::panic::catch_unwind(|| normalize(h, Lang::EnUs));
        assert!(result.is_ok(), "panicked on {h:?}");
        // The oversized case must error, not degrade.
        if h.len() > MAX_INPUT {
            assert_eq!(result.unwrap(), Err(NormalizeError::TooLong));
        }
    }
}

#[test]
fn deterministic() {
    let samples = ["Call 555-0134 by 3:30 p.m. on Oct. 7th, it's $3.50 (20% off)", "1,234", "[pause 1s]", "TDA & NASA"];
    for s in samples {
        let a = normalize(s, Lang::EnUs).unwrap();
        let b = normalize(s, Lang::EnUs).unwrap();
        assert_eq!(a, b);
        let c = normalize(s, Lang::EnGb).unwrap();
        assert_eq!(flat(&a), flat(&c), "langs only change word choices, not structure");
    }
}

#[test]
fn british_vs_american() {
    assert_eq!(words("£2.50", Lang::EnGb), "two pounds and fifty pence");
    assert_eq!(words("7/10/2026", Lang::EnGb), "the seventh of October twenty twenty-six");
    assert_eq!(words("10/7/2026", Lang::EnUs), "October seventh twenty twenty-six");
    assert_eq!(words("1,234", Lang::EnGb), "one thousand two hundred thirty-four");
}

#[test]
fn currency_followed_by_scale_word_reads_scale_before_unit() {
    assert_eq!(words("$5 million", Lang::EnUs), "five million dollars");
    assert_eq!(words("$2.5 billion", Lang::EnUs), "two point five billion dollars");
    assert_eq!(words("worth $40 billion.", Lang::EnUs), "worth forty billion dollars |");
    assert_eq!(words("$1 million", Lang::EnUs), "one million dollars");
    assert_eq!(words("£3 Thousand", Lang::EnGb), "three thousand pounds");
    assert_eq!(words("€1.5 trillion", Lang::EnUs), "one point five trillion euros");
    // No scale word: unchanged. A longer word is not a scale word.
    assert_eq!(words("$5 millions", Lang::EnUs), "five dollars millions");
    assert_eq!(words("$2.50 now", Lang::EnUs), "two dollars and fifty cents now");
}

#[test]
fn sentence_final_numbers_end_the_sentence() {
    assert_eq!(words("in 1999.", Lang::EnUs), "in nineteen ninety-nine |");
    assert_eq!(words("costs $1,234,567.", Lang::EnUs), "costs one million two hundred thirty-four thousand five hundred sixty-seven dollars |");
    assert_eq!(words("It was 3.5 percent.", Lang::EnUs), "It was three point five percent |");
    assert_eq!(words("Version 2. Next.", Lang::EnUs), "Version two | Next |");
    assert_eq!(words("Call 555-0134.", Lang::EnUs), "Call five five five zero one three four |");
    assert_eq!(words("At 3:30.", Lang::EnUs), "At three thirty |");
    assert_eq!(words("On 10/7/2026.", Lang::EnUs), "On October seventh twenty twenty-six |");
    assert_eq!(words("Up 20%.", Lang::EnUs), "Up twenty percent |");
    // The review's two failing sentences.
    assert_eq!(words("Dr. Smith lives on Main St. in 1999.", Lang::EnUs), "doctor Smith lives on Main street in nineteen ninety-nine |");
    assert_eq!(
        words("The TDA project for NASA costs $1,234,567.", Lang::EnUs),
        "The T D A project for NASA costs one million two hundred thirty-four thousand five hundred sixty-seven dollars |"
    );
}

#[test]
fn year_rule_survives_punctuation() {
    assert_eq!(words("1999.", Lang::EnUs), "nineteen ninety-nine |");
    assert_eq!(words("1999,", Lang::EnUs), "nineteen ninety-nine ,");
    assert_eq!(words("1999!", Lang::EnUs), "nineteen ninety-nine |!");
    assert_eq!(words("(1999)", Lang::EnUs), "( nineteen ninety-nine )");
    assert_eq!(words("in 1999.", Lang::EnGb), "in nineteen ninety-nine |");
}

/// A currency amount whose digit run passes the cap must still be read digit by digit:
/// the run is consumed as a whole, not only up to the cap. The leftover used to be spoken
/// as a cardinal, so a `$` followed by 25 digits ended in "four hundred fifty-six billion".
#[test]
fn money_run_past_the_cap_is_read_digit_by_digit() {
    assert_eq!(words("$123456789012345", Lang::EnUs), "one two three four five six seven eight nine zero one two three four five");
    assert_eq!(
        words("$1234567890123456789012345", Lang::EnUs),
        "one two three four five six seven eight nine zero one two three four five six seven eight nine zero one two three four five"
    );
    // At or below the cap nothing changes: an amount still goes through the amount path.
    assert_eq!(words("$12.50", Lang::EnUs), "twelve dollars and fifty cents");
}
