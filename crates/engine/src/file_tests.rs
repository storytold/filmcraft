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
fn save_as_and_save_copy_require_explicit_replacement() {
    let d = temp_dir("save-collision");
    let original = d.join("source.fcproj").to_string_lossy().to_string();
    let target = d.join("existing.fcproj").to_string_lossy().to_string();
    let mut s = demo();
    s.execute("file.saveAs", json!({"path": original})).unwrap();
    std::fs::write(&target, b"preserve me").unwrap();
    let project_before = s.project.to_json();
    for command in ["file.saveAs", "file.saveCopy"] {
        let err = s.execute(command, json!({"path":target})).unwrap_err().to_string();
        assert!(err.contains("destination already exists"), "{command}: {err}");
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve me");
        assert_eq!(s.project.to_json(), project_before);
    }
    s.execute("file.saveCopy", json!({"path":target,"overwrite":true})).unwrap();
    assert_ne!(std::fs::read(&target).unwrap(), b"preserve me");
    assert_eq!(s.path.as_deref(), Some(original.as_str()));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn failed_save_as_keeps_project_name_path_and_revision() {
    let d = temp_dir("save-fail-state");
    let mut s = demo();
    let old_name = s.project.name.clone();
    let old_project = s.project.to_json();
    let old_path = s.path.clone();
    let old_revision = s.revision;
    let failed_path = d.join("failed.fcproj");
    std::fs::create_dir(&failed_path).unwrap();
    let error = s.execute("file.saveAs", json!({"path":failed_path})).unwrap_err().to_string();
    assert!(!error.is_empty());
    assert_eq!(s.project.name, old_name);
    assert_eq!(s.project.to_json(), old_project);
    assert_eq!(s.path, old_path);
    assert_eq!(s.revision, old_revision);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn native_create_new_allows_only_one_concurrent_writer() {
    let d = temp_dir("write-new-race");
    let path = d.join("race.fcproj").to_string_lossy().to_string();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let workers: Vec<_> = [b"writer one".as_slice(), b"writer two".as_slice()]
        .into_iter()
        .map(|bytes| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                (bytes.to_vec(), crate::FsServices.write_file_new(&path, bytes))
            })
        })
        .collect();
    barrier.wait();
    let results: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    let successes: Vec<_> = results.iter().filter(|(_, result)| result.is_ok()).collect();
    assert_eq!(successes.len(), 1, "{results:?}");
    assert_eq!(std::fs::read(&path).unwrap(), successes[0].0);
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
