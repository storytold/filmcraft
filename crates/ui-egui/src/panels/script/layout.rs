//! Shared row measurements and candle geometry; painting and hit targets use the same coordinates.

use super::*;

pub(super) fn thumbnail_width(state: &ScriptUi) -> f32 {
    state.thumbnail_width.unwrap_or(DEFAULT_THUMB_W).clamp(MIN_THUMB_W, MAX_THUMB_W)
}

pub(super) fn thumbnail_height(width: f32) -> f32 {
    width * 9.0 / 16.0 + THUMB_CHIN_H
}

/// Measured once per document identity and view settings; a 4 MiB script may have 50,000 rows, so only
/// visible rows are painted while their cached prefix heights keep the candles in exact alignment.
pub(super) fn row_layout(app: &mut FilmcraftApp, ui: &egui::Ui, doc: &Arc<ScriptDocument>, left_w: f32, text_inset: f32) {
    let width = left_w.round().max(1.0) as u32;
    let inset = text_inset.round().max(1.0) as u32;
    let script_font_size = app.ui.script.font_size.unwrap_or(SCRIPT_FONT_SIZE).clamp(10.0, 24.0);
    let font_size = script_font_size.to_bits();
    let thumb_w = thumbnail_width(&app.ui.script);
    let thumb_width = thumb_w.round() as u32;
    if app.ui.script.layout_item == app.ui.script.item
        && app.ui.script.layout_doc.as_ref().is_some_and(|cached| Arc::ptr_eq(cached, doc))
        && app.ui.script.layout_width == width
        && app.ui.script.layout_inset == inset
        && app.ui.script.layout_font_size == font_size
        && app.ui.script.layout_thumbnail_width == thumb_width
        && app.ui.script.row_tops.len() == doc.lines.len().saturating_add(1)
    {
        return;
    }
    let mut tops = Vec::with_capacity(doc.lines.len().saturating_add(1));
    let leading_blank = doc.lines.iter().take(5).take_while(|line| line.text.is_empty()).count();
    let clearance = thumbnail_height(thumb_w) + TAKE_HEADER_H + 10.0;
    let top_space = if leading_blank >= 3 { (clearance - leading_blank as f32 * MIN_ROW_H).max(8.0) } else { clearance };
    tops.push(top_space);
    let mut total = top_space;
    let font = Tokens::ui(script_font_size);
    let visual_line_height = ui.painter().layout_no_wrap("Mg".to_owned(), font.clone(), app.tokens.text).size().y;
    let max_height = (visual_line_height * MAX_VISUAL_LINES + 4.0).max(MIN_ROW_H);
    for line in &doc.lines {
        let text_w = (left_w - text_inset).max(48.0);
        // Measure the rendered font instead of assigning a fixed row height.
        // The cap also scales with the font, preserving up to 12 visible lines.
        let galley = ui.painter().layout(line.text.clone(), font.clone(), app.tokens.text, text_w);
        let height = (galley.size().y + 4.0).clamp(MIN_ROW_H, max_height);
        total += height;
        tops.push(total);
    }
    app.ui.script.row_tops = tops;
    app.ui.script.layout_item = app.ui.script.item;
    app.ui.script.layout_doc = Some(Arc::clone(doc));
    app.ui.script.layout_width = width;
    app.ui.script.layout_inset = inset;
    app.ui.script.layout_font_size = font_size;
    app.ui.script.layout_thumbnail_width = thumb_width;
}

pub(super) fn first_visible(tops: &[f32], y: f32) -> usize {
    tops.partition_point(|top| *top <= y).saturating_sub(1)
}

/// The inclusive visible coverage, including a live endpoint-drag preview.
pub(super) fn coverage_rows(state: &ScriptUi, take: &ScriptTake, indices: &HashMap<u64, usize>) -> Option<(usize, usize)> {
    let (&start, &end) = indices.get(&take.start_line.0).zip(indices.get(&take.end_line.0))?;
    Some(match (state.drag_endpoint, state.drag_target) {
        (Some((id, true)), Some(target)) if id == take.id.0 => (target.min(end), end),
        (Some((id, false)), Some(target)) if id == take.id.0 => (start, target.max(start)),
        _ => (start, end),
    })
}

pub(super) fn candle_row_bounds(row: Rect) -> (f32, f32) {
    (row.top(), row.bottom())
}
