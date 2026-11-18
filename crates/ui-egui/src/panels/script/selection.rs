//! Document-local selection and editing state. Project commands own persisted edits.

use std::sync::Arc;

use super::{ScriptDocument, ScriptLine, ScriptTake, ScriptUi};

impl ScriptUi {
    /// A new project can reuse the same item IDs. Its edits and take links need a fresh owner.
    pub(crate) fn sync_project(&mut self, project: &Arc<()>) {
        if let Some(previous) = &self.project {
            if Arc::ptr_eq(previous, project) {
                return;
            }
            self.open_document(None);
            self.section_clipboard = None;
        }
        self.project = Some(project.clone());
    }

    /// IDs belong to one script. Retain view preferences and the cross-script clipboard,
    /// but never carry an edit, drag or replacement confirmation into another document.
    pub(crate) fn open_document(&mut self, item: Option<u64>) {
        if self.item == item {
            return;
        }
        let previous = std::mem::take(self);
        *self = Self {
            item,
            project: previous.project,
            paint_color: previous.paint_color,
            section_clipboard: previous.section_clipboard,
            text_column_width: previous.text_column_width,
            text_inset: previous.text_inset,
            font_size: previous.font_size,
            thumbnail_width: previous.thumbnail_width,
            show_row_colors: previous.show_row_colors,
            show_line_numbers: previous.show_line_numbers,
            search: previous.search,
            ..Self::default()
        };
    }

    pub(super) fn clear_object_selection(&mut self) {
        self.selected_mark = None;
        self.selected_take = None;
        self.selected_slate = None;
        self.selected_thumbnail = None;
        self.selected_section_slates.clear();
    }
}

pub(super) fn selected_range(doc: &ScriptDocument, pair: Option<(u64, u64)>) -> Option<(usize, usize)> {
    let (a, b) = pair?;
    let a = doc.lines.iter().position(|line| line.id.0 == a)?;
    let b = doc.lines.iter().position(|line| line.id.0 == b)?;
    Some((a.min(b), a.max(b)))
}

pub(super) fn take_by_id(doc: &ScriptDocument, id: Option<u64>) -> Option<&ScriptTake> {
    let id = id?;
    doc.slates.iter().flat_map(|slate| &slate.takes).find(|take| take.id.0 == id)
}

pub(super) fn current_line<'a>(doc: &'a ScriptDocument, state: &ScriptUi) -> Option<&'a ScriptLine> {
    let id = state.selected_lines?.1;
    doc.lines.iter().find(|line| line.id.0 == id)
}

pub(super) fn select_line(state: &mut ScriptUi, line: &ScriptLine, extend: bool) {
    state.selected_mark = None;
    if state.selected_lines.is_none_or(|pair| pair.1 != line.id.0) {
        state.edit_scene = None;
        state.edit_page = None;
    }
    state.selected_lines = Some(if extend { (state.selected_lines.map_or(line.id.0, |pair| pair.0), line.id.0) } else { (line.id.0, line.id.0) });
    if state.label_line != Some(line.id.0) {
        state.label_line = Some(line.id.0);
        state.scene_text = line.scene.clone().unwrap_or_default();
        state.page_text = line.page.clone().unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_selection_and_linked_clipboard_belong_to_the_current_project() {
        let first = Arc::new(());
        let second = Arc::new(());
        let mut state = ScriptUi::default();
        state.sync_project(&first);
        state.open_document(Some(1));
        state.selected_take = Some(2);
        state.section_clipboard = Some(serde_json::json!({"lines":[],"slates":[]}));
        state.sync_project(&first);
        assert_eq!(state.selected_take, Some(2));
        assert!(state.section_clipboard.is_some());
        state.sync_project(&second);
        state.open_document(Some(1));
        assert_eq!(state.selected_take, None);
        assert!(state.section_clipboard.is_none());
    }
}
