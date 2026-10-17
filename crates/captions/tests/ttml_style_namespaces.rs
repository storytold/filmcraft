//! Styling identity must not depend on XML attribute order or foreign extensions.

#[test]
fn recognized_styling_namespaces_have_fixed_priority() {
    let namespaces = [
        "http://www.w3.org/ns/ttml#styling",
        "http://www.w3.org/2006/10/ttaf1#style",
        "http://www.w3.org/2006/10/ttaf1#styling",
        "http://www.w3.org/2006/04/ttaf1#style",
        "http://www.w3.org/2006/04/ttaf1#styling",
    ];
    for (property, positive, reset, tag) in
        [("fontStyle", "italic", "normal", "i"), ("fontWeight", "bold", "normal", "b"), ("textDecoration", "underline", "noUnderline", "u")]
    {
        for (i, high) in namespaces.iter().enumerate() {
            for low in namespaces.iter().skip(i + 1) {
                for attrs in
                    [format!(r#"lo:{property}="{reset}" hi:{property}="{positive}""#), format!(r#"hi:{property}="{positive}" lo:{property}="{reset}""#)]
                {
                    check(
                        "",
                        &format!(r#"<div><p xmlns:hi="{high}" xmlns:lo="{low}" {attrs} begin="1s" end="2s">Hello</p></div>"#),
                        &format!("<{tag}>Hello</{tag}>"),
                    );
                }
            }
        }
    }
}

use filmcraft_captions::{Format, parse};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn check(head: &str, body: &str, expected: &str) {
    let xml = format!(
        r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xmlns:app="urn:example">{head}<body>{body}</body></tt>"#
    );
    for format in [Format::Ttml, Format::Dfxp] {
        let doc = parse(xml.as_bytes(), format).unwrap();
        assert!(doc.warnings.is_empty(), "{xml}");
        assert_eq!(doc.cues.len(), 1, "{xml}");
        assert_eq!((doc.cues[0].start, doc.cues[0].end), (Tick(TICKS_PER_SECOND), Tick(2 * TICKS_PER_SECOND)));
        assert_eq!(doc.cues[0].text, expected, "{format:?}, {xml}");
    }
}

#[test]
fn genuine_styling_wins_over_foreign_and_unqualified_properties() {
    for (property, positive, reset, tag) in
        [("fontStyle", "italic", "normal", "i"), ("fontWeight", "bold", "normal", "b"), ("textDecoration", "underline", "noUnderline", "u")]
    {
        for other in ["app:", ""] {
            for reversed in [false, true] {
                let actual = format!(r#"tts:{property}="{positive}""#);
                let collision = format!(r#"{other}{property}="{reset}""#);
                let attrs = if reversed { format!("{actual} {collision}") } else { format!("{collision} {actual}") };
                for body in [
                    format!(r#"<div {attrs}><p begin="1s" end="2s">Hello</p></div>"#),
                    format!(r#"<div><p {attrs} begin="1s" end="2s">Hello</p></div>"#),
                    format!(r#"<div><p><span {attrs} begin="1s" end="2s">Hello</span></p></div>"#),
                ] {
                    check("", &body, &format!("<{tag}>Hello</{tag}>"));
                }
                let head = format!(r#"<head><styling><style xml:id="emphasis" {attrs}/></styling></head>"#);
                check(&head, r#"<div style="emphasis"><p begin="1s" end="2s">Hello</p></div>"#, &format!("<{tag}>Hello</{tag}>"));
                let actual_reset = format!(r#"tts:{property}="{reset}""#);
                let foreign_positive = format!(r#"app:{property}="{positive}""#);
                let reset_attrs = if reversed { format!("{actual_reset} {foreign_positive}") } else { format!("{foreign_positive} {actual_reset}") };
                check("", &format!(r#"<div tts:{property}="{positive}"><p {reset_attrs} begin="1s" end="2s">Hello</p></div>"#), "Hello");
            }
        }
        check("", &format!(r#"<div><p app:{property}="{positive}" begin="1s" end="2s">Hello</p></div>"#), "Hello");
        check(
            "",
            &format!(r#"<div tts:{property}="{positive}"><p app:{property}="{reset}" begin="1s" end="2s">Hello</p></div>"#),
            &format!("<{tag}>Hello</{tag}>"),
        );
        // Include body-root selection, not only descendants.
        let xml = format!(
            r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xmlns:app="urn:example"><body app:{property}="{reset}" tts:{property}="{positive}"><div><p begin="1s" end="2s">Hello</p></div></body></tt>"#
        );
        for format in [Format::Ttml, Format::Dfxp] {
            let doc = parse(xml.as_bytes(), format).unwrap();
            assert_eq!(doc.cues.len(), 1);
            assert_eq!(doc.cues[0].text, format!("<{tag}>Hello</{tag}>"));
        }
    }
}

#[test]
fn foreign_style_references_and_definitions_are_ignored() {
    let real = r#"<style xml:id="emphasis" tts:fontStyle="italic"/>"#;
    let foreign = r#"<app:style xml:id="emphasis" tts:fontStyle="normal"/>"#;
    for definitions in [format!("{real}{foreign}"), format!("{foreign}{real}")] {
        let head = format!("<head><styling>{definitions}</styling></head>");
        for attrs in [r#"app:style="missing" style="emphasis""#, r#"style="emphasis" app:style="missing""#] {
            check(&head, &format!(r#"<div><p {attrs} begin="1s" end="2s">Hello</p></div>"#), "<i>Hello</i>");
        }
        check(&head, r#"<div><p app:style="emphasis" begin="1s" end="2s">Hello</p></div>"#, "Hello");
    }
}

#[test]
fn genuine_style_ids_win_over_foreign_and_legacy_ids() {
    for attrs in [
        r#"app:id="wrong" xml:id="emphasis""#,
        r#"xml:id="emphasis" app:id="wrong""#,
        r#"id="wrong" xml:id="emphasis""#,
        r#"xml:id="emphasis" id="wrong""#,
        r#"id="emphasis""#,
    ] {
        check(
            &format!(r#"<head><styling><style {attrs} tts:fontStyle="italic"/></styling></head>"#),
            r#"<div><p style="emphasis" begin="1s" end="2s">Hello</p></div>"#,
            "<i>Hello</i>",
        );
    }
    check(
        r#"<head><styling><style app:id="emphasis" tts:fontStyle="italic"/></styling></head>"#,
        r#"<div><p style="emphasis" begin="1s" end="2s">Hello</p></div>"#,
        "Hello",
    );
}

#[test]
fn modern_draft_and_unqualified_styling_remain_supported() {
    for (tt, tts, id) in [
        ("http://www.w3.org/ns/ttml", "http://www.w3.org/ns/ttml#styling", "xml:id"),
        ("http://www.w3.org/2006/04/ttaf1", "http://www.w3.org/2006/04/ttaf1#style", "id"),
        ("http://www.w3.org/2006/10/ttaf1", "http://www.w3.org/2006/10/ttaf1#style", "xml:id"),
        ("http://www.w3.org/2006/04/ttaf1", "http://www.w3.org/2006/04/ttaf1#styling", "id"),
        ("http://www.w3.org/2006/10/ttaf1", "http://www.w3.org/2006/10/ttaf1#styling", "xml:id"),
        ("", "", "id"),
    ] {
        let ns = if tt.is_empty() { String::new() } else { format!(r#"xmlns="{tt}" xmlns:tts="{tts}""#) };
        let prefix = if tts.is_empty() { "" } else { "tts:" };
        let xml = format!(
            r#"<tt {ns}><head><styling><style {id}="emphasis" {prefix}fontStyle="italic"/></styling></head><body style="emphasis"><div><p {prefix}fontWeight="bold" begin="1s" end="2s">Hello</p></div></body></tt>"#
        );
        for format in [Format::Ttml, Format::Dfxp] {
            let doc = parse(xml.as_bytes(), format).unwrap();
            assert_eq!(doc.cues.len(), 1);
            assert!(doc.warnings.is_empty());
            assert_eq!((doc.cues[0].start, doc.cues[0].end), (Tick(TICKS_PER_SECOND), Tick(2 * TICKS_PER_SECOND)));
            assert_eq!(doc.cues[0].text, "<i><b>Hello</b></i>", "{format:?}, {xml}");
        }
    }
}
