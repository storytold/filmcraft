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
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            }),
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
        render_sequence(&self.p, seq, t, RenderOptions::default(), &self.map).unwrap()
    }

    /// The same frame through the frame plan (what the GPU compositor is given).
    fn planned(&self, seq: ItemId, t: Tick) -> Image {
        plan::execute_cpu(&plan::plan_frame(&self.p, seq, t, RenderOptions::default(), &self.map).unwrap()).unwrap()
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
    let shown = render_sequence(&r.p, outer, t, RenderOptions { captions: true, ..Default::default() }, &r.map).unwrap();
    assert!(worst(&shown, &with) > 0.1, "and are drawn when asked for");
}

// ---- the frame plan draws a plain nest's layers itself

/// How many layers the plan of `seq` at `t` has (None when it fell back to one CPU image).
fn layer_count(r: &Rig, seq: ItemId, t: Tick) -> Option<usize> {
    match plan::plan_frame(&r.p, seq, t, RenderOptions::default(), &r.map).unwrap() {
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
