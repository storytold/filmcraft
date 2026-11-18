use super::{FilmcraftApp, ImeComposition, PanelKind, ime_reset, panels};

impl FilmcraftApp {
    /// Start a new replacement epoch for the Program monitor's text edit target.
    ///
    /// The owner key names the sequence, clip and component. A recreated edit session on the
    /// same key (or a document/sequence replacement) must not inherit the previous IME context,
    /// so the app advances this epoch and the editor's next pass starts a new composer.
    pub(crate) fn restart_ime_target(&mut self) {
        self.ime_replacement_epoch = self.ime_replacement_epoch.wrapping_add(1);
        self.invalidate_ime_composition();
    }

    /// Context-less commands block immediately and retire the prior request on a real change.
    /// The next Context-bearing local event boundary requests its current destination's fence.
    pub(crate) fn invalidate_ime_composition(&mut self) {
        let origin = match self.ime_composition {
            Some(ImeComposition::Active(target) | ImeComposition::TerminalPending(target))
                if self.ime_composition_viewport == Some(self.ime_reset.viewport) =>
            {
                Some(target)
            }
            _ => self.ime_reset.origin(self.ime_reset.viewport),
        };
        let Some(origin) = origin else { return };
        let destination = panels::graphics::current_reset_destination(self);
        self.ime_reset.interrupt(origin, destination);
        self.ime_composition = Some(ImeComposition::Stale);
        self.ime_composition_viewport = Some(self.ime_reset.viewport);
    }

    /// Feed the live local edit destination to the reset reducer before each raw event. When an
    /// origin exists, a changed destination retires the old composition token and marks the
    /// composition stale; an unchanged destination only re-arms a pending `NeedsRequest`. Call
    /// with the editor's actual text, never a staged engine copy.
    pub(crate) fn sync_live_ime_destination(&mut self, ctx: &egui::Context, destination: ime_reset::Destination) {
        let viewport = ctx.viewport_id();
        self.ime_reset.viewport = viewport;
        if self.ime_reset.incoming_changed(viewport, destination) {
            let origin = match self.ime_composition {
                Some(ImeComposition::Active(target) | ImeComposition::TerminalPending(target)) if self.ime_composition_viewport == Some(viewport) => {
                    Some(target)
                }
                _ => self.ime_reset.origin(viewport),
            };
            if let Some(origin) = origin {
                self.ime_reset.interrupt(origin, Some(destination));
                self.ime_composition = Some(ImeComposition::Stale);
                self.ime_composition_viewport = Some(viewport);
            }
        }
        self.ime_reset.sync(ctx, destination);
    }

    /// Retire the current composition and suspend this viewport's IME fence. UI paths that lose
    /// the editor route through this; context-less control commands call
    /// `invalidate_ime_composition` directly. Only an eligible final descriptor can re-arm the guard.
    pub(crate) fn suspend_ime_input(&mut self, ctx: &egui::Context) {
        self.ime_reset.viewport = ctx.viewport_id();
        self.invalidate_ime_composition();
        self.ime_reset.suspend(ctx.viewport_id());
    }

    /// The editor did not own this raw route. Controls stay inert after suspension; stock
    /// text/key events remain available to their widgets. Never use this before a live loop.
    pub(crate) fn suspended_ime_controls(&mut self, ctx: &egui::Context) {
        self.suspend_ime_input(ctx);
        ctx.input_mut(|input| input.events.retain(|event| !matches!(event, egui::Event::ImeResetCompleted { .. })));
    }

    /// An actual modal owns input: the in-app `dialog` (Recovery, Revert, About, …) or a
    /// host-shown `native_dialog` picker. `file_dialogs` is only dialog-local state (the recovery
    /// choice); it is not an open modal on its own, so it must not block an otherwise eligible
    /// editor between frames.
    pub(crate) fn ime_modal_blocks_input(&self) -> bool {
        self.dialog.is_some() || self.native_dialog_open()
    }

    /// Hosts with post-logic application work call this at their actual outer return.
    pub fn finish_ime_reset_logic(&mut self, ctx: &egui::Context) {
        self.ime_reset.viewport = ctx.viewport_id();
        self.sync_ime_target_context(ctx);
        if !ime_reset::in_ui_pass(ctx) {
            self.suspend_ime_input(ctx);
        }
    }

    /// End-of-UI-pass reconciliation. Suspend and finish as ineligible when the custom output is
    /// absent, the window is unfocused, a modal owns input, or the text edit lacks focus;
    /// otherwise sync the live destination and let `finish` bind or allocate only for actual
    /// custom eligibility.
    pub(super) fn finish_ime_reset_ui(&mut self, ctx: &egui::Context) {
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(input.focused));
        let eligible = self.ime_reset.owns_output(ctx)
            && focused
            && !self.ime_modal_blocks_input()
            && ctx.memory(|memory| memory.has_focus(egui::Id::unique(panels::graphics::TEXT_EDIT_ID)));
        if !eligible {
            self.suspend_ime_input(ctx);
            let destination = panels::graphics::current_reset_destination(self);
            self.ime_reset.finish(ctx, destination, false);
            return;
        }
        let destination = panels::graphics::current_reset_destination(self);
        if let Some(destination) = destination {
            // Same-pass control work can change only the caret after the overlay drew. Do
            // not allocate until actual custom eligibility has been established above.
            self.sync_live_ime_destination(ctx, destination);
        }
        self.ime_reset.finish(ctx, destination, self.ime_reset.owns_output(ctx));
    }

    /// [`Self::ui`]'s end-of-pass IME check for a native floating window, after its last panel
    /// body: the Program monitor's editor, when it was drawn there, keeps or drops the
    /// composer's descriptor, and the window's reset fence suspends or rearms.
    pub(crate) fn finish_ime_window(&mut self, ctx: &egui::Context) {
        let viewport = ctx.viewport_id();
        if self.ime_drawn_editor.as_ref().is_none_or(|drawn| drawn.viewport != viewport) {
            self.ime_reset.suspend(viewport);
            return;
        }
        self.ime_reset.viewport = viewport;
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(input.focused));
        let eligible = panels::graphics::finish_graphics_ime(self, ctx, false) && focused;
        let destination = panels::graphics::current_reset_destination(self);
        if !eligible {
            self.suspend_ime_input(ctx);
            self.ime_reset.finish_window(ctx, None, None);
            return;
        }
        if let Some(destination) = destination {
            self.sync_live_ime_destination(ctx, destination);
        }
        let output = self.ime_composer_target.and_then(|target| ime_reset::published(ctx, target.owner));
        self.ime_reset.finish_window(ctx, destination, output);
    }

    /// Notice a document replacement or an active-sequence change that did not emit
    /// `OpenSequence` (`file.newProject`, a project with no sequence, `sequence.close`). The
    /// edit target and any in-flight composition belong to the old context.
    pub(super) fn sync_ime_target_context(&mut self, ctx: &egui::Context) {
        let document = self.session.document_epoch;
        let sequence = self.session.state.active_sequence;
        let replaced = document != self.ime_document_epoch;
        if !replaced && sequence == self.ime_active_sequence {
            return;
        }
        self.ime_document_epoch = document;
        self.ime_active_sequence = sequence;
        self.restart_ime_target();
        self.ui.gfx_edit = None;
        self.ime_drawn_editor = None;
        let text_edit = egui::Id::unique(panels::graphics::TEXT_EDIT_ID);
        ctx.memory_mut(|memory| memory.surrender_focus(text_edit));
        ctx.data_mut(|data| data.remove::<PanelKind>(text_edit.with("panel")));
    }

    /// Remember a viewport the Program monitor's editor published in, for [`eframe::App::on_exit`].
    pub(crate) fn note_ime_viewport(&mut self, viewport: egui::ViewportId) {
        if !self.ime_viewports.contains(&viewport) {
            self.ime_viewports.push(viewport);
        }
    }

    /// The main window's end-of-pass check of the Program monitor's editor, after the pass's
    /// control requests ran. Native floating windows ran theirs in [`Self::finish_ime_window`].
    pub(crate) fn finish_ime_pass(&mut self, ctx: &egui::Context) {
        let _ = panels::graphics::finish_graphics_ime(self, ctx, true);
    }
}
