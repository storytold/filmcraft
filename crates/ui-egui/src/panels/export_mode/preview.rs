use super::settings::{DragOptions, combo, drag};
use super::{
    Align2, CUSTOM, Color32, ExportUi, FilmcraftApp, FrameKey, Rect, Reg, Scaling, Sense, Target, Tick, Tokens, export_params, format_bytes, json, output_path,
    pos2, vec2,
};

// ---------------------------------------------------------------------------------------------
// right: preview, range, summary, buttons
// ---------------------------------------------------------------------------------------------

pub(crate) const RANGES: [(&str, &str); 4] = [("entire", "Entire Source"), ("inOut", "Source In/Out"), ("workArea", "Work Area"), ("custom", "Custom")];

pub(super) fn preview_column(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, seq_id: filmcraft_engine::project::ItemId) {
    let t = app.tokens;
    let mut reg = Reg::default();
    let Some(q) = app.session.active_sequence().cloned() else { return };
    let rate = q.settings.frame_rate;
    // preview (aspect of the output frame)
    let r = app.ui.export.settings.resolve(q.settings.width, q.settings.height, rate, q.settings.sample_rate);
    let pv_area = Rect::from_min_size(rect.min + vec2(14.0, 14.0), vec2(rect.width() - 28.0, (rect.height() * 0.42).max(160.0)));
    let pic = crate::panels::monitor::fit(pv_area, r.width as f32, r.height as f32);
    ui.painter().rect_filled(pic, 0.0, Color32::BLACK);
    if app.ui.export.settings.has_video() {
        let frame = rate.frame_at(app.session.playhead());
        let key = FrameKey { target: Target::Sequence(seq_id), frame, size: 500, revision: app.session.revision, draft: false };
        let project = app.session.project.clone();
        app.frames.request(key, rate.tick_of(frame), 0.5, &project, 0);
        if let Some(img) = app.frames.get(&key) {
            let ctx = ui.ctx().clone();
            let tex = app.texture_for(&ctx, "export-preview", key, &img);
            let inner = match app.ui.export.settings.scaling {
                Scaling::StretchToFill => pic,
                Scaling::ScaleToFit => crate::panels::monitor::fit(pic, q.settings.width as f32, q.settings.height as f32),
                Scaling::ScaleToFill => {
                    let s = (pic.width() / q.settings.width as f32).max(pic.height() / q.settings.height as f32);
                    Rect::from_center_size(pic.center(), vec2(q.settings.width as f32 * s, q.settings.height as f32 * s))
                }
            };
            ui.painter().with_clip_rect(pic).image(tex, inner, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    } else {
        ui.painter().text(pic.center(), Align2::CENTER_CENTER, tl!("Audio only"), Tokens::ui(13.0), t.text_dim);
    }
    reg.add("export.preview", pic, "Preview");
    // range + scaling
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(rect.min.x + 14.0, pv_area.max.y + 8.0), pos2(rect.max.x - 14.0, rect.max.y - 50.0)))
            .id_salt("export-right"),
    );
    let ex = &mut app.ui.export;
    let has_work_area = q.work_area.is_some();
    child.horizontal(|ui| {
        ui.label(egui::RichText::new(tl!("Range")).color(t.text_dim));
        let cur = RANGES.iter().find(|(k, _)| *k == ex.range).map(|x| x.1).unwrap_or("Entire Source");
        let labels: Vec<(String, bool)> = RANGES.iter().map(|(k, l)| (l.to_string(), *k != "workArea" || has_work_area)).collect();
        if let Some(i) = combo(ui, &mut reg, "export.range", cur, &labels, 140.0) {
            ex.range = RANGES[i].0.into();
        }
        if ex.range == "custom" {
            drag(ui, &mut reg, "export.range.start", &mut ex.custom_start, DragOptions { range: 0.0..=86_400.0, speed: 0.04, suffix: " s", decimals: 2 });
            drag(ui, &mut reg, "export.range.end", &mut ex.custom_end, DragOptions { range: 0.0..=86_400.0, speed: 0.04, suffix: " s", decimals: 2 });
        }
    });
    if ex.settings.has_video() {
        child.horizontal(|ui| {
            ui.label(egui::RichText::new(tl!("Scaling")).color(t.text_dim));
            let o =
                [(Scaling::ScaleToFit, tl!("Scale To Fit")), (Scaling::ScaleToFill, tl!("Scale To Fill")), (Scaling::StretchToFill, tl!("Stretch To Fill"))];
            let cur = o.iter().find(|(x, _)| *x == ex.settings.scaling).map_or(o[0].1, |x| x.1);
            let labels: Vec<(String, bool)> = o.iter().map(|(_, l)| (l.to_string(), true)).collect();
            if let Some(i) = combo(ui, &mut reg, "export.scaling", cur, &labels, 140.0) {
                ex.settings.scaling = o[i].0;
            }
        });
    }
    // summary
    let range = range_ticks(ex, &q);
    let sum = ex.settings.summary(q.settings.width, q.settings.height, rate, q.settings.sample_rate, range.1 - range.0);
    child.add_space(8.0);
    let resp = egui::Frame::NONE
        .fill(t.field_bg)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::same(10))
        .show(&mut child, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(tl!("Summary")).strong());
            egui::Grid::new("export-summary-grid").num_columns(2).spacing(vec2(10.0, 3.0)).show(ui, |ui| {
                let line = |ui: &mut egui::Ui, k: &str, v: &str| {
                    ui.label(egui::RichText::new(crate::i18n::t(k)).color(t.text_dim).size(11.5));
                    ui.add(egui::Label::new(egui::RichText::new(v).size(11.5)).wrap_mode(egui::TextWrapMode::Wrap).halign(egui::Align::Min));
                    ui.end_row();
                };
                line(ui, "Output", &output_path(ex));
                line(ui, "", &sum.format);
                line(ui, "", &sum.video);
                line(ui, "", &sum.audio);
                line(
                    ui,
                    "Source",
                    &tlf!(
                        "Sequence, {w}x{h}, {fps} fps, {rate} Hz",
                        w = q.settings.width,
                        h = q.settings.height,
                        fps = rate.label(),
                        rate = q.settings.sample_rate
                    ),
                );
                line(
                    ui,
                    "Range",
                    &format!(
                        "{} ({:.2} s)",
                        crate::i18n::t(RANGES.iter().find(|(k, _)| *k == ex.range).map(|x| x.1).unwrap_or("")),
                        (range.1 - range.0).seconds()
                    ),
                );
            });
        })
        .response;
    reg.add("export.summary", resp.rect, sum.video.clone());
    child.add_space(4.0);
    let est = child.label(egui::RichText::new(tlf!("Estimated file size: {size}", size = format_bytes(sum.estimated_bytes))).color(t.text_dim));
    reg.add("export.estimate", est.rect, sum.estimated_size.clone());
    // buttons
    let go = Rect::from_min_size(pos2(rect.max.x - 120.0, rect.max.y - 44.0), vec2(104.0, 30.0));
    let queue = Rect::from_min_size(pos2(go.min.x - 132.0, go.min.y), vec2(122.0, 30.0));
    let qresp = ui.interact(queue, egui::Id::unique("export-queue-send"), Sense::click());
    ui.painter().rect_stroke(queue, 15.0, egui::Stroke::new(1.0, if qresp.hovered() { t.text } else { t.text_dim }), egui::StrokeKind::Inside);
    ui.painter().text(queue.center(), Align2::CENTER_CENTER, tl!("Send to Queue"), Tokens::semibold(12.5), t.text);
    reg.add("export.sendToQueue", queue, "Send to Queue");
    let resp = ui.interact(go, egui::Id::unique("export-go"), Sense::click());
    ui.painter().rect_filled(go, 15.0, if resp.hovered() { t.accent_hover } else { t.accent });
    ui.painter().text(go.center(), Align2::CENTER_CENTER, tl!("Export"), Tokens::semibold(13.0), Color32::WHITE);
    reg.add("export.button", go, "Export");
    reg.flush(app);
    if resp.clicked() {
        let r = app.session.execute("file.exportMedia", export_params(app));
        match r {
            Ok(v) => app.ui.status = tlf!("Exporting {path}", path = v["path"].as_str().unwrap_or("")),
            Err(e) => app.ui.status = e.to_string(),
        }
    }
    if qresp.clicked() {
        let mut p = export_params(app);
        if app.ui.export.preset != CUSTOM {
            // the queue shows which preset an item came from
            p["preset"] = json!(app.ui.export.preset);
        }
        match app.session.execute("export.queue.add", p) {
            Ok(_) => app.ui.status = tl!("Added to the export queue").into(),
            Err(e) => app.ui.status = e.to_string(),
        }
    }
}

/// The export range in sequence ticks (for the summary).
fn range_ticks(ex: &ExportUi, q: &filmcraft_engine::project::Sequence) -> (Tick, Tick) {
    let fd = q.settings.frame_rate.frame_duration();
    let end = q.duration().max(fd);
    match ex.range.as_str() {
        "inOut" => {
            let a = q.mark_in.unwrap_or(Tick::ZERO);
            (a, q.mark_out.map(|o| o + fd).unwrap_or(end).max(a + fd))
        }
        "workArea" => q.work_area.map(|w| (w.start, w.end())).unwrap_or((Tick::ZERO, end)),
        "custom" => (Tick::from_seconds_f64(ex.custom_start.max(0.0)), Tick::from_seconds_f64(ex.custom_end.max(ex.custom_start + 0.001))),
        _ => (Tick::ZERO, end),
    }
}
