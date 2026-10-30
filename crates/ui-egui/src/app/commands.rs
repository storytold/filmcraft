use super::{FilmcraftApp, Value, json, menus};
use crate::OsEvent;
#[cfg(test)]
use filmcraft_engine::Session;

impl FilmcraftApp {
    // ---------------------------------------------------------------- input

    /// Sends the menu tree to the native menu bar when it changed, so its items show the same
    /// labels, shortcuts and enablement as the in-window menus (Undo with nothing to undo is
    /// disabled). The items a focused text field takes stay enabled while one has the keyboard.
    /// Without a native menu bar this does nothing.
    pub(super) fn sync_native_menu(&mut self, ctx: &egui::Context) {
        if self.hooks.menu_changed.is_none() {
            return;
        }
        let mut items = menus::menu_items(self);
        if menus::text_field_window(ctx).is_some() {
            for it in items.iter_mut().filter(|it| menus::edits_text(&it.id)) {
                it.enabled = true;
            }
        }
        if items == self.menu_sent {
            return;
        }
        if let Some(hook) = self.hooks.menu_changed.as_mut() {
            hook(&items);
        }
        self.menu_sent = items;
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

    /// Act on the operating system's requests: files to open (a Finder double-click, Open With, a
    /// drop on the Dock icon) and the Dock's Quit.
    pub(super) fn logic_os_events(&mut self, ctx: &egui::Context) {
        let events = self.hooks.os_events.as_mut().map(|poll| poll()).unwrap_or_default();
        for event in events {
            match event {
                // control-only mode takes no files from outside, as it takes no native menu commands (#1943)
                OsEvent::Open { .. } if self.control_input_only => {}
                OsEvent::Open { project, media } => self.open_os_files(ctx, project, media),
                OsEvent::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }

    /// Open the project and import the media the OS asked for, then bring the window forward
    /// (restored if it was minimized): the request came from another app.
    fn open_os_files(&mut self, ctx: &egui::Context, project: Option<String>, media: Vec<String>) {
        // a failure is shown in the status bar
        if let Some(path) = project {
            let _ = menus::invoke(self, ctx, "file.open", json!({"path": path}));
        }
        if !media.is_empty() {
            let bin = self.import_bin().0;
            if let Ok(v) = menus::invoke(self, ctx, "file.import", json!({"paths": media, "bin": bin}))
                && let Some(errs) = v.get("errors").and_then(Value::as_array)
                && !errs.is_empty()
            {
                self.ui.status = errs.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ");
            }
        }
        if ctx.input(|i| i.viewport().minimized) == Some(true) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
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
}
