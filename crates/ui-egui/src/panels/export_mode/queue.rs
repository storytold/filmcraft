use super::{Align2, CUSTOM, Color32, FilmcraftApp, MEDIA_FILE, Rect, Reg, Sense, Tokens, Value, YOUTUBE, json, pos2, vec2};

// ---------------------------------------------------------------------------------------------
// left: destinations + queue
// ---------------------------------------------------------------------------------------------

pub(super) fn queue_column(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut reg = Reg::default();
    ui.painter().text(rect.min + vec2(14.0, 20.0), Align2::LEFT_CENTER, tl!("Destinations"), Tokens::semibold(13.0), t.text);
    // Media File, and YouTube where this build can publish: a click shows a destination's
    // settings in the middle, YouTube's box says whether Export uploads too
    let youtube = super::youtube::available(app);
    let on_youtube = youtube && app.ui.export.destination == YOUTUBE;
    let fill = |selected: bool, hovered: bool| {
        if selected {
            t.row_selected
        } else if hovered {
            t.hover
        } else {
            Color32::TRANSPARENT
        }
    };
    let mf = Rect::from_min_size(rect.min + vec2(8.0, 38.0), vec2(rect.width() - 16.0, 26.0));
    let r = ui.interact(mf, egui::Id::unique("export-destination-media"), Sense::click());
    ui.painter().rect_filled(mf, 4.0, fill(!on_youtube, r.hovered()));
    ui.painter().text(pos2(mf.min.x + 10.0, mf.center().y), Align2::LEFT_CENTER, tl!("Media File"), Tokens::ui(12.5), t.text);
    reg.add("export.destination.mediaFile", mf, "Media File");
    if r.clicked() {
        app.ui.export.destination = MEDIA_FILE.into();
    }
    let mut qtop = mf.max.y + 18.0;
    if youtube {
        let yt = mf.translate(vec2(0.0, mf.height() + 4.0));
        let r = ui.interact(yt, egui::Id::unique("export-destination-youtube"), Sense::click());
        ui.painter().rect_filled(yt, 4.0, fill(on_youtube, r.hovered()));
        let cb = Rect::from_min_size(pos2(yt.min.x + 6.0, yt.center().y - 9.0), vec2(18.0, 18.0));
        let mut on = app.ui.export.youtube.enabled;
        let c = crate::widgets::accessible_name(ui.put(cb, egui::Checkbox::without_text(&mut on)), tl!("Upload to YouTube"));
        reg.add("export.destination.youtube.enabled", c.rect, "Upload to YouTube");
        ui.painter().text(pos2(cb.max.x + 8.0, yt.center().y), Align2::LEFT_CENTER, tl!("YouTube"), Tokens::ui(12.5), t.text);
        reg.add("export.destination.youtube", yt, "YouTube");
        if c.changed() {
            app.ui.export.youtube.enabled = on;
            if on {
                app.ui.export.destination = YOUTUBE.into();
            }
        } else if r.clicked() {
            app.ui.export.destination = YOUTUBE.into();
        }
        qtop = yt.max.y + 18.0;
    }
    let n = app.session.export_queue.items.len();
    let p = ui.painter();
    p.text(pos2(rect.min.x + 14.0, qtop), Align2::LEFT_CENTER, tlf!("Queue ({n})", n), Tokens::semibold(13.0), t.text);
    let body = Rect::from_min_max(pos2(rect.min.x + 8.0, qtop + 14.0), pos2(rect.max.x - 8.0, rect.max.y - 40.0));
    let items: Vec<Value> = app.session.execute("export.queue.list", json!({})).ok().and_then(|v| v["items"].as_array().cloned()).unwrap_or_default();
    let now = app.now();
    let encoding: std::collections::HashMap<u64, String> = items
        .iter()
        .filter(|it| it["status"] == "encoding")
        .map(|it| {
            let id = it["id"].as_u64().unwrap_or(0);
            let text = app.steady_texts.get(&format!("queue.{id}"), now, || {
                tlf!("Encoding {n}%", n = format!("{:.0}", it["progress"].as_f64().unwrap_or(0.0) * 100.0)) + &crate::panels::eta_suffix(it)
            });
            (id, text)
        })
        .collect();
    let mut action: Option<(&'static str, Value)> = None;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("export-queue"));
    egui::ScrollArea::vertical().id_salt("export-queue-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        if items.is_empty() {
            ui.label(egui::RichText::new(tl!("Send exports here with Send to Queue, then start the queue.")).color(t.text_dim).size(11.5));
        }
        for (i, it) in items.iter().enumerate() {
            let id = it["id"].as_u64().unwrap_or(0);
            let status = it["status"].as_str().unwrap_or("");
            let frame = egui::Frame::NONE.fill(if i % 2 == 0 { t.row_alt } else { Color32::TRANSPARENT }).inner_margin(egui::Margin::same(6));
            let r = frame
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let file = std::path::Path::new(it["path"].as_str().unwrap_or("")).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    ui.label(egui::RichText::new(format!("{} → {file}", it["sequenceName"].as_str().unwrap_or(""))).size(12.0).color(t.text));
                    let preset = it["preset"].as_str().filter(|s| !s.is_empty()).unwrap_or(CUSTOM);
                    let (label, col) = match status {
                        "ready" => (tl!("Ready").to_string(), t.text_dim),
                        "encoding" => (encoding.get(&id).cloned().unwrap_or_default(), t.accent),
                        "done" => (tl!("Done").to_string(), t.render_green),
                        "failed" => (tlf!("Failed: {e}", e = it["error"].as_str().unwrap_or("")), t.danger),
                        _ => (tl!("Cancelled").to_string(), t.text_faint),
                    };
                    if status == "encoding" {
                        // the progress and the time left on a line of their own: the column is narrow, and a wrapped "· 15 s left" reads badly
                        ui.label(egui::RichText::new(crate::i18n::t(preset)).size(11.0).color(t.text_dim));
                        // once a second, with digits of one width, so the time left doesn't shuffle the line
                        crate::widgets::tabular_label(ui, &label, &Tokens::ui(11.0), col);
                    } else {
                        ui.label(egui::RichText::new(format!("{} · {label}", crate::i18n::t(preset))).size(11.0).color(col));
                    }
                    if status == "encoding" {
                        ui.add(egui::ProgressBar::new(it["progress"].as_f64().unwrap_or(0.0) as f32).desired_height(6.0));
                    }
                    ui.horizontal(|ui| {
                        let mut b = |ui: &mut egui::Ui, key: &'static str, text: &str, enabled: bool, cmd: &'static str, params: Value| {
                            let r = ui.add_enabled(enabled, egui::Button::new(egui::RichText::new(text).size(11.0)).small());
                            reg.add(format!("export.queue.item.{id}.{key}"), r.rect, text);
                            if r.clicked() {
                                action = Some((cmd, params));
                            }
                        };
                        b(ui, "up", "Up", i > 0 && status != "encoding", "export.queue.move", json!({"id": id, "by": -1}));
                        b(ui, "down", "Down", i + 1 < items.len(), "export.queue.move", json!({"id": id, "by": 1}));
                        if matches!(status, "ready" | "encoding") {
                            b(ui, "cancel", "Cancel", true, "export.queue.cancel", json!({"id": id}));
                        } else {
                            b(ui, "retry", "Retry", true, "export.queue.retry", json!({"id": id, "start": true}));
                        }
                        b(ui, "remove", "Remove", status != "encoding", "export.queue.remove", json!({"id": id}));
                    });
                })
                .response;
            reg.add(format!("export.queue.item.{id}"), r.rect, status);
        }
    });
    // footer
    let running = app.session.export_queue.running;
    let has_ready = items.iter().any(|i| i["status"] == "ready");
    let foot = Rect::from_min_max(pos2(rect.min.x + 8.0, rect.max.y - 34.0), pos2(rect.max.x - 8.0, rect.max.y - 6.0));
    let mut f = ui.new_child(egui::UiBuilder::new().max_rect(foot).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let r = f.add_enabled(!running && has_ready, egui::Button::new(tl!("Start Queue")));
    reg.add("export.queue.start", r.rect, "Start Queue");
    if r.clicked() {
        action = Some(("export.queue.start", json!({})));
    }
    let r = f.add_enabled(running, egui::Button::new(tl!("Stop")));
    reg.add("export.queue.stop", r.rect, "Stop");
    if r.clicked() {
        action = Some(("export.queue.stop", json!({})));
    }
    let r =
        f.add_enabled(items.iter().any(|i| matches!(i["status"].as_str(), Some("done" | "failed" | "cancelled"))), egui::Button::new(tl!("Clear Finished")));
    reg.add("export.queue.clear", r.rect, "Clear Finished");
    if r.clicked() {
        action = Some(("export.queue.clear", json!({})));
    }
    reg.flush(app);
    if let Some((cmd, p)) = action
        && let Err(e) = app.session.execute(cmd, p)
    {
        app.ui.status = e.to_string();
    }
}
