//! The life of a plug-in in a FilmCraft session wired the way `apps/filmcraft/src/main.rs` wires it.
//! The plug-in is the `edits` WAT guest of `tests/common/guests.rs`.
//!
//! The wiring matters. `craft.link` registers native dynamic commands. The plug-in host lends the
//! session to a guest call only (`Services::lend_session`), so those commands keep the real session.
//! A rig that skips the link install would never notice if that broke.

mod common;

use std::path::PathBuf;

use craft_plugin_host::lifecycle::PluginState;
use filmcraft_engine::Session;
use filmcraft_link::record::{Format, LinkRecord};
use filmcraft_plugins::{HostConfig, PluginHost};
use filmcraft_project::ItemId;
use serde_json::{Map, json};

use common::guests::{EDITS, edits_package_version};

const LINK_ID: &str = "0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d";

/// A temp directory that is removed when it drops.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("fc-lifecycle-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What `main.rs` builds: the link commands first, then the plug-in host over a plug-in directory.
fn app_session(store: &Scratch) -> (Session, std::sync::Arc<PluginHost>) {
    let mut session = Session::default();
    filmcraft_link::commands::install(&mut session);
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let cfg = HostConfig { store_dir: Some(store.0.join("Plug-ins")), ..HostConfig::default() };
    let host = PluginHost::start(&mut session, cfg).expect("plug-in host");
    (session, host)
}

/// A package file in the scratch directory.
fn package_file(store: &Scratch, version: &str, label_suffix: &str) -> PathBuf {
    let path = store.0.join(format!("edits-{version}.wasm"));
    std::fs::write(&path, edits_package_version(version, label_suffix)).expect("write the package");
    path
}

/// The label the registry shows for `id`.
fn label_of(session: &Session, id: &str) -> Option<String> {
    let mut found = None;
    session.registry.for_each(|view| {
        if view.id == id {
            found = Some(view.label.to_string());
        }
    });
    found
}

fn offline_link(session: &mut Session) -> ItemId {
    let made = session
        .execute(
            "file.newOfflineFile",
            json!({"name": "Result", "fileName": "/nonexistent/fc-lifecycle/result.mov", "video": true, "audio": false, "width": 160, "height": 90, "fps": 24.0, "seconds": 1.0}),
        )
        .expect("offline item");
    let item = ItemId(made["item"].as_u64().expect("item id"));
    let record = LinkRecord {
        v: 1,
        link_id: LINK_ID.to_string(),
        target_app: "effectcraft".to_string(),
        target_project: "/nonexistent/ec.ecproj".to_string(),
        target_comp: "c".to_string(),
        nested_sequence: "7".to_string(),
        result_path: "/nonexistent/fc-lifecycle/result.mov".to_string(),
        result_epoch: 0,
        format: Format { w: 160, h: 90, rate_num: 24, rate_den: 1, duration_ticks: 24, alpha: false, extra: Map::new() },
        created: "2026-10-24T00:00:00Z".to_string(),
        extra: Map::new(),
    };
    session
        .project_mut()
        .item_mut(item)
        .expect("the new item")
        .metadata
        .insert(filmcraft_link::link_ops::LINK_KEY.to_string(), serde_json::to_string(&record).expect("record json"));
    item
}

/// `plugin.craft.link.refresh` must find the item the link record sits on. Run while a plug-in is
/// installed, so the host's session scope is live.
fn assert_link_answers(session: &mut Session, when: &str) {
    let reply =
        session.execute("plugin.craft.link.refresh", json!({"link_id": LINK_ID, "epoch": 1})).unwrap_or_else(|e| panic!("craft.link.refresh {when}: {e}"));
    assert_eq!(reply["link_id"], LINK_ID, "{when}: {reply}");
}

#[test]
fn the_plugin_host_leaves_the_link_commands_the_real_session() {
    let store = Scratch::new("wiring");
    let (mut session, _host) = app_session(&store);
    let _ = offline_link(&mut session);
    assert_link_answers(&mut session, "right after the host started");
}

#[test]
fn a_plugin_lives_a_whole_life_next_to_craft_ipc() {
    let store = Scratch::new("life");
    let (mut session, host) = app_session(&store);
    let (v1, v2) = (package_file(&store, "1.0.0", ""), package_file(&store, "1.0.1", " (v2)"));
    let ok = format!("plugin.{EDITS}.ok");
    let link_item = offline_link(&mut session);
    assert_link_answers(&mut session, "before any plug-in");

    // Install: the prompt comes first, nothing runs before the answer, then the command is usable.
    let r = session.execute("plugin.app.filmcraft.install", json!({"path": v1})).expect("install");
    assert_eq!(r["outcome"], json!("needs-grant"), "{r}");
    assert_eq!(r["required"], json!(["commands.run", "doc.read"]));
    assert!(session.registry.resolve(&ok).is_none(), "nothing runs before the answer");
    assert_eq!(host.waiting_prompts(), vec![EDITS.to_string()]);
    session.execute("plugin.app.filmcraft.grant", json!({"id": EDITS})).expect("grant");
    assert!(matches!(host.manager.state(EDITS), Some(PluginState::Active { generation: 1 })));

    // A plug-in command runs through Session::execute, in the session scope, and edits the document.
    let steps = session.history.undo.len();
    let done = session.execute(&ok, json!({})).expect("the plug-in command");
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(session.history.undo.len(), steps + 1, "the plug-in's edit is one undo step");
    assert_eq!(label_of(&session, &ok).as_deref(), Some("ok"));

    // The link commands still answer, with a guest installed and the scope live.
    assert_link_answers(&mut session, "with a plug-in installed");
    let status = session.execute("media.status", json!({"item": link_item.0})).expect("media.status");
    assert_eq!(status[0]["status"], "offline", "{status}");

    // Hot reload to v2: no prompt, same ids, new label.
    let r = session.execute("plugin.app.filmcraft.install", json!({"path": v2})).expect("update");
    assert_eq!(r["outcome"], json!("loaded"), "{r}");
    session.execute(&ok, json!({})).expect("the command runs on v2");
    assert!(matches!(host.manager.state(EDITS), Some(PluginState::Active { generation: 2 })));
    assert_eq!(label_of(&session, &ok).as_deref(), Some("ok (v2)"));
    assert_link_answers(&mut session, "after a hot reload");

    // Disable takes the commands away. Enable brings them back.
    session.execute("plugin.app.filmcraft.disable", json!({"id": EDITS})).expect("disable");
    assert!(session.registry.resolve(&ok).is_none(), "disable removes the command");
    assert_link_answers(&mut session, "with the plug-in disabled");
    session.execute("plugin.app.filmcraft.enable", json!({"id": EDITS})).expect("enable");
    assert!(session.registry.resolve(&ok).is_some());

    // Uninstall.
    session.execute("plugin.app.filmcraft.remove", json!({"id": EDITS})).expect("remove");
    assert!(session.registry.resolve(&ok).is_none() && host.manager.info(EDITS).is_none());
    assert_link_answers(&mut session, "after the plug-in is gone");
    assert!(session.registry.resolve("plugin.craft.link.focus").is_some() && session.registry.resolve("plugin.craft.link.unlink").is_some());
}
