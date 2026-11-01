//! Sequence Settings ▸ "Scale motion effects proportionally when changing frame size".
//!
//! Motion's Position is in sequence pixels and its Scale is relative to the source, so a new frame
//! size leaves every clip where it was in pixels: smaller and off-centre in a larger frame, cropped
//! in a smaller one. With the option on, the whole picture scales instead, by one factor about the
//! frame centre. On an aspect change (16:9 → 9:16) that factor is the one that fits the old frame
//! inside the new one (`min(W1/W0, H1/H0)`), so nothing is cropped and the old composition sits
//! centred, letterboxed or pillarboxed, rather than filling the new frame.

use filmcraft_project::{ParamValue, Sequence, TrackKind};

/// Motion Scale's upper bound (the parameter's range, in %).
const MAX_SCALE: f64 = 10_000.0;

/// The uniform factor `k` and the old and new frame centres for `from` → `to`, or `None` for an
/// empty frame on either side.
fn fit(from: (u32, u32), to: (u32, u32)) -> Option<(f64, (f64, f64), (f64, f64))> {
    if from.0 == 0 || from.1 == 0 || to.0 == 0 || to.1 == 0 {
        return None;
    }
    let (w0, h0, w1, h1) = (f64::from(from.0), f64::from(from.1), f64::from(to.0), f64::from(to.1));
    let k = (w1 / w0).min(h1 / h0);
    (k.is_finite() && k > 0.0).then_some((k, (w0 / 2.0, h0 / 2.0), (w1 / 2.0, h1 / 2.0)))
}

/// Rescale the Motion of every video clip in `seq` for a frame-size change from `from` to `to`:
/// static values and every keyframe. Returns how many clips were rescaled (0 when the size does not
/// change).
///
/// - Position maps through `p' = c1 + k·(p − c0)`. An automatic (NaN) position stays automatic: it
///   follows the frame centre on its own.
/// - Scale and Scale Width are multiplied by `k`, up to the parameter's 10 000 %. A clip with Scale to
///   Frame Size keeps its Scale, because the renderer refits it to the new frame anyway.
/// - Anchor Point (source pixels) and Rotation do not depend on the frame size and stay as they are.
/// - Only the intrinsic Motion effect changes, as the option's name says: the points of standard
///   effects (a Mirror centre, the Transform effect's Position) keep their pixel values.
pub fn scale_motion(seq: &mut Sequence, from: (u32, u32), to: (u32, u32)) -> usize {
    if from == to {
        return 0;
    }
    let Some((k, c0, c1)) = fit(from, to) else { return 0 };
    let mut n = 0;
    for track in seq.all_tracks_mut().filter(|t| t.kind == TrackKind::Video) {
        for item in &mut track.items {
            let refits = item.scale_to_frame;
            let Some(motion) = item.effect_mut("motion") else { continue };
            if let Some(p) = motion.param_mut("position") {
                p.map_values(|v| match v {
                    ParamValue::Vec2(mut v) => {
                        v.x = c1.0 + k * (v.x - c0.0);
                        v.y = c1.1 + k * (v.y - c0.1);
                        ParamValue::Vec2(v)
                    }
                    other => other,
                });
            }
            if !refits {
                for id in ["scale", "scale_width"] {
                    if let Some(p) = motion.param_mut(id) {
                        p.map_values(|v| match v {
                            ParamValue::Float(s) => ParamValue::Float((s * k).clamp(0.0, MAX_SCALE)),
                            other => other,
                        });
                    }
                }
            }
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{ClipId, ItemId, Keyframe, Label, Project, SequenceSettings, TrackItem, find_effect};
    use filmcraft_time::Tick;

    fn seq_with(items: Vec<TrackItem>) -> Sequence {
        let mut p = Project::new("t");
        let s = p.new_sequence("s", SequenceSettings::default(), 2, 1, None);
        let mut seq = p.sequence(s).unwrap().clone();
        seq.video_tracks[0].items = items;
        seq
    }

    fn clip(id: u64, pos: (f64, f64), scale: f64) -> TrackItem {
        let mut motion = find_effect("motion").unwrap().instance();
        motion.param_mut("position").unwrap().value = ParamValue::Vec2(filmcraft_geom::Vec2::new(pos.0, pos.1));
        motion.param_mut("scale").unwrap().value = ParamValue::Float(scale);
        TrackItem {
            id: ClipId(id),
            item: ItemId(1),
            name: format!("c{id}"),
            label: Label::Iris,
            start: Tick(id as i64 * 1_000_000_000_000),
            duration: Tick(1_000_000_000_000),
            source_in: Tick::ZERO,
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: vec![motion],
            markers: vec![],
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

    fn pos(seq: &Sequence, i: usize) -> (f64, f64) {
        match seq.video_tracks[0].items[i].effect("motion").unwrap().param("position").unwrap().value {
            ParamValue::Vec2(v) => (v.x, v.y),
            _ => panic!("position is a point"),
        }
    }

    fn scale(seq: &Sequence, i: usize) -> f64 {
        match seq.video_tracks[0].items[i].effect("motion").unwrap().param("scale").unwrap().value {
            ParamValue::Float(s) => s,
            _ => panic!("scale is a number"),
        }
    }

    fn close(a: (f64, f64), b: (f64, f64)) -> bool {
        (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9
    }

    #[test]
    fn same_aspect_scales_position_and_scale_together() {
        let mut seq = seq_with(vec![clip(1, (960.0, 540.0), 100.0), clip(2, (300.0, 90.0), 50.0)]);
        assert_eq!(scale_motion(&mut seq, (1920, 1080), (1280, 720)), 2);
        assert!(close(pos(&seq, 0), (640.0, 360.0)), "{:?}", pos(&seq, 0));
        assert!(close(pos(&seq, 1), (200.0, 60.0)), "{:?}", pos(&seq, 1));
        assert!((scale(&seq, 0) - 100.0 * 2.0 / 3.0).abs() < 1e-9);
        assert!((scale(&seq, 1) - 50.0 * 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn aspect_change_fits_the_old_frame_inside_the_new_one() {
        // 1920x1080 -> 1080x1920: k = min(1080/1920, 1920/1080) = 0.5625, centred
        let mut seq = seq_with(vec![clip(1, (960.0, 540.0), 100.0), clip(2, (0.0, 0.0), 100.0)]);
        scale_motion(&mut seq, (1920, 1080), (1080, 1920));
        assert!(close(pos(&seq, 0), (540.0, 960.0)), "{:?}", pos(&seq, 0));
        // the old top-left corner lands on the new frame's left edge, letterboxed vertically
        assert!(close(pos(&seq, 1), (0.0, 960.0 - 0.5625 * 540.0)), "{:?}", pos(&seq, 1));
        assert!((scale(&seq, 0) - 56.25).abs() < 1e-9);
    }

    #[test]
    fn keyframes_scale_too() {
        let mut c = clip(1, (960.0, 540.0), 100.0);
        let m = c.effect_mut("motion").unwrap();
        m.param_mut("position").unwrap().keyframes = vec![
            Keyframe::new(Tick::ZERO, ParamValue::Vec2(filmcraft_geom::Vec2::new(0.0, 0.0))),
            Keyframe::new(Tick(10), ParamValue::Vec2(filmcraft_geom::Vec2::new(1920.0, 1080.0))),
        ];
        m.param_mut("scale").unwrap().keyframes = vec![Keyframe::new(Tick::ZERO, ParamValue::Float(100.0)), Keyframe::new(Tick(10), ParamValue::Float(200.0))];
        let mut seq = seq_with(vec![c]);
        scale_motion(&mut seq, (1920, 1080), (3840, 2160));
        let m = seq.video_tracks[0].items[0].effect("motion").unwrap();
        let kp: Vec<_> = m.param("position").unwrap().keyframes.iter().map(|k| k.value.clone()).collect();
        assert_eq!(kp, vec![ParamValue::Vec2(filmcraft_geom::Vec2::new(0.0, 0.0)), ParamValue::Vec2(filmcraft_geom::Vec2::new(3840.0, 2160.0))]);
        let ks: Vec<_> = m.param("scale").unwrap().keyframes.iter().map(|k| k.value.clone()).collect();
        assert_eq!(ks, vec![ParamValue::Float(200.0), ParamValue::Float(400.0)]);
    }

    #[test]
    fn automatic_position_stays_automatic() {
        let mut seq = seq_with(vec![clip(1, (f64::NAN, f64::NAN), 100.0)]);
        scale_motion(&mut seq, (1920, 1080), (1280, 720));
        let (x, y) = pos(&seq, 0);
        assert!(x.is_nan() && y.is_nan());
        assert!((scale(&seq, 0) - 100.0 * 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn scale_to_frame_clips_keep_their_scale_but_move() {
        let mut c = clip(1, (480.0, 270.0), 100.0);
        c.scale_to_frame = true;
        let mut seq = seq_with(vec![c]);
        scale_motion(&mut seq, (1920, 1080), (3840, 2160));
        assert!(close(pos(&seq, 0), (960.0, 540.0)), "{:?}", pos(&seq, 0));
        assert_eq!(scale(&seq, 0), 100.0);
    }

    #[test]
    fn scale_is_capped_at_the_parameter_range() {
        let mut seq = seq_with(vec![clip(1, (1.0, 1.0), 5000.0)]);
        scale_motion(&mut seq, (2, 2), (8, 8));
        assert_eq!(scale(&seq, 0), MAX_SCALE);
    }

    #[test]
    fn same_size_or_empty_frame_changes_nothing() {
        let mut seq = seq_with(vec![clip(1, (100.0, 100.0), 100.0)]);
        let before = seq.clone();
        assert_eq!(scale_motion(&mut seq, (1920, 1080), (1920, 1080)), 0);
        assert_eq!(scale_motion(&mut seq, (0, 1080), (1280, 720)), 0);
        assert_eq!(scale_motion(&mut seq, (1920, 1080), (1280, 0)), 0);
        // Debug, not ==: the automatic anchor is NaN, which never equals itself
        assert_eq!(format!("{:?}", seq.video_tracks[0].items[0].effects), format!("{:?}", before.video_tracks[0].items[0].effects));
    }
}
