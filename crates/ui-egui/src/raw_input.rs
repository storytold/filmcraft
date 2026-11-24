/// `--control-input-only`: drop the main window's input from the window system. Screenshot
/// results arrive through `ScreenshotCallback`, not raw input; all viewport lifecycle and
/// geometry data stay. The runtime drops the input of the OS windows it hosts
/// (`WindowInput::Dropped`, set in [`crate::framework::before_input`]).
pub(crate) fn drop_native_input(raw_input: &mut egui::RawInput) {
    raw_input.events.clear();
    raw_input.hovered_files.clear();
    raw_input.dropped_files.clear();
}

#[cfg(test)]
mod native_window_input_tests {
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use crate::{FilmcraftApp, PanelKind};
    use craft_ui::docking::WindowFiles;
    use egui_kittest::Harness;
    use filmcraft_engine::Session;

    #[derive(Debug)]
    struct OsFile(PathBuf);

    impl egui::DroppedFile for OsFile {
        fn path(&self) -> &Path {
            &self.0
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            std::fs::read(&self.0).map_err(|e| e.to_string())
        }
    }

    fn step(h: &mut Harness<'static, FilmcraftApp>) {
        let ctx = h.ctx.clone();
        let mut raw = std::mem::take(h.input_mut());
        eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
        *h.input_mut() = raw;
        ctx.request_repaint();
        h.step();
    }

    fn clips(h: &Harness<'static, FilmcraftApp>) -> usize {
        h.state().session.active_sequence().map_or(0, |q| q.video_tracks.iter().map(|t| t.items.len()).sum())
    }

    /// Files dropped on a dock panel in its own OS window are imported; on the Timeline they are
    /// placed at the pointer, by the Timeline drawn in that window only.
    #[test]
    fn files_dropped_on_native_windows_import_and_reach_their_timeline() {
        let dir = tempfile::tempdir().unwrap();
        let still = dir.path().join("native_window_drop.png");
        image::RgbaImage::from_pixel(64, 36, image::Rgba([40, 140, 220, 255])).save(&still).unwrap();
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        s.execute("file.newSequence", json!({"name": "Drop test", "width": 1280, "height": 720, "fps": 24})).unwrap();
        let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |cc| {
            crate::framework::install_headless(&cc.egui_ctx);
            FilmcraftApp::new(s)
        });
        for _ in 0..4 {
            step(&mut h);
        }
        let row = h.state().auto.find("timeline.track.V1.target").unwrap().rect;
        let panel = h.state().auto.find("panel.Timeline").unwrap().rect;
        let at = egui::pos2(panel[0] + h.state().ui.timeline.header_w + 60.0, row[1] + row[3] / 2.0);
        let drop_files = |h: &mut Harness<'static, FilmcraftApp>, panel: PanelKind, viewport: egui::ViewportId| {
            let files = WindowFiles { window: 1, viewport, panel, pos: Some(at), hovered: Vec::new(), dropped: vec![Arc::new(OsFile(still.clone()))] };
            h.state_mut().window_files = vec![files];
            step(h);
            step(h);
        };
        let items = h.state().session.project.items.len();
        // another window's Timeline: imported, and the main window's Timeline leaves it
        drop_files(&mut h, PanelKind::Timeline, egui::ViewportId::from_hash_of("another window"));
        assert_eq!((h.state().session.project.items.len(), clips(&h)), (items + 1, 0), "status: {}", h.state().ui.status);
        // a panel other than the Timeline: imported to the bin only
        drop_files(&mut h, PanelKind::Project, egui::ViewportId::ROOT);
        assert_eq!((h.state().session.project.items.len(), clips(&h)), (items + 2, 0), "status: {}", h.state().ui.status);
        // the Timeline drawn in the window the files were dropped on places them
        drop_files(&mut h, PanelKind::Timeline, egui::ViewportId::ROOT);
        assert_eq!((h.state().session.project.items.len(), clips(&h)), (items + 3, 1), "status: {}", h.state().ui.status);
    }
}
