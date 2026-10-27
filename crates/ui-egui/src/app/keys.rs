use super::*;

impl FilmcraftApp {
    /// Per-pass shortcut handling: reset the pasted image, rebuild bindings only when the shortcut revision or workspace set changes, and leave unmodified keys
    /// to a focused text field except dock dividers.
    pub(super) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // A paste image payload belongs to the paste event in this pass only.
        self.pasted_image = None;
        let workspaces = (dock::names(&self.workspaces), self.ui.workspace.clone());
        if self.bindings_rev != self.session.shortcuts.revision || workspaces != self.menu_workspaces {
            self.menu_workspaces = workspaces;
            self.bindings = menus::bindings(self);
            self.bindings_rev = self.session.shortcuts.revision;
        }
        // A focused dock divider or floating-window grip takes the arrow keys to nudge it, but no
        // other key: Space and the other shortcuts keep working (#1482).
        let divider_focused = self.divider_focused(ctx);
        if (ctx.egui_wants_keyboard_input() && !divider_focused) || self.dialog == Some(Dialog::Shortcuts) {
            return;
        }
        // A focused text field takes every unmodified key, including the ones our shortest
        // shortcuts use, so an input method can compose there (Vietnamese Telex, Korean, …; #243).
        // `egui_wants_keyboard_input` above is not enough: it is read before this pass lays the UI
        // out, so it still reports the previous pass, and it is true for any focused widget, which
        // would leave the editor without its single-key tool shortcuts while a slider has focus.
        let text_focused = ctx.text_edit_focused();
        // Esc cancels a dynamic trim in progress
        if self.session.trim_play.dynamic.is_some() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            let _ = self.session.execute("trim.cancelDynamic", json!({}));
        }
        panels::masks::pen_take_back(self, ctx, text_focused);
        let fire = ctx.input_mut(|i| self.match_shortcuts(i, text_focused, divider_focused));
        self.fire_shortcuts(ctx, fire);
    }

    /// Key, text and clipboard events that a floating panel in its own OS window left over
    /// (`DockOutput::forwarded_events`): they run the main window's bindings in the frame they
    /// arrive, so shortcuts work while a floating window has focus. craft-ui forwards nothing
    /// while that window's text field has focus.
    pub(crate) fn handle_forwarded_keys(&mut self, ctx: &egui::Context, events: Vec<egui::Event>) {
        if events.is_empty() || self.dialog == Some(Dialog::Shortcuts) {
            return;
        }
        let mut input = egui::InputState::default();
        input.modifiers = self.floating_window_modifiers(ctx);
        input.events = events;
        if self.session.trim_play.dynamic.is_some() && input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            let _ = self.session.execute("trim.cancelDynamic", json!({}));
        }
        let fire = self.match_shortcuts(&mut input, false, false);
        self.fire_shortcuts(ctx, fire);
    }

    /// The modifiers held in the focused native floating window. Copy, Cut and Paste events
    /// carry none, and egui-winit drops the key press they stand for.
    fn floating_window_modifiers(&self, ctx: &egui::Context) -> egui::Modifiers {
        self.ui
            .dock
            .floating
            .iter()
            .filter(|window| window.native.is_some())
            .map(|window| craft_ui::docking::DockArea::<PanelKind>::window_viewport(dock_ui::area_id(), window.id))
            .find_map(|viewport| ctx.input_for(viewport, |i| (i.viewport().focused == Some(true)).then_some(i.modifiers)))
            .unwrap_or_default()
    }

    /// The binding ids the key presses in `i` fire, consuming those presses.
    fn match_shortcuts(&mut self, i: &mut egui::InputState, text_focused: bool, divider_focused: bool) -> Vec<String> {
        // Panel shortcuts of the focused panel override application shortcuts on the same chord.
        // egui ignores extra Shift and Alt when matching, so the most specific chords go first
        // across both scopes (the sort is stable, so the panel still wins a tie): otherwise the
        // Timeline's `Delete` (Clear) swallows the app-wide `Shift+Delete` (Ripple Delete) (#680).
        let focused = self.ui.focused.title();
        let mut fire = Vec::new();
        let modifiers = i.modifiers;
        // With no text on the system clipboard, egui-winit drops the Ctrl+V press but still
        // passes its release: put the press back, so Paste can paste files or an image (#611).
        let mut swallowed = None;
        for e in &i.events {
            match e {
                egui::Event::Key { key: egui::Key::V, pressed, modifiers: m, .. } => {
                    if !pressed && !self.v_down {
                        swallowed = Some(*m);
                    }
                    self.v_down = *pressed;
                }
                egui::Event::PasteImage(image) => {
                    // Keep the delivered pixels; re-reading the OS clipboard later is racy and
                    // the web host has no clipboard-media hook at all.
                    self.pasted_image = Some(image.clone());
                    if modifiers.command {
                        self.v_down = true;
                    }
                }
                egui::Event::Paste(_) if modifiers.command => self.v_down = true,
                _ => {}
            }
        }
        if let Some(m) = swallowed {
            i.events.push(egui::Event::Key {
                key: egui::Key::V,
                physical_key: Some(egui::Key::V),
                pressed: true,
                repeat: false,
                modifiers: m | egui::Modifiers::COMMAND,
            });
        }
        clipboard_events_as_keys(&mut i.events, modifiers);
        let panel = self.bindings.iter().filter(|b| b.3.as_deref() == Some(focused));
        let app_wide = self.bindings.iter().filter(|b| b.3.is_none());
        let mut candidates: Vec<&menus::KeyBinding> = panel.chain(app_wide).collect();
        candidates.sort_by_key(|b| std::cmp::Reverse(menus::specificity(b.0)));
        for (m, k, id, _) in candidates {
            // With a text field focused, an unmodified key belongs to the field (and to the input
            // method composing in it), not to a tool shortcut: `J`, `Z`, `R`, `A`… are the keys
            // Vietnamese Telex combines into diacritics (#243). Chords with a modifier stay:
            // Ctrl+S and friends are not something a text field or an IME consumes.
            if lets_text_field_keep(*m, text_focused) {
                continue;
            }
            // The focused divider consumes the arrows itself (egui matches them with any Shift/Alt).
            if divider_focused && matches!(k, egui::Key::ArrowLeft | egui::Key::ArrowRight | egui::Key::ArrowUp | egui::Key::ArrowDown) {
                continue;
            }
            if i.consume_key(*m, *k) {
                fire.push(id.clone());
            }
        }
        fire
    }

    /// Run the commands of the bindings `match_shortcuts` fired, as the focused panel scopes them.
    fn fire_shortcuts(&mut self, ctx: &egui::Context, fire: Vec<String>) {
        for mut id in fire {
            // Select All / Deselect All act on the Project panel's items when it has focus (#168).
            // Select All takes only what the shown bin lists, not the whole project (#456).
            if self.ui.focused == PanelKind::Project && id == "edit.selectAll" {
                let (cmd, params) = panels::project::select_all_command(self);
                if let Err(e) = menus::invoke(self, ctx, &cmd, params) {
                    self.ui.status = e;
                }
                continue;
            }
            if self.ui.focused == PanelKind::Project && id == "edit.deselectAll" {
                id = "project.deselectAll".into();
            }
            // Clear in the Project panel removes the selected bin when no item is selected (#587).
            let clear = if self.ui.focused == PanelKind::Project && id == "project.delete" {
                self.ui.project_panel.rename = None;
                crate::panels::project_views::clear_params(&self.session.state.project_selection, self.ui.project_panel.selected_bin)
            } else {
                None
            };
            // Mark In/Out in the Source monitor when it has focus.
            let params = if let Some(p) = clear {
                p
            } else if self.ui.focused == PanelKind::Source
                && (matches!(id.as_str(), "markers.markIn" | "markers.markOut" | "markers.clearInOut")
                    || id.starts_with("markers.markSplit")
                    || id.starts_with("markers.goToSplit"))
            {
                json!({"target": "source"})
            } else {
                json!({})
            };
            if let Err(e) = menus::invoke(self, ctx, &id, params) {
                self.ui.status = e;
            }
        }
        // Only the paste action of this pass may consume the event's image.
        self.pasted_image = None;
    }
}

/// The windowing layer (egui-winit, and egui's web backend) reports the clipboard shortcuts as
/// `Event::Copy` / `Event::Cut` / `Event::Paste` instead of key presses: Ctrl+C/X/V on Windows and
/// Linux, plus Ctrl+Insert, Shift+Insert and Shift+Delete on Windows. The shortcut bindings only
/// match key presses, so Copy, Cut, Paste, Paste Insert, Paste Attributes and (on Windows) Ripple
/// Delete never fired from the keyboard (#199). macOS was unaffected because its native menu bar
/// takes the key equivalents first.
///
/// Add the key press each event stands for, with the modifiers held, so the bindings see it. The
/// original event stays for anything else that reads it. Only called when no text field has
/// keyboard focus; text fields keep handling the clipboard themselves.
fn clipboard_events_as_keys(events: &mut Vec<egui::Event>, modifiers: egui::Modifiers) {
    use egui::{Event, Key};
    let keys: Vec<Key> = events
        .iter()
        .filter_map(|e| match e {
            Event::Copy if modifiers.command => Some(Key::C),
            Event::Copy => Some(Key::Copy),
            Event::Cut if modifiers.command => Some(Key::X),
            Event::Cut if modifiers.shift => Some(Key::Delete),
            Event::Cut => Some(Key::Cut),
            Event::Paste(_) | Event::PasteImage(_) if modifiers.command => Some(Key::V),
            Event::Paste(_) | Event::PasteImage(_) if modifiers.shift => Some(Key::Insert),
            Event::Paste(_) | Event::PasteImage(_) => Some(Key::Paste),
            _ => None,
        })
        .collect();
    events.extend(keys.into_iter().map(|key| Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }));
}

/// Whether a shortcut chord must be left alone because a text field is focused.
///
/// A focused text field owns every unmodified key: an input method composes there from plain
/// letters (Vietnamese Telex builds `á` from `a` + `s`, `ả` from `a` + `r`, `ạ` from `a` + `j`),
/// and our single-key tool shortcuts would swallow them before the field sees them (#243). Chords
/// with a modifier are not what a text field or an IME consumes, so they keep working.
fn lets_text_field_keep(modifiers: egui::Modifiers, text_focused: bool) -> bool {
    text_focused && menus::specificity(modifiers) == 0
}

#[cfg(test)]
mod clipboard_key_tests {
    use egui::{Event, Key, Modifiers};

    fn keys(events: Vec<Event>, m: Modifiers) -> Vec<(Key, Modifiers)> {
        let mut events = events;
        super::clipboard_events_as_keys(&mut events, m);
        events.into_iter().filter_map(|e| if let Event::Key { key, modifiers, pressed: true, .. } = e { Some((key, modifiers)) } else { None }).collect()
    }

    /// Clipboard Copy/Cut/Paste events become C/X/V key presses so they match the shortcut bindings.
    #[test]
    fn ctrl_c_x_v_become_key_presses() {
        let c = Modifiers::COMMAND;
        assert_eq!(keys(vec![Event::Copy], c), vec![(Key::C, c)]);
        assert_eq!(keys(vec![Event::Cut], c), vec![(Key::X, c)]);
        assert_eq!(keys(vec![Event::Paste("x".into())], c), vec![(Key::V, c)]);
        let image = || Event::PasteImage(std::sync::Arc::new(egui::ColorImage::filled([1, 1], egui::Color32::RED)));
        assert_eq!(keys(vec![image()], c), vec![(Key::V, c)]);
        assert_eq!(keys(vec![image()], Modifiers::SHIFT), vec![(Key::Insert, Modifiers::SHIFT)]);
        assert_eq!(keys(vec![image()], Modifiers::NONE), vec![(Key::Paste, Modifiers::NONE)]);
    }

    #[test]
    fn extra_modifiers_are_kept() {
        // Paste Insert (Ctrl+Shift+V) and Paste Attributes (Ctrl+Alt+V) also arrive as Paste
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        assert_eq!(keys(vec![Event::Paste("x".into())], cs), vec![(Key::V, cs)]);
        let ca = Modifiers::COMMAND | Modifiers::ALT;
        assert_eq!(keys(vec![Event::Paste("x".into())], ca), vec![(Key::V, ca)]);
    }

    #[test]
    fn windows_insert_and_delete_variants() {
        // Shift+Delete (Ripple Delete) arrives as Cut on Windows; Shift+Insert as Paste
        assert_eq!(keys(vec![Event::Cut], Modifiers::SHIFT), vec![(Key::Delete, Modifiers::SHIFT)]);
        assert_eq!(keys(vec![Event::Paste("x".into())], Modifiers::SHIFT), vec![(Key::Insert, Modifiers::SHIFT)]);
    }

    #[test]
    fn other_events_are_left_alone() {
        let mut events = vec![Event::Text("a".into()), Event::Copy];
        super::clipboard_events_as_keys(&mut events, Modifiers::COMMAND);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], Event::Text(_)));
        assert!(matches!(events[1], Event::Copy));
    }
}

#[cfg(test)]
mod forwarded_key_tests {
    use egui::{Event, Key, Modifiers};

    /// A key a floating native window left over runs the main window's bindings.
    #[test]
    fn forwarded_keys_fire_shortcuts() {
        let mut app = super::FilmcraftApp::new(filmcraft_engine::Session::default());
        app.bindings = super::menus::bindings(&app);
        app.ui.focused = crate::dock::PanelKind::Timeline;
        let shift_backtick = Event::Key { key: Key::Backtick, physical_key: Some(Key::Backtick), pressed: true, repeat: false, modifiers: Modifiers::SHIFT };
        app.handle_forwarded_keys(&egui::Context::default(), vec![Event::Text("~".into()), shift_backtick]);
        assert_eq!(app.ui.keys.maximized, Some(crate::dock::PanelKind::Timeline), "Shift+` maximizes the focused frame");
    }
}

#[cfg(test)]
mod text_focus_shortcut_tests {
    use super::lets_text_field_keep;
    use egui::{Key, Modifiers};

    /// Vietnamese Telex composes diacritics from unmodified letters (`a` + `s` → `á`, `a` + `r` →
    /// `ả`, `a` + `j` → `ạ`), and the editor binds `J`, `Z`, `R`, `A`, `C`, `T`, `K`, `L`, `B`, `N`,
    /// `P`, `V`, `Y`, `U`, `H` as single-key tool shortcuts. While a text field is focused those
    /// keys must reach the field, or no diacritic can ever be typed (#243).
    #[test]
    fn an_unmodified_key_reaches_a_focused_text_field() {
        assert!(lets_text_field_keep(Modifiers::NONE, true), "the IME needs the plain letter keys");
    }

    /// Without a text field the editor keeps its single-key tool shortcuts.
    #[test]
    fn an_unmodified_key_still_fires_without_a_text_field() {
        assert!(!lets_text_field_keep(Modifiers::NONE, false));
    }

    /// A modifier chord is not what a text field or an IME consumes, so it keeps firing either way.
    #[test]
    fn a_modified_chord_still_fires_while_text_is_focused() {
        for m in [Modifiers::COMMAND, Modifiers::CTRL, Modifiers::SHIFT, Modifiers::ALT, Modifiers::COMMAND | Modifiers::SHIFT] {
            assert!(!lets_text_field_keep(m, true), "{m:?} is not an IME key");
        }
    }

    /// The keys a Vietnamese Telex IME needs must be exactly the ones we now hand to the field.
    #[test]
    fn the_telex_trigger_keys_are_left_to_the_field() {
        // s=acute, f=grave, r=hook above, x=tilde, j=dot below, w=horn, z=remove diacritic
        for key in [Key::S, Key::F, Key::R, Key::X, Key::J, Key::W, Key::Z] {
            assert!(lets_text_field_keep(Modifiers::NONE, true), "{key:?} must reach the IME");
        }
        // The letters the editor binds as tools are the ones Telex composes with, so they must be
        // left to the field too: `A` (Track Select Forward), `R` (Rate Stretch), `J` (Shuttle Left),
        // `Z` (Zoom), `C` (Razor), `T` (Type).
        for key in [Key::A, Key::R, Key::J, Key::Z, Key::C, Key::T] {
            assert!(lets_text_field_keep(Modifiers::NONE, true), "{key:?} is a Telex key the tools stole");
        }
    }
}
