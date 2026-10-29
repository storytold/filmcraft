//! The media fetch order of a project: active sequence first, media under the playhead first.

use filmcraft_media::{DemoScene, MediaSource};
use filmcraft_project::{
    ClipId, ItemId, ItemKind, Label, MediaClip, MediaRef, Project, ProjectView, Sequence, SequenceSettings, TrackItem, effect::intrinsic_audio,
};
use filmcraft_time::Tick;

use crate::prewarm_plan::{Plan, plan, plan_bytes};

/// A project holding media items by path.
fn media(p: &mut Project, path: &str) -> ItemId {
    let info = filmcraft_media::generators::GeneratorSource::demo(DemoScene::OceanSunset).info().clone();
    let clip = MediaClip {
        media: MediaRef::File { path: path.into() },
        info,
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    p.add_item(path, Label::Iris, ItemKind::Media(Box::new(clip)), None)
}

/// A clip of `item` at `start` for `duration` ticks.
fn clip(p: &mut Project, item: ItemId, start: i64, duration: i64) -> TrackItem {
    TrackItem {
        id: ClipId(p.alloc_id()),
        item,
        name: "c".into(),
        label: Label::Iris,
        start: Tick(start),
        duration: Tick(duration),
        source_in: Tick::ZERO,
        speed: 1.0,
        reverse: false,
        enabled: true,
        link: None,
        group: None,
        effects: intrinsic_audio(),
        markers: vec![],
        gain_db: 0.0,
        frame_hold: None,
        scale_to_frame: false,
        essential: None,
        multicam: None,
        time_interpolation: Default::default(),
        hold_filters: false,
        field_options: None,
        source_channels: Vec::new(),
        audio_stream: 0,
        graphic: None,
    }
}

/// A sequence with one video track holding `clips`.
fn sequence(p: &mut Project, name: &str, clips: &[(ItemId, i64, i64)]) -> ItemId {
    let id = p.new_sequence(name, SequenceSettings::default(), 1, 0, None);
    let items: Vec<TrackItem> = clips.iter().map(|(i, s, d)| clip(p, *i, *s, *d)).collect();
    seq_mut(p, id).video_tracks[0].items = items;
    id
}

/// The sequence of item `id`.
fn seq_mut(p: &mut Project, id: ItemId) -> &mut Sequence {
    p.sequence_mut(id).expect("sequence")
}

fn paths(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The active sequence's media comes first; the other sequences' media is the rest.
#[test]
fn active_sequence_first() {
    let mut p = Project::new("t");
    let (a, b, c) = (media(&mut p, "/r/a.webm"), media(&mut p, "/r/b.webm"), media(&mut p, "/r/c.webm"));
    let s1 = sequence(&mut p, "S1", &[(a, 0, 100)]);
    let s2 = sequence(&mut p, "S2", &[(b, 0, 100)]);
    let _s3 = sequence(&mut p, "S3", &[(c, 0, 100)]);
    let view = ProjectView { active_sequence: Some(s2), ..Default::default() };
    let pl = plan(&p, Some(&view));
    assert_eq!(pl.focus, paths(&["/r/b.webm"]));
    assert_eq!(pl.rest, paths(&["/r/a.webm", "/r/c.webm"]));
    // no view: the first sequence, as the engine opens it
    let pl = plan(&p, None);
    assert_eq!(pl.focus, paths(&["/r/a.webm"]));
    let _ = s1;
}

/// Media under the playhead leads the focus list.
#[test]
fn playhead_media_first() {
    let mut p = Project::new("t");
    let (a, b) = (media(&mut p, "/r/early.mp4"), media(&mut p, "/r/late.mp4"));
    let s = sequence(&mut p, "S", &[(a, 0, 100), (b, 500, 100)]);
    let mut view = ProjectView { active_sequence: Some(s), ..Default::default() };
    assert_eq!(plan(&p, Some(&view)).focus, paths(&["/r/early.mp4", "/r/late.mp4"]));
    view.playheads.insert(s, Tick(550));
    assert_eq!(plan(&p, Some(&view)).focus, paths(&["/r/late.mp4", "/r/early.mp4"]));
}

/// A nested sequence's media belongs to the sequence that nests it; a sequence that nests itself ends.
#[test]
fn nested_sequences_and_cycles() {
    let mut p = Project::new("t");
    let (a, b, other) = (media(&mut p, "/r/a.mp4"), media(&mut p, "/r/b.mp4"), media(&mut p, "/r/other.mp4"));
    let inner = sequence(&mut p, "Inner", &[(b, 0, 100)]);
    let outer = sequence(&mut p, "Outer", &[(a, 0, 100), (inner, 100, 100)]);
    let _unrelated = sequence(&mut p, "Other", &[(other, 0, 100)]);
    // the inner sequence nests the outer one: a cycle
    let back = clip(&mut p, outer, 100, 100);
    seq_mut(&mut p, inner).video_tracks[0].items.push(back);
    let selfref = clip(&mut p, outer, 300, 100);
    seq_mut(&mut p, outer).video_tracks[0].items.push(selfref);
    let view = ProjectView { active_sequence: Some(outer), ..Default::default() };
    let pl = plan(&p, Some(&view));
    assert_eq!(pl.focus, paths(&["/r/a.mp4", "/r/b.mp4"]));
    assert_eq!(pl.rest, paths(&["/r/other.mp4"]));
}

/// A subclip refers to its parent's file; a missing item is skipped; a file used twice is listed once.
#[test]
fn subclips_missing_items_and_duplicates() {
    let mut p = Project::new("t");
    let a = media(&mut p, "/r/a.mp4");
    let sub = p.add_item(
        "sub",
        Label::Iris,
        ItemKind::Subclip { parent: a, range: filmcraft_time::TimeRange::new(Tick::ZERO, Tick(10)), restrict_trims: false },
        None,
    );
    let s = sequence(&mut p, "S", &[(sub, 0, 10), (a, 10, 10), (ItemId(9999), 20, 10)]);
    let view = ProjectView { active_sequence: Some(s), ..Default::default() };
    assert_eq!(plan(&p, Some(&view)).focus, paths(&["/r/a.mp4"]));
}

/// A view naming a sequence that does not exist falls back to the first one; no sequence: all media is the rest.
#[test]
fn unknown_active_and_no_sequence() {
    let mut p = Project::new("t");
    let a = media(&mut p, "/r/a.mp4");
    let view = ProjectView { active_sequence: Some(ItemId(777)), ..Default::default() };
    assert_eq!(plan(&p, Some(&view)), Plan { focus: vec![], rest: paths(&["/r/a.mp4"]) });
    let _s = sequence(&mut p, "S", &[(a, 0, 10)]);
    assert_eq!(plan(&p, Some(&view)).focus, paths(&["/r/a.mp4"]));
    assert_eq!(plan(&Project::new("empty"), None), Plan::default());
}

/// A clip with a huge start or duration (hostile project) neither overflows nor panics.
#[test]
fn extreme_clip_times() {
    let mut p = Project::new("t");
    let a = media(&mut p, "/r/a.mp4");
    let s = sequence(&mut p, "S", &[(a, i64::MAX, i64::MAX), (a, i64::MIN, i64::MAX)]);
    let view = ProjectView { active_sequence: Some(s), ..Default::default() };
    assert_eq!(plan(&p, Some(&view)).focus, paths(&["/r/a.mp4"]));
}

/// A project file round-trips into a plan; garbage and hostile JSON give an empty plan.
#[test]
fn bytes_roundtrip_and_hostile_input() {
    let mut p = Project::new("t");
    let a = media(&mut p, "/r/a.mp4");
    let s = sequence(&mut p, "S", &[(a, 0, 10)]);
    let view = ProjectView { active_sequence: Some(s), ..Default::default() };
    let bytes = filmcraft_format::encode_with_view(&p, Some(&view), false);
    assert_eq!(plan_bytes(&bytes).focus, paths(&["/r/a.mp4"]));
    for bad in [&b""[..], b"not json", b"{}", b"[]", b"null", b"{\"schema_version\":4294967295}", &[0xff, 0xfe, 0x00][..]] {
        assert_eq!(plan_bytes(bad), Plan::default());
    }
}
