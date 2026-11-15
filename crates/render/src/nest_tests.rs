//! The picture of nested sequences: a nest shows its sequence at that sequence's own size and
//! frame rate, through the clip's speed, holds and effects, with the sequence's captions.
//! Premiere's behaviour was observed in Premiere Pro 26.5.2.

use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{Caption, CaptionFormat, CaptionTrack, ClipId, Label, MediaClip, MediaRef, ParamValue, SequenceSettings, TrackId, TrackKind};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

struct Rig {
    p: Project,
    map: SourceMap,
}

impl Rig {
    fn new() -> Rig {
        Rig { p: Project::new("nest"), map: SourceMap::default() }
    }

    fn add(&mut self, g: GeneratorSource) -> ItemId {
        let info = g.info().clone();
        let id = self.p.add_item(
            &info.name.clone(),
            Label::Iris,
            ItemKind::Media(Box::new(MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            })),
            None,
        );
        self.map.0.insert(id, Arc::new(g) as SharedSource);
        id
    }

    fn matte(&mut self, color: [f32; 4], w: u32, h: u32) -> ItemId {
        self.add(GeneratorSource::new(Generator::ColorMatte { color }, w, h, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND)))
    }

    fn seq(&mut self, name: &str, w: u32, h: u32, rate: FrameRate) -> ItemId {
        self.p.new_sequence(name, SequenceSettings { width: w, height: h, frame_rate: rate, ..Default::default() }, 2, 1, None)
    }

    /// `frames` frames of `item` from its start, at the start of V1 of `seq`.
    fn put(&mut self, seq: ItemId, item: ItemId, frames: i64) -> ClipId {
        let rate = self.p.sequence(seq).unwrap().settings.frame_rate;
        let ti = self.p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(frames)), rate).unwrap();
        let id = ti.id;
        self.p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
        id
    }

    fn clip(&mut self, seq: ItemId, clip: ClipId) -> &mut TrackItem {
        self.p.sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1
    }

    fn frame(&self, seq: ItemId, t: Tick) -> Image {
        render_sequence(&self.p, seq, t, RenderOptions::default(), &self.map)
    }

    /// The same frame through the frame plan (what the GPU compositor is given).
    fn planned(&self, seq: ItemId, t: Tick) -> Image {
        plan::execute_cpu(&plan::plan_frame(&self.p, seq, t, RenderOptions::default(), plan::DeviceCaps::ALL, &self.map))
    }
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 0.02)
}

/// The largest channel difference between two images of the same size.
fn worst(a: &Image, b: &Image) -> f32 {
    assert_eq!((a.w, a.h), (b.w, b.h));
    a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn a_smaller_nest_is_shown_at_its_own_size_in_the_middle() {
    // Premiere: a 1280x720 sequence in a 1920x1080 one comes in centred at 100%
    let mut r = Rig::new();
    let red = r.matte(RED, 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    r.put(inner, red, 48);
    let outer = r.seq("outer", 640, 360, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    for img in [r.frame(outer, Tick(1000)), r.planned(outer, Tick(1000))] {
        assert_eq!((img.w, img.h), (640, 360));
        // the nest covers x 160..480, y 90..270
        assert!(close(img.get(320, 180), RED) && close(img.get(170, 100), RED) && close(img.get(470, 260), RED));
        assert!(img.get(150, 180)[3] < 0.02 && img.get(320, 80)[3] < 0.02 && img.get(10, 10)[3] < 0.02, "empty around it");
    }
    // Scale to Frame Size fills the frame without touching Motion ▸ Scale
    r.clip(outer, nest).scale_to_frame = true;
    let img = r.frame(outer, Tick(1000));
    assert!(close(img.get(10, 10), RED) && close(img.get(630, 350), RED));
    // Motion ▸ Scale 200% (what Fit to frame sets here) does the same
    let c = r.clip(outer, nest);
    c.scale_to_frame = false;
    c.effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(200.0);
    let img = r.frame(outer, Tick(1000));
    assert!(close(img.get(10, 10), RED) && close(img.get(630, 350), RED));
}

#[test]
fn a_larger_nest_is_cropped_by_the_frame() {
    let mut r = Rig::new();
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let inner = r.seq("inner", w, h, FrameRate::FPS_24);
    r.put(inner, ocean, 48);
    let outer = r.seq("outer", w / 2, h / 2, FrameRate::FPS_24);
    r.put(outer, inner, 48);
    let (big, small) = (r.frame(inner, Tick(1000)), r.frame(outer, Tick(1000)));
    assert_eq!((small.w as u32, small.h as u32), (w / 2, h / 2));
    // the outer frame is the middle of the inner one, pixel for pixel
    let (ox, oy) = (big.w / 4, big.h / 4);
    for (x, y) in [(0, 0), (small.w / 2, small.h / 2), (small.w - 1, small.h - 1), (7, small.h - 3)] {
        assert!(close(small.get(x, y), big.get(x + ox, y + oy)), "({x}, {y})");
    }
}

#[test]
fn a_nest_at_another_frame_rate_shows_its_sequence_at_the_same_moment() {
    let mut r = Rig::new();
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let inner = r.seq("inner", w, h, FrameRate::FPS_30);
    r.put(inner, ocean, 90);
    let outer = r.seq("outer", w, h, FrameRate::FPS_24);
    r.put(outer, inner, 72);
    // 1.5 s into the 24 fps sequence is 1.5 s into the 30 fps one
    let t = FrameRate::FPS_24.tick_of(36);
    assert!(worst(&r.frame(outer, t), &r.frame(inner, t)) < 0.01);
    assert!(worst(&r.frame(inner, t), &r.frame(inner, Tick::ZERO)) > 0.05, "the footage moves, so the comparison means something");
}

#[test]
fn speed_reverse_and_frame_hold_on_a_nest_pick_the_frame_of_its_sequence() {
    let mut r = Rig::new();
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let rate = FrameRate::FPS_24;
    let inner = r.seq("inner", w, h, rate);
    r.put(inner, ocean, 96);
    let outer = r.seq("outer", w, h, rate);
    let nest = r.put(outer, inner, 48);
    let f = |n: i64| rate.tick_of(n);
    // double speed: frame 10 of the nest is frame 20 of its sequence
    r.clip(outer, nest).speed = 2.0;
    assert!(worst(&r.frame(outer, f(10)), &r.frame(inner, f(20))) < 0.01);
    // reversed (48 frames at normal speed): 10 frames in is 10 frames back from the end of what it
    // covers, a tick short of frame 38
    let c = r.clip(outer, nest);
    c.speed = 1.0;
    c.reverse = true;
    assert!(worst(&r.frame(outer, f(10)), &r.frame(inner, f(38) - Tick(1))) < 0.01);
    // a frame hold shows one frame of the sequence throughout
    let c = r.clip(outer, nest);
    c.reverse = false;
    c.frame_hold = Some(f(30));
    assert!(worst(&r.frame(outer, f(5)), &r.frame(inner, f(30))) < 0.01);
    assert!(worst(&r.frame(outer, f(40)), &r.frame(inner, f(30))) < 0.01);
}

#[test]
fn opacity_and_effects_on_a_nest_apply_to_its_whole_picture() {
    let mut r = Rig::new();
    let red = r.matte(RED, 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    r.put(inner, red, 48);
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    r.clip(outer, nest).effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    for img in [r.frame(outer, Tick(1000)), r.planned(outer, Tick(1000))] {
        let c = img.get(160, 90);
        assert!((c[3] - 0.5).abs() < 0.02, "half transparent: {c:?}");
    }
    // a crop on the nest cuts its picture like any clip's
    let c = r.clip(outer, nest);
    c.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(100.0);
    let mut crop = filmcraft_project::effect::find_effect("crop").unwrap().instance();
    crop.params.get_mut("left").unwrap().value = ParamValue::Float(50.0);
    c.effects.push(crop);
    let img = r.frame(outer, Tick(1000));
    assert!(img.get(40, 90)[3] < 0.02 && close(img.get(280, 90), RED));
}

#[test]
fn captions_inside_a_nest_are_part_of_its_picture() {
    // Premiere draws a nested sequence's captions in the sequence it is nested in
    let mut r = Rig::new();
    let red = r.matte(RED, 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    r.put(inner, red, 48);
    let mut track = CaptionTrack::new(TrackId(9001), "Subtitles".into(), CaptionFormat::Subtitle);
    track.captions.push(Caption {
        id: ClipId(9002),
        start: Tick::ZERO,
        duration: Tick(TICKS_PER_SECOND),
        text: "NESTED".into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    });
    r.p.sequence_mut(inner).unwrap().caption_tracks.push(track);
    let outer = r.seq("outer", 640, 360, FrameRate::FPS_24);
    r.put(outer, inner, 48);
    let t = Tick(1000);
    let with = r.frame(outer, t);
    r.p.sequence_mut(inner).unwrap().caption_tracks[0].enabled = false;
    let without = r.frame(outer, t);
    r.p.sequence_mut(inner).unwrap().caption_tracks[0].enabled = true;
    // the caption changes pixels, all of them inside the nest's part of the frame (x 160..480, y 90..270)
    let mut changed = 0;
    for y in 0..with.h {
        for x in 0..with.w {
            if !close(with.get(x, y), without.get(x, y)) {
                changed += 1;
                assert!((160..480).contains(&x) && (90..270).contains(&y), "caption pixel outside the nest at ({x}, {y})");
            }
        }
    }
    assert!(changed > 20, "the caption is drawn ({changed} pixels)");
    // after the caption ends there is nothing to draw
    assert!(worst(&r.frame(outer, Tick(TICKS_PER_SECOND + 1000)), &without) < 0.01);
    // the frame plan gives the same picture
    assert!(worst(&r.planned(outer, t), &with) < 0.02);
    // and the outer sequence's own captions are still its own business: with captions off for
    // the outer render, the nested ones show all the same
    let mut outer_track = CaptionTrack::new(TrackId(9003), "Outer".into(), CaptionFormat::Subtitle);
    outer_track.captions.push(Caption {
        id: ClipId(9004),
        start: Tick::ZERO,
        duration: Tick(TICKS_PER_SECOND),
        text: "OUTER".into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    });
    r.p.sequence_mut(outer).unwrap().caption_tracks.push(outer_track);
    assert!(worst(&r.frame(outer, t), &with) < 0.01, "outer captions are only drawn when asked for");
    let shown = render_sequence(&r.p, outer, t, RenderOptions { captions: true, ..Default::default() }, &r.map);
    assert!(worst(&shown, &with) > 0.1, "and are drawn when asked for");
}

// ---- the frame plan draws a plain nest's layers itself

/// How many layers the plan of `seq` at `t` has (None when it fell back to one CPU image).
fn layer_count(r: &Rig, seq: ItemId, t: Tick) -> Option<usize> {
    match plan::plan_frame(&r.p, seq, t, RenderOptions::default(), plan::DeviceCaps::ALL, &r.map) {
        plan::FramePlan::Layers { layers, .. } => Some(layers.len()),
        _ => None,
    }
}

/// An inner sequence with the ocean on V1 and a half-size, half-transparent red matte over it on
/// V2, nested alone in an outer sequence of the same size. Returns (inner, outer, nest clip).
fn two_layer_nest(r: &mut Rig) -> (ItemId, ItemId, ClipId) {
    let ocean = r.add(GeneratorSource::demo(DemoScene::OceanSunset));
    let (w, h) = {
        let v = r.map.0[&ocean].info().video.clone().unwrap();
        (v.width, v.height)
    };
    let red = r.matte(RED, w / 2, h / 2);
    let inner = r.seq("inner", w, h, FrameRate::FPS_24);
    r.put(inner, ocean, 96);
    let rate = FrameRate::FPS_24;
    let mut top = r.p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(96)), rate).unwrap();
    top.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    r.p.sequence_mut(inner).unwrap().video_tracks[1].items.push(top);
    let outer = r.seq("outer", w, h, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 96);
    (inner, outer, nest)
}

#[test]
fn a_plain_nest_is_planned_as_its_own_layers_and_looks_the_same() {
    let mut r = Rig::new();
    let (inner, outer, _) = two_layer_nest(&mut r);
    let t = FrameRate::FPS_24.tick_of(20);
    // two layers (the nest's clips), not one image of the nest
    assert_eq!(layer_count(&r, outer, t), Some(2));
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 0.01);
    assert!(worst(&r.planned(outer, t), &r.frame(inner, t)) < 0.01, "and it is the nested sequence's picture");
    // a nest of the nest still plans down to the two clips
    let outermost = r.seq("outermost", r.p.sequence(outer).unwrap().settings.width, r.p.sequence(outer).unwrap().settings.height, FrameRate::FPS_24);
    r.put(outermost, outer, 96);
    assert_eq!(layer_count(&r, outermost, t), Some(2));
    assert!(worst(&r.planned(outermost, t), &r.frame(outermost, t)) < 0.01);
    // with something under the nest in the outer sequence too
    let under = r.matte([0.0, 0.0, 1.0, 1.0], 64, 64);
    let rate = FrameRate::FPS_24;
    let nest_clip = r.p.sequence_mut(outermost).unwrap().video_tracks[0].items.remove(0);
    let mut below = r.p.make_track_item(under, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(96)), rate).unwrap();
    below.scale_to_frame = true;
    let s = r.p.sequence_mut(outermost).unwrap();
    s.video_tracks[0].items.push(below);
    s.video_tracks[1].items.push(nest_clip);
    assert_eq!(layer_count(&r, outermost, t), Some(3));
    assert!(worst(&r.planned(outermost, t), &r.frame(outermost, t)) < 0.01);
}

#[test]
fn a_nest_that_is_not_plain_is_still_drawn_right() {
    let t = FrameRate::FPS_24.tick_of(20);
    // each of these keeps the nest one layer (rendered on the CPU), and the plan matches the reference
    let cases: [(&str, fn(&mut Rig, ItemId, ItemId, ClipId)); 5] = [
        ("half-transparent nest", |r, _, outer, nest| {
            r.clip(outer, nest).effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
        }),
        ("moved nest", |r, _, outer, nest| {
            r.clip(outer, nest).effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(60.0);
        }),
        ("nest with an effect", |r, _, outer, nest| {
            let mut crop = filmcraft_project::effect::find_effect("crop").unwrap().instance();
            crop.params.get_mut("left").unwrap().value = ParamValue::Float(30.0);
            r.clip(outer, nest).effects.push(crop);
        }),
        ("a blend mode inside the nest", |r, inner, _, _| {
            let top = &mut r.p.sequence_mut(inner).unwrap().video_tracks[1].items[0];
            top.effect_mut("opacity").unwrap().params.get_mut("blend").unwrap().value = ParamValue::Choice(3);
        }),
        ("a nest of another size", |r, inner, _, _| {
            r.p.sequence_mut(inner).unwrap().settings.width /= 2;
        }),
    ];
    for (what, change) in cases {
        let mut r = Rig::new();
        let (inner, outer, nest) = two_layer_nest(&mut r);
        // under the nest, so that blending the nest as a whole or layer by layer would differ
        let under = r.matte([0.0, 1.0, 0.0, 1.0], 64, 64);
        let rate = FrameRate::FPS_24;
        let nest_clip = r.p.sequence_mut(outer).unwrap().video_tracks[0].items.remove(0);
        let mut below = r.p.make_track_item(under, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, rate.tick_of(96)), rate).unwrap();
        below.scale_to_frame = true;
        let s = r.p.sequence_mut(outer).unwrap();
        s.video_tracks[0].items.push(below);
        s.video_tracks[1].items.push(nest_clip);
        change(&mut r, inner, outer, nest);
        assert_eq!(layer_count(&r, outer, t), Some(2), "{what}: the matte and one image of the nest");
        let w = worst(&r.planned(outer, t), &r.frame(outer, t));
        assert!(w < 0.01, "{what}: plan and reference differ by {w}");
    }
}

#[test]
fn transformed_plain_nests_preserve_the_group_picture() {
    let t = FrameRate::FPS_24.tick_of(20);
    for (scale, rotation) in [(100.0, 0.0), (60.0, 0.0), (130.0, 19.0)] {
        let mut r = Rig::new();
        let (inner, outer, nest) = two_layer_nest(&mut r);
        let sequence = r.p.sequence(inner).unwrap();
        let child = sequence.video_tracks[0].items[0].id;
        let position = filmcraft_geom::Vec2::new(sequence.settings.width as f64 * 0.75, sequence.settings.height as f64 * 0.5);
        r.clip(inner, child).effect_mut("motion").unwrap().param_mut("position").unwrap().value = ParamValue::Vec2(position);
        let motion = r.clip(outer, nest).effect_mut("motion").unwrap();
        motion.param_mut("position").unwrap().value = ParamValue::Vec2(filmcraft_geom::Vec2::new(175.0, 75.0));
        motion.param_mut("scale").unwrap().value = ParamValue::Float(scale);
        motion.param_mut("rotation").unwrap().value = ParamValue::Float(rotation);
        for preview in [1.0, 0.5] {
            let opts = RenderOptions { scale: preview, ..Default::default() };
            let reference = render_sequence(&r.p, outer, t, opts, &r.map);
            let frame_plan = plan::plan_frame(&r.p, outer, t, opts, plan::DeviceCaps::ALL, &r.map);
            let planned = plan::execute_cpu(&frame_plan);
            let difference = worst(&planned, &reference);
            assert!(difference < 0.001, "scale {scale}, rotation {rotation}, preview {preview}: {difference}");
        }
    }
}

#[test]
fn a_transformed_nest_keeps_captions_at_its_held_source_time() {
    let mut r = Rig::new();
    let (inner, outer, nest) = two_layer_nest(&mut r);
    let mut captions = CaptionTrack::new(TrackId(9901), "Nested".into(), CaptionFormat::Subtitle);
    captions.captions.push(Caption {
        id: ClipId(9902),
        start: Tick::ZERO,
        duration: Tick(TICKS_PER_SECOND),
        text: "HELD".into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    });
    r.p.sequence_mut(inner).unwrap().caption_tracks.push(captions);
    let clip = r.clip(outer, nest);
    clip.frame_hold = Some(FrameRate::FPS_24.tick_of(12));
    clip.effect_mut("motion").unwrap().param_mut("scale").unwrap().value = ParamValue::Float(75.0);
    let t = FrameRate::FPS_24.tick_of(30);
    let with = r.frame(outer, t);
    assert!(worst(&r.planned(outer, t), &with) < 0.001);
    r.p.sequence_mut(inner).unwrap().caption_tracks[0].enabled = false;
    let without = r.frame(outer, t);
    assert!(worst(&with, &without) > 0.1, "held caption must be visible after its outer-time end");
    assert!(worst(&r.planned(outer, t), &without) < 0.001);
}

/// A project that stores a media width far from the file's (here 1 against 4096) used to scale
/// every effect size by the ratio. The median filter's cost is its radius squared per pixel.
#[test]
fn a_wrong_stored_media_width_does_not_blow_up_effect_sizes() {
    let mut r = Rig::new();
    let m = r.matte(RED, 4096, 16);
    if let Some(v) = r.p.item_mut(m).and_then(|i| i.as_media_mut()).and_then(|c| c.info.video.as_mut()) {
        v.width = 1;
    }
    let s = r.seq("S", 4096, 16, FrameRate::FPS_24);
    let c = r.put(s, m, 10);
    let mut e = filmcraft_project::find_effect("median").unwrap().instance();
    e.params.get_mut("radius").unwrap().value = ParamValue::Float(1.0);
    r.clip(s, c).effects.push(e);
    let t = std::time::Instant::now();
    let _ = r.frame(s, Tick::ZERO);
    assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
}

// ---- flattening a single-GPU-layer nest so its outer effects run on the GPU

/// An effect instance with `set` applied to its parameters.
fn fx(id: &str, set: impl FnOnce(&mut filmcraft_project::EffectInstance)) -> filmcraft_project::EffectInstance {
    let mut e = filmcraft_project::effect::find_effect(id).unwrap().instance();
    set(&mut e);
    e
}

fn set(e: &mut filmcraft_project::EffectInstance, id: &str, v: ParamValue) {
    e.params.get_mut(id).unwrap().value = v;
}

/// A crop instance (left/right percentages, edge feather in full-res pixels).
fn crop_effect(left: f64, right: f64, feather: f64) -> filmcraft_project::EffectInstance {
    fx("crop", |e| {
        set(e, "left", ParamValue::Float(left));
        set(e, "right", ParamValue::Float(right));
        set(e, "feather", ParamValue::Float(feather));
    })
}

/// A nest of one `w`×`h` colour matte carrying `inner_effects`, in an outer sequence of the same
/// size, all at 24 fps. Returns (inner, outer, nest clip). The matte is the nest's own size and its
/// motion is identity, so the nest flattens to a single GPU layer when every inner effect is
/// GPU-capable.
fn single_clip_nest(r: &mut Rig, w: u32, h: u32, inner_effects: Vec<filmcraft_project::EffectInstance>) -> (ItemId, ItemId, ClipId) {
    let matte = r.matte([0.6, 0.3, 0.2, 1.0], w, h);
    let inner = r.seq("inner", w, h, FrameRate::FPS_24);
    let clip = r.put(inner, matte, 48);
    r.clip(inner, clip).effects.extend(inner_effects);
    let outer = r.seq("outer", w, h, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    (inner, outer, nest)
}

/// The plan of `seq` at `t`: the layer count (None when the whole frame went to the CPU) and whether
/// the first layer carries a GPU effect stage (a flattened GPU nest) rather than a pre-rendered image
/// (the CPU fallback).
fn plan_shape(r: &Rig, seq: ItemId, t: Tick) -> (Option<usize>, bool) {
    match plan::plan_frame(&r.p, seq, t, RenderOptions::default(), plan::DeviceCaps::ALL, &r.map) {
        plan::FramePlan::Layers { layers, .. } => (Some(layers.len()), layers.first().is_some_and(|l| l.fx.is_some())),
        _ => (None, false),
    }
}

/// Max per-RGBA-channel error and the PSNR (over all four channels) of two same-size images.
fn channel_error(a: &Image, b: &Image) -> ([f32; 4], f64) {
    assert_eq!((a.w, a.h), (b.w, b.h));
    let mut max = [0.0f32; 4];
    let mut sq = 0.0f64;
    for (x, y) in a.px.as_chunks::<4>().0.iter().zip(b.px.as_chunks::<4>().0.iter()) {
        for c in 0..4 {
            let d = (x[c] - y[c]).abs();
            max[c] = max[c].max(d);
            sq += (d as f64) * (d as f64);
        }
    }
    let mse = sq / (a.w * a.h * 4) as f64;
    let psnr = if mse <= 0.0 { f64::INFINITY } else { 10.0 * (1.0 / mse).log10() };
    (max, psnr)
}

#[test]
fn a_gpu_crop_on_a_single_gpu_clip_nest_flattens_and_matches() {
    let mut r = Rig::new();
    let (_, outer, nest) = single_clip_nest(
        &mut r,
        320,
        180,
        vec![fx("brightness_contrast", |e| {
            set(e, "brightness", ParamValue::Float(25.0));
            set(e, "contrast", ParamValue::Float(-15.0));
        })],
    );
    // crop left/right keyframed over the nest's time, feather 4
    let mut crop = crop_effect(5.0, 8.0, 4.0);
    crop.params.get_mut("left").unwrap().put_keyframe(Tick::ZERO, ParamValue::Float(5.0));
    crop.params.get_mut("left").unwrap().put_keyframe(FrameRate::FPS_24.tick_of(24), ParamValue::Float(17.0));
    crop.params.get_mut("right").unwrap().put_keyframe(Tick::ZERO, ParamValue::Float(8.0));
    crop.params.get_mut("right").unwrap().put_keyframe(FrameRate::FPS_24.tick_of(24), ParamValue::Float(16.0));
    r.clip(outer, nest).effects.push(crop);
    for frame in [0, 12, 24] {
        let t = FrameRate::FPS_24.tick_of(frame);
        let (count, gpu) = plan_shape(&r, outer, t);
        assert_eq!(count, Some(1), "frame {frame}: one layer");
        assert!(gpu, "frame {frame}: the nest flattened to a GPU layer");
        let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
        println!("crop keyframed frame {frame}: max channel err {max:?} PSNR {psnr:.2} dB");
        assert!(max.iter().all(|&e| e < 0.01), "frame {frame}: max channel err {max:?}");
    }
}

#[test]
fn a_nest_of_four_gpu_effects_flattens_and_matches() {
    let mut r = Rig::new();
    let inner_effects = vec![
        fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(12.0))),
        fx("color_balance", |e| {
            set(e, "mid_r", ParamValue::Float(20.0));
            set(e, "hi_b", ParamValue::Float(-10.0));
        }),
        fx("levels", |e| {
            set(e, "in_black", ParamValue::Float(8.0));
            set(e, "in_white", ParamValue::Float(240.0));
        }),
        fx("horizontal_flip", |_| {}),
    ];
    let (_, outer, nest) = single_clip_nest(&mut r, 320, 180, inner_effects);
    r.clip(outer, nest).effects.push(crop_effect(12.0, 18.0, 4.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1));
    assert!(gpu, "the nest flattened to a GPU layer");
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("four gpu effects: max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 0.01), "max channel err {max:?}");
}

#[test]
fn a_nest_with_a_cpu_only_effect_falls_back_without_regression() {
    let mut r = Rig::new();
    // `ultra_key` has no GPU implementation today: the single-layer gate must refuse to flatten and
    // keep the nest on its current CPU path, with the same picture as before.
    let inner_effects = vec![fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(12.0))), fx("ultra_key", |_| {})];
    let (_, outer, nest) = single_clip_nest(&mut r, 320, 180, inner_effects);
    r.clip(outer, nest).effects.push(crop_effect(12.0, 18.0, 4.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1), "the nest renders as one CPU image");
    assert!(!gpu, "the nest did not flatten (CPU-only inner effect)");
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("cpu-only effect (fallback): max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 0.01), "max channel err {max:?}");
}

#[test]
fn a_nest_with_a_sharpening_lumetri_falls_back_without_regression() {
    // `lumetri` is in `GPU_EFFECTS`, so the single-layer gate passes on membership, but sharpen makes
    // it evaluate CPU-only: the inner layer ends up with no GPU stage and the nest falls back to a
    // CPU image layer, keeping the rest of the frame on the GPU, with the same picture.
    let mut r = Rig::new();
    let inner_effects = vec![fx("lumetri", |e| set(e, "sharpen", ParamValue::Float(20.0)))];
    let (_, outer, nest) = single_clip_nest(&mut r, 320, 180, inner_effects);
    r.clip(outer, nest).effects.push(crop_effect(12.0, 18.0, 4.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1), "the nest renders as one CPU image layer, not a whole-frame fallback");
    assert!(!gpu, "no GPU effect stage on the inner layer");
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("sharpening lumetri (fallback): max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 1e-6), "max channel err {max:?}");
}

#[test]
fn a_frame_blended_inner_clip_falls_back_to_a_cpu_layer_not_a_whole_frame() {
    // A frame-blended inner clip is decoded and blended on the CPU, so its layer has no GPU effect
    // stage; the nest then falls back to one CPU image layer (not a whole-frame fallback), keeping
    // the rest of the frame on the GPU, exactly as before flattening.
    let mut r = Rig::new();
    let matte = r.matte([0.6, 0.3, 0.2, 1.0], 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    let clip = r.put(inner, matte, 48);
    r.clip(inner, clip).effects.push(fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(10.0))));
    let c = r.clip(inner, clip);
    c.speed = 1.5;
    c.time_interpolation = filmcraft_project::TimeInterpolation::FrameBlending;
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    r.clip(outer, nest).effects.push(crop_effect(10.0, 15.0, 4.0));
    let t = FrameRate::FPS_24.tick_of(5);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1), "the nest falls back to one CPU image layer, not a whole-frame fallback");
    assert!(!gpu);
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 0.01);
}

#[test]
fn a_nest_inside_a_nest_keeps_its_picture() {
    // The outer nest holds a nested sequence (not a media clip), so the single-media-layer gate
    // refuses it: the outer nest stays on the CPU, which renders the inner nest, and the picture is
    // unchanged. The inner GPU nest still flattens on its own when planned directly.
    let mut r = Rig::new();
    let (inner, _mid, _) = single_clip_nest(&mut r, 320, 180, vec![fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(20.0)))]);
    let mid = r.seq("mid", 320, 180, FrameRate::FPS_24);
    r.put(mid, inner, 48);
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    let nest = r.put(outer, mid, 48);
    r.clip(outer, nest).effects.push(crop_effect(10.0, 15.0, 3.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("nest in nest: max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 0.01), "max channel err {max:?}");
}

#[test]
fn a_two_clip_nest_does_not_flatten() {
    let mut r = Rig::new();
    let matte = r.matte([0.6, 0.3, 0.2, 1.0], 320, 180);
    let top = r.matte([0.1, 0.5, 0.4, 1.0], 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    let clip = r.put(inner, matte, 48);
    r.clip(inner, clip).effects.push(fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(10.0))));
    let mut top = r.p.make_track_item(top, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(48)), FrameRate::FPS_24).unwrap();
    top.effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    r.p.sequence_mut(inner).unwrap().video_tracks[1].items.push(top);
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    r.clip(outer, nest).effects.push(crop_effect(10.0, 10.0, 3.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1), "one CPU image of the nest");
    assert!(!gpu, "two clips: no flatten");
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 0.01);
}

#[test]
fn speed_reverse_and_frame_hold_on_a_gpu_nest_keep_the_picture() {
    let mut r = Rig::new();
    let (_, outer, nest) = single_clip_nest(&mut r, 320, 180, vec![fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(15.0)))]);
    r.clip(outer, nest).effects.push(crop_effect(8.0, 12.0, 3.0));
    let f = |n: i64| FrameRate::FPS_24.tick_of(n);
    // double speed
    r.clip(outer, nest).speed = 2.0;
    for t in [f(2), f(10)] {
        assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 0.01, "speed 2x at {t:?}");
    }
    // reversed
    let c = r.clip(outer, nest);
    c.speed = 1.0;
    c.reverse = true;
    assert!(worst(&r.planned(outer, f(10)), &r.frame(outer, f(10))) < 0.01, "reverse");
    // frame hold
    let c = r.clip(outer, nest);
    c.reverse = false;
    c.frame_hold = Some(f(8));
    for t in [f(4), f(30)] {
        assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 0.01, "frame hold at {t:?}");
    }
}

#[test]
fn source_in_offsets_and_edge_crop_keep_the_picture() {
    let mut r = Rig::new();
    let (_, outer, nest) = single_clip_nest(&mut r, 320, 180, vec![fx("brightness_contrast", |e| set(e, "contrast", ParamValue::Float(20.0)))]);
    r.clip(outer, nest).source_in = FrameRate::FPS_24.tick_of(6);
    // crop cuts the nest's edges
    r.clip(outer, nest).effects.push(crop_effect(15.0, 15.0, 5.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("source_in offset + edge crop: max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 0.01), "max channel err {max:?}");
}

#[test]
fn a_video_less_nest_contributes_no_picture() {
    let mut r = Rig::new();
    // a sequence with an audio track but no video track
    let audio_only =
        r.p.new_sequence("audio_only", SequenceSettings { width: 320, height: 180, frame_rate: FrameRate::FPS_24, ..Default::default() }, 0, 1, None);
    let tone = r.add(GeneratorSource::new(Generator::Tone { hz: 440.0, db: -20.0 }, 1, 1, FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND)));
    let ti = r.p.make_track_item(tone, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(48)), FrameRate::FPS_24).unwrap();
    r.p.sequence_mut(audio_only).unwrap().audio_tracks[0].items.push(ti);
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    r.put(outer, audio_only, 48);
    let t = FrameRate::FPS_24.tick_of(10);
    let img = r.planned(outer, t);
    assert_eq!((img.w, img.h), (320, 180));
    assert!(img.px.iter().all(|v| v.abs() < 1e-6), "a video-less nest draws nothing");
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 1e-6);
}

#[test]
fn an_empty_nest_plans_to_nothing() {
    let mut r = Rig::new();
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    r.put(outer, inner, 48);
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, _) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(0), "the nest has no layers");
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 1e-6);
}

#[test]
fn a_self_nesting_sequence_stops_at_the_depth_guard() {
    let mut r = Rig::new();
    let red = r.matte(RED, 32, 32);
    let s = r.seq("S", 32, 32, FrameRate::FPS_24);
    r.put(s, red, 10);
    // nest S inside itself
    let ti = r.p.make_track_item(s, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(10)), FrameRate::FPS_24).unwrap();
    r.p.sequence_mut(s).unwrap().video_tracks[0].items.push(ti);
    let img = r.planned(s, Tick::ZERO);
    assert_eq!((img.w, img.h), (32, 32));
    assert!(img.px.iter().all(|v| v.is_finite()), "the depth guard stops the descent");
}

#[test]
fn a_zooming_crop_resamples_instead_of_being_forced_to_the_gpu() {
    let mut r = Rig::new();
    let (_, outer, nest) = single_clip_nest(&mut r, 320, 180, vec![fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(10.0)))]);
    // zoom crop that magnifies (stays on the GPU)
    let mut zoom = crop_effect(10.0, 10.0, 0.0);
    set(&mut zoom, "zoom", ParamValue::Bool(true));
    r.clip(outer, nest).effects.push(zoom.clone());
    let t = FrameRate::FPS_24.tick_of(10);
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("zooming crop (magnify): max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 0.01), "max channel err {max:?}");
    // a negative (zoom-out) crop minifies: the gate must not force it onto the GPU
    let c = r.clip(outer, nest);
    c.effects.pop(); // drop the magnifying zoom crop (keep motion and opacity)
    let mut out = crop_effect(-20.0, -20.0, 0.0);
    set(&mut out, "zoom", ParamValue::Bool(true));
    c.effects.push(out);
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 0.01, "minifying zoom crop stays exact");
}

#[test]
fn odd_dimensions_keep_the_picture() {
    let mut r = Rig::new();
    let (_, outer, nest) = single_clip_nest(&mut r, 321, 181, vec![fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(10.0)))]);
    r.clip(outer, nest).effects.push(crop_effect(7.0, 11.0, 4.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("odd dimensions: max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 0.01), "max channel err {max:?}");
}

#[test]
fn outer_opacity_and_blend_keep_the_nest_on_the_cpu() {
    let mut r = Rig::new();
    let (_, outer, nest) = single_clip_nest(&mut r, 320, 180, vec![fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(10.0)))]);
    r.clip(outer, nest).effects.push(crop_effect(10.0, 10.0, 3.0));
    let t = FrameRate::FPS_24.tick_of(10);
    // half transparent: the nest must not flatten (it would change how the crop blends)
    r.clip(outer, nest).effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1));
    assert!(!gpu, "partial opacity keeps the nest on the CPU");
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 0.01);
}

#[test]
fn a_partial_opacity_inner_clip_keeps_the_nest_on_the_cpu() {
    // The CPU bakes the inner clip's opacity into the nest's picture before the nest's outer
    // effects; the flattened layer would apply it at the composite after them. A non-linear outer
    // effect (gamma) makes the two orders differ, so the nest must not flatten.
    let mut r = Rig::new();
    let matte = r.matte([0.6, 0.3, 0.2, 1.0], 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    let clip = r.put(inner, matte, 48);
    r.clip(inner, clip).effects.push(fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(10.0))));
    r.clip(inner, clip).effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    r.clip(outer, nest).effects.push(fx("gamma_correction", |e| set(e, "gamma", ParamValue::Float(22.0))));
    r.clip(outer, nest).effects.push(crop_effect(12.0, 18.0, 4.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1));
    assert!(!gpu, "partial inner opacity keeps the nest on the CPU");
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("partial inner opacity: max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 0.01), "max channel err {max:?}");
}

#[test]
fn a_nest_with_visible_captions_keeps_its_picture_on_the_cpu() {
    // The CPU bakes the nest's captions into its picture and then applies the outer crop, so the
    // crop cuts the captions too; the flatten branch would draw the captions over the cropped layer.
    // A visible caption therefore keeps the nest on the CPU.
    let mut r = Rig::new();
    let (inner, outer, nest) = single_clip_nest(&mut r, 320, 180, vec![fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(10.0)))]);
    let mut track = CaptionTrack::new(TrackId(9001), "Subtitles".into(), CaptionFormat::Subtitle);
    track.captions.push(Caption {
        id: ClipId(9002),
        start: Tick::ZERO,
        duration: Tick(TICKS_PER_SECOND),
        text: "NESTED".into(),
        speaker: None,
        cue_id: None,
        settings: String::new(),
    });
    r.p.sequence_mut(inner).unwrap().caption_tracks.push(track);
    r.clip(outer, nest).effects.push(crop_effect(15.0, 20.0, 4.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1));
    assert!(!gpu, "a visible caption keeps the nest on the CPU");
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("nest with captions: max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 0.01), "max channel err {max:?}");
}

#[test]
fn a_nest_at_another_frame_rate_flattens_and_matches() {
    let mut r = Rig::new();
    let matte = r.matte([0.6, 0.3, 0.2, 1.0], 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_30);
    let clip = r.put(inner, matte, 60);
    r.clip(inner, clip).effects.push(fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(10.0))));
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    r.clip(outer, nest).effects.push(crop_effect(10.0, 15.0, 4.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1));
    assert!(gpu, "the nest flattened across the frame-rate change");
    let (max, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    println!("30fps nest in 24fps seq: max channel err {max:?} PSNR {psnr:.2} dB");
    assert!(max.iter().all(|&e| e < 0.01), "max channel err {max:?}");
}

#[test]
fn an_anamorphic_nest_stays_on_the_cpu_and_matches() {
    // A square clip in an anamorphic (2:1) nest is not identity Motion (the pixel aspect differs),
    // so the nest must not flatten; the picture still matches the CPU.
    let mut r = Rig::new();
    let matte = r.matte([0.6, 0.3, 0.2, 1.0], 320, 180);
    let inner = r.seq("inner", 320, 180, FrameRate::FPS_24);
    r.p.sequence_mut(inner).unwrap().settings.par = (2, 1);
    let clip = r.put(inner, matte, 48);
    r.clip(inner, clip).effects.push(fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(10.0))));
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    r.clip(outer, nest).effects.push(crop_effect(10.0, 15.0, 4.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1));
    assert!(!gpu, "an anamorphic nest keeps the nest on the CPU");
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 0.01);
}

// ---- flattening a nest whose own Motion is not the identity

/// A nest of one textured (colour bars) `w`x`h` clip carrying GPU effects of the same kind as the
/// publicity project's `webcam_full` chain, in an outer sequence of the same size, all at 24 fps.
/// Returns (outer, nest clip). Bars have hard edges, so a resampling difference shows.
fn bars_nest(r: &mut Rig, w: u32, h: u32) -> (ItemId, ItemId, ClipId) {
    let bars = r.add(GeneratorSource::new(Generator::BarsAndTone, w, h, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND)));
    let inner = r.seq("inner", w, h, FrameRate::FPS_24);
    let clip = r.put(inner, bars, 48);
    r.clip(inner, clip).effects.extend([
        fx("brightness_contrast", |e| set(e, "brightness", ParamValue::Float(12.0))),
        fx("color_balance", |e| {
            set(e, "mid_r", ParamValue::Float(20.0));
            set(e, "hi_b", ParamValue::Float(-10.0));
        }),
        fx("levels", |e| {
            set(e, "in_black", ParamValue::Float(8.0));
            set(e, "in_white", ParamValue::Float(240.0));
        }),
        fx("horizontal_flip", |_| {}),
        fx("lumetri", |e| set(e, "saturation", ParamValue::Float(120.0))),
    ]);
    let outer = r.seq("outer", w, h, FrameRate::FPS_24);
    let nest = r.put(outer, inner, 48);
    (inner, outer, nest)
}

/// The outer clip's Motion: scale (%), position, and optionally rotation, anchor and a separate
/// width scale.
#[derive(Clone, Copy, Default)]
struct Mo {
    scale: f64,
    pos: (f64, f64),
    rotation: f64,
    anchor: Option<(f64, f64)>,
    scale_width: Option<f64>,
}

fn apply_motion(r: &mut Rig, outer: ItemId, nest: ClipId, m: Mo) {
    let e = r.clip(outer, nest).effect_mut("motion").unwrap();
    set(e, "position", ParamValue::Vec2(filmcraft_geom::Vec2::new(m.pos.0, m.pos.1)));
    set(e, "scale", ParamValue::Float(m.scale));
    set(e, "rotation", ParamValue::Float(m.rotation));
    if let Some((x, y)) = m.anchor {
        set(e, "anchor", ParamValue::Vec2(filmcraft_geom::Vec2::new(x, y)));
    }
    if let Some(sw) = m.scale_width {
        set(e, "uniform_scale", ParamValue::Bool(false));
        set(e, "scale_width", ParamValue::Float(sw));
    }
}

/// The crop of the real project's `webcam_full` clips: feather 4, edges keyframed.
fn keyframed_crop() -> filmcraft_project::EffectInstance {
    let mut crop = crop_effect(0.0, 0.0, 4.0);
    crop.params.get_mut("left").unwrap().put_keyframe(Tick::ZERO, ParamValue::Float(5.0));
    crop.params.get_mut("left").unwrap().put_keyframe(FrameRate::FPS_24.tick_of(24), ParamValue::Float(17.0));
    crop.params.get_mut("right").unwrap().put_keyframe(Tick::ZERO, ParamValue::Float(8.0));
    crop.params.get_mut("right").unwrap().put_keyframe(FrameRate::FPS_24.tick_of(24), ParamValue::Float(16.0));
    crop
}

/// Plans the nest at frame 10 (preview `scale`) and returns whether it flattened to one GPU layer,
/// with the PSNR and per-channel max error of the plan against the CPU render.
fn motion_parity(label: &str, w: u32, h: u32, m: Mo, outer_fx: Vec<filmcraft_project::EffectInstance>, scale: f32) -> (bool, f64, [f32; 4]) {
    let mut r = Rig::new();
    let (_, outer, nest) = bars_nest(&mut r, w, h);
    apply_motion(&mut r, outer, nest, m);
    r.clip(outer, nest).effects.extend(outer_fx);
    let t = FrameRate::FPS_24.tick_of(10);
    let opts = RenderOptions { scale, ..Default::default() };
    let plan = plan::plan_frame(&r.p, outer, t, opts, plan::DeviceCaps::ALL, &r.map);
    let flat = matches!(&plan, plan::FramePlan::Layers { layers, .. } if layers.len() == 1 && layers[0].fx.is_some());
    let reference = render_sequence(&r.p, outer, t, opts, &r.map);
    let (max, psnr) = channel_error(&plan::execute_cpu(&plan), &reference);
    println!("nest motion [{label}] preview {scale}: flattened {flat}, PSNR {psnr:.2} dB, max channel err {max:?}");
    (flat, psnr, max)
}

#[test]
fn the_real_project_motion_values_flatten_and_match() {
    // webcam_full: a 1080x1920 nest in a 1080x1920 sequence, Motion 62 % at (540, 1324), and 100 %
    // at (540, 960), with and without the keyframed crop (feather 4), at the full frame and at
    // the half and quarter sizes the monitor plays at
    for (label, m, crop) in [
        ("62% (540,1324)", Mo { scale: 62.0, pos: (540.0, 1324.0), ..Default::default() }, false),
        ("62% (540,1324) + crop", Mo { scale: 62.0, pos: (540.0, 1324.0), ..Default::default() }, true),
        ("100% (540,960) + crop", Mo { scale: 100.0, pos: (540.0, 960.0), ..Default::default() }, true),
    ] {
        for preview in [1.0, 0.5, 0.25] {
            let outer_fx = if crop { vec![keyframed_crop()] } else { vec![] };
            let (flat, psnr, _) = motion_parity(label, 1080, 1920, m, outer_fx, preview);
            assert!(flat, "{label} (preview {preview}): the nest flattens to one GPU layer");
            // the criterion (40 dB) holds at the full frame; the half and quarter sizes the monitor
            // plays at fall short of it by a few dB (see the numbers printed above), kept as a floor
            let floor = if preview == 1.0 { 40.0 } else { 30.0 };
            assert!(psnr >= floor, "{label} (preview {preview}): PSNR {psnr}");
        }
    }
}

#[test]
fn a_scaled_magnified_or_anchored_nest_flattens_within_resampling_error() {
    // hard bar edges in a 320x180 frame: about one pixel in eight sits on an edge, so PSNR is
    // edge-limited (the real 1080x1920 frame above reaches 43 dB and more)
    let base = Mo { scale: 62.0, pos: (200.0, 120.0), ..Default::default() };
    for (label, m) in [("magnify 130", Mo { scale: 130.0, ..base }), ("anchor", Mo { anchor: Some((80.0, 40.0)), ..base })] {
        for scale in [1.0, 0.5] {
            for with_crop in [false, true] {
                let outer_fx = if with_crop { vec![keyframed_crop()] } else { vec![] };
                let (flat, psnr, _) = motion_parity(label, 320, 180, m, outer_fx, scale);
                assert!(flat, "{label} (preview {scale}, crop {with_crop}) flattens");
                assert!(psnr >= 33.0, "{label} (preview {scale}, crop {with_crop}): PSNR {psnr}");
            }
        }
    }
}

#[test]
fn a_rotated_or_stretched_nest_keeps_the_cpu_picture() {
    let base = Mo { scale: 62.0, pos: (200.0, 120.0), ..Default::default() };
    for (label, m) in [
        ("rotation 15", Mo { rotation: 15.0, ..base }),
        ("scale_width 50", Mo { scale: 70.0, scale_width: Some(50.0), ..base }),
        ("anchor + rotation + width", Mo { rotation: -30.0, anchor: Some((250.0, 150.0)), scale_width: Some(120.0), ..base }),
        ("negative scale", Mo { scale: -50.0, ..base }),
    ] {
        for scale in [1.0, 0.5] {
            for with_crop in [false, true] {
                let outer_fx = if with_crop { vec![keyframed_crop()] } else { vec![] };
                let (flat, psnr, max) = motion_parity(label, 320, 180, m, outer_fx, scale);
                assert!(!flat, "{label} (preview {scale}, crop {with_crop}) stays on the CPU");
                assert!(psnr.is_infinite() && max.iter().all(|&e| e == 0.0), "{label}: the picture is the CPU's");
            }
        }
    }
}

#[test]
fn an_outer_flip_with_motion_keeps_the_cpu_picture() {
    let m = Mo { scale: 62.0, pos: (200.0, 120.0), ..Default::default() };
    let (flat, psnr, _) = motion_parity("flip + crop", 320, 180, m, vec![fx("horizontal_flip", |_| {}), crop_effect(6.0, 9.0, 3.0)], 1.0);
    assert!(!flat, "a flip about the nest's edges stays on the CPU when Motion moves the nest");
    assert!(psnr.is_infinite(), "the picture is the CPU's");
    // with the identity Motion it still flattens, as before
    let (flat, _, _) =
        motion_parity("flip, identity", 320, 180, Mo { scale: 100.0, pos: (160.0, 90.0), ..Default::default() }, vec![fx("horizontal_flip", |_| {})], 1.0);
    assert!(flat);
}

#[test]
fn a_nest_with_motion_and_no_effects_of_its_own_flattens() {
    let m = Mo { scale: 62.0, pos: (200.0, 120.0), ..Default::default() };
    let (flat, psnr, _) = motion_parity("motion only", 320, 180, m, vec![], 1.0);
    assert!(flat, "Motion alone flattens the nest onto the inner clip's GPU layer");
    assert!(psnr >= 33.0, "PSNR {psnr}");
}

#[test]
fn a_nest_in_a_nest_with_motion_keeps_its_picture() {
    let mut r = Rig::new();
    let (inner, _, _) = bars_nest(&mut r, 320, 180);
    let mid = r.seq("mid", 320, 180, FrameRate::FPS_24);
    r.put(mid, inner, 48);
    let outer = r.seq("outer", 320, 180, FrameRate::FPS_24);
    let nest = r.put(outer, mid, 48);
    apply_motion(&mut r, outer, nest, Mo { scale: 62.0, pos: (200.0, 120.0), ..Default::default() });
    r.clip(outer, nest).effects.push(crop_effect(10.0, 15.0, 3.0));
    let t = FrameRate::FPS_24.tick_of(10);
    let (_, psnr) = channel_error(&r.planned(outer, t), &r.frame(outer, t));
    assert!(psnr >= 33.0, "PSNR {psnr}");
}

#[test]
fn a_self_nesting_sequence_with_motion_stops_at_the_depth_guard() {
    let mut r = Rig::new();
    let red = r.matte(RED, 32, 32);
    let s = r.seq("S", 32, 32, FrameRate::FPS_24);
    r.put(s, red, 10);
    let ti = r.p.make_track_item(s, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(10)), FrameRate::FPS_24).unwrap();
    let id = ti.id;
    r.p.sequence_mut(s).unwrap().video_tracks[0].items.push(ti);
    apply_motion(&mut r, s, id, Mo { scale: 50.0, pos: (16.0, 16.0), ..Default::default() });
    r.clip(s, id).effects.push(crop_effect(10.0, 10.0, 2.0));
    let img = r.planned(s, Tick::ZERO);
    assert_eq!((img.w, img.h), (32, 32));
    assert!(img.px.iter().all(|v| v.is_finite()));
}

#[test]
fn a_degenerate_nest_motion_does_not_crash() {
    for scale in [0.0, f64::NAN, 1.0e9, -50.0] {
        let mut r = Rig::new();
        let (_, outer, nest) = bars_nest(&mut r, 64, 36);
        apply_motion(&mut r, outer, nest, Mo { scale, pos: (32.0, 18.0), ..Default::default() });
        r.clip(outer, nest).effects.push(crop_effect(10.0, 10.0, 2.0));
        let img = r.planned(outer, FrameRate::FPS_24.tick_of(3));
        assert_eq!((img.w, img.h), (64, 36), "scale {scale}");
    }
}

#[test]
fn a_cpu_only_outer_effect_with_motion_keeps_the_old_picture() {
    // `ultra_key` has no GPU implementation here: the nest stays one CPU image layer and the plan
    // is the CPU render, with the clip's Motion applied as before
    let mut r = Rig::new();
    let (_, outer, nest) = bars_nest(&mut r, 320, 180);
    apply_motion(&mut r, outer, nest, Mo { scale: 62.0, pos: (200.0, 120.0), ..Default::default() });
    r.clip(outer, nest).effects.push(fx("ultra_key", |_| {}));
    let t = FrameRate::FPS_24.tick_of(10);
    let (count, gpu) = plan_shape(&r, outer, t);
    assert_eq!(count, Some(1));
    assert!(!gpu, "a CPU-only outer effect keeps the nest on the CPU");
    assert!(worst(&r.planned(outer, t), &r.frame(outer, t)) < 1e-6);
}
