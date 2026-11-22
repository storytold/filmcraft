//! The Slip Monitor: while the Slip tool (Y) drags a clip, the Program monitor shows the slip's
//! four frames on a black canvas the shape of the sequence frame: the slipped clip's new first and
//! last frames large, side by side across the canvas, with their source timecodes over them, and
//! the previous clip's last frame and the next clip's first frame half that size above their
//! outer corners. The large two follow the drag; the small two stay put. Below the canvas are the
//! slip offset as a signed timecode and in frames; the Timeline shows the offset at the pointer.
//!
//! The frames come from [`filmcraft_engine::slip::preview`] (`timeline.slipPreview`), worked out
//! once per change of the drag's delta ([`preview`], kept in [`Drag::Slip`]) with the clamp
//! `timeline.slip` applies on release, so what is shown is what is committed. The project is not
//! touched while dragging. Each picture asks the frame server for its frame in its own view
//! ([`crate::frames::FrameServer::request_in_view`]): the newest position replaces the ones the
//! pointer has left behind, and the nearest decoded frame stands in until it's ready, so a fast
//! drag over long-GOP 4K media never queues up stale decodes or blocks the UI.
//!
//! The monitor is shown for as long as the drag lasts: releasing commits the slip, Escape drops
//! it (#580), and either way the Program monitor is back the next frame.
//!
//! Automation ids: `slipMonitor.previous`, `slipMonitor.in`, `slipMonitor.out`,
//! `slipMonitor.next`, `slipMonitor.previousTimecode`, `slipMonitor.inTimecode`,
//! `slipMonitor.outTimecode`, `slipMonitor.nextTimecode`, `slipMonitor.offset`,
//! `slipMonitor.offsetTimecode`.

use std::sync::Arc;

use egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::ClipId;
use filmcraft_time::{Tick, TimeDisplay, format_time};
use serde_json::Value;

use crate::FilmcraftApp;
use crate::panels::timeline::Drag;
use crate::panels::trim_monitor::{RIPPLE_RED, ROLL_YELLOW, picture, shift_text};
use crate::theme::Tokens;

/// Frame server views of the four pictures (one item can fill all four: clips cut from one take).
const VIEWS: [u32; 4] = [3, 4, 5, 6];
const TEXTURES: [&str; 4] = ["slip-previous", "slip-in", "slip-out", "slip-next"];
const IDS: [&str; 4] = ["slipMonitor.previous", "slipMonitor.in", "slipMonitor.out", "slipMonitor.next"];
/// The neighbours' pictures' size relative to the slipped clip's two.
const SMALL: f32 = 0.5;

/// What slipping `clip` by `delta` shows (`timeline.slipPreview`); `None` when it can't be worked
/// out (no such clip any more, a locked track), and the Program monitor stays as it is.
pub fn preview(app: &FilmcraftApp, clip: ClipId, delta: Tick) -> Option<Arc<Value>> {
    filmcraft_engine::slip::preview(&app.session, clip, delta).ok().map(Arc::new)
}

/// The preview of the Slip drag in progress, if one is.
fn current(app: &FilmcraftApp) -> Option<Arc<Value>> {
    match &app.tl.drag {
        Some(Drag::Slip { preview: Some(p), .. }) => Some(p.clone()),
        _ => None,
    }
}

/// True while a Slip drag replaces the Program monitor with the Slip Monitor.
pub fn active(app: &FilmcraftApp) -> bool {
    current(app).is_some()
}

/// The slip offset as a signed sequence timecode (`+00:00:05:20`).
pub fn offset_timecode(app: &FilmcraftApp, info: &Value) -> String {
    let n = info["offsetFrames"].as_i64().unwrap_or(0);
    let (rate, df) = app.session.active_sequence().map(|q| (q.settings.frame_rate, q.settings.drop_frame)).unwrap_or_default();
    let tc = format_time(rate.tick_of(n.saturating_abs()), rate, df, TimeDisplay::Timecode, 48000);
    format!("{}{tc}", if n < 0 { "-" } else { "+" })
}

/// The slip offset in frames, and whether the media's end holds it back ("Slip: +25 frames").
fn offset_text(info: &Value) -> (String, bool) {
    let n = info["offsetFrames"].as_i64().unwrap_or(0);
    let offset = shift_text(n);
    let text = if n.abs() == 1 { tlf!("Slip: {offset} frame", offset) } else { tlf!("Slip: {offset} frames", offset) };
    let at_limit = info["atLimit"].as_bool().unwrap_or(false);
    if at_limit { (format!("{text}  ·  {}", tl!("at the media limit")), true) } else { (text, false) }
}

/// Where the four pictures go on a canvas `w` wide: the slipped clip's In and Out side by side
/// across it, the neighbours at `SMALL` size above their outer corners, the group centred in
/// `area` (scaled down when `area` is too short for it). Returns `[previous, in, out, next]`.
fn layout(area: Rect, aspect: f32) -> [Rect; 4] {
    let mut big_w = area.width() / 2.0;
    let total_h = |w: f32| w / aspect * (1.0 + SMALL);
    if total_h(big_w) > area.height() {
        big_w = area.height() / (1.0 + SMALL) * aspect;
    }
    let big_h = big_w / aspect;
    let (small_w, small_h) = (big_w * SMALL, big_h * SMALL);
    let left = area.center().x - big_w;
    let top = area.center().y - total_h(big_w) / 2.0;
    let big_top = top + small_h;
    [
        Rect::from_min_size(pos2(left, top), vec2(small_w, small_h)),
        Rect::from_min_size(pos2(left, big_top), vec2(big_w, big_h)),
        Rect::from_min_size(pos2(left + big_w, big_top), vec2(big_w, big_h)),
        Rect::from_min_size(pos2(left + 2.0 * big_w - small_w, top), vec2(small_w, small_h)),
    ]
}

/// White text with a dark outline, readable over any picture. Returns its rect.
fn overlay_text(p: &Painter, at: Pos2, anchor: Align2, text: &str, font: FontId) -> Rect {
    let shadow = Color32::from_black_alpha(200);
    for d in [vec2(-1.5, 0.0), vec2(1.5, 0.0), vec2(0.0, -1.5), vec2(0.0, 1.5), vec2(1.5, 1.5)] {
        p.text(at + d, anchor, text, font.clone(), shadow);
    }
    p.text(at, anchor, text, font, Color32::WHITE)
}

/// The Slip Monitor in the Program monitor's `rect`.
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let Some(info) = current(app) else { return };
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    let (sw, sh) = app.session.active_sequence().map(|q| (q.settings.width.max(1) as f32, q.settings.height.max(1) as f32)).unwrap_or((16.0, 9.0));
    let aspect = sw / sh;

    // the canvas: the sequence frame's shape, black, above a bar with the offset
    let bar_h = 34.0;
    let area = Rect::from_min_max(rect.min + vec2(8.0, 8.0), pos2(rect.max.x - 8.0, rect.max.y - bar_h));
    let s = (area.width() / sw).min(area.height() / sh).max(0.0);
    let canvas = Rect::from_center_size(area.center(), vec2(sw * s, sh * s));
    ui.painter().rect_filled(canvas, 0.0, Color32::BLACK);
    let slots = layout(canvas, aspect);

    let sides = [&info["previous"], &info["in"], &info["out"], &info["next"]];
    let mut pics = slots;
    for (i, side) in sides.iter().enumerate() {
        let slot = slots[i];
        pics[i] = if side.is_null() {
            // no clip touches it on this side: black, as at a gap
            ui.painter().rect_stroke(slot, 0.0, Stroke::new(1.0, Color32::from_gray(48)), StrokeKind::Inside);
            ui.painter().text(slot.center(), Align2::CENTER_CENTER, tl!("(gap)"), Tokens::ui(11.0), t.text_faint);
            slot
        } else {
            picture(app, ui, slot, side, TEXTURES[i], VIEWS[i]).0
        };
        app.auto.add(IDS[i], pics[i], side["sourceTimecode"].as_str().unwrap_or(""));
    }

    // the new source In and Out timecodes over the large pictures
    let size = (pics[1].height() * 0.11).clamp(11.0, 40.0);
    for i in [1, 2] {
        let tc = sides[i]["sourceTimecode"].as_str().unwrap_or("--:--:--:--");
        let r = overlay_text(ui.painter(), pos2(pics[i].center().x, pics[i].max.y - size * 0.6), Align2::CENTER_BOTTOM, tc, Tokens::semibold(size));
        app.auto.add(&format!("{}Timecode", IDS[i]), r, tc);
    }
    // the neighbours' timecodes, small, inside their pictures
    let small = (pics[0].height() * 0.12).clamp(9.0, 16.0);
    for (i, anchor) in [(0, Align2::LEFT_BOTTOM), (3, Align2::RIGHT_BOTTOM)] {
        let Some(tc) = sides[i]["sourceTimecode"].as_str() else { continue };
        let at = if i == 0 { pos2(pics[i].min.x + 6.0, pics[i].max.y - 4.0) } else { pos2(pics[i].max.x - 6.0, pics[i].max.y - 4.0) };
        let r = overlay_text(ui.painter(), at, anchor, tc, Tokens::ui(small));
        app.auto.add(&format!("{}Timecode", IDS[i]), r, tc);
    }
    // which clips these are, in the black space between the neighbours
    let gap = Rect::from_min_max(pos2(slots[0].max.x + 12.0, slots[0].min.y), pos2(slots[3].min.x - 12.0, slots[1].min.y));
    if gap.width() > 40.0 && gap.height() > 24.0 {
        let name = |v: &Value| v["name"].as_str().unwrap_or("").to_string();
        let (prev, slipped, next) = (name(sides[0]), name(sides[1]), name(sides[3]));
        let lines = [
            (Some(slipped).filter(|n| !n.is_empty()), Tokens::semibold(12.0), t.text),
            (Some(prev).filter(|n| !n.is_empty()).map(|name| tlf!("Previous: {name}", name)), Tokens::ui(10.5), t.text_dim),
            (Some(next).filter(|n| !n.is_empty()).map(|name| tlf!("Next: {name}", name)), Tokens::ui(10.5), t.text_dim),
        ];
        let mut y = gap.center().y - 20.0;
        for (text, font, col) in lines {
            if let Some(text) = text {
                let text = elide(ui, &text, &font, gap.width());
                ui.painter().text(pos2(gap.center().x, y), Align2::CENTER_CENTER, text, font, col);
            }
            y += 17.0;
        }
    }

    // the offset: a signed timecode (as in the monitor's time display) and in frames
    let bar = Rect::from_min_max(pos2(canvas.min.x, canvas.max.y + 4.0), pos2(canvas.max.x, rect.max.y));
    let tc = offset_timecode(app, &info);
    let r = ui.painter().text(pos2(bar.min.x + 2.0, bar.center().y), Align2::LEFT_CENTER, &tc, Tokens::semibold(14.0), t.hot_text);
    app.auto.add("slipMonitor.offsetTimecode", r, &tc);
    let (text, at_limit) = offset_text(&info);
    let r = ui.painter().text(
        pos2(bar.max.x - 2.0, bar.center().y),
        Align2::RIGHT_CENTER,
        &text,
        Tokens::semibold(13.0),
        if at_limit { RIPPLE_RED } else { ROLL_YELLOW },
    );
    app.auto.add("slipMonitor.offset", r, &text);
}

/// The offset next to the pointer in the Timeline while slipping (as the Program monitor shows
/// it), in a small tag under `at`.
pub fn paint_timeline_offset(app: &FilmcraftApp, p: &Painter, at: Pos2) {
    let Some(info) = current(app) else { return };
    let t = app.tokens;
    let tc = offset_timecode(app, &info);
    let font = Tokens::semibold(11.0);
    let galley = p.layout_no_wrap(tc, font, t.text);
    let r = Rect::from_center_size(at + vec2(0.0, 26.0), galley.size() + vec2(12.0, 6.0));
    p.rect_filled(r, 4.0, Color32::from_black_alpha(200));
    p.rect_stroke(r, 4.0, Stroke::new(1.0, Color32::from_gray(90)), StrokeKind::Inside);
    p.galley(r.center() - galley.size() / 2.0, galley, t.text);
}

/// `text` cut to fit `width` with a trailing "…".
fn elide(ui: &egui::Ui, text: &str, font: &egui::FontId, width: f32) -> String {
    let fits = |s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x <= width;
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len()).rev().map(|n| format!("{}…", chars.iter().take(n).collect::<String>())).find(|s| fits(s)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_large_pair_spans_the_canvas_and_the_neighbours_sit_on_its_outer_corners() {
        let canvas = Rect::from_min_size(pos2(100.0, 50.0), vec2(1600.0, 900.0));
        let [prev, inn, out, next] = layout(canvas, 16.0 / 9.0);
        assert_eq!((inn.min.x, out.max.x), (100.0, 1700.0), "In and Out fill the width");
        assert_eq!(inn.max.x, out.min.x);
        assert!((inn.width() - 800.0).abs() < 1e-3 && (inn.height() - 450.0).abs() < 1e-3);
        assert!((prev.width() - 400.0).abs() < 1e-3 && (next.width() - 400.0).abs() < 1e-3);
        assert_eq!((prev.min.x, prev.max.y), (inn.min.x, inn.min.y), "previous on the In's top-left corner");
        assert_eq!((next.max.x, next.max.y), (out.max.x, out.min.y), "next on the Out's top-right corner");
        // the group (675 high) is centred in the 900 high canvas
        assert!((prev.min.y - (50.0 + 112.5)).abs() < 1e-3);
    }

    #[test]
    fn a_short_canvas_scales_the_group_down_to_fit() {
        let canvas = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 300.0));
        let [prev, inn, out, _] = layout(canvas, 16.0 / 9.0);
        assert!(prev.min.y >= -1e-3 && inn.max.y <= 300.0 + 1e-3);
        assert!((inn.height() * 1.5 - 300.0).abs() < 1e-3);
        assert!(((inn.center().x + out.center().x) / 2.0 - 800.0).abs() < 1e-3, "centred");
    }
}
