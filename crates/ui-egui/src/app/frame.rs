use super::{FilmcraftApp, Tick, Tokens, crash, header, i18n, panels, state, updates_ui};

impl FilmcraftApp {
    // ---------------------------------------------------------------- frame

    /// Settings ▸ Media ▸ Automatically refresh growing files: every *Refresh growing files every*
    /// seconds, and as soon as the window comes back to the front (after a render in another app),
    /// stamp the project's media files on a worker thread and read the changed ones again
    /// ([`filmcraft_engine::media_watch`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn tick_media_refresh(&mut self, ctx: &egui::Context, now: f64) {
        if let Some(r) = self.session.poll_media_scan() {
            match r {
                Ok(r) => {
                    if let Some(n) = r["refreshed"].as_array().map(Vec::len).filter(|n| *n > 0) {
                        self.toast = Some((format!("Refreshed {n} media item(s) changed on disk"), now));
                    }
                }
                Err(e) => self.toast = Some((e.to_string(), now)),
            }
        }
        let media = &self.session.prefs.media;
        let (on, every) = (media.refresh_growing_files, f64::from(media.growing_refresh_seconds.max(1)));
        let (last_key, focus_key) = (egui::Id::unique("mediaWatch.lastScan"), egui::Id::unique("mediaWatch.focused"));
        let focused = self.native_window_focused.unwrap_or(false);
        let was_focused: bool = ctx.data(|d| d.get_temp(focus_key)).unwrap_or(focused);
        ctx.data_mut(|d| d.insert_temp(focus_key, focused));
        if !on || filmcraft_engine::media_watch::media_files(&self.session.project, &self.session.media).is_empty() {
            return;
        }
        let last: f64 = ctx.data(|d| d.get_temp(last_key)).unwrap_or(f64::NEG_INFINITY);
        if now - last >= every || (focused && !was_focused) {
            ctx.data_mut(|d| d.insert_temp(last_key, now));
            self.session.start_media_scan();
        }
        let wait = if self.session.media_scan.is_some() { 0.1 } else { (every - (now - last)).clamp(0.1, every) };
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
    }

    /// Top-level UI pass: set the language, honor the injected panic test hook, lay out header, body and status bar, and dispatch the active mode.
    pub(super) fn frame(&mut self, ui: &mut egui::Ui) {
        i18n::set_current(self.ui.language);
        if std::mem::take(&mut self.panic_next_frame) {
            crash::injected_fault("injected UI fault");
        }
        let ctx = ui.ctx().clone();
        // Text to Speech panel selection and its repaint cadence are presentation state; they do
        // not poll a background queue.
        panels::tts::follow_selection(self);
        if self.ui.tts.pending.is_some() || self.ui.tts.download_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let t = self.tokens;
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0.0, t.app_bg);
        let header_h = if self.ui.show_header { header::HEIGHT } else { 0.0 };
        let header = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), header_h));
        if self.ui.show_header {
            header::show(self, ui, header);
        }
        let status_h = if self.ui.show_status_bar { 20.0 } else { 0.0 };
        let body = egui::Rect::from_min_max(egui::pos2(full.min.x + 1.0, header.max.y + 1.0), egui::pos2(full.max.x - 1.0, full.max.y - status_h - 2.0));
        match self.ui.mode {
            state::Mode::Edit => self.dock_area(ui, body),
            state::Mode::Import => panels::import_mode::show(self, ui, body),
            state::Mode::Export => panels::export_mode::show(self, ui, body),
        }
        // the Timeline has had its chance to place a file drop this frame; anything left stays a bin import
        self.pending_timeline_drop = None;
        panels::dialogs::show(self, &ctx);
        updates_ui::show(self, &ctx);
        if !self.ui.show_status_bar {
            return;
        }
        // Status / hint bar
        let sb = egui::Rect::from_min_max(egui::pos2(full.min.x, full.max.y - status_h), full.max);
        ui.painter().rect_filled(sb, 0.0, t.header_bg);
        let now = ui.input(|i| i.time);
        if self.ui.status != self.status_seen.0 {
            self.status_seen = (self.ui.status.clone(), now);
        } else if !self.ui.status.is_empty() {
            if now - self.status_seen.1 > 8.0 {
                self.ui.status.clear();
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            }
        }
        // The control server's startup notice (#3436): a session that asked for `--control` must
        // be able to see that it is not serving, whenever it looks. Any real status replaces it;
        // it comes back once that clears. Each cause translates as a whole sentence, so the
        // status bar reads in the interface language throughout.
        if self.ui.status.is_empty()
            && let Some(notice) = &self.control_notice
        {
            match notice {
                crate::ControlNotice::Token => {
                    self.ui.status =
                        tl!("The control server is disabled: FILMCRAFT_CONTROL_TOKEN must contain 64 random hex characters (see docs/control-protocol.md)")
                            .into()
                }
            }
        }
        let hint = if !self.ui.status.is_empty() { self.ui.status.clone() } else { self.hint_text() };
        ui.painter().text(egui::pos2(sb.min.x + 10.0, sb.center().y), egui::Align2::LEFT_CENTER, hint, Tokens::ui(11.0), t.text_dim);
        let resp = ui.interact(sb, egui::Id::unique("status-bar"), egui::Sense::click());
        if resp.clicked() {
            self.ui.status.clear();
        }
        self.job_status(ui, sb, &t);
    }

    /// The running job (export / render previews), if any; plays the rendered range when a preview
    /// render completes. Runs every frame, with or without the status bar.
    pub(super) fn poll_jobs(&mut self, ctx: &egui::Context) -> Option<filmcraft_engine::Job> {
        use std::sync::atomic::Ordering;
        let running = self.session.jobs.iter().rev().find(|j| !j.progress.finished.load(Ordering::Relaxed)).cloned();
        // Play after rendering previews.
        if let Some((id, from)) = self.watched_render
            && let Some(j) = self.session.jobs.iter().find(|j| j.id == id)
            && j.progress.finished.load(Ordering::Relaxed)
        {
            self.watched_render = None;
            let ok = j.progress.error.lock().map(|e| e.is_none()).unwrap_or(false);
            if ok && self.ui.play_after_render && !self.playback.playing {
                self.session.set_playhead(from);
                self.play(1.0);
            }
        }
        let job = running?;
        if job.label.starts_with("Rendering ") && !job.label.contains("audio") && self.watched_render.is_none_or(|w| w.0 != job.id) {
            let from = self.session.active_sequence().and_then(|q| q.mark_in).unwrap_or(Tick::ZERO);
            self.watched_render = Some((job.id, from));
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(150));
        Some(job)
    }

    /// The running job for drawing only; polling/play-after-render stays in logic.
    fn running_job(&self) -> Option<filmcraft_engine::Job> {
        use std::sync::atomic::Ordering;
        self.session.jobs.iter().rev().find(|j| !j.progress.finished.load(Ordering::Relaxed)).cloned()
    }

    /// Right side of the status bar: the running job (export / render previews) with a progress
    /// bar and a cancel button.
    fn job_status(&mut self, ui: &mut egui::Ui, sb: egui::Rect, t: &Tokens) {
        use std::sync::atomic::Ordering;
        let Some(job) = self.running_job() else { return };
        let f = job.progress.fraction().clamp(0.0, 1.0);
        let left = job.progress.eta().map(panels::left_text).unwrap_or_default();
        let cancel = egui::Rect::from_center_size(egui::pos2(sb.max.x - 14.0, sb.center().y), egui::vec2(14.0, 14.0));
        let bar = egui::Rect::from_min_size(egui::pos2(cancel.min.x - 128.0, sb.center().y - 3.0), egui::vec2(120.0, 6.0));
        let p = ui.painter();
        p.rect_filled(bar, 3.0, t.separator);
        p.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * f, bar.height())), 3.0, t.accent);
        let verb = if job.label.starts_with("Rendering") { crate::i18n::t(&job.label).to_string() } else { tl!("Exporting").to_string() };
        p.text(
            egui::pos2(bar.min.x - 8.0, sb.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("{verb}… {:.0}%{left}", f * 100.0),
            Tokens::ui(11.0),
            t.text_dim,
        );
        let resp = ui.interact(cancel, egui::Id::unique(("job-cancel", job.id)), egui::Sense::click());
        let c = if resp.hovered() { t.hot_text } else { t.text_dim };
        let k = 3.5;
        p.line_segment([cancel.center() - egui::vec2(k, k), cancel.center() + egui::vec2(k, k)], egui::Stroke::new(1.4, c));
        p.line_segment([cancel.center() + egui::vec2(-k, k), cancel.center() + egui::vec2(k, -k)], egui::Stroke::new(1.4, c));
        self.auto.add("status.job.cancel", cancel, &format!("Cancel {}", job.label));
        self.auto.add("status.job.progress", bar, &format!("{:.0}%{left}", f * 100.0));
        if resp.on_hover_text(tl!("Cancel")).clicked() {
            job.progress.cancel.store(true, Ordering::Relaxed);
            self.watched_render = None;
        }
    }

    /// Contextual hint for the status bar (Premiere shows tool/gesture hints here).
    fn hint_text(&self) -> String {
        let mac = cfg!(target_os = "macos");
        match self.ui.tool {
            state::Tool::Selection if mac => {
                tl!("Click to select, or click in empty space and drag to marquee select. Use Shift, Opt, and Cmd for other options.")
            }
            state::Tool::Selection => tl!("Click to select, or click in empty space and drag to marquee select. Use Shift, Alt, and Ctrl for other options."),
            state::Tool::TrackSelectForward => tl!("Click to select all clips to the right in all tracks. Shift-click for a single track."),
            state::Tool::TrackSelectBackward => tl!("Click to select all clips to the left in all tracks. Shift-click for a single track."),
            state::Tool::Ripple => tl!("Drag an edit point to ripple trim; later clips move to keep the gap closed."),
            state::Tool::Rolling => tl!("Drag an edit point to roll it: the out of one clip and the in of the next move together."),
            state::Tool::RateStretch => tl!("Drag an edge to change the clip's speed so it fills the new duration."),
            state::Tool::Remix => tl!("Drag the edge of a music clip to remix it to the new duration at musically matching beats."),
            state::Tool::Razor => tl!("Click to split a clip. Shift-click to split all tracks."),
            state::Tool::Slip => tl!("Drag a clip to slip its source in/out without moving it."),
            state::Tool::Slide => tl!("Drag a clip to slide it between its neighbours."),
            state::Tool::Hand => tl!("Drag to scroll the timeline."),
            state::Tool::Zoom if mac => tl!("Click to zoom in; Opt-click to zoom out."),
            state::Tool::Zoom => tl!("Click to zoom in; Alt-click to zoom out."),
            _ => "",
        }
        .to_string()
    }
}

impl FilmcraftApp {
    /// The "FilmCraft hit an error" window after a caught UI panic. Automation ids:
    /// `error.dismiss`, `error.save`.
    pub(super) fn error_window(&mut self, ctx: &egui::Context) {
        let Some(msg) = self.ui_error.clone() else { return };
        let mut close = false;
        egui::Window::new(tl!("FilmCraft hit an error"))
            .id(egui::Id::unique("FilmCraft hit an error"))
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(crate::widgets::dialog_center(ctx))
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.label(tl!("Something went wrong while drawing the window. Your project is still open; save it to be safe."));
                ui.add_space(6.0);
                ui.label(egui::RichText::new(&msg).monospace().small());
                if let Some(p) = crash::log_path() {
                    ui.label(egui::RichText::new(tlf!("Details: {path}", path = p.display())).small());
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let s = ui.button(tl!("Save Project"));
                    self.auto.add("error.save", s.rect, "Save Project");
                    if s.clicked() {
                        let _ = crate::menus::invoke(self, ctx, "file.save", serde_json::json!({}));
                    }
                    let d = ui.button(tl!("Continue"));
                    self.auto.add("error.dismiss", d.rect, "Continue");
                    close |= d.clicked();
                });
            });
        if close {
            self.ui_error = None;
        }
    }
}
