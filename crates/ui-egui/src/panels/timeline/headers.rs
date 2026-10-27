use super::layout::Row;
use super::*;

/// A Premiere track-header button (patch / target): highlight-blue fill when on, full track height.
fn patch_button(ui: &mut egui::Ui, r: Rect, clip: Rect, label: &str, on: bool, show_off: bool, id: egui::Id, t: &Tokens) -> egui::Response {
    let resp = ui.interact(r.intersect(clip), id, Sense::click());
    let p = ui.painter().with_clip_rect(clip);
    if on {
        p.rect_filled(r, 2.0, t.track_target);
        p.text(r.center(), Align2::CENTER_CENTER, label, Tokens::semibold(10.0), Color32::from_rgb(0xeb, 0xeb, 0xeb));
    } else if show_off {
        if resp.hovered() {
            p.rect_filled(r, 2.0, t.hover);
        }
        p.text(r.center(), Align2::CENTER_CENTER, label, Tokens::semibold(10.0), t.text_dim);
    } else if resp.hovered() {
        p.rect_stroke(r, 2.0, Stroke::new(1.0, t.separator), StrokeKind::Inside);
    }
    resp
}

/// Draw only the visible track headers, including colour label, targeting, lock and header controls clipped to each row.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_headers(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, rows: &[Row], rect: Rect, vclip: Rect, aclip: Rect, t: &Tokens) {
    let hw = app.ui.timeline.header_w;
    let tg = app.session.targeting();
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut vo_action = None;
    for r in rows {
        let Some(tr) = seq.track(r.track) else { continue };
        let clip_rect = if r.kind == TrackKind::Video { vclip } else { aclip };
        let hrect = Rect::from_min_max(pos2(rect.min.x, r.rect.min.y), pos2(rect.min.x + hw, r.rect.max.y));
        let visible = hrect.intersect(clip_rect);
        if visible.height() <= 0.0 {
            continue;
        }
        let p = ui.painter().with_clip_rect(visible);
        p.rect_filled(hrect, 0.0, t.tl_header_bg);
        // 0. colour label (#1186): a tint over the header, and the strip at its left edge, which
        // opens the colour menu
        let strip = Rect::from_min_max(hrect.min, pos2(hrect.min.x + 9.0, hrect.max.y));
        if let Some(l) = tr.label {
            let c = label_color(app, l);
            p.rect_filled(hrect, 0.0, c.gamma_multiply(TRACK_HEADER_TINT));
            p.rect_filled(Rect::from_min_max(strip.min, pos2(strip.min.x + 5.0, strip.max.y)), 0.0, c);
        }
        let cresp = ui.interact(strip.intersect(visible), egui::Id::unique(("trackColor", r.track.0)), Sense::click()).on_hover_text(tl!("Track Color"));
        app.auto.add(&format!("timeline.track.{}{}.color", if r.kind == TrackKind::Video { "V" } else { "A" }, r.index + 1), strip, "Track Color");
        if cresp.hovered() && tr.label.is_none() {
            p.rect_filled(Rect::from_min_max(strip.min, pos2(strip.min.x + 5.0, strip.max.y)), 0.0, t.hover);
        }
        egui::Popup::menu(&cresp).show(|ui| {
            let prefix = format!("timeline.track.{}{}.color", if r.kind == TrackKind::Video { "V" } else { "A" }, r.index + 1);
            let none = ui.selectable_label(tr.label.is_none(), tl!("None"));
            app.auto.add(&format!("{prefix}.None"), none.rect, "None");
            if none.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "label": ""})));
                ui.close();
            }
            // the user's label names and colours, as in Edit ▸ Label
            let labels = app.session.prefs.labels.clone();
            for l in filmcraft_project::Label::ALL {
                if crate::menus::label_choice(ui, &mut app.auto, &labels, l, &prefix).clicked() {
                    actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "label": l.name()})));
                    ui.close();
                }
            }
        });
        p.line_segment([pos2(hrect.min.x, hrect.max.y - 0.5), pos2(hrect.max.x, hrect.max.y - 0.5)], Stroke::new(1.0, t.separator));
        if r.kind == TrackKind::Video && r.index + 1 == seq.video_tracks.len() {
            p.line_segment([pos2(hrect.min.x, hrect.min.y + 0.5), pos2(hrect.max.x, hrect.min.y + 0.5)], Stroke::new(1.0, t.separator));
        }
        let label = format!("{}{}", if r.kind == TrackKind::Video { "V" } else { "A" }, r.index + 1);
        let btn_rect = |x0: f32| Rect::from_min_max(pos2(hrect.min.x + x0, hrect.min.y + 1.0), pos2(hrect.min.x + x0 + 24.0, hrect.max.y - 2.0));
        // 1. source patch (absent when unpatched)
        let patched = if r.kind == TrackKind::Video { tg.video_dest == Some(r.track) } else { tg.audio_dest == Some(r.track) };
        let pr = btn_rect(13.0);
        app.auto.add(&format!("timeline.track.{label}.sourcePatch"), pr, "Source patch");
        if patch_button(ui, pr, visible, &label, patched, false, egui::Id::unique(("patch", r.track.0)), t).clicked() {
            actions.push(("timeline.setTargeting".into(), json!({"track": r.track.0, "sourcePatch": !patched})));
        }
        // 2. track lock
        let small = |x: f32, y: f32| Rect::from_center_size(pos2(hrect.min.x + x, y), vec2(18.0, 18.0));
        let upper_y = hrect.min.y + (16.0f32).min(hrect.height() / 2.0);
        let lock_r = small(49.0, hrect.center().y);
        let lresp = crate::widgets::icon_toggle(
            ui,
            lock_r.intersect(visible),
            if tr.locked { Icon::Lock } else { Icon::Unlock },
            true,
            t,
            egui::Id::unique(("locked", r.track.0)),
            Some(if tr.locked { t.icon_active } else { t.text_dim }),
        );
        app.auto.add(&format!("timeline.track.{label}.locked"), lock_r, "Toggle Track Lock");
        if lresp.clicked() {
            actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "locked": !tr.locked})));
        }
        if tr.locked {
            // diagonal hatch over locked lanes is drawn by the lane painter; here a subtle tint
            p.rect_filled(Rect::from_min_max(pos2(hrect.max.x - 4.0, hrect.min.y), hrect.max), 0.0, t.pressed);
        }
        // 3. target
        let targeted = tg.targeted.contains(&r.track);
        let trr = btn_rect(61.0);
        app.auto.add(&format!("timeline.track.{label}.target"), trr, "Toggle track targeting");
        if patch_button(ui, trr, visible, &label, targeted, true, egui::Id::unique(("target", r.track.0)), t).clicked() {
            actions.push(("timeline.setTargeting".into(), json!({"track": r.track.0, "targeted": !targeted})));
        }
        // 4. upper line icons
        let mut x = 102.0;
        let sr = small(x, upper_y);
        let sresp = crate::widgets::icon_toggle(
            ui,
            sr.intersect(visible),
            Icon::SyncLock,
            tr.sync_lock,
            t,
            egui::Id::unique(("syncLock", r.track.0)),
            Some(t.text_dim),
        );
        app.auto.add(&format!("timeline.track.{label}.syncLock"), sr, "Toggle Sync Lock");
        if sresp.clicked() {
            actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "syncLock": !tr.sync_lock})));
        }
        x += 24.0;
        if r.kind == TrackKind::Video {
            let er = small(x, upper_y);
            let eresp = crate::widgets::icon_toggle(
                ui,
                er.intersect(visible),
                if tr.enabled { Icon::Eye } else { Icon::EyeOff },
                true,
                t,
                egui::Id::unique(("enabled", r.track.0)),
                Some(t.text_dim),
            );
            app.auto.add(&format!("timeline.track.{label}.enabled"), er, "Toggle Track Output");
            if eresp.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "enabled": !tr.enabled})));
            }
        } else {
            let mr = Rect::from_center_size(pos2(hrect.min.x + x, upper_y), vec2(14.0, 14.0));
            let mresp = crate::widgets::letter_toggle(
                ui,
                mr.intersect(visible),
                "M",
                tr.muted,
                Color32::from_rgb(0x2d, 0x9d, 0x78),
                t,
                egui::Id::unique(("mute", r.track.0)),
            );
            app.auto.add(&format!("timeline.track.{label}.muted"), mr, "Mute Track");
            if mresp.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "muted": !tr.muted})));
            }
            let s2 = Rect::from_center_size(pos2(hrect.min.x + x + 20.0, upper_y), vec2(14.0, 14.0));
            let so = crate::widgets::letter_toggle(
                ui,
                s2.intersect(visible),
                "S",
                tr.solo,
                Color32::from_rgb(0xf0, 0xf0, 0x4f),
                t,
                egui::Id::unique(("solo", r.track.0)),
            );
            app.auto.add(&format!("timeline.track.{label}.solo"), s2, "Solo Track");
            if so.clicked() {
                actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "solo": !tr.solo})));
            }
            let vr = small(x + 42.0, upper_y);
            if let Some(a) = crate::panels::voiceover::header_button(app, ui, r.track, vr, visible, &label, t) {
                vo_action = Some(a);
            }
            let kr = small(x + 64.0, upper_y);
            crate::panels::timeline_automation::header_button(app, ui, seq, r, kr, visible, &label, t);
        }
        // 5. name on the lower line (or inline when short)
        if hrect.height() >= 40.0 {
            p.text(pos2(hrect.min.x + 94.0, hrect.min.y + 34.0), Align2::LEFT_CENTER, &tr.name, Tokens::ui(11.0), t.text_dim);
        }
        // resize track height by dragging the header's bottom edge, or the outer edge of the top video
        // track or the last audio track, which also reaches into the empty space past it
        let header_column = Rect::from_min_max(pos2(hrect.min.x, clip_rect.min.y), pos2(hrect.max.x, clip_rect.max.y));
        let last_audio = r.kind == TrackKind::Audio && r.index + 1 == seq.audio_tracks.len();
        let bottom = Rect::from_min_max(pos2(hrect.min.x, hrect.max.y - 3.0), pos2(hrect.max.x, hrect.max.y + 2.0));
        let top = (r.kind == TrackKind::Video && r.index + 1 == seq.video_tracks.len())
            .then(|| ("resizeTop", Rect::from_min_max(pos2(hrect.min.x, hrect.min.y - 2.0), pos2(hrect.max.x, hrect.min.y + 3.0)).intersect(header_column)));
        for (name, edge) in std::iter::once(("resize", bottom.intersect(if last_audio { header_column } else { visible }))).chain(top) {
            if edge.height() <= 0.0 {
                continue;
            }
            app.auto.add(&format!("timeline.track.{label}.{name}"), edge, "Drag to change the track height");
            let eresp = ui.interact(edge, egui::Id::unique(("trackh", name, r.track.0)), Sense::drag());
            if eresp.hovered() || eresp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            }
            if eresp.dragged() {
                let d = eresp.drag_delta().y * if r.kind == TrackKind::Video { -1.0 } else { 1.0 };
                let h = if r.kind == TrackKind::Video { &mut app.ui.timeline.video_track_h } else { &mut app.ui.timeline.audio_track_h };
                *h = (*h + d).clamp(22.0, 220.0);
            }
        }
        let resp = ui.interact(
            Rect::from_min_max(pos2(hrect.min.x + 90.0, hrect.min.y + 26.0), hrect.max).intersect(visible),
            egui::Id::unique(("hdr", r.track.0)),
            Sense::click(),
        );
        if resp.double_clicked() {
            let h = if r.kind == TrackKind::Video { &mut app.ui.timeline.video_track_h } else { &mut app.ui.timeline.audio_track_h };
            *h = if *h < 50.0 { 64.0 } else { 30.0 };
        }
    }
    // column separator
    ui.painter().line_segment([pos2(rect.min.x + hw - 0.5, vclip.min.y), pos2(rect.min.x + hw - 0.5, aclip.max.y + MASTER_H)], Stroke::new(1.0, t.separator));
    for (cmd, params) in actions {
        if let Err(e) = app.session.execute(&cmd, params) {
            app.ui.status = e.to_string();
        }
    }
    if let Some(a) = vo_action {
        crate::panels::voiceover::run(app, &ui.ctx().clone(), a);
    }
}
