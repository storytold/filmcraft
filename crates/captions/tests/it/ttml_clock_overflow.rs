//! TTML / DFXP export must not overflow for cue times near `i64::MAX` (e.g. from an MCC with a
//! huge timecode).

use filmcraft_captions::{Cue, Document, Format, WriteOptions, write};
use filmcraft_time::Tick;

fn doc(end: Tick) -> Document {
    Document { cues: vec![Cue { start: Tick(0), end, text: "x".into(), ..Cue::default() }], ..Document::default() }
}

#[test]
fn ttml_and_dfxp_write_survive_tick_max() {
    for format in [Format::Ttml, Format::Dfxp] {
        let out = write(&doc(Tick(i64::MAX)), format, WriteOptions::default());
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("end=\""), "{format:?}: {s}");
    }
}
