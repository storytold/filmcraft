use super::{FilmcraftApp, PanelKind, dock_ui, json};

impl FilmcraftApp {
    /// Import dropped files, on the main window and on dock panels in their own OS windows.
    /// With Timeline ▸ drop imports to the timeline, the new items are also queued to be placed
    /// at the pointer (see `timeline::interact`); a drop over the Source monitor opens the first
    /// new item there, ready to cut.
    pub(super) fn handle_drops(&mut self, ctx: &egui::Context) {
        let to_timeline = self.session.prefs.timeline.drop_import_to_timeline;
        // While files hover, the OS sends no cursor events: poll the cursor each frame to draw the Timeline preview.
        let hovering = ctx.input(|i| i.raw.hovered_files.len());
        self.file_drag_hover = None;
        if hovering > 0 && to_timeline {
            ctx.request_repaint();
            self.file_drag_hover = self.drop_pointer(ctx, egui::ViewportId::ROOT, None).map(|p| (hovering, p, egui::ViewportId::ROOT));
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            let pointer = self.drop_pointer(ctx, egui::ViewportId::ROOT, None);
            self.import_dropped(&dropped, None, pointer.map(|p| (p, egui::ViewportId::ROOT)));
        }
        for files in std::mem::take(&mut self.window_files) {
            // Only the Timeline places files at the pointer; elsewhere they go to the bin.
            let at_timeline = to_timeline && files.panel == PanelKind::Timeline;
            let pointer = if at_timeline { self.drop_pointer(ctx, files.viewport, files.pos) } else { None };
            if !files.hovered.is_empty() && self.file_drag_hover.is_none() && at_timeline {
                ctx.request_repaint();
                self.file_drag_hover = pointer.map(|p| (files.hovered.len(), p, files.viewport));
            }
            if !files.dropped.is_empty() {
                self.import_dropped(&files.dropped, Some(files.panel), pointer.map(|p| (p, files.viewport)));
            }
        }
    }

    /// Import the dropped files that exist; with a pointer and Timeline ▸ drop imports to the
    /// timeline, queue the new items for the Timeline drawn in that viewport; a drop over
    /// the Source monitor opens the first item there.
    fn import_dropped(&mut self, dropped: &[egui::DroppedFileHandle], panel: Option<PanelKind>, pointer: Option<(egui::Pos2, egui::ViewportId)>) {
        let mut paths = Vec::new();
        let mut drop_errors = Vec::new();
        for f in dropped {
            let p = f.path();
            match std::fs::metadata(p) {
                Ok(_) => paths.push(p.to_string_lossy().to_string()),
                Err(e) => drop_errors.push(format!("{}: {e}", p.display())),
            }
        }
        if paths.is_empty() {
            // The web handles drops itself (browser files → take_files): their names
            // are not filesystem paths, so there is nothing to import here and no
            // error to report.
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.ui.status = tl!("Could not import the dropped files").to_string();
            }
            return;
        }
        let bin = self.import_bin().0;
        match self.session.execute("file.import", json!({"paths": paths, "bin": bin})) {
            Ok(v) => {
                let items: Vec<filmcraft_engine::project::ItemId> =
                    v["items"].as_array().map(|a| a.iter().filter_map(|i| i.as_u64()).map(filmcraft_engine::project::ItemId).collect()).unwrap_or_default();
                let mut errors: Vec<String> =
                    v["errors"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect()).unwrap_or_default();
                errors.extend(drop_errors);
                let docs = v["documents"].as_array().map_or(0, |a| a.len());
                let seqs = if docs == 0 { v["sequences"].as_array().map_or(0, |a| a.len()) } else { 0 };
                let count = items.len() + docs + seqs;
                self.ui.status = if count == 0 {
                    errors.join("; ")
                } else {
                    let mut s = if count == 1 { tl!("Imported 1 file").to_string() } else { tlf!("Imported {n} files", n = count) };
                    if !errors.is_empty() {
                        s.push_str("; ");
                        s.push_str(&errors.join("; "));
                    }
                    s
                };
                if items.is_empty() {
                    return;
                }
                if let Some((pos, viewport)) = pointer
                    && self.session.prefs.timeline.drop_import_to_timeline
                {
                    self.pending_timeline_drop = Some((items.clone(), pos, viewport));
                }
                let over_source = match panel {
                    Some(p) => p == PanelKind::Source,
                    None => pointer.is_some_and(|(pos, viewport)| self.over_panel(pos, viewport, "panel.Source")),
                };
                if over_source
                    && let Some(first) = items.first()
                    && self.session.execute("source.open", json!({"item": first.0})).is_err()
                {
                    // the import already landed in the bin; a Source that cannot open
                    // this item (a caption sidecar, say) is not an error worth surfacing
                }
            }
            Err(e) => {
                let mut all = vec![e.to_string()];
                all.extend(drop_errors);
                self.ui.status = all.join("; ");
            }
        }
    }

    /// Whether `pos` falls on last frame's rect of the automation element `id` in `viewport`
    /// (a `panel.<Name>` body, a Timeline row target…).
    pub(crate) fn over_panel(&self, pos: egui::Pos2, viewport: egui::ViewportId, id: &str) -> bool {
        self.auto.all().iter().find(|e| e.id == id && e.viewport == viewport).is_some_and(|e| {
            let [x, y, w, h] = e.rect;
            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)).contains(pos)
        })
    }

    /// While files from the OS file manager hover over the window, say where they
    /// will land: the Timeline previews its own placeholder, this banner covers
    /// everywhere else (Project bin, Source monitor…).
    pub(super) fn drop_hint(&self, ctx: &egui::Context) {
        if !matches!(self.ui.mode, crate::state::Mode::Edit) {
            return;
        }
        let hovering = ctx.input(|i| i.raw.hovered_files.len());
        if hovering == 0 {
            return;
        }
        let files = if hovering == 1 { tl!("1 file").to_string() } else { tlf!("{n} files", n = hovering) };
        egui::Area::new(egui::Id::unique("os_file_drop_hint"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new(format!("{} {files}", tl!("Drop to import"))).strong());
                    if self.session.prefs.timeline.drop_import_to_timeline {
                        ui.label(egui::RichText::new(tl!("Drop on the Timeline to place the clips")).small());
                    }
                });
            });
    }

    /// Where files dragged in from the OS are over `viewport`. The OS reports no cursor motion during such a drag
    /// (winit, Windows), so ask it through the host hook, else use `last` (a native window's last
    /// pointer position), else the last pointer position egui saw in the main window.
    fn drop_pointer(&self, ctx: &egui::Context, viewport: egui::ViewportId, last: Option<egui::Pos2>) -> Option<egui::Pos2> {
        let os_pointer = self
            .hooks
            .cursor_screen_position
            .as_ref()
            .and_then(|f| f())
            .and_then(|px| ctx.input_for(viewport, |i| i.viewport().inner_rect.map(|r| screen_px_to_ui(px, i.pixels_per_point, r.min))));
        let fallback = || if viewport == egui::ViewportId::ROOT { ctx.input(|i| i.pointer.hover_pos().or(i.pointer.latest_pos())) } else { None };
        os_pointer.or(last).or_else(fallback)
    }

    /// Whether keyboard focus is on any dock divider or floating-window resize grip, as drawn last pass.
    pub(super) fn divider_focused(&self, ctx: &egui::Context) -> bool {
        use craft_ui::docking::{DockArea, Hit};
        let Some(focused) = ctx.memory(|m| m.focused()) else { return false };
        let area = dock_ui::area_id();
        DockArea::<PanelKind>::hit_rects(ctx, area)
            .iter()
            .any(|(hit, _)| matches!(hit, Hit::Divider(_) | Hit::Resize(..)) && DockArea::hit_id(area, hit) == focused)
    }

    /// Remember the undo history as a mouse press begins, before any panel acts on it (a color
    /// picker sets its color on the press itself), for [`Self::cancel_value_drag`].
    pub(super) fn begin_press(&mut self, ctx: &egui::Context) {
        match ctx.input(|i| i.pointer.press_start_time().filter(|_| i.pointer.any_down())) {
            None => self.value_press = None,
            Some(p) if self.value_press.is_some_and(|(q, _)| q == p) => {}
            Some(p) => self.value_press = Some((p, self.session.undo_mark())),
        }
    }

    /// Latch Escape during pass 0, where the key event is fresh.
    pub(super) fn latch_escape_cancel(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.pointer.any_down() && i.key_pressed(egui::Key::Escape)) {
            self.escape_cancel = true;
        }
    }

    /// Escape while a mouse button is held takes back every edit this press made (#580): value
    /// drags (one merged step or one step per change), color picks, anything committed on the
    /// Escape frame. Called once after the final non-discarded UI pass; the latch outlives a
    /// discarded pass, and pass 1+ has no Escape key event to re-read.
    pub(super) fn cancel_value_drag(&mut self, _ctx: &egui::Context) {
        if !self.escape_cancel {
            return;
        }
        self.escape_cancel = false;
        if let Some((press, mark)) = self.value_press
            && self.session.revert_to(mark) > 0
        {
            self.value_press = Some((press, self.session.undo_mark()));
        }
    }
}

/// A physical-pixel screen position as a point in the window's UI space, given the window's content origin in points.
///
/// Assumes the whole virtual screen uses the window's `pixels_per_point`: with monitors of mixed
/// DPI the point can be off when the cursor and the window's origin are on different monitors.
fn screen_px_to_ui(px: (i32, i32), pixels_per_point: f32, content_min: egui::Pos2) -> egui::Pos2 {
    let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
    egui::pos2(px.0 as f32 / ppp - content_min.x, px.1 as f32 / ppp - content_min.y)
}

#[cfg(test)]
mod screen_px_tests {
    use super::screen_px_to_ui;

    #[test]
    fn maps_screen_pixels_into_the_window() {
        let p = screen_px_to_ui((1300, 900), 2.0, egui::pos2(100.0, 50.0));
        assert_eq!((p.x, p.y), (550.0, 400.0));
    }

    #[test]
    fn a_hostile_scale_does_not_divide_by_zero() {
        for ppp in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let p = screen_px_to_ui((10, 20), ppp, egui::pos2(0.0, 0.0));
            assert!(p.x.is_finite() && p.y.is_finite());
        }
    }
}
