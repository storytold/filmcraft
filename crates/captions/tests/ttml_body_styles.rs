//! Body styles must be inherited by TTML caption paragraphs.

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
fn body_inline_styles_are_inherited_by_paragraphs() {
    for format in [Format::Ttml, Format::Dfxp] {
        for (attribute, expected) in
            [(r#"tts:fontStyle="italic""#, "<i>Hello</i>"), (r#"tts:fontWeight="bold""#, "<b>Hello</b>"), (r#"tts:textDecoration="underline""#, "<u>Hello</u>")]
        {
            let xml = format!(
                r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><body {attribute}><div><p begin="1s" end="2s">Hello</p></div></body></tt>"#
            );
            assert_eq!(caption(&xml, format), expected, "{format:?}, {attribute}");
        }
    }
}

#[test]
fn body_referenced_styles_are_inherited_by_paragraphs() {
    let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling">
<head><styling><style xml:id="emphasis" tts:fontStyle="italic" tts:fontWeight="bold"/></styling></head>
<body style="emphasis"><div><p begin="1s" end="2s">Hello</p></div></body></tt>"#;
    for format in [Format::Ttml, Format::Dfxp] {
        assert_eq!(caption(xml, format), "<i><b>Hello</b></i>");
    }
}

#[test]
fn child_spans_can_reset_inherited_and_paragraph_root_styles() {
    for format in [Format::Ttml, Format::Dfxp] {
        for (attribute, reset, tag) in [
            (r#"tts:fontStyle="italic""#, r#"tts:fontStyle="normal""#, "i"),
            (r#"tts:fontWeight="bold""#, r#"tts:fontWeight="normal""#, "b"),
            (r#"tts:textDecoration="underline""#, r#"tts:textDecoration="noUnderline""#, "u"),
        ] {
            for (body, paragraph) in [(attribute, ""), ("", attribute)] {
                let xml = format!(
                    r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><body {body}><div><p {paragraph} begin="1s" end="2s">Before<span {reset}>plain</span>After</p></div></body></tt>"#
                );
                assert_eq!(caption(&xml, format), format!("<{tag}>Before</{tag}>plain<{tag}>After</{tag}>"), "{format:?}, {body}, {paragraph}, {reset}");
            }
        }
    }
}

#[test]
fn referenced_normal_styles_override_inherited_values() {
    for format in [Format::Ttml, Format::Dfxp] {
        for (attribute, reset) in [
            (r#"tts:fontStyle="italic""#, r#"tts:fontStyle="normal""#),
            (r#"tts:fontWeight="bold""#, r#"tts:fontWeight="normal""#),
            (r#"tts:textDecoration="underline""#, r#"tts:textDecoration="noUnderline""#),
        ] {
            let xml = format!(
                r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><head><styling><style xml:id="plain" {reset}/></styling></head><body {attribute}><div><p style="plain" begin="1s" end="2s">Hello</p></div></body></tt>"#
            );
            assert_eq!(caption(&xml, format), "Hello", "{format:?}, {attribute}, {reset}");
        }
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><head><styling><style xml:id="plain" tts:fontStyle="normal"/></styling></head><body tts:fontStyle="italic" tts:fontWeight="bold"><div><p style="plain" begin="1s" end="2s">Hello</p></div></body></tt>"#;
        assert_eq!(caption(xml, format), "<b>Hello</b>", "{format:?}, unspecified bold remains inherited");
    }
}

#[test]
fn pending_spaces_keep_their_source_text_style() {
    for format in [Format::Ttml, Format::Dfxp] {
        for (body_style, text, expected) in [
            ("", r#"Before<span tts:textDecoration="underline"> world</span>After"#, "Before<u> world</u>After"),
            (r#"tts:textDecoration="underline""#, r#"Before<span tts:textDecoration="noUnderline"> world</span>After"#, "<u>Before</u> world<u>After</u>"),
            (r#"tts:textDecoration="underline""#, r#"Before <span tts:textDecoration="noUnderline">world</span>After"#, "<u>Before </u>world<u>After</u>"),
            ("", r#"Before<span tts:textDecoration="underline">world </span>After"#, "Before<u>world </u>After"),
            ("", r#"Before<span tts:textDecoration="underline"> </span><span tts:textDecoration="noUnderline"> </span>After"#, "Before<u> </u>After"),
            ("", r#"Before<span tts:textDecoration="noUnderline"> </span><span tts:textDecoration="underline"> </span>After"#, "Before After"),
            ("", r#"<span tts:textDecoration="underline"> </span><span tts:textDecoration="noUnderline"> </span>After"#, "After"),
            (r#"tts:textDecoration="underline""#, r#"Before<span style="plain"> world</span>After"#, "<u>Before</u> world<u>After</u>"),
        ] {
            for cue in [format!(r#"<p begin="1s" end="2s">{text}</p>"#), format!(r#"<p><span begin="1s" end="2s">{text}</span></p>"#)] {
                let xml = format!(
                    r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><head><styling><style xml:id="plain" tts:textDecoration="noUnderline"/></styling></head><body {body_style}><div>{cue}</div></body></tt>"#
                );
                assert_eq!(caption(&xml, format), expected, "{format:?}, {body_style}, {cue}");
            }
        }
    }
}

#[test]
fn root_styles_keep_fractional_timing_and_speaker_metadata() {
    for format in [Format::Ttml, Format::Dfxp] {
        for cue in [r#"<p begin="1.5t" end="2.5t" ttm:agent="voice">Hello</p>"#, r#"<p><span begin="1.5t" end="2.5t" ttm:agent="voice">Hello</span></p>"#] {
            let xml = format!(
                r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" ttp:tickRate="10"><head><metadata><ttm:agent xml:id="voice" type="person"><ttm:name type="full">Voice_One 日本</ttm:name></ttm:agent></metadata></head><body begin="5s" tts:fontStyle="italic"><div begin="2s">{cue}</div></body></tt>"#
            );
            let doc = parse(xml.as_bytes(), format).unwrap();
            assert!(doc.warnings.is_empty(), "{format:?}, {cue}: {:?}", doc.warnings);
            assert_eq!(doc.cues.len(), 1, "{format:?}, {cue}");
            let parsed = &doc.cues[0];
            assert_eq!((parsed.start, parsed.end), (Tick(TICKS_PER_SECOND * 7 + TICKS_PER_SECOND * 3 / 20), Tick(TICKS_PER_SECOND * 7 + TICKS_PER_SECOND / 4)));
            assert_eq!(parsed.text, "<i>Hello</i>", "{format:?}, {cue}");
            assert_eq!(parsed.speaker.as_deref(), Some("Voice_One 日本"), "{format:?}, {cue}");
        }
    }
}

#[test]
fn descendant_overrides_and_unstyled_body_keep_their_behavior() {
    for xml in [
        r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling"><body tts:fontStyle="italic"><div><p tts:fontStyle="normal" begin="1s" end="2s">Hello</p></div></body></tt>"#,
        r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s" end="2s">Hello</p></div></body></tt>"#,
    ] {
        assert_eq!(caption(xml, Format::Ttml), "Hello");
    }
}
