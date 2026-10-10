//! Import mode (header "Import"): large browser cards for importing media and demo footage.

use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;
use crate::theme::Tokens;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, t.radius, t.panel_bg);
    ui.painter().text(rect.min + vec2(24.0, 30.0), Align2::LEFT_CENTER, tl!("Import"), Tokens::semibold(22.0), t.text);
    ui.painter().text(
        rect.min + vec2(24.0, 58.0),
        Align2::LEFT_CENTER,
        tl!("Add media to your project. Drop files anywhere in the window, or choose a source below."),
        Tokens::ui(13.0),
        t.text_dim,
    );
    let ctx = ui.ctx().clone();
    let cards = [
        (tl!("Browse files…"), "file.import", tl!("Movies, audio and stills from disk")),
        (tl!("Demo footage"), "file.importDemoFootage", tl!("Six procedural 1080p clips with sound")),
        (tl!("Demo project"), "file.openDemoProject", tl!("A cut sequence with transitions, effects and music")),
        (tl!("Bars and Tone"), "file.newBarsAndTone", tl!("SMPTE HD bars with 1 kHz reference tone")),
    ];
    let cw = 260.0;
    for (i, (title, cmd, sub)) in cards.iter().enumerate() {
        let r = Rect::from_min_size(rect.min + vec2(24.0 + i as f32 * (cw + 16.0), 90.0), vec2(cw, 150.0));
        let resp = ui.interact(r, egui::Id::new(("imp", *cmd)), Sense::click());
        app.auto.add(&format!("import.{cmd}"), r, title);
        ui.painter().rect_filled(r, 8.0, if resp.hovered() { t.hover } else { t.tl_header_bg });
        ui.painter().text(r.min + vec2(16.0, 110.0), Align2::LEFT_CENTER, *title, Tokens::semibold(14.0), t.text);
        ui.painter().text(r.min + vec2(16.0, 132.0), Align2::LEFT_CENTER, *sub, Tokens::ui(11.5), t.text_dim);
        crate::icons::paint(
            ui.painter(),
            Rect::from_min_size(r.min + vec2(16.0, 18.0), vec2(56.0, 56.0)),
            [crate::icons::Icon::Folder, crate::icons::Icon::Film, crate::icons::Icon::Sequence, crate::icons::Icon::Grid][i],
            Color32::from_rgb(140, 150, 255),
        );
        if resp.clicked() {
            let _ = crate::menus::invoke(app, &ctx, cmd, json!({}));
            app.ui.mode = crate::state::Mode::Edit;
        }
    }
    // Community and project links.
    let y = 90.0 + 150.0 + 36.0;
    // ArtCraft wordmark (first-party trademark, docs/brand/), then the section title.
    let logo = crate::brand::paint_wordmark(ui, rect.min + vec2(24.0, y), 15.0, app.ui.dark);
    let tx = logo.map_or(rect.min.x + 24.0, |r| r.max.x + 12.0);
    ui.painter().text(pos2(tx, rect.min.y + y), Align2::LEFT_CENTER, tl!("Community"), Tokens::semibold(15.0), t.text);
    let mut x = rect.min.x + 24.0;
    for (i, (id, label, url)) in crate::links::ALL.iter().take(4).enumerate() {
        let primary = i == 0;
        let label = if primary { tl!("Join us on Discord") } else { crate::i18n::t(label) };
        let g = ui.painter().layout_no_wrap(label.to_string(), Tokens::ui(13.0), t.text);
        let r = Rect::from_min_size(pos2(x, rect.min.y + y + 18.0), vec2(g.size().x + 44.0, 34.0));
        let resp = ui.interact(r, egui::Id::new(("imp-link", *id)), Sense::click()).on_hover_text(*url);
        app.auto.add(&format!("import.link.{}", id.trim_start_matches("help.")), r, label);
        let (bg, fg) = if primary {
            (if resp.hovered() { t.accent_hover } else { t.accent }, Color32::WHITE)
        } else {
            (if resp.hovered() { t.hover } else { t.tl_header_bg }, t.text)
        };
        ui.painter().rect_filled(r, 8.0, bg);
        let icon = [crate::icons::Icon::Chat, crate::icons::Icon::Globe, crate::icons::Icon::Globe, crate::icons::Icon::Code][i];
        crate::icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 20.0, r.center().y), vec2(16.0, 16.0)), icon, fg);
        ui.painter().galley(pos2(r.min.x + 34.0, r.center().y - g.size().y / 2.0), g, fg);
        if resp.clicked() {
            crate::links::open(app, &ctx, url);
        }
        x = r.max.x + 12.0;
    }
}
