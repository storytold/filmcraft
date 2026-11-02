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
}
