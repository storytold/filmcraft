//! Cues beyond the supported time range must not end up in a caption track, or the saved
//! project fails its bounds check on load.

use filmcraft_captions::{Format, parse, track_from_document};
use filmcraft_project::{CaptionFormat, TrackId};

#[test]
fn srt_cue_beyond_tick_max_is_skipped() {
    let srt = "1\n05000:00:00,000 --> 05000:00:01,000\nfar\n\n2\n00:00:01,000 --> 00:00:02,000\nnear\n";
    let doc = parse(srt.as_bytes(), Format::Srt).unwrap();
    let mut next = 0;
    let t = track_from_document(&doc, TrackId(1), "c", CaptionFormat::Subtitle, &mut || {
        next += 1;
        next
    });
    assert!(t.check_bounds().is_ok());
    assert_eq!(t.captions.len(), 1);
    assert_eq!(t.captions[0].text, "near");
}
