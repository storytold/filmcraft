//! Essential Graphics ▸ Browse (Graphics Templates), the template properties in the Edit tab and
//! the Export As Motion Graphics Template / Replace Fonts dialogs, driven headless through the
//! control channel (like `graphics_ui.rs`). With `FILMCRAFT_UI_SHOTS=<dir>` it also renders the
//! UI with wgpu and writes PNGs there for review.

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
    fn demo(data: &str) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let dir = std::env::temp_dir().join(format!("filmcraft-gfx-ui-{data}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        session.prefs_path = Some(dir.join("prefs.json"));
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
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(8);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}: {v}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }
    /// Wheel `id` clear of `panel_id`'s edges before pressing it: a press on a half-clipped
    /// row hits the scrollbar instead and never reaches the widget. Negative wheel reveals
    /// content below, positive content above; then wait for motion to stop.
    fn reveal(&mut self, id: &str, panel_id: &str) {
        let panel = self.rect(panel_id);
        let (wx, wy) = (panel[0] + panel[2] / 2.0, panel[1] + panel[3] / 2.0);
        let lo = panel[1] + 40.0;
        let hi = panel[1] + panel[3] - 60.0;
        for _ in 0..6 {
            let r = self.rect(id);
            let c = r[1] + r[3] / 2.0;
            if r[3] >= 18.0 && c > lo && c < hi {
                break;
            }
            let dy = if c >= hi { -150.0 } else { 150.0 };
            self.ok("ui.scroll", json!({"x": wx, "y": wy, "dy": dy}));
            self.frames(2);
        }
        for _ in 0..10 {
            let a = self.rect(id);
            self.frames(4);
            let b = self.rect(id);
            if a == b {
                break;
            }
        }
    }
    fn clips_named(&mut self, name: &str) -> Vec<u64> {
        let q = self.exec("sequence.inspect", json!({}));
        let mut out = Vec::new();
        for t in q["video"].as_array().cloned().unwrap_or_default() {
            for c in t["items"].as_array().cloned().unwrap_or_default() {
                if c["name"] == name {
                    out.push(c["clip"].as_u64().unwrap());
                }
            }
        }
        out
    }
}

#[test]
fn browse_search_apply_and_edit_template_properties() {
    let mut d = Driver::demo("browse");
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.ok("ui.menu.invoke", json!({"id": "window.panel.EssentialGraphics"}));
    d.ok("ui.set", json!({"gfxTemplates": {"tab": "browse"}}));
    d.frames(4);
    let items = d.ids("gfxTemplates.item.");
    assert!(items.len() >= 8, "built-in templates listed: {items:?}");
    assert!(items.contains(&"gfxTemplates.item.builtin:end-credits-roll".to_string()));
    d.shot("templates-browse");
    // search narrows the list
    d.ok("ui.click", json!({"id": "gfxTemplates.search"}));
    d.ok("ui.type", json!({"text": "credits"}));
    d.frames(3);
    assert_eq!(d.ids("gfxTemplates.item."), vec!["gfxTemplates.item.builtin:end-credits-roll"]);
    // select and apply
    d.exec("playhead.set", json!({"seconds": 300}));
    d.ok("ui.click", json!({"id": "gfxTemplates.item.builtin:end-credits-roll"}));
    d.ok("ui.click", json!({"id": "gfxTemplates.apply"}));
    d.frames(3);
    let clips = d.clips_named("End Credits – Roll");
    assert_eq!(clips.len(), 1, "applied once");
    // the Edit tab shows the template's properties; editing one runs graphics.template.set
    d.ok("ui.set", json!({"gfxTemplates": {"tab": "edit", "query": ""}}));
    d.frames(3);
    let ctl = d.ids("gfxTemplates.control.");
    assert!(ctl.contains(&"gfxTemplates.control.heading".to_string()), "{ctl:?}");
    d.shot("templates-edit");
    d.ok("ui.click", json!({"id": "gfxTemplates.control.heading"}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "THANKS"}));
    d.frames(2);
    let c = d.exec("graphics.template.controls", json!({"clip": clips[0]}));
    assert_eq!(c["controls"][0]["value"], "THANKS", "{c}");
    // the roll options are in Responsive Design - Time (no layer selected)
    d.exec("graphics.selectLayer", json!({"clip": clips[0], "layers": []}));
    d.frames(3);
    assert!(d.ids("graphics.roll.").contains(&"graphics.roll.startOffScreen".to_string()));
    assert!(d.ids("graphics.time.").contains(&"graphics.time.intro".to_string()));
}

#[test]
fn drag_template_onto_the_timeline() {
    let mut d = Driver::demo("drag");
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.ok("ui.menu.invoke", json!({"id": "window.panel.EssentialGraphics"}));
    d.ok("ui.set", json!({"gfxTemplates": {"tab": "browse", "query": "tag"}}));
    d.frames(4);
    let before = d.clips_named("Callout – Tag").len();
    d.ok("ui.drag", json!({"from": {"id": "gfxTemplates.item.builtin:callout-tag"}, "to": {"id": "timeline.tracks", "fx": 0.85, "fy": 0.08}, "steps": 16}));
    d.frames(4);
    assert_eq!(d.clips_named("Callout – Tag").len(), before + 1, "dropped on the timeline");
}

#[test]
fn export_dialog_and_install_round_trip() {
    let mut d = Driver::demo("export");
    d.exec("playhead.set", json!({"seconds": 300}));
    let r = d.exec("graphics.newText", json!({"text": "Exported Title"}));
    let clip = r["clip"].as_u64().unwrap();
    // the menu item opens the dialog
    let v = d.exec("graphics.template.export", json!({}));
    assert_eq!(v["dialog"], "exportTemplate");
    d.frames(3);
    assert!(d.ids("exportTemplate.").contains(&"exportTemplate.ok".to_string()));
    let st = d.ok("ui.inspect", json!({}));
    let draft = st["ui"]["gfx_templates"]["export"].clone();
    assert_eq!(draft["clip"], clip, "{draft}");
    d.ok("ui.click", json!({"id": "exportTemplate.name"}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "UI Title"}));
    d.ok("ui.click", json!({"id": "exportTemplate.ok"}));
    d.frames(3);
    let list = d.exec("graphics.template.list", json!({"source": "user"}));
    assert_eq!(list[0]["name"], "UI Title", "{list}");
    assert_eq!(list[0]["controls"][0]["kind"], "text");
    // the new template shows in Browse
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.ok("ui.menu.invoke", json!({"id": "window.panel.EssentialGraphics"}));
    d.ok("ui.set", json!({"gfxTemplates": {"tab": "browse", "query": ""}}));
    d.frames(130);
    assert!(d.ids("gfxTemplates.item.").contains(&"gfxTemplates.item.user:ui-title".to_string()));
    // Replace Fonts dialog
    let v = d.exec("file.replaceFonts", json!({}));
    assert_eq!(v["dialog"], "replaceFonts");
    d.frames(3);
    assert!(d.ids("replaceFonts.").contains(&"replaceFonts.ok".to_string()));
    d.ok("ui.set", json!({"gfxTemplates": {"replace": {"from": "Inter", "to": "Noto Serif"}}}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "replaceFonts.ok"}));
    d.frames(2);
    let used = d.exec("graphics.fonts.used", json!({}));
    assert!(used.as_array().unwrap().iter().any(|f| f["family"] == "Noto Serif"), "{used}");
}

#[test]
fn type_tool_selection_styles_characters() {
    let mut d = Driver::demo("chars");
    d.exec("playhead.set", json!({"seconds": 300}));
    let r = d.exec("graphics.newText", json!({"text": "Hello World"}));
    let clip = r["clip"].as_u64().unwrap();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.exec("graphics.selectLayer", json!({"clip": clip, "layers": [0]}));
    // a Type-tool selection of "World" (bytes 6..11)
    d.ok("ui.set", json!({"tool": "Type", "gfxEdit": {"clip": clip, "layer": 0, "caret": 11, "anchor": 6}}));
    d.frames(3);
    // the button can sit below the fold when the panel is short: wheel it into view first
    d.reveal("graphics.prop.faux_bold", "panel.EssentialGraphics");
    d.ok("ui.click", json!({"id": "graphics.prop.faux_bold"}));
    d.frames(2);
    let l = d.exec("graphics.list", json!({"clip": clip}));
    assert_eq!(l["layers"][0]["styles"], json!([{"start": 6, "end": 11, "fauxBold": true}]), "{l}");
    let hist = d.exec("history.list", json!({}));
    assert_eq!(hist["undo"].as_array().unwrap().last().unwrap(), "Character Style");
}

/// #611: installing a file FilmCraft refuses (a ZIP — EffectCraft's template package renamed
/// to .fcgt) from the Browse panel must say why on the status bar, not do nothing.
#[test]
fn install_refused_shows_the_reason_on_the_status_bar() {
    let mut d = Driver::demo("install-refused");
    let file = std::env::temp_dir().join(format!("filmcraft-gfx-ui-zip-{}.fcgt", std::process::id()));
    std::fs::write(&file, [0x50u8, 0x4b, 0x03, 0x04]).unwrap();
    let picked = file.to_string_lossy().to_string();
    d.harness.state_mut().hooks.pick_open_file = Some(Box::new(move |_, _| Some(picked.clone())));
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.ok("ui.set", json!({"gfxTemplates": {"tab": "browse"}}));
    d.ok("ui.click", json!({"id": "gfxTemplates.install"}));
    let st = d.ok("ui.inspect", json!({}));
    let _ = std::fs::remove_file(&file);
    let msg = st["ui"]["status"].as_str().unwrap_or_default().to_lowercase();
    assert!(msg.contains("zip archive"), "the refused install left no visible reason: {:?}", st["ui"]["status"]);
}
