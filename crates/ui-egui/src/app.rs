use super::audio::{AUDIO_RETRY_S, AUDIO_STALL_S};
use super::gpu::{gpu_compositor_unsupported, plan_side};
use super::hooks::HostHooks;
use super::raw_input::drop_native_input;
#[cfg(target_os = "macos")]
use super::raw_input::synthesize_mac_ctrl_click;
use super::*;

mod commands;
mod control;
mod files;
mod frame;
mod gpu;
mod ime;
mod input;
mod keys;
mod playback;
mod prefs;
mod textures;
mod window;

impl FilmcraftApp {
    /// Build the app around one shared `FrameServer`, falling back to the default language on an unparsable preference, loading workspaces from the prefs
    /// directory and seeding IME epoch/sequence identity.
    pub fn new(mut session: Session) -> Self {
        session.shortcuts.register_external(menus::external_commands());
        let recovery = !session.recovery_candidates().is_empty();
        let frames = Arc::new(FrameServer::new(session.media.clone(), session.services.clone(), session.previews.clone(), FrameServer::default_workers()));
        let workspaces =
            session.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| dock::WorkspacePrefs::load(&d.join(dock::WORKSPACES_FILE))).unwrap_or_default();
        let language = i18n::Language::parse(&session.prefs.general.interface_language).unwrap_or_default();
        let ime_document_epoch = session.document_epoch;
        let ime_active_sequence = session.state.active_sequence;
        Self {
            session,
            ui: UiState { language, ..UiState::default() },
            tokens: Tokens::for_kind(ThemeKind::Dark),
            frames,
            playback: Playback { speed: 1.0, ..Default::default() },
            source_playback: Default::default(),
            audio: None,
            scrub: Default::default(),
            hooks: HostHooks::default(),
            // Unsaved changes left by a session that died are offered first thing.
            dialog: recovery.then_some(Dialog::Recovery),
            updates_rt: Default::default(),
            file_dialogs: Default::default(),
            native_dialog: None,
            auto: Default::default(),
            ime_replacement_epoch: 0,
            ime_composition: None,
            ime_reset: Default::default(),
            ime_composition_viewport: None,
            ime_composer: craft_ui::ime::Composer::new(),
            ime_composer_target: None,
            ime_drawn_editor: None,
            ime_document_epoch,
            ime_active_sequence,
            ime_viewports: Vec::new(),
            textures: HashMap::new(),
            thumb_used: HashMap::new(),
            control_rx: None,
            control_notice: None,
            pending_panel_raise: None,
            control_input_only: false,
            native_window_focused: None,
            synthetic_modifiers: egui::Modifiers::NONE,
            deferred: Vec::new(),
            ui_requests: Vec::new(),
            last_ui_time: 0.0,
            synthetic: Vec::new(),
            synthetic_pending_step: None,
            synthetic_step_offered: false,
            synthetic_modifiers_offered: false,
            pasted_image: None,
            clock_start: web_time::Instant::now(),
            ui_clock: 0.0,
            ui_clock_offset: None,
            #[cfg(test)]
            test_now: None,
            mac_ctrl_click_armed: false,
            native_ctrl_click_armed: Arc::default(),
            value_press: None,
            escape_cancel: false,
            loudness: None,
            status_seen: (String::new(), 0.0),
            shots: capture::Shots::default(),
            capture_geometry: None,
            capture_epoch: 0,
            monitor_inexact: false,
            monitor_asked: HashMap::new(),
            program_shown: None,
            timeline_still: 0,
            input_waiters: Vec::new(),
            styled: false,
            ui_error: None,
            panic_next_frame: false,
            quit_confirmed: false,
            fonts_ready: false,
            integrated_titlebar: false,
            last_timeline_width: 1000.0,
            timeline_view_of: None,
            pending_timeline_drop: None,
            file_drag_hover: None,
            window_files: Vec::new(),
            timeline_view_last: None,
            fps: 60.0,
            last_time: 0.0,
            bindings: Vec::new(),
            bindings_rev: 0,
            menu_sent: Vec::new(),
            shortcut_editor: Default::default(),
            toast: None,
            graphics_notice: None,
            tl: Default::default(),
            transcript_cache: Default::default(),
            command_inbox: None,
            v_down: false,
            stale_clipboard_media: None,
            gpu: None,
            watched_render: None,
            applied_prefs: None,
            applied_ui_scale: None,
            workspace_restored: false,
            workspaces,
            menu_workspaces: Default::default(),
        }
    }

    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    /// Isolate an automation-owned app from native keyboard, pointer, drop and menu input.
    /// Window close/quit and screenshot delivery remain available.
    pub fn with_control_input_only(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_input_only = true;
        self.with_control(rx)
    }

    /// `--control` was given but the control server is not serving: keep the reason available to
    /// the status bar (drawn whenever it is idle), where a user or an agent can see it (#3436).
    pub fn with_control_notice(mut self, notice: crate::ControlNotice) -> Self {
        self.control_notice = Some(notice);
        self
    }

    pub fn services(&self) -> Arc<dyn Services> {
        self.session.services.clone()
    }

    /// Time used by logic-only ticks, playback and deadlines.
    ///
    /// The host clock is authoritative (egui's `InputState::time` is stale during
    /// `Context::run_logic`), but an egui pass's reported time can be ahead of it: deterministic
    /// harnesses step a simulated clock without sleeping. Taking the later value keeps those
    /// runs deterministic while a hidden tick still moves on the monotonic host clock.
    pub(crate) fn now(&self) -> f64 {
        #[cfg(test)]
        if let Some(now) = self.test_now {
            return now;
        }
        let host = self.clock_start.elapsed().as_secs_f64();
        let host = self.ui_clock_offset.map_or(host, |offset| host + offset);
        host.max(self.ui_clock)
    }

    /// Override `now()` with a deterministic value for tests; compiled only under `cfg(test)`.
    #[cfg(test)]
    pub(crate) fn set_now_for_test(&mut self, now: f64) {
        self.test_now = Some(now);
    }

    /// Rebuild the frame server if the session's media pool was replaced (e.g. project opened).
    fn sync_pool(&mut self) {
        if !Arc::ptr_eq(&self.frames.pool, &self.session.media) || !Arc::ptr_eq(&self.frames.previews, &self.session.previews) {
            self.frames = Arc::new(FrameServer::new(
                self.session.media.clone(),
                self.session.services.clone(),
                self.session.previews.clone(),
                FrameServer::default_workers(),
            ));
            self.textures.clear();
            self.thumb_used.clear();
            self.tl.reset_media_caches();
            // the new frame server needs the Memory settings
            self.applied_prefs = None;
        }
    }

    pub fn status(&mut self, s: impl Into<String>) {
        self.ui.status = s.into();
    }
}

impl FilmcraftApp {
    /// Install the theme once, then keep fonts ready; a saved language without its system face
    /// falls back to English as before.
    fn logic_theme(&mut self, ctx: &egui::Context) {
        if !self.styled {
            theme::install(ctx, &self.tokens);
            // theme::install replaces the fonts: add the system Japanese font back (or fall back to
            // English when a saved Japanese setting meets a system without one)
            // (likewise Chinese without a Chinese face, Korean without a Hangul one, Georgian without a Georgian one)
            let japanese_missing = self.ui.language == i18n::Language::Ja && !i18n::install_japanese_font(ctx);
            if japanese_missing
                || (self.ui.language == i18n::Language::ZhCn && !i18n::chinese_font_available())
                || (self.ui.language == i18n::Language::Ko && !i18n::korean_font_available())
                || (self.ui.language == i18n::Language::Ka && !i18n::georgian_font_available())
            {
                self.ui.language = i18n::Language::En;
            }
            self.styled = true;
            ctx.request_repaint();
        } else {
            self.fonts_ready = true;
        }
    }

    /// Advance the monotonic host clock and the smoothed frame rate, returning the pass time.
    /// Hidden playback is marked so a hidden window's skipped frames are not counted as drops.
    fn logic_clock(&mut self, ctx: &egui::Context) -> f64 {
        // Monotonic host time. Native and web both call logic once per layout pass and through
        // `run_logic` while hidden; `ctx.input(i.time)` is stale in that second case.
        let now = self.now();
        let dt = (now - self.last_time) as f32;
        if dt > 0.0 && dt.is_finite() {
            self.fps = self.fps * 0.9 + (1.0 / dt).min(480.0) * 0.1;
        }
        self.last_time = now;
        if self.playback.playing && ctx.input(|i| i.viewport().visible()) == Some(false) {
            // Nothing is shown while the window is hidden: not a dropped frame.
            self.playback.hidden = true;
        }
        now
    }

    /// Close-request handling and control-driven synthetic input, in the original order.
    fn logic_control(&mut self, ctx: &egui::Context) {
        // Quitting with unsaved changes asks to save them first.
        if ctx.input(|i| i.viewport().close_requested()) && panels::clip_dialogs::intercept_quit(self) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let had_synthetic = !self.synthetic.is_empty() || self.synthetic_pending_step.is_some();
        self.drain_control(ctx);
        if (!self.synthetic.is_empty() || self.synthetic_pending_step.is_some()) && !had_synthetic {
            // Occluded macOS windows stop running `ui`; bring the window forward (without taking
            // keyboard focus) so the input is processed.
            self.raise_for_control(ctx);
        }
        if !self.synthetic.is_empty() || self.synthetic_pending_step.is_some() {
            ctx.request_repaint();
            // The next step is consumed by a UI pass, not by another logic tick. Native eframe
            // honours this; the pinned web runner logs it as unhandled (a recorded fork gap).
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
    }

    /// Background queues and idempotent deadlines that are safe on every logic call.
    fn logic_background(&mut self, ctx: &egui::Context, now: f64) {
        self.frames.set_context(ctx);
        self.frames.pump(std::time::Duration::from_millis(if self.playback.playing { 24 } else { 40 }));
        self.session.poll_persistence();
        panels::trim_monitor::advance_at(self, now);
        if self.session.trim_play.active() {
            ctx.request_repaint();
        }
        if self.session.persistence.is_some() && self.session.is_dirty() {
            // Keep polling the auto-save worker (status, "also save the project" results) while hidden.
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        self.sync_pool();
        updates_ui::tick(self, ctx);
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_media_refresh(ctx, now);
    }

    /// Apply engine events that change the active sequence/view, IME target, toast or project.
    fn logic_session_events(&mut self, now: f64) {
        for ev in self.session.drain_events() {
            match ev {
                filmcraft_engine::Event::OpenSequence(_) => {
                    // A sequence switch (including opening/reverting/recovering a project, which
                    // emits this event for the newly active sequence) is a new IME owner context.
                    self.restart_ime_target();
                    // show the sequence with its own view (or fitted, the first time)
                    self.timeline_view_of = None;
                    self.ui.dock.restore_timeline();
                    let _ = self.apply_dock(craft_ui::docking::Action::Activate { panel: PanelKind::Timeline });
                }
                filmcraft_engine::Event::OpenSource(_) => {
                    let _ = self.apply_dock(craft_ui::docking::Action::Activate { panel: PanelKind::Source });
                }
                filmcraft_engine::Event::RevealInProject(item) => self.reveal_in_project(item),
                filmcraft_engine::Event::Toast { message, .. } => self.toast = Some((message, now)),
                filmcraft_engine::Event::ProjectChanged { .. } => {}
            }
        }
    }

    /// Run one logic pass in order: retain the later UI clock, poll the native dialog, apply the
    /// logic subsystems, advance playback, poll jobs, then drain completed screenshots and ask
    /// for hidden paints. Safe on hidden logic-only callbacks.
    pub fn logic_inner(&mut self, ctx: &egui::Context) {
        // Remember the pass's clock before anything reads `now()`. A hidden logic-only tick keeps
        // the last UI time (which is stale there), so `now()` never runs behind the host clock.
        self.ui_clock = self.ui_clock.max(ctx.input(|i| i.time));
        // a native file dialog's answer first, so the panels draw what it chose (#260)
        self.poll_native_dialog(ctx);
        self.logic_theme(ctx);
        let now = self.logic_clock(ctx);
        self.logic_control(ctx);
        self.logic_background(ctx, now);
        self.logic_session_events(now);
        self.logic_command_inbox(ctx);
        self.advance_playback(ctx);
        self.advance_source_playback(ctx);
        self.poll_jobs(ctx);
        capture::drain(self, ctx);
        capture::request_hidden_paint(self, ctx);
    }

    /// Draw one real UI pass. Establish the UI clock offset once, run first-pass input handling,
    /// draw panels under a panic guard, drain UI requests and check the IME editor only after
    /// this pass's commands ran. Screenshots are offered to `capture::issue`, which issues only
    /// on an accepted pass.
    fn ui_inner(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if self.ui_clock_offset.is_none() {
            let egui_time = ui.ctx().input(|input| input.time);
            if egui_time.is_finite() {
                let host = self.clock_start.elapsed().as_secs_f64();
                // Never move host time backwards when a harness clock starts behind it; the host
                // clock stays authoritative in that case.
                self.ui_clock_offset = Some((egui_time - host).max(0.0));
            }
        }
        if !self.fonts_ready {
            // This pass did not consume the pending synthetic input; the next hook must offer it
            // again (the UI path does not preserve raw input across this early return).
            self.synthetic_step_offered = false;
            self.synthetic_modifiers_offered = false;
            self.finish_ime_pass(ui.ctx());
            ui.ctx().request_repaint();
            return;
        }
        frame.set_ui_language(Some(self.ui.language.bcp47()));
        let ctx = ui.ctx().clone();
        // A project/sequence replacement may have happened in the previous pass's control drain.
        // Drop the old edit and IME composition before shortcuts or the frame see them.
        self.sync_ime_target_context(&ctx);
        // Each pass draws at most one editor; a stale entry from a previous pass must not be
        // observed as this pass's editor.
        self.ime_drawn_editor = None;
        // Only an actual pass counts toward screenshot settlement; hidden logic ticks do not draw.
        self.timeline_still = if self.ui.timeline.animating() { 0 } else { self.timeline_still.saturating_add(1) };
        // This is a real UI pass: the one synthetic step offered to raw_input_hook has been fed to
        // egui now. A hidden logic tick never reaches this point and keeps it pending.
        self.synthetic_pending_step = None;
        self.synthetic_step_offered = false;
        self.synthetic_modifiers_offered = false;
        let first_pass = ctx.current_pass_index() == 0;
        if first_pass {
            // Setup and fresh-input handling happen where `RawInput::events` are available. Pass 1+
            // has no new events and must not re-run a click, shortcut or drop.
            self.auto.begin_frame();
            self.apply_prefs(&ctx);
            self.sync_timeline_view();
            panels::metadata::follow_selection(self);
            self.handle_drops(&ctx);
            self.begin_press(&ctx);
            self.latch_escape_cancel(&ctx);
            self.handle_shortcuts(&ctx);
            self.sync_native_menu(&ctx);
            self.scrub_audio(&ctx);
        } else {
            // Pass 1+ redraws the same frame; drop the discarded pass's registrations so lookups
            // and `ui.elements` see one entry per widget, from the layout that is accepted.
            self.auto.elements.clear();
        }
        // A panic in one panel must not close the app (losing unsaved work) or leave a blank
        // window: catch it, keep the session, and say what happened.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.frame(ui))).is_err() {
            self.ui_error = Some(crash::take_last().unwrap_or_else(|| "unknown error".into()));
            self.playback.playing = false;
        }
        if !ctx.will_discard() {
            self.cancel_value_drag(&ctx);
        }
        self.error_window(ui.ctx());
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| graphics_notice::show(self, ui.ctx()))).is_err() {
            // a broken notice must not take the session down: drop it
            self.graphics_notice = None;
        }
        self.last_ui_time = self.now();
        if !self.synthetic.is_empty() || self.synthetic_pending_step.is_some() {
            ctx.request_repaint();
        }
        if self.frames.queue_len() > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        if !ctx.will_discard() {
            self.drain_ui_requests(&ctx);
        }
        // Check only after this pass's control requests ran: `ui.menu.invoke`/`engine.execute`
        // may have edited the text or replaced the target. Every other pass (hidden editor, focus
        // in another panel, no editor at all) turns the composer off.
        self.finish_ime_pass(&ctx);
        // Ack `ui.*` waiters once the action had a final, non-discarded UI pass to apply in. Holding
        // them through a discard avoids reporting success for a pass egui threw away.
        if !ctx.will_discard() && self.synthetic.is_empty() && self.synthetic_pending_step.is_none() && !self.input_waiters.is_empty() {
            for w in self.input_waiters.drain(..) {
                let _ = w.send(json!({"ok": true, "result": null}));
            }
        }
        // Only a real, accepted UI pass may issue a capture: it observes the final layout for this
        // pass and `screenshot` itself forces the paint needed when the window is hidden.
        capture::issue(self, &ctx);
    }
}

impl eframe::App for FilmcraftApp {
    /// Snapshot this callback's fresh surface geometry, optionally synthesize macOS ctrl-click
    /// and agent input, and offer at most one queued synthetic step. A step already preserved by
    /// hidden logic callbacks is not appended twice; a real UI pass clears it.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.native_window_focused = Some(raw_input.focused);
        capture::note_raw_geometry(self, ctx, raw_input);
        #[cfg(target_os = "macos")]
        if !self.control_input_only {
            synthesize_mac_ctrl_click(&mut self.mac_ctrl_click_armed, raw_input);
        }
        if self.control_input_only {
            drop_native_input(raw_input);
        }
        if self.control_input_only || !self.synthetic.is_empty() || self.synthetic_pending_step.is_some() {
            // Native modifiers cannot replace those requested by a control gesture. A queued
            // reset runs in the frame after release, so clicked() still sees Shift/Alt.
            if !self.synthetic_modifiers_offered {
                raw_input.events.push(egui::Event::ModifiersChanged(self.synthetic_modifiers));
                self.synthetic_modifiers_offered = true;
            }
            // Widget keyboard handling requires effective egui focus. Control input must work
            // in a background window without activating it or taking the user's OS keyboard.
            raw_input.focused = true;
        }
        if self.synthetic_pending_step.is_none() && !self.synthetic.is_empty() {
            // Pointer events go one per frame so egui sees press → moves → release as a real drag
            // (all in one frame reads as a click); key/text runs go together up to a key release.
            let pointer = |e: &egui::Event| matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. });
            let n = if pointer(&self.synthetic[0]) {
                1
            } else {
                self.synthetic
                    .iter()
                    .position(|e| pointer(e) || matches!(e, egui::Event::Key { pressed: false, .. }))
                    .map_or(self.synthetic.len(), |i| if pointer(&self.synthetic[i]) { i.max(1) } else { i + 1 })
            };
            let step: Vec<egui::Event> = self.synthetic.drain(..n).collect();
            for event in &step {
                if let egui::Event::ModifiersChanged(modifiers) = event {
                    self.synthetic_modifiers = *modifiers;
                }
            }
            self.synthetic_pending_step = Some(step);
            self.synthetic_step_offered = false;
        }
        if let Some(step) = &self.synthetic_pending_step {
            // `raw_input_hook` also runs before hidden `run_logic` ticks, whose raw input egui does
            // not feed to widgets. Keep the exact same one-step batch pending and offer it again
            // on the next callback; a real UI pass clears it at `ui()` start. Never coalesce a
            // press with its release and, crucially, never append a step that the integration
            // already preserved in its pending raw input: the UI pass would then see two copies.
            if !self.synthetic_step_offered {
                raw_input.events.extend(step.iter().cloned());
                self.synthetic_step_offered = true;
            }
        }
    }

    /// Eframe's hidden logic callback: run the logic pass, sync the IME target context, then
    /// suspend reset state because a logic-only callback is never a UI pass.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.logic_inner(ctx);
        self.finish_ime_reset_logic(ctx);
    }

    /// Cancel outstanding screenshots with one error reply each, end per-viewport IME reset
    /// state, and shut the session down so its recovery journal is flushed or removed.
    fn on_exit(&mut self) {
        // Late screenshot callbacks now find a dropped slot and write nothing; clients get one error.
        capture::cancel_all(self);
        for viewport in self.ime_viewports.drain(..) {
            self.ime_reset.end_viewport(viewport);
        }
        // Flush the recovery journal and stop the auto-save worker; a session with nothing unsaved
        // removes its journal, one with unsaved changes keeps it for the next launch.
        self.session.shutdown();
    }

    /// Eframe's UI entry: begin this pass's IME reset state, suspend custom controls when no
    /// eligible editor owns input, draw `ui_inner`, then run end-of-pass IME reconciliation.
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ime_reset.begin_pass(&ctx);
        self.ime_drawn_editor = None;
        let focused = ctx.input(|input| input.viewport().focused.unwrap_or(input.focused));
        let another_field = ctx.memory(|memory| memory.focused().is_some_and(|id| id != egui::Id::unique(panels::graphics::TEXT_EDIT_ID)));
        if !self.fonts_ready || self.ui.gfx_edit.is_none() || self.ime_modal_blocks_input() || !focused || another_field {
            self.suspended_ime_controls(&ctx);
        }
        self.ui_inner(ui, frame);
        self.finish_ime_reset_ui(&ctx);
    }
}

#[cfg(test)]
mod lifecycle_synthetic_tests {
    use egui::{Event, Modifiers, PointerButton, pos2};
    use filmcraft_engine::Session;

    use super::FilmcraftApp;

    /// Extract the pointer-moved positions from a raw event batch, for synthetic-input tests.
    fn pointer_events(events: &[Event]) -> Vec<egui::Pos2> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::PointerMoved(pos) => Some(*pos),
                _ => None,
            })
            .collect()
    }

    /// `raw_input_hook` runs before hidden logic ticks too, but those raw events are not fed to
    /// widgets. The same one-step batch must remain pending, instead of advancing to the next
    /// step or being silently lost; a real UI pass consumes it.
    #[test]
    fn one_synthetic_step_survives_hidden_logic_callbacks() {
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(Session::default());
        app.synthetic = vec![
            Event::PointerMoved(pos2(10.0, 10.0)),
            Event::PointerMoved(pos2(20.0, 20.0)),
            Event::PointerButton { pos: pos2(20.0, 20.0), button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE },
            Event::PointerButton { pos: pos2(20.0, 20.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE },
        ];
        let mut first = egui::RawInput::default();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut first);
        assert_eq!(pointer_events(&first.events), vec![pos2(10.0, 10.0)]);
        assert_eq!(app.synthetic.len(), 3, "only one step was popped");
        assert_eq!(app.synthetic_pending_step.as_ref().map(Vec::len), Some(1));

        // The integration keeps the raw input that did not reach widgets (native
        // `pending_raw_input`, web `input.raw`) and replays it through the hook.
        let mut hidden = first.clone();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut hidden);
        assert_eq!(pointer_events(&hidden.events), vec![pos2(10.0, 10.0)], "the preserved step is not appended twice");
        assert_eq!(app.synthetic.len(), 3, "a hidden callback must not advance the queue");

        // A real UI pass consumes the pending step (App::ui clears it at its start).
        app.synthetic_pending_step = None;
        app.synthetic_step_offered = false;
        app.synthetic_modifiers_offered = false;
        let mut next = egui::RawInput::default();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut next);
        assert_eq!(pointer_events(&next.events), vec![pos2(20.0, 20.0)]);
        assert_eq!(app.synthetic.len(), 2);
    }

    /// eframe's hidden `run_logic` path saves the raw input (including our step) and hands it to
    /// `raw_input_hook` again on the next hidden callback. The hook must not append the same step
    /// a second time, or the eventual UI pass inserts text / clicks / runs a command more than once.
    #[test]
    fn hidden_callbacks_do_not_duplicate_a_step_preserved_in_raw_input() {
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(Session::default());
        app.synthetic =
            vec![Event::Text("once".into()), Event::Key { key: egui::Key::F35, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE }];
        let mut first = egui::RawInput::default();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut first);
        let texts = |events: &[Event]| events.iter().filter(|e| matches!(e, Event::Text(t) if t == "once")).count();
        assert_eq!(texts(&first.events), 1);

        // What the integration preserves as pending_raw_input, then replays through the hook on
        // the next hidden logic tick.
        let mut preserved = first.clone();
        eframe::App::raw_input_hook(&mut app, &ctx, &mut preserved);
        assert_eq!(texts(&preserved.events), 1, "the preserved raw input must keep exactly one copy");
        assert_eq!(app.synthetic_pending_step.as_ref().map(Vec::len), Some(2), "the queue did not advance");
    }
}
