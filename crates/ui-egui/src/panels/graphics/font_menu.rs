//! The font-family drop-down contents shared by every font picker (Properties, Effect Controls,
//! graphics template controls, caption style): a Favourites-only filter, the recently used
//! families, then every family with a star toggle, starred ones first (#3577).

use filmcraft_engine::autosave::FontPrefs;
use serde_json::{Value, json};

/// What the user did in the menu this frame.
#[derive(Default)]
pub(crate) struct FontMenuOut {
    /// The family clicked, if any.
    pub picked: Option<String>,
    /// Commands to run: star toggles, and the recent-fonts record for a pick.
    pub actions: Vec<(String, Value)>,
}

fn star(ui: &mut egui::Ui, on: bool) -> egui::Response {
    let (glyph, tip) = if on { ("★", tl!("Remove from Favourites")) } else { ("☆", tl!("Add to Favourites")) };
    ui.add(egui::Button::new(glyph).frame(false).min_size(egui::vec2(18.0, 0.0))).on_hover_text(tip)
}

/// Draw the family list into the open drop-down. `auto(family, rect)` registers the row for
/// automation: `auto("<family>")`, `"<family>.favourite"`, `"recent.<family>"`, `"favouritesOnly"`. Names starting with '.' are macOS-private faces and are skipped.
pub(crate) fn font_menu(
    ui: &mut egui::Ui,
    families: &[(String, Vec<String>)],
    current: &str,
    prefs: &FontPrefs,
    auto: &mut dyn FnMut(&str, egui::Rect),
) -> FontMenuOut {
    let mut out = FontMenuOut::default();
    let only_fav_id = ui.make_persistent_id("font-menu-favourites-only");
    let mut only_fav = ui.data(|d| d.get_temp::<bool>(only_fav_id)).unwrap_or(false);
    let filter = ui.checkbox(&mut only_fav, tl!("Favourites only"));
    auto("favouritesOnly", filter.rect);
    if filter.changed() {
        ui.data_mut(|d| d.insert_temp(only_fav_id, only_fav));
    }
    let exists = |f: &str| families.iter().any(|(n, _)| n.eq_ignore_ascii_case(f));
    let mut row = |ui: &mut egui::Ui, out: &mut FontMenuOut, f: &str, recent: bool| {
        ui.horizontal(|ui| {
            let fav = prefs.is_favourite(f);
            let s = star(ui, fav);
            // Recent rows repeat families of the main list, so they get their own ids
            let id = if recent { format!("recent.{f}") } else { f.to_string() };
            auto(&format!("{id}.favourite"), s.rect);
            if s.clicked() {
                out.actions.push(("graphics.fonts.favourite".into(), json!({"family": f, "on": !fav})));
            }
            let r = ui.selectable_label(f.eq_ignore_ascii_case(current), f);
            auto(&id, r.rect);
            if r.clicked() {
                out.picked = Some(f.to_string());
                out.actions.push(("graphics.fonts.recent".into(), json!({"family": f})));
            }
        });
    };
    if !only_fav {
        let recent: Vec<&String> = prefs.recent.iter().filter(|f| exists(f)).collect();
        if !recent.is_empty() {
            ui.label(egui::RichText::new(tl!("Recent")).weak());
            for f in recent {
                row(ui, &mut out, f, true);
            }
            ui.separator();
        }
    }
    let listed = families.iter().map(|(f, _)| f).filter(|f| !f.starts_with('.'));
    let (starred, others): (Vec<&String>, Vec<&String>) = listed.partition(|f| prefs.is_favourite(f));
    for f in starred {
        row(ui, &mut out, f, false);
    }
    if !only_fav {
        for f in others {
            row(ui, &mut out, f, false);
        }
    }
    out
}
