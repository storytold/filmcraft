//! OpenTimelineIO: a hand-written plain OTIO document (from the public OTIO schema docs) with gaps,
//! transitions, markers and legacy `Clip.1`; lossless round trips through `metadata.filmcraft`.

use crate::support;

use filmcraft_interchange::{ExportOptions, Format, detect, import};
use filmcraft_media::Generator;
use filmcraft_project::{Keyframe, Label, Marker, MarkerId, MarkerKind, MediaRef, Param, ParamValue, Project, TrackKind, TransitionAlign};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use proptest::prelude::*;
use support::*;

fn rt(v: f64, rate: f64) -> String {
    format!(r#"{{"OTIO_SCHEMA": "RationalTime.1", "rate": {rate}, "value": {v}}}"#)
}
fn tr(s: f64, d: f64, rate: f64) -> String {
    format!(r#"{{"OTIO_SCHEMA": "TimeRange.1", "start_time": {}, "duration": {}}}"#, rt(s, rate), rt(d, rate))
}

fn doc() -> String {
    let r = 24.0;
    format!(
        r#"{{
  "OTIO_SCHEMA": "Timeline.1",
  "name": "River Story",
  "global_start_time": {gst},
  "metadata": {{}},
  "tracks": {{
    "OTIO_SCHEMA": "Stack.1",
    "name": "tracks",
    "children": [
      {{
        "OTIO_SCHEMA": "Track.1",
        "name": "V1",
        "kind": "Video",
        "children": [
          {{"OTIO_SCHEMA": "Gap.1", "name": "", "source_range": {gap}}},
          {{"OTIO_SCHEMA": "Transition.1", "name": "Fade In", "transition_type": "SMPTE_Dissolve", "in_offset": {z}, "out_offset": {t6}}},
          {{
            "OTIO_SCHEMA": "Clip.2",
            "name": "river_a",
            "source_range": {src_a},
            "media_references": {{"DEFAULT_MEDIA": {{"OTIO_SCHEMA": "ExternalReference.1", "target_url": "file:///footage/river%20a.mov", "available_range": {avail_a}}}}},
            "active_media_reference_key": "DEFAULT_MEDIA",
            "markers": [{{"OTIO_SCHEMA": "Marker.2", "name": "kingfisher", "color": "RED", "comment": "", "marked_range": {mk}}}]
          }},
          {{"OTIO_SCHEMA": "Transition.1", "name": "", "transition_type": "SMPTE_Dissolve", "in_offset": {t6}, "out_offset": {t6}}},
          {{
            "OTIO_SCHEMA": "Clip.1",
            "name": "river_b",
            "source_range": {src_b},
            "media_reference": {{"OTIO_SCHEMA": "ExternalReference.1", "target_url": "clips/river_b.mov", "available_range": null}},
            "effects": [{{"OTIO_SCHEMA": "LinearTimeWarp.1", "effect_name": "LinearTimeWarp", "time_scalar": 2.0}}]
          }},
          {{"OTIO_SCHEMA": "Gap.1", "source_range": {gap2}}},
          {{
            "OTIO_SCHEMA": "Clip.2",
            "name": "lost",
            "source_range": {src_c},
            "media_references": {{"DEFAULT_MEDIA": {{"OTIO_SCHEMA": "MissingReference.1", "name": "lost_shot.mov"}}}},
            "active_media_reference_key": "DEFAULT_MEDIA",
            "enabled": false
          }},
          {{"OTIO_SCHEMA": "Transition.1", "name": "Fade Out", "transition_type": "SMPTE_Dissolve", "in_offset": {t6}, "out_offset": {z}}}
        ]
      }},
      {{
        "OTIO_SCHEMA": "Track.1",
        "name": "A1",
        "kind": "Audio",
        "children": [
          {{
            "OTIO_SCHEMA": "Clip.2",
            "name": "ambience",
            "source_range": {src_amb},
            "media_references": {{"DEFAULT_MEDIA": {{"OTIO_SCHEMA": "ExternalReference.1", "target_url": "file:///footage/amb.wav"}}}},
            "active_media_reference_key": "DEFAULT_MEDIA",
            "effects": [{{"OTIO_SCHEMA": "Effect.1", "effect_name": "Reverb"}}]
          }}
        ]
      }}
    ],
    "markers": [{{"OTIO_SCHEMA": "Marker.2", "name": "act two", "color": "BLUE", "comment": "", "marked_range": {smk}}}]
  }}
}}"#,
        gst = rt(86_400.0, r),
        z = rt(0.0, r),
        t6 = rt(6.0, r),
        gap = tr(0.0, 12.0, r),
        avail_a = tr(86_400.0, 2400.0, r),
        src_a = tr(86_448.0, 48.0, r),
        mk = tr(86_460.0, 0.0, r),
        src_b = tr(10.0, 36.0, r),
        gap2 = tr(0.0, 24.0, r),
        src_c = tr(0.0, 30.0, r),
        src_amb = tr(0.0, 150.0, r),
        smk = tr(60.0, 0.0, r),
    )
}

#[test]
fn golden_gaps_and_transitions() {
    let d = doc();
    assert_eq!(detect(d.as_bytes(), Some("otio")), Some(Format::Otio));
    let (imp, rep) = import(d.as_bytes(), Format::Otio, Some("/proj")).unwrap();
    let p = &imp.project;
    let sid = only_seq(&imp);
    let s = p.sequence(sid).unwrap();
    let r = FrameRate::FPS_24;
    assert_eq!(p.item(sid).unwrap().name, "River Story");
    assert_eq!(s.settings.frame_rate, r);
    assert_eq!(s.start_timecode, 86_400);
    let v1 = &s.video_tracks[0];
    assert_eq!(v1.items.len(), 3);
    let (a, b, c) = (&v1.items[0], &v1.items[1], &v1.items[2]);
    // source times are relative to the reference's available range start
    assert_eq!((a.start, a.duration, a.source_in), (r.tick_of(12), r.tick_of(48), r.tick_of(48)));
    assert_eq!(media_key(p, a.item), "/footage/river a.mov");
    assert_eq!(a.markers[0].name, "kingfisher");
    assert_eq!(a.markers[0].start, r.tick_of(60));
    assert_eq!(a.markers[0].color, Label::Rose);
    assert_eq!((b.start, b.duration, b.source_in, b.speed), (r.tick_of(60), r.tick_of(36), r.tick_of(10), 2.0));
    assert_eq!(media_key(p, b.item), "/proj/clips/river_b.mov");
    assert_eq!(c.start, r.tick_of(120));
    assert!(!c.enabled);
    assert!(p.item(c.item).unwrap().as_media().unwrap().offline);
    let ts = &v1.transitions;
    assert_eq!(ts.len(), 3);
    assert_eq!((ts[0].from, ts[0].to, ts[0].start, ts[0].duration, ts[0].align), (None, Some(a.id), r.tick_of(12), r.tick_of(6), TransitionAlign::StartAtCut));
    assert_eq!(
        (ts[1].from, ts[1].to, ts[1].start, ts[1].duration, ts[1].align),
        (Some(a.id), Some(b.id), r.tick_of(54), r.tick_of(12), TransitionAlign::CenterAtCut)
    );
    assert_eq!((ts[2].from, ts[2].to, ts[2].start, ts[2].align), (Some(c.id), None, r.tick_of(144), TransitionAlign::EndAtCut));
    assert!(ts.iter().all(|t| t.effect.effect == "cross_dissolve"), "{rep}");
    let amb = &s.audio_tracks[0].items[0];
    assert_eq!(amb.duration, r.tick_of(150));
    assert!(rep.mentions("Reverb"), "{rep}");
    assert_eq!(s.markers[0].name, "act two");
    assert_eq!(s.markers[0].start, r.tick_of(60));
}

fn project() -> (Project, filmcraft_project::ItemId) {
    let r = FrameRate::FPS_23_976;
    let mut p = Project::new("P");
    p.custom_colors = vec![filmcraft_project::HexColor([18, 171, 205])];
    let a = media(&mut p, "/m/a.mov", true, true, r);
    p.item_mut(a).unwrap().custom_label_color = filmcraft_project::HexColor::parse("#ABCDEF");
    if let Some(m) = p.item_mut(a).and_then(|i| i.as_media_mut()) {
        m.info.start_timecode = Some(86_400);
    }
    let b = media(&mut p, "/m/b.mov", true, true, r);
    let g = generator(&mut p, "Matte", Generator::ColorMatte { color: [0.2, 0.4, 0.6, 1.0] });
    let nest = sequence(&mut p, "Inner", r, false);
    clip(&mut p, nest, TrackKind::Video, 0, b, 0..50, 5);
    let s = sequence(&mut p, "Outer", r, false);
    let v1 = clip(&mut p, s, TrackKind::Video, 0, a, 0..48, 24);
    let a1 = clip(&mut p, s, TrackKind::Audio, 0, a, 0..48, 24);
    link(&mut p, s, &[v1, a1]);
    let v2 = clip(&mut p, s, TrackKind::Video, 0, nest, 48..48 + (40), 3);
    transition(&mut p, s, (TrackKind::Video, 0), "iris_round", (Some(v1), Some(v2)), 40..40 + (16), TransitionAlign::CenterAtCut);
    clip(&mut p, s, TrackKind::Video, 1, g, 10..10 + (20), 0);
    {
        let q = p.sequence_mut(s).unwrap();
        // sub-frame audio position
        let mut ai = q.audio_tracks[0].items[0].clone();
        ai.id = filmcraft_project::ClipId(9999);
        ai.link = None;
        ai.start = Tick(TICKS_PER_SECOND * 3 + TICKS_PER_SECOND / 48_000 * 7);
        ai.duration = Tick(TICKS_PER_SECOND / 48_000 * 33_333);
        ai.gain_db = 4.5;
        q.audio_tracks[1].items.push(ai);
        let (_, it) = q.find_item_mut(v1).unwrap();
        it.label = Label::Teal;
        it.custom_label_color = filmcraft_project::HexColor::parse("#12ABCD");
        let mut op = Param::new(ParamValue::Float(100.0));
        op.keyframes.push(Keyframe::new(r.tick_of(24), ParamValue::Float(0.0)));
        op.keyframes.push(Keyframe::new(r.tick_of(36), ParamValue::Float(100.0)));
        it.effect_mut("opacity").unwrap().params.insert("opacity".into(), op);
        it.effects.push(filmcraft_project::find_effect("gaussian_blur").unwrap().instance());
        it.markers.push(Marker {
            url: String::new(),
            author: "Editor 日本語".into(),
            custom_color: filmcraft_project::HexColor::parse("#FF80C0"),
            id: MarkerId(5),
            start: r.tick_of(30),
            duration: r.tick_of(2),
            name: "m".into(),
            comment: "c".into(),
            kind: MarkerKind::Comment,
            color: Label::Yellow,
        });
        let (_, n) = q.find_item_mut(v2).unwrap();
        n.reverse = true;
        n.speed = 1.5;
        q.video_tracks[1].locked = true;
        q.audio_tracks[1].volume_db = -3.0;
        q.markers.push(Marker {
            url: String::new(),
            author: "Editor 日本語".into(),
            custom_color: filmcraft_project::HexColor::parse("#FF80C0"),
            id: MarkerId(6),
            start: r.tick_of(70),
            duration: Tick::ZERO,
            name: "chap".into(),
            comment: String::new(),
            kind: MarkerKind::Chapter,
            color: Label::Mango,
        });
    }
    (p, s)
}

#[test]
fn lossless_roundtrip_with_filmcraft_metadata() {
    let (p, s) = project();
    let (imp, text, rep) = roundtrip(&p, s, Format::Otio, &ExportOptions::default());
    assert!(!rep.has_warnings(), "{rep}");
    assert!(text.contains("\"OTIO_SCHEMA\": \"Timeline.1\""));
    assert!(text.contains("\"target_url\": \"file:///m/a.mov\""));
    let si = only_seq(&imp);
    let q = &imp.project;
    assert_eq!(q.custom_colors, p.custom_colors);
    assert!(q.items.values().any(|i| i.custom_label_color == filmcraft_project::HexColor::parse("#ABCDEF")));
    let (qs, ps) = (q.sequence(si).unwrap(), p.sequence(s).unwrap());
    for kind in [TrackKind::Video, TrackKind::Audio] {
        assert_eq!(structure(q, si, kind), structure(&p, s, kind), "{text}");
    }
    assert_eq!(links(q, si), links(&p, s));
    assert_eq!(qs.settings, ps.settings);
    assert_eq!(
        qs.markers.iter().map(|m| (&m.name, m.start, m.kind, m.color, m.custom_color, &m.author)).collect::<Vec<_>>(),
        ps.markers.iter().map(|m| (&m.name, m.start, m.kind, m.color, m.custom_color, &m.author)).collect::<Vec<_>>()
    );
    assert!(qs.video_tracks[1].locked);
    assert_eq!(qs.audio_tracks[1].volume_db, -3.0);
    // clip-level fields restored exactly
    let (qa, pa) = (&qs.video_tracks[0].items[0], &ps.video_tracks[0].items[0]);
    assert_eq!(format!("{:?}", qa.effects), format!("{:?}", pa.effects)); // NaN auto points compare unequal
    assert_eq!(qa.label, Label::Teal);
    assert_eq!(qa.custom_label_color, filmcraft_project::HexColor::parse("#12ABCD"));
    assert_eq!(
        qa.markers.iter().map(|m| (&m.name, m.start, m.duration, m.custom_color, &m.author)).collect::<Vec<_>>(),
        pa.markers.iter().map(|m| (&m.name, m.start, m.duration, m.custom_color, &m.author)).collect::<Vec<_>>()
    );
    assert_eq!(qs.audio_tracks[1].items[0].gain_db, 4.5);
    assert_eq!(qs.audio_tracks[1].items[0].start, ps.audio_tracks[1].items[0].start);
    assert_eq!(qs.video_tracks[0].transitions[0].effect.effect, "iris_round");
    // media info and generator restored
    let qa_media = q.item(qa.item).unwrap().as_media().unwrap();
    assert_eq!(qa_media.info, p.item(pa.item).unwrap().as_media().unwrap().info);
    let gm = q.item(qs.video_tracks[1].items[0].item).unwrap().as_media().unwrap();
    assert_eq!(gm.media, MediaRef::Generator(Generator::ColorMatte { color: [0.2, 0.4, 0.6, 1.0] }));
    // nested sequence
    let nested = qs.video_tracks[0].items[1].item;
    assert_eq!(q.item(nested).unwrap().name, "Inner");
    assert_eq!(q.sequence(nested).unwrap().video_tracks[0].items[0].source_in, FrameRate::FPS_23_976.tick_of(5));
    // relative media paths
    let o = ExportOptions { relative_to: Some("/m/otio".into()), ..Default::default() };
    let (bytes, _) = filmcraft_interchange::export(&p, s, Format::Otio, &o).unwrap();
    let t = String::from_utf8(bytes.clone()).unwrap();
    assert!(t.contains("\"target_url\": \"../a.mov\""), "{t}");
    let (imp2, _) = import(&bytes, Format::Otio, Some("/m/otio")).unwrap();
    assert_eq!(structure(&imp2.project, imp2.sequences[0], TrackKind::Video), structure(&p, s, TrackKind::Video));
}

#[test]
fn collections_and_bare_tracks() {
    let one = r#"{"OTIO_SCHEMA": "SerializableCollection.1", "name": "Bundle", "children": [
        {"OTIO_SCHEMA": "Timeline.1", "name": "T1", "tracks": {"OTIO_SCHEMA": "Stack.1", "children": [
            {"OTIO_SCHEMA": "Track.1", "kind": "Video", "children": [
                {"OTIO_SCHEMA": "Clip.2", "name": "x", "source_range": {"OTIO_SCHEMA": "TimeRange.1", "start_time": {"OTIO_SCHEMA": "RationalTime.1", "rate": 25, "value": 0}, "duration": {"OTIO_SCHEMA": "RationalTime.1", "rate": 25, "value": 50}},
                 "media_references": {"DEFAULT_MEDIA": {"OTIO_SCHEMA": "GeneratorReference.1", "generator_kind": "SMPTEBars"}}, "active_media_reference_key": "DEFAULT_MEDIA"}]}]}},
        {"OTIO_SCHEMA": "Timeline.1", "name": "T2", "tracks": {"OTIO_SCHEMA": "Stack.1", "children": []}}
    ]}"#;
    let (imp, _) = import(one.as_bytes(), Format::Otio, None).unwrap();
    assert_eq!(imp.sequences.len(), 2);
    let s = imp.project.sequence(imp.sequences[0]).unwrap();
    assert_eq!(s.settings.frame_rate, FrameRate::FPS_25);
    assert_eq!(s.video_tracks[0].items[0].duration, FrameRate::FPS_25.tick_of(50));
    let m = imp.project.item(s.video_tracks[0].items[0].item).unwrap().as_media().unwrap();
    assert_eq!(m.media, MediaRef::Generator(Generator::BarsAndTone));
    assert!(import(b"{\"OTIO_SCHEMA\": \"Clip.2\"}", Format::Otio, None).is_err());
    assert!(import(b"not json", Format::Otio, None).is_err());
}

fn palette_collection(first: serde_json::Value, second: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"OTIO_SCHEMA":"SerializableCollection.1","children":[
        {"OTIO_SCHEMA":"Timeline.1","name":"First","metadata":{"filmcraft":first},"tracks":{"OTIO_SCHEMA":"Stack.1","children":[]}},
        {"OTIO_SCHEMA":"Timeline.1","name":"Second","metadata":{"filmcraft":second},"tracks":{"OTIO_SCHEMA":"Stack.1","children":[]}}
    ]})
}

#[test]
fn review_orphan_palette_names_do_not_hide_retained_names() {
    use filmcraft_project::HexColor;
    use serde_json::json;
    let mut names = serde_json::Map::new();
    for i in 0..70 {
        names.insert(HexColor([0, 0, i]).hex(), json!("Orphan"));
    }
    names.insert("#FFFFFE".into(), json!("Later name"));
    names.insert("#FFFFFF".into(), json!("Retained name"));
    let doc = palette_collection(
        json!({"custom_colors":["#FFFFFE"],"custom_color_names":{"#FFFFFE":"Original name"}}),
        json!({"custom_colors":["#FFFFFF"],"custom_color_names":names}),
    );
    let (imp, _) = import(&serde_json::to_vec(&doc).unwrap(), Format::Otio, None).unwrap();
    assert_eq!(imp.project.custom_color_names.len(), 2);
    assert_eq!(imp.project.custom_color_names[&HexColor([255, 255, 254])], "Original name");
    assert_eq!(imp.project.custom_color_names[&HexColor([255, 255, 255])], "Retained name");
}

#[test]
fn review_generator_preset_labels_survive_otio_round_trip() {
    let mut p = Project::new("Generator labels");
    let s = sequence(&mut p, "Main", FrameRate::FPS_25, false);
    for (index, (kind, label, custom)) in [
        (Generator::BlackVideo, Label::Teal, None),
        (Generator::BarsAndTone, Label::Magenta, filmcraft_project::HexColor::parse("#123456")),
        (Generator::ColorMatte { color: [0.2, 0.4, 0.6, 1.0] }, Label::Mango, None),
    ]
    .into_iter()
    .enumerate()
    {
        let id = generator(&mut p, &format!("Generator {index}"), kind);
        let item = p.item_mut(id).unwrap();
        item.label = label;
        item.custom_label_color = custom;
        clip(&mut p, s, TrackKind::Video, 0, id, (index as i64 * 25)..(index as i64 * 25 + 25), 0);
    }
    let (bytes, _) = filmcraft_interchange::export(&p, s, Format::Otio, &ExportOptions::default()).unwrap();
    let (imp, _) = import(&bytes, Format::Otio, None).unwrap();
    for original in p.items.values().filter(|i| i.as_media().is_some()) {
        let restored = imp.project.items.values().find(|i| i.name == original.name).unwrap();
        assert_eq!(restored.label, original.label, "{}", original.name);
        assert_eq!(restored.custom_label_color, original.custom_label_color);
        assert_eq!(restored.as_media().unwrap().media, original.as_media().unwrap().media);
    }
}

#[test]
fn review_distinct_identical_generators_keep_identity_and_shared_references() {
    let mut p = Project::new("Generator identity");
    let s = sequence(&mut p, "Main", FrameRate::FPS_25, false);
    let mut pairs = Vec::new();
    for (index, kind) in [Generator::BlackVideo, Generator::BarsAndTone, Generator::ColorMatte { color: [0.2, 0.4, 0.6, 1.0] }].into_iter().enumerate() {
        let first = generator(&mut p, &format!("First {index}"), kind.clone());
        let second = generator(&mut p, &format!("Second {index}"), kind);
        p.item_mut(first).unwrap().label = Label::Teal;
        let item = p.item_mut(second).unwrap();
        item.label = Label::Mango;
        item.custom_label_color = filmcraft_project::HexColor::parse("#123456");
        for (offset, id) in [first, second, first].into_iter().enumerate() {
            clip(&mut p, s, TrackKind::Video, 0, id, ((index * 3 + offset) as i64 * 25)..((index * 3 + offset) as i64 * 25 + 25), 0);
        }
        pairs.push((first, second));
    }
    let (imp, _, _) = roundtrip(&p, s, Format::Otio, &ExportOptions::default());
    let clips = &imp.project.sequence(only_seq(&imp)).unwrap().video_tracks[0].items;
    assert_eq!(clips.len(), 9);
    assert_eq!(imp.project.items.values().filter(|i| i.as_media().is_some()).count(), 6);
    for ((first, second), group) in pairs.into_iter().zip(clips.as_chunks::<3>().0) {
        assert_ne!(group[0].item, group[1].item, "distinct source items must remain distinct");
        assert_eq!(group[0].item, group[2].item, "repeated source references must remain shared");
        for (original_id, clip) in [(first, &group[0]), (second, &group[1])] {
            let original = p.item(original_id).unwrap();
            let restored = imp.project.item(clip.item).unwrap();
            assert_eq!(restored.name, original.name);
            assert_eq!(restored.label, original.label);
            assert_eq!(restored.custom_label_color, original.custom_label_color);
            assert_eq!(restored.as_media().unwrap().media, original.as_media().unwrap().media);
        }
    }
}

#[test]
fn review_generators_without_valid_item_ids_keep_legacy_sharing() {
    use serde_json::json;
    let mut p = Project::new("Legacy generators");
    let s = sequence(&mut p, "Main", FrameRate::FPS_25, false);
    let first = generator(&mut p, "Black", Generator::BlackVideo);
    let second = generator(&mut p, "Other Black", Generator::BlackVideo);
    let bars = generator(&mut p, "Bars", Generator::BarsAndTone);
    for (offset, id) in [first, second, first, bars].into_iter().enumerate() {
        clip(&mut p, s, TrackKind::Video, 0, id, (offset as i64 * 25)..(offset as i64 * 25 + 25), 0);
    }
    let (bytes, _) = filmcraft_interchange::export(&p, s, Format::Otio, &ExportOptions::default()).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for identity in [None, Some(json!(null)), Some(json!(-1)), Some(json!("invalid")), Some(json!({})), Some(json!([]))] {
        let mut doc = doc.clone();
        for clip in doc["tracks"]["children"][0]["children"].as_array_mut().unwrap() {
            let meta = clip["media_references"]["DEFAULT_MEDIA"]["metadata"]["filmcraft"].as_object_mut().unwrap();
            meta.remove("item");
            if let Some(ref identity) = identity {
                meta.insert("item".into(), identity.clone());
            }
        }
        let (imp, _) = import(&serde_json::to_vec(&doc).unwrap(), Format::Otio, None).unwrap();
        let clips = &imp.project.sequence(only_seq(&imp)).unwrap().video_tracks[0].items;
        assert_eq!(clips[0].item, clips[1].item);
        assert_eq!(clips[0].item, clips[2].item);
        assert_ne!(clips[0].item, clips[3].item);
    }
}

#[test]
fn review_collection_palettes_union_distinct_colors_and_preserve_first_names() {
    use filmcraft_project::HexColor;
    use serde_json::json;
    let doc = palette_collection(
        json!({"custom_colors":["#112233"],"custom_color_names":{"#112233":"First name"}}),
        json!({"custom_colors":["#445566","#112233"],"custom_color_names":{"#445566":"Second name","#112233":"Later name"}}),
    );
    let (imp, _) = import(&serde_json::to_vec(&doc).unwrap(), Format::Otio, None).unwrap();
    let colors = [HexColor([0x11, 0x22, 0x33]), HexColor([0x44, 0x55, 0x66])];
    assert_eq!(imp.project.custom_colors, colors);
    assert_eq!(imp.project.custom_color_names[&colors[0]], "First name");
    assert_eq!(imp.project.custom_color_names[&colors[1]], "Second name");
    let mut target = Project::new("Existing");
    filmcraft_interchange::merge_into(&mut target, imp, None);
    assert_eq!(target.custom_colors, colors);
    assert_eq!(target.custom_color_names[&colors[0]], "First name");
}

#[test]
fn review_collection_empty_or_absent_palettes_do_not_clear_earlier_colors() {
    use serde_json::json;
    for later in [json!({"custom_colors":[]}), json!({})] {
        let doc = palette_collection(json!({"custom_colors":["#112233"],"custom_color_names":{"#112233":"Saved"}}), later);
        let (imp, _) = import(&serde_json::to_vec(&doc).unwrap(), Format::Otio, None).unwrap();
        let mut target = Project::new("Existing");
        filmcraft_interchange::merge_into(&mut target, imp, None);
        let color = filmcraft_project::HexColor([0x11, 0x22, 0x33]);
        assert_eq!(target.custom_colors, [color]);
        assert_eq!(target.custom_color_names[&color], "Saved");
    }
}

#[test]
fn review_collection_full_palette_keeps_names_with_retained_colors() {
    use filmcraft_project::HexColor;
    use serde_json::json;
    let colors: Vec<_> = (0..64).map(|i| HexColor([0, 0, i])).collect();
    let doc = palette_collection(
        json!({"custom_colors":colors.iter().map(|c|c.hex()).collect::<Vec<_>>(),"custom_color_names":{"#000000":"Original"}}),
        json!({"custom_colors":["#445566","#000001","#000000"],"custom_color_names":{"#445566":"Cannot retain","#000001":"Filled","#000000":"Later"}}),
    );
    let (imp, report) = import(&serde_json::to_vec(&doc).unwrap(), Format::Otio, None).unwrap();
    assert_eq!(imp.project.custom_colors, colors);
    assert_eq!(imp.project.custom_color_names[&colors[0]], "Original");
    assert_eq!(imp.project.custom_color_names[&colors[1]], "Filled");
    assert!(!imp.project.custom_color_names.contains_key(&HexColor([0x44, 0x55, 0x66])));
    assert!(report.to_string().contains("64"), "discarded palette entries must be reported: {report}");
}

#[test]
fn review_collection_palette_deduplicates_before_applying_capacity() {
    use filmcraft_project::HexColor;
    use serde_json::json;
    let mut entries = vec!["#000000".to_owned(); 70];
    entries.extend((0..70).map(|i| HexColor([0, 0, i]).hex()));
    let doc = palette_collection(json!({"custom_colors":entries}), json!({}));
    let (imp, report) = import(&serde_json::to_vec(&doc).unwrap(), Format::Otio, None).unwrap();
    assert_eq!(imp.project.custom_colors, (0..64).map(|i| HexColor([0, 0, i])).collect::<Vec<_>>());
    assert!(report.to_string().contains("64"));
}

#[derive(Clone, Debug)]
struct G {
    track: usize,
    gap: i64,
    dur: i64,
    media: usize,
    src: i64,
    speed: u8,
    enabled: bool,
    tr: u8,
    tr_len: i64,
    audio: bool,
    subframe: i64,
}

fn gen_clip() -> impl Strategy<Value = G> {
    (0usize..3, 0i64..3, 20i64..150, 0usize..3, 0i64..2000, 0u8..5, prop::bool::weighted(0.9), 0u8..6, 1i64..9, any::<bool>(), 0i64..2000).prop_map(
        |(track, gap, dur, media, src, speed, enabled, tr, tr_len, audio, subframe)| G {
            track,
            gap: gap * 5,
            dur,
            media,
            src,
            speed,
            enabled,
            tr,
            tr_len,
            audio,
            subframe,
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn otio_roundtrip(clips in prop::collection::vec(gen_clip(), 1..20), ri in 0usize..5) {
        let rate = [FrameRate::FPS_23_976, FrameRate::FPS_25, FrameRate::FPS_29_97, FrameRate::FPS_59_94, FrameRate::FPS_24][ri];
        let mut p = Project::new("P");
        let ms = [media(&mut p, "/x/one.mov", true, true, rate), media(&mut p, "/x/two.mov", true, true, rate), media(&mut p, "/y/three four.mxf", true, true, rate)];
        let s = sequence(&mut p, "S", rate, rate == FrameRate::FPS_29_97);
        let mut ends = [0i64; 3];
        let mut prev: [Option<filmcraft_project::ClipId>; 3] = [None; 3];
        let mut aend = [Tick::ZERO; 3];
        for g in &clips {
            let t = ends[g.track] + g.gap;
            let v = clip(&mut p, s, TrackKind::Video, g.track, ms[g.media], t..t + (g.dur), g.src);
            {
                let q = p.sequence_mut(s).unwrap();
                let (_, it) = q.find_item_mut(v).unwrap();
                it.enabled = g.enabled;
                match g.speed {
                    1 => it.speed = 2.0,
                    2 => it.speed = 0.5,
                    3 => it.reverse = true,
                    4 => it.frame_hold = Some(rate.tick_of(g.src + 3)),
                    _ => {}
                }
            }
            let d = g.tr_len;
            let adjacent = g.gap == 0 && prev[g.track].is_some();
            match g.tr {
                0 if adjacent => transition(&mut p, s, (TrackKind::Video, g.track), "cross_dissolve", (prev[g.track], Some(v)), t - d / 2..t - d / 2 + (d), TransitionAlign::CenterAtCut),
                1 if adjacent => transition(&mut p, s, (TrackKind::Video, g.track), "dip_to_white", (prev[g.track], Some(v)), t..t + (d), TransitionAlign::StartAtCut),
                2 if adjacent => transition(&mut p, s, (TrackKind::Video, g.track), "push", (prev[g.track], Some(v)), t - d..t - d + (d), TransitionAlign::EndAtCut),
                3 if !adjacent => transition(&mut p, s, (TrackKind::Video, g.track), "cross_dissolve", (None, Some(v)), t..t + (d), TransitionAlign::StartAtCut),
                _ => {}
            }
            // Audio at sample (not frame) positions, linked to the video.
            let at = rate.tick_of(t) + Tick(TICKS_PER_SECOND / 48_000 * g.subframe % rate.frame_duration().0.max(1));
            if g.audio && aend[g.track] <= at {
                let a = clip(&mut p, s, TrackKind::Audio, g.track, ms[g.media], t..t + (g.dur), g.src);
                let q = p.sequence_mut(s).unwrap();
                let (_, ai) = q.find_item_mut(a).unwrap();
                ai.start = at;
                aend[g.track] = ai.end();
                link(&mut p, s, &[v, a]);
            }
            ends[g.track] = t + g.dur;
            prev[g.track] = Some(v);
        }
        let (imp, text, _) = roundtrip(&p, s, Format::Otio, &ExportOptions::default());
        let si = only_seq(&imp);
        prop_assert_eq!(structure(&imp.project, si, TrackKind::Video), structure(&p, s, TrackKind::Video), "{}", text);
        prop_assert_eq!(structure(&imp.project, si, TrackKind::Audio), structure(&p, s, TrackKind::Audio));
        prop_assert_eq!(links(&imp.project, si), links(&p, s));
    }
}

#[test]
fn other_export_formats_report_marker_property_loss() {
    let (p, s) = project();
    for format in [Format::Fcp7Xml, Format::Fcpxml, Format::Edl] {
        let (_, report) = filmcraft_interchange::export(&p, s, format, &ExportOptions::default()).unwrap();
        assert!(report.to_string().contains("marker authors or exact custom colors"), "{format:?}: {report}");
    }
}

#[test]
fn marker_urls_roundtrip_in_otio_and_warn_in_other_formats() {
    let (mut p, s) = project();
    let sequence_url = "https://example.com/sequence?q=日本語#review";
    let clip_url = "https://example.com/clip?take=2";
    {
        let q = p.sequence_mut(s).unwrap();
        q.markers[0].url = sequence_url.into();
        q.markers[0].kind = MarkerKind::WebLink;
        q.video_tracks[0].items[0].markers[0].url = clip_url.into();
        q.video_tracks[0].items[0].markers[0].kind = MarkerKind::WebLink;
    }
    let (imp, _, report) = roundtrip(&p, s, Format::Otio, &ExportOptions::default());
    assert!(!report.has_warnings(), "{report}");
    let imported = imp.project.sequence(only_seq(&imp)).unwrap();
    assert_eq!(imported.markers[0].url, sequence_url);
    assert_eq!(imported.video_tracks[0].items[0].markers[0].url, clip_url);
    assert_eq!(imported.markers[0].comment, p.sequence(s).unwrap().markers[0].comment);
    // Imported metadata remains editable text; it must never become permission to launch a handler.
    let unsafe_url = "javascript:alert(1)";
    p.sequence_mut(s).unwrap().markers[0].url = unsafe_url.into();
    let (untrusted, _, _) = roundtrip(&p, s, Format::Otio, &ExportOptions::default());
    let retained = &untrusted.project.sequence(only_seq(&untrusted)).unwrap().markers[0].url;
    assert_eq!(retained, unsafe_url);
    assert!(filmcraft_project::marker_web_url(retained).is_none());
    p.sequence_mut(s).unwrap().markers[0].url = sequence_url.into();

    // URL loss is reported even if there are no authors, custom colors or saved swatches.
    p.custom_colors.clear();
    p.custom_color_names.clear();
    for item in p.items.values_mut() {
        item.custom_label_color = None;
        match &mut item.kind {
            filmcraft_project::ItemKind::Sequence(seq) => {
                let seq = std::sync::Arc::make_mut(seq);
                for marker in &mut seq.markers {
                    marker.author.clear();
                    marker.custom_color = None;
                }
                for clip in seq.all_tracks_mut().flat_map(|track| &mut track.items) {
                    clip.custom_label_color = None;
                    for marker in &mut clip.markers {
                        marker.author.clear();
                        marker.custom_color = None;
                    }
                }
            }
            filmcraft_project::ItemKind::Media(media) => {
                for marker in &mut media.markers {
                    marker.author.clear();
                    marker.custom_color = None;
                }
            }
            _ => {}
        }
    }
    for format in [Format::Fcp7Xml, Format::Fcpxml, Format::Edl, Format::Aaf, Format::Omf] {
        let (_, report) = filmcraft_interchange::export(&p, s, format, &ExportOptions::default()).unwrap();
        assert!(report.to_string().contains("marker URLs"), "{format:?}: {report}");
    }
}

/// #2195: a relative `target_url` is a URL: `Clip%20A.mov` is the file `Clip A.mov`.
#[test]
fn relative_target_urls_are_percent_decoded() {
    let doc = r#"{"OTIO_SCHEMA": "Timeline.1", "name": "T", "tracks": {"OTIO_SCHEMA": "Stack.1", "children": [
        {"OTIO_SCHEMA": "Track.1", "kind": "Video", "children": [
            {"OTIO_SCHEMA": "Clip.2", "name": "a", "source_range": {"OTIO_SCHEMA": "TimeRange.1", "start_time": {"OTIO_SCHEMA": "RationalTime.1", "rate": 25, "value": 0}, "duration": {"OTIO_SCHEMA": "RationalTime.1", "rate": 25, "value": 50}},
             "media_references": {"DEFAULT_MEDIA": {"OTIO_SCHEMA": "ExternalReference.1", "target_url": "media/Clip%20A.mov"}}, "active_media_reference_key": "DEFAULT_MEDIA"}]}]}}"#;
    let (imp, _) = import(doc.as_bytes(), Format::Otio, Some("/proj")).unwrap();
    let p = &imp.project;
    let clip = &p.sequence(imp.sequences[0]).unwrap().video_tracks[0].items[0];
    assert_eq!(media_key(p, clip.item), "/proj/media/Clip A.mov");
}
