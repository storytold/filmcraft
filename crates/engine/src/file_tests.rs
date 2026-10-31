//! `.fcproj` save/open through the command interface: schema envelope, legacy upgrade, Save a Copy,
//! Revert.

use super::*;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("fc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn saved_file_is_versioned_and_save_copy_keeps_state() {
    let d = temp_dir("schema");
    let path = d.join("p.fcproj").to_string_lossy().to_string();
    let mut s = demo();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["schema_version"], filmcraft_format::SCHEMA_VERSION);
    assert_eq!(v["format"], "filmcraft.project");
    s.execute("sequence.addEdit", json!({"seconds": 1.0})).unwrap();
    assert!(s.is_dirty());
    let copy = d.join("copy.fcproj").to_string_lossy().to_string();
    s.execute("file.saveCopy", json!({"path": copy})).unwrap();
    assert!(s.is_dirty(), "Save a Copy leaves the project unsaved");
    assert_eq!(s.path.as_deref(), Some(path.as_str()));
    let mut t = Session::default();
    t.execute("file.open", json!({"path": copy})).unwrap();
    assert_eq!(*t.project, *s.project);
    // Revert discards the edit.
    s.execute("file.revert", json!({})).unwrap();
    assert!(!s.is_dirty());
    assert_ne!(*t.project, *s.project);
    assert!(!Session::default().is_enabled("file.revert"), "nothing to revert to before a save");
    // No temp files left next to the project.
    let stray: Vec<_> = std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
    assert!(stray.is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn legacy_project_opens_and_first_save_keeps_a_backup() {
    let d = temp_dir("legacy");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../format/tests/fixtures/v1-edit.fcproj");
    let path = d.join("old.fcproj");
    std::fs::copy(&fixture, &path).unwrap();
    let p = path.to_string_lossy().to_string();
    let mut s = Session::default();
    let r = s.execute("file.open", json!({"path": p})).unwrap();
    assert_eq!(r["schemaVersion"], 1);
    assert_eq!(r["migrated"], true);
    assert_eq!(s.project.name, "Legacy Edit");
    assert!(s.drain_events().iter().any(|e| matches!(e, Event::Toast { message, .. } if message.contains("Upgraded"))));
    let r = s.execute("file.save", json!({})).unwrap();
    let backup = r["backup"].as_str().unwrap().to_string();
    assert!(backup.ends_with("old (schema v1 backup).fcproj"), "{backup}");
    assert_eq!(std::fs::read(&backup).unwrap(), std::fs::read(&fixture).unwrap());
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v["schema_version"], filmcraft_format::SCHEMA_VERSION);
    // Second save: no new backup.
    assert!(s.execute("file.save", json!({})).unwrap()["backup"].is_null());
    // A file from the future is refused clearly.
    let mut v = v;
    v["schema_version"] = json!(filmcraft_format::SCHEMA_VERSION + 1);
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    let e = Session::default().execute("file.open", json!({"path": p})).unwrap_err().to_string();
    assert!(e.contains("newer version of FilmCraft"), "{e}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn project_is_named_after_its_file() {
    let d = temp_dir("name");
    let mut s = demo();
    let a = d.join("Apollo 11 - Tranquility.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": a})).unwrap();
    assert_eq!(s.project.name, "Apollo 11 - Tranquility");
    assert_eq!(s.project.root.name, "Apollo 11 - Tranquility");
    assert!(!s.is_dirty(), "renaming on Save As is not an edit");
    // Save a Copy keeps the current name.
    let c = d.join("Backup copy.fcproj").to_string_lossy().to_string();
    s.execute("file.saveCopy", json!({"path": c})).unwrap();
    assert_eq!(s.project.name, "Apollo 11 - Tranquility");
    s.execute("file.open", json!({"path": c})).unwrap();
    assert_eq!(s.project.name, "Apollo 11 - Tranquility");
    let _ = std::fs::remove_dir_all(&d);
}

/// #1545: `file.newProject` answers with the project it made (name and revision, as
/// `project_inspect` reports them) instead of `null`.
#[test]
fn new_project_answers_with_its_name_and_revision() {
    let mut s = Session::default();
    let r = s.execute("file.newProject", json!({"name": "Trailer"})).unwrap();
    assert_eq!(r["name"], "Trailer");
    assert_eq!(r["revision"].as_u64(), Some(s.revision));
    assert_eq!(s.project.name, "Trailer");
    let r = s.execute("file.newProject", json!({})).unwrap();
    assert_eq!(r["name"], "Untitled", "the default name");
}

/// Media paths are stored relative to the project file, so the project and the media beside it
/// keep working when the folder moves to another drive, share or machine (#3067). Files from
/// older builds hold absolute paths and open the same as before.
#[test]
fn media_paths_are_stored_relative_to_the_project_file() {
    let d = temp_dir("rel-paths");
    let media = d.join("media");
    std::fs::create_dir_all(&media).unwrap();
    let clip = media.join("clip.mov");
    crate::media_test_util::make_movie(&clip, filmcraft_media::DemoScene::OceanSunset, 64, 36, 4);
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [clip.to_string_lossy()]})).unwrap();
    assert_eq!(r["errors"], json!([]), "{r}");
    let item = filmcraft_project::ItemId(r["items"][0].as_u64().unwrap());
    let proj = d.join("cut.fcproj");
    s.execute("file.saveAs", json!({"path": proj.to_string_lossy()})).unwrap();
    // the file holds the relative form; the session keeps the path it imported
    let loaded = filmcraft_format::decode(&std::fs::read(&proj).unwrap()).unwrap();
    let m = loaded.project.item(item).and_then(|i| i.as_media()).unwrap();
    assert_eq!(m.media, filmcraft_project::MediaRef::File { path: "media/clip.mov".into() });
    assert!(
        matches!(&s.project.item(item).unwrap().as_media().unwrap().media, filmcraft_project::MediaRef::File { path } if path == clip.to_string_lossy().as_ref())
    );
    // Save As to another folder rebases the stored path, it does not blindly copy it.
    let other = d.join("nested").join("saveas.fcproj");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    s.execute("file.saveAs", json!({"path": other.to_string_lossy()})).unwrap();
    let loaded = filmcraft_format::decode(&std::fs::read(&other).unwrap()).unwrap();
    let m = loaded.project.item(item).and_then(|i| i.as_media()).unwrap();
    assert_eq!(m.media, filmcraft_project::MediaRef::File { path: "../media/clip.mov".into() });
    // Move the whole folder: the moved project opens with its media found.
    let moved = temp_dir("rel-paths-moved");
    std::fs::remove_dir(&moved).unwrap();
    std::fs::rename(&d, &moved).unwrap();
    let mut t = Session::default();
    let r = t.execute("file.open", json!({"path": moved.join("cut.fcproj").to_string_lossy()})).unwrap();
    assert_eq!(r["missingMedia"], json!(0), "{r}");
    let m = t.project.item(item).and_then(|i| i.as_media()).unwrap();
    assert_eq!(m.media, filmcraft_project::MediaRef::File { path: moved.join("media").join("clip.mov").to_string_lossy().into_owned() });
    // A file written by an older build holds absolute paths; it opens and finds its media too.
    let mut doc: Value = serde_json::from_slice(&std::fs::read(moved.join("cut.fcproj")).unwrap()).unwrap();
    doc["project"]["items"][item.0.to_string()]["kind"]["Media"]["media"]["File"]["path"] = json!(moved.join("media").join("clip.mov").to_string_lossy());
    let old = moved.join("old.fcproj");
    std::fs::write(&old, serde_json::to_vec(&doc).unwrap()).unwrap();
    let mut u = Session::default();
    let r = u.execute("file.open", json!({"path": old.to_string_lossy()})).unwrap();
    assert_eq!(r["missingMedia"], json!(0), "{r}");
    let _ = std::fs::remove_dir_all(&moved);
}

/// Opening the demo after a saved project must not keep that path: Save would overwrite it.
#[test]
fn open_demo_project_clears_saved_path_so_save_does_not_overwrite() {
    let d = temp_dir("demo-path");
    let original = d.join("original.fcproj").to_string_lossy().to_string();
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Original"})).unwrap();
    s.execute("file.newBin", json!({"name": "ORIGINAL"})).unwrap();
    s.execute("file.saveAs", json!({"path": original})).unwrap();
    s.execute("file.openDemoProject", json!({})).unwrap();
    assert!(s.path.is_none(), "demo is unsaved, like New Project");
    let err = s.execute("file.save", json!({})).unwrap_err().to_string();
    assert!(err.contains("no path") || err.contains("Save As"), "{err}");
    let mut reopen = Session::default();
    reopen.execute("file.open", json!({"path": original})).unwrap();
    assert!(
        reopen.project.root.children.iter().any(|e| matches!(e, filmcraft_project::BinEntry::Bin(b) if b.name == "ORIGINAL")),
        "original.fcproj still has the ORIGINAL bin"
    );
    let _ = std::fs::remove_dir_all(&d);
}
