use filmcraft_captions::{Cue, Document, Format, WriteOptions, parse, write};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

/// Control characters that XML 1.0 forbids must not make the export unreadable.
#[test]
fn ttml_export_drops_xml_illegal_control_chars() {
    for format in [Format::Ttml, Format::Dfxp] {
        let cue = Cue { start: Tick(TICKS_PER_SECOND), end: Tick(2 * TICKS_PER_SECOND), text: "a\u{1}b\u{0}c\u{B}d\u{1F}e".into(), ..Default::default() };
        let doc = Document { cues: vec![cue], ..Default::default() };
        let xml = String::from_utf8(write(&doc, format, WriteOptions::default())).unwrap();
        let back = parse(xml.as_bytes(), format);
        assert!(back.is_ok(), "{format:?}: {xml}");
        assert_eq!(back.unwrap().cues[0].text, "abcde", "{format:?}");
    }
}
