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
}

#[derive(Default)]
pub struct Registry {
    /// This frame's elements, as they are drawn.
    pub elements: Vec<Element>,
    /// The frame before's.
    pub previous: Vec<Element>,
    /// The one before that: an element only takes clicks once it has stayed put for two frames.
    older: Vec<Element>,
    /// Index in `elements` where this frame's dialog windows start (see [`Registry::mark_dialogs`]).
    dialogs_from: Option<usize>,
    previous_dialogs_from: Option<usize>,
    /// `elements` is complete: between frames, where control requests are handled.
    done: bool,
}

impl Registry {
    pub fn add(&mut self, id: &str, rect: Rect, label: &str) {
        self.elements.push(Element { id: id.to_string(), label: label.to_string(), rect: [rect.min.x, rect.min.y, rect.width(), rect.height()] });
    }
    pub fn begin_frame(&mut self) {
        self.older = std::mem::replace(&mut self.previous, std::mem::take(&mut self.elements));
        self.previous_dialogs_from = self.dialogs_from.take();
        self.done = false;
    }
    /// The frame is drawn: lookups from now on see it.
    pub fn end_frame(&mut self) {
        self.done = true;
    }
    /// Everything registered from here on this frame is drawn in a dialog window, above the panels.
    pub fn mark_dialogs(&mut self) {
        self.dialogs_from = Some(self.elements.len());
    }
    /// The last complete frame's elements (and where its dialogs start), and the frame before's.
    fn latest(&self) -> (&[Element], Option<usize>, &[Element]) {
        match self.done {
            true => (&self.elements, self.dialogs_from, &self.previous),
            false => (&self.previous, self.previous_dialogs_from, &self.older),
        }
    }
    /// `id` in the last complete frame or, mid-frame, among the elements drawn so far.
    pub fn find(&self, id: &str) -> Option<&Element> {
        let (latest, _, _) = self.latest();
        let drawing: &[Element] = if self.done { &[] } else { &self.elements };
        latest.iter().chain(drawing.iter()).find(|e| e.id == id)
    }
    /// `id` when it is ready for a click: drawn in each of the last two frames at the same place.
    /// A popup's first frame is an invisible sizing pass that takes no clicks, and a panel that
    /// just appeared may still be laying out; a click aimed at either lands on something else.
    pub fn settled(&self, id: &str) -> Option<&Element> {
        let (latest, _, before) = self.latest();
        let e = latest.iter().find(|e| e.id == id)?;
        before.iter().find(|e| e.id == id).filter(|o| o.rect == e.rect).map(|_| e)
    }
    /// Whether `id` is drawn in a dialog window (last complete frame).
    pub fn in_dialog(&self, id: &str) -> bool {
        self.dialog_elements().iter().any(|e| e.id == id)
    }
    /// The last complete frame's elements drawn in dialog windows.
    pub fn dialog_elements(&self) -> &[Element] {
        let (latest, from, _) = self.latest();
        from.and_then(|f| latest.get(f..)).unwrap_or_default()
    }
    /// The last complete frame's elements whose id starts with a prefix.
    pub fn query(&self, prefix: &str) -> Vec<&Element> {
        self.latest().0.iter().filter(|e| e.id.starts_with(prefix)).collect()
    }
}
