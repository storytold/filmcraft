//! Media paths stored in a project or an interchange file are opened only when they name a file
//! on this computer or on the document's own share (`doc_path`): the open-time scan, rendering
//! and the growing-files look never reach another computer, while a path beside the document and
//! a file the user links in this session still open.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_media::DemoScene;
use filmcraft_project::{ItemId, MediaRef};
use filmcraft_render::offline::OfflineReason;
use serde_json::json;

use crate::media_test_util::{make_movie, session_with, tmp_dir};
use crate::{FsServices, Services, Session};

const W: u32 = 160;
const H: u32 = 90;

/// The file system, remembering every path it is asked about.
#[derive(Default)]
struct Recording(Arc<Mutex<Vec<String>>>);

impl Recording {
    fn note(&self, path: &str) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(path.to_string());
    }
}

impl Services for Recording {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.note(path);
        FsServices.read_file(path)
    }
    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        self.note(path);
        FsServices.write_file(path, data)
    }
    fn file_size(&self, path: &str) -> std::io::Result<u64> {
        self.note(path);
        FsServices.file_size(path)
    }
    fn read_range(&self, path: &str, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        self.note(path);
        FsServices.read_range(path, offset, len)
    }
    fn reader(&self, path: &str) -> Option<std::io::Result<filmcraft_media::SharedReader>> {
        self.note(path);
        FsServices.reader(path)
    }
    fn list_dir(&self, dir: &str) -> Option<std::io::Result<Vec<String>>> {
        self.note(dir);
        FsServices.list_dir(dir)
    }
}

/// A session reading files through [`Recording`], and the paths it was asked about.
fn recording_session() -> (Session, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::<Mutex<Vec<String>>>::default();
    let s = Session { services: Arc::new(Recording(seen.clone())), ..Session::default() };
    (s, seen)
}

/// Paths that name another computer, or `//<local path>` (on Unix the system reads it as the
/// local file; the resolver reads it as a share and refuses it).
fn foreign(paths: &[String]) -> Vec<&String> {
    paths.iter().filter(|p| p.contains("192.0.2.1") || p.starts_with("//") || p.starts_with(r"\\") || p.starts_with("file:")).collect()
}

/// `//<path>` for an absolute Unix path (None elsewhere).
fn slashed(path: &Path) -> Option<String> {
    cfg!(unix).then(|| format!("/{}", path.display()))
}

#[test]
fn stored_media_paths_open_only_on_this_computer_or_beside_the_project() {
    let root = tmp_dir("stored-paths");
    let media = root.join("Media");
    let project = root.join("Project");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let a = media.join("a.mov");
    let beside = project.join("b.mov");
    make_movie(&a, DemoScene::Aurora, W, H, 12);
    make_movie(&beside, DemoScene::CityNight, W, H, 12);

    let mut stored = vec![r"\\192.0.2.1\share\a.mov".to_string(), "file://192.0.2.1/s/a.mov".to_string()];
    stored.extend(slashed(&a));
    let refused = stored.len();
    stored.push("b.mov".to_string());
    let files: Vec<&Path> = (0..refused).map(|_| a.as_path()).chain([beside.as_path()]).collect();
    let (mut s, items, _) = session_with(&files);
    let p = Arc::make_mut(&mut s.project);
    for (id, path) in items.iter().zip(&stored) {
        p.item_mut(*id).unwrap().as_media_mut().unwrap().media = MediaRef::File { path: path.clone() };
    }
    let doc = project.join("p.fcproj").to_string_lossy().into_owned();
    s.execute("file.save", json!({"path": doc})).unwrap();

    let (mut t, seen) = recording_session();
    let r = t.execute("file.open", json!({"path": doc})).unwrap();
    assert_eq!(r["missingMedia"], json!(refused), "{r}");
    for &id in &items {
        assert!(t.media.full_res_source(&t.project, id, &*t.services).is_some());
    }
    t.execute("media.refreshChanged", json!({})).unwrap();

    let asked = seen.lock().unwrap().clone();
    assert!(foreign(&asked).is_empty(), "stored paths on other computers were opened: {:?}", foreign(&asked));
    for (id, path) in items.iter().zip(&stored).take(refused) {
        let st = t.media.offline_status(*id).unwrap_or_else(|| panic!("{path} must render offline"));
        assert_eq!(st.reason, OfflineReason::Missing);
        assert!(st.error.contains("is not opened"), "{st:?}");
        assert!(!t.media_stamps.contains_key(path), "{path} was stamped");
    }
    let rel = items[refused];
    assert_eq!(t.media.offline_status(rel), None, "a path beside the project opens");
    assert!(t.media_stamps.contains_key("b.mov"));

    // linking a file in this session opens it, even at the path the project stored
    if let Some(path) = slashed(&a) {
        let id = items[refused - 1];
        t.execute("media.relink", json!({"item": id.0, "path": path, "force": true})).unwrap();
        t.media.remove(id);
        assert!(t.media.full_res_source(&t.project, id, &*t.services).is_some());
        assert_eq!(t.media.offline_status(id), None, "a file linked in this session opens");
    }
}

/// A one-clip FCP7 XML sequence using the file at `pathurl`.
fn xml(pathurl: &str) -> String {
    let rate = "<rate><timebase>24</timebase><ntsc>FALSE</ntsc></rate>";
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE xmeml>
<xmeml version="4">
  <sequence id="sequence-1">
    <name>Cut</name>
    <duration>12</duration>
    {rate}
    <media>
      <video>
        <format><samplecharacteristics>{rate}<width>{W}</width><height>{H}</height></samplecharacteristics></format>
        <track>
          <clipitem id="clipitem-1">
            <name>shot</name>
            {rate}
            <start>0</start><end>12</end><in>0</in><out>12</out>
            <file id="file-1">
              <name>shot.mov</name>
              <pathurl>{pathurl}</pathurl>
              {rate}
              <duration>12</duration>
            </file>
          </clipitem>
        </track>
      </video>
    </media>
  </sequence>
</xmeml>
"#
    )
}

#[test]
fn interchange_media_paths_open_only_on_this_computer_or_beside_the_document() {
    let root = tmp_dir("stored-paths-xml");
    std::fs::create_dir_all(root.join("media")).unwrap();
    let shot = root.join("media").join("shot.mov");
    make_movie(&shot, DemoScene::Aurora, W, H, 12);

    let mut urls = vec!["file://192.0.2.1/s/shot.mov".to_string()];
    urls.extend(slashed(&shot).map(|p| format!("file://{p}")));
    for (k, url) in urls.iter().enumerate() {
        let doc = root.join(format!("refused-{k}.xml"));
        std::fs::write(&doc, xml(url)).unwrap();
        let (mut s, seen) = recording_session();
        let r = s.execute("file.import", json!({"paths": [doc.to_string_lossy()]})).unwrap();
        assert_eq!(r["documents"][0]["linkedMedia"], json!(0), "{r}");
        let asked = seen.lock().unwrap().clone();
        assert!(foreign(&asked).is_empty(), "{url} was opened: {:?}", foreign(&asked));
        let ids: Vec<ItemId> = s.project.items.keys().copied().collect();
        for id in ids {
            s.media.full_res_source(&s.project, id, &*s.services);
        }
        let asked = seen.lock().unwrap().clone();
        assert!(foreign(&asked).is_empty(), "{url} was rendered: {:?}", foreign(&asked));
    }

    let doc = root.join("beside.xml");
    std::fs::write(&doc, xml("media/shot.mov")).unwrap();
    let (mut s, _) = recording_session();
    let r = s.execute("file.import", json!({"paths": [doc.to_string_lossy()]})).unwrap();
    assert_eq!(r["documents"][0]["linkedMedia"], json!(1), "a path beside the document links: {r}");
}
