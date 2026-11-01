use super::*;
use filmcraft_media::{DemoScene, MediaSource};

fn demo_project() -> (Project, ItemId, ItemId) {
    let mut p = Project::new("Test");
    let src = filmcraft_media::generators::GeneratorSource::demo(DemoScene::OceanSunset);
    let info = src.info().clone();
    let clip = p.add_item(
        "Ocean_Sunset.mp4",
        Label::Iris,
        ItemKind::Media(Box::new(MediaClip {
            media: MediaRef::Generator(Generator::Demo(DemoScene::OceanSunset)),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        })),
        None,
    );
    let seq = p.new_sequence("Sequence 01", SequenceSettings::default(), 3, 3, None);
    (p, clip, seq)
}

#[test]
fn json_roundtrip() {
    let (mut p, clip, seq) = demo_project();
    let rate = p.sequence(seq).unwrap().settings.frame_rate;
    let mut ti = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(5 * TICKS_PER_SECOND)), rate).unwrap();
    for e in &mut ti.effects {
        resolve_auto_points(e, (1920, 1080), (1920, 1080));
    }
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    let s = p.to_json();
    let q = Project::from_json(&s).unwrap();
    assert_eq!(p, q);
    assert!(q.sequence(seq).unwrap().check().is_ok());
}

#[test]
fn hostile_frame_rates_are_rejected_before_frame_arithmetic() {
    for frame_rate in [FrameRate { num: i64::MAX, den: i64::MAX }, FrameRate { num: 1, den: i64::from(u32::MAX) }] {
        let settings = SequenceSettings { frame_rate, ..Default::default() };
        assert!(settings.validate().is_err());
    }
    for (width, height) in [(7680, 4320), (15360, 8640), (16384, 8192), (16384, 16384), (32768, 8192)] {
        assert!(SequenceSettings { width, height, ..Default::default() }.validate().is_ok(), "{width}x{height}");
    }
    assert!(validate_frame_size(32768, 16384).is_err());
    assert!(validate_frame_size(65536, 1).is_err());
}

#[test]
fn corrupt_timeline_times_are_rejected_before_end_arithmetic() {
    let (mut project, media, seq) = demo_project();
    let rate = project.sequence(seq).unwrap().settings.frame_rate;
    let range = TimeRange::new(Tick::ZERO, rate.tick_of(24));
    let first = project.make_track_item(media, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
    let second = project.make_track_item(media, TrackKind::Video, rate.tick_of(24), range, rate).unwrap();
    project.sequence_mut(seq).unwrap().video_tracks[0].items = vec![first, second];
    for start in [Tick(i64::MAX), Tick(i64::MIN), Tick::MAX] {
        project.sequence_mut(seq).unwrap().video_tracks[0].items[0].start = start;
        let result = std::panic::catch_unwind(|| project.sequence(seq).unwrap().check());
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
    project.sequence_mut(seq).unwrap().video_tracks[0].items[0].start = Tick::ZERO;
    for mark in [Tick(i64::MAX), Tick(i64::MIN)] {
        project.sequence_mut(seq).unwrap().mark_out = Some(mark);
        assert!(project.sequence(seq).unwrap().check().is_err());
    }
}

#[test]
fn point_params_keep_auto_through_json_and_no_other_point_reads_null() {
    // serde_json writes NaN as `null`; a point parameter reads it back as NaN ("auto")
    let json = serde_json::to_string(&ParamValue::Vec2(Vec2::new(f64::NAN, 540.0))).unwrap();
    assert_eq!(json, r#"{"Vec2":{"x":null,"y":540.0}}"#);
    let v = serde_json::from_str::<ParamValue>(&json).unwrap().as_vec2().unwrap();
    assert!(v.x.is_nan() && v.y == 540.0);
    let both = serde_json::from_str::<ParamValue>(r#"{"Vec2":{"x":null,"y":null}}"#).unwrap().as_vec2().unwrap();
    assert!(both.x.is_nan() && both.y.is_nan());
    assert_eq!(serde_json::from_str::<ParamValue>(r#"{"Vec2":{"x":1.5,"y":-2}}"#).unwrap(), ParamValue::Vec2(Vec2::new(1.5, -2.0)));
    // anything else is still an error, and so is a missing coordinate
    for bad in [r#"{"Vec2":{"x":"a","y":0}}"#, r#"{"Vec2":{"x":0}}"#, r#"{"Vec2":null}"#, r#"{"Float":null}"#] {
        assert!(serde_json::from_str::<ParamValue>(bad).is_err(), "{bad}");
    }
    // only point parameters: a `null` coordinate in a mask vertex (or any other point) is damage
    assert!(serde_json::from_str::<Vec2>(r#"{"x":null,"y":0}"#).is_err());
    let mut path = serde_json::to_value(MaskPath::ellipse(Vec2::new(100.0, 100.0), Vec2::new(50.0, 40.0))).unwrap();
    assert!(serde_json::from_value::<MaskPath>(path.clone()).is_ok());
    path["vertices"][0]["p"]["x"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<MaskPath>(path.clone()).is_err());
    assert!(serde_json::from_value::<ParamValue>(serde_json::json!({ "Path": path })).is_err());
}

#[test]
fn media_resolution_bounds_depth_and_preserves_the_outer_range() {
    let (mut p, media, _) = demo_project();
    assert_eq!(p.resolve_media(media).map(|(id, _, range)| (id, range)), Some((media, None)));
    let streams = p.source_streams(media);
    let mut parent = media;
    let mut last_range = TimeRange::default();
    for n in 0..15 {
        last_range = TimeRange::new(Tick(n * 1000), Tick(100));
        parent = p.add_item("Sub", Label::Iris, ItemKind::Subclip { parent, range: last_range, restrict_trims: false }, None);
    }
    assert_eq!(p.resolve_media(parent).map(|(id, _, range)| (id, range)), Some((media, Some(last_range))));
    assert_eq!(p.source_streams(parent), streams);
    let too_deep = p.add_item("Too deep", Label::Iris, ItemKind::Subclip { parent, range: last_range, restrict_trims: false }, None);
    assert!(p.resolve_media(too_deep).is_none());
    assert!(p.resolve_media(ItemId(u64::MAX)).is_none());
    assert_eq!(p.source_streams(too_deep), (false, false));
    assert_eq!(p.source_streams(ItemId(u64::MAX)), (false, false));
    p.item_mut(parent).unwrap().kind = ItemKind::Subclip { parent, range: last_range, restrict_trims: false };
    assert_eq!(p.source_streams(parent), (false, false));
}

#[test]
fn placed_auto_points_resolve_once_the_source_size_is_final() {
    let (mut p, clip, seq) = demo_project();
    let src = p.item(clip).and_then(|i| i.as_media()).and_then(|m| m.info.video.clone()).unwrap();
    let rate = p.sequence(seq).unwrap().settings.frame_rate;
    let second = TimeRange::new(Tick::ZERO, Tick(TICKS_PER_SECOND));
    let sub = p.add_item("Sub", Label::Iris, ItemKind::Subclip { parent: clip, range: second, restrict_trims: true }, None);
    let sub_of_sub = p.add_item("Sub 2", Label::Iris, ItemKind::Subclip { parent: sub, range: second, restrict_trims: true }, None);
    assert_eq!(p.source_size(clip), Some((src.width, src.height)));
    assert_eq!((p.source_size(sub), p.source_size(sub_of_sub)), (p.source_size(clip), p.source_size(clip)), "a subclip has its parent's size");
    assert_eq!(p.source_size(ItemId(987_654_321)), None);
    // clips whose Motion position / anchor were never resolved (as an interchange import builds them)
    let mut ids = Vec::new();
    for (n, item) in [clip, sub, sub_of_sub].into_iter().enumerate() {
        let ti = p.make_track_item(item, TrackKind::Video, Tick(n as i64 * TICKS_PER_SECOND), second, rate).unwrap();
        assert!(ti.effect("motion").unwrap().vec2_at("position", Tick::ZERO).x.is_nan());
        ids.push(ti.id);
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    }
    let st = p.sequence(seq).unwrap().settings.clone();
    let point = |p: &Project, id: ClipId, k: &str| p.sequence(seq).unwrap().find_item(id).unwrap().1.effect("motion").unwrap().vec2_at(k, Tick::ZERO);
    let (frame_centre, source_centre) = (Vec2::new(st.width as f64 / 2.0, st.height as f64 / 2.0), Vec2::new(src.width as f64 / 2.0, src.height as f64 / 2.0));
    // sequences that are not selected are left alone
    let mut untouched = p.clone();
    untouched.resolve_placed_auto_points(|_| false, |_| true);
    assert!(point(&untouched, ids[0], "position").x.is_nan());
    // the source size is not final yet: the frame's points resolve, the anchor stays auto
    p.resolve_placed_auto_points(|_| true, |_| false);
    for id in &ids {
        assert_eq!(point(&p, *id, "position"), frame_centre);
        assert!(point(&p, *id, "anchor").x.is_nan() && point(&p, *id, "anchor").y.is_nan());
    }
    // ...and survives a save: NaN is written as `null` and read back as NaN
    let mut q = Project::from_json(&p.to_json()).unwrap();
    assert!(point(&q, ids[0], "anchor").x.is_nan());
    assert_eq!(q.to_json(), p.to_json());
    // final for the media item only: its clip and the clips of its subclips get the source centre
    q.resolve_placed_auto_points(|_| true, |i| i.id == clip);
    for id in &ids {
        assert_eq!((point(&q, *id, "position"), point(&q, *id, "anchor")), (frame_centre, source_centre));
    }
    assert!(!q.to_json().contains("null}}"), "no NaN point is left to write");
    // running it again changes nothing
    let once = q.clone();
    q.resolve_placed_auto_points(|_| true, |_| true);
    assert_eq!(q, once);
}

#[test]
fn item_at_and_source_time() {
    let (mut p, clip, seq) = demo_project();
    let rate = FrameRate::FPS_24;
    let mut a = p.make_track_item(clip, TrackKind::Video, Tick(0), TimeRange::new(Tick(1000), rate.tick_of(48)), rate).unwrap();
    a.speed = 2.0;
    let t = &mut p.sequence_mut(seq).unwrap().video_tracks[0];
    t.items.push(a.clone());
    assert_eq!(t.item_at(rate.tick_of(10)).unwrap().id, a.id);
    assert!(t.item_at(rate.tick_of(48)).is_none());
    assert_eq!(a.source_time_at(rate.tick_of(10)), Tick(1000) + rate.tick_of(20));
}

#[test]
fn projects_without_caption_tracks_load() {
    let (p, _, seq) = demo_project();
    let mut v: serde_json::Value = serde_json::from_str(&p.to_json()).unwrap();
    // a v1 file has no `caption_tracks` key
    let items = v["items"].as_object_mut().unwrap();
    for it in items.values_mut() {
        if let Some(s) = it["kind"].get_mut("Sequence") {
            s.as_object_mut().unwrap().remove("caption_tracks");
        }
    }
    let q = Project::from_json(&v.to_string()).unwrap();
    assert!(q.sequence(seq).unwrap().caption_tracks.is_empty());
}

#[test]
fn caption_tracks_roundtrip() {
    let (mut p, _, seq) = demo_project();
    let id = TrackId(p.alloc_id());
    let mut ct = CaptionTrack::new(id, "Subtitle".into(), CaptionFormat::Cea608);
    ct.captions.push(Caption {
        id: ClipId(p.alloc_id()),
        start: Tick(10),
        duration: Tick(TICKS_PER_SECOND),
        text: "Hello\n<i>world</i>".into(),
        speaker: Some("Ann".into()),
        cue_id: Some("1".into()),
        settings: "line:90%".into(),
    });
    p.sequence_mut(seq).unwrap().caption_tracks.push(ct);
    let q = Project::from_json(&p.to_json()).unwrap();
    assert_eq!(p, q);
    assert_eq!(q.sequence(seq).unwrap().duration(), Tick(10 + TICKS_PER_SECOND));
}

#[test]
fn bins() {
    let mut p = Project::new("x");
    let b = p.add_bin("Footage", None);
    let i = p.add_item("a", Label::Iris, ItemKind::AdjustmentLayer { width: 10, height: 10, rate: FrameRate::FPS_24, duration: Tick(1) }, Some(b));
    assert_eq!(p.root.parent_of(i), Some(b));
    assert!(p.root.remove_item(i));
    assert_eq!(p.root.parent_of(i), None);
}

#[test]
fn a_bin_moves_into_another_bin_with_its_contents() {
    let mut p = Project::new("x");
    let outer = p.add_bin("Footage", None);
    let inner = p.add_bin("B-Roll", None);
    let deep = p.add_bin("Drone", Some(inner));
    let i = p.add_item("a", Label::Iris, ItemKind::AdjustmentLayer { width: 10, height: 10, rate: FrameRate::FPS_24, duration: Tick(1) }, Some(deep));
    assert_eq!(p.move_to_bin(&[ItemId(inner.0)], Some(outer)), Some(1));
    let o = p.root.find_bin(outer).unwrap();
    assert!(o.children.iter().any(|c| matches!(c, BinEntry::Bin(b) if b.id == inner)));
    assert!(!p.root.children.iter().any(|c| matches!(c, BinEntry::Bin(b) if b.id == inner)));
    assert_eq!(p.root.parent_of(i), Some(deep));
    assert!(o.find_bin(deep).is_some());
    // never into itself or its own sub-bin, and the root never moves
    assert_eq!(p.move_to_bin(&[ItemId(outer.0)], Some(outer)), Some(0));
    assert_eq!(p.move_to_bin(&[ItemId(outer.0)], Some(deep)), Some(0));
    assert_eq!(p.move_to_bin(&[ItemId(p.root.id.0)], Some(outer)), Some(0));
    assert!(p.root.find_bin(outer).unwrap().find_bin(deep).is_some());
    // and back out to the root
    assert_eq!(p.move_to_bin(&[ItemId(inner.0)], None), Some(1));
    assert!(p.root.children.iter().any(|c| matches!(c, BinEntry::Bin(b) if b.id == inner)));
}

#[test]
fn project_clone_shares_sequences_until_mutated() {
    fn sequence_arc(p: &Project, id: ItemId) -> &std::sync::Arc<Sequence> {
        match &p.item(id).unwrap().kind {
            ItemKind::Sequence(q) => q,
            _ => unreachable!(),
        }
    }

    let (mut p, _, a) = demo_project();
    let b = p.new_sequence("Sequence 02", SequenceSettings::default(), 1, 1, None);
    let before = p.clone();

    assert!(std::sync::Arc::ptr_eq(sequence_arc(&p, a), sequence_arc(&before, a)));
    assert!(std::sync::Arc::ptr_eq(sequence_arc(&p, b), sequence_arc(&before, b)));

    p.sequence_mut(a).unwrap().mark_in = Some(Tick(1));

    assert!(!std::sync::Arc::ptr_eq(sequence_arc(&p, a), sequence_arc(&before, a)));
    assert!(std::sync::Arc::ptr_eq(sequence_arc(&p, b), sequence_arc(&before, b)));
    assert_eq!(before.sequence(a).unwrap().mark_in, None);
    assert_eq!(Project::from_json(&p.to_json()).unwrap(), p);
}

#[test]
fn nest_cycle_finds_sequences_that_contain_themselves() {
    let (mut p, clip, a) = demo_project();
    let rate = p.sequence(a).unwrap().settings.frame_rate;
    let b = p.new_sequence("b", SequenceSettings::default(), 1, 1, None);
    let c = p.new_sequence("c", SequenceSettings::default(), 1, 1, None);
    let put = |p: &mut Project, outer: ItemId, inner: ItemId| {
        let it = p.make_track_item(inner, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(10)), rate).unwrap();
        let id = it.id;
        p.sequence_mut(outer).unwrap().video_tracks[0].items.push(it);
        id
    };
    // media and a chain of nests (a in b in c, a twice in c) are sound
    put(&mut p, a, clip);
    put(&mut p, b, a);
    put(&mut p, c, b);
    put(&mut p, c, a);
    assert_eq!(p.nest_cycle(), None);
    // c inside a closes the loop a → c → b → a
    let closing = put(&mut p, a, c);
    assert_eq!(p.nest_cycle(), Some(a));
    p.sequence_mut(a).unwrap().video_tracks[0].items.retain(|i| i.id != closing);
    assert_eq!(p.nest_cycle(), None);
    // a sequence directly inside itself; one that only leads into the loop is not reported
    put(&mut p, c, c);
    let d = p.new_sequence("d", SequenceSettings::default(), 1, 1, None);
    put(&mut p, d, c);
    assert_eq!(p.nest_cycle(), Some(c));
}

#[test]
fn nest_overhang_is_the_part_past_the_contents() {
    let (mut p, clip, a) = demo_project();
    let rate = p.sequence(a).unwrap().settings.frame_rate;
    let f = |n: i64| rate.tick_of(n);
    // a holds 100 frames of media; b nests a for 150 frames starting at frame 10
    let media = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, f(100)), rate).unwrap();
    assert_eq!(p.nest_overhang(&media), None, "not a nest");
    p.sequence_mut(a).unwrap().video_tracks[0].items.push(media);
    let mut nest = p.make_track_item(a, TrackKind::Video, f(10), TimeRange::new(Tick::ZERO, f(150)), rate).unwrap();
    assert_eq!(p.nest_overhang(&nest), Some(TimeRange::new(f(110), f(50))));
    // within the contents, or exactly to their end: none
    nest.duration = f(100);
    assert_eq!(p.nest_overhang(&nest), None);
    // trimmed in: 60 frames of contents are left
    nest.source_in = f(40);
    assert_eq!(p.nest_overhang(&nest), Some(TimeRange::new(f(70), f(40))));
    // starting past the end: all of it
    nest.source_in = f(100);
    assert_eq!(p.nest_overhang(&nest), Some(nest.range()));
    // double speed uses the contents up twice as fast
    nest.source_in = Tick::ZERO;
    nest.speed = 2.0;
    assert_eq!(p.nest_overhang(&nest), Some(TimeRange::new(f(60), f(50))));
    // reversed, the empty part comes first
    nest.reverse = true;
    assert_eq!(p.nest_overhang(&nest), Some(TimeRange::new(f(10), f(50))));
    // a frame hold shows one frame throughout
    nest.reverse = false;
    nest.speed = 1.0;
    nest.frame_hold = Some(f(20));
    assert_eq!(p.nest_overhang(&nest), None);
    nest.frame_hold = Some(f(100));
    assert_eq!(p.nest_overhang(&nest), Some(nest.range()));
}

#[test]
fn pixel_aspect_ratios_are_reduced_and_hostile_ones_are_square() {
    assert_eq!(checked_par((4, 3)), Some((4, 3)));
    assert_eq!(checked_par((40, 30)), Some((4, 3)));
    assert_eq!(checked_par((u32::MAX, u32::MAX)), Some((1, 1)));
    assert_eq!(checked_par((u32::MAX, u32::MAX - 1)), Some((u32::MAX, u32::MAX - 1)));
    assert_eq!(checked_par((8, 1)), Some((8, 1)));
    assert_eq!(checked_par((1, 8)), Some((1, 8)));
    for bad in [(0, 0), (0, 1), (1, 0), (9, 1), (1, 9), (u32::MAX, 1), (1, u32::MAX)] {
        assert_eq!(checked_par(bad), None, "{bad:?}");
        assert_eq!(sane_par(bad), (1, 1), "{bad:?}");
        assert_eq!(par_ratio(bad), 1.0, "{bad:?}");
    }
    assert!((par_ratio((4, 3)) - 4.0 / 3.0).abs() < 1e-12);
    // 1440 x 1080 with 4:3 pixels is 1920 x 1080 square pixels, and itself in a 4:3 frame
    assert_eq!(conformed_size((1440, 1080), (4, 3), (1, 1)), (1920.0, 1080.0));
    assert_eq!(conformed_size((1440, 1080), (4, 3), (8, 6)), (1440.0, 1080.0));
    assert_eq!(conformed_size((1920, 1080), (1, 1), (4, 3)), (1440.0, 1080.0));
    assert_eq!(conformed_size((u32::MAX, u32::MAX), (8, 1), (1, 8)), (u32::MAX as f64 * 64.0, u32::MAX as f64));
    assert_eq!(conformed_size((1440, 1080), (0, 0), (u32::MAX, 1)), (1440.0, 1080.0));
}

#[test]
fn source_par_follows_interpret_footage_subclips_and_sequences() {
    let (mut p, clip, seq) = demo_project();
    assert_eq!(p.source_par(clip), Some((1, 1)));
    let sub = p.add_item("Sub", Label::Iris, ItemKind::Subclip { parent: clip, range: TimeRange::new(Tick::ZERO, Tick(100)), restrict_trims: false }, None);
    fn media(p: &mut Project, id: ItemId) -> &mut MediaClip {
        match &mut p.item_mut(id).unwrap().kind {
            ItemKind::Media(m) => m,
            _ => unreachable!(),
        }
    }
    // the file's ratio, reduced
    media(&mut p, clip).info.video.as_mut().unwrap().par = (40, 30);
    assert_eq!((p.source_par(clip), p.source_par(sub)), (Some((4, 3)), Some((4, 3))));
    assert_eq!(p.conformed_source_size(clip, (1, 1)), Some((1920.0 * 4.0 / 3.0, 1080.0)));
    // Interpret Footage wins over the file
    media(&mut p, clip).interpret.par = Some((2, 1));
    assert_eq!(p.source_par(sub), Some((2, 1)));
    // a nonsensical override falls back to the file, a nonsensical file value to square
    media(&mut p, clip).interpret.par = Some((0, 7));
    assert_eq!(p.source_par(clip), Some((4, 3)));
    media(&mut p, clip).info.video.as_mut().unwrap().par = (u32::MAX, 1);
    assert_eq!(p.source_par(clip), Some((1, 1)));
    // a nested sequence has its own ratio; graphics and missing items have none
    p.sequence_mut(seq).unwrap().settings.par = (0, 0);
    assert_eq!(p.source_par(seq), Some((1, 1)));
    p.sequence_mut(seq).unwrap().settings.par = (8, 6);
    assert_eq!(p.source_par(seq), Some((4, 3)));
    assert_eq!(p.conformed_source_size(seq, (1, 1)), Some((2560.0, 1080.0)));
    let adj = p.add_item("Adj", Label::Iris, ItemKind::AdjustmentLayer { width: 1440, height: 1080, rate: FrameRate::FPS_24, duration: Tick(100) }, None);
    assert_eq!(p.source_par(adj), None);
    assert_eq!(p.conformed_source_size(adj, (4, 3)), Some((1440.0, 1080.0)));
    assert_eq!(p.source_par(ItemId(987_654_321)), None);
}

// ---------- time remapping ----------

/// Set the Time Remapping speed of a clip (static value, keyframes cleared).
fn set_remap_speed(it: &mut TrackItem, percent: f64) {
    let e = it.effect_mut("time_remap").expect("intrinsic time_remap");
    e.params.insert("speed".into(), Param::new(ParamValue::Float(percent)));
}

/// Replace the Time Remapping speed with a keyframed curve (absolute media times, percent).
fn set_remap_curve(it: &mut TrackItem, keys: &[(i64, f64)]) {
    let e = it.effect_mut("time_remap").expect("intrinsic time_remap");
    let mut p = Param::new(ParamValue::Float(100.0));
    for (m, v) in keys {
        p.put_keyframe(Tick(*m), ParamValue::Float(*v));
    }
    e.params.insert("speed".into(), p);
}

fn remapped_clip(p: &mut Project, clip: ItemId, seq: ItemId, seconds: i64) -> (TrackItem, i64) {
    // 24 fps: whole seconds snap to whole frames, so the assertions stay exact
    p.sequence_mut(seq).unwrap().settings.frame_rate = FrameRate::FPS_24;
    let it = p.make_track_item(clip, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, Tick(seconds * TICKS_PER_SECOND)), FrameRate::FPS_24).unwrap();
    let s = TICKS_PER_SECOND;
    (it, s)
}

#[test]
fn time_remap_speed_scales_the_shown_media_time() {
    let (mut p, clip, seq) = demo_project();
    let (mut it, s) = remapped_clip(&mut p, clip, seq, 4);
    // 100 %: one second of media per second of timeline
    assert_eq!(it.source_time_at(Tick(s)), Tick(s));
    assert_eq!(it.source_out(), Tick(4 * s));
    assert_eq!(it.effective_speed(), 1.0);
    // 50 %: half the media over the same timeline span
    set_remap_speed(&mut it, 50.0);
    assert_eq!(it.effective_speed(), 0.5);
    assert_eq!(it.source_time_at(Tick(s)), Tick(s / 2));
    assert_eq!(it.source_out(), Tick(2 * s));
    assert_eq!(it.source_out_unremapped(), Tick(4 * s), "Speed/Duration still scales the base factor");
    // 200 %: twice the media
    set_remap_speed(&mut it, 200.0);
    assert_eq!(it.source_time_at(Tick(s)), Tick(2 * s));
    assert_eq!(it.source_out(), Tick(8 * s));
    // the remap composes with Speed/Duration
    it.speed = 2.0;
    assert_eq!(it.source_time_at(Tick(s)), Tick(4 * s), "200 % Speed/Duration x 200 % Time Remapping");
}

#[test]
fn time_remap_ramp_integrates_the_speed_curve() {
    let (mut p, clip, seq) = demo_project();
    let (mut it, s) = remapped_clip(&mut p, clip, seq, 8);
    // constant 100 % over media [0, 1 s), then a linear ramp 100 % -> 300 % over media [1 s, 2 s),
    // then a constant 300 %: the mapping integrates the rate, so the ramp segment takes
    // ln(3)/2 ≈ 0.5493 s of timeline time and √2 − 1 ≈ 0.4142 s of media is half of it
    set_remap_curve(&mut it, &[(0, 100.0), (s, 100.0), (2 * s, 300.0)]);
    let ln3_2 = (3.0f64).ln() / 2.0;
    assert_eq!(it.source_time_at(Tick(s / 2)).0, Tick(s / 2).0, "constant segment");
    // the ramp starts at timeline 1 s and takes ln(3)/2 s
    let cross = s + (ln3_2 * s as f64).round() as i64;
    let at_cross = it.source_time_at(Tick(cross));
    assert!((at_cross.0 as f64 / (2.0 * s as f64) - 1.0).abs() < 1e-6, "the ramp ends at media 2 s, got {}", at_cross.0 as f64 / s as f64);
    let half = s + (ln3_2 / 2.0 * s as f64).round() as i64;
    let at_half = it.source_time_at(Tick(half));
    let want = 1.0 + ((3.0f64).sqrt() - 1.0) / 2.0;
    assert!((at_half.0 as f64 / s as f64 - want).abs() < 1e-3, "half the ramp reaches media {} s, got {}", want, at_half.0 as f64 / s as f64);
    // after the ramp the rate is a constant 300 %
    let late = cross + s / 2;
    let at_late = it.source_time_at(Tick(late));
    assert!((at_late.0 as f64 / s as f64 - 3.5).abs() < 1e-3, "300 % after the ramp, got {}", at_late.0 as f64 / s as f64);
    // the covered span: the ramp plus 300 % for the rest of the 8 s clip
    let span = (it.source_out() - it.source_in).0 as f64 / s as f64;
    let want_span = 2.0 + 3.0 * (8.0 - ln3_2 - 1.0);
    assert!((span - want_span).abs() < 1e-3, "span {span} vs {want_span}");
    // the inverse mapping lands back on the timeline times it came from
    for f in [0.25, 0.5, 1.0, 1.2, 1.5, 2.0, 5.5] {
        let t = Tick((f * s as f64).round() as i64);
        let m = it.source_time_at(t);
        let back = it.timeline_time_of_media(m).expect("media time within the clip");
        assert!((back - t).0.abs() <= rate_frame(&p, seq).0, "media {} s maps back to {} s, wanted {} s", m.0 as f64 / s as f64, back.0 as f64 / s as f64, f);
    }
}

#[test]
fn time_remap_ramp_is_monotonic_and_continuous() {
    let (mut p, clip, seq) = demo_project();
    let (mut it, s) = remapped_clip(&mut p, clip, seq, 6);
    set_remap_curve(&mut it, &[(0, 25.0), (s, 400.0), (3 * s, 50.0), (5 * s, 150.0)]);
    let mut prev = it.source_time_at(Tick::ZERO);
    for i in 1..=600 {
        let m = it.source_time_at(Tick(i * 10 * s / 600));
        assert!(m >= prev, "media time must not go back at {} s", i as f64 / 60.0);
        prev = m;
    }
    let span = (it.source_out().0 as f64 - it.source_in.0 as f64) / s as f64;
    assert!(span > 0.0 && span.is_finite());
}

#[test]
fn time_remap_hold_keeps_the_left_rate() {
    let (mut p, clip, seq) = demo_project();
    let (mut it, s) = remapped_clip(&mut p, clip, seq, 4);
    let e = it.effect_mut("time_remap").unwrap();
    let mut prm = Param::new(ParamValue::Float(100.0));
    let mut k0 = Keyframe::new(Tick(0), ParamValue::Float(100.0));
    k0.interp = Interpolation::Hold;
    prm.keyframes.push(k0);
    let mut k = Keyframe::new(Tick(s), ParamValue::Float(300.0));
    k.interp = Interpolation::Hold;
    prm.keyframes.push(k);
    e.params.insert("speed".into(), prm);
    // the hold keeps 100 % until media 1 s, then the rate steps to 300 %
    assert_eq!(it.source_time_at(Tick(s)), Tick(s));
    assert_eq!(it.source_time_at(Tick(s + s / 2)), Tick(s + 3 * s / 2));
}

#[test]
fn time_remap_freeze_reverse_and_single_keyframe() {
    let (mut p, clip, seq) = demo_project();
    let (mut it, s) = remapped_clip(&mut p, clip, seq, 4);
    // 0 % freezes on the frame it reached (the integration floor crawls at a millionth of the rate)
    set_remap_curve(&mut it, &[(0, 0.0), (s, 0.0)]);
    assert!(it.source_time_at(Tick(s)).0 < 1_000_000, "a 0 % section barely moves");
    assert!(it.source_out() > it.source_in, "the span stays representable");
    // one keyframe: the curve holds its value everywhere
    set_remap_curve(&mut it, &[(3 * s, 200.0)]);
    assert_eq!(it.source_time_at(Tick(s)), Tick(2 * s));
    assert_eq!(it.source_out(), Tick(8 * s));
    // reversed: the walk starts at the covered span's far end and runs back to the In point
    set_remap_speed(&mut it, 100.0);
    it.reverse = true;
    assert_eq!(it.source_time_at(Tick::ZERO), Tick(4 * s) - Tick(1));
    assert_eq!(it.source_time_at(Tick(4 * s) - Tick(1)), Tick(0));
    set_remap_curve(&mut it, &[(0, 100.0), (s, 100.0), (2 * s, 300.0)]);
    let far = it.source_time_at(Tick::ZERO);
    assert!(far > it.source_time_at(Tick(2 * s)), "a reversed clip walks back through the curve");
    let last_shown = it.source_time_at((it.end() - Tick(1)).max(it.start));
    assert!(it.timeline_time_of_media(last_shown).is_some(), "the media at the end maps back into the clip");
}

#[test]
fn time_remap_hostile_values_fall_back_to_normal_speed() {
    let (mut p, clip, seq) = demo_project();
    let rate = p.sequence(seq).unwrap().settings.frame_rate;
    let (mut it, s) = remapped_clip(&mut p, clip, seq, 4);
    // a non-finite static value stands for 100 %
    set_remap_speed(&mut it, f64::NAN);
    assert_eq!(it.source_time_at(rate.tick_of(12)), rate.tick_of(12));
    assert_eq!(it.source_out(), Tick(4 * s));
    set_remap_speed(&mut it, f64::INFINITY);
    assert_eq!(it.source_out(), Tick(4 * s));
    // a NaN keyframe among real ones acts as 100 % there, nothing panics, the walk stays finite
    set_remap_curve(&mut it, &[(0, 100.0), (s, f64::NAN), (3 * s, 100.0)]);
    for i in 0..=40 {
        let m = it.source_time_at(Tick(i * s / 10));
        assert!(m.0.checked_abs().is_some(), "media time stays in range");
    }
    // a hostile base speed: the curve is refused and the constant formula saturates safely
    set_remap_curve(&mut it, &[(0, 200.0), (s, 200.0)]);
    it.speed = 0.0;
    assert_eq!(it.source_time_at(Tick(s)), Tick(0), "a 0 base speed freezes at the In point");
    it.speed = f64::NAN;
    assert_eq!(it.source_time_at(Tick(s)), it.source_in, "NaN falls back to the In point");
}

#[test]
fn time_remap_inverse_maps_media_time_to_the_timeline() {
    let (mut p, clip, seq) = demo_project();
    let (mut it, s) = remapped_clip(&mut p, clip, seq, 4);
    // static 50 %: media 0.5 s shows one second in
    set_remap_speed(&mut it, 50.0);
    assert_eq!(it.timeline_time_of_media(Tick(s / 2)), Some(Tick(s)));
    assert_eq!(it.timeline_time_of_media(Tick(4 * s)), None, "media past the clip's out point");
    // reversed: the In point shows at the end
    set_remap_speed(&mut it, 100.0);
    it.reverse = true;
    assert_eq!(it.timeline_time_of_media(Tick(0)), Some(Tick(4 * s) - Tick(1)));
    it.reverse = false;
    // a curve: the ramp segment's middle
    set_remap_curve(&mut it, &[(0, 100.0), (s, 100.0), (2 * s, 300.0)]);
    let ln3_2 = (3.0f64).ln() / 2.0;
    let t = it.timeline_time_of_media(Tick(2 * s)).expect("media 2 s is inside the ramp");
    assert!((t.0 as f64 / s as f64 - (1.0 + ln3_2)).abs() < 1e-3, "media 2 s at timeline {} s", t.0 as f64 / s as f64);
}

fn rate_frame(p: &Project, seq: ItemId) -> Tick {
    p.sequence(seq).map(|q| q.settings.frame_rate.frame_duration()).unwrap_or(Tick(1))
}

#[test]
fn splitting_a_remapped_clip_keeps_the_content_continuous() {
    let (mut p, clip, seq) = demo_project();
    let (mut it, s) = remapped_clip(&mut p, clip, seq, 8);
    set_remap_curve(&mut it, &[(0, 100.0), (s, 100.0), (2 * s, 300.0)]);
    let before: Vec<_> = (0..=80).map(|i| it.source_time_at(Tick(i * s / 10))).collect();
    // razor at 2 s: the right piece starts on the media the left piece reached
    let cut = Tick(2 * s);
    let cut_media = it.media_span(cut);
    let mut right = it.clone();
    it.duration = cut;
    right.start = cut;
    right.duration -= cut;
    right.source_in += cut_media;
    for i in 0..=20usize {
        let t = Tick(i as i64 * s / 10);
        let (whole, split) = (before[20 + i], right.source_time_at(cut + t));
        assert!((whole.0 - split.0).abs() <= 1, "timeline {} s shows the same frame whole and split ({} vs {})", 2.0 + i as f64 / 10.0, whole.0, split.0);
    }
    assert_eq!(it.source_time_at(Tick(19 * s / 10)), before[19], "the left piece keeps its frames");
}
