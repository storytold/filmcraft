//! Project Manager: collect copies used media (excluding unused clips) and the copy opens and
//! plays; consolidate trims to the used range plus handles and every edit stays in place.

use filmcraft_media::DemoScene;
use filmcraft_project::{ItemId, MediaRef};
use serde_json::json;

use crate::Session;
use crate::media_test_util::{frame_rgba, make_movie, psnr, session_with, tmp_dir};

fn project(root: &std::path::Path) -> (Session, Vec<ItemId>, String) {
    let media = root.join("Media");
    std::fs::create_dir_all(&media).unwrap();
    let (a, b, unused) = (media.join("a.mov"), media.join("b.mov"), media.join("unused.mov"));
    make_movie(&a, DemoScene::Aurora, 160, 90, 48);
    make_movie(&b, DemoScene::Dunes, 160, 90, 24);
    make_movie(&unused, DemoScene::Plasma, 160, 90, 12);
    let (mut s, items, _) = session_with(&[&a, &b]);
    s.execute("file.import", json!({"paths": [unused.to_string_lossy()]})).unwrap();
    // use only frames 20..32 of a (V1 + A1)
    let seq = s.state.active_sequence.unwrap();
    let mut p = (*s.project).clone();
    let rate = filmcraft_time::FrameRate::FPS_24;
    let q = p.sequence_mut(seq).unwrap();
    for t in q.all_tracks_mut() {
        let it = t.items.iter_mut().find(|i| i.item == items[0]).unwrap();
        it.source_in = rate.tick_of(20);
        it.duration = rate.tick_of(12);
        for x in t.items.iter_mut().filter(|i| i.item == items[1]) {
            x.start = rate.tick_of(12);
        }
    }
    s.project = std::sync::Arc::new(p);
    let path = root.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.save", json!({"path": path})).unwrap();
    (s, items, path)
}

fn opened(path: &str) -> Session {
    let mut s = Session::default();
    let r = s.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(r["missingMedia"], json!(0), "{r}");
    s
}

#[test]
fn collect_files_copies_used_media_and_opens() {
    let root = tmp_dir("pm-collect");
    let (mut s, _, _) = project(&root);
    let before: Vec<Vec<u8>> = (0..36).step_by(5).map(|f| frame_rgba(&mut s, f, 1.0).2).collect();
    let dest = root.join("Collected");
    let dry = s.execute("file.projectManager", json!({"destination": dest.to_string_lossy(), "dryRun": true})).unwrap();
    assert_eq!(dry["files"].as_array().unwrap().len(), 2, "the unused clip is excluded: {dry}");
    assert_eq!(dry["excluded"], json!(1));
    let size = |n: &str| std::fs::metadata(root.join("Media").join(n)).unwrap().len();
    assert_eq!(dry["originalBytes"].as_u64().unwrap(), size("a.mov") + size("b.mov"));
    assert!(!dest.exists(), "a dry run writes nothing");
    let r = s.execute("file.projectManager", json!({"destination": dest.to_string_lossy(), "wait": true})).unwrap();
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    assert!(jobs[0]["result"].get("error").is_none(), "{jobs}");
    // the copy opens with its media next to it, and plays the same frames
    drop(s); // Windows does not allow renaming a directory while decoder files are open.
    std::fs::rename(root.join("Media"), root.join("Media-gone")).unwrap();
    let mut t = opened(r["project"].as_str().unwrap());
    for it in t.project.items.values().filter_map(|i| i.as_media()) {
        let MediaRef::File { path } = &it.media else { panic!() };
        assert!(std::path::Path::new(path).starts_with(&dest), "{path}");
        assert!(it.identity.is_some());
    }
    let after: Vec<Vec<u8>> = (0..36).step_by(5).map(|f| frame_rgba(&mut t, f, 1.0).2).collect();
    assert_eq!(before, after);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn consolidate_trims_to_used_ranges_with_handles() {
    let root = tmp_dir("pm-consolidate");
    let (mut s, items, _) = project(&root);
    let frames = [0i64, 5, 11, 12, 20, 35];
    let before: Vec<Vec<u8>> = frames.iter().map(|f| frame_rgba(&mut s, *f, 1.0).2).collect();
    let dest = root.join("Consolidated");
    let r = s
        .execute(
            "file.projectManager",
            json!({"destination": dest.to_string_lossy(), "mode": "consolidate", "handles": 4, "preset": "prores_hq", "wait": true}),
        )
        .unwrap();
    let trim = &r["files"].as_array().unwrap().iter().find(|f| f["item"] == json!(items[0].0)).unwrap()["trim"];
    let rate = filmcraft_time::FrameRate::FPS_24;
    assert_eq!(trim["start"], json!(rate.tick_of(16).0), "used from frame 20, 4 handle frames: {r}");
    assert_eq!(trim["end"], json!(rate.tick_of(36).0), "used through frame 31, 4 handles: {r}");
    assert!(r["resultBytes"].as_u64().unwrap() > 0);
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    assert!(jobs[0]["result"].get("error").is_none(), "{jobs}");
    let mut t = opened(r["project"].as_str().unwrap());
    let a = t.project.item(items[0]).unwrap().as_media().unwrap().clone();
    assert_eq!(a.info.duration, rate.tick_of(20), "a.mov now holds only the used range");
    for (k, f) in frames.iter().enumerate() {
        let q = psnr(&frame_rgba(&mut t, *f, 1.0).2, &before[k]);
        assert!(q > 38.0, "frame {f}: {q:.1} dB");
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn nested_subclip_keeps_its_whole_parent_chain_used() {
    use filmcraft_project::{ItemKind, Label};
    use filmcraft_time::{Tick, TimeRange};
    let root = tmp_dir("pm-nested-subclip");
    let (s, items, _) = project(&root);
    let seq = s.state.active_sequence.unwrap();
    let mut p = (*s.project).clone();
    let range = TimeRange::new(Tick::ZERO, Tick(1000));
    let inner = p.add_item("Inner", Label::Iris, ItemKind::Subclip { parent: items[0], range, restrict_trims: false }, None);
    let outer = p.add_item("Outer", Label::Iris, ItemKind::Subclip { parent: inner, range, restrict_trims: false }, None);
    for t in p.sequence_mut(seq).unwrap().all_tracks_mut() {
        for x in t.items.iter_mut().filter(|i| i.item == items[0]) {
            x.item = outer;
        }
    }
    let (_, used) = super::project_manager::used_ranges(&p, &[seq]);
    assert!(used.contains(&outer) && used.contains(&inner) && used.contains(&items[0]), "{used:?}");
    let _ = std::fs::remove_dir_all(&root);
}
