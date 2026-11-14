//! Motion path: the route a selected clip's Motion > Position takes from its first keyframe to its
//! last, drawn over the Program monitor (Premiere shows the same line). Keyframes are squares with
//! an arrow on each leg for the direction of travel, and the position at the playhead is a dot.
//!
//! Interpolation eases every component of a position with the same factor, so each leg is a straight
//! line between its two keyframe values whatever the interpolation (it changes the speed along the
//! leg, not the route). The path is therefore just the keyframe values joined up: no sampling.
//!
//! The one exception is a Hold leg: the value stays at the first keyframe and jumps at the second,
//! so nothing travels. It is drawn as a faint thin line with no arrow, to show where the jump goes.
//!
//! Display only: the keyframe squares are registered for automation but cannot be dragged. Edit
//! Position keyframes in Effect Controls.

use crate::FilmcraftApp;
use egui::{Color32, Pos2, Rect, Stroke, StrokeKind, Vec2, pos2, vec2};
use filmcraft_project::Interpolation;

/// Keyframes past this many are ignored, so a hostile project cannot make the overlay allocate.
const MAX_KEYFRAMES: usize = 512;

/// Sequence-pixel point → screen point on the monitor picture `pic`.
fn to_screen(pic: Rect, frame: (u32, u32), x: f64, y: f64) -> Option<Pos2> {
    let (w, h) = (f64::from(frame.0), f64::from(frame.1));
    if w < 1.0 || h < 1.0 || !x.is_finite() || !y.is_finite() {
        return None;
    }
    let p = pos2(pic.min.x + (x / w) as f32 * pic.width(), pic.min.y + (y / h) as f32 * pic.height());
    (p.x.is_finite() && p.y.is_finite()).then_some(p)
}

/// Legs longer than this many screen points are drawn as one solid line: a dotted line makes a
/// shape per few points, so a huge Position value would build millions of them.
const MAX_DOTTED_LEN: f32 = 20_000.0;

/// The shapes of one leg `a`→`b`: dotted, or a single line segment when the leg is too long (or
/// its length is not a number) to dot.
fn leg_shapes(a: Pos2, b: Pos2, colour: Color32, width: f32) -> Vec<egui::Shape> {
    let len = (b - a).length();
    if len.is_nan() || len > MAX_DOTTED_LEN {
        return vec![egui::Shape::line_segment([a, b], Stroke::new(width, colour))];
    }
    egui::Shape::dotted_line(&[a, b], colour, 5.0, width)
}

/// Unit direction of travel along the leg `a`→`b`, or `None` when the leg is too short for an arrow.
/// A reversed clip evaluates its effects backwards in media time, so it travels `b`→`a`.
fn travel_dir(a: Pos2, b: Pos2, reverse: bool) -> Option<Vec2> {
    let d = if reverse { a - b } else { b - a };
    let len = d.length();
    (len.is_finite() && len >= 24.0).then(|| d / len)
}

pub fn monitor_overlay(app: &mut FilmcraftApp, ui: &mut egui::Ui, pic: Rect, frame: (u32, u32)) {
    let Some(seq) = app.session.active_sequence() else { return };
    let ph = app.session.playhead();
    let t = app.tokens;
    let painter = ui.painter().with_clip_rect(pic.expand(8.0));
    let stroke = Stroke::new(1.5, t.accent);
    let mut keys: Vec<(Rect, String)> = Vec::new();
    for clip in &app.session.state.selection {
        let Some((_, it)) = seq.find_item(*clip) else { continue };
        if ph < it.start || ph >= it.end() {
            continue;
        }
        let Some(motion) = it.effect("motion").filter(|m| m.enabled) else { continue };
        let Some(param) = motion.param("position").filter(|p| p.keyframes.len() >= 2) else { continue };
        let points: Vec<(Pos2, i64, Interpolation)> = param
            .keyframes
            .iter()
            .take(MAX_KEYFRAMES)
            .filter_map(|k| {
                let v = k.value.as_vec2()?;
                Some((to_screen(pic, frame, v.x, v.y)?, k.time.0, k.interp))
            })
            .collect();
        for pair in points.windows(2) {
            let (Some((a, _, interp)), Some((b, _, _))) = (pair.first(), pair.get(1)) else { continue };
            if *interp == Interpolation::Hold {
                // nothing travels on a Hold leg: the value jumps at the next keyframe
                painter.extend(leg_shapes(*a, *b, t.accent.gamma_multiply(0.35), 0.8));
                continue;
            }
            painter.extend(leg_shapes(*a, *b, t.accent, 1.2));
            let Some(n) = travel_dir(*a, *b, it.reverse) else { continue };
            // direction arrow at the middle of the leg
            let m = a.lerp(*b, 0.5);
            let side = vec2(-n.y, n.x) * 4.0;
            painter.line_segment([m - n * 6.0 + side, m + n * 3.0], stroke);
            painter.line_segment([m - n * 6.0 - side, m + n * 3.0], stroke);
        }
        for (p, time, _) in &points {
            let r = Rect::from_center_size(*p, vec2(6.0, 6.0));
            painter.rect_filled(r, 0.0, Color32::WHITE);
            painter.rect_stroke(r, 0.0, stroke, StrokeKind::Middle);
            keys.push((r, format!("program.motionPath.{}.keyframe.{time}", clip.0)));
        }
        let now = motion.vec2_at("position", it.effect_time_at(ph));
        if let Some(p) = to_screen(pic, frame, now.x, now.y) {
            painter.circle_filled(p, 3.5, t.accent);
            painter.circle_stroke(p, 3.5, Stroke::new(1.0, Color32::WHITE));
        }
    }
    for (r, id) in keys {
        app.auto.add(&id, r, "motion path keyframe");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sequence_pixels_onto_the_picture() {
        let pic = Rect::from_min_max(pos2(10.0, 20.0), pos2(110.0, 70.0));
        assert_eq!(to_screen(pic, (1920, 1080), 960.0, 540.0), Some(pos2(60.0, 45.0)));
    }

    #[test]
    fn arrows_follow_playback_direction_and_flip_for_reversed_clips() {
        let (a, b) = (pos2(0.0, 0.0), pos2(100.0, 0.0));
        assert_eq!(travel_dir(a, b, false), Some(vec2(1.0, 0.0)));
        assert_eq!(travel_dir(a, b, true), Some(vec2(-1.0, 0.0)));
        assert_eq!(travel_dir(a, pos2(10.0, 0.0), true), None);
    }

    #[test]
    fn hostile_numbers_draw_nothing_instead_of_panicking() {
        let pic = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 50.0));
        assert_eq!(to_screen(pic, (0, 0), 1.0, 1.0), None);
        assert_eq!(to_screen(pic, (1920, 1080), f64::NAN, 1.0), None);
        assert_eq!(to_screen(pic, (1920, 1080), 1.0, f64::INFINITY), None);
    }

    #[test]
    fn a_huge_position_does_not_build_millions_of_shapes() {
        let pic = Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 50.0));
        let b = to_screen(pic, (1920, 1080), 1e9, 1e9).expect("finite");
        assert!(leg_shapes(pos2(0.0, 0.0), b, Color32::WHITE, 1.2).len() <= 1);
        assert_eq!(to_screen(pic, (1920, 1080), 1e300, 0.0), None);
        assert!(leg_shapes(pos2(0.0, 0.0), pos2(f32::INFINITY, 0.0), Color32::WHITE, 1.2).len() <= 1);
    }
}
