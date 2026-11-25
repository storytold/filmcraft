//! Headless UI tests of Clip ▸ Remix: the menu entries, the Remix Properties dialog and the Remix
//! tool (drag a music clip's Out edge), driven by automation id through the control channel.

use std::sync::Arc;

use crate::support::{Drive, Launch, builder, driver, pointer_drag};
use filmcraft_engine::Session;
use serde_json::json;

const SR: u32 = 48_000;

driver!(Driver);

impl Driver {
    fn new(session: Session) -> Self {
        Self::open(session, builder())
    }
    /// Press at `from`, move to `to` over several frames (as a mouse does), release. Use steps of a
    /// few pixels: the drag starts where the pointer has moved past egui's drag threshold, and edge
    /// hits are 7 px wide.
    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2, steps: usize) {
        pointer_drag(self, from, to, steps);
    }

    fn duration(&mut self, clip: u64) -> f64 {
        let s = self.harness.state();
        s.session.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.duration.seconds()
    }
}

/// A session with 32 s of generated 120 BPM music on A1 (selected).
fn session() -> (Session, u64) {
    let (m, _) = filmcraft_audio_dsp::remix::test_music(SR, 120.0, 16, 5);
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "remix", "audio": 2, "video": 1})).unwrap();
    let inter: Vec<f32> = (0..m[0].len()).flat_map(|i| [m[0][i], m[1][i]]).collect();
    let bytes: Arc<[u8]> = filmcraft_engine::previews::write_wav_f32(&inter, SR).into();
    let item = filmcraft_engine::commands::import_bytes(&mut s, "/music.wav", bytes, None).unwrap();
    let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let c = r["clips"][0].as_u64().unwrap();
    s.execute("timeline.select", json!({"clips": [c]})).unwrap();
    (s, c)
}

#[test]
fn remix_menu_and_properties_dialog() {
    let (s, clip) = session();
    let mut d = Driver::new(s);
    // the menu lists the Remix submenu
    let items = d.ok("ui.menu.list", json!({}));
    let ids: Vec<&str> = items.as_array().unwrap().iter().filter_map(|i| i["id"].as_str()).collect();
    for id in ["clip.remix.enable", "clip.remix.properties", "clip.remix.revert"] {
        assert!(ids.contains(&id), "{id} missing from the menus");
    }
    let original = d.duration(clip);
    // properties before enabling: refused with a reason
    let v = d.call("ui.menu.invoke", json!({"id": "clip.remix.properties"}));
    assert_eq!(v["ok"], json!(false));
    d.ok("ui.menu.invoke", json!({"id": "clip.remix.enable"}));
    let r = d.ok("ui.menu.invoke", json!({"id": "clip.remix.properties"}));
    assert_eq!(r["dialog"], "remixProperties");
    d.frames(3);
    let ids = d.ids("remixProperties.");
    for id in ["duration", "segments", "variations", "info", "ok", "cancel"] {
        assert!(ids.contains(&format!("remixProperties.{id}")), "remixProperties.{id} missing: {ids:?}");
    }
    // Cancel changes nothing
    let before = d.duration(clip);
    d.ok("ui.click", json!({"id": "remixProperties.cancel"}));
    d.frames(3);
    assert!(d.ids("remixProperties.").is_empty());
    assert_eq!(d.duration(clip), before);
    // type a target duration and press OK: one undo step, within a beat of the target
    d.ok("ui.menu.invoke", json!({"id": "clip.remix.properties"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "remixProperties.duration"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "20"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    let undo_before = d.harness.state().session.history.undo.len();
    d.ok("ui.click", json!({"id": "remixProperties.ok"}));
    d.frames(3);
    let after = d.duration(clip);
    assert!((after - 20.0).abs() <= 0.5, "remixed to {after} s");
    assert_eq!(d.harness.state().session.history.undo.len(), undo_before + 1);
    let p = d.exec("clip.remix.properties", json!({"clip": clip}));
    assert_eq!(p["targetSeconds"].as_f64().unwrap(), 20.0);
    // Revert Remix from the menu
    d.ok("ui.menu.invoke", json!({"id": "clip.remix.revert"}));
    assert_eq!(d.duration(clip), original);
}

#[test]
fn remix_tool_drag_sets_the_duration() {
    let (s, clip) = session();
    let mut d = Driver::new(s);
    d.ok("ui.menu.invoke", json!({"id": "tool.remix"}));
    let ui = d.ok("ui.inspect", json!({}));
    assert_eq!(ui["ui"]["tool"], "Remix");
    // the tool sits in the Rate Stretch group of the Tools panel
    d.frames(2);
    let edge = d.ok("ui.timeline.locate", json!({"clip": clip, "edge": "out"}));
    let (x, y) = (edge["x"].as_f64().unwrap(), edge["y"].as_f64().unwrap());
    let hit = d.ok("ui.timeline.hit", json!({"x": x - 1.0, "y": y}));
    assert!(hit.to_string().contains("out") || hit.to_string().contains("Out"), "{hit}");
    // drag the Out edge to the left by a third of the clip
    let start = d.ok("ui.timeline.locate", json!({"clip": clip, "edge": "in"}));
    let w = x - start["x"].as_f64().unwrap();
    let undo_before = d.harness.state().session.history.undo.len();
    d.drag(egui::pos2(x as f32 - 0.5, y as f32), egui::pos2((x - 0.5 - w / 3.0) as f32, y as f32), 300);
    d.frames(4);
    let dur = d.duration(clip);
    assert!((dur - 32.0 * 2.0 / 3.0).abs() < 1.5, "remixed to {dur} s");
    assert_eq!(d.harness.state().session.history.undo.len(), undo_before + 1, "one undo step");
    let p = d.exec("clip.remix.properties", json!({"clip": clip}));
    assert_eq!(p["remixed"], true);
    assert!((p["seconds"].as_f64().unwrap() - p["targetSeconds"].as_f64().unwrap()).abs() <= 0.5);
    assert!(!p["cuts"].as_array().unwrap().is_empty());
}
