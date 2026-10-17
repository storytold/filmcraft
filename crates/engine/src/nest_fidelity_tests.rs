//! Nests whose sequence is another size than the sequence they are in: how they come in, and
//! Scale to Frame Size / Fit to frame / Fill frame on them. Premiere's behaviour was observed in
//! Premiere Pro 26.5.2 (a 1280x720 nest in a 1920x1080 sequence).

use super::*;
use filmcraft_project::TrackItem;
use serde_json::json;

/// The demo's 1920x1080 sequence with a 1280x720 sequence nested on V3 after its last clip.
/// Returns the session and the nest's picture clip.
fn small_nest() -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let main = s.state.active_sequence.unwrap();
    assert_eq!((s.active_sequence().unwrap().settings.width, s.active_sequence().unwrap().settings.height), (1920, 1080));
    let footage = s.active_sequence().unwrap().video_tracks[0].items[0].item;
    let small = ItemId(s.execute("file.newSequence", json!({"name": "Small", "width": 1280, "height": 720})).unwrap()["sequence"].as_u64().unwrap());
    s.execute("sequence.open", json!({"item": small.0})).unwrap();
    s.execute("timeline.place", json!({"item": footage.0, "seconds": 0.0})).unwrap();
    s.execute("sequence.open", json!({"item": main.0})).unwrap();
    let r = s.execute("timeline.place", json!({"item": small.0, "track": "V3", "seconds": 120.0})).unwrap();
    let nest = ClipId(r["clips"][0].as_u64().unwrap());
    assert_eq!(s.active_sequence().unwrap().find_item(nest).unwrap().1.item, small);
    (s, nest)
}

fn clip(s: &Session, id: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(id).unwrap().1.clone()
}

fn motion(it: &TrackItem, param: &str) -> f64 {
    it.effect("motion").unwrap().param(param).unwrap().value.as_f64().unwrap()
}

#[test]
fn a_nest_of_another_size_comes_in_at_its_own_size() {
    let (s, nest) = small_nest();
    let it = clip(&s, nest);
    assert!((motion(&it, "scale") - 100.0).abs() < 1e-9 && !it.scale_to_frame);
    assert_eq!(filmcraft_render::source_size(&s.project, it.item), Some((1280, 720)));
}

#[test]
fn fit_and_fill_frame_scale_a_nest_to_the_sequence() {
    let (mut s, nest) = small_nest();
    s.execute("timeline.select", json!({"clips": [nest.0]})).unwrap();
    // 1280x720 into 1920x1080: 150% either way (the same shape)
    s.execute("clip.fitToFrame", json!({})).unwrap();
    assert!((motion(&clip(&s, nest), "scale") - 150.0).abs() < 1e-6);
    s.execute("edit.undo", json!({})).unwrap();
    assert!((motion(&clip(&s, nest), "scale") - 100.0).abs() < 1e-9);
    s.execute("clip.fillFrame", json!({})).unwrap();
    assert!((motion(&clip(&s, nest), "scale") - 150.0).abs() < 1e-6);
}

#[test]
fn scale_to_frame_size_on_a_nest_fills_the_frame_and_leaves_scale_alone() {
    let (mut s, nest) = small_nest();
    s.execute("timeline.select", json!({"clips": [nest.0]})).unwrap();
    s.execute("clip.scaleToFrameSize", json!({})).unwrap();
    let it = clip(&s, nest);
    assert!(it.scale_to_frame && (motion(&it, "scale") - 100.0).abs() < 1e-9);
    // the picture: the nest's corner is now the frame's corner
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: 0.25, ..Default::default() };
    let t = it.start + Tick(it.duration.0 / 2);
    let filled = filmcraft_render::render_sequence(&s.project, s.state.active_sequence.unwrap(), t, opts, &provider).unwrap();
    assert!(filled.get(2, 2)[3] > 0.98 && filled.get(filled.w - 3, filled.h - 3)[3] > 0.98);
    // a toggle: off again, the nest is back in the middle with nothing in the corners
    s.execute("clip.scaleToFrameSize", json!({})).unwrap();
    let centred = filmcraft_render::render_sequence(&s.project, s.state.active_sequence.unwrap(), t, opts, &provider).unwrap();
    assert!(centred.get(2, 2)[3] < 0.02 && centred.get(centred.w / 2, centred.h / 2)[3] > 0.98);
}
