//! Headless UI test of the community links: Help menu commands, the header Discord button, the
//! About dialog and the Home (Import) screen.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `links-*.png` there; without it no GPU is needed.

use crate::support::{Drive, Launch, builder, demo_session, driver, save_snapshot, with_snapshots};
use filmcraft_ui_egui::links;
use serde_json::json;

driver!(Driver);

impl Driver {
    fn demo() -> Self {
        Self::open(demo_session(), with_snapshots(builder()))
    }
    fn snapshot(&mut self, name: &str) {
        save_snapshot(self, 2, &format!("links-{name}.png"));
    }
}

#[test]
fn community_links_everywhere() {
    let mut d = Driver::demo();
    // Help menu: every link is a command that reports the URL it opened.
    for (id, _, url) in links::ALL {
        let r = d.ok("engine.execute", json!({"command": id, "params": {}}));
        assert_eq!(r["url"], json!(url), "{id}");
    }
    let menu = d.ok("ui.menu.list", json!({}));
    let text = menu.to_string();
    for (id, label, _) in links::ALL {
        assert!(text.contains(id) && text.contains(label), "Help menu lists {id}");
    }
    // Header: the Discord button is always there.
    assert_eq!(d.ids("header.discord"), vec!["header.discord".to_string()]);
    d.snapshot("header");
    // About dialog: Discord first, then website, app page, GitHub, issues.
    d.ok("engine.execute", json!({"command": "app.about", "params": {}}));
    d.frames(3);
    let about = d.ids("about.");
    for id in ["about.discord", "about.website", "about.appPage", "about.github", "about.reportIssue"] {
        assert!(about.iter().any(|a| a == id), "{id} in {about:?}");
    }
    d.snapshot("about");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    assert!(d.ids("about.").is_empty(), "Escape closes About");
    // Home screen (Import mode): community links.
    d.ok("engine.execute", json!({"command": "mode.import", "params": {}}));
    d.frames(3);
    let home = d.ids("import.link.");
    for id in ["import.link.discord", "import.link.website", "import.link.appPage", "import.link.github"] {
        assert!(home.iter().any(|a| a == id), "{id} in {home:?}");
    }
    d.snapshot("home");
}
