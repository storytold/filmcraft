//! Frame rates and start timecodes a hostile document carries are bounded on import, so no export
//! overflows its frame arithmetic. (FCP7 `timebase` and start-timecode cases: `tests/fcp7.rs`.)

use filmcraft_interchange::{ExportOptions, Format, export, import};

/// A `frameDuration` of `1/9223372036854775807s` is a rate of 9e18 per second: `frame_at` clamps to
/// `i64::MAX` and `frames_round` then overflowed adding 1 (a panic in debug builds) as soon as the
/// sequence had a start timecode or a marker. An implausible rate falls back to the default, with a warning.
#[test]
fn absurd_fcpxml_frame_duration_does_not_overflow_the_export() {
    for fd in ["1/9223372036854775807s", "1/4294967296s", "1/2000000s"] {
        let xml = format!(
            r#"<?xml version="1.0"?><fcpxml version="1.10"><resources><format id="f1" frameDuration="{fd}" width="1920" height="1080"/>
<asset id="a1" name="c" start="0s" duration="10s" hasVideo="1" format="f1"><media-rep kind="original-media" src="file:///tmp/c.mov"/></asset></resources>
<library><event name="E"><project name="P"><sequence format="f1" tcStart="3600/1s" tcFormat="DF"><spine>
<asset-clip ref="a1" offset="3600s" duration="5s"><marker start="1s" duration="1/25s" value="m"/></asset-clip></spine></sequence></project></event></library></fcpxml>"#
        );
        let (imported, report) = import(xml.as_bytes(), Format::Fcpxml, None).expect("import succeeds");
        for seq in &imported.sequences {
            for format in [Format::Fcpxml, Format::Fcp7Xml, Format::Edl, Format::Otio] {
                export(&imported.project, *seq, format, &ExportOptions::default()).unwrap_or_else(|e| panic!("{fd} {format:?}: {e}"));
            }
        }
        assert!(report.mentions("implausible frame rate"), "{fd}: {report}");
    }
}
