//! Paste files and images from the system clipboard (#611): the real `FilmcraftApp` under
//! `egui_kittest`, with a stand-in for the desktop app's clipboard hook.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{
    Arc,
    mpsc::{Sender, channel},
};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::{ClipboardMedia, FilmcraftApp};
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    dir: std::path::PathBuf,
}

impl Driver {
    /// The demo project saved (in name) in a fresh folder, so pasted images are written there.
    fn demo(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("filmcraft-clipboard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.path = Some(dir.join("film.fcproj").to_string_lossy().into_owned());
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, dir };
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

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    /// The clipboard hook hands out `media` (a copy each time); returns how often it was asked.
    fn clipboard(&mut self, media: Option<fn() -> ClipboardMedia>) -> Rc<Cell<usize>> {
        let asked = Rc::new(Cell::new(0));
        let count = asked.clone();
        self.app().hooks.clipboard_media = Some(Box::new(move || {
            count.set(count.get() + 1);
            media.map(|m| m())
        }));
        asked
    }

    /// Ctrl+V as egui-winit delivers it when the system clipboard holds no text: the press is
    /// dropped, only the release arrives.
    fn ctrl_v_without_text(&mut self, modifiers: egui::Modifiers) {
        let key = |pressed| egui::Event::Key { key: egui::Key::V, physical_key: Some(egui::Key::V), pressed, repeat: false, modifiers };
        self.harness.input_mut().events.extend([egui::Event::ModifiersChanged(modifiers), key(false)]);
        self.frames(1);
        self.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        self.frames(2);
    }

    /// Ctrl+V with text on the clipboard: egui-winit sends `Event::Paste`, then the release.
    fn ctrl_v_with_text(&mut self) {
        let release =
            egui::Event::Key { key: egui::Key::V, physical_key: Some(egui::Key::V), pressed: false, repeat: false, modifiers: egui::Modifiers::COMMAND };
        self.harness.input_mut().events.extend([egui::Event::ModifiersChanged(egui::Modifiers::COMMAND), egui::Event::Paste("text".into()), release]);
        self.frames(1);
        self.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        self.frames(2);
    }

    /// (track kind, track index, clip id, item name, start, end) of every clip, video first.
    fn clips(&mut self) -> Vec<(char, usize, u64, String, i64, i64)> {
        let q = self.app().session.active_sequence().unwrap().clone();
        let mut v = Vec::new();
        for (kind, tracks) in [('V', &q.video_tracks), ('A', &q.audio_tracks)] {
            for (i, t) in tracks.iter().enumerate() {
                v.extend(t.items.iter().map(|c| (kind, i, c.id.0, c.name.clone(), c.start.0, c.end().0)));
            }
        }
        v
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn red_image() -> ClipboardMedia {
    ClipboardMedia::Image { width: 32, height: 18, rgba: [220, 30, 30, 255].repeat(32 * 18) }
}

/// Ctrl+V in the Timeline with an image on the clipboard (no text, so only the key release
/// arrives) saves it next to the project and places it at the playhead above the footage,
/// overwriting nothing. One undo takes it back.
#[test]
fn ctrl_v_pastes_an_image_above_the_footage() {
    let mut d = Driver::demo("image");
    d.ok("ui.set", json!({"focused": "Timeline"}));
    d.clipboard(Some(red_image));
    let before = d.clips();
    let at = d.app().session.playhead().0;
    d.ctrl_v_without_text(egui::Modifiers::COMMAND);
    let after = d.clips();
    for c in &before {
        assert!(after.contains(c), "{c:?} is untouched");
    }
    let new: Vec<_> = after.iter().filter(|c| !before.contains(c)).cloned().collect();
    assert_eq!(new.len(), 1, "one clip pasted: {new:?}");
    let (kind, track, _, name, start, end) = new[0].clone();
    assert_eq!((kind, name.as_str(), start), ('V', "Pasted Image 1.png", at));
    assert!(track > 0 && before.iter().any(|c| c.0 == 'V' && c.1 < track && c.4 < end && c.5 > start), "it sits above a clip");
    assert!(d.dir.join("Pasted Image 1.png").is_file(), "saved next to the project");
    d.ok("ui.menu.invoke", json!({"id": "edit.undo"}));
    assert_eq!(d.clips(), before);
}

/// A paste delivered by `Event::PasteImage` carries the image bytes: paste uses that payload even
/// with no clipboard-media hook (the web host installs none) and without re-reading the OS.
#[test]
fn paste_image_event_imports_its_own_payload_without_a_clipboard_hook() {
    let mut d = Driver::demo("paste-image-event");
    d.ok("ui.set", json!({"focused": "Timeline"}));
    assert!(d.app().hooks.clipboard_media.is_none());
    let before = d.clips();
    let image = egui::ColorImage::filled([8, 6], egui::Color32::from_rgb(10, 20, 30));
    d.harness.input_mut().events.extend([egui::Event::ModifiersChanged(egui::Modifiers::COMMAND), egui::Event::PasteImage(Arc::new(image))]);
    d.frames(1);
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    d.frames(2);
    let new: Vec<_> = d.clips().into_iter().filter(|c| !before.contains(c)).collect();
    assert_eq!(new.iter().map(|c| c.3.as_str()).collect::<Vec<_>>(), ["Pasted Image 1.png"]);
    assert!(d.dir.join("Pasted Image 1.png").is_file(), "the event payload was written, not an empty clipboard");
}

/// The saved PNG stores straight alpha. `Color32` is premultiplied internally, so a
/// semi-transparent clipboard pixel must keep its colour through the paste.
#[test]
fn paste_image_event_writes_straight_alpha_rgba() {
    let mut d = Driver::demo("paste-image-alpha");
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let before = d.clips();
    // Four half-transparent red pixels; premultiplied storage would be (128, 0, 0, 128).
    let rgba = [255, 0, 0, 128, 255, 0, 0, 128, 255, 0, 0, 128, 255, 0, 0, 128];
    let image = egui::ColorImage::from_rgba_unmultiplied([2, 2], &rgba);
    d.harness.input_mut().events.extend([egui::Event::ModifiersChanged(egui::Modifiers::COMMAND), egui::Event::PasteImage(Arc::new(image))]);
    d.frames(1);
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    d.frames(2);
    let new: Vec<_> = d.clips().into_iter().filter(|c| !before.contains(c)).collect();
    assert_eq!(new.iter().map(|c| c.3.as_str()).collect::<Vec<_>>(), ["Pasted Image 1.png"]);
    let png = std::fs::read(d.dir.join("Pasted Image 1.png")).expect("the pasted PNG");
    let decoded = image::load_from_memory(&png).expect("decode the pasted PNG").to_rgba8();
    assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 128], "straight alpha, not the premultiplied colour");
    assert_eq!(decoded.get_pixel(1, 1).0, [255, 0, 0, 128]);
}

/// Files copied in the file manager paste the same way, even when the clipboard also holds text
/// (Ctrl+V then arrives as a paste event); Ctrl+Shift+V inserts them on the targeted track.
#[test]
fn copied_files_paste_and_paste_insert_inserts() {
    let mut d = Driver::demo("files");
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let png = d.dir.join("copied.png");
    image::RgbaImage::from_pixel(16, 9, image::Rgba([0, 200, 0, 255])).save(&png).unwrap();
    let path = png.to_string_lossy().into_owned();
    d.app().hooks.clipboard_media = Some(Box::new(move || Some(ClipboardMedia::Files(vec![path.clone()]))));
    let before = d.clips();
    d.ctrl_v_with_text();
    let new: Vec<_> = d.clips().into_iter().filter(|c| !before.contains(c)).collect();
    assert_eq!(new.iter().map(|c| c.3.as_str()).collect::<Vec<_>>(), ["copied.png"]);
    assert!(new[0].1 > 0, "not over the footage on V1");
    // Paste Insert: on V1, pushing the clips after the playhead right
    let v1_end = |d: &mut Driver| d.clips().iter().filter(|c| c.0 == 'V' && c.1 == 0).map(|c| c.5).max().unwrap();
    let end = v1_end(&mut d);
    d.ctrl_v_without_text(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT);
    assert!(v1_end(&mut d) > end, "Ctrl+Shift+V inserts on V1");
}

/// In the Project panel Ctrl+V imports into the shown bin and places nothing.
#[test]
fn ctrl_v_in_the_project_panel_imports() {
    let mut d = Driver::demo("project");
    d.ok("ui.set", json!({"focused": "Project"}));
    d.clipboard(Some(red_image));
    let (before, items) = (d.clips(), d.app().session.project.items.len());
    d.ctrl_v_without_text(egui::Modifiers::COMMAND);
    assert_eq!(d.clips(), before, "nothing on the Timeline");
    assert_eq!(d.app().session.project.items.len(), items + 1);
    assert!(d.app().session.project.items.values().any(|i| i.name == "Pasted Image 1.png"));
}

/// With nothing but clips copied in FilmCraft, Ctrl+V pastes the clips as before, also when the
/// system clipboard holds no text at all. A plain V press and release (the Selection tool) pastes
/// nothing and doesn't read the clipboard.
#[test]
fn without_media_ctrl_v_pastes_copied_clips() {
    let mut d = Driver::demo("clips");
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let asked = d.clipboard(None);
    let first = d.clips().into_iter().find(|c| c.0 == 'V' && c.1 == 0).unwrap();
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [first.2]}}));
    d.ok("ui.menu.invoke", json!({"id": "edit.copy"}));
    let copies = |d: &mut Driver| d.clips().iter().filter(|c| c.0 == 'V' && c.3 == first.3).count();
    let n = copies(&mut d);
    let asked_after_copy = asked.get();
    assert!(asked_after_copy >= 1, "Copy looks at lingering OS media");
    d.ctrl_v_without_text(egui::Modifiers::COMMAND);
    assert_eq!(copies(&mut d), n + 1, "the copied clip is pasted");
    assert_eq!(asked.get(), asked_after_copy + 1);
    for pressed in [true, false] {
        d.harness.input_mut().events.push(egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        d.frames(1);
    }
    d.frames(2);
    assert_eq!((copies(&mut d), asked.get()), (n + 1, asked_after_copy + 1), "V alone pastes nothing");
}

/// Files left on the OS clipboard from before Copy/Cut must not steal Paste: Ctrl+V pastes the
/// cut clip, as the Edit menu does (#697).
#[test]
fn ctrl_v_pastes_a_cut_clip_even_when_old_files_linger_on_the_clipboard() {
    let mut d = Driver::demo("cut-linger");
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let png = d.dir.join("stale.png");
    image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 200, 255])).save(&png).unwrap();
    let path = png.to_string_lossy().into_owned();
    d.app().hooks.clipboard_media = Some(Box::new(move || Some(ClipboardMedia::Files(vec![path.clone()]))));
    let first = d.clips().into_iter().find(|c| c.0 == 'V' && c.1 == 0).unwrap();
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [first.2]}}));
    d.ok("ui.menu.invoke", json!({"id": "edit.cut"}));
    assert!(d.clips().iter().all(|c| c.2 != first.2), "the clip left the timeline");
    let named = |d: &mut Driver| d.clips().iter().filter(|c| c.3 == first.3).count();
    assert_eq!(named(&mut d), 0);
    d.ctrl_v_with_text();
    assert!(named(&mut d) >= 1, "the cut clip is pasted");
    assert!(!d.clips().iter().any(|c| c.3.contains("stale")), "lingering OS files are not pasted");
}

/// Lift and Extract put the range on the clipboard like Cut, so Ctrl+V pastes it rather than files
/// left on the OS clipboard (#3019).
#[test]
fn ctrl_v_pastes_a_lifted_or_extracted_range_even_when_old_files_linger_on_the_clipboard() {
    for cmd in ["sequence.lift", "sequence.extract"] {
        let mut d = Driver::demo("lift-linger");
        d.ok("ui.set", json!({"focused": "Timeline"}));
        let png = d.dir.join("stale.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([200, 0, 200, 255])).save(&png).unwrap();
        let path = png.to_string_lossy().into_owned();
        d.app().hooks.clipboard_media = Some(Box::new(move || Some(ClipboardMedia::Files(vec![path.clone()]))));
        let first = d.clips().into_iter().find(|c| c.0 == 'V' && c.1 == 0).unwrap();
        d.ok("engine.execute", json!({"command": "markers.markIn", "params": {"frame": 6}}));
        d.ok("engine.execute", json!({"command": "markers.markOut", "params": {"frame": 17}}));
        d.ok("ui.menu.invoke", json!({"id": cmd}));
        d.ok("engine.execute", json!({"command": "playhead.end", "params": {}}));
        let named = |d: &mut Driver| d.clips().iter().filter(|c| c.0 == 'V' && c.3 == first.3).count();
        let before = named(&mut d);
        d.ctrl_v_with_text();
        assert_eq!(named(&mut d), before + 1, "{cmd}: the range is pasted");
        assert!(!d.clips().iter().any(|c| c.3.contains("stale")), "{cmd}: lingering OS files are not pasted");
    }
}

/// A Lift or Extract over a range with no clips on the targeted tracks leaves both clipboards
/// alone: an image copied afterwards in another app is not taken for stale, and Ctrl+V pastes it
/// rather than the clip copied before (#3019).
#[test]
fn lift_or_extract_over_no_clips_keeps_a_newer_os_image_pasteable() {
    for cmd in ["sequence.lift", "sequence.extract"] {
        let mut d = Driver::demo("lift-empty");
        d.ok("ui.set", json!({"focused": "Timeline"}));
        let first = d.clips().into_iter().find(|c| c.0 == 'V' && c.1 == 0).unwrap();
        d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [first.2]}}));
        d.ok("ui.menu.invoke", json!({"id": "edit.copy"}));
        d.clipboard(Some(red_image));
        let tracks: Vec<u64> = d.app().session.active_sequence().unwrap().all_tracks().map(|t| t.id.0).collect();
        let target = |d: &mut Driver, on: bool| {
            for t in &tracks {
                d.ok("engine.execute", json!({"command": "timeline.setTargeting", "params": {"track": t, "targeted": on}}));
            }
        };
        target(&mut d, false);
        d.ok("engine.execute", json!({"command": "markers.markIn", "params": {"frame": 6}}));
        d.ok("engine.execute", json!({"command": "markers.markOut", "params": {"frame": 17}}));
        d.ok("ui.menu.invoke", json!({"id": cmd}));
        target(&mut d, true);
        let before = d.clips();
        d.ctrl_v_without_text(egui::Modifiers::COMMAND);
        let new: Vec<_> = d.clips().into_iter().filter(|c| !before.contains(c)).map(|c| c.3).collect();
        assert_eq!(new, ["Pasted Image 1.png"], "{cmd}");
    }
}

/// Copy from the keyboard (Ctrl+C as `Event::Copy`) also snapshots lingering OS media, so Ctrl+V
/// pastes the clips rather than those files.
#[test]
fn ctrl_c_then_ctrl_v_pastes_clips_not_lingering_files() {
    let mut d = Driver::demo("copy-linger");
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let png = d.dir.join("stale.png");
    image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 200, 0, 255])).save(&png).unwrap();
    let path = png.to_string_lossy().into_owned();
    d.app().hooks.clipboard_media = Some(Box::new(move || Some(ClipboardMedia::Files(vec![path.clone()]))));
    let first = d.clips().into_iter().find(|c| c.0 == 'V' && c.1 == 0).unwrap();
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [first.2]}}));
    d.harness.input_mut().events.extend([egui::Event::ModifiersChanged(egui::Modifiers::COMMAND), egui::Event::Copy]);
    d.frames(1);
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    d.frames(2);
    assert!(!d.app().session.state.clipboard.is_empty());
    let n = d.clips().iter().filter(|c| c.0 == 'V' && c.3 == first.3).count();
    d.ctrl_v_with_text();
    assert_eq!(d.clips().iter().filter(|c| c.0 == 'V' && c.3 == first.3).count(), n + 1);
}
