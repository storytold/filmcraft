//! Headless UI tests of nested sequence clips in the Timeline.

use crate::support::{Drive, Launch, driver};
use serde_json::json;

driver!(Driver);

impl Driver {
    /// (x, y, width, height) of an element, if it is on screen.
    fn rect(&mut self, id: &str) -> Option<[f64; 4]> {
        self.element_rect(id)
    }
}

/// As in Premiere, a nest keeps its length when its sequence gets shorter, and the
/// Timeline hatches the part that is now past the contents.
#[test]
fn the_empty_end_of_a_nest_is_marked_in_the_timeline() {
    let mut d = Driver::demo();
    let outer = d.app().session.state.active_sequence.unwrap().0;
    let (a, b) = {
        let v1 = &d.app().session.active_sequence().unwrap().video_tracks[0];
        (v1.items[3].clone(), v1.items[4].clone())
    };
    d.exec("timeline.select", json!({"clips": [a.id.0, b.id.0]}));
    let r = d.exec("clip.nest", json!({"name": "Inner"}));
    let (nested, nest) = (r["sequence"].as_u64().unwrap(), r["clips"][0].as_u64().unwrap());
    d.frames(3);
    let clip = d.rect(&format!("timeline.clip.{nest}")).expect("the nest clip is on screen");
    assert!(d.rect(&format!("timeline.clip.{nest}.empty")).is_none(), "a full nest has no empty part");
    // remove the second clip inside the nest
    d.exec("sequence.open", json!({"item": nested}));
    d.exec("timeline.select", json!({"clips": [b.id.0]}));
    d.exec("edit.clear", json!({}));
    d.exec("sequence.open", json!({"item": outer}));
    d.frames(3);
    let clip2 = d.rect(&format!("timeline.clip.{nest}")).expect("the nest clip is on screen");
    assert!((clip2[2] - clip[2]).abs() < 1.0, "the nest is as long as before");
    let empty = d.rect(&format!("timeline.clip.{nest}.empty")).expect("the empty part is marked");
    // it covers the removed clip's share of the nest, at its right end
    let share = b.duration.0 as f64 / (a.duration.0 + b.duration.0) as f64;
    assert!((empty[2] - clip2[2] * share).abs() < 2.0, "{empty:?} of {clip2:?}, share {share}");
    assert!((empty[0] + empty[2] - (clip2[0] + clip2[2])).abs() < 1.0, "it ends where the clip ends");
}
