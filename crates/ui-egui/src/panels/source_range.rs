//! Source and Program In/Out handles and range translation. Preview is transient; release is one engine edit.
use super::monitor::Which;
use crate::{FilmcraftApp, dock::PanelKind};
use egui::{Rect, Sense, Stroke, pos2, vec2};
use filmcraft_engine::{
    clip_ops::source_view,
    source_monitor::{RangeEdge, adjust_range},
};
use filmcraft_project::ItemId;
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde_json::json;

#[derive(Clone, Copy, Debug)]
struct Drag {
    item: ItemId,
    edge: RangeEdge,
    bounds: TimeRange,
    original: TimeRange,
    preview: TimeRange,
    rate: FrameRate,
    pointer_x: f32,
    revision: u64,
}
fn key(which: Which) -> egui::Id {
    egui::Id::new(("monitor-range-edit", which as u8))
}
fn self_item(app: &FilmcraftApp, which: Which) -> Option<ItemId> {
    if which == Which::Source { app.session.state.source_item } else { app.session.state.active_sequence }
}
fn drag(ctx: &egui::Context, which: Which) -> Option<Drag> {
    ctx.data(|d| d.get_temp(key(which)))
}
fn clear(ctx: &egui::Context, which: Which) {
    ctx.data_mut(|d| d.remove::<Drag>(key(which)));
}

pub fn preview(app: &FilmcraftApp, ctx: &egui::Context) -> Option<TimeRange> {
    preview_for(app, ctx, Which::Source)
}
pub fn preview_for(app: &FilmcraftApp, ctx: &egui::Context, which: Which) -> Option<TimeRange> {
    drag(ctx, which).filter(|d| Some(d.item) == self_item(app, which) && d.revision == app.session.revision).map(|d| d.preview)
}

#[derive(Default)]
pub struct Interaction {
    pub range: Option<TimeRange>,
    pub in_hot: bool,
    pub out_hot: bool,
    pub handled: bool,
    pub scrub: Option<Tick>,
}

pub fn interact(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, which: Which) -> Interaction {
    let ctx = ui.ctx().clone();
    let Some(item) = self_item(app, which) else {
        clear(&ctx, which);
        return Interaction::default();
    };
    let Some(view) = source_view(&app.session, item) else {
        clear(&ctx, which);
        return Interaction::default();
    };
    let enabled = view.subclip.is_none() && view.start.0 >= 0 && view.end > view.start;
    if let Some(d) = drag(&ctx, which)
        && (d.item != item || d.revision != app.session.revision || ctx.input(|i| i.key_pressed(egui::Key::Escape)))
    {
        clear(&ctx, which);
    }
    let selected = preview_for(app, &ctx, which).unwrap_or_else(|| view.selected_range());
    let span = view.end.0.saturating_sub(view.start.0).max(1) as f64;
    let xof = |t: Tick| bar.min.x + (t.0.saturating_sub(view.start.0) as f64 / span).clamp(0.0, 1.0) as f32 * bar.width();
    let (a, b) = (xof(selected.start), xof(selected.end()));
    let mid = (a + b) * 0.5;
    let band = Rect::from_min_max(pos2(a, bar.min.y + 8.0), pos2(b, bar.max.y));
    let body = Rect::from_min_max(pos2((a + 6.0).min(mid), band.min.y), pos2((b - 6.0).max(mid), band.max.y));
    let inside = Rect::from_min_max(pos2(a - 6.0, bar.min.y), pos2((a + 6.0).min(mid), bar.max.y)).intersect(bar.expand2(vec2(6.0, 0.0)));
    let outside = Rect::from_min_max(pos2((b - 6.0).max(mid), bar.min.y), pos2(b + 6.0, bar.max.y)).intersect(bar.expand2(vec2(6.0, 0.0)));
    let mut result = Interaction::default();
    let ids = if which == Which::Source {
        ["source.range.body", "source.range.in", "source.range.out"]
    } else {
        ["program.range.body", "program.range.in", "program.range.out"]
    };
    for (id, label, edge, rect) in [
        (ids[0], "Move marked range", RangeEdge::Move, body),
        (ids[1], "Drag In point", RangeEdge::In, inside),
        (ids[2], "Drag Out point", RangeEdge::Out, outside),
    ] {
        if which == Which::Program
            && match edge {
                RangeEdge::In => view.mark_in.is_none(),
                RangeEdge::Out => view.mark_out.is_none(),
                RangeEdge::Move => view.mark_in.is_none() || view.mark_out.is_none(),
            }
        {
            continue;
        }
        app.auto.add(id, rect, label);
        let response =
            ui.add_enabled_ui(enabled && rect.width() > 0.0, |ui| ui.interact(rect, egui::Id::new(id), Sense::click_and_drag())).inner.on_hover_text(label);
        let cursor = if edge == RangeEdge::Move {
            if cfg!(windows) { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Grab }
        } else {
            egui::CursorIcon::ResizeHorizontal
        };
        let response = response.on_hover_cursor(cursor);
        result.handled |= response.hovered() || response.dragged() || response.clicked();
        if edge == RangeEdge::In {
            result.in_hot = response.hovered();
        }
        if edge == RangeEdge::Out {
            result.out_hot = response.hovered();
        }
        if response.drag_started() {
            let Some(pointer) = ctx.input(|i| i.pointer.press_origin()) else { continue };
            let bounds = TimeRange::from_bounds(view.start, view.end);
            let original = view.selected_range();
            if let Err(e) = adjust_range(bounds, original, view.rate, edge, Tick::ZERO) {
                app.ui.status = e.to_string();
                continue;
            }
            if which == Which::Source {
                app.stop_source();
            } else {
                app.stop();
            }
            app.ui.focused = if which == Which::Source { PanelKind::Source } else { PanelKind::Program };
            ctx.data_mut(|d| {
                d.insert_temp(
                    key(which),
                    Drag { item, edge, bounds, original, preview: original, rate: view.rate, pointer_x: pointer.x, revision: app.session.revision },
                )
            });
        }
        if let Some(mut d) = drag(&ctx, which).filter(|d| d.edge == edge && d.item == item) {
            result.handled = true;
            ctx.set_cursor_icon(cursor);
            if let Some(pos) = response.interact_pointer_pos() {
                let raw = (f64::from(pos.x - d.pointer_x) / f64::from(bar.width().max(1.0)) * d.bounds.duration.0 as f64)
                    .clamp(-(d.bounds.duration.0 as f64), d.bounds.duration.0 as f64);
                if raw.is_finite() {
                    match adjust_range(d.bounds, d.original, d.rate, d.edge, Tick(raw as i64)) {
                        Ok(r) => {
                            d.preview = r;
                            ctx.data_mut(|v| v.insert_temp(key(which), d));
                        }
                        Err(e) => {
                            app.ui.status = e.to_string();
                            clear(&ctx, which);
                        }
                    }
                }
            }
            if edge == RangeEdge::In {
                result.in_hot = true;
            }
            if edge == RangeEdge::Out {
                result.out_hot = true;
            }
            if response.drag_stopped() {
                clear(&ctx, which);
                if d.preview != d.original && d.revision == app.session.revision && self_item(app, which) == Some(item) {
                    let mut params = json!({"item":item.0});
                    if edge != RangeEdge::Out {
                        params["in"] = json!(d.preview.start.0);
                    }
                    if edge != RangeEdge::In {
                        params["out"] = json!(d.preview.end().0.saturating_sub(d.rate.frame_duration().0).max(view.start.0));
                    }
                    if let Err(e) = app.session.execute("project.setMarks", params) {
                        app.ui.status = e.to_string();
                    }
                }
            } else if !ctx.input(|i| i.pointer.any_down()) {
                clear(&ctx, which);
            }
        }
        if response.clicked()
            && edge == RangeEdge::Move
            && let Some(pos) = response.interact_pointer_pos()
        {
            let time = view.start.0.saturating_add(((f64::from(pos.x - bar.min.x) / f64::from(bar.width().max(1.0))).clamp(0.0, 1.0) * span) as i64);
            if let Err(e) = app.session.execute(if which == Which::Source { "source.setPlayhead" } else { "playhead.set" }, json!({"time":time})) {
                app.ui.status = e.to_string();
            }
        }
    }
    let time = if which == Which::Source { app.session.state.source_playhead } else { app.session.playhead() };
    let x = xof(time);
    let rect = Rect::from_min_max(pos2(x - 7.0, bar.max.y - 12.0), pos2(x + 7.0, bar.max.y));
    let id = if which == Which::Source { "source.playhead" } else { "program.playhead" };
    app.auto.add(id, rect, "Scrub playhead");
    let playhead = ui.interact(rect, egui::Id::new(id), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    if (playhead.dragged() || playhead.clicked())
        && let Some(pos) = playhead.interact_pointer_pos()
    {
        let f = (f64::from(pos.x - bar.min.x) / f64::from(bar.width().max(1.0))).clamp(0.0, 1.0);
        result.scrub = Some(view.rate.snap(Tick(view.start.0.saturating_add((f * span) as i64))));
        result.handled = true;
    }
    result.range = preview_for(app, &ctx, which);
    result
}

/// Original generic red bracket and bidirectional arrows; no third-party artwork.
pub fn trim_cue(p: &egui::Painter, x: f32, bar: Rect, inward: f32) {
    let red = crate::panels::trim_monitor::RIPPLE_RED;
    let (top, bottom) = (bar.min.y + 8.0, bar.max.y);
    let mid = (top + bottom) * 0.5;
    let stroke = Stroke::new(1.2, red);
    p.line(vec![pos2(x + inward, top), pos2(x, top), pos2(x, bottom), pos2(x + inward, bottom)], stroke);
    p.line_segment([pos2(x - 9.0, mid), pos2(x + 9.0, mid)], stroke);
    for direction in [-1.0_f32, 1.0] {
        p.line(vec![pos2(x + direction * 6.0, mid - 2.5), pos2(x + direction * 9.0, mid), pos2(x + direction * 6.0, mid + 2.5)], stroke);
    }
}
