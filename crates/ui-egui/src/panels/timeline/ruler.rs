use super::layout::Layout;
use super::{
    Align2, Color32, Drag, EDGE_PX, FilmcraftApp, Icon, ItemId, Rect, Sense, Sequence, Stroke, ThumbnailMode, Tick, TimeDisplay, TimeRange, Tokens, Value,
    format_time, icons, json, marker_range_rect, marker_rect, pos2, vec2, work_area_rect,
};

/// The Ruler's two strides, in frames: `(tick, label)`.
///
/// `label` is the first ladder step wide enough to keep timestamps apart (110 pt); `tick` is the
/// finest divisor of `label` that still leaves ticks 12 pt apart. Deriving the tick stride as a
/// divisor puts every label on a tick and keeps the ticks between labels evenly spaced.
pub fn ruler_steps(frame_px: f64, base: i64) -> (i64, i64) {
    // `FrameRate` is plain serde data, so a step past `base` can overflow; saturate instead.
    let big = |k: i64| base.saturating_mul(k);
    let steps = [1, 2, 5, 10, base / 2, base, big(2), big(5), big(10), big(15), big(30), big(60), big(120), big(300), big(600), big(1800), big(3600)];
    let label_step = (*steps.iter().find(|s| **s as f64 * frame_px >= 110.0).unwrap_or(&big(3600))).max(1);
    // Labels are at least 110 pt apart, so a divisor no larger than 9 keeps ticks >= 12 pt.
    let minor =
        (1..=9).rev().filter(|k| label_step.rem_euclid(*k) == 0).map(|k| label_step / k).find(|m| *m as f64 * frame_px >= 12.0).unwrap_or(label_step).max(1);
    (minor, label_step)
}

/// Draw editable timecode, the timeline toolbar and ruler. Rejected timecode edits report a status message; toolbar command errors are ignored.
pub(super) fn draw_top(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, seq: &Sequence, layout: &Layout, t: &Tokens, seq_id: ItemId) {
    let p = ui.painter().clone();
    let rate = seq.settings.frame_rate;
    let hw = app.ui.timeline.header_w;
    let ruler = layout.ruler;
    // background of the whole top block
    p.rect_filled(Rect::from_min_max(rect.min, pos2(rect.max.x, ruler.max.y)), 0.0, t.panel_bg);
    // current timecode: Premiere's big blue timecode
    let tc = format_time(app.session.playhead(), rate, seq.settings.drop_frame, TimeDisplay::Timecode, seq.settings.sample_rate as i64);
    let tc_rect = Rect::from_min_size(pos2(rect.min.x + 14.0, rect.min.y + 4.0), vec2(hw - 20.0, 20.0));
    let tc_id = egui::Id::unique(("timeline-tc", seq_id));
    match crate::widgets::timecode_field(ui, tc_id, tc_rect, &tc, app.session.playhead(), Tick::ZERO, rate, seq.settings.drop_frame, t.timecode) {
        Some(Ok(time)) => {
            if let Err(e) = app.session.execute("playhead.set", json!({"time": time.0})) {
                app.ui.status = e.to_string();
            }
        }
        Some(Err(e)) => app.ui.status = e,
        None => {}
    }
    app.auto.add("timeline.timecode", tc_rect, &tc);
    // toolbar: 30 × 30 buttons, "on" = #4b4b4b fill
    let mut x = rect.min.x + 12.0;
    let y = rect.min.y + 26.0;
    let toggles: [(Icon, &str, bool, bool, &str); 6] = [
        (Icon::Nest, "nest", !app.session.state.sequences_as_clips, true, tl!("Insert and overwrite sequences as nests or individual clips")),
        (Icon::Magnet, "snap", app.session.state.snapping, true, tl!("Snap in Timeline (S)")),
        (Icon::Link, "linked", app.session.state.linked_selection, true, tl!("Linked Selection")),
        (Icon::Captions, "captions", false, false, tl!("Caption track options")),
        (Icon::Marker, "marker", false, false, tl!("Add Marker (M)")),
        (Icon::Wrench, "settings", false, false, tl!("Timeline Display Settings")),
    ];
    for (icon, key, on, toggle, tip) in toggles {
        let r = Rect::from_min_size(pos2(x, y), vec2(28.0, 28.0));
        let resp = ui.interact(r, egui::Id::unique(("tl-toggle", key)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("timeline.toggle.{key}"), r, tip);
        if toggle && on {
            p.rect_filled(r, 4.0, t.pressed);
        } else if resp.hovered() {
            p.rect_filled(r, 4.0, t.hover);
        }
        icons::paint(&p, r.shrink(7.0), icon, if toggle && on { t.text } else { t.text_dim });
        if resp.clicked() {
            let _ = match key {
                "nest" => app.session.execute("sequence.nestSequences", json!({})),
                "snap" => app.session.execute("sequence.snap", json!({})),
                "linked" => app.session.execute("sequence.linkedSelection", json!({})),
                "marker" => app.session.execute("markers.add", json!({})),
                _ => Ok(Value::Null),
            };
        }
        if key == "settings" {
            egui::Popup::menu(&resp).show(|ui| timeline_settings(app, ui));
        }
        x += 30.0;
    }
    // ----- ruler: markers row · labels · ticks · 2 pt render bar
    let clip = p.with_clip_rect(ruler);
    let label_y = ruler.min.y + 24.0;
    let tick_base = ruler.max.y - 3.0;
    let (minor, label_step) = ruler_steps(layout.pps / rate.as_f64(), rate.timecode_base());
    // In/Out band
    if seq.mark_in.is_some() || seq.mark_out.is_some() {
        let a = layout.x_of(seq.mark_in.unwrap_or(Tick::ZERO));
        let b = layout.x_of(seq.mark_out.map(|o| o + rate.frame_duration()).unwrap_or(seq.duration()));
        clip.rect_filled(Rect::from_min_max(pos2(a, label_y + 7.0), pos2(b, tick_base)), 0.0, Color32::from_rgb(0x5c, 0x5c, 0x5c));
    }
    // split points: a short bracket with a V or A tag (video above, audio below)
    let sp = seq.split;
    for (t, is_in, tag, low) in
        [(sp.video_in, true, "V", false), (sp.video_out, false, "V", false), (sp.audio_in, true, "A", true), (sp.audio_out, false, "A", true)]
    {
        let Some(t) = t else { continue };
        let x = layout.x_of(if is_in { t } else { t + rate.frame_duration() });
        let y0 = if low { tick_base - 7.0 } else { label_y + 7.0 };
        let col = Color32::from_rgb(0xd0, 0xd0, 0xd0);
        let dx = if is_in { 4.0 } else { -4.0 };
        clip.line_segment([pos2(x, y0), pos2(x, y0 + 7.0)], Stroke::new(1.5, col));
        clip.line_segment([pos2(x, y0), pos2(x + dx, y0)], Stroke::new(1.5, col));
        clip.text(pos2(x + 2.0 * dx, y0 + 3.5), Align2::CENTER_CENTER, tag, Tokens::ui(8.5), col);
        let kind = if low { "audio" } else { "video" };
        let side = if is_in { "In" } else { "Out" };
        app.auto.add(&format!("timeline.split.{kind}{side}"), Rect::from_center_size(pos2(x, y0 + 3.5), vec2(12.0, 9.0)), &format!("Split {tag} {side}"));
    }
    let f0 = rate.frame_at(layout.tick_at(ruler.min.x)).max(0);
    let f1 = rate.frame_at(layout.tick_at(ruler.max.x)) + 1;
    let mut f = (f0 / minor) * minor;
    while f <= f1 {
        let x = layout.x_of(rate.tick_of(f));
        let major = f % label_step == 0;
        let h = if major { 10.0 } else { 4.0 };
        clip.line_segment([pos2(x, tick_base - h), pos2(x, tick_base)], Stroke::new(1.0, t.tl_ruler_tick));
        if major {
            let label = format_time(rate.tick_of(f), rate, seq.settings.drop_frame, TimeDisplay::Timecode, 48000);
            clip.text(pos2(x + 1.0, label_y), Align2::CENTER_CENTER, label, Tokens::ui(11.0), t.tl_ruler_text);
        }
        f += minor;
    }
    // render bar
    // Per segment (engine `previews`): green = rendered preview, yellow = should play in real time,
    // red = needs rendering, nothing = plays natively.
    let rb = Rect::from_min_max(pos2(ruler.min.x, ruler.max.y - 2.0), ruler.max);
    let bar = app.session.previews.bar(&app.session.project, seq_id);
    for (i, span) in bar.iter().enumerate() {
        use filmcraft_engine::previews::BarState;
        let (c, what) = match span.state {
            BarState::None => continue,
            BarState::Yellow => (t.render_yellow, tl!("Unrendered: should play back in real time")),
            BarState::Red => (t.render_red, tl!("Unrendered: render to play back in real time")),
            BarState::Green => (t.render_green, tl!("Rendered preview")),
        };
        let r = Rect::from_min_max(pos2(layout.x_of(span.start), rb.min.y), pos2(layout.x_of(span.end), rb.max.y)).intersect(ruler);
        if r.width() <= 0.0 {
            continue;
        }
        clip.rect_filled(r, 0.0, c);
        let hit = r.expand2(vec2(0.0, 2.0));
        app.auto.add(&format!("timeline.renderBar.{i}"), hit, what);
        if ui.rect_contains_pointer(hit) {
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::unique(("rb", i)), egui::PopupAnchor::Pointer).show(|ui| {
                ui.label(what);
            });
        }
    }
    // work area bar: between the markers row and the time labels, as dragged while a drag of it runs
    let shown = match app.tl.drag {
        Some(Drag::WorkArea { start, end, .. }) => Some(TimeRange::from_bounds(start, end)),
        _ => seq.work_area,
    };
    if let Some(area) = shown {
        let r = work_area_rect(layout, area);
        clip.rect_filled(r, 1.0, t.text_dim);
        for x in [r.min.x, r.max.x] {
            clip.rect_filled(Rect::from_center_size(pos2(x, r.center().y), vec2(3.0, r.height() + 2.0)), 1.0, t.text);
        }
        app.auto.add("timeline.workArea", r.intersect(ruler), "Work Area Bar");
        let fmt = |tick: Tick| format_time(tick, rate, seq.settings.drop_frame, TimeDisplay::Timecode, 48000);
        if ui.rect_contains_pointer(r.expand2(vec2(EDGE_PX, 1.0)).intersect(ruler)) {
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::unique("work-area-tip"), egui::PopupAnchor::Pointer).show(|ui| {
                ui.label(tlf!("Start: {tc}", tc = fmt(area.start)));
                ui.label(tlf!("End: {tc}", tc = fmt(area.end())));
                ui.label(tlf!("Duration: {tc}", tc = fmt(area.duration)));
            });
        }
    }
    // markers: 8 × 12 pt pentagons in the marker colour, on the top row
    draw_markers(app, ui, &clip, layout, seq);
    app.auto.add("timeline.ruler", ruler, "time ruler");
}

/// The ruler's marker row: one pentagon per marker in its colour, its range band behind it and,
/// on hover, its name.
fn draw_markers(app: &mut FilmcraftApp, ui: &egui::Ui, clip: &egui::Painter, layout: &Layout, seq: &Sequence) {
    let marker_y = layout.ruler.min.y + 2.0;
    for m in &seq.markers {
        let x = layout.x_of(m.start);
        let c = m.color.marker_rgb();
        let c = Color32::from_rgb(c[0], c[1], c[2]);
        let shape =
            vec![pos2(x - 4.0, marker_y), pos2(x + 4.0, marker_y), pos2(x + 4.0, marker_y + 8.0), pos2(x, marker_y + 12.0), pos2(x - 4.0, marker_y + 8.0)];
        if let Some(band) = marker_range_rect(layout, m).filter(|r| r.width() > 0.0) {
            clip.rect_filled(band, 0.0, c.gamma_multiply(0.6));
            app.auto.add(&format!("timeline.markerRange.{}", m.id.0), band, &m.name);
        }
        clip.add(egui::Shape::convex_polygon(shape, c, Stroke::NONE));
        let mr = marker_rect(layout, m.start);
        app.auto.add(&format!("timeline.marker.{}", m.id.0), mr, &m.name);
        if ui.rect_contains_pointer(mr) && !m.name.is_empty() {
            egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::unique(("mk", m.id.0)), egui::PopupAnchor::Pointer).show(|ui| {
                ui.label(&m.name);
            });
        }
    }
}

/// The Timeline Display Settings popup from the ruler toolbar: thumbnails, waveforms, edit
/// markers and the track-height presets.
fn timeline_settings(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let c = ui.checkbox(&mut app.ui.timeline.show_thumbnails, tl!("Show Video Thumbnails"));
    app.auto.add("timeline.settings.showThumbnails", c.rect, "Show Video Thumbnails");
    let on = app.ui.timeline.show_thumbnails;
    ui.indent("timeline.settings.thumbnails", |ui| {
        for (mode, label) in
            [(ThumbnailMode::Head, tl!("Head")), (ThumbnailMode::HeadAndTail, tl!("Head and Tail")), (ThumbnailMode::Continuous, tl!("Continuous"))]
        {
            let r = ui.add_enabled(on, egui::RadioButton::new(app.ui.timeline.thumbnail_mode == mode, label));
            app.auto.add(&format!("timeline.settings.thumbnails.{}", mode.name()), r.rect, label);
            if r.clicked() {
                app.ui.timeline.thumbnail_mode = mode;
            }
        }
    });
    ui.checkbox(&mut app.ui.timeline.show_waveforms, tl!("Show Audio Waveform"));
    ui.separator();
    let mut te = app.session.state.show_through_edits;
    let c = ui.checkbox(&mut te, tl!("Show Through Edits"));
    app.auto.add("timeline.settings.showThroughEdits", c.rect, "Show Through Edits");
    if c.changed() {
        let _ = app.session.execute("sequence.showThroughEdits", json!({"on": te}));
    }
    let mut df = app.session.state.show_duplicate_frame_markers;
    let c = ui.checkbox(&mut df, tl!("Show Duplicate Frame Markers"));
    app.auto.add("timeline.settings.showDuplicateFrameMarkers", c.rect, "Show Duplicate Frame Markers");
    if c.changed() {
        let _ = app.session.execute("sequence.showDuplicateFrameMarkers", json!({"on": df}));
    }
    ui.separator();
    if ui.button(tl!("Expand All Tracks")).clicked() {
        app.ui.timeline.video_track_h = 64.0;
        app.ui.timeline.audio_track_h = 64.0;
    }
    if ui.button(tl!("Minimize All Tracks")).clicked() {
        app.ui.timeline.video_track_h = 26.0;
        app.ui.timeline.audio_track_h = 26.0;
    }
}
