//! Native controls for AI masking, inside Opacity's existing mask editor.

use egui::{Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_engine::ai_masks::{Pick, RefineView};
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::ClipId;
use serde_json::{Value, json};

use crate::{FilmcraftApp, theme::Tokens};

fn row(ui: &mut egui::Ui, body: Rect) -> egui::Ui {
    let (r, _) = ui.allocate_exact_size(vec2(body.width(), 23.0), Sense::hover());
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(4.0, 2.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    child.style_mut().override_font_id = Some(Tokens::ui(11.0));
    child.spacing_mut().interact_size.y = 18.0;
    child.set_clip_rect(child.clip_rect().intersect(r));
    child
}

pub fn effect_rows(app: &mut FilmcraftApp, ui: &mut egui::Ui, body: Rect, clip: ClipId, actions: &mut Vec<(String, Value)>) {
    let draft = app.session.state.ai_mask.clone().filter(|d| d.clip == clip);
    let mut line = row(ui, body);
    line.label("AI Rotoscope");
    let button = line.add_enabled(app.session.ai_mask_backend.is_some(), egui::Button::new("Select object"));
    app.auto.add("aiMask.select", button.rect, "Select object");
    if button.clicked() {
        app.ui.mask_pen = None;
        actions.push(("masks.ai.select".into(), json!({"clip":clip.0})));
        app.ui.status = "Click the object in the Program monitor; use Exclude for background points".into();
    }
    if app.session.ai_mask_backend.is_none() {
        button.on_hover_text("Start run-editor.sh to prepare the native Rust models");
    }
    let Some(draft) = draft else { return };
    let clear = line.button("Clear points").on_hover_text("Remove all points; Cmd/Ctrl-Z restores them. Option/Alt-click a dot to remove one.");
    app.auto.add("aiMask.clear", clear.rect, "Clear points");
    if clear.clicked() {
        actions.push(("masks.ai.select".into(), json!({"clip":clip.0,"clear":true})));
    }

    let running = filmcraft_engine::ai_masks::active_job(&app.session, clip).map(|j| j.id);
    let mut tools = row(ui, body);
    tools.add_enabled_ui(running.is_none(), |ui| {
        for (name, kind) in [("Include", Pick::Foreground), ("Exclude", Pick::Background), ("Refine Edge", Pick::Refine), ("Done", Pick::Off)] {
            let resp = ui.selectable_label(draft.pick == kind, name);
            app.auto.add(&format!("aiMask.{kind:?}"), resp.rect, name);
            if resp.clicked() {
                actions.push(("masks.ai.select".into(), json!({"clip":clip.0,"kind":kind})));
            }
        }
    });
    let mut modes = row(ui, body);
    modes.add_enabled_ui(running.is_none(), |ui| {
        for (name, quality) in [("Fast", false), ("Quality", true)] {
            let resp = ui.selectable_label(draft.quality == quality, name);
            app.auto.add(&format!("aiMask.{name}"), resp.rect, name);
            if resp.clicked() {
                actions.push(("masks.ai.settings".into(), json!({"mode":if quality {"quality"} else {"fast"}})));
            }
        }
        if running.is_none() {
            let preview = ui.button("Preview").on_hover_text(
                "Include shows green; Exclude also shows red. Refine Edge runs the matting model after each stroke. Alpha shows its transparency; Composite shows the cutout.",
            );
            app.auto.add("aiMask.preview", preview.rect, "Preview frame");
            if preview.clicked() {
                actions.push(("masks.ai.preview".into(), json!({})));
            }
        }
        let generate = ui.button("Generate");
        app.auto.add("aiMask.generate", generate.rect, "Generate");
        if generate.clicked() {
            actions.push(("masks.ai.generate".into(), json!({})));
        }
    });
    if let Some(id) = running {
        modes.spinner();
        let cancel = modes.button("Cancel");
        app.auto.add("aiMask.cancel", cancel.rect, "Cancel");
        if cancel.clicked() {
            actions.push(("jobs.cancel".into(), json!({"job":id})));
        }
    }
    if draft.pick == Pick::Refine {
        let mut views = row(ui, body);
        for (label, view) in [("Edge region", RefineView::Region), ("Alpha", RefineView::Alpha), ("Composite", RefineView::Composite)] {
            let response = views.selectable_label(draft.view == view, label);
            app.auto.add(&format!("aiMask.view.{view:?}"), response.rect, label);
            if response.clicked() {
                actions.push(("masks.ai.settings".into(), json!({"view":view})));
            }
        }
    }
    let mut options = row(ui, body);
    options.add_enabled_ui(running.is_none(), |ui| {
        ui.label("Frames");
        let mut frames = draft.frames;
        let resp = ui.add(egui::DragValue::new(&mut frames).range(1..=120).speed(1));
        app.auto.add("aiMask.frames", resp.rect, "Frames");
        if resp.changed() {
            actions.push(("masks.ai.settings".into(), json!({"frames":frames})));
        }
        ui.label("Edge band");
        let mut band = draft.band;
        let resp = ui.add(egui::DragValue::new(&mut band).range(1..=32).speed(1));
        app.auto.add("aiMask.band", resp.rect, "Edge band");
        if resp.changed() {
            actions.push(("masks.ai.settings".into(), json!({"band":band})));
        }
        ui.label(format!("{} points", draft.points.len()));
    });
    let mut note = row(ui, body);
    let preview =
        app.session.state.active_sequence.is_some_and(|seq| filmcraft_engine::ai_masks::monitor_preview(&app.session, seq, app.session.playhead()).is_some());
    let text = if running.is_some() {
        "Calculating selection…".to_string()
    } else if preview {
        match draft.pick {
            Pick::Background => "Green includes · Red excludes · Preview ready",
            Pick::Refine => "Refined alpha · Paint across edges · Preview ready",
            _ => "Green overlay · Preview ready",
        }
        .to_string()
    } else {
        format!("First {} source frames; others stay visible", draft.frames)
    };
    note.label(text).on_hover_text("Option/Alt-click a dot to remove it; Cmd/Ctrl-Z undoes the edit. Refine Edge automatically uses Quality: paint across mixed foreground/background details such as hair, then release to infer soft alpha. Edge region shows local alpha detail over the footage; Alpha shows the full matte; Composite shows the result. Strokes affect their painted frame; temporal propagation is not included yet.");
    let mut help = row(ui, body);
    help.label(if cfg!(target_os = "macos") { "Option-click dot: remove · Cmd-Z: undo" } else { "Alt-click dot: remove · Ctrl-Z: undo" });
}

/// Returns true when the AI selection tool owns Program-monitor input.
pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) -> bool {
    let Some(draft) = app.session.state.ai_mask.clone() else { return false };
    if draft.pick == Pick::Off || !app.session.state.selection.contains(&draft.clip) || frame.0 == 0 || frame.1 == 0 {
        return false;
    }
    let Some(seq) = app.session.active_sequence() else { return false };
    let Some((_, item)) = seq.find_item(draft.clip) else { return false };
    let ph = app.session.playhead();
    if ph < item.start || ph >= item.end() {
        return false;
    }
    let time = item.source_time_at(ph);
    let size = filmcraft_render::source_size(&app.session.project, item.item).unwrap_or(frame);
    let motion = filmcraft_render::motion_matrix(seq, item, size, filmcraft_render::source_par(&app.session.project, item.item), time);
    let view = Affine::translate(pic.min.x as f64, pic.min.y as f64)
        .then_apply(&Affine::scale(pic.width() as f64 / frame.0 as f64, pic.height() as f64 / frame.1 as f64));
    let to_screen = view.then_apply(&motion);
    let Some(to_clip) = to_screen.inverse() else { return false };
    let response = ui.interact(pic, egui::Id::new("ai-mask-points"), Sense::click_and_drag());
    app.auto.add("program.aiMask", pic, "AI object selection");
    let rate = app.session.source(item.item).map(|s| s.info().frame_rate()).unwrap_or_default();
    let frame_time = rate.frame_at(time);
    let pointer = response.interact_pointer_pos().or_else(|| response.hover_pos());
    // Pointer events carry the modifier that was held for the click. Synthetic control
    // input can release that modifier within the same UI pass, before i.modifiers is read.
    let modifiers = ui.input(|i| {
        i.events
            .iter()
            .rev()
            .find_map(|event| match event {
                egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: false, modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(i.modifiers)
    });
    let remove = modifiers.alt || modifiers.command || modifiers.ctrl;
    let painting = draft.pick == Pick::Refine && !remove && response.dragged_by(egui::PointerButton::Primary);
    let painter = ui.painter().with_clip_rect(pic.intersect(ui.clip_rect()));
    // Brush and trimap radii use the same processed-image pixels as inference.
    let longest = size.0.max(size.1).max(1) as f64;
    let radius = draft.band as f64 * 2.0 * longest / longest.min(720.0);
    let brush = |center: Vec2| -> Vec<Pos2> {
        (0..32)
            .map(|index| {
                let angle = index as f64 * std::f64::consts::TAU / 32.0;
                let p = to_screen.apply(Vec2::new(center.x + radius * angle.cos(), center.y + radius * angle.sin()));
                pos2(p.x as f32, p.y as f32)
            })
            .collect()
    };
    let mut hit: Option<(usize, f32, Pos2)> = None;
    for (index, point) in draft.points.iter().enumerate().filter(|(_, p)| rate.frame_at(p.time) == frame_time) {
        let p = to_screen.apply(Vec2::new(point.x, point.y));
        let position = pos2(p.x as f32, p.y as f32);
        if !pic.contains(position) {
            continue;
        }
        app.auto.add(&format!("program.aiMask.point.{index}"), Rect::from_center_size(position, vec2(20.0, 20.0)).intersect(pic), "AI selection point");
        if let Some(pointer) = pointer.filter(|p| pic.contains(*p)) {
            let distance = pointer.distance_sq(position);
            if distance <= 100.0 && hit.is_none_or(|(_, best, _)| distance <= best) {
                hit = Some((index, distance, position));
            }
        }
        if point.kind == Pick::Refine && draft.pick == Pick::Refine && painting {
            painter.add(egui::Shape::convex_polygon(brush(Vec2::new(point.x, point.y)), Color32::WHITE, Stroke::NONE));
        }
        let color = match point.kind {
            Pick::Foreground => Color32::from_rgb(70, 215, 150),
            Pick::Background => Color32::from_rgb(245, 90, 90),
            _ => Color32::from_gray(65),
        };
        painter.circle_filled(position, 4.0, color);
        painter.circle_stroke(position, 5.0, Stroke::new(1.0, Color32::WHITE));
    }
    if let Some((_, _, position)) = hit.filter(|_| response.hovered()) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        painter.circle_stroke(position, 7.0, Stroke::new(1.0, Color32::WHITE));
        response.clone().on_hover_text("Option/Alt-click or Cmd/Ctrl-click to remove this point");
        if remove {
            painter.line_segment([position - vec2(3.0, 0.0), position + vec2(3.0, 0.0)], Stroke::new(2.0, Color32::WHITE));
        }
    }
    let stroke_id = egui::Id::new("ai-refine-gesture");
    if response.drag_started_by(egui::PointerButton::Primary) {
        let gesture_frame = ui.ctx().cumulative_frame_nr();
        ui.ctx().data_mut(|data| data.insert_temp(stroke_id, gesture_frame));
    }
    let gesture = painting.then(|| ui.ctx().data(|data| data.get_temp::<u64>(stroke_id))).flatten();
    if draft.pick == Pick::Refine
        && !remove
        && let Some(pointer) = pointer.filter(|_| response.hovered() || painting)
    {
        let circle = brush(to_clip.apply(Vec2::new(pointer.x as f64, pointer.y as f64)));
        painter.add(egui::Shape::closed_line(circle.clone(), Stroke::new(3.0, Color32::BLACK)));
        painter.add(egui::Shape::closed_line(circle, Stroke::new(1.0, Color32::WHITE)));
    }
    if (response.clicked_by(egui::PointerButton::Primary) || painting)
        && let Some(Pos2 { x, y }) = response.interact_pointer_pos()
    {
        let action = if remove {
            if let Some((index, _, _)) = hit {
                Some(("masks.ai.removePoint", json!({"point":index})))
            } else if modifiers.alt && draft.pick != Pick::Refine {
                let p = to_clip.apply(Vec2::new(x as f64, y as f64));
                Some(("masks.ai.point", json!({"x":p.x,"y":p.y,"kind":"background"})))
            } else {
                None
            }
        } else {
            let p = to_clip.apply(Vec2::new(x as f64, y as f64));
            let mut params = json!({"x":p.x,"y":p.y});
            if painting {
                params["preview"] = json!(false);
            }
            if let Some(stroke) = gesture {
                params["stroke"] = json!(stroke);
            }
            Some(("masks.ai.point", params))
        };
        if let Some((command, params)) = action
            && let Err(error) = app.session.execute(command, params)
        {
            app.ui.status = error.to_string();
        }
    }
    if draft.pick == Pick::Refine
        && response.drag_stopped_by(egui::PointerButton::Primary)
        && let Err(error) = app.session.execute("masks.ai.preview", json!({}))
    {
        app.ui.status = error.to_string();
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        let _ = app.session.execute("masks.ai.select", json!({"clip":draft.clip.0,"kind":"off"}));
    }
    true
}
