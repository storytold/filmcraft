//! [`working_set::frame_working_set`]: the estimate follows the decimation the renderer picks, so a
//! 4K clip scaled just over half size on a 1080-wide output costs a full 4K float image (#1443).

use super::*;
use filmcraft_media::{MediaInfo, MediaKind, VideoStreamInfo};
use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

use crate::working_set::frame_working_set;

const FPS: FrameRate = FrameRate::FPS_60;

fn info(w: u32, h: u32) -> MediaInfo {
    MediaInfo {
        name: "uhd".into(),
        kind: MediaKind::Movie,
        duration: Tick(10 * TICKS_PER_SECOND),
        video: Some(VideoStreamInfo {
            width: w,
            height: h,
            frame_rate: FPS,
            par: (1, 1),
            codec: "test".into(),
            pixel_format: "YUV 4:2:0 10-bit".into(),
            color: filmcraft_color::ColorInfo::SRGB_FULL,
            has_alpha: false,
            bitrate: None,
            hdr: None,
            color_assumed: None,
        }),
        audio_streams: Vec::new(),
        container: "test".into(),
        start_timecode: None,
        file_size: None,
    }
}

/// A `src`-sized clip at Motion `scale` % on V1 of a 1080 x 1920 sequence.
fn project(src: (u32, u32), scale: f64) -> (Project, ItemId) {
    let mut p = Project::new("ws");
    let clip = MediaClip {
        media: MediaRef::File { path: "uhd.mov".into() },
        info: info(src.0, src.1),
        interpret: Default::default(),
        mark_in: None,
        mark_out: None,
        markers: vec![],
        offline: false,
        proxy: None,
        identity: None,
    };
    let item = p.add_item("uhd", Label::Iris, ItemKind::Media(Box::new(clip)), None);
    let settings = SequenceSettings { width: 1080, height: 1920, frame_rate: FPS, ..Default::default() };
    let sid = p.new_sequence("s", settings, 1, 0, None);
    let mut ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FPS.tick_of(60)), FPS).unwrap();
    ti.effect_mut("motion").unwrap().param_mut("scale").unwrap().value = ParamValue::Float(scale);
    p.sequence_mut(sid).unwrap().video_tracks[0].items.push(ti);
    (p, sid)
}

/// One float RGBA image of `w` x `h`.
fn img(w: u64, h: u64) -> u64 {
    w * h * 16
}

#[test]
fn a_4k_clip_over_half_size_costs_full_resolution_float_images() {
    let at = |scale, effect: bool| {
        let (mut p, s) = project((3840, 2160), scale);
        if effect {
            let blur = filmcraft_project::effect::find_effect("gaussian_blur").unwrap().instance();
            p.sequence_mut(s).unwrap().video_tracks[0].items[0].effects.push(blur);
        }
        frame_working_set(&p, s, Tick::ZERO, RenderOptions::default())
    };
    let canvas = img(1080, 1920);
    let decoded = 3840 * 2160 * 4;
    // 50 %: decimated by two to 1920 x 1080, then placed on the output
    assert_eq!(at(50.0, false), canvas + decoded + img(1920, 1080) + canvas);
    // an effect works on a copy of the layer
    assert_eq!(at(50.0, true), canvas + decoded + 2 * img(1920, 1080) + canvas);
    // 55 %: 2112 px wanted, more than half of 3840, so the renderer keeps all of it
    assert_eq!(at(55.0, true), canvas + decoded + 2 * img(3840, 2160) + canvas);
    assert!(at(55.0, true) > 2 * at(50.0, true));
    // at a quarter the decimation is four
    assert_eq!(at(25.0, false), canvas + decoded + img(960, 540) + canvas);
}

#[test]
fn a_clip_that_is_the_output_as_it_is_needs_no_placed_copy() {
    // a 1080 x 1920 clip at 100 % on a 1080 x 1920 sequence: decoded and converted, nothing else
    let (p, s) = project((1080, 1920), 100.0);
    assert_eq!(frame_working_set(&p, s, Tick::ZERO, RenderOptions::default()), img(1080, 1920) + 1080 * 1920 * 4 + img(1080, 1920));
}

#[test]
fn the_output_scale_shrinks_the_estimate_like_the_render() {
    let (p, s) = project((3840, 2160), 55.0);
    let half = RenderOptions { scale: 0.5, ..Default::default() };
    // a half-size output wants 1056 px of the 3840: decimation two
    assert_eq!(frame_working_set(&p, s, Tick::ZERO, half), img(540, 960) + 3840 * 2160 * 4 + img(1920, 1080) + img(540, 960));
}

#[test]
fn empty_time_disabled_tracks_and_missing_sequences_cost_only_the_canvas_or_nothing() {
    let (mut p, s) = project((3840, 2160), 100.0);
    let canvas = img(1080, 1920);
    // past the clip: the canvas alone
    assert_eq!(frame_working_set(&p, s, FPS.tick_of(120), RenderOptions::default()), canvas);
    p.sequence_mut(s).unwrap().video_tracks[0].enabled = false;
    assert_eq!(frame_working_set(&p, s, Tick::ZERO, RenderOptions::default()), canvas);
    assert_eq!(frame_working_set(&p, ItemId(9999), Tick::ZERO, RenderOptions::default()), 0);
}

#[test]
fn hostile_sizes_saturate_instead_of_overflowing() {
    let (p, s) = project((u32::MAX, u32::MAX), 100.0);
    let ws = frame_working_set(&p, s, Tick::ZERO, RenderOptions::default());
    assert!(ws >= img(3840, 2160), "{ws}");
    let bad = RenderOptions { scale: f32::NAN, ..Default::default() };
    let _ = frame_working_set(&p, s, Tick::ZERO, bad);
}
