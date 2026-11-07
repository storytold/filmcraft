//! TTML / DFXP inherited timing: `begin`, `end` and `dur` on `body` and `div` time containers
//! offset and clip the paragraphs inside them (#895).

use filmcraft_captions::{Format, WriteOptions, parse, write};

/// (start, end, text) of every cue, in seconds; panics on a parse error.
fn cues(xml: &str, format: Format) -> Vec<(f64, f64, String)> {
    let doc = parse(xml.as_bytes(), format).unwrap();
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    doc.cues.iter().map(|c| (c.start.seconds(), c.end.seconds(), c.text.clone())).collect()
}

fn both(xml: &str, want: &[(f64, f64, &str)]) {
    let want: Vec<(f64, f64, String)> = want.iter().map(|(s, e, t)| (*s, *e, t.to_string())).collect();
    for format in [Format::Ttml, Format::Dfxp] {
        assert_eq!(cues(xml, format), want, "{format:?}: {xml}");
    }
}

#[test]
fn body_begin_is_inherited() {
    both(r#"<tt xmlns="http://www.w3.org/ns/ttml"><body begin="5s"><div><p begin="1s" end="2s">x</p></div></body></tt>"#, &[(6.0, 7.0, "x")]);
}

#[test]
fn div_begin_is_inherited() {
    both(r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div begin="5s"><p begin="1s" end="2s">x</p></div></body></tt>"#, &[(6.0, 7.0, "x")]);
}

#[test]
fn untimed_containers_leave_times_unchanged() {
    both(r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s" end="2s">x</p></div></body></tt>"#, &[(1.0, 2.0, "x")]);
}

#[test]
fn nested_begins_add_up() {
    both(
        r#"<tt xmlns="http://www.w3.org/ns/ttml"><body begin="5s"><div begin="00:00:10.000"><div begin="1s"><p begin="1s" end="2s">deep</p></div><p begin="0s" dur="1s">mid</p></div><p begin="1s" end="2s">top</p></body></tt>"#,
        &[(6.0, 7.0, "top"), (15.0, 16.0, "mid"), (17.0, 18.0, "deep")],
    );
}

#[test]
fn body_begin_offsets_timed_spans() {
    both(r#"<tt xmlns="http://www.w3.org/ns/ttml"><body begin="5s"><div><p><span begin="1s" end="2s">x</span></p></div></body></tt>"#, &[(6.0, 7.0, "x")]);
}

#[test]
fn container_ends_clip_descendants() {
    // div active 11 s to 13 s: a later paragraph is dropped, a longer one is cut at 13 s
    both(
        r#"<tt xmlns="http://www.w3.org/ns/ttml"><body begin="1s"><div begin="10s" end="12s"><p begin="1s" end="5s">cut</p><p begin="3s" end="4s">gone</p></div></body></tt>"#,
        &[(12.0, 13.0, "cut")],
    );
    both(r#"<tt xmlns="http://www.w3.org/ns/ttml"><body dur="3s"><div><p begin="1s" end="5s">x</p></div></body></tt>"#, &[(1.0, 3.0, "x")]);
}

#[test]
fn paragraphs_without_an_end_last_until_their_container_ends() {
    both(
        r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div begin="2s" end="6s"><p begin="1s">open</p></div><div begin="10s" dur="2s"><p>whole</p></div></body></tt>"#,
        &[(3.0, 6.0, "open"), (10.0, 12.0, "whole")],
    );
}

#[test]
fn end_and_dur_take_the_earlier() {
    both(r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s" end="5s" dur="2s">x</p></div></body></tt>"#, &[(1.0, 3.0, "x")]);
}

#[test]
fn seq_containers_warn() {
    let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter"><body><div timeContainer="seq"><p begin="1s" end="2s">x</p></div></body></tt>"#;
    for format in [Format::Ttml, Format::Dfxp] {
        let doc = parse(xml.as_bytes(), format).unwrap();
        assert_eq!(doc.cues.len(), 1);
        assert!(doc.warnings.iter().any(|w| w.contains("seq")), "{:?}", doc.warnings);
    }
}

#[test]
fn inherited_times_survive_export() {
    // the writer uses untimed body / div and absolute paragraph times, so the resolved times stay
    let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body begin="5s"><div begin="1s" end="4s"><p begin="1s" end="9s">x</p></div></body></tt>"#;
    for format in [Format::Ttml, Format::Dfxp] {
        let doc = parse(xml.as_bytes(), format).unwrap();
        let back = parse(&write(&doc, format, WriteOptions::default()), format).unwrap();
        let times = |d: &filmcraft_captions::Document| d.cues.iter().map(|c| (c.start, c.end, c.text.clone())).collect::<Vec<_>>();
        assert_eq!(times(&back), times(&doc), "{format:?}");
        assert_eq!((back.cues[0].start.seconds(), back.cues[0].end.seconds()), (7.0, 9.0), "{format:?}");
    }
}
