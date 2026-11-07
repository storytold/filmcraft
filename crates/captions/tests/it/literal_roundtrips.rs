//! Literal text stays distinct from style markup across repeated caption exports.
use filmcraft_captions::{Cue, Document, srt, stl};
use filmcraft_project::caption::plain_text;
use filmcraft_time::Tick;

fn document(text: &str) -> Document {
    Document {
        cues: vec![Cue { start: Tick::from_seconds_f64(1.0), end: Tick::from_seconds_f64(2.0), text: text.into(), ..Default::default() }],
        ..Default::default()
    }
}

#[test]
fn srt_roundtrips_literal_brackets_and_style_tags() {
    for text in ["1 &lt; 2 & <i>x</i>", "x &lt;y> <u>z</u>", "<i>Type &lt; and &amp;lt;</i>"] {
        let expected = plain_text(text);
        let mut doc = document(text);
        for _ in 0..2 {
            let encoded = srt::write(&doc);
            doc = srt::parse(&encoded).unwrap();
            assert_eq!(doc.cues.len(), 1);
            assert_eq!(plain_text(&doc.cues[0].text), expected, "{text:?}: {encoded}");
            assert_eq!(doc.cues[0].text, text, "literal entities and genuine tags stay distinct");
        }
    }
}

#[test]
fn stl_roundtrips_literal_brackets_and_style_controls() {
    // The italic control and 110 letters put the literal '<' at the last text-field byte.
    let boundary = format!("<i>{}&lt;i></i> &amp;lt;", "x".repeat(110));
    for (text, stored) in [
        ("1 &lt; 2 & <i>x</i>", "1 &lt; 2 &amp; <i>x</i>"),
        ("x &lt;y> <u>z</u>", "x &lt;y> <u>z</u>"),
        ("<i>Type &lt; and &amp;lt;</i>", "<i>Type &lt; and &amp;lt;</i>"),
        ("<u>literal &lt;i> text</u> <i>styled</i>", "<u>literal &lt;i> text</u> <i>styled</i>"),
        (boundary.as_str(), boundary.as_str()),
    ] {
        let expected = plain_text(text);
        let mut doc = document(text);
        for _ in 0..2 {
            let encoded = stl::write(&doc, None);
            if text == boundary {
                assert!(encoded.len() > 1024 + 128, "the fixture crosses a text-field boundary");
            }
            doc = stl::parse(&encoded).unwrap();
            assert_eq!(doc.cues.len(), 1);
            assert_eq!(plain_text(&doc.cues[0].text), expected, "{text:?}: {:?}", doc.cues[0].text);
            assert_eq!(doc.cues[0].text, stored, "literal text and styled ranges remain distinct");
            assert_eq!(stl::write(&doc, None), encoded, "the next export preserves text and style-control bytes");
            for tag in ["<i>", "</i>", "<u>", "</u>"] {
                assert_eq!(doc.cues[0].text.contains(tag), text.contains(tag), "style control {tag}");
            }
        }
    }
}
