use filmcraft_captions::{Cue, Document, Format, WriteOptions, document_from_track, parse, track_from_document, write};
use filmcraft_project::{CaptionFormat, TrackId};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn cue() -> Cue {
    Cue { start: Tick(TICKS_PER_SECOND), end: Tick(2 * TICKS_PER_SECOND), text: "Olá".into(), ..Default::default() }
}

fn ids() -> impl FnMut() -> u64 {
    let mut next = 100;
    move || {
        next += 1;
        next
    }
}

/// TTML and DFXP exports write the caption track's language as `xml:lang` (#4025).
#[test]
fn ttml_and_dfxp_write_the_track_language() {
    let doc = Document { cues: vec![cue()], ..Default::default() };
    let mut track = track_from_document(&doc, TrackId(1), "pt", CaptionFormat::Subtitle, &mut ids());
    assert_eq!(track.language, "en", "a file with no language keeps the default");
    track.language = "pt-BR".into();
    for format in [Format::Ttml, Format::Dfxp] {
        let doc = document_from_track(&track, Tick::ZERO);
        let xml = String::from_utf8(write(&doc, format, WriteOptions::default())).unwrap();
        assert!(xml.contains(r#"xml:lang="pt-BR""#), "{format:?}: {xml}");
        // and the language comes back on import
        let back = parse(xml.as_bytes(), format).unwrap();
        assert_eq!(back.language.as_deref(), Some("pt-BR"), "{format:?}");
        assert_eq!(track_from_document(&back, TrackId(2), "pt", CaptionFormat::Subtitle, &mut ids()).language, "pt-BR");
    }
    // a track with no language (projects saved before tracks had one) still writes a valid tag
    track.language.clear();
    let xml = String::from_utf8(write(&document_from_track(&track, Tick::ZERO), Format::Ttml, WriteOptions::default())).unwrap();
    assert!(xml.contains(r#"xml:lang="en""#), "{xml}");
}
