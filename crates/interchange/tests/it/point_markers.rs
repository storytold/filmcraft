//! FCPXML point markers: a one-frame `<marker>` is a FilmCraft point (duration 0), not a range.

use crate::support;

use filmcraft_interchange::{ExportOptions, Format, import};
use filmcraft_project::{Label, Marker, MarkerId, MarkerKind, Project, TrackKind};
use filmcraft_time::{FrameRate, Tick};
use support::*;

fn fcpxml_with_markers(frame: &str, point_dur: &str, range_dur: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE fcpxml>
<fcpxml version="1.10">
  <resources>
    <format id="r1" frameDuration="{frame}" width="1920" height="1080"/>
    <asset id="r2" name="Clip" start="0s" duration="10s" hasVideo="1" hasAudio="1" format="r1">
      <media-rep kind="original-media" src="file:///Media/clip.mov"/>
    </asset>
  </resources>
  <library>
    <event name="E">
      <project name="P">
        <sequence format="r1" tcStart="0s" tcFormat="NDF">
          <spine>
            <asset-clip ref="r2" name="Clip" offset="0s" start="0s" duration="10s">
              <marker start="1s" duration="{point_dur}" value="Point"/>
              <marker start="2s" duration="{range_dur}" value="Range" note="span"/>
            </asset-clip>
          </spine>
        </sequence>
      </project>
    </event>
  </library>
</fcpxml>
"#
    )
}

#[test]
fn one_frame_fcpxml_marker_is_a_point() {
    let r = FrameRate::FPS_24;
    let xml = fcpxml_with_markers("1/24s", "1/24s", "2/24s");
    let (imp, _) = import(xml.as_bytes(), Format::Fcpxml, None).unwrap();
    let s = imp.project.sequence(only_seq(&imp)).unwrap();
    assert_eq!(s.markers.len(), 2);
    assert_eq!((s.markers[0].name.as_str(), s.markers[0].start, s.markers[0].duration), ("Point", r.tick_of(24), Tick::ZERO));
    assert_eq!((s.markers[1].name.as_str(), s.markers[1].start, s.markers[1].duration), ("Range", r.tick_of(48), r.tick_of(2)));
}

#[test]
fn point_roundtrips_through_the_one_frame_convention() {
    let r = FrameRate::FPS_24;
    let mut p = Project::new("Lib");
    let a = media(&mut p, "/m/a.mov", true, true, r);
    let s = sequence(&mut p, "Cut", r, false);
    clip(&mut p, s, TrackKind::Video, 0, a, 0, 240, 0);
    {
        let q = p.sequence_mut(s).unwrap();
        q.markers.push(Marker {
            id: MarkerId(1),
            start: r.tick_of(24),
            duration: Tick::ZERO,
            name: "Point".into(),
            comment: String::new(),
            kind: MarkerKind::Comment,
            color: Label::Blue,
        });
        q.markers.push(Marker {
            id: MarkerId(2),
            start: r.tick_of(48),
            duration: r.tick_of(5),
            name: "Range".into(),
            comment: "span".into(),
            kind: MarkerKind::Comment,
            color: Label::Blue,
        });
    }
    let (imp, text, _) = roundtrip(&p, s, Format::Fcpxml, &ExportOptions::default());
    assert!(text.contains("duration=\"1/24s\""), "{text}");
    let qs = imp.project.sequence(only_seq(&imp)).unwrap();
    assert_eq!(
        qs.markers.iter().map(|m| (m.name.as_str(), m.start, m.duration)).collect::<Vec<_>>(),
        vec![("Point", r.tick_of(24), Tick::ZERO), ("Range", r.tick_of(48), r.tick_of(5))]
    );
}
