use filmcraft_tts_text::{Lang, Token, normalize};

fn spoken(text: &str) -> String {
    let words: Vec<String> = normalize(text, Lang::EnUs).unwrap().into_iter().filter_map(|t| if let Token::Word(w) = t { Some(w) } else { None }).collect();
    words.join(" ")
}

#[test]
fn bare_year_is_unchanged() {
    assert_eq!(spoken("in 1990"), "in nineteen ninety");
}

#[test]
fn decade_reads_as_plural_year() {
    assert_eq!(spoken("the 1990s"), "the nineteen nineties");
    assert_eq!(spoken("the 2010s."), "the twenty tens");
    assert_eq!(spoken("the 1900s"), "the nineteen hundreds");
    assert_eq!(spoken("the 1100s"), "the eleven hundreds");
    assert_eq!(spoken("the 2090s"), "the twenty nineties");
}

#[test]
fn only_round_years_followed_by_a_lone_s_are_decades() {
    assert_ne!(spoken("the 1995s"), "the nineteen ninety fives");
    assert!(!spoken("the 1990sx").contains("nineties"));
    assert!(!spoken("the 3000s").contains("thousands"));
}
