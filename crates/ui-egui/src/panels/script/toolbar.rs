//! Script toolbar, settings, and keyboard coverage controls.

use super::*;

pub(super) fn header(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    rect: Rect,
    scripts: &[(ItemId, String)],
    doc: Option<&Arc<ScriptDocument>>,
    actions: &mut Vec<Action>,
) {
    let t = app.tokens;
    let mut top = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2((rect.width() - 16.0).max(1.0), 30.0))));
    egui::ScrollArea::horizontal().id_salt("script.toolbar").auto_shrink([false, true]).show(&mut top, |ui| {
        ui.horizontal(|ui| {
            let name =
                app.ui.script.item.and_then(|id| scripts.iter().find(|entry| entry.0.0 == id).map(|entry| entry.1.as_str())).unwrap_or(tl!("Choose a script"));
            egui::ComboBox::from_id_salt("script.document").selected_text(name).width(180.0).show_ui(ui, |ui| {
                for (id, name) in scripts {
                    let response = ui.selectable_label(app.ui.script.item == Some(id.0), name);
                    app.auto.add(&format!("script.document.{}", id.0), response.rect, name);
                    if response.clicked() {
                        app.ui.script.item = Some(id.0);
                        app.ui.script.selected_lines = None;
                        app.ui.script.selected_slate = None;
                        app.ui.script.selected_thumbnail = None;
                        app.ui.script.selected_take = None;
                        app.ui.script.selected_section_slates.clear();
                    }
                }
            });
            let settings = ui.menu_button("⚙", |ui| {
                ui.label(tl!("Script settings"));
                let mut font_size = app.ui.script.font_size.unwrap_or(SCRIPT_FONT_SIZE);
                let size = ui.add(egui::Slider::new(&mut font_size, 10.0..=24.0).text(tl!("Text size")));
                app.auto.add("script.settings.textSize", size.rect, "Script text size");
                if size.changed() {
                    app.ui.script.font_size = Some(font_size);
                }
                let mut thumb_w = thumbnail_width(&app.ui.script);
                let thumb_size = ui.add(egui::Slider::new(&mut thumb_w, MIN_THUMB_W..=MAX_THUMB_W).text(tl!("Thumbnail size")).suffix(" px"));
                app.auto.add("script.settings.thumbnailSize", thumb_size.rect, "Thumbnail width; picture keeps a 16:9 aspect ratio");
                if thumb_size.changed() {
                    app.ui.script.thumbnail_width = Some(thumb_w.round());
                }
                let mut left_margin = app.ui.script.text_inset.unwrap_or(DEFAULT_TEXT_INSET);
                let margin = ui.add(egui::DragValue::new(&mut left_margin).range(54.0..=600.0).suffix(" px").prefix(format!("{} ", tl!("Left margin"))));
                app.auto.add("script.settings.leftMargin", margin.rect, "Script left margin");
                if margin.changed() {
                    app.ui.script.text_inset = Some(left_margin);
                }
                let mut wrap_margin = app.ui.script.text_column_width.unwrap_or((rect.width() * 0.49).max(180.0));
                let wrap = ui.add(egui::DragValue::new(&mut wrap_margin).range(180.0..=1600.0).suffix(" px").prefix(format!("{} ", tl!("Wrap margin"))));
                app.auto.add("script.settings.wrapMargin", wrap.rect, "Script wrap margin");
                if wrap.changed() {
                    app.ui.script.text_column_width = Some(wrap_margin);
                }
                let mut row_colors = app.ui.script.show_row_colors.unwrap_or(true);
                let stripes = ui.checkbox(&mut row_colors, tl!("Show row colors"));
                app.auto.add("script.settings.rowColors", stripes.rect, "Show row colors");
                if stripes.changed() {
                    app.ui.script.show_row_colors = Some(row_colors);
                }
                let mut line_numbers = app.ui.script.show_line_numbers.unwrap_or(true);
                let numbers = ui.checkbox(&mut line_numbers, tl!("Show line numbers"));
                app.auto.add("script.settings.lineNumbers", numbers.rect, "Show line numbers");
                if numbers.changed() {
                    app.ui.script.show_line_numbers = Some(line_numbers);
                }
            });
            app.auto.add("script.settings", settings.response.rect, "Script settings");
            for (key, text, action) in [("new", tl!("New"), Action::Create), ("import", tl!("Import Text…"), Action::Import)] {
                let response = ui.button(text);
                let response = if key == "import" { response.on_hover_text(tl!("Plain text .txt supported.")) } else { response };
                app.auto.add(&format!("script.{key}"), response.rect, text);
                if response.clicked() {
                    actions.push(action);
                }
            }
            let paint = ui
                .add(egui::Button::new("  ").min_size(vec2(29.0, 23.0)))
                .on_hover_text(tl!("Set selected candle rows to the current color; right-click for palette"));
            app.auto.add("script.color.apply", paint.rect, "Set candle row color");
            let sample = script_color(app, app.ui.script.paint_color);
            let icon = paint.rect.shrink2(vec2(7.0, 4.0));
            ui.painter().rect_filled(icon, 1.0, sample);
            ui.painter().rect_stroke(icon, 1.0, Stroke::new(1.0, t.text_dim), egui::StrokeKind::Inside);
            let corner = paint.rect.right_bottom() - vec2(3.0, 3.0);
            ui.painter().add(egui::Shape::convex_polygon(vec![corner - vec2(6.0, 0.0), corner - vec2(0.0, 6.0), corner], t.text_dim, Stroke::NONE));
            paint.clone().context_menu(|ui| {
                ui.label(tl!("Candle color"));
                for colors in ScriptColor::ALL.chunks(9) {
                    ui.horizontal(|ui| {
                        for &color in colors {
                            let response = ui
                                .add(egui::Button::new("  ").fill(script_color(app, color)).min_size(vec2(24.0, 21.0)))
                                .on_hover_text(crate::i18n::t(color.name()));
                            app.auto.add(&format!("script.color.{}", color.name().to_lowercase()), response.rect, color.name());
                            if response.clicked() {
                                app.ui.script.paint_color = color;
                                ui.close();
                            }
                        }
                    });
                }
            });
            if paint.clicked()
                && let Some(doc) = doc
                && let Some(take) = take_by_id(doc, app.ui.script.selected_take)
            {
                let ids = selected_take_lines(doc, &app.ui.script, take);
                if !ids.is_empty() {
                    let mut updated = take.line_colors.clone();
                    updated.retain(|entry| !ids.contains(&entry.line));
                    for line in ids {
                        if app.ui.script.paint_color != take.color {
                            updated.push(filmcraft_project::ScriptLineColor { line, color: app.ui.script.paint_color });
                        }
                    }
                    actions.push(Action::Command(
                        "script.setTakeStyle",
                        json!({"item": app.ui.script.item, "take": take.id.0,
                        "lineColors": updated.iter().map(|entry| json!({"line":entry.line.0,"color":entry.color.name()})).collect::<Vec<_>>()}),
                    ));
                }
            }
            if let Some(doc) = doc {
                for (key, label, focus, tooltip) in
                    [("scene.add", "S#", 1, tl!("Add or edit Scene on the selected line")), ("page.add", "P#", 2, tl!("Add or edit Page on the selected line"))]
                {
                    let response = ui.add_enabled(current_line(doc, &app.ui.script).is_some(), egui::Button::new(label)).on_hover_text(tooltip);
                    app.auto.add(&format!("script.{key}"), response.rect, tooltip);
                    if response.clicked()
                        && let Some(line) = current_line(doc, &app.ui.script)
                    {
                        app.ui.script.label_line = Some(line.id.0);
                        app.ui.script.scroll_to_line = Some(line.id.0);
                        app.ui.script.label_focus = Some(focus);
                        if focus == 1 {
                            app.ui.script.scene_text = line.scene.clone().unwrap_or_default();
                            app.ui.script.edit_scene = Some(line.id.0);
                            app.ui.script.edit_page = None;
                        } else {
                            app.ui.script.page_text = line.page.clone().unwrap_or_default();
                            app.ui.script.edit_page = Some(line.id.0);
                            app.ui.script.edit_scene = None;
                        }
                    }
                }
                for (key, caption, scene) in [("scene", "Sc:", true), ("page", "Pg:", false)] {
                    ui.label(caption);
                    // Labels are navigation points: repeated numbers belong to
                    // distinct script rows and must remain separate options.
                    let values: Vec<_> = doc
                        .lines
                        .iter()
                        .filter_map(|line| {
                            let value = if scene { line.scene.as_deref() } else { line.page.as_deref() }?.trim();
                            (!value.is_empty()).then_some((value, line))
                        })
                        .collect();
                    let selected_line = current_line(doc, &app.ui.script).and_then(|line| doc.line_index(line.id)).and_then(|index| {
                        doc.lines[..=index]
                            .iter()
                            .rev()
                            .find(|line| (if scene { line.scene.as_deref() } else { line.page.as_deref() }).is_some_and(|value| !value.trim().is_empty()))
                    });
                    let selected = selected_line.and_then(|line| if scene { line.scene.as_deref() } else { line.page.as_deref() });
                    let combo =
                        egui::ComboBox::from_id_salt(format!("script.{key}.combo")).selected_text(selected.unwrap_or("—")).width(44.0).show_ui(ui, |ui| {
                            for (value, line) in &values {
                                let response = ui.selectable_label(selected_line.is_some_and(|selected| selected.id == line.id), *value);
                                app.auto.add(&format!("script.{key}.option.{}", line.id.0), response.rect, value);
                                if response.clicked() {
                                    select_line(&mut app.ui.script, line, false);
                                    app.ui.script.scroll_to_line = Some(line.id.0);
                                }
                            }
                        });
                    app.auto.add(&format!("script.{key}.combo"), combo.response.rect, &tlf!("Jump to {caption} label", caption));
                }
                if let (Some(line), Some(take)) = (current_line(doc, &app.ui.script), take_by_id(doc, app.ui.script.selected_take)) {
                    let in_candle = doc
                        .line_range(take.start_line, take.end_line)
                        .is_some_and(|(start, end)| doc.line_index(line.id).is_some_and(|current| (start..=end).contains(&current)));
                    let can_mark = take.source.is_some() && in_candle;
                    let mark = ui
                        .add_enabled(can_mark, egui::Button::new("  ").min_size(vec2(29.0, 23.0)))
                        .on_hover_text(tl!("Add a mark at the Source playhead; cues the selected candle's source if needed"));
                    app.auto.add("script.mark.add", mark.rect, "Mark this line at the Source playhead");
                    let center = mark.rect.center();
                    let icon_color = if can_mark { t.text } else { t.text_dim };
                    ui.painter().add(egui::Shape::convex_polygon(
                        vec![center + vec2(0.0, -5.5), center + vec2(5.5, 0.0), center + vec2(0.0, 5.5), center + vec2(-5.5, 0.0)],
                        icon_color,
                        Stroke::NONE,
                    ));
                    if mark.clicked() {
                        actions.push(Action::AddMarkAtSelection);
                    }
                    let ids = selected_take_lines(doc, &app.ui.script, take);
                    let all_offscreen = !ids.is_empty() && ids.iter().all(|id| take.offscreen_lines.contains(id));
                    let offscreen = ui
                        .add_enabled(!ids.is_empty(), egui::Button::new("  ").min_size(vec2(29.0, 23.0)).selected(all_offscreen))
                        .on_hover_text(tl!("Mark or clear the selected candle rows as off-screen"));
                    app.auto.add("script.offscreen.toggle", offscreen.rect, "Toggle off-screen passage");
                    let center = offscreen.rect.center();
                    ui.painter().add(egui::Shape::line(
                        vec![center + vec2(0.0, -6.0), center + vec2(-3.0, -3.0), center + vec2(3.0, 0.0), center + vec2(-3.0, 3.0), center + vec2(0.0, 6.0)],
                        Stroke::new(2.5, if ids.is_empty() { t.text_dim } else { t.text }),
                    ));
                    if offscreen.clicked() {
                        actions.push(Action::ToggleOffscreenSelection);
                    }
                }
            }
        })
    });
    let mut search = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(8.0, 39.0), vec2((rect.width() - 16.0).max(1.0), 30.0))));
    search.horizontal(|ui| {
        ui.label(tl!("Find in Script"));
        let response = ui.add(egui::TextEdit::singleline(&mut app.ui.script.search).hint_text(tl!("Search text")).desired_width(140.0));
        app.auto.add_response("script.search", &response, "Find in Script");
        if let Some(doc) = doc {
            let query_changed =
                response.changed() || app.ui.script.search_cache_query != app.ui.script.search || app.ui.script.search_cache_item != app.ui.script.item;
            if query_changed || app.ui.script.search_cache_doc.as_ref().is_none_or(|cached| !Arc::ptr_eq(cached, doc)) {
                let query = app.ui.script.search.to_lowercase();
                app.ui.script.search_results = if query.is_empty() {
                    Vec::new()
                } else {
                    doc.lines.iter().filter(|line| line.text.to_lowercase().contains(&query)).map(|line| line.id.0).collect()
                };
                app.ui.script.search_cache_item = app.ui.script.item;
                app.ui.script.search_cache_doc = Some(Arc::clone(doc));
                app.ui.script.search_cache_query = app.ui.script.search.clone();
                if query_changed {
                    app.ui.script.search_result_index = 0;
                    if let Some(&id) = app.ui.script.search_results.first()
                        && let Some(line) = doc.lines.iter().find(|line| line.id.0 == id)
                    {
                        select_line(&mut app.ui.script, line, false);
                        app.ui.script.scroll_to_line = Some(id);
                    } else if !query.is_empty() {
                        app.ui.script.selected_lines = None;
                    }
                } else {
                    app.ui.script.search_result_index = app.ui.script.search_result_index.min(app.ui.script.search_results.len().saturating_sub(1));
                }
            }
            let count = app.ui.script.search_results.len();
            if !app.ui.script.search.is_empty() {
                let previous = ui.add_enabled(count > 0, egui::Button::new("‹")).on_hover_text(tl!("Previous match (Shift+Enter)"));
                app.auto.add("script.search.previous", previous.rect, "Previous search match");
                let next = ui.add_enabled(count > 0, egui::Button::new("›")).on_hover_text(tl!("Next match (Enter)"));
                app.auto.add("script.search.next", next.rect, "Next search match");
                let enter = response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                if count > 0 && (previous.clicked() || next.clicked() || enter) {
                    let backwards = previous.clicked() || enter && ui.input(|input| input.modifiers.shift);
                    let index = &mut app.ui.script.search_result_index;
                    *index = if backwards { (*index + count - 1) % count } else { (*index + 1) % count };
                    let id = app.ui.script.search_results[*index];
                    if let Some(line) = doc.lines.iter().find(|line| line.id.0 == id) {
                        select_line(&mut app.ui.script, line, false);
                        app.ui.script.scroll_to_line = Some(id);
                    }
                }
                let status = if count == 0 {
                    tl!("No matches").to_owned()
                } else {
                    tlf!("{current} of {count}", current = app.ui.script.search_result_index + 1, count)
                };
                let label = ui.label(status);
                app.auto.add("script.search.count", label.rect, "Search result count");
            }
        }
        if let Some(source) = app.ui.script.find_source {
            let matches = source_matches(app, ItemId(source));
            let chooser =
                egui::ComboBox::from_id_salt("script.findSource").selected_text(tlf!("Choose linked take ({count})", count = matches.len())).width(190.0);
            chooser.show_ui(ui, |ui| {
                for entry in matches {
                    let response = ui.selectable_label(false, &entry.label);
                    app.auto.add(&format!("script.findSource.{}.{}", entry.item.0, entry.take), response.rect, &entry.label);
                    if response.clicked() {
                        select_source_match(&mut app.ui.script, &entry);
                        app.ui.script.find_source = None;
                    }
                }
            });
        }
        if let Some(line) = doc.and_then(|doc| current_line(doc, &app.ui.script)) {
            ui.label(tlf!(
                "Line {current} of {total}",
                current = doc.map_or(0, |d| d.lines.iter().position(|candidate| candidate.id == line.id).map_or(0, |index| index + 1)),
                total = doc.map_or(0, |d| d.lines.len())
            ));
        }
    });
    ui.painter().line_segment([pos2(rect.min.x, rect.max.y - 1.0), pos2(rect.max.x, rect.max.y - 1.0)], Stroke::new(1.0, t.separator));
}

/// The selected take's keyboard coverage commands stay active without a second toolbar row.
pub(super) fn coverage_hotkeys(app: &mut FilmcraftApp, ui: &egui::Ui, doc: &ScriptDocument, actions: &mut Vec<Action>) {
    if app.ui.focused == PanelKind::Script
        && !ui.ctx().text_edit_focused()
        && let Some(take) = take_by_id(doc, app.ui.script.selected_take)
    {
        let move_key = ui.input(|input| {
            input.events.iter().find_map(|event| {
                let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { return None };
                if modifiers.alt && !modifiers.command && !modifiers.ctrl {
                    match key {
                        egui::Key::ArrowUp => Some((!modifiers.shift, -1_i32)),
                        egui::Key::ArrowDown => Some((!modifiers.shift, 1_i32)),
                        _ => None,
                    }
                } else {
                    None
                }
            })
        });
        if let Some((top, delta)) = move_key {
            let start = doc.lines.iter().position(|line| line.id == take.start_line);
            let end = doc.lines.iter().position(|line| line.id == take.end_line);
            if let (Some(start), Some(end)) = (start, end) {
                let idx = if top { start } else { end };
                let next = if delta < 0 { idx.checked_sub(1) } else { idx.checked_add(1) };
                if let Some(line) = next.filter(|n| *n < doc.lines.len() && if top { *n <= end } else { *n >= start }).and_then(|n| doc.lines.get(n)) {
                    actions.push(Action::Command(
                        "script.setCoverage",
                        json!({"item": app.ui.script.item, "take": take.id.0,
                        "startLine": if top { line.id.0 } else { take.start_line.0 },
                        "endLine": if top { take.end_line.0 } else { line.id.0 }}),
                    ));
                }
            }
        }
    }
}
