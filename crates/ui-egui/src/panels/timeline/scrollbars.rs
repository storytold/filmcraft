use super::layout::timeline_extent;
use super::*;

/// The narrowest the zoom scroll bar's thumb gets: room for its two handles and a part between
/// them to grab.
const MIN_THUMB_W: f32 = 34.0;

/// Where the zoom scroll bar's thumb is: (left edge, width, seconds of scroll per point of thumb
/// travel). The thumb is as wide as the share of the Timeline in view (never narrower than
/// [`MIN_THUMB_W`]) and its travel covers the whole scroll range, so it reaches both ends of the
/// bar and moves with the pointer when dragged.
fn zoom_thumb(bar: Rect, seq_seconds: f64, scroll: f64, pps: f64) -> (f32, f32, f64) {
    let vis = bar.width() as f64 / pps.max(1e-9);
    let total = timeline_extent(seq_seconds).max(vis);
    let w = ((vis / total) as f32 * bar.width()).clamp(MIN_THUMB_W.min(bar.width()), bar.width());
    let travel = (bar.width() - w).max(0.0);
    let range = (total - vis).max(0.0);
    let at = if range > 0.0 { (scroll / range).clamp(0.0, 1.0) as f32 * travel } else { 0.0 };
    (bar.min.x + at, w, if travel > 0.5 { range / travel as f64 } else { 0.0 })
}

/// Draw the zoom scroll bar: the thumb reflects the visible range, a press beside it centres the thumb under the pointer, and the thumb and track are
/// registered for automation.
pub(super) fn zoom_scrollbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, seq_seconds: f64, t: &Tokens) {
    let p = ui.painter();
    p.rect_filled(bar, bar.height() / 2.0, t.separator);
    let v = &mut app.ui.timeline;
    let vis = bar.width() as f64 / v.pps;
    let total = timeline_extent(seq_seconds).max(vis);
    let (left, w, per_point) = zoom_thumb(bar, seq_seconds, v.scroll, v.pps);
    let thumb = Rect::from_min_max(pos2(left, bar.min.y), pos2(left + w, bar.max.y));
    let hover = ui.rect_contains_pointer(thumb);
    p.rect_filled(thumb, thumb.height() / 2.0, if hover { t.scroll_thumb_hover } else { t.scroll_thumb });
    for x in [thumb.min.x + thumb.height() / 2.0, thumb.max.x - thumb.height() / 2.0] {
        p.circle_filled(pos2(x, thumb.center().y), 4.5, t.panel_bg);
        p.circle_stroke(pos2(x, thumb.center().y), 4.5, Stroke::new(1.5, t.icon_active));
    }
    app.auto.add("timeline.zoomBar", thumb, "zoom scroll bar");
    app.auto.add("timeline.zoomBar.track", bar, "zoom scroll bar track");
    let resp = ui.interact(bar, egui::Id::unique("tl-zoombar"), Sense::click_and_drag());
    let range = (total - vis).max(0.0);
    // a press beside the thumb brings the thumb's middle under the pointer (and a drag goes on
    // from there)
    let jump = |v: &mut crate::state::TimelineView, x: f32| {
        if per_point > 0.0 {
            v.target_scroll = ((x - bar.min.x - w / 2.0) as f64 * per_point).clamp(0.0, range);
            v.scroll = v.target_scroll;
        }
    };
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos().filter(|pos| !thumb.contains(*pos))
    {
        jump(&mut app.ui.timeline, pos.x);
    }
    // (a drag is known to be one only after the pointer has moved: what was grabbed is what was
    // under the pointer when the button went down)
    if resp.drag_started()
        && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        let mode = if !thumb.expand2(vec2(0.0, 4.0)).contains(pos) {
            jump(&mut app.ui.timeline, pos.x);
            0
        } else if (pos.x - thumb.min.x).abs() < 9.0 {
            1
        } else if (pos.x - thumb.max.x).abs() < 9.0 {
            2
        } else {
            0
        };
        app.tl.drag = Some(Drag::ZoomBar { grab: pos.x, start: app.ui.timeline.target_scroll, start_pps: app.ui.timeline.target_pps, mode });
    }
    if matches!(app.tl.drag, Some(Drag::ZoomBar { .. }))
        && ui.input(|i| i.key_pressed(egui::Key::Escape))
        && let Some(Drag::ZoomBar { start, start_pps, .. }) = app.tl.drag.take()
    {
        let v = &mut app.ui.timeline;
        v.target_scroll = start;
        v.scroll = start;
        v.target_pps = start_pps;
        v.pps = start_pps;
    }
    if let Some(Drag::ZoomBar { grab, start, mode, .. }) = app.tl.drag.clone()
        && resp.dragged()
    {
        let v = &mut app.ui.timeline;
        match mode {
            // the thumb's middle: scroll, the thumb staying under the pointer
            0 => {
                if let Some(pos) = resp.interact_pointer_pos() {
                    v.target_scroll = (start + (pos.x - grab) as f64 * per_point).clamp(0.0, range);
                    v.scroll = v.target_scroll;
                }
            }
            // a handle: zoom, the other end of the view staying where it is
            _ => {
                let dx = resp.drag_delta().x as f64 / bar.width().max(1.0) as f64 * total;
                if mode == 1 {
                    let end = v.scroll + vis;
                    let ns = (v.scroll + dx).clamp(0.0, (end - 0.05).max(0.0));
                    v.pps = (bar.width() as f64 / (end - ns).max(0.05)).clamp(0.05, 24_000.0);
                    v.scroll = ns;
                    v.target_scroll = ns;
                } else {
                    let ne = (v.scroll + vis + dx).max(v.scroll + 0.05);
                    v.pps = (bar.width() as f64 / (ne - v.scroll)).clamp(0.05, 24_000.0);
                }
                v.target_pps = v.pps;
            }
        }
    }
    if resp.drag_stopped() {
        app.tl.drag = None;
    }
}

pub(super) fn vertical_scrollbar(ui: &mut egui::Ui, bar: Rect, value: &mut f32, max: f32, invert: bool, t: &Tokens, id: &str) {
    if max <= 0.5 || bar.height() < 20.0 {
        return;
    }
    let p = ui.painter();
    let frac_vis = bar.height() / (bar.height() + max);
    let h = (bar.height() * frac_vis).max(18.0);
    let pos = if invert { 1.0 - *value / max } else { *value / max };
    let y = bar.min.y + (bar.height() - h) * pos;
    let track = bar.shrink2(vec2(2.0, 0.0));
    let thumb = Rect::from_min_size(pos2(track.min.x, y), vec2(track.width(), h));
    p.rect_filled(track, track.width() / 2.0, t.separator);
    let hover = ui.rect_contains_pointer(thumb);
    p.rect_filled(thumb, thumb.width() / 2.0, if hover { t.scroll_thumb_hover } else { t.scroll_thumb });
    let resp = ui.interact(bar, egui::Id::unique(("tl-vs", id)), Sense::drag());
    if resp.dragged() {
        let d = resp.drag_delta().y / (bar.height() - h).max(1.0) * max;
        *value = (*value + if invert { -d } else { d }).clamp(0.0, max);
    }
}
