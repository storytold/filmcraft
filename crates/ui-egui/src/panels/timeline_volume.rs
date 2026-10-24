//! Clip keyframe lines on the timeline: the Volume line on audio clips (#223), and on any clip the
//! parameter picked with "Show Clip Keyframes" in its context menu (#2213), Premiere's rubber
//! band. The line is the parameter's value over time, with its keyframes; Volume and other dB
//! parameters use the scale of the track faders, the rest their slider range. As in Premiere, drag
//! the line up or down to change the value (with keyframes: the stretch between the two around the
//! pointer), Cmd/Ctrl-click it or click it with the Pen tool to add a keyframe, drag keyframes,
//! right-click one to delete it. Presses on the line go through the Timeline's own hit-testing
//! ([`super::timeline_hit`]), so a right-click on it still opens the clip menu.
//!
//! Audio clips show Volume ▸ Level unless another parameter is picked; video clips show nothing
//! until one is. The choice is kept per clip in [`crate::state::TimelineView::clip_lanes`] as
//! `<effect id>#<n>/<param id>` (`n`: which instance of that effect on the clip) or `none`.
//!
//! Automation ids: `timeline.clip.<id>.volume` (a point on a Volume line, `….keyframes` for other
//! parameters), `….kf.<n>` (the keyframes) and `….kf.<n>.delete` (their menu); the menu entries
//! `timeline.clipMenu.showKeyframes.<key>`.

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::mixer::{FADER_MAX_DB, FADER_MIN_DB};
use filmcraft_project::{ClipId, Param, ParamKind, Sequence, TrackItem, TrackKind};
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

/// The stored choice that hides a clip's line.
pub const NONE: &str = "none";

/// How a parameter's value maps to the height of its line.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Scale {
    /// The fader taper of the mixer (Volume and other dB parameters) instead of a straight line.
    db: bool,
    /// The range the line spans (the parameter's slider range).
    min: f64,
    max: f64,
    /// What a value may be set to (the parameter's hard limits).
    lo: f64,
    hi: f64,
    unit: &'static str,
    decimals: u8,
}

impl Scale {
    /// Height in the band, 0 at the bottom and 1 at the top; values outside the range sit on its
    /// edges.
    fn pos(&self, v: f64) -> f32 {
        if self.db {
            return db_to_pos(v);
        }
        let span = self.max - self.min;
        if span.is_nan() || span <= 0.0 || !v.is_finite() {
            return 0.0;
        }
        (((v - self.min) / span) as f32).clamp(0.0, 1.0)
    }
    /// The value at height `p`, within the limits.
    fn value(&self, p: f32) -> f64 {
        let v = if self.db { pos_to_db(p) } else { self.min + f64::from(p.clamp(0.0, 1.0)) * (self.max - self.min) };
        self.clamp(v)
    }
    fn clamp(&self, v: f64) -> f64 {
        if v.is_finite() { v.clamp(self.lo, self.hi) } else { self.lo.max(self.min) }
    }
    fn text(&self, v: f64) -> String {
        if self.db {
            return if v <= FADER_MIN_DB { format!("{:>7} dB", "-∞") } else { format!("{v:>+7.2} dB") };
        }
        let sep = if self.unit.is_empty() || self.unit == "%" || self.unit == "°" { "" } else { " " };
        format!("{v:.*}{sep}{}", self.decimals as usize, self.unit)
    }
}

/// The parameter a clip's line shows: which of its effects (by index, as the effect commands take
/// it) and which parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    index: usize,
    param: String,
    scale: Scale,
    /// Volume ▸ Level: keeps the `….volume` automation ids of the line audio clips always had.
    volume: bool,
}

/// The scale of parameter `param` of effect `effect`, when it can be drawn as a line (a number or
/// an angle that can have keyframes).
fn scale_of(effect: &str, param: &str) -> Option<Scale> {
    let def = filmcraft_project::find_effect(effect)?.params.iter().find(|d| d.id == param && d.animatable)?;
    match def.kind {
        ParamKind::Float { min, max, soft_min, soft_max, unit, decimals } => {
            let db = unit == "dB";
            let (smin, smax) = if db { (FADER_MIN_DB, FADER_MAX_DB) } else { (soft_min, soft_max) };
            let (lo, hi) = if db { (min.max(FADER_MIN_DB), max.min(FADER_MAX_DB)) } else { (min, max) };
            (smax > smin && hi >= lo).then_some(Scale { db, min: smin, max: smax, lo, hi, unit, decimals })
        }
        ParamKind::Angle => Some(Scale { db: false, min: -360.0, max: 360.0, lo: -36_000.0, hi: 36_000.0, unit: "°", decimals: 1 }),
        _ => None,
    }
}

/// The key that stores a choice: `<effect id>#<n>/<param id>`.
fn key_of(it: &TrackItem, index: usize, param: &str) -> String {
    let id = it.effects.get(index).map_or("", |e| e.effect.as_str());
    let n = it.effects.iter().take(index).filter(|e| e.effect == id).count();
    format!("{id}#{n}/{param}")
}

/// The effect index a stored key names on `it`.
fn index_of(it: &TrackItem, key: &str) -> Option<(usize, String)> {
    let (effect, param) = key.split_once('/')?;
    let (id, n) = effect.split_once('#').map_or((effect, Some(0)), |(id, n)| (id, n.parse::<usize>().ok()));
    let index = it.effects.iter().enumerate().filter(|(_, e)| e.effect == id).nth(n?)?.0;
    Some((index, param.to_string()))
}

/// The line `it` shows on a `kind` track, given the stored choice (`None`: never picked).
pub fn lane_for(choice: Option<&str>, kind: TrackKind, it: &TrackItem) -> Option<Lane> {
    let default = || (kind == TrackKind::Audio).then(|| "volume#0/level".to_string());
    let key = match choice {
        Some(NONE) => return None,
        Some(k) if index_of(it, k).is_some() => k.to_string(),
        // a picked parameter whose effect has gone: the clip's usual line
        _ => default()?,
    };
    let (index, param) = index_of(it, &key)?;
    let e = it.effects.get(index)?;
    e.param(&param)?;
    let scale = scale_of(&e.effect, &param)?;
    Some(Lane { volume: e.effect == "volume" && param == "level", index, param, scale })
}

/// The line `it` shows on row `r` of the timeline.
pub fn lane(layout: &Layout, r: &Row, it: &TrackItem) -> Option<Lane> {
    if r.lane {
        return None;
    }
    lane_for(layout.clip_lanes.get(&it.id.0).map(String::as_str), r.kind, it)
}

/// The parameters a clip can show: (stored key, effect name, parameter label), effect by effect.
pub fn choices(it: &TrackItem) -> Vec<(String, &'static str, &'static str)> {
    let mut out = Vec::new();
    for (i, e) in it.effects.iter().enumerate() {
        let Some(def) = filmcraft_project::find_effect(&e.effect) else { continue };
        for d in &def.params {
            if e.param(d.id).is_some() && scale_of(def.id, d.id).is_some() {
                out.push((key_of(it, i, d.id), def.name, d.label));
            }
        }
    }
    out
}

/// Where the line goes in a clip drawn at `body`: under the name, down to the bottom edge.
fn band(body: Rect) -> Rect {
    Rect::from_min_max(pos2(body.min.x, body.min.y + 17.0), pos2(body.max.x, body.max.y - 3.0))
}

fn param<'a>(it: &'a TrackItem, l: &Lane) -> Option<&'a Param> {
    it.effects.get(l.index)?.param(&l.param)
}

/// The value the clip has at timeline time `t`.
fn value_at(it: &TrackItem, l: &Lane, t: Tick) -> f64 {
    param(it, l).map_or(0.0, |p| p.f64_at(it.source_time_at(t)))
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

fn y_of(body: Rect, l: &Lane, v: f64) -> f32 {
    let b = band(body);
    b.max.y - l.scale.pos(v) * b.height()
}

/// The line from `x0` to `x1`; keyframes bend it, so it then gets a point every 2 px.
fn line(it: &TrackItem, l: &Lane, body: Rect, x0: f32, x1: f32) -> Vec<Pos2> {
    let n = if param(it, l).is_some_and(Param::is_animated) { (((x1 - x0) / 2.0).ceil() as usize).clamp(1, 4096) } else { 1 };
    (0..=n)
        .map(|i| {
            let x = x0 + (x1 - x0) * i as f32 / n as f32;
            pos2(x, y_of(body, l, value_at(it, l, t_at(it, body, x))))
        })
        .collect()
}

/// The keyframes: media time and where each is drawn.
fn keys(it: &TrackItem, l: &Lane, body: Rect) -> Vec<(Tick, Pos2)> {
    let Some(p) = param(it, l) else { return Vec::new() };
    p.keyframes.iter().map(|k| (k.time, pos2(x_of(it, body, timeline_time(it, k.time)), y_of(body, l, k.value.as_f64().unwrap_or(0.0))))).collect()
}

/// Draw the line over a clip drawn at `body`: white with a dark shadow.
pub fn paint(p: &egui::Painter, body: Rect, it: &TrackItem, l: &Lane) {
    let vis = p.clip_rect().intersect(body);
    if band(body).height() < 6.0 || vis.width() <= 0.0 {
        return;
    }
    let p = p.with_clip_rect(vis);
    let pts = line(it, l, body, vis.min.x, vis.max.x);
    p.add(egui::Shape::line(pts.iter().map(|q| *q + vec2(0.0, 1.0)).collect(), Stroke::new(1.0, Color32::BLACK)));
    p.add(egui::Shape::line(pts, Stroke::new(1.0, Color32::WHITE)));
    for (_, c) in keys(it, l, body).into_iter().filter(|(_, c)| vis.expand(6.0).contains(*c)) {
        icons::paint(&p, Rect::from_center_size(c, vec2(9.0, 9.0)), Icon::Keyframe, KEY_COL);
    }
}

/// A drag of a clip's line, pressed at height `y` where the line was at `from`. `keys` are the
/// keyframes on either side of that point, which move with it (none: the value is not animated).
/// `last` is the value sent last: the first change starts the drag's undo step.
#[derive(Clone, Debug)]
pub struct LineDrag {
    clip: ClipId,
    lane: Lane,
    from: f64,
    y: f32,
    keys: [Option<(Tick, f64)>; 2],
    last: Option<f64>,
}

impl LineDrag {
    /// Whether the drag has changed the value yet (its first change began an undo step).
    pub fn changed(&self) -> bool {
        self.last.is_some()
    }
}

fn find<'a>(seq: &'a Sequence, layout: &'a Layout, clip: ClipId) -> Option<(&'a Row, &'a TrackItem, Lane)> {
    let (tid, it) = seq.find_item(clip)?;
    let r = layout.rows.iter().find(|r| r.track == tid)?;
    let l = lane(layout, r, it)?;
    Some((r, it, l))
}

fn body(layout: &Layout, r: &Row, it: &TrackItem) -> Rect {
    Rect::from_min_max(pos2(layout.x_of(it.start), r.rect.min.y + 1.0), pos2(layout.x_of(it.end()), r.rect.max.y - 1.0))
}

/// Whether `pos` is on `clip`'s line, within 4 px.
pub fn on_line(seq: &Sequence, layout: &Layout, clip: ClipId, pos: Pos2) -> bool {
    let Some((r, it, l)) = find(seq, layout, clip) else { return false };
    if seq.track(r.track).is_none_or(|t| t.locked) {
        return false;
    }
    let body = body(layout, r, it);
    band(body).height() >= 6.0 && (pos.y - y_of(body, &l, value_at(it, &l, t_at(it, body, pos.x)))).abs() <= 4.0
}

/// A press on `clip`'s line at `at` that became a drag.
pub fn start(seq: &Sequence, layout: &Layout, clip: ClipId, at: Pos2) -> Option<LineDrag> {
    let (r, it, l) = find(seq, layout, clip)?;
    let t = t_at(it, body(layout, r, it), at.x);
    let m = it.source_time_at(t);
    let keys = param(it, &l).filter(|p| p.is_animated()).map_or([None, None], |p| {
        let i = p.keyframes.partition_point(|k| k.time <= m);
        let kv = |j: Option<usize>| j.and_then(|j| p.keyframes.get(j)).map(|k| (k.time, k.value.as_f64().unwrap_or(0.0)));
        [kv(i.checked_sub(1)), kv(Some(i))]
    });
    let from = value_at(it, &l, t);
    Some(LineDrag { clip, lane: l, from, y: at.y, keys, last: None })
}

/// Follow the pointer, now at height `y`. The clip changes as the line moves, so it is seen and heard.
pub fn drag(app: &mut FilmcraftApp, ui: &egui::Ui, seq: &Sequence, layout: &Layout, mut d: LineDrag, y: f32) -> LineDrag {
    let Some((r, it, _)) = find(seq, layout, d.clip) else { return d };
    let sc = d.lane.scale;
    let v = sc.value(sc.pos(d.from) + (d.y - y) / band(body(layout, r, it)).height().max(1.0));
    tip(ui, &sc, v);
    if d.last == Some(v) {
        return d;
    }
    let begin = d.last.is_none();
    let (effect, pid) = (d.lane.index, d.lane.param.clone());
    let mut acts = Vec::new();
    if d.keys.iter().all(Option::is_none) {
        acts.push(("effects.setParam", json!({"clip": d.clip.0, "effect": effect, "param": pid, "value": v, "merge": true, "begin": begin})));
    }
    for (n, (m, kv)) in d.keys.iter().flatten().enumerate() {
        let kv = sc.clamp(kv + v - d.from);
        let p = json!({"clip": d.clip.0, "effect": effect, "param": pid, "mediaTime": m.0, "value": kv, "merge": true, "begin": begin && n == 0});
        acts.push(("effects.setKeyframe", p));
    }
    run(app, acts);
    d.last = Some(v);
    d
}

/// The Pen tool or a Cmd/Ctrl-click on `clip`'s line at `x`: a keyframe there.
pub fn add_keyframe(app: &mut FilmcraftApp, seq: &Sequence, layout: &Layout, clip: ClipId, x: f32) {
    let Some((r, it, l)) = find(seq, layout, clip) else { return };
    let t = seq.settings.frame_rate.snap_nearest(t_at(it, body(layout, r, it), x));
    run(app, vec![("effects.addKeyframe", json!({"clip": clip.0, "effect": l.index, "param": l.param, "time": t.0}))]);
}

fn run(app: &mut FilmcraftApp, acts: Vec<(&str, Value)>) {
    for (id, p) in acts {
        if let Err(e) = app.session.execute(id, p) {
            app.ui.status = e.to_string();
        }
    }
}

/// The value next to the pointer while dragging: one line, always as wide, so it does not jump.
fn tip(ui: &egui::Ui, sc: &Scale, v: f64) {
    let text = sc.text(v);
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

/// The keyframes on the lines of the clips in view: drag one in time and value, right-click it to
/// delete it. Runs after the timeline's own interaction, so they are above the clip.
/// `vclip` and `aclip` are the visible areas of the video and audio tracks.
pub fn interact(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, layout: &Layout, vclip: Rect, aclip: Rect) {
    let held: Option<KeyDrag> = ui.data(|d| d.get_temp(key_drag_id()));
    if held.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        if let Some(h) = held
            && h.begun
        {
            let _ = app.session.execute("edit.undo", json!({}));
        }
        ui.data_mut(|d| d.remove::<KeyDrag>(key_drag_id()));
        return;
    }
    if held.is_none() && (app.tl.drag.is_some() || !matches!(app.ui.tool, Tool::Selection | Tool::Pen)) {
        return;
    }
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let rate = seq.settings.frame_rate;
    let mut next = held;
    let mut acts: Vec<(&str, Value)> = Vec::new();
    for r in layout.rows.iter().filter(|r| !r.lane) {
        let Some(tr) = seq.track(r.track).filter(|t| !t.locked) else { continue };
        let area = if r.kind == TrackKind::Video { vclip } else { aclip }.intersect(layout.content);
        let row = r.rect.intersect(area);
        if row.height() <= 0.0 {
            continue;
        }
        for it in &tr.items {
            let Some(l) = lane(layout, r, it) else { continue };
            let body = body(layout, r, it);
            let vis = body.intersect(row);
            if vis.width() <= 0.0 || band(body).height() < 6.0 {
                continue;
            }
            let (name, what) = if l.volume { ("volume", "Volume") } else { ("keyframes", "Clip keyframes") };
            // a point on the line for agents, away from the trim zones at the edges
            let edge = EDGE_PX.min(body.width() / 3.0);
            let (x0, x1) = (vis.min.x.max(body.min.x + edge), vis.max.x.min(body.max.x - edge));
            if x1 > x0 {
                let xm = (x0 + x1) / 2.0;
                let c = pos2(xm, y_of(body, &l, value_at(it, &l, t_at(it, body, xm))));
                app.auto.add(&format!("timeline.clip.{}.{name}", it.id.0), Rect::from_center_size(c, vec2(12.0, 8.0)), what);
            }
            let ks = keys(it, &l, body);
            for (i, &(time, c)) in ks.iter().enumerate() {
                if !vis.expand2(vec2(5.0, 0.0)).contains(c) {
                    continue;
                }
                let kr = Rect::from_center_size(c, vec2(11.0, 11.0));
                let resp = ui.interact(kr, egui::Id::new(("tl-volume-kf", it.id.0, i)), Sense::click_and_drag());
                app.auto.add(&format!("timeline.clip.{}.{name}.kf.{i}", it.id.0), kr, &format!("{what} keyframe"));
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
                    let v = l.scale.value((band(body).max.y - q.y) / band(body).height());
                    if resp.drag_delta() != egui::Vec2::ZERO {
                        let to = it.moving_source_time_at(t);
                        let p = json!({"clip": h.clip.0, "effect": l.index, "param": l.param, "mediaTime": time.0, "to": to.0, "value": v, "merge": true, "begin": !h.begun});
                        acts.push(("effects.moveKeyframe", p));
                        next = Some(KeyDrag { begun: true, ..h });
                    }
                    tip(ui, &l.scale, v);
                }
                resp.context_menu(|ui| {
                    let b = ui.button(tl!("Delete"));
                    app.auto.add(&format!("timeline.clip.{}.{name}.kf.{i}.delete", it.id.0), b.rect, "Delete");
                    if b.clicked() {
                        acts.push(("effects.deleteKeyframe", json!({"clip": it.id.0, "effect": l.index, "param": l.param, "mediaTime": time.0})));
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

/// The clip menu's "Show Clip Keyframes" submenu for the clips in `picked`: the parameters of the
/// first one's effects, effect by effect (video clips can also show none). The pick applies to
/// every selected clip that has that parameter.
pub fn menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, seq: &Sequence, picked: &[&TrackItem]) {
    let Some(first) = picked.first().copied() else {
        ui.add_enabled(false, egui::Button::new(tl!("Show Clip Keyframes")));
        return;
    };
    let on_video = |it: &TrackItem| seq.find_item(it.id).and_then(|(t, _)| seq.track(t)).is_none_or(|t| t.kind == TrackKind::Video);
    let kind = if on_video(first) { TrackKind::Video } else { TrackKind::Audio };
    // None is offered when any picked clip is a video clip, not only when the first one is: a
    // right-click on the audio half of a linked selection lists the audio clip first
    let offers_none = picked.iter().any(|it| on_video(it));
    let shown = lane_for(app.ui.timeline.clip_lanes.get(&first.id.0).map(String::as_str), kind, first).map(|l| key_of(first, l.index, &l.param));
    let mut pick: Option<String> = None;
    let r = ui.menu_button(tl!("Show Clip Keyframes"), |ui| {
        if offers_none {
            let label = tl!("None");
            let text = if shown.is_none() { format!("✓ {label}") } else { label.to_string() };
            let b = ui.button(text);
            app.auto.add("timeline.clipMenu.showKeyframes.none", b.rect, label);
            if b.clicked() {
                pick = Some(NONE.to_string());
            }
        }
        let all = choices(first);
        let prefix = |k: &str| k.split('/').next().unwrap_or_default().to_string();
        let mut i = 0;
        while let Some((k0, effect, _)) = all.get(i) {
            let fx = prefix(k0);
            let group: Vec<&(String, &str, &str)> = all[i..].iter().take_while(|c| prefix(&c.0) == fx).collect();
            i += group.len();
            let ename = crate::i18n::t(effect);
            let er = ui.menu_button(ename, |ui| {
                for (key, _, label) in &group {
                    let label = crate::i18n::t(label);
                    let text = if shown.as_deref() == Some(key.as_str()) { format!("✓ {label}") } else { label.to_string() };
                    let b = ui.button(text);
                    app.auto.add(&format!("timeline.clipMenu.showKeyframes.{key}"), b.rect, label);
                    if b.clicked() {
                        pick = Some(key.clone());
                    }
                }
            });
            app.auto.add(&format!("timeline.clipMenu.showKeyframes.{fx}"), er.response.rect, ename);
        }
    });
    app.auto.add("timeline.clipMenu.clip.showKeyframes", r.response.rect, "Show Clip Keyframes");
    if let Some(key) = pick {
        for it in picked {
            // None is offered for video clips only, since an audio clip always shows its Volume
            // line, so only they take it. Linked Selection (on by default) puts a video clip's
            // linked audio clip in `picked` as well, and writing None there would take away a
            // line that the audio clip's own menu never offers to bring back.
            let applies = if key == NONE { on_video(it) } else { index_of(it, &key).is_some() };
            if applies {
                app.ui.timeline.clip_lanes.insert(it.id.0, key.clone());
            }
        }
        ui.close();
    }
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
            let l = lane_for(None, TrackKind::Audio, &it).unwrap();
            assert_eq!(keys(&it, &l, body).len(), 2);
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
        let l = lane_for(None, TrackKind::Audio, &it).unwrap();
        let flat = line(&it, &l, body, 0.0, 400.0);
        assert_eq!(flat.len(), 2);
        assert!(flat.iter().all(|p| (p.y - y_of(body, &l, 0.0)).abs() < 1e-3));
        let p = it.effect_mut("volume").and_then(|e| e.param_mut("level")).unwrap();
        p.put_keyframe(Tick(500), filmcraft_project::ParamValue::Float(0.0));
        p.put_keyframe(Tick(4_500), filmcraft_project::ParamValue::Float(-20.0));
        let ramp = line(&it, &l, body, 0.0, 400.0);
        assert!(ramp.len() > 100);
        assert!(ramp.windows(2).all(|w| w[1].y >= w[0].y), "goes down as the level falls");
        assert!(ramp.last().unwrap().y > y_of(body, &l, -19.0));
        // damaged geometry draws nothing silly rather than looping or panicking
        assert_eq!(line(&it, &l, body, 0.0, f32::NAN).len(), 2);
        assert!(line(&it, &l, body, 0.0, f32::INFINITY).len() <= 4097);
    }

    fn video(effects: Vec<filmcraft_project::EffectInstance>) -> TrackItem {
        TrackItem { effects, ..clip(1.0, false) }
    }

    #[test]
    fn audio_clips_show_volume_and_video_clips_what_was_picked() {
        let a = clip(1.0, false);
        let l = lane_for(None, TrackKind::Audio, &a).unwrap();
        assert!(l.volume);
        assert_eq!((l.index, l.param.as_str()), (0, "level"));
        assert!(lane_for(Some(NONE), TrackKind::Audio, &a).is_none());
        let pan = lane_for(Some("panner#0/balance"), TrackKind::Audio, &a).unwrap();
        assert_eq!((pan.index, pan.param.as_str(), pan.volume), (2, "balance", false));

        let mut fx = filmcraft_project::effect::intrinsic_video();
        fx.push(filmcraft_project::find_effect("gaussian_blur").unwrap().instance());
        fx.push(filmcraft_project::find_effect("gaussian_blur").unwrap().instance());
        let v = video(fx);
        assert!(lane_for(None, TrackKind::Video, &v).is_none(), "video clips show nothing until a parameter is picked");
        let o = lane_for(Some("opacity#0/opacity"), TrackKind::Video, &v).unwrap();
        assert_eq!((o.index, o.param.as_str()), (1, "opacity"));
        // the second Gaussian Blur, not the first
        let keys: Vec<String> = choices(&v).into_iter().map(|c| c.0).collect();
        let second = keys.iter().find(|k| k.starts_with("gaussian_blur#1/")).unwrap();
        assert_eq!(lane_for(Some(second), TrackKind::Video, &v).unwrap().index, 4);
        // a picked effect that is gone falls back to the clip's usual line
        assert!(lane_for(Some("gaussian_blur#2/blurriness"), TrackKind::Video, &v).is_none());
        assert!(lane_for(Some("motion#0/position"), TrackKind::Video, &v).is_none(), "points are not lines");
        for k in ["", "/", "#", "opacity#x/opacity", "opacity#99999999999999999999/opacity", "nope"] {
            assert!(lane_for(Some(k), TrackKind::Video, &v).is_none(), "{k}");
        }
        // only numbers and angles that can be keyframed are offered
        assert!(keys.contains(&"motion#0/scale".to_string()) && keys.contains(&"motion#0/rotation".to_string()));
        assert!(keys.contains(&"time_remap#0/speed".to_string()));
        assert!(!keys.iter().any(|k| k.ends_with("/position") || k.ends_with("/uniform_scale") || k.ends_with("/blend")));
    }

    #[test]
    fn values_map_to_the_band_and_back_within_the_limits() {
        let v = video(filmcraft_project::effect::intrinsic_video());
        let l = lane_for(Some("opacity#0/opacity"), TrackKind::Video, &v).unwrap();
        let body = Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 80.0));
        let b = band(body);
        assert!((y_of(body, &l, 100.0) - b.min.y).abs() < 1e-3);
        assert!((y_of(body, &l, 0.0) - b.max.y).abs() < 1e-3);
        assert!((l.scale.value(0.5) - 50.0).abs() < 1e-9);
        assert_eq!(l.scale.value(7.0), 100.0);
        assert_eq!(l.scale.clamp(f64::NAN), 0.0);
        assert_eq!(l.scale.text(42.0), "42.0%");
        let r = lane_for(Some("motion#0/rotation"), TrackKind::Video, &v).unwrap();
        assert!(r.scale.clamp(1e300) <= 36_000.0);
        assert_eq!(r.scale.pos(f64::INFINITY), 0.0);
    }
}
