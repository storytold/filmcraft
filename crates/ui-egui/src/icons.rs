//! Vector icons: one Tabler-style SVG file per icon, drawn by [`icons_svg`] (crisp at any DPI,
//! recolourable, no Adobe artwork). `ATTRIBUTION.md` lists each file and its licence.

use egui::{Color32, Painter, Rect, vec2};
use serde::{Deserialize, Serialize};

use crate::icons_svg;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Icon {
    Selection,
    TrackSelectFwd,
    TrackSelectBack,
    Ripple,
    Rolling,
    RateStretch,
    Remix,
    Razor,
    Slip,
    Slide,
    Pen,
    Rectangle,
    Ellipse,
    Hand,
    Zoom,
    Type,
    Play,
    Pause,
    StepBack,
    StepFwd,
    GoToIn,
    GoToOut,
    MarkIn,
    MarkOut,
    Marker,
    Insert,
    Overwrite,
    Lift,
    Extract,
    Camera,
    Loop,
    Wrench,
    Plus,
    Eye,
    EyeOff,
    Speaker,
    Mute,
    Lock,
    Unlock,
    SyncLock,
    Mic,
    Folder,
    Film,
    Sequence,
    Audio,
    Image,
    Search,
    ListView,
    IconView,
    Freeform,
    NewItem,
    Trash,
    Home,
    Workspaces,
    Hamburger,
    ChevronDown,
    ChevronRight,
    Magnet,
    Link,
    Keyframe,
    Stopwatch,
    Fx,
    Reset,
    Close,
    Fullscreen,
    Export,
    /// An arrow down into a tray (Import).
    Import,
    Gear,
    Info,
    Captions,
    Adjust,
    Nest,
    Undo,
    Redo,
    Bell,
    /// Community chat (the ArtCraft Discord): a speech bubble with three dots. Our own drawing,
    /// not the Discord logo.
    Chat,
    /// Website: a globe.
    Globe,
    /// Source code (GitHub repository): angle brackets and a slash.
    Code,
    Sparkle,
    Grid,
    Square,
    /// Toggle Proxies: a small frame inside a large one.
    Proxy,
    /// Offline media: a broken link.
    Offline,
    /// Mask tracking: backward continuously / one frame, forward one frame / continuously.
    TrackMaskBack,
    TrackMaskBackFrame,
    TrackMaskFwdFrame,
    TrackMaskFwd,
    /// Project panel footer: Sort Icons (lines of decreasing length).
    SortIcons,
    /// Project panel footer: Automate to Sequence (three clips in a row).
    Automate,
    /// Media Browser: a favourite (five-pointed star).
    Star,
    /// Media Browser: back / forward / up one level.
    ChevronLeft,
    ArrowUp,
    /// Media Browser: a local drive and a network location.
    Drive,
    Network,
    /// Media Browser: recent directories (a clock face).
    Clock,
    /// Appearance Mode: Auto (a display on a stand), Light (a sun), Dark (a crescent moon).
    Monitor,
    Sun,
    Moon,
    /// Colour parameters: pick a colour from the Program monitor (a pipette).
    Eyedropper,
}

/// Paint `icon` centred in `rect` from its SVG file ([`icons_svg::svg_for`]). The table has an entry
/// for every icon (a test checks it). An icon without one draws nothing: no panic, no fallback.
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    if let Some(svg) = icons_svg::svg_for(icon) {
        icons_svg::paint_svg(painter, rect, svg, color);
    }
}

/// An icon button: returns the response; `active` draws the selected state.
pub fn button(ui: &mut egui::Ui, icon: Icon, size: f32, active: bool, t: &crate::theme::Tokens, tooltip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), egui::Sense::click());
    let bg = if resp.is_pointer_button_down_on() {
        Some(t.pressed)
    } else if resp.hovered() {
        Some(t.hover)
    } else {
        None
    };
    if let Some(bg) = bg {
        ui.painter().rect_filled(rect, t.radius_sm, bg);
    }
    let col = if active {
        t.icon_active
    } else if resp.hovered() {
        t.tab_text_active
    } else {
        t.icon
    };
    paint(ui.painter(), rect.shrink(size * 0.2), icon, col);
    if tooltip.is_empty() { resp } else { resp.on_hover_text(tooltip) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Icon::*;
    use egui::epaint::{ColorMode, Shape};
    use egui::pos2;

    const ALL: &[Icon] = &[
        Monitor,
        Sun,
        Moon,
        Selection,
        TrackSelectFwd,
        TrackSelectBack,
        Ripple,
        Rolling,
        RateStretch,
        Remix,
        Razor,
        Slip,
        Slide,
        Pen,
        Rectangle,
        Ellipse,
        Hand,
        Zoom,
        Type,
        Play,
        Pause,
        StepBack,
        StepFwd,
        GoToIn,
        GoToOut,
        MarkIn,
        MarkOut,
        Marker,
        Insert,
        Overwrite,
        Lift,
        Extract,
        Camera,
        Loop,
        Wrench,
        Plus,
        Eye,
        EyeOff,
        Speaker,
        Mute,
        Lock,
        Unlock,
        SyncLock,
        Mic,
        Folder,
        Film,
        Sequence,
        Audio,
        Image,
        Search,
        ListView,
        IconView,
        Freeform,
        NewItem,
        Trash,
        Home,
        Workspaces,
        Hamburger,
        ChevronDown,
        ChevronRight,
        Magnet,
        Link,
        Keyframe,
        Stopwatch,
        Fx,
        Reset,
        Close,
        Fullscreen,
        Export,
        Import,
        Gear,
        Info,
        Captions,
        Adjust,
        Nest,
        Undo,
        Redo,
        Bell,
        Chat,
        Globe,
        Code,
        Sparkle,
        Grid,
        Square,
        Proxy,
        Offline,
        TrackMaskBack,
        TrackMaskBackFrame,
        TrackMaskFwdFrame,
        TrackMaskFwd,
        SortIcons,
        Automate,
        Star,
        ChevronLeft,
        ArrowUp,
        Drive,
        Network,
        Clock,
        Eyedropper,
    ];

    /// Paint `icon` at `ppp` device pixels per point into a rect of `size` points whose corner is
    /// off the pixel grid, and return what was drawn.
    fn drawn(icon: Icon, ppp: f32, size: f32) -> Vec<Shape> {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(ppp);
        let mut shapes = Vec::new();
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                paint(ui.painter(), Rect::from_min_size(pos2(10.3, 7.7), vec2(size, size)), icon, Color32::WHITE);
            });
            out.textures_delta.clear();
            shapes = out.shapes.into_iter().map(|c| c.shape).collect();
        }
        fn flat(s: Shape, out: &mut Vec<Shape>) {
            match s {
                Shape::Vec(v) => v.into_iter().for_each(|s| flat(s, out)),
                s => out.push(s),
            }
        }
        let mut out = Vec::new();
        shapes.into_iter().for_each(|s| flat(s, &mut out));
        out
    }

    fn on_grid(v: f32, ppp: f32, width_px: f32) -> bool {
        let want = if width_px.round() as i32 % 2 == 1 { 0.5 } else { 0.0 };
        ((v * ppp).fract().abs() - want).abs() < 1e-3 || ((v * ppp).fract().abs() - (1.0 - want)).abs() < 1e-3
    }

    /// Every straight line of every icon covers whole device pixels at 1x and 2x (no half-pixel
    /// blur): a whole number of pixels wide, its middle where that width needs it.
    #[test]
    fn straight_lines_land_on_whole_pixels() {
        for &ppp in &[1.0, 2.0] {
            for &size in &[16.0, 13.0, 9.6, 20.0] {
                for &icon in ALL {
                    for shape in drawn(icon, ppp, size) {
                        if let Shape::Path(p) = shape {
                            if p.stroke.width == 0.0 || matches!(p.stroke.color, ColorMode::Solid(c) if c == Color32::TRANSPARENT) {
                                continue;
                            }
                            let w = p.stroke.width * ppp;
                            assert!((w - w.round()).abs() < 1e-3 && w >= 1.0, "{icon:?} at {ppp}x size {size}: width {w} px");
                            let n = p.points.len();
                            let segs = if p.closed { n } else { n - 1 };
                            for i in 0..segs {
                                let (a, b) = (p.points[i], p.points[(i + 1) % n]);
                                if (a.y - b.y).abs() < 1e-4 {
                                    assert!(on_grid(a.y, ppp, w), "{icon:?} at {ppp}x size {size}: horizontal line at y {} px", a.y * ppp);
                                }
                                if (a.x - b.x).abs() < 1e-4 {
                                    assert!(on_grid(a.x, ppp, w), "{icon:?} at {ppp}x size {size}: vertical line at x {} px", a.x * ppp);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The panel menu mark (≡): three lines, the same two gaps, at every scale.
    #[test]
    fn menu_mark_lines_are_evenly_spaced() {
        for &ppp in &[1.0, 1.5, 2.0] {
            for &size in &[16.0, 13.0, 9.6, 12.0] {
                let ys: Vec<f32> = drawn(Icon::Hamburger, ppp, size)
                    .into_iter()
                    .filter_map(|s| if let Shape::Path(p) = s { Some(p.points[0].y * ppp) } else { None })
                    .collect();
                assert_eq!(ys.len(), 3);
                let (g1, g2) = (ys[1] - ys[0], ys[2] - ys[1]);
                assert!((g1 - g2).abs() < 1e-3 && g1 >= 2.0, "{ppp}x size {size}: gaps {g1} and {g2} px");
            }
        }
    }

    /// `ALL` lists the 99 icons, and the table has an SVG for each of them. An icon without one draws
    /// nothing, so this is the check that none is missing.
    #[test]
    fn every_icon_has_an_svg() {
        assert_eq!(ALL.len(), 99, "ALL lists the 99 icons");
        for &icon in ALL {
            assert!(icons_svg::svg_for(icon).is_some(), "{icon:?} has no svg");
        }
    }
}
