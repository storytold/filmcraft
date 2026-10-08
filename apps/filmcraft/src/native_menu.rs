//! Native macOS menu bar (muda), generated from the command registry — like Premiere, menus live in
//! the system menu bar. Items send command ids to the app's command inbox.

use std::sync::mpsc::{Receiver, channel};

use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::menus::{MENUS, MenuItem as Item, menu_items};
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

/// Updates native key equivalents when the active keyboard shortcuts change.
pub type ShortcutUpdater = Box<dyn FnMut(&[Item])>;

pub fn install(app: &FilmcraftApp, ctx: egui::Context) -> (Receiver<String>, ShortcutUpdater) {
    let items = menu_items(app);
    let mut native: Vec<(String, MenuItem)> = Vec::new();
    let mut titles: Vec<(String, Submenu)> = Vec::new();
    let bar = Menu::new();
    let app_menu = Submenu::new("FilmCraft", true);
    // FilmCraft ▸ Settings ▸ <category> (Premiere's app-menu layout; General is Cmd+,)
    let settings = Submenu::new(app.ui.language.tr("Settings"), true);
    titles.push(("Settings".into(), settings.clone()));
    for it in items.iter().filter(|i| i.id.starts_with("app.settings.")) {
        let label = it.label.trim_end_matches('…').to_string();
        let mi = MenuItem::with_id(it.id.clone(), label, true, it.shortcut.as_deref().and_then(accel));
        native.push((it.id.clone(), mi.clone()));
        let _ = settings.append(&mi);
    }
    let _ = app_menu.append_items(&[
        &MenuItem::with_id("app.about", "About FilmCraft", true, None),
        &MenuItem::with_id("help.discord", "Join the ArtCraft Discord…", true, None),
        &PredefinedMenuItem::separator(),
        &settings,
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(None),
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
            let mi = MenuItem::with_id(it.id.clone(), &it.label, true, it.shortcut.as_deref().and_then(accel));
            native.push((it.id.clone(), mi.clone()));
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
    let update: ShortcutUpdater = Box::new(move |items: &[Item]| {
        let checked = |id: &str| items.iter().any(|it| it.id == id && it.checked == Some(true));
        let language = if checked("app.language.japanese") {
            filmcraft_ui_egui::i18n::Language::Ja
        } else if checked("app.language.chinese") {
            filmcraft_ui_egui::i18n::Language::ZhCn
        } else if checked("app.language.spanish") {
            filmcraft_ui_egui::i18n::Language::Es
        } else if checked("app.language.portuguese") {
            filmcraft_ui_egui::i18n::Language::PtBr
        } else {
            filmcraft_ui_egui::i18n::Language::En
        };
        for (title, submenu) in &titles {
            submenu.set_text(language.tr(title));
        }
        for (id, mi) in &native {
            if let Some(it) = items.iter().find(|i| &i.id == id) {
                mi.set_text(&it.label);
                let _ = mi.set_accelerator(it.shortcut.as_deref().and_then(accel));
            }
        }
    });
    (rx, update)
}
