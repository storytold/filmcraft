//! The mouse cursor for the active tool over the Timeline and the monitors.
//!
//! Tools with a fitting system cursor use it (Selection, Hand, Zoom, Type). The rest have no
//! system equivalent, so the pointer is hidden and the tool's own glyph from [`crate::icons`] is
//! painted at it with a dark outline, the glyph's hotspot on the pointer. The glyphs are FilmCraft's
//! original code-drawn tool icons (`ATTRIBUTION.md`); no cursor artwork is copied from any
//! other application.

use egui::{Color32, Context, CursorIcon, Id, LayerId, Modifiers, Order, Pos2, Rect, Stroke, pos2, vec2};

use crate::icons::{self, Icon};
use crate::panels::timeline_hit::EdgeKind;
use crate::state::Tool;

/// A tool icon used as the cursor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drawn {
    pub icon: Icon,
    /// The point of the glyph (16×16 icon grid) that sits on the pointer.
    pub hotspot: (f32, f32),
    /// Draw a crosshair on the pointer and the icon beside it as a badge (shape tools), instead of
    /// the icon itself on the pointer.
    pub crosshair: bool,
}

/// The cursor a tool shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToolCursor {
    System(CursorIcon),
    Drawn(Drawn),
}

const fn drawn(icon: Icon, hotspot: (f32, f32)) -> ToolCursor {
    ToolCursor::Drawn(Drawn { icon, hotspot, crosshair: false })
}

const fn badge(icon: Icon) -> ToolCursor {
    ToolCursor::Drawn(Drawn { icon, hotspot: (8.0, 8.0), crosshair: true })
}

/// The cursor of `tool` away from anything that changes it (a clip edge, a Volume line…).
/// `mods.alt` turns the Zoom tool into zoom out, as its click does.
pub fn base(tool: Tool, mods: Modifiers) -> ToolCursor {
    match tool {
        Tool::Selection => ToolCursor::System(CursorIcon::Default),
        // hotspots: the arrow's tip, the bracket the edit happens at, or the glyph's centre
        Tool::TrackSelectForward => drawn(Icon::TrackSelectFwd, (2.0, 3.0)),
        Tool::TrackSelectBackward => drawn(Icon::TrackSelectBack, (14.0, 3.0)),
        Tool::Ripple => drawn(Icon::Ripple, (6.5, 8.0)),
        Tool::Rolling => drawn(Icon::Rolling, (8.0, 8.0)),
        Tool::RateStretch => drawn(Icon::RateStretch, (8.0, 8.0)),
        Tool::Remix => drawn(Icon::Remix, (8.0, 8.0)),
        Tool::Razor => drawn(Icon::Razor, (8.0, 8.0)),
        Tool::Slip => drawn(Icon::Slip, (8.0, 8.0)),
        Tool::Slide => drawn(Icon::Slide, (8.0, 8.0)),
        Tool::Pen => drawn(Icon::Pen, (8.0, 2.0)),
        Tool::Rectangle => badge(Icon::Rectangle),
        Tool::Ellipse => badge(Icon::Ellipse),
        Tool::Hand => ToolCursor::System(CursorIcon::Grab),
        Tool::Zoom if mods.alt => ToolCursor::System(CursorIcon::ZoomOut),
        Tool::Zoom => ToolCursor::System(CursorIcon::ZoomIn),
        Tool::Type => ToolCursor::System(CursorIcon::Text),
        Tool::VerticalType => ToolCursor::System(CursorIcon::VerticalText),
    }
}

/// The cursor over a clip edge, by the trim a press there starts: a plain trim keeps the resize
/// cursor, the others show the tool that does them, so the Selection tool's modifier and
/// edge-zone trims (#259) say which trim they will be.
pub fn edge(kind: EdgeKind) -> ToolCursor {
    match kind {
        EdgeKind::Trim => ToolCursor::System(CursorIcon::ResizeColumn),
        EdgeKind::Ripple => base(Tool::Ripple, Modifiers::NONE),
        EdgeKind::Roll { .. } => base(Tool::Rolling, Modifiers::NONE),
        EdgeKind::Stretch => base(Tool::RateStretch, Modifiers::NONE),
        EdgeKind::Remix => base(Tool::Remix, Modifiers::NONE),
    }
}

/// The cursor while a drag started by `tool` is under way.
pub fn dragging(tool: Tool, mods: Modifiers) -> ToolCursor {
    match tool {
        Tool::Hand => ToolCursor::System(CursorIcon::Grabbing),
        _ => base(tool, mods),
    }
}

#[derive(Clone, Copy)]
struct Pending {
    drawn: Drawn,
    pos: Pos2,
    pass: u64,
}

fn pending_id() -> Id {
    Id::unique("filmcraft-tool-cursor")
}

/// Show `cursor` with the pointer at `pos` this frame. Like [`Context::set_cursor_icon`], a later
/// call this frame wins: a drawn cursor is painted only if the pointer is still hidden when the
/// frame ends ([`paint_pending`]).
pub fn set(ctx: &Context, cursor: ToolCursor, pos: Pos2) {
    match cursor {
        ToolCursor::System(icon) => ctx.set_cursor_icon(icon),
        ToolCursor::Drawn(drawn) => {
            ctx.set_cursor_icon(CursorIcon::None);
            let pass = ctx.cumulative_pass_nr();
            ctx.data_mut(|d| d.insert_temp(pending_id(), Pending { drawn, pos, pass }));
        }
    }
}

/// Paint the drawn cursor [`set`] asked for this frame, on top of everything. Called once at the
/// end of the UI pass.
pub fn paint_pending(ctx: &Context) {
    let Some(p) = ctx.data_mut(|d| {
        let p = d.get_temp::<Pending>(pending_id());
        d.remove::<Pending>(pending_id());
        p
    }) else {
        return;
    };
    if p.pass != ctx.cumulative_pass_nr() || ctx.output(|o| o.cursor_icon) != CursorIcon::None {
        return;
    }
    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, pending_id()));
    paint(&painter, p.drawn, p.pos);
}

/// Glyph size in points: a little larger than a toolbar icon so it reads as a pointer.
const SIZE: f32 = 20.0;

fn paint(painter: &egui::Painter, d: Drawn, pos: Pos2) {
    let s = SIZE / 16.0;
    let outline = Color32::from_black_alpha(220);
    let (rect, hot) = if d.crosshair {
        // the crosshair on the pointer, the icon as a smaller badge below right
        let arm = 7.0;
        let gap = 2.0;
        for (w, c) in [(3.0, outline), (1.0, Color32::WHITE)] {
            let st = Stroke::new(w, c);
            painter.line_segment([pos2(pos.x - arm, pos.y), pos2(pos.x - gap, pos.y)], st);
            painter.line_segment([pos2(pos.x + gap, pos.y), pos2(pos.x + arm, pos.y)], st);
            painter.line_segment([pos2(pos.x, pos.y - arm), pos2(pos.x, pos.y - gap)], st);
            painter.line_segment([pos2(pos.x, pos.y + gap), pos2(pos.x, pos.y + arm)], st);
        }
        (Rect::from_min_size(pos + vec2(5.0, 5.0), vec2(12.0, 12.0)), None)
    } else {
        let min = pos - vec2(d.hotspot.0 * s, d.hotspot.1 * s);
        (Rect::from_min_size(min, vec2(SIZE, SIZE)), Some(pos))
    };
    for o in [vec2(-1.0, 0.0), vec2(1.0, 0.0), vec2(0.0, -1.0), vec2(0.0, 1.0), vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0), vec2(1.0, -1.0)] {
        icons::paint(painter, rect.translate(o), d.icon, outline);
    }
    icons::paint(painter, rect, d.icon, Color32::WHITE);
    // a dot on the exact hotspot, so the point that acts is visible on every glyph
    if let Some(h) = hot {
        painter.circle_filled(h, 1.5, outline);
        painter.circle_filled(h, 0.75, Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool in the tools panel has its own cursor, so the pointer always says which tool is
    /// active (Track Select Forward used to show the plain arrow and Razor a crosshair).
    #[test]
    fn every_tool_has_a_distinct_cursor() {
        let all: Vec<(Tool, ToolCursor)> = Tool::ALL.iter().map(|t| (*t, base(*t, Modifiers::NONE))).collect();
        for (i, (a, ca)) in all.iter().enumerate() {
            for (b, cb) in &all[i + 1..] {
                assert_ne!(ca, cb, "{a:?} and {b:?} share a cursor");
            }
        }
    }

    #[test]
    fn tools_without_a_system_cursor_draw_their_own_icon() {
        assert_eq!(base(Tool::TrackSelectForward, Modifiers::NONE), drawn(Icon::TrackSelectFwd, (2.0, 3.0)));
        assert!(matches!(base(Tool::Razor, Modifiers::NONE), ToolCursor::Drawn(Drawn { icon: Icon::Razor, crosshair: false, .. })));
        assert_eq!(base(Tool::Selection, Modifiers::NONE), ToolCursor::System(CursorIcon::Default));
        assert_eq!(base(Tool::Hand, Modifiers::NONE), ToolCursor::System(CursorIcon::Grab));
        assert_eq!(base(Tool::Type, Modifiers::NONE), ToolCursor::System(CursorIcon::Text));
        assert_eq!(base(Tool::VerticalType, Modifiers::NONE), ToolCursor::System(CursorIcon::VerticalText));
    }

    /// Every drawn hotspot lies on the 16×16 icon grid, so the acting point is under the glyph.
    #[test]
    fn hotspots_are_on_the_glyph() {
        for t in Tool::ALL {
            if let ToolCursor::Drawn(d) = base(t, Modifiers::NONE) {
                assert!((0.0..=16.0).contains(&d.hotspot.0) && (0.0..=16.0).contains(&d.hotspot.1), "{t:?}");
            }
        }
    }

    #[test]
    fn zoom_out_with_alt_and_grabbing_while_panning() {
        assert_eq!(base(Tool::Zoom, Modifiers::ALT), ToolCursor::System(CursorIcon::ZoomOut));
        assert_eq!(dragging(Tool::Hand, Modifiers::NONE), ToolCursor::System(CursorIcon::Grabbing));
        assert_eq!(dragging(Tool::Razor, Modifiers::NONE), base(Tool::Razor, Modifiers::NONE));
    }

    /// At a clip edge the cursor names the trim a press starts.
    #[test]
    fn edge_cursor_follows_the_trim_kind() {
        use filmcraft_project::ClipId;
        assert_eq!(edge(EdgeKind::Trim), ToolCursor::System(CursorIcon::ResizeColumn));
        assert_eq!(edge(EdgeKind::Ripple), base(Tool::Ripple, Modifiers::NONE));
        assert_eq!(edge(EdgeKind::Roll { left: ClipId(1), right: ClipId(2) }), base(Tool::Rolling, Modifiers::NONE));
        assert_eq!(edge(EdgeKind::Stretch), base(Tool::RateStretch, Modifiers::NONE));
        assert_eq!(edge(EdgeKind::Remix), base(Tool::Remix, Modifiers::NONE));
    }

    /// A drawn cursor is painted only while nothing later in the frame replaced it.
    #[test]
    fn a_later_system_cursor_wins_over_a_drawn_one() {
        let ctx = Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            set(ui.ctx(), base(Tool::Razor, Modifiers::NONE), pos2(10.0, 10.0));
            assert_eq!(ui.ctx().output(|o| o.cursor_icon), CursorIcon::None);
            ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
            paint_pending(ui.ctx());
            assert_eq!(ui.ctx().output(|o| o.cursor_icon), CursorIcon::ResizeColumn);
            assert!(ui.ctx().data_mut(|d| d.get_temp::<Pending>(pending_id())).is_none(), "the request is consumed");
        });
        out.textures_delta.clear();
    }
}
