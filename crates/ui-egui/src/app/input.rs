use super::{FilmcraftApp, json};

impl FilmcraftApp {
    /// Import dropped files, on the main window and on dock panels in their own OS windows.
    /// With Timeline ▸ drop imports to the timeline, the new items are also queued to be placed
    /// at the pointer (see `timeline::interact`).
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
            self.import_dropped(ctx, &dropped, pointer.map(|p| (p, egui::ViewportId::ROOT)));
        }
        // Give each queued native point a paint pass of its own so it cannot overwrite
        // a placement from an ordinary drop handled earlier in this pass.
        if self.pending_timeline_drop.is_none()
            && let Some(drop) = self.native_timeline_drops.pop_front()
        {
            self.import_paths(ctx, drop.paths, Some((drop.point, drop.viewport)));
        }
        if !self.native_timeline_drops.is_empty() {
            ctx.request_repaint();
        }
        // The dock's window fallback reports drops not handled by the native Timeline's
        // own target. It has no panel or pointer, so these imports go only to the bin.
        let window_files: Vec<String> =
            std::mem::take(&mut self.window_files).into_iter().filter_map(|file| file.path).map(|path| path.to_string_lossy().to_string()).collect();
        if !window_files.is_empty() {
            self.import_paths(ctx, window_files, None);
        }
    }

    /// Take a drop over the Timeline while its own OS window is drawing. The dock's
    /// window fallback receives only files no more-specific target took.
    pub(crate) fn native_timeline_file_drop(&mut self, ui: &egui::Ui, rect: egui::Rect) {
        use craft_behavior::DropOperation;
        use craft_ui::runtime::drag::{DropMark, DropTarget, DropZone, OsFiles, zone_target};
        use craft_ui::runtime::windows::WindowHost;

        if !self.session.prefs.timeline.drop_import_to_timeline || !ui.is_enabled() {
            return;
        }
        struct TimelineFiles(egui::Rect);
        impl DropTarget<OsFiles> for TimelineFiles {
            type Drop = (Vec<String>, egui::Pos2);

            fn accept(&self, files: &OsFiles, at: egui::Pos2, _: DropOperation) -> Option<Self::Drop> {
                let paths: Vec<_> = files.files.iter().filter_map(|file| file.path.as_ref()).map(|path| path.to_string_lossy().into_owned()).collect();
                // Some platforms report only a type while hovering. Keep the zone registered
                // so that its eventual payload can supply the paths.
                (!files.files.is_empty()).then_some((paths, at))
            }

            fn mark(&self, (_, at): &Self::Drop) -> DropMark {
                DropMark::line(*at, egui::pos2(at.x, (at.y + 12.0).min(self.0.bottom())))
            }
        }

        let ctx = ui.ctx();
        let viewport = ctx.viewport_id();
        let hovering = ui.input(|input| input.raw.hovered_files.len());
        if hovering > 0 {
            let pointer = WindowHost::of(ctx).pointer_in(viewport).or_else(|| ui.input(|i| i.pointer.latest_pos()));
            self.file_drag_hover = pointer.filter(|pos| rect.contains(*pos)).map(|pos| (hovering, pos, viewport));
        }
        if let Some((paths, pos)) = zone_target(ui, ui.scope_id().with("native-timeline-files"), DropZone::new(ui.layer_id(), rect), &TimelineFiles(rect)) {
            self.file_drag_hover = None;
            self.native_timeline_drops.push_back(crate::NativeTimelineDrop { paths, point: pos, viewport });
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
    }

    /// Import the dropped files and folders that exist (a folder as a bin of its media, each
    /// sub-folder a bin within it, as in Premiere); with a pointer and Timeline ▸ drop imports to the
    /// timeline, queue the new items for the Timeline drawn in that viewport. A dropped project
    /// file opens instead, as File ▸ Open would (asking first about unsaved changes, #3843).
    fn import_dropped(&mut self, ctx: &egui::Context, dropped: &[egui::DroppedFileHandle], pointer: Option<(egui::Pos2, egui::ViewportId)>) {
        let paths: Vec<String> = dropped.iter().map(|f| f.path().to_string_lossy().to_string()).collect();
        self.import_paths(ctx, paths, pointer);
    }

    /// [`Self::import_dropped`] for the files at `paths`.
    fn import_paths(&mut self, ctx: &egui::Context, paths: Vec<String>, pointer: Option<(egui::Pos2, egui::ViewportId)>) {
        let paths: Vec<String> = paths.into_iter().filter(|p| std::path::Path::new(p).exists()).collect();
        if let Some(project) = paths.iter().find(|p| is_project_file(p)) {
            // one project is open at a time: the rest of the drop is not imported into either
            let _ = crate::menus::invoke(self, ctx, "file.open", json!({"path": project}));
            return;
        }
        if paths.is_empty() {
            return;
        }
        // A dropped folder becomes a bin named after it, with its sub-folders as bins inside it
        // (Premiere's behaviour): `mediaBrowser.import` walks it with the Media Browser's depth and
        // folder budgets. Plain files import exactly as before: `imageSequence: false` keeps
        // `file.import`'s single-file behaviour.
        let imported = self.session.execute("mediaBrowser.import", json!({"paths": paths, "bin": self.import_bin().0, "imageSequence": false}));
        // Say why a drop did not fully import (a folder without media, an unsupported file), as the
        // Import dialog does.
        match &imported {
            Ok(v) => {
                if let Some(errs) = v["errors"].as_array().filter(|e| !e.is_empty()) {
                    self.ui.status = errs.iter().filter_map(|e| e.as_str()).collect::<Vec<_>>().join("; ");
                } else if let Some(presets) = v["presets"].as_array().filter(|p| !p.is_empty()) {
                    // dropped preset files land in the Presets bin: open it and say how many
                    if !self.ui.expanded_fx.iter().any(|k| k == "Presets") {
                        self.ui.expanded_fx.push("Presets".to_string());
                    }
                    self.ui.status = tlf!("Imported {n} preset(s)", n = presets.len());
                }
            }
            Err(e) => self.ui.status = e.to_string(),
        }
        if let Ok(v) = imported
            && let Some((pos, viewport)) = pointer
            && self.session.prefs.timeline.drop_import_to_timeline
        {
            let items: Vec<filmcraft_engine::project::ItemId> =
                v["items"].as_array().map(|a| a.iter().filter_map(|i| i.as_u64()).map(filmcraft_engine::project::ItemId).collect()).unwrap_or_default();
            if !items.is_empty() {
                self.pending_timeline_drop = Some((items, pos, viewport));
            }
        }
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
        if ctx.input(|i| i.pointer.any_down()) && (crate::framework::escape_pressed(ctx) || crate::framework::escape_arrived(ctx)) {
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

/// Whether a dropped file is a FilmCraft project (opened, not imported).
fn is_project_file(path: &str) -> bool {
    std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("fcproj"))
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
