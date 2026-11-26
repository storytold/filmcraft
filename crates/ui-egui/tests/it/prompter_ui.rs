//! The Prompter overlay on the Program monitor (#5260), driven through the control channel: off by
//! default, `view.prompter` shows it, it follows the playhead through the talking points and from
//! section to section as `prompter.inspect` reports them, its text is on an accessibility node,
//! and it stays along the bottom of the picture, also in full-screen Program.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the window with wgpu and write
//! `prompter-*.png` there.

use crate::support::{Drive, Launch, builder, demo_session, driver, rect_of, save_settled_shot, with_shots};
use egui_kittest::kittest::{NodeT, Queryable};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

driver!(Driver);

impl Driver {
    /// The demo with three sections: talking points with and without durations, a script, and points
    /// that need more time than their section.
    fn sections() -> Self {
        let mut d = Self::open(demo_session(), with_shots(builder()));
        let rate = d.app().session.sequence_rate();
        let frames = |s: i64| rate.frame_at(Tick(s * TICKS_PER_SECOND));
        for (at, len, name, comment) in [
            (0, 8, "Opening", "- Say hello [0:03]\n- Introduce the topic\n- Show the first chart [0:02]"),
            (
                8,
                10,
                "The numbers",
                "Sales grew every quarter this year, and the second half carried most of it. The chart on screen shows each region, starting with the north.",
            ),
            (18, 6, "Wrap up", "- Thank the team [0:04]\n- Point to the website [0:04]"),
        ] {
            d.exec("markers.add", json!({"time": rate.tick_of(frames(at)).0, "name": name, "comment": comment, "durationFrames": frames(len)}));
        }
        d
    }

    fn at(&mut self, seconds: f64) {
        self.exec("playhead.set", json!({"seconds": seconds}));
        self.frames(3);
    }

    /// The overlay's automation element, if it is drawn.
    fn prompter(&mut self) -> Option<Value> {
        self.frames(2);
        self.find_element("program.prompter")
    }

    fn label(&mut self) -> String {
        self.prompter().and_then(|e| e["label"].as_str().map(str::to_owned)).unwrap_or_default()
    }
}

#[test]
fn the_prompter_is_off_by_default_and_the_view_command_toggles_it() {
    let mut d = Driver::sections();
    d.at(1.0);
    let before = (d.app().session.project.clone(), d.app().session.history.undo.len());
    assert!(d.prompter().is_none(), "off by default");
    assert_eq!(d.ok("ui.menu.invoke", json!({"id": "view.prompter"}))["prompter"], true);
    assert!(d.label().starts_with("Opening"), "on, showing the section at the playhead: {}", d.label());
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": false}}));
    assert!(d.prompter().is_none(), "and off again");
    let after = (d.app().session.project.clone(), d.app().session.history.undo.len());
    assert!(*after.0 == *before.0 && after.1 == before.1, "toggling and showing it never changes the project or its history");
}

#[test]
fn the_prompter_follows_the_playhead_through_points_and_sections() {
    let mut d = Driver::sections();
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": true}}));
    let mut shown = Vec::new();
    for s in [1.0, 4.0, 7.5, 9.0, 16.5, 19.0] {
        d.at(s);
        let q = d.exec("prompter.inspect", json!({}));
        let label = d.label();
        let section = q["section"]["name"].as_str().unwrap_or_default().to_owned();
        assert!(label.starts_with(&section), "at {s} s the overlay shows the section the engine reports ({section}): {label}");
        if let Some(i) = q["section"]["current"].as_u64() {
            let point = q["section"]["points"][i as usize]["text"].as_str().unwrap_or_default();
            assert!(label.contains(&format!("Now: {point}")), "at {s} s the current point is {point}: {label}");
        }
        if q["section"]["script"].is_object() {
            assert!(label.contains("Script: "), "at {s} s the script is shown: {label}");
        }
        assert_eq!(label.contains("Next: "), q["next"]["soon"] == true, "at {s} s the next section is named only near the end: {label}");
        shown.push((section, q["section"]["current"].as_u64()));
    }
    assert_eq!(shown[0], ("Opening".to_owned(), Some(0)));
    assert_eq!(shown[1], ("Opening".to_owned(), Some(1)), "the second point after the first one's three seconds");
    assert_eq!(shown[2], ("Opening".to_owned(), Some(2)));
    assert_eq!(shown[3].0, "The numbers", "the next section at its start");
    assert_eq!(shown[5].0, "Wrap up");
    save_settled_shot(&mut d, "prompter-points.png");
    d.at(12.0);
    save_settled_shot(&mut d, "prompter-script.png");

    // the overlay's text is on an accessibility node, not only painted
    d.at(4.0);
    let node = d.harness.query_by_label_contains("Now: Introduce the topic").map(|n| (n.accesskit_node().role(), n.accesskit_node().label()));
    assert!(node.as_ref().is_some_and(|(role, _)| *role == egui::accesskit::Role::Label), "a label node carries the prompter's text: {node:?}");
}

#[test]
fn the_prompter_runs_along_the_bottom_of_the_picture_and_stays_in_full_screen_program() {
    let mut d = Driver::sections();
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": true}}));
    d.ok("ui.menu.invoke", json!({"id": "view.safeMargins", "params": {"enabled": true}}));
    d.at(16.5);
    let picture = rect_of(&d.find_element("program.picture").expect("the Program picture"));
    let band = rect_of(&d.prompter().expect("the prompter"));
    let inside = |r: [f64; 4], o: [f64; 4]| r[0] >= o[0] - 0.5 && r[1] >= o[1] - 0.5 && r[0] + r[2] <= o[0] + o[2] + 0.5 && r[1] + r[3] <= o[1] + o[3] + 0.5;
    assert!(inside(band, picture), "inside the picture: {band:?} in {picture:?}");
    assert!((band[1] + band[3] - (picture[1] + picture[3])).abs() < 1.0, "along its bottom edge");
    assert!(band[3] < picture[3] / 2.0, "leaving most of the picture clear");

    let cinema = d.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}));
    assert_eq!(cinema["fullScreenProgram"], true);
    assert!(d.app().ui.program.safe_margins, "the saved setting stays");
    let full = d.prompter().expect("the prompter stays in full-screen Program, where the other overlays are off");
    let window = d.harness.ctx.content_rect();
    let full = rect_of(&full);
    assert!(full[2] > band[2], "it grows with the picture: {full:?} after {band:?}");
    assert!(full[1] + full[3] <= f64::from(window.max.y) + 0.5 && full[3] < f64::from(window.height()) / 2.0, "still a band at the bottom: {full:?}");
    assert!(d.label().contains("Next: Wrap up"), "with the same content: {}", d.label());
    save_settled_shot(&mut d, "prompter-full-screen.png");
}

#[test]
fn points_longer_than_their_section_are_reported_on_the_overlay() {
    let mut d = Driver::sections();
    let rate = d.app().session.sequence_rate();
    let start = rate.tick_of(rate.frame_at(Tick(24 * TICKS_PER_SECOND)));
    d.exec(
        "markers.add",
        json!({"time": start.0, "name": "Too much", "comment": "- One [0:05]\n- Two [0:05]", "durationFrames": rate.frame_at(Tick(4 * TICKS_PER_SECOND))}),
    );
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": true}}));
    d.at(25.0);
    assert!(d.label().contains("past the end of the section"), "the overrun is said, not hidden: {}", d.label());
    d.at(1.0);
    assert!(!d.label().contains("past the end"), "and only where it happens: {}", d.label());
}
