use super::settings::{DragOptions, combo, drag};
use super::{
    Align2, CUSTOM, Color32, ExportUi, FilmcraftApp, FrameKey, Rect, Reg, Scaling, Sense, Target, Tick, Tokens, Value, active_export_job, export_params,
    format_bytes, is_export_busy, json, output_path, pos2, vec2,
};

// ---------------------------------------------------------------------------------------------
// right: preview, range, summary, buttons
// ---------------------------------------------------------------------------------------------

pub(crate) const RANGES: [(&str, &str); 4] = [("entire", "Entire Source"), ("inOut", "Source In/Out"), ("workArea", "Work Area"), ("custom", "Custom")];

/// First and last visible frames in the export range; invalid ranges still clamp safely.
pub(crate) fn preview_bounds(app: &FilmcraftApp) -> Option<(i64, i64)> {
    let q = app.session.active_sequence()?;
    let rate = q.settings.frame_rate;
    let (start, end) = range_ticks(&app.ui.export, q);
    let maximum = rate.frame_at(Tick::MAX).max(0);
    let first = rate.frame_at(start).clamp(0, maximum);
    let last = rate.frame_at(Tick(end.0.saturating_sub(1))).clamp(first, maximum);
    Some((first, last))
}

/// Navigate the sequence playhead without editing project content or export settings.
pub(crate) fn preview_command(app: &mut FilmcraftApp, command: &str, params: &Value) -> Result<Value, String> {
    if app.ui.mode != crate::state::Mode::Export {
        return Err("Open Export mode to use its preview controls".into());
    }
    let (first, last) = preview_bounds(app).ok_or("Open a sequence to export")?;
    let rate = app.session.sequence_rate();
    let current = rate.frame_at(app.session.playhead());
    if command == "export.preview.toggle" {
        if app.playback.playing {
            app.stop();
        } else {
            let frame = if current < first || current >= last { first } else { current };
            app.session.execute("playhead.set", json!({"frame": frame})).map_err(|e| e.to_string())?;
            app.play(1.0);
            // Program's restart-at-end preference must not move a one-frame export to zero.
            app.session.execute("playhead.set", json!({"frame": frame})).map_err(|e| e.to_string())?;
            app.playback.anchor_tick = app.session.playhead();
        }
        return Ok(json!({"playing": app.playback.playing}));
    }
    let frame = match command {
        "export.preview.seek" => params.get("frame").and_then(Value::as_i64).ok_or("Export preview seek needs an integer frame")?,
        "export.preview.first" => first,
        "export.preview.last" => last,
        "export.preview.stepBack" => current.saturating_sub(1),
        "export.preview.stepForward" => current.saturating_add(1),
        _ => return Err("Unknown export preview command".into()),
    }
    .clamp(first, last);
    app.stop();
    app.stop_source();
    app.session.execute("playhead.set", json!({"frame": frame})).map_err(|e| e.to_string())
}

/// Render the export transport and bounded scrubber with stable automation IDs.
fn preview_controls(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let Some((first, last)) = preview_bounds(app) else { return };
    let rate = app.session.sequence_rate();
    let frame = rate.frame_at(app.session.playhead());
    let drop = app.session.active_sequence().is_some_and(|q| q.settings.drop_frame);
    let tc = |f| filmcraft_time::format_timecode_frames(f, rate, drop);
    let response = ui.label(egui::RichText::new(format!("{} / {}", tc(frame), tc(last))).monospace());
    app.auto.add_response("export.preview.timecode", &response, &format!("{} / {}", tc(frame), tc(last)));
    let mut at = frame.clamp(first, last);
    let response = ui
        .scope(|ui| {
            ui.spacing_mut().slider_width = ui.available_width().max(40.0);
            ui.add(egui::Slider::new(&mut at, first..=last).show_value(false))
        })
        .inner;
    app.auto.add_response("export.preview.scrub", &response, "Preview");
    if response.changed()
        && let Err(e) = crate::menus::invoke(app, ui.ctx(), "export.preview.seek", json!({"frame": at}))
    {
        app.ui.status = e;
    }
    ui.horizontal(|ui| {
        use crate::icons::{self, Icon};
        for (id, icon, label) in [
            ("export.preview.first", Icon::GoToIn, tl!("Go to In (Shift+I)")),
            ("export.preview.stepBack", Icon::StepBack, tl!("Step Back 1 Frame (Left)")),
            ("export.preview.toggle", if app.playback.playing { Icon::Pause } else { Icon::Play }, tl!("Play-Stop Toggle (Space)")),
            ("export.preview.stepForward", Icon::StepFwd, tl!("Step Forward 1 Frame (Right)")),
            ("export.preview.last", Icon::GoToOut, tl!("Go to Out (Shift+O)")),
        ] {
            let response = icons::button(ui, icon, 28.0, false, &app.tokens, label);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, response.enabled(), label));
            if response.has_focus() {
                ui.painter().rect_stroke(response.rect, 4.0, egui::Stroke::new(1.0, app.tokens.accent), egui::StrokeKind::Inside);
            }
            app.auto.add_response(id, &response, label);
            if response.clicked()
                && let Err(e) = crate::menus::invoke(app, ui.ctx(), id, json!({}))
            {
                app.ui.status = e;
            }
        }
    });
    ui.add_space(4.0);
}
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
        if app.playback.playing {
            app.frames.schedule_playback(key, rate, 0.5, &project, app.playback.speed, app.playback.preroll.is_some());
            if app.playback.preroll.is_some() {
                app.playback.preroll_ready = app.frames.preroll_ready(key, app.playback.speed, preview_bounds(app).map(|(_, last)| last).unwrap_or(frame));
            }
        } else {
            app.frames.request(key, rate.tick_of(frame), 0.5, &project, 0);
        }
        let asked = crate::panels::monitor::asked_before(app, "export-preview", key, app.playback.playing);
        // Keep one texture, but never show a previous sequence while its replacement renders.
        let name = "export-preview";
        if app.texture_key(name).is_some_and(|old| old.target != key.target) {
            app.textures.remove(name);
        }
        let (texture, shown) = crate::panels::monitor::cpu_texture(app, ui.ctx(), name, key, &asked, None, crate::state::DisplayMode::Composite);
        if shown != Some(key) {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        }
        if let Some(tex) = texture {
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
        app.playback.preroll_ready = true;
        ui.painter().text(pic.center(), Align2::CENTER_CENTER, tl!("Audio only"), Tokens::ui(13.0), t.text_dim);
    }
    reg.add("export.preview", pic, "Preview");
    let busy = is_export_busy(app);
    let active_job = active_export_job(app);
    let queue_active = app.session.export_queue.is_active();
    // range + scaling
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(rect.min.x + 14.0, pv_area.max.y + 8.0), pos2(rect.max.x - 14.0, rect.max.y - 50.0)))
            .id_salt("export-right"),
    );
    egui::ScrollArea::vertical().id_salt("export-preview-details").show(&mut child, |child| {
        preview_controls(app, child);
        let ex = &mut app.ui.export;
        let has_work_area = q.work_area.is_some();
        child.add_enabled_ui(!busy, |child| {
            child.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(tl!("Range")).color(t.text_dim));
                let cur = RANGES.iter().find(|(k, _)| *k == ex.range).map(|x| x.1).unwrap_or("Entire Source");
                let labels: Vec<(String, bool)> = RANGES.iter().map(|(k, l)| (l.to_string(), *k != "workArea" || has_work_area)).collect();
                if let Some(i) = combo(ui, &mut reg, "export.range", cur, &labels, 140.0) {
                    ex.range = RANGES[i].0.into();
                }
                if ex.range == "custom" {
                    drag(
                        ui,
                        &mut reg,
                        "export.range.start",
                        &mut ex.custom_start,
                        DragOptions { range: 0.0..=86_400.0, speed: 0.04, suffix: " s", decimals: 2 },
                    );
                    drag(ui, &mut reg, "export.range.end", &mut ex.custom_end, DragOptions { range: 0.0..=86_400.0, speed: 0.04, suffix: " s", decimals: 2 });
                }
                let (start, end) = range_ticks(ex, &q);
                let span = filmcraft_time::TimeRange::from_bounds(start, end);
                let frames = rate.frame_at(end - Tick(1)).saturating_add(1).saturating_sub(rate.frame_at(start)).max(0);
                let seconds = format!("{:.3}", span.duration.seconds());
                let label = if frames == 1 {
                    tlf!("1 frame ({seconds} s)", seconds = seconds)
                } else {
                    tlf!("{frames} frames ({seconds} s)", frames = frames, seconds = seconds)
                };
                let resp = ui.label(egui::RichText::new(&label).color(t.text_dim));
                reg.add("export.range.duration", resp.rect, label);
            });
            let (start, end) = range_ticks(ex, &q);
            if ex.range == "inOut" && filmcraft_engine::export_tools::single_frame_in_out(&q, filmcraft_time::TimeRange::from_bounds(start, end)) {
                let warning = tl!("Source In/Out is only 1 frame. Choose Entire Source to export the whole sequence.");
                let resp = child.add(egui::Label::new(egui::RichText::new(warning).color(t.danger)).wrap());
                reg.add("export.range.warning", resp.rect, warning);
            }
            if ex.settings.has_video() {
                child.horizontal(|ui| {
                    ui.label(egui::RichText::new(tl!("Scaling")).color(t.text_dim));
                    let o = [
                        (Scaling::ScaleToFit, tl!("Scale To Fit")),
                        (Scaling::ScaleToFill, tl!("Scale To Fill")),
                        (Scaling::StretchToFill, tl!("Stretch To Fill")),
                    ];
                    let cur = o.iter().find(|(x, _)| *x == ex.settings.scaling).map_or(o[0].1, |x| x.1);
                    let labels: Vec<(String, bool)> = o.iter().map(|(_, l)| (l.to_string(), true)).collect();
                    if let Some(i) = combo(ui, &mut reg, "export.scaling", cur, &labels, 140.0) {
                        ex.settings.scaling = o[i].0;
                    }
                });
            }
        });
        // summary
        let range = range_ticks(ex, &q);
        let sum = ex.settings.summary(q.settings.width, q.settings.height, rate, q.settings.sample_rate, range.1 - range.0);
        child.add_space(8.0);
        let resp = egui::Frame::NONE
            .fill(t.field_bg)
            .corner_radius(4.0)
            .inner_margin(egui::Margin::same(10))
            .show(child, |ui| {
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
    });
    // buttons
    let go = Rect::from_min_size(pos2(rect.max.x - 120.0, rect.max.y - 44.0), vec2(104.0, 30.0));
    let queue = Rect::from_min_size(pos2(go.min.x - 132.0, go.min.y), vec2(122.0, 30.0));
    let qresp = ui.interact(queue, egui::Id::unique("export-queue-send"), if busy { Sense::hover() } else { Sense::click() });
    let stroke_color = if busy {
        t.text_faint
    } else if qresp.hovered() {
        t.text
    } else {
        t.text_dim
    };
    let text_color = if busy { t.text_faint } else { t.text };
    ui.painter().rect_stroke(queue, 15.0, egui::Stroke::new(1.0, stroke_color), egui::StrokeKind::Inside);
    ui.painter().text(queue.center(), Align2::CENTER_CENTER, tl!("Send to Queue"), Tokens::semibold(12.5), text_color);
    reg.add("export.sendToQueue", queue, "Send to Queue");

    let button_label = if queue_active {
        tl!("Exporting")
    } else if active_job.is_some() {
        tl!("Cancel")
    } else {
        tl!("Export")
    };
    let go_enabled = !queue_active;
    let resp = ui.interact(go, egui::Id::unique("export-go"), if go_enabled { Sense::click() } else { Sense::hover() });
    let fill_color = if !go_enabled {
        t.field_bg
    } else if active_job.is_some() {
        t.danger
    } else if resp.hovered() {
        t.accent_hover
    } else {
        t.accent
    };
    ui.painter().rect_filled(go, 15.0, fill_color);
    ui.painter().text(go.center(), Align2::CENTER_CENTER, button_label, Tokens::semibold(13.0), if go_enabled { Color32::WHITE } else { t.text_faint });
    reg.add("export.button", go, button_label);
    reg.flush(app);

    if resp.clicked() && go_enabled {
        if let Some(job) = active_job {
            job.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        } else {
            let r = app.session.execute("file.exportMedia", export_params(app));
            match r {
                Ok(v) => app.ui.status = tlf!("Exporting {path}", path = v["path"].as_str().unwrap_or("")),
                Err(e) => app.ui.status = e.to_string(),
            }
        }
    }
    if qresp.clicked() && !busy {
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

#[cfg(test)]
mod preview_tests {
    use super::*;
    use crate::{dock::PanelKind, state::Mode};

    fn app() -> FilmcraftApp {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let mut app = FilmcraftApp::new(s);
        app.ui.mode = Mode::Export;
        app.ui.export.range = "custom".into();
        app.ui.export.custom_start = 2.0;
        app.ui.export.custom_end = 4.0;
        app
    }

    #[test]
    fn export_preview_commands_are_rejected_outside_export_mode() {
        for mode in [Mode::Edit, Mode::Import] {
            let mut app = app();
            app.ui.mode = mode;
            let ctx = egui::Context::default();
            let before = serde_json::to_value(&app.session.project).unwrap();
            let playhead = app.session.playhead();
            let revision = app.session.revision;
            let dirty = app.session.is_dirty();
            let undo = app.session.undo_mark();
            for command in [
                "export.preview.seek",
                "export.preview.toggle",
                "export.preview.first",
                "export.preview.last",
                "export.preview.stepBack",
                "export.preview.stepForward",
            ] {
                assert!(crate::menus::invoke(&mut app, &ctx, command, json!({"frame": 51})).is_err(), "{command} in {mode:?}");
                assert!(!app.playback.playing);
                assert_eq!(app.session.playhead(), playhead);
                assert_eq!(serde_json::to_value(&app.session.project).unwrap(), before);
                assert_eq!(app.session.revision, revision);
                assert_eq!(app.session.is_dirty(), dirty);
                assert_eq!(app.session.undo_mark(), undo);
            }
        }
    }

    #[test]
    fn preview_navigation_is_bounded_and_does_not_edit_the_project() {
        let mut app = app();
        let before = serde_json::to_value(&app.session.project).unwrap();
        let revision = app.session.revision;
        let dirty = app.session.is_dirty();
        let undo = app.session.undo_mark();
        let (first, last) = preview_bounds(&app).unwrap();
        for (input, expected) in [(i64::MIN, first), (i64::MAX, last), (first + 1, first + 1)] {
            preview_command(&mut app, "export.preview.seek", &json!({"frame": input})).unwrap();
            assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), expected);
        }
        for input in [json!({}), json!({"frame": 1.5}), json!({"frame": "bad"}), json!({"frame": u64::MAX})] {
            assert!(preview_command(&mut app, "export.preview.seek", &input).is_err());
        }
        preview_command(&mut app, "export.preview.first", &json!({})).unwrap();
        preview_command(&mut app, "export.preview.stepBack", &json!({})).unwrap();
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), first);
        preview_command(&mut app, "export.preview.last", &json!({})).unwrap();
        preview_command(&mut app, "export.preview.stepForward", &json!({})).unwrap();
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), last);
        assert_eq!(serde_json::to_value(&app.session.project).unwrap(), before);
        assert_eq!(app.session.revision, revision);
        assert_eq!(app.session.is_dirty(), dirty);
        assert_eq!(app.session.undo_mark(), undo);
    }

    #[test]
    fn export_transport_treats_null_monitor_as_omitted() {
        let mut app = app();
        app.ui.focused = PanelKind::Source;
        let ctx = egui::Context::default();
        let (first, last) = preview_bounds(&app).unwrap();
        for params in [json!({}), json!({"monitor": null})] {
            for (command, expected) in [("markers.goToIn", first), ("playhead.stepBack", first), ("markers.goToOut", last), ("playhead.stepForward", last)] {
                crate::menus::invoke(&mut app, &ctx, command, params.clone()).unwrap();
                assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), expected);
            }
            crate::menus::invoke(&mut app, &ctx, "playback.toggle", params.clone()).unwrap();
            assert!(app.playback.playing);
            assert!(!app.source_playback.clock.playing);
            assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), first);
            crate::menus::invoke(&mut app, &ctx, "playback.toggle", params).unwrap();
            assert!(!app.playback.playing);
        }
        // An explicit Program target retains the unrestricted transport behavior.
        crate::menus::invoke(&mut app, &ctx, "playhead.stepBack", json!({"monitor": "program"})).unwrap();
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), first - 1);
    }

    #[test]
    fn export_keyboard_targets_sequence_and_playback_stops_at_range_end_even_with_loop_on() {
        let mut app = app();
        app.ui.focused = PanelKind::Source;
        app.playback.looping = true;
        let ctx = egui::Context::default();
        crate::menus::invoke(&mut app, &ctx, "playback.toggle", json!({})).unwrap();
        assert!(app.playback.playing);
        assert!(!app.source_playback.clock.playing);
        assert_eq!(app.session.playhead(), app.session.sequence_rate().snap(Tick::from_seconds_f64(2.0)));
        app.playback.preroll = None;
        app.playback.anchor_time = 1.0;
        app.set_now_for_test(4.0);
        let mut out = ctx.run_ui(egui::RawInput { time: Some(4.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
        out.textures_delta.clear();
        assert!(!app.playback.playing);
        assert!(app.playback.looping, "the Program loop preference is preserved");
        let (_, last) = preview_bounds(&app).unwrap();
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), last);
        crate::menus::invoke(&mut app, &ctx, "playback.toggle", json!({})).unwrap();
        assert_eq!(app.session.playhead(), app.session.sequence_rate().snap(Tick::from_seconds_f64(2.0)));
        crate::menus::invoke(&mut app, &ctx, "playhead.stepForward", json!({})).unwrap();
        assert!(!app.playback.playing);
        assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), preview_bounds(&app).unwrap().0 + 1);
    }

    #[test]
    fn preview_ranges_cover_in_out_work_area_empty_and_hostile_custom_values() {
        let mut app = app();
        let id = app.session.state.active_sequence.unwrap();
        let rate = app.session.sequence_rate();
        let q = std::sync::Arc::make_mut(&mut app.session.project).sequence_mut(id).unwrap();
        q.mark_in = Some(rate.tick_of(10));
        q.mark_out = Some(rate.tick_of(20));
        q.work_area = Some(filmcraft_time::TimeRange::new(rate.tick_of(30), rate.tick_of(10)));
        app.ui.export.range = "inOut".into();
        assert_eq!(preview_bounds(&app), Some((10, 20)));
        app.ui.export.range = "workArea".into();
        assert_eq!(preview_bounds(&app), Some((30, 39)));
        app.ui.export.range = "custom".into();
        for (start, end) in [(f64::NAN, f64::INFINITY), (5.0, 2.0), (-10.0, -20.0), (f64::MAX, f64::MAX)] {
            app.ui.export.custom_start = start;
            app.ui.export.custom_end = end;
            let (first, last) = preview_bounds(&app).unwrap();
            assert!(first >= 0 && last >= first);
        }
        let mut empty = FilmcraftApp::new(filmcraft_engine::Session::default());
        assert!(preview_command(&mut empty, "export.preview.toggle", &json!({})).is_err());
    }
}
