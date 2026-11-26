//! The Properties panel edits the selected caption (#1846): an imported SRT caption picked in the
//! timeline shows its text and in/out times there, and editing the text changes the caption.

use crate::support::{Drive, Launch, builder, demo_session, driver};
use serde_json::{Value, json};

driver!(Driver);

impl Driver {
    fn ids(&mut self, prefix: &str) -> Vec<String> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().expect("elements").iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn captions(&mut self) -> Vec<Value> {
        let r = self.exec("captions.list", json!({}));
        r["tracks"][0]["captions"].as_array().expect("captions").clone()
    }
}

#[test]
fn properties_panel_edits_the_selected_caption() {
    let dir = std::env::temp_dir().join(format!("filmcraft-caption-props-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let srt = dir.join("two.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,500\nFirst line\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond line\n").expect("write srt");

    let mut d = Driver::open(demo_session(), builder());
    d.exec("captions.import", json!({"path": srt.to_string_lossy()}));
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "{caps:?}");
    let first = caps[0]["id"].as_u64().expect("caption id");

    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.ok("ui.click", json!({"id": format!("timeline.caption.{first}")}));
    d.frames(3);
    assert_eq!(d.harness.state().session.state.caption_selection.iter().map(|c| c.0).collect::<Vec<_>>(), vec![first]);
    let shown = d.ids("properties.caption.");
    for id in ["properties.caption.text", "properties.caption.in", "properties.caption.out"] {
        assert!(shown.iter().any(|s| s == id), "{id} missing from Properties: {shown:?}");
    }

    // the field holds the caption's text: typing at its end extends it
    d.ok("ui.click", json!({"id": "properties.caption.text"}));
    d.ok("ui.key", json!({"key": "End"}));
    d.ok("ui.type", json!({"text": " edited"}));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    let caps = d.captions();
    assert_eq!(caps[0]["text"], "First line edited", "{caps:?}");
    assert_eq!(caps[1]["text"], "Second line", "{caps:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
