//! TTML / DFXP export keeps literal `<` and `>` text that is not markup.

use filmcraft_captions::{Cue, Document, Format, WriteOptions, write};
use filmcraft_time::FrameRate;

fn export(text: &str, format: Format) -> String {
    let rate = FrameRate::FPS_25;
    let doc = Document { cues: vec![Cue { start: rate.tick_of(25), end: rate.tick_of(75), text: text.into(), ..Default::default() }], ..Default::default() };
    String::from_utf8(write(&doc, format, WriteOptions::default())).unwrap()
}

#[test]
fn literal_angle_brackets_keep_middle_text() {
    for format in [Format::Ttml, Format::Dfxp] {
        for (input, expected) in [("1 < 2 and 3 > 2", "1 &lt; 2 and 3 &gt; 2"), ("a<b and c>d", "a&lt;b and c&gt;d")] {
            let text = export(input, format);
            assert!(text.contains(expected), "{format:?}: {text}");
        }
    }
}

#[test]
fn real_tags_still_produce_markup() {
    for format in [Format::Ttml, Format::Dfxp] {
        let text = export("<b>text</b> x <y> z <i>it</i>", format);
        assert!(text.contains("<span tts:fontWeight=\"bold\">text</span>"), "{format:?}: {text}");
        assert!(text.contains("<span tts:fontStyle=\"italic\">it</span>"), "{format:?}: {text}");
        assert!(text.contains(" x  z "), "{format:?}: {text}");
    }
}

#[test]
fn self_closing_tags_are_still_stripped() {
    for format in [Format::Ttml, Format::Dfxp] {
        let text = export("a<br/>b <b/>c", format);
        assert!(text.contains(">ab c</p>"), "{format:?}: {text}");
    }
}
