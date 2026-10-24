//! Source-media (master clip) markers must survive FCP7 XML export for xmeml 4 and 5.

use crate::support;

use filmcraft_interchange::{ExportOptions, Format, export, import};
use filmcraft_project::{Label, Marker, MarkerId, MarkerKind, TrackKind};
use filmcraft_time::{FrameRate, Tick};
use support::*;

fn project_with_media_marker(rate: FrameRate) -> (filmcraft_project::Project, filmcraft_project::ItemId) {
    let mut p = filmcraft_project::Project::new("Markers");
    let a = media(&mut p, "/m/src.mov", true, true, rate);
    if let Some(m) = p.item_mut(a).and_then(|i| i.as_media_mut()) {
        m.markers.push(Marker {
            id: MarkerId(1),
            start: rate.tick_of(24),
            duration: Tick::ZERO,
            name: "media hit".into(),
            comment: "on source".into(),
            kind: MarkerKind::Comment,
            color: Label::Green,
        });
    }
    let s = sequence(&mut p, "Cut", rate, false);
    clip(&mut p, s, TrackKind::Video, 0, a, 0, 48, 0);
    (p, s)
}

#[test]
fn fcp7_xml_exports_master_clip_markers_on_xmeml_4_and_5() {
    let rate = FrameRate::FPS_24;
    let (p, s) = project_with_media_marker(rate);
    for version in [4u32, 5] {
        let opts = ExportOptions { xmeml_version: version, ..Default::default() };
        let (bytes, report) = export(&p, s, Format::Fcp7Xml, &opts).expect("export");
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains(&format!("<xmeml version=\"{version}\">")), "{text}");
        assert!(text.contains("<clip id=\"masterclip-"), "{text}");
        assert!(text.contains("<name>media hit</name>"), "{text}");
        assert!(!report.has_warnings(), "export must not silently drop media markers: {report}");
        let (imp, irep) = import(&bytes, Format::Fcp7Xml, None).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert!(!irep.has_warnings(), "{irep}");
        let media = imp.project.items.values().find(|i| i.name == "src.mov").and_then(|i| i.as_media()).expect("media");
        assert_eq!(media.markers.len(), 1, "{text}");
        assert_eq!(media.markers[0].name, "media hit");
        assert_eq!(media.markers[0].comment, "on source");
        assert_eq!(media.markers[0].start, rate.tick_of(24));
    }
}
