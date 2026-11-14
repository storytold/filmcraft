use super::{HashMap, Mutex};

/// `--control-input-only`: drop the window system's input. Screenshot results arrive through
/// `ScreenshotCallback`, not raw input; all viewport lifecycle and geometry data stay.
pub(crate) fn drop_native_input(raw_input: &mut egui::RawInput) {
    raw_input.events.clear();
    raw_input.hovered_files.clear();
    raw_input.dropped_files.clear();
}

/// The main window's input rewrite ([`eframe::App::raw_input_hook`]) for a dock panel in its
/// own OS window, which eframe does not run the hook for: macOS Control+Click becomes a
/// secondary click, and `--control-input-only` drops the window system's input. Control
/// gestures go to the main window only, so nothing is added.
pub(crate) fn rewrite_native_window_input(
    control_input_only: bool,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] armed: &Mutex<HashMap<egui::ViewportId, bool>>,
    raw_input: &mut egui::RawInput,
) {
    if control_input_only {
        drop_native_input(raw_input);
    } else {
        #[cfg(target_os = "macos")]
        synthesize_mac_ctrl_click(armed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(raw_input.viewport_id).or_default(), raw_input);
    }
}

/// On macOS a Control+Click means "right click": AppKit delivers it to the view as a *primary*
/// press carrying the Control modifier (`rightMouseDown:` only fires for a real right button or
/// two-finger click), so the app translates it to a secondary click before egui computes pointer
/// state. `armed` pairs the press with its release, so the release is still a context click when
/// Control is let go mid-click. Called from [`eframe::App::raw_input_hook`], which runs before
/// egui's pass; only wired in on macOS, where Ctrl+Click is not a selection modifier.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn synthesize_mac_ctrl_click(armed: &mut bool, raw_input: &mut egui::RawInput) {
    for event in &mut raw_input.events {
        match event {
            egui::Event::PointerButton { button, pressed, modifiers, .. } if *button == egui::PointerButton::Primary => {
                if *pressed {
                    if modifiers.ctrl {
                        *button = egui::PointerButton::Secondary;
                        *armed = true;
                    } else {
                        // A plain primary press ends any pairing left open (eg. the pointer left
                        // the window mid-click, which egui also forgets).
                        *armed = false;
                    }
                } else if *armed {
                    *button = egui::PointerButton::Secondary;
                    *armed = false;
                }
            }
            egui::Event::PointerGone => *armed = false,
            _ => {}
        }
    }
}

#[cfg(test)]
mod mac_ctrl_click_tests {
    use egui::{Event, Modifiers, PointerButton, RawInput};

    fn click(button: PointerButton, pressed: bool, ctrl: bool) -> Event {
        Event::PointerButton { pos: egui::pos2(40.0, 12.0), button, pressed, modifiers: if ctrl { Modifiers::CTRL } else { Modifiers::NONE } }
    }

    fn buttons(raw: &mut RawInput) -> Vec<(PointerButton, bool)> {
        raw.events
            .iter()
            .filter_map(|e| match e {
                Event::PointerButton { button, pressed, .. } => Some((*button, *pressed)),
                _ => None,
            })
            .collect()
    }

    /// AppKit delivers Control+Click as a primary press with the Control modifier; it must reach
    /// egui as a secondary click so `secondary_clicked()` (the context menus) fires on macOS.
    #[test]
    fn ctrl_click_is_a_secondary_click() {
        let mut raw = RawInput { events: vec![click(PointerButton::Primary, true, true), click(PointerButton::Primary, false, true)], ..Default::default() };
        let mut armed = false;
        super::synthesize_mac_ctrl_click(&mut armed, &mut raw);
        assert!(!armed);
        assert_eq!(buttons(&mut raw), vec![(PointerButton::Secondary, true), (PointerButton::Secondary, false)]);
    }

    /// End to end: the rewritten events are what egui's pointer state sees, so the context
    /// menus (`secondary_clicked()`) fire on a Control+Click exactly like on a real right click.
    #[test]
    fn synthesized_click_fires_secondary_clicked() {
        let ctx = egui::Context::default();
        let mut raw = RawInput { events: vec![click(PointerButton::Primary, true, true), click(PointerButton::Primary, false, true)], ..Default::default() };
        let mut armed = false;
        super::synthesize_mac_ctrl_click(&mut armed, &mut raw);
        let mut secondary = false;
        // No renderer here: drop the pass output, clearing its texture deltas so epaint's drop
        // guard does not panic on the font texture.
        let mut out = ctx.run_ui(raw, |ui| secondary = ui.ctx().input(|i| i.pointer.secondary_clicked()));
        out.textures_delta.clear();
        assert!(secondary);
    }

    /// Releasing Control before the mouse button still closes the click as a context click; the
    /// mouse release alone must not become a stray primary click.
    #[test]
    fn release_is_paired_to_the_ctrl_press() {
        let mut raw = RawInput {
            events: vec![
                click(PointerButton::Primary, true, true),
                click(PointerButton::Primary, false, false), // Control already released
            ],
            ..Default::default()
        };
        let mut armed = false;
        super::synthesize_mac_ctrl_click(&mut armed, &mut raw);
        assert!(!armed);
        assert_eq!(buttons(&mut raw), vec![(PointerButton::Secondary, true), (PointerButton::Secondary, false)]);
    }

    /// A plain click keeps its meaning (on macOS Cmd is the selection modifier, not Control).
    #[test]
    fn plain_click_is_untouched() {
        let mut raw = RawInput { events: vec![click(PointerButton::Primary, true, false), click(PointerButton::Primary, false, false)], ..Default::default() };
        super::synthesize_mac_ctrl_click(&mut false, &mut raw);
        assert_eq!(buttons(&mut raw), vec![(PointerButton::Primary, true), (PointerButton::Primary, false)]);
    }

    /// A half-open pairing (pointer left the window mid-click) must not turn the next plain click
    /// into a context click.
    #[test]
    fn new_plain_press_clears_a_stale_pairing() {
        let mut raw = RawInput {
            events: vec![
                click(PointerButton::Primary, true, true),
                Event::PointerGone,
                click(PointerButton::Primary, true, false),
                click(PointerButton::Primary, false, false),
            ],
            ..Default::default()
        };
        let mut armed = false;
        super::synthesize_mac_ctrl_click(&mut armed, &mut raw);
        assert!(!armed);
        assert_eq!(buttons(&mut raw), vec![(PointerButton::Secondary, true), (PointerButton::Primary, true), (PointerButton::Primary, false),]);
    }
}

#[cfg(test)]
mod native_window_input_tests {
    use super::*;
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
        let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(s));
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

    /// `--control-input-only` drops a native window's own input as the main window's hook does.
    #[test]
    fn control_input_only_drops_native_window_input() {
        let armed = Mutex::new(HashMap::new());
        let mut raw = egui::RawInput { viewport_id: egui::ViewportId::from_hash_of("palette"), ..Default::default() };
        raw.events = vec![egui::Event::Text("typing".into()), egui::Event::PointerMoved(egui::pos2(4.0, 4.0))];
        raw.hovered_files.push(Default::default());
        raw.dropped_files.push(Arc::new(OsFile(PathBuf::from("/unrelated-input.txt"))));
        let mut kept = raw.clone();
        rewrite_native_window_input(false, &armed, &mut kept);
        assert_eq!(kept.events.len(), 2);
        assert_eq!(kept.dropped_files.len(), 1);
        rewrite_native_window_input(true, &armed, &mut raw);
        assert!(raw.events.is_empty() && raw.hovered_files.is_empty() && raw.dropped_files.is_empty());
    }
}
