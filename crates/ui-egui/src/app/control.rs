use super::*;
use crate::control;

impl FilmcraftApp {
    // ---------------------------------------------------------------- control channel

    /// Make the UI pass run for a control request without stealing the user's keyboard focus.
    pub(crate) fn raise_for_control(&mut self, ctx: &egui::Context) {
        if let Some(raise) = self.hooks.raise_without_focus.as_mut() {
            raise();
        }
        ctx.request_repaint();
    }

    /// Drain queued control requests without blocking: UI requests wait for a layout pass with a 3 s deadline (except screenshots), non-UI requests run now,
    /// and hidden windows are raised and repainted.
    pub(super) fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        let now = self.now();
        let mut reqs: Vec<(ControlRequest, f64)> = std::mem::take(&mut self.deferred);
        while let Ok(req) = rx.try_recv() {
            // UI requests need rendered frames: raise the window if `ui` hasn't run recently
            // (occluded macOS windows stop running `ui`).
            if req.method.starts_with("ui.") && now - self.last_ui_time > 0.25 {
                self.raise_for_control(ctx);
                // A hidden native window still needs a real UI pass for widget state/geometry.
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            }
            reqs.push((req, now + 3.0));
        }
        for (req, deadline) in reqs {
            if req.method.starts_with("ui.") && req.method != "ui.screenshot" {
                self.ui_requests.push((req, deadline));
                ctx.request_repaint();
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
                continue;
            }
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::Retry(msg) => {
                    if now < deadline {
                        self.deferred.push((req, deadline));
                        ctx.request_repaint();
                    } else {
                        let _ = reply.send(json!({"ok": false, "error": msg}));
                    }
                }
                control::Outcome::AfterInput => self.input_waiters.push(reply),
                control::Outcome::Screenshot { path, crop } => {
                    capture::admit(self, ctx, path, crop, reply);
                }
            }
        }
        self.control_rx = Some(rx);
        self.ui_requests.retain(|(req, deadline)| {
            if now > *deadline {
                let _ = req.reply.send(json!({"ok": false, "error": "no UI pass ran before the control request deadline"}));
                false
            } else {
                true
            }
        });
    }

    /// Process control requests that need the current layout/response. Called at the end of a
    /// final, non-discarded UI pass; `ui.click`/`ui.drag` queue synthetic input for the next pass.
    pub(super) fn drain_ui_requests(&mut self, ctx: &egui::Context) {
        let reqs = std::mem::take(&mut self.ui_requests);
        let now = self.now();
        let mut pending = reqs.into_iter();
        while let Some((req, deadline)) = pending.next() {
            // A panel/dock/resize request changes the next layout. Anything after it in this
            // batch (ui.click, ui.elements, ui.screenshot, …) must see the new geometry, so it
            // waits for the next accepted pass rather than reading the layout drawn before it.
            let changes_layout = matches!(req.method.as_str(), "ui.set" | "ui.panel.show" | "ui.panel.close" | "ui.resize");
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::Retry(msg) => {
                    if now < deadline {
                        self.ui_requests.push((req, deadline));
                        ctx.request_repaint();
                    } else {
                        let _ = reply.send(json!({"ok": false, "error": msg}));
                    }
                }
                control::Outcome::AfterInput => self.input_waiters.push(reply),
                control::Outcome::Screenshot { path, crop } => capture::admit(self, ctx, path, crop, reply),
            }
            if changes_layout {
                let rest: Vec<_> = pending.collect();
                if !rest.is_empty() {
                    self.ui_requests.extend(rest);
                    ctx.request_repaint();
                    ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
                }
                break;
            }
        }
    }
}
