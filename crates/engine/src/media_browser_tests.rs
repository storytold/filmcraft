//! Tests of the Media Browser (`media_browser`) against a fake filesystem: listing, file-type
//! filter, navigation history, Favorites, recent directories, import and Open in Source Monitor.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::media_browser::{self, DirEntry, Volume, VolumeKind};
use crate::{Services, Session};

/// An in-memory filesystem: directories and files (with bytes).
#[derive(Default)]
struct FakeFs {
    dirs: Vec<String>,
    files: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl FakeFs {
    fn new() -> Self {
        let wav = filmcraft_media::wav::write_wav16(&[0.0; 4800], 2, 48_000);
        let mut files = BTreeMap::new();
        for f in ["/home/me/Movies/b-roll.wav", "/home/me/Movies/A-interview.wav", "/home/me/Movies/notes.txt", "/home/me/Movies/.hidden.wav"] {
            files.insert(f.to_string(), wav.clone());
        }
        for f in [
            "/home/me/Movies/shot_0001.png",
            "/home/me/Movies/shot_0002.png",
            "/home/me/Movies/edit.edl",
            "/home/me/Movies/subs.srt",
            "/home/me/Movies/clip.mov",
        ] {
            files.insert(f.to_string(), vec![0; 16]);
        }
        files.insert("/Volumes/Card/DCIM/take1.wav".into(), wav);
        FakeFs {
            dirs: ["/", "/home", "/home/me", "/home/me/Movies", "/home/me/Movies/Selects", "/Volumes", "/Volumes/Card", "/Volumes/Card/DCIM"]
                .iter()
                .map(|d| d.to_string())
                .collect(),
            files: Mutex::new(files),
        }
    }
}

impl Services for FakeFs {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.files.lock().unwrap().get(path).cloned().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string()))
    }
    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        self.files.lock().unwrap().insert(path.into(), data.to_vec());
        Ok(())
    }
    fn file_size(&self, path: &str) -> std::io::Result<u64> {
        self.read_file(path).map(|b| b.len() as u64)
    }
    fn list_entries(&self, dir: &str) -> Option<std::io::Result<Vec<DirEntry>>> {
        if !self.dirs.iter().any(|d| d == dir) {
            return Some(Err(std::io::Error::new(std::io::ErrorKind::NotFound, dir.to_string())));
        }
        let prefix = if dir == "/" { "/".to_string() } else { format!("{dir}/") };
        let direct = |p: &str| p.strip_prefix(&prefix).filter(|r| !r.is_empty() && !r.contains('/')).map(str::to_string);
        let mut v: Vec<DirEntry> =
            self.dirs.iter().filter_map(|d| direct(d)).map(|name| DirEntry { name, is_dir: true, size: None, modified: Some(0) }).collect();
        for (p, b) in self.files.lock().unwrap().iter() {
            if let Some(name) = direct(p) {
                v.push(DirEntry { name, is_dir: false, size: Some(b.len() as u64), modified: Some(1_700_000_000) });
            }
        }
        Some(Ok(v))
    }
    fn list_dir(&self, dir: &str) -> Option<std::io::Result<Vec<String>>> {
        self.list_entries(dir).map(|r| r.map(|v| v.into_iter().map(|e| e.name).collect()))
    }
    fn volumes(&self) -> Vec<Volume> {
        vec![
            Volume { name: "Macintosh HD".into(), path: "/".into(), kind: VolumeKind::Local },
            Volume { name: "Card".into(), path: "/Volumes/Card".into(), kind: VolumeKind::Local },
            Volume { name: "Studio".into(), path: "/Network/Studio".into(), kind: VolumeKind::Network },
        ]
    }
    fn home_dir(&self) -> Option<String> {
        Some("/home/me".into())
    }
}

fn session() -> Session {
    Session::new(Arc::new(FakeFs::new()))
}

fn names(v: &Value) -> Vec<String> {
    v["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect()
}

#[test]
fn listing_puts_folders_first_and_hides_unsupported_files() {
    let mut s = session();
    // starts in the home directory
    let l = s.execute("mediaBrowser.list", json!({})).unwrap();
    assert_eq!(l["dir"], "/home/me");
    assert_eq!(names(&l), ["Movies"]);
    let l = s.execute("mediaBrowser.list", json!({"path": "/home/me/Movies"})).unwrap();
    assert_eq!(names(&l), ["Selects", "A-interview.wav", "b-roll.wav", "clip.mov", "edit.edl", "shot_0001.png", "shot_0002.png", "subs.srt"]);
    let e = &l["entries"][0];
    assert_eq!((e["isDir"].as_bool(), e["kind"].as_str()), (Some(true), Some("folder")));
    let shot = l["entries"].as_array().unwrap().iter().find(|e| e["name"] == "shot_0001.png").unwrap();
    assert_eq!(shot["numbered"], true);
    assert_eq!(shot["path"], "/home/me/Movies/shot_0001.png");
    assert!(s.execute("mediaBrowser.list", json!({"path": "/nope"})).is_err());
}

#[test]
fn file_type_filter() {
    let mut s = session();
    let l = s.execute("mediaBrowser.list", json!({"path": "/home/me/Movies", "fileTypes": "audio"})).unwrap();
    assert_eq!(names(&l), ["Selects", "A-interview.wav", "b-roll.wav"], "folders always show");
    let l = s.execute("mediaBrowser.list", json!({"path": "/home/me/Movies", "fileTypes": "image"})).unwrap();
    assert_eq!(names(&l), ["Selects", "shot_0001.png", "shot_0002.png"]);
    let l = s.execute("mediaBrowser.list", json!({"path": "/home/me/Movies", "fileTypes": "mov"})).unwrap();
    assert_eq!(names(&l), ["Selects", "clip.mov"]);
    // the setting persists and applies by default
    s.execute("mediaBrowser.settings", json!({"fileTypes": "project"})).unwrap();
    let l = s.execute("mediaBrowser.list", json!({"path": "/home/me/Movies"})).unwrap();
    assert_eq!(names(&l), ["Selects", "edit.edl"]);
    assert!(s.execute("mediaBrowser.settings", json!({"fileTypes": "docx"})).is_err());
    assert!(s.execute("mediaBrowser.settings", json!({"view": "cards"})).is_err());
    s.execute("mediaBrowser.settings", json!({"view": "thumbnails", "columns": ["size", "Frame Rate"]})).unwrap();
    assert_eq!(s.prefs.media_browser.view, "thumbnails");
    assert_eq!(s.prefs.media_browser.columns, ["Name", "Size", "Frame Rate"]);
}

#[test]
fn navigation_back_forward_up_and_recent() {
    let mut s = session();
    s.execute("mediaBrowser.navigate", json!({"path": "/home/me/Movies"})).unwrap();
    s.execute("mediaBrowser.navigate", json!({"path": "/home/me/Movies/Selects"})).unwrap();
    let r = s.execute("mediaBrowser.navigate", json!({"up": true})).unwrap();
    assert_eq!(r["dir"], "/home/me/Movies");
    let r = s.execute("mediaBrowser.navigate", json!({"back": true})).unwrap();
    assert_eq!(r["dir"], "/home/me/Movies/Selects");
    assert_eq!(r["canForward"], true);
    let r = s.execute("mediaBrowser.navigate", json!({"back": true})).unwrap();
    assert_eq!(r["dir"], "/home/me/Movies");
    let r = s.execute("mediaBrowser.navigate", json!({"forward": true})).unwrap();
    assert_eq!(r["dir"], "/home/me/Movies/Selects");
    // a new location clears forward history
    s.execute("mediaBrowser.navigate", json!({"path": "/Volumes/Card"})).unwrap();
    assert!(s.execute("mediaBrowser.navigate", json!({"forward": true})).is_err());
    // a directory that can't be listed leaves the browser where it was
    assert!(s.execute("mediaBrowser.navigate", json!({"path": "/missing"})).is_err());
    assert_eq!(media_browser::current_dir(&s), "/Volumes/Card");
    // recent: newest first, no duplicates; the last directory is remembered
    assert_eq!(s.prefs.media_browser.recent[..3], ["/Volumes/Card", "/home/me/Movies/Selects", "/home/me/Movies"]);
    assert_eq!(s.prefs.media_browser.last_dir, "/Volumes/Card");
    let roots = s.execute("mediaBrowser.roots", json!({})).unwrap();
    assert_eq!(roots["localDrives"].as_array().unwrap().len(), 2);
    assert_eq!(roots["network"][0]["name"], "Studio");
    assert_eq!(roots["recent"][0]["name"], "Card");
    s.execute("mediaBrowser.clearRecent", json!({})).unwrap();
    assert!(s.prefs.media_browser.recent.is_empty());
    // up at the root fails
    s.execute("mediaBrowser.navigate", json!({"path": "/"})).unwrap();
    assert!(s.execute("mediaBrowser.navigate", json!({"up": true})).is_err());
}

#[test]
fn favorites_add_and_remove() {
    let mut s = session();
    s.execute("mediaBrowser.navigate", json!({"path": "/home/me/Movies"})).unwrap();
    s.execute("mediaBrowser.favorite", json!({})).unwrap();
    s.execute("mediaBrowser.favorite", json!({"path": "/Volumes/Card/DCIM"})).unwrap();
    s.execute("mediaBrowser.favorite", json!({})).unwrap();
    assert_eq!(s.prefs.media_browser.favorites, ["/home/me/Movies", "/Volumes/Card/DCIM"], "no duplicates");
    let r = s.execute("mediaBrowser.list", json!({})).unwrap();
    assert_eq!(r["favorite"], true);
    let roots = s.execute("mediaBrowser.roots", json!({})).unwrap();
    assert_eq!(roots["favorites"][1]["name"], "DCIM");
    s.execute("mediaBrowser.favorite", json!({"remove": true})).unwrap();
    assert_eq!(s.prefs.media_browser.favorites, ["/Volumes/Card/DCIM"]);
    assert!(s.execute("mediaBrowser.favorite", json!({"remove": true})).is_err());
}

#[test]
fn import_the_selection_and_open_in_source() {
    let mut s = session();
    s.execute("mediaBrowser.navigate", json!({"path": "/home/me/Movies"})).unwrap();
    assert!(s.execute("mediaBrowser.import", json!({})).is_err(), "nothing selected");
    s.execute("mediaBrowser.select", json!({"paths": ["/home/me/Movies/b-roll.wav", "/home/me/Movies/A-interview.wav"]})).unwrap();
    let r = s.execute("mediaBrowser.import", json!({})).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(s.project.items.len(), 2);
    // undoable like any import
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.project.items.is_empty());
    // File ▸ Import from Media Browser uses the selection too
    let r = s.execute("file.importFromMediaBrowser", json!({})).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 2);
    // Open In Source Monitor reuses the imported item
    let n = s.project.items.len();
    let r = s.execute("mediaBrowser.openInSource", json!({"path": "/home/me/Movies/b-roll.wav"})).unwrap();
    assert_eq!(s.project.items.len(), n);
    assert_eq!(s.state.source_item.map(|i| i.0), r["item"].as_u64());
    // ... and imports a file not in the project yet
    s.execute("mediaBrowser.openInSource", json!({"path": "/Volumes/Card/DCIM/take1.wav"})).unwrap();
    assert_eq!(s.project.items.len(), n + 1);
    // importing a folder imports its media files
    let mut s = session();
    let r = s.execute("mediaBrowser.import", json!({"paths": ["/Volumes/Card/DCIM"]})).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 1);
}

/// Importing a folder keeps its structure (#411): the folder becomes a bin named after it, each
/// sub-folder with media a bin inside that, and every file lands in its folder's bin.
#[test]
fn importing_a_folder_mirrors_its_sub_folders_as_bins() {
    let mut fs = FakeFs::new();
    let wav = filmcraft_media::wav::write_wav16(&[0.0; 4800], 2, 48_000);
    fs.dirs.extend(["/Shoot", "/Shoot/Day 1", "/Shoot/Day 1/Cam A", "/Shoot/Empty"].map(String::from));
    for f in ["/Shoot/slate.wav", "/Shoot/Day 1/room.wav", "/Shoot/Day 1/Cam A/take1.wav", "/Shoot/Day 1/Cam A/take2.wav"] {
        fs.files.get_mut().unwrap().insert(f.into(), wav.clone());
    }
    let mut s = Session::new(Arc::new(fs));
    let r = s.execute("mediaBrowser.import", json!({"paths": ["/Shoot"]})).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 4, "{r}");
    assert_eq!(r["bins"].as_array().unwrap().len(), 3, "{r}");
    let tree = |b: &filmcraft_project::Bin| -> Vec<String> {
        b.children
            .iter()
            .map(|c| match c {
                filmcraft_project::BinEntry::Bin(b) => format!("bin {}", b.name),
                filmcraft_project::BinEntry::Item(i) => s.project.item(*i).unwrap().name.clone(),
            })
            .collect()
    };
    let sub = |b: &filmcraft_project::Bin, name: &str| -> filmcraft_project::Bin {
        b.children
            .iter()
            .find_map(|c| match c {
                filmcraft_project::BinEntry::Bin(b) if b.name == name => Some(b.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(tree(&s.project.root), ["bin Shoot"]);
    let shoot = sub(&s.project.root, "Shoot");
    // a folder without media gets no bin
    assert_eq!(tree(&shoot), ["slate.wav", "bin Day 1"]);
    let day = sub(&shoot, "Day 1");
    assert_eq!(tree(&day), ["room.wav", "bin Cam A"]);
    assert_eq!(tree(&sub(&day, "Cam A")), ["take1.wav", "take2.wav"]);
}

#[test]
fn import_with_ingest_settings() {
    let mut s = session();
    s.execute("project.ingestSettings", json!({"enabled": true, "action": "copy", "destination": "/home/me/Ingest"})).unwrap();
    let r = s.execute("mediaBrowser.import", json!({"paths": ["/home/me/Movies/b-roll.wav"]})).unwrap();
    // the copy itself needs a real disk: the fake one makes it fail, which the result reports
    let ran = !r["ingest"].is_null() || r["errors"].as_array().is_some_and(|e| e.iter().any(|x| x.as_str().unwrap_or("").starts_with("ingest")));
    assert!(ran, "ingest ran: {r}");
    assert_eq!(r["items"].as_array().unwrap().len(), 1);
}

#[test]
fn probe_and_columns() {
    let mut s = session();
    let r = s.execute("mediaBrowser.probe", json!({"path": "/home/me/Movies/b-roll.wav"})).unwrap();
    assert_eq!(r["media"], true);
    let l = media_browser::list(&*s.services, "/home/me/Movies", "all").unwrap();
    let e = l.iter().find(|e| e.name == "b-roll.wav").unwrap();
    let info = media_browser::probe(&mut s, &e.path);
    assert_eq!(media_browser::column_text(e, "Audio Info", info.as_ref()), "48000 Hz - 2 ch");
    assert!(media_browser::column_text(e, "Media Duration", info.as_ref()).starts_with("00:00:00"));
    assert_eq!(media_browser::column_text(e, "Media Type", None), "Audio");
    assert_eq!(media_browser::column_text(e, "Date Modified", None), "2023-11-14 22:13");
    // not media: cached as such
    assert!(media_browser::probe(&mut s, "/home/me/Movies/edit.edl").is_none());
}

#[test]
fn path_helpers() {
    assert_eq!(media_browser::join("/a", "b"), "/a/b");
    assert_eq!(media_browser::join("/", "b"), "/b");
    assert_eq!(media_browser::join("C:\\Media", "b.mov"), "C:\\Media\\b.mov");
    assert_eq!(media_browser::parent("/a/b"), Some("/a".into()));
    assert_eq!(media_browser::parent("/a"), Some("/".into()));
    assert_eq!(media_browser::parent("/"), None);
    assert_eq!(media_browser::parent("C:\\Media"), Some("C:\\".into()));
    assert_eq!(media_browser::base_name("/home/me/Movies/"), "Movies");
}

/// #157: browsing a folder read every file no streaming reader takes (an AVI, a WAV) whole into
/// memory, just to show its properties or to find out it isn't supported. Large ones are now
/// refused after the head; small ones still show their properties; MP4 is read through its index.
#[cfg(any(unix, windows))]
#[test]
fn probing_large_unstreamable_files_does_not_read_them_whole() {
    let dir = crate::media_test_util::tmp_dir("browser-probe-large");
    // a 100 MB "AVI" (sparse: nothing is written past the header)
    let avi = dir.join("camera.avi");
    let mut head = vec![0u8; 64];
    head[..4].copy_from_slice(b"RIFF");
    head[8..12].copy_from_slice(b"AVI ");
    std::fs::write(&avi, &head).unwrap();
    std::fs::OpenOptions::new().write(true).open(&avi).unwrap().set_len(crate::media_pool::PROBE_WHOLE_FILE_MAX + 36 * 1024 * 1024).unwrap();
    let wav = dir.join("voice.wav");
    std::fs::write(&wav, filmcraft_media::wav::write_wav16(&[0.1; 9_600], 2, 48_000)).unwrap();
    let mov = dir.join("clip.mov");
    crate::media_test_util::make_movie(&mov, filmcraft_media::DemoScene::OceanSunset, 64, 36, 24);
    let mut s = Session::default();
    assert!(media_browser::probe(&mut s, &avi.to_string_lossy()).is_none());
    assert_eq!(media_browser::probe(&mut s, &wav.to_string_lossy()).unwrap().audio().unwrap().channels, 2);
    assert!(media_browser::probe(&mut s, &mov.to_string_lossy()).unwrap().video.is_some());
    // importing is unchanged: it still opens files through the full path
    let r = s.execute("file.import", json!({"paths": [wav.to_string_lossy()]})).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
