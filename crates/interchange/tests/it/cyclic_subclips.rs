//! Verify that cyclic and deeply chained subclips do not cause infinite recursion (stack overflow)
//! during interchange export.
//!
//! Regression test for https://github.com/storytold/filmcraft/issues/2788.

use super::support::*;
use filmcraft_interchange::aaf::{self, AafOptions};
use filmcraft_interchange::ale::export_items;
use filmcraft_interchange::omf::{self, OmfOptions};
use filmcraft_interchange::{ExportOptions, Format, export, export_edl_per_track};
use filmcraft_project::{ItemId, ItemKind, Label, Project, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};

#[test]
fn cyclic_subclips_do_not_stack_overflow_on_any_interchange_export() {
    let r = FrameRate::FPS_24;
    let mut p = Project::new("cyclic_test");

    let media_id = media(&mut p, "/media/source.mp4", true, true, r);

    // 1. Chained subclips (valid)
    let sub1 = p.add_item(
        "Sub 1",
        Label::Iris,
        ItemKind::Subclip { parent: media_id, range: TimeRange::new(r.tick_of(0), r.tick_of(100)), restrict_trims: false },
        None,
    );
    let sub2 =
        p.add_item("Sub 2", Label::Iris, ItemKind::Subclip { parent: sub1, range: TimeRange::new(r.tick_of(10), r.tick_of(50)), restrict_trims: false }, None);

    // 2. Self-referencing subclip (direct cycle)
    let self_cycle =
        p.add_item("Self Cycle", Label::Iris, ItemKind::Subclip { parent: ItemId(99999), range: TimeRange::default(), restrict_trims: false }, None);
    p.item_mut(self_cycle).unwrap().kind = ItemKind::Subclip { parent: self_cycle, range: TimeRange::new(Tick::ZERO, r.tick_of(50)), restrict_trims: false };

    // 3. Mutual cycle (A -> B -> A)
    let cycle_a = p.add_item("Cycle A", Label::Iris, ItemKind::Subclip { parent: ItemId(99999), range: TimeRange::default(), restrict_trims: false }, None);
    let cycle_b = p.add_item(
        "Cycle B",
        Label::Iris,
        ItemKind::Subclip { parent: cycle_a, range: TimeRange::new(Tick::ZERO, r.tick_of(50)), restrict_trims: false },
        None,
    );
    p.item_mut(cycle_a).unwrap().kind = ItemKind::Subclip { parent: cycle_b, range: TimeRange::new(Tick::ZERO, r.tick_of(50)), restrict_trims: false };

    // 4. Overly deep subclip chain (> 16 levels)
    let mut deep_parent = media_id;
    for n in 0..20 {
        deep_parent = p.add_item(
            &format!("Deep {n}"),
            Label::Iris,
            ItemKind::Subclip { parent: deep_parent, range: TimeRange::new(Tick::ZERO, r.tick_of(50)), restrict_trims: false },
            None,
        );
    }
    let too_deep = deep_parent;

    // 5. Subclip with missing parent item
    let missing_parent = p.add_item(
        "Missing Parent",
        Label::Iris,
        ItemKind::Subclip { parent: ItemId(888_888_888), range: TimeRange::new(Tick::ZERO, r.tick_of(50)), restrict_trims: false },
        None,
    );

    // Place all items on timeline sequences (both video and audio)
    let seq = sequence(&mut p, "Timeline with cyclic clips", r, false);
    clip(&mut p, seq, TrackKind::Video, 0, sub2, 0..50, 0);
    clip(&mut p, seq, TrackKind::Video, 0, self_cycle, 50..100, 0);
    clip(&mut p, seq, TrackKind::Video, 0, cycle_a, 100..150, 0);
    clip(&mut p, seq, TrackKind::Video, 0, cycle_b, 150..200, 0);
    clip(&mut p, seq, TrackKind::Video, 0, too_deep, 200..250, 0);
    clip(&mut p, seq, TrackKind::Video, 0, missing_parent, 250..300, 0);

    clip(&mut p, seq, TrackKind::Audio, 0, sub2, 0..50, 0);
    clip(&mut p, seq, TrackKind::Audio, 0, self_cycle, 50..100, 0);
    clip(&mut p, seq, TrackKind::Audio, 0, cycle_a, 100..150, 0);
    clip(&mut p, seq, TrackKind::Audio, 0, too_deep, 200..250, 0);

    // Verify ALE export skips cyclic items without hanging
    let ale_doc = export_items(&p, &[media_id, sub1, sub2, self_cycle, cycle_a, cycle_b, too_deep, missing_parent]);
    // media_id, sub1, sub2 should succeed (3 items)
    assert_eq!(ale_doc.rows.len(), 3);
    assert_eq!(ale_doc.get(0, "Name"), Some("source.mp4"));
    assert_eq!(ale_doc.get(1, "Name"), Some("Sub 1"));
    assert_eq!(ale_doc.get(2, "Name"), Some("Sub 2"));

    // Verify FCP7 XML export terminates safely and includes the valid chained subclip
    let (fcp7_bytes, _) = export(&p, seq, Format::Fcp7Xml, &ExportOptions::default()).expect("FCP7 XML export should succeed without stack overflow");
    let fcp7_text = String::from_utf8_lossy(&fcp7_bytes);
    assert!(fcp7_text.contains("<name>Sub 2</name>"), "FCP7 XML should carry valid chained subclip 'Sub 2'");

    // Verify FCPXML export terminates safely and includes the valid chained subclip
    let (fcpxml_bytes, _) = export(&p, seq, Format::Fcpxml, &ExportOptions::default()).expect("FCPXML export should succeed without stack overflow");
    let fcpxml_text = String::from_utf8_lossy(&fcpxml_bytes);
    assert!(fcpxml_text.contains("name=\"Sub 2\""), "FCPXML should carry valid chained subclip 'Sub 2'");

    // Verify EDL export terminates safely and includes the valid chained subclip
    let (edl_bytes, _) = export(&p, seq, Format::Edl, &ExportOptions::default()).expect("EDL export should succeed without stack overflow");
    let edl_text = String::from_utf8_lossy(&edl_bytes);
    assert!(edl_text.contains("Sub 2"), "EDL should carry valid chained subclip 'Sub 2'");

    let (edl_per_track, _) = export_edl_per_track(&p, seq, &ExportOptions::default()).expect("EDL per track export should succeed without stack overflow");
    assert!(!edl_per_track.is_empty());
    assert!(edl_per_track.iter().any(|(_, b)| String::from_utf8_lossy(b).contains("Sub 2")), "EDL per track should carry 'Sub 2'");

    // Verify OTIO export terminates safely and includes the valid chained subclip
    let (otio_bytes, _) = export(&p, seq, Format::Otio, &ExportOptions::default()).expect("OTIO export should succeed without stack overflow");
    let otio_text = String::from_utf8_lossy(&otio_bytes);
    assert!(otio_text.contains("\"name\": \"Sub 2\""), "OTIO should carry valid chained subclip 'Sub 2'");

    // Verify AAF export terminates safely and produces non-empty output containing the subclip
    let (aaf_bytes, _) = aaf::export(&p, seq, &AafOptions::default()).expect("AAF export should succeed without stack overflow");
    assert!(!aaf_bytes.is_empty(), "AAF export should produce output");
    let aaf_has_sub2 = aaf_bytes.windows(5).any(|w| w == b"Sub 2") || aaf_bytes.windows(10).any(|w| w == b"S\0u\0b\0 \x002\0");
    assert!(aaf_has_sub2, "AAF export should carry valid chained subclip 'Sub 2'");

    // Verify OMF export terminates safely and produces non-empty output containing the subclip
    let (omf_bytes, _) = omf::export(&p, seq, &OmfOptions::default()).expect("OMF export should succeed without stack overflow");
    assert!(!omf_bytes.is_empty(), "OMF export should produce output");
    assert!(omf_bytes.windows(5).any(|w| w == b"Sub 2"), "OMF export should carry valid chained subclip 'Sub 2'");
}
