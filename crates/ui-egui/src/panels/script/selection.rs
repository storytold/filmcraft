//! Document-local selection and editing state. Project commands own persisted edits.

use super::*;

impl ScriptUi {
    /// IDs belong to one script. Retain view preferences and the cross-script clipboard,
    /// but never carry an edit, drag or replacement confirmation into another document.
    pub(crate) fn open_document(&mut self, item: Option<u64>) {
        if self.item == item {
            return;
        }
        let previous = std::mem::take(self);
        *self = Self {
            item,
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
