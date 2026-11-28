//! The Timeline marks the clips of a linked pair that are out of sync with their offset (#4275).
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `out-of-sync.png` there; without it no GPU is needed.

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

fn step(h: &mut Harness<'static, FilmcraftApp>, n: usize) {
    for _ in 0..n {
        h.step();
    }
}

/// The indicator's label for `clip`, if it shows one.
fn badge(h: &Harness<'static, FilmcraftApp>, clip: ClipId) -> Option<String> {
    h.state().auto.elements.iter().find(|e| e.id == format!("timeline.clip.{}.outOfSync", clip.0)).map(|e| e.label.clone())
}

#[test]
fn a_one_sided_move_shows_the_offset_on_both_clips_until_undone() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sync", "video": 1, "audio": 1})).unwrap();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).unwrap().id;
    let rate = s.sequence_rate();
    s.execute("timeline.place", json!({"item": item.0, "frame": 0, "duration": rate.tick_of(96).0})).unwrap();
    let q = s.active_sequence().unwrap();
    let (video, sound) = (q.video_tracks[0].items[0].id, q.audio_tracks[0].items[0].id);
    let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
    let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0));
    if snapshots.is_some() {
        b = b.wgpu();
    }
    let mut h = b.build_eframe(move |_cc| FilmcraftApp::new(s));
    step(&mut h, 4);
    assert!(h.state().auto.find(&format!("timeline.clip.{}", video.0)).is_some(), "the clip is on screen");
    assert_eq!((badge(&h, video), badge(&h, sound)), (None, None), "in sync: nothing shown");

    let time = rate.tick_of(3).0;
    h.state_mut()
        .session
        .execute("timeline.move", json!({"moves": [{"clip": video.0, "track": "V1", "time": time}], "insert": false, "linked": false}))
        .unwrap();
    step(&mut h, 2);
    assert_eq!((badge(&h, video), badge(&h, sound)), (Some("+3".into()), Some("-3".into())));
    if let Some(dir) = snapshots {
        std::fs::create_dir_all(&dir).unwrap();
        h.render().unwrap().save(dir.join("out-of-sync.png")).unwrap();
    }

    h.state_mut().session.execute("edit.undo", json!({})).unwrap();
    step(&mut h, 2);
    assert_eq!((badge(&h, video), badge(&h, sound)), (None, None), "undo clears it");
}
