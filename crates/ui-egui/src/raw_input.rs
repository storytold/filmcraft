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

    use crate::FilmcraftApp;
    use egui_kittest::Harness;
    use filmcraft_engine::Session;

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

    /// The dock's fallback reports a native-window drop without a Timeline target. Those files
    /// are imported to the bin but are not placed on the Timeline.
    #[test]
    fn files_dropped_on_native_windows_import_to_the_bin() {
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
        let items = h.state().session.project.items.len();
        let mut file = craft_ui::runtime::drag::OsFile::default();
        file.path = Some(still.clone());
        h.state_mut().window_files = vec![file];
        step(&mut h);
        step(&mut h);
        assert_eq!((h.state().session.project.items.len(), clips(&h)), (items + 1, 0), "status: {}", h.state().ui.status);
    }
}
