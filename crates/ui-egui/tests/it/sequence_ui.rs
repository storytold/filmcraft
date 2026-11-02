//! Headless UI tests of the Sequence / Markers menu additions: the Add Tracks and Delete Tracks dialogs, the
//! through-edit marks and duplicate frame markers on the timeline, the Markers panel colour filter and the Shift+; gap key.
//! The real `FilmcraftApp` under `egui_kittest`, driven over the control channel by automation id.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `sequence-settings-*.png` there; without it no GPU is needed.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

#[test]
fn marker_text_menu_commands_belong_to_each_field_not_the_timeline() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let marker = d.exec("markers.addRange", json!({"frame":30,"durationFrames":24}))["marker"].as_u64().unwrap();
    d.exec("markers.edit", json!({"marker":marker,"kind":"webLink","url":"https://example.com"}));
    d.double_click(&format!("timeline.marker.{marker}"));
    let before = d.exec("sequence.inspect", json!({}));
    let project = d.app().session.project.to_json();
    let history = d.app().session.history.undo.len();
    for field in ["name", "author", "comment", "time", "duration", "url"] {
        let id = format!("markerProperties.{field}");
        let value = if matches!(field, "time" | "duration") { "01:02:03:04" } else { "Clipboard αβ" };
        d.set_text(&id, value);
        let menu = d.ok("ui.menu.list", json!({}));
        for command in ["edit.copy", "edit.cut", "edit.paste", "edit.selectAll", "edit.undo", "edit.redo"] {
            assert_eq!(menu.as_array().unwrap().iter().find(|item| item["id"] == command).unwrap()["enabled"], true);
        }
        d.ok("ui.menu.invoke", json!({"id":"edit.selectAll"}));
        assert!(d.exec("sequence.inspect", json!({})) == before, "Select All in {field} must not select clips");
        d.ok("ui.menu.invoke", json!({"id":"edit.copy"}));
        // Main now forwards Copy/Cut directly as text events, including native floating windows.
        d.frames(1);
        assert!(d.harness.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(s) if s == value)), "Copy {field}");
        d.ok("ui.menu.invoke", json!({"id":"edit.cut"}));
        d.frames(1);
        assert_eq!(marker_draft_text(&d, field), "");
        d.ok("ui.menu.invoke", json!({"id":"edit.paste"}));
        assert!(d.harness.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(&egui::ViewportCommand::RequestPaste)));
        d.harness.input_mut().events.push(egui::Event::Paste(value.into()));
        d.frames(2);
        assert_eq!(marker_draft_text(&d, field), value);
        d.ok("ui.menu.invoke", json!({"id":"edit.undo"}));
        d.frames(3);
        assert_ne!(marker_draft_text(&d, field), value, "text Undo {field}");
        d.ok("ui.menu.invoke", json!({"id":"edit.redo"}));
        d.frames(3);
        assert_eq!(marker_draft_text(&d, field), value);
        assert_eq!(d.app().session.project.to_json(), project);
        assert_eq!(d.app().session.history.undo.len(), history);
    }
    d.click("markerProperties.cancel");
    assert_eq!(d.exec("sequence.inspect", json!({})), before);
}

fn marker_draft_text<'a>(d: &'a Driver, field: &str) -> &'a str {
    let m = d.harness.state().ui.marker_properties.as_ref().unwrap();
    match field {
        "name" => &m.name,
        "author" => &m.author,
        "comment" => &m.comment,
        "time" => &m.time,
        "duration" => &m.duration,
        "url" => &m.url,
        _ => panic!("field"),
    }
}

#[test]
fn high_frame_rate_marker_duration_starts_at_one_valid_frame() {
    let mut d = Driver::demo();
    d.exec("file.newSequence", json!({"name":"120 fps", "fps":120}));
    let marker = d.exec("markers.add", json!({"frame":0}))["marker"].as_u64().unwrap();
    d.double_click(&format!("timeline.marker.{marker}"));
    assert_eq!(marker_draft_text(&d, "time"), "00:00:00:000");
    d.click("markerProperties.range");
    assert_eq!(marker_draft_text(&d, "duration"), "00:00:00:001");
    assert_eq!(d.ok("ui.elements", json!({"prefix":"markerProperties.apply"}))[0]["enabled"], true);
    d.click("markerProperties.apply");
    assert_eq!(d.app().session.active_sequence().unwrap().markers[0].duration, filmcraft_time::FrameRate::FPS_120.tick_of(1));
}

#[test]
fn marker_timecodes_fit_inside_their_paint_clip() {
    fn check(shape: &egui::Shape, clip: egui::Rect, field: egui::Rect, expected: &str, found: &mut bool) {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == expected => {
                let bounds = text.galley.rect.translate(text.pos.to_vec2());
                if field.intersects(bounds) {
                    *found = true;
                    assert!(clip.expand(0.5).contains_rect(bounds), "{expected}: text {bounds:?} is clipped by {clip:?}");
                }
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| check(shape, clip, field, expected, found)),
            _ => {}
        }
    }
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let marker = d.exec("markers.addRange", json!({"frame":30,"durationFrames":24}))["marker"].as_u64().unwrap();
    d.double_click(&format!("timeline.marker.{marker}"));
    let elements = d.ok("ui.elements", json!({"prefix":"markerProperties."}));
    for field in ["time", "duration"] {
        let expected = marker_draft_text(&d, field).to_owned();
        let entry = elements.as_array().unwrap().iter().find(|e| e["id"] == format!("markerProperties.{field}")).unwrap();
        let r = &entry["rect"];
        let rect = egui::Rect::from_min_size(
            egui::pos2(r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32),
            egui::vec2(r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32),
        );
        let mut found = false;
        for clipped in &d.harness.output().shapes {
            check(&clipped.shape, clipped.clip_rect, rect, &expected, &mut found);
        }
        assert!(found, "{field}: timecode text is drawn");
    }
}

#[test]
fn marker_notes_scroll_and_timecode_fields_require_complete_valid_values() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let marker = d.exec("markers.addRange", json!({"frame":30,"durationFrames":24}))["marker"].as_u64().unwrap();
    d.double_click(&format!("timeline.marker.{marker}"));
    let rect = |v: &Value, id: &str| v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap()["rect"].clone();
    let before = d.ok("ui.elements", json!({"prefix":"markerProperties."}));
    d.set_text("markerProperties.comment", &"Long note line\n".repeat(200));
    let after = d.ok("ui.elements", json!({"prefix":"markerProperties."}));
    assert_eq!(rect(&before, "markerProperties.apply"), rect(&after, "markerProperties.apply"), "Notes must not resize or move the dialog");
    assert!(rect(&after, "markerProperties.comment")[3].as_f64().unwrap() <= 130.0);
    d.set_text("markerProperties.time", "a00:b00:c00:d01");
    assert_eq!(marker_draft_text(&d, "time"), "00:00:00:01");
    for field in ["time", "duration"] {
        for invalid in ["", "00:00:0:01", "00:60:00:00", "00:00:60:00", "00:00:00:99"] {
            d.set_text(&format!("markerProperties.{field}"), invalid);
            let v = d.ok("ui.elements", json!({"prefix":"markerProperties.apply"}));
            assert_eq!(v[0]["enabled"], false, "{field}: {invalid}");
        }
        d.set_text(&format!("markerProperties.{field}"), "00:00:00:01");
    }
    assert_eq!(d.ok("ui.elements", json!({"prefix":"markerProperties.apply"}))[0]["enabled"], true);
    d.snapshot("bounded-marker-notes", "markerProperties.");
    d.click("markerProperties.cancel");
}

#[test]
fn marker_navigation_saves_one_edit_skips_noops_and_orders_coincident_markers() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let last = d.exec("markers.add", json!({"frame":90}))["marker"].as_u64().unwrap();
    let first = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    let second = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    let before = d.app().session.project.to_json();
    let history = d.app().session.history.undo.len();
    let owner = filmcraft_ui_egui::panels::marker_properties::Owner::current(d.app(), false).unwrap();
    let marker = owner.details(d.app()).unwrap().3.into_iter().find(|m| m.id.0 == first).unwrap();
    filmcraft_ui_egui::panels::marker_properties::open_owned(d.app(), &marker, owner);
    d.frames(3);
    let previous = d.ok("ui.elements", json!({"prefix":"markerProperties.previous"}));
    assert_eq!(previous[0]["enabled"], false);
    d.set_text("markerProperties.name", "Reviewed");
    d.set_text("markerProperties.author", "BENJI");
    d.set_text("markerProperties.comment", "Keep this note");
    d.click("markerProperties.preset.Mango");
    d.click("markerProperties.kind.chapter");
    d.snapshot("marker-editor-navigation", "markerProperties.");
    d.click("markerProperties.next");
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().marker, second);
    let marker = d.app().session.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == first).unwrap().clone();
    assert_eq!((&*marker.name, &*marker.author, &*marker.comment), ("Reviewed", "BENJI", "Keep this note"));
    assert_eq!(marker.color, filmcraft_project::Label::Mango);
    assert_eq!(marker.kind, filmcraft_project::MarkerKind::Chapter);
    assert_eq!(d.app().session.history.undo.len(), history + 1);
    let saved = d.app().session.project.to_json();
    d.click("markerProperties.previous");
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().marker, first);
    d.click("markerProperties.next");
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().marker, second);
    assert_eq!(d.app().session.history.undo.len(), history + 1);
    d.set_text("markerProperties.comment", "Discard this draft");
    d.click("markerProperties.cancel");
    assert_eq!(d.app().session.project.to_json(), saved);
    d.double_click(&format!("timeline.marker.{last}"));
    let next = d.ok("ui.elements", json!({"prefix":"markerProperties.next"}));
    assert_eq!(next[0]["enabled"], false);
    d.click("markerProperties.apply");
    assert_eq!(d.app().session.history.undo.len(), history + 1);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), before);
    d.exec("edit.redo", json!({}));
    assert_eq!(d.app().session.project.to_json(), saved);
}

#[test]
fn marker_navigation_rejects_invalid_drafts_without_moving_or_mutating() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let first = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    let second = d.exec("markers.add", json!({"frame":90}))["marker"].as_u64().unwrap();
    d.double_click(&format!("timeline.marker.{first}"));
    let before = d.app().session.project.to_json();
    let history = d.app().session.history.undo.len();
    let time = d.app().ui.marker_properties.as_ref().unwrap().time.clone();
    for (field, invalid, reset) in [("time", "invalid".to_owned(), time), ("author", "x".repeat(1025), String::new())] {
        d.set_text(&format!("markerProperties.{field}"), &invalid);
        d.click("markerProperties.next");
        let draft = d.app().ui.marker_properties.as_ref().unwrap();
        assert_eq!(draft.marker, first);
        assert!(!draft.error.is_empty());
        assert_eq!(d.app().session.project.to_json(), before);
        assert_eq!(d.app().session.history.undo.len(), history);
        d.set_text(&format!("markerProperties.{field}"), &reset);
    }
    d.click("markerProperties.kind.webLink");
    d.set_text("markerProperties.url", "file:///tmp/test");
    d.click("markerProperties.next");
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().marker, first);
    assert_eq!(d.app().session.project.to_json(), before);
    d.set_text("markerProperties.url", "");
    d.click("markerProperties.kind.comment");
    d.click("markerProperties.range");
    d.set_text("markerProperties.duration", "00:00:00:00");
    d.click("markerProperties.next");
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().marker, first);
    assert_eq!(d.app().session.project.to_json(), before);
    d.click("markerProperties.range");
    assert!(!d.app().ui.marker_properties.as_ref().unwrap().range);
    d.click("markerProperties.next");
    let draft = d.app().ui.marker_properties.as_ref().unwrap();
    assert_eq!(draft.marker, second, "{draft:?}");
    assert_eq!(d.app().session.history.undo.len(), history);
}

#[test]
fn marker_navigation_keeps_the_original_source_owner_after_source_switch() {
    let mut d = Driver::demo();
    d.exec("source.open", json!({"item":5}));
    d.exec("markers.clearAll", json!({"target":"source"}));
    let first = d.exec("markers.add", json!({"target":"source","time":0}))["marker"].as_u64().unwrap();
    let second = d.exec("markers.add", json!({"target":"source","time":254016000000_i64}))["marker"].as_u64().unwrap();
    let owner = filmcraft_ui_egui::panels::marker_properties::Owner::Source(5);
    let marker = owner.details(d.app()).unwrap().3.into_iter().find(|m| m.id.0 == first).unwrap();
    filmcraft_ui_egui::panels::marker_properties::open_owned(d.app(), &marker, owner);
    d.frames(3);
    d.exec("source.open", json!({"item":6}));
    let unrelated = d.app().session.project.item(filmcraft_project::ItemId(6)).unwrap().clone();
    d.set_text("markerProperties.name", "Original source edit");
    d.click("markerProperties.next");
    let draft = d.app().ui.marker_properties.as_ref().unwrap();
    assert_eq!(draft.owner, owner);
    assert_eq!(draft.marker, second);
    assert_eq!(owner.details(d.app()).unwrap().3.iter().find(|m| m.id.0 == first).unwrap().name, "Original source edit");
    assert_eq!(d.app().session.project.item(filmcraft_project::ItemId(6)).unwrap(), &unrelated);
    assert_eq!(d.app().session.state.source_item, Some(filmcraft_project::ItemId(6)));
    d.click("markerProperties.cancel");
}

#[test]
fn marker_navigation_keeps_the_original_sequence_after_sequence_switch() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let first = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    let second = d.exec("markers.add", json!({"frame":90}))["marker"].as_u64().unwrap();
    let owner = filmcraft_ui_egui::panels::marker_properties::Owner::current(d.app(), false).unwrap();
    d.double_click(&format!("timeline.marker.{first}"));
    d.exec("file.newSequence", json!({"name":"Unrelated sequence"}));
    let active = d.app().session.state.active_sequence;
    let unrelated = d.app().session.active_sequence().unwrap().clone();
    d.set_text("markerProperties.name", "Original sequence edit");
    d.click("markerProperties.next");
    let draft = d.app().ui.marker_properties.as_ref().unwrap();
    assert_eq!(draft.owner, owner);
    assert_eq!(draft.marker, second);
    assert_eq!(owner.details(d.app()).unwrap().3.iter().find(|m| m.id.0 == first).unwrap().name, "Original sequence edit");
    assert_eq!(d.app().session.active_sequence().unwrap(), &unrelated);
    assert_eq!(d.app().session.state.active_sequence, active);
    d.click("markerProperties.cancel");
}

#[test]
fn marker_editor_wide_form_and_small_window_keep_actions_accessible() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let marker = d.exec("markers.add", json!({"frame":30,"name":"Review this moment","author":"BENJI"}))["marker"].as_u64().unwrap();
    d.double_click(&format!("timeline.marker.{marker}"));
    let rect = |elements: &Value, id: &str| -> Vec<f64> {
        elements.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap()["rect"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap()).collect()
    };
    let elements = d.ok("ui.elements", json!({"prefix":"markerProperties."}));
    let name = rect(&elements, "markerProperties.name");
    let author = rect(&elements, "markerProperties.author");
    let notes = rect(&elements, "markerProperties.comment");
    let done = rect(&elements, "markerProperties.apply");
    assert!(name[2] > 300.0 && name[2] < 400.0);
    assert_eq!(author[2], name[2]);
    assert!(author[1] > name[1] + name[3]);
    assert!(notes[3] > 70.0);
    assert!(done[0] > notes[0] + notes[2]);
    for kind in ["comment", "chapter", "segmentation", "webLink"] {
        assert!(!d.ids(&format!("markerProperties.kind.{kind}")).is_empty());
    }
    d.snapshot("marker-editor-wide-layout", "markerProperties.");
    for size in [egui::vec2(480.0, 600.0), egui::vec2(360.0, 600.0), egui::vec2(900.0, 500.0)] {
        d.harness.set_size(size);
        d.frames(8);
        let elements = d.ok("ui.elements", json!({"prefix":"markerProperties."}));
        for id in ["apply", "cancel", "previous", "next", "delete"] {
            let action = rect(&elements, &format!("markerProperties.{id}"));
            assert!(action[0] >= 0.0 && action[0] + action[2] <= f64::from(size.x), "{size:?} {id}: {action:?}");
            assert!(action[1] >= 0.0 && action[1] + action[3] <= f64::from(size.y), "{size:?} {id}: {action:?}");
        }
        d.snapshot(&format!("marker-editor-{}x{}", size.x, size.y), "markerProperties.");
    }
    d.click("markerProperties.cancel");
    assert!(d.app().ui.marker_properties.is_none());
}

#[test]
fn review_marker_double_click_over_work_area_preserves_project_and_history() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let point = d.exec("markers.add", json!({"frame":36}))["marker"].as_u64().unwrap();
    let range = d.exec("markers.addRange", json!({"frame":48,"durationFrames":24}))["marker"].as_u64().unwrap();
    let rate = d.app().session.sequence_rate();
    d.exec("sequence.setWorkArea", json!({"start":rate.tick_of(24).0,"end":rate.tick_of(120).0}));
    d.ok("ui.menu.invoke", json!({"id":"view.zoomToSequence"}));
    d.frames(60);
    let before = d.app().session.project.to_json();
    let area = d.app().session.active_sequence().unwrap().work_area;
    let history = d.app().session.history.undo.len();
    for (id, marker) in
        [(format!("timeline.marker.{point}"), point), (format!("timeline.marker.{range}"), range), (format!("timeline.markerEnd.{range}"), range)]
    {
        let elements = d.ok("ui.elements", json!({"prefix":id}));
        let rect = elements.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap()["rect"].as_array().unwrap();
        let pos = egui::pos2(
            (rect[0].as_f64().unwrap() + rect[2].as_f64().unwrap() / 2.0) as f32,
            (rect[1].as_f64().unwrap() + rect[3].as_f64().unwrap() - 0.5) as f32,
        );
        let hit = d.ok("ui.timeline.hit", json!({"x":pos.x,"y":pos.y}));
        assert_eq!(hit["hit"], "workArea", "the regression must exercise the overlapping hit rectangles");
        d.double_click_at(pos);
        assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().marker, marker);
        assert_eq!(d.app().session.active_sequence().unwrap().work_area, area, "opening marker properties must not change the work area");
        assert_eq!(d.app().session.history.undo.len(), history);
        d.snapshot("marker-work-area-overlap", "markerProperties.");
        d.click("markerProperties.cancel");
        assert_eq!(d.app().session.project.to_json(), before);
        assert_eq!(d.app().session.history.undo.len(), history);
    }
    let elements = d.ok("ui.elements", json!({"prefix":"timeline.workArea"}));
    let rect = elements.as_array().unwrap().iter().find(|e| e["id"] == "timeline.workArea").unwrap()["rect"].as_array().unwrap();
    let pos =
        egui::pos2((rect[0].as_f64().unwrap() + rect[2].as_f64().unwrap() / 2.0) as f32, (rect[1].as_f64().unwrap() + rect[3].as_f64().unwrap() / 2.0) as f32);
    d.double_click_at(pos);
    assert!(d.app().ui.marker_properties.is_none());
    assert_ne!(d.app().session.active_sequence().unwrap().work_area, area);
    assert_eq!(d.app().session.history.undo.len(), history + 1);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), before);
}

#[test]
fn web_link_editor_keeps_notes_and_opens_only_after_an_explicit_click() {
    use std::{cell::RefCell, rc::Rc};
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let marker = d.exec("markers.add", json!({"frame":30,"name":"Review","comment":"Original notes"}))["marker"].as_u64().unwrap();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let recorder = opened.clone();
    d.app().hooks.open_url = Some(Box::new(move |url| {
        recorder.borrow_mut().push(url.to_string());
        Ok(())
    }));
    let before = d.app().session.project.to_json();
    let history = d.app().session.history.undo.len();
    d.double_click(&format!("timeline.marker.{marker}"));
    assert!(d.ids("markerProperties.url").is_empty());
    d.click("markerProperties.kind.webLink");
    d.set_text("markerProperties.url", "https://example.com/review?take=2#notes");
    d.set_text("markerProperties.comment", "Awaiting producer approval");
    d.snapshot("web-link-marker-editor", "markerProperties.");
    assert_eq!(d.app().session.project.to_json(), before);
    assert!(opened.borrow().is_empty());
    d.click("markerProperties.openLink");
    assert_eq!(*opened.borrow(), ["https://example.com/review?take=2#notes"]);
    assert_eq!(d.app().session.project.to_json(), before);
    assert_eq!(d.app().session.history.undo.len(), history);
    d.click("markerProperties.kind.comment");
    assert!(d.ids("markerProperties.url").is_empty());
    d.click("markerProperties.kind.webLink");
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().url, "https://example.com/review?take=2#notes");
    d.click("markerProperties.cancel");
    assert_eq!(d.app().session.project.to_json(), before);
    d.double_click(&format!("timeline.marker.{marker}"));
    d.click("markerProperties.kind.webLink");
    d.set_text("markerProperties.url", "https://example.com/final");
    d.set_text("markerProperties.comment", "Keep notes separately");
    d.click("markerProperties.apply");
    let saved = d.app().session.active_sequence().unwrap().markers[0].clone();
    assert_eq!((&*saved.url, &*saved.comment), ("https://example.com/final", "Keep notes separately"));
    assert_eq!(d.app().session.history.undo.len(), history + 1);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), before);
    d.exec("edit.redo", json!({}));
    d.exec("playhead.set", json!({"frame":30}));
    d.frames(3);
    assert_eq!(opened.borrow().len(), 1, "editing, reopening, Undo/Redo and seeking must never launch a browser");
    d.double_click(&format!("timeline.marker.{marker}"));
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().url, saved.url);
    d.click("markerProperties.kind.comment");
    d.click("markerProperties.apply");
    assert_eq!(d.app().session.active_sequence().unwrap().markers[0].url, saved.url, "type changes must not discard the hidden URL");
}

#[test]
fn web_link_rejects_unsafe_addresses_and_preserves_draft_on_browser_failure() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let marker = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    d.double_click(&format!("timeline.marker.{marker}"));
    d.click("markerProperties.kind.webLink");
    let before = d.app().session.project.to_json();
    for bad in ["file:///tmp/test", "javascript:alert(1)", "example.com", "https://"] {
        d.set_text("markerProperties.url", bad);
        assert_eq!(d.app().auto.elements.iter().find(|e| e.id == "markerProperties.openLink").unwrap().enabled, Some(false));
        d.click("markerProperties.apply");
        assert_eq!(d.app().session.project.to_json(), before);
        assert!(d.app().ui.marker_properties.is_some());
    }
    d.set_text("markerProperties.url", "https://example.com/review");
    d.app().hooks.open_url = Some(Box::new(|_| Err("Test browser unavailable".into())));
    d.click("markerProperties.openLink");
    assert_eq!(d.app().ui.failed_link.as_ref().unwrap().url, "https://example.com/review");
    let draft = d.app().ui.marker_properties.as_ref().unwrap();
    assert_eq!(draft.url, "https://example.com/review");
    assert!(draft.error.contains("Test browser unavailable"));
    assert_eq!(d.app().session.project.to_json(), before);
}

#[test]
fn marker_type_uses_the_existing_editor_and_one_undo_step() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let marker = d.exec("markers.add", json!({"frame":30,"name":"Original"}))["marker"].as_u64().unwrap();
    let before = d.app().session.project.to_json();
    let history = d.app().session.history.undo.len();
    d.double_click(&format!("timeline.marker.{marker}"));
    d.click("markerProperties.kind.chapter");
    d.set_text("markerProperties.name", "Chapter draft");
    d.snapshot("marker-type-editor", "markerProperties.");
    assert_eq!(d.app().session.project.to_json(), before);
    d.click("markerProperties.cancel");
    assert_eq!(d.app().session.project.to_json(), before);
    d.double_click(&format!("timeline.marker.{marker}"));
    d.click("markerProperties.kind.chapter");
    d.set_text("markerProperties.name", "Chapter saved");
    d.click("markerProperties.apply");
    assert_eq!(d.app().session.active_sequence().unwrap().markers[0].kind, filmcraft_project::MarkerKind::Chapter);
    assert_eq!(d.app().session.history.undo.len(), history + 1);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), before);
    d.exec("edit.redo", json!({}));
    assert_eq!(d.app().session.active_sequence().unwrap().markers[0].kind, filmcraft_project::MarkerKind::Chapter);
}

#[test]
fn closing_project_discards_palette_and_clip_color_drafts_and_native_sessions() {
    let mut d = Driver::demo();
    let probe = NativeProbe::install(&mut d);
    d.ok("ui.panel.show", json!({"panel":"Markers"}));
    d.frames(3);
    d.click("markers.manageColors");
    d.click("palette.add");
    assert_eq!(probe.opens.get(), 1);
    d.exec("file.closeProject", json!({"force":true}));
    d.frames(3);
    assert!(d.app().ui.palette_manager.is_none());
    assert_eq!(probe.drops.get(), 1);
    assert!(d.app().session.project.custom_colors.is_empty());
    d.exec("file.openDemoProject", json!({}));
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    d.exec("timeline.select", json!({"clips":[clip.0]}));
    d.ok("ui.click", json!({"id":format!("timeline.clip.{}",clip.0),"button":"right"}));
    d.frames(3);
    d.click("timeline.clipMenu.edit.label");
    d.click("timeline.clipMenu.label.addColor");
    assert_eq!(probe.opens.get(), 2);
    d.exec("file.closeProject", json!({"force":true}));
    d.frames(3);
    assert!(d.app().ui.custom_label_color.is_none());
    assert_eq!(probe.drops.get(), 2);
    assert!(d.app().session.project.custom_colors.is_empty());
}

#[test]
fn custom_marker_properties_preserve_presets_and_commit_once() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let id = d.exec("markers.add", json!({"frame":30,"name":"Review","color":"Blue"}))["marker"].as_u64().unwrap();
    let before = d.app().session.project.to_json();
    let history = d.app().session.history.undo.len();
    d.right_marker(id, false);
    d.click("timeline.markerMenu.label");
    assert_eq!(d.ids("timeline.markerMenu.color.").len(), filmcraft_project::Label::ALL.len());
    d.click("timeline.markerMenu.customColor");
    assert!(!d.ids("markerColor.picker").is_empty());
    d.click("markerColor.hex");
    d.ok("ui.key", json!({"key":"Cmd+A"}));
    d.ok("ui.type", json!({"text":"#FF80C0"}));
    d.frames(2);
    assert_eq!(d.app().session.project.to_json(), before, "color preview is only a draft");
    d.snapshot("marker-color-picker", "markerColor.");
    d.click("markerColor.add");
    assert_eq!(d.app().session.project.custom_colors, vec![filmcraft_project::HexColor([255, 128, 192])]);
    assert!(d.ids("markerColor.picker").is_empty(), "the picker is not embedded in the marker window");
    assert_eq!(d.ids("markerProperties.preset.").len(), filmcraft_project::Label::ALL.len());
    let with_palette = d.app().session.project.to_json();
    d.click("markerProperties.author");
    d.ok("ui.type", json!({"text":"BENJI"}));
    d.frames(2);
    assert_eq!(d.app().session.project.to_json(), with_palette, "typing changes only the marker draft");
    d.snapshot("marker-custom-properties", "markerProperties.");
    d.click("markerProperties.apply");
    let marker = &d.app().session.active_sequence().unwrap().markers[0];
    assert_eq!(marker.author, "BENJI");
    assert_eq!(marker.rgb(), [255, 128, 192]);
    assert_eq!(d.app().session.history.undo.len(), history + 2, "one palette addition, one marker edit");
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), with_palette);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), before);
    d.exec("edit.redo", json!({}));
    d.exec("edit.redo", json!({}));
    let second = d.exec("markers.add", json!({"frame":90,"name":"Another"}))["marker"].as_u64().unwrap();
    d.right_marker(second, false);
    d.click("timeline.markerMenu.label");
    assert_eq!(d.ids("timeline.markerMenu.color.").len(), filmcraft_project::Label::ALL.len());
    assert!(!d.ids("timeline.markerMenu.custom.#FF80C0").is_empty());
    d.snapshot("marker-custom-menu", "timeline.markerMenu.");
    d.click("timeline.markerMenu.custom.#FF80C0");
    assert_eq!(d.app().session.active_sequence().unwrap().markers[1].rgb(), [255, 128, 192]);
    d.right_marker(id, false);
    d.click("timeline.markerMenu.label");
    d.click("timeline.markerMenu.color.Blue");
    assert_eq!(d.app().session.active_sequence().unwrap().markers[0].custom_color, None);
    assert_eq!(d.app().session.active_sequence().unwrap().markers[0].author, "BENJI");
}

#[test]
fn custom_marker_cancel_and_desaturation_do_not_edit_the_project() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let id = d.exec("markers.add", json!({"frame":30,"color":"#FF0000"}))["marker"].as_u64().unwrap();
    let before = d.app().session.project.to_json();
    d.right_marker(id, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.addColor");
    // Click the established picker's saturation/value square near its white corner.
    // Preview changes only the draft until confirmation.
    let elements = d.ok("ui.elements", json!({"prefix":"markerColor.picker"}));
    let rect = elements[0]["rect"].as_array().unwrap();
    let x = rect[0].as_f64().unwrap();
    let y = rect[1].as_f64().unwrap();
    d.ok("ui.click", json!({"x":x+2.0,"y":y+60.0}));
    d.frames(2);
    assert_ne!(d.app().ui.marker_properties.as_ref().unwrap().color_editor.as_ref().unwrap().hex, "FF0000");
    assert_eq!(d.app().session.project.to_json(), before);
    d.click("markerColor.cancel");
    d.click("markerProperties.cancel");
    assert_eq!(d.app().session.project.to_json(), before);
}

#[test]
fn custom_clip_picker_reuses_the_project_palette_and_is_undoable() {
    let mut d = Driver::demo();
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    d.exec("timeline.select", json!({"clips":[clip.0]}));
    let before = d.app().session.project.to_json();
    d.ok("ui.click", json!({"id":format!("timeline.clip.{}",clip.0),"button":"right"}));
    d.frames(3);
    d.click("timeline.clipMenu.edit.label");
    d.click("timeline.clipMenu.label.addColor");
    d.click("labelColor.hex");
    d.ok("ui.key", json!({"key":"Cmd+A"}));
    d.ok("ui.type", json!({"text":"#12ABCD"}));
    d.frames(2);
    assert_eq!(d.app().session.project.to_json(), before);
    d.snapshot("clip-color-picker", "labelColor.");
    d.click("labelColor.apply");
    let expected = filmcraft_project::HexColor::parse("#12ABCD");
    assert_eq!(d.app().session.active_sequence().unwrap().find_item(clip).unwrap().1.custom_label_color, expected);
    assert_eq!(d.app().session.project.custom_colors, vec![expected.unwrap()]);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), before);
    d.exec("edit.redo", json!({}));
    let marker = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.custom.#12ABCD");
    d.click("markerProperties.apply");
    assert_eq!(d.app().session.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == marker).unwrap().custom_color, expected);
}

#[test]
fn marker_editor_does_not_overwrite_newer_marker_edits() {
    let mut d = Driver::demo();
    let probe = NativeProbe::install(&mut d);
    d.exec("markers.clearAll", json!({}));
    let marker = d.exec("markers.add", json!({"frame":30,"name":"Original"}))["marker"].as_u64().unwrap();
    let other = d.exec("markers.add", json!({"frame":90}))["marker"].as_u64().unwrap();
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    d.set_text("markerProperties.name", "Unsaved draft");
    d.exec("markers.edit", json!({"marker":other,"name":"Unrelated edit"}));
    d.frames(2);
    assert!(d.app().ui.marker_properties.is_some(), "unrelated marker edits must not discard the draft");
    d.click("markerProperties.addColor");
    assert_eq!(probe.opens.get(), 1);
    d.exec("markers.edit", json!({"marker":marker,"name":"Newer edit","comment":"Keep this"}));
    let updated = d.app().session.project.to_json();
    let history = d.app().session.history.undo.len();
    d.frames(3);
    assert!(d.app().ui.marker_properties.is_none(), "a changed marker must invalidate its old draft");
    assert_eq!(probe.drops.get(), 1, "invalidating the draft also releases its native color session");
    assert_eq!(d.app().session.project.to_json(), updated);
    assert_eq!(d.app().session.history.undo.len(), history);
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().name, "Newer edit");
    d.exec("markers.delete", json!({"marker":marker}));
    d.frames(2);
    assert!(d.app().ui.marker_properties.is_none(), "deleted markers must invalidate their drafts too");
}

#[test]
fn custom_color_dialogs_do_not_follow_a_different_project_or_saved_ui_state() {
    let mut d = Driver::demo();
    let marker = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.addColor");
    d.exec("file.openDemoProject", json!({}));
    d.frames(2);
    assert!(d.app().ui.marker_properties.is_none());
    assert!(d.app().session.project.custom_colors.is_empty());

    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    d.ok("ui.click", json!({"id":format!("timeline.clip.{}",clip.0),"button":"right"}));
    d.frames(3);
    d.click("timeline.clipMenu.edit.label");
    d.click("timeline.clipMenu.label.addColor");
    let saved_ui = serde_json::to_value(&d.app().ui).unwrap();
    assert!(saved_ui.get("custom_label_color").is_none());
    assert!(saved_ui.get("marker_properties").is_none());
    d.exec("file.openDemoProject", json!({}));
    d.frames(2);
    assert!(d.app().ui.custom_label_color.is_none());
    assert!(d.app().session.project.custom_colors.is_empty());
}

#[test]
fn marker_editor_time_range_delete_and_source_owner() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let id = d.exec("markers.add", json!({"frame":30,"name":"Review"}))["marker"].as_u64().unwrap();
    d.double_click(&format!("timeline.marker.{id}"));
    d.frames(3);
    assert!(d.app().ui.marker_properties.is_some());
    d.set_text("markerProperties.time", "00:00:02:00");
    d.click("markerProperties.range");
    d.set_text("markerProperties.duration", "00:00:01:00");
    d.snapshot("marker-complete-editor", "markerProperties.");
    d.click("markerProperties.apply");
    let rate = d.app().session.sequence_rate();
    let m = &d.app().session.active_sequence().unwrap().markers[0];
    assert_eq!(m.start, rate.tick_of(rate.timecode_base() * 2));
    assert_eq!(m.duration, rate.tick_of(rate.timecode_base()));
    d.right_marker(id, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.range");
    assert_eq!(marker_draft_text(&d, "duration"), "00:00:01:00", "unchecking keeps the draft value");
    assert_eq!(d.ok("ui.elements", json!({"prefix":"markerProperties.duration"}))[0]["enabled"], false);
    d.click("markerProperties.range");
    assert_eq!(marker_draft_text(&d, "duration"), "00:00:01:00", "rechecking restores the draft value");
    d.click("markerProperties.range");
    d.click("markerProperties.apply");
    assert_eq!(d.app().session.active_sequence().unwrap().markers[0].duration, filmcraft_time::Tick::ZERO);
    let second = d.exec("markers.add", json!({"frame":60}))["marker"].as_u64().unwrap();
    d.right_marker(second, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.delete");
    assert_eq!(d.app().session.active_sequence().unwrap().markers.len(), 1);
    d.exec("source.open", json!({"item":5}));
    let source = d.exec("markers.add", json!({"target":"source","time":0,"name":"Source cue"}))["marker"].as_u64().unwrap();
    d.frames(3);
    d.ok("ui.click", json!({"id":format!("source.marker.{source}"),"button":"right"}));
    d.frames(3);
    d.click("monitor.markerMenu.properties");
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().owner, filmcraft_ui_egui::panels::marker_properties::Owner::Source(5));
    d.exec("source.open", json!({"item":6}));
    d.set_text("markerProperties.author", "BENJI");
    d.click("markerProperties.apply");
    let view = filmcraft_engine::clip_ops::source_view(&d.app().session, filmcraft_project::ItemId(5)).unwrap();
    assert_eq!(view.markers[0].author, "BENJI");
}

#[test]
fn marker_panel_search_selection_filters_and_palette_manager() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let a = d.exec("markers.add", json!({"frame":10,"name":"Review","author":"BENJI","color":"#123456"}))["marker"].as_u64().unwrap();
    let b = d.exec("markers.add", json!({"frame":20,"name":"Other","comment":"Review"}))["marker"].as_u64().unwrap();
    d.ok("ui.panel.show", json!({"panel":"Markers"}));
    d.frames(3);
    d.set_text("markers.search", "benji");
    assert_eq!(d.ids("markers.row."), vec![format!("markers.row.{a}")]);
    d.set_text("markers.search", "review");
    assert_eq!(d.ids("markers.row.").len(), 2);
    d.click(&format!("markers.row.{a}"));
    d.ok("ui.click", json!({"id":format!("markers.row.{b}"),"modifiers":{"command":true}}));
    d.frames(2);
    assert_eq!(d.app().ui.markers_panel.selected.len(), 2);
    d.frames(30);
    d.click(&format!("markers.row.{a}"));
    d.ok("ui.click", json!({"id":format!("markers.row.{b}"),"modifiers":{"shift":true}}));
    d.frames(2);
    assert_eq!(d.app().ui.markers_panel.selected.len(), 2);

    d.click("markers.selectAll");
    let history = d.app().session.history.undo.len();
    d.click("markers.bulkColor");
    d.click("markers.bulkColor.Blue");
    assert_eq!(d.app().session.history.undo.len(), history + 1);
    assert!(d.app().session.active_sequence().unwrap().markers.iter().all(|m| m.color == filmcraft_project::Label::Blue && m.custom_color.is_none()));
    d.exec("edit.undo", json!({}));
    d.frames(2);
    d.click("markers.filter.#123456");
    assert_eq!(d.ids("markers.row."), vec![format!("markers.row.{b}")]);
    d.ok("ui.menu.invoke", json!({"id":"markers.showAllMarkerColors"}));
    d.frames(3);
    assert_eq!(d.ids("markers.row.").len(), 2);
    d.click("markers.selectAll");
    d.snapshot("marker-panel", "markers.");
    d.click("markers.deleteSelected");
    assert!(d.app().session.active_sequence().unwrap().markers.is_empty());
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.active_sequence().unwrap().markers.len(), 2);
    d.click("markers.manageColors");
    d.click("palette.add");
    d.set_text("palette.name", "Review");
    d.set_text("paletteColor.hex", "12ABCD");
    d.snapshot("palette-color-editor", "palette");
    d.click("palette.save");
    let color = filmcraft_project::HexColor::parse("12ABCD").unwrap();
    assert_eq!(d.app().session.project.custom_color_names[&color], "Review");
    d.snapshot("project-palette-manager", "palette.");
    d.click("palette.edit.#12ABCD");
    d.set_text("palette.name", "Approved");
    d.set_text("paletteColor.hex", "123456");
    d.click("palette.save");
    assert_eq!(d.app().session.project.custom_color_names[&filmcraft_project::HexColor::parse("123456").unwrap()], "Approved");
    d.click("palette.remove.#123456");
    assert!(d.app().session.project.custom_colors.is_empty());
    d.click("palette.close");
    d.exec("source.open", json!({"item":5}));
    let source = d.exec("markers.add", json!({"target":"source","time":0,"name":"Review source"}))["marker"].as_u64().unwrap();
    d.click("markers.scope.Source");
    assert_eq!(d.ids("markers.row."), vec![format!("markers.row.{source}")]);
    d.double_click(&format!("markers.row.{source}"));
    d.frames(3);
    assert!(d.app().ui.marker_properties.is_some());
}

#[derive(Debug)]
struct NativeProbeSession {
    state: std::rc::Rc<std::cell::Cell<filmcraft_ui_egui::widgets::color_picker::NativeColorState>>,
    drops: std::rc::Rc<std::cell::Cell<usize>>,
}
impl filmcraft_ui_egui::widgets::color_picker::NativeColorSession for NativeProbeSession {
    fn poll(&self) -> Result<filmcraft_ui_egui::widgets::color_picker::NativeColorState, String> {
        Ok(self.state.get())
    }
    fn set_color(&self, color: [u8; 3]) -> Result<(), String> {
        self.state.set(filmcraft_ui_egui::widgets::color_picker::NativeColorState { color: Some(color), open: true });
        Ok(())
    }
}
impl Drop for NativeProbeSession {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}
struct NativeProbe {
    state: std::rc::Rc<std::cell::Cell<filmcraft_ui_egui::widgets::color_picker::NativeColorState>>,
    drops: std::rc::Rc<std::cell::Cell<usize>>,
    opens: std::rc::Rc<std::cell::Cell<usize>>,
}
impl NativeProbe {
    fn install(d: &mut Driver) -> Self {
        use filmcraft_ui_egui::widgets::color_picker::{NativeColorSession, NativeColorState};
        use std::{cell::Cell, rc::Rc};
        let probe =
            Self { state: Rc::new(Cell::new(NativeColorState { color: None, open: false })), drops: Rc::new(Cell::new(0)), opens: Rc::new(Cell::new(0)) };
        let (state, drops, opens) = (probe.state.clone(), probe.drops.clone(), probe.opens.clone());
        d.harness.state_mut().hooks.pick_color = Some(Box::new(move |color| {
            opens.set(opens.get() + 1);
            state.set(NativeColorState { color: Some(color), open: true });
            Ok(Rc::new(NativeProbeSession { state: state.clone(), drops: drops.clone() }) as Rc<dyn NativeColorSession>)
        }));
        probe
    }
    fn choose(&self, color: [u8; 3]) {
        self.state.set(filmcraft_ui_egui::widgets::color_picker::NativeColorState { color: Some(color), open: true });
    }
}

#[test]
fn adding_a_marker_keeps_creation_immediate_without_opening_editors() {
    let mut d = Driver::demo();
    let probe = NativeProbe::install(&mut d);
    d.exec("markers.clearAll", json!({}));
    d.ok("ui.set", json!({"focused":"Timeline"}));
    d.ok("ui.key", json!({"key":"M"}));
    d.frames(3);
    assert_eq!(d.app().session.active_sequence().unwrap().markers.len(), 1);
    assert!(d.app().ui.marker_properties.is_none());
    assert!(d.app().ui.custom_label_color.is_none());
    assert_eq!(probe.opens.get(), 0, "creation must not open the native picker");
    let marker = d.app().session.active_sequence().unwrap().markers[0].id.0;
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    assert!(d.app().ui.marker_properties.is_some());
    assert_eq!(probe.opens.get(), 0, "editing presets does not open custom color selection");
    d.click("markerProperties.addColor");
    assert_eq!(probe.opens.get(), 1);
}

#[test]
fn shared_picker_preserves_exact_hex_and_incomplete_input_until_confirmation() {
    let mut d = Driver::demo();
    let marker = d.exec("markers.add", json!({"frame":30,"color":"#010203"}))["marker"].as_u64().unwrap();
    let before = d.app().session.project.to_json();
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.addColor");
    d.frames(12);
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().color_editor.as_ref().unwrap().hex, "010203");
    d.set_text("markerColor.hex", "01");
    d.frames(12);
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().color_editor.as_ref().unwrap().hex, "01");
    assert_eq!(d.app().session.project.to_json(), before);
    d.set_text("markerColor.hex", "12ABCD");
    d.frames(12);
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().color_editor.as_ref().unwrap().value(), Some(filmcraft_project::HexColor([18, 171, 205])));
    d.click("markerColor.cancel");
    d.click("markerProperties.cancel");
    assert_eq!(d.app().session.project.to_json(), before);
}

#[test]
fn marker_and_palette_controls_translate_without_changing_automation_ids() {
    let mut d = Driver::demo();
    let marker = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    d.harness.state_mut().ui.language = filmcraft_ui_egui::i18n::Language::Es;
    d.frames(3);
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    let labels = d.ok("ui.elements", json!({"prefix":"markerProperties.author"}));
    assert_eq!(labels[0]["label"], "Autor");
    d.click("markerProperties.addColor");
    let labels = d.ok("ui.elements", json!({"prefix":"markerColor.picker"}));
    assert_eq!(labels[0]["label"], "Selector de color");
    d.click("markerColor.cancel");
    d.click("markerProperties.cancel");
    d.ok("ui.panel.show", json!({"panel":"Markers"}));
    d.frames(3);
    d.click("markers.manageColors");
    d.click("palette.add");
    let labels = d.ok("ui.elements", json!({"prefix":"palette.save"}));
    assert_eq!(labels[0]["label"], "Guardar color");
    d.click("palette.cancelEdit");
    d.click("palette.close");
    d.harness.state_mut().ui.language = filmcraft_ui_egui::i18n::Language::En;
    d.frames(1);
}

#[test]
fn native_marker_color_preview_cancel_and_undo_preserve_project_ownership() {
    let mut d = Driver::demo();
    let probe = NativeProbe::install(&mut d);
    d.exec("markers.clearAll", json!({}));
    let marker = d.exec("markers.add", json!({"frame":30,"color":"Blue"}))["marker"].as_u64().unwrap();
    let before = d.app().session.project.to_json();
    let history = d.app().session.history.undo.len();
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.addColor");
    assert_eq!(probe.opens.get(), 1);
    assert!(d.ids("markerColor.picker").is_empty());
    assert!(!d.ids("markerColor.native").is_empty());
    probe.choose([18, 171, 205]);
    d.frames(3);
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().color_editor.as_ref().unwrap().hex, "12ABCD");
    assert_eq!(d.app().session.project.to_json(), before, "native color changes are draft-only");
    d.set_text("markerColor.hex", "123456");
    assert_eq!(probe.state.get().color, Some([18, 52, 86]), "typed hex updates the open system picker");
    d.set_text("markerColor.hex", "12");
    d.frames(3);
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().color_editor.as_ref().unwrap().hex, "12", "polling must not erase incomplete hex input");
    d.click("markerColor.native");
    assert_eq!(probe.opens.get(), 2);
    assert_eq!(probe.drops.get(), 1);
    assert_eq!(probe.state.get().color, Some([18, 52, 86]), "reopening with incomplete hex must retain the last valid color");
    d.frames(3);
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().color_editor.as_ref().unwrap().hex, "12");
    d.click("markerColor.cancel");
    assert_eq!(probe.drops.get(), 2);
    d.click("markerProperties.cancel");
    assert_eq!(d.app().session.project.to_json(), before);
    assert_eq!(d.app().session.history.undo.len(), history);
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.addColor");
    probe.choose([18, 171, 205]);
    d.frames(3);
    d.click("markerColor.add");
    assert_eq!(probe.drops.get(), 3);
    d.click("markerProperties.apply");
    assert_eq!(d.app().session.active_sequence().unwrap().markers[0].custom_color, Some(filmcraft_project::HexColor([18, 171, 205])));
    assert_eq!(d.app().session.history.undo.len(), history + 2, "one palette add and one marker apply");
    d.exec("edit.undo", json!({}));
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), before);
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.addColor");
    d.exec("file.openDemoProject", json!({}));
    d.frames(3);
    assert_eq!(probe.drops.get(), 4, "switching projects releases the native picker");
    assert!(d.app().ui.marker_properties.is_none());
}

#[test]
fn native_clip_and_palette_colors_keep_confirmation_and_close_the_panel() {
    let mut d = Driver::demo();
    let probe = NativeProbe::install(&mut d);
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    d.exec("timeline.select", json!({"clips":[clip.0]}));
    let before = d.app().session.project.to_json();
    d.ok("ui.click", json!({"id":format!("timeline.clip.{}",clip.0),"button":"right"}));
    d.frames(3);
    d.click("timeline.clipMenu.edit.label");
    d.click("timeline.clipMenu.label.addColor");
    probe.choose([18, 171, 205]);
    d.frames(3);
    assert_eq!(d.app().session.project.to_json(), before);
    d.click("labelColor.apply");
    assert_eq!(probe.drops.get(), 1);
    assert_eq!(d.app().session.active_sequence().unwrap().find_item(clip).unwrap().1.custom_label_color, Some(filmcraft_project::HexColor([18, 171, 205])));
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), before);
    d.ok("ui.panel.show", json!({"panel":"Markers"}));
    d.frames(3);
    d.click("markers.manageColors");
    d.click("palette.add");
    probe.choose([17, 34, 51]);
    d.frames(3);
    d.click("palette.cancelEdit");
    assert_eq!(probe.drops.get(), 2);
    assert_eq!(d.app().session.project.to_json(), before);
    d.click("palette.add");
    probe.choose([17, 34, 51]);
    d.frames(3);
    d.set_text("palette.name", "Review");
    d.click("palette.save");
    assert_eq!(probe.drops.get(), 3);
    assert_eq!(d.app().session.project.custom_color_names[&filmcraft_project::HexColor([17, 34, 51])], "Review");
    d.exec("edit.undo", json!({}));
    assert_eq!(d.app().session.project.to_json(), before);
}

#[test]
fn native_panel_hiding_keeps_its_draft_until_confirmation_or_cancel() {
    use filmcraft_ui_egui::widgets::color_picker::NativeColorState;
    let mut d = Driver::demo();
    let probe = NativeProbe::install(&mut d);
    let marker = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    let before = d.app().session.project.to_json();
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.addColor");
    probe.state.set(NativeColorState { color: Some([18, 171, 205]), open: false });
    d.frames(4);
    assert_eq!(probe.opens.get(), 1, "hiding must not automatically reopen the panel");
    assert_eq!(probe.drops.get(), 0, "a hidden panel can return when the app activates");
    probe.choose([17, 34, 51]);
    d.frames(3);
    assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().color_editor.as_ref().unwrap().hex, "112233");
    assert_eq!(d.app().session.project.to_json(), before);
    d.click("markerColor.cancel");
    assert_eq!(probe.drops.get(), 1);
    d.click("markerProperties.cancel");
    assert_eq!(d.app().session.project.to_json(), before);
}

#[test]
fn failed_native_picker_keeps_the_shared_color_editor_available() {
    let mut d = Driver::demo();
    d.harness.state_mut().hooks.pick_color = Some(Box::new(|_| Err("Test picker unavailable".into())));
    let marker = d.exec("markers.add", json!({"frame":30}))["marker"].as_u64().unwrap();
    d.right_marker(marker, false);
    d.click("timeline.markerMenu.properties");
    d.click("markerProperties.addColor");
    assert!(!d.ids("markerColor.picker").is_empty());
    d.set_text("markerColor.hex", "123456");
    d.click("markerColor.add");
    d.click("markerProperties.apply");
    assert_eq!(
        d.app().session.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == marker).unwrap().custom_color,
        Some(filmcraft_project::HexColor([18, 52, 86]))
    );
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn double_click(&mut self, id: &str) {
        self.frames(3);
        let v = self.ok("ui.elements", json!({"prefix":id}));
        let rect = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap()["rect"].as_array().unwrap();
        let pos = egui::pos2(
            (rect[0].as_f64().unwrap() + rect[2].as_f64().unwrap() / 2.0) as f32,
            (rect[1].as_f64().unwrap() + rect[3].as_f64().unwrap() / 2.0) as f32,
        );
        self.double_click_at(pos);
    }

    fn double_click_at(&mut self, pos: egui::Pos2) {
        // Control-channel event pacing is too slow for a physical double click.
        self.frames(60);
        self.harness.input_mut().events.push(egui::Event::PointerMoved(pos));
        self.frames(1);
        for _ in 0..2 {
            for pressed in [true, false] {
                self.harness.input_mut().events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                });
                self.frames(1);
            }
        }
        self.frames(3);
    }

    fn set_text(&mut self, id: &str, text: &str) {
        self.click(id);
        self.ok("ui.key", json!({"key":"Cmd+A"}));
        if text.is_empty() {
            self.ok("ui.key", json!({"key":"Backspace"}));
        } else {
            self.ok("ui.type", json!({"text":text}));
        }
        self.frames(2);
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// (video, audio, submix) track names of the active sequence, lowest first.
    fn tracks(&mut self) -> [Vec<String>; 3] {
        let q = self.app().session.active_sequence().unwrap();
        [&q.video_tracks, &q.audio_tracks, &q.submix_tracks].map(|t| t.iter().map(|t| t.name.clone()).collect())
    }

    fn label(&mut self, id: &str) -> String {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        e["label"].as_str().unwrap_or_default().to_string()
    }

    /// With `FILMCRAFT_UI_SNAPSHOT_DIR` set, render the window offscreen and write `<name>.png`,
    /// cropped to the elements whose ids start with `prefix`.
    fn snapshot(&mut self, name: &str, prefix: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for e in v.as_array().unwrap() {
            let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
            bb = [bb[0].min(r[0]), bb[1].min(r[1]), bb[2].max(r[0] + r[2]), bb[3].max(r[1] + r[3])];
        }
        self.frames(2);
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        let ppp = img.width() as f32 / self.harness.ctx.content_rect().width();
        let x0 = ((bb[0] - 24.0) * ppp).max(0.0) as u32;
        let y0 = ((bb[1] - if prefix == "markerProperties." { 75.0 } else { 40.0 }) * ppp).max(0.0) as u32;
        let x1 = (((bb[2] + 24.0) * ppp) as u32).min(img.width());
        let y1 = (((bb[3] + 24.0) * ppp) as u32).min(img.height());
        let img = image::imageops::crop_imm(&img, x0, y0, x1.saturating_sub(x0), y1.saturating_sub(y0)).to_image();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.json")), serde_json::to_vec_pretty(&v).unwrap()).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }

    fn right_marker(&mut self, marker: u64, range: bool) {
        self.frames(3);
        let id = format!("timeline.marker.{marker}");
        let elements = self.ok("ui.elements", json!({"prefix":id}));
        let rect = elements.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap()["rect"].as_array().unwrap();
        let mut x = rect[0].as_f64().unwrap() + rect[2].as_f64().unwrap() / 2.0;
        let y = rect[1].as_f64().unwrap() + rect[3].as_f64().unwrap() / 2.0;
        if range {
            let app = self.app();
            let m = app.session.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == marker).unwrap();
            x += m.duration.seconds() * app.ui.timeline.pps * 0.5;
        }
        self.ok("ui.click", json!({"x":x,"y":y,"button":"right"}));
        self.frames(3);
    }

    /// The ids under `prefix` once `done` holds for them: the timeline works out duplicate frame
    /// markers on a background thread.
    fn ids_once(&mut self, prefix: &str, mut done: impl FnMut(&[String]) -> bool) -> Vec<String> {
        let t0 = std::time::Instant::now();
        loop {
            let ids = self.ids(prefix);
            if done(&ids) || t0.elapsed().as_secs() >= 30 {
                return ids;
            }
            self.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
}

/// Sequence ▸ Add Tracks… opens Premiere Pro's dialog instead of adding a video track at once:
/// amounts, a placement and (for audio and submix tracks) a track type for each kind, with
/// Premiere's defaults (one video and one audio track after the last ones, no submix track).
#[test]
fn add_tracks_dialog_offers_amount_placement_and_type() {
    let mut d = Driver::demo();
    let before = d.tracks();
    assert_eq!(
        before,
        [vec!["Video 1", "Video 2", "Video 3"], vec!["Audio 1", "Audio 2", "Audio 3"], vec![]]
            .map(|v: Vec<&str>| v.into_iter().map(String::from).collect::<Vec<_>>())
    );
    let r = d.ok("ui.menu.invoke", json!({"id": "sequence.addTracks"}));
    assert_eq!(r["dialog"], "addTracks", "{r}");
    d.frames(3);
    assert_eq!(d.tracks(), before, "opening the dialog adds nothing");
    let ids = d.ids("addTracks.");
    for id in [
        "addTracks.video.amount",
        "addTracks.video.placement",
        "addTracks.audio.amount",
        "addTracks.audio.placement",
        "addTracks.audio.type",
        "addTracks.submix.amount",
        "addTracks.submix.placement",
        "addTracks.submix.type",
        "addTracks.ok",
        "addTracks.cancel",
    ] {
        assert!(ids.iter().any(|i| i == id), "{id} missing: {ids:?}");
    }
    assert!(!ids.iter().any(|i| i == "addTracks.video.type"), "video tracks have no type");
    // the defaults, as shown
    for (id, shown) in [
        ("addTracks.video.amount", "1"),
        ("addTracks.video.placement", "After Video 3"),
        ("addTracks.audio.amount", "1"),
        ("addTracks.audio.placement", "After Audio 3"),
        ("addTracks.audio.type", "Standard"),
        ("addTracks.submix.amount", "0"),
        ("addTracks.submix.placement", "Before First Track"),
        ("addTracks.submix.type", "Stereo"),
    ] {
        assert_eq!(d.label(id), shown, "{id}");
    }
    // OK with the defaults: one video and one audio track after the last ones
    d.click("addTracks.ok");
    assert!(d.ids("addTracks.").is_empty(), "dialog closed");
    let [v, a, s] = d.tracks();
    assert_eq!((v.len(), a.len(), s.len()), (4, 4, 0));
    assert_eq!((v[3].as_str(), a[3].as_str()), ("Video 4", "Audio 4"));
    assert_eq!(d.app().session.active_sequence().unwrap().audio_tracks[3].channels, filmcraft_engine::project::AudioChannels::Stereo);
    // one undo step for the whole dialog
    d.exec("edit.undo", json!({}));
    assert_eq!(d.tracks(), before);
}

#[test]
fn add_tracks_dialog_places_and_types_the_new_tracks() {
    let mut d = Driver::demo();
    let on_v2 = d.app().session.active_sequence().unwrap().video_tracks[1].items[0].id;
    d.ok("ui.menu.invoke", json!({"id": "sequence.addTracks"}));
    d.frames(3);
    // Placement lists "Before First Track" and every track; pick "After Video 1"
    d.click("addTracks.video.placement");
    let options = d.ids("addTracks.video.placement.option.");
    assert_eq!(options.len(), 4, "{options:?}");
    assert_eq!(
        (d.label("addTracks.video.placement.option.0"), d.label("addTracks.video.placement.option.1")),
        ("Before First Track".into(), "After Video 1".into())
    );
    d.click("addTracks.video.placement.option.1");
    assert_eq!(d.label("addTracks.video.placement"), "After Video 1");
    // audio: a mono track before the first one
    d.click("addTracks.audio.placement");
    d.click("addTracks.audio.placement.option.0");
    d.click("addTracks.audio.type");
    let types = d.ids("addTracks.audio.type.option.");
    assert_eq!(types.len(), 4, "Standard, 5.1, Adaptive, Mono: {types:?}");
    d.click("addTracks.audio.type.option.mono");
    // submix: 5.1 (the types are Stereo, 5.1, Adaptive, Mono)
    d.click("addTracks.submix.type");
    assert!(
        d.ids("addTracks.submix.type.option.").iter().any(|i| i.ends_with(".stereo"))
            && !d.ids("addTracks.submix.type.option.").iter().any(|i| i.ends_with(".standard"))
    );
    d.click("addTracks.submix.type.option.5.1");
    // amounts (number fields): two video tracks, one audio, one submix
    d.app().ui.add_tracks.video = 2;
    d.app().ui.add_tracks.submix = 1;
    d.frames(2);
    assert_eq!((d.label("addTracks.video.amount"), d.label("addTracks.submix.amount")), ("2".into(), "1".into()));
    d.click("addTracks.ok");
    assert!(d.ids("addTracks.").is_empty(), "dialog closed");
    let [v, a, s] = d.tracks();
    assert_eq!(v, ["Video 1", "Video 2", "Video 3", "Video 4", "Video 5"]);
    assert_eq!(a, ["Audio 1", "Audio 2", "Audio 3", "Audio 4"]);
    assert_eq!(s, ["Submix 1"]);
    let q = d.app().session.active_sequence().unwrap().clone();
    assert!(q.video_tracks[1].items.is_empty() && q.video_tracks[2].items.is_empty(), "the new tracks are after Video 1");
    assert_eq!(q.video_tracks[3].items[0].id, on_v2, "what was on Video 2 moved up to Video 4");
    use filmcraft_engine::project::AudioChannels;
    assert_eq!((q.audio_tracks[0].channels, q.audio_tracks[0].items.len()), (AudioChannels::Mono, 0));
    assert_eq!(q.submix_tracks[0].channels, AudioChannels::Surround51);

    // with a submix track in the sequence its placement can be chosen too
    d.ok("ui.menu.invoke", json!({"id": "sequence.addTracks"}));
    d.frames(3);
    assert_eq!(d.label("addTracks.submix.placement"), "After Submix 1");
    assert_eq!(
        (d.label("addTracks.video.amount"), d.label("addTracks.audio.type")),
        ("1".into(), "Standard".into()),
        "the dialog starts from the defaults again"
    );
    // Cancel and Escape add nothing; neither does OK with every amount at 0
    d.click("addTracks.cancel");
    assert!(d.ids("addTracks.").is_empty());
    d.ok("ui.menu.invoke", json!({"id": "sequence.addTracks"}));
    d.frames(3);
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.ids("addTracks.").is_empty());
    d.ok("ui.menu.invoke", json!({"id": "sequence.addTracks"}));
    d.frames(3);
    d.app().ui.add_tracks.video = 0;
    d.app().ui.add_tracks.audio = 0;
    d.frames(2);
    d.click("addTracks.ok");
    assert!(d.ids("addTracks.").is_empty());
    assert_eq!(d.tracks(), [v, a, s]);
    assert!(d.app().ui.status.is_empty(), "{}", d.app().ui.status);
}

#[test]
fn delete_tracks_dialog_deletes_empty_tracks() {
    let mut d = Driver::demo();
    let before = d.exec("sequence.inspect", json!({}));
    let nv = before["video"].as_array().unwrap().len();
    let na = before["audio"].as_array().unwrap().len();
    let r = d.ok("ui.menu.invoke", json!({"id": "sequence.deleteTracks"}));
    assert_eq!(r["dialog"], "deleteTracks", "{r}");
    d.frames(3);
    let ids = d.ids("deleteTracks.");
    for id in ["deleteTracks.video", "deleteTracks.audio", "deleteTracks.video.target", "deleteTracks.audio.target", "deleteTracks.ok", "deleteTracks.cancel"] {
        assert!(ids.iter().any(|i| i == id), "{id} missing: {ids:?}");
    }
    d.click("deleteTracks.video");
    d.click("deleteTracks.audio");
    d.click("deleteTracks.ok");
    assert!(d.ids("deleteTracks.").is_empty(), "dialog closed");
    let after = d.exec("sequence.inspect", json!({}));
    let (nv2, na2) = (after["video"].as_array().unwrap().len(), after["audio"].as_array().unwrap().len());
    assert!(nv2 < nv && na2 < na, "empty tracks deleted: {nv}->{nv2}, {na}->{na2}");
    // Cancel leaves the sequence alone
    d.ok("ui.menu.invoke", json!({"id": "sequence.deleteTracks"}));
    d.frames(3);
    d.click("deleteTracks.video");
    d.click("deleteTracks.cancel");
    assert_eq!(d.exec("sequence.inspect", json!({}))["video"].as_array().unwrap().len(), nv2);
}

#[test]
fn through_edits_are_marked_on_the_timeline() {
    let mut d = Driver::demo();
    d.exec("playhead.set", json!({"frame": 30}));
    d.exec("sequence.addEditAllTracks", json!({}));
    d.frames(3);
    assert!(d.ids("timeline.throughEdit.").is_empty(), "hidden until Show Through Edits");
    d.ok("ui.menu.invoke", json!({"id": "sequence.showThroughEdits"}));
    d.frames(3);
    let marks = d.ids("timeline.throughEdit.");
    assert!(marks.len() >= 2, "V1 + A1 marks: {marks:?}");
    d.exec("sequence.joinThroughEdits", json!({"all": true}));
    d.frames(3);
    assert!(d.ids("timeline.throughEdit.").is_empty(), "joined");
}

#[test]
fn duplicate_frame_markers_are_drawn_on_repeated_clips() {
    let mut d = Driver::demo();
    // the demo's first V1 clip placed again on V2 at the same time: both show the same frames
    let (first, item, start, source_in, duration) = {
        let c = &d.app().session.active_sequence().unwrap().video_tracks[0].items[0];
        (c.id.0, c.item.0, c.start.0, c.source_in.0, c.duration.0)
    };
    let r = d.exec("timeline.place", json!({"item": item, "track": "V2", "time": start, "sourceIn": source_in, "duration": duration, "audio": false}));
    let copy = r["clips"][0].as_u64().unwrap();
    d.frames(3);
    assert!(d.ids("timeline.duplicateFrames.").is_empty(), "hidden until Show Duplicate Frame Markers");
    d.click("timeline.toggle.settings");
    d.click("timeline.settings.showDuplicateFrameMarkers");
    assert!(d.app().session.state.show_duplicate_frame_markers);
    let has = |marks: &[String], clip: u64| marks.contains(&format!("timeline.duplicateFrames.{clip}.0"));
    let marks = d.ids_once("timeline.duplicateFrames.", |m| has(m, first) && has(m, copy));
    for clip in [first, copy] {
        assert!(has(&marks, clip), "clip {clip}: {marks:?}");
    }
    // the markers follow edits: without the copy the first clip repeats nothing (the demo
    // repeats frames of another clip elsewhere, and those markers stay)
    let ours = |marks: &[String]| marks.iter().filter(|m| [first, copy].iter().any(|c| m.starts_with(&format!("timeline.duplicateFrames.{c}.")))).count();
    d.exec("edit.undo", json!({}));
    let marks = d.ids_once("timeline.duplicateFrames.", |m| ours(m) == 0);
    assert_eq!(ours(&marks), 0, "the copy is gone: {marks:?}");
    assert!(!marks.is_empty(), "the demo's own repeat: {marks:?}");
    d.exec("edit.redo", json!({}));
    assert_eq!(ours(&d.ids_once("timeline.duplicateFrames.", |m| ours(m) == 2)), 2, "the copy is back");
    // at 200 % the copy shows every other frame of the original, so the original repeats in
    // single frames
    d.app()
        .session
        .edit_sequence("test", |q, _, _| {
            q.find_item_mut(filmcraft_project::ClipId(copy)).unwrap().1.speed = 2.0;
            Ok(())
        })
        .unwrap();
    let listed = d.exec("sequence.duplicateFrames", json!({}));
    let runs = listed.as_array().unwrap().iter().find(|e| e["clip"] == json!(first)).unwrap()["ranges"].as_array().unwrap().len();
    assert!(runs > 4, "{listed}");
    // only the stripes in view are drawn: scrolled to the middle of the clip, the first is not
    let (scroll, pps) = (d.app().ui.timeline.scroll, d.app().ui.timeline.pps);
    let middle = (start + duration / 2) as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
    (d.app().ui.timeline.scroll, d.app().ui.timeline.target_scroll) = (middle, middle);
    let scrolled = |m: &[String]| !m.is_empty() && !m.contains(&format!("timeline.duplicateFrames.{first}.0"));
    let marks = d.ids_once(&format!("timeline.duplicateFrames.{first}."), scrolled);
    assert!(scrolled(&marks), "{marks:?}");
    (d.app().ui.timeline.scroll, d.app().ui.timeline.target_scroll) = (scroll, scroll);
    // zoomed far out, the single frames make one stripe
    (d.app().ui.timeline.pps, d.app().ui.timeline.target_pps) = (1.0, 1.0);
    d.frames(3);
    let marks = d.ids(&format!("timeline.duplicateFrames.{first}."));
    assert_eq!(marks.len(), 1, "{marks:?}");
    (d.app().ui.timeline.pps, d.app().ui.timeline.target_pps) = (pps, pps);
    d.exec("sequence.showDuplicateFrameMarkers", json!({"on": false}));
    d.frames(3);
    assert!(d.ids("timeline.duplicateFrames.").is_empty(), "turned off");
}

#[test]
fn markers_panel_colour_filter() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    d.exec("markers.add", json!({"frame": 10, "name": "green"}));
    d.exec("markers.add", json!({"frame": 20, "name": "red", "color": "Rose"}));
    d.ok("ui.panel.show", json!({"panel": "Markers"}));
    d.frames(3);
    assert_eq!(d.ids("markers.row.").len(), 2);
    assert!(d.ids("markers.filter.").len() >= 7);
    d.click("markers.filter.Rose");
    d.frames(2);
    assert_eq!(d.ids("markers.row.").len(), 1, "red hidden");
    let m = d.ok("ui.menu.list", json!({}));
    let show_all = m.as_array().unwrap().iter().find(|i| i["id"] == "markers.showAllMarkerColors").unwrap().clone();
    assert_eq!(show_all["enabled"], json!(true));
    d.ok("ui.menu.invoke", json!({"id": "markers.showAllMarkerColors"}));
    d.frames(3);
    assert_eq!(d.ids("markers.row.").len(), 2);
}

/// #695: a marker on the Timeline could not be cleared: with snapping off a click on it left the
/// playhead on the frame under the pointer, not on the marker Clear Selected Marker looks for, and
/// right-clicking it opened the clip menu.
#[test]
fn a_clicked_timeline_marker_can_be_cleared() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let first = d.exec("markers.add", json!({"frame": 30}))["marker"].as_u64().unwrap();
    let second = d.exec("markers.add", json!({"frame": 90}))["marker"].as_u64().unwrap();
    d.exec("sequence.snap", json!({"on": false}));
    d.exec("playhead.set", json!({"frame": 0}));
    // zoomed out so that a few px beside a marker is another frame
    let v = &mut d.app().ui.timeline;
    (v.pps, v.target_pps, v.scroll, v.target_scroll) = (60.0, 60.0, 0.0, 0.0);
    d.frames(4);
    let markers = |d: &mut Driver| d.app().session.active_sequence().unwrap().markers.iter().map(|m| m.id.0).collect::<Vec<_>>();
    d.ok("ui.click", json!({"id": format!("timeline.marker.{first}"), "fx": 0.95}));
    d.frames(2);
    d.exec("markers.clearCurrent", json!({}));
    assert_eq!(markers(&mut d), vec![second], "the clicked marker is cleared");
    d.ok("ui.click", json!({"id": format!("timeline.marker.{second}"), "fx": 0.95, "button": "right"}));
    d.frames(3);
    d.click("timeline.markerMenu.markers.clearCurrent");
    assert!(markers(&mut d).is_empty(), "cleared from its context menu");
}

#[test]
fn shift_semicolon_goes_to_the_next_gap() {
    let mut d = Driver::demo();
    d.exec("markers.markIn", json!({"frame": 48}));
    d.exec("markers.markOut", json!({"frame": 71}));
    d.exec("sequence.lift", json!({}));
    d.exec("playhead.set", json!({"frame": 0}));
    // the key a US layout sends for Shift+; is `:`
    d.harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Colon,
        physical_key: Some(egui::Key::Semicolon),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    d.frames(3);
    let ph = d.exec("sequence.inspect", json!({}))["playhead"].clone();
    let want = d.exec("playhead.set", json!({"frame": 48}))["time"].clone();
    assert_eq!(ph, want);
}

fn first_movie(v: &Value) -> Option<u64> {
    match v {
        Value::Object(m) => {
            if let Some(id) = m.get("item").and_then(Value::as_u64)
                && m.get("type").and_then(Value::as_str) == Some("Movie")
            {
                return Some(id);
            }
            m.values().find_map(first_movie)
        }
        Value::Array(a) => a.iter().find_map(first_movie),
        _ => None,
    }
}

/// On-screen x of a clip's in and out edges in the timeline (not clipped to the panel).
fn clip_span(d: &mut Driver, clip: u64) -> (f64, f64) {
    let x = |d: &mut Driver, edge: &str| d.ok("ui.timeline.locate", json!({"clip": clip, "edge": edge}))["x"].as_f64().expect("x");
    (x(d, "in"), x(d, "out"))
}

#[test]
fn new_empty_sequence_fits_its_first_clip() {
    let mut d = Driver::demo();
    let project = d.exec("project.inspect", json!({}));
    let item = first_movie(&project).expect("a movie in the demo project");
    // an empty sequence fits its 1 s minimum; the clip placed into it afterwards is fitted again
    d.exec("file.newSequence", json!({"name": "Empty"}));
    d.frames(30);
    let clip = d.exec("timeline.place", json!({"item": item, "track": "V1", "seconds": 0}))["clips"][0].as_u64().expect("clip");
    d.frames(60);
    let (x_in, x_out) = clip_span(&mut d, clip);
    assert!(x_out < 1600.0 && x_out - x_in > 500.0, "clip spans x {x_in}..{x_out}: not fitted to the 1600 px window");

    // a zoom chosen while the sequence is still empty is kept
    d.exec("file.newSequence", json!({"name": "Zoomed"}));
    d.frames(30);
    d.ok("ui.set", json!({"timeline": {"pps": 300.0}}));
    d.frames(30);
    let clip = d.exec("timeline.place", json!({"item": item, "track": "V1", "seconds": 0}))["clips"][0].as_u64().expect("clip");
    d.frames(60);
    let secs = d.exec("sequence.inspect", json!({}))["video"][0]["items"][0]["duration"].as_f64().expect("duration") / 254_016_000_000.0;
    let (x_in, x_out) = clip_span(&mut d, clip);
    assert!(((x_out - x_in) - (secs * 300.0 - 4.0)).abs() < 2.0, "zoom not kept: {} px for {secs} s at 300 px/s", x_out - x_in);
}

#[test]
fn later_clips_keep_the_zoom_once_the_sequence_has_content() {
    let mut d = Driver::demo();
    let project = d.exec("project.inspect", json!({}));
    let item = first_movie(&project).expect("a movie in the demo project");
    d.exec("file.newSequence", json!({"name": "Grows"}));
    d.frames(30);
    d.exec("timeline.place", json!({"item": item, "track": "V1", "seconds": 0}));
    d.frames(60);
    // the editor zooms in; a second clip arriving must not re-fit (only the first clip into an
    // empty sequence does)
    d.ok("ui.set", json!({"timeline": {"pps": 300.0}}));
    d.frames(30);
    let end = d.exec("sequence.inspect", json!({}))["duration"].as_f64().expect("duration") / 254_016_000_000.0;
    let clip = d.exec("timeline.place", json!({"item": item, "track": "V2", "seconds": end}))["clips"][0].as_u64().expect("clip");
    d.frames(60);
    let q = d.exec("sequence.inspect", json!({}));
    let secs = q["video"][1]["items"][0]["duration"].as_f64().expect("duration") / 254_016_000_000.0;
    let (x_in, x_out) = clip_span(&mut d, clip);
    assert!(((x_out - x_in) - (secs * 300.0 - 4.0)).abs() < 2.0, "zoom changed: {} px for {secs} s at 300 px/s", x_out - x_in);
}

impl Driver {
    /// A clipboard shortcut as Windows and Linux deliver it: egui-winit sends Ctrl+C/X/V as
    /// `Event::Copy`/`Cut`/`Paste`, not as a key press (#199).
    fn clipboard_shortcut(&mut self, ev: egui::Event, modifiers: egui::Modifiers) {
        self.harness.input_mut().events.extend([egui::Event::ModifiersChanged(modifiers), ev]);
        self.frames(1);
        self.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        self.frames(2);
    }

    /// Names of every clip in the active sequence's video tracks.
    fn video_clip_names(&mut self) -> Vec<String> {
        let q = self.app().session.active_sequence().unwrap();
        q.video_tracks.iter().flat_map(|t| t.items.iter().map(|i| i.name.clone())).collect()
    }
}

#[test]
fn ctrl_c_and_ctrl_v_copy_and_paste_clips() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let q = d.app().session.active_sequence().unwrap().clone();
    let first = q.video_tracks[0].items[0].clone();
    let end = q.video_tracks.iter().chain(&q.audio_tracks).flat_map(|t| t.items.iter().map(|i| i.end())).max().unwrap();
    let copies = |d: &mut Driver| d.video_clip_names().iter().filter(|n| **n == first.name).count();
    let before = copies(&mut d);
    d.exec("timeline.select", json!({"clips": [first.id.0]}));

    d.clipboard_shortcut(egui::Event::Copy, egui::Modifiers::COMMAND);
    assert!(!d.app().session.state.clipboard.is_empty(), "Ctrl+C copies the selected clip");

    d.exec("playhead.set", json!({"time": end.0}));
    d.clipboard_shortcut(egui::Event::Paste(first.name.clone()), egui::Modifiers::COMMAND);
    assert_eq!(copies(&mut d), before + 1, "Ctrl+V pastes it at the playhead");
}

#[test]
fn ctrl_x_cuts_clips() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let first = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].clone();
    d.exec("timeline.select", json!({"clips": [first.id.0]}));
    d.clipboard_shortcut(egui::Event::Cut, egui::Modifiers::COMMAND);
    assert!(!d.app().session.state.clipboard.is_empty());
    assert!(d.app().session.active_sequence().unwrap().find_item(first.id).is_none(), "the cut clip leaves the timeline");
}

/// Sequence ▸ Sequence Settings… opens Premiere Pro's dialog (it used to do nothing, #172): the
/// General tab shows the sequence's settings, the ones FilmCraft can't change yet are shown (greyed
/// out), and the Color Management and VR Properties tabs are there. Opening and cancelling change
/// nothing.
#[test]
fn sequence_settings_dialog_shows_the_sequence() {
    let mut d = Driver::demo();
    let before = (*d.app().session.project).clone();
    let history = d.app().session.history.undo.len();
    let r = d.ok("ui.menu.invoke", json!({"id": "sequence.settings"}));
    assert_eq!(r["dialog"], "sequenceSettings", "{r}");
    d.frames(3);
    for (id, shown) in [
        ("sequenceSettings.name", "Main Edit"),
        ("sequenceSettings.editingMode", "Custom"),
        ("sequenceSettings.timebase", "23.976 frames/second"),
        ("sequenceSettings.width", "1920"),
        ("sequenceSettings.height", "1080"),
        ("sequenceSettings.aspect", "16:9"),
        ("sequenceSettings.scaleMotion", "true"),
        ("sequenceSettings.par", "Square Pixels (1.0)"),
        ("sequenceSettings.fields", "No Fields (Progressive Scan)"),
        ("sequenceSettings.videoDisplay", "23.976 fps Timecode"),
        ("sequenceSettings.channelFormat", "Stereo"),
        ("sequenceSettings.channels", "2"),
        ("sequenceSettings.sampleRate", "48000 Hz"),
        ("sequenceSettings.maxRenderQuality", "false"),
        ("sequenceSettings.linearColor", "true"),
    ] {
        assert_eq!(d.label(id), shown, "{id}");
    }
    for id in ["sequenceSettings.tab.general", "sequenceSettings.tab.color", "sequenceSettings.tab.vr", "sequenceSettings.ok", "sequenceSettings.cancel"] {
        assert!(d.ids(id).iter().any(|i| i == id), "{id} missing");
    }
    d.snapshot("sequence-settings-general", "sequenceSettings.");
    // the timebase list offers the common rates
    d.click("sequenceSettings.timebase");
    let rates = d.ids("sequenceSettings.timebase.option.");
    for r in ["24000/1001", "24/1", "25/1", "30000/1001", "60/1"] {
        assert!(rates.iter().any(|i| i.ends_with(r)), "{r} missing: {rates:?}");
    }
    d.click("sequenceSettings.timebase.option.24000/1001");
    d.click("sequenceSettings.tab.color");
    assert_eq!(d.label("sequenceSettings.workingSpace"), "Rec. 709");
    assert_eq!(d.label("sequenceSettings.autoToneMap"), "true");
    d.snapshot("sequence-settings-color", "sequenceSettings.");
    d.click("sequenceSettings.tab.vr");
    assert_eq!(d.label("sequenceSettings.vr.projection"), "None");
    d.snapshot("sequence-settings-vr", "sequenceSettings.");
    d.click("sequenceSettings.cancel");
    assert!(d.ids("sequenceSettings.").is_empty(), "dialog closed");
    assert_eq!(*d.app().session.project, before);
    assert_eq!(d.app().session.history.undo.len(), history, "no undo step");
    // OK without a change closes it and adds no undo step either
    d.ok("ui.menu.invoke", json!({"id": "sequence.settings"}));
    d.frames(3);
    d.click("sequenceSettings.ok");
    assert!(d.ids("sequenceSettings.").is_empty());
    assert_eq!(d.app().session.history.undo.len(), history);
}

/// OK applies the dialog in one undo step: a new timebase (29.97 starts on drop-frame timecode), a
/// smaller frame with the clips' Motion scaled to match, and Maximum Render Quality.
#[test]
fn sequence_settings_dialog_applies_in_one_undo_step() {
    let mut d = Driver::demo();
    let before = (*d.app().session.project).clone();
    let motion = |d: &mut Driver| {
        let q = d.app().session.active_sequence().unwrap();
        let m = q.video_tracks.iter().flat_map(|t| &t.items).find_map(|i| i.effect("motion")).unwrap().clone();
        match (&m.param("position").unwrap().value, &m.param("scale").unwrap().value) {
            (filmcraft_engine::project::ParamValue::Vec2(p), filmcraft_engine::project::ParamValue::Float(s)) => (p.x, p.y, *s),
            _ => panic!("Motion has a point and a number"),
        }
    };
    let (x0, y0, s0) = motion(&mut d);
    d.ok("ui.menu.invoke", json!({"id": "sequence.settings"}));
    // the window sizes itself over its first frames; click once it has settled
    d.frames(10);
    d.click("sequenceSettings.timebase");
    d.snapshot("sequence-settings-timebase-list", "sequenceSettings.");
    d.click("sequenceSettings.timebase.option.30000/1001");
    assert_eq!(d.label("sequenceSettings.timebase"), "29.97 frames/second");
    assert_eq!(d.label("sequenceSettings.videoDisplay"), "29.97 fps Drop-Frame Timecode");
    // the frame-size fields are drag values; set the draft as typing would
    d.app().ui.sequence_settings.width = 1280;
    d.app().ui.sequence_settings.height = 720;
    d.frames(2);
    assert_eq!(d.label("sequenceSettings.aspect"), "16:9");
    d.click("sequenceSettings.maxRenderQuality");
    d.click("sequenceSettings.ok");
    assert!(d.ids("sequenceSettings.").is_empty(), "dialog closed");
    let st = d.app().session.active_sequence().unwrap().settings.clone();
    assert_eq!((st.width, st.height, st.frame_rate), (1280, 720, filmcraft_engine::time::FrameRate::FPS_29_97));
    assert!(st.drop_frame && st.max_render_quality);
    let (x1, y1, s1) = motion(&mut d);
    let k = 2.0 / 3.0;
    assert!((x1 - x0 * k).abs() < 1e-9 && (y1 - y0 * k).abs() < 1e-9 && (s1 - s0 * k).abs() < 1e-9, "{x0},{y0},{s0} -> {x1},{y1},{s1}");
    d.exec("edit.undo", json!({}));
    assert_eq!(*d.app().session.project, before, "one undo step for the whole dialog");
}

/// The General tab's Sequence Name renames the sequence (its Project panel item) together with
/// the other settings, in the same undo step; a blank name keeps the old one.
#[test]
fn sequence_settings_dialog_renames_the_sequence() {
    let mut d = Driver::demo();
    let id = d.app().session.state.active_sequence.unwrap();
    let name = |d: &mut Driver| d.app().session.project.item(id).unwrap().name.clone();
    assert_eq!(name(&mut d), "Main Edit");
    d.ok("ui.menu.invoke", json!({"id": "sequence.settings"}));
    d.frames(10);
    assert_eq!(d.label("sequenceSettings.name"), "Main Edit");
    // the name field is a text edit; set the draft as typing would
    d.app().ui.sequence_settings.name = "Rough Cut".into();
    d.app().ui.sequence_settings.max_render_quality = true;
    d.frames(2);
    d.click("sequenceSettings.ok");
    assert!(d.ids("sequenceSettings.").is_empty(), "dialog closed");
    assert_eq!(name(&mut d), "Rough Cut");
    assert!(d.app().session.active_sequence().unwrap().settings.max_render_quality);
    d.exec("edit.undo", json!({}));
    assert_eq!(name(&mut d), "Main Edit", "one undo step for the name and the settings");
    assert!(!d.app().session.active_sequence().unwrap().settings.max_render_quality);
    // a blank name changes nothing
    let history = d.app().session.history.undo.len();
    d.ok("ui.menu.invoke", json!({"id": "sequence.settings"}));
    d.frames(10);
    d.app().ui.sequence_settings.name = "   ".into();
    d.frames(2);
    d.click("sequenceSettings.ok");
    assert_eq!(name(&mut d), "Main Edit");
    assert_eq!(d.app().session.history.undo.len(), history);
}

/// #1824: right-clicking a sequence in the Project panel offers Sequence Settings…, which opens the
/// dialog on that sequence even when another one is active in the Timeline (it opens it first).
#[test]
fn project_panel_sequence_menu_opens_its_settings() {
    let mut d = Driver::demo();
    let main = d.app().session.state.active_sequence.unwrap();
    let vertical = d.exec("file.newSequence", json!({"name": "Vertical", "width": 1080, "height": 1920}))["sequence"].as_u64().expect("sequence id");
    d.exec("sequence.open", json!({"item": main.0}));
    assert_eq!(d.app().session.state.active_sequence, Some(main));
    d.ok("ui.panel.show", json!({"panel": "Project"}));
    d.exec("project.view.set", json!({"view": "list"}));
    d.frames(4);
    let row = format!("project.item.{vertical}");
    assert!(d.ids(&row).contains(&row), "{:?}", d.ids("project.item."));
    d.ok("ui.click", json!({"id": row, "button": "right"}));
    d.frames(3);
    d.click("project.itemMenu.sequenceSettings");
    d.frames(3);
    assert!(matches!(d.app().dialog, Some(filmcraft_ui_egui::Dialog::SequenceSettings)));
    assert_eq!(d.label("sequenceSettings.name"), "Vertical");
    assert_eq!((d.label("sequenceSettings.width"), d.label("sequenceSettings.height")), ("1080".into(), "1920".into()));
    assert_eq!(d.app().session.state.active_sequence.map(|i| i.0), Some(vertical));
}

/// File ▸ New ▸ Sequence… (Cmd+N) made a default sequence at once; it opens New Sequence: the
/// Sequence Settings dialog from the default settings, named like the command would name it, with
/// a Tracks tab. OK makes the sequence with what was chosen (one undo step); Cancel makes none.
#[test]
fn new_sequence_opens_its_dialog() {
    let mut d = Driver::demo();
    let sequences = |d: &mut Driver| d.app().session.project.sequences().count();
    let n0 = sequences(&mut d);
    let r = d.ok("ui.menu.invoke", json!({"id": "file.newSequence"}));
    assert_eq!(r["dialog"], "newSequence", "{r}");
    d.frames(10);
    assert_eq!(sequences(&mut d), n0, "nothing is made before OK");
    assert_eq!(d.label("sequenceSettings.name"), format!("Sequence {:02}", n0 + 1));
    assert_eq!(d.label("sequenceSettings.timebase"), "23.976 frames/second");
    assert!(d.ids("sequenceSettings.tab.").iter().any(|i| i == "sequenceSettings.tab.tracks"));
    assert!(d.ids("sequenceSettings.tab.color").is_empty(), "New Sequence has General and Tracks");
    d.click("sequenceSettings.tab.tracks");
    assert_eq!(d.label("sequenceSettings.tracks.video"), "3");
    assert_eq!(d.label("sequenceSettings.tracks.audio"), "3");
    d.snapshot("new-sequence-tracks", "sequenceSettings.");
    // set the draft as typing would: a vertical 30 fps sequence with 2 video and 4 audio tracks
    {
        let s = &mut d.app().ui.sequence_settings;
        s.name = "Shorts".into();
        (s.width, s.height) = (1080, 1920);
        (s.fps_num, s.fps_den) = (30, 1);
        (s.video_tracks, s.audio_tracks) = (2, 4);
    }
    d.frames(2);
    d.click("sequenceSettings.ok");
    assert!(d.ids("sequenceSettings.").is_empty(), "dialog closed");
    assert_eq!(sequences(&mut d), n0 + 1);
    let id = d.app().session.state.active_sequence.unwrap();
    assert_eq!(d.app().session.project.item(id).unwrap().name, "Shorts", "the new sequence is the open one");
    let q = d.app().session.active_sequence().unwrap().clone();
    assert_eq!((q.settings.width, q.settings.height, q.settings.frame_rate), (1080, 1920, filmcraft_engine::time::FrameRate::FPS_30));
    assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (2, 4));
    d.exec("edit.undo", json!({}));
    assert_eq!(sequences(&mut d), n0, "one undo step");
    // Cancel makes nothing
    d.ok("ui.menu.invoke", json!({"id": "file.newSequence"}));
    d.frames(10);
    d.click("sequenceSettings.cancel");
    assert_eq!(sequences(&mut d), n0);
    // with params (a clip, an agent) the command still makes the sequence directly
    d.ok("ui.menu.invoke", json!({"id": "file.newSequence", "params": {"name": "Direct"}}));
    d.frames(2);
    assert_eq!(sequences(&mut d), n0 + 1);
    // and Sequence Settings… is still the active sequence's dialog, with its three tabs
    d.ok("ui.menu.invoke", json!({"id": "sequence.settings"}));
    d.frames(10);
    assert_eq!(d.label("sequenceSettings.name"), "Direct");
    assert!(d.ids("sequenceSettings.tab.color").iter().any(|i| i == "sequenceSettings.tab.color"));
    d.click("sequenceSettings.cancel");
}

#[test]
fn timeline_marker_label_edits_only_the_clicked_marker_and_is_undoable() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let target = d.exec("markers.add", json!({"frame":30,"name":"Review","comment":"Keep this note"}))["marker"].as_u64().unwrap();
    d.exec("markers.add", json!({"frame":90,"name":"Other","color":"Rose"}));
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    d.exec("timeline.select", json!({"clips":[clip.0]}));
    let before = d.app().session.active_sequence().unwrap().clone();
    let undo = d.app().session.history.undo.len();
    d.right_marker(target, false);
    assert!(d.ids("timeline.markerMenu.").contains(&"timeline.markerMenu.label".into()), "a marker needs its own Label menu");
    d.click("timeline.markerMenu.label");
    assert_eq!(d.ids("timeline.markerMenu.color.").len(), filmcraft_project::Label::ALL.len());
    d.snapshot("marker-colour-menu", "timeline.markerMenu.");
    d.click("timeline.markerMenu.color.Blue");
    let mut expected = before.clone();
    expected.markers.iter_mut().find(|m| m.id.0 == target).unwrap().color = filmcraft_project::Label::Blue;
    let actual = d.app().session.active_sequence().unwrap().clone();
    assert_eq!(serde_json::to_value(&actual).unwrap(), serde_json::to_value(&expected).unwrap(), "only the chosen marker changes, not a selected clip");
    assert_eq!(d.app().session.history.undo.len(), undo + 1);
    d.exec("edit.undo", json!({}));
    assert_eq!(serde_json::to_value(d.app().session.active_sequence().unwrap()).unwrap(), serde_json::to_value(&before).unwrap());
    d.exec("edit.redo", json!({}));
    assert_eq!(serde_json::to_value(d.app().session.active_sequence().unwrap()).unwrap(), serde_json::to_value(&expected).unwrap());
}

#[test]
fn range_marker_end_flags_keep_their_owner_in_timeline_and_monitors() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let point = d.exec("markers.add", json!({"frame":10,"name":"Point"}))["marker"].as_u64().unwrap();
    let range = d.exec("markers.addRange", json!({"frame":30,"durationFrames":48,"name":"Split range","color":"#AB56CD"}))["marker"].as_u64().unwrap();
    assert!(d.ids(&format!("timeline.markerEnd.{point}")).is_empty());
    assert!(d.ids(&format!("program.markerEnd.{point}")).is_empty());
    d.snapshot("range-marker-timeline", "timeline.marker");
    d.snapshot("range-marker-program", "program.scrubBar");
    for (panel, menu) in [("timeline", "timeline.markerMenu.properties"), ("program", "monitor.markerMenu.properties")] {
        d.ok("ui.click", json!({"id":format!("{panel}.markerEnd.{range}"),"button":"right"}));
        d.frames(3);
        d.click(menu);
        assert_eq!(d.app().ui.marker_properties.as_ref().unwrap().marker, range);
        d.click("markerProperties.cancel");
    }
    // A zoomed ruler still shows the right half when the range's start is offscreen.
    let seq = d.app().session.active_sequence().unwrap();
    let marker = seq.markers.iter().find(|m| m.id.0 == range).unwrap();
    let middle = (marker.start.0 as f64 + marker.duration.0 as f64 / 2.0) / seq.duration().0 as f64;
    d.harness.state_mut().ui.program.time_zoom = Some([middle, 1.0]);
    d.frames(3);
    assert!(d.ids(&format!("program.marker.{range}")).is_empty());
    assert!(!d.ids(&format!("program.markerEnd.{range}")).is_empty());
    d.snapshot("range-marker-program-clipped", "program.scrubBar");
    d.exec("source.open", json!({"item":5}));
    let source = d.exec("markers.add", json!({"target":"source","time":filmcraft_time::TICKS_PER_SECOND,"durationFrames":48,"name":"Source range"}))["marker"]
        .as_u64()
        .unwrap();
    d.frames(3);
    d.snapshot("range-marker-source", "source.scrubBar");
    d.ok("ui.click", json!({"id":format!("source.markerEnd.{source}"),"button":"right"}));
    d.frames(3);
    d.click("monitor.markerMenu.properties");
    let draft = d.app().ui.marker_properties.as_ref().unwrap();
    assert_eq!(draft.marker, source);
    assert_eq!(draft.owner, filmcraft_ui_egui::panels::marker_properties::Owner::Source(5));
}

#[test]
fn a_range_marker_band_opens_its_menu_and_keeps_the_marker_identity() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let other = d.exec("markers.add", json!({"frame":30,"name":"Underneath","color":"Rose"}))["marker"].as_u64().unwrap();
    let target = d.exec("markers.addRange", json!({"frame":30,"durationFrames":48,"name":"Review range"}))["marker"].as_u64().unwrap();
    let before = d.app().session.active_sequence().unwrap().clone();
    d.right_marker(target, true);
    assert!(
        d.ids("timeline.markerMenu.").contains(&"timeline.markerMenu.markers.clearCurrent".into()),
        "right-clicking the range band must open the marker menu"
    );
    assert!(!d.ids("timeline.clipMenu.").iter().any(|id| id == "timeline.clipMenu.edit.cut"));
    d.exec("playhead.set", json!({"frame":0}));
    d.click("timeline.markerMenu.label");
    d.click("timeline.markerMenu.color.Yellow");
    let mut expected = before;
    expected.markers.iter_mut().find(|m| m.id.0 == target).unwrap().color = filmcraft_project::Label::Yellow;
    let actual = d.app().session.active_sequence().unwrap();
    assert_eq!(actual.markers.iter().find(|m| m.id.0 == other).unwrap().color, filmcraft_project::Label::Rose);
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(&expected).unwrap(),
        "target the clicked id, even when starts coincide or the playhead changes"
    );
    d.snapshot("marker-range-colour", "timeline.markerRange.");
}

#[test]
fn markers_panel_label_refreshes_the_colour_filter_in_both_themes() {
    for theme in ["dark", "light"] {
        let mut d = Driver::demo();
        d.ok("ui.set", json!({"theme":theme}));
        d.exec("markers.clearAll", json!({}));
        let target = d.exec("markers.add", json!({"frame":30,"name":"Review colour"}))["marker"].as_u64().unwrap();
        d.exec("markers.filterColors", json!({"color":"Blue","visible":false}));
        d.ok("ui.panel.show", json!({"panel":"Markers"}));
        d.frames(3);
        d.ok("ui.click", json!({"id":format!("markers.row.{target}"),"button":"right"}));
        d.frames(3);
        d.click("markers.menu.label");
        assert_eq!(d.ids("markers.menu.color.").len(), filmcraft_project::Label::ALL.len());
        d.snapshot(&format!("marker-panel-label-{theme}"), "markers.menu.");
        let undo = d.app().session.history.undo.len();
        d.click("markers.menu.color.Blue");
        assert_eq!(d.app().session.active_sequence().unwrap().markers[0].color, filmcraft_project::Label::Blue);
        assert_eq!(d.app().session.history.undo.len(), undo + 1);
        assert!(d.ids("markers.row.").is_empty(), "the newly blue marker follows the existing filter");
        d.exec("edit.undo", json!({}));
        d.frames(3);
        assert_eq!(d.ids("markers.row."), vec![format!("markers.row.{target}")]);
    }
}

#[test]
fn timeline_clip_label_is_disabled_without_a_selected_clip() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    d.exec("timeline.select", json!({"clips":[]}));
    // empty space above the top video track, where no track row is (the ruler has a menu of its
    // own, #1535, and an empty spot in a row opens the gap menu)
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 30.0}}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"timeline.tracks","fx":0.5,"fy":0.02,"button":"right"}));
    d.frames(3);
    let enabled = |d: &mut Driver| {
        let elements = d.ok("ui.elements", json!({"prefix":"timeline.clipMenu.edit.label"}));
        elements[0]["enabled"].clone()
    };
    assert_eq!(enabled(&mut d), json!(false));
    let undo = d.app().session.history.undo.len();
    d.click("timeline.clipMenu.edit.label");
    assert!(d.ids("timeline.clipMenu.label.").is_empty());
    assert_eq!(d.app().session.history.undo.len(), undo);
    d.ok("ui.key", json!({"key":"Escape"}));
    let clip = d.app().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    d.ok("ui.click", json!({"id":format!("timeline.clip.{}",clip.0),"fx":0.5,"button":"right"}));
    d.frames(3);
    assert_eq!(enabled(&mut d), json!(true));
    d.click("timeline.clipMenu.edit.label");
    d.click("timeline.clipMenu.label.Blue");
    assert_eq!(d.app().session.active_sequence().unwrap().find_item(clip).unwrap().1.label, filmcraft_project::Label::Blue);
    assert!(d.app().session.active_sequence().unwrap().markers.is_empty());
}

/// New Sequence stays open when the engine rejects the frame size so the rest of the draft is not lost.
#[test]
fn new_sequence_stays_open_on_invalid_frame_size() {
    let mut d = Driver::demo();
    let n0 = d.app().session.project.sequences().count();
    d.ok("ui.menu.invoke", json!({"id": "file.newSequence"}));
    d.frames(10);
    {
        let s = &mut d.app().ui.sequence_settings;
        s.name = "Keep Me".into();
        // within the dialog's per-side range, over the engine's pixel budget
        (s.width, s.height) = (20_000, 20_000);
    }
    d.frames(2);
    d.click("sequenceSettings.ok");
    d.frames(2);
    assert_eq!(d.app().session.project.sequences().count(), n0, "nothing is made");
    assert!(!d.ids("sequenceSettings.").is_empty(), "dialog stays open: {}", d.app().ui.status);
    assert_eq!(d.label("sequenceSettings.name"), "Keep Me");
    assert_eq!(d.label("sequenceSettings.width"), "20000");
    assert_eq!(d.label("sequenceSettings.height"), "20000");
}
