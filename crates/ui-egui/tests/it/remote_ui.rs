//! Remote storage in the UI: the File menu with and without a host that has remote storage, and
//! the remote dialog (navigation, a listing that is still loading, open, cancel, save names).

use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::media_browser::{DirEntry, Volume, VolumeKind};
use filmcraft_engine::{Services, Session};
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::panels::remote_dialog::save_target;
use filmcraft_ui_egui::{FilmcraftApp, RemoteInfo};
use serde_json::{Value, json};

/// A fake remote mount `/data` with a `clips` folder; the first listing of `clips` is still loading.
struct FakeRemote {
    clips_calls: AtomicUsize,
}

impl Services for FakeRemote {
    /// Read file.
    fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
        Err(io::Error::new(io::ErrorKind::NotFound, path.to_string()))
    }
    /// Write file.
    fn write_file(&self, _: &str, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
    /// Volumes.
    fn volumes(&self) -> Vec<Volume> {
        vec![Volume { name: "Server".into(), path: "/data".into(), kind: VolumeKind::Network }]
    }
    /// List entries.
    fn list_entries(&self, dir: &str) -> Option<io::Result<Vec<DirEntry>>> {
        let file = |n: &str| DirEntry { name: n.into(), size: Some(10), ..Default::default() };
        match dir {
            "/data" => Some(Ok(vec![DirEntry { name: "clips".into(), is_dir: true, ..Default::default() }, file("one.fcproj"), file("clip.mp4")])),
            "/data/clips" if self.clips_calls.fetch_add(1, Ordering::SeqCst) == 0 => Some(Err(io::Error::new(io::ErrorKind::WouldBlock, "Loading…"))),
            "/data/clips" => Some(Ok(vec![file("two.fcproj")])),
            _ => Some(Err(io::Error::other("no such folder"))),
        }
    }
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    opened: Rc<RefCell<Vec<String>>>,
    imports: Rc<RefCell<Vec<(String, Value)>>>,
}

impl Driver {
    /// New.
    fn new(remote: bool) -> Self {
        let session = Session::new(Arc::new(FakeRemote { clips_calls: AtomicUsize::new(0) }));
        let (tx, rx) = channel();
        let mut app = FilmcraftApp::new(session).with_control(rx);
        let opened: Rc<RefCell<Vec<String>>> = Rc::default();
        let imports: Rc<RefCell<Vec<(String, Value)>>> = Rc::default();
        if remote {
            let i = imports.clone();
            app.hooks.import_command = Some(Box::new(move |id, p| {
                i.borrow_mut().push((id.to_string(), p.clone()));
                p["paths"].as_array().is_some_and(|a| a.iter().any(|x| x.as_str().is_some_and(|x| x.starts_with("/data/"))))
            }));
            app.hooks.remote = Some(RemoteInfo { name: "Server".into() });
            let o = opened.clone();
            app.hooks.open_project = Some(Box::new(move |p| o.borrow_mut().push(p.to_string())));
        }
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, opened, imports };
        d.frames(4);
        d
    }

    /// Frames.
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    /// Call.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    /// Click.
    fn click(&mut self, id: &str) {
        let r = self.call("ui.click", json!({"id": id}));
        assert_eq!(r["ok"], true, "click {id}: {r}");
        self.frames(3);
    }

    /// Has.
    fn has(&mut self, id: &str) -> bool {
        let v = self.call("ui.elements", json!({"prefix": id}));
        v["result"].as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    /// (id, label) of every menu entry.
    fn menu(&mut self) -> Vec<(String, String)> {
        let v = self.call("ui.menu.list", json!({}));
        v["result"].as_array().unwrap().iter().map(|m| (m["id"].as_str().unwrap().to_string(), m["label"].as_str().unwrap().to_string())).collect()
    }
}

/// The label of menu item `id`, if present.
fn label(menu: &[(String, String)], id: &str) -> Option<String> {
    menu.iter().find(|(i, _)| i == id).map(|(_, l)| l.clone())
}

/// The index of menu item `id`; panics when it is missing.
fn position(menu: &[(String, String)], id: &str) -> usize {
    menu.iter().position(|(i, _)| i == id).unwrap_or_else(|| panic!("{id} not in the menu"))
}

/// Without remote storage the menu is unchanged.
#[test]
fn without_remote_storage_the_menu_is_unchanged() {
    let mut d = Driver::new(false);
    let m = d.menu();
    assert_eq!(label(&m, "file.open").as_deref(), Some("Open Project…"));
    assert_eq!(label(&m, "file.import").as_deref(), Some("Import…"));
    assert!(m.iter().all(|(id, l)| !id.contains("Remote") && !l.contains("Remote") && !l.contains("Local")), "{m:?}");
    // the commands refuse
    let r = d.call("ui.menu.invoke", json!({"id": "file.openRemote"}));
    assert_eq!(r["ok"], false);
    assert_eq!(d.call("ui.menu.invoke", json!({"id": "file.saveAsRemote"}))["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("remote storage is not configured"), "{r}");
}

/// With remote storage the local entries say local and remote ones follow.
#[test]
fn with_remote_storage_the_local_entries_say_local_and_remote_ones_follow() {
    let mut d = Driver::new(true);
    let m = d.menu();
    assert_eq!(label(&m, "file.open").as_deref(), Some("Open Local Project…"));
    assert_eq!(label(&m, "file.import").as_deref(), Some("Import Local…"));
    assert_eq!(label(&m, "file.importImageSequence").as_deref(), Some("Import Local Image Sequence…"));
    assert_eq!(label(&m, "file.openRemote").as_deref(), Some("Open Remote Project…"));
    assert_eq!(label(&m, "file.importRemote").as_deref(), Some("Import from Remote…"));
    assert_eq!(position(&m, "file.openRemote"), position(&m, "file.open") + 1);
    assert_eq!(position(&m, "file.importRemote"), position(&m, "file.import") + 1);
    assert_eq!(label(&m, "file.saveAs").as_deref(), Some("Save As…"));
    assert_eq!(label(&m, "file.saveAsRemote").as_deref(), Some("Save As Remote…"));
    assert_eq!(position(&m, "file.saveAsRemote"), position(&m, "file.saveAs") + 1);
    // Open Recent stays after the remote entry
    assert!(position(&m, "file.clearRecent") > position(&m, "file.openRemote"));
}

/// The remote dialog navigates waits for a loading folder and opens a project.
#[test]
fn the_remote_dialog_navigates_waits_for_a_loading_folder_and_opens_a_project() {
    let mut d = Driver::new(true);
    assert_eq!(d.call("ui.menu.invoke", json!({"id": "file.openRemote"}))["result"]["dialog"], "remote");
    d.frames(3);
    // a single mount opens in it; only projects and folders are listed
    assert!(d.has("remoteDialog.entry.clips"));
    assert!(d.has("remoteDialog.entry.one.fcproj"));
    assert!(!d.has("remoteDialog.entry.clip.mp4"));
    // the folder is still loading, then arrives
    d.click("remoteDialog.entry.clips");
    d.frames(8);
    assert!(d.has("remoteDialog.entry.two.fcproj"));
    d.click("remoteDialog.entry.two.fcproj");
    d.click("remoteDialog.accept");
    assert_eq!(*d.opened.borrow(), ["/data/clips/two.fcproj"]);
    assert!(!d.has("remoteDialog.accept"), "the dialog closed");
}

/// Cancel opens nothing and a second dialog is refused.
#[test]
fn cancel_opens_nothing_and_a_second_dialog_is_refused() {
    let mut d = Driver::new(true);
    d.call("ui.menu.invoke", json!({"id": "file.openRemote"}));
    d.frames(3);
    assert_eq!(d.call("ui.menu.invoke", json!({"id": "file.importRemote"}))["ok"], false);
    d.click("remoteDialog.cancel");
    assert!(d.opened.borrow().is_empty());
    assert!(!d.has("remoteDialog.cancel"));
}

/// Save names stay in the folder and get the project extension.
#[test]
fn save_names_stay_in_the_folder_and_get_the_project_extension() {
    assert_eq!(save_target("/data/a", "film").unwrap(), "/data/a/film.fcproj");
    assert_eq!(save_target("/data/a", " film.FCPROJ ").unwrap(), "/data/a/film.FCPROJ");
    for bad in ["", "  ", ".", "..", "../x", "a/b", "a\\b", "a\nb"] {
        assert!(save_target("/data/a", bad).is_err(), "{bad:?}");
    }
    assert!(save_target("", "film").is_err());
}

/// Save as remote saves to the chosen path.
#[test]
fn save_as_remote_saves_to_the_chosen_path() {
    let mut d = Driver::new(true);
    assert_eq!(d.call("ui.menu.invoke", json!({"id": "file.saveAsRemote"}))["result"]["dialog"], "remote");
    d.frames(3);
    d.click("remoteDialog.entry.clips");
    d.frames(8);
    // the suggested name (the project's) is used; the fake services accept the write
    d.click("remoteDialog.accept");
    let path = d.harness.state_mut().session.path.clone();
    assert!(path.is_some_and(|p| p.starts_with("/data/clips/")), "saved under the remote folder");
}

/// Imports of remote files go to the host first.
#[test]
fn imports_of_remote_files_go_to_the_host_first() {
    let mut d = Driver::new(true);
    // the host takes a remote path over: nothing is imported by the command itself
    let r = d.call("ui.menu.invoke", json!({"id": "mediaBrowser.import", "params": {"paths": ["/data/clip.mp4"]}}));
    assert_eq!(r["result"]["pending"], true, "{r}");
    // the selection is resolved into `paths` for the host
    d.harness.state_mut().session.browser.selection = vec!["/data/one.fcproj".into()];
    let r = d.call("ui.menu.invoke", json!({"id": "mediaBrowser.import"}));
    assert_eq!(r["result"]["pending"], true, "{r}");
    let seen = d.imports.borrow().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0, "mediaBrowser.import");
    assert_eq!(seen[1].1["paths"], json!(["/data/one.fcproj"]));
    // a local path is left to the command (the host answers false)
    d.call("ui.menu.invoke", json!({"id": "mediaBrowser.import", "params": {"paths": ["/local/a.mp4"]}}));
    assert_eq!(d.imports.borrow().len(), 3);
}
