//! FCPXML `adjust-conform` (#2218): absent means `fit`, so a clip is scaled into the frame before
//! its transform; `none` keeps the native pixels; `fill` is reported and fitted.

mod support;

use filmcraft_interchange::{ExportOptions, Format, Report, import};
use filmcraft_project::{Project, TrackKind};
use filmcraft_time::FrameRate;
use support::*;

fn doc(conform: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<fcpxml version="1.10">
  <resources>
    <format id="r1" frameDuration="1/25s" width="1920" height="1080"/>
    <format id="r2" frameDuration="1/25s" width="3840" height="2160"/>
    <asset id="a1" name="UHD" start="0s" duration="10s" hasVideo="1" format="r2"><media-rep kind="original-media" src="file:///m/uhd.mov"/></asset>
  </resources>
  <library><event name="E"><project name="P">
    <sequence format="r1" tcStart="0s" tcFormat="NDF"><spine>
      <asset-clip ref="a1" name="UHD" offset="0s" duration="2s">{conform}<adjust-transform scale="1.5 1.5"/></asset-clip>
    </spine></sequence>
  </project></event></library>
</fcpxml>"#
    )
}

fn scale_to_frame(conform: &str) -> (bool, Report) {
    let (imp, rep) = import(doc(conform).as_bytes(), Format::Fcpxml, None).unwrap();
    let s = imp.project.sequence(only_seq(&imp)).unwrap();
    (s.tracks(TrackKind::Video)[0].items[0].scale_to_frame, rep)
}

fn warned(rep: &Report) -> bool {
    rep.entries.iter().any(|e| e.message.contains("adjust-conform"))
}

#[test]
fn a_missing_adjust_conform_means_fit() {
    assert!(scale_to_frame("").0);
    assert!(scale_to_frame(r#"<adjust-conform type="fit"/>"#).0);
}

#[test]
fn none_keeps_the_native_pixels() {
    let (fit, rep) = scale_to_frame(r#"<adjust-conform type="none"/>"#);
    assert!(!fit && !warned(&rep));
}

#[test]
fn fill_is_reported_and_fitted() {
    let (fit, rep) = scale_to_frame(r#"<adjust-conform type="fill"/>"#);
    assert!(fit && warned(&rep));
}

#[test]
fn export_writes_none_for_clips_that_are_not_fitted_and_round_trips() {
    let mut p = Project::new("t");
    let r = FrameRate::FPS_25;
    let a = media(&mut p, "/media/A.mov", true, false, r);
    let s = sequence(&mut p, "Seq", r, false);
    clip(&mut p, s, TrackKind::Video, 0, a, 0, 50, 0);
    clip(&mut p, s, TrackKind::Video, 0, a, 50, 50, 0);
    p.sequence_mut(s).unwrap().tracks_mut(TrackKind::Video)[0].items[1].scale_to_frame = true;
    let (imp, text, _) = roundtrip(&p, s, Format::Fcpxml, &ExportOptions::default());
    assert_eq!(text.matches(r#"<adjust-conform type="none"/>"#).count(), 1, "{text}");
    let items = &imp.project.sequence(only_seq(&imp)).unwrap().tracks(TrackKind::Video)[0].items;
    assert_eq!((items[0].scale_to_frame, items[1].scale_to_frame), (false, true));
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]
    /// Whatever mix of fitted and native-size clips on any video track, a round trip keeps each flag.
    #[test]
    fn scale_to_frame_survives_a_round_trip(flags in proptest::collection::vec((0usize..3, proptest::bool::ANY), 1..20)) {
        let mut p = Project::new("t");
        let r = FrameRate::FPS_25;
        let a = media(&mut p, "/media/A.mov", true, false, r);
        let s = sequence(&mut p, "Seq", r, false);
        let mut ends = [0i64; 3];
        for (track, fit) in &flags {
            let c = clip(&mut p, s, TrackKind::Video, *track, a, ends[*track], 10, 0);
            ends[*track] += 10;
            p.sequence_mut(s).unwrap().find_item_mut(c).unwrap().1.scale_to_frame = *fit;
        }
        let want = |p: &Project, s| -> Vec<Vec<bool>> {
            p.sequence(s).unwrap().tracks(TrackKind::Video).iter().map(|t| t.items.iter().map(|i| i.scale_to_frame).collect()).collect()
        };
        let (imp, text, _) = roundtrip(&p, s, Format::Fcpxml, &ExportOptions::default());
        let got = want(&imp.project, only_seq(&imp));
        let mut exp = want(&p, s);
        exp.retain(|t| !t.is_empty());
        let mut got_t = got;
        got_t.retain(|t| !t.is_empty());
        proptest::prop_assert_eq!(got_t, exp, "{}", text);
    }
}
