//! The Razor tool (C) on a caption in the Timeline cuts it, as it cuts clips. The real
//! `FilmcraftApp` under `egui_kittest` with the demo project.

use super::caption_option_drag_ui::Driver;
use crate::support::{Drive, Launch};
use egui::{Modifiers, Pos2};
use serde_json::json;

impl Driver {
    fn click_at(&mut self, at: Pos2) {
        self.drag_with(at, at, 0, Modifiers::NONE);
    }
}

/// With the Razor tool (C), a click on a caption in the Timeline cuts it in two there. The
/// caption blocks took the click for selecting and moving whatever the tool, so it did nothing.
#[test]
fn razor_tool_splits_a_caption() {
    let mut d = Driver::demo_snap_off();
    let id = d.exec("captions.add", json!({"seconds": 2.0, "durationSeconds": 4.0, "text": "cut me"}))["caption"].as_u64().expect("id");
    d.ok("ui.set", json!({"tool": "razor"}));
    let r = d.rect(&format!("timeline.caption.{id}")).expect("caption block");
    d.click_at(r.center());
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "the caption was cut: {caps:?}");
    assert_eq!(caps[0].1, caps[1].0, "the halves meet at the cut");
    assert!(caps.iter().all(|c| c.2 == "cut me"));
}

const TICKS_PER_SECOND: f64 = 254_016_000_000.0;

/// Clips on every video and audio track of the active sequence.
fn clip_count(d: &mut Driver) -> usize {
    let s = d.exec("sequence.inspect", json!({}));
    ["video", "audio"].iter().flat_map(|k| s[*k].as_array().cloned().unwrap_or_default()).map(|t| t["items"].as_array().map_or(0, Vec::len)).sum()
}

/// A caption over the whole first V1 clip; returns (clip id, caption id).
fn caption_over_first_clip(d: &mut Driver) -> (u64, u64) {
    let s = d.exec("sequence.inspect", json!({}));
    let item = s["video"][0]["items"][0].clone();
    let clip = item["clip"].as_u64().expect("a V1 clip");
    let start = item["start"].as_f64().expect("start") / TICKS_PER_SECOND;
    let duration = item["duration"].as_f64().expect("duration") / TICKS_PER_SECOND;
    let cap = d.exec("captions.add", json!({"seconds": start, "durationSeconds": duration, "text": "all tracks"}))["caption"].as_u64().expect("id");
    (clip, cap)
}

/// A point inside `r`, near its left end so it is on screen even for a long block.
fn inside(r: Rect) -> Pos2 {
    pos2(r.min.x + (r.width() / 2.0).min(40.0), r.center().y)
}

/// Shift+click with the Razor on a clip cuts every unlocked track, caption tracks included, as in
/// Premiere Pro (the tooltip: "Shift-click to split all tracks"). It cut the clip tracks only.
#[test]
fn shift_razor_on_a_clip_cuts_the_captions_too() {
    let mut d = Driver::demo();
    let (clip, _) = caption_over_first_clip(&mut d);
    let clips_before = clip_count(&mut d);
    d.ok("ui.set", json!({"tool": "razor"}));
    let r = d.rect(&format!("timeline.clip.{clip}")).expect("clip block");
    d.drag_with(inside(r), inside(r), 0, Modifiers::SHIFT);
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "the caption was cut with the clips: {caps:?}");
    assert_eq!(caps[0].1, caps[1].0, "the halves meet at the cut");
    assert!(clip_count(&mut d) > clips_before, "the clips were cut");
}

/// Shift+click with the Razor on a caption cuts the clips under it too, not only the captions.
#[test]
fn shift_razor_on_a_caption_cuts_the_clips_too() {
    let mut d = Driver::demo();
    let (_, cap) = caption_over_first_clip(&mut d);
    let clips_before = clip_count(&mut d);
    d.ok("ui.set", json!({"tool": "razor"}));
    let r = d.rect(&format!("timeline.caption.{cap}")).expect("caption block");
    d.drag_with(inside(r), inside(r), 0, Modifiers::SHIFT);
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "the caption was cut: {caps:?}");
    assert!(clip_count(&mut d) > clips_before, "the clips under the caption were cut too");
}
