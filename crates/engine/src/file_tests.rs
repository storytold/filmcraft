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

/// Saving stores the Program Monitor frame as the project's thumbnail, outside the edit: the save
/// leaves the project clean, adds no undo step, and the file opens with it (Home ▸ Recent Projects
/// reads it with `filmcraft_format::read_thumbnail`).
#[test]
fn save_stores_a_thumbnail_of_the_program_monitor_without_dirtying() {
    let d = temp_dir("thumb");
    let path = d.join("p.fcproj").to_string_lossy().to_string();
    let mut s = demo();
    let seq = s.state.active_sequence.unwrap();
    s.state.playheads.insert(seq, crate::commands::secs(2.0));
    let undo = s.history.undo.len();
    let r = s.execute("file.saveAs", json!({"path": path})).unwrap();
    let (rev, saved) = (s.revision, s.saved_revision);
    assert!(!s.is_dirty());
    assert_eq!(s.history.undo.len(), undo, "the thumbnail is never an undo step");
    let settings = &s.project.sequence(seq).unwrap().settings;
    let (sw, sh) = (settings.width, settings.height);
    let t = s.thumbnail.clone().expect("a thumbnail of the active sequence");
    assert_eq!(t.width.max(t.height), 320.min(sw.max(sh)));
    assert_eq!(r["thumbnail"]["width"], t.width);
    let img = filmcraft_format::read_thumbnail(std::path::Path::new(&path)).unwrap().expect("the file has a thumbnail");
    assert_eq!((img.width, img.height), (t.width, t.height));
    assert!(img.rgba.chunks(4).any(|p| p[0] > 16 || p[1] > 16 || p[2] > 16), "the demo frame is not all black");
    // saving again re-renders it and still leaves the project clean
    s.execute("file.save", json!({})).unwrap();
    assert!(!s.is_dirty());
    assert_eq!((s.revision, s.history.undo.len()), (rev, undo));
    assert!(s.saved_revision >= saved);
    // the file opens with it
    let mut o = Session::default();
    o.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(o.thumbnail.as_ref(), s.thumbnail.as_ref());
    assert!(!o.is_dirty());
}

/// No open sequence: the save works and keeps the thumbnail the file was opened with (none for a
/// new project).
#[test]
fn save_without_an_active_sequence_keeps_the_previous_thumbnail() {
    let d = temp_dir("thumb-noseq");
    let path = d.join("p.fcproj").to_string_lossy().to_string();
    let mut s = Session::default();
    assert!(s.state.active_sequence.is_none());
    let r = s.execute("file.saveAs", json!({"path": path})).unwrap();
    assert!(r["thumbnail"].is_null());
    assert_eq!(filmcraft_format::read_thumbnail(std::path::Path::new(&path)).unwrap(), None);
    let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(v.get("thumbnail").is_none());

    let mut s = demo();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let mut o = Session::default();
    o.execute("file.open", json!({"path": path})).unwrap();
    let kept = o.thumbnail.clone().unwrap();
    o.state.active_sequence = None;
    o.state.open_sequences.clear();
    o.execute("file.save", json!({})).unwrap();
    let img = filmcraft_format::read_thumbnail(std::path::Path::new(&path)).unwrap().unwrap();
    assert_eq!((img.width, img.height), (kept.width, kept.height));
}

/// A thumbnail belongs to its project: after File ▸ Close Project or New Project, a Save As
/// (with no sequence open to render a new one) writes none, and the file opens without one.
#[test]
fn closing_or_replacing_the_project_drops_its_thumbnail() {
    let d = temp_dir("thumb-close");
    let first = d.join("first.fcproj").to_string_lossy().to_string();
    for (i, (cmd, params)) in [("file.closeProject", json!({"force": true})), ("file.newProject", json!({"name": "Next"}))].into_iter().enumerate() {
        let mut s = demo();
        s.execute("file.saveAs", json!({"path": first})).unwrap();
        assert!(s.thumbnail.is_some());
        s.execute(cmd, params).unwrap();
        assert!(s.state.active_sequence.is_none());
        assert!(s.thumbnail.is_none(), "{cmd} keeps the closed project's thumbnail");
        let next = d.join(format!("next {i}.fcproj")).to_string_lossy().to_string();
        s.execute("file.saveAs", json!({"path": next})).unwrap();
        assert_eq!(filmcraft_format::read_thumbnail(std::path::Path::new(&next)).unwrap(), None, "{cmd}: no thumbnail of the closed project");
        let mut o = Session::default();
        o.execute("file.open", json!({"path": next})).unwrap();
        assert!(o.thumbnail.is_none(), "{cmd}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// A thumbnail that cannot be read never stops the project from opening, and the next save
/// replaces it.
#[test]
fn a_damaged_thumbnail_is_ignored_on_open() {
    let d = temp_dir("thumb-bad");
    let path = d.join("p.fcproj").to_string_lossy().to_string();
    let mut s = demo();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let mut v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for junk in [
        json!("garbage"),
        json!({"format": "jpeg", "width": 1u64 << 40, "height": 1, "data": "AAAA"}),
        json!({"format": "jpeg", "width": 320, "height": 180, "data": "/9j/AAAA"}),
    ] {
        v["thumbnail"] = junk;
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        let mut o = Session::default();
        o.execute("file.open", json!({"path": path})).unwrap();
        assert_eq!(*o.project, *s.project);
        assert_eq!(filmcraft_format::read_thumbnail(std::path::Path::new(&path)).unwrap(), None);
        o.execute("file.save", json!({})).unwrap();
        assert!(filmcraft_format::read_thumbnail(std::path::Path::new(&path)).unwrap().is_some(), "the save writes a fresh one");
    }
}
