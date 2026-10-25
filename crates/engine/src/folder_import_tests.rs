//! `file.import` with `bins: true`: a folder becomes a bin of its media, subfolders sub-bins, in
//! one undo step, with the caps and the errors reported.

use std::path::{Path, PathBuf};

use filmcraft_project::{Bin, BinEntry};
use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fc-folder-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wav(path: &Path) {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(path, filmcraft_media::wav::write_wav16(&[0.0; 4800], 2, 48_000)).unwrap();
}

fn bin_named<'a>(parent: &'a Bin, name: &str) -> Option<&'a Bin> {
    parent.children.iter().find_map(|e| match e {
        BinEntry::Bin(b) if b.name == name => Some(b),
        _ => None,
    })
}

fn items_in(bin: &Bin) -> usize {
    bin.children.iter().filter(|e| matches!(e, BinEntry::Item(_))).count()
}

fn import(s: &mut Session, paths: &[&Path]) -> crate::Result<serde_json::Value> {
    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    s.execute("file.import", json!({"paths": paths, "bins": true}))
}

/// Dropping a folder gave no bin: its files were imported flat, or not at all.
#[test]
fn a_folder_becomes_a_bin_with_its_media_and_sub_bins() {
    let d = temp_dir("tree");
    let root = d.join("Shoot");
    wav(&root.join("b.wav"));
    wav(&root.join("a.wav"));
    wav(&root.join("Day2").join("c.wav"));
    wav(&root.join(".hidden.wav"));
    std::fs::write(root.join("notes.txt"), "not media").unwrap();
    let mut s = Session::default();
    let steps = s.history.undo.len();
    let r = import(&mut s, &[&root]).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(r["bins"].as_array().unwrap().len(), 2);
    assert_eq!(r["errors"], json!([]));
    let shoot = bin_named(&s.project.root, "Shoot").expect("a bin named like the folder");
    assert_eq!(items_in(shoot), 2, "hidden files and non-media stay out");
    let day2 = bin_named(shoot, "Day2").expect("a sub-bin for the subfolder");
    assert_eq!(items_in(day2), 1);
    assert_eq!(s.history.undo.len(), steps + 1, "one undo step for the whole folder");
    s.undo();
    assert!(bin_named(&s.project.root, "Shoot").is_none(), "one undo removes the bin and its media");
    assert!(s.project.items.values().all(|i| i.as_media().is_none()));
    let _ = std::fs::remove_dir_all(d);
}

/// Folders and plain files in one call: the files go to the target bin, the folder to its own.
#[test]
fn files_and_folders_can_be_mixed() {
    let d = temp_dir("mixed");
    wav(&d.join("lone.wav"));
    wav(&d.join("Clips").join("x.wav"));
    let mut s = Session::default();
    let r = import(&mut s, &[&d.join("lone.wav"), &d.join("Clips")]).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 2);
    assert_eq!(items_in(&s.project.root), 1, "the plain file lands in the target bin");
    assert_eq!(items_in(bin_named(&s.project.root, "Clips").unwrap()), 1);
    let _ = std::fs::remove_dir_all(d);
}

/// An empty folder is still a bin.
#[test]
fn an_empty_folder_is_an_empty_bin() {
    let d = temp_dir("empty");
    std::fs::create_dir_all(d.join("Nothing")).unwrap();
    let mut s = Session::default();
    import(&mut s, &[&d.join("Nothing")]).unwrap();
    assert_eq!(items_in(bin_named(&s.project.root, "Nothing").unwrap()), 0);
    let _ = std::fs::remove_dir_all(d);
}

/// Nothing to import is an error, not a silent no-op, and the project is untouched.
#[test]
fn a_missing_path_is_reported_and_changes_nothing() {
    let d = temp_dir("missing");
    let mut s = Session::default();
    let steps = s.history.undo.len();
    let e = import(&mut s, &[&d.join("nope")]).unwrap_err().to_string();
    assert!(e.contains("nope"), "{e}");
    assert_eq!(s.history.undo.len(), steps);
    assert!(s.execute("file.import", json!({"paths": [], "bins": true})).is_err());
    let _ = std::fs::remove_dir_all(d);
}

/// A tree nested deeper than the cap is entered down to the cap, and the rest is reported.
#[test]
fn folders_nest_to_a_capped_depth() {
    let d = temp_dir("deep");
    let mut dir = d.join("L0");
    for k in 1..(super::folder_import::MAX_DEPTH + 4) {
        dir = dir.join(format!("L{k}"));
    }
    wav(&dir.join("deep.wav"));
    wav(&d.join("L0").join("top.wav"));
    let mut s = Session::default();
    let r = import(&mut s, &[&d.join("L0")]).unwrap();
    assert_eq!(r["bins"].as_array().unwrap().len(), super::folder_import::MAX_DEPTH);
    assert_eq!(r["items"].as_array().unwrap().len(), 1, "only the file above the cap");
    assert!(r["errors"].as_array().unwrap().iter().any(|e| e.as_str().unwrap().contains("not entered")), "{r}");
    let _ = std::fs::remove_dir_all(d);
}

/// A host that lists the same huge folder wherever it is asked.
struct Huge;
impl crate::Services for Huge {
    fn read_file(&self, _: &str) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::other("no files"))
    }
    fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
    fn list_entries(&self, _: &str) -> Option<std::io::Result<Vec<crate::media_browser::DirEntry>>> {
        let n = super::folder_import::MAX_ENTRIES + 1;
        Some(Ok((0..n).map(|k| crate::media_browser::DirEntry { name: format!("f{k}.txt"), ..Default::default() }).collect()))
    }
}

/// A folder with more entries than the cap is not read, the import says so, and it does not hang.
#[test]
fn a_huge_folder_is_refused_not_read() {
    let mut s = Session::new(std::sync::Arc::new(Huge));
    let r = s.execute("file.import", json!({"paths": ["/huge"], "bins": true})).unwrap();
    assert_eq!(r["items"], json!([]));
    assert!(r["errors"].as_array().unwrap().iter().any(|e| e.as_str().unwrap().contains("stopped")), "{r}");
}

/// `replace` starts a new project first; the folder dropped with a plain file is imported into that
/// one, not wiped by the file's replace.
#[test]
fn replace_keeps_the_imported_folders() {
    let d = temp_dir("replace");
    wav(&d.join("Keep").join("k.wav"));
    let mut s = Session::default();
    s.execute("file.import", json!({"paths": [d.join("Keep").join("k.wav").to_string_lossy()]})).unwrap();
    wav(&d.join("lone.wav"));
    let paths = [d.join("Keep").to_string_lossy().into_owned(), d.join("lone.wav").to_string_lossy().into_owned()];
    let r = s.execute("file.import", json!({"paths": paths, "bins": true, "replace": true})).unwrap();
    assert_eq!(r["items"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(s.project.items.values().filter(|i| i.as_media().is_some()).count(), 2, "the old project's media is gone");
    assert_eq!(items_in(bin_named(&s.project.root, "Keep").unwrap()), 1);
    let _ = std::fs::remove_dir_all(d);
}
