//! File ▸ Import From ▸ DaVinci Resolve Project: finding projects in a Resolve project library and
//! importing one end to end, with a real movie for its media.

use std::path::Path;

use filmcraft_project::{ItemKind, MediaRef};
use filmcraft_sqlite::Value;
use filmcraft_sqlite::write::Writer;
use filmcraft_time::FrameRate;
use serde_json::json;

use crate::media_test_util::{make_movie, tmp_dir};
use crate::{FsServices, Session};

/// A TEXT value.
fn t(s: &str) -> Value {
    Value::Text(s.into())
}

/// A one-timeline Resolve database: `movie` cut twice on V1 with its sound linked on A1, at
/// 01:00:00:00 and 24 fps.
fn database(movie: &str) -> Vec<u8> {
    let link = "(\"DbOwner\" uuid NOT NULL, \"DbAssociate\" uuid, \"DbPropertyName\" character varying NOT NULL, \"DbIndex\" integer NOT NULL)";
    let rate = [24f64.to_le_bytes(), [0; 8]].concat();
    let extents = [3600f64.to_le_bytes(), 2f64.to_le_bytes()].concat();
    let size = [64u64.to_be_bytes(), 36u64.to_be_bytes()].concat();
    let mut w = Writer::new(4096);
    w.table(
        "SM_Project",
        "CREATE TABLE \"SM_Project\" (\"SM_Project_id\" uuid NOT NULL, \"ProjectName\" character varying)",
        vec![vec![t("p"), t("Weekend Cut")]],
    );
    w.table(
        "Sm2Timeline",
        "CREATE TABLE \"Sm2Timeline\" (\"Sm2Timeline_id\" uuid NOT NULL, \"Name\" character varying, \"Sequence\" uuid)",
        vec![vec![t("tl"), t("Assembly"), t("s")]],
    );
    w.table(
        "Sm2Sequence",
        "CREATE TABLE \"Sm2Sequence\" (\"Sm2Sequence_id\" uuid NOT NULL, \"FrameRate\" BLOB, \"Resolution\" BLOB, \"MediaExtents\" BLOB)",
        vec![vec![t("s"), Value::Blob(rate.clone()), Value::Blob(size), Value::Blob(extents)]],
    );
    w.table(
        "Sm2SequenceContainer",
        "CREATE TABLE \"Sm2SequenceContainer\" (\"Sm2SequenceContainer_id\" uuid NOT NULL, \"Sm2Sequence_id\" uuid)",
        vec![vec![t("c"), t("s")]],
    );
    let tl = |o: &str, p: &str, i: i64, a: &str| vec![t(o), t(a), t(p), Value::Integer(i)];
    w.table(
        "Sm2SequenceContainer_Sm2TiTrack",
        &format!("CREATE TABLE \"Sm2SequenceContainer_Sm2TiTrack\" {link}"),
        vec![tl("c", "VideoTrackVec", 0, "v1"), tl("c", "AudioTrackVec", 0, "a1")],
    );
    w.table(
        "Sm2TiTrack",
        "CREATE TABLE \"Sm2TiTrack\" (\"Sm2TiTrack_id\" uuid NOT NULL, \"Type\" integer)",
        vec![vec![t("v1"), Value::Integer(0)], vec![t("a1"), Value::Integer(1)]],
    );
    let item = |id: &str, ty: &str, start: i64, dur: i64, inn: i64, track: &str| {
        vec![t(id), t(ty), t("clip"), t(&start.to_string()), t(&dur.to_string()), t(&inn.to_string()), t("m"), t(movie), Value::Blob(rate.clone()), t(track)]
    };
    w.table(
        "Sm2TiItem",
        "CREATE TABLE \"Sm2TiItem\" (\"Sm2TiItem_id\" uuid NOT NULL, \"DbType\" character varying, \"Name\" character varying, \"Start\" character varying, \"Duration\" character varying, \"In\" character varying, \"MediaRef\" uuid, \"MediaFilePath\" character varying, \"MediaFrameRate\" BLOB, \"Sm2TiTrack_id\" uuid)",
        vec![
            item("i1", "Sm2TiVideoClip", 86400, 12, 0, "v1"),
            item("i2", "Sm2TiVideoClip", 86412, 12, 24, "v1"),
            item("i3", "Sm2TiAudioClip", 86400, 12, 0, "a1"),
            item("i4", "Sm2TiAudioClip", 86412, 12, 24, "a1"),
        ],
    );
    w.finish()
}

/// `<root>/Resolve Projects/Users/guest/Projects/Shorts/Weekend Cut/Project.db`, with its movie.
fn library(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = tmp_dir(name);
    let movie = root.join("a.mov");
    make_movie(&movie, filmcraft_media::DemoScene::Aurora, 64, 36, 48);
    let project = root.join("Resolve Projects").join("Users").join("guest").join("Projects").join("Shorts").join("Weekend Cut");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("Project.db"), database(&movie.to_string_lossy())).unwrap();
    (root, project)
}

/// A session on the real file system.
fn session() -> Session {
    Session::new(std::sync::Arc::new(FsServices))
}

/// `path` as commands take it.
fn p(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A library's projects are listed with their name, user, Project Manager folders and database; an
/// empty or missing library lists none.
#[test]
fn lists_the_projects_of_a_library() {
    let (root, project) = library("resolve-list");
    let mut s = session();
    let v = s.execute("file.listResolveProjects", json!({ "root": p(&root) })).unwrap();
    let projects = v["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1, "{v}");
    assert_eq!(projects[0]["name"], "Weekend Cut");
    assert_eq!(projects[0]["user"], "guest");
    assert_eq!(projects[0]["folder"], json!(["Shorts"]));
    assert_eq!(projects[0]["path"].as_str().map(Path::new), Some(project.join("Project.db").as_path()));
    // an empty or missing library lists nothing
    let v = s.execute("file.listResolveProjects", json!({ "root": p(&root.join("nothing")) })).unwrap();
    assert_eq!(v["projects"], json!([]));
}

/// Importing a project folder brings in its timeline and links its media.
#[test]
fn imports_a_project_folder_and_links_its_media() {
    let (root, project) = library("resolve-import");
    let mut s = session();
    let before = s.project.items.len();
    let r = s.execute("file.importResolve", json!({ "path": p(&project) })).unwrap();
    assert_eq!(r["format"], "DaVinci Resolve");
    assert_eq!(r["linkedMedia"], 1, "{r}");
    assert_eq!(r["offlineMedia"], json!([]));
    let seq_id = filmcraft_project::ItemId(r["sequences"][0].as_u64().unwrap());
    assert_eq!(s.state.active_sequence, Some(seq_id));
    let item = s.project.item(seq_id).unwrap();
    assert_eq!(item.name, "Assembly");
    let seq = s.project.sequence(seq_id).unwrap();
    let fps = FrameRate::FPS_24;
    assert_eq!(seq.start_timecode, 86400);
    let v1 = &seq.video_tracks[0].items;
    assert_eq!((v1[0].start, v1[1].start, v1[1].source_in), (fps.tick_of(0), fps.tick_of(12), fps.tick_of(24)));
    assert!(v1[0].link.is_some() && v1[0].link == seq.audio_tracks[0].items[0].link);
    let ItemKind::Media(m) = &s.project.item(v1[0].item).unwrap().kind else { panic!("not media") };
    assert_eq!(m.media, MediaRef::File { path: p(&root.join("a.mov")) });
    assert!(!m.offline);
    assert_eq!(m.info.video.as_ref().map(|v| (v.width, v.height)), Some((64, 36)));
    // undo Link Media, then the import itself (as for every interchange import)
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(seq_id).is_some());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.item(seq_id).is_none());
    assert!(s.project.items.len() <= before + 1);
}

/// `file.import` recognises a `Project.db` or a project folder and imports it as a Resolve project.
#[test]
fn file_import_recognises_a_project_database() {
    let (_root, project) = library("resolve-file-import");
    let mut s = session();
    let r = s.execute("file.import", json!({ "paths": [p(&project.join("Project.db"))] })).unwrap();
    assert_eq!(r["sequences"].as_array().map(Vec::len), Some(1), "{r}");
    // and a project folder dropped as is
    let r = s.execute("file.import", json!({ "paths": [p(&project)] })).unwrap();
    assert_eq!(r["sequences"].as_array().map(Vec::len), Some(1), "{r}");
}

/// A `Project.db-wal` holding changes Resolve has not saved yet is reported.
#[test]
fn warns_about_changes_still_in_the_write_ahead_log() {
    let (_root, project) = library("resolve-wal");
    std::fs::write(project.join("Project.db-wal"), [1u8; 64]).unwrap();
    let mut s = session();
    let r = s.execute("file.importResolve", json!({ "path": p(&project.join("Project.db")) })).unwrap();
    assert!(r["report"].as_array().unwrap().iter().any(|m| m.as_str().unwrap().contains("not saved into Project.db yet")), "{r}");
}

/// Files and folders that are not Resolve projects are errors that say so.
#[test]
fn reports_what_is_not_a_resolve_project() {
    let dir = tmp_dir("resolve-bad");
    let mut s = session();
    assert!(s.execute("file.importResolve", json!({ "path": p(&dir.join("missing")) })).is_err());
    let mut w = Writer::new(4096);
    w.table("notes", "CREATE TABLE notes (a)", vec![vec![t("x")]]);
    std::fs::write(dir.join("Project.db"), w.finish()).unwrap();
    let e = s.execute("file.importResolve", json!({ "path": p(&dir) })).unwrap_err().to_string();
    assert!(e.contains("not a DaVinci Resolve project"), "{e}");
    std::fs::write(dir.join("Project.db"), b"garbage").unwrap();
    assert!(s.execute("file.importResolve", json!({ "path": p(&dir) })).is_err());
    assert!(s.execute("file.importResolve", json!({})).is_err());
}

/// Poll `file.resolveImportStatus` until the background import finishes (at most 30 s).
fn wait_for(s: &mut Session) -> Result<serde_json::Value, String> {
    for _ in 0..3000 {
        match s.execute("file.resolveImportStatus", json!({})) {
            Ok(v) if v["finished"] == json!(true) => return Ok(v),
            Ok(v) => assert_eq!(v["running"], json!(true), "{v}"),
            Err(e) => return Err(e.to_string()),
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Err("the import did not finish".into())
}

/// A background import reports its progress and merges once it is ready.
#[test]
fn imports_in_the_background() {
    let (_root, project) = library("resolve-background");
    let mut s = session();
    let before = s.project.items.len();
    let r = s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
    assert_eq!(r["started"], json!(true), "{r}");
    // one at a time
    assert!(s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).is_err());
    let done = wait_for(&mut s).unwrap();
    assert_eq!(done["format"], "DaVinci Resolve");
    assert_eq!(done["linkedMedia"], 1, "{done}");
    assert_eq!(done["sequences"].as_array().map(Vec::len), Some(1));
    assert!(s.project.items.len() > before);
    assert!(s.resolve_import.is_none());
    assert_eq!(s.execute("file.resolveImportStatus", json!({})).unwrap()["running"], json!(false));
}

/// A cancelled background import leaves the project as it was.
#[test]
fn a_cancelled_background_import_changes_nothing() {
    let (_root, project) = library("resolve-cancel");
    let mut s = session();
    let before = s.project.items.len();
    let revision = s.revision;
    // cancelled at once (while the worker reads the database or opens the media) and, the case
    // the worker never sees, after it has finished but before the result is collected
    for late in [false, true] {
        s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
        if late {
            let progress = s.resolve_import.as_ref().unwrap().progress.clone();
            while progress.done.load(std::sync::atomic::Ordering::Relaxed) < 1 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            // (the result lands just after the last file is counted)
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        assert_eq!(s.execute("file.cancelResolveImport", json!({})).unwrap()["cancelled"], json!(true));
        let e = wait_for(&mut s).unwrap_err();
        assert!(e.contains("cancelled"), "late: {late}: {e}");
        assert_eq!((s.project.items.len(), s.revision), (before, revision), "late: {late}");
        assert!(s.resolve_import.is_none());
    }
    assert_eq!(s.execute("file.cancelResolveImport", json!({})).unwrap()["cancelled"], json!(false));
}

/// A background import that cannot read its database reports why and ends.
#[test]
fn a_failed_background_import_reports_its_error() {
    let dir = tmp_dir("resolve-background-bad");
    std::fs::write(dir.join("Project.db"), b"garbage").unwrap();
    let mut s = session();
    s.execute("file.importResolve", json!({ "path": p(&dir), "background": true })).unwrap();
    let e = wait_for(&mut s).unwrap_err();
    assert!(e.contains("SQLite"), "{e}");
    assert!(s.resolve_import.is_none());
}

/// A dropped import records none of its media paths in the pool, and leaves another document's
/// alone; a merged one records them with its database.
#[test]
fn a_dropped_import_leaves_no_stored_paths_behind() {
    let (root, project) = library("resolve-stored");
    let movie = p(&root.join("a.mov"));
    let db = p(&project.join("Project.db"));
    let cancelled = |s: &mut Session| {
        s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
        s.execute("file.cancelResolveImport", json!({})).unwrap();
        assert!(wait_for(s).unwrap_err().contains("cancelled"));
    };
    // nobody had recorded the movie: the cancelled import leaves no trace of it
    let mut s = session();
    cancelled(&mut s);
    assert_eq!(s.media.stored_document(&movie), None);
    // another document had: its entry stays as it was
    let mut s = session();
    s.media.add_stored_paths([movie.clone()], Some("other.fcproj"));
    cancelled(&mut s);
    assert_eq!(s.media.stored_document(&movie), Some(Some("other.fcproj".into())));
    // an import that goes through keeps its own
    let mut s = session();
    s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
    wait_for(&mut s).unwrap();
    assert_eq!(s.media.stored_document(&movie), Some(Some(db.clone().into())));
    // one that goes through while another of the same files is pending keeps them when the
    // pending one is cancelled (and its media stays linked)
    let mut s = session();
    s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
    let done = s.execute("file.importResolve", json!({ "path": p(&project) })).unwrap();
    assert_eq!(done["linkedMedia"], 1, "{done}");
    s.execute("file.cancelResolveImport", json!({})).unwrap();
    assert!(wait_for(&mut s).unwrap_err().contains("cancelled"));
    assert_eq!(s.media.stored_document(&movie), Some(Some(db.into())));
    assert_eq!(s.media.follow(&movie).as_deref(), Ok(movie.as_str()));
}

/// The real filesystem, counting every read of files whose name ends in `watched`.
struct Counting {
    watched: &'static str,
    reads: std::sync::atomic::AtomicUsize,
}

impl Counting {
    /// Count a read of `path` when it is a watched file.
    fn hit(&self, path: &str) {
        if path.ends_with(self.watched) {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl crate::Services for Counting {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.hit(path);
        FsServices.read_file(path)
    }
    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        FsServices.write_file(path, data)
    }
    fn file_size(&self, path: &str) -> std::io::Result<u64> {
        FsServices.file_size(path)
    }
    fn read_range(&self, path: &str, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        self.hit(path);
        FsServices.read_range(path, offset, len)
    }
    fn reader(&self, path: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
        self.hit(path);
        FsServices.reader(path)
    }
}

/// A media file that failed to open in the background is not read again when the import is merged.
#[test]
fn the_merge_does_not_read_a_failed_file_again() {
    // the project's only media file is there but is no movie
    let root = tmp_dir("resolve-unreadable");
    let bad = root.join("bad.mov");
    std::fs::write(&bad, vec![0x5a; 4096]).unwrap();
    let project = root.join("Weekend Cut");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("Project.db"), database(&p(&bad))).unwrap();
    let services = std::sync::Arc::new(Counting { watched: "bad.mov", reads: Default::default() });
    let mut s = Session::new(services.clone());
    s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
    // let the worker finish before the result is collected
    let progress = s.resolve_import.as_ref().unwrap().progress.clone();
    while progress.done.load(std::sync::atomic::Ordering::Relaxed) < 1 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    let on_the_worker = services.reads.load(std::sync::atomic::Ordering::Relaxed);
    assert!(on_the_worker > 0, "the worker tried the file");
    let done = wait_for(&mut s).unwrap();
    assert_eq!(done["offlineMedia"].as_array().map(Vec::len), Some(1), "{done}");
    assert_eq!(services.reads.load(std::sync::atomic::Ordering::Relaxed), on_the_worker, "the merge read it again");
}

/// An import is dropped, not merged, when another project is created before it finishes; one
/// started in the new project goes through.
#[test]
fn an_import_never_lands_in_a_project_opened_meanwhile() {
    let (_root, project) = library("resolve-replaced");
    let mut s = session();
    s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
    // the user starts a new project before the import has been merged
    s.execute("file.newProject", json!({ "name": "Something Else" })).unwrap();
    let revision = s.revision;
    let items = s.project.items.len();
    let e = wait_for(&mut s).unwrap_err();
    assert!(e.contains("was closed"), "{e}");
    assert_eq!((s.project.name.as_str(), s.project.items.len(), s.revision), ("Something Else", items, revision));
    assert!(s.resolve_import.is_none());
    // an import started in the new project goes through
    s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
    assert_eq!(wait_for(&mut s).unwrap()["linkedMedia"], 1);
}

/// The real filesystem, holding every read of a `Project.db` until `open` is set (at most five
/// seconds): a large project, or one on a slow drive.
struct Gated {
    open: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl crate::Services for Gated {
    /// Hold a `Project.db` read until the gate opens.
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        if path.ends_with("Project.db") {
            for _ in 0..1000 {
                if self.open.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        FsServices.read_file(path)
    }
    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        FsServices.write_file(path, data)
    }
    fn file_size(&self, path: &str) -> std::io::Result<u64> {
        FsServices.file_size(path)
    }
    fn reader(&self, path: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
        FsServices.reader(path)
    }
}

/// An import whose project was replaced ends at once, even while its worker is still reading:
/// the app does not wait on it, and a new import can start straight away.
#[test]
fn a_replaced_import_ends_without_waiting_for_its_worker() {
    let (_root, project) = library("resolve-replaced-busy");
    let open = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut s = Session::new(std::sync::Arc::new(Gated { open: open.clone() }));
    s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
    s.execute("file.newProject", json!({ "name": "Something Else" })).unwrap();
    // the worker is still inside its read of the database
    let e = s.execute("file.resolveImportStatus", json!({})).unwrap_err().to_string();
    assert!(e.contains("was closed"), "{e}");
    assert!(s.resolve_import.is_none());
    s.execute("file.importResolve", json!({ "path": p(&project), "background": true })).unwrap();
    open.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(wait_for(&mut s).unwrap()["linkedMedia"], 1);
    assert_eq!(s.project.name, "Something Else");
}

/// A relative media path is the database's, in the background and at once: it opens the file next
/// to `Project.db` even when another document stored the same text first, and the project keeps
/// that file, not text a saved project would take from its own folder.
#[test]
fn relative_media_paths_are_taken_from_the_database_folder() {
    let root = tmp_dir("resolve-relative");
    let project = root.join("Weekend Cut");
    std::fs::create_dir_all(project.join("media")).unwrap();
    make_movie(&project.join("media").join("a.mov"), filmcraft_media::DemoScene::Aurora, 64, 36, 48);
    std::fs::write(project.join("Project.db"), database("media/a.mov")).unwrap();
    let movie = p(&project.join("media").join("a.mov"));
    for background in [true, false] {
        let mut s = session();
        // another document, in a folder without the file, stored the same relative text first
        s.media.add_stored_paths(["media/a.mov".to_string()], Some(&p(&root.join("elsewhere").join("other.fcproj"))));
        let r = s.execute("file.importResolve", json!({ "path": p(&project), "background": background })).unwrap();
        let r = if background { wait_for(&mut s).unwrap() } else { r };
        assert_eq!(r["linkedMedia"], 1, "{r}");
        let paths: Vec<String> = s
            .project
            .items
            .values()
            .filter_map(|i| match &i.kind {
                ItemKind::Media(m) => match &m.media {
                    MediaRef::File { path } => Some(path.clone()),
                    MediaRef::Generator(_) => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(paths, [movie.as_str()]);
    }
}
