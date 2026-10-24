//! Synchronized script rows, setup thumbnails, candles, and mark interactions.

use super::*;

struct SlateHeader<'a> {
    id: u64,
    first_column: usize,
    center_x: f32,
    start_index: usize,
    source: Option<ItemId>,
    source_time: Option<Tick>,
    source_name: &'a str,
    last_take: &'a ScriptTake,
}

pub(super) fn content(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, doc: &Arc<ScriptDocument>, actions: &mut Vec<Action>) {
    let t = app.tokens;
    let thumb_w = thumbnail_width(&app.ui.script);
    let min_left = 180.0_f32.min((rect.width() - 85.0).max(100.0));
    let max_left = (rect.width() - 85.0).max(min_left);
    let default_left = (rect.width() * if rect.width() < 520.0 { 0.63 } else { 0.49 }).clamp(min_left, 490.0_f32.max(min_left)).min(max_left);
    let left_w = app.ui.script.text_column_width.unwrap_or(default_left).clamp(min_left, max_left);
    let min_inset = 54.0_f32.min((left_w - 48.0).max(42.0));
    let max_inset = (left_w - 48.0).max(min_inset);
    let text_inset = app.ui.script.text_inset.unwrap_or(DEFAULT_TEXT_INSET).clamp(min_inset, max_inset);
    row_layout(app, ui, doc, left_w, text_inset);
    let line_indices: HashMap<u64, usize> = doc.lines.iter().enumerate().map(|(index, line)| (line.id.0, index)).collect();
    // A coverage trim keeps annotations in the model, but a previously selected or
    // dragged mark must stop being interactive as soon as its row is hidden.
    let visible_mark = |take_id: u64, mark_id: u64| {
        doc.take(filmcraft_project::ScriptTakeId(take_id)).is_some_and(|take| {
            let Some((&start, &end)) = line_indices.get(&take.start_line.0).zip(line_indices.get(&take.end_line.0)) else { return false };
            take.marks.iter().any(|mark| mark.id.0 == mark_id && line_indices.get(&mark.line.0).is_some_and(|row| (start..=end).contains(row)))
        })
    };
    if app.ui.script.selected_mark.is_some_and(|mark| app.ui.script.selected_take.is_none_or(|take| !visible_mark(take, mark))) {
        app.ui.script.selected_mark = None;
    }
    if app.ui.script.drag_mark.is_some_and(|(take, mark)| !visible_mark(take, mark)) {
        app.ui.script.drag_mark = None;
        app.ui.script.drag_mark_target = None;
    }
    // Script text, setup thumbnails and candles share one canvas. The wrap margin only
    // controls text layout; it is not a clipping boundary for setups.
    let canvas = rect;
    let columns: Vec<_> = doc
        .slates
        .iter()
        .flat_map(|slate| {
            if slate.takes.is_empty() {
                vec![(slate.id.0, None, slate.name.as_str())]
            } else {
                slate.takes.iter().map(|take| (slate.id.0, Some(take), slate.name.as_str())).collect()
            }
        })
        .collect();
    // Keep setup placement independent of the two text margin markers. The
    // default happens to start near the initial wrap edge, but moving that edge
    // must only reflow text, never slide thumbnails or candles horizontally.
    // Reserve a small page-number gutter between the initial text margin and
    // the first setup. Both stay anchored if the user later moves a margin.
    let take_origin = default_left + 70.0;
    let mut first_column = 0;
    let mut next_group_x = 0.0;
    let mut column_centers = Vec::with_capacity(columns.len());
    let mut slate_headers = Vec::new();
    for slate in &doc.slates {
        let count = slate.takes.len().max(1);
        let group_width = (count as f32 * TAKE_W).max(thumb_w + 8.0);
        let preview_x = if app.ui.script.drag_slate == Some(slate.id.0) { app.ui.script.drag_slate_delta.x } else { 0.0 };
        let center_x = slate.thumbnail_x.map_or(take_origin + next_group_x + group_width * 0.5, |x| x * canvas.width()) + preview_x;
        let candle_left = center_x - count as f32 * TAKE_W * 0.5;
        for column in 0..count {
            column_centers.push(candle_left + (column as f32 + 0.5) * TAKE_W);
        }
        if let (Some(last_take), Some(first_candle_index)) = (
            slate.takes.last(),
            slate
                .takes
                .iter()
                .filter_map(|take| {
                    let preview = if app.ui.script.drag_endpoint == Some((take.id.0, true)) {
                        app.ui.script.drag_target.and_then(|index| doc.lines.get(index)).map_or(take.start_line, |line| line.id)
                    } else {
                        take.start_line
                    };
                    line_indices.get(&preview.0).copied()
                })
                .min(),
        ) {
            let start_index = slate.thumbnail_line.and_then(|line| line_indices.get(&line.0).copied()).unwrap_or(first_candle_index);
            let display_take = slate.takes.iter().find(|take| app.ui.script.selected_take == Some(take.id.0)).or_else(|| slate.takes.first());
            slate_headers.push(SlateHeader {
                id: slate.id.0,
                first_column,
                center_x,
                start_index,
                source: display_take.and_then(|take| take.source),
                source_time: display_take.and_then(|take| take.thumbnail_time.or_else(|| doc.first_visible_mark_time(take))),
                source_name: display_take.map_or("", |take| take.source_name.as_str()),
                last_take,
            });
        }
        first_column += count;
        next_group_x += group_width + SLATE_GAP;
    }
    let take_total = (take_origin + next_group_x - SLATE_GAP).max(0.0);
    let max_scroll = (take_total - canvas.width()).max(0.0);
    app.ui.script.take_scroll = app.ui.script.take_scroll.clamp(0.0, max_scroll);
    let canvas = Rect::from_min_max(pos2(rect.min.x, rect.min.y + 16.0), rect.max);
    let body = if max_scroll > 0.0 { Rect::from_min_max(canvas.min, pos2(canvas.max.x, canvas.max.y - 22.0)) } else { canvas };
    let slider_r = Rect::from_min_max(pos2(canvas.min.x + 8.0, body.max.y + 2.0), pos2(canvas.max.x - 8.0, canvas.max.y - 2.0));
    if slider_r.width() > 48.0 && max_scroll > 0.0 {
        let response = ui.put(slider_r, egui::Slider::new(&mut app.ui.script.take_scroll, 0.0..=max_scroll).show_value(false));
        app.auto.add("script.takeScroll", response.rect, "Scroll take columns");
    }
    margin_ruler(app, ui, Rect::from_min_max(rect.min, pos2(rect.max.x, canvas.min.y)), left_w, text_inset, min_left, max_left, min_inset, max_inset);
    if body.height() < MIN_ROW_H {
        return;
    }
    let (pointer, alt_pressed, primary_down, released) = ui.input(|input| {
        (input.pointer.hover_pos(), input.modifiers.alt && input.pointer.primary_pressed(), input.pointer.primary_down(), input.pointer.any_released())
    });
    if alt_pressed && let Some(pointer) = pointer.filter(|pointer| body.contains(*pointer)) {
        app.ui.script.lasso_start = Some(pointer);
        app.ui.script.lasso_end = Some(pointer);
    }
    if app.ui.script.lasso_start.is_some()
        && primary_down
        && let Some(pointer) = pointer
    {
        app.ui.script.lasso_end = Some(pointer);
        ui.ctx().request_repaint();
    }
    let empty_canvas = ui.interact(body, egui::Id::new("script.emptyCanvas"), Sense::click());
    app.auto.add("script.emptyCanvas", empty_canvas.rect, "Clear script selection");
    let mut scroll_ui = ui.new_child(egui::UiBuilder::new().max_rect(body));
    let project_drag = crate::panels::dragged_project_item(ui).is_some();
    let mut canvas_drop = None;
    let mut clicked_mark = None;
    let mut clicked_candle = None;
    let mut foreground_click = false;
    let mut take_click = false;
    let selecting_section = app.ui.script.lasso_start.is_some();
    let mut scroll_origin = body.min.y;
    let range = selected_range(doc, app.ui.script.selected_lines);
    let tops = app.ui.script.row_tops.clone();
    egui::ScrollArea::vertical().id_salt("script.rows").auto_shrink([false, false]).show_viewport(&mut scroll_ui, |ui, viewport| {
        let total = tops.last().copied().unwrap_or_default();
        ui.set_height(total);
        let base_y = ui.max_rect().top();
        scroll_origin = base_y;
        let start = first_visible(&tops, viewport.min.y).min(doc.lines.len());
        let end = first_visible(&tops, viewport.max.y).saturating_add(2).min(doc.lines.len());
        // A Project drag belongs to its source widget, so ordinary widget hover is
        // suppressed. Hit-test the whole canvas, including the open take columns.
        let project_drop_row = pointer
            .filter(|_| project_drag && ui.rect_contains_pointer(body) && !doc.lines.is_empty())
            .map(|pointer| first_visible(&tops, (pointer.y - base_y).max(0.0)).min(doc.lines.len() - 1));
        let project_drop_range = project_drop_row.map(|index| range.unwrap_or((index, index)));
        if let Some((a, b)) = project_drop_range
            && let Some((&y0, &y1)) = tops.get(a).zip(tops.get(b + 1))
        {
            let preview = Rect::from_min_max(pos2(body.left(), base_y + y0), pos2(body.right(), base_y + y1)).intersect(body);
            if preview.is_positive() {
                ui.painter().rect_filled(preview, 0.0, t.accent.gamma_multiply(0.18));
                ui.painter().rect_stroke(preview, 0.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
                app.auto.add("script.dropPreview", preview, "Drop clip on these script lines");
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Copy);
            if released && let Some(source) = crate::panels::dragged_project_item(ui) {
                canvas_drop = Some((source, a, b));
                // Selected rows anchor the new setup even when the pointer is over
                // another row, a candle, or a floating thumbnail.
                if range.is_some() {
                    crate::panels::clear_drag(ui);
                }
            }
        }
        let drag_hover_row =
            app.ui.script.drag_mark.and_then(|_| ui.input(|input| input.pointer.hover_pos())).filter(|pointer| body.contains(*pointer)).and_then(|pointer| {
                let row = first_visible(&tops, (pointer.y - base_y).max(0.0)).min(doc.lines.len().saturating_sub(1));
                let (take_id, _) = app.ui.script.drag_mark?;
                let take = doc.slates.iter().flat_map(|slate| &slate.takes).find(|take| take.id.0 == take_id)?;
                let (&start, &end) = line_indices.get(&take.start_line.0).zip(line_indices.get(&take.end_line.0))?;
                Some(row.clamp(start, end))
            });
        if app.ui.script.drag_mark.is_some() {
            if let Some(target) = drag_hover_row {
                app.ui.script.drag_mark_target = Some(target);
            }
            ui.ctx().request_repaint();
        }
        for index in start..end {
            let (Some(&y0), Some(&y1)) = (tops.get(index), tops.get(index + 1)) else { continue };
            let row = Rect::from_min_max(pos2(body.left(), base_y + y0), pos2(body.right(), base_y + y1));
            if app.ui.script.show_row_colors.unwrap_or(true) && index % 2 == 1 {
                let stripe = if t.field_bg.r() < 128 { Color32::from_white_alpha(4) } else { Color32::from_black_alpha(6) };
                ui.painter().rect_filled(row.intersect(body), 0.0, stripe);
            }
            if range.is_some_and(|(a, b)| (a..=b).contains(&index)) {
                ui.painter().rect_filled(row.intersect(body), 0.0, t.hover.gamma_multiply(0.7));
            }
            if drag_hover_row == Some(index) {
                ui.painter().rect_filled(row.intersect(body), 0.0, t.hover.gamma_multiply(0.9));
            }
        }
        // Each take has its own connector from its number box to its candle.
        // During a drag, use the stepped preview rows so every connector stays attached.
        for header in &slate_headers {
            let Some(slate) = doc.slates.iter().find(|slate| slate.id.0 == header.id) else { continue };
            let shown_index =
                if app.ui.script.drag_slate == Some(header.id) { app.ui.script.drag_slate_target.unwrap_or(header.start_index) } else { header.start_index };
            let Some(&shown_top) = tops.get(shown_index).or_else(|| tops.get(header.start_index)) else { continue };
            let box_top = base_y + shown_top;
            let preview_y = if app.ui.script.drag_slate == Some(header.id) && app.ui.script.drag_slate_group { app.ui.script.drag_slate_delta.y } else { 0.0 };
            for (ordinal, take) in slate.takes.iter().enumerate() {
                let Some(&center) = column_centers.get(header.first_column + ordinal) else { continue };
                let Some(&stored_start) = line_indices.get(&take.start_line.0) else { continue };
                let take_start =
                    if app.ui.script.drag_endpoint == Some((take.id.0, true)) { app.ui.script.drag_target.unwrap_or(stored_start) } else { stored_start };
                let Some(&row_top) = tops.get(take_start) else { continue };
                let cx = body.min.x + center - app.ui.script.take_scroll;
                if cx < body.left() || cx > body.right() {
                    continue;
                }
                let box_bottom = take_metadata_rect(cx, box_top).bottom();
                let candle_top = base_y + row_top + preview_y + 16.0;
                if (box_bottom - candle_top).abs() > 1.0 {
                    ui.painter().with_clip_rect(body).line_segment([pos2(cx, box_bottom), pos2(cx, candle_top)], Stroke::new(1.0, t.text_dim));
                }
            }
        }
        for index in start..end {
            let (Some(line), Some(&y0), Some(&y1)) = (doc.lines.get(index), tops.get(index), tops.get(index + 1)) else { continue };
            let row = Rect::from_min_size(pos2(ui.max_rect().left(), base_y + y0), vec2(body.width().max(1.0), y1 - y0));
            if row.intersects(body) {
                row_view(
                    app,
                    ui,
                    row,
                    left_w,
                    text_inset,
                    index,
                    line,
                    &columns,
                    &column_centers,
                    &line_indices,
                    &tops,
                    &mut clicked_mark,
                    &mut clicked_candle,
                    &mut foreground_click,
                    &mut take_click,
                    actions,
                );
            }
        }
        // Avid's off-screen notation sits on top of the take line. Keep the
        // straight candle and its script marks beneath this narrow overlay.
        paint_offscreen_paths(app, ui, doc, &columns, &column_centers, &line_indices, &tops, base_y, body, start, end);
        for header in &slate_headers {
            let Some(&row_y) = tops.get(header.start_index) else { continue };
            let thumbnail = thumbnail_rect(body.min.x, base_y + row_y, header, app.ui.script.take_scroll, thumb_w);
            if thumbnail.intersects(body) {
                thumbnail_interaction(app, ui, header, thumbnail, body, &tops, base_y, doc, &mut foreground_click, &mut take_click, actions);
            }
        }
        for index in start..end {
            let (Some(&y0), Some(&y1)) = (tops.get(index), tops.get(index + 1)) else { continue };
            let row = Rect::from_min_size(pos2(ui.max_rect().left(), base_y + y0), vec2(body.width().max(1.0), y1 - y0));
            if !row.intersects(body) {
                continue;
            }
            for (column, (slate, maybe_take, _)) in columns.iter().enumerate() {
                let Some(take) = maybe_take else { continue };
                if app.ui.script.drag_slate == Some(*slate) && app.ui.script.drag_slate_group {
                    continue;
                }
                let (Some(&take_start), Some(&take_end)) = (line_indices.get(&take.start_line.0), line_indices.get(&take.end_line.0)) else { continue };
                if index != take_start && index != take_end {
                    continue;
                }
                let cx = body.min.x + column_centers[column] - app.ui.script.take_scroll;
                if cx + TAKE_W * 0.5 < body.min.x || cx - TAKE_W * 0.5 > body.max.x {
                    continue;
                }
                endpoint_interaction(
                    app,
                    ui,
                    take,
                    row,
                    body,
                    cx,
                    index,
                    take_start,
                    take_end,
                    &tops,
                    base_y,
                    doc,
                    &mut foreground_click,
                    &mut take_click,
                    actions,
                );
            }
        }
        // Paint last so a setup may sit over the script text, like a floating slate.
        for header in &slate_headers {
            let Some(&row_y) = tops.get(header.start_index) else { continue };
            let thumbnail = thumbnail_rect(body.min.x, base_y + row_y, header, app.ui.script.take_scroll, thumb_w);
            if thumbnail.intersects(body) {
                thumbnail_paint(app, ui, header, thumbnail, body);
                take_metadata_paint(app, ui, doc, header, &column_centers, thumbnail, body);
            }
        }
        if let Some(target) = app.ui.script.scroll_to_line
            && let Some(index) = doc.lines.iter().position(|line| line.id.0 == target)
            && let (Some(&y0), Some(&y1)) = (tops.get(index), tops.get(index + 1))
        {
            let target_rect = Rect::from_min_max(pos2(ui.max_rect().left(), base_y + y0), pos2(ui.max_rect().right(), base_y + y1));
            ui.scroll_to_rect(target_rect, Some(egui::Align::Center));
            app.ui.script.scroll_to_line = None;
        }
    });
    if let (Some(start), Some(end)) = (app.ui.script.lasso_start, app.ui.script.lasso_end) {
        let lasso = Rect::from_two_pos(start, end).intersect(body);
        ui.painter().rect_filled(lasso, 0.0, t.accent.gamma_multiply(0.14));
        ui.painter().rect_stroke(lasso, 0.0, Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
        if released {
            foreground_click = true;
            let rows: Vec<_> = (0..doc.lines.len())
                .filter(|index| {
                    tops.get(*index).zip(tops.get(index + 1)).is_some_and(|(&a, &b)| {
                        Rect::from_min_max(pos2(body.left(), scroll_origin + a), pos2(body.right(), scroll_origin + b)).intersects(lasso)
                    })
                })
                .collect();
            if let (Some(&first), Some(&last)) = (rows.first(), rows.last()) {
                app.ui.script.selected_lines = Some((doc.lines[first].id.0, doc.lines[last].id.0));
                app.ui.script.selected_section_slates = slate_headers
                    .iter()
                    .filter(|header| {
                        let thumbnail = thumbnail_rect(body.min.x, scroll_origin + tops[header.start_index], header, app.ui.script.take_scroll, thumb_w);
                        thumbnail.intersects(lasso)
                            || doc.slates.iter().find(|slate| slate.id.0 == header.id).is_some_and(|slate| {
                                slate.takes.iter().enumerate().any(|(local, take)| {
                                    let Some((&a, &b)) = line_indices.get(&take.start_line.0).zip(line_indices.get(&take.end_line.0)) else { return false };
                                    let cx = body.left() + column_centers[header.first_column + local] - app.ui.script.take_scroll;
                                    let candle = Rect::from_min_max(
                                        pos2(cx - TAKE_W * 0.5, scroll_origin + tops[a]),
                                        pos2(cx + TAKE_W * 0.5, scroll_origin + tops[b + 1]),
                                    );
                                    candle.intersects(lasso)
                                })
                            })
                    })
                    .map(|header| header.id)
                    .collect();
            }
            app.ui.script.lasso_start = None;
            app.ui.script.lasso_end = None;
        }
    }
    if empty_canvas.clicked() && !foreground_click {
        app.ui.script.selected_lines = None;
    }
    if (empty_canvas.clicked() || foreground_click) && !take_click && !selecting_section {
        app.ui.script.selected_take = None;
        app.ui.script.selected_slate = None;
        app.ui.script.selected_thumbnail = None;
        app.ui.script.selected_mark = None;
        app.ui.script.selected_section_slates.clear();
    }
    if let Some((take, mark)) = clicked_mark {
        actions.push(Action::CueMark { take, mark });
    } else if let Some((take, line)) = clicked_candle {
        actions.push(Action::CueLine { take, line });
    }
    if let Some((take, mark)) = app.ui.script.drag_mark
        && ui.input(|input| input.pointer.any_released())
    {
        if let Some(target) = app.ui.script.drag_mark_target
            && let Some(line) = doc.lines.get(target)
            && ui.input(|input| input.pointer.hover_pos().is_some_and(|pointer| body.contains(pointer)))
        {
            actions.push(Action::Command("script.moveMark", json!({"item": app.ui.script.item, "take": take, "mark": mark, "line": line.id.0})));
        }
        app.ui.script.drag_mark = None;
        app.ui.script.drag_mark_target = None;
    }
    if let Some((source, start, end)) = canvas_drop
        && (range.is_some() || crate::panels::dragged_project_item(ui) == Some(source))
    {
        if let (Some(a), Some(b)) = (doc.lines.get(start), doc.lines.get(end)) {
            // Without a row selection, explicit thumbnail and candle targets may
            // consume the payload first. Otherwise commit the exact preview range.
            actions.push(Action::Command("script.addTake", json!({"item": app.ui.script.item, "source": source.0, "startLine": a.id.0, "endLine": b.id.0})));
        }
        crate::panels::clear_drag(ui);
    }
}

#[allow(clippy::too_many_arguments)]
fn margin_ruler(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    rect: Rect,
    left_w: f32,
    text_inset: f32,
    min_left: f32,
    max_left: f32,
    min_inset: f32,
    max_inset: f32,
) {
    let t = app.tokens;
    let paint = ui.painter();
    paint.line_segment([pos2(rect.left(), rect.bottom() - 1.0), pos2(rect.right(), rect.bottom() - 1.0)], Stroke::new(1.0, t.separator.gamma_multiply(0.55)));
    for tick in 0..=((rect.width() / 20.0).ceil() as usize) {
        let x = rect.left() + tick as f32 * 20.0;
        if x > rect.right() {
            break;
        }
        let length = if tick % 4 == 0 { 7.0 } else { 3.0 };
        paint.line_segment([pos2(x, rect.bottom() - 2.0), pos2(x, rect.bottom() - length - 2.0)], Stroke::new(1.0, t.text_dim.gamma_multiply(0.45)));
    }
    for (id, x, description, left_marker) in [
        ("script.margin.left", rect.left() + text_inset, tl!("Drag to change the script's left margin"), true),
        ("script.margin.right", rect.left() + left_w, tl!("Drag to change script wrapping"), false),
    ] {
        let marker = Rect::from_center_size(pos2(x, rect.center().y), vec2(16.0, rect.height()));
        let response = ui.interact(marker, egui::Id::new(id), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal).on_hover_text(description);
        app.auto.add(id, response.rect, description);
        paint.add(egui::Shape::convex_polygon(
            vec![pos2(x - 4.0, rect.top() + 2.0), pos2(x + 4.0, rect.top() + 2.0), pos2(x, rect.top() + 9.0)],
            t.text_dim.gamma_multiply(0.85),
            Stroke::NONE,
        ));
        if response.dragged() {
            if left_marker {
                app.ui.script.text_inset = Some((text_inset + response.drag_delta().x).clamp(min_inset, max_inset));
            } else {
                app.ui.script.text_column_width = Some((left_w + response.drag_delta().x).clamp(min_left, max_left));
            }
            ui.ctx().request_repaint();
        }
    }
}

fn thumbnail_rect(right_x: f32, row_y: f32, header: &SlateHeader<'_>, scroll: f32, width: f32) -> Rect {
    let group_center = right_x + header.center_x - scroll;
    let height = thumbnail_height(width);
    Rect::from_min_size(pos2(group_center - width * 0.5, row_y - height), vec2(width, height))
}

/// Paint one narrow overlay for each contiguous off-screen passage. Restarting on every
/// script row leaves visible seams at the zigzag joins.
#[allow(clippy::too_many_arguments)]
fn paint_offscreen_paths(
    app: &FilmcraftApp,
    ui: &egui::Ui,
    doc: &ScriptDocument,
    columns: &[(u64, Option<&ScriptTake>, &str)],
    column_centers: &[f32],
    line_indices: &HashMap<u64, usize>,
    tops: &[f32],
    base_y: f32,
    clip: Rect,
    visible_start: usize,
    visible_end: usize,
) {
    let paint = ui.painter().with_clip_rect(clip);
    for (column, (slate, maybe_take, _)) in columns.iter().enumerate() {
        let Some(take) = maybe_take else { continue };
        let (Some(&stored_start), Some(&stored_end)) = (line_indices.get(&take.start_line.0), line_indices.get(&take.end_line.0)) else { continue };
        let (start, end) = match (app.ui.script.drag_endpoint, app.ui.script.drag_target) {
            (Some((id, true)), Some(target)) if id == take.id.0 => (target.min(stored_end), stored_end),
            (Some((id, false)), Some(target)) if id == take.id.0 => (stored_start, target.max(stored_start)),
            _ => (stored_start, stored_end),
        };
        let cx = clip.left() + column_centers[column] - app.ui.script.take_scroll;
        if cx + TAKE_W * 0.5 < clip.left() || cx - TAKE_W * 0.5 > clip.right() {
            continue;
        }
        let mut overlay = Vec::new();
        let flush = |overlay: &mut Vec<egui::Pos2>| {
            if overlay.len() > 1 {
                paint.add(egui::Shape::line(std::mem::take(overlay), Stroke::new(2.25, app.tokens.text_dim.gamma_multiply(0.9))));
            }
        };
        let preview_y = if app.ui.script.drag_slate == Some(*slate) && app.ui.script.drag_slate_group { app.ui.script.drag_slate_delta.y } else { 0.0 };
        for index in start.max(visible_start.saturating_sub(1))..=end.min(visible_end) {
            let Some(line) = doc.lines.get(index) else { continue };
            if !take.offscreen_lines.contains(&line.id) {
                flush(&mut overlay);
                continue;
            }
            let y0 = base_y + tops[index] + preview_y + if index == start { 16.0 } else { 0.0 };
            let y1 = base_y + tops[index + 1] + preview_y - if index == end { 2.0 } else { 0.0 };
            let delta = (y1 - y0).max(0.0);
            let points = [pos2(cx, y0), pos2(cx - 5.0, y0 + delta / 3.0), pos2(cx + 5.0, y0 + delta * 2.0 / 3.0), pos2(cx, y1)];
            overlay.extend(points.iter().copied().skip(usize::from(!overlay.is_empty())));
        }
        flush(&mut overlay);
    }
}

#[allow(clippy::too_many_arguments)]
fn row_view(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    row: Rect,
    left_w: f32,
    text_inset: f32,
    index: usize,
    line: &ScriptLine,
    columns: &[(u64, Option<&ScriptTake>, &str)],
    column_centers: &[f32],
    line_indices: &HashMap<u64, usize>,
    tops: &[f32],
    clicked_mark: &mut Option<(u64, u64)>,
    clicked_candle: &mut Option<(u64, u64)>,
    foreground_click: &mut bool,
    take_click: &mut bool,
    actions: &mut Vec<Action>,
) {
    let t = app.tokens;
    let left = Rect::from_min_size(row.min, vec2(left_w, row.height()));
    let right = row;
    let interaction = ui.interact(row, egui::Id::new(("script.line", line.id.0)), Sense::click());
    app.auto.add(&format!("script.line.{}", line.id.0), interaction.rect, &line.text);
    // Select on press: a slight pointer slip or releasing Shift before the
    // mouse button must not leave the range at the previous row.
    let select_pressed =
        interaction.contains_pointer() && interaction.is_pointer_button_down_on() && ui.input(|input| input.pointer.primary_pressed() && !input.modifiers.alt);
    let select_activated = interaction.clicked() && !interaction.clicked_by(egui::PointerButton::Primary);
    if select_pressed || select_activated {
        select_line(&mut app.ui.script, line, shift_click(ui));
    }
    *foreground_click |= select_pressed || interaction.clicked();
    if interaction.double_clicked() {
        app.ui.script.edit_line = Some(line.id.0);
        app.ui.script.edit_text = line.text.clone();
    }
    interaction.clone().context_menu(|ui| {
        if ui.button(tl!("Edit Line")).clicked() {
            app.ui.script.edit_line = Some(line.id.0);
            app.ui.script.edit_text = line.text.clone();
            ui.close();
        }
        if ui.button(tl!("Insert Line Below")).clicked() {
            actions.push(Action::Command("script.insertLine", json!({"item": app.ui.script.item, "after": line.id.0, "text": ""})));
            ui.close();
        }
        if ui.button(tl!("Delete Line")).clicked() {
            actions.push(Action::Command("script.deleteLine", json!({"item": app.ui.script.item, "line": line.id.0})));
            ui.close();
        }
    });
    let label_y = left.center().y;
    let mut scene_start = 6.0;
    if app.ui.script.show_line_numbers.unwrap_or(true) {
        let number = (index + 1).to_string();
        let font = Tokens::ui(10.0);
        scene_start += ui.painter().layout_no_wrap(number.clone(), font.clone(), t.text_dim).size().x + 5.0;
        ui.painter().text(pos2(left.min.x + 6.0, label_y), Align2::LEFT_CENTER, number, font, t.text_dim.gamma_multiply(0.55));
    }
    let page_rect = Rect::from_min_size(pos2(left.max.x, left.min.y + 1.0), vec2(52.0, MIN_ROW_H - 2.0));
    let scene_rect =
        Rect::from_min_size(pos2(left.min.x + scene_start, left.min.y + 1.0), vec2((text_inset - scene_start - 4.0).clamp(1.0, 72.0), MIN_ROW_H - 2.0));
    if app.ui.script.edit_page == Some(line.id.0) {
        let response = ui.put(page_rect, egui::TextEdit::singleline(&mut app.ui.script.page_text).hint_text(tl!("Page")));
        *foreground_click |= response.clicked();
        if app.ui.script.label_focus == Some(2) {
            response.request_focus();
            app.ui.script.label_focus = None;
        }
        app.auto.add_response(&format!("script.page.{}.edit", line.id.0), &response, "Edit page label");
        if response.lost_focus() || response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            let page = app.ui.script.page_text.trim();
            actions.push(Action::Command(
                "script.setLabel",
                json!({"item": app.ui.script.item, "line": line.id.0,
                "scene": line.scene.as_deref(), "page": if page.is_empty() { None } else { Some(page) }}),
            ));
            app.ui.script.edit_page = None;
        }
    } else if let Some(page) = &line.page {
        let response = ui
            .put(
                page_rect,
                egui::Label::new(egui::RichText::new(format!("Pg {}", truncate(page, 3))).italics().color(t.text_dim).size(10.5)).sense(Sense::click()),
            )
            .on_hover_text(tl!("Double-click to edit Page"));
        *foreground_click |= response.clicked();
        app.auto.add(&format!("script.page.{}", line.id.0), response.rect, "Page label");
        if response.double_clicked() {
            select_line(&mut app.ui.script, line, false);
            app.ui.script.edit_page = Some(line.id.0);
            app.ui.script.label_focus = Some(2);
        }
    }
    if app.ui.script.edit_scene == Some(line.id.0) {
        let response = ui.put(scene_rect, egui::TextEdit::singleline(&mut app.ui.script.scene_text).hint_text(tl!("Scene")));
        *foreground_click |= response.clicked();
        if app.ui.script.label_focus == Some(1) {
            response.request_focus();
            app.ui.script.label_focus = None;
        }
        app.auto.add_response(&format!("script.scene.{}.edit", line.id.0), &response, "Edit scene label");
        if response.lost_focus() || response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            let scene = app.ui.script.scene_text.trim();
            actions.push(Action::Command(
                "script.setLabel",
                json!({"item": app.ui.script.item, "line": line.id.0,
                "scene": if scene.is_empty() { None } else { Some(scene) }, "page": line.page.as_deref()}),
            ));
            app.ui.script.edit_scene = None;
        }
    } else if let Some(scene) = &line.scene {
        let response = ui
            .put(
                scene_rect,
                egui::Label::new(egui::RichText::new(format!("Sc {scene}")).italics().color(t.text_dim).size(11.0)).truncate().sense(Sense::click()),
            )
            .on_hover_text(tl!("Double-click to edit Scene"));
        *foreground_click |= response.clicked();
        app.auto.add(&format!("script.scene.{}", line.id.0), response.rect, "Scene label");
        if response.double_clicked() {
            select_line(&mut app.ui.script, line, false);
            app.ui.script.edit_scene = Some(line.id.0);
            app.ui.script.label_focus = Some(1);
        }
    }
    let text_x = left.min.x + text_inset;
    if app.ui.script.edit_line == Some(line.id.0) {
        let edit_rect = Rect::from_min_max(pos2(text_x, left.min.y + 2.0), pos2(left.max.x, left.max.y - 2.0));
        let response = ui.put(edit_rect, egui::TextEdit::singleline(&mut app.ui.script.edit_text).desired_width(edit_rect.width()));
        app.auto.add_response(&format!("script.line.{}.edit", line.id.0), &response, "Edit script line");
        if let Some(at) = app.ui.script.edit_caret.take()
            && let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), response.id)
        {
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(at))));
            state.store(ui.ctx(), response.id);
            response.request_focus();
        }
        let cursor = egui::text_edit::TextEditState::load(ui.ctx(), response.id).and_then(|state| state.cursor.char_range());
        if response.has_focus()
            && index > 0
            && ui.input(|input| input.key_pressed(egui::Key::Backspace))
            && cursor.is_some_and(|range| usize::from(range.primary.index) == 0 && usize::from(range.secondary.index) == 0)
        {
            actions.push(Action::MergeWithPrevious { line: line.id.0, text: app.ui.script.edit_text.clone() });
        } else if (response.has_focus() || response.lost_focus()) && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            let at = cursor.map_or(app.ui.script.edit_text.chars().count(), |range| range.primary.index.into());
            actions.push(Action::SplitLine { line: line.id.0, text: app.ui.script.edit_text.clone(), at });
            app.ui.script.edit_line = None;
        } else if response.lost_focus() {
            actions.push(Action::Command("script.editLine", json!({"item": app.ui.script.item, "line": line.id.0, "text": app.ui.script.edit_text})));
            app.ui.script.edit_line = None;
        }
    } else {
        let text_rect = Rect::from_min_max(pos2(text_x, left.min.y + 2.0), pos2(left.max.x, left.max.y - 2.0));
        let galley = ui.painter().layout(
            line.text.clone(),
            Tokens::ui(app.ui.script.font_size.unwrap_or(SCRIPT_FONT_SIZE).clamp(10.0, 24.0)),
            t.text,
            text_rect.width().max(48.0),
        );
        let clipped = galley.size().y > text_rect.height();
        ui.painter().with_clip_rect(text_rect).galley(text_rect.min, galley, t.text);
        if clipped {
            interaction.on_hover_text(&line.text);
        }
    }
    let clip = Rect::from_min_max(right.min, row.max);
    for (column, (slate, maybe_take, _name)) in columns.iter().enumerate() {
        let Some(take) = maybe_take else { continue };
        let cx = right.min.x + column_centers[column] - app.ui.script.take_scroll;
        if cx + TAKE_W * 0.5 < right.min.x || cx - TAKE_W * 0.5 > right.max.x {
            continue;
        }
        let (Some(&stored_start), Some(&stored_end)) = (line_indices.get(&take.start_line.0), line_indices.get(&take.end_line.0)) else { continue };
        let (start, end) = match (app.ui.script.drag_endpoint, app.ui.script.drag_target) {
            (Some((id, true)), Some(target)) if id == take.id.0 => (target.min(stored_end), stored_end),
            (Some((id, false)), Some(target)) if id == take.id.0 => (stored_start, target.max(stored_start)),
            _ => (stored_start, stored_end),
        };
        if !(start..=end).contains(&index) {
            continue;
        }
        let preview_y = if app.ui.script.drag_slate == Some(*slate) && app.ui.script.drag_slate_group { app.ui.script.drag_slate_delta.y } else { 0.0 };
        let draw_row = row.translate(vec2(0.0, preview_y));
        let draw_clip = clip.translate(vec2(0.0, preview_y)).intersect(ui.clip_rect());
        let row_color = take.line_colors.iter().find(|entry| entry.line == line.id).map_or(take.color, |entry| entry.color);
        let color = script_color(app, row_color);
        let selected_take = app.ui.script.selected_take == Some(take.id.0);
        let stroke = Stroke::new(5.0, color);
        // Strokes are centered on the same path: selection adds a thicker gray
        // outline equally on both sides of the colored candle.
        let halo = Stroke::new(if selected_take { 9.0 } else { 7.0 }, t.text_dim);
        let paint = ui.painter().with_clip_rect(draw_clip);
        let candle_top = if index == start { draw_row.min.y + 16.0 } else { draw_row.min.y };
        let candle_bottom = if index == end { draw_row.max.y - 2.0 } else { draw_row.max.y };
        paint.line_segment([pos2(cx, candle_top), pos2(cx, candle_bottom)], halo);
        paint.line_segment([pos2(cx, candle_top), pos2(cx, candle_bottom)], stroke);
        // Keep the short exposed tips at either endpoint the same color as
        // the outline. A row's fill can begin just below the take number (or
        // continue past the final row), so a tiny cap lets the take's base
        // color peek through even when the adjacent range is recolored.
        let segment_height = (candle_bottom - candle_top).max(0.0);
        let cap_height = if start == end {
            12.0_f32.min(segment_height * 0.5)
        } else {
            let first_span = tops.get(start).zip(tops.get(start + 1)).map_or(0.0, |(&top, &bottom)| (bottom - top - 16.0).max(0.0));
            let last_span = tops.get(end).zip(tops.get(end + 1)).map_or(0.0, |(&top, &bottom)| (bottom - top - 2.0).max(0.0));
            12.0_f32.min(first_span).min(last_span)
        };
        if index == start {
            paint.rect_filled(Rect::from_min_size(pos2(cx - halo.width * 0.5, candle_top), vec2(halo.width, cap_height)), 0.0, t.text_dim);
        }
        if index == end {
            paint.rect_filled(Rect::from_min_size(pos2(cx - halo.width * 0.5, candle_bottom - cap_height), vec2(halo.width, cap_height)), 0.0, t.text_dim);
        }
        let candle = Rect::from_min_size(pos2(cx - TAKE_W * 0.5, draw_row.min.y), vec2(TAKE_W, draw_row.height()));
        let response = ui.interact(candle.intersect(draw_clip), egui::Id::new(("script.candle", take.id.0, line.id.0)), Sense::click());
        app.auto.add(&format!("script.take.{}.line.{}", take.id.0, line.id.0), response.rect, "Take coverage");
        if response.clicked() {
            *foreground_click = true;
            *take_click = true;
            let extend = shift_click(ui)
                && app.ui.script.selected_take == Some(take.id.0)
                && app.ui.script.selected_lines.is_some_and(|(anchor, _)| line_indices.get(&anchor).is_some_and(|at| (start..=end).contains(at)));
            let anchor = if extend { app.ui.script.selected_lines.map_or(line.id.0, |pair| pair.0) } else { line.id.0 };
            app.ui.script.selected_slate = Some(*slate);
            app.ui.script.selected_thumbnail = None;
            app.ui.script.selected_take = Some(take.id.0);
            app.ui.script.selected_lines = Some((anchor, line.id.0));
            app.ui.script.selected_mark = None;
        }
        if response.double_clicked() && !shift_click(ui) {
            *clicked_candle = Some((take.id.0, line.id.0));
        }
        response.clone().context_menu(|ui| take_menu(app, ui, take, actions));
        if response.contains_pointer()
            && ui.input(|input| input.pointer.any_released())
            && let Some(source) = crate::panels::dragged_project_item(ui)
        {
            actions.push(Action::Rebind { take: take.id.0, source: source.0 });
            crate::panels::clear_drag(ui);
        }
        // Draw the dragged mark on top of an existing mark at the destination row.
        // Otherwise the preview can disappear until the pointer is released.
        let dragged_here = take.marks.iter().find(|mark| {
            app.ui.script.drag_mark == Some((take.id.0, mark.id.0))
                && line_indices.get(&mark.line.0).is_some_and(|row| (start..=end).contains(row))
                && app.ui.script.drag_mark_target == Some(index)
        });
        let mark = dragged_here.or_else(|| take.marks.iter().find(|mark| app.ui.script.drag_mark != Some((take.id.0, mark.id.0)) && mark.line == line.id));
        if let Some(mark) = mark {
            let mark_y = if index == start { draw_row.max.y - 8.0 } else { draw_row.center().y };
            let mark_rect = Rect::from_center_size(pos2(cx, mark_y), vec2(23.0, 18.0));
            let marker = ui.painter().with_clip_rect(draw_clip);
            marker.add(egui::Shape::convex_polygon(
                vec![pos2(cx - 4.0, mark_y - 5.0), pos2(cx - 11.0, mark_y), pos2(cx - 4.0, mark_y + 5.0)],
                t.text_dim,
                Stroke::NONE,
            ));
            marker.add(egui::Shape::convex_polygon(
                vec![pos2(cx + 4.0, mark_y - 5.0), pos2(cx + 11.0, mark_y), pos2(cx + 4.0, mark_y + 5.0)],
                t.text_dim,
                Stroke::NONE,
            ));
            let response = ui
                .interact(mark_rect.intersect(draw_clip), egui::Id::new(("script.mark", mark.id.0)), Sense::click_and_drag())
                .on_hover_text(tl!("Click to cue Source; Command/Ctrl-drag to move this mark to another script line"));
            app.auto.add(&format!("script.mark.{}", mark.id.0), response.rect, "Open this mark in Source monitor");
            if response.drag_started() && ui.input(|input| input.modifiers.command) {
                app.ui.script.drag_mark = Some((take.id.0, mark.id.0));
                app.ui.script.drag_mark_target = Some(index);
            }
            *take_click |= response.clicked();
            if response.clicked() && shift_click(ui) {
                *foreground_click = true;
                let extend = app.ui.script.selected_take == Some(take.id.0);
                app.ui.script.selected_slate = Some(*slate);
                app.ui.script.selected_thumbnail = None;
                app.ui.script.selected_take = Some(take.id.0);
                select_line(&mut app.ui.script, line, extend);
            } else if response.clicked() && !ui.input(|input| input.modifiers.command) {
                *foreground_click = true;
                app.ui.script.selected_slate = Some(*slate);
                app.ui.script.selected_thumbnail = None;
                app.ui.script.selected_take = Some(take.id.0);
                select_line(&mut app.ui.script, line, false);
                app.ui.script.selected_mark = Some(mark.id.0);
                *clicked_mark = Some((take.id.0, mark.id.0));
            }
            response.context_menu(|ui| {
                if ui.button(tl!("Delete Mark")).clicked() {
                    actions.push(Action::Command("script.deleteMark", json!({"item": app.ui.script.item, "take": take.id.0, "mark": mark.id.0})));
                    ui.close();
                }
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn endpoint_interaction(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    take: &ScriptTake,
    row: Rect,
    clip: Rect,
    cx: f32,
    index: usize,
    start: usize,
    end: usize,
    tops: &[f32],
    base_y: f32,
    doc: &ScriptDocument,
    foreground_click: &mut bool,
    take_click: &mut bool,
    actions: &mut Vec<Action>,
) {
    let candle_top = if index == start { row.min.y + 16.0 } else { row.min.y };
    for (top, y) in [(true, candle_top), (false, row.max.y - 2.0)] {
        if (top && index != start) || (!top && index != end) {
            continue;
        }
        let handle = Rect::from_center_size(pos2(cx, y), vec2(24.0, 14.0)).intersect(clip);
        let response = ui
            .interact(handle, egui::Id::new(("script.endpoint", take.id.0, top)), Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::ResizeVertical)
            .on_hover_text(tl!("Command-drag (Mac) or Ctrl-drag (Windows) to adjust candle length"));
        app.auto.add(&format!("script.take.{}.{}Handle", take.id.0, if top { "top" } else { "bottom" }), response.rect, "Adjust candle length");
        if response.clicked() || response.drag_started() {
            *foreground_click = true;
            *take_click = true;
        }
        if response.clicked() {
            let line = if top { take.start_line } else { take.end_line };
            let extend = shift_click(ui) && app.ui.script.selected_take == Some(take.id.0);
            let anchor = if extend { app.ui.script.selected_lines.map_or(line.0, |pair| pair.0) } else { line.0 };
            app.ui.script.selected_slate = doc.slates.iter().find(|slate| slate.takes.iter().any(|candidate| candidate.id == take.id)).map(|slate| slate.id.0);
            app.ui.script.selected_thumbnail = None;
            app.ui.script.selected_take = Some(take.id.0);
            app.ui.script.selected_lines = Some((anchor, line.0));
            app.ui.script.selected_mark = None;
        }
        if response.drag_started() && ui.input(|input| input.modifiers.command) {
            app.ui.script.drag_endpoint = Some((take.id.0, top));
            app.ui.script.drag_target = Some(index);
        }
        if app.ui.script.drag_endpoint == Some((take.id.0, top)) {
            if let Some(pointer) = response.interact_pointer_pos() {
                let target = first_visible(tops, (pointer.y - base_y).max(0.0)).min(doc.lines.len().saturating_sub(1));
                app.ui.script.drag_target = Some(if top { target.min(end) } else { target.max(start) });
            }
            if response.drag_stopped() {
                if let Some(target) = app.ui.script.drag_target.and_then(|index| doc.lines.get(index)) {
                    actions.push(Action::Command(
                        "script.setCoverage",
                        json!({"item": app.ui.script.item, "take": take.id.0,
                        "startLine": if top { target.id.0 } else { take.start_line.0 },
                        "endLine": if top { take.end_line.0 } else { target.id.0 }}),
                    ));
                }
                app.ui.script.drag_endpoint = None;
                app.ui.script.drag_target = None;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn thumbnail_interaction(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    header: &SlateHeader<'_>,
    thumbnail: Rect,
    clip: Rect,
    tops: &[f32],
    base_y: f32,
    doc: &ScriptDocument,
    foreground_click: &mut bool,
    take_click: &mut bool,
    actions: &mut Vec<Action>,
) {
    let response =
        ui.interact(thumbnail.intersect(clip), egui::Id::new(("script.thumbnail", header.id)), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::Grab);
    app.auto.add(&format!("script.slate.{}.thumbnail", header.id), response.rect, "Setup thumbnail");
    if response.clicked() || response.drag_started() {
        *foreground_click = true;
        *take_click = true;
    }
    if response.drag_started() && app.ui.script.lasso_start.is_none() {
        app.ui.script.drag_slate = Some(header.id);
        app.ui.script.drag_slate_target = Some(header.start_index);
        app.ui.script.drag_slate_group = ui.input(|input| input.modifiers.command);
        app.ui.script.drag_slate_delta = egui::Vec2::ZERO;
        app.ui.script.drag_slate_origin_x = thumbnail.center().x;
        app.ui.script.drag_slate_grab_y = response.interact_pointer_pos().map_or(-thumbnail.height() * 0.5, |pointer| pointer.y - thumbnail.bottom());
    }
    if response.dragged() && app.ui.script.drag_slate == Some(header.id) {
        app.ui.script.drag_slate_delta.x += response.drag_delta().x;
    }
    if app.ui.script.drag_slate == Some(header.id) {
        if let Some(pointer) = response.interact_pointer_pos() {
            let bottom_y = (pointer.y - app.ui.script.drag_slate_grab_y - base_y).max(0.0);
            let lower = first_visible(tops, bottom_y).min(doc.lines.len().saturating_sub(1));
            let upper = (lower + 1).min(doc.lines.len().saturating_sub(1));
            let mut target = if (tops[upper] - bottom_y).abs() < (tops[lower] - bottom_y).abs() { upper } else { lower };
            if app.ui.script.drag_slate_group
                && let Some(slate) = doc.slates.iter().find(|slate| slate.id.0 == header.id)
            {
                // A whole setup cannot move past either script edge. Stop at the nearest
                // valid row instead of previewing a move that the document will reject.
                let bounds = slate
                    .takes
                    .iter()
                    .flat_map(|take| [take.start_line, take.end_line])
                    .filter_map(|line| doc.line_index(line))
                    .fold((header.start_index, header.start_index), |(lo, hi), at| (lo.min(at), hi.max(at)));
                let offset = target as i64 - header.start_index as i64;
                let offset = offset.clamp(-(bounds.0 as i64), doc.lines.len() as i64 - 1 - bounds.1 as i64);
                target = (header.start_index as i64 + offset) as usize;
            }
            app.ui.script.drag_slate_target = Some(target);
            app.ui.script.drag_slate_delta.y = tops[target] - tops[header.start_index];
        }
        if response.drag_stopped() {
            if let Some(target) = app.ui.script.drag_slate_target {
                let x = ((app.ui.script.drag_slate_origin_x + app.ui.script.drag_slate_delta.x - clip.left() + app.ui.script.take_scroll)
                    / clip.width().max(1.0))
                .clamp(0.0, 1.0);
                if app.ui.script.drag_slate_group {
                    let delta = target as i64 - header.start_index as i64;
                    actions.push(Action::Command("script.shiftSlate", json!({"item": app.ui.script.item, "slate": header.id, "delta": delta, "x": x})));
                } else if let Some(line) = doc.lines.get(target) {
                    actions.push(Action::Command("script.moveThumbnail", json!({"item": app.ui.script.item, "slate": header.id, "line": line.id.0, "x": x})));
                }
            }
            // Keep painting the release-frame preview until the queued command commits.
            // Clearing it here briefly paints the old stored position before the edit runs.
            actions.push(Action::FinishSlateDrag);
        }
    }
    if response.clicked() {
        app.ui.script.selected_slate = Some(header.id);
        app.ui.script.selected_thumbnail = Some(header.id);
        app.ui.script.selected_mark = None;
        if let Some(slate) = doc.slates.iter().find(|slate| slate.id.0 == header.id)
            && !slate.takes.iter().any(|take| app.ui.script.selected_take == Some(take.id.0))
        {
            app.ui.script.selected_take = slate.takes.first().map(|take| take.id.0);
        }
        app.ui.script.thumbnail_shuttle_count = 0;
    }
    response.clone().context_menu(|ui| {
        if let Some(source) = header.last_take.source
            && ui.button(tl!("Add Another Candle")).clicked()
        {
            actions.push(Action::Command(
                "script.addTake",
                json!({"item": app.ui.script.item, "slate": header.id,
                    "source": source.0, "startLine": header.last_take.start_line.0, "endLine": header.last_take.end_line.0}),
            ));
            ui.close();
        }
    });
    if response.contains_pointer()
        && ui.input(|input| input.pointer.any_released())
        && let Some(source) = crate::panels::dragged_project_item(ui)
    {
        actions.push(Action::Command(
            "script.addTake",
            json!({"item": app.ui.script.item, "slate": header.id,
                "source": source.0, "startLine": header.last_take.start_line.0, "endLine": header.last_take.end_line.0}),
        ));
        crate::panels::clear_drag(ui);
    }
}

fn thumbnail_paint(app: &mut FilmcraftApp, ui: &egui::Ui, header: &SlateHeader<'_>, thumbnail: Rect, clip: Rect) {
    let t = app.tokens;
    let shown = if app.ui.script.drag_slate == Some(header.id) { thumbnail.translate(vec2(0.0, app.ui.script.drag_slate_delta.y)) } else { thumbnail };
    let picture = Rect::from_min_size(shown.min, vec2(shown.width(), shown.width() * 9.0 / 16.0));
    let chin = Rect::from_min_max(pos2(shown.left(), picture.bottom()), shown.max);
    let paint = ui.painter().with_clip_rect(clip);
    paint.rect_filled(shown, 3.0, t.field_bg);
    let selected = app.ui.script.selected_slate == Some(header.id) || app.ui.script.selected_section_slates.contains(&header.id);
    paint.rect_filled(chin, 1.0, if selected { t.hover } else { t.field_bg });
    if let Some(source) = header.source.and_then(|id| app.session.project.item(id)) {
        let resolved = app.session.project.resolve_media(source.id);
        let thumb_item = resolved.map_or(source.id, |value| value.0);
        let thumb_time = header.source_time.unwrap_or_else(|| resolved.and_then(|(_, _, span)| span.map(|range| range.start)).unwrap_or(Tick::ZERO));
        if let Some((texture, size)) = app.thumbnail(ui.ctx(), thumb_item, thumb_time, shown.width().round() as u32) {
            paint.image(texture, crate::panels::monitor::fit(picture, size.x, size.y), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
    paint.with_clip_rect(chin.shrink2(vec2(4.0, 0.0))).text(
        pos2(chin.left() + 5.0, chin.center().y),
        Align2::LEFT_CENTER,
        header.source_name,
        Tokens::ui(10.0),
        t.text,
    );
    paint.rect_stroke(shown, 3.0, Stroke::new(if selected { 1.75 } else { 1.25 }, t.text_dim), egui::StrokeKind::Middle);
}

/// Each candle's take number hangs from the shared setup thumbnail, even when the
/// thumbnail floats above or below the candle's first script line.
fn take_metadata_paint(app: &mut FilmcraftApp, ui: &egui::Ui, doc: &ScriptDocument, header: &SlateHeader<'_>, centers: &[f32], thumbnail: Rect, clip: Rect) {
    let Some(slate) = doc.slates.iter().find(|slate| slate.id.0 == header.id) else { return };
    let preview_y = if app.ui.script.drag_slate == Some(header.id) { app.ui.script.drag_slate_delta.y } else { 0.0 };
    let bottom = thumbnail.bottom() + preview_y;
    let paint = ui.painter().with_clip_rect(clip);
    for (ordinal, take) in slate.takes.iter().enumerate() {
        let Some(&center) = centers.get(header.first_column + ordinal) else { continue };
        let cx = clip.left() + center - app.ui.script.take_scroll;
        let take_number = take
            .source
            .and_then(|id| app.session.project.item(id))
            .and_then(|item| item.metadata.get("Take"))
            .and_then(|value| value.trim().parse::<u32>().ok())
            .map_or_else(|| (ordinal + 1).to_string(), |value| value.to_string());
        let meta = take_metadata_rect(cx, bottom);
        paint.rect_filled(meta, 2.0, app.tokens.panel_bg);
        paint.rect_stroke(meta, 2.0, Stroke::new(1.25, app.tokens.text_dim), egui::StrokeKind::Inside);
        paint.text(meta.center(), Align2::CENTER_CENTER, &take_number, Tokens::ui(if take_number.len() > 2 { 7.0 } else { 9.5 }), app.tokens.text);
        app.auto.add(&format!("script.take.{}.metadata", take.id.0), meta, "Take metadata");
    }
}

fn take_metadata_rect(cx: f32, thumbnail_bottom: f32) -> Rect {
    Rect::from_min_size(pos2(cx - 8.0, thumbnail_bottom), vec2(16.0, 15.0))
}

fn take_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, take: &ScriptTake, actions: &mut Vec<Action>) {
    if let Some(doc) = app.ui.script.item.and_then(|id| app.session.project.item(ItemId(id))).and_then(|item| item.as_script())
        && let Some(slate) = doc.slates.iter().find(|slate| slate.takes.iter().any(|candidate| candidate.id == take.id))
        && slate.takes.len() > 1
    {
        let index = slate.takes.iter().position(|candidate| candidate.id == take.id).unwrap_or(0);
        let left = ui.add_enabled(index > 0, egui::Button::new(tl!("Move Left")));
        app.auto.add(&format!("script.take.{}.moveLeft", take.id.0), left.rect, "Move candle left");
        if left.clicked() {
            actions.push(Action::Command("script.moveTake", json!({"item":app.ui.script.item,"take":take.id.0,"direction":-1})));
            ui.close();
        }
        let right = ui.add_enabled(index + 1 < slate.takes.len(), egui::Button::new(tl!("Move Right")));
        app.auto.add(&format!("script.take.{}.moveRight", take.id.0), right.rect, "Move candle right");
        if right.clicked() {
            actions.push(Action::Command("script.moveTake", json!({"item":app.ui.script.item,"take":take.id.0,"direction":1})));
            ui.close();
        }
        ui.separator();
    }
    if let Some(source) = take.source
        && ui.button(tl!("Find in Project")).clicked()
    {
        actions.push(Action::FindInProject(source));
        ui.close();
    }
    ui.menu_button(tl!("Candle Color"), |ui| {
        for color in ScriptColor::ALL {
            if ui.selectable_label(take.color == color, crate::i18n::t(color.name())).clicked() {
                actions.push(Action::Command("script.setTakeStyle", json!({"item": app.ui.script.item, "take": take.id.0, "color": color.name()})));
                ui.close();
            }
        }
    });
    let selected_ids = app
        .ui
        .script
        .item
        .and_then(|id| app.session.project.item(ItemId(id)))
        .and_then(|item| item.as_script())
        .map(|doc| selected_take_lines(doc, &app.ui.script, take));
    if let Some(ids) = selected_ids.filter(|ids| !ids.is_empty()) {
        let all_offscreen = ids.iter().all(|id| take.offscreen_lines.contains(id));
        if ui.button(if all_offscreen { tl!("Clear Offscreen") } else { tl!("Mark Offscreen") }).clicked() {
            toggle_offscreen(app, take, ids, actions);
            ui.close();
        }
    }
}
