//! Headless UI tests of editing with nested sequences: the nest toggle button, Reveal in Project
//! and the clip menu's Multi-Camera submenu.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::dock::PanelKind;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks and keys enter through the input hook
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
        self.frames(3);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

#[test]
fn the_nest_button_toggles_how_sequences_are_edited_in() {
    let mut d = Driver::new();
    assert!(!d.app().session.state.sequences_as_clips, "on by default: sequences nest");
    d.click("timeline.toggle.nest");
    assert!(d.app().session.state.sequences_as_clips);
    d.click("timeline.toggle.nest");
    assert!(!d.app().session.state.sequences_as_clips);
}

#[test]
fn reveal_in_project_shows_the_clips_item_in_the_project_panel() {
    use filmcraft_engine::project_panel::ViewMode;
    for mode in [ViewMode::List, ViewMode::Icon] {
        let mut d = Driver::new();
        d.app().session.prefs.project_panel.view.mode = mode;
        let (clip, item) = {
            let it = &d.app().session.active_sequence().unwrap().video_tracks[0].items[2];
            (it.id.0, it.item)
        };
        let bin = d.app().session.project.root.parent_of(item).expect("the demo keeps its footage in a bin").0;
        // the Project panel is closed, its bins are collapsed and a search hides everything
        d.app().ui.dock.apply(craft_ui::docking::Action::Close { panel: PanelKind::Project }).unwrap();
        d.app().ui.expanded_bins.clear();
        d.app().ui.project_search = "no such thing".into();
        d.frames(2);
        assert!(!d.has(&format!("project.item.{}", item.0)));
        d.exec("clip.revealInProject", json!({"clip": clip}));
        d.frames(3);
        assert!(filmcraft_ui_egui::dock::LayoutExt::is_visible(&d.app().ui.dock, PanelKind::Project), "{mode:?}");
        assert!(d.app().ui.project_search.is_empty());
        assert_eq!(d.app().session.state.project_selection, vec![item]);
        if mode == ViewMode::List {
            // the list shows the tree with the item's bin opened
            assert!(d.app().ui.expanded_bins.contains(&bin));
            assert_eq!(d.app().ui.project_panel.bin, None);
        } else {
            // icons show one bin at a time: the panel is inside the item's bin
            assert_eq!(d.app().ui.project_panel.bin, Some(bin));
        }
        assert!(d.has(&format!("project.item.{}", item.0)), "{mode:?}: the item is on show");
    }
}

/// Whether the Project panel shows `item` highlighted and on screen: its highlight (the row or
/// card, cut to what the panel shows) holds the row's label chip (List) or the card's thumbnail
/// (Icon), so a row scrolled out of sight does not count.
fn revealed_on_show(d: &mut Driver, item: u64, mode: filmcraft_engine::project_panel::ViewMode) -> bool {
    let v = d.ok("ui.elements", json!({"prefix": format!("project.item.{item}")}));
    let rect = |id: String| {
        v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).map(|e| {
            let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
            egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
        })
    };
    let marker = match mode {
        filmcraft_engine::project_panel::ViewMode::List => format!("project.item.{item}.label"),
        _ => format!("project.item.{item}"),
    };
    match (rect(format!("project.item.{item}.selected")), rect(marker)) {
        (Some(sel), Some(m)) => sel.height() > 0.0 && sel.contains_rect(m),
        _ => false,
    }
}

#[test]
fn reveal_in_project_scrolls_to_and_selects_a_clip_in_an_open_folder() {
    // #4760: with the folder already open, Reveal in Project selected the source item but left the
    // panel where it was, so a row or card out of sight never showed as selected
    use filmcraft_engine::project_panel::ViewMode;
    for mode in [ViewMode::List, ViewMode::Icon] {
        let mut d = Driver::new();
        d.app().session.prefs.project_panel.view.mode = mode;
        let (clip, item) = {
            let it = d.app().session.active_sequence().unwrap().video_tracks[0].items.last().unwrap();
            (it.id.0, it.item)
        };
        if mode == ViewMode::List {
            // the clip's media sits in a folder below the others, and every folder is open
            let bin = d.exec("file.newBin", json!({"name": "Transfers"}))["bin"].as_u64().unwrap();
            d.exec("project.moveToBin", json!({"items": [item.0], "bin": bin}));
            let bins: Vec<u64> = d
                .app()
                .session
                .project
                .root
                .children
                .iter()
                .filter_map(|c| if let filmcraft_project::BinEntry::Bin(b) = c { Some(b.id.0) } else { None })
                .collect();
            d.app().ui.expanded_bins = bins;
        } else {
            // large cards, one to a row, with the panel already inside the clip's folder
            d.app().session.prefs.project_panel.view.icon_size = 300.0;
            let bin = d.app().session.project.root.parent_of(item).unwrap().0;
            d.app().ui.project_panel.bin = Some(bin);
        }
        // the item is out of sight: selecting it does not show it
        d.exec("project.select", json!({"items": [item.0]}));
        d.frames(3);
        assert!(!revealed_on_show(&mut d, item.0, mode), "{mode:?}: the test needs the item out of sight");
        d.exec("project.select", json!({"items": []}));
        d.frames(2);
        if mode == ViewMode::List {
            // and the list is scrolled sideways past the Name column, which Reveal brings back
            let first_bin = d.app().ui.expanded_bins[0];
            d.ok("ui.scroll", json!({"id": format!("project.bin.{first_bin}"), "dx": -4000.0}));
            d.frames(3);
            let names = d.ok("ui.elements", json!({"prefix": "project.list.header.Name"}));
            assert!(names.as_array().unwrap().is_empty(), "the test needs the Name column scrolled out of sight");
        }

        d.ok("ui.click", json!({"id": format!("timeline.clip.{clip}"), "button": "right"}));
        d.frames(3);
        d.click("timeline.clipMenu.clip.revealInProject");
        d.frames(4);
        assert_eq!(d.app().session.state.project_selection, vec![item], "{mode:?}");
        assert!(revealed_on_show(&mut d, item.0, mode), "{mode:?}: the panel scrolls to the selected item");
    }
}

#[test]
fn the_clip_menu_offers_multi_camera_and_reveal_in_project() {
    let mut d = Driver::new();
    let (clip, item) = {
        let it = &d.app().session.active_sequence().unwrap().video_tracks[0].items[0];
        (it.id.0, it.item)
    };
    d.ok("ui.click", json!({"id": format!("timeline.clip.{clip}"), "button": "right"}));
    d.frames(3);
    assert!(d.has("timeline.clipMenu.clip.multicam"));
    assert!(d.has("timeline.clipMenu.clip.revealInProject"));
    d.click("timeline.clipMenu.clip.revealInProject");
    assert_eq!(d.app().session.state.project_selection, vec![item]);
}
