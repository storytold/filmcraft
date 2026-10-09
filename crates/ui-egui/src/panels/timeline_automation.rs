//! Track keyframes in the timeline.
//!
//! An audio track can show one of its automation lanes (Volume, Panner, Mute, send levels, insert
//! parameters) instead of clip keyframes: the header's keyframe button picks it (Premiere's
//! "Show Keyframes ▸ Track Keyframes"). The lane is drawn across the whole track as a line with
//! keyframe diamonds; the Pen tool (or ⌘-click) adds keyframes, dragging a diamond moves it in time
//! and value, right-click deletes. Everything goes through `mixer.setKeyframe` /
//! `mixer.moveKeyframe` / `mixer.deleteKeyframe`.
//!
//! Automation ids: `timeline.track.A1.keyframes` (the header button) with its menu entries
//! (`timeline.track.A1.keyframes.<lane>`, `….clip`), `timeline.track.A1.lane` (the band) and
//! `timeline.track.A1.lane.kf.<n>` (the diamonds).

use egui::{Align2, Color32, CursorIcon, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::mixer::{LANE_MUTE, LANE_PAN, LANE_VOLUME, lane_info, parse_fx_lane, send_lane};
use filmcraft_project::{ParamKind, Sequence, Track, TrackKind};
use filmcraft_time::Tick;
use serde_json::json;

use super::timeline::{Layout, Row};
use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Tool;
use crate::theme::Tokens;

const LANE_COL: Color32 = Color32::from_rgb(0xe8, 0xc5, 0x47);

/// The lanes an audio track can show: (key, label).
pub fn lane_options(tr: &Track) -> Vec<(String, String)> {
    let mut v =
        vec![(LANE_VOLUME.to_string(), "Volume".to_string()), (LANE_PAN.to_string(), "Panner".to_string()), (LANE_MUTE.to_string(), "Mute".to_string())];
    for (i, s) in tr.mixer.sends.iter().enumerate() {
        let _ = s;
        v.push((send_lane(i), format!("Send {} Level", i + 1)));
    }
    for (slot, e) in tr.effects.iter().enumerate() {
        let Some(def) = e.def() else { continue };
        for p in def.params.iter().filter(|p| matches!(p.kind, ParamKind::Float { .. }) && p.animatable) {
            v.push((filmcraft_project::mixer::fx_lane(slot, p.id), format!("{}: {}", def.name, p.label)));
        }
    }
    v
}

/// Value range of a lane: (min, max, hold).
fn range(tr: &Track, key: &str) -> (f64, f64, bool) {
    if let Some(i) = lane_info(key) {
        return (i.min, i.max, i.hold);
    }
    if let Some((slot, pid)) = parse_fx_lane(key)
        && let Some(ParamKind::Float { soft_min, soft_max, .. }) = tr.effects.get(slot).and_then(|e| e.def()).and_then(|d| d.param(pid)).map(|p| p.kind.clone())
    {
        return (soft_min, soft_max, false);
    }
    (0.0, 1.0, false)
}

/// 0 (bottom) … 1 (top) for a value.
fn norm(tr: &Track, key: &str, v: f64) -> f32 {
    if key == LANE_VOLUME || key.starts_with("send.") {
        return super::mixer::db_to_pos(v);
    }
    let (lo, hi, _) = range(tr, key);
    (((v - lo) / (hi - lo).max(1e-9)) as f32).clamp(0.0, 1.0)
}

fn denorm(tr: &Track, key: &str, n: f32) -> f64 {
    if key == LANE_VOLUME || key.starts_with("send.") {
        return super::mixer::pos_to_db(n);
    }
    let (lo, hi, hold) = range(tr, key);
    let v = lo + (hi - lo) * n.clamp(0.0, 1.0) as f64;
    if hold { v.round() } else { v }
}

/// The stored automation (shown even when the strip's mode is Off).
fn lane_at(tr: &Track, key: &str, t: Tick) -> f64 {
    match tr.lane(key).filter(|p| p.is_animated()) {
        Some(p) if range(tr, key).2 => {
            let i = p.keyframes.partition_point(|k| k.time <= t);
            p.keyframes[i.saturating_sub(1)].value.as_f64().unwrap_or(0.0)
        }
        Some(p) => p.scalar_at(t),
        None => tr.lane_static(key).unwrap_or(0.0),
    }
}

fn band(r: &Row) -> Rect {
    Rect::from_min_max(pos2(r.rect.min.x, r.rect.min.y + 17.0), pos2(r.rect.max.x, r.rect.max.y - 4.0))
}

/// Header button (diamond) choosing clip keyframes or a track lane.
#[allow(clippy::too_many_arguments)]
pub fn header_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, r: &Row, rect: Rect, visible: Rect, label: &str, t: &Tokens) {
    let Some(tr) = seq.track(r.track) else { return };
    let showing = app.ui.timeline.track_lanes.get(&r.track.0).cloned();
    let resp =
        crate::widgets::icon_toggle(ui, rect.intersect(visible), Icon::Keyframe, showing.is_some(), t, egui::Id::new(("lanebtn", r.track.0)), Some(LANE_COL))
            .on_hover_text("Show Keyframes");
    app.auto.add(&format!("timeline.track.{label}.keyframes"), rect, "Show Keyframes");
    let opts = lane_options(tr);
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(170.0);
        let c = ui.selectable_label(showing.is_none(), "Clip Keyframes");
        app.auto.add(&format!("timeline.track.{label}.keyframes.clip"), c.rect, "Clip Keyframes");
        if c.clicked() {
            app.ui.timeline.track_lanes.remove(&r.track.0);
        }
        ui.separator();
        ui.label(egui::RichText::new("Track Keyframes").small());
        for (k, l) in &opts {
            let e = ui.selectable_label(showing.as_deref() == Some(k.as_str()), l);
            app.auto.add(&format!("timeline.track.{label}.keyframes.{k}"), e.rect, l);
            if e.clicked() {
                app.ui.timeline.track_lanes.insert(r.track.0, k.clone());
            }
        }
    });
}

#[derive(Clone, Copy)]
struct KfDrag {
    track: u64,
    index: usize,
    orig: Tick,
    /// pointer offset at grab
    grab: egui::Vec2,
}

fn drag_id() -> egui::Id {
    egui::Id::new("tl-lane-kf-drag")
}

/// Draw the shown lanes (after clips) and handle their editing (after the timeline's own interaction,
/// so keyframes sit on top).
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, aclip: Rect) {
    if app.ui.timeline.track_lanes.is_empty() {
        return;
    }
    let rows: Vec<Row> = layout.rows.iter().filter(|r| r.kind == TrackKind::Audio).cloned().collect();
    let drag: Option<KfDrag> = ui.data(|d| d.get_temp(drag_id()));
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let mut acts: Vec<(&str, serde_json::Value)> = Vec::new();
    for r in &rows {
        let Some(key) = app.ui.timeline.track_lanes.get(&r.track.0).cloned() else { continue };
        let Some(tr) = seq.track(r.track) else { continue };
        if !lane_options(tr).iter().any(|(k, _)| *k == key) {
            continue;
        }
        let label = format!("A{}", r.index + 1);
        let b = band(r);
        let vis = b.intersect(aclip).intersect(layout.content);
        if vis.height() <= 2.0 {
            continue;
        }
        let p = ui.painter().with_clip_rect(vis.expand2(vec2(0.0, 5.0)).intersect(aclip));
        let y_of = |v: f64| b.max.y - norm(tr, &key, v) * b.height();
        // the curve
        let mut pts = Vec::new();
        let mut x = vis.min.x;
        while x <= vis.max.x + 2.0 {
            pts.push(pos2(x, y_of(lane_at(tr, &key, layout.tick_at(x)))));
            x += 2.0;
        }
        p.add(egui::Shape::line(pts.iter().map(|q| *q + vec2(0.0, 1.0)).collect(), Stroke::new(1.0, Color32::BLACK)));
        p.add(egui::Shape::line(pts, Stroke::new(1.5, LANE_COL)));
        let name = lane_options(tr).into_iter().find(|(k, _)| *k == key).map(|(_, l)| l).unwrap_or_default();
        p.text(pos2(vis.max.x - 4.0, b.min.y + 1.0), Align2::RIGHT_TOP, &name, Tokens::ui(9.5), LANE_COL.gamma_multiply(0.85));
        app.auto.add(&format!("timeline.track.{label}.lane"), vis, &format!("{name} track keyframes"));
        // add keyframes: Pen tool, or ⌘-click with any tool
        let adding = app.ui.tool == Tool::Pen || ui.input(|i| i.modifiers.command);
        if adding {
            let bresp = ui.interact(vis, egui::Id::new(("lane-band", r.track.0)), Sense::click());
            if bresp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
            if bresp.clicked()
                && let Some(pp) = pointer
            {
                let tt = layout.tick_at(pp.x).max(Tick::ZERO);
                let n = ((b.max.y - pp.y) / b.height()).clamp(0.0, 1.0);
                acts.push(("mixer.setKeyframe", json!({"strip": r.track.0, "lane": key, "time": tt.0, "value": denorm(tr, &key, n)})));
            }
        }
        // keyframe diamonds
        let kfs = tr.lane_keyframes(&key).to_vec();
        for (i, k) in kfs.iter().enumerate() {
            let v = k.value.as_f64().unwrap_or(0.0);
            let mut c = pos2(layout.x_of(k.time), y_of(v));
            let dragging = drag.filter(|d| d.track == r.track.0 && d.index == i);
            if let (Some(d), Some(pp)) = (dragging, pointer) {
                c = pp - d.grab;
                c.y = c.y.clamp(b.min.y, b.max.y);
            }
            if c.x < vis.min.x - 6.0 || c.x > vis.max.x + 6.0 {
                continue;
            }
            let hr = Rect::from_center_size(c, vec2(11.0, 11.0));
            let resp = ui.interact(hr, egui::Id::new(("lane-kf", r.track.0, i)), Sense::click_and_drag());
            let hot = resp.hovered() || dragging.is_some();
            icons::paint(&p, Rect::from_center_size(c, vec2(9.0, 9.0)), Icon::Keyframe, if hot { Color32::WHITE } else { LANE_COL });
            app.auto.add(&format!("timeline.track.{label}.lane.kf.{i}"), hr, &format!("{name} keyframe {v:.1}"));
            if resp.drag_started()
                && let Some(pp) = pointer
            {
                ui.data_mut(|d| d.insert_temp(drag_id(), KfDrag { track: r.track.0, index: i, orig: k.time, grab: pp - c }));
            }
            if resp.drag_stopped()
                && let Some(d) = dragging
            {
                let nt = layout.tick_at(c.x).max(Tick::ZERO);
                let nv = denorm(tr, &key, ((b.max.y - c.y) / b.height()).clamp(0.0, 1.0));
                acts.push(("mixer.moveKeyframe", json!({"strip": r.track.0, "lane": key, "time": d.orig.0, "newTime": nt.0, "value": nv})));
                ui.data_mut(|dd| dd.remove::<KfDrag>(drag_id()));
            }
            if dragging.is_some() {
                let nv = denorm(tr, &key, ((b.max.y - c.y) / b.height()).clamp(0.0, 1.0));
                egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::new("lane-kf-tip"), egui::PopupAnchor::Pointer).show(|ui| {
                    ui.label(if key == LANE_VOLUME { format!("{nv:.2} dB") } else { format!("{nv:.2}") });
                });
            }
            let time = k.time;
            let key2 = key.clone();
            resp.context_menu(|ui| {
                let b = ui.button("Delete");
                app.auto.add(&format!("timeline.track.{label}.lane.kf.{i}.delete"), b.rect, "Delete");
                if b.clicked() {
                    acts.push(("mixer.deleteKeyframe", json!({"strip": r.track.0, "lane": key2, "time": time.0})));
                    ui.close();
                }
            });
        }
    }
    if drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
        ui.data_mut(|d| d.remove::<KfDrag>(drag_id()));
    }
    for (id, p) in acts {
        if let Err(e) = app.session.execute(id, p) {
            app.ui.status = e.to_string();
        }
    }
}
