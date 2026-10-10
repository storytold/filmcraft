//! Headless UI tests of the M12.7 Project panel and Media Browser: List view (columns, sorting,
//! resizing, inline rename), Icon view (hover scrub sets In / Out, thumbnail size), Freeform view
//! (dragging places a card, stacks), the panel menu and Metadata Display… dialog, view presets,
//! bins opened in place / in a tab / in a window, and the Media Browser (navigation, Favorites,
//! Edit Columns…, import, drag to the Project panel, Open In Source Monitor).
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render offscreen with wgpu and write
//! `project-*.png` / `media-browser-*.png`.

#![allow(dead_code)]

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project_panel::{FREEFORM_POS, FREEFORM_STACK, ViewMode};
use filmcraft_project::{BinEntry, ItemId};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn with(session: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d
    }

    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        Self::with(session)
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

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn has(&mut self, id: &str) -> bool {
        self.ids(id).iter().any(|i| i == id)
    }

    fn rect(&mut self, id: &str) -> [f32; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r = &e["rect"];
        [r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32]
    }

    fn label(&mut self, id: &str) -> String {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).and_then(|e| e["label"].as_str().map(str::to_string)).unwrap_or_default()
    }

    /// Step frames until `id` is registered.
    fn wait_for(&mut self, id: &str) {
        for _ in 0..400 {
            if self.has(id) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            self.frames(1);
        }
        panic!("{id} never appeared: {:?}", self.ids(id.split('.').next().unwrap()));
    }

    fn key(&mut self, key: &str) {
        self.ok("ui.key", json!({"key": key}));
        self.frames(2);
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// The demo's Footage bin and its media items.
    fn footage(&mut self) -> (u64, Vec<u64>) {
        let p = &self.app().session.project;
        let bin = p
            .root
            .children
            .iter()
            .find_map(|e| match e {
                BinEntry::Bin(b) if b.name == "Footage" => Some(b.clone()),
                _ => None,
            })
            .expect("Footage bin");
        let items = bin.children.iter().filter_map(|e| if let BinEntry::Item(i) = e { Some(i.0) } else { None }).collect();
        (bin.id.0, items)
    }

    fn snapshot(&mut self, name: &str, prefix: Option<&str>) {
        let Some(dir) = self.snapshots.clone() else { return };
        let crop = prefix.map(|p| self.rect(p));
        self.frames(2);
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        let ppp = img.width() as f32 / 1600.0;
        let img = match crop {
            Some([x, y, w, h]) => {
                let (x0, y0) = ((x * ppp).max(0.0) as u32, (y * ppp).max(0.0) as u32);
                let (x1, y1) = ((((x + w) * ppp) as u32).min(img.width()), (((y + h) * ppp) as u32).min(img.height()));
                image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image()
            }
            None => img,
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        img.save(&path).unwrap();
        eprintln!("snapshot: {}", path.display());
    }
}

/// The demo project with the Project panel maximized (room for the List view's columns).
fn project_driver() -> Driver {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Project"}));
    d.ok("ui.set", json!({"focused": "Project"}));
    d.ok("ui.menu.invoke", json!({"id": "window.maximizeFrame"}));
    d.frames(3);
    d
}

/// Double-click a bin (synthetic input runs one event per frame, too slow for a double-click, so
/// this sends what the double-click does: open as Settings ▸ General ▸ Bins says).
fn double_click_bin(d: &mut Driver, bin: u64, alt: bool) {
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin, "alt": alt}}));
    d.frames(3);
}

#[test]
fn list_view_columns_sort_resize_and_rename() {
    let mut d = project_driver();
    d.exec("project.view.set", json!({"view": "list"}));
    let (bin, items) = d.footage();
    d.frames(2);
    // expand the Footage bin
    d.click(&format!("project.bin.{bin}.toggle"));
    assert!(d.app().ui.expanded_bins.contains(&bin));
    d.wait_for(&format!("project.item.{}", items[0]));
    for c in ["Name", "Frame Rate", "Media Start", "Media Duration"] {
        assert!(d.has(&format!("project.list.header.{c}")), "{c} header");
    }
    assert!(d.has(&format!("project.item.{}.label", items[0])), "label chip");
    let y = |d: &mut Driver, i: u64| d.rect(&format!("project.item.{i}"))[1];
    // Name ascending: rows ordered by name
    let mut by_name: Vec<(String, u64)> = items.iter().map(|i| (d.app().session.project.item(ItemId(*i)).unwrap().name.to_lowercase(), *i)).collect();
    by_name.sort();
    let (first, last) = (by_name[0].1, by_name.last().unwrap().1);
    assert!(y(&mut d, first) < y(&mut d, last));
    d.snapshot("project-list", Some("panel.Project"));
    // clicking the Name header reverses the order
    d.click("project.list.header.Name");
    assert!(d.app().session.prefs.project_panel.view.sort.descending);
    d.frames(2);
    assert!(y(&mut d, first) > y(&mut d, last));
    // drag the Frame Rate column wider (saved in preferences)
    let w0 = d.app().session.prefs.project_panel.view.columns.iter().find(|c| c.name == "Frame Rate").unwrap().width;
    let r = d.rect("project.list.resize.Frame Rate");
    let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + 60.0, "y": cy}}));
    d.frames(3);
    let w1 = d.app().session.prefs.project_panel.view.columns.iter().find(|c| c.name == "Frame Rate").unwrap().width;
    assert!(w1 > w0 + 30.0, "{w0} → {w1}");
    // inline rename: Rename, type, Enter (one undo step)
    d.exec("project.select", json!({"items": [items[0]]}));
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.rename"}));
    d.frames(3);
    assert!(d.has("project.rename"));
    d.key("Cmd+A");
    d.ok("ui.type", json!({"text": "Hero shot"}));
    d.frames(2);
    d.key("Enter");
    d.frames(2);
    assert_eq!(d.app().session.project.item(ItemId(items[0])).unwrap().name, "Hero shot");
    assert!(d.app().ui.project_panel.rename.is_none());
    d.exec("edit.undo", json!({}));
    assert_ne!(d.app().session.project.item(ItemId(items[0])).unwrap().name, "Hero shot");
}

#[test]
fn icon_view_hover_scrub_sets_in_and_out() {
    let mut d = project_driver();
    d.exec("project.view.set", json!({"view": "icon"}));
    let (bin, items) = d.footage();
    // open the Footage bin in place (Settings ▸ Bins ▸ Double-click: Open in place)
    d.click(&format!("project.bin.{bin}"));
    double_click_bin(&mut d, bin, false);
    assert_eq!(d.app().ui.project_panel.bin, Some(bin));
    assert!(d.label("project.breadcrumb").ends_with("/ Footage"));
    let card = format!("project.item.{}", items[0]);
    d.wait_for(&card);
    d.ok("ui.set", json!({"focused": "Project"}));
    // hover a quarter in, press I; three quarters in, press O
    d.ok("ui.move", json!({"id": card, "fx": 0.25, "fy": 0.5}));
    d.frames(2);
    let h = d.app().ui.project_panel.hover.expect("hover scrub");
    assert_eq!(h.item, items[0]);
    d.key("I");
    d.ok("ui.move", json!({"id": card, "fx": 0.75, "fy": 0.5}));
    d.frames(2);
    d.key("O");
    let it = d.app().session.project.item(ItemId(items[0])).unwrap().clone();
    let m = it.as_media().unwrap();
    let (i, o) = (m.mark_in.expect("In set"), m.mark_out.expect("Out set"));
    let dur = it.duration().0 as f64;
    assert!((i.0 as f64 / dur - 0.25).abs() < 0.05, "In at {}", i.0 as f64 / dur);
    assert!((o.0 as f64 / dur - 0.75).abs() < 0.05, "Out at {}", o.0 as f64 / dur);
    d.frames(2);
    assert!(d.has(&format!("project.item.{}.inOut", items[0])), "In/Out bar on the card");
    // the sequence's marks are untouched
    assert!(d.app().session.active_sequence().unwrap().mark_in.is_none());
    d.snapshot("project-icon", Some("panel.Project"));
    // Shift+] / Shift+[ change the thumbnail size
    let s0 = d.app().session.prefs.project_panel.view.icon_size;
    d.key("Shift+]");
    assert!(d.app().session.prefs.project_panel.view.icon_size > s0);
    d.key("Shift+[");
    assert!((d.app().session.prefs.project_panel.view.icon_size - s0).abs() < 1.0);
    // Hover Scrub off: no hover, I marks the sequence again
    d.key("Shift+H");
    assert!(!d.app().session.prefs.project_panel.hover_scrub);
    d.ok("ui.move", json!({"id": card, "fx": 0.5, "fy": 0.5}));
    d.frames(4);
    d.key("I");
    assert!(d.app().session.active_sequence().unwrap().mark_in.is_some());
    // up one level
    d.click("project.up");
    assert_eq!(d.app().ui.project_panel.bin, None);
}

#[test]
fn freeform_view_places_and_stacks_cards() {
    let mut d = project_driver();
    let (bin, items) = d.footage();
    d.exec("project.view.set", json!({"view": "freeform"}));
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin, "how": "inPlace"}}));
    let card = format!("project.item.{}", items[0]);
    d.wait_for(&card);
    assert!(d.has("project.freeform"));
    let r = d.rect(&card);
    let canvas = d.rect("project.freeform");
    let from = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let to = (from.0 + 140.0, from.1 + 90.0);
    d.ok("ui.drag", json!({"from": {"x": from.0, "y": from.1}, "to": {"x": to.0, "y": to.1}, "steps": 16}));
    d.frames(4);
    let pos = d.app().session.project.item(ItemId(items[0])).unwrap().metadata.get(FREEFORM_POS).cloned().expect("card placed");
    let (x, y, _) = filmcraft_engine::project_panel::parse_pos(&pos).unwrap();
    let (ex, ey) = (r[0] - canvas[0] + 140.0, r[1] - canvas[1] + 90.0);
    assert!((x - ex).abs() < 4.0 && (y - ey).abs() < 4.0, "placed at {x},{y}, expected {ex},{ey}");
    d.frames(2);
    let r2 = d.rect(&card);
    assert!((r2[0] - r[0] - 140.0).abs() < 4.0, "card drawn where it was dropped");
    d.snapshot("project-freeform", Some("panel.Project"));
    // the move is one undo step
    d.exec("edit.undo", json!({}));
    assert!(!d.app().session.project.item(ItemId(items[0])).unwrap().metadata.contains_key(FREEFORM_POS));
    // stack two cards from the card menu
    d.exec("project.select", json!({"items": [items[0], items[1]]}));
    d.ok("ui.click", json!({"id": card, "button": "right"}));
    d.frames(2);
    d.click("project.cardMenu.stack");
    assert_eq!(d.app().session.project.item(ItemId(items[1])).unwrap().metadata.get(FREEFORM_STACK).map(String::as_str), Some(items[0].to_string().as_str()));
    d.frames(2);
    assert!(d.has(&format!("project.stack.{}", items[0])), "×2 badge");
    // Freeform View Options… from the panel menu
    d.click("panel.menu.Project");
    d.click("project.menu.freeformOptions");
    assert!(d.has("projectDialog.snap"));
    d.click("projectDialog.snap");
    d.click("projectDialog.ok");
    assert!(d.app().session.prefs.project_panel.freeform.snap);
    assert!(d.app().ui.project_panel.dialog.is_none());
}

#[test]
fn metadata_display_dialog_and_view_presets() {
    let mut d = project_driver();
    d.exec("project.view.set", json!({"view": "list"}));
    let (bin, _) = d.footage();
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin, "how": "inPlace"}}));
    d.frames(2);
    // the panel menu lists Premiere's items
    d.click("panel.menu.Project");
    for id in [
        "project.menu.closeProject",
        "project.menu.newBin",
        "project.menu.newSearchBin",
        "project.menu.rename",
        "project.menu.automateToSequence",
        "project.menu.view.List",
        "project.menu.view.Freeform",
        "project.menu.previewArea",
        "project.menu.hoverScrub",
        "project.menu.fontSize",
        "project.menu.refreshSortOrder",
        "project.menu.metadataDisplay",
        "project.menu.saveViewPresetAs",
        "project.menu.restoreViewPreset",
        "project.menu.freeformOptions",
    ] {
        assert!(d.has(id), "{id}");
    }
    d.click("project.menu.metadataDisplay");
    d.frames(4);
    assert!(matches!(d.app().ui.project_panel.dialog, Some(filmcraft_ui_egui::panels::project::ProjectDialog::MetadataDisplay { .. })));
    d.click("projectDialog.field.Label");
    // fields further down: narrow the list with the search box
    d.click("projectDialog.filter");
    d.ok("ui.type", json!({"text": "usage"}));
    d.frames(2);
    d.click("projectDialog.field.Video Usage");
    d.click("projectDialog.filter");
    d.key("Cmd+A");
    d.ok("ui.type", json!({"text": "tape"}));
    d.frames(2);
    d.click("projectDialog.field.Tape Name");
    // move Label up next to Name
    for _ in 0..12 {
        if !d.has("projectDialog.up.Label") {
            break;
        }
        d.click("projectDialog.up.Label");
    }
    d.snapshot("project-metadata-display", None);
    d.click("projectDialog.ok");
    let cols: Vec<String> = d.app().session.prefs.project_panel.view.columns.iter().map(|c| c.name.clone()).collect();
    assert_eq!(cols[0], "Name");
    assert_eq!(cols[1], "Label", "{cols:?}");
    assert!(cols.contains(&"Video Usage".to_string()) && !cols.contains(&"Tape Name".to_string()));
    d.frames(2);
    assert!(d.has("project.list.header.Label") && d.has("project.list.header.Video Usage"));

    // Save As New View Preset (named), change the view, restore it from Manage
    d.click("panel.menu.Project");
    d.click("project.menu.saveViewPresetAs");
    d.ok("ui.set", json!({"projectPanel": {"dialog": {"savePresetAs": {"name": "Logging"}}}}));
    d.frames(2);
    d.click("projectDialog.ok");
    assert_eq!(d.app().session.prefs.project_panel.presets[0].as_ref().unwrap().name, "Logging");
    d.exec("project.view.set", json!({"view": "icon", "fontSize": "large"}));
    d.click("panel.menu.Project");
    d.click("project.menu.manageViewPresets");
    d.click("projectDialog.preset.1");
    d.click("projectDialog.restore");
    assert_eq!(d.app().session.prefs.project_panel.view.mode, ViewMode::List);
    d.click("projectDialog.ok");
    assert!(d.app().ui.project_panel.dialog.is_none());
    // Font Size and Preview Area
    d.exec("project.view.set", json!({"fontSize": "extraLarge", "previewArea": true}));
    let first = d.footage().1[0];
    d.exec("project.select", json!({"items": [first]}));
    d.frames(3);
    assert!(d.has("project.previewArea"));
    d.snapshot("project-list-preview-xl", Some("panel.Project"));
}

#[test]
fn bins_open_in_place_in_a_tab_or_in_a_window() {
    let mut d = project_driver();
    d.exec("project.view.set", json!({"view": "list"}));
    let (bin, items) = d.footage();
    d.app().session.prefs.general.bins_double_click = "openNewTab".into();
    d.frames(2);
    double_click_bin(&mut d, bin, false);
    assert_eq!(d.app().ui.project_panel.tabs.len(), 1);
    assert!(d.has("project.tab.0") && d.has("project.tab.project"));
    // the tab shows the bin's items under its own ids
    d.wait_for(&format!("projectBin.0.item.{}", items[0]));
    d.click("project.tab.project");
    assert_eq!(d.app().ui.project_panel.active_tab, None);
    d.click("project.tab.0.close");
    assert!(d.app().ui.project_panel.tabs.is_empty());
    // Opt+double-click: Open in New Window (a floating bin panel)
    double_click_bin(&mut d, bin, true);
    assert!(d.app().ui.project_panel.tabs.first().is_some_and(|t| t.floating), "{:?}", d.app().ui.project_panel.tabs);
    d.wait_for("projectBin.0.window");
    // a second Project panel on the project
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.newPanel"}));
    d.frames(3);
    assert!(d.has("projectBin.1.window"));
    d.snapshot("project-bin-windows", None);
    // inline rename of a bin
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.rename", "params": {"bin": bin}}));
    d.frames(3);
    d.key("Cmd+A");
    d.ok("ui.type", json!({"text": "Camera A"}));
    d.key("Enter");
    let name = d.app().session.project.root.find_bin(filmcraft_project::BinId(bin)).unwrap().name.clone();
    assert_eq!(name, "Camera A");
}

#[test]
fn select_all_takes_only_the_shown_bin() {
    // #456: Cmd+A selected every item in the project, closed bins and other tabs included
    let mut d = project_driver();
    d.exec("project.view.set", json!({"view": "list"}));
    let (bin, items) = d.footage();
    assert!(!items.is_empty());
    let sorted = |v: &[ItemId]| {
        let mut v: Vec<u64> = v.iter().map(|i| i.0).collect();
        v.sort();
        v
    };
    let mut footage = items.clone();
    footage.sort();
    // the root with the Footage bin closed: none of its items
    d.app().ui.expanded_bins.clear();
    d.frames(2);
    d.key("Cmd+A");
    let sel = d.app().session.state.project_selection.clone();
    assert!(sel.iter().all(|i| !items.contains(&i.0)), "{sel:?}");
    assert!(sel.iter().all(|i| d.app().session.project.root.children.contains(&BinEntry::Item(*i))), "{sel:?}");
    // twirled open in List view, its items are shown and count
    d.app().ui.expanded_bins.push(bin);
    d.frames(2);
    d.key("Cmd+A");
    let sel = d.app().session.state.project_selection.clone();
    assert!(items.iter().all(|i| sel.contains(&ItemId(*i))), "{sel:?}");
    d.key("Cmd+Shift+A");
    assert!(d.app().session.state.project_selection.is_empty());
    // the bin opened in its own tab: exactly its items
    d.app().ui.expanded_bins.clear();
    d.app().session.prefs.general.bins_double_click = "openNewTab".into();
    double_click_bin(&mut d, bin, false);
    assert_eq!(d.app().ui.project_panel.active_tab, Some(0));
    d.key("Cmd+A");
    assert_eq!(sorted(&d.app().session.state.project_selection), footage);
}

#[test]
fn a_dragged_bin_nests_in_another_and_back_out() {
    // #456: a bin stayed wherever it was created; dragging it onto another bin did nothing
    let mut d = project_driver();
    d.exec("project.view.set", json!({"view": "list"}));
    let outer = d.exec("file.newBin", json!({"name": "Outer"}))["bin"].as_u64().unwrap();
    let inner = d.exec("file.newBin", json!({"name": "Inner"}))["bin"].as_u64().unwrap();
    d.app().ui.expanded_bins.clear();
    d.frames(3);
    let parent =
        |d: &mut Driver, b: u64| filmcraft_ui_egui::panels::project::parent_bin(&d.app().session.project.root, filmcraft_project::BinId(b)).map(|p| p.0);
    let root = d.app().session.project.root.id.0;
    let drag = |d: &mut Driver, from: [f32; 4], to: (f32, f32)| {
        let start = (from[0] + 80.0, from[1] + from[3] / 2.0);
        d.ok("ui.drag", json!({"from": {"x": start.0, "y": start.1}, "to": {"x": to.0, "y": to.1}, "steps": 12}));
        d.frames(3);
    };
    // onto the Outer row: Inner nests in it
    let (from, to) = (d.rect(&format!("project.bin.{inner}")), d.rect(&format!("project.bin.{outer}")));
    drag(&mut d, from, (to[0] + 80.0, to[1] + to[3] / 2.0));
    assert_eq!(parent(&mut d, inner), Some(outer));
    // a bin never goes into itself
    d.app().ui.expanded_bins.push(outer);
    d.frames(3);
    let (from, to) = (d.rect(&format!("project.bin.{outer}")), d.rect(&format!("project.bin.{inner}")));
    drag(&mut d, from, (to[0] + 80.0, to[1] + to[3] / 2.0));
    assert_eq!(parent(&mut d, outer), Some(root));
    assert_eq!(parent(&mut d, inner), Some(outer));
    // onto empty space: back to the bin the view shows
    let from = d.rect(&format!("project.bin.{inner}"));
    let to = centre(d.rect("project.empty"));
    drag(&mut d, from, to);
    assert_eq!(parent(&mut d, inner), Some(root));
}

#[test]
fn footer_buttons_are_wired() {
    let mut d = project_driver();
    let bins0 = d.app().session.project.root.children.len();
    d.click("project.button.file.newBin");
    assert_eq!(d.app().session.project.root.children.len(), bins0 + 1);
    d.click("project.button.find");
    assert!(d.app().ui.extras.dialog.is_some(), "Find… dialog");
    d.app().ui.extras.dialog = None;
    d.click("project.button.new-item");
    d.wait_for("project.newItem.file.newColorMatte");
    let n0 = d.app().session.project.items.len();
    d.click("project.newItem.file.newColorMatte");
    // the New Color Matte dialog asks for the color first (#29)
    assert!(d.app().ui.extras.dialog.is_some(), "New Color Matte dialog");
    d.click("colorMatte.ok");
    assert_eq!(d.app().session.project.items.len(), n0 + 1);
    d.click("project.button.project.delete");
    assert_eq!(d.app().session.project.items.len(), n0, "Clear removes the selected new matte");
    for v in ["List", "Icon", "Freeform"] {
        d.click(&format!("project.view.{v}"));
        assert_eq!(format!("{:?}", d.app().session.prefs.project_panel.view.mode), v);
    }
    d.click("project.view.Icon");
    assert!(d.has("project.sortIcons"));
    d.click("project.sortIcons");
    d.click("project.sortIcons.Name");
    assert_eq!(d.app().session.prefs.project_panel.view.icon_sort.column, "Name");
}

#[test]
fn new_bin_asks_for_its_name() {
    // #456: New Bin opens the name field of the new bin instead of leaving it "New Bin"
    let mut d = project_driver();
    d.exec("project.view.set", json!({"view": "list"}));
    d.click("project.button.file.newBin");
    d.frames(2);
    let bin = d.app().ui.project_panel.rename.as_ref().and_then(|r| r.bin).expect("the new bin's name is being edited");
    assert!(d.has("project.rename"), "the name field is shown");
    d.key("Cmd+A");
    d.ok("ui.type", json!({"text": "Interviews"}));
    d.key("Enter");
    let name = d.app().session.project.root.find_bin(filmcraft_project::BinId(bin)).unwrap().name.clone();
    assert_eq!(name, "Interviews");
    // with a name (agents) the bin is named directly and nothing is edited
    let r = d.ok("ui.menu.invoke", json!({"id": "file.newBin", "params": {"name": "B-roll"}}));
    assert!(r["bin"].as_u64().is_some(), "{r}");
    assert!(d.app().ui.project_panel.rename.is_none());
}

// ------------------------------------------------------------------------------------ Media Browser

fn media_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fc-mb-ui-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("Selects")).unwrap();
    let wav = filmcraft_media::wav::write_wav16(&[0.0; 9600], 2, 48_000);
    for n in ["a-interview.wav", "b-roll.wav"] {
        std::fs::write(dir.join(n), &wav).unwrap();
    }
    std::fs::write(dir.join("Selects").join("pick.wav"), &wav).unwrap();
    std::fs::write(dir.join("readme.txt"), b"not media").unwrap();
    dir
}

#[test]
fn file_dialog_import_uses_shown_bin_unless_explicit() {
    let dir = media_dir("import-bin");
    let path = dir.join("a-interview.wav").to_string_lossy().into_owned();
    let mut app = FilmcraftApp::new(Session::default());
    app.hooks.pick_files = Some(Box::new(move |_| vec![path.clone()]));
    let parent = app.session.execute("file.newBin", json!({"name": "Footage"})).unwrap()["bin"].as_u64().unwrap();
    let child = app.session.execute("file.newBin", json!({"name": "Selects", "parent": parent})).unwrap()["bin"].as_u64().unwrap();
    let root = app.session.project.root.id.0;
    app.ui.project_panel.tabs.push(filmcraft_ui_egui::panels::project::BinTab { bin: parent, ..Default::default() });
    for (shown, tab, params, expected) in [
        (None, None, json!({}), root),
        (Some(child), None, json!({}), child),
        (Some(child), Some(0), json!({}), parent),
        (Some(child), None, json!({"bin": null}), root),
        (Some(parent), None, json!({"bin": child}), child),
    ] {
        app.ui.project_panel.bin = shown;
        app.ui.project_panel.active_tab = tab;
        let result = app.file_dialog("file.import", &params).unwrap();
        assert!(result["errors"].as_array().unwrap().is_empty(), "{result}");
        let item = ItemId(result["items"][0].as_u64().unwrap());
        let bin = app.session.project.root.find_bin(filmcraft_project::BinId(expected)).unwrap();
        assert!(bin.children.contains(&BinEntry::Item(item)), "shown={shown:?}, tab={tab:?}, params={params}: expected bin {expected}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn media_browser_navigation_import_and_columns() {
    let dir = media_dir("nav");
    let d0 = dir.to_string_lossy().to_string();
    let mut d = Driver::with(Session::default());
    d.ok("ui.panel.show", json!({"panel": "MediaBrowser"}));
    d.ok("ui.set", json!({"focused": "MediaBrowser"}));
    d.ok("ui.menu.invoke", json!({"id": "window.maximizeFrame"}));
    d.exec("mediaBrowser.navigate", json!({"path": d0}));
    d.frames(3);
    d.wait_for("mediaBrowser.entry.a-interview.wav");
    assert!(d.has("mediaBrowser.entry.Selects") && !d.has("mediaBrowser.entry.readme.txt"));
    for id in [
        "mediaBrowser.back",
        "mediaBrowser.forward",
        "mediaBrowser.up",
        "mediaBrowser.path",
        "mediaBrowser.fileTypes",
        "mediaBrowser.ingest",
        "mediaBrowser.importAsImageSequence",
        "mediaBrowser.tree.localDrives",
    ] {
        assert!(d.has(id), "{id}");
    }
    // double-click a folder, then Back / Forward
    // type a directory into the path field
    d.click("mediaBrowser.path");
    d.key("Cmd+A");
    d.ok("ui.type", json!({"text": format!("{d0}/Selects")}));
    d.key("Enter");
    d.wait_for("mediaBrowser.entry.pick.wav");
    d.click("mediaBrowser.back");
    d.wait_for("mediaBrowser.entry.b-roll.wav");
    d.click("mediaBrowser.forward");
    d.wait_for("mediaBrowser.entry.pick.wav");
    d.click("mediaBrowser.up");
    d.wait_for("mediaBrowser.entry.b-roll.wav");
    // Favorites and Recent Directories in the tree
    d.click("panel.menu.MediaBrowser");
    d.click("mediaBrowser.menu.favorite");
    assert_eq!(d.app().session.prefs.media_browser.favorites, std::slice::from_ref(&d0));
    d.frames(2);
    let fav_name = dir.file_name().unwrap().to_string_lossy().to_string();
    assert!(d.has(&format!("mediaBrowser.tree.favorites.{fav_name}")));
    assert!(d.has("mediaBrowser.tree.recent"));
    // select two files and Import from the panel menu
    d.click("mediaBrowser.entry.a-interview.wav");
    d.ok("ui.click", json!({"id": "mediaBrowser.entry.b-roll.wav", "command": true}));
    d.frames(2);
    assert_eq!(d.app().session.browser.selection.len(), 2);
    assert!(d.label("mediaBrowser.count").starts_with("2 of 2"));
    d.snapshot("media-browser-list", Some("panel.MediaBrowser"));
    d.click("panel.menu.MediaBrowser");
    d.click("mediaBrowser.menu.import");
    assert_eq!(d.app().session.project.items.len(), 2);
    // Shift+O opens the selection in the Source monitor (already imported: no duplicate)
    d.click("mediaBrowser.entry.b-roll.wav");
    d.ok("ui.set", json!({"focused": "MediaBrowser"}));
    d.key("Shift+O");
    assert!(d.app().session.state.source_item.is_some());
    assert_eq!(d.app().session.project.items.len(), 2);
    // Edit Columns…
    d.click("panel.menu.MediaBrowser");
    d.click("mediaBrowser.menu.editColumns");
    d.click("mediaBrowser.columns.Audio Info");
    d.click("mediaBrowser.columns.ok");
    assert!(d.app().session.prefs.media_browser.columns.contains(&"Audio Info".to_string()));
    d.frames(6);
    assert!(d.has("mediaBrowser.header.Audio Info"));
    // thumbnails view and the file-type filter
    d.click("mediaBrowser.view.thumbnails");
    assert_eq!(d.app().session.prefs.media_browser.view, "thumbnails");
    d.wait_for("mediaBrowser.entry.b-roll.wav");
    d.snapshot("media-browser-thumbnails", Some("panel.MediaBrowser"));
    d.exec("mediaBrowser.settings", json!({"fileTypes": "image", "view": "list"}));
    d.frames(3);
    assert!(!d.has("mediaBrowser.entry.b-roll.wav") && d.has("mediaBrowser.entry.Selects"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn media_browser_drag_imports_into_the_project_panel() {
    let dir = media_dir("drag");
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let mut d = Driver::with(session);
    // the Editing workspace has Project and Media Browser in one group: put the browser beside it
    d.ok("ui.set", json!({"workspace": "Editing"}));
    d.ok("ui.panel.show", json!({"panel": "MediaBrowser"}));
    d.exec("mediaBrowser.navigate", json!({"path": dir.to_string_lossy()}));
    d.frames(3);
    d.wait_for("mediaBrowser.entry.b-roll.wav");
    let n0 = d.app().session.project.items.len();
    // dropped on the Media Browser itself: the import is taken back
    let e = d.rect("mediaBrowser.entry.b-roll.wav");
    let from = (e[0] + 40.0, e[1] + e[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": from.0, "y": from.1}, "to": {"x": from.0 + 30.0, "y": from.1 + 60.0}}));
    d.frames(6);
    assert_eq!(d.app().session.project.items.len(), n0, "dropped nowhere");
    // dropped on the Timeline: imported and placed
    let tl = d.rect("panel.Timeline");
    d.ok("ui.drag", json!({"from": {"x": from.0, "y": from.1}, "to": {"x": tl[0] + tl[2] * 0.6, "y": tl[1] + tl[3] * 0.75}, "steps": 20}));
    d.frames(6);
    assert_eq!(d.app().session.project.items.len(), n0 + 1, "kept after a drop on the Timeline");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Double-clicking empty space in the Project panel opens Import, also between and beside the
/// Icon view's cards once the project has some. Only the strip under the last row used to answer,
/// so with a project loaded a double-click in the visible blank space did nothing. Real pointer
/// input at 60 frames per second, so the two clicks are a real double-click.
#[test]
fn double_click_empty_area_imports_with_a_project_loaded() {
    fn driver(demo: bool) -> (Driver, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let mut session = Session::default();
        if demo {
            session.execute("file.openDemoProject", json!({})).expect("demo project");
        }
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        let picks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = picks.clone();
        app.hooks.pick_files = Some(Box::new(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Vec::new()
        }));
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots: None };
        d.frames(10);
        (d, picks)
    }
    fn double_click(d: &mut Driver, at: egui::Pos2) {
        let push = |d: &mut Driver, e: egui::Event| d.harness.input_mut().events.push(e);
        // a pause first, as a person makes: clicks closer together continue the last gesture
        // (a triple click) instead of starting a double-click
        d.frames(60);
        push(d, egui::Event::PointerMoved(at));
        d.frames(1);
        for _ in 0..2 {
            for pressed in [true, false] {
                push(d, egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
                d.frames(1);
            }
        }
        d.frames(3);
    }
    let count = |p: &std::sync::Arc<std::sync::atomic::AtomicUsize>| p.load(std::sync::atomic::Ordering::SeqCst);

    // an empty project: the whole panel is the empty area
    let (mut d, picks) = driver(false);
    let r = d.rect("project.empty");
    double_click(&mut d, egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0));
    assert_eq!(count(&picks), 1, "empty project");

    // the demo project, Icon view: the gap between the first two bins, and the space right of the
    // last card in the first row
    let (mut d, picks) = driver(true);
    d.ok("ui.set", json!({}));
    let (a, b) = (d.rect("project.bin.1"), d.rect("project.bin.2"));
    assert!((a[1] - b[1]).abs() < 1.0 && b[0] > a[0] + a[2], "bins 1 and 2 share a row: {a:?} {b:?}");
    double_click(&mut d, egui::pos2((a[0] + a[2] + b[0]) / 2.0, a[1] + a[3] / 2.0));
    assert_eq!(count(&picks), 1, "between two bins");
    let row: Vec<[f32; 4]> = d.ids("project.bin.").iter().map(|id| d.rect(id)).filter(|r| (r[1] - a[1]).abs() < 1.0).collect();
    let last = row.iter().fold(a, |m, r| if r[0] > m[0] { *r } else { m });
    let panel = d.rect("project.count");
    if last[0] + last[2] + 30.0 < panel[0] + panel[2] {
        double_click(&mut d, egui::pos2(last[0] + last[2] + 20.0, last[1] + last[3] / 2.0));
        assert_eq!(count(&picks), 2, "right of the last card");
    }
    // a card still takes its own double-click: the bin opens, Import does not
    double_click(&mut d, egui::pos2(a[0] + a[2] / 2.0, a[1] + a[3] / 2.0));
    let picked = count(&picks);
    assert!(picked <= 2, "double-clicking a bin must not open Import");
}

/// The report: with audio placed in the timeline, double-clicking the Project panel's empty space
/// no longer opened Import. A new sequence with a tone on A1 (so the panel shows the sequence and
/// the audio clip as cards); every empty spot opens Import: the strip under the cards and the space
/// beside them.
#[test]
fn double_click_imports_with_audio_in_the_timeline() {
    let mut session = Session::default();
    session.execute("file.newSequence", json!({"name": "Mix", "audio": 2, "video": 1})).unwrap();
    let inter: Vec<f32> = (0..48_000).flat_map(|i| [(i as f32 * 0.0576).sin() * 0.5; 2]).collect();
    let bytes: std::sync::Arc<[u8]> = filmcraft_engine::previews::write_wav_f32(&inter, 48_000).into();
    let item = filmcraft_engine::commands::import_bytes(&mut session, "/tone.wav", bytes, None).unwrap();
    session.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let (tx, rx) = channel();
    let mut app = FilmcraftApp::new(session).with_control(rx);
    let picks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = picks.clone();
    app.hooks.pick_files = Some(Box::new(move |_| {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Vec::new()
    }));
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx, snapshots: None };
    d.frames(10);
    let double_click = |d: &mut Driver, at: egui::Pos2| {
        let push = |d: &mut Driver, e: egui::Event| d.harness.input_mut().events.push(e);
        d.frames(60);
        push(d, egui::Event::PointerMoved(at));
        d.frames(1);
        for _ in 0..2 {
            for pressed in [true, false] {
                push(d, egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
                d.frames(1);
            }
        }
        d.frames(3);
    };
    let count = || picks.load(std::sync::atomic::Ordering::SeqCst);
    let cards: Vec<[f32; 4]> = d.ids("project.item.").iter().filter(|id| id.matches('.').count() == 2).map(|id| d.rect(id)).collect();
    assert!(!cards.is_empty(), "the sequence and the tone show as cards");
    let empty = d.rect("project.empty");
    eprintln!("cards {cards:?}, empty strip {empty:?}");
    double_click(&mut d, egui::pos2(empty[0] + empty[2] / 2.0, empty[1] + empty[3] / 2.0));
    let strip = count();
    let last = cards.iter().fold(cards[0], |m, r| if r[0] > m[0] { *r } else { m });
    double_click(&mut d, egui::pos2(last[0] + last[2] + 40.0, last[1] + last[3] / 2.0));
    let beside = count() - strip;
    assert_eq!((strip, beside), (1, 1), "double-click on (the strip under the cards, the space beside them)");
}

// ---- marquee selection (#578)

/// The Footage bin open in place in `view`, with its items' rects (row by row, left to right).
fn footage_in(d: &mut Driver, view: &str) -> Vec<(u64, [f32; 4])> {
    let (bin, items) = d.footage();
    d.exec("project.view.set", json!({"view": view}));
    d.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin, "how": "inPlace"}}));
    d.wait_for(&format!("project.item.{}", items[0]));
    let mut rects: Vec<(u64, [f32; 4])> = items.iter().map(|i| (*i, d.rect(&format!("project.item.{i}")))).collect();
    rects.sort_by(|a, b| (a.1[1], a.1[0]).partial_cmp(&(b.1[1], b.1[0])).unwrap());
    rects
}

fn selection(d: &mut Driver) -> Vec<u64> {
    let mut s: Vec<u64> = d.app().session.state.project_selection.iter().map(|i| i.0).collect();
    s.sort_unstable();
    s
}

fn sorted(mut v: Vec<u64>) -> Vec<u64> {
    v.sort_unstable();
    v
}

fn centre(r: [f32; 4]) -> (f32, f32) {
    (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

fn marquee(d: &mut Driver, from: (f32, f32), to: (f32, f32), shift: bool) {
    d.ok("ui.drag", json!({"from": {"x": from.0, "y": from.1}, "to": {"x": to.0, "y": to.1}, "steps": 12, "modifiers": {"shift": shift}}));
    d.frames(3);
}

#[test]
fn marquee_selects_cards_in_icon_view() {
    let mut d = project_driver();
    let cards = footage_in(&mut d, "icon");
    let [(i0, a), (i1, b), (i2, c)] = [cards[0], cards[1], cards[2]];
    assert!((a[1] - c[1]).abs() < 1.0, "the first three cards share a row");
    // from the gap above-left of the first card to the middle of the second
    marquee(&mut d, (a[0] - 4.0, a[1] - 4.0), centre(b), false);
    assert_eq!(selection(&mut d), sorted(vec![i0, i1]));
    // Shift adds
    marquee(&mut d, (c[0] + 4.0, c[1] - 4.0), centre(c), true);
    assert_eq!(selection(&mut d), sorted(vec![i0, i1, i2]));
    // a plain drag replaces
    marquee(&mut d, (a[0] - 4.0, a[1] - 4.0), centre(a), false);
    assert_eq!(selection(&mut d), vec![i0]);
    // the gaps between cards are empty space: a click there deselects
    d.ok("ui.click", json!({"x": a[0] - 4.0, "y": a[1] - 4.0}));
    d.frames(2);
    assert!(selection(&mut d).is_empty());
}

#[test]
fn escape_cancels_a_marquee_and_keeps_the_selection() {
    let mut d = project_driver();
    let cards = footage_in(&mut d, "icon");
    let [(i0, a), (i1, b), (i2, c)] = [cards[0], cards[1], cards[2]];
    d.exec("project.select", json!({"items": [i2]}));
    let send = |d: &mut Driver, e: egui::Event| {
        d.harness.input_mut().events.push(e);
        d.frames(1);
    };
    let (from, to) = (egui::pos2(a[0] - 4.0, a[1] - 4.0), egui::pos2(centre(b).0, centre(b).1));
    send(&mut d, egui::Event::PointerMoved(from));
    send(&mut d, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    for k in 1..=10 {
        send(&mut d, egui::Event::PointerMoved(from.lerp(to, k as f32 / 10.0)));
    }
    assert_eq!(selection(&mut d), sorted(vec![i0, i1]), "the selection follows the rectangle");
    send(&mut d, egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
    let end = egui::pos2(centre(c).0, centre(c).1);
    send(&mut d, egui::Event::PointerMoved(end));
    send(&mut d, egui::Event::PointerButton { pos: end, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);
    assert_eq!(selection(&mut d), vec![i2], "Escape restored the selection the drag began with");
}

#[test]
fn only_the_left_button_draws_a_marquee() {
    let mut d = project_driver();
    let cards = footage_in(&mut d, "icon");
    let [(i0, a), (_, b)] = [cards[0], cards[1]];
    d.exec("project.select", json!({"items": [i0]}));
    let (from, to) = (egui::pos2(a[0] - 4.0, a[1] - 4.0), egui::pos2(centre(b).0, centre(b).1));
    for button in [egui::PointerButton::Middle, egui::PointerButton::Secondary] {
        let mut send = |e: egui::Event| {
            d.harness.input_mut().events.push(e);
            d.frames(1);
        };
        send(egui::Event::PointerMoved(from));
        send(egui::Event::PointerButton { pos: from, button, pressed: true, modifiers: Default::default() });
        for k in 1..=10 {
            send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 10.0)));
        }
        send(egui::Event::PointerButton { pos: to, button, pressed: false, modifiers: Default::default() });
        d.key("Escape");
        assert_eq!(selection(&mut d), vec![i0], "{button:?} drag left the selection alone");
        assert!(!d.has("project.marquee"));
    }
}

#[test]
fn marquee_selects_rows_in_list_view() {
    let mut d = project_driver();
    let rows = footage_in(&mut d, "list");
    let n = rows.len();
    let (last, before_last) = (rows[n - 1], rows[n - 2]);
    let empty = d.rect("project.empty");
    // from the empty space under the rows up into the second-to-last row
    marquee(&mut d, (empty[0] + 60.0, empty[1] + 20.0), (empty[0] + 60.0, centre(before_last.1).1), false);
    assert_eq!(selection(&mut d), sorted(vec![last.0, before_last.0]));
}

#[test]
fn marquee_selects_cards_in_freeform_view() {
    let mut d = project_driver();
    let cards = footage_in(&mut d, "freeform");
    let [(i0, a), (i1, b)] = [cards[0], cards[1]];
    assert!((a[1] - b[1]).abs() < 1.0, "the first two cards share a row");
    marquee(&mut d, (a[0] - 6.0, a[1] - 6.0), centre(b), false);
    assert_eq!(selection(&mut d), sorted(vec![i0, i1]));
    assert!(!d.app().session.project.item(ItemId(i0)).unwrap().metadata.contains_key(FREEFORM_POS), "the marquee moved no card");
}
