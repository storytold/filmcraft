//! The Settings dialog, driven headless through the control channel: opening categories from the
//! `app.settings.<category>` commands and Cmd+,, editing fields by automation id (checkboxes,
//! dropdowns, numbers, label names), OK / Cancel / Escape / Reset… semantics, and settings that
//! change the UI right away (theme, label names in the Edit ▸ Label menu, tooltips).
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the UI with wgpu and writes
//! `settings-*.png`.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
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
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn menu(&mut self, id: &str) -> Value {
        let r = self.ok("ui.menu.invoke", json!({"id": id}));
        self.frames(3);
        r
    }
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == id)
    }
    fn inspect(&mut self) -> Value {
        self.ok("ui.inspect", json!({}))
    }
    /// The open dialog's draft (`UiState::settings`), Null when closed.
    fn draft(&mut self) -> Value {
        self.inspect()["ui"]["settings"].clone()
    }
    fn pref(&mut self, key: &str) -> Value {
        self.exec("prefs.get", json!({"key": key}))
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(4);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("settings-{name}.png"))).unwrap();
    }
}

const CATEGORIES: [&str; 16] = [
    "general",
    "appearance",
    "audio",
    "audioHardware",
    "autoSave",
    "color",
    "graphics",
    "labels",
    "media",
    "mediaAnalysis",
    "mediaCache",
    "memory",
    "playback",
    "plugins",
    "timeline",
    "trim",
];

#[test]
fn every_category_opens_from_its_command_and_menu() {
    let mut d = Driver::demo();
    let menu = d.ok("ui.menu.list", json!({}));
    for c in CATEGORIES {
        let id = format!("app.settings.{c}");
        let item = menu.as_array().unwrap().iter().find(|m| m["id"] == id.as_str()).unwrap_or_else(|| panic!("no menu item {id}"));
        assert_eq!(item["path"], json!(["Edit", "Preferences"]));
    }
    let general = menu.as_array().unwrap().iter().find(|m| m["id"] == "app.settings.general").unwrap();
    assert_eq!(general["shortcut"], "Cmd+,");
    for c in CATEGORIES {
        let r = d.menu(&format!("app.settings.{c}"));
        assert_eq!(r["page"], c);
        let ui = d.inspect();
        assert_eq!(ui["dialog"], "Preferences");
        assert_eq!(ui["ui"]["settings"]["page"], c);
        for other in CATEGORIES {
            assert!(d.has(&format!("settings.category.{other}")), "category list row {other} on {c}");
        }
        assert!(d.has("settings.ok") && d.has("settings.cancel") && d.has("settings.reset") && d.has("settings.help"));
        d.shot(c);
        d.click("settings.cancel");
        assert!(d.inspect()["dialog"].is_null(), "Cancel closes ({c})");
    }
    // the first field of a few pages, by automation id
    for (c, field) in [
        ("general", "settings.general.atStartup"),
        ("timeline", "settings.timeline.stillImageDuration"),
        ("timeline", "settings.timeline.stillImageUnit"),
        ("trim", "settings.trim.largeTrimOffset"),
        ("labels", "settings.labels.colors.violet.name"),
        ("labels", "settings.labels.defaults.movie"),
        ("autoSave", "settings.autoSave.enabled"),
        ("mediaCache", "settings.mediaCache.clean"),
        ("audioHardware", "settings.audioHardware.defaultOutput"),
        ("appearance", "settings.appearance.highlightColor"),
    ] {
        d.menu(&format!("app.settings.{c}"));
        assert!(d.has(field), "{field} on {c}");
        d.click("settings.cancel");
    }
    assert!(d.exec("prefs.get", json!({})).is_object());
    // unknown category
    let r = d.call("ui.menu.invoke", json!({"id": "app.settings.nope"}));
    assert_eq!(r["ok"], false);
}

#[test]
fn cmd_comma_opens_general_and_the_list_switches_pages() {
    let mut d = Driver::demo();
    d.ok("ui.key", json!({"key": "Cmd+,"}));
    d.frames(3);
    assert_eq!(d.draft()["page"], "general");
    d.click("settings.category.playback");
    assert_eq!(d.draft()["page"], "playback");
    assert!(d.has("settings.playback.stepManyFrames"));
    assert!(!d.has("settings.general.atStartup"), "only the shown page's fields are on screen");
    // Escape cancels
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.draft().is_null());
}

#[test]
fn checkbox_edits_apply_on_ok_and_not_on_cancel() {
    let mut d = Driver::demo();
    d.menu("app.settings.trim");
    assert_eq!(d.draft()["values"]["trim"]["selectionToolRollRipple"], false);
    d.click("settings.trim.selectionToolRollRipple");
    assert_eq!(d.draft()["values"]["trim"]["selectionToolRollRipple"], true, "the draft changes");
    assert_eq!(d.pref("trim.selectionToolRollRipple"), false, "nothing applied before OK");
    d.click("settings.cancel");
    assert_eq!(d.pref("trim.selectionToolRollRipple"), false, "Cancel discards");

    d.menu("app.settings.trim");
    d.click("settings.trim.selectionToolRollRipple");
    d.click("settings.category.timeline");
    d.click("settings.timeline.snapPlayhead");
    d.click("settings.ok");
    assert!(d.draft().is_null(), "OK closes");
    assert_eq!(d.pref("trim.selectionToolRollRipple"), true, "edits on several pages apply together");
    assert_eq!(d.pref("timeline.snapPlayhead"), false, "on by default (as in Premiere), the click turns it off");
}

#[test]
fn dropdowns_numbers_and_reset() {
    let mut d = Driver::demo();
    d.menu("app.settings.timeline");
    // dropdown: open it, pick an item by its automation id
    d.click("settings.timeline.autoScroll");
    assert!(d.has("settings.timeline.autoScroll.smoothScroll"), "items of the open dropdown carry ids");
    d.click("settings.timeline.autoScroll.smoothScroll");
    assert_eq!(d.draft()["values"]["timeline"]["autoScroll"], "smoothScroll");
    // duration unit dropdown
    d.click("settings.timeline.stillImageUnit");
    d.click("settings.timeline.stillImageUnit.frames");
    // numbers through ui.set on the draft (agents) …
    d.ok("ui.set", json!({"settings": {"values": {"timeline.stillImageDuration": 48}}}));
    d.click("settings.ok");
    assert_eq!(d.pref("timeline.autoScroll"), "smoothScroll");
    assert_eq!(d.pref("timeline.stillImageUnit"), "frames");
    assert_eq!(d.pref("timeline.stillImageDuration"), 48.0);
    // … or typed into the field
    d.menu("app.settings.playback");
    d.ok("ui.click", json!({"id": "settings.playback.stepManyFrames"}));
    d.frames(3);
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "9"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    assert_eq!(d.draft()["values"]["playback"]["stepManyFrames"], 9);
    d.click("settings.ok");
    assert_eq!(d.pref("playback.stepManyFrames"), 9);
    // Reset… puts the page back to its defaults (applied with OK)
    d.menu("app.settings.timeline");
    d.click("settings.reset");
    assert_eq!(d.draft()["values"]["timeline"]["autoScroll"], "pageScroll");
    assert_eq!(d.pref("timeline.autoScroll"), "smoothScroll", "not yet applied");
    d.click("settings.ok");
    assert_eq!(d.pref("timeline.autoScroll"), "pageScroll");
    assert_eq!(d.pref("playback.stepManyFrames"), 9, "other pages untouched");
    // ui.set rejects unknown keys
    d.menu("app.settings.trim");
    let r = d.call("ui.set", json!({"settings": {"values": {"trim.nope": 1}}}));
    assert_eq!(r["ok"], false);
    d.ok("ui.set", json!({"settings": {"page": "memory"}}));
    assert_eq!(d.draft()["page"], "memory");
}

#[test]
fn view_menu_and_settings_name_each_color_theme_alike() {
    let mut d = Driver::demo();
    let menu = d.ok("ui.menu.list", json!({}));
    let menu_label = |id: &str| menu.as_array().unwrap().iter().find(|m| m["id"] == id).unwrap()["label"].as_str().unwrap().to_string();
    d.menu("app.settings.appearance");
    // the theme cards' choices (the single Color Theme is no longer drawn)
    for (menu_id, id) in [
        ("view.theme.dark", "settings.appearance.darkTheme.darkest"),
        ("view.theme.medium", "settings.appearance.darkTheme.dark"),
        ("view.theme.light", "settings.appearance.lightTheme.light"),
    ] {
        let found = d.ok("ui.elements", json!({"prefix": id}));
        let choice = found.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap();
        assert_eq!(menu_label(menu_id), choice["label"].as_str().unwrap(), "{menu_id} and {id} name the same theme");
    }
}

#[test]
fn appearance_labels_and_tooltips_take_effect() {
    let mut d = Driver::demo();
    assert_eq!(d.inspect()["ui"]["dark"], true);
    d.menu("app.settings.appearance");
    // the Appearance Mode selector above the light and dark theme cards
    assert!(d.has("settings.appearance.lightTheme.light") && d.has("settings.appearance.darkTheme.dark"));
    assert!(!d.has("settings.appearance.colorTheme"), "the single Color Theme is not shown");
    // Each radio belongs below its preview, even though the cards are laid out side by side.
    let elements = d.ok("ui.elements", json!({"prefix": "settings.appearance."}));
    let top = |id: &str| elements.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap()["rect"][1].as_f64().unwrap();
    for (card, radio) in
        [("settings.appearance.lightTheme", "settings.appearance.lightTheme.light"), ("settings.appearance.darkTheme", "settings.appearance.darkTheme.darkest")]
    {
        assert!(top(radio) > top(card) + 128.0, "{radio} sits below its preview");
    }
    d.click("settings.appearance.darkTheme.dark");
    d.click("settings.appearance.appearanceMode");
    d.click("settings.appearance.appearanceMode.light");
    d.ok("ui.set", json!({"settings": {"values": {"appearance.highlightColor": "#e0457b"}}}));
    d.click("settings.ok");
    d.frames(2);
    assert_eq!(d.inspect()["ui"]["dark"], false, "Light theme applied");
    assert_eq!((d.pref("appearance.appearanceMode"), d.pref("appearance.darkTheme")), (json!("light"), json!("dark")));
    // the header button cycles Auto, Light, Dark
    d.click("header.appearance");
    assert_eq!(d.pref("appearance.appearanceMode"), "dark");
    assert_eq!(d.harness.state().tokens.kind, filmcraft_ui_egui::theme::ThemeKind::Medium, "the saved dark theme");
    d.click("header.appearance");
    assert_eq!(d.pref("appearance.appearanceMode"), "auto");
    d.menu("view.appearanceMode.next");
    assert_eq!(d.pref("appearance.appearanceMode"), "light");
    assert_eq!(d.pref("appearance.highlightColor"), "#e0457b");
    assert_eq!(d.harness.state().tokens.accent, egui::Color32::from_rgb(0xe0, 0x45, 0x7b));
    // View ▸ Appearance writes the setting
    d.menu("view.theme.dark");
    assert_eq!(d.pref("appearance.colorTheme"), "darkest");
    assert_eq!((d.pref("appearance.appearanceMode"), d.pref("appearance.darkTheme")), (json!("dark"), json!("darkest")));
    assert_eq!(d.inspect()["ui"]["dark"], true);

    // label names show in Edit ▸ Label
    d.menu("app.settings.labels");
    d.ok("ui.click", json!({"id": "settings.labels.colors.rose.name"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "Interview"}));
    d.frames(2);
    assert_eq!(d.draft()["values"]["labels"]["colors"]["rose"]["name"], "Interview");
    d.click("settings.category.labels");
    d.click("settings.labels.defaults.sequence");
    d.click("settings.labels.defaults.sequence.Teal");
    d.click("settings.ok");
    let menu = d.ok("ui.menu.list", json!({}));
    let rose = menu.as_array().unwrap().iter().find(|m| m["id"] == "edit.label.rose").unwrap();
    assert_eq!(rose["label"], "Interview");
    let r = d.exec("file.newSequence", json!({"name": "Labelled"}));
    let seq = r["sequence"].as_u64().unwrap();
    assert_eq!(d.harness.state().session.project.item(filmcraft_project::ItemId(seq)).unwrap().label, filmcraft_project::Label::Teal);

    // Show Tool Tips off → egui never shows them
    d.menu("app.settings.general");
    d.click("settings.general.showToolTips");
    d.click("settings.ok");
    let delay = d.harness.ctx.global_style().interaction.tooltip_delay;
    assert!(delay > 1.0e6, "tooltips disabled: delay {delay}");
    d.exec("prefs.set", json!({"key": "general.showToolTips", "value": true}));
    d.frames(2);
    assert!(d.harness.ctx.global_style().interaction.tooltip_delay < 1.0);
}

#[test]
fn hex_field_takes_typing_a_character_at_a_time() {
    let mut d = Driver::demo();
    d.menu("app.settings.appearance");
    let hex = |d: &mut Driver| d.draft()["values"]["appearance"]["highlightColor"].clone();
    let before = hex(&mut d);
    d.ok("ui.click", json!({"id": "settings.appearance.highlightColor.hex"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    // not a colour yet: the field keeps it and the setting stays
    d.ok("ui.type", json!({"text": "#12ab"}));
    d.frames(2);
    assert_eq!(hex(&mut d), before);
    d.ok("ui.type", json!({"text": "34"}));
    d.frames(2);
    assert_eq!(hex(&mut d), "#12ab34");
    // deleting a character leaves the last colour
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.frames(2);
    assert_eq!(hex(&mut d), "#12ab34");
    d.ok("ui.type", json!({"text": "5"}));
    d.frames(2);
    assert_eq!(hex(&mut d), "#12ab35");
}

#[test]
fn memory_and_media_cache_pages() {
    let mut d = Driver::demo();
    d.menu("app.settings.memory");
    d.ok("ui.set", json!({"settings": {"values": {"memory.frameCacheMb": 256}}}));
    d.click("settings.ok");
    assert_eq!(d.harness.state().frames.cache_usage().1, 256 << 20, "frame cache budget follows Memory");
    // Media Cache: no data directory in this session → the button reports it
    d.menu("app.settings.mediaCache");
    assert!(d.draft()["cache_info"].is_object());
    d.click("settings.mediaCache.clean");
    assert!(d.draft()["message"].as_str().unwrap().contains("no media cache"), "{}", d.draft());
    d.click("settings.cancel");
    // play after rendering previews follows Timeline
    d.exec("prefs.set", json!({"key": "timeline.playAfterRendering", "value": false}));
    d.frames(2);
    assert_eq!(d.inspect()["ui"]["play_after_render"], false);
}

#[test]
fn snap_playhead_and_return_to_beginning() {
    let mut d = Driver::demo();
    // At playback end, return to beginning
    let dur = d.harness.state().session.active_sequence().unwrap().duration();
    d.exec("playhead.set", json!({"time": dur.0}));
    d.exec("prefs.set", json!({"key": "timeline.returnToBeginning", "value": false}));
    d.frames(2);
    d.ok("ui.playback", json!({"action": "play"}));
    let ph = d.harness.state().session.playhead();
    d.ok("ui.playback", json!({"action": "stop"}));
    assert!(ph.0 > 0, "stays at the end");
    d.exec("prefs.set", json!({"key": "timeline.returnToBeginning", "value": true}));
    d.exec("playhead.set", json!({"time": dur.0}));
    d.ok("ui.playback", json!({"action": "play"}));
    let ph = d.harness.state().session.playhead();
    d.ok("ui.playback", json!({"action": "stop"}));
    assert!(ph.0 < dur.0 / 2, "restarts from the beginning: {ph:?}");
}

/// #164: dragging the playhead in the ruler snaps to a cut, and moves freely away from one
/// (it used to snap onto its own position and only move in jumps).
#[test]
fn playhead_drag_snaps_to_cuts() {
    let mut d = Driver::demo();
    d.frames(2);
    let seq = d.harness.state().session.active_sequence().unwrap().clone();
    let v1 = &seq.video_tracks[0];
    let cut = v1.items[1].start;
    let id = v1.items[1].id.0;
    let rect = |d: &mut Driver, id: &str| -> [f64; 4] {
        let v = d.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    };
    let clip = rect(&mut d, &format!("timeline.clip.{id}"));
    let ruler = rect(&mut d, "timeline.ruler");
    let y = ruler[1] + ruler[3] / 2.0;
    let (from, near) = (clip[0] + 40.0, clip[0] + 3.0);
    d.ok("ui.drag", json!({"from": {"x": from, "y": y}, "to": {"x": near, "y": y}, "steps": 8}));
    d.frames(2);
    assert_eq!(d.harness.state().session.playhead(), cut, "lands on the cut");
    // a step at a time from the cut, it follows the mouse instead of sticking
    d.ok("ui.drag", json!({"from": {"x": clip[0] + 30.0, "y": y}, "to": {"x": clip[0] + 22.0, "y": y}, "steps": 8}));
    d.frames(2);
    let ph = d.harness.state().session.playhead();
    assert!(ph > cut, "moves off the cut freely: {ph:?} vs {cut:?}");
}
