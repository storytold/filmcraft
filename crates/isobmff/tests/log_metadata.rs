use filmcraft_isobmff::*;
use std::io::Cursor;

fn movie(identifier: &str) -> Result<Vec<u8>> {
    let mut entry = SampleEntry::prores(FourCc(*b"apch"), 64, 64);
    entry.video.as_mut().unwrap().log_transfer_function = Some(identifier.into());
    let mut out = Mp4Writer::new(Cursor::new(Vec::new()), WriterOptions::new(Brand::Mov))?;
    let track = out.add_track(TrackConfig::new(entry, 30))?;
    out.write_sample(track, WriteSample { data: b"original dummy sample", duration: 1, composition_offset: 0, is_sync: true })?;
    Ok(out.finish()?.into_inner())
}
#[test]
fn quicktime_log_identifiers_round_trip_without_guessing_their_meaning() {
    for id in ["com.apple.rec2020.apple-log", "com.apple.rec2020.apple-log2", "another.vendor.unknown-curve"] {
        let bytes = movie(id).unwrap();
        let parsed = open(bytes.as_slice()).unwrap();
        assert_eq!(parsed.tracks[0].entries[0].video.as_ref().unwrap().log_transfer_function.as_deref(), Some(id));
    }
}
#[test]
fn invalid_owned_log_identifiers_are_rejected_instead_of_truncated() {
    for id in [String::new(), "a".repeat(256), "com.apple.rec2020.apple-log\0".into()] {
        let result = std::panic::catch_unwind(|| movie(&id)).unwrap();
        assert!(result.is_err());
    }
}
