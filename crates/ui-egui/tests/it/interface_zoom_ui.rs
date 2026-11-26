//! Headless UI tests of the interface zoom (#2253).

use crate::support::{Drive, Launch, builder, demo_session, driver};
use serde_json::json;

driver!(Driver);

impl Driver {
    fn demo() -> Self {
        Self::open(demo_session(), builder().with_step_dt(1.0 / 60.0))
    }
    /// One key press with `modifiers`, as the window reports it.
    fn press(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        let events = &mut self.harness.input_mut().events;
        events.push(egui::Event::ModifiersChanged(modifiers));
        for pressed in [true, false] {
            events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
        }
        events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        self.frames(3);
    }
}

/// #2253: with the Timeline focused, Ctrl+= and Ctrl+- are its track-height keys, but Ctrl+Plus
/// (the `+` key, or Shift+=) fell through to egui and scaled the whole interface, which the taken
/// Ctrl+- and Ctrl+0 could not undo. `+` now goes where `=` goes, and View ▸ Reset Interface Zoom
/// gives the configured scale back.
#[test]
fn ctrl_plus_does_what_ctrl_equals_does_and_the_interface_zoom_can_be_reset() {
    let mut d = Driver::demo();
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.click(&format!("timeline.clip.{clip}"));
    assert_eq!(d.app().ui.focused, filmcraft_ui_egui::dock::PanelKind::Timeline);
    let zoom = d.harness.ctx.zoom_factor();
    let height = d.app().ui.timeline.video_track_h;
    d.press(egui::Key::Plus, egui::Modifiers::COMMAND);
    assert_eq!(d.harness.ctx.zoom_factor(), zoom, "the interface keeps its scale");
    assert!(d.app().ui.timeline.video_track_h > height, "Ctrl+Plus makes the video tracks taller, as Ctrl+= does");

    // however it got scaled, View ▸ Reset Interface Zoom brings it back
    d.harness.ctx.set_zoom_factor(1.75);
    d.frames(2);
    assert_eq!(d.harness.ctx.zoom_factor(), 1.75);
    let r = d.ok("ui.menu.invoke", json!({"id": "view.resetInterfaceZoom"}));
    assert_eq!(r["zoom"], json!(1.0), "the reply says the zoom it set: {r}");
    d.frames(2);
    assert_eq!(d.harness.ctx.zoom_factor(), 1.0);
}
