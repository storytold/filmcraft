//! Root styles preserve literal comparisons without changing caption spelling.
use filmcraft_captions::{Cue, Document, Format, WriteOptions, document_from_track, parse, track_from_document, write};
use filmcraft_project::{CaptionFormat, TrackId, caption::plain_text};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};

#[test]
fn legacy_padded_style_controls_keep_display_and_ttml_conversion() {
    for tag in ["i", "b", "u", "I", "B", "U"] {
        for (open, close) in [
            (format!("< {tag}>"), format!("</ {tag}>")),
            (format!("< {tag} >"), format!("< / {tag} >")),
            (format!("< {tag} title='legacy'>"), format!("</ {tag} >")),
        ] {
            let text = format!("{open}Hello{close}");
            assert_eq!(plain_text(&text), "Hello");
            let doc = Document {
                cues: vec![Cue { start: Tick(TICKS_PER_SECOND), end: Tick(2 * TICKS_PER_SECOND), text: text.clone(), ..Default::default() }],
                ..Default::default()
            };
            let tag = tag.to_ascii_lowercase();
            let expected = format!("<{tag}>Hello</{tag}>");
            let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
            for format in [Format::Ttml, Format::Dfxp] {
                let bytes = write(&doc, format, options);
                let back = parse(&bytes, format).unwrap();
                assert_eq!(back.cues.len(), 1);
                assert_eq!(back.cues[0].text, expected);
                assert_eq!((back.cues[0].start, back.cues[0].end), (doc.cues[0].start, doc.cues[0].end));
                assert_eq!(write(&back, format, options), bytes);
            }
            let srt = write(&doc, Format::Srt, options);
            assert_eq!(parse(&srt, Format::Srt).unwrap().cues[0].text, text);
        }
    }
}

#[test]
fn literal_comparisons_survive_styled_roots_and_export() {
    for format in [Format::Ttml, Format::Dfxp] {
        for (xml_text, visible) in
            [("1 &lt; 2", "1 < 2"), ("2 &lt; 3 &gt; 1", "2 < 3 > 1"), ("x &lt; y &gt; z", "x < y > z"), ("5 &lt;7&gt; 6", "5 <7> 6"), ("A &amp; B", "A & B")]
        {
            for (body, cue, styled) in [
                (r#"tts:fontStyle="italic""#, format!(r#"<p begin="1s" end="2s">{xml_text}</p>"#), true),
                ("", format!(r#"<p tts:fontStyle="italic" begin="1s" end="2s">{xml_text}</p>"#), true),
                ("", format!(r#"<p><span tts:fontStyle="italic" begin="1s" end="2s">{xml_text}</span></p>"#), true),
                ("", format!(r#"<p begin="1s" end="2s">{xml_text}</p>"#), false),
            ] {
                let xml = format!(
                    r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xml:lang="en"><body {body}><div>{cue}</div></body></tt>"#
                );
                let doc = parse(xml.as_bytes(), format).unwrap();
                assert!(doc.warnings.is_empty());
                assert_eq!(doc.cues.len(), 1);
                let parsed = &doc.cues[0];
                assert_eq!((parsed.start, parsed.end), (Tick(TICKS_PER_SECOND), Tick(2 * TICKS_PER_SECOND)));
                let expected = if styled { format!("<i>{visible}</i>") } else { visible.into() };
                assert_eq!(parsed.text, expected);
                assert_eq!(plain_text(&parsed.text), visible);
                let track = track_from_document(&doc, TrackId(1), "Comparisons", CaptionFormat::Subtitle, &mut || 2);
                assert_eq!(track.captions[0].plain_lines().join("\n"), visible);
                let exported = document_from_track(&track, Tick::ZERO);
                for output in [Format::Ttml, Format::Dfxp, Format::Stl, Format::Srt] {
                    let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
                    let bytes = write(&exported, output, options);
                    let back = parse(&bytes, output).unwrap();
                    assert_eq!(back.cues.len(), 1);
                    assert_eq!(back.cues[0].text, expected, "{format:?} -> {output:?}, {cue}");
                    assert_eq!(plain_text(&back.cues[0].text), visible);
                    assert_eq!((back.cues[0].start, back.cues[0].end), (parsed.start, parsed.end));
                    assert_eq!(write(&back, output, options), bytes);
                    if output == Format::Srt {
                        assert!(std::str::from_utf8(&bytes).unwrap().contains(&expected));
                    }
                }
            }
        }
    }
}

#[test]
fn raw_model_comparisons_and_ampersands_keep_exact_spelling() {
    for text in ["A & B", "2 < 3 > 1", "x < y > z", "5 <7> 6"] {
        let doc = Document {
            cues: vec![Cue { start: Tick(TICKS_PER_SECOND), end: Tick(2 * TICKS_PER_SECOND), text: text.into(), ..Default::default() }],
            ..Default::default()
        };
        for format in [Format::Ttml, Format::Dfxp, Format::Stl, Format::Srt] {
            let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
            let bytes = write(&doc, format, options);
            let back = parse(&bytes, format).unwrap();
            assert_eq!(back.cues.len(), 1);
            assert_eq!(back.cues[0].text, text);
            assert_eq!(plain_text(&back.cues[0].text), text);
            assert_eq!((back.cues[0].start, back.cues[0].end), (doc.cues[0].start, doc.cues[0].end));
            assert_eq!(write(&back, format, options), bytes);
        }
    }
}

#[test]
fn non_xml_spaces_remain_literal_text_at_cue_edges() {
    for space in ['\u{a0}', '\u{202f}', '\u{3000}'] {
        let visible = format!("{space}A{space}B{space}");
        for format in [Format::Ttml, Format::Dfxp] {
            for attribute in ["", r#"tts:fontStyle="italic""#] {
                let xml = format!(
                    r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:tts="http://www.w3.org/ns/ttml#styling" xml:lang="en"><body><div><p {attribute} begin="1s" end="2s">{visible}</p></div></body></tt>"#
                );
                let doc = parse(xml.as_bytes(), format).unwrap();
                assert_eq!(doc.cues.len(), 1);
                assert!(doc.warnings.is_empty());
                assert_eq!(plain_text(&doc.cues[0].text), visible);
                let track = track_from_document(&doc, TrackId(1), "Spaces", CaptionFormat::Subtitle, &mut || 2);
                assert_eq!(track.captions[0].plain_lines().join("\n"), visible);
                let bytes = write(&doc, format, WriteOptions::default());
                let back = parse(&bytes, format).unwrap();
                assert_eq!(plain_text(&back.cues[0].text), visible);
                assert_eq!(write(&back, format, WriteOptions::default()), bytes);
            }
        }
    }
}

#[test]
fn webvtt_timestamps_remain_markup_during_plain_text_and_export() {
    let text = "<i><00:01.234>Hi<00:00:05.678> there</i>";
    assert_eq!(plain_text(text), "Hi there");
    let doc = Document {
        cues: vec![Cue { start: Tick(TICKS_PER_SECOND), end: Tick(8 * TICKS_PER_SECOND), text: text.into(), ..Default::default() }],
        ..Default::default()
    };
    for format in [Format::Ttml, Format::Dfxp] {
        let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
        let bytes = write(&doc, format, options);
        let back = parse(&bytes, format).unwrap();
        assert_eq!(back.cues.len(), 1);
        assert_eq!(back.cues[0].text, "<i>Hi there</i>");
        assert_eq!(write(&back, format, options), bytes);
    }
}

#[test]
fn stl_keeps_its_literal_declaration_and_timestamp_spelling() {
    for text in ["A <!DOCTYPE x> B", "A <?render?> B", "A <!--note--> B", "<i><00:01.234>Hi<00:00:05.678> there</i>"] {
        let doc = Document {
            cues: vec![Cue { start: Tick(TICKS_PER_SECOND), end: Tick(8 * TICKS_PER_SECOND), text: text.into(), ..Default::default() }],
            ..Default::default()
        };
        let options = WriteOptions { rate: Some(FrameRate::FPS_25), ..Default::default() };
        let bytes = write(&doc, Format::Stl, options);
        let back = parse(&bytes, Format::Stl).unwrap();
        assert_eq!(back.cues.len(), 1);
        assert_eq!(back.cues[0].text, text);
        assert_eq!((back.cues[0].start, back.cues[0].end), (doc.cues[0].start, doc.cues[0].end));
        assert_eq!(write(&back, Format::Stl, options), bytes);
    }
}
