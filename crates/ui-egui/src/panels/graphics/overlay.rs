use super::canvas::{anchor_screen, canvas_target, sp, to_canvas, unscale, visible_layers};
use super::handles::{
    DragKind, DragState, HandleDrag, corner_drag, corner_handles, corner_points, handle_points, is_paragraph, resize_handle_cursor, round_rect_outline,
};
use super::ime::{current_graphics_ime_target, draw_preedit, graphics_ime_target, note_graphics_ime_editor, reconcile_gfx_edit};
use super::layer_identity::{current_layer_text, current_layer_uid, layer_display_name_view};
use super::text_edit::{char_boundary_floor, text_edit, text_of};
use super::{
    Affine, ClipId, Color32, FilmcraftApp, GfxEdit, ImeTarget, ItemKind, LayerView, Pos2, Rect, Sense, Stroke, StrokeKind, Tool, Value, Vec2, eval_layer, json,
    layer_indices, pos2, selected_graphic, text_layout, vec2,
};

/// `graphics.newText` for the Type tool: point text at `at`, or paragraph text in the box from
/// `at` to `corner`.
fn new_text_action(app: &FilmcraftApp, pic: Rect, frame: (u32, u32), at: Pos2, corner: Option<Pos2>) -> (String, Value) {
    let (into, m) = canvas_target(app, pic, frame);
    let a = to_canvas(&m, at);
    let size = 100.0 * frame.1 as f64 / 1080.0;
    let mut prm = json!({"text": "", "position": [a.x, a.y], "size": size.round()});
    if let Some(b) = corner.map(|c| to_canvas(&m, c)) {
        let (w, h) = ((b.x - a.x).abs(), (b.y - a.y).abs());
        // a box too small to hold a letter was meant as a click
        if w >= 4.0 && h >= 4.0 {
            prm["position"] = json!([a.x.min(b.x), a.y.min(b.y)]);
            prm["box"] = json!([w, h]);
        }
    }
    if let Some(cl) = into {
        prm["clip"] = json!(cl.0);
    }
    ("graphics.newText#edit".into(), prm)
}

pub(crate) const TEXT_EDIT_ID: &str = "gfx-text-edit";

/// End text editing: type the marked text still being composed, clear `gfx_edit`, invalidate any
/// in-flight IME composition, surrender focus and forget the panel tag so a late commit cannot
/// reach the next editor.
pub(super) fn end_edit(app: &mut FilmcraftApp, ui: &egui::Ui) {
    commit_marked_text(app, ui.ctx());
    app.ui.gfx_edit = None;
    // Any composition that was in flight belongs to the editor that just ended; a late commit
    // must not be inserted into whatever is edited next.
    app.invalidate_ime_composition();
    ui.memory_mut(|m| m.surrender_focus(egui::Id::unique(TEXT_EDIT_ID)));
    ui.data_mut(|d| d.remove::<crate::dock::PanelKind>(egui::Id::unique(TEXT_EDIT_ID).with("panel")));
}

/// End the composer's composition. Its marked text, when it is the edit's live composition, goes
/// into the layer's text at the caret, over the selection, as one `graphics.setText`: Korean
/// commits a syllable only when the next one starts, so the last one would be lost (#2445). The
/// composer drops the input method's late commit of that text.
fn commit_marked_text(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let target = app.ui.gfx_edit.as_ref().and_then(|ed| current_graphics_ime_target(app, ed.clip, ed.layer));
    let live = target.is_some_and(|target| composing_live(app, ctx, target));
    let Some(marked) = app.ime_composer.end(ctx).filter(|_| live) else { return };
    let Some(ed) = app.ui.gfx_edit.as_ref() else { return };
    let Some(mut text) = current_layer_text(&app.session, ed.clip, ed.layer).map(str::to_string) else { return };
    let marked: String = marked.chars().filter(|c| !c.is_control()).collect();
    let (a, b) = (char_boundary_floor(&text, ed.caret.min(ed.anchor)), char_boundary_floor(&text, ed.caret.max(ed.anchor)));
    text.replace_range(a..b, &marked);
    let params = json!({"clip": ed.clip, "layer": ed.layer, "text": text, "merge": true});
    if let Err(e) = app.session.execute("graphics.setText", params) {
        app.ui.status = e.to_string();
    }
}

/// Whether the composer's marked text is `target`'s composition in `ctx`'s viewport, not one a
/// command, a control request or a caret move retired.
fn composing_live(app: &FilmcraftApp, ctx: &egui::Context, target: ImeTarget) -> bool {
    app.ime_composer_target == Some(target)
        && app.ime_composition == Some(crate::ImeComposition::Active(target))
        && app.ime_composition_viewport == Some(ctx.viewport_id())
}

/// A click on the Program monitor's empty space (the picture's, or around it): end the text edit
/// and let go of the selected graphic layers and mask (#683).
pub(crate) fn deselect_on_monitor(app: &mut FilmcraftApp, ui: &egui::Ui) {
    end_edit(app, ui);
    app.session.state.graphic_layers.clear();
    if app.session.state.selected_mask.is_some()
        && let Err(e) = app.session.execute("masks.select", json!({"none": true}))
    {
        app.ui.status = e.to_string();
    }
}

/// Draw the overlay and handle graphics tools on the Program monitor picture `pic`.
pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    // A project command may have reordered or deleted layers since the last pass. Follow the
    // edited layer (or end the edit) before anything reads `gfx_edit`.
    reconcile_gfx_edit(app);
    let t = app.tokens;
    let views = visible_layers(app, pic, frame);
    let tool = app.ui.tool;
    let accent = t.accent;
    let painter = ui.painter().with_clip_rect(pic.expand(12.0));
    for v in &views {
        let q = v.quad();
        let r = Rect::from_points(&q);
        let name = layer_display_name_view(app, v);
        app.auto.add(&format!("program.layer.{}.{}", v.clip.0, v.layer), r, &name);
    }
    let sel_clip = selected_graphic(app).map(|(c, _)| c);
    let sel_layers = app.session.state.graphic_layers.clone();
    let editing = app.ui.gfx_edit.clone();
    // ---- boxes and handles
    let graphics_tool = matches!(tool, Tool::Selection | Tool::Type | Tool::VerticalType | Tool::Rectangle | Tool::Ellipse | Tool::Pen);
    draw_boxes(app, &painter, &views, sel_clip, &sel_layers, &editing, graphics_tool);
    let id = egui::Id::unique("gfx-overlay");
    let resp = ui.interact(pic, id, if graphics_tool { Sense::click_and_drag() } else { Sense::hover() });
    let hover = resp.hover_pos();
    let mut actions: Vec<(String, Value)> = Vec::new();
    let drag_id = id.with("drag");
    let mut drag: Option<DragState> = ui.data(|d| d.get_temp(drag_id));

    // cursors
    set_hover_cursor(ui, hover, tool, &views, sel_clip, &sel_layers, &editing);

    // ---- text editing
    if let Some(ed) = editing.clone() {
        match views.iter().find(|v| v.clip.0 == ed.clip && v.layer == ed.layer && v.is_text()) {
            Some(v) => text_edit(app, ui, v, ed, &resp),
            None => end_edit(app, ui),
        }
    }
    let editing = app.ui.gfx_edit.clone();
    let edit_view = editing.as_ref().and_then(|e| views.iter().find(|v| v.clip.0 == e.clip && v.layer == e.layer));

    // ---- presses (the left button only: a middle or right drag leaves the layers alone)
    press(app, ui, &resp, graphics_tool, pic, tool, &views, sel_clip, &sel_layers, &editing, edit_view, drag_id, &mut drag, &mut actions);
    // ---- drag feedback / commit
    drag_step(app, ui, &painter, &resp, hover, pic, frame, tool, &views, drag, drag_id, &mut actions);
    // ---- clicks
    click(app, ui, &resp, graphics_tool, drag, pic, frame, tool, &views, &editing, edit_view, &mut actions);
    // ---- pen path in progress
    pen_path(app, ui, &painter, pic, frame, tool, hover, accent, &mut actions);
    // ---- run accepted actions before drawing the caret
    run_actions(app, actions);
    draw_caret(app, ui, &painter, pic, frame);
}
/// The selection boxes, resize/scale handles, corner-radius handles and anchor points of the
/// selected (or being-edited) graphic layers.
#[allow(clippy::too_many_arguments)]
fn draw_boxes(
    app: &mut FilmcraftApp,
    painter: &egui::Painter,
    views: &[LayerView],
    sel_clip: Option<ClipId>,
    sel_layers: &[usize],
    editing: &Option<GfxEdit>,
    graphics_tool: bool,
) {
    let t = app.tokens;
    let accent = t.accent;
    if graphics_tool {
        for v in views.iter().filter(|v| Some(v.clip) == sel_clip) {
            let q = v.quad();
            let selected = sel_layers.contains(&v.layer);
            let is_edit = editing.as_ref().is_some_and(|e| e.clip == v.clip.0 && e.layer == v.layer);
            // only the selected layers have a box; that of the text being typed into is red, as in Premiere
            if !(selected || is_edit) {
                continue;
            }
            painter.add(egui::Shape::closed_line(q.to_vec(), Stroke::new(1.0, if is_edit { t.danger } else { accent })));
            if selected && !is_edit {
                let paragraph = text_of(&v.spec).filter(|tp| is_paragraph(tp));
                let label = if paragraph.is_some() { "resize handle" } else { "scale handle" };
                // text the box hides: its bottom-right handle turns into a red plus
                let overflow = paragraph.is_some_and(|tp| text_layout(tp).overflow);
                for (n, c) in q.iter().enumerate() {
                    let hr = Rect::from_center_size(*c, vec2(7.0, 7.0));
                    if overflow && n == 2 {
                        painter.rect_filled(hr.expand(1.0), 0.0, t.danger);
                        painter.line_segment([*c - vec2(3.0, 0.0), *c + vec2(3.0, 0.0)], Stroke::new(1.0, Color32::WHITE));
                        painter.line_segment([*c - vec2(0.0, 3.0), *c + vec2(0.0, 3.0)], Stroke::new(1.0, Color32::WHITE));
                    } else {
                        painter.rect_filled(hr, 0.0, Color32::WHITE);
                        painter.rect_stroke(hr, 0.0, Stroke::new(1.0, accent), StrokeKind::Middle);
                    }
                    app.auto.add(&format!("program.layer.{}.{}.handle.{n}", v.clip.0, v.layer), hr.expand(3.0), label);
                }
                for (n, m) in handle_points(&q).iter().enumerate().skip(4) {
                    let hr = Rect::from_center_size(*m, vec2(5.0, 5.0));
                    painter.rect_filled(hr, 0.0, Color32::WHITE);
                    app.auto.add(&format!("program.layer.{}.{}.handle.{n}", v.clip.0, v.layer), hr.expand(4.0), label);
                }
                // a rectangle's corner-radius handles, just inside its corners
                for (n, c) in corner_handles(v).map(|(_, pts)| pts).iter().flatten().enumerate() {
                    painter.circle_filled(*c, 3.0, Color32::WHITE);
                    painter.circle_stroke(*c, 3.0, Stroke::new(1.0, accent));
                    app.auto.add(&format!("program.layer.{}.{}.corner.{n}", v.clip.0, v.layer), Rect::from_center_size(*c, vec2(12.0, 12.0)), "corner radius");
                }
                // anchor point
                let a = anchor_screen(v);
                painter.circle_stroke(a, 4.0, Stroke::new(1.0, accent));
                painter.line_segment([a - vec2(6.0, 0.0), a + vec2(6.0, 0.0)], Stroke::new(1.0, accent));
                painter.line_segment([a - vec2(0.0, 6.0), a + vec2(0.0, 6.0)], Stroke::new(1.0, accent));
                app.auto.add(&format!("program.layer.{}.{}.anchor", v.clip.0, v.layer), Rect::from_center_size(a, vec2(12.0, 12.0)), "anchor point");
            }
        }
    }
}

/// The cursor over the picture: text, crosshair, a resize/scale handle, or the move icon.
#[allow(clippy::too_many_arguments)]
fn set_hover_cursor(
    ui: &egui::Ui,
    hover: Option<Pos2>,
    tool: Tool,
    views: &[LayerView],
    sel_clip: Option<ClipId>,
    sel_layers: &[usize],
    editing: &Option<GfxEdit>,
) {
    // cursors
    if let Some(p) = hover {
        match tool {
            Tool::Type | Tool::VerticalType => ui.ctx().set_cursor_icon(egui::CursorIcon::Text),
            Tool::Rectangle | Tool::Ellipse | Tool::Pen => ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair),
            Tool::Selection => {
                let selected = || views.iter().filter(|v| Some(v.clip) == sel_clip && sel_layers.contains(&v.layer));
                let handle = selected().find_map(|v| resize_handle_cursor(v, p));
                let radius_handle = selected().find_map(|v| {
                    let centre = v.quad()[0] + (v.quad()[2] - v.quad()[0]) * 0.5;
                    corner_handles(v)?.1.iter().find(|c| (**c - p).length() <= 6.0).map(|c| crate::panels::transform_handles::resize_cursor(*c - centre))
                });
                if editing.as_ref().is_some_and(|e| views.iter().any(|v| v.clip.0 == e.clip && v.layer == e.layer && v.hit(p))) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
                } else if let Some(icon) = handle {
                    ui.ctx().set_cursor_icon(icon);
                } else if let Some(icon) = radius_handle {
                    ui.ctx().set_cursor_icon(icon);
                } else if views.iter().any(|v| v.hit(p)) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                }
            }
            _ => {}
        }
    }
}

/// Where the left button went down: start dragging a handle, an anchor or a layer, or put the
/// caret into the text under the pointer. Presses only (a middle or right drag leaves the
/// layers alone).
#[allow(clippy::too_many_arguments)]
fn press(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    resp: &egui::Response,
    graphics_tool: bool,
    pic: Rect,
    tool: Tool,
    views: &[LayerView],
    sel_clip: Option<ClipId>,
    sel_layers: &[usize],
    editing: &Option<GfxEdit>,
    edit_view: Option<&LayerView>,
    drag_id: egui::Id,
    drag: &mut Option<DragState>,
    actions: &mut Vec<(String, Value)>,
) {
    // ---- presses (the left button only: a middle or right drag leaves the layers alone)
    if graphics_tool && resp.drag_started_by(egui::PointerButton::Primary) {
        // where the button went down (a drag starts only after the pointer has moved a little)
        let p = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()).unwrap_or(pic.center());
        let shift = ui.input(|i| i.modifiers.shift);
        let in_edit = edit_view.is_some_and(|v| v.hit(p));
        *drag = None;
        if in_edit {
            if let (Some(v), Some(mut ed)) = (edit_view, editing.clone())
                && let Some(tp) = text_of(&v.spec)
            {
                let l = text_layout(tp);
                if let Some((x, y)) = v.to_local(p) {
                    let previous_selection = (ed.caret, ed.anchor);
                    ed.caret = l.hit(x, y);
                    if !shift {
                        ed.anchor = ed.caret;
                    }
                    if previous_selection != (ed.caret, ed.anchor) {
                        app.invalidate_ime_composition();
                    }
                    app.ui.gfx_edit = Some(ed);
                }
                *drag = Some(DragState { kind: DragKind::TextSelect, clip: v.clip, layer: v.layer, start: p });
            }
        } else {
            match tool {
                Tool::Selection | Tool::Type | Tool::VerticalType => {
                    // of the selected layers: the anchor point first (it can sit on a handle), then the handles
                    let selected = || views.iter().filter(|v| Some(v.clip) == sel_clip && sel_layers.contains(&v.layer));
                    let anchor = selected().find(|v| tool == Tool::Selection && (anchor_screen(v) - p).length() <= 7.0);
                    let handle = selected().find_map(|v| handle_points(&v.quad()).iter().position(|c| (*c - p).length() <= 7.0).map(|n| (v.clip, v.layer, n)));
                    let corner = selected()
                        .find_map(|v| corner_handles(v).and_then(|(_, pts)| pts.iter().position(|c| (*c - p).length() <= 6.0)).map(|n| (v.clip, v.layer, n)));
                    if let Some(v) = anchor {
                        *drag = Some(DragState { kind: DragKind::Anchor, clip: v.clip, layer: v.layer, start: p });
                    } else if let Some((c, l, n)) = handle {
                        *drag = Some(DragState { kind: DragKind::Handle(n), clip: c, layer: l, start: p });
                    } else if let Some((c, l, n)) = corner {
                        *drag = Some(DragState { kind: DragKind::Corner(n), clip: c, layer: l, start: p });
                    } else if tool == Tool::Type && !views.iter().any(|v| v.is_text() && v.hit(p)) {
                        *drag = Some(DragState { kind: DragKind::NewTextBox, clip: ClipId(0), layer: 0, start: p });
                    } else if tool == Tool::Selection
                        && let Some(v) = views.iter().find(|v| v.hit(p))
                    {
                        if editing.is_some() {
                            end_edit(app, ui);
                        }
                        if !(Some(v.clip) == sel_clip && sel_layers.contains(&v.layer)) {
                            actions.push(("graphics.selectLayer".into(), json!({"clip": v.clip.0, "layers": [v.layer]})));
                        }
                        *drag = Some(DragState { kind: DragKind::Move, clip: v.clip, layer: v.layer, start: p });
                    }
                }
                Tool::Rectangle | Tool::Ellipse => {
                    *drag = Some(DragState { kind: DragKind::NewShape, clip: ClipId(0), layer: 0, start: p });
                }
                _ => {}
            }
        }
        ui.data_mut(|dd| {
            if let Some(d) = *drag {
                dd.insert_temp(drag_id, d);
            } else {
                dd.remove::<DragState>(drag_id);
            }
        });
    }
}

/// Draw the in-flight drag (a ghost box, the moved outline, snap lines) and, when the button
/// comes up, commit it as actions.
#[allow(clippy::too_many_arguments)]
fn drag_step(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    painter: &egui::Painter,
    resp: &egui::Response,
    hover: Option<Pos2>,
    pic: Rect,
    frame: (u32, u32),
    tool: Tool,
    views: &[LayerView],
    drag: Option<DragState>,
    drag_id: egui::Id,
    actions: &mut Vec<(String, Value)>,
) {
    let t = app.tokens;
    // ---- drag feedback / commit
    if let Some(d) = drag {
        let cur = resp.interact_pointer_pos().or(hover).unwrap_or(d.start);
        let v = views.iter().find(|v| v.clip == d.clip && v.layer == d.layer);
        // Snap in Program Monitor (hold ⌘/Ctrl to move freely)
        let snap_off = match (d.kind, v) {
            (DragKind::Move, Some(v)) if !ui.input(|i| i.modifiers.command) => {
                crate::panels::monitor_view::snap_move(app, pic, frame, Rect::from_points(&v.quad()), cur - d.start)
            }
            _ => (cur - d.start, Vec::new()),
        };
        match d.kind {
            DragKind::Move => {
                if let Some(v) = v {
                    let off = snap_off.0;
                    painter.add(egui::Shape::closed_line(v.quad().iter().map(|q| *q + off).collect(), Stroke::new(1.0, Color32::WHITE)));
                    crate::panels::monitor_view::draw_snap_lines(painter, pic, &snap_off.1);
                }
            }
            DragKind::Handle(n) => {
                if let Some(v) = v {
                    let h = HandleDrag::new(v, n, d.start, cur, ui.input(|i| i.modifiers.shift));
                    painter.add(egui::Shape::closed_line(h.quad(v).to_vec(), Stroke::new(1.0, Color32::WHITE)));
                }
            }
            DragKind::Anchor => {
                if let Some(v) = v {
                    let a = anchor_screen(v) + (cur - d.start);
                    painter.circle_stroke(a, 4.0, Stroke::new(1.0, Color32::WHITE));
                    painter.line_segment([a - vec2(6.0, 0.0), a + vec2(6.0, 0.0)], Stroke::new(1.0, Color32::WHITE));
                    painter.line_segment([a - vec2(0.0, 6.0), a + vec2(0.0, 6.0)], Stroke::new(1.0, Color32::WHITE));
                }
            }
            DragKind::Corner(n) => {
                if let Some(v) = v
                    && let Some((r0, _)) = corner_handles(v)
                {
                    let r = corner_drag(v, r0, n, d.start, cur);
                    painter.add(egui::Shape::closed_line(round_rect_outline(v, r), Stroke::new(1.0, Color32::WHITE)));
                    for c in corner_points(v, r) {
                        painter.circle_stroke(c, 3.0, Stroke::new(1.0, Color32::WHITE));
                    }
                }
            }
            DragKind::NewTextBox => {
                painter.rect_stroke(Rect::from_two_pos(d.start, cur), 0.0, Stroke::new(1.0, t.danger), StrokeKind::Middle);
            }
            DragKind::NewShape => {
                let r = Rect::from_two_pos(d.start, cur);
                if tool == Tool::Ellipse {
                    painter.add(egui::Shape::ellipse_stroke(r.center(), r.size() / 2.0, Stroke::new(1.0, Color32::WHITE)));
                } else {
                    painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Middle);
                }
            }
            DragKind::TextSelect => {
                if let (Some(v), Some(mut ed)) = (v, app.ui.gfx_edit.clone())
                    && let Some(tp) = text_of(&v.spec)
                {
                    let l = text_layout(tp);
                    if let Some((x, y)) = v.to_local(cur) {
                        let previous_selection = (ed.caret, ed.anchor);
                        ed.caret = l.hit(x, y);
                        if previous_selection != (ed.caret, ed.anchor) {
                            app.invalidate_ime_composition();
                        }
                        app.ui.gfx_edit = Some(ed);
                    }
                }
            }
        }
        if resp.drag_stopped() {
            ui.data_mut(|dd| dd.remove::<DragState>(drag_id));
            // egui ends a drag on Escape: that stop abandons it (#580)
            let moved = (cur - d.start).length() > 2.0 && !ui.input(|i| i.key_pressed(egui::Key::Escape));
            match d.kind {
                DragKind::Move if moved => {
                    if let Some(v) = v {
                        let lin = Affine { e: 0.0, f: 0.0, ..v.canvas_to_screen };
                        let off = snap_off.0;
                        let delta = lin.inverse().map(|i| i.apply(Vec2::new(off.x as f64, off.y as f64))).unwrap_or_default();
                        let p0 = v.spec.transform.position;
                        actions.push((
                            "graphics.set".into(),
                            json!({"clip": v.clip.0, "layer": v.layer, "props": {"position": [p0.x + delta.x, p0.y + delta.y]}}),
                        ));
                    }
                }
                DragKind::Handle(n) if moved => {
                    if let Some(v) = v {
                        let props = HandleDrag::new(v, n, d.start, cur, ui.input(|i| i.modifiers.shift)).props(v);
                        actions.push(("graphics.set".into(), json!({"clip": v.clip.0, "layer": v.layer, "props": props})));
                    }
                }
                DragKind::Anchor if moved => {
                    if let Some(v) = v {
                        // the anchor moves over the layer; the position follows it, so the layer stays
                        let (canvas, local) = (unscale(&v.canvas_to_screen, cur - d.start), unscale(&v.to_screen, cur - d.start));
                        let (p0, a0) = (v.spec.transform.position, v.spec.transform.anchor);
                        actions.push((
                            "graphics.set".into(),
                            json!({"clip": v.clip.0, "layer": v.layer, "props": {"position": [p0.x + canvas.x, p0.y + canvas.y], "anchor": [a0.x + local.x, a0.y + local.y]}}),
                        ));
                    }
                }
                DragKind::Corner(n) if moved => {
                    if let Some(v) = v
                        && let Some((r0, _)) = corner_handles(v)
                    {
                        let r = corner_drag(v, r0, n, d.start, cur);
                        actions.push(("graphics.set".into(), json!({"clip": v.clip.0, "layer": v.layer, "props": {"corner_radius": r}})));
                    }
                }
                DragKind::NewTextBox if moved => {
                    end_edit(app, ui);
                    actions.push(new_text_action(app, pic, frame, d.start, Some(cur)));
                }
                DragKind::NewShape if moved => {
                    let (into, m) = canvas_target(app, pic, frame);
                    let (a, b) = (to_canvas(&m, d.start), to_canvas(&m, cur));
                    let c = Vec2::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                    let mut p = json!({
                        "shape": if tool == Tool::Ellipse { "ellipse" } else { "rectangle" },
                        "position": [c.x, c.y],
                        "size": [(b.x - a.x).abs(), (b.y - a.y).abs()],
                    });
                    if let Some(cl) = into {
                        p["clip"] = json!(cl.0);
                    }
                    actions.push(("graphics.newShape".into(), p));
                }
                _ => {}
            }
        }
    }
}

/// A click of the graphics tools on the picture: edit text, select a layer, add a pen point.
#[allow(clippy::too_many_arguments)]
fn click(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    resp: &egui::Response,
    graphics_tool: bool,
    drag: Option<DragState>,
    pic: Rect,
    frame: (u32, u32),
    tool: Tool,
    views: &[LayerView],
    editing: &Option<GfxEdit>,
    edit_view: Option<&LayerView>,
    actions: &mut Vec<(String, Value)>,
) {
    // ---- clicks
    if graphics_tool && resp.clicked() && drag.is_none_or(|d| !matches!(d.kind, DragKind::TextSelect)) {
        let p = resp.interact_pointer_pos().unwrap_or(pic.center());
        let in_edit = edit_view.is_some_and(|v| v.hit(p));
        match tool {
            // a click in the text being edited puts the caret there (Shift extends the selection),
            // as a press that becomes a drag already does (#2430)
            _ if in_edit => {
                if let (Some(v), Some(mut ed)) = (edit_view, editing.clone())
                    && let Some(tp) = text_of(&v.spec)
                    && let Some((x, y)) = v.to_local(p)
                {
                    let previous_selection = (ed.caret, ed.anchor);
                    ed.caret = text_layout(tp).hit(x, y);
                    if !ui.input(|i| i.modifiers.shift) {
                        ed.anchor = ed.caret;
                    }
                    if previous_selection != (ed.caret, ed.anchor) {
                        app.invalidate_ime_composition();
                    }
                    app.ui.gfx_edit = Some(ed);
                }
            }
            Tool::Type | Tool::VerticalType => {
                if let Some(v) = views.iter().find(|v| v.is_text() && v.hit(p)) {
                    start_edit_at(app, ui, v, p, actions);
                } else {
                    end_edit(app, ui);
                    let (command, prm) = new_text_action(app, pic, frame, p, None);
                    // vertical text is always point text: a click, never a drawn box
                    let command = if tool == Tool::VerticalType { "graphics.newVerticalText#edit".into() } else { command };
                    actions.push((command, prm));
                }
            }
            Tool::Selection => {
                if resp.double_clicked() {
                    if let Some(v) = views.iter().find(|v| v.is_text() && v.hit(p)) {
                        start_edit_at(app, ui, v, p, actions);
                        // double-click selects all the text so typing replaces it, as in Premiere
                        if let (Some(tp), Some(e)) = (text_of(&v.spec), app.ui.gfx_edit.as_mut()) {
                            (e.anchor, e.caret) = (0, tp.text.len());
                        }
                    }
                } else if let Some(v) = views.iter().find(|v| v.hit(p)) {
                    if app.ui.gfx_edit.is_some() {
                        end_edit(app, ui);
                    }
                    actions.push(("graphics.selectLayer".into(), json!({"clip": v.clip.0, "layers": [v.layer]})));
                } else {
                    deselect_on_monitor(app, ui);
                }
            }
            Tool::Pen => {
                let (_, m) = canvas_target(app, pic, frame);
                let c = to_canvas(&m, p);
                let close = app.ui.pen_points.len() >= 3
                    && app.ui.pen_points.first().is_some_and(|f| {
                        let fs = m.apply(Vec2::new(f[0], f[1]));
                        (pos2(fs.x as f32, fs.y as f32) - p).length() <= 8.0
                    });
                if close {
                    finish_pen(app, pic, frame, actions);
                } else {
                    app.ui.pen_points.push([c.x, c.y]);
                }
            }
            _ => {}
        }
    }
}

/// Draw the pen path being traced, and finish it on Enter (Escape drops it).
#[allow(clippy::too_many_arguments)]
fn pen_path(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    painter: &egui::Painter,
    pic: Rect,
    frame: (u32, u32),
    tool: Tool,
    hover: Option<Pos2>,
    accent: Color32,
    actions: &mut Vec<(String, Value)>,
) {
    // ---- pen path in progress
    if tool == Tool::Pen && !app.ui.pen_points.is_empty() {
        let (_, m) = canvas_target(app, pic, frame);
        let pts: Vec<Pos2> = app.ui.pen_points.iter().map(|q| m.apply(Vec2::new(q[0], q[1]))).map(|v| pos2(v.x as f32, v.y as f32)).collect();
        painter.add(egui::Shape::line(pts.clone(), Stroke::new(1.0, accent)));
        if let (Some(last), Some(h)) = (pts.last(), hover) {
            painter.line_segment([*last, h], Stroke::new(1.0, accent.gamma_multiply(0.5)));
        }
        for (i, q) in pts.iter().enumerate() {
            painter.rect_filled(Rect::from_center_size(*q, vec2(6.0, 6.0)), 0.0, if i == 0 { accent } else { Color32::WHITE });
        }
        let (enter, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if enter && app.ui.pen_points.len() >= 3 {
            finish_pen(app, pic, frame, actions);
        } else if esc {
            app.ui.pen_points.clear();
        }
    } else if tool != Tool::Pen && !app.ui.pen_points.is_empty() {
        app.ui.pen_points.clear();
    }
}

/// Run the overlay's accepted `graphics.*` commands in order; `#edit` opens the text edit on
/// the layer the command created or selected.
fn run_actions(app: &mut FilmcraftApp, actions: Vec<(String, Value)>) {
    // ---- run accepted actions before drawing the caret
    for (cmd, p) in actions {
        let (cmd, then_edit) = match cmd.strip_suffix("#edit") {
            Some(c) => (c.to_string(), true),
            None => (cmd, false),
        };
        match app.session.execute(&cmd, p) {
            Ok(r) => {
                if then_edit && let (Some(c), Some(l)) = (r["clip"].as_u64(), r["layer"].as_u64()) {
                    app.restart_ime_target();
                    let uid = current_layer_uid(&app.session, c, l as usize).unwrap_or(0);
                    app.ui.gfx_edit = Some(GfxEdit { clip: c, layer: l as usize, caret: 0, anchor: 0, uid, fingerprint: 0 });
                    app.ui.focused = crate::dock::PanelKind::Program;
                }
            }
            Err(e) => app.ui.status = e.to_string(),
        }
    }
}

/// Draw the post-action caret, selection highlight and IME preedit of the text being edited
/// (the model may already have changed), and note the editor's IME target.
fn draw_caret(app: &mut FilmcraftApp, ui: &egui::Ui, painter: &egui::Painter, pic: Rect, frame: (u32, u32)) {
    // The model may have changed above; draw the post-action caret and committed text.
    let views = visible_layers(app, pic, frame);
    if let Some(ed) = app.ui.gfx_edit.clone()
        && let Some(v) = views.iter().find(|v| v.clip.0 == ed.clip && v.layer == ed.layer)
        && let Some(tp) = text_of(&v.spec)
    {
        let l = text_layout(tp);
        let caret = char_boundary_floor(&tp.text, ed.caret);
        let anchor = char_boundary_floor(&tp.text, ed.anchor);
        for r in l.selection_rects(anchor, caret) {
            let q = [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])].map(|(x, y)| sp(&v.to_screen, x, y));
            painter.add(egui::Shape::convex_polygon(q.to_vec(), Color32::from_rgba_unmultiplied(80, 140, 255, 90), Stroke::NONE));
        }
        let (x, base, li) = l.caret(caret);
        let (ascent, descent) = l.lines.get(li.min(l.lines.len().saturating_sub(1))).map_or((0.0, 0.0), |line| (line.ascent, line.descent));
        let (a, b) = (sp(&v.to_screen, x, base - ascent), sp(&v.to_screen, x, base + descent));
        let cursor_rect = Rect::from_two_pos(a, b);
        let sequence = app.session.state.active_sequence.map_or(0, |sequence| sequence.0);
        let target = graphics_ime_target(sequence, v.clip.0, v.layer, v.spec.uid, app.ime_replacement_epoch);
        let live = composing_live(app, ui.ctx(), target);
        match app.ime_composer.preedit().filter(|_| live).map(|preedit| draw_preedit(painter, a, b, preedit)) {
            Some(drawn) => {
                app.auto.add("program.textEdit.preedit", drawn.plate, &drawn.text);
                if let Some((rect, clause)) = drawn.clause {
                    app.auto.add("program.textEdit.preedit.clause", rect, &clause);
                }
            }
            None => {
                painter.line_segment([a, b], Stroke::new(1.5, Color32::WHITE));
                painter.line_segment([a + vec2(1.0, 0.0), b + vec2(1.0, 0.0)], Stroke::new(0.5, Color32::BLACK));
            }
        }
        let er = Rect::from_points(&v.quad()).union(cursor_rect);
        app.auto.add("program.textEdit", er, &tp.text);
        // `drain_ui_requests` later in this pass may run a command or replace `gfxEdit`:
        // `finish_graphics_ime` checks the post-drain state against this one.
        note_graphics_ime_editor(app, ui, v, cursor_rect);
    }
}

fn finish_pen(app: &mut FilmcraftApp, pic: Rect, frame: (u32, u32), actions: &mut Vec<(String, Value)>) {
    let pts = std::mem::take(&mut app.ui.pen_points);
    if pts.len() < 3 {
        return;
    }
    let n = pts.len() as f64;
    let (cx, cy) = (pts.iter().map(|p| p[0]).sum::<f64>() / n, pts.iter().map(|p| p[1]).sum::<f64>() / n);
    let (into, _) = canvas_target(app, pic, frame);
    let mut p = json!({
        "shape": "path",
        "position": [cx, cy],
        "points": pts.iter().map(|q| [q[0] - cx, q[1] - cy]).collect::<Vec<_>>(),
    });
    if let Some(c) = into {
        p["clip"] = json!(c.0);
    }
    actions.push(("graphics.newShape".into(), p));
}

/// Start editing a text layer at screen point `p` (caret at the text hit, else the end), restart the IME target, record clip/layer/uid and select the layer.
fn start_edit_at(app: &mut FilmcraftApp, ui: &egui::Ui, v: &LayerView, p: Pos2, actions: &mut Vec<(String, Value)>) {
    let Some(tp) = text_of(&v.spec) else { return };
    let l = text_layout(tp);
    let caret = v.to_local(p).map_or(tp.text.len(), |(x, y)| l.hit(x, y));
    // the marked text still being composed goes into the layer it was typed into
    commit_marked_text(app, ui.ctx());
    app.restart_ime_target();
    app.ui.gfx_edit = Some(GfxEdit { clip: v.clip.0, layer: v.layer, caret, anchor: caret, uid: v.spec.uid, fingerprint: 0 });
    app.ui.focused = crate::dock::PanelKind::Program;
    actions.push(("graphics.selectLayer".into(), json!({"clip": v.clip.0, "layers": [v.layer]})));
}

/// Edit ▸ Select All while text is edited on the monitor selects all of that text, not the
/// timeline clips: on macOS the native menu takes Cmd+A before the text sees the key (#1102).
/// False when no text is being edited.
pub(crate) fn select_all_edited_text(app: &mut FilmcraftApp) -> bool {
    let Some((clip, layer)) = app.ui.gfx_edit.as_ref().map(|e| (e.clip, e.layer)) else { return false };
    let t = app.session.playhead();
    let Some((_, it)) = app.session.active_sequence().and_then(|q| q.find_item(ClipId(clip))) else { return false };
    let Some(ItemKind::Graphic { width, height, .. }) = app.session.project.item(it.item).map(|p| &p.kind) else { return false };
    let Some(effect) = layer_indices(&it.effects).get(layer).and_then(|&i| it.effects.get(i)) else { return false };
    let Some(len) = eval_layer(effect, it.source_time_at(t), (*width, *height)).as_ref().and_then(text_of).map(|tp| tp.text.len()) else { return false };
    let Some(ed) = app.ui.gfx_edit.as_mut() else { return false };
    let moved = (ed.anchor, ed.caret) != (0, len);
    (ed.anchor, ed.caret) = (0, len);
    if moved {
        app.invalidate_ime_composition();
    }
    true
}
