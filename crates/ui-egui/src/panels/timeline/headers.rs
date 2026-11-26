use super::layout::Row;
use super::menus::{run_track_menu, track_column_menu, track_header_popup};
use super::{
    Align2, Color32, FilmcraftApp, Icon, MASTER_H, Rect, Sense, Sequence, Stroke, StrokeKind, TRACK_HEADER_TINT, Tokens, TrackKind, Value, json, label_color,
    pos2, vec2,
};

/// A Premiere track-header button (patch / target): highlight-blue fill when on, full track height.
struct PatchButton<'a> {
    rect: Rect,
    clip: Rect,
    label: &'a str,
    active: bool,
    show_inactive: bool,
    id: egui::Id,
    tokens: &'a Tokens,
}

fn patch_button(ui: &mut egui::Ui, button: PatchButton<'_>) -> egui::Response {
    let PatchButton { rect: r, clip, label, active: on, show_inactive: show_off, id, tokens: t } = button;
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
    // a command picked in a track header's menu, run after the headers are drawn
    let mut header_menu: Option<(String, Value)> = None;
    // right-click on the empty part of the header column (above the top video track, below the last
    // audio track) offers the dialogs; each track's header, added after it, keeps its own menu
    let column = Rect::from_min_max(pos2(rect.min.x, vclip.min.y), pos2(rect.min.x + hw, aclip.max.y));
    let column_resp = ui.interact(column, egui::Id::unique("hdr-column-menu"), Sense::click());
    column_resp.context_menu(|ui| track_column_menu(app, ui, &mut header_menu));
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
        let label = format!("{}{}", if r.kind == TrackKind::Video { "V" } else { "A" }, r.index + 1);
        // right-click anywhere on the header: add or delete tracks, here or through the dialogs of
        // Sequence ▸ Add Tracks… / Delete Tracks… (#1117). Added before the colour strip and the
        // buttons so they win where they overlap; a right-click on one still opens the menu, since it
        // takes only the click. Automation ids `timeline.track.<V1|A1…>.menu.<item>`.
        track_header_popup(app, ui, visible, &label, r.track, r.kind == TrackKind::Video, &mut header_menu);
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
        ui.painter()
            .with_clip_rect(clip_rect)
            .line_segment([pos2(hrect.min.x, hrect.max.y - 0.5), pos2(hrect.max.x, hrect.max.y - 0.5)], Stroke::new(1.0, t.separator));
        if r.kind == TrackKind::Video && r.index + 1 == seq.video_tracks.len() {
            ui.painter()
                .with_clip_rect(clip_rect)
                .line_segment([pos2(hrect.min.x, hrect.min.y + 0.5), pos2(hrect.max.x, hrect.min.y + 0.5)], Stroke::new(1.0, t.separator));
        }
        let btn_rect = |x0: f32| Rect::from_min_max(pos2(hrect.min.x + x0, hrect.min.y + 1.0), pos2(hrect.min.x + x0 + 24.0, hrect.max.y - 2.0));
        // 1. source patch (absent when unpatched)
        let patched = if r.kind == TrackKind::Video { tg.video_dest == Some(r.track) } else { tg.audio_dest == Some(r.track) };
        let pr = btn_rect(13.0);
        app.auto.add(&format!("timeline.track.{label}.sourcePatch"), pr, "Source patch");
        if patch_button(
            ui,
            PatchButton {
                rect: pr,
                clip: visible,
                label: &label,
                active: patched,
                show_inactive: false,
                id: egui::Id::unique(("patch", r.track.0)),
                tokens: t,
            },
        )
        .clicked()
        {
            actions.push(("timeline.setTargeting".into(), json!({"track": r.track.0, "sourcePatch": !patched})));
        }
        // 2. track lock
        let small = |x: f32, y: f32| Rect::from_center_size(pos2(hrect.min.x + x, y), vec2(18.0, 18.0));
        // Keep the lock with the row's upper control line. Centering it in the whole row made
        // the icon drift vertically whenever the track height changed, even when the opposite
        // timeline viewport edge was anchored in place.
        let lock_y = r.rect.min.y + 11.0;
        let lock_r = small(49.0, lock_y);
        let lresp = crate::widgets::icon_toggle_clipped(
            ui,
            lock_r,
            visible,
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
        if patch_button(
            ui,
            PatchButton {
                rect: trr,
                clip: visible,
                label: &label,
                active: targeted,
                show_inactive: true,
                id: egui::Id::unique(("target", r.track.0)),
                tokens: t,
            },
        )
        .clicked()
        {
            actions.push(("timeline.setTargeting".into(), json!({"track": r.track.0, "targeted": !targeted})));
        }
        // 4. upper line icons
        header_line_buttons(app, ui, seq, r, tr, visible, hrect, &label, t, &mut actions, &mut vo_action);
        // 5. name on the lower line (or inline when short)
        if hrect.height() >= 40.0 {
            p.text(pos2(hrect.min.x + 94.0, hrect.min.y + 34.0), Align2::LEFT_CENTER, &tr.name, Tokens::ui(11.0), t.text_dim);
        }
        draw_header_resize_controls(
            app,
            ui,
            HeaderResizeInput { seq, row: r, visible, header_rect: hrect, clip_rect, video_clip: vclip, audio_clip: aclip, label: &label },
        );
    }
    // column separator
    ui.painter().line_segment([pos2(rect.min.x + hw - 0.5, vclip.min.y), pos2(rect.min.x + hw - 0.5, aclip.max.y + MASTER_H)], Stroke::new(1.0, t.separator));
    for (cmd, params) in actions {
        if let Err(e) = app.session.execute(&cmd, params) {
            app.ui.status = e.to_string();
        }
    }
    run_track_menu(app, ui, header_menu);
    if let Some(a) = vo_action {
        crate::panels::voiceover::run(app, &ui.ctx().clone(), a);
    }
}

struct HeaderResizeInput<'a> {
    seq: &'a Sequence,
    row: &'a Row,
    visible: Rect,
    header_rect: Rect,
    clip_rect: Rect,
    video_clip: Rect,
    audio_clip: Rect,
    label: &'a str,
}

fn draw_header_resize_controls(app: &mut FilmcraftApp, ui: &mut egui::Ui, input: HeaderResizeInput<'_>) {
    let HeaderResizeInput { seq, row: r, visible, header_rect: hrect, clip_rect, video_clip: vclip, audio_clip: aclip, label } = input;
    // Preserve all three original resize hit zones: each row's bottom, the top of the highest
    // video row, and the outer bottom of the final audio row. Keep them clipped to their
    // respective track pane so none can capture input from the A/V divider or the other pane.
    let header_column = Rect::from_min_max(pos2(hrect.min.x, clip_rect.min.y), pos2(hrect.max.x, clip_rect.max.y));
    let last_audio = r.kind == TrackKind::Audio && r.index + 1 == seq.audio_tracks.len();
    let bottom = Rect::from_min_max(pos2(hrect.min.x, hrect.max.y - 4.0), pos2(hrect.max.x, hrect.max.y + 2.0));
    let top = (r.kind == TrackKind::Video && r.index + 1 == seq.video_tracks.len())
        .then(|| ("resizeTop", Rect::from_min_max(pos2(hrect.min.x, hrect.min.y - 2.0), pos2(hrect.max.x, hrect.min.y + 3.0)).intersect(header_column)));
    for (name, edge) in std::iter::once(("resize", bottom.intersect(if last_audio { header_column } else { visible }))).chain(top) {
        if edge.height() <= 0.0 {
            continue;
        }
        let resize = ui.interact(edge, egui::Id::unique(("header-resize", name, r.track.0)), Sense::click_and_drag());
        app.auto.add(&format!("timeline.track.{label}.{name}"), edge, "Track height");
        let (height_now, window, slots, height, scroll) = if r.kind == TrackKind::Video {
            (
                app.ui.timeline.video_track_h,
                vclip.height(),
                seq.video_tracks.len() as f32 + 1.0 / 3.0,
                &mut app.ui.timeline.video_track_h,
                &mut app.ui.timeline.v_scroll,
            )
        } else {
            (
                app.ui.timeline.audio_track_h,
                aclip.height(),
                seq.audio_tracks.len() as f32 + 1.0 / 3.0,
                &mut app.ui.timeline.audio_track_h,
                &mut app.ui.timeline.a_scroll,
            )
        };
        let min_height = super::scrollbars::track_min_height(window, slots);
        if resize.ctx.has_accesskit_node(resize.id) {
            resize.ctx.accesskit_node_builder(resize.id, |node| {
                node.set_role(egui::accesskit::Role::Slider);
                node.set_label(tl!("Track height"));
                node.set_numeric_value(f64::from(height_now));
                node.set_min_numeric_value(f64::from(min_height));
                node.set_max_numeric_value(f64::from(220.0f32.max(min_height * 2.0)));
            });
        }
        if resize.hovered() || resize.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
        if resize.clicked() || resize.drag_started() {
            resize.request_focus();
        }
        let keyboard_delta = if resize.has_focus() {
            ui.input(|input| {
                if input.key_pressed(egui::Key::ArrowUp) {
                    -1.0
                } else if input.key_pressed(egui::Key::ArrowDown) {
                    1.0
                } else {
                    0.0
                }
            })
        } else {
            0.0
        };
        if resize.dragged() || keyboard_delta != 0.0 {
            let delta = if resize.dragged() { resize.drag_delta().y } else { keyboard_delta };
            let dy = if r.kind == TrackKind::Audio { -delta } else { delta };
            super::scrollbars::resize_tracks_from_timeline(height, scroll, window, slots, (window - 8.0).max(1.0), dy);
            app.tl.track_anim = None;
        }
    }
    let resp = ui.interact(
        Rect::from_min_max(pos2(hrect.min.x + 90.0, hrect.min.y + 26.0), hrect.max).intersect(visible),
        egui::Id::unique(("hdr", r.track.0)),
        Sense::click(),
    );
    if resp.double_clicked() {
        toggle_track_height(app, r.kind);
    }
}

fn toggle_track_height(app: &mut FilmcraftApp, kind: TrackKind) {
    app.tl.track_anim = None;
    let height = if kind == TrackKind::Video { &mut app.ui.timeline.video_track_h } else { &mut app.ui.timeline.audio_track_h };
    *height = if *height < 50.0 { 64.0 } else { 30.0 };
}

/// A track header's upper line: sync-lock, then output enable for a video track, or mute,
/// solo, voiceover and automation for an audio track.
#[allow(clippy::too_many_arguments)]
fn header_line_buttons(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    seq: &Sequence,
    r: &Row,
    tr: &filmcraft_project::Track,
    visible: Rect,
    hrect: Rect,
    label: &str,
    t: &Tokens,
    actions: &mut Vec<(String, Value)>,
    vo_action: &mut Option<crate::panels::voiceover::Action>,
) {
    let upper_y = r.rect.min.y + 11.0;
    let small = |x: f32, y: f32| Rect::from_center_size(pos2(hrect.min.x + x, y), vec2(18.0, 18.0));
    let mut x = 102.0;
    let sr = small(x, upper_y);
    let sresp =
        crate::widgets::icon_toggle_clipped(ui, sr, visible, Icon::SyncLock, tr.sync_lock, t, egui::Id::unique(("syncLock", r.track.0)), Some(t.text_dim));
    app.auto.add(&format!("timeline.track.{label}.syncLock"), sr, "Toggle Sync Lock");
    if sresp.clicked() {
        actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "syncLock": !tr.sync_lock})));
    }
    x += 24.0;
    if r.kind == TrackKind::Video {
        let er = small(x, upper_y);
        let eresp = crate::widgets::icon_toggle_clipped(
            ui,
            er,
            visible,
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
        let mresp = crate::widgets::letter_toggle_clipped(
            ui,
            mr,
            visible,
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
        let so =
            crate::widgets::letter_toggle_clipped(ui, s2, visible, "S", tr.solo, Color32::from_rgb(0xf0, 0xf0, 0x4f), t, egui::Id::unique(("solo", r.track.0)));
        app.auto.add(&format!("timeline.track.{label}.solo"), s2, "Solo Track");
        if so.clicked() {
            actions.push(("timeline.setTrack".into(), json!({"track": r.track.0, "solo": !tr.solo})));
        }
        let vr = small(x + 42.0, upper_y);
        if let Some(a) = crate::panels::voiceover::header_button(app, ui, r.track, vr, visible, label, t) {
            *vo_action = Some(a);
        }
        let kr = small(x + 64.0, upper_y);
        crate::panels::timeline_automation::header_button(app, ui, seq, r, kr, visible, label, t);
    }
}

#[cfg(test)]
mod tests {
    use filmcraft_engine::Session;
    use filmcraft_project::TrackKind;

    use super::toggle_track_height;
    use crate::FilmcraftApp;

    #[test]
    fn header_height_toggle_cancels_stale_track_slider_animation() {
        let mut session = Session::default();
        assert!(session.execute("file.openDemoProject", serde_json::json!({})).is_ok());
        let mut app = FilmcraftApp::new(session);
        app.ui.timeline.video_track_h = 30.0;
        app.tl.track_anim = Some([90.0, 8.0, 40.0, 2.0]);

        toggle_track_height(&mut app, TrackKind::Video);

        assert_eq!(app.ui.timeline.video_track_h, 64.0);
        assert_eq!(app.tl.track_anim, None);
    }
}
