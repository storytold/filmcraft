use filmcraft_tts_text::{Lang, Token, normalize};

fn spoken(text: &str) -> String {
    let words: Vec<String> = normalize(text, Lang::EnUs).unwrap().into_iter().filter_map(|t| if let Token::Word(w) = t { Some(w) } else { None }).collect();
    words.join(" ")
}

#[test]
fn denominator_one_reads_over_one() {
    assert_eq!(spoken("5/1"), "five over one");
    assert_eq!(spoken("1/1"), "one over one");
}
