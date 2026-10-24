//! Home (the header's Home button, and the window at startup with Settings ▸ General ▸ At Startup ▸
//! Show Home): New Project and Open, the recent projects, new sequences at common frame sizes and
//! the Community links. Opening or creating a project leaves Home for Edit.

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::state::Mode;
use crate::theme::Tokens;

/// Recent projects listed on Home, as in File ▸ Open Recent.
const RECENT_ROWS: usize = 6;
const RECENT_ROW_H: f32 = 28.0;
/// The widest the Home column gets; wider windows centre it.
const COLUMN_W: f32 = 820.0;
const CARD: egui::Vec2 = vec2(124.0, 128.0);
const CARD_GAP: f32 = 14.0;

/// The frame sizes Start a New Sequence offers: (kind, tag, width, height). Horizontal and vertical
/// HD and UHD, vertical 4:5 (1080 × 1350) and square, the usual sizes for video sites and social posts.
const SIZES: [(&str, &str, u32, u32); 6] = [
    ("Horizontal", "1080p", 1920, 1080),
    ("Horizontal", "4K", 3840, 2160),
    ("Vertical", "1080p", 1080, 1920),
    ("Vertical", "4K", 2160, 3840),
    ("Vertical", "4:5", 1080, 1350),
    ("Square", "", 1080, 1080),
];

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, t.radius, t.panel_bg);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt("home"));
    child.set_clip_rect(rect);
    egui::ScrollArea::vertical().id_salt("home-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        let avail = ui.available_width();
        let w = (avail - 48.0).clamp(1.0, COLUMN_W);
        // Only projects still on disk, so one beyond the first rows shows when newer ones are gone
        // (At Startup ▸ Show Home chooses Home when any recent project exists).
        let recent: Vec<String> =
            app.session.prefs.general.recent_projects.iter().filter(|p| std::path::Path::new(p).exists()).take(RECENT_ROWS).cloned().collect();
        let columns = (((w + CARD_GAP) / (CARD.x + CARD_GAP)).floor() as usize).max(1);
        let card_rows = SIZES.len().div_ceil(columns);
        // Section tops, from the top of the content.
        let recent_y = 184.0;
        let sizes_y = if recent.is_empty() { recent_y } else { recent_y + 16.0 + recent.len() as f32 * RECENT_ROW_H + 36.0 };
        let community_y = sizes_y + 16.0 + card_rows as f32 * (CARD.y + CARD_GAP) + 28.0;
        let (content, _) = ui.allocate_exact_size(vec2(avail, community_y + 80.0), Sense::hover());
        let origin = pos2(content.min.x + ((avail - w) / 2.0).max(0.0), content.min.y);
        welcome(app, ui, origin);
        if !recent.is_empty() {
            recent_projects(app, ui, origin + vec2(0.0, recent_y), w, &recent);
        }
        sizes(app, ui, origin + vec2(0.0, sizes_y), columns);
        crate::panels::import_mode::community(app, ui, origin + vec2(0.0, community_y), "home");
    });
}

/// Registers an automation id for the part of `r` that is on screen (Home scrolls), so `ui.click`
/// never aims at a point outside the window.
pub(crate) fn register(app: &mut FilmcraftApp, ui: &egui::Ui, id: &str, r: Rect, label: &str) {
    let visible = r.intersect(ui.clip_rect());
    if visible.is_positive() {
        app.auto.add(id, visible, label);
    }
}

/// The title, a line about FilmCraft, and the New Project and Open buttons.
fn welcome(app: &mut FilmcraftApp, ui: &mut egui::Ui, origin: egui::Pos2) {
    let t = app.tokens;
    ui.painter().text(origin + vec2(0.0, 60.0), Align2::LEFT_CENTER, tl!("Welcome to FilmCraft"), Tokens::semibold(26.0), t.text);
    ui.painter().text(origin + vec2(0.0, 92.0), Align2::LEFT_CENTER, tl!("Video editing — fast, open, scriptable."), Tokens::ui(14.0), t.text_dim);
    let ctx = ui.ctx().clone();
    let mut x = origin.x;
    for (id, label, cmd, primary) in [("home.new", tl!("New Project"), "file.newProject", true), ("home.open", tl!("Open"), "file.open", false)] {
        let g = ui.painter().layout_no_wrap(label.to_string(), Tokens::semibold(13.0), t.text);
        let r = Rect::from_min_size(pos2(x, origin.y + 120.0), vec2(g.size().x + 32.0, 32.0));
        let resp = ui.interact(r, egui::Id::new(id), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        register(app, ui, id, r, label);
        let (bg, fg) = if primary {
            (if resp.hovered() { t.accent_hover } else { t.accent }, Color32::WHITE)
        } else {
            (if resp.hovered() { t.hover } else { t.tl_header_bg }, t.text)
        };
        ui.painter().rect_filled(r, 6.0, bg);
        if !primary {
            // outlined, since the plain button's fill can match the page
            ui.painter().rect_stroke(r, 6.0, Stroke::new(1.0, t.text_dim), StrokeKind::Inside);
        }
        ui.painter().galley(r.center() - g.size() / 2.0, g, fg);
        if resp.clicked() {
            // Open shows the file dialog; the editor replaces Home once the project opens.
            if crate::menus::invoke(app, &ctx, cmd, json!({})).is_ok() && cmd == "file.newProject" {
                app.ui.mode = Mode::Edit;
            }
        }
        x = r.max.x + 10.0;
    }
}

/// The recent projects, newest first: each row its file name with the folder beside it, the whole
/// row one target with a hover background; a click opens the project.
fn recent_projects(app: &mut FilmcraftApp, ui: &mut egui::Ui, origin: egui::Pos2, width: f32, paths: &[String]) {
    let t = app.tokens;
    ui.painter().text(origin, Align2::LEFT_CENTER, tl!("Recent Projects"), Tokens::semibold(15.0), t.text);
    let mut open = None;
    for (i, path) in paths.iter().enumerate() {
        let row = Rect::from_min_size(origin + vec2(-8.0, 16.0 + i as f32 * RECENT_ROW_H), vec2(width + 16.0, RECENT_ROW_H));
        let name = crate::menus::recent_name(path);
        let resp =
            ui.interact(row, egui::Id::new(("home-recent", i)), Sense::click()).on_hover_text(path.as_str()).on_hover_cursor(egui::CursorIcon::PointingHand);
        register(app, ui, &format!("home.recent.{i}"), row, &name);
        if resp.hovered() {
            ui.painter().rect_filled(row, 4.0, t.hover);
        }
        let p = ui.painter().with_clip_rect(row.intersect(ui.clip_rect()));
        crate::icons::paint(&p, Rect::from_center_size(pos2(row.min.x + 16.0, row.center().y), vec2(14.0, 14.0)), crate::icons::Icon::Sequence, t.text_dim);
        let name_r = p.text(pos2(row.min.x + 32.0, row.center().y), Align2::LEFT_CENTER, &name, Tokens::ui(13.0), t.text);
        if let Some(folder) = std::path::Path::new(path).parent() {
            p.text(pos2(name_r.max.x + 12.0, row.center().y), Align2::LEFT_CENTER, folder.to_string_lossy(), Tokens::ui(11.5), t.text_dim);
        }
        if resp.clicked() {
            open = Some(path.clone());
        }
    }
    if let Some(path) = open {
        let ctx = ui.ctx().clone();
        if crate::menus::invoke(app, &ctx, "file.open", json!({"path": path})).is_ok() {
            app.ui.mode = Mode::Edit;
        }
    }
}

/// Start a New Sequence: a card per frame size (`SIZES`), each a shape of that aspect over its
/// name and size. A click adds a sequence of that size to the project and opens it in Edit.
fn sizes(app: &mut FilmcraftApp, ui: &mut egui::Ui, origin: egui::Pos2, columns: usize) {
    let t = app.tokens;
    ui.painter().text(origin, Align2::LEFT_CENTER, tl!("Start a New Sequence"), Tokens::semibold(15.0), t.text);
    let ctx = ui.ctx().clone();
    for (i, (kind, tag, w, h)) in SIZES.iter().enumerate() {
        let (col, row) = (i % columns, i / columns);
        let r = Rect::from_min_size(origin + vec2(col as f32 * (CARD.x + CARD_GAP) - 8.0, 16.0 + row as f32 * (CARD.y + CARD_GAP)), CARD);
        let name = format!("{} {tag}", crate::i18n::t(kind)).trim_end().to_string();
        let id = format!("home.sequence.{w}x{h}");
        let resp = ui.interact(r, egui::Id::new(&id), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        register(app, ui, &id, r, &name);
        if resp.hovered() {
            ui.painter().rect_filled(r, 8.0, t.hover);
        }
        // The frame's shape, fitted into a 64 pt square above the label.
        let side = 64.0;
        let (fw, fh) = (*w as f32, *h as f32);
        let scale = side / fw.max(fh);
        let frame = Rect::from_center_size(pos2(r.center().x, r.min.y + 14.0 + side / 2.0), vec2(fw * scale, fh * scale));
        ui.painter().rect_filled(frame, 3.0, t.tl_header_bg);
        ui.painter().rect_stroke(frame, 3.0, Stroke::new(1.5, if resp.hovered() { t.accent } else { t.text_dim }), StrokeKind::Inside);
        ui.painter().text(pos2(r.center().x, r.min.y + 98.0), Align2::CENTER_CENTER, &name, Tokens::semibold(12.5), t.text);
        ui.painter().text(pos2(r.center().x, r.min.y + 115.0), Align2::CENTER_CENTER, format!("{w} × {h}"), Tokens::ui(11.5), t.text_dim);
        if resp.clicked() {
            let made = crate::menus::invoke(app, &ctx, "file.newSequence", json!({"name": name, "width": w, "height": h}));
            if made.is_ok() {
                app.ui.mode = Mode::Edit;
            }
        }
    }
}
