//! Round-trip property tests: write(parse(write(doc))) is stable and parse(write(doc)) == doc for
//! documents each format can represent exactly.

use filmcraft_captions::{Cue, Document, Format, WriteOptions, parse, scc, write};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use proptest::prelude::*;

const MS: i64 = TICKS_PER_SECOND / 1000;

/// A text line: printable words without markup-significant sequences.
fn line() -> impl Strategy<Value = String> {
    proptest::collection::vec("[A-Za-z0-9éñü,.!?'\"()&%-]{1,10}", 1..6).prop_map(|w| w.join(" "))
}

fn text() -> impl Strategy<Value = String> {
    proptest::collection::vec(line(), 1..4).prop_map(|l| l.join("\n"))
}

/// Cues on a millisecond grid, non-overlapping, sorted.
fn ms_cues(max: usize) -> impl Strategy<Value = Vec<Cue>> {
    proptest::collection::vec((0i64..5_000, 1i64..8_000, text()), 0..max).prop_map(|v| {
        let mut t = 0i64;
        v.into_iter()
            .map(|(gap, dur, text)| {
                let start = t + gap;
                t = start + dur;
                Cue { start: Tick(start * MS), end: Tick(t * MS), text, ..Default::default() }
            })
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn srt_roundtrip(cues in ms_cues(12)) {
        let doc = Document { cues, ..Default::default() };
        let bytes = write(&doc, Format::Srt, WriteOptions::default());
        let back = parse(&bytes, Format::Srt).unwrap();
        prop_assert_eq!(&back.cues, &doc.cues);
        prop_assert_eq!(write(&back, Format::Srt, WriteOptions::default()), bytes);
    }

    #[test]
    fn srt_crlf_bom_roundtrip(cues in ms_cues(6)) {
        let doc = Document { cues, ..Default::default() };
        let s = String::from_utf8(write(&doc, Format::Srt, WriteOptions::default())).unwrap();
        let mut bytes = vec![0xef, 0xbb, 0xbf];
        bytes.extend(s.replace('\n', "\r\n").into_bytes());
        let back = parse(&bytes, Format::Srt).unwrap();
        prop_assert_eq!(back.cues, doc.cues);
    }

    #[test]
    fn vtt_roundtrip(
        cues in ms_cues(12),
        ids in proptest::collection::vec(proptest::option::of("[a-z][a-z0-9-]{0,8}"), 12),
        speakers in proptest::collection::vec(proptest::option::of("[A-Z][a-z]{1,6}( [A-Z][a-z]{1,6})?"), 12),
        settings in proptest::collection::vec(prop_oneof![Just(String::new()), Just("line:90%".to_string()), Just("align:start position:10%".to_string())], 12),
    ) {
        let cues: Vec<Cue> = cues
            .into_iter()
            .enumerate()
            .map(|(i, mut c)| {
                c.id = ids[i].clone();
                c.speaker = speakers[i].clone();
                c.settings = settings[i].clone();
                c
            })
            .collect();
        let doc = Document { cues, blocks: vec!["STYLE\n::cue { color: #ff0 }".into()], ..Default::default() };
        let bytes = write(&doc, Format::WebVtt, WriteOptions::default());
        let back = parse(&bytes, Format::WebVtt).unwrap();
        prop_assert_eq!(&back.cues, &doc.cues);
        prop_assert_eq!(&back.blocks, &doc.blocks);
        prop_assert_eq!(write(&back, Format::WebVtt, WriteOptions::default()), bytes);
    }

    /// SCC: frame-exact at 29.97 fps for cues with room to load (≥ 2 s apart) and 608-safe text.
    #[test]
    fn scc_roundtrip(
        v in proptest::collection::vec((60i64..400, 3i64..300, proptest::collection::vec("[A-Za-z0-9,.!?'()é-]{1,8}", 1..4)), 1..8),
        drop_frame in any::<bool>(),
    ) {
        let mut f = 0i64;
        let cues: Vec<Cue> = v
            .into_iter()
            .map(|(gap, dur, words)| {
                let start = f + gap;
                f = start + dur;
                Cue { start: scc::RATE.tick_of(start), end: scc::RATE.tick_of(f), text: words.join(" "), ..Default::default() }
            })
            .collect();
        let doc = Document { cues, ..Default::default() };
        let bytes = write(&doc, Format::Scc, WriteOptions { drop_frame, ..Default::default() });
        let back = parse(&bytes, Format::Scc).unwrap();
        prop_assert_eq!(&back.cues, &doc.cues);
        prop_assert_eq!(write(&back, Format::Scc, WriteOptions { drop_frame, ..Default::default() }), bytes);
    }

    /// Back-to-back SCC captions (no gap) still come back frame-exact when each lasts long enough
    /// for the next one to load while it is showing.
    #[test]
    fn scc_back_to_back(v in proptest::collection::vec((45i64..200, "[A-Z][a-z]{1,8}( [a-z]{1,8}){0,2}"), 1..6)) {
        let mut f = 90i64;
        let cues: Vec<Cue> = v
            .into_iter()
            .map(|(dur, text)| {
                let start = f;
                f += dur;
                Cue { start: scc::RATE.tick_of(start), end: scc::RATE.tick_of(f), text, ..Default::default() }
            })
            .collect();
        let doc = Document { cues, ..Default::default() };
        let back = parse(&write(&doc, Format::Scc, WriteOptions::default()), Format::Scc).unwrap();
        prop_assert_eq!(back.cues, doc.cues);
    }

    /// MCC: frame-exact at 29.97 fps (CEA-608 path), like SCC.
    #[test]
    fn mcc_roundtrip(
        v in proptest::collection::vec((60i64..400, 3i64..300, proptest::collection::vec("[A-Za-z0-9,.!?'()é-]{1,8}", 1..4)), 1..6),
        drop_frame in any::<bool>(),
    ) {
        let mut f = 0i64;
        let cues: Vec<Cue> = v
            .into_iter()
            .map(|(gap, dur, words)| {
                let start = f + gap;
                f = start + dur;
                Cue { start: scc::RATE.tick_of(start), end: scc::RATE.tick_of(f), text: words.join(" "), ..Default::default() }
            })
            .collect();
        let doc = Document { cues, ..Default::default() };
        let bytes = write(&doc, Format::Mcc, WriteOptions { drop_frame, ..Default::default() });
        let back = parse(&bytes, Format::Mcc).unwrap();
        prop_assert_eq!(&back.cues, &doc.cues);
        prop_assert_eq!(write(&back, Format::Mcc, WriteOptions { drop_frame, ..Default::default() }), bytes);
    }

    /// MCC CEA-708 path alone is frame-exact too.
    #[test]
    fn mcc_708_roundtrip(v in proptest::collection::vec((60i64..400, 3i64..300, proptest::collection::vec("[A-Za-z0-9,.!?'()é-]{1,8}", 1..4)), 1..6)) {
        let mut f = 0i64;
        let cues: Vec<Cue> = v
            .into_iter()
            .map(|(gap, dur, words)| {
                let start = f + gap;
                f = start + dur;
                Cue { start: scc::RATE.tick_of(start), end: scc::RATE.tick_of(f), text: words.join(" "), ..Default::default() }
            })
            .collect();
        let doc = Document { cues, ..Default::default() };
        let text = filmcraft_captions::mcc::write_with(&doc, true, false, true);
        let back = parse(text.as_bytes(), Format::Mcc).unwrap();
        prop_assert_eq!(&back.cues, &doc.cues);
    }

    /// EBU STL: frame-exact at 25 and 29.97 fps, multi-line text with accents, long subtitles in
    /// extension blocks.
    #[test]
    fn stl_roundtrip(v in proptest::collection::vec((0i64..200, 1i64..300, text()), 0..10), ntsc in any::<bool>()) {
        let rate = if ntsc { FrameRate::FPS_29_97 } else { FrameRate::FPS_25 };
        let mut f = 0i64;
        let cues: Vec<Cue> = v
            .into_iter()
            .map(|(gap, dur, text)| {
                let start = f + gap;
                f = start + dur;
                Cue { start: rate.tick_of(start), end: rate.tick_of(f), text, ..Default::default() }
            })
            .collect();
        let doc = Document { cues, ..Default::default() };
        let opts = WriteOptions { rate: Some(rate), ..Default::default() };
        let bytes = write(&doc, Format::Stl, opts);
        let back = parse(&bytes, Format::Stl).unwrap();
        prop_assert_eq!(&back.cues, &doc.cues);
        prop_assert_eq!(write(&back, Format::Stl, opts), bytes);
    }

    /// TTML (IMSC1): frame-exact at the sequence rate; ids and speakers kept.
    #[test]
    fn ttml_roundtrip(
        v in proptest::collection::vec((0i64..200, 1i64..300, text()), 0..10),
        rate in prop_oneof![Just(FrameRate::FPS_23_976), Just(FrameRate::FPS_25), Just(FrameRate::FPS_29_97), Just(FrameRate::FPS_59_94)],
        speakers in proptest::collection::vec(proptest::option::of("[A-Z][a-z]{1,6}( [A-Z][a-z]{1,6})?"), 10),
    ) {
        let mut f = 0i64;
        let cues: Vec<Cue> = v
            .into_iter()
            .enumerate()
            .map(|(i, (gap, dur, text))| {
                let start = f + gap;
                f = start + dur;
                Cue { start: rate.tick_of(start), end: rate.tick_of(f), text, speaker: speakers[i].clone(), id: Some(format!("c{}", i + 1)), ..Default::default() }
            })
            .collect();
        let doc = Document { cues, ..Default::default() };
        let opts = WriteOptions { rate: Some(rate), ..Default::default() };
        let bytes = write(&doc, Format::Ttml, opts);
        let back = parse(&bytes, Format::Ttml).unwrap();
        prop_assert_eq!(&back.cues, &doc.cues);
        prop_assert_eq!(write(&back, Format::Ttml, opts), bytes);
    }

    /// DFXP: clock times in milliseconds; frame-exact after snapping to the sequence rate.
    #[test]
    fn dfxp_roundtrip(v in proptest::collection::vec((0i64..200, 1i64..300, text()), 0..10), ntsc in any::<bool>()) {
        let rate = if ntsc { FrameRate::FPS_29_97 } else { FrameRate::FPS_25 };
        let mut f = 0i64;
        let cues: Vec<Cue> = v
            .into_iter()
            .enumerate()
            .map(|(i, (gap, dur, text))| {
                let start = f + gap;
                f = start + dur;
                Cue { start: rate.tick_of(start), end: rate.tick_of(f), text, id: Some(format!("c{}", i + 1)), ..Default::default() }
            })
            .collect();
        let doc = Document { cues, ..Default::default() };
        let bytes = write(&doc, Format::Dfxp, WriteOptions { rate: Some(rate), ..Default::default() });
        let mut back = parse(&bytes, Format::Dfxp).unwrap();
        back.snap_to_frames(rate);
        prop_assert_eq!(&back.cues, &doc.cues);
    }

    /// Readers never panic on arbitrary input.
    #[test]
    fn readers_are_total(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
        for f in Format::ALL {
            let _ = parse(&bytes, f);
        }
    }

    /// The binary STL reader never panics on a valid header followed by arbitrary blocks, and
    /// the MCC reader on arbitrary data lines.
    #[test]
    fn stl_and_mcc_readers_are_total(tail in proptest::collection::vec(any::<u8>(), 0..1200), line in "[0-9A-Za-z:;\t ]{0,200}") {
        let mut stl = write(&Document::default(), Format::Stl, WriteOptions::default());
        stl.extend(tail);
        let _ = parse(&stl, Format::Stl);
        let mcc = format!("File Format=MacCaption_MCC V1.0\nTime Code Rate=30DF\n\n00:00:00;00\t{line}\n");
        let _ = parse(mcc.as_bytes(), Format::Mcc);
    }
}

#[test]
fn srt_to_vtt_to_scc_keeps_text_and_frames() {
    let srt = b"1\r\n00:00:02,002 --> 00:00:04,004\r\nHello there\r\n\r\n2\r\n00:00:05,005 --> 00:00:07,007\r\n<i>General</i> Kenobi\r\n";
    let mut doc = parse(srt, Format::Srt).unwrap();
    doc.snap_to_frames(scc::RATE);
    let vtt = write(&doc, Format::WebVtt, WriteOptions::default());
    let doc2 = parse(&vtt, Format::WebVtt).unwrap();
    let scc_bytes = write(&doc2, Format::Scc, WriteOptions::default());
    let doc3 = parse(&scc_bytes, Format::Scc).unwrap();
    let texts: Vec<&str> = doc3.cues.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["Hello there", "General Kenobi"]);
    // 2.002 s = frame 60 at 29.97
    assert_eq!(scc::frame_of(doc3.cues[0].start), 60);
    assert_eq!(scc::frame_of(doc3.cues[1].end), 210);
}
