//! Crate API: detection, merging fragments into a project, timecode rebasing.

use crate::support;

use std::collections::HashSet;

use filmcraft_interchange::{ExportOptions, Format, detect, export, import, merge_into, rebase_source_timecode};
use filmcraft_project::{ItemKind, Project, TrackKind, TransitionAlign};
use filmcraft_time::FrameRate;
use support::*;

#[test]
fn detect_by_content_and_extension() {
    assert_eq!(detect(b"<?xml version=\"1.0\"?><xmeml version=\"4\"/>", Some("xml")), Some(Format::Fcp7Xml));
    assert_eq!(detect(b"<?xml version=\"1.0\"?><fcpxml version=\"1.11\"/>", Some("xml")), Some(Format::Fcpxml));
    assert_eq!(detect(b"{\"OTIO_SCHEMA\": \"Timeline.1\"}", None), Some(Format::Otio));
    assert_eq!(detect(b"TITLE: x\n", None), Some(Format::Edl));
    assert_eq!(detect(b"", Some("EDL")), Some(Format::Edl));
    assert_eq!(Format::from_extension(".fcpxml"), Some(Format::Fcpxml));
}

#[test]
fn merge_reallocates_ids_and_keeps_links() {
    let r = FrameRate::FPS_25;
    let mut src = Project::new("src");
    let a = media(&mut src, "/m/a.mov", true, true, r);
    let s = sequence(&mut src, "S", r, false);
    let v1 = clip(&mut src, s, TrackKind::Video, 0, a, 0..50, 0);
    let a1 = clip(&mut src, s, TrackKind::Audio, 0, a, 0..50, 0);
    link(&mut src, s, &[v1, a1]);
    let v2 = clip(&mut src, s, TrackKind::Video, 0, a, 50..50 + (50), 100);
    transition(&mut src, s, (TrackKind::Video, 0), "cross_dissolve", (Some(v1), Some(v2)), 45..45 + (10), TransitionAlign::CenterAtCut);
    let (bytes, _) = export(&src, s, Format::Fcp7Xml, &ExportOptions::default()).unwrap();
    let (frag, _) = import(&bytes, Format::Fcp7Xml, None).unwrap();

    let mut target = Project::new("target");
    let existing = media(&mut target, "/other.mov", true, false, r);
    let bin = target.add_bin("Imported", None);
    let seqs = merge_into(&mut target, frag, Some(bin));
    assert_eq!(seqs.len(), 1);
    assert!(target.item(existing).is_some());
    let ns = seqs[0];
    assert_eq!(structure(&target, ns, TrackKind::Video), structure(&src, s, TrackKind::Video));
    assert_eq!(links(&target, ns), links(&src, s));
    assert_eq!(target.root.parent_of(ns), Some(bin));
    // every id is unique and below next_id
    let mut ids = HashSet::new();
    for (id, it) in &target.items {
        assert!(ids.insert(id.0));
        if let ItemKind::Sequence(q) = &it.kind {
            for t in q.all_tracks() {
                assert!(ids.insert(t.id.0));
                for c in &t.items {
                    assert!(ids.insert(c.id.0));
                    assert!(target.item(c.item).is_some());
                }
                for x in &t.transitions {
                    assert!(ids.insert(x.id.0));
                    assert!(x.from.is_none_or(|f| t.item(f).is_some()));
                    assert!(x.to.is_none_or(|f| t.item(f).is_some()));
                }
            }
        }
    }
    assert!(ids.iter().all(|i| *i < target.next_id));
}

#[test]
fn rebase_shifts_source_times() {
    let edl = "TITLE: T\nFCM: NON-DROP FRAME\n\n001  CAM      V     C        01:00:10:00 01:00:12:00 01:00:00:00 01:00:02:00\n* FROM CLIP NAME: cam.mov\n* SOURCE FILE: /m/cam.mov\n";
    let (imp, _) = import(edl.as_bytes(), Format::Edl, None).unwrap();
    let mut p = imp.project;
    let s = imp.sequences[0];
    let r = p.sequence(s).unwrap().settings.frame_rate;
    let item = p.sequence(s).unwrap().video_tracks[0].items[0].item;
    rebase_source_timecode(&mut p, item, 3600 * r.timecode_base(), r);
    assert_eq!(p.sequence(s).unwrap().video_tracks[0].items[0].source_in, r.tick_of(10 * r.timecode_base()));
    assert_eq!(p.item(item).unwrap().as_media().unwrap().info.start_timecode, Some(3600 * r.timecode_base()));
}
