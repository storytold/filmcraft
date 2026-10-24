use serde_json::json;

use crate::Session;
use filmcraft_project::{ItemId, ParamValue, TrackKind};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// Mean brightness of row `y` of a rendered frame (red + green).
fn row(f: &filmcraft_render::Image, y: usize) -> f32 {
    (0..f.w).map(|x| f.px[(y * f.w + x) * 4] + f.px[(y * f.w + x) * 4 + 1]).sum::<f32>() / f.w as f32
}

/// Auto Reframe Sequence (#1564): a 9:16 copy of the demo's 16:9 edit whose clips fill the tall
/// frame (the effect alone left them letterboxed), in one undo step, the original untouched.
#[test]
fn auto_reframe_sequence_fills_a_vertical_copy() {
    let mut s = demo();
    let source = s.state.active_sequence.unwrap();
    let before = s.active_sequence().unwrap().clone();
    assert_eq!((before.settings.width, before.settings.height), (1920, 1080));
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    let letterboxed = s.render_program(0.25).unwrap();
    let r = s.execute("sequence.autoReframe", json!({"aspect": "9:16", "motion": "faster"})).unwrap();
    let id = ItemId(r["sequence"].as_u64().unwrap());
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(1080), Some(1920)), "{r}");
    assert_ne!(id, source);
    assert_eq!(s.state.active_sequence, Some(id), "the new sequence opens");
    // and its Timeline is shown, even when the panel was closed (#2453 review)
    assert!(s.drain_events().contains(&crate::Event::OpenSequence(id)));
    assert!(s.project.item(id).unwrap().name.ends_with("Vertical 9x16"), "{r}");
    assert_eq!(s.project.sequence(source).unwrap(), &before, "the original is untouched");

    // every video clip with a picture: Auto Reframe at 9:16, faster; scaled to cover the frame
    let q = s.active_sequence().unwrap().clone();
    let mut reframed = 0;
    for it in q.all_tracks().filter(|t| t.kind == TrackKind::Video).flat_map(|t| &t.items) {
        let Some(e) = it.effects.iter().find(|e| e.effect == "auto_reframe") else {
            assert!(it.graphic.is_some() || s.project.source_par(it.item).is_none(), "{} was not reframed", it.name);
            continue;
        };
        reframed += 1;
        assert_eq!(e.param("aspect").map(|p| p.value.clone()), Some(ParamValue::Choice(1)), "{}", it.name);
        assert_eq!(e.param("preset").map(|p| p.value.clone()), Some(ParamValue::Choice(2)), "{}", it.name);
        assert!(!it.scale_to_frame);
        let old = before.find_item(it.id).unwrap().1;
        let old_scale = old.effect("motion").map_or(100.0, |m| m.f64_at("scale", filmcraft_time::Tick::ZERO));
        let (w, h) = s.project.source_size(it.item).unwrap();
        if (w, h) == (1920, 1080) && !old.scale_to_frame {
            let scale = it.effect("motion").unwrap().f64_at("scale", filmcraft_time::Tick::ZERO);
            // a picture filling the old frame covers the new one; a picture-in-picture keeps its
            // share of the frame (#2453 review)
            let k = if old_scale >= 100.0 { 1920.0 / 1080.0 } else { 1080.0 / 1920.0 };
            assert!((scale - old_scale * k).abs() < 0.01, "{}: {old_scale} → {scale}", it.name);
        }
    }
    assert_eq!(r["clips"].as_u64(), Some(reframed), "{r}");
    assert!(reframed >= 4, "{r}");

    // the picture reaches the top of the tall frame: the 16:9 edit in a 9:16 frame would be black there
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    let tall = s.render_program(0.25).unwrap();
    assert_eq!((tall.w, tall.h), (270, 480));
    assert!(row(&tall, 2) > 0.005 && row(&tall, tall.h - 3) > 0.005, "top {} bottom {}", row(&tall, 2), row(&tall, tall.h - 3));
    assert!(row(&letterboxed, letterboxed.h / 2) > 0.05);

    // one undo step removes it
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(id).is_none());
    s.execute("edit.redo", json!({})).unwrap();
    assert!(s.project.sequence(id).is_some());
}

#[test]
fn auto_reframe_sizes_and_refusals() {
    let mut s = demo();
    let source = s.state.active_sequence.unwrap();
    for (aspect, size) in [("1:1", (1080, 1080)), ("4:5", (1080, 1350)), ("16:9", (1920, 1080))] {
        let r = s.execute("sequence.autoReframe", json!({"sequence": source.0, "aspect": aspect, "name": format!("Cut {aspect}")})).unwrap();
        let q = s.project.sequence(ItemId(r["sequence"].as_u64().unwrap())).unwrap();
        assert_eq!((q.settings.width, q.settings.height), size, "{aspect}");
        assert_eq!(s.project.item(ItemId(r["sequence"].as_u64().unwrap())).unwrap().name, format!("Cut {aspect}"));
        s.active_sequence().unwrap().check().unwrap();
    }
    for bad in [json!({"aspect": "2:1"}), json!({"aspect": "9:16", "motion": "sideways"}), json!({"sequence": 999_999, "aspect": "1:1"})] {
        assert!(s.execute("sequence.autoReframe", bad.clone()).is_err(), "{bad}");
    }
}

/// #2453 review: a picture that didn't fill the old frame (a 3:2 still and a portrait phone clip,
/// both with Scale to Frame Size in the 16:9 sequence) is scaled to cover the new one, not carried
/// over with its letterbox.
#[test]
fn letterboxed_pictures_are_scaled_to_cover_the_new_frame() {
    let dir = crate::media_test_util::tmp_dir("reframe-letterbox");
    let mut s = demo();
    let mut items = Vec::new();
    for (name, w, h) in [("still.png", 3000u32, 2000u32), ("portrait.png", 1080, 1920)] {
        let path = dir.join(name);
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 120, 40, 255])).save(&path).unwrap();
        let r = s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap();
        items.push(ItemId(r["items"][0].as_u64().unwrap()));
    }
    let mut clips = Vec::new();
    for (k, item) in items.iter().enumerate() {
        s.execute("source.open", json!({"item": item.0})).unwrap();
        s.execute("playhead.set", json!({"seconds": 300.0 + 10.0 * k as f64})).unwrap();
        s.execute("source.overwrite", json!({})).unwrap();
        let q = s.active_sequence().unwrap();
        let at = s.playhead();
        let clip = q.video_tracks[0].items.iter().find(|c| c.item == *item && c.start <= at).unwrap().id;
        s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
        s.execute("clip.scaleToFrameSize", json!({})).unwrap();
        assert!(s.active_sequence().unwrap().find_item(clip).unwrap().1.scale_to_frame);
        clips.push(clip);
    }
    let r = s.execute("sequence.autoReframe", json!({"aspect": "9:16"})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(1080), Some(1920)), "{r}");
    let q = s.active_sequence().unwrap().clone();
    assert_eq!(clips.len(), 2);
    for item in &items {
        let it = q.all_tracks().flat_map(|t| &t.items).find(|c| c.item == *item).unwrap();
        let (w, h) = s.project.source_size(*item).unwrap();
        let cover = (1080.0 / f64::from(w)).max(1920.0 / f64::from(h));
        let scale = it.effect("motion").unwrap().f64_at("scale", filmcraft_time::Tick::ZERO) / 100.0;
        assert!((scale - cover).abs() < 1e-6, "{}x{}: scale {scale}, covering the 9:16 frame takes {cover}", w, h);
    }
}

/// #2453 review: a portrait picture-in-picture keeps its share of the frame (it used to shrink to a
/// third of it), and a non-uniform stretch survives the cover adjustment.
#[test]
fn picture_in_picture_and_stretch_survive_a_reframe() {
    let dir = crate::media_test_util::tmp_dir("reframe-pip");
    let mut s = demo();
    let mut clips = Vec::new();
    for (name, w, h) in [("portrait.png", 1080u32, 1920u32), ("still.png", 3000, 2000)] {
        let path = dir.join(name);
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 120, 40, 255])).save(&path).unwrap();
        let item = ItemId(s.execute("file.import", json!({"paths": [path.to_string_lossy()]})).unwrap()["items"][0].as_u64().unwrap());
        s.execute("source.open", json!({"item": item.0})).unwrap();
        s.execute("playhead.set", json!({"seconds": 400.0 + 10.0 * clips.len() as f64})).unwrap();
        s.execute("source.overwrite", json!({})).unwrap();
        let at = s.playhead();
        let clip = s.active_sequence().unwrap().video_tracks[0].items.iter().find(|c| c.item == item && c.start <= at).unwrap().id;
        s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
        s.execute("clip.scaleToFrameSize", json!({})).unwrap();
        clips.push(item);
    }
    let (portrait, still) = (clips[0], clips[1]);
    let set = |s: &mut Session, item: ItemId, param: &str, value: f64| {
        let clip = s.active_sequence().unwrap().all_tracks().flat_map(|t| &t.items).find(|c| c.item == item).unwrap().id;
        s.execute("effects.setParam", json!({"clip": clip.0, "effect": "motion", "param": param, "value": value})).unwrap();
    };
    set(&mut s, portrait, "scale", 50.0);
    set(&mut s, still, "scale_width", 110.0);
    s.execute("sequence.autoReframe", json!({"aspect": "9:16"})).unwrap();
    let q = s.active_sequence().unwrap().clone();
    let motion = |item: ItemId, param: &str| {
        let it = q.all_tracks().flat_map(|t| &t.items).find(|c| c.item == item).unwrap();
        it.effect("motion").unwrap().f64_at(param, filmcraft_time::Tick::ZERO)
    };
    // half the old frame's height, fitted: half the new frame's height, which it fits at 100 %
    assert!((motion(portrait, "scale") - 50.0).abs() < 1e-6, "{}", motion(portrait, "scale"));
    // covering the 9:16 frame, with its 10 % horizontal stretch kept
    let (sc, sw) = (motion(still, "scale"), motion(still, "scale_width"));
    assert!((sc / 100.0 - 1920.0 / 2000.0).abs() < 1e-6, "{sc}");
    assert!((sw / sc - 1.1).abs() < 1e-6, "{sw} / {sc}");
}
