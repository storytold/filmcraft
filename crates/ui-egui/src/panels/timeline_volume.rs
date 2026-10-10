//! The Volume line on audio clips (#223): the clip's Volume level over time, with its keyframes,
//! on the same scale as the track faders. As in Premiere, drag the line up or down to change the
//! level (with keyframes: the stretch between the two around the pointer), Cmd/Ctrl-click it or
//! click it with the Pen tool to add a keyframe, drag keyframes, right-click one to delete it.
//! Presses on the line go through the Timeline's own hit-testing ([`super::timeline_hit`]), so a
//! right-click on it still opens the clip menu.
//!
//! Automation ids: `timeline.clip.<id>.volume` (a point on the line),
//! `timeline.clip.<id>.volume.kf.<n>` (the keyframes) and `….kf.<n>.delete` (their menu).

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::mixer::{FADER_MAX_DB, FADER_MIN_DB};
use filmcraft_project::{ClipId, Param, Sequence, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::mixer::{db_to_pos, pos_to_db};
use super::timeline::{Layout, Row};
use super::timeline_hit::EDGE_PX;
use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Tool;
use crate::theme::Tokens;

const KEY_COL: Color32 = Color32::from_rgb(0xc8, 0xc8, 0xc8);

/// Where the line goes in a clip drawn at `body`: under the name, down to the bottom edge.
fn band(body: Rect) -> Rect {
    Rect::from_min_max(pos2(body.min.x, body.min.y + 17.0), pos2(body.max.x, body.max.y - 3.0))
}

fn level(it: &TrackItem) -> Option<&Param> {
    it.effect("volume")?.param("level")
}

/// The level (dB) the clip plays at timeline time `t`.
fn level_at(it: &TrackItem, t: Tick) -> f64 {
    level(it).map_or(0.0, |p| p.f64_at(it.source_time_at(t)))
}

/// The clip's Volume as a gain at timeline time `t`, for its waveform (1 when Volume is off or
/// bypassed, as the mixer plays it).
pub fn gain_at(it: &TrackItem, t: Tick) -> f32 {
    match it.effect("volume").filter(|e| e.enabled && !e.param("bypass").and_then(|p| p.value.as_bool()).unwrap_or(false)) {
        Some(e) => filmcraft_render::audio::db_to_gain(e.f64_at("level", it.source_time_at(t))),
        None => 1.0,
    }
}

/// Timeline time of media time `m`, the inverse of [`TrackItem::moving_source_time_at`].
fn timeline_time(it: &TrackItem, m: Tick) -> Tick {
    let rel = if it.reverse { it.source_out().0.saturating_sub(1).saturating_sub(m.0) } else { m.0.saturating_sub(it.source_in.0) };
    Tick(it.start.0.saturating_add((rel as f64 / it.speed.abs().max(1e-6)).round() as i64))
}

fn x_of(it: &TrackItem, body: Rect, t: Tick) -> f32 {
    body.min.x + (t.0.saturating_sub(it.start.0) as f64 / it.duration.0.max(1) as f64) as f32 * body.width()
}

fn t_at(it: &TrackItem, body: Rect, x: f32) -> Tick {
    let f = ((x - body.min.x) / body.width().max(1.0)).clamp(0.0, 1.0) as f64;
    Tick(it.start.0.saturating_add((f * it.duration.0 as f64) as i64))
}

fn y_of(body: Rect, db: f64) -> f32 {
    let b = band(body);
    b.max.y - db_to_pos(db) * b.height()
}

/// The line from `x0` to `x1`; keyframes bend it, so it then gets a point every 2 px.
fn line(it: &TrackItem, body: Rect, x0: f32, x1: f32) -> Vec<Pos2> {
    let n = if level(it).is_some_and(Param::is_animated) { (((x1 - x0) / 2.0).ceil() as usize).clamp(1, 4096) } else { 1 };
    (0..=n)
        .map(|i| {
            let x = x0 + (x1 - x0) * i as f32 / n as f32;
            pos2(x, y_of(body, level_at(it, t_at(it, body, x))))
        })
        .collect()
}

/// The keyframes: media time and where each is drawn.
fn keys(it: &TrackItem, body: Rect) -> Vec<(Tick, Pos2)> {
    let Some(p) = level(it) else { return Vec::new() };
    p.keyframes.iter().map(|k| (k.time, pos2(x_of(it, body, timeline_time(it, k.time)), y_of(body, k.value.as_f64().unwrap_or(0.0))))).collect()
}

/// Draw the line over an audio clip drawn at `body`: white with a dark shadow.
pub fn paint(p: &egui::Painter, body: Rect, it: &TrackItem) {
    let vis = p.clip_rect().intersect(body);
    if band(body).height() < 6.0 || vis.width() <= 0.0 {
        return;
    }
    let p = p.with_clip_rect(vis);
    let pts = line(it, body, vis.min.x, vis.max.x);
    p.add(egui::Shape::line(pts.iter().map(|q| *q + vec2(0.0, 1.0)).collect(), Stroke::new(1.0, Color32::BLACK)));
    p.add(egui::Shape::line(pts, Stroke::new(1.0, Color32::WHITE)));
    for (_, c) in keys(it, body).into_iter().filter(|(_, c)| vis.expand(6.0).contains(*c)) {
        icons::paint(&p, Rect::from_center_size(c, vec2(9.0, 9.0)), Icon::Keyframe, KEY_COL);
    }
}

/// A drag of a clip's Volume line, pressed at height `y` where the line was at `from` dB. `keys`
/// are the keyframes on either side of that point, which move with it (none: the level is not
/// animated). `last` is the level sent last: the first change starts the drag's undo step.
#[derive(Clone, Debug)]
pub struct LineDrag {
    clip: ClipId,
    from: f64,
    y: f32,
    keys: [Option<(Tick, f64)>; 2],
    last: Option<f64>,
}

fn find<'a>(seq: &'a Sequence, layout: &'a Layout, clip: ClipId) -> Option<(&'a Row, &'a TrackItem)> {
    let (tid, it) = seq.find_item(clip)?;
    Some((layout.rows.iter().find(|r| r.track == tid)?, it))
}

fn body(layout: &Layout, r: &Row, it: &TrackItem) -> Rect {
    Rect::from_min_max(pos2(layout.x_of(it.start), r.rect.min.y + 1.0), pos2(layout.x_of(it.end()), r.rect.max.y - 1.0))
}

/// Whether `pos` is on `clip`'s Volume line, within 4 px.
pub fn on_line(seq: &Sequence, layout: &Layout, clip: ClipId, pos: Pos2) -> bool {
    let Some((r, it)) = find(seq, layout, clip) else { return false };
    if r.kind != TrackKind::Audio || r.lane || seq.track(r.track).is_none_or(|t| t.locked) {
        return false;
    }
    let body = body(layout, r, it);
    band(body).height() >= 6.0 && (pos.y - y_of(body, level_at(it, t_at(it, body, pos.x)))).abs() <= 4.0
}

/// A press on `clip`'s line at `at` that became a drag.
pub fn start(seq: &Sequence, layout: &Layout, clip: ClipId, at: Pos2) -> Option<LineDrag> {
    let (r, it) = find(seq, layout, clip)?;
    let t = t_at(it, body(layout, r, it), at.x);
    let m = it.source_time_at(t);
    let keys = level(it).filter(|p| p.is_animated()).map_or([None, None], |p| {
        let i = p.keyframes.partition_point(|k| k.time <= m);
        let kv = |j: Option<usize>| j.and_then(|j| p.keyframes.get(j)).map(|k| (k.time, k.value.as_f64().unwrap_or(0.0)));
        [kv(i.checked_sub(1)), kv(Some(i))]
    });
    Some(LineDrag { clip, from: level_at(it, t), y: at.y, keys, last: None })
}

/// Follow the pointer, now at height `y`. The clip changes as the line moves, so it is heard.
pub fn drag(app: &mut FilmcraftApp, ui: &egui::Ui, seq: &Sequence, layout: &Layout, mut d: LineDrag, y: f32) -> LineDrag {
    let Some((r, it)) = find(seq, layout, d.clip) else { return d };
    let db = pos_to_db(db_to_pos(d.from) + (d.y - y) / band(body(layout, r, it)).height().max(1.0));
    tip(ui, db);
    if d.last == Some(db) {
        return d;
    }
    let begin = d.last.is_none();
    let mut acts = Vec::new();
    if d.keys.iter().all(Option::is_none) {
        acts.push(("effects.setParam", json!({"clip": d.clip.0, "effect": "volume", "param": "level", "value": db, "merge": true, "begin": begin})));
    }
    for (n, (m, v)) in d.keys.iter().flatten().enumerate() {
        let v = (v + db - d.from).clamp(FADER_MIN_DB, FADER_MAX_DB);
        let p = json!({"clip": d.clip.0, "effect": "volume", "param": "level", "mediaTime": m.0, "value": v, "merge": true, "begin": begin && n == 0});
        acts.push(("effects.setKeyframe", p));
    }
    run(app, acts);
    d.last = Some(db);
    d
}

/// The Pen tool or a Cmd/Ctrl-click on `clip`'s line at `x`: a keyframe there.
pub fn add_keyframe(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, clip: ClipId, x: f32) {
    let Some((r, it)) = find(seq, layout, clip) else { return };
    let t = seq.settings.frame_rate.snap_nearest(t_at(it, body(layout, r, it), x));
    run(app, vec![("effects.addKeyframe", json!({"clip": clip.0, "effect": "volume", "param": "level", "time": t.0}))]);
}

fn run(app: &mut FilmcraftApp, acts: Vec<(&str, Value)>) {
    for (id, p) in acts {
        if let Err(e) = app.session.execute(id, p) {
            app.ui.status = e.to_string();
        }
    }
}

/// The level next to the pointer while dragging: one line, always as wide, so it does not jump.
fn tip(ui: &egui::Ui, db: f64) {
    let text = if db <= FADER_MIN_DB { format!("{:>7} dB", "-∞") } else { format!("{db:>+7.2} dB") };
    egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), egui::Id::new("tl-volume-tip"), egui::PopupAnchor::Pointer).show(|ui| {
        ui.add(egui::Label::new(egui::RichText::new(text).font(Tokens::mono(12.0))).wrap_mode(egui::TextWrapMode::Extend));
    });
}

/// A keyframe being dragged: `index` of `clip`, grabbed `grab` away from its centre.
#[derive(Clone, Copy)]
struct KeyDrag {
    clip: ClipId,
    index: usize,
    grab: egui::Vec2,
    begun: bool,
}

fn key_drag_id() -> egui::Id {
    egui::Id::new("tl-volume-key-drag")
}

/// The keyframes on the lines of the audio clips in view: drag one in time and level, right-click
/// it to delete it. Runs after the timeline's own interaction, so they are above the clip.
pub fn interact(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, aclip: Rect) {
    let held: Option<KeyDrag> = ui.data(|d| d.get_temp(key_drag_id()));
    if held.is_none() && (app.tl.drag.is_some() || !matches!(app.ui.tool, Tool::Selection | Tool::Pen)) {
        return;
    }
    let area = aclip.intersect(layout.content);
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let rate = seq.settings.frame_rate;
    let mut next = held;
    let mut acts: Vec<(&str, Value)> = Vec::new();
    for r in layout.rows.iter().filter(|r| r.kind == TrackKind::Audio && !r.lane) {
        let Some(tr) = seq.track(r.track).filter(|t| !t.locked) else { continue };
        let row = r.rect.intersect(area);
        if row.height() <= 0.0 {
            continue;
        }
        for it in &tr.items {
            let body = body(layout, r, it);
            let vis = body.intersect(row);
            if vis.width() <= 0.0 || band(body).height() < 6.0 {
                continue;
            }
            // a point on the line for agents, away from the trim zones at the edges
            let edge = EDGE_PX.min(body.width() / 3.0);
            let (x0, x1) = (vis.min.x.max(body.min.x + edge), vis.max.x.min(body.max.x - edge));
            if x1 > x0 {
                let xm = (x0 + x1) / 2.0;
                let c = pos2(xm, y_of(body, level_at(it, t_at(it, body, xm))));
                app.auto.add(&format!("timeline.clip.{}.volume", it.id.0), Rect::from_center_size(c, vec2(12.0, 8.0)), "Volume");
            }
            let ks = keys(it, body);
            for (i, &(time, c)) in ks.iter().enumerate() {
                if !vis.expand2(vec2(5.0, 0.0)).contains(c) {
                    continue;
                }
                let kr = Rect::from_center_size(c, vec2(11.0, 11.0));
                let resp = ui.interact(kr, egui::Id::new(("tl-volume-kf", it.id.0, i)), Sense::click_and_drag());
                app.auto.add(&format!("timeline.clip.{}.volume.kf.{i}", it.id.0), kr, "Volume keyframe");
                let mine = held.filter(|h| h.clip == it.id && h.index == i);
                if resp.hovered() || mine.is_some() {
                    icons::paint(&ui.painter().with_clip_rect(area), Rect::from_center_size(c, vec2(9.0, 9.0)), Icon::Keyframe, Color32::WHITE);
                    ui.ctx().set_cursor_icon(CursorIcon::Move);
                }
                if resp.drag_started()
                    && let Some(p) = pointer
                {
                    next = Some(KeyDrag { clip: it.id, index: i, grab: p - c, begun: false });
                }
                if let (Some(h), Some(p)) = (mine, pointer)
                    && resp.dragged()
                {
                    let q = p - h.grab;
                    // on a frame, at least a frame away from the keyframes on either side
                    let fd = rate.frame_duration();
                    let now = timeline_time(it, time);
                    let (mut lo, mut hi) = (it.start, Tick(it.end().0.saturating_sub(fd.0)));
                    for j in [i.checked_sub(1), i.checked_add(1)].into_iter().flatten() {
                        if let Some(&(m, _)) = ks.get(j) {
                            let n = timeline_time(it, m);
                            if n < now {
                                lo = lo.max(Tick(n.0.saturating_add(fd.0)));
                            } else {
                                hi = hi.min(Tick(n.0.saturating_sub(fd.0)));
                            }
                        }
                    }
                    let t = if lo <= hi { rate.snap_nearest(t_at(it, body, q.x)).max(lo).min(hi) } else { now };
                    let db = pos_to_db((band(body).max.y - q.y) / band(body).height());
                    if resp.drag_delta() != egui::Vec2::ZERO {
                        let to = it.moving_source_time_at(t);
                        let p = json!({"clip": h.clip.0, "effect": "volume", "param": "level", "mediaTime": time.0, "to": to.0, "value": db, "merge": true, "begin": !h.begun});
                        acts.push(("effects.moveKeyframe", p));
                        next = Some(KeyDrag { begun: true, ..h });
                    }
                    tip(ui, db);
                }
                resp.context_menu(|ui| {
                    let b = ui.button(tl!("Delete"));
                    app.auto.add(&format!("timeline.clip.{}.volume.kf.{i}.delete", it.id.0), b.rect, "Delete");
                    if b.clicked() {
                        acts.push(("effects.deleteKeyframe", json!({"clip": it.id.0, "effect": "volume", "param": "level", "mediaTime": time.0})));
                        ui.close();
                    }
                });
            }
        }
    }
    if !ui.input(|i| i.pointer.any_down()) {
        next = None;
    }
    match next {
        Some(h) => ui.data_mut(|d| {
            d.insert_temp(key_drag_id(), h);
        }),
        None => ui.data_mut(|d| d.remove::<KeyDrag>(key_drag_id())),
    }
    run(app, acts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{ItemId, Label};

    fn clip(speed: f64, reverse: bool) -> TrackItem {
        TrackItem {
            id: ClipId(1),
            item: ItemId(1),
            name: String::new(),
            label: Label::Iris,
            start: Tick(1_000),
            duration: Tick(4_000),
            source_in: Tick(500),
            speed,
            reverse,
            enabled: true,
            link: None,
            group: None,
            effects: filmcraft_project::effect::intrinsic_audio(),
            markers: Vec::new(),
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: Default::default(),
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        }
    }

    #[test]
    fn keyframes_sit_where_their_media_time_plays() {
        for (speed, reverse) in [(1.0, false), (2.0, false), (0.5, false), (1.0, true), (2.0, true)] {
            let it = clip(speed, reverse);
            for t in [1_000, 1_001, 2_500, 4_999] {
                let m = it.moving_source_time_at(Tick(t));
                assert!((timeline_time(&it, m).0 - t).abs() <= 1, "speed {speed} reverse {reverse}: {t}");
            }
        }
    }

    #[test]
    fn damaged_keyframe_times_do_not_panic() {
        for (speed, reverse) in [(1.0, false), (1e-12, true), (f64::NAN, false), (-3.0, true)] {
            let mut it = clip(speed, reverse);
            let p = it.effect_mut("volume").and_then(|e| e.param_mut("level")).unwrap();
            p.put_keyframe(Tick(i64::MIN), filmcraft_project::ParamValue::Float(f64::NAN));
            p.put_keyframe(Tick(i64::MAX), filmcraft_project::ParamValue::Float(1e300));
            let body = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 80.0));
            assert_eq!(keys(&it, body).len(), 2);
        }
    }

    #[test]
    fn the_waveform_gain_is_the_volume_that_plays() {
        let mut it = clip(1.0, false);
        assert_eq!(gain_at(&it, Tick(2_000)), 1.0);
        let e = it.effect_mut("volume").unwrap();
        e.param_mut("level").unwrap().value = filmcraft_project::ParamValue::Float(-6.0);
        assert!((gain_at(&it, Tick(2_000)) - 0.501).abs() < 0.01);
        let e = it.effect_mut("volume").unwrap();
        e.param_mut("bypass").unwrap().value = filmcraft_project::ParamValue::Bool(true);
        assert_eq!(gain_at(&it, Tick(2_000)), 1.0, "bypassed");
    }

    #[test]
    fn the_line_follows_the_level() {
        let mut it = clip(1.0, false);
        let body = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 80.0));
        let flat = line(&it, body, 0.0, 400.0);
        assert_eq!(flat.len(), 2);
        assert!(flat.iter().all(|p| (p.y - y_of(body, 0.0)).abs() < 1e-3));
        let p = it.effect_mut("volume").and_then(|e| e.param_mut("level")).unwrap();
        p.put_keyframe(Tick(500), filmcraft_project::ParamValue::Float(0.0));
        p.put_keyframe(Tick(4_500), filmcraft_project::ParamValue::Float(-20.0));
        let ramp = line(&it, body, 0.0, 400.0);
        assert!(ramp.len() > 100);
        assert!(ramp.windows(2).all(|w| w[1].y >= w[0].y), "goes down as the level falls");
        assert!(ramp.last().unwrap().y > y_of(body, -19.0));
        // damaged geometry draws nothing silly rather than looping or panicking
        assert_eq!(line(&it, body, 0.0, f32::NAN).len(), 2);
        assert!(line(&it, body, 0.0, f32::INFINITY).len() <= 4097);
    }
}
