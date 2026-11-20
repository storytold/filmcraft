//! Import mode (header "Import", also Home): large browser cards for importing media and demo footage,
//! and the recent projects.

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::theme::Tokens;

/// Page margin, gap between cards and the card size range: cards share the row and wrap onto
/// more rows as the window narrows.
const MARGIN: f32 = 24.0;
const GAP: f32 = 16.0;
const CARD_MIN_W: f32 = 200.0;
const CARD_MAX_W: f32 = 320.0;
const CARD_H: f32 = 150.0;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let avail = (rect.width() - 2.0 * MARGIN).max(0.0);
    ui.painter().rect_filled(rect, t.radius, t.panel_bg);
    ui.painter().text(rect.min + vec2(24.0, 30.0), Align2::LEFT_CENTER, tl!("Import"), Tokens::semibold(22.0), t.text);
    let intro = ui.painter().layout(
        tl!("Add media to your project. Drop files or folders anywhere in the window, or choose a source below.").to_string(),
        Tokens::ui(13.0),
        t.text_dim,
        avail,
    );
    let intro_h = intro.size().y;
    ui.painter().galley(rect.min + vec2(MARGIN, 50.0), intro, t.text_dim);
    let ctx = ui.ctx().clone();
    let cards = [
        (tl!("Browse files…"), "file.import", tl!("Movies, audio and stills from disk")),
        (tl!("Demo footage"), "file.importDemoFootage", tl!("Six procedural 1080p clips with sound")),
        (tl!("Demo project"), "file.openDemoProject", tl!("A cut sequence with transitions, effects and music")),
        (tl!("Bars and Tone"), "file.newBarsAndTone", tl!("SMPTE HD bars with 1 kHz reference tone")),
    ];
    let (cols, cw) = card_grid(avail, cards.len());
    let top = 50.0 + intro_h + 24.0;
    for (i, (title, cmd, sub)) in cards.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let r = Rect::from_min_size(rect.min + vec2(MARGIN + col as f32 * (cw + GAP), top + row as f32 * (CARD_H + GAP)), vec2(cw, CARD_H));
        let resp = ui.interact(r, egui::Id::unique(("imp", *cmd)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        app.auto.add(&format!("import.{cmd}"), r, title);
        let a = paint_frame(ui, &t, &resp, [card_fill(&t, false), card_fill(&t, true), t.pressed], [t.separator, t.accent]);
        // the icon rises a little on hover
        crate::icons::paint(
            ui.painter(),
            Rect::from_min_size(r.min + vec2(16.0, 20.0 - 3.0 * a), vec2(40.0, 40.0)),
            [crate::icons::Icon::Folder, crate::icons::Icon::Film, crate::icons::Icon::Sequence, crate::icons::Icon::Grid][i],
            t.accent_hover,
        );
        let p = ui.painter().with_clip_rect(r.shrink(1.0));
        p.text(r.min + vec2(16.0, 84.0), Align2::LEFT_CENTER, *title, Tokens::semibold(14.0), t.text);
        // Wrap the description inside the card (long or translated text ran into the next card).
        let sub = p.layout(sub.to_string(), Tokens::ui(11.5), t.text_dim, (cw - 32.0).max(0.0));
        p.galley(r.min + vec2(16.0, 100.0), sub, t.text_dim);
        if resp.clicked() {
            let _ = crate::menus::invoke(app, &ctx, cmd, json!({}));
            app.ui.mode = crate::state::Mode::Edit;
        }
    }
    let rows = cards.len().div_ceil(cols);
    let y = recent(app, ui, rect, top + rows as f32 * (CARD_H + GAP) - GAP + 36.0);
    // Community and project links.
    // ArtCraft wordmark (first-party trademark, docs/brand/), then the section title.
    let logo = crate::brand::paint_wordmark(ui, rect.min + vec2(24.0, y), 15.0, app.ui.dark);
    let tx = logo.map_or(rect.min.x + 24.0, |r| r.max.x + 12.0);
    ui.painter().text(pos2(tx, rect.min.y + y), Align2::LEFT_CENTER, tl!("Community"), Tokens::semibold(15.0), t.text);
    let (left, right) = (rect.min.x + MARGIN, rect.min.x + MARGIN + avail);
    let (mut x, mut by) = (left, rect.min.y + y + 18.0);
    for (i, (id, label, url)) in crate::links::ALL.iter().take(4).enumerate() {
        let primary = i == 0;
        let label = if primary { tl!("Join us on Discord") } else { crate::i18n::t(label) };
        let g = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(13.0), t.text);
        let w = g.size().x + 44.0;
        // wrap the links onto another row rather than run past the window edge
        if x > left && x + w > right {
            (x, by) = (left, by + 34.0 + 10.0);
        }
        let r = Rect::from_min_size(pos2(x, by), vec2(w, 34.0));
        let resp = ui.interact(r, egui::Id::unique(("imp-link", *id)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(*url);
        app.auto.add(&format!("import.link.{}", id.trim_start_matches("help.")), r, label);
        let fg = if primary {
            paint_frame(ui, &t, &resp, [t.accent, t.accent_hover, t.accent], [t.accent, t.accent_hover]);
            Color32::WHITE
        } else {
            paint_frame(ui, &t, &resp, [card_fill(&t, false), card_fill(&t, true), t.pressed], [t.separator, t.accent]);
            t.text
        };
        let icon = [crate::icons::Icon::Chat, crate::icons::Icon::Globe, crate::icons::Icon::Globe, crate::icons::Icon::Code][i];
        crate::icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 20.0, r.center().y), vec2(16.0, 16.0)), icon, fg);
        ui.painter().galley(pos2(r.min.x + 34.0, r.center().y - g.size().y / 2.0), g, fg);
        if resp.clicked() {
            crate::links::open(app, &ctx, url);
        }
        x = r.max.x + 12.0;
    }
}

/// How many source cards fit on a row of `avail` points (1 to `n`), and their shared width:
/// as many as fit at `CARD_MIN_W`, widened to fill the row up to `CARD_MAX_W`.
fn card_grid(avail: f32, n: usize) -> (usize, f32) {
    let n = n.max(1);
    let fit = ((avail + GAP) / (CARD_MIN_W + GAP)).floor();
    let cols = if fit.is_finite() && fit >= 1.0 { (fit as usize).min(n) } else { 1 };
    let w = (avail - GAP * (cols - 1) as f32) / cols as f32;
    (cols, if w.is_finite() { w.clamp(0.0, CARD_MAX_W) } else { 0.0 })
}

/// Paints a button's rounded frame in `resp.rect`: `fill` is [resting, hovered, pressed] and
/// `edge` is [resting, hovered]; hover and keyboard focus ease between them, focus draws the focus
/// ring. Returns the hover amount (0 to 1) for content that moves with it.
fn paint_frame(ui: &egui::Ui, t: &Tokens, resp: &egui::Response, fill: [Color32; 3], edge: [Color32; 2]) -> f32 {
    let a = ui.ctx().animate_bool_with_time(resp.id.with("hover"), resp.hovered() || resp.has_focus(), 0.12);
    let bg = if resp.is_pointer_button_down_on() { fill[2] } else { fill[0].lerp_to_gamma(fill[1], a) };
    let line = if resp.has_focus() { t.focus } else { edge[0].lerp_to_gamma(edge[1], a) };
    ui.painter().rect(resp.rect, 8.0, bg, egui::Stroke::new(1.0, line), egui::StrokeKind::Inside);
    a
}

/// A source card's fill: a surface that differs from `panel_bg` in every theme, so the cards read
/// as buttons before they are hovered (`tl_header_bg` equals `panel_bg` and hid them).
fn card_fill(t: &Tokens, hovered: bool) -> Color32 {
    if hovered { t.hover } else { t.control_surface }
}

/// Recent projects listed on Home, as in File ▸ Open Recent.
const RECENT_ROWS: usize = 6;
const RECENT_ROW_H: f32 = 28.0;

/// Home's Recent list at `y`: the recent projects (newest first, file name with its folder beside
/// it), a click opens one. Returns where the next section starts.
fn recent(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, y: f32) -> f32 {
    let paths: Vec<String> = app.session.prefs.general.recent_projects.iter().take(RECENT_ROWS).cloned().collect();
    if paths.is_empty() {
        return y;
    }
    let t = app.tokens;
    ui.painter().text(rect.min + vec2(24.0, y), Align2::LEFT_CENTER, tl!("Recent"), Tokens::semibold(15.0), t.text);
    let mut open = None;
    for (i, path) in paths.iter().enumerate() {
        let row =
            Rect::from_min_size(rect.min + vec2(16.0, y + 16.0 + i as f32 * RECENT_ROW_H), vec2(560.0_f32.min(rect.width() - 32.0).max(0.0), RECENT_ROW_H));
        let name = crate::menus::recent_name(path);
        let resp = ui.interact(row, egui::Id::unique(("imp-recent", i)), Sense::click()).on_hover_text(path.as_str());
        app.auto.add(&format!("import.recent.{i}"), row, &name);
        if resp.hovered() {
            ui.painter().rect_filled(row, 4.0, t.hover);
        }
        let p = ui.painter().with_clip_rect(row);
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
        // unsaved changes: Save / Don't Save / Cancel first, asked over the project they were made in;
        // Home switches to Edit once the chosen project opens, and stays after Cancel
        if crate::panels::clip_dialogs::ask_before_open(app, &path).is_some() {
            if let Some(info) = app.ui.clip_dialog.as_mut().and_then(|d| d.info.as_object_mut()) {
                info.insert("editAfterOpen".into(), json!(true));
            }
        } else if crate::menus::invoke(app, &ctx, "file.open", json!({"path": path})).is_ok() {
            app.ui.mode = crate::state::Mode::Edit;
        }
    }
    y + 16.0 + paths.len() as f32 * RECENT_ROW_H + 36.0
}

#[cfg(test)]
mod tests {
    use super::{CARD_MAX_W, CARD_MIN_W, card_fill, card_grid};
    use crate::theme::{ThemeKind, Tokens};

    #[test]
    fn source_cards_stand_out_from_the_panel_in_every_theme() {
        for kind in [ThemeKind::Dark, ThemeKind::Medium, ThemeKind::Light] {
            let t = Tokens::for_kind(kind);
            assert_ne!(card_fill(&t, false), t.panel_bg, "{kind:?}: resting card is invisible");
            assert_ne!(card_fill(&t, true), card_fill(&t, false), "{kind:?}: hover shows no change");
        }
    }

    #[test]
    fn source_cards_follow_the_window_width() {
        // wide: one row of four, no wider than the cap
        assert_eq!(card_grid(2000.0, 4), (4, CARD_MAX_W));
        // medium: two per row, filling the row
        let (cols, w) = card_grid(600.0, 4);
        assert_eq!(cols, 2);
        assert!(w >= CARD_MIN_W && (2.0 * w + 16.0 - 600.0).abs() < 0.01);
        // narrow, zero, negative or NaN width: one column, never a negative size
        for avail in [120.0, 0.0, -50.0, f32::NAN] {
            let (cols, w) = card_grid(avail, 4);
            assert_eq!(cols, 1, "{avail}");
            assert!(w >= 0.0, "{avail}: {w}");
        }
    }
}
