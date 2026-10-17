//! Timed spans emitted as cues must keep their own resolved text styles.

use filmcraft_captions::{Format, parse};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn caption(xml: &str, format: Format) -> String {
    let doc = parse(xml.as_bytes(), format).unwrap();
    assert_eq!(doc.cues.len(), 1);
    assert_eq!((doc.cues[0].start, doc.cues[0].end), (Tick(TICKS_PER_SECOND), Tick(TICKS_PER_SECOND * 2)));
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    doc.cues[0].text.clone()
}

#[test]
fn timed_span_root_styles_match_timed_paragraph_styles() {
    for format in [Format::Ttml, Format::Dfxp] {
        for (attribute, expected) in
            [(r#"tts:fontStyle="italic""#, "<i>Hello</i>"), (r#"tts:fontWeight="bold""#, "<b>Hello</b>"), (r#"tts:textDecoration="underline""#, "<u>Hello</u>")]
        {
            let head = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><body><div>"#;
            let paragraph = format!(r#"{head}<p {attribute} begin="1s" end="2s">Hello</p></div></body></tt>"#);
            let span = format!(r#"{head}<p><span {attribute} begin="1s" end="2s">Hello</span></p></div></body></tt>"#);
            assert_eq!(caption(&span, format), expected, "{format:?}, timed span, {attribute}");
            assert_eq!(caption(&paragraph, format), expected, "{format:?}, paragraph, {attribute}");
        }
    }
}

#[test]
fn timed_span_referenced_and_nested_styles_are_kept() {
    let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling">
<head><styling><style xml:id="emphasis" tts:fontStyle="italic"/></styling></head>
<body><div><p><span style="emphasis" begin="1s" end="2s">Hello <span tts:fontWeight="bold">world</span></span></p></div></body></tt>"#;
    for format in [Format::Ttml, Format::Dfxp] {
        assert_eq!(caption(xml, format), "<i>Hello <b>world</b></i>");
    }
}

#[test]
fn body_and_paragraph_styles_are_kept_on_timed_spans() {
    let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><body tts:fontStyle="italic"><div><p tts:fontWeight="bold"><span tts:textDecoration="underline" begin="1s" end="2s">Hello</span></p></div></body></tt>"#;
    for format in [Format::Ttml, Format::Dfxp] {
        assert_eq!(caption(xml, format), "<i><b><u>Hello</u></b></i>");
    }
}

#[test]
fn empty_styled_timed_spans_remain_empty() {
    let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><body><div><p><span tts:fontStyle="italic" begin="1s" end="2s"> <br/> </span></p></div></body></tt>"#;
    for format in [Format::Ttml, Format::Dfxp] {
        let doc = parse(xml.as_bytes(), format).unwrap();
        assert!(doc.cues.is_empty());
        assert!(doc.warnings.is_empty());
    }
}

#[test]
fn timed_span_children_can_reset_root_styles() {
    for format in [Format::Ttml, Format::Dfxp] {
        for (attribute, reset, tag) in [
            (r#"tts:fontStyle="italic""#, r#"tts:fontStyle="normal""#, "i"),
            (r#"tts:fontWeight="bold""#, r#"tts:fontWeight="normal""#, "b"),
            (r#"tts:textDecoration="underline""#, r#"tts:textDecoration="noUnderline""#, "u"),
        ] {
            let xml = format!(
                r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><body><div><p><span {attribute} begin="1s" end="2s">Before<span {reset}>plain</span>After</span></p></div></body></tt>"#
            );
            assert_eq!(caption(&xml, format), format!("<{tag}>Before</{tag}>plain<{tag}>After</{tag}>"), "{format:?}, {attribute}, {reset}");
            let xml = format!(
                r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><body><div><p><span {attribute} begin="1s" end="2s"><span {reset}>Hello</span></span></p></div></body></tt>"#
            );
            assert_eq!(caption(&xml, format), "Hello", "{format:?}, {attribute}, {reset}");
        }
    }
}

#[test]
fn styled_roots_preserve_break_cleanup_and_interior_lines() {
    for format in [Format::Ttml, Format::Dfxp] {
        for (text, expected) in [
            ("<br/>Hello<br/>", "<i>Hello</i>"),
            ("Hello<br/><br/>world<br/>", "<i>Hello\n\nworld</i>"),
            (
                r#"<span tts:fontStyle="normal"><span tts:fontStyle="italic">Hello</span><br/><span tts:fontWeight="bold">world</span></span>"#,
                "<i>Hello\n</i><b>world</b>",
            ),
            ("<br/> <br/>Hello<br/> <br/>", "<i>Hello</i>"),
            (r#"Hello<span tts:fontWeight="bold">world<br/></span>"#, "<i>Hello<b>world</b></i>"),
        ] {
            for cue in [
                format!(r#"<p tts:fontStyle="italic" begin="1s" end="2s">{text}</p>"#),
                format!(r#"<p><span tts:fontStyle="italic" begin="1s" end="2s">{text}</span></p>"#),
            ] {
                let xml = format!(r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><body><div>{cue}</div></body></tt>"#);
                assert_eq!(caption(&xml, format), expected, "{format:?}, {cue}");
            }
        }
    }
}

#[test]
fn unstyled_timed_span_remains_plain() {
    let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p><span begin="1s" end="2s">Hello</span></p></div></body></tt>"#;
    assert_eq!(caption(xml, Format::Ttml), "Hello");
}
