//! The Prompter overlay on the Program monitor (#5260), driven through the control channel: off by
//! default, `view.prompter` shows it, it follows the playhead through the talking points and from
//! section to section as `prompter.inspect` reports them, its text is on an accessibility node,
//! and it stays along the bottom of the picture, also in full-screen Program. Next and Previous
//! (#5262) move it off the playhead, with an ahead/behind readout, until it returns to the
//! playhead or playback starts.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the window with wgpu and write
//! `prompter-*.png` there, with the prompter's accessibility node beside each as `*.a11y.txt`.

use crate::support::{Drive, Launch, builder, demo_session, driver, rect_of, save_settled_shot, shots_dir, with_shots};
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
        assert!(d.prompter().is_some(), "at {s} s the overlay is drawn");
        let label = d.label();
        let section = q["section"]["name"].as_str().unwrap_or_default().to_owned();
        assert!(!section.is_empty(), "at {s} s the playhead is in a named section: {q}");
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
    // inset from the edge by the safe margin (5% of the picture's shorter side), so near it, not on it
    let gap = picture[1] + picture[3] - (band[1] + band[3]);
    assert!((0.0..picture[3] * 0.1).contains(&gap), "at its bottom edge: {gap} above it");
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

#[test]
fn the_accessibility_text_names_every_point_the_card_lists() {
    let mut d = Driver::open(demo_session(), with_shots(builder()));
    let rate = d.app().session.sequence_rate();
    let len = rate.frame_at(Tick(12 * TICKS_PER_SECOND));
    // six points sharing twelve seconds: two seconds each
    d.exec("markers.add", json!({"time": 0, "name": "Six", "comment": "- One\n- Two\n- Three\n- Four\n- Five\n- Six", "durationFrames": len}));
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": true}}));
    d.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}));
    d.at(3.0);
    let label = d.label();
    // full screen has room for the finished point, the current one, three next ones and the count
    for part in ["Done: One", "Now: Two", "Then: Three", "Then: Four", "Then: Five", "1 more"] {
        assert!(label.contains(part), "the card lists it, so the text says it ({part}): {label}");
    }
    assert!(!label.contains("Then: Six"), "a point the card only counts is counted, not named: {label}");
}

impl Driver {
    /// The text of the overlay's accessibility node (a Label), if there is one.
    fn node_text(&mut self) -> Option<String> {
        self.frames(2);
        let section = self.exec("prompter.inspect", json!({}))["section"]["name"].as_str().unwrap_or("No section").to_owned();
        self.harness.query_all_by_role(egui::accesskit::Role::Label).find_map(|n| {
            let node = n.accesskit_node();
            let text = node.label().or_else(|| node.value()).unwrap_or_default();
            text.starts_with(&section).then_some(text)
        })
    }

    /// The settled window as `<name>.png`, and the overlay's node beside it, when shots are on.
    fn shot(&mut self, name: &str) {
        save_settled_shot(self, &format!("{name}.png"));
        if let Some(dir) = shots_dir() {
            let text = self.node_text().unwrap_or_default();
            std::fs::write(dir.join(format!("{name}.a11y.txt")), format!("role: Label\nautomation id: program.prompter\ntext: {text}\n")).unwrap();
        }
    }

    fn point_at(&mut self, time: Option<i64>) -> String {
        let q = match time {
            Some(t) => self.exec("prompter.inspect", json!({"time": t})),
            None => self.exec("prompter.inspect", json!({})),
        };
        let i = q["section"]["current"].as_u64().unwrap_or(0) as usize;
        q["section"]["points"][i]["text"].as_str().unwrap_or_default().to_owned()
    }
}

#[test]
fn after_next_the_overlay_shows_the_prompter_point_with_an_ahead_or_behind_readout_until_it_returns() {
    let mut d = Driver::sections();
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": true}}));
    d.at(1.0);
    let playhead = d.exec("prompter.inspect", json!({}))["playhead"].as_i64();
    let at_playhead = d.point_at(playhead);
    assert!(!d.label().contains("ahead") && !d.label().contains("behind"), "no readout in step: {}", d.label());

    d.ok("ui.key", json!({"key": "Alt+PageDown"}));
    d.frames(3);
    let q = d.exec("prompter.inspect", json!({}));
    assert!(q["gap"].as_i64().unwrap_or(0) > 0, "the shortcut ran Next: {q}");
    let prompted = d.point_at(None);
    assert_ne!(prompted, at_playhead);
    let label = d.label();
    assert!(
        label.contains(&format!("Now: {prompted}")) && !label.contains(&format!("Now: {at_playhead}")),
        "the prompter's point, not the playhead's: {label}"
    );
    assert!(label.contains("ahead 0:0"), "ahead, in words: {label}");
    assert!(d.node_text().is_some_and(|t| t.contains("ahead 0:0")), "the readout is on the accessibility node");
    d.shot("prompter-ahead");

    d.exec("prompter.toPlayhead", json!({}));
    let label = d.label();
    assert!(!label.contains("ahead") && label.contains(&format!("Now: {at_playhead}")), "back on the playhead's point: {label}");
    d.shot("prompter-in-step");

    d.at(7.5);
    d.exec("prompter.previous", json!({}));
    d.exec("prompter.previous", json!({}));
    let label = d.label();
    assert!(label.contains("behind 0:0") && !label.contains("ahead"), "behind when the picture moved on: {label}");
    assert!(d.node_text().is_some_and(|t| t.contains("behind 0:0")));
    d.shot("prompter-behind");

    d.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}));
    d.exec("prompter.next", json!({}));
    d.exec("prompter.next", json!({}));
    d.exec("prompter.next", json!({}));
    assert!(d.label().starts_with("The numbers") && d.label().contains("ahead 0:0"), "into the next section, also in full-screen Program: {}", d.label());
    d.shot("prompter-full-screen-ahead");
}

#[test]
fn starting_playback_puts_the_prompter_back_in_step() {
    let mut d = Driver::sections();
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": true}}));
    d.at(1.0);
    d.exec("prompter.next", json!({}));
    assert!(d.label().contains("ahead"), "{}", d.label());
    d.exec("playback.toggle", json!({}));
    d.frames(3);
    assert!(d.app().playback.playing);
    assert_eq!(d.exec("prompter.inspect", json!({}))["gap"], 0, "in step once playback starts");
    assert!(!d.label().contains("ahead"), "and the readout is gone: {}", d.label());
    d.exec("prompter.next", json!({}));
    assert!(d.label().contains("ahead"), "Next still works while playing: {}", d.label());
    d.shot("prompter-playing-ahead");
    d.exec("playback.toggle", json!({}));
    d.frames(2);
    assert!(d.label().contains("ahead"), "stopping playback keeps the gap: {}", d.label());
}

#[test]
fn the_overlay_shows_the_rate_while_it_is_not_normal_and_the_readout_shows_the_gap_growing_or_closing() {
    let mut d = Driver::sections();
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": true}}));
    d.at(1.0);
    assert!(!d.label().contains("rate"), "no rate shown at the normal rate: {}", d.label());
    let playhead = d.app().session.playhead();
    let (project, undo) = (d.app().session.project.clone(), d.app().session.history.undo.len());

    // the shortcuts run the commands
    for _ in 0..4 {
        d.ok("ui.key", json!({"key": "Alt+Shift+PageDown"}));
    }
    d.frames(3);
    let q = d.exec("prompter.inspect", json!({}));
    assert!(q["rate"].as_i64().unwrap_or(100) > 100, "the shortcut ran Faster: {q}");
    assert_eq!(d.app().session.playhead(), playhead, "Faster does not move the playhead");
    assert!(d.label().contains("rate 120%"), "the overlay says the rate: {}", d.label());
    assert!(d.node_text().is_some_and(|t| t.contains("rate 120%")), "and so does its accessibility node");
    assert!(!d.label().contains("ahead") && !d.label().contains("behind"), "no gap yet, so no ahead/behind: {}", d.label());

    // five seconds on, the prompter has pulled ahead of the picture
    d.at(6.0);
    let label = d.label();
    assert!(label.contains("ahead 0:0") && label.contains("rate 120%"), "ahead, and the rate beside it: {label}");
    assert!(d.node_text().is_some_and(|t| t.contains("ahead 0:0") && t.contains("rate 120%")));
    d.shot("prompter-faster");

    // the gap closes at a slower rate
    d.exec("prompter.resetRate", json!({}));
    assert!(!d.label().contains("rate"), "the rate is gone after reset: {}", d.label());
    assert!(d.label().contains("ahead 0:0"), "the gap stays: {}", d.label());
    for _ in 0..6 {
        d.ok("ui.key", json!({"key": "Alt+Shift+PageUp"}));
    }
    assert!(d.label().contains("rate 70%"), "{}", d.label());
    let wide = d.exec("prompter.inspect", json!({}))["gap"].as_i64().unwrap_or(0);
    d.at(9.0);
    let narrow = d.exec("prompter.inspect", json!({}))["gap"].as_i64().unwrap_or(0);
    assert!(narrow < wide && narrow >= 0, "the gap is closing: {wide} then {narrow}");
    assert!(d.label().contains("rate 70%") && d.label().contains("ahead"), "still ahead, closing: {}", d.label());
    d.at(12.0);
    assert!(d.label().contains("behind 0:0"), "and opened the other way: {}", d.label());
    d.shot("prompter-slower");

    d.ok("ui.key", json!({"key": "Alt+Shift+Home"}));
    assert!(!d.label().contains("rate") && d.label().contains("behind"), "reset keeps the gap and drops the rate: {}", d.label());
    assert!(!d.node_text().unwrap_or_default().contains("rate"), "also on the accessibility node");
    assert_eq!(*d.app().session.project, *project, "no project change");
    assert_eq!(d.app().session.history.undo.len(), undo, "and no undo step");
}

#[test]
fn a_rate_does_not_change_playback_and_ends_when_playback_stops_or_starts() {
    let mut d = Driver::sections();
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": true}}));
    d.at(1.0);
    d.exec("playback.toggle", json!({}));
    d.frames(3);
    assert!(d.app().playback.playing);
    let (speed, audio_clock) = (d.app().playback.speed, d.app().playback.audio_clock);
    let before = d.app().session.playhead();
    for _ in 0..4 {
        d.exec("prompter.faster", json!({}));
    }
    d.frames(3);
    assert!(d.app().playback.playing, "the picture keeps playing");
    assert_eq!(d.app().playback.speed, speed, "at the speed it had");
    assert_eq!(d.app().playback.audio_clock, audio_clock, "on the same clock");
    assert!(d.app().session.playhead() >= before, "forward only, never skipped back or ahead by the rate");
    assert!(d.label().contains("rate 120%"), "{}", d.label());
    d.shot("prompter-playing-faster");

    // stopping ends the rate and keeps what the prompter had gained
    d.exec("playback.toggle", json!({}));
    d.frames(3);
    assert!(!d.app().playback.playing);
    let q = d.exec("prompter.inspect", json!({}));
    assert_eq!(q["rate"], 100, "stopping playback ends the rate: {q}");
    assert!(q["gap"].as_i64().unwrap_or(0) >= 0, "the gap is kept, not lost: {q}");
    assert!(!d.label().contains("rate"), "{}", d.label());

    // and the next start begins at the normal rate
    d.exec("prompter.faster", json!({}));
    d.exec("playback.toggle", json!({}));
    d.frames(3);
    let q = d.exec("prompter.inspect", json!({}));
    assert_eq!((q["rate"].as_i64(), q["gap"].as_i64()), (Some(100), Some(0)), "playback starts in step at the normal rate: {q}");
    d.exec("playback.toggle", json!({}));
}

#[test]
fn the_rate_is_shown_in_full_screen_program_too() {
    let mut d = Driver::sections();
    d.ok("ui.menu.invoke", json!({"id": "view.prompter", "params": {"enabled": true}}));
    d.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}));
    d.at(1.0);
    for _ in 0..4 {
        d.exec("prompter.faster", json!({}));
    }
    d.at(6.0);
    assert!(d.label().contains("rate 120%") && d.label().contains("ahead"), "{}", d.label());
    assert!(d.node_text().is_some_and(|t| t.contains("rate 120%") && t.contains("ahead")));
    d.shot("prompter-full-screen-faster");
    for _ in 0..10 {
        d.exec("prompter.slower", json!({}));
    }
    d.at(10.0);
    assert!(d.label().contains("rate 70%"), "{}", d.label());
    d.shot("prompter-full-screen-slower");
}
