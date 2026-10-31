use super::{FilmcraftApp, menus};
#[cfg(test)]
use filmcraft_engine::Session;
#[cfg(test)]
use serde_json::json;

impl FilmcraftApp {
    // ---------------------------------------------------------------- input

    /// Sends the menu tree to the native menu bar when it changed, so its items show the same
    /// labels, shortcuts and enablement as the in-window menus (Undo with nothing to undo is
    /// disabled). The items a focused text field takes stay enabled while one has the keyboard.
    /// Runs every frame, so the tree is not rebuilt each time: see [`menus::MenuCache`].
    /// Without a native menu bar this does nothing.
    pub(super) fn sync_native_menu(&mut self, ctx: &egui::Context) {
        if self.hooks.menu_changed.is_none() {
            return;
        }
        let text_field = menus::text_field_window(ctx).is_some();
        let mut cache = std::mem::take(&mut self.menu_cache);
        if let Some(items) = cache.changed(self, text_field)
            && let Some(hook) = self.hooks.menu_changed.as_mut()
        {
            hook(items);
        }
        self.menu_cache = cache;
    }
}

impl FilmcraftApp {
    /// Drain native menu/control command ids, preserving the control-only Quit exception.
    pub(super) fn logic_command_inbox(&mut self, ctx: &egui::Context) {
        if let Some(rx) = self.command_inbox.take() {
            while let Ok(id) = rx.try_recv() {
                // control-only mode ignores native menu commands, but Quit stays available (#1943)
                if self.control_input_only && id != "app.quit" {
                    continue;
                }
                if let Err(e) = menus::activate_native(self, ctx, &id) {
                    self.ui.status = e;
                }
            }
            self.command_inbox = Some(rx);
        }
    }
}

#[cfg(test)]
mod native_menu_tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn enabled(items: &[menus::MenuItem], id: &str) -> Option<bool> {
        items.iter().find(|it| it.id == id).map(|it| it.enabled)
    }

    /// The native menu bar (macOS) gets the menu tree whenever it changes, so Edit ▸ Undo and Redo
    /// follow the history instead of staying enabled (#902).
    #[test]
    fn native_menu_follows_undo_and_redo_availability() {
        let sent: Rc<RefCell<Vec<Vec<menus::MenuItem>>>> = Rc::default();
        let mut app = FilmcraftApp::new(Session::default());
        let sink = sent.clone();
        app.hooks.menu_changed = Some(Box::new(move |items: &[menus::MenuItem]| sink.borrow_mut().push(items.to_vec())));
        let ctx = egui::Context::default();

        app.sync_native_menu(&ctx);
        let last = |sent: &Rc<RefCell<Vec<Vec<menus::MenuItem>>>>, id: &str| sent.borrow().last().and_then(|items| enabled(items, id));
        assert_eq!(last(&sent, "edit.undo"), Some(false), "nothing to undo in an empty project");
        assert_eq!(last(&sent, "edit.redo"), Some(false), "nothing to redo in an empty project");
        assert_eq!(last(&sent, "file.newProject"), Some(true));

        app.sync_native_menu(&ctx);
        assert_eq!(sent.borrow().len(), 1, "an unchanged menu is not sent again");

        app.session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        app.sync_native_menu(&ctx);
        assert_eq!(last(&sent, "edit.undo"), Some(true), "an edit can be undone");
        assert_eq!(last(&sent, "edit.redo"), Some(false));

        app.session.execute("edit.undo", json!({})).unwrap();
        app.sync_native_menu(&ctx);
        assert_eq!(last(&sent, "edit.undo"), Some(false));
        assert_eq!(last(&sent, "edit.redo"), Some(true), "the undone edit can be redone");
    }

    /// What a full rebuild sends: [`menus::menu_items`], with the items a focused text field
    /// takes enabled while one has the keyboard (as `sync_native_menu` did before the cache).
    fn rebuilt(app: &FilmcraftApp, ctx: &egui::Context) -> Vec<menus::MenuItem> {
        let mut items = menus::menu_items(app);
        if menus::text_field_window(ctx).is_some() {
            for it in items.iter_mut().filter(|it| menus::edits_text(&it.id)) {
                it.enabled = true;
            }
        }
        items
    }

    /// The native menu is no longer rebuilt every frame (it cost ~0.5 ms a frame), so every kind
    /// of change it shows must still reach it, on the next frame: enablement, checkmarks, labels
    /// (also a language switched at runtime), shortcuts, workspaces, recent projects and a text
    /// field taking the keyboard and letting it go. After each change the tree sent must be
    /// exactly what a full rebuild gives.
    #[test]
    fn native_menu_follows_every_change_it_shows() {
        let sent: Rc<RefCell<Vec<Vec<menus::MenuItem>>>> = Rc::default();
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let mut app = FilmcraftApp::new(session);
        let sink = sent.clone();
        app.hooks.menu_changed = Some(Box::new(move |items: &[menus::MenuItem]| sink.borrow_mut().push(items.to_vec())));
        let ctx = egui::Context::default();
        let row = |sent: &Rc<RefCell<Vec<Vec<menus::MenuItem>>>>, id: &str| sent.borrow().last().and_then(|items| items.iter().find(|it| it.id == id).cloned());
        app.session.execute("edit.deselectAll", json!({})).unwrap();
        app.sync_native_menu(&ctx);
        let ids: Vec<(String, Vec<String>)> = sent.borrow()[0].iter().map(|it| (it.id.clone(), it.path.clone())).collect();
        assert_eq!(row(&sent, "edit.copy").map(|it| it.enabled), Some(false), "nothing selected");

        let field = egui::Id::unique("native-menu-test-field");
        type Change = Box<dyn Fn(&mut FilmcraftApp, &egui::Context)>;
        let menu = |id: &'static str| -> Change {
            Box::new(move |app, ctx| {
                let _ = menus::invoke(app, ctx, id, json!({}));
            })
        };
        let changes: Vec<(&str, Change)> = vec![
            ("select every clip", menu("edit.selectAll")),
            (
                "a text field takes the keyboard",
                Box::new(move |_, ctx| {
                    egui::text_edit::TextEditState::default().store(ctx, field);
                    ctx.memory_mut(|m| m.request_focus(field));
                }),
            ),
            ("the text field lets it go", Box::new(move |_, ctx| ctx.memory_mut(|m| m.surrender_focus(field)))),
            ("quarter playback resolution", menu("view.playbackRes.quarter")),
            ("a guide on the Program monitor", Box::new(|app, _| app.ui.program.guides.push(crate::state::Guide { vertical: true, position: 10.0 }))),
            ("focus the Source monitor", Box::new(|app, _| app.ui.focused = crate::dock::PanelKind::Source)),
            ("dynamic waveforms toggled", Box::new(|app, _| app.ui.extras.dynamic_waveforms = !app.ui.extras.dynamic_waveforms)),
            ("German at runtime", menu("app.language.german")),
            ("rename a label", Box::new(|app, _| app.session.prefs.labels.colors.entry("yellow".into()).or_default().name = "B-Roll".into())),
            ("rebind Save", Box::new(|app, _| assert!(app.session.shortcuts.set("file.save", "Cmd+Shift+9", None, false, true).is_ok()))),
            ("a recent project", Box::new(|app, _| app.session.prefs.general.recent_projects.insert(0, "/tmp/Trailer.fcproj".into()))),
            ("a saved workspace", Box::new(|app, _| app.workspaces.saved.push(crate::dock::SavedWorkspace { name: "Grading".into(), ..Default::default() }))),
            ("the Audio workspace", menu("window.workspace.audio")),
            ("English again", menu("app.language.english")),
        ];
        for (what, change) in &changes {
            let before = rebuilt(&app, &ctx);
            let count = sent.borrow().len();
            change(&mut app, &ctx);
            let expected = rebuilt(&app, &ctx);
            assert_ne!(before, expected, "{what}: the change must show in the menu, or this step tests nothing");
            app.sync_native_menu(&ctx);
            assert_eq!(sent.borrow().len(), count + 1, "{what}: the changed menu was not sent");
            assert!(sent.borrow().last() == Some(&expected), "{what}: the menu sent differs from a full rebuild");
        }
        assert_eq!(row(&sent, "edit.copy").map(|it| it.enabled), Some(true));
        assert_eq!(row(&sent, "edit.paste").map(|it| it.enabled), Some(false), "the text field let go: nothing to paste in the project");
        assert!(sent.borrow().iter().any(|items| items.iter().any(|it| it.id == "edit.paste" && it.enabled)), "Paste was enabled for the text field");
        assert_eq!(row(&sent, "view.playbackRes.quarter").and_then(|it| it.checked), Some(false), "Source monitor focused: its resolution");
        assert_eq!(row(&sent, "edit.label.yellow").map(|it| it.label), Some("B-Roll".to_string()));
        assert_eq!(row(&sent, "file.save").and_then(|it| it.shortcut), Some("Cmd+Shift+9".to_string()));
        assert_eq!(row(&sent, "file.openRecent.0").map(|it| it.label), Some("Trailer.fcproj".to_string()));
        assert_eq!(row(&sent, "file.clearRecent").map(|it| it.enabled), Some(true));
        assert_eq!(row(&sent, "window.workspace.audio").and_then(|it| it.checked), Some(true));
        assert_eq!(row(&sent, "window.workspace.grading").map(|it| it.label), Some("Grading".to_string()));
        assert_eq!(row(&sent, "view.lockGuides").map(|it| it.enabled), Some(false), "no guides on the focused Source monitor");
        assert_eq!(row(&sent, "app.language.english").and_then(|it| it.checked), Some(true));
        // German was shown in between: translated labels and the checkmark, on the same rows
        let german = sent.borrow().iter().find(|items| items.iter().any(|it| it.id == "app.language.german" && it.checked == Some(true))).cloned();
        let german = german.expect("the German menu was sent");
        let save = german.iter().find(|it| it.id == "file.save").map(|it| it.label.clone());
        assert_eq!(save.as_deref(), Some(crate::i18n::Language::De.tr("Save")));
        assert_ne!(save.as_deref(), Some("Save"));
        // The native items were made once, at launch, and are matched by id: a language switch
        // must keep every id and path (designcraft#1548 lost its menu commands that way).
        let german_ids: Vec<(String, Vec<String>)> = german.iter().map(|it| (it.id.clone(), it.path.clone())).collect();
        let without_recent = |v: &[(String, Vec<String>)]| v.iter().filter(|(id, _)| !id.starts_with("file.openRecent.")).cloned().collect::<Vec<_>>();
        assert_eq!(without_recent(&german_ids), without_recent(&ids));
    }

    /// Frames where nothing it shows changed neither rebuild the rows nor resend them; a change
    /// of enablement alone refreshes them without a rebuild.
    #[test]
    fn an_unchanged_native_menu_is_not_rebuilt() {
        let sent = Rc::new(RefCell::new(0usize));
        let mut app = FilmcraftApp::new(Session::default());
        let sink = sent.clone();
        app.hooks.menu_changed = Some(Box::new(move |_: &[menus::MenuItem]| *sink.borrow_mut() += 1));
        let ctx = egui::Context::default();
        for _ in 0..100 {
            app.sync_native_menu(&ctx);
        }
        assert_eq!((*sent.borrow(), app.menu_cache.builds), (1, 1));
        app.session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
        app.sync_native_menu(&ctx);
        assert_eq!((*sent.borrow(), app.menu_cache.builds), (2, 1), "Undo became enabled: sent, from the same rows");
        app.ui.language = crate::i18n::Language::Ja;
        app.sync_native_menu(&ctx);
        assert_eq!((*sent.borrow(), app.menu_cache.builds), (3, 2), "new labels: rebuilt");
        let mut none = FilmcraftApp::new(Session::default());
        none.sync_native_menu(&ctx);
        assert_eq!(none.menu_cache.builds, 0, "no native menu bar, no menu tree");
    }
}
