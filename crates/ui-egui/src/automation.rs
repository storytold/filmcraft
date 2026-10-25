//! Automation registry: every interactive element registers a stable id and its screen rect each
//! frame, so agents can click/drag by id (`ui.click {id:"timeline.track.V1.lock"}`) and
//! `ui.inspect` can report the full on-screen widget tree.

use egui::Rect;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct Element {
    pub id: String,
    pub label: String,
    pub rect: [f32; 4],
    /// Present only when this entry represents a real egui widget, rather than a click alias.
    #[serde(rename = "widgetId", skip_serializing_if = "Option::is_none")]
    pub widget_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focusable: Option<bool>,
    #[serde(skip)]
    pub(crate) response_id: Option<egui::Id>,
}

#[derive(Default)]
pub struct Registry {
    pub elements: Vec<Element>,
    /// Last frame's elements (for lookups from control handlers between frames).
    pub previous: Vec<Element>,
}

impl Registry {
    pub fn add(&mut self, id: &str, rect: Rect, label: &str) {
        self.elements.push(Element {
            id: id.to_string(),
            label: label.to_string(),
            rect: [rect.min.x, rect.min.y, rect.width(), rect.height()],
            widget_id: None,
            enabled: None,
            focusable: None,
            response_id: None,
        });
    }
    /// Observe the widget without changing its accessibility role or input behavior.
    pub fn add_response(&mut self, id: &str, response: &egui::Response, label: &str) {
        // The automation label is also the name a screen reader and kittest use. Keep an
        // existing visible label/placeholder; this only fills a missing name.
        crate::widgets::accessible_name(response.clone(), label);
        self.add(id, response.rect, label);
        if let Some(element) = self.elements.last_mut() {
            element.widget_id = Some(format!("{:016x}", response.id.value()));
            element.enabled = Some(response.enabled());
            element.focusable = Some(response.sense.is_focusable());
            element.response_id = Some(response.id);
        }
    }
    pub fn begin_frame(&mut self) {
        self.previous = std::mem::take(&mut self.elements);
    }
    /// The entries of the last accepted UI pass, or the pass before it while the current pass is
    /// still being laid out (pass 0 clears `elements`; `drain_ui_requests` runs after the layout).
    pub fn all(&self) -> &[Element] {
        if self.elements.is_empty() { &self.previous } else { &self.elements }
    }
    /// A widget drawn in the current accepted pass. Unlike [`Self::find`] this never falls back to
    /// the previous pass, for callers that need fresh geometry (e.g. a screenshot crop).
    pub fn find_current(&self, id: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.id == id)
    }
    /// Look up a widget, preferring the layout just drawn. The previous pass is only used before
    /// this pass has registered anything (input handling runs before the panels draw).
    pub fn find(&self, id: &str) -> Option<&Element> {
        self.all().iter().find(|e| e.id == id)
    }
    /// Elements whose id starts with a prefix, from the current accepted pass when there is one.
    pub fn query(&self, prefix: &str) -> Vec<&Element> {
        self.all().iter().filter(|e| e.id.starts_with(prefix)).collect()
    }
}

#[cfg(test)]
mod tests {
    use egui::{Rect, pos2, vec2};

    use super::Registry;

    /// Test helper: a 10×10 rect at `x` for pass-order fixtures.
    fn rect(x: f32) -> Rect {
        Rect::from_min_size(pos2(x, 0.0), vec2(10.0, 10.0))
    }

    /// Lookups and prefix queries must resolve the pass just drawn, not stale geometry.
    #[test]
    fn lookups_prefer_the_current_accepted_pass() {
        let mut registry = Registry::default();
        registry.add("panel.project", rect(1.0), "old");
        registry.begin_frame();
        registry.add("panel.project", rect(200.0), "new");
        assert_eq!(registry.find("panel.project").map(|e| e.rect[0]), Some(200.0));
        assert_eq!(registry.query("panel.").iter().map(|e| e.rect[0]).collect::<Vec<_>>(), vec![200.0]);
        assert_eq!(registry.find_current("panel.project").map(|e| e.rect[0]), Some(200.0));
    }

    /// Once the current pass registers an element, a removed id must not resolve from the previous pass.
    #[test]
    fn removed_elements_do_not_resolve_from_a_populated_current_pass() {
        let mut registry = Registry::default();
        registry.add("dialog.closed", rect(1.0), "old control");
        registry.begin_frame();
        assert!(registry.find("dialog.closed").is_some(), "before drawing, the previous pass remains usable");
        registry.add("panel.current", rect(200.0), "current control");
        assert!(registry.find("dialog.closed").is_none(), "a closed control cannot target stale coordinates");
        assert_eq!(registry.find("panel.current").map(|e| e.rect[0]), Some(200.0));
        assert!(registry.query("dialog.").is_empty());
    }

    /// The previous pass is usable only before the current pass draws; a discarded pass leaves exactly one entry.
    #[test]
    fn the_previous_pass_is_only_a_fallback_before_this_pass_draws() {
        let mut registry = Registry::default();
        registry.add("panel.project", rect(1.0), "old");
        registry.begin_frame();
        assert_eq!(registry.find("panel.project").map(|e| e.rect[0]), Some(1.0));
        assert_eq!(registry.query("panel.").len(), 1);
        assert!(registry.find_current("panel.project").is_none());

        // A discarded pass is cleared and redrawn, so exactly one entry per widget remains.
        registry.add("panel.project", rect(2.0), "discarded");
        registry.elements.clear();
        registry.add("panel.project", rect(3.0), "accepted");
        assert_eq!(registry.elements.len(), 1);
        assert_eq!(registry.find("panel.project").map(|e| e.rect[0]), Some(3.0));
    }
}
