//! Offline media and relinking end to end: moved media opens offline (the slate renders),
//! relinking one file finds the others by folder remap, fingerprint mismatches are refused,
//! Make Offline, folder search and undo.

use filmcraft_media::DemoScene;
use filmcraft_project::ItemId;
use filmcraft_render::offline::{OfflineReason, slate_rgba8};
use serde_json::json;

use crate::Session;
use crate::media_test_util::{frame_rgba, make_movie, psnr, session_with, tmp_dir};
use crate::relink::{apply_remap, derive_remap, index_folder};

const W: u32 = 160;
const H: u32 = 90;

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
    // same name, duration, size and rate — different pictures
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
fn script_marks_follow_same_file_relink_but_block_unverified_forced_replacement() {
    let root = tmp_dir("relink-script-marks");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let script = s.execute("script.importText", json!({"name":"Script","text":"a line"})).unwrap()["item"].as_u64().unwrap();
    let line = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
    let take = s.execute("script.addTake", json!({"item":script,"source":items[0].0,"startLine":line,"endLine":line})).unwrap()["take"].as_u64().unwrap();
    s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":0})).unwrap();
    s.execute("file.save", json!({"path":path})).unwrap();

    let moved = root.join("Moved");
    std::fs::rename(root.join("Media"), &moved).unwrap();
    let mut s = open(&path);
    let same = moved.join("a.mov").to_string_lossy().into_owned();
    s.execute("media.relink", json!({"item":items[0].0,"path":same,"relinkOthers":false})).unwrap();
    let script_take =
        |session: &Session| session.project.item(ItemId(script)).unwrap().as_script().unwrap().take(filmcraft_project::ScriptTakeId(take)).unwrap().clone();
    assert_eq!(script_take(&s).marks[0].source_time.0, 0);
    assert_eq!(script_take(&s).source_identity, s.project.item(items[0]).unwrap().as_media().unwrap().identity);

    let other = root.join("Other");
    std::fs::create_dir_all(&other).unwrap();
    make_movie(&other.join("a.mov"), DemoScene::Dunes, W, H, 12);
    let replacement = other.join("a.mov").to_string_lossy().into_owned();
    let err = s.execute("media.relink", json!({"item":items[0].0,"path":replacement,"force":true,"relinkOthers":false})).unwrap_err().to_string();
    assert!(err.contains(crate::relink::SCRIPT_MARKS_RELINK_REVIEW), "{err}");
    assert_eq!(script_take(&s).marks.len(), 1);
    assert_eq!(s.project.item(items[0]).unwrap().as_media().unwrap().media, filmcraft_project::MediaRef::File { path: same });

    // The command accepts a user's explicit same-footage assertion. The engine cannot infer
    // visual equivalence from bytes; the UI requires a separate review before sending it.
    s.execute("media.relink", json!({"item":items[0].0,"path":replacement,"force":true,"confirmScriptSameFootage":true,"relinkOthers":false})).unwrap();
    assert_eq!(script_take(&s).marks.len(), 1);
    assert_eq!(script_take(&s).source_identity, s.project.item(items[0]).unwrap().as_media().unwrap().identity);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn confirming_one_script_relink_does_not_confirm_automatically_found_sources() {
    let root = tmp_dir("relink-script-review-each-source");
    let (path, items) = saved_project(&root);
    let mut s = open(&path);
    let script = s.execute("script.importText", json!({"name":"Script","text":"A\nB"})).unwrap()["item"].as_u64().unwrap();
    let lines: Vec<u64> = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines.iter().map(|line| line.id.0).collect();
    let takes: Vec<u64> = items
        .iter()
        .zip(&lines)
        .map(|(source, line)| {
            let take = s.execute("script.addTake", json!({"item":script,"source":source.0,"startLine":line,"endLine":line})).unwrap()["take"].as_u64().unwrap();
            s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":0})).unwrap();
            take
        })
        .collect();
    {
        let project = std::sync::Arc::make_mut(&mut s.project);
        let doc = project.item_mut(ItemId(script)).unwrap().as_script_mut().unwrap();
        for take in &takes {
            doc.take_mut(filmcraft_project::ScriptTakeId(*take)).unwrap().source_identity = None;
        }
        // An older project can have no stored identity for an automatically matched clip.
        project.item_mut(items[1]).unwrap().as_media_mut().unwrap().identity = None;
    }
    s.execute("file.save", json!({"path":path})).unwrap();
    drop(s);

    let moved = root.join("Moved");
    std::fs::rename(root.join("Media"), &moved).unwrap();
    let mut s = open(&path);
    let result = s
        .execute(
            "media.relink",
            json!({
                "item":items[0].0,
                "path":moved.join("a.mov").to_string_lossy(),
                "relinkOthers":true,
                "confirmScriptSameFootage":true
            }),
        )
        .unwrap();
    assert_eq!(result["relinked"].as_array().unwrap().len(), 1, "{result}");
    assert_eq!(result["remaining"], json!([items[1].0]));
    assert_eq!(result["needsReview"][0]["item"], items[1].0);
    let second = s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(filmcraft_project::ScriptTakeId(takes[1])).unwrap();
    assert_eq!(second.source_identity, None);
    assert_eq!(second.marks.len(), 1);

    s.execute(
        "media.relink",
        json!({
            "item":items[1].0,
            "path":moved.join("b.mov").to_string_lossy(),
            "relinkOthers":false,
            "confirmScriptSameFootage":true
        }),
    )
    .unwrap();
    let second = s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(filmcraft_project::ScriptTakeId(takes[1])).unwrap();
    assert!(second.source_identity.is_some());
    assert_eq!(second.marks.len(), 1);
    drop(s);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn auto_relink_reports_script_review_without_blocking_other_sources() {
    for folder_search in [false, true] {
        let root = tmp_dir("auto-relink-script-review");
        let (path, items) = saved_project(&root);
        let mut s = open(&path);
        let script = s.execute("script.importText", json!({"name":"Script","text":"Line"})).unwrap()["item"].as_u64().unwrap();
        let line = s.project.item(ItemId(script)).unwrap().as_script().unwrap().lines[0].id.0;
        let take = s.execute("script.addTake", json!({"item":script,"source":items[0].0,"startLine":line,"endLine":line})).unwrap()["take"].as_u64().unwrap();
        s.execute("script.addMark", json!({"item":script,"take":take,"line":line,"sourceTime":0})).unwrap();
        let take_id = filmcraft_project::ScriptTakeId(take);
        let project = std::sync::Arc::make_mut(&mut s.project);
        project.item_mut(items[0]).unwrap().as_media_mut().unwrap().identity = None;
        project.item_mut(ItemId(script)).unwrap().as_script_mut().unwrap().take_mut(take_id).unwrap().source_identity = None;
        s.execute("file.save", json!({"path":path})).unwrap();
        drop(s);
        let moved = root.join("Moved");
        std::fs::rename(root.join("Media"), &moved).unwrap();
        let mut s = open(&path);
        let original = s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(take_id).unwrap().clone();
        let params = if folder_search {
            json!({"folder":moved.to_string_lossy()})
        } else {
            json!({"from":root.join("Media").to_string_lossy(),"to":moved.to_string_lossy()})
        };
        let result = s.execute("media.autoRelink", params).unwrap();
        assert_eq!(result["relinked"].as_array().unwrap().len(), 1, "{result}");
        assert_eq!(result["relinked"][0]["item"], items[1].0);
        assert_eq!(result["needsReview"].as_array().unwrap().len(), 1);
        assert_eq!(result["needsReview"][0]["item"], items[0].0);
        assert_eq!(result["failed"], json!([]));
        assert_eq!(result["remaining"], json!([items[0].0]));
        assert_eq!(s.project.item(ItemId(script)).unwrap().as_script().unwrap().take(take_id).unwrap(), &original);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(
            s.project.item(items[1]).unwrap().as_media().unwrap().media,
            filmcraft_project::MediaRef::File { path: root.join("Media/b.mov").to_string_lossy().into_owned() }
        );
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(
            s.project.item(items[1]).unwrap().as_media().unwrap().media,
            filmcraft_project::MediaRef::File { path: moved.join("b.mov").to_string_lossy().into_owned() }
        );
        drop(s);
        let _ = std::fs::remove_dir_all(root);
    }
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

    let index = index_folder(&inside, 20_000);
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
    // `on_open` → `refresh` → `scan` registers items[0] as missing.
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
    assert!(!s.offline.missing.contains(&items[0]), "issue #110 — file.import did not refresh s.offline.missing (still contains {:?})", s.offline.missing);
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
