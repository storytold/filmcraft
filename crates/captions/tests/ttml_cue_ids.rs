use std::collections::BTreeSet;

use filmcraft_captions::{Cue, Document, Format, WriteOptions, parse, write};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

const XML: &str = "http://www.w3.org/XML/1998/namespace";
const TT: &str = "http://www.w3.org/ns/ttml";

fn timeline(doc: &Document) -> Vec<(Tick, Tick, &str)> {
    doc.cues.iter().map(|cue| (cue.start, cue.end, cue.text.as_str())).collect()
}

fn distinct_vtt_ids_do_not_collide_with_document_ids(format: Format) {
    // Every WebVTT identifier is distinct. Spaces are legal in WebVTT identifiers.
    // The fourth identifier requires an XML-name fallback, which must not reuse c4.
    let vtt = br#"WEBVTT

s1
00:00:01.000 --> 00:00:02.000
Opening

r1
00:00:03.000 --> 00:00:04.000
Reply

c4
00:00:05.000 --> 00:00:06.000
Explicit identifier

display label
00:00:07.000 --> 00:00:08.000
Display identifier

keep
00:00:09.000 --> 00:00:10.000
Closing
"#;
    let doc = parse(vtt, Format::WebVtt).unwrap();
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    let input_ids: Vec<_> = doc.cues.iter().map(|cue| cue.id.as_deref().unwrap()).collect();
    assert_eq!(input_ids, ["s1", "r1", "c4", "display label", "keep"]);
    assert_eq!(input_ids.iter().collect::<BTreeSet<_>>().len(), input_ids.len(), "the source identifiers are unique");
    let expected = [
        (Tick(TICKS_PER_SECOND), Tick(2 * TICKS_PER_SECOND), "Opening"),
        (Tick(3 * TICKS_PER_SECOND), Tick(4 * TICKS_PER_SECOND), "Reply"),
        (Tick(5 * TICKS_PER_SECOND), Tick(6 * TICKS_PER_SECOND), "Explicit identifier"),
        (Tick(7 * TICKS_PER_SECOND), Tick(8 * TICKS_PER_SECOND), "Display identifier"),
        (Tick(9 * TICKS_PER_SECOND), Tick(10 * TICKS_PER_SECOND), "Closing"),
    ];
    assert_eq!(timeline(&doc), expected);

    // Check all document IDs independently of the production allocator.
    let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
    let bytes = write(&doc, format, options);
    assert_eq!(bytes, write(&doc, format, options), "stable export for {format:?}");
    let text = std::str::from_utf8(&bytes).unwrap();
    let xml = roxmltree::Document::parse(text).unwrap();
    let paragraphs: Vec<_> = xml.descendants().filter(|node| node.has_tag_name((TT, "p"))).collect();
    assert_eq!(paragraphs.len(), expected.len());
    assert!(paragraphs.iter().all(|node| node.attribute((XML, "id")).is_some_and(|id| !id.is_empty())));
    for paragraph in &paragraphs {
        assert_ncname(paragraph.attribute((XML, "id")).unwrap());
    }
    assert_eq!(paragraphs.last().unwrap().attribute((XML, "id")), Some("keep"), "an unrelated valid ID stays intact");
    assert_eq!(paragraphs[2].attribute((XML, "id")), Some("c4"), "the existing explicit collision ID stays intact");
    assert_ne!(paragraphs[3].attribute((XML, "id")), Some("display label"), "{format:?}: do not emit the space-containing display label verbatim as xml:id");
    let mut seen = BTreeSet::new();
    let repeated: Vec<_> = xml.descendants().filter_map(|node| node.attribute((XML, "id"))).filter(|id| !seen.insert(*id)).map(str::to_owned).collect();
    let back = parse(&bytes, format).unwrap();
    assert_eq!(timeline(&back), expected, "{format:?}: renaming IDs must preserve every cue and its literal timing");
    assert!(repeated.is_empty(), "{format:?}: all document xml:id values must be unique: {repeated:?}");
}

// Validate the public writer's observed ID independently of its allocator and
// without requiring ASCII: an NCName is an XML name with no colon.
fn assert_ncname(id: &str) {
    assert!(!id.contains(':'), "an xml:id must not contain a colon: {id:?}");
    let witness = format!("<{id}/>");
    let xml = roxmltree::Document::parse(&witness).expect("the generated ID must be a valid XML name");
    assert_eq!(xml.root_element().tag_name().name(), id, "the entire generated ID must be the XML name");
}

fn valid_ascii_and_unicode_cue_ids_survive_export(format: Format) {
    // U+00E9 followed by ASCII pisode_1 is a valid Unicode NCName.
    let ids = ["c4", "épisode_1"];
    for id in ids {
        assert_ncname(id);
    }
    let doc = Document {
        cues: vec![
            Cue {
                start: Tick(TICKS_PER_SECOND),
                end: Tick(2 * TICKS_PER_SECOND),
                text: "ASCII identifier".into(),
                id: Some(ids[0].into()),
                ..Default::default()
            },
            Cue {
                start: Tick(3 * TICKS_PER_SECOND),
                end: Tick(4 * TICKS_PER_SECOND),
                text: "Unicode identifier".into(),
                id: Some(ids[1].into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let expected = [
        (Tick(TICKS_PER_SECOND), Tick(2 * TICKS_PER_SECOND), "ASCII identifier"),
        (Tick(3 * TICKS_PER_SECOND), Tick(4 * TICKS_PER_SECOND), "Unicode identifier"),
    ];
    assert_eq!(timeline(&doc), expected);
    let expected_ids: Vec<_> = ids.into_iter().map(|id| Some(id.to_string())).collect();
    let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
    let bytes = write(&doc, format, options);
    assert_eq!(bytes, write(&doc, format, options), "stable export for {format:?}");
    let xml = roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
    let exported_ids: Vec<_> =
        xml.descendants().filter(|node| node.has_tag_name((TT, "p"))).map(|node| node.attribute((XML, "id")).map(str::to_string)).collect();
    let mut seen = BTreeSet::new();
    let duplicates: Vec<_> = xml.descendants().filter_map(|node| node.attribute((XML, "id"))).filter(|id| !seen.insert(*id)).map(str::to_string).collect();
    let back = parse(&bytes, format).unwrap();
    assert_eq!(timeline(&back), expected, "{format:?}: valid IDs must preserve literal cue timing and text");
    let imported_ids: Vec<_> = back.cues.iter().map(|cue| cue.id.clone()).collect();
    assert_eq!(exported_ids, expected_ids, "{format:?}: valid ASCII and Unicode IDs must survive verbatim in XML");
    assert_eq!(imported_ids, expected_ids, "{format:?}: valid ASCII and Unicode IDs must survive owned-reader import");
    assert!(duplicates.is_empty(), "{format:?}: all document xml:id values must be unique: {duplicates:?}");
}

fn a_generated_id_does_not_take_a_later_explicit_cue_id(format: Format) {
    let earlier = Cue { start: Tick(TICKS_PER_SECOND), end: Tick(2 * TICKS_PER_SECOND), text: "Earlier unlabeled cue".into(), ..Default::default() };
    assert!(earlier.id.is_none());
    let probe = Document { cues: vec![earlier.clone()], ..Default::default() };
    let expected = [
        (Tick(TICKS_PER_SECOND), Tick(2 * TICKS_PER_SECOND), "Earlier unlabeled cue"),
        (Tick(3 * TICKS_PER_SECOND), Tick(4 * TICKS_PER_SECOND), "Later explicit cue"),
    ];
    assert_eq!(timeline(&probe), expected[..1]);
    let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
    // The first ordinary export selects a collision input, not the expected
    // replacement spelling. An unsuitable generated ID is a failure, not a skip.
    let probe_bytes = write(&probe, format, options);
    let probe_xml = roxmltree::Document::parse(std::str::from_utf8(&probe_bytes).unwrap()).unwrap();
    let probe_paragraphs: Vec<_> = probe_xml.descendants().filter(|node| node.has_tag_name((TT, "p"))).collect();
    assert_eq!(probe_paragraphs.len(), 1);
    let observed = probe_paragraphs[0].attribute((XML, "id")).expect("an unlabeled cue must receive an ID").to_string();
    assert_ncname(&observed);
    let probe_back = parse(&probe_bytes, format).unwrap();
    assert_eq!(timeline(&probe_back), expected[..1], "{format:?}: the ordinary probe must preserve literal timing and text");

    let doc = Document {
        cues: vec![
            earlier.clone(),
            Cue {
                start: Tick(3 * TICKS_PER_SECOND),
                end: Tick(4 * TICKS_PER_SECOND),
                text: "Later explicit cue".into(),
                id: Some(observed.clone()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert_eq!(timeline(&doc), expected);
    assert!(doc.cues[0].id.is_none());
    assert_eq!(doc.cues[1].id.as_deref(), Some(observed.as_str()));
    let bytes = write(&doc, format, options);
    assert_eq!(bytes, write(&doc, format, options), "stable export for {format:?}");
    let xml = roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
    let paragraphs: Vec<_> = xml.descendants().filter(|node| node.has_tag_name((TT, "p"))).collect();
    assert_eq!(paragraphs.len(), expected.len());
    let exported_ids: Vec<_> = paragraphs.iter().map(|node| node.attribute((XML, "id")).expect("each cue must have an ID").to_string()).collect();
    for id in &exported_ids {
        assert_ncname(id);
    }
    let mut seen = BTreeSet::new();
    let duplicates: Vec<_> = xml.descendants().filter_map(|node| node.attribute((XML, "id"))).filter(|id| !seen.insert(*id)).map(str::to_string).collect();
    let back = parse(&bytes, format).unwrap();
    assert_eq!(timeline(&back), expected, "{format:?}: reserving the later ID must preserve every cue and its literal timing");
    assert_eq!(exported_ids[1], observed, "{format:?}: the later explicit ID must survive verbatim");
    assert_ne!(exported_ids[0], observed, "{format:?}: the earlier generated ID must not take the later explicit ID");
    assert_eq!(back.cues[1].id.as_deref(), Some(observed.as_str()), "{format:?}: the later explicit ID must survive owned-reader import");
    assert!(duplicates.is_empty(), "{format:?}: all document xml:id values must be unique: {duplicates:?}");
}

#[test]
fn ttml_distinct_vtt_ids_do_not_collide_with_document_ids() {
    distinct_vtt_ids_do_not_collide_with_document_ids(Format::Ttml);
}

#[test]
fn dfxp_distinct_vtt_ids_do_not_collide_with_document_ids() {
    distinct_vtt_ids_do_not_collide_with_document_ids(Format::Dfxp);
}

#[test]
fn ttml_valid_ascii_and_unicode_cue_ids_survive_export() {
    valid_ascii_and_unicode_cue_ids_survive_export(Format::Ttml);
}

#[test]
fn dfxp_valid_ascii_and_unicode_cue_ids_survive_export() {
    valid_ascii_and_unicode_cue_ids_survive_export(Format::Dfxp);
}

#[test]
fn ttml_generated_id_reserves_later_explicit_cue_id() {
    a_generated_id_does_not_take_a_later_explicit_cue_id(Format::Ttml);
}

#[test]
fn dfxp_generated_id_reserves_later_explicit_cue_id() {
    a_generated_id_does_not_take_a_later_explicit_cue_id(Format::Dfxp);
}

fn duplicate_parser_accepted_cue_ids_keep_first_id(format: Format) {
    // FilmCraft tolerates duplicate input IDs; WebVTT itself requires unique identifiers.
    // Keep the first usable ID and give the later cue a distinct ID when writing XML.
    let vtt = br#"WEBVTT

shared
00:00:01.000 --> 00:00:02.000
First duplicate identifier

shared
00:00:03.000 --> 00:00:04.000
Second duplicate identifier
"#;
    assert_ncname("shared");
    let doc = parse(vtt, Format::WebVtt).unwrap();
    let imported_ids: Vec<_> = doc.cues.iter().map(|cue| cue.id.as_deref()).collect();
    assert_eq!(imported_ids, [Some("shared"), Some("shared")], "both input IDs must reach the writer unchanged");
    let expected = [
        (Tick(TICKS_PER_SECOND), Tick(2 * TICKS_PER_SECOND), "First duplicate identifier"),
        (Tick(3 * TICKS_PER_SECOND), Tick(4 * TICKS_PER_SECOND), "Second duplicate identifier"),
    ];
    assert_eq!(timeline(&doc), expected);

    let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
    let bytes = write(&doc, format, options);
    assert_eq!(bytes, write(&doc, format, options), "stable export for {format:?}");
    let xml = roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
    let paragraphs: Vec<_> = xml.descendants().filter(|node| node.has_tag_name((TT, "p"))).collect();
    assert_eq!(paragraphs.len(), expected.len());
    let exported_ids: Vec<_> = paragraphs.iter().map(|node| node.attribute((XML, "id")).expect("each cue must have an ID")).collect();
    for id in &exported_ids {
        assert_ncname(id);
    }
    assert_eq!(exported_ids[0], "shared", "{format:?}: preserve the first usable explicit ID verbatim");
    assert_ne!(exported_ids[1], "shared", "{format:?}: the second explicit ID must become distinct");
    let mut seen = BTreeSet::new();
    let duplicates: Vec<_> = xml.descendants().filter_map(|node| node.attribute((XML, "id"))).filter(|id| !seen.insert(*id)).map(str::to_owned).collect();
    let back = parse(&bytes, format).unwrap();
    assert_eq!(timeline(&back), expected, "{format:?}: duplicate-ID tolerance must preserve literal cue timing and text");
    assert_eq!(back.cues[0].id.as_deref(), Some("shared"), "{format:?}: the first explicit ID must survive owned-reader import");
    assert!(duplicates.is_empty(), "{format:?}: all document xml:id values must be unique: {duplicates:?}");
}

#[test]
fn ttml_duplicate_parser_accepted_cue_ids_keep_first_id() {
    duplicate_parser_accepted_cue_ids_keep_first_id(Format::Ttml);
}

#[test]
fn dfxp_duplicate_parser_accepted_cue_ids_keep_first_id() {
    duplicate_parser_accepted_cue_ids_keep_first_id(Format::Dfxp);
}

fn decomposed_and_composed_cue_ids_stay_distinct(format: Format) {
    // XML 1.0 Fifth Edition NameChar permits U+0301 after e. Do not normalize either ID.
    let ids = ["e\u{301}_1", "é_1"];
    assert_ne!(ids[0], ids[1]);
    for id in ids {
        assert_ncname(id);
    }
    let vtt = format!(
        "WEBVTT\n\n{}\n00:00:01.000 --> 00:00:02.000\nDecomposed identifier\n\n{}\n00:00:03.000 --> 00:00:04.000\nComposed identifier\n",
        ids[0], ids[1]
    );
    let doc = parse(vtt.as_bytes(), Format::WebVtt).unwrap();
    assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    let input_ids: Vec<_> = doc.cues.iter().map(|cue| cue.id.as_deref()).collect();
    assert_eq!(input_ids, [Some(ids[0]), Some(ids[1])], "the parser must preserve both exact input IDs");
    let expected = [
        (Tick(TICKS_PER_SECOND), Tick(2 * TICKS_PER_SECOND), "Decomposed identifier"),
        (Tick(3 * TICKS_PER_SECOND), Tick(4 * TICKS_PER_SECOND), "Composed identifier"),
    ];
    assert_eq!(timeline(&doc), expected);
    let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
    let bytes = write(&doc, format, options);
    assert_eq!(bytes, write(&doc, format, options), "stable export for {format:?}");
    let xml = roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
    let exported_ids: Vec<_> = xml.descendants().filter(|node| node.has_tag_name((TT, "p"))).map(|node| node.attribute((XML, "id"))).collect();
    assert_eq!(exported_ids, input_ids, "{format:?}: preserve the decomposed and composed IDs verbatim without normalization");
    let mut seen = BTreeSet::new();
    assert!(xml.descendants().filter_map(|node| node.attribute((XML, "id"))).all(|id| seen.insert(id)), "all document xml:id values must be unique");
    let back = parse(&bytes, format).unwrap();
    assert!(back.warnings.is_empty(), "{:?}", back.warnings);
    assert_eq!(timeline(&back), expected, "{format:?}: preserve literal cue timing and text");
    let imported_ids: Vec<_> = back.cues.iter().map(|cue| cue.id.as_deref()).collect();
    assert_eq!(imported_ids, input_ids, "{format:?}: both exact IDs must survive owned-reader import");
}

fn non_xml_numeric_cue_id_uses_fallback(format: Format) {
    // Leading combining marks and colons are unchanged invalid controls.
    // U+00B2 is numeric in Rust, but is not XML 1.0 Fifth Edition NameChar.
    for id in ["\u{301}_1", "a:b", "a\u{b2}"] {
        let vtt = format!("WEBVTT\n\n{id}\n00:00:01.000 --> 00:00:02.000\nIdentifier fallback\n");
        let doc = parse(vtt.as_bytes(), Format::WebVtt).unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.cues.len(), 1);
        assert_eq!(doc.cues[0].id.as_deref(), Some(id));
        let expected = [(Tick(TICKS_PER_SECOND), Tick(2 * TICKS_PER_SECOND), "Identifier fallback")];
        assert_eq!(timeline(&doc), expected);
        let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
        let bytes = write(&doc, format, options);
        assert_eq!(bytes, write(&doc, format, options), "stable export for {format:?}: {id:?}");
        let xml = roxmltree::Document::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
        let paragraphs: Vec<_> = xml.descendants().filter(|node| node.has_tag_name((TT, "p"))).collect();
        assert_eq!(paragraphs.len(), 1);
        let output_id = paragraphs[0].attribute((XML, "id")).expect("the cue must have an ID");
        assert_ne!(output_id, id, "{format:?}: an invalid XML NCName must receive a fallback: {id:?}");
        assert_ncname(output_id);
        let mut seen = BTreeSet::new();
        assert!(xml.descendants().filter_map(|node| node.attribute((XML, "id"))).all(|id| seen.insert(id)), "all document xml:id values must be unique");
        let back = parse(&bytes, format).unwrap();
        assert!(back.warnings.is_empty(), "{:?}", back.warnings);
        assert_eq!(timeline(&back), expected, "{format:?}: fallback must preserve literal cue timing and text");
        assert_eq!(back.cues[0].id.as_deref(), Some(output_id));
    }
}

#[test]
fn ttml_decomposed_and_composed_cue_ids_stay_distinct() {
    decomposed_and_composed_cue_ids_stay_distinct(Format::Ttml);
}

#[test]
fn dfxp_decomposed_and_composed_cue_ids_stay_distinct() {
    decomposed_and_composed_cue_ids_stay_distinct(Format::Dfxp);
}

#[test]
fn ttml_non_xml_numeric_cue_id_uses_fallback() {
    non_xml_numeric_cue_id_uses_fallback(Format::Ttml);
}

#[test]
fn dfxp_non_xml_numeric_cue_id_uses_fallback() {
    non_xml_numeric_cue_id_uses_fallback(Format::Dfxp);
}
