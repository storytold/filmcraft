//! Every button an agent can name by automation id does what its label says when clicked through
//! the control channel (`ui.click {id}`): the widgets that had no id before (dropdown entries,
//! right-click menu items, title-bar ×s, twirls, splitters…), and ids that did not work for an
//! agent (grouped tools, the Source monitor's Play, the monitors' Button Editor, two copies of one
//! effect, Caps Lock in Keyboard Shortcuts, the narrow Text panel's toolbar, sliders that jumped
//! to their middle on a click).

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::dock::{DockNode, PanelKind, SplitSize};
use filmcraft_ui_egui::panels::monitor::source_playing;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        // 60 frames a second, so a double-click (one pointer event per frame) is one
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
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

    /// A control request; `watch` sees every frame's output until the reply.
    fn call_watching(&mut self, method: &str, params: Value, mut watch: impl FnMut(&egui::FullOutput)) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..1200 {
            self.frames(1);
            watch(self.harness.output());
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.call_watching(method, params, |_| {})
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn menu(&mut self, id: &str) {
        self.ok("ui.menu.invoke", json!({"id": id}));
        self.frames(3);
    }

    /// `ui.click {id}`, then a few frames for the click to take effect.
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn right_click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id, "button": "right"}));
        self.frames(3);
    }

    fn elements(&mut self, prefix: &str) -> Vec<Value> {
        self.ok("ui.elements", json!({"prefix": prefix})).as_array().cloned().unwrap_or_default()
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        self.elements(prefix).iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn has(&mut self, id: &str) -> bool {
        self.ids(id).iter().any(|i| i == id)
    }

    fn element(&mut self, id: &str) -> Value {
        self.elements(id).into_iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}: {:?}", self.ids("")))
    }

    fn rect(&mut self, id: &str) -> [f64; 4] {
        let e = self.element(id);
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }

    /// Frames until `id` is on screen.
    fn wait_for(&mut self, id: &str) {
        for _ in 0..30 {
            if self.has(id) {
                return;
            }
            self.frames(2);
        }
        panic!("{id} never appeared: {:?}", self.ids(id.split('.').next().unwrap_or("")));
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn tool(&mut self) -> String {
        format!("{:?}", self.app().ui.tool)
    }

    /// The first clip on V1 (and its project item).
    fn first_clip(&mut self) -> (u64, u64) {
        let it = self.app().session.active_sequence().unwrap().video_tracks[0].items[0].clone();
        (it.id.0, it.item.0)
    }

    fn effects(&mut self, clip: u64) -> Vec<filmcraft_engine::project::EffectInstance> {
        let q = self.app().session.active_sequence().unwrap();
        q.find_item(filmcraft_engine::project::ClipId(clip)).unwrap().1.effects.clone()
    }

    fn param(&mut self, clip: u64, effect: usize, param: &str) -> f64 {
        self.effects(clip)[effect].params[param].value.as_f64().unwrap()
    }

    fn undo_len(&mut self) -> usize {
        self.app().session.history.undo.len()
    }
}

// ------------------------------------------------------------------------------------ task 2

/// A group's tools share one button: only the tool it shows carries `tools.<Tool>`, so the id
/// selects exactly the tool it names; the others are picked from the flyout.
#[test]
fn a_tool_id_selects_exactly_that_tool() {
    let mut d = Driver::demo();
    assert!(d.has("tools.Ripple") && d.has("tools.group.Ripple"));
    assert!(!d.has("tools.Rolling"), "Rolling is not on screen: its id must not press Ripple");
    d.click("tools.Ripple");
    assert_eq!(d.tool(), "Ripple");
    // the flyout picks Rolling; the group's button then shows (and is) Rolling
    d.right_click("tools.group.Ripple");
    d.wait_for("tools.select.Rolling");
    d.click("tools.select.Rolling");
    assert_eq!(d.tool(), "Rolling");
    assert!(d.has("tools.Rolling") && !d.has("tools.Ripple"), "{:?}", d.ids("tools."));
    d.click("tools.Rolling");
    assert_eq!(d.tool(), "Rolling", "tools.Rolling selects Rolling");
    // Ripple is not on screen now: its id is an error, not a press of Rolling's button
    let v = d.call("ui.click", json!({"id": "tools.Ripple"}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert_eq!(d.tool(), "Rolling");
    d.click("tools.Razor");
    assert_eq!(d.tool(), "Razor");
    // with another group's tool active the group shows its first tool again
    assert!(d.has("tools.Ripple") && !d.has("tools.Rolling"));
}

/// The Source monitor's Play button plays the clip in the Source monitor and stops it again; the
/// Program monitor starting to play stops it.
#[test]
fn source_monitor_play_button_plays_and_stops() {
    let mut d = Driver::demo();
    let (_, item) = d.first_clip();
    d.exec("source.open", json!({"item": item}));
    d.ok("ui.panel.show", json!({"panel": "Source"}));
    d.frames(3);
    let t0 = d.app().session.state.source_playhead;
    d.click("source.transport.src.play");
    let ctx = d.harness.ctx.clone();
    assert!(source_playing(&ctx), "Play started the Source monitor");
    d.frames(40);
    let t1 = d.app().session.state.source_playhead;
    assert!(t1 > t0, "the Source playhead moves while playing: {t0:?} → {t1:?}");
    assert!(!d.app().playback.playing, "the Program monitor stays stopped");
    d.click("source.transport.src.play");
    assert!(!source_playing(&ctx), "a second click stops it");
    let t2 = d.app().session.state.source_playhead;
    d.frames(20);
    assert_eq!(d.app().session.state.source_playhead, t2, "stopped: the playhead stays");
    // one monitor plays at a time
    d.click("source.transport.src.play");
    assert!(source_playing(&ctx));
    d.ok("ui.playback", json!({"action": "play"}));
    d.frames(3);
    assert!(!source_playing(&ctx), "Program playback stops the Source monitor");
    d.ok("ui.playback", json!({"action": "stop"}));
}

/// The `+` at the right of a transport bar opens the Button Editor: buttons shown or hidden by id,
/// extra ones added, and Reset Layout.
#[test]
fn button_editor_adds_and_removes_transport_buttons() {
    let mut d = Driver::demo();
    assert!(d.has("program.transport.markers.add") && !d.has("program.transport.playback.loop"));
    d.click("program.transport.buttonEditor");
    d.wait_for("program.buttonEditor.playback.loop");
    d.click("program.buttonEditor.playback.loop");
    d.click("program.buttonEditor.markers.add");
    assert_eq!(d.app().ui.panels.transport.program.added, vec!["playback.loop".to_string()]);
    assert_eq!(d.app().ui.panels.transport.program.hidden, vec!["markers.add".to_string()]);
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.has("program.transport.playback.loop"), "Loop is on the bar: {:?}", d.ids("program.transport."));
    assert!(!d.has("program.transport.markers.add"), "Add Marker is off the bar");
    assert!(!d.app().playback.looping);
    d.click("program.transport.playback.loop");
    assert!(d.app().playback.looping, "the added button does what it says");
    // the Source monitor's bar is its own
    assert!(d.ids("source.transport.").is_empty() || !d.has("source.transport.playback.loop"));
    d.click("program.transport.buttonEditor");
    d.wait_for("program.buttonEditor.reset");
    d.click("program.buttonEditor.reset");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.has("program.transport.markers.add") && !d.has("program.transport.playback.loop"), "Reset Layout");
}

/// Two copies of one effect on a clip get distinct ids (the second `<effect>@<index>`), so the
/// header, its fx switch and its graph keyframes reach the copy they name.
#[test]
fn two_copies_of_an_effect_have_their_own_ids() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let (clip, _) = d.first_clip();
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("effects.apply", json!({"effect": "gaussian_blur"}));
    d.exec("effects.apply", json!({"effect": "gaussian_blur"}));
    let blurs: Vec<usize> = d.effects(clip).iter().enumerate().filter(|(_, e)| e.effect == "gaussian_blur").map(|(i, _)| i).collect();
    assert_eq!(blurs.len(), 2);
    let (a, b) = (blurs[0], blurs[1]);
    d.frames(4);
    // fold the fixed effects so both blurs are on screen
    for fx in ["motion", "opacity", "time_remap"] {
        if d.has(&format!("effectControls.effect.{fx}")) {
            d.click(&format!("effectControls.effect.{fx}"));
        }
    }
    let second = format!("gaussian_blur@{b}");
    assert!(d.has("effectControls.effect.gaussian_blur") && d.has(&format!("effectControls.effect.{second}")), "{:?}", d.ids("effectControls.effect."));
    d.click(&format!("effectControls.effect.{second}.enabled"));
    let fx = d.effects(clip);
    assert!(fx[a].enabled && !fx[b].enabled, "the second copy's fx switch bypasses the second copy only");
    // a value typed into the second copy's field lands on the second copy
    d.click(&format!("effectControls.{second}.blurriness.value"));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "42"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    assert_eq!(d.param(clip, b, "blurriness"), 42.0);
    assert_ne!(d.param(clip, a, "blurriness"), 42.0);
    // the value graphs of each copy: their keyframes have distinct ids
    for i in [a, b] {
        d.exec("effects.toggleAnimation", json!({"clip": clip, "effect": i, "param": "blurriness"}));
    }
    d.frames(3);
    // (the lower copy first: the upper one's graphs push it down)
    for key in [second.as_str(), "gaussian_blur"] {
        d.click(&format!("effectControls.{key}.blurriness.graphs"));
    }
    let first_kf = d.ids("effectControls.gaussian_blur.blurriness.graph.keyframe.");
    let second_kf = d.ids(&format!("effectControls.{second}.blurriness.graph.keyframe."));
    assert!(!first_kf.is_empty() && !second_kf.is_empty(), "{first_kf:?} {second_kf:?}");
}

/// Keyboard Shortcuts: Caps Lock is drawn but is no modifier (it used to toggle Ctrl as
/// `shortcuts.mod.Caps`); the title-bar × closes the dialog like Cancel.
#[test]
fn caps_lock_is_not_ctrl_and_the_shortcuts_dialog_closes() {
    let mut d = Driver::demo();
    d.menu("app.keyboardShortcuts");
    d.wait_for("shortcuts.key.CapsLock");
    assert!(!d.has("shortcuts.mod.Caps"));
    d.click("shortcuts.key.CapsLock");
    assert!(!d.app().shortcut_editor.mods.ctrl, "Caps Lock leaves Ctrl alone");
    assert!(d.app().shortcut_editor.message.contains("Caps Lock"), "{}", d.app().shortcut_editor.message);
    d.click("shortcuts.mod.Shift");
    assert!(d.app().shortcut_editor.mods.shift, "a real modifier key still toggles");
    d.click("shortcuts.close");
    assert!(d.app().dialog.is_none(), "the × closed Keyboard Shortcuts");
    assert!(!d.has("shortcuts.ok"));
}

/// In a Text panel too narrow for the caption toolbar, the buttons move to an overflow menu (»)
/// that keeps their ids.
#[test]
fn narrow_text_panel_puts_caption_tools_in_an_overflow_menu() {
    let mut d = Driver::demo();
    d.exec("captions.newTrack", json!({"format": "Subtitle"}));
    let tabs = |p: PanelKind| DockNode::Tabs { panels: vec![p], active: 0 };
    d.app().ui.dock =
        DockNode::Split { vertical: false, size: SplitSize::FixedB(260.0), a: Box::new(tabs(PanelKind::Timeline)), b: Box::new(tabs(PanelKind::Text)) };
    d.app().ui.text_tab = "Captions".into();
    d.frames(4);
    d.wait_for("text.captions.overflow");
    assert!(!d.has("text.captions.add"), "the tools that don't fit are not drawn");
    let picker = d.rect("text.captions.track");
    let over = d.rect("text.captions.overflow");
    assert!(picker[0] + picker[2] <= over[0], "the overflow button sits beside the track picker: {picker:?} {over:?}");
    let n0 = d.app().session.active_sequence().unwrap().caption_tracks[0].captions.len();
    d.click("text.captions.overflow");
    d.wait_for("text.captions.add");
    d.click("text.captions.add");
    let n1 = d.app().session.active_sequence().unwrap().caption_tracks[0].captions.len();
    assert_eq!(n1, n0 + 1, "the overflow menu's Add caption added one");
}

/// Essential Sound sliders: the id is the knob, so a click on it keeps the value; a drag from it
/// moves the value; `.track` clicks set a position.
#[test]
fn essential_sound_slider_click_keeps_the_value() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.frames(3);
    let a1 = d.app().session.active_sequence().unwrap().audio_tracks[0].items[0].id.0;
    d.exec("timeline.select", json!({"clips": [a1]}));
    d.frames(3);
    d.click("essentialSound.type.Dialogue");
    d.click("essentialSound.repair.noise.on");
    let amount =
        |d: &mut Driver| d.exec("essentialSound.inspect", json!({"clips": [a1]}))["clips"][0]["settings"]["repair"]["noise"]["amount"].as_f64().unwrap();
    let v0 = amount(&mut d);
    let undo0 = d.undo_len();
    d.click("essentialSound.repair.noise.amount");
    assert_eq!(amount(&mut d), v0, "a click on the knob keeps the value");
    assert_eq!(d.undo_len(), undo0, "and makes no edit");
    let tr = d.rect("essentialSound.repair.noise.amount.track");
    d.ok("ui.drag", json!({"from": {"id": "essentialSound.repair.noise.amount"}, "to": {"x": tr[0] + tr[2] * 0.95, "y": tr[1] + tr[3] / 2.0}, "steps": 8}));
    d.frames(3);
    assert!(amount(&mut d) > v0 + 2.0, "a drag from the knob moves it: {v0} → {}", amount(&mut d));
    d.ok("ui.click", json!({"id": "essentialSound.repair.noise.amount.track", "fx": 0.1}));
    d.frames(3);
    assert!(amount(&mut d) < 2.0, "a click on the track sets the value there: {}", amount(&mut d));
}

// ---------------------------------------------- widgets that no other command or setting reaches

/// About: the contributors' sort entries and name links, and the title-bar ×.
#[test]
fn about_credits_sort_links_and_close() {
    let mut d = Driver::demo();
    d.menu("app.about");
    d.wait_for("about.close");
    d.click("about.tab.contributors");
    d.click("about.credits.sort");
    d.wait_for("about.credits.sort.option.1");
    d.click("about.credits.sort.option.1");
    let label = d.element("about.credits.sort")["label"].as_str().unwrap().to_string();
    assert!(label.starts_with("Sort: ") && !label.contains("First"), "the sort changed: {label}");
    // a contributor's name opens their GitHub profile
    let links = d.ids("about.credits.contributor.");
    assert!(!links.is_empty() || filmcraft_ui_egui::credits::CONTRIBUTORS.is_empty(), "contributor names have ids");
    if let Some(link) = links.first() {
        let login = link.trim_start_matches("about.credits.contributor.").to_string();
        let mut opened = Vec::new();
        let v = d.call_watching("ui.click", json!({"id": link}), |o| {
            for c in &o.platform_output.commands {
                if let egui::OutputCommand::OpenUrl(u) = c {
                    opened.push(u.url.clone());
                }
            }
        });
        assert_eq!(v["ok"], json!(true), "{v}");
        d.frames(2);
        assert!(opened.iter().any(|u| u == &format!("https://github.com/{login}")), "{link} opened {opened:?}");
    }
    d.click("about.close");
    assert!(d.app().dialog.is_none(), "the × closed About");
}

/// The header's free stretch: a double-click maximizes the window.
#[test]
fn header_drag_area_double_click_maximizes() {
    let mut d = Driver::demo();
    let mut maximize = false;
    let v = d.call_watching("ui.click", json!({"id": "header.drag", "count": 2}), |o| {
        for vo in o.viewport_output.values() {
            maximize |= vo.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Maximized(_)));
        }
    });
    assert_eq!(v["ok"], json!(true), "{v}");
    for _ in 0..4 {
        d.frames(1);
        maximize |= d.harness.output().viewport_output.values().any(|vo| vo.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Maximized(_))));
    }
    assert!(maximize, "double-clicking the header asks the window to maximize");
}

/// Properties panel: the section twirls, the value fields, and the Speed button.
#[test]
fn properties_sections_values_and_speed() {
    let mut d = Driver::demo();
    let (clip, _) = d.first_clip();
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.frames(3);
    for (id, key) in [("properties.section.crop", "props:Crop"), ("properties.section.transform", "props:Transform")] {
        d.click(id);
        assert!(d.app().ui.collapsed_fx.iter().any(|k| k == key), "{id} folds the section");
        d.click(id);
        assert!(!d.app().ui.collapsed_fx.iter().any(|k| k == key), "{id} unfolds it");
    }
    // a value: click, type, Enter
    d.click("properties.opacity.opacity.value");
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "35"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    let op = d.effects(clip).iter().position(|e| e.effect == "opacity").unwrap();
    assert_eq!(d.param(clip, op, "opacity"), 35.0);
    // Speed opens Clip Speed / Duration
    d.click("properties.speed");
    assert_eq!(d.app().ui.clip_dialog.as_ref().map(|c| c.command.clone()).as_deref(), Some("clip.speedDuration"));
    d.click("speedDuration.cancel");
    // an audio clip (the music, which has no video): the Audio section
    let music = d.app().session.active_sequence().unwrap().audio_tracks[1].items[0].id.0;
    d.exec("timeline.select", json!({"clips": [music]}));
    d.frames(3);
    d.click("properties.section.audio");
    assert!(d.app().ui.collapsed_fx.iter().any(|k| k == "props:Audio"));
}

/// Lumetri: the RGB Curves channel swatches pick the curve shown; sliders keep their value on a
/// knob click and take one on a track click; Look's entries.
#[test]
fn lumetri_curve_channels_sliders_and_look() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Color"}));
    let (clip, _) = d.first_clip();
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.frames(3);
    d.click("lumetri.addToClip");
    let lum = d.effects(clip).iter().position(|e| e.effect == "lumetri").expect("Add Lumetri Color to clip");
    let ex0 = d.param(clip, lum, "exposure");
    d.click("lumetri.param.exposure");
    assert_eq!(d.param(clip, lum, "exposure"), ex0, "a click on the knob keeps the value");
    d.ok("ui.click", json!({"id": "lumetri.param.exposure.track", "fx": 0.9}));
    d.frames(3);
    assert!(d.param(clip, lum, "exposure") > ex0, "a click on the track sets it");
    d.click("lumetri.fx");
    assert!(!d.effects(clip)[lum].enabled, "fx bypasses Lumetri");
    d.click("lumetri.fx");
    // curves: fold Basic Correction, open Curves
    d.click("lumetri.section.Basic Correction");
    d.click("lumetri.section.Curves");
    d.wait_for("lumetri.curve.curve_luma");
    d.click("lumetri.curves.channel.red");
    assert!(d.has("lumetri.curve.curve_red") && !d.has("lumetri.curve.curve_luma"), "{:?}", d.ids("lumetri.curve"));
    // Creative ▸ Look
    d.click("lumetri.section.Curves");
    d.click("lumetri.section.Creative");
    d.click("lumetri.look");
    d.wait_for("lumetri.look.option.1");
    d.click("lumetri.look.option.1");
    assert_eq!(d.effects(clip)[lum].params["look"].value, filmcraft_engine::project::ParamValue::Choice(1));
}

/// Effect Controls: a mask's twirl, a keyframe's right-click menu, the effect's Clear.
#[test]
fn effect_controls_mask_twirl_keyframe_menu_and_clear() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let (clip, _) = d.first_clip();
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("effects.apply", json!({"effect": "gaussian_blur"}));
    let idx = d.effects(clip).iter().position(|e| e.effect == "gaussian_blur").unwrap();
    d.exec("masks.add", json!({"clip": clip, "effect": idx, "shape": "ellipse"}));
    d.exec("effects.toggleAnimation", json!({"clip": clip, "effect": idx, "param": "blurriness"}));
    d.frames(4);
    for fx in ["motion", "opacity", "time_remap"] {
        if d.has(&format!("effectControls.effect.{fx}")) {
            d.click(&format!("effectControls.effect.{fx}"));
        }
    }
    d.click("effectControls.gaussian_blur.mask0.twirl");
    let key = format!("mask:{clip}:{idx}:0");
    assert!(d.app().ui.collapsed_fx.contains(&key), "the twirl folds the mask");
    // the keyframe's menu: Hold
    let kf = d.ids("effectControls.gaussian_blur.blurriness.keyframe.").into_iter().find(|i| i.matches('.').count() == 4).expect("a lane keyframe");
    d.right_click(&kf);
    d.wait_for(&format!("{kf}.interp.hold"));
    d.click(&format!("{kf}.interp.hold"));
    let k = &d.effects(clip)[idx].params["blurriness"].keyframes[0];
    assert_eq!(k.interp, filmcraft_engine::project::Interpolation::Hold);
    // Clear removes the effect
    d.right_click("effectControls.effect.gaussian_blur");
    d.wait_for("effectControls.effect.gaussian_blur.clear");
    d.click("effectControls.effect.gaussian_blur.clear");
    assert!(!d.effects(clip).iter().any(|e| e.effect == "gaussian_blur"), "Clear removed Gaussian Blur");
}

/// Media Browser: an entry's Reveal in Finder hands the path to the operating system.
#[test]
fn media_browser_reveal_in_finder() {
    let dir = std::env::temp_dir().join(format!("fc-auto-ids-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("clip.wav"), filmcraft_media::wav::write_wav16(&[0.0; 4800], 2, 48_000)).unwrap();
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Editing"}));
    d.ok("ui.panel.show", json!({"panel": "MediaBrowser"}));
    d.exec("mediaBrowser.navigate", json!({"path": dir.to_string_lossy()}));
    d.frames(3);
    d.wait_for("mediaBrowser.entry.clip.wav");
    d.right_click("mediaBrowser.entry.clip.wav");
    d.wait_for("mediaBrowser.entryMenu.reveal");
    d.click("mediaBrowser.entryMenu.reveal");
    let opened = d.app().ui.extras.opened.last().cloned().unwrap_or_default();
    assert!(opened.ends_with("clip.wav"), "revealed {opened}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Audio Track Mixer ▸ Show/Hide Tracks: the window's title-bar × closes it.
#[test]
fn mixer_show_hide_window_closes_by_its_x() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "AudioTrackMixer"}));
    d.frames(3);
    d.click("mixer.menu");
    d.wait_for("mixer.menu.showHide");
    d.click("mixer.menu.showHide");
    d.wait_for("mixer.showHide.close");
    d.click("mixer.showHide.close");
    d.frames(2);
    assert!(!d.has("mixer.showHide.ok"), "the × closed Show/Hide Tracks");
}

/// The Timeline's display toggles (panel menu and wrench) and the video / audio divider.
#[test]
fn timeline_display_toggles_and_divider() {
    let mut d = Driver::demo();
    let thumbs = d.app().ui.timeline.show_thumbnails;
    d.click("panel.menu.Timeline");
    d.wait_for("panel.menu.Timeline.videoThumbnails");
    d.click("panel.menu.Timeline.videoThumbnails");
    assert_eq!(d.app().ui.timeline.show_thumbnails, !thumbs);
    let waves = d.app().ui.timeline.show_waveforms;
    d.click("panel.menu.Timeline");
    d.wait_for("panel.menu.Timeline.audioWaveforms");
    d.click("panel.menu.Timeline.audioWaveforms");
    assert_eq!(d.app().ui.timeline.show_waveforms, !waves);
    // the wrench (Timeline Display Settings) has the same two
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    d.click("timeline.toggle.settings");
    d.wait_for("timeline.settings.showThumbnails");
    d.click("timeline.settings.showThumbnails");
    assert_eq!(d.app().ui.timeline.show_thumbnails, thumbs);
    d.click("timeline.toggle.settings");
    d.wait_for("timeline.settings.showWaveforms");
    d.click("timeline.settings.showWaveforms");
    assert_eq!(d.app().ui.timeline.show_waveforms, waves);
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    // dragging the divider moves the video / audio split
    let split = d.app().ui.timeline.split;
    let r = d.rect("timeline.divider");
    d.ok("ui.drag", json!({"from": {"id": "timeline.divider"}, "to": {"x": r[0] + r[2] / 2.0, "y": r[1] + 60.0}}));
    d.frames(3);
    assert!(d.app().ui.timeline.split > split, "the divider moved down: {split} → {}", d.app().ui.timeline.split);
}

/// Text ▸ Captions: the track picker's entries show that track.
#[test]
fn caption_track_picker_entries_show_their_track() {
    let mut d = Driver::demo();
    d.exec("captions.newTrack", json!({"format": "Subtitle"}));
    d.exec("captions.newTrack", json!({"format": "Subtitle"}));
    let c1 = d.exec("captions.add", json!({"track": "C1", "text": "one", "seconds": 1.0}))["caption"].as_u64().unwrap();
    let c2 = d.exec("captions.add", json!({"track": "C2", "text": "two", "seconds": 3.0}))["caption"].as_u64().unwrap();
    d.ok("ui.panel.show", json!({"panel": "Text"}));
    d.app().ui.text_tab = "Captions".into();
    d.exec("captions.select", json!({"captions": []}));
    d.frames(3);
    assert!(d.has(&format!("text.captions.{c1}.goto")) && !d.has(&format!("text.captions.{c2}.goto")), "C1 shown first");
    d.click("text.captions.track");
    d.wait_for("text.captions.track.2");
    d.click("text.captions.track.2");
    d.frames(2);
    assert!(d.has(&format!("text.captions.{c2}.goto")) && !d.has(&format!("text.captions.{c1}.goto")), "C2 shown");
}

// ------------------------------------------------------------------- a sample of the rest

/// Dropdown entries, right-click menus and window ×s across the app.
#[test]
fn dropdown_entries_menus_and_window_closes() {
    let mut d = Driver::demo();
    // a menu dialog's dropdown (Create Search Bin ▸ Search column)
    d.menu("file.newSearchBin");
    d.click("searchBin.column");
    d.wait_for("searchBin.column.option.Name");
    d.click("searchBin.column.option.Name");
    assert_eq!(d.app().ui.extras.dialog.as_ref().unwrap().params["column"], json!("Name"));
    d.click("searchBin.cancel");
    // Sequence ▸ Color Management: the ×
    d.menu("sequence.colorSettings");
    d.wait_for("colorDialog.close");
    d.click("colorDialog.close");
    assert!(d.app().ui.color_dialog.is_none(), "the × closed the colour dialog");
    // the Program monitor's resolution dropdown
    d.click("program.resolution");
    d.wait_for("program.resolution.quarter");
    d.click("program.resolution.quarter");
    assert_eq!(d.app().ui.program.res, filmcraft_ui_egui::state::PlaybackRes::Quarter);
    // the wrench: Show Transport Controls hides the transport bar, Lumetri Scopes shows the panel
    d.click("program.settings");
    d.wait_for("program.settings.showTransport");
    d.click("program.settings.showTransport");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(!d.has("program.transport.markers.add"), "the transport bar is hidden");
    d.click("program.settings");
    d.wait_for("program.settings.lumetriScopes");
    d.click("program.settings.lumetriScopes");
    assert!(d.app().ui.dock.contains(PanelKind::LumetriScopes), "Lumetri Scopes opened");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    // a timeline clip's Label submenu
    let (clip, _) = d.first_clip();
    d.ok("ui.set", json!({"workspace": "Editing"}));
    d.frames(3);
    d.right_click(&format!("timeline.clip.{clip}"));
    d.wait_for("timeline.clipMenu.edit.label");
    d.ok("ui.move", json!({"id": "timeline.clipMenu.edit.label"}));
    d.frames(3);
    d.wait_for("timeline.clipMenu.edit.label.Violet");
    d.click("timeline.clipMenu.edit.label.Violet");
    let q = d.app().session.active_sequence().unwrap();
    let label = q.find_item(filmcraft_engine::project::ClipId(clip)).unwrap().1.label;
    assert_eq!(label.name(), "Violet");
    // Effects ▸ search
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.frames(3);
    d.click("effects.search");
    d.ok("ui.type", json!({"text": "blur"}));
    d.frames(2);
    assert_eq!(d.app().ui.effects_search, "blur");
}

/// History rows: a click goes back to just after that step.
#[test]
fn history_rows_undo_and_redo() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "History"}));
    let (clip, _) = d.first_clip();
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("effects.apply", json!({"effect": "gaussian_blur"}));
    d.exec("effects.apply", json!({"effect": "gaussian_blur"}));
    d.frames(3);
    let n = d.undo_len();
    assert!(n >= 2);
    d.click(&format!("history.row.{}", n - 2));
    assert_eq!(d.undo_len(), n - 1, "back to just after the step before the last");
    d.click("history.redo.0");
    assert_eq!(d.undo_len(), n, "redone");
}

/// Every title-bar × id sits on egui's own close button (found through the accessibility tree,
/// "Close window"), and clicking it closes that window.
#[test]
fn window_close_ids_sit_on_the_title_bar_close_button() {
    use egui_kittest::kittest::Queryable;
    let mut d = Driver::demo();
    let (clip, _) = d.first_clip();
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("effects.apply", json!({"effect": "gaussian_blur"}));
    let a1 = d.app().session.active_sequence().unwrap().audio_tracks[0].items[0].id.0;
    d.exec("timeline.select", json!({"clips": [a1]}));
    d.exec("effects.apply", json!({"effect": "parametric_eq"}));
    let eq = d.effects(a1).iter().position(|e| e.effect == "parametric_eq").unwrap();
    d.exec("timeline.select", json!({"clips": [clip]}));
    let root = d.app().session.project.root.id.0;
    type Open = Box<dyn Fn(&mut Driver)>;
    type Closed = Box<dyn Fn(&mut Driver) -> bool>;
    let windows: Vec<(&str, Open, Closed)> = vec![
        ("about.close", Box::new(|d| d.menu("app.about")), Box::new(|d| d.app().dialog.is_none())),
        ("shortcuts.close", Box::new(|d| d.menu("app.keyboardShortcuts")), Box::new(|d| d.app().dialog.is_none())),
        ("colorDialog.close", Box::new(|d| d.menu("sequence.colorSettings")), Box::new(|d| d.app().ui.color_dialog.is_none())),
        ("colorDialog.close", Box::new(|d| d.menu("clip.interpretFootage")), Box::new(|d| d.app().ui.color_dialog.is_none())),
        (
            "savePreset.close",
            Box::new(move |d| filmcraft_ui_egui::panels::presets::open_save(d.app(), clip, vec![0], "Gaussian Blur")),
            Box::new(|d| d.app().ui.save_preset.is_none()),
        ),
        (
            "fxEditor.parametric_eq.windowClose",
            Box::new(move |d| {
                filmcraft_ui_egui::panels::audio_fx_editor::open(d.app(), filmcraft_ui_egui::panels::audio_fx_editor::FxTarget::Clip { clip: a1, index: eq })
            }),
            Box::new(|d| d.app().ui.audio_fx_editors.is_empty()),
        ),
        (
            "projectBin.0.close",
            Box::new(move |d| drop(d.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": root, "how": "newWindow"}})))),
            Box::new(|d| !d.app().ui.project_panel.tabs.iter().any(|t| t.floating)),
        ),
        // (in Export mode, so last)
        (
            "presetManager.close",
            Box::new(|d| drop(d.ok("ui.set", json!({"mode": "export", "export": {"manager": {}}})))),
            Box::new(|d| d.app().ui.export.manager.is_none()),
        ),
    ];
    for (id, open, closed) in windows {
        open(&mut d);
        d.frames(3);
        d.wait_for(id);
        let r = d.rect(id);
        let ours = egui::Rect::from_min_size(egui::pos2(r[0] as f32, r[1] as f32), egui::vec2(r[2] as f32, r[3] as f32)).expand(1.0);
        let buttons: Vec<egui::Rect> = d.harness.query_all_by_label("Close window").map(|n| n.rect()).collect();
        assert!(buttons.iter().any(|b| ours.contains(b.center())), "{id} at {ours:?}, egui's close buttons at {buttons:?}");
        d.click(id);
        d.frames(2);
        assert!(closed(&mut d), "{id} closed its window");
    }
}
