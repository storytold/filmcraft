//! A folder dropped from the OS onto the window is imported as a bin of its media (`file.import`
//! with `bins: true`), subfolders as sub-bins, in one undo step; plain files still import flat.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{Bin, BinEntry};
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

fn step(h: &mut Harness<'static, FilmcraftApp>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint();
    h.step();
}

fn harness() -> Harness<'static, FilmcraftApp> {
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(Session::default()));
    for _ in 0..4 {
        step(&mut h);
    }
    h
}

/// A file or folder the OS dropped on the window, as the native integration hands it to egui.
#[derive(Debug)]
struct OsFile(PathBuf);

impl egui::DroppedFile for OsFile {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn drop_paths(h: &mut Harness<'static, FilmcraftApp>, paths: &[&Path]) {
    for p in paths {
        h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(p.to_path_buf())));
    }
    step(h);
    h.input_mut().dropped_files.clear();
    step(h);
}

fn wav(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
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

#[test]
fn dropping_a_folder_imports_it_as_a_bin_in_one_undo_step() {
    let dir = std::env::temp_dir().join(format!("fc-folder-drop-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("Interviews");
    wav(&root.join("a.wav"));
    wav(&root.join("b.wav"));
    wav(&root.join("Day2").join("c.wav"));
    wav(&dir.join("lone.wav"));
    let mut h = harness();
    let steps = h.state().session.history.undo.len();
    drop_paths(&mut h, &[&root, &dir.join("lone.wav")]);
    let p = &h.state().session.project;
    let bin = bin_named(&p.root, "Interviews").expect("a bin named like the folder");
    assert_eq!(items_in(bin), 2);
    assert_eq!(items_in(bin_named(bin, "Day2").expect("a sub-bin")), 1);
    assert_eq!(items_in(&p.root), 1, "a dropped file still imports into the shown bin");
    assert_eq!(h.state().session.history.undo.len(), steps + 1, "one undo step for the whole drop");
    h.state_mut().session.execute("edit.undo", json!({})).unwrap();
    assert!(h.state().session.project.items.values().all(|i| i.as_media().is_none()), "one undo removes everything dropped");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_drop_that_imports_nothing_says_why() {
    let dir = std::env::temp_dir().join(format!("fc-folder-drop-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let junk = dir.join("junk.xyz");
    std::fs::write(&junk, b"nothing").unwrap();
    let mut h = harness();
    drop_paths(&mut h, &[&junk]);
    assert!(h.state().ui.status.contains("junk.xyz"), "the error is shown, not dropped: {:?}", h.state().ui.status);
    let _ = std::fs::remove_dir_all(dir);
}
