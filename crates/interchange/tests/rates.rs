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

/// The FCP7 `timebase` clamp and the OTIO rate fallback happen while parsing, so they must report
/// themselves; only the FCPXML path used to (via the post-import pass).
#[test]
fn clamped_timebase_and_replaced_otio_rate_are_reported() {
    let xml = r#"<?xml version="1.0"?><xmeml version="4"><sequence id="s"><name>S</name><rate><timebase>9223372036854775807</timebase><ntsc>FALSE</ntsc></rate>
<media><video><track><clipitem id="c"><name>A</name><start>0</start><end>100</end><in>0</in><out>100</out>
<file id="f"><name>a.mov</name><pathurl>file:///tmp/a.mov</pathurl><media><video/></media></file></clipitem></track></video></media></sequence></xmeml>"#;
    let (_, rep) = import(xml.as_bytes(), Format::Fcp7Xml, None).unwrap();
    assert!(rep.mentions("timebase above 1000000"), "{rep}");

    let otio = |rate: &str| {
        format!(
            r#"{{"OTIO_SCHEMA":"Timeline.1","name":"T","tracks":{{"OTIO_SCHEMA":"Stack.1","children":[{{"OTIO_SCHEMA":"Track.1","kind":"Video","children":[
{{"OTIO_SCHEMA":"Clip.1","name":"c","source_range":{{"OTIO_SCHEMA":"TimeRange.1","start_time":{{"OTIO_SCHEMA":"RationalTime.1","value":0,"rate":{rate}}},"duration":{{"OTIO_SCHEMA":"RationalTime.1","value":10,"rate":{rate}}}}},
"media_reference":{{"OTIO_SCHEMA":"MissingReference.1"}}}}]}}]}}}}"#
        )
    };
    // DSD64's 2 822 400 Hz is beyond the limit too
    for rate in ["2822400", "1e300"] {
        let (_, rep) = import(otio(rate).as_bytes(), Format::Otio, None).unwrap();
        assert!(rep.mentions("OTIO rate above 1000000"), "{rate}: {rep}");
    }
    // an ordinary rate says nothing
    let (_, rep) = import(otio("24").as_bytes(), Format::Otio, None).unwrap();
    assert!(!rep.mentions("above 1000000"), "{rep}");
}

/// The AAF and OMF importers return before the shared pass in `import_with`, so they bound start
/// timecodes themselves: an AAF whose start timecode is beyond the limit is reset with a warning (OMF has none to carry).
#[test]
fn aaf_and_omf_start_timecodes_are_bounded() {
    use filmcraft_interchange::{ImportOptions, aaf, omf};
    use filmcraft_project::{Project, TrackKind};
    use filmcraft_time::FrameRate;
    #[path = "support/mod.rs"]
    mod support;
    let r = FrameRate::FPS_25;
    let mut p = Project::new("t");
    let a = support::media(&mut p, "/media/A.mov", true, true, r);
    let s = support::sequence(&mut p, "Seq", r, false);
    support::clip(&mut p, s, TrackKind::Video, 0, a, 0, 50, 0);
    support::clip(&mut p, s, TrackKind::Audio, 0, a, 0, 50, 0);
    p.sequence_mut(s).unwrap().start_timecode = 4_000_000_000_000;
    for format in [Format::Aaf, Format::Omf] {
        let (bytes, _) = export(&p, s, format, &ExportOptions::default()).unwrap();
        let (imp, _, rep) =
            if format == Format::Aaf { aaf::import(&bytes, &ImportOptions::default()) } else { omf::import(&bytes, &ImportOptions::default()) }.unwrap();
        // OMF does not carry a sequence start timecode, so only AAF has something to reset
        assert!(format == Format::Omf || rep.mentions("start timecode"), "{format:?}: {rep}");
        for q in &imp.sequences {
            assert!(imp.project.sequence(*q).unwrap().start_timecode <= 360_000_000_000, "{format:?}");
        }
    }
}
