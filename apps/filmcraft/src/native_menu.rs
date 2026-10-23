//! Native macOS menu bar (muda), generated from the command registry — like Premiere, menus live in
//! the system menu bar. Items send command ids to the app's command inbox.

use std::sync::mpsc::{Receiver, channel};

use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::menus::{MENUS, MenuItem as Item, NATIVE_APP_MENU, menu_items};
use muda::accelerator::Accelerator;
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};

fn accel(s: &str) -> Option<Accelerator> {
    // Only shortcuts with a modifier become native key equivalents (single keys stay in-app so
    // typing in fields keeps working).
    if !(s.contains("Cmd+") || s.contains("Ctrl+") || s.contains("Alt+")) {
        return None;
    }
    let mapped =
        s.replace("Cmd+", "CmdOrCtrl+").replace(";", "Semicolon").replace("'", "Quote").replace("/", "Slash").replace("=", "Equal").replace("\\", "Backslash");
    mapped.parse().ok()
}

/// The app menu's Quit item: the `app.quit` command (which asks to save unsaved changes) on its
/// shortcut, Cmd+Q unless remapped in the Keyboard Shortcuts editor. The key equivalent is read
/// once, when the menu bar is built: `app.quit` has no menu path, so it is not in the menu tree the
/// updater receives, and a remap shows on the Quit item after the next launch.
fn quit_item(app: &FilmcraftApp) -> (&'static str, Option<Accelerator>) {
    let keys = app.session.shortcuts.primary("app.quit").unwrap_or_else(|| "Cmd+Q".into());
    ("app.quit", accel(&keys))
}

/// Updates native item labels, key equivalents and enablement when the menu tree changes.
pub type MenuUpdater = Box<dyn FnMut(&[Item])>;

pub fn install(app: &FilmcraftApp, ctx: egui::Context) -> (Receiver<String>, MenuUpdater) {
    let items = menu_items(app);
    // each native item with the menu entry it currently shows, so updates touch only what changed
    let mut native: Vec<(Item, MenuItem)> = Vec::new();
    let mut titles: Vec<(String, Submenu)> = Vec::new();
    let bar = Menu::new();
    let app_menu = Submenu::new("FilmCraft", true);
    let [settings_label, hide_label, hide_others_label, quit_label] = NATIVE_APP_MENU;
    // FilmCraft ▸ Settings ▸ <category> (Premiere's app-menu layout; General is Cmd+,)
    let settings = Submenu::new(app.ui.language.tr(settings_label), true);
    titles.push((settings_label.into(), settings.clone()));
    for it in items.iter().filter(|i| i.id.starts_with("app.settings.")) {
        let label = it.label.trim_end_matches('…').to_string();
        let mi = MenuItem::with_id(it.id.clone(), label, it.enabled, it.shortcut.as_deref().and_then(accel));
        native.push((it.clone(), mi.clone()));
        let _ = settings.append(&mi);
    }
    // About and Discord repeat Help menu commands: same translated labels, updated with them
    let label = |id: &str, english: &str| items.iter().find(|i| i.id == id).map_or_else(|| english.to_string(), |i| i.label.clone());
    let about = MenuItem::with_id("app.about", label("app.about", "About FilmCraft"), true, None);
    let discord = MenuItem::with_id("help.discord", label("help.discord", "Join the ArtCraft Discord…"), true, None);
    for (id, mi) in [("app.about", &about), ("help.discord", &discord)] {
        if let Some(it) = items.iter().find(|i| i.id == id) {
            // recorded as built (enabled, no shortcut), so the first update applies the real state
            native.push((Item { enabled: true, shortcut: None, ..it.clone() }, mi.clone()));
        }
    }
    // the predefined items keep their native actions; our text lets them follow the language
    let predefined = [
        (hide_label, PredefinedMenuItem::hide(Some(app.ui.language.tr(hide_label)))),
        (hide_others_label, PredefinedMenuItem::hide_others(Some(app.ui.language.tr(hide_others_label)))),
    ];
    let [(_, hide), (_, hide_others)] = &predefined;
    // Quit is ours, not the predefined item: that one calls `terminate:` and skips the unsaved
    // changes prompt (#1943); the `app.quit` command closes the window through it
    let (quit_id, quit_accel) = quit_item(app);
    let quit = MenuItem::with_id(quit_id, app.ui.language.tr(quit_label), true, quit_accel);
    let _ = app_menu.append_items(&[
        &about,
        &discord,
        &PredefinedMenuItem::separator(),
        &settings,
        &PredefinedMenuItem::separator(),
        hide,
        hide_others,
        &PredefinedMenuItem::separator(),
        &quit,
    ]);
    let _ = bar.append(&app_menu);
    for top in MENUS {
        let sub = Submenu::new(app.ui.language.tr(top), true);
        titles.push((top.into(), sub.clone()));
        let mine: Vec<&Item> = items.iter().filter(|i| i.path.first().map(String::as_str) == Some(top) && !i.id.starts_with("app.settings.")).collect();
        // submenus by path prefix (e.g. Clip ▸ Video Options ▸ Time Interpolation), created where
        // their first item appears
        let mut subs: Vec<(Vec<String>, Submenu)> = Vec::new();
        for it in mine {
            let mi = MenuItem::with_id(it.id.clone(), &it.label, it.enabled, it.shortcut.as_deref().and_then(accel));
            native.push(((*it).clone(), mi.clone()));
            let mut parent = sub.clone();
            for depth in 1..it.path.len() {
                let key = it.path[..=depth].to_vec();
                parent = match subs.iter().find(|(k, _)| *k == key) {
                    Some((_, s)) => s.clone(),
                    None => {
                        let s = Submenu::new(app.ui.language.tr(&it.path[depth]), true);
                        titles.push((it.path[depth].clone(), s.clone()));
                        let _ = parent.append(&s);
                        subs.push((key, s.clone()));
                        s
                    }
                };
            }
            let _ = parent.append(&mi);
        }
        let _ = bar.append(&sub);
    }
    bar.init_for_nsapp();
    if let Some(help) = bar.items().last().and_then(|i| i.as_submenu().cloned()) {
        help.set_as_help_menu_for_nsapp();
    }
    Box::leak(Box::new(bar));
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        while let Ok(ev) = MenuEvent::receiver().recv() {
            if tx.send(ev.id().0.clone()).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    let mut shown_language = app.ui.language;
    let update: MenuUpdater = Box::new(move |items: &[Item]| {
        let checked = |id: &str| items.iter().any(|it| it.id == id && it.checked == Some(true));
        let language = if checked("app.language.japanese") {
            filmcraft_ui_egui::i18n::Language::Ja
        } else if checked("app.language.spanish") {
            filmcraft_ui_egui::i18n::Language::Es
        } else if checked("app.language.portuguese") {
            filmcraft_ui_egui::i18n::Language::PtBr
        } else if checked("app.language.ukrainian") {
            filmcraft_ui_egui::i18n::Language::Uk
        } else if checked("app.language.chinese") {
            filmcraft_ui_egui::i18n::Language::ZhCn
        } else if checked("app.language.german") {
            filmcraft_ui_egui::i18n::Language::De
        } else if checked("app.language.russian") {
            filmcraft_ui_egui::i18n::Language::Ru
        } else if checked("app.language.french") {
            filmcraft_ui_egui::i18n::Language::Fr
        } else if checked("app.language.indonesian") {
            filmcraft_ui_egui::i18n::Language::Id
        } else if checked("app.language.italian") {
            filmcraft_ui_egui::i18n::Language::It
        } else if checked("app.language.korean") {
            filmcraft_ui_egui::i18n::Language::Ko
        } else if checked("app.language.thai") {
            filmcraft_ui_egui::i18n::Language::Th
        } else {
            filmcraft_ui_egui::i18n::Language::En
        };
        if language != shown_language {
            shown_language = language;
            for (title, submenu) in &titles {
                submenu.set_text(language.tr(title));
            }
        }
        for (label, item) in &predefined {
            item.set_text(language.tr(label));
        }
        quit.set_text(language.tr(quit_label));
        for (shown, mi) in &mut native {
            let Some(it) = items.iter().find(|i| i.id == shown.id) else { continue };
            if it.label != shown.label {
                // Settings entries drop the trailing ellipsis, as when the menu was built
                mi.set_text(if it.id.starts_with("app.settings.") { it.label.trim_end_matches('…') } else { &it.label });
            }
            if it.shortcut != shown.shortcut {
                let _ = mi.set_accelerator(it.shortcut.as_deref().and_then(accel));
            }
            if it.enabled != shown.enabled {
                mi.set_enabled(it.enabled);
            }
            *shown = it.clone();
        }
    });
    (rx, update)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_engine::Session;

    /// #1943: Cmd+Q must reach the `app.quit` command, which closes the window through the unsaved
    /// changes prompt, instead of the predefined Quit item's `terminate:`.
    #[test]
    fn quit_item_runs_the_app_quit_command_on_cmd_q() {
        let app = FilmcraftApp::new(Session::default());
        let (id, accelerator) = quit_item(&app);
        assert_eq!(id, "app.quit");
        assert_eq!(accelerator, accel("Cmd+Q"));
        assert!(accelerator.is_some(), "Cmd+Q is a native key equivalent");
    }
}
