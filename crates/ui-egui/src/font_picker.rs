//! The menu behind every font picker: a search box, the families marked as favourites, the ones
//! used recently and the whole installed list. Families are stored and sent as their English name
//! (what projects hold); the rows show the Chinese name a font carries (`思源黑体`), with the
//! English family as the row's tooltip so two look-alike names stay apart.

use egui::{Key, Ui};
use serde_json::json;

use crate::FilmcraftApp;

/// What a row in the menu asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontAction {
    /// Use this family.
    Pick(String),
    /// Mark this family as a favourite, or clear the mark.
    Favorite(String),
}

/// The label a picker shows for a family: its Chinese name when the font has one, else the family.
pub fn label(family: &str) -> String {
    filmcraft_text::chinese_name(family).unwrap_or_else(|| family.to_string())
}

/// The style names a family offers (`Regular`, `Bold`, …), empty for an unknown family.
pub fn styles(family: &str) -> Vec<String> {
    filmcraft_text::families().into_iter().find(|(f, _)| f.eq_ignore_ascii_case(family)).map(|(_, v)| v).unwrap_or_default()
}

/// A style name for display: the common ones say 常规 / 粗体 / 斜体 instead of Regular / Bold.
pub fn style_label(style: &str) -> String {
    // The context keeps these apart from the same English words elsewhere in the interface
    // ("Light" the panel, "Black" the colour).
    match style.trim() {
        "Regular" | "Bold" | "Italic" | "Bold Italic" | "Light" | "Medium" | "SemiBold" | "Black" | "Thin" | "Heavy" => {
            crate::i18n::tr_ctx("font style", style.trim()).to_string()
        }
        other => other.to_string(),
    }
}

/// One family row: the favourite star (when the caller can write preferences), the shown name
/// (clickable) and the English family beside it. `prefix` is the automation id base the callers
/// already use (`….option.{family}`), so tests and scripts keep finding the same rows.
fn row(
    ui: &mut Ui,
    e: &filmcraft_text::FamilyEntry,
    current: &str,
    prefix: &str,
    starred: Option<bool>,
    register: &mut dyn FnMut(&str, egui::Rect, &str),
) -> Option<FontAction> {
    let mut action = None;
    ui.horizontal(|ui| {
        if let Some(starred) = starred {
            let hint = if starred { tl!("Remove from favorites") } else { tl!("Add to favorites") };
            let star = if starred { "★" } else { "☆" };
            let r = ui.add(egui::Button::new(star).frame(false).small()).on_hover_text(hint);
            let visible = r.rect.intersect(ui.clip_rect());
            if visible.is_positive() {
                register(&format!("{prefix}.favorite.{}", e.family), visible, &e.family);
            }
            if r.clicked() {
                action = Some(FontAction::Favorite(e.family.clone()));
            }
        }
        // The Chinese name is what people look for; the English family is the tooltip, and the
        // row text stays the family itself for the families that have no Chinese name.
        let selected = e.family.eq_ignore_ascii_case(current);
        let mut r = ui.selectable_label(selected, egui::RichText::new(e.label()).small());
        if e.zh.is_some() {
            r = r.on_hover_text(&e.family);
        }
        // An edge row may be only partly visible; automation must click its visible portion.
        let visible = r.rect.intersect(ui.clip_rect());
        if visible.is_positive() {
            register(&format!("{prefix}.{}", e.family), visible, &e.family);
        }
        if r.clicked() {
            action = Some(FontAction::Pick(e.family.clone()));
        }
    });
    action
}

/// The menu's contents. `stars` is `None` where the caller cannot write preferences.
/// What the menu knows about the user's preferences; `stars` is `None` where the caller cannot
/// write them.
struct Scope<'a> {
    favorites: &'a [String],
    recent: &'a [String],
    stars: Option<bool>,
    hide_private: bool,
}

fn body(ui: &mut Ui, query: &str, current: &str, prefix: &str, scope: &Scope<'_>, register: &mut dyn FnMut(&str, egui::Rect, &str)) -> Vec<FontAction> {
    let Scope { favorites, recent, stars, hide_private } = *scope;
    let mut out = Vec::new();
    let q = query.trim().to_lowercase();
    let entries: Vec<filmcraft_text::FamilyEntry> =
        filmcraft_text::picker_entries().into_iter().filter(|e| !(hide_private && filmcraft_text::is_private(&e.family))).filter(|e| e.matches(&q)).collect();
    let is_favorite = |f: &str| favorites.iter().any(|x| x.eq_ignore_ascii_case(f));
    let section = |ui: &mut Ui, title: &str| ui.label(egui::RichText::new(title).small().strong());

    // rows in the favourite and recent sections show no star: they are already favourites
    if !favorites.is_empty() {
        let favs: Vec<&filmcraft_text::FamilyEntry> = entries.iter().filter(|e| is_favorite(&e.family)).collect();
        if !favs.is_empty() {
            section(ui, tl!("Favorites"));
            for e in favs {
                let a = row(ui, e, current, prefix, Some(true), register);
                let picked = matches!(a, Some(FontAction::Pick(_)));
                out.extend(a);
                if picked {
                    ui.close();
                }
            }
            ui.separator();
        }
    }

    let recents: Vec<&filmcraft_text::FamilyEntry> =
        recent.iter().filter_map(|r| entries.iter().find(|e| e.family.eq_ignore_ascii_case(r))).filter(|e| !is_favorite(&e.family)).collect();
    if !recents.is_empty() {
        section(ui, tl!("Recently used"));
        for e in recents {
            let a = row(ui, e, current, prefix, stars, register);
            let picked = matches!(a, Some(FontAction::Pick(_)));
            out.extend(a);
            if picked {
                ui.close();
            }
        }
        ui.separator();
    }

    if entries.is_empty() {
        ui.label(egui::RichText::new(tl!("No font matches the search")).weak());
        return out;
    }
    section(ui, tl!("All fonts"));
    for e in &entries {
        let starred = stars.map(|_| is_favorite(&e.family));
        let a = row(ui, e, current, prefix, starred, register);
        let picked = matches!(a, Some(FontAction::Pick(_)));
        out.extend(a);
        if picked {
            ui.close();
        }
    }
    out
}

/// The menu body for a panel that can write preferences: favourites can be starred, and the rows
/// are registered for automation under `prefix`.
pub fn menu(app: &mut FilmcraftApp, ui: &mut Ui, query: &str, current: &str, prefix: &str, hide_private: bool) -> Vec<FontAction> {
    let favorites = app.session.prefs.fonts.favorites.clone();
    let recent = app.session.prefs.fonts.recent.clone();
    let mut register = |id: &str, rect: egui::Rect, label: &str| app.auto.add(id, rect, label);
    let scope = Scope { favorites: &favorites, recent: &recent, stars: Some(true), hide_private };
    body(ui, query, current, prefix, &scope, &mut register)
}

/// The menu body where the preferences can only be read (a dialog drawn from `&FilmcraftApp`):
/// the same search, favourites and recents, without the star buttons.
pub fn menu_readonly(prefs: &filmcraft_engine::autosave::FontPrefs, ui: &mut Ui, query: &str, current: &str, prefix: &str) -> Vec<FontAction> {
    let mut register = |_: &str, _: egui::Rect, _: &str| {};
    let scope = Scope { favorites: &prefs.favorites, recent: &prefs.recent, stars: None, hide_private: false };
    body(ui, query, current, prefix, &scope, &mut register)
}

/// The menu's search field: call it from the header closure of
/// [`craft_ui::ComboBox::show_ui_with_header`]. The text lives in the widget's own state.
pub fn search(ui: &mut Ui, id: egui::Id) -> String {
    let mut query: String = ui.data(|d| d.get_temp(id).unwrap_or_default());
    let resp = ui.add(egui::TextEdit::singleline(&mut query).hint_text(tl!("Search fonts")).desired_width(f32::INFINITY).id(id.with("field")));
    // Typing goes straight into the field while the menu is open; Escape clears it.
    if !resp.has_focus() && ui.input(|i| i.pointer.any_released() || i.key_pressed(Key::Escape) || i.focused) {
        resp.request_focus();
    }
    if ui.input(|i| i.key_pressed(Key::Escape)) {
        query.clear();
    }
    ui.data_mut(|d| d.insert_temp(id, query.clone()));
    query
}

/// Run the menu's actions: favourites and recents are written to the preferences, and the family
/// to switch to is returned.
pub fn apply(app: &mut FilmcraftApp, actions: Vec<FontAction>) -> Option<String> {
    let mut picked = None;
    for a in actions {
        match a {
            FontAction::Favorite(family) => {
                let mut fonts = app.session.prefs.fonts.clone();
                fonts.toggle_favorite(&family);
                let value = json!(fonts.favorites);
                let _ = app.session.execute("prefs.set", json!({"key": "fonts.favorites", "value": value}));
            }
            FontAction::Pick(family) => {
                let mut fonts = app.session.prefs.fonts.clone();
                fonts.used(&family);
                let value = json!(fonts.recent);
                let _ = app.session.execute("prefs.set", json!({"key": "fonts.recent", "value": value}));
                picked = Some(family);
            }
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picker row shows the Chinese name, and the English family stays the value.
    #[test]
    fn labels_are_chinese_when_the_font_has_a_chinese_name() {
        let entries = filmcraft_text::picker_entries();
        if let Some(e) = entries.iter().find(|e| e.zh.is_some()) {
            assert_eq!(label(&e.family), e.zh.clone().unwrap_or_default());
        }
        assert_eq!(label("NoSuchFamilyOfOurs"), "NoSuchFamilyOfOurs");
    }

    #[test]
    fn common_styles_are_named() {
        // The English interface shows the English name; a translation supplies 常规 in Chinese.
        assert_eq!(style_label("Regular"), "Regular");
        assert_eq!(style_label("Something Custom"), "Something Custom");
    }
}
