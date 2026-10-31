use std::collections::{HashMap, HashSet};

use filmcraft_captions::{Cue, Document, Format, WriteOptions, parse, write};
use filmcraft_time::FrameRate;

const TT: &str = "http://www.w3.org/ns/ttml";
const META: &str = "http://www.w3.org/ns/ttml#metadata";
const XML: &str = "http://www.w3.org/XML/1998/namespace";

#[test]
fn imports_speaker_names_from_significant_ttml_metadata() {
    for format in [Format::Ttml, Format::Dfxp] {
        for metadata in [
            r#"<ttm:agent xml:id="speaker1" type="person"><ttm:name type="alias">Ann</ttm:name><ttm:name type="full">Ann Lee</ttm:name></ttm:agent>"#
                .to_string(),
            r#"<metadata><ttm:agent xml:id="speaker1" type="person"><ttm:name type="full">Ann Lee</ttm:name></ttm:agent></metadata>"#.to_string(),
            r#"<ttm:agent xml:id="speaker1" type="person"><ttm:name type="full">Ann<!-- editorial note --> Lee</ttm:name></ttm:agent>"#.to_string(),
        ] {
            let xml = format!(
                r#"<tt xmlns="{TT}" xmlns:ttm="{META}"><head>{metadata}</head><body><div><p begin="1s" end="2s" ttm:agent="speaker1">Hello</p></div></body></tt>"#
            );
            let doc = parse(xml.as_bytes(), format).unwrap();
            assert_eq!(doc.cues.len(), 1);
            assert_eq!(doc.cues[0].speaker.as_deref(), Some("Ann Lee"));
            assert_eq!(doc.cues[0].text, "Hello");
        }
    }
}

#[test]
fn exported_speakers_have_unique_resolvable_ids_and_exact_names() {
    let rate = FrameRate::FPS_25;
    let speakers =
        ["Ann Lee", "Ann_Lee", "O’Connor & <Guest>", "_Ann", "山田 花子", "Ann Lee", "", "  Ann Lee  ", "Ann\rLee", "Ann\r\nLee", "Ann\nLee", "Ann\tLee"];
    let ids = ["agent1", "agent2", "speaker1", "speaker2", "c5", "c6", "c7", "c8", "c9", "c10", "c11", "c12"];
    let doc = Document {
        cues: speakers
            .iter()
            .enumerate()
            .map(|(i, name)| Cue {
                start: rate.tick_of(i as i64 * 50),
                end: rate.tick_of(i as i64 * 50 + 25),
                text: format!("Line {}", i + 1),
                speaker: Some((*name).into()),
                id: Some(ids[i].into()),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    for format in [Format::Ttml, Format::Dfxp] {
        let bytes = write(&doc, format, WriteOptions { rate: Some(rate), ..Default::default() });
        assert_eq!(bytes, write(&doc, format, WriteOptions { rate: Some(rate), ..Default::default() }), "stable output");
        let text = std::str::from_utf8(&bytes).unwrap();
        let xml = roxmltree::Document::parse(text).unwrap();
        let mut all_ids = HashSet::new();
        for id in xml.descendants().filter_map(|n| n.attribute((XML, "id"))) {
            assert!(all_ids.insert(id), "duplicate xml:id {id}");
        }
        let mut agents = HashMap::new();
        for agent in xml.descendants().filter(|n| n.has_tag_name((META, "agent"))) {
            assert_eq!(agent.attribute("type"), Some("other"));
            let name = agent.children().find(|n| n.has_tag_name((META, "name"))).unwrap();
            assert_eq!(name.attribute("type"), Some("full"));
            assert_eq!(name.attribute((XML, "space")), Some("preserve"));
            agents.insert(agent.attribute((XML, "id")).unwrap(), name.text().unwrap_or(""));
        }
        assert_eq!(agents.len(), 11, "each exact name is defined once");
        let paragraphs: Vec<_> = xml.descendants().filter(|n| n.has_tag_name((TT, "p"))).collect();
        assert_eq!(paragraphs.len(), speakers.len());
        for (paragraph, expected) in paragraphs.iter().zip(speakers) {
            let id = paragraph.attribute((META, "agent")).unwrap();
            assert_eq!(agents.get(id).copied(), Some(expected), "reference {id}");
        }
        let back = parse(&bytes, format).unwrap();
        assert_eq!(back.cues, doc.cues, "{format:?} names and cue content survive");
    }
}

#[test]
fn imports_multiple_agent_names_in_reference_order() {
    let xml = format!(
        r#"<tt xmlns="{TT}" xmlns:ttm="{META}"><head>
        <ttm:agent xml:id="ann" type="person"><ttm:name type="full">Ann Lee</ttm:name></ttm:agent>
        <ttm:agent xml:id="guest" type="person"><ttm:name type="alias">O’Connor</ttm:name></ttm:agent>
        </head><body><div><p begin="1s" end="2s" ttm:agent="guest ann">Together</p></div></body></tt>"#
    );
    let doc = parse(xml.as_bytes(), Format::Ttml).unwrap();
    assert_eq!(doc.cues[0].speaker.as_deref(), Some("O’Connor, Ann Lee"));
}

#[test]
fn keeps_legacy_filmcraft_speaker_references_readable() {
    let xml = format!(
        r#"<tt xmlns="{TT}" xmlns:ttm="{META}"><body><div>
        <p begin="1s" end="2s" ttm:agent="Ann_Lee">Hello</p>
        </div></body></tt>"#
    );
    let doc = parse(xml.as_bytes(), Format::Ttml).unwrap();
    assert_eq!(doc.cues[0].speaker.as_deref(), Some("Ann Lee"));
}

#[test]
fn timed_span_can_name_its_own_speaker() {
    let xml = format!(
        r#"<tt xmlns="{TT}" xmlns:ttm="{META}"><head>
        <ttm:agent xml:id="ann" type="person"><ttm:name type="full">Ann Lee</ttm:name></ttm:agent>
        <ttm:agent xml:id="guest" type="person"><ttm:name type="full">Guest</ttm:name></ttm:agent>
        </head><body><div><p ttm:agent="ann"><span begin="1s" end="2s" ttm:agent="guest">Hello</span>
        <span begin="3s" end="4s">Welcome</span></p></div></body></tt>"#
    );
    let doc = parse(xml.as_bytes(), Format::Ttml).unwrap();
    assert_eq!(doc.cues.len(), 2);
    assert_eq!(doc.cues[0].speaker.as_deref(), Some("Guest"));
    assert_eq!(doc.cues[1].speaker.as_deref(), Some("Ann Lee"));
}

#[test]
fn foreign_metadata_does_not_replace_ttml_speakers() {
    let real = r#"<ttm:agent xml:id="speaker1" type="person"><ttm:name type="full">Ann Lee</ttm:name></ttm:agent>"#;
    for (metadata, attributes) in [
        (
            format!(r#"{real}<metadata><app:agent id="speaker1"><app:name type="full">Library editor</app:name></app:agent></metadata>"#),
            r#"ttm:agent="speaker1""#,
        ),
        (real.into(), r#"app:agent="library" ttm:agent="speaker1""#),
    ] {
        let xml = format!(
            r#"<tt xmlns="{TT}" xmlns:ttm="{META}" xmlns:app="urn:example:caption-library"><head>{metadata}</head><body><div><p begin="1s" end="2s" {attributes}>Hello</p></div></body></tt>"#
        );
        let doc = parse(xml.as_bytes(), Format::Ttml).unwrap();
        assert_eq!(doc.cues[0].speaker.as_deref(), Some("Ann Lee"), "{xml}");
    }
    // A TTML agent nested inside another application's metadata container is not significant.
    let xml = format!(
        r#"<tt xmlns="{TT}" xmlns:ttm="{META}" xmlns:app="urn:example:caption-library"><head><app:metadata>{real}</app:metadata></head><body><div><p begin="1s" end="2s" ttm:agent="speaker1">Hello</p></div></body></tt>"#
    );
    let doc = parse(xml.as_bytes(), Format::Ttml).unwrap();
    assert_eq!(doc.cues[0].speaker.as_deref(), Some("speaker1"));
}

#[test]
fn imports_draft_metadata_and_legacy_unqualified_speaker_references() {
    for (tt, metadata, attributes) in
        [("http://www.w3.org/2006/10/ttaf1", "http://www.w3.org/2006/10/ttaf1#metadata", r#"ttm:agent="speaker1""#), (TT, META, r#"agent="speaker1""#)]
    {
        let xml = format!(
            r#"<tt xmlns="{tt}" xmlns:ttm="{metadata}"><head><ttm:agent xml:id="speaker1" type="person"><ttm:name type="full">Ann Lee</ttm:name></ttm:agent></head><body><div><p begin="1s" end="2s" {attributes}>Hello</p></div></body></tt>"#
        );
        let doc = parse(xml.as_bytes(), Format::Ttml).unwrap();
        assert_eq!(doc.cues[0].speaker.as_deref(), Some("Ann Lee"));
    }
}

#[test]
fn keeps_april_2006_draft_speaker_references_readable() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xml:lang="en" xmlns="http://www.w3.org/2006/04/ttaf1" xmlns:ttm="http://www.w3.org/2006/04/ttaf1#metadata">
  <head><metadata>
    <ttm:agent id="Ann_Lee" type="person"><ttm:name type="full">Ann Lee</ttm:name></ttm:agent>
  </metadata></head>
  <body><div><p begin="1s" end="2s" ttm:agent="Ann_Lee">Hello</p></div></body>
</tt>"#;
    for format in [Format::Ttml, Format::Dfxp] {
        let doc = parse(xml.as_bytes(), format).unwrap();
        assert_eq!(doc.cues.len(), 1);
        assert_eq!(doc.cues[0].text, "Hello");
        assert_eq!(doc.cues[0].start, FrameRate::FPS_25.tick_of(25));
        assert_eq!(doc.cues[0].end, FrameRate::FPS_25.tick_of(50));
        assert_eq!(doc.cues[0].speaker.as_deref(), Some("Ann Lee"), "the documented draft keeps its previously readable speaker");
    }
}

#[test]
fn reads_april_2006_agent_definitions_with_unqualified_ids() {
    let xml = r#"<tt xml:lang="en" xmlns="http://www.w3.org/2006/04/ttaf1" xmlns:ttm="http://www.w3.org/2006/04/ttaf1#metadata">
        <head><ttm:agent id="speaker1" type="person"><ttm:name type="full">Ann-Marie Lee</ttm:name></ttm:agent></head>
        <body><div><p begin="1s" end="2s" ttm:agent="speaker1">Welcome</p></div></body></tt>"#;
    for format in [Format::Ttml, Format::Dfxp] {
        let doc = parse(xml.as_bytes(), format).unwrap();
        assert_eq!(doc.cues.len(), 1);
        assert_eq!(doc.cues[0].text, "Welcome");
        assert_eq!(doc.cues[0].speaker.as_deref(), Some("Ann-Marie Lee"), "the name resolves through the draft's unqualified id");
    }
}

fn assert_full_speaker_name_with_type(attributes: &str) {
    let xml = format!(
        r#"<tt xml:lang="en" xmlns="{TT}" xmlns:ttm="{META}" xmlns:app="urn:example:caption-library">
        <head><ttm:agent xml:id="speaker1" type="person">
        <ttm:name type="alias">Ann</ttm:name><ttm:name {attributes}>Ann Lee</ttm:name>
        </ttm:agent></head><body><div><p begin="1s" end="2s" ttm:agent="speaker1">Hello</p></div></body></tt>"#
    );
    let mut speakers = Vec::new();
    for format in [Format::Ttml, Format::Dfxp] {
        let doc = parse(xml.as_bytes(), format).unwrap();
        assert_eq!(doc.cues.len(), 1);
        assert_eq!(doc.cues[0].text, "Hello");
        assert_eq!(doc.cues[0].start, FrameRate::FPS_25.tick_of(25));
        assert_eq!(doc.cues[0].end, FrameRate::FPS_25.tick_of(50));
        speakers.push(doc.cues[0].speaker.clone());
    }
    assert_eq!(speakers, [Some("Ann Lee".into()), Some("Ann Lee".into())], "TTML and DFXP prefer the full name: {attributes}");
}

#[test]
fn name_type_without_application_attribute_prefers_full_name() {
    assert_full_speaker_name_with_type(r#"type="full""#);
}

#[test]
fn name_type_after_application_attribute_prefers_full_name() {
    assert_full_speaker_name_with_type(r#"app:type="contact" type="full""#);
}

#[test]
fn name_type_before_application_attribute_prefers_full_name() {
    assert_full_speaker_name_with_type(r#"type="full" app:type="contact""#);
}
