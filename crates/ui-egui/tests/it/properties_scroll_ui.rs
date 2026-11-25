//! The Properties panel scrolls (#1322): with a short panel the Audio section and the speed button
//! sit below the fold, and the mouse wheel has to bring them into view.

use crate::support::{Drive, Launch, builder, demo_session, driver};
use serde_json::json;

driver!(Driver);

impl Driver {
    fn top(&mut self, id: &str) -> Option<f64> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array()?.iter().find(|e| e["id"] == id)?;
        e["rect"].as_array()?.get(1)?.as_f64()
    }
}

#[test]
fn properties_panel_scrolls_to_its_lower_sections() {
    let mut d = Driver::open(demo_session(), builder().with_size(egui::vec2(1600.0, 560.0)));
    let seq = d.ok("engine.execute", json!({"command": "sequence.inspect", "params": {}}));
    let clip = seq["video"][0]["items"][0]["clip"].as_u64().expect("first video clip");
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [clip]}}));
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.frames(3);
    let before = d.top("properties.speed").expect("the speed button is laid out");
    d.ok("ui.scroll", json!({"id": "panel.Properties", "dy": -2000.0}));
    d.frames(3);
    let after = d.top("properties.speed").expect("the speed button is still laid out");
    assert!(after < before - 20.0, "the wheel scrolls the panel: {before} -> {after}");
}
