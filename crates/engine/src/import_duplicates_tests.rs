//! `file.import` of a file the project already has (#356, `import_duplicates`): no second item.
//! Tier 1 (the normalised path), tier 2 (the file identity, confirmed by the fingerprint) and
//! tier 3 (the media identity: offline items are offered for relinking, the original of an
//! ingested copy is skipped), and what is *not* a duplicate: a copy.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use filmcraft_media::MediaKind;
use filmcraft_project::{BinId, ItemId, MediaRef, Project};
use serde_json::{Value, json};

use crate::media_test_util::tmp_dir;
use crate::{Event, FileIdentity, FsServices, Services, Session};

/// A stereo 16-bit WAV with `frames` sample frames at `level`: another `frames` is another size,
/// another `level` other content of the same size.
fn wav_at(frames: usize, level: f32) -> Vec<u8> {
    filmcraft_media::wav::write_wav16(&vec![level; frames * 2], 2, 48_000)
}

fn wav(frames: usize) -> Vec<u8> {
    wav_at(frames, 0.25)
}

fn write_wav(path: &Path, frames: usize) -> String {
    std::fs::write(path, wav(frames)).unwrap();
    path.to_string_lossy().to_string()
}

/// Numbered PNG stills `prefix0001.png`… in `dir`; returns the paths.
fn write_frames(dir: &Path, prefix: &str, numbers: &[u32]) -> Vec<String> {
    numbers
        .iter()
        .map(|&n| {
            let p = dir.join(format!("{prefix}{n:04}.png"));
            image::RgbaImage::from_pixel(32, 18, image::Rgba([(10 * n) as u8, 99, 7, 255])).save(&p).unwrap();
            p.to_string_lossy().to_string()
        })
        .collect()
}

fn import(s: &mut Session, p: Value) -> Value {
    s.execute("file.import", p).unwrap()
}

fn items(r: &Value) -> Vec<ItemId> {
    r["items"].as_array().unwrap().iter().map(|v| ItemId(v.as_u64().unwrap())).collect()
}

/// The `(item, tier)` of every duplicate a result reports.
fn duplicates(r: &Value) -> Vec<(ItemId, String)> {
    let rows = r["duplicates"].as_array().unwrap_or_else(|| panic!("{r}"));
    rows.iter().map(|d| (ItemId(d["item"].as_u64().unwrap()), d["tier"].as_str().unwrap().to_string())).collect()
}

fn dup(item: ItemId, tier: &str) -> Vec<(ItemId, String)> {
    vec![(item, tier.to_string())]
}

fn media_count(p: &Project) -> usize {
    p.items.values().filter(|i| i.as_media().is_some()).count()
}

fn path_of(s: &Session, item: ItemId) -> String {
    match &s.project.item(item).unwrap().as_media().unwrap().media {
        MediaRef::File { path } => path.clone(),
        MediaRef::Generator(_) => panic!("not a file"),
    }
}

// ---- tier 1: the path ----

#[test]
fn the_same_path_again_is_reported_not_added() {
    let dir = tmp_dir("dup-same");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let r = import(&mut s, json!({"paths": [a]}));
    let id = items(&r)[0];
    assert_eq!((&r["duplicates"], &r["relink"]), (&json!([]), &json!([])), "the fields are always there: {r}");
    let undo = s.history.undo.len();
    s.drain_events();

    let r = import(&mut s, json!({"paths": [a]}));
    assert_eq!(r["items"], json!([]));
    assert_eq!(r["duplicates"], json!([{"path": a, "item": id.0, "tier": "path"}]));
    assert_eq!(r["errors"], json!([]), "a duplicate is not an error: {r}");
    assert_eq!(media_count(&s.project), 1);
    assert_eq!(s.history.undo.len(), undo, "nothing to undo");
    assert!(s.drain_events().iter().any(|e| matches!(e, Event::Toast { message, error: false } if message == "Already in the project: a.wav")));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lag of #356: a folder dragged in again and again stays one item per file.
#[test]
fn a_folder_imported_again_adds_nothing() {
    let dir = tmp_dir("dup-folder");
    let paths: Vec<String> = (0..12).map(|i| write_wav(&dir.join(format!("clip{i:02}.wav")), 2400 + i * 16)).collect();
    let mut s = Session::default();
    assert_eq!(items(&import(&mut s, json!({"paths": paths}))).len(), 12);
    for _ in 0..3 {
        let r = import(&mut s, json!({"paths": paths}));
        assert_eq!((items(&r).len(), duplicates(&r).len()), (0, 12), "{r}");
    }
    assert_eq!(media_count(&s.project), 12);
    assert!(
        s.drain_events()
            .iter()
            .any(|e| matches!(e, Event::Toast { message, .. } if message == "Already in the project: clip00.wav, clip01.wav, clip02.wav and 9 more"))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_path_listed_twice_in_one_import_is_added_once() {
    let dir = tmp_dir("dup-once");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let b = write_wav(&dir.join("b.wav"), 2400);
    let mut s = Session::default();
    let r = import(&mut s, json!({"paths": [a, b, a]}));
    let ids = items(&r);
    assert_eq!(ids.len(), 2, "{r}");
    assert_eq!(duplicates(&r), dup(ids[0], "path"));
    assert_eq!(media_count(&s.project), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn duplicates_and_errors_are_reported_apart() {
    let dir = tmp_dir("dup-errors");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let missing = dir.join("missing.wav").to_string_lossy().to_string();
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];

    // a duplicate beside a failure: the command succeeds and says both, each in its place
    let r = import(&mut s, json!({"paths": [a, missing]}));
    assert_eq!(duplicates(&r), dup(id, "path"));
    let errors: Vec<&str> = r["errors"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(errors.len(), 1, "{r}");
    assert!(errors[0].starts_with(&missing), "{r}");
    assert!(!r["errors"].to_string().contains("a.wav"), "{r}");

    // nothing but failures is still an error
    let e = s.execute("file.import", json!({"paths": [missing]})).unwrap_err().to_string();
    assert!(e.contains("missing.wav"), "{e}");
    // and a file that is missing now is not a duplicate of the item it once was
    std::fs::remove_file(&a).unwrap();
    assert!(s.execute("file.import", json!({"paths": [a]})).is_err());
    assert_eq!(media_count(&s.project), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A duplicate stays where the user put it: `bin` says where *new* items go, nothing more.
#[test]
fn a_duplicate_is_never_moved_to_the_bin_asked_for() {
    let dir = tmp_dir("dup-bin");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let b = write_wav(&dir.join("b.wav"), 2400);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    let root = s.project.root.id;
    let bin = BinId(s.execute("file.newBin", json!({"name": "Sound"})).unwrap()["bin"].as_u64().unwrap());
    let undo = s.history.undo.len();

    let r = import(&mut s, json!({"paths": [a], "bin": bin.0}));
    assert_eq!(r["duplicates"], json!([{"path": a, "item": id.0, "tier": "path"}]), "no `moved`: {r}");
    assert_eq!(s.project.root.parent_of(id), Some(root));
    assert_eq!(s.history.undo.len(), undo);
    // the new file of the same import does go there
    let r = import(&mut s, json!({"paths": [a, b], "bin": bin.0}));
    assert_eq!(s.project.root.parent_of(items(&r)[0]), Some(bin));
    assert_eq!(s.project.root.parent_of(id), Some(root));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The path decides: a file replaced on disk under the same path is still the item's file.
#[test]
fn a_file_replaced_under_its_path_is_still_the_same_item() {
    let dir = tmp_dir("dup-replaced");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    write_wav(Path::new(&a), 9600);
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [a]}))), dup(id, "path"));
    assert_eq!(media_count(&s.project), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn unix_spellings_of_a_path_are_one_file() {
    let dir = tmp_dir("dup-unix");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];

    let sym = dir.join("sym.wav");
    std::os::unix::fs::symlink(&a, &sym).unwrap();
    let chain = dir.join("sub").join("chain.wav");
    std::os::unix::fs::symlink(&sym, &chain).unwrap();
    let linked_dir = dir.join("linked");
    std::os::unix::fs::symlink(&dir, &linked_dir).unwrap();
    let spellings = [
        sym,
        chain,
        dir.join("sub").join("..").join("a.wav"),
        dir.join(".").join("a.wav"),
        linked_dir.join("a.wav"),
        linked_dir.join("sub").join("..").join("a.wav"),
        std::path::PathBuf::from(format!("{}//a.wav", dir.display())),
    ];
    for p in &spellings {
        let r = import(&mut s, json!({"paths": [p.to_string_lossy()]}));
        assert_eq!(duplicates(&r), dup(id, "path"), "{}: {r}", p.display());
        assert_eq!(r["duplicates"][0]["path"], json!(p.to_string_lossy()), "the path as it was given");
    }
    // the item imported through a link, then the file itself: the same the other way round
    let b = write_wav(&dir.join("b.wav"), 2400);
    let b_sym = dir.join("b-sym.wav");
    std::os::unix::fs::symlink(&b, &b_sym).unwrap();
    let b_id = items(&import(&mut s, json!({"paths": [b_sym.to_string_lossy()]})))[0];
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [b]}))), dup(b_id, "path"));
    // a symlink that leads nowhere is an error, not a duplicate
    let dangling = dir.join("dangling.wav");
    std::os::unix::fs::symlink(dir.join("gone.wav"), &dangling).unwrap();
    assert!(s.execute("file.import", json!({"paths": [dangling.to_string_lossy()]})).is_err());
    assert_eq!(media_count(&s.project), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_relative_path_and_the_absolute_one_are_one_file() {
    let dir = tmp_dir("dup-relative");
    let a = write_wav(&dir.join("a.wav"), 4800);
    // (relative to wherever the tests run: the temp directory by way of the current one)
    let cwd = std::env::current_dir().unwrap();
    let Some(up) = pathdiff(&cwd, Path::new(&a)) else { return };
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [up]}))), dup(id, "path"), "{up}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `to` written relative to `from`: as many `..` as `from` is deep, then `to` from the root.
/// `None` when `to` is on another drive (Windows).
fn pathdiff(from: &Path, to: &Path) -> Option<String> {
    use std::path::Component;
    let (from, to) = (from.canonicalize().ok()?, to.canonicalize().ok()?);
    let prefix = |p: &Path| p.components().find_map(|c| if let Component::Prefix(x) = c { Some(x.as_os_str().to_owned()) } else { None });
    if prefix(&from) != prefix(&to) {
        return None;
    }
    let mut out = std::path::PathBuf::new();
    from.components().filter(|c| matches!(c, Component::Normal(_))).for_each(|_| out.push(".."));
    to.components().filter(|c| matches!(c, Component::Normal(_))).for_each(|c| out.push(c));
    Some(out.to_string_lossy().to_string())
}

// ---- copies ----

/// A copy is its own file, however alike: proxy and versioning workflows keep them on purpose.
#[test]
fn a_copy_is_another_item() {
    let dir = tmp_dir("dup-copy");
    std::fs::create_dir_all(dir.join("Proxies")).unwrap();
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    let same_name = dir.join("Proxies").join("a.wav");
    let other_name = dir.join("a copy.wav");
    std::fs::copy(&a, &same_name).unwrap();
    std::fs::copy(&a, &other_name).unwrap();
    for p in [&same_name, &other_name] {
        let r = import(&mut s, json!({"paths": [p.to_string_lossy()]}));
        assert_eq!((items(&r).len(), duplicates(&r).len(), r["relink"].as_array().unwrap().len()), (1, 0, 0), "{}: {r}", p.display());
        // the same media identity as the original, and still not it
        assert_eq!(s.project.item(items(&r)[0]).unwrap().as_media().unwrap().identity, s.project.item(id).unwrap().as_media().unwrap().identity);
    }
    assert_eq!(media_count(&s.project), 3);
    // each copy again is a duplicate of itself, not of the original
    let r = import(&mut s, json!({"paths": [same_name.to_string_lossy()]}));
    assert_ne!(duplicates(&r)[0].0, id);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- tier 2: the file identity, confirmed by the fingerprint ----

#[cfg(unix)]
#[test]
fn unix_hard_links_are_one_file() {
    let dir = tmp_dir("dup-hard");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    let hard = dir.join("sub").join("hard.wav");
    std::fs::hard_link(&a, &hard).unwrap();
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [hard.to_string_lossy()]}))), dup(id, "file"));
    // through a symlink to the hard link too
    let sym = dir.join("sym-to-hard.wav");
    std::os::unix::fs::symlink(&hard, &sym).unwrap();
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [sym.to_string_lossy()]}))), dup(id, "file"));
    // two links of a new file in one import: one item
    let b = write_wav(&dir.join("b.wav"), 2400);
    let b_hard = dir.join("b-hard.wav");
    std::fs::hard_link(&b, &b_hard).unwrap();
    let r = import(&mut s, json!({"paths": [b, b_hard.to_string_lossy()]}));
    assert_eq!(duplicates(&r), dup(items(&r)[0], "file"), "{r}");
    assert_eq!(media_count(&s.project), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn native_paths_and_file_identities() {
    let dir = tmp_dir("dup-native");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let b = write_wav(&dir.join("b.wav"), 4800);
    let canon = FsServices.canonical_path(&a).unwrap();
    assert_eq!(FsServices.canonical_path(&dir.join(".").join("a.wav").to_string_lossy()), Some(canon.clone()));
    assert_ne!(FsServices.canonical_path(&b), Some(canon));
    assert_eq!(FsServices.canonical_path(&dir.join("none.wav").to_string_lossy()), None);
    #[cfg(unix)]
    {
        let hard = dir.join("hard.wav");
        std::fs::hard_link(&a, &hard).unwrap();
        let ia = FsServices.file_identity(&a).unwrap();
        assert_eq!(FsServices.file_identity(&hard.to_string_lossy()), Some(ia));
        assert_ne!(FsServices.file_identity(&b), Some(ia));
        // a directory is not a file to import; a missing path has no identity
        assert_eq!(FsServices.file_identity(&dir.to_string_lossy()), None);
        assert_eq!(FsServices.file_identity(&dir.join("none.wav").to_string_lossy()), None);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An in-memory host: files with the identity a filesystem would report (or none) and the
/// canonical spelling of their path (or none), counting the reads of each path. Paths are opaque
/// keys, so Windows spellings and misbehaving mounts work on every platform.
#[derive(Default)]
struct FakeFs {
    files: Mutex<BTreeMap<String, FakeFile>>,
    reads: Mutex<BTreeMap<String, usize>>,
}

struct FakeFile {
    bytes: Vec<u8>,
    id: Option<(u64, u64)>,
    canonical: Option<String>,
}

impl FakeFs {
    fn add(&self, path: &str, bytes: Vec<u8>, id: Option<(u64, u64)>, canonical: Option<&str>) {
        self.files.lock().unwrap().insert(path.to_string(), FakeFile { bytes, id, canonical: canonical.map(str::to_string) });
    }
    fn reads(&self, path: &str) -> usize {
        self.reads.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

impl Services for FakeFs {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        *self.reads.lock().unwrap().entry(path.to_string()).or_default() += 1;
        self.files.lock().unwrap().get(path).map(|f| f.bytes.clone()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string()))
    }
    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        self.add(path, data.to_vec(), None, None);
        Ok(())
    }
    fn file_size(&self, path: &str) -> std::io::Result<u64> {
        self.files.lock().unwrap().get(path).map(|f| f.bytes.len() as u64).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string()))
    }
    fn canonical_path(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(path)?.canonical.clone()
    }
    fn file_identity(&self, path: &str) -> Option<FileIdentity> {
        self.files.lock().unwrap().get(path)?.id.map(|(volume, index)| FileIdentity { volume, index })
    }
}

/// What a Windows host reports (one canonical path for every spelling), and what the import does
/// with it.
#[test]
fn windows_spellings_of_one_file_are_one_item() {
    const CANON: &str = r"\\?\C:\Media\Clip.wav";
    let fs = Arc::new(FakeFs::default());
    let spellings = [r"c:\media\CLIP.WAV", r"C:\Media\Sub\..\Clip.wav", r"\\?\C:\Media\Clip.wav", "C:/Media/Clip.wav", r"C:\Junction\Clip.wav"];
    fs.add(r"C:\Media\Clip.wav", wav(4800), None, Some(CANON));
    for p in spellings {
        fs.add(p, wav(4800), None, Some(CANON));
    }
    // a copy on another drive: the same content under another canonical path
    fs.add(r"E:\Media\Clip.wav", wav(4800), None, Some(r"\\?\E:\Media\Clip.wav"));
    let mut s = Session::new(fs.clone());
    let id = items(&import(&mut s, json!({"paths": [r"C:\Media\Clip.wav"]})))[0];
    s.drain_events();
    for p in spellings {
        let r = import(&mut s, json!({"paths": [p]}));
        assert_eq!(r["duplicates"], json!([{"path": p, "item": id.0, "tier": "path"}]), "{p}");
        assert_eq!(r["items"], json!([]), "{p}");
    }
    // the toast names the file, not its Windows path
    assert!(s.drain_events().iter().any(|e| matches!(e, Event::Toast { message, .. } if message == "Already in the project: Clip.wav")));
    let r = import(&mut s, json!({"paths": [r"E:\Media\Clip.wav"]}));
    assert_eq!((items(&r).len(), duplicates(&r).len()), (1, 0), "{r}");
    assert_eq!(media_count(&s.project), 2);
}

/// A network share or FUSE mount that gives every file the same number: the fingerprint keeps
/// different files apart, also files of one size.
#[test]
fn a_file_identity_alone_does_not_make_a_duplicate() {
    let fs = Arc::new(FakeFs::default());
    fs.add("/mnt/share/a.wav", wav_at(4800, 0.25), Some((7, 1)), None);
    fs.add("/mnt/share/b.wav", wav_at(4800, 0.5), Some((7, 1)), None);
    fs.add("/mnt/share/c.wav", wav_at(2400, 0.25), Some((7, 1)), None);
    // a hard link the mount reports truthfully: the same number and the same content
    fs.add("/mnt/share/link-to-a.wav", wav_at(4800, 0.25), Some((7, 1)), None);
    // the same number on another volume is another file
    fs.add("/mnt/other/a.wav", wav_at(4800, 0.25), Some((8, 1)), None);
    let mut s = Session::new(fs.clone());
    let r = import(&mut s, json!({"paths": ["/mnt/share/a.wav", "/mnt/share/b.wav", "/mnt/share/c.wav"]}));
    let ids = items(&r);
    assert_eq!((ids.len(), duplicates(&r).len()), (3, 0), "{r}");
    assert_eq!(duplicates(&import(&mut s, json!({"paths": ["/mnt/share/link-to-a.wav"]}))), dup(ids[0], "file"));
    let r = import(&mut s, json!({"paths": ["/mnt/other/a.wav"]}));
    assert_eq!((items(&r).len(), duplicates(&r).len()), (1, 0), "{r}");
    assert_eq!(media_count(&s.project), 4);
}

/// A host without canonical paths or file identities (the web): the path as written is all
/// there is, and no file is read to find that out.
#[test]
fn a_host_that_cannot_tell_compares_paths_as_written() {
    let fs = Arc::new(FakeFs::default());
    fs.add("/dropped/a.wav", wav(4800), None, None);
    fs.add("/dropped/A.wav", wav(4800), None, None);
    let mut s = Session::new(fs.clone());
    let id = items(&import(&mut s, json!({"paths": ["/dropped/a.wav"]})))[0];
    let reads = fs.reads("/dropped/a.wav");
    assert_eq!(duplicates(&import(&mut s, json!({"paths": ["/dropped/a.wav"]}))), dup(id, "path"));
    assert_eq!(fs.reads("/dropped/a.wav"), reads, "a path match needs no read");
    let r = import(&mut s, json!({"paths": ["/dropped/A.wav"]}));
    assert_eq!((items(&r).len(), duplicates(&r).len()), (1, 0), "{r}");
}

// ---- tier 3: the media identity ----

/// The file of an offline clip, found somewhere else: Link Media is offered; no second item.
#[test]
fn the_media_of_an_offline_item_is_offered_for_relinking() {
    let dir = tmp_dir("dup-relink");
    std::fs::create_dir_all(dir.join("moved")).unwrap();
    let a = write_wav(&dir.join("a.wav"), 4800);
    let b = write_wav(&dir.join("b.wav"), 2400);
    let mut s = Session::default();
    let ids = items(&import(&mut s, json!({"paths": [a, b]})));
    // a.wav moves away (missing); b.wav is made offline on purpose and a copy turns up
    let a_moved = dir.join("moved").join("a.wav").to_string_lossy().to_string();
    std::fs::rename(&a, &a_moved).unwrap();
    s.execute("media.makeOffline", json!({"items": [ids[1].0]})).unwrap();
    let b_copy = dir.join("moved").join("renamed.wav").to_string_lossy().to_string();
    std::fs::copy(&b, &b_copy).unwrap();
    // a file of the same size as a.wav with other content is new media
    let other = dir.join("moved").join("other.wav");
    std::fs::write(&other, wav_at(4800, 0.5)).unwrap();
    s.offline.prompt = false;
    s.drain_events();
    let undo = s.history.undo.len();

    let r = import(&mut s, json!({"paths": [a_moved, b_copy]}));
    assert_eq!(r["relink"], json!([{"path": a_moved, "item": ids[0].0}, {"path": b_copy, "item": ids[1].0}]));
    assert_eq!((&r["items"], &r["duplicates"], &r["errors"]), (&json!([]), &json!([]), &json!([])), "{r}");
    assert_eq!((media_count(&s.project), s.history.undo.len()), (2, undo), "nothing is imported and nothing relinked yet");
    assert_eq!(path_of(&s, ids[0]), a);
    assert!(s.offline.prompt, "the Link Media dialog is asked for");
    let toasts: Vec<String> = s.drain_events().into_iter().filter_map(|e| if let Event::Toast { message, .. } = e { Some(message) } else { None }).collect();
    assert_eq!(toasts, ["Media of offline clips, not imported: a.wav, renamed.wav. Use Link Media to reconnect."]);

    // an offline item's own path offers it too (it was made offline on purpose: not a duplicate)
    assert_eq!(import(&mut s, json!({"paths": [b]}))["relink"], json!([{"path": b, "item": ids[1].0}]));
    let r = import(&mut s, json!({"paths": [other.to_string_lossy()]}));
    assert_eq!((items(&r).len(), r["relink"].as_array().unwrap().len()), (1, 0), "{r}");

    // taking the offer: the path it reported links the clip, and then the file is a duplicate
    s.execute("media.relink", json!({"item": ids[0].0, "path": a_moved, "relinkOthers": false})).unwrap();
    assert_eq!(path_of(&s, ids[0]), a_moved);
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [a_moved]}))), dup(ids[0], "path"));
    assert_eq!(media_count(&s.project), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A missing file that comes back under its own path needs no relinking: the item is online
/// again and the path is a duplicate (#110).
#[test]
fn a_missing_file_back_at_its_path_is_a_duplicate_again() {
    let dir = tmp_dir("dup-back");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    std::fs::remove_file(&a).unwrap();
    crate::relink::refresh(&mut s);
    assert_eq!(s.offline.missing, [id]);
    write_wav(Path::new(&a), 4800);
    let r = import(&mut s, json!({"paths": [a]}));
    assert_eq!((duplicates(&r), r["relink"].clone()), (dup(id, "path"), json!([])));
    assert!(s.offline.missing.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Ingest copies on import and the item uses the copy: the original imported again is the file
/// the project already holds, not one to copy over it.
#[test]
fn the_original_of_an_ingested_copy_is_a_duplicate() {
    let dir = tmp_dir("dup-ingest");
    let (card, dest) = (dir.join("Card"), dir.join("Ingest"));
    std::fs::create_dir_all(&card).unwrap();
    let a = write_wav(&card.join("a.wav"), 4800);
    let mut s = Session::default();
    s.execute("project.ingestSettings", json!({"enabled": true, "action": "copy", "destination": dest.to_string_lossy()})).unwrap();
    let r = import(&mut s, json!({"paths": [a]}));
    let id = items(&r)[0];
    let copy = dest.join("a.wav").to_string_lossy().to_string();
    assert_eq!(path_of(&s, id), copy, "{r}");

    let r = import(&mut s, json!({"paths": [a]}));
    assert_eq!(r["duplicates"], json!([{"path": a, "item": id.0, "tier": "ingested"}]), "{r}");
    assert!(r["ingest"].is_null(), "nothing is copied again: {r}");
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [copy]}))), dup(id, "path"));
    // another card's file of the same name would be ingested to the same path: other content,
    // so not the file the project has (ingest then says what it does with it)
    let card2 = dir.join("Card 2");
    std::fs::create_dir_all(&card2).unwrap();
    let other = card2.join("a.wav");
    std::fs::write(&other, wav_at(4800, 0.5)).unwrap();
    let r = import(&mut s, json!({"paths": [other.to_string_lossy()]}));
    assert_eq!((items(&r).len(), duplicates(&r).len()), (1, 0), "{r}");
    // with ingest off, the original is a file of its own again (a copy of the item's)
    s.execute("project.ingestSettings", json!({"enabled": false})).unwrap();
    let r = import(&mut s, json!({"paths": [a]}));
    assert_eq!((items(&r).len(), duplicates(&r).len()), (1, 0), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- image sequences ----

#[test]
fn an_image_sequence_and_its_first_still_are_two_items() {
    let dir = tmp_dir("dup-seq");
    let f = write_frames(&dir, "shot.", &[1, 2, 3]);
    let mut s = Session::default();
    let seq = items(&import(&mut s, json!({"paths": [f[0]], "imageSequence": true})))[0];
    assert_eq!(s.project.item(seq).unwrap().as_media().unwrap().info.kind, MediaKind::ImageSequence);
    // the sequence again
    let r = import(&mut s, json!({"paths": [f[0]], "imageSequence": true}));
    assert_eq!(duplicates(&r), dup(seq, "path"));
    assert!(r.get("imageSequences").is_none(), "{r}");
    let r = s.execute("file.importImageSequence", json!({"path": f[0]})).unwrap();
    assert_eq!(duplicates(&r), dup(seq, "path"));
    // its first frame as a still is another item; then that still again is a duplicate of the still
    let still = items(&import(&mut s, json!({"paths": [f[0]]})))[0];
    assert_eq!(s.project.item(still).unwrap().as_media().unwrap().info.kind, MediaKind::Still);
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [f[0]]}))), dup(still, "path"));
    // the sequence from its second frame on is other media
    let r = import(&mut s, json!({"paths": [f[1]], "imageSequence": true}));
    assert_eq!(items(&r).len(), 1, "{r}");
    // Settings ▸ Media ▸ Import image sequences: a detected sequence is the same sequence
    s.execute("prefs.set", json!({"key": "media.importImageSequences", "value": true})).unwrap();
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [f[0]]}))), dup(seq, "path"));
    assert_eq!(media_count(&s.project), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A hard link to a sequence's first frame is the same file but not the same sequence: the frames
/// beside it are others (or none), in its own directory or under its own name.
#[cfg(unix)]
#[test]
fn unix_linked_frames_start_their_own_sequences() {
    let dir = tmp_dir("dup-seq-links");
    let (one, two) = (dir.join("one"), dir.join("two"));
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let f = write_frames(&one, "shot.", &[1, 2, 3]);
    let mut s = Session::default();
    let seq = items(&import(&mut s, json!({"paths": [f[0]], "imageSequence": true})))[0];
    let frames = |s: &Session, id: ItemId| {
        let m = s.project.item(id).unwrap().as_media().unwrap();
        m.info.duration.0 / m.info.frame_rate().tick_of(1).0
    };
    assert_eq!(frames(&s, seq), 3);

    // in another directory, with other frames beside it
    let other = two.join("shot.0001.png");
    std::fs::hard_link(&f[0], &other).unwrap();
    write_frames(&two, "shot.", &[2, 3, 4, 5]);
    let r = import(&mut s, json!({"paths": [other.to_string_lossy()], "imageSequence": true}));
    assert_eq!(duplicates(&r), [], "{r}");
    assert_eq!(frames(&s, items(&r)[0]), 5);
    // under another name in the same directory (a symlink: its target's name is not its own)
    let renamed = one.join("alt.0001.png");
    std::os::unix::fs::symlink(&f[0], &renamed).unwrap();
    let r = import(&mut s, json!({"paths": [renamed.to_string_lossy()], "imageSequence": true}));
    assert_eq!(duplicates(&r), [], "{r}");
    assert_eq!(frames(&s, items(&r)[0]), 1);

    // the same sequence through a symlinked directory, or `..`, is the same sequence
    let linked = dir.join("linked");
    std::os::unix::fs::symlink(&one, &linked).unwrap();
    for p in [linked.join("shot.0001.png"), two.join("..").join("one").join("shot.0001.png")] {
        let r = import(&mut s, json!({"paths": [p.to_string_lossy()], "imageSequence": true}));
        assert_eq!(duplicates(&r), dup(seq, "path"), "{}: {r}", p.display());
    }
    // as single stills, the hard links are one file
    let still = items(&import(&mut s, json!({"paths": [f[0]]})))[0];
    assert_eq!(duplicates(&import(&mut s, json!({"paths": [other.to_string_lossy()]}))), dup(still, "file"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the commands that import on the way ----

#[test]
fn commands_that_import_use_the_item_that_is_already_there() {
    let dir = tmp_dir("dup-callers");
    let a = write_wav(&dir.join("a.wav"), 4800);
    let mut s = Session::default();
    let id = items(&import(&mut s, json!({"paths": [a]})))[0];
    assert_eq!(crate::commands::imported_item(&import(&mut s, json!({"paths": [a]}))), Some(id));
    assert_eq!(crate::commands::imported_item(&json!({"items": [], "duplicates": [], "relink": []})), None);
    let _ = std::fs::remove_dir_all(&dir);
}
