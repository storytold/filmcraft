//! Offline media and relinking end to end: moved media opens offline (the slate renders),
//! relinking one file finds the others by folder remap, fingerprint mismatches are refused,
//! Make Offline, folder search and undo.

use filmcraft_media::DemoScene;
use filmcraft_project::ItemId;
use filmcraft_render::offline::{OfflineReason, slate_rgba8};
use serde_json::json;

use crate::Session;
use crate::media_test_util::{frame_rgba, make_movie, psnr, session_with, tmp_dir};
use crate::relink::{apply_remap, derive_remap};

const W: u32 = 160;
const H: u32 = 90;

/// A replacement whose bytes can be decoded but not fingerprinted must not inherit the old identity.
#[test]
fn replace_footage_clears_unreadable_identity_and_undo_restores_it() {
    struct NoFingerprint;
    impl crate::Services for NoFingerprint {
        fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
            std::fs::read(path)
        }
        fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
            crate::Services::write_file(&crate::FsServices, path, data)
        }
        fn file_size(&self, _path: &str) -> std::io::Result<u64> {
            Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "fingerprint metadata unavailable"))
        }
    }
    let root = tmp_dir("replace-no-fingerprint");
    let original = root.join("original.mov");
    let replacement = root.join("replacement.mov");
    make_movie(&original, DemoScene::Aurora, W, H, 12);
    make_movie(&replacement, DemoScene::Dunes, W, H, 12);
    let (mut s, items, _) = session_with(&[&original]);
    let item = items[0];
    let before = s.project.clone();
    assert!(before.item(item).unwrap().as_media().unwrap().identity.is_some());
    s.services = std::sync::Arc::new(NoFingerprint);
    s.execute("media.replaceFootage", json!({"item":item.0,"path":replacement.to_string_lossy()})).unwrap();
    let media = s.project.item(item).unwrap().as_media().unwrap();
    assert_eq!(media.media, filmcraft_project::MediaRef::File { path: replacement.to_string_lossy().into_owned() });
    assert!(media.identity.is_none(), "the old footage's fingerprint cannot describe the replacement");
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
    let path = root.join("replaced.fcproj");
    s.execute("file.save", json!({"path":path.to_string_lossy()})).unwrap();
    assert_eq!(open(path.to_str().unwrap()).project.item(item), after.item(item));
    let _ = std::fs::remove_dir_all(root);
}

/// A differently sized replacement keeps its visible centre in sequence coordinates.
#[test]
fn replace_footage_keeps_smaller_and_larger_sources_centered() {
    let root = tmp_dir("replace-centre-pixels");
    let original = root.join("original.mov");
    make_movie(&original, DemoScene::Aurora, W, H, 12);
    let (mut s, items, _) = session_with(&[&original]);
    for (width, height) in [(80, 40), (240, 120), (80, 40)] {
        let replacement = root.join(format!("{width}-{height}.png"));
        // A small opaque marker at the source centre makes both up- and down-sizing visible.
        image::RgbaImage::from_fn(width, height, |x, y| {
            if (width / 2 - 10..width / 2 + 10).contains(&x) && (height / 2 - 5..height / 2 + 5).contains(&y) {
                image::Rgba([255, 255, 255, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        })
        .save(&replacement)
        .unwrap();
        let before = s.project.clone();
        s.execute("media.replaceFootage", json!({"item":items[0].0,"path":replacement.to_string_lossy()})).unwrap();
        let after = s.project.clone();
        let (w, h, pixels) = frame_rgba(&mut s, 2, 1.0);
        assert_eq!((w, h), (W as usize, H as usize));
        let lit: Vec<(usize, usize)> =
            pixels.as_chunks::<4>().0.iter().enumerate().filter(|(_, p)| p[0] > 100 || p[1] > 100 || p[2] > 100).map(|(i, _)| (i % w, i / w)).collect();
        assert!(lit.len() > 10);
        assert_eq!(
            lit.iter().map(|p| p.0).min().unwrap() + lit.iter().map(|p| p.0).max().unwrap(),
            w - 1,
            "horizontal centre after {width}x{height} replacement"
        );
        assert_eq!(
            lit.iter().map(|p| p.1).min().unwrap() + lit.iter().map(|p| p.1).max().unwrap(),
            h - 1,
            "vertical centre after {width}x{height} replacement"
        );
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
    }
    let path = root.join("centred.fcproj");
    s.execute("file.save", json!({"path":path.to_string_lossy()})).unwrap();
    assert_eq!(open(path.to_str().unwrap()).project, s.project);
    let _ = std::fs::remove_dir_all(root);
}

/// Default pivots follow the source through subclips; custom and animated pivots stay intact.
#[test]
fn replace_footage_preserves_custom_motion_and_centers_subclips_in_every_sequence() {
    use filmcraft_geom::Vec2;
    use filmcraft_project::{ItemKind, Keyframe, Label, ParamValue, TrackKind};
    use filmcraft_time::{Tick, TimeRange};
    let root = tmp_dir("replace-centre-motion");
    let original = root.join("original.mov");
    let replacement = root.join("replacement.mov");
    make_movie(&original, DemoScene::Aurora, W, H, 12);
    make_movie(&replacement, DemoScene::Dunes, W * 2, H * 2, 12);
    let (mut s, items, first) = session_with(&[&original, &original]);
    let rate = s.sequence_rate();
    let p = std::sync::Arc::make_mut(&mut s.project);
    let range = TimeRange::new(Tick::ZERO, rate.tick_of(12));
    let sub = p.add_item("sub", Label::Iris, ItemKind::Subclip { parent: items[0], range, restrict_trims: false }, None);
    let nested = p.add_item("nested sub", Label::Iris, ItemKind::Subclip { parent: sub, range, restrict_trims: false }, None);
    let settings = p.sequence(first).unwrap().settings.clone();
    let second = p.new_sequence("Other sequence", settings, 1, 0, None);
    let mut clips = Vec::new();
    for (source, kind) in [(nested, 0), (items[0], 1), (items[0], 2)] {
        let mut clip = p.make_track_item(source, TrackKind::Video, Tick::ZERO, range, rate).unwrap();
        for e in &mut clip.effects {
            filmcraft_project::resolve_auto_points(e, (W, H), (W, H));
        }
        let motion = clip.effect_mut("motion").unwrap();
        motion.params.get_mut("position").unwrap().value = ParamValue::Vec2(Vec2::new(31.0, 42.0));
        motion.params.get_mut("scale").unwrap().value = ParamValue::Float(75.0);
        motion.params.get_mut("rotation").unwrap().value = ParamValue::Float(12.0);
        let anchor = motion.params.get_mut("anchor").unwrap();
        if kind == 1 {
            anchor.value = ParamValue::Vec2(Vec2::new(12.0, 17.0));
        }
        if kind == 2 {
            anchor.keyframes = vec![Keyframe::new(Tick::ZERO, anchor.value.clone()), Keyframe::new(rate.tick_of(5), ParamValue::Vec2(Vec2::new(20.0, 30.0)))];
        }
        clips.push(clip);
    }
    p.sequence_mut(second).unwrap().video_tracks[0].items = clips;
    let mut expected = p.sequence(second).unwrap().clone();
    expected.video_tracks[0].items[0].effect_mut("motion").unwrap().params.get_mut("anchor").unwrap().value =
        ParamValue::Vec2(Vec2::new(f64::from(W), f64::from(H)));
    let other = p.item(items[1]).unwrap().clone();
    let before = s.project.clone();
    s.execute("media.replaceFootage", json!({"item":items[0].0,"path":replacement.to_string_lossy()})).unwrap();
    assert_eq!(s.project.sequence(second), Some(&expected));
    assert_eq!(s.project.item(items[1]), Some(&other));
    let first_seq = s.project.sequence(first).unwrap();
    assert_eq!(first_seq.video_tracks[0].items[0].effect("motion").unwrap().vec2_at("anchor", Tick::ZERO), Vec2::new(f64::from(W), f64::from(H)));
    assert_eq!(first_seq.video_tracks[0].items[1], before.sequence(first).unwrap().video_tracks[0].items[1]);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Replacement preserves edits, refreshes source pixels, and survives undo/redo and reopen.
fn replace_footage_preserves_edits_updates_pixels_and_round_trips() {
    let root = tmp_dir("replace-footage");
    let original = root.join("original.mov");
    let replacement = root.join("replacement.mov");
    make_movie(&original, DemoScene::Aurora, W, H, 12);
    // A shorter replacement with a different size must not move or trim existing edits.
    make_movie(&replacement, DemoScene::Dunes, W * 2, H * 2, 6);
    let (mut s, items, seq) = session_with(&[&original, &original]);
    let item = items[0];
    s.edit("attach test proxy", |project, _| {
        project.item_mut(item).unwrap().as_media_mut().unwrap().proxy =
            Some(filmcraft_project::MediaRef::File { path: original.to_string_lossy().into_owned() });
        Ok(())
    })
    .unwrap();
    let before = s.project.clone();
    let pixels = frame_rgba(&mut s, 2, 1.0).2;
    // Explicit targeting works without a Project selection, as required by CLI/MCP callers.
    s.state.project_selection.clear();
    s.execute("media.replaceFootage", json!({"item":item.0,"path":replacement.to_string_lossy(),"relinkOthers":true,"alignTimecode":true})).unwrap();
    let after = s.project.clone();
    let mut expected = before.sequence(seq).unwrap().clone();
    for clip in expected.all_tracks_mut().flat_map(|t| t.items.iter_mut()).filter(|c| c.item == item) {
        if let Some(motion) = clip.effect_mut("motion") {
            motion.params.get_mut("anchor").unwrap().value = filmcraft_project::ParamValue::Vec2(filmcraft_geom::Vec2::new(f64::from(W), f64::from(H)));
        }
    }
    assert_eq!(after.sequence(seq), Some(&expected), "only the default source anchor changes; all edits survive");
    assert_eq!(after.items.len(), before.items.len());
    let media = after.item(item).unwrap().as_media().unwrap();
    assert_eq!(media.info.video.as_ref().unwrap().width, W * 2);
    assert!(media.proxy.is_none(), "old proxy must not hide replacement footage");
    assert_eq!(after.item(items[1]), before.item(items[1]), "another bin item using the original file is untouched");
    assert_ne!(frame_rgba(&mut s, 2, 1.0).2, pixels, "cached source is replaced immediately");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before);
    assert_eq!(frame_rgba(&mut s, 2, 1.0).2, pixels, "undo invalidates the replacement source too");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after);
    let path = root.join("replaced.fcproj");
    s.execute("file.save", json!({"path":path.to_string_lossy()})).unwrap();
    let reopened = open(path.to_str().unwrap());
    assert_eq!(reopened.project.item(item), after.item(item));
    assert_eq!(reopened.project.sequence(seq), after.sequence(seq));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Invalid targets and files must leave the project and its undo history unchanged.
fn replace_footage_rejects_bad_targets_and_files_without_mutation() {
    let root = tmp_dir("replace-invalid");
    let original = root.join("original.mov");
    make_movie(&original, DemoScene::Aurora, W, H, 12);
    let (mut s, items, seq) = session_with(&[&original]);
    s.state.project_selection.clear();
    assert!(!s.is_enabled("media.replaceFootage"));
    s.state.project_selection = vec![seq];
    assert!(!s.is_enabled("media.replaceFootage"));
    s.state.project_selection = vec![items[0], seq];
    assert!(!s.is_enabled("media.replaceFootage"));
    s.state.project_selection = vec![items[0]];
    assert!(s.is_enabled("media.replaceFootage"));
    let corrupt = root.join("bad.mov");
    std::fs::write(&corrupt, b"not a movie").unwrap();
    let before = s.project.clone();
    let undo = s.undo_mark();
    for p in [
        json!(null),
        json!([]),
        json!({}),
        json!({"item":u64::MAX,"path":"bad"}),
        json!({"item":seq.0,"path":original.to_string_lossy()}),
        json!({"item":items[0].0,"path":root.join("missing.mov").to_string_lossy()}),
        json!({"item":items[0].0,"path":corrupt.to_string_lossy()}),
    ] {
        assert!(s.execute("media.replaceFootage", p).is_err());
        assert_eq!(s.project, before);
        assert_eq!(s.undo_mark(), undo);
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// A completed job for another source cannot overwrite replacement footage.
fn replace_footage_ignores_old_background_proxy_and_ingest_outputs() {
    use crate::proxies::{OnDone, PendingJob};
    let root = tmp_dir("replace-pending");
    let original = root.join("original.mov");
    let replacement = root.join("replacement.mov");
    make_movie(&original, DemoScene::Aurora, W, H, 12);
    // Matching properties: proxy validation alone cannot tell this is old footage.
    make_movie(&replacement, DemoScene::Dunes, W, H, 12);
    for on_done in [OnDone::AttachProxies, OnDone::ReplaceMedia] {
        let (mut s, items, _) = session_with(&[&original, &original]);
        let sources = items
            .iter()
            .map(|id| {
                let media = s.project.item(*id).unwrap().as_media().unwrap();
                (*id, media.media.clone(), media.identity)
            })
            .collect();
        s.execute("media.replaceFootage", json!({"item":items[0].0,"path":replacement.to_string_lossy()})).unwrap();
        let expected = s.project.item(items[0]).unwrap().clone();
        s.media_jobs.push(PendingJob {
            job: u64::MAX,
            on_done,
            sources,
            outputs: std::sync::Arc::new(std::sync::Mutex::new(items.iter().map(|id| (*id, original.to_string_lossy().into_owned())).collect())),
        });
        crate::proxies::poll(&mut s);
        assert_eq!(s.project.item(items[0]), Some(&expected), "stale output cannot undo the replacement");
        if on_done == OnDone::AttachProxies {
            assert!(s.project.item(items[1]).unwrap().as_media().unwrap().proxy.is_some(), "valid outputs in the same batch still attach");
        }
        assert!(s.media_jobs.is_empty());
    }
    let _ = std::fs::remove_dir_all(root);
}

/// Replacing with the same file invalidates older jobs, without discarding other batch items.
#[test]
fn replace_footage_invalidates_pending_jobs_for_the_same_source() {
    use crate::proxies::{OnDone, PendingJob};
    let root = tmp_dir("replace-same-source");
    let original = root.join("original.mov");
    make_movie(&original, DemoScene::Aurora, W, H, 12);
    for on_done in [OnDone::AttachProxies, OnDone::ReplaceMedia] {
        let (mut s, items, _) = session_with(&[&original, &original]);
        let sources = items
            .iter()
            .map(|id| {
                let media = s.project.item(*id).unwrap().as_media().unwrap();
                (*id, media.media.clone(), media.identity)
            })
            .collect();
        let outputs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        s.media_jobs.push(PendingJob { job: u64::MAX, on_done, sources, outputs: outputs.clone() });
        // Rejected replacements must not invalidate work that still belongs to this source.
        assert!(s.execute("media.replaceFootage", json!({"item":items[0].0,"path":root.join("missing.mov").to_string_lossy()})).is_err());
        assert_eq!(s.media_jobs[0].sources.len(), 2);
        s.execute("media.replaceFootage", json!({"item":items[0].0,"path":original.to_string_lossy()})).unwrap();
        assert_eq!(s.media_jobs[0].sources.iter().map(|(id, _, _)| *id).collect::<Vec<_>>(), vec![items[1]]);
        // Simulate the worker finishing only after replacement, with the unchanged source tuple.
        outputs.lock().unwrap().extend(items.iter().map(|id| (*id, original.to_string_lossy().into_owned())));
        crate::proxies::poll(&mut s);
        assert!(s.project.item(items[0]).unwrap().as_media().unwrap().proxy.is_none());
        if on_done == OnDone::AttachProxies {
            assert!(s.project.item(items[1]).unwrap().as_media().unwrap().proxy.is_some());
        }
        assert!(s.media_jobs.is_empty());
    }
    let _ = std::fs::remove_dir_all(root);
}

/// Rejected job files are cleaned up, but files adopted as current media remain available.
#[test]
fn replace_footage_cleans_up_unreferenced_job_outputs() {
    use crate::proxies::{OnDone, PendingJob};
    let root = tmp_dir("replace-cleanup");
    let original = root.join("original.mov");
    make_movie(&original, DemoScene::Aurora, W, H, 12);
    for (on_done, adopt_output) in
        [(OnDone::AttachProxies, 0), (OnDone::AttachProxies, 1), (OnDone::AttachProxies, 2), (OnDone::AttachProxies, 3), (OnDone::Nothing, 0)]
    {
        let stale = root.join("stale-proxy.mov");
        let valid = root.join("valid-proxy.mov");
        std::fs::copy(&original, &stale).unwrap();
        std::fs::copy(&original, &valid).unwrap();
        let (mut s, items, _) = session_with(&[&original, &original]);
        let sources = items
            .iter()
            .map(|id| {
                let m = s.project.item(*id).unwrap().as_media().unwrap();
                (*id, m.media.clone(), m.identity)
            })
            .collect();
        s.media_jobs.push(PendingJob {
            job: u64::MAX,
            on_done,
            sources,
            outputs: std::sync::Arc::new(std::sync::Mutex::new(vec![
                (items[0], stale.to_string_lossy().into_owned()),
                (items[1], valid.to_string_lossy().into_owned()),
            ])),
        });
        let alias = root.join(".").join("stale-proxy.mov");
        let replacement = match adopt_output {
            1 | 3 => &stale,
            2 => &alias,
            _ => &original,
        };
        s.execute("media.replaceFootage", json!({"item":items[0].0,"path":replacement.to_string_lossy()})).unwrap();
        if adopt_output == 3 {
            // The output is no longer current, but undo must still be able to restore it.
            s.execute("media.replaceFootage", json!({"item":items[0].0,"path":original.to_string_lossy()})).unwrap();
        }
        crate::proxies::poll(&mut s);
        assert_eq!(stale.exists(), adopt_output != 0 || on_done == OnDone::Nothing, "only an unreferenced obsolete output is deleted");
        assert!(valid.exists(), "accepted output is untouched");
        assert!(original.exists(), "input source is untouched");
        assert_eq!(s.project.item(items[1]).unwrap().as_media().unwrap().proxy.is_some(), on_done == OnDone::AttachProxies);
    }
    let _ = std::fs::remove_dir_all(root);
}

/// A saved project at `<root>/Project/p.fcproj` using `<root>/Media/{a,b}.mov`.
fn saved_project(root: &std::path::Path) -> (String, Vec<ItemId>) {
    let media = root.join("Media");
    std::fs::create_dir_all(&media).unwrap();
    let (a, b) = (media.join("a.mov"), media.join("b.mov"));
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    make_movie(&b, DemoScene::CityNight, W, H, 12);
    let (mut s, items, _) = session_with(&[&a, &b]);
    let proj = root.join("Project");
    std::fs::create_dir_all(&proj).unwrap();
    let path = proj.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.save", json!({"path": path})).unwrap();
    (path, items)
}

fn open(path: &str) -> Session {
    let mut s = Session::default();
    s.execute("file.open", json!({"path": path})).unwrap();
    s
}

#[test]
fn identity_and_remap_helpers() {
    let a = crate::relink::identity_of_bytes(&vec![7u8; 3 << 20]);
    let mut v = vec![7u8; 3 << 20];
    v[(3 << 20) - 5] = 1;
    let b = crate::relink::identity_of_bytes(&v);
    assert_eq!(a.size, b.size);
    assert_ne!(a.fingerprint, b.fingerprint, "a change in the last MiB changes the fingerprint");
    assert_eq!(derive_remap("/Volumes/A/shoot/day1/a.mov", "/Users/me/shoot/day1/a.mov"), Some(("/Volumes/A".into(), "/Users/me".into())));
    assert_eq!(derive_remap(r"D:\proj\media\a.mov", "/Volumes/D/proj/media/a.mov"), Some(("D:".into(), "/Volumes/D".into())));
    assert_eq!(apply_remap(r"D:\proj\media\b.mov", "D:", "/Volumes/D").as_deref(), Some("/Volumes/D/proj/media/b.mov"));
    assert_eq!(apply_remap("/Volumes/AB/x.mov", "/Volumes/A", "/x"), None, "prefix must end at a separator");
}

#[test]
fn moved_media_opens_offline_and_relinks_others_by_folder() {
    let root = tmp_dir("relink-moved");
    let (path, items) = saved_project(&root);
    let online = frame_rgba(&mut open(&path), 3, 1.0).2;
    // move the media folder
    let moved = root.join("Moved").join("Media");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(root.join("Media"), &moved).unwrap();

    let mut s = Session::default();
    let r = s.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(r["missingMedia"], json!(2), "{r}");
    assert!(s.offline.prompt, "the Link Media dialog is requested");
    let (w, h, px) = frame_rgba(&mut s, 3, 1.0);
    let slate = slate_rgba8(w, h, "a.mov", OfflineReason::Missing);
    assert!(psnr(&px, &slate) > 40.0, "offline clip renders the offline slate");
    let m = s.execute("media.findMissing", json!({})).unwrap();
    assert_eq!(m["missing"].as_array().unwrap().len(), 2);

    // link a; b follows by the derived folder remap
    let r = s.execute("media.relink", json!({"item": items[0].0, "path": moved.join("a.mov").to_string_lossy()})).unwrap();
    assert_eq!(r["relinked"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["remaining"], json!([]));
    assert!(!s.offline.prompt);
    assert_eq!(frame_rgba(&mut s, 3, 1.0).2, online, "relinked media renders as before the move");
    let b_path = s.project.item(items[1]).unwrap().as_media().unwrap().media.clone();
    let filmcraft_project::MediaRef::File { path: b_path } = b_path else { panic!("relinked media must remain file-based") };
    assert_eq!(std::path::Path::new(&b_path), moved.join("b.mov"));
    // undo brings the old (missing) paths back, and the slate with them
    s.execute("edit.undo", json!({})).unwrap();
    assert!(psnr(&frame_rgba(&mut s, 3, 1.0).2, &slate) > 40.0);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(frame_rgba(&mut s, 3, 1.0).2, online);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn auto_relink_by_prefix_and_by_folder_search() {
    let root = tmp_dir("relink-auto");
    let (path, items) = saved_project(&root);
    let dest = root.join("Archive").join("2026");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::rename(root.join("Media"), dest.join("Media")).unwrap();
    let mut s = open(&path);
    let r = s.execute("media.autoRelink", json!({"from": root.join("Media").to_string_lossy(), "to": dest.join("Media").to_string_lossy()})).unwrap();
    assert_eq!(r["relinked"].as_array().unwrap().len(), 2, "{r}");
    assert!(s.execute("media.findMissing", json!({})).unwrap()["missing"].as_array().unwrap().is_empty());

    // again, this time searching a folder tree
    let mut s = open(&path);
    let r = s.execute("media.search", json!({"folder": root.to_string_lossy(), "item": items[1].0})).unwrap();
    let c = &r["results"][0]["candidates"][0];
    assert_eq!(c["identityMatch"], json!(true), "{r}");
    let r = s.execute("media.autoRelink", json!({"folder": root.to_string_lossy()})).unwrap();
    assert_eq!(r["relinked"].as_array().unwrap().len(), 2, "{r}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn fingerprint_mismatch_is_refused_unless_forced() {
    let root = tmp_dir("relink-mismatch");
    let (path, items) = saved_project(&root);
    std::fs::remove_dir_all(root.join("Media")).unwrap();
    // same name, duration, size and rate -- different pictures
    let other = root.join("Other");
    std::fs::create_dir_all(&other).unwrap();
    make_movie(&other.join("a.mov"), DemoScene::Dunes, W, H, 12);
    let mut s = open(&path);
    let e = s.execute("media.relink", json!({"item": items[0].0, "path": other.join("a.mov").to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("not the same file"), "{e}");
    assert_eq!(s.offline.missing.len(), 2, "nothing changed");
    let r =
        s.execute("media.relink", json!({"item": items[0].0, "path": other.join("a.mov").to_string_lossy(), "force": true, "relinkOthers": false})).unwrap();
    assert_eq!(r["relinked"].as_array().unwrap().len(), 1);
    assert_eq!(r["remaining"], json!([items[1].0]));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn forced_relink_takes_the_new_files_properties() {
    let root = tmp_dir("relink-forced-info");
    let (path, items) = saved_project(&root);
    std::fs::remove_dir_all(root.join("Media")).unwrap();
    // same name, different pictures and a different size
    let other = root.join("Other");
    std::fs::create_dir_all(&other).unwrap();
    make_movie(&other.join("a.mov"), DemoScene::Dunes, W * 2, H * 2, 12);
    let mut s = open(&path);
    s.execute("media.relink", json!({"item": items[0].0, "path": other.join("a.mov").to_string_lossy(), "force": true, "relinkOthers": false})).unwrap();
    let width = s.project.item(items[0]).and_then(|i| i.as_media()).and_then(|m| m.info.video.as_ref().map(|v| v.width));
    assert_eq!(width, Some(W * 2), "the item describes the replacement file");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn make_offline_shows_the_slate_until_relinked() {
    let root = tmp_dir("relink-offline");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let online = frame_rgba(&mut s, 3, 1.0);
    s.execute("project.select", json!({"items": [items[0].0]})).unwrap();
    s.execute("media.makeOffline", json!({})).unwrap();
    let (w, h, px) = frame_rgba(&mut s, 3, 1.0);
    assert!(psnr(&px, &slate_rgba8(w, h, "a.mov", OfflineReason::MadeOffline)) > 40.0);
    let st = s.execute("media.status", json!({"item": items[0].0})).unwrap();
    assert_eq!(st[0]["status"], json!("offline"));
    assert!(root.join("Media/a.mov").exists(), "media stays on disk by default");
    let a = root.join("Media/a.mov").to_string_lossy().into_owned();
    s.execute("media.relink", json!({"item": items[0].0, "path": a})).unwrap();
    assert_eq!(frame_rgba(&mut s, 3, 1.0), online);
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn relink_search_does_not_follow_directory_symlinks() {
    use std::os::unix::fs::symlink;

    let root = tmp_dir("relink-no-symlink-walk");
    let inside = root.join("inside");
    let outside = root.join("outside");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(inside.join("kept.mov"), b"kept").unwrap();
    std::fs::write(outside.join("outside.mov"), b"outside").unwrap();
    symlink(&outside, inside.join("loop")).unwrap();
    symlink(outside.join("outside.mov"), inside.join("linked.mov")).unwrap();

    let index = crate::relink::index_folder(&inside, 20_000);
    assert!(index.contains_key("kept.mov"));
    assert!(!index.contains_key("outside.mov"));
    // a symlinked file is still media the user can relink to
    assert!(index.contains_key("linked.mov"));
    let _ = std::fs::remove_dir_all(&root);
}

// Regression test for issue #110: after `file.import` re-imports a file at a path the project
// already listed as offline, `s.offline.missing` must drop that item so the slate and the
// "Media missing" badge clear without reopening the project. Before the fix (the patch in
// `file.import` that calls `crate::relink::refresh(s)`), the slate stayed stale until reopen
// or per-item relink.
#[test]
fn reimport_clears_offline_list_after_path_resolves() {
    let root = tmp_dir("relink-reimport");
    let (path, items) = saved_project(&root);
    let a = root.join("Media/a.mov");

    // Delete a.mov so the saved project now points at a missing file, then reopen:
    // `on_open` -> `refresh` -> `scan` registers items[0] as missing.
    let online = frame_rgba(&mut open(&path), 3, 1.0);
    std::fs::remove_file(&a).unwrap();
    let mut s = open(&path);
    assert!(s.offline.missing.contains(&items[0]), "setup: a.mov missing on reopen");
    assert_ne!(frame_rgba(&mut s, 3, 1.0), online, "setup: the slate shows while a.mov is missing");

    // Recreate the file at the exact path the project stores, then import. `file.import`
    // must drop items[0] from `s.offline.missing`. Without the fix, the entry stays stale
    // and the user has to reopen or `media.relink` each item.
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    s.execute("file.import", json!({"paths": [a.to_string_lossy()]})).unwrap();
    assert!(!s.offline.missing.contains(&items[0]), "issue #110 -- file.import did not refresh s.offline.missing (still contains {:?})", s.offline.missing);
    assert_eq!(s.execute("media.findMissing", json!({})).unwrap()["missing"].as_array().unwrap().len(), 0);
    // and the monitors show the file again, not the cached slate
    assert_eq!(frame_rgba(&mut s, 3, 1.0), online);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn explicit_folder_remaps_normalize_both_prefixes_and_respect_boundaries() {
    assert_eq!(apply_remap(r"C:\shoot\Media\a.mov", r"C:\shoot\Media\", r"D:\archive\Media\"), Some("D:/archive/Media/a.mov".into()));
    assert_eq!(apply_remap("C:/shoot/Media/a.mov", r"C:\shoot\Media", r"D:\archive\Media"), Some("D:/archive/Media/a.mov".into()));
    assert_eq!(apply_remap("C:/shoot/Media2/a.mov", r"C:\shoot\Media", r"D:\archive\Media"), None);
}

/// #1054: an export (or a rendered frame) that draws the offline slate names the offline items it
/// used, with their status, so a headless caller can tell placeholder frames from real ones. Media
/// outside the exported range is not reported.
#[test]
fn export_names_the_offline_media_it_rendered() {
    let root = tmp_dir("relink-export-offline");
    let (path, items) = saved_project(&root);
    let out = |name: &str| root.join(name).to_string_lossy().into_owned();
    let mut s = open(&path);
    let r = s
        .execute("file.exportMedia", json!({"path": out("on.png"), "format": "png", "range": "custom", "startSeconds": 0, "endSeconds": 0.05, "wait": true}))
        .unwrap();
    assert_eq!(r["offlineMedia"], json!([]), "{r}");
    std::fs::remove_file(root.join("Media/a.mov")).unwrap();
    let mut s = open(&path);
    // b.mov (frames 12..24) is still there
    let r = s
        .execute("file.exportMedia", json!({"path": out("b.png"), "format": "png", "range": "custom", "startSeconds": 0.6, "endSeconds": 0.65, "wait": true}))
        .unwrap();
    assert_eq!(r["offlineMedia"], json!([]), "{r}");
    let r = s.execute("file.exportMedia", json!({"path": out("all.wav"), "format": "wav", "wait": true})).unwrap();
    let off = r["offlineMedia"].as_array().unwrap();
    assert_eq!(off.len(), 1, "{r}");
    assert_eq!(off[0]["item"], json!(items[0].0));
    assert_eq!(off[0]["name"], json!("a.mov"));
    assert_eq!(off[0]["status"], json!("missing"));
    assert!(off[0]["path"].as_str().unwrap().ends_with("a.mov"), "{r}");
    // made offline on purpose
    s.execute("media.makeOffline", json!({"items": [items[1].0]})).unwrap();
    let r = s
        .execute("file.exportMedia", json!({"path": out("b2.png"), "format": "png", "range": "custom", "startSeconds": 0.6, "endSeconds": 0.65, "wait": true}))
        .unwrap();
    assert_eq!(
        r["offlineMedia"].as_array().map(|a| a.iter().map(|o| (o["item"].clone(), o["status"].clone())).collect::<Vec<_>>()),
        Some(vec![(json!(items[1].0), json!("offline"))]),
        "{r}"
    );
    // the queue reports it per item
    let r = s
        .execute(
            "export.queue.add",
            json!({"path": out("q.png"), "format": "png", "range": "custom", "startSeconds": 0.6, "endSeconds": 0.65, "start": true, "wait": true}),
        )
        .unwrap();
    assert_eq!(r["items"][0]["offlineMedia"][0]["status"], json!("offline"), "{r}");
    // one rendered frame
    let (_, off) = s.try_render_program_with_offline_at(0.5, s.sequence_rate().tick_of(3)).unwrap();
    assert_eq!(off.iter().map(|o| (o.item, o.status)).collect::<Vec<_>>(), [(items[0].0, "missing")]);
    let (_, off) = s.try_render_program_with_offline_at(0.5, s.sequence_rate().tick_of(30)).unwrap();
    assert!(off.is_empty(), "nothing on the timeline there: {off:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// #1054: `jobs.list` carries an export job's `offlineMedia`, for callers that poll the job (the
/// bridge and MCP long exports) instead of waiting in `file.exportMedia`.
#[test]
fn jobs_list_reports_an_exports_offline_media() {
    let root = tmp_dir("relink-jobs-offline");
    let (path, items) = saved_project(&root);
    std::fs::remove_file(root.join("Media/b.mov")).unwrap();
    let mut s = open(&path);
    let out = root.join("all.wav").to_string_lossy().into_owned();
    let job = s.execute("file.exportMedia", json!({"path": out, "format": "wav", "wait": true})).unwrap()["job"].clone();
    let jobs = s.execute("jobs.list", json!({})).unwrap();
    let j = jobs.as_array().unwrap().iter().find(|j| j["id"] == job).unwrap();
    assert_eq!(j["offlineMedia"][0]["item"], json!(items[1].0), "{j}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn deleting_bin_keeps_subclip_source_but_removes_unused_media() {
    let root = tmp_dir("delete-bin-used-media");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let p = std::sync::Arc::make_mut(&mut s.project);
    let seq = s.state.active_sequence.unwrap();
    let range = filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, p.item(items[0]).unwrap().duration());
    let sub =
        p.add_item("Subclip", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Subclip { parent: items[0], range, restrict_trims: false }, None);
    for track in p.sequence_mut(seq).unwrap().all_tracks_mut() {
        track.items.retain(|clip| clip.item != items[1]);
        for clip in &mut track.items {
            clip.item = sub;
        }
    }
    let bin = p.add_bin("Sources", None);
    p.move_to_bin(&items, Some(bin)).unwrap();
    let before = s.active_sequence().unwrap().clone();
    s.execute("project.delete", json!({"items": [bin.0, items[0].0, bin.0]})).unwrap();
    assert_eq!(s.active_sequence().unwrap(), &before);
    assert!(s.project.root.find_bin(bin).is_none());
    assert!(s.project.item(items[0]).unwrap().as_media().unwrap().offline);
    assert!(s.project.item(items[1]).is_none());
    assert!(s.project.resolve_media(sub).unwrap().1.offline);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.root.parent_of(items[0]), Some(bin));
    assert!(s.project.item(items[1]).is_some());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_sequence_and_its_media_does_not_keep_an_unused_source() {
    let root = tmp_dir("delete-sequence-and-media");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let seq = s.state.active_sequence.unwrap();
    s.execute("project.delete", json!({"items": [seq.0, items[0].0]})).unwrap();
    assert!(s.project.item(seq).is_none());
    assert!(s.project.item(items[0]).is_none());
    assert!(s.project.item(items[1]).is_some());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_subclip_and_source_does_not_keep_an_unused_source() {
    let root = tmp_dir("delete-subclip-and-source");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let seq = s.state.active_sequence.unwrap();
    let p = std::sync::Arc::make_mut(&mut s.project);
    let range = filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, p.item(items[0]).unwrap().duration());
    let sub =
        p.add_item("Subclip", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Subclip { parent: items[0], range, restrict_trims: false }, None);
    for t in p.sequence_mut(seq).unwrap().all_tracks_mut() {
        for c in &mut t.items {
            if c.item == items[0] {
                c.item = sub;
            }
        }
    }
    s.execute("project.delete", json!({"items": [sub.0, items[0].0]})).unwrap();
    assert!(s.project.item(sub).is_none());
    assert!(s.project.item(items[0]).is_none(), "a removed timeline subclip must not retain its unused source");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_bin_preserves_intermediate_subclip_needed_by_surviving_clip() {
    let root = tmp_dir("delete-intermediate-subclip");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let seq = s.state.active_sequence.unwrap();
    let p = std::sync::Arc::make_mut(&mut s.project);
    let range = filmcraft_time::TimeRange::new(filmcraft_time::Tick::ZERO, p.item(items[0]).unwrap().duration());
    let parent =
        p.add_item("Parent", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Subclip { parent: items[0], range, restrict_trims: false }, None);
    let outer = p.add_item("Outer", filmcraft_project::Label::Iris, filmcraft_project::ItemKind::Subclip { parent, range, restrict_trims: false }, None);
    for t in p.sequence_mut(seq).unwrap().all_tracks_mut() {
        for c in &mut t.items {
            if c.item == items[0] {
                c.item = outer;
            }
        }
    }
    let bin = p.add_bin("Sources", None);
    p.move_to_bin(&[items[0], parent], Some(bin)).unwrap();
    let before = s.active_sequence().unwrap().clone();
    s.execute("project.delete", json!({"items": [bin.0]})).unwrap();
    assert_eq!(s.active_sequence().unwrap(), &before);
    assert!(s.project.resolve_media(outer).is_some(), "the surviving nested subclip must still resolve");
    assert!(s.project.resolve_media(outer).unwrap().1.offline);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.root.parent_of(parent), Some(bin));
    let _ = std::fs::remove_dir_all(root);
}

/// Point item `id` at `path` the way a project file from someone else could.
fn set_media_path(s: &mut Session, id: ItemId, path: &str) {
    let p = std::sync::Arc::make_mut(&mut s.project);
    let m = p.item_mut(id).and_then(|i| i.as_media_mut()).unwrap();
    m.media = filmcraft_project::MediaRef::File { path: path.to_string() };
}

// #2782: Make Offline ▸ delete removed whatever file an item's path named.
#[test]
fn make_offline_delete_only_removes_the_media_the_item_was_linked_to() {
    let root = tmp_dir("relink-delete-guard");
    let a = root.join("a.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    let (mut s, items, _) = session_with(&[&a]);
    let notes = root.join("notes.txt");
    std::fs::write(&notes, b"keep me").unwrap();
    let other = root.join("other.mov");
    make_movie(&other, DemoScene::CityNight, W, H, 12);
    let dir = root.join("folder.mov");
    std::fs::create_dir_all(&dir).unwrap();

    // not media, a different take under the same kind of name, and a folder: all survive
    for victim in [&notes, &other, &dir] {
        set_media_path(&mut s, items[0], &victim.to_string_lossy());
        let r = s.execute("media.makeOffline", json!({"items": [items[0].0], "deleteFiles": true})).unwrap();
        assert_eq!(r["deleted"], json!([]), "{victim:?}");
        assert!(victim.exists(), "{victim:?} was deleted");
    }
    // an item with no recorded identity (a placeholder, an older project) never deletes anything
    set_media_path(&mut s, items[0], &a.to_string_lossy());
    std::sync::Arc::make_mut(&mut s.project).item_mut(items[0]).and_then(|i| i.as_media_mut()).unwrap().identity = None;
    let r = s.execute("media.makeOffline", json!({"items": [items[0].0], "deleteFiles": true})).unwrap();
    assert_eq!(r["deleted"], json!([]), "{r}");
    assert!(a.exists(), "a file with no recorded identity was deleted");
    // the file the item was imported from is deleted
    let id_a = crate::relink::identity_of(&*s.services, &a.to_string_lossy()).unwrap();
    std::sync::Arc::make_mut(&mut s.project).item_mut(items[0]).and_then(|i| i.as_media_mut()).unwrap().identity = Some(id_a);
    set_media_path(&mut s, items[0], &a.to_string_lossy());
    let r = s.execute("media.makeOffline", json!({"items": [items[0].0], "deleteFiles": true})).unwrap();
    assert_eq!(r["deleted"].as_array().map(Vec::len), Some(1), "{r}");
    assert!(!a.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn new_offline_file_keeps_only_the_file_name() {
    let mut s = Session::default();
    for (given, want) in [
        ("/etc/passwd", "passwd"),
        ("..\\..\\x\\y.mov", "y.mov"),
        ("clip.mov", "clip.mov"),
        ("C:target.mov", "target.mov"),
        ("C:", "Offline.mov"),
        ("/", "Offline.mov"),
        ("..", "Offline.mov"),
    ] {
        let r = s.execute("file.newOfflineFile", json!({"fileName": given})).unwrap();
        let id = ItemId(r["item"].as_u64().unwrap());
        let m = s.project.item(id).unwrap().as_media().unwrap();
        assert_eq!(m.media, filmcraft_project::MediaRef::File { path: want.to_string() }, "{given}");
    }
}

// Offline All must be saved with the project: reopening it should not ask for the files again
// (Premiere behaves this way). It used to only close the dialog, so every open asked again.
#[test]
fn offline_all_is_saved_and_reopening_does_not_ask_again() {
    let root = tmp_dir("relink-offline-all");
    let (path, items) = saved_project(&root);
    std::fs::remove_dir_all(root.join("Media")).unwrap();
    let mut s = open(&path);
    assert!(s.offline.prompt, "setup: missing media asks");
    s.execute("media.offlineAll", json!({})).unwrap();
    assert!(!s.offline.prompt);
    assert!(items.iter().all(|i| s.project.item(*i).unwrap().as_media().unwrap().offline));
    s.execute("file.save", json!({})).unwrap();

    let s = open(&path);
    assert!(!s.offline.prompt, "reopening after Offline All must not show Link Media again");
    assert_eq!(s.offline.missing.len(), 2, "File ▸ Link Media… still lists them");

    // media that goes missing later still asks on the next open
    let mut s = open(&path);
    s.edit("relink b", |p, _| {
        let m = p.item_mut(items[1]).unwrap().as_media_mut().unwrap();
        m.offline = false;
        Ok(())
    })
    .unwrap();
    s.execute("file.save", json!({})).unwrap();
    assert!(open(&path).offline.prompt, "newly missing media asks again");
    let _ = std::fs::remove_dir_all(&root);
}
