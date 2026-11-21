use filmcraft_tts_text::{Lang, Token, normalize};

#[test]
fn dotted_initialism_has_no_inner_sentence_end() {
    let tokens = normalize("The U.S.A. economy grew", Lang::EnUs).unwrap();
    let expected: Vec<Token> = ["The", "U", "S", "A", "economy", "grew"].iter().map(|w| Token::Word((*w).into())).collect();
    assert_eq!(tokens, expected);
}

#[test]
fn dotted_initialism_at_end_still_ends_sentence() {
    let tokens = normalize("I saw the U.S.A.", Lang::EnUs).unwrap();
    assert_eq!(tokens.last(), Some(&Token::SentenceEnd));
    assert_eq!(tokens.iter().filter(|t| matches!(t, Token::SentenceEnd)).count(), 1);
}
