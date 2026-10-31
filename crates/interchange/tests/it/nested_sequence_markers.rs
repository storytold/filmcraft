//! Nested sequence markers vs clip instance markers in OTIO (issue #901).
//!
//! FCP7 XML and FCPXML already keep these on the right objects. OTIO must not drop the nested
//! sequence's markers when the using clip has none, and must not put clip instance markers on the
//! nested sequence.

use crate::support;

use filmcraft_interchange::{ExportOptions, Format};
use filmcraft_project::{Label, Marker, MarkerId, MarkerKind, TrackKind};
use filmcraft_time::{FrameRate, Tick};
use support::*;

fn nest_project(clip_has_marker: bool) -> (filmcraft_project::Project, filmcraft_project::ItemId, filmcraft_project::ItemId) {
    let r = FrameRate::FPS_24;
    let mut p = filmcraft_project::Project::new("P");
    let a = media(&mut p, "/m/a.mov", true, true, r);
    let nest = sequence(&mut p, "Inner", r, false);
    clip(&mut p, nest, TrackKind::Video, 0, a, 0..48, 0);
    {
        let q = p.sequence_mut(nest).unwrap();
        q.markers.push(Marker {
            id: MarkerId(1),
            start: r.tick_of(12),
            duration: Tick::ZERO,
            name: "on nest".into(),
            comment: String::new(),
            kind: MarkerKind::Comment,
            color: Label::Green,
        });
    }
    let s = sequence(&mut p, "Outer", r, false);
    let nest_clip = clip(&mut p, s, TrackKind::Video, 0, nest, 0..48, 0);
    if clip_has_marker {
        let q = p.sequence_mut(s).unwrap();
        let (_, it) = q.find_item_mut(nest_clip).unwrap();
        it.markers.push(Marker {
            id: MarkerId(2),
            start: r.tick_of(6),
            duration: Tick::ZERO,
            name: "on clip".into(),
            comment: String::new(),
            kind: MarkerKind::Comment,
            color: Label::Yellow,
        });
    }
    (p, s, nest)
}

fn names(ms: &[Marker]) -> Vec<&str> {
    ms.iter().map(|m| m.name.as_str()).collect()
}

#[test]
fn otio_keeps_nested_sequence_markers_when_clip_has_none() {
    let (p, s, nest) = nest_project(false);
    let (imp, text, rep) = roundtrip(&p, s, Format::Otio, &ExportOptions::default());
    assert!(!rep.has_warnings(), "{rep}");
    let outer = only_seq(&imp);
    let nest_clip = &imp.project.sequence(outer).unwrap().video_tracks[0].items[0];
    assert!(nest_clip.markers.is_empty(), "clip instance must stay marker-free\n{text}");
    let inner = nest_clip.item;
    assert_eq!(imp.project.item(inner).unwrap().name, "Inner");
    assert_eq!(names(&imp.project.sequence(inner).unwrap().markers), names(&p.sequence(nest).unwrap().markers), "{text}");
}

#[test]
fn otio_keeps_clip_instance_markers_off_the_nested_sequence() {
    let (p, s, nest) = nest_project(true);
    let (imp, text, _) = roundtrip(&p, s, Format::Otio, &ExportOptions::default());
    let outer = only_seq(&imp);
    let nest_clip = &imp.project.sequence(outer).unwrap().video_tracks[0].items[0];
    assert_eq!(names(&nest_clip.markers), vec!["on clip"], "{text}");
    assert_eq!(nest_clip.markers[0].start, FrameRate::FPS_24.tick_of(6));
    let inner = nest_clip.item;
    assert_eq!(names(&imp.project.sequence(inner).unwrap().markers), names(&p.sequence(nest).unwrap().markers), "{text}");
}
