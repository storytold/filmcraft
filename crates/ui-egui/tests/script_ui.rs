//! The Script window keeps wrapped text, candle coverage and marks aligned in one scroll space.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ItemKind;
use filmcraft_time::{FrameRate, TICKS_PER_SECOND};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn new() -> Self {
        Self::with_size(egui::vec2(1600.0, 980.0))
    }

    fn with_size(size: egui::Vec2) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut builder = Harness::builder().with_size(size).with_step_dt(1.0 / 60.0).with_max_steps(10_000);
        if snapshots.is_some() {
            builder = builder.wgpu();
        }
        let harness = builder.build_eframe(move |_cc| app);
        let mut driver = Self { harness, tx, snapshots };
        driver.frames(4);
        driver
    }

    fn frames(&mut self, count: usize) {
        for _ in 0..count {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (request, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(request).expect("control channel");
        for _ in 0..600 {
            self.frames(1);
            if let Ok(value) = reply.try_recv() {
                assert_eq!(value["ok"], true, "{method} {params}: {value}");
                return value["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn rect(&mut self, id: &str) -> [f64; 4] {
        self.frames(2);
        let value = self.ok("ui.elements", json!({"prefix": id}));
        let element = value.as_array().expect("elements").iter().find(|entry| entry["id"] == id).unwrap_or_else(|| panic!("missing {id}: {value}"));
        let coordinates = element["rect"].as_array().expect("rect");
        [0, 1, 2, 3].map(|index| coordinates[index].as_f64().expect("coordinate"))
    }

    fn double_click(&mut self, id: &str) {
        let [x, y, width, height] = self.rect(id);
        let pos = egui::pos2((x + width * 0.5) as f32, (y + height * 0.5) as f32);
        self.frames(60);
        self.harness.input_mut().events.push(egui::Event::PointerMoved(pos));
        self.frames(1);
        for _ in 0..2 {
            for pressed in [true, false] {
                self.harness.input_mut().events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
                self.frames(1);
            }
        }
        self.frames(2);
    }

    fn snapshot(&mut self, name: &str) {
        let Some(directory) = self.snapshots.clone() else { return };
        std::fs::create_dir_all(&directory).expect("snapshot directory");
        self.frames(2);
        match self.harness.render() {
            Ok(image) => image.save(directory.join(format!("script-{name}.png"))).expect("save script snapshot"),
            Err(error) => eprintln!("script snapshot skipped: {error}"),
        }
    }
}

#[test]
fn wrapped_line_and_candle_share_height_and_find_chooses_a_take() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).expect("demo media").id.0;
    let long = "This is a deliberately long screenplay line. ".repeat(12);
    let mut text = format!("{long}\nA second, short line.");
    for index in 0..45 {
        text.push_str(&format!("\nScene filler {index}"));
    }
    text.push_str("\nFind this distinctive final scene");
    let script = driver.exec("script.importText", json!({"name": "Script QA", "text": text}))["item"].as_u64().expect("script id");
    let doc = driver.exec("script.inspect", json!({"item": script}));
    let first = doc["document"]["lines"][0]["id"].as_u64().expect("first line");
    let second = doc["document"]["lines"][1]["id"].as_u64().expect("second line");
    let last = doc["document"]["lines"].as_array().expect("lines").last().expect("last")["id"].as_u64().expect("last line id");
    let added = driver.exec("script.addTake", json!({"item": script, "source": source, "startLine": first, "endLine": second}));
    let take = added["take"].as_u64().expect("take");
    let slate = added["slate"].as_u64().expect("slate");
    let mark = driver.exec("script.addMark", json!({"item": script, "take": take, "line": second, "sourceTime": 0}))["mark"].as_u64().expect("mark");
    driver.ok("ui.menu.invoke", json!({"id": "window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id": "script.openPanel", "params": {"item": script}}));
    driver.frames(4);
    let row = driver.rect(&format!("script.line.{first}"));
    let candle = driver.rect(&format!("script.take.{take}.line.{first}"));
    assert!(row[3] > 30.0, "long logical line should wrap: {row:?}");
    assert!((row[1] - candle[1]).abs() < 1.0 && (row[3] - candle[3]).abs() < 1.0, "text and candle must stay aligned: {row:?} {candle:?}");
    let thumbnail = driver.rect(&format!("script.slate.{slate}.thumbnail"));
    assert!((thumbnail[2] - 96.0).abs() < 1.0, "setup thumbnail should retain a 16:9 picture width: {thumbnail:?}");
    let second_candle = driver.rect(&format!("script.take.{take}.line.{second}"));
    let mark_rect = driver.rect(&format!("script.mark.{mark}"));
    assert!(
        (mark_rect[0] + mark_rect[2] * 0.5 - second_candle[0] - second_candle[2] * 0.5).abs() < 1.0,
        "script mark belongs on the candle: {mark_rect:?} {second_candle:?}"
    );
    driver.ok("ui.click", json!({"id":format!("script.mark.{mark}")}));
    assert_eq!(driver.harness.state_mut().ui.script.selected_lines, Some((second, second)), "a clicked mark selects its full-width row");
    driver.ok("ui.click", json!({"id":"script.offscreen.toggle"}));
    let styled = driver.exec("script.inspect", json!({"item":script}));
    assert_eq!(styled["document"]["slates"][0]["takes"][0]["offscreen_lines"][0], second);
    driver.ok("ui.click", json!({"id":"script.offscreen.toggle"}));
    let cleared = driver.exec("script.inspect", json!({"item":script}));
    assert!(cleared["document"]["slates"][0]["takes"][0]["offscreen_lines"].as_array().is_none_or(Vec::is_empty));
    driver.ok("ui.click", json!({"id":"script.emptyCanvas"}));
    assert_eq!(driver.harness.state_mut().ui.script.selected_lines, None, "clicking empty script canvas clears the text selection");
    driver.snapshot("wrapped-dark");
    driver.ok("ui.set", json!({"theme": "light"}));
    driver.snapshot("wrapped-light");
    let second_take =
        driver.exec("script.addTake", json!({"item": script, "source": source, "startLine": second, "endLine": second}))["take"].as_u64().expect("second take");
    let result = driver.ok("ui.menu.invoke", json!({"id": "script.findInScript", "params": {"item": source}}));
    assert_eq!(result["matches"], 2);
    assert_eq!(result["chooser"], true);
    driver.frames(3);
    let _ = driver.rect("script.search");
    assert_ne!(take, second_take);
    driver.ok("ui.click", json!({"id": "script.search"}));
    driver.ok("ui.type", json!({"text": "distinctive final scene"}));
    driver.frames(4);
    let found = driver.rect(&format!("script.line.{last}"));
    assert!(found[3] >= 20.0, "search should scroll the matching logical line into view: {found:?}");
    driver.snapshot("search-light");
}

#[test]
fn candle_range_color_offscreen_and_selected_mark_retime() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).unwrap().id.0;
    let script = driver.exec("script.importText", json!({"name":"Range","text":"First\nSecond\nThird"}))["item"].as_u64().unwrap();
    let doc = driver.exec("script.inspect", json!({"item":script}));
    let lines: Vec<_> = doc["document"]["lines"].as_array().unwrap().iter().map(|line| line["id"].as_u64().unwrap()).collect();
    let take = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":lines[0],"endLine":lines[2]}))["take"].as_u64().unwrap();
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    driver.frames(4);
    driver.ok("ui.click", json!({"id":format!("script.take.{take}.line.{}", lines[0]),"fy":0.1}));
    assert_eq!(driver.harness.state_mut().ui.script.selected_lines, Some((lines[0], lines[0])));
    driver.ok("ui.click", json!({"id":format!("script.take.{take}.line.{}", lines[1]),"modifiers":{"shift":true}}));
    assert_eq!(driver.harness.state_mut().ui.script.selected_lines, Some((lines[0], lines[1])));
    driver.ok("ui.click", json!({"id":"script.color.apply","button":"right"}));
    driver.ok("ui.click", json!({"id":"script.color.rose"}));
    driver.ok("ui.click", json!({"id":"script.color.apply"}));
    driver.ok("ui.click", json!({"id":"script.offscreen.toggle"}));
    let styled = driver.exec("script.inspect", json!({"item":script}));
    let take_data = &styled["document"]["slates"][0]["takes"][0];
    assert_eq!(take_data["line_colors"].as_array().unwrap().len(), 2);
    assert_eq!(take_data["offscreen_lines"].as_array().unwrap().len(), 2);
    assert_eq!(take_data["line_colors"][0]["color"], "Rose");
    driver.snapshot("offscreen-selected");
    let mark = driver.exec("script.addMark", json!({"item":script,"take":take,"line":lines[1],"sourceTime":0}))["mark"].as_u64().unwrap();
    driver.ok("ui.click", json!({"id":format!("script.mark.{mark}")}));
    assert_eq!(driver.harness.state_mut().ui.script.selected_mark, Some(mark));
    let retime = FrameRate::FPS_24.tick_of(10).0;
    driver.exec("source.setPlayhead", json!({"time":retime}));
    let actual_retime = driver.harness.state_mut().session.state.source_playhead.0;
    assert!(actual_retime > 0);
    driver.ok("ui.click", json!({"id":"script.mark.add"}));
    assert_eq!(driver.harness.state_mut().ui.script.selected_mark, Some(mark));
    let retimed = driver.exec("script.inspect", json!({"item":script}));
    let marks = retimed["document"]["slates"][0]["takes"][0]["marks"].as_array().unwrap();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0]["source_time"], actual_retime);
}

#[test]
fn alt_lasso_copies_rows_and_setup_to_another_script() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).unwrap().id.0;
    let original = driver.exec("script.importText", json!({"name":"First","text":"A\nB\nC"}))["item"].as_u64().unwrap();
    let rows: Vec<_> = driver.exec("script.inspect", json!({"item":original}))["document"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["id"].as_u64().unwrap())
        .collect();
    let added = driver.exec("script.addTake", json!({"item":original,"source":source,"startLine":rows[0],"endLine":rows[2]}));
    let slate = added["slate"].as_u64().unwrap();
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":original}}));
    driver.frames(4);
    let thumb = driver.rect(&format!("script.slate.{slate}.thumbnail"));
    let last = driver.rect(&format!("script.line.{}", rows[2]));
    driver.ok(
        "ui.drag",
        json!({"from":{"x":thumb[0]+2.0,"y":thumb[1]+2.0},
        "to":{"x":last[0]+2.0,"y":last[1]+last[3]*0.5},"steps":12,"modifiers":{"alt":true}}),
    );
    assert!(driver.harness.state_mut().ui.script.selected_section_slates.contains(&slate));
    assert_eq!(driver.harness.state_mut().ui.script.selected_lines, Some((rows[0], rows[2])));
    driver.ok("ui.key", json!({"key":"C","command":true}));
    let target = driver.exec("script.importText", json!({"name":"Second","text":"Destination"}))["item"].as_u64().unwrap();
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":target}}));
    driver.ok("ui.key", json!({"key":"V","command":true}));
    let pasted = driver.exec("script.inspect", json!({"item":target}));
    assert_eq!(pasted["document"]["lines"].as_array().unwrap().len(), 4);
    assert_eq!(pasted["document"]["slates"].as_array().unwrap().len(), 1);
    assert_eq!(pasted["document"]["slates"][0]["takes"][0]["source"], source);
}

#[test]
fn command_dragging_thumbnail_moves_all_candles_and_keeps_mark_time() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).unwrap().id.0;
    let script = driver.exec("script.importText", json!({"name":"Move Setup","text":"A\nB\nC\nD\nE\nF\nG\nH\nI"}))["item"].as_u64().unwrap();
    let rows: Vec<_> = driver.exec("script.inspect", json!({"item":script}))["document"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["id"].as_u64().unwrap())
        .collect();
    let added = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":rows[2],"endLine":rows[3]}));
    let slate = added["slate"].as_u64().unwrap();
    let first_take = added["take"].as_u64().unwrap();
    driver.exec("script.addTake", json!({"item":script,"slate":slate,"source":source,"startLine":rows[3],"endLine":rows[4]}));
    driver.exec("script.addMark", json!({"item":script,"take":first_take,"line":rows[3],"sourceTime":12345}));
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    let thumb = driver.rect(&format!("script.slate.{slate}.thumbnail"));
    let first_box = driver.rect(&format!("script.take.{first_take}.metadata"));
    assert!((first_box[1] - (thumb[1] + thumb[3])).abs() < 2.0, "take number should hang from the thumbnail");
    let row = driver.rect(&format!("script.line.{}", rows[3]));
    driver.ok(
        "ui.drag",
        json!({"from":{"x":thumb[0]+thumb[2]*0.5,"y":thumb[1]+thumb[3]*0.5},
        "to":{"x":thumb[0]+thumb[2]*0.5,"y":thumb[1]+thumb[3]*0.5+row[3]*2.0},"steps":12,"modifiers":{"command":true}}),
    );
    let updated = driver.exec("script.inspect", json!({"item":script}));
    let takes = updated["document"]["slates"][0]["takes"].as_array().unwrap();
    let moved_to = takes[0]["start_line"].as_u64().unwrap();
    let moved_index = rows.iter().position(|id| *id == moved_to).unwrap();
    assert!(moved_index > 2, "command drag should move the first candle: {updated}");
    let delta = moved_index - 2;
    assert_eq!(takes[0]["end_line"], rows[3 + delta]);
    assert_eq!(takes[1]["start_line"], rows[3 + delta]);
    assert_eq!(takes[1]["end_line"], rows[4 + delta]);
    assert_eq!(takes[0]["marks"][0]["line"], rows[3 + delta]);
    assert_eq!(takes[0]["marks"][0]["source_time"], 12345);
}

#[test]
fn candle_context_menu_reorders_takes_under_one_thumbnail() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).unwrap().id.0;
    let script = driver.exec("script.importText", json!({"name":"Order","text":"A\nB\nC"}))["item"].as_u64().unwrap();
    let lines = driver.exec("script.inspect", json!({"item":script}))["document"]["lines"].as_array().unwrap().clone();
    let first = lines[0]["id"].as_u64().unwrap();
    let middle = lines[1]["id"].as_u64().unwrap();
    let last = lines[2]["id"].as_u64().unwrap();
    let first_added = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":first,"endLine":last}));
    let slate = first_added["slate"].as_u64().unwrap();
    let first_take = first_added["take"].as_u64().unwrap();
    let second_take =
        driver.exec("script.addTake", json!({"item":script,"slate":slate,"source":source,"startLine":first,"endLine":last}))["take"].as_u64().unwrap();
    let third_take =
        driver.exec("script.addTake", json!({"item":script,"slate":slate,"source":source,"startLine":first,"endLine":last}))["take"].as_u64().unwrap();
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    driver.ok("ui.click", json!({"id":format!("script.take.{second_take}.line.{middle}"),"button":"right"}));
    driver.ok("ui.click", json!({"id":format!("script.take.{second_take}.moveLeft")}));
    let moved = driver.exec("script.inspect", json!({"item":script}));
    let takes = moved["document"]["slates"][0]["takes"].as_array().unwrap();
    assert_eq!(takes.iter().map(|take| take["id"].as_u64().unwrap()).collect::<Vec<_>>(), [second_take, first_take, third_take]);
    let left = driver.rect(&format!("script.take.{second_take}.line.{first}"));
    let right = driver.rect(&format!("script.take.{first_take}.line.{first}"));
    assert!(left[0] < right[0]);
    driver.exec("edit.undo", json!({}));
    let restored = driver.exec("script.inspect", json!({"item":script}));
    assert_eq!(restored["document"]["slates"][0]["takes"][0]["id"], first_take);
}

#[test]
fn selected_thumbnail_shuttles_independently_for_each_candle() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).unwrap().id.0;
    let script = driver.exec("script.importText", json!({"name":"Shuttle","text":"A\nB"}))["item"].as_u64().unwrap();
    let first = driver.exec("script.inspect", json!({"item":script}))["document"]["lines"][0]["id"].as_u64().unwrap();
    let added = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":first,"endLine":first}));
    let slate = added["slate"].as_u64().unwrap();
    let first_take = added["take"].as_u64().unwrap();
    let second_take =
        driver.exec("script.addTake", json!({"item":script,"slate":slate,"source":source,"startLine":first,"endLine":first}))["take"].as_u64().unwrap();
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    driver.ok("ui.click", json!({"id":format!("script.slate.{slate}.thumbnail")}));
    driver.ok("ui.key", json!({"key":"ArrowRight","shift":true}));
    let first_state = driver.exec("script.inspect", json!({"item":script}));
    let takes = first_state["document"]["slates"][0]["takes"].as_array().unwrap();
    assert_eq!(takes[0]["id"], first_take);
    assert!((takes[0]["thumbnail_time"].as_i64().unwrap() - TICKS_PER_SECOND).abs() < TICKS_PER_SECOND / 20);
    assert!(takes[1]["thumbnail_time"].is_null());
    driver.ok("ui.click", json!({"id":format!("script.take.{second_take}.line.{first}")}));
    driver.ok("ui.key", json!({"key":"ArrowRight","shift":true}));
    let second_state = driver.exec("script.inspect", json!({"item":script}));
    assert!(second_state["document"]["slates"][0]["takes"][1]["thumbnail_time"].as_i64().unwrap() > 0);
    assert_eq!(second_state["document"]["slates"][0]["takes"][0]["thumbnail_time"], takes[0]["thumbnail_time"]);
    driver.exec("edit.undo", json!({}));
    let undone = driver.exec("script.inspect", json!({"item":script}));
    assert!(undone["document"]["slates"][0]["takes"][1]["thumbnail_time"].is_null());
}

#[test]
fn script_mark_and_offscreen_shortcuts_work_from_a_selected_candle() {
    let mut driver = Driver::new();
    assert!(driver.harness.state_mut().session.shortcuts.for_command("script.addMarkAtSelection").is_empty());
    assert!(driver.harness.state_mut().session.shortcuts.for_command("script.toggleOffscreen").is_empty());
    driver.exec("shortcuts.set", json!({"command":"script.addMarkAtSelection","keys":"M","panel":"Script"}));
    driver.exec("shortcuts.set", json!({"command":"script.toggleOffscreen","keys":"Shift+M","panel":"Script"}));
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).unwrap().id.0;
    let script = driver.exec("script.importText", json!({"name":"Shortcuts","text":"A\nB\nC"}))["item"].as_u64().unwrap();
    let rows: Vec<_> = driver.exec("script.inspect", json!({"item":script}))["document"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["id"].as_u64().unwrap())
        .collect();
    let take = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":rows[0],"endLine":rows[2]}))["take"].as_u64().unwrap();
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    driver.ok("ui.click", json!({"id":format!("script.take.{take}.line.{}", rows[1])}));
    driver.harness.state_mut().session.state.source_item = None;
    driver.ok("ui.click", json!({"id":"script.mark.add"}));
    assert_eq!(driver.harness.state_mut().session.state.source_item.unwrap().0, source);
    let marked = driver.exec("script.inspect", json!({"item":script}));
    assert_eq!(marked["document"]["slates"][0]["takes"][0]["marks"].as_array().unwrap().len(), 1);
    driver.ok("ui.key", json!({"key":"M"}));
    let marked_again = driver.exec("script.inspect", json!({"item":script}));
    assert_eq!(marked_again["document"]["slates"][0]["takes"][0]["marks"].as_array().unwrap().len(), 2);
    driver.ok("ui.key", json!({"key":"M","shift":true}));
    let offscreen = driver.exec("script.inspect", json!({"item":script}));
    assert_eq!(offscreen["document"]["slates"][0]["takes"][0]["offscreen_lines"][0], rows[1]);
}

#[test]
fn narrow_script_panel_keeps_take_controls_accessible() {
    let mut driver = Driver::with_size(egui::vec2(620.0, 700.0));
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).expect("demo media").id.0;
    let script =
        driver.exec("script.importText", json!({"name": "Narrow Script", "text": "First line\nSecond line\nThird line"}))["item"].as_u64().expect("script");
    let doc = driver.exec("script.inspect", json!({"item": script}));
    let first = doc["document"]["lines"][0]["id"].as_u64().expect("first line");
    let third = doc["document"]["lines"][2]["id"].as_u64().expect("third line");
    let take = driver.exec("script.addTake", json!({"item": script, "source": source, "startLine": first, "endLine": third}))["take"].as_u64().expect("take");
    driver.ok("ui.menu.invoke", json!({"id": "window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id": "script.openPanel", "params": {"item": script}}));
    driver.harness.state_mut().ui.script.selected_lines = Some((first, first));
    driver.harness.state_mut().ui.script.selected_take = Some(take);
    driver.frames(4);
    let _ = driver.rect("script.scene.add");
    let _ = driver.rect("script.page.add");
    let _ = driver.rect("script.margin.left");
    let _ = driver.rect("script.margin.right");
    let _ = driver.rect(&format!("script.take.{take}.topHandle"));
    let _ = driver.rect(&format!("script.take.{take}.bottomHandle"));
    let number = driver.rect(&format!("script.take.{take}.metadata"));
    assert!(number[2] <= 20.0 && number[3] <= 20.0, "take number should be a small square: {number:?}");
    driver.snapshot("narrow-controls");
}

#[test]
fn repeated_source_uses_one_setup_thumbnail_and_multiple_candles() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).expect("demo media").id.0;
    let script =
        driver.exec("script.importText", json!({"name": "Reset", "text": "Action\nReset\nAction again\nAfter\nEnd"}))["item"].as_u64().expect("script");
    let doc = driver.exec("script.inspect", json!({"item": script}));
    let first = doc["document"]["lines"][0]["id"].as_u64().expect("first line");
    let second = doc["document"]["lines"][1]["id"].as_u64().expect("second line");
    let last = doc["document"]["lines"][2]["id"].as_u64().expect("last line");
    let fifth = doc["document"]["lines"][4]["id"].as_u64().expect("fifth line");
    driver.exec("script.setLabel", json!({"item": script, "line": first, "scene": "2A", "page": "7"}));
    driver.exec("script.setLabel", json!({"item": script, "line": second, "scene": "2B", "page": "8"}));
    let original = driver.exec("script.addTake", json!({"item": script, "source": source, "startLine": first, "endLine": last}));
    let slate = original["slate"].as_u64().expect("slate");
    let duplicate = driver.exec("script.addTake", json!({"item": script, "slate": slate, "source": source, "startLine": first, "endLine": last}));
    driver.ok("ui.menu.invoke", json!({"id": "window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id": "script.openPanel", "params": {"item": script}}));
    let scene_combo = driver.rect("script.scene.combo");
    let page_combo = driver.rect("script.page.combo");
    assert!(scene_combo[0] < page_combo[0], "Scene must precede Page in the toolbar");
    let scene_label = driver.rect(&format!("script.scene.{first}"));
    let first_row = driver.rect(&format!("script.line.{first}"));
    assert!(
        (14.0..=20.0).contains(&(scene_label[0] - first_row[0])),
        "scene label should sit close to, but separate from, the row number: {scene_label:?} {first_row:?}"
    );
    driver.ok("ui.click", json!({"id": "script.scene.combo"}));
    let _ = driver.rect(&format!("script.scene.option.{second}"));
    driver.ok("ui.click", json!({"id": format!("script.scene.option.{second}")}));
    assert_eq!(driver.harness.state_mut().ui.script.selected_lines, Some((second, second)));
    let thumbnail = driver.rect(&format!("script.slate.{slate}.thumbnail"));
    let first_candle = driver.rect(&format!("script.take.{}.line.{first}", original["take"].as_u64().expect("first take")));
    let second_candle = driver.rect(&format!("script.take.{}.line.{first}", duplicate["take"].as_u64().expect("second take")));
    let wrap_margin = driver.rect("script.margin.right");
    let page = driver.rect(&format!("script.page.{first}"));
    assert!(page[0] > wrap_margin[0], "page number belongs to the right of the wrap margin");
    assert!((page[0] - wrap_margin[0] - wrap_margin[2] * 0.5).abs() < 1.0, "page text should start at the right margin marker");
    driver.harness.state_mut().ui.script.text_column_width = Some(350.0);
    let moved_margin = driver.rect("script.margin.right");
    let moved_page = driver.rect(&format!("script.page.{first}"));
    let fixed_thumbnail = driver.rect(&format!("script.slate.{slate}.thumbnail"));
    let fixed_candle = driver.rect(&format!("script.take.{}.line.{first}", original["take"].as_u64().expect("first take")));
    assert!((moved_margin[0] - wrap_margin[0]).abs() > 20.0, "the wrap margin should move");
    assert!((moved_page[0] - page[0] - (moved_margin[0] - wrap_margin[0])).abs() < 1.0, "page number should follow the wrap margin");
    assert!((fixed_thumbnail[0] - thumbnail[0]).abs() < 1.0, "moving the wrap margin must not shift a thumbnail");
    assert!((fixed_candle[0] - first_candle[0]).abs() < 1.0, "moving the wrap margin must not shift a candle");
    assert!(second_candle[0] > first_candle[0], "the repeated clip has its own candle beneath one setup thumbnail");
    assert!((thumbnail[1] + thumbnail[3] - first_candle[1]).abs() < 1.0, "thumbnail bottom must meet a row boundary without stretching that row");
    assert!(second_candle[0] - first_candle[0] <= 32.0, "candles beneath one thumbnail should form a tight group");
    assert!(first_candle[3] <= 26.0, "a short text row must not grow to fit the thumbnail");
    driver.snapshot("multi-take-dark");
    driver.exec("script.addMark", json!({"item":script,"take":original["take"],"line":second,"sourceTime":100}));
    let first_row = driver.rect(&format!("script.line.{first}"));
    let last_row = driver.rect(&format!("script.line.{last}"));
    driver.ok(
        "ui.drag",
        json!({"from": {"id": format!("script.slate.{slate}.thumbnail")},
        "to": {"x": thumbnail[0] + thumbnail[2] * 0.5, "y": thumbnail[1] + thumbnail[3] * 0.5 + last_row[1] - first_row[1]},
        "steps": 12, "modifiers": {"command": true}}),
    );
    let moved = driver.exec("script.inspect", json!({"item": script}));
    for take in moved["document"]["slates"][0]["takes"].as_array().expect("takes") {
        assert_eq!(take["start_line"], last);
        assert_eq!(take["end_line"], fifth);
    }
    let moved_mark = &moved["document"]["slates"][0]["takes"][0]["marks"][0];
    assert_eq!(moved_mark["line"], doc["document"]["lines"][3]["id"]);
    assert_eq!(moved_mark["source_time"], 100);
    let moved_thumb = driver.rect(&format!("script.slate.{slate}.thumbnail"));
    let fifth_row = driver.rect(&format!("script.line.{fifth}"));
    driver.ok(
        "ui.drag",
        json!({"from": {"id": format!("script.slate.{slate}.thumbnail")},
        "to": {"x": moved_thumb[0] + moved_thumb[2] * 0.5, "y": moved_thumb[1] + moved_thumb[3] * 0.5 + fifth_row[1] - last_row[1]}, "steps": 12}),
    );
    let thumbnail_only = driver.exec("script.inspect", json!({"item": script}));
    assert_eq!(thumbnail_only["document"]["slates"][0]["thumbnail_line"], fifth);
    for take in thumbnail_only["document"]["slates"][0]["takes"].as_array().expect("takes") {
        assert_eq!(take["start_line"], last);
        assert_eq!(take["end_line"], fifth);
    }
    let original_take = original["take"].as_u64().expect("original take");
    driver.ok(
        "ui.drag",
        json!({"from": {"id": format!("script.take.{original_take}.topHandle")},
        "to": {"id": format!("script.line.{second}"), "fx": 0.5, "fy": 0.5},
        "steps": 12, "modifiers": {"command": true}}),
    );
    let resized = driver.exec("script.inspect", json!({"item": script}));
    assert_eq!(resized["document"]["slates"][0]["takes"][0]["start_line"], second);
    assert_eq!(resized["document"]["slates"][0]["takes"][0]["end_line"], fifth);
    driver.ok(
        "ui.drag",
        json!({"from": {"id": format!("script.slate.{slate}.thumbnail")},
        "to": {"id": format!("script.line.{second}"), "fx": 0.5, "fy": 0.5}, "steps": 12}),
    );
    let overlapped = driver.exec("script.inspect", json!({"item": script}));
    assert!(overlapped["document"]["slates"][0]["thumbnail_x"].as_f64().is_some(), "drag should store a horizontal thumbnail position");
    let thumb = driver.rect(&format!("script.slate.{slate}.thumbnail"));
    let text_row = driver.rect(&format!("script.line.{second}"));
    assert!(thumb[0] < text_row[0] + text_row[2], "thumbnail should be able to overlap the text area: thumb {thumb:?}, text {text_row:?}");
    driver.snapshot("thumbnail-over-text");
}

#[test]
fn dropping_repeated_source_on_script_row_creates_a_new_setup() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).expect("demo media").id.0;
    let script = driver.exec("script.importText", json!({"name":"Repeated source", "text":"First\nSecond\nThird"}))["item"].as_u64().expect("script");
    let inspected = driver.exec("script.inspect", json!({"item":script}));
    let lines = inspected["document"]["lines"].as_array().expect("lines");
    let first = lines[0]["id"].as_u64().expect("first");
    let second = lines[1]["id"].as_u64().expect("second");
    let original = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":first,"endLine":first}));
    let slate = original["slate"].as_u64().expect("slate");
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    driver.harness.state_mut().ui.script.selected_slate = Some(slate);
    if let Some(bin) = driver.harness.state_mut().session.project.root.parent_of(filmcraft_project::ItemId(source)) {
        driver.harness.state_mut().ui.project_panel.bin = Some(bin.0);
    }
    driver.frames(4);
    let project_item = format!("project.item.{source}");
    let target_row = format!("script.line.{second}");
    driver.rect(&project_item);
    driver.rect(&target_row);
    driver.ok("ui.drag", json!({"from":{"id":project_item},"to":{"id":target_row,"fx":0.6,"fy":0.5},"steps":16}));
    let result = driver.exec("script.inspect", json!({"item":script}));
    let slates = result["document"]["slates"].as_array().expect("slates");
    assert_eq!(slates.len(), 2, "a row drop must create a fresh thumbnail, even when one is selected");
    assert_ne!(slates[0]["id"], slates[1]["id"]);
    assert_eq!(slates[1]["takes"][0]["source"], source);
}

#[test]
fn candle_double_click_cues_source_start_or_interpolated_mark_time() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).expect("demo media").id.0;
    let script = driver.exec("script.importText", json!({"name":"Cue rows", "text":"First\nBetween\nLast"}))["item"].as_u64().expect("script");
    let doc = driver.exec("script.inspect", json!({"item":script}));
    let lines = doc["document"]["lines"].as_array().expect("lines");
    let first = lines[0]["id"].as_u64().expect("first");
    let middle = lines[1]["id"].as_u64().expect("middle");
    let last = lines[2]["id"].as_u64().expect("last");
    let take = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":first,"endLine":last}))["take"].as_u64().expect("take");
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    let before_click = (driver.harness.state_mut().session.state.source_item, driver.harness.state_mut().session.state.source_playhead);
    driver.ok("ui.click", json!({"id":format!("script.take.{take}.line.{middle}")}));
    assert_eq!((driver.harness.state_mut().session.state.source_item, driver.harness.state_mut().session.state.source_playhead), before_click);
    driver.double_click(&format!("script.take.{take}.line.{middle}"));
    assert_eq!(driver.harness.state_mut().session.state.source_item.map(|id| id.0), Some(source));
    assert_eq!(driver.harness.state_mut().session.state.source_playhead.0, 0);
    let a = FrameRate::FPS_24.tick_of(24).0;
    let b = FrameRate::FPS_24.tick_of(72).0;
    driver.exec("script.addMark", json!({"item":script,"take":take,"line":first,"sourceTime":a}));
    driver.exec("script.addMark", json!({"item":script,"take":take,"line":last,"sourceTime":b}));
    driver.frames(60);
    driver.ok("ui.click", json!({"id":format!("script.take.{take}.line.{middle}")}));
    assert_eq!(driver.harness.state_mut().session.state.source_playhead.0, 0);
    driver.double_click(&format!("script.take.{take}.line.{middle}"));
    let expected = filmcraft_engine::clip_ops::source_view(&driver.harness.state_mut().session, filmcraft_project::ItemId(source))
        .expect("source view")
        .rate
        .snap(FrameRate::FPS_24.tick_of(48));
    assert_eq!(driver.harness.state_mut().session.state.source_playhead, expected);
}

#[test]
fn command_dragging_a_mark_changes_its_line_without_changing_source_time() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).expect("demo media").id.0;
    let script = driver.exec("script.importText", json!({"name":"Marks","text":"A\nB\nC\nD"}))["item"].as_u64().expect("script");
    let doc = driver.exec("script.inspect", json!({"item":script}));
    let lines = doc["document"]["lines"].as_array().expect("lines");
    let ids: Vec<u64> = lines.iter().map(|line| line["id"].as_u64().expect("line id")).collect();
    let take = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":ids[0],"endLine":ids[3]}))["take"].as_u64().expect("take");
    let mark = driver.exec("script.addMark", json!({"item":script,"take":take,"line":ids[1],"sourceTime":0}))["mark"].as_u64().expect("mark");
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    driver.frames(4);
    driver.ok("ui.drag", json!({"from":{"id":format!("script.mark.{mark}")},"to":{"id":format!("script.line.{}", ids[2]),"fx":0.5,"fy":0.5},"steps":12,"modifiers":{"command":true}}));
    let moved = driver.exec("script.inspect", json!({"item":script}));
    let stored = &moved["document"]["slates"][0]["takes"][0]["marks"][0];
    assert_eq!(stored["line"], ids[2]);
    assert_eq!(stored["source_time"], 0);
}

#[test]
fn enter_in_script_text_splits_the_numbered_row_at_the_caret() {
    let mut driver = Driver::new();
    let script = driver.exec("script.importText", json!({"name": "Split", "text": "Door locked\nNext line"}))["item"].as_u64().expect("script");
    let doc = driver.exec("script.inspect", json!({"item": script}));
    let first = doc["document"]["lines"][0]["id"].as_u64().expect("first line");
    driver.ok("ui.menu.invoke", json!({"id": "window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id": "script.openPanel", "params": {"item": script}}));
    driver.harness.state_mut().ui.script.edit_line = Some(first);
    driver.harness.state_mut().ui.script.edit_text = "Door locked".into();
    driver.frames(3);
    driver.ok("ui.click", json!({"id": format!("script.line.{first}.edit")}));
    driver.ok("ui.key", json!({"key": "Home"}));
    for _ in 0..4 {
        driver.ok("ui.key", json!({"key": "ArrowRight"}));
    }
    driver.ok("ui.key", json!({"key": "Enter"}));
    let updated = driver.exec("script.inspect", json!({"item": script}));
    let rows = updated["document"]["lines"].as_array().expect("rows");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["text"], "Door");
    assert_eq!(rows[1]["text"], " locked");
    assert_eq!(rows[2]["text"], "Next line");
}

#[test]
fn backspace_at_row_start_joins_text_and_drops_only_that_rows_marks() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).expect("demo media").id.0;
    let script = driver.exec("script.importText", json!({"name":"Join","text":"First\n second\nThird"}))["item"].as_u64().expect("script");
    let doc = driver.exec("script.inspect", json!({"item":script}));
    let first = doc["document"]["lines"][0]["id"].as_u64().expect("first");
    let second = doc["document"]["lines"][1]["id"].as_u64().expect("second");
    let third = doc["document"]["lines"][2]["id"].as_u64().expect("third");
    let take = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":first,"endLine":third}))["take"].as_u64().expect("take");
    let kept = driver.exec("script.addMark", json!({"item":script,"take":take,"line":first,"sourceTime":0}))["mark"].as_u64().expect("mark");
    driver.exec("script.addMark", json!({"item":script,"take":take,"line":second,"sourceTime":100}));
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    driver.harness.state_mut().ui.script.edit_line = Some(second);
    driver.harness.state_mut().ui.script.edit_text = " second".into();
    driver.frames(3);
    driver.ok("ui.click", json!({"id":format!("script.line.{second}.edit")}));
    driver.ok("ui.key", json!({"key":"Home"}));
    driver.ok("ui.key", json!({"key":"Backspace"}));
    let updated = driver.exec("script.inspect", json!({"item":script}));
    let lines = updated["document"]["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["text"], "First second");
    assert_eq!(lines[0]["id"], first);
    assert_eq!(updated["document"]["slates"][0]["takes"][0]["marks"].as_array().expect("marks").len(), 1);
    assert_eq!(updated["document"]["slates"][0]["takes"][0]["marks"][0]["id"], kept);
    assert_eq!(driver.harness.state_mut().ui.script.edit_line, Some(first));
    driver.frames(2);
    driver.ok("ui.type", json!({"text":"!"}));
    driver.ok("ui.click", json!({"id":format!("script.line.{third}")}));
    let continued = driver.exec("script.inspect", json!({"item":script}));
    assert_eq!(continued["document"]["lines"][0]["text"], "First! second", "editing should continue at the removed line break");
}

#[test]
fn search_steps_through_repeated_terms_and_wraps() {
    let mut driver = Driver::new();
    let script =
        driver.exec("script.importText", json!({"name":"Repeated","text":"Morning\nEvening\nMorning again\nMorning"}))["item"].as_u64().expect("script");
    let doc = driver.exec("script.inspect", json!({"item":script}));
    let matches: Vec<u64> = [0, 2, 3].map(|index| doc["document"]["lines"][index]["id"].as_u64().expect("line")).to_vec();
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    driver.ok("ui.click", json!({"id":"script.search"}));
    driver.ok("ui.type", json!({"text":"morning"}));
    assert_eq!(driver.harness.state_mut().ui.script.selected_lines, Some((matches[0], matches[0])));
    let _ = driver.rect("script.search.count");
    for expected in [matches[1], matches[2], matches[0]] {
        driver.ok("ui.click", json!({"id":"script.search.next"}));
        assert_eq!(driver.harness.state_mut().ui.script.selected_lines, Some((expected, expected)));
    }
    driver.ok("ui.click", json!({"id":"script.search.previous"}));
    assert_eq!(driver.harness.state_mut().ui.script.selected_lines, Some((matches[2], matches[2])));
}

#[test]
fn opening_script_panel_preserves_workspace_and_empty_state_has_actions() {
    let mut driver = Driver::new();
    driver.ok("ui.menu.invoke", json!({"id": "window.panel.Script"}));
    let state = driver.ok("ui.inspect", json!({}));
    assert_eq!(state["ui"]["workspace"], "Editing");
    let _ = driver.rect("script.new");
    let _ = driver.rect("script.import");
    driver.ok("ui.click", json!({"id":"script.settings"}));
    let _ = driver.rect("script.settings.textSize");
    let _ = driver.rect("script.settings.thumbnailSize");
    driver.ok("ui.click", json!({"id":"script.settings.rowColors"}));
    assert_eq!(driver.harness.state_mut().ui.script.show_row_colors, Some(false));
    driver.ok("ui.menu.invoke", json!({"id": "window.workspace.script"}));
    driver.ok("ui.click", json!({"id":"script.new"}));
    let item = driver.harness.state_mut().ui.script.item.expect("new script item");
    let created = driver.exec("script.inspect", json!({"item":item}));
    assert_eq!(created["document"]["lines"].as_array().unwrap().len(), 1);
    assert!(driver.ok("ui.elements", json!({"prefix":"script.line.add"})).as_array().unwrap().is_empty());
    driver.snapshot("empty-dark");
}

#[test]
fn thumbnail_size_slider_resizes_the_picture_and_its_layout() {
    let mut driver = Driver::new();
    let source = driver.harness.state_mut().session.project.items.values().find(|item| matches!(item.kind, ItemKind::Media(_))).unwrap().id.0;
    let script = driver.exec("script.importText", json!({"name":"Thumbnail size","text":"\n\n\n\n\nAction\nDialogue"}))["item"].as_u64().unwrap();
    let doc = driver.exec("script.inspect", json!({"item":script}));
    let lines = doc["document"]["lines"].as_array().unwrap();
    let first = lines[5]["id"].as_u64().unwrap();
    let last = lines[6]["id"].as_u64().unwrap();
    let slate = driver.exec("script.addTake", json!({"item":script,"source":source,"startLine":first,"endLine":last}))["slate"].as_u64().unwrap();
    driver.ok("ui.menu.invoke", json!({"id":"window.workspace.script"}));
    driver.ok("ui.menu.invoke", json!({"id":"script.openPanel","params":{"item":script}}));
    let id = format!("script.slate.{slate}.thumbnail");
    let before = driver.rect(&id);
    driver.ok("ui.click", json!({"id":"script.settings"}));
    driver.ok("ui.drag", json!({"from":{"id":"script.settings.thumbnailSize","fx":0.3},"to":{"id":"script.settings.thumbnailSize","fx":0.8},"steps":12}));
    let after = driver.rect(&id);
    assert!(after[2] > before[2] + 10.0, "thumbnail slider should enlarge the picture: {before:?} -> {after:?}");
    assert!((after[3] - (after[2] * 9.0 / 16.0 + 16.0)).abs() < 1.0, "picture remains 16:9 above its chin: {after:?}");
}
