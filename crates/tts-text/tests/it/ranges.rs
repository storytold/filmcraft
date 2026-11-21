use filmcraft_tts_text::{Lang, Token, normalize};

fn spoken(text: &str) -> String {
    let words: Vec<String> = normalize(text, Lang::EnUs).unwrap().into_iter().filter_map(|t| if let Token::Word(w) = t { Some(w) } else { None }).collect();
    words.join(" ")
}

#[test]
fn range_followed_by_percent_says_percent() {
    assert_eq!(spoken("Growth of 10-20% expected"), "Growth of ten to twenty percent expected");
    assert!(!normalize("Growth of 10-20% expected", Lang::EnUs).unwrap().contains(&Token::Punct('%')));
}

#[test]
fn multi_group_range_followed_by_percent_says_percent() {
    assert_eq!(spoken("Growth of 5-10-15% expected"), "Growth of five to ten to fifteen percent expected");
}
