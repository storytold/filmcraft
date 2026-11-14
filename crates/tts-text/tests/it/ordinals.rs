use filmcraft_tts_text::{Lang, Token, normalize};

fn words(text: &str) -> Vec<String> {
    normalize(text, Lang::EnUs).unwrap().into_iter().filter_map(|t| if let Token::Word(w) = t { Some(w) } else { None }).collect()
}

#[test]
fn multi_word_ordinals_are_one_token_per_word() {
    assert_eq!(words("112th"), ["one", "hundred", "twelfth"]);
    assert_eq!(words("101st"), ["one", "hundred", "first"]);
    assert_eq!(words("100th"), ["one", "hundredth"]);
    assert_eq!(words("1000th"), ["one", "thousandth"]);
    assert_eq!(words("21st"), ["twenty-first"]);
}
