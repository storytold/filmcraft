//! CEA-708 timing of back-to-back one-frame captions in MCC.

use filmcraft_captions::{Cue, Document, Format, mcc, parse};

/// A frame carries 18 DTVCC pairs, and with one-frame captions back to back it holds one
/// caption's display (3 pairs) and the next one's load. A load of up to 19 characters (15 pairs)
/// fits beside it. A SetPenLocation in every load made it 16 pairs from 17 characters on, and
/// the display a frame late.
#[test]
fn back_to_back_one_frame_708_captions_keep_their_frames() {
    for len in [1, 16, 17, 18, 19] {
        let cues: Vec<Cue> = (0..4i64)
            .map(|k| Cue {
                start: mcc::RATE.tick_of(60 + k),
                end: mcc::RATE.tick_of(61 + k),
                // a different text each time, so the reader sees every change
                text: ('a'..='z').cycle().skip(k as usize).take(len).collect(),
                ..Default::default()
            })
            .collect();
        let doc = Document { cues, ..Default::default() };
        let back = parse(mcc::write_with(&doc, true, false, true).as_bytes(), Format::Mcc).unwrap();
        let frames: Vec<(i64, i64)> = back.cues.iter().map(|c| (mcc::RATE.frame_at(c.start), mcc::RATE.frame_at(c.end))).collect();
        assert_eq!(frames, [(60, 61), (61, 62), (62, 63), (63, 64)], "{len} characters");
        assert_eq!(back.cues, doc.cues, "{len} characters");
    }
}
