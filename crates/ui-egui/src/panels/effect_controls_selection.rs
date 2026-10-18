//! Keyframe selection gestures. The project changes only on a completed key drag; a marquee,
//! modifier click, or cancelled preview only changes the UI selection.
use egui::{Color32, Id, Pos2, Rect, Response, Sense, Stroke, vec2};
use filmcraft_project::{Interpolation, TrackItem};
use filmcraft_time::{FrameRate, Tick};
use serde_json::json;

use crate::{
    FilmcraftApp,
    icons::{self, Icon},
    state::KeyframeRef,
};

fn state_id() -> Id {
    Id::new("effect-controls-keyframe-selection")
}

#[derive(Clone)]
struct Point {
    key: KeyframeRef,
    rect: Rect,
    time: Tick,
    pixels_per_tick: f64,
    interp: Interpolation,
}

#[derive(Clone)]
struct Group {
    anchor: KeyframeRef,
    keys: Vec<KeyframeRef>,
    press: Pos2,
    time: Tick,
    ticks_per_pixel: f64,
    low: i64,
    high: i64,
    delta: Tick,
    snap: Option<(Tick, f32)>,
    ended: bool,
}

#[derive(Clone)]
enum Gesture {
    Marquee { origin: Pos2, end: Pos2, base: Vec<KeyframeRef> },
    Group(Group),
}

#[derive(Clone, Default)]
struct State {
    clip: u64,
    points: Vec<Point>,
    gesture: Option<Gesture>,
}

pub(crate) struct Interaction {
    state: State,
    points: Vec<Point>,
    area: Rect,
    background: Response,
    cancelled: bool,
}

fn limits(app: &FilmcraftApp, keys: &[KeyframeRef]) -> Option<(i64, i64)> {
    let q = app.session.active_sequence()?;
    let frame = q.settings.frame_rate.frame_duration();
    let (mut low, mut high) = (i64::MIN, i64::MAX);
    for key in keys {
        let (_, it) = q.find_item(filmcraft_project::ClipId(key.clip))?;
        let speed = it.speed.abs() * if it.reverse { -1.0 } else { 1.0 };
        if !speed.is_finite() || speed == 0.0 || it.duration < frame {
            return None;
        }
        let da = (it.moving_source_time_at(it.start) - key.time).0 as f64 / speed;
        let db = (it.moving_source_time_at(it.end() - frame) - key.time).0 as f64 / speed;
        low = low.max(da.min(db).ceil() as i64);
        high = high.min(da.max(db).floor() as i64);
    }
    (low <= high).then_some((low, high))
}

fn update(group: &mut Group, points: &[Point], pos: Pos2, shift: bool, playhead: Tick, rate: FrameRate) {
    let offset = f64::from(pos.x - group.press.x) * group.ticks_per_pixel;
    if !offset.is_finite() {
        return;
    }
    let requested = rate.snap_nearest(group.time + Tick(offset.round() as i64)) - group.time;
    group.delta = Tick(requested.0.clamp(group.low, group.high));
    group.snap = None;
    if !shift {
        return;
    }
    let mut closest = 8.0;
    for point in points.iter().filter(|p| group.keys.contains(&p.key)) {
        for target in std::iter::once(playhead).chain(points.iter().filter(|p| !group.keys.contains(&p.key)).map(|p| p.time)) {
            let distance = ((target - point.time - requested).0 as f64 * point.pixels_per_tick).abs();
            let delta = (target - point.time).0;
            if distance <= closest && (group.low..=group.high).contains(&delta) {
                closest = distance;
                group.delta = Tick(delta);
                group.snap = Some((target, point.rect.center().x + (delta as f64 * point.pixels_per_tick) as f32));
            }
        }
    }
}

impl Interaction {
    pub(crate) fn begin(app: &mut FilmcraftApp, ui: &mut egui::Ui, area: Rect, clip: u64) -> Self {
        let mut state = ui.data(|d| d.get_temp::<State>(state_id())).unwrap_or_default();
        if state.clip != clip {
            state = State { clip, ..Default::default() };
        }
        let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if cancelled {
            state.gesture = None;
        }
        let background = ui.interact(area, Id::new(("ec-select", clip)), Sense::click_and_drag());
        if background.clicked_by(egui::PointerButton::Primary) && !ui.input(|i| i.modifiers.shift) {
            app.ui.keyframe_selection.clear();
        }
        if !cancelled
            && background.drag_started_by(egui::PointerButton::Primary)
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            let base = if ui.input(|i| i.modifiers.shift) { app.ui.keyframe_selection.clone() } else { Vec::new() };
            state.gesture = Some(Gesture::Marquee { origin, end: origin, base });
        }
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            match &mut state.gesture {
                Some(Gesture::Marquee { end, .. }) => *end = pos,
                Some(Gesture::Group(group)) => {
                    update(group, &state.points, pos, ui.input(|i| i.modifiers.shift), app.session.playhead(), app.session.sequence_rate())
                }
                None => {}
            }
        }
        Self { state, points: Vec::new(), area, background, cancelled }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn key(
        &mut self,
        app: &mut FilmcraftApp,
        ui: &egui::Ui,
        key: KeyframeRef,
        it: &TrackItem,
        lane: Rect,
        rect: Rect,
        response: &Response,
        interp: Interpolation,
    ) -> Rect {
        let shift = ui.input(|i| i.modifiers.shift);
        if response.clicked_by(egui::PointerButton::Primary) {
            if shift {
                if let Some(index) = app.ui.keyframe_selection.iter().position(|k| *k == key) {
                    app.ui.keyframe_selection.remove(index);
                } else {
                    app.ui.keyframe_selection.push(key.clone());
                }
            } else {
                app.ui.keyframe_selection = vec![key.clone()];
            }
        }
        let speed_key = key.param == "speed" && it.effects.get(key.effect).is_some_and(|e| e.effect == "time_remap");
        let time = super::timeline_time_of(it, key.time, speed_key);
        let pixels_per_tick = f64::from(lane.width().max(1.0)) / it.duration.0.max(1) as f64;
        if self.area.contains(rect.center()) && ui.clip_rect().contains(rect.center()) {
            self.points.push(Point { key: key.clone(), rect, time, pixels_per_tick, interp });
        }
        if !self.cancelled && response.drag_started_by(egui::PointerButton::Primary) {
            if !app.ui.keyframe_selection.contains(&key) {
                if !shift {
                    app.ui.keyframe_selection.clear();
                }
                app.ui.keyframe_selection.push(key.clone());
            }
            let keys = app.ui.keyframe_selection.clone();
            if let Some((low, high)) = limits(app, &keys) {
                let press = ui.input(|i| i.pointer.press_origin()).unwrap_or(rect.center());
                let mut group = Group {
                    anchor: key.clone(),
                    keys,
                    press,
                    time,
                    ticks_per_pixel: 1.0 / pixels_per_tick,
                    low,
                    high,
                    delta: Tick::ZERO,
                    snap: None,
                    ended: false,
                };
                if let Some(pos) = response.interact_pointer_pos() {
                    update(&mut group, &self.state.points, pos, shift, app.session.playhead(), app.session.sequence_rate());
                }
                self.state.gesture = Some(Gesture::Group(group));
            }
        }
        if let Some(Gesture::Group(group)) = &mut self.state.gesture {
            if group.anchor == key && response.drag_stopped_by(egui::PointerButton::Primary) {
                group.ended = true;
            }
            if group.keys.contains(&key) {
                return rect.translate(vec2((group.delta.0 as f64 * pixels_per_tick) as f32, 0.0));
            }
        }
        rect
    }

    pub(crate) fn finish(mut self, app: &mut FilmcraftApp, ui: &egui::Ui) {
        if let Some(Gesture::Marquee { origin, end, base }) = &self.state.gesture {
            let rect = Rect::from_two_pos(*origin, *end).intersect(self.area);
            let mut selected = base.clone();
            for point in &self.points {
                if rect.contains(point.rect.center()) && !selected.contains(&point.key) {
                    selected.push(point.key.clone());
                }
            }
            app.ui.keyframe_selection = selected;
            let painter = ui.painter().with_clip_rect(self.area);
            painter.rect_filled(rect, 0.0, app.tokens.accent.linear_multiply(0.15));
            painter.rect_stroke(rect, 0.0, Stroke::new(1.0, app.tokens.accent), egui::StrokeKind::Inside);
            app.auto.add("effectControls.marquee", rect, "keyframe selection");
            for point in &self.points {
                let color = if app.ui.keyframe_selection.contains(&point.key) { app.tokens.hot_text } else { Color32::from_rgb(0xb0, 0xb0, 0xb0) };
                paint(&painter, point.rect, point.interp, color);
            }
            if self.background.drag_stopped_by(egui::PointerButton::Primary) {
                self.state.gesture = None;
            }
        }
        if let Some(Gesture::Group(group)) = &self.state.gesture {
            if let Some((time, x)) = group.snap {
                let rect = Rect::from_min_max(egui::pos2(x - 1.0, self.area.min.y), egui::pos2(x + 1.0, self.area.max.y));
                ui.painter().with_clip_rect(self.area).line_segment([rect.center_top(), rect.center_bottom()], Stroke::new(1.0, app.tokens.accent));
                app.auto.add("effectControls.snapTarget", rect, &time.0.to_string());
            }
            if group.ended {
                if group.delta != Tick::ZERO {
                    match app.session.execute("effects.moveKeyframes", json!({"keyframes":group.keys,"delta":group.delta.0})) {
                        Ok(result) => {
                            if let Ok(keys) = serde_json::from_value(result["keyframes"].clone()) {
                                app.ui.keyframe_selection = keys;
                            }
                        }
                        Err(error) => app.ui.status = error.to_string(),
                    }
                }
                self.state.gesture = None;
            }
        }
        self.state.points = self.points;
        ui.data_mut(|d| d.insert_temp(state_id(), self.state));
    }
}

pub(crate) fn paint(painter: &egui::Painter, rect: Rect, interp: Interpolation, color: Color32) {
    match interp {
        Interpolation::Hold => {
            painter.rect_filled(Rect::from_center_size(rect.center(), vec2(9.0, 9.0)), 0.0, color);
        }
        Interpolation::Linear => icons::paint(painter, rect, Icon::Keyframe, color),
        _ => {
            painter.circle_filled(rect.center(), 5.5, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyframe_timeline_coordinates_follow_trim_speed_and_reverse() {
        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let mut it = session.active_sequence().unwrap().video_tracks[0].items[0].clone();
        it.start += Tick::from_seconds_f64(4.0);
        it.source_in += Tick::from_seconds_f64(3.0);
        for speed in [0.5, 1.0, 1.5, 2.0] {
            for reverse in [false, true] {
                it.speed = speed;
                it.reverse = reverse;
                for time in [it.start, it.start + Tick(it.duration.0 / 4), it.end() - session.sequence_rate().frame_duration()] {
                    let media = it.moving_source_time_at(time);
                    assert!((super::super::timeline_time_of(&it, media, false) - time).abs() <= Tick(2), "speed={speed}, reverse={reverse}");
                }
            }
        }
    }

    #[test]
    fn clip_clamping_does_not_extend_the_screen_space_snap_threshold() {
        let key = KeyframeRef { clip: 1, effect: 0, param: "scale".into(), mask: None, time: Tick::ZERO };
        let other = KeyframeRef { param: "rotation".into(), time: Tick::from_seconds_f64(0.95), ..key.clone() };
        let points = [
            Point {
                key: key.clone(),
                rect: Rect::from_center_size(Pos2::ZERO, vec2(15.0, 15.0)),
                time: Tick::ZERO,
                pixels_per_tick: 100.0 / filmcraft_time::TICKS_PER_SECOND as f64,
                interp: Interpolation::Linear,
            },
            Point {
                key: other,
                rect: Rect::from_center_size(egui::pos2(95.0, 30.0), vec2(15.0, 15.0)),
                time: Tick::from_seconds_f64(0.95),
                pixels_per_tick: 100.0 / filmcraft_time::TICKS_PER_SECOND as f64,
                interp: Interpolation::Linear,
            },
        ];
        let mut group = Group {
            anchor: key.clone(),
            keys: vec![key],
            press: Pos2::ZERO,
            time: Tick::ZERO,
            ticks_per_pixel: filmcraft_time::TICKS_PER_SECOND as f64 / 100.0,
            low: 0,
            high: Tick::from_seconds_f64(1.0).0,
            delta: Tick::ZERO,
            snap: None,
            ended: false,
        };
        update(&mut group, &points, egui::pos2(120.0, 0.0), true, Tick::from_seconds_f64(3.0), FrameRate::from_f64(100.0));
        assert_eq!(group.delta, Tick::from_seconds_f64(1.0));
        assert!(group.snap.is_none(), "the pointer is 25 px from the target, even though the clamped preview is only 5 px away");
    }
}
