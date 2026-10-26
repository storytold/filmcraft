//! Adjustment layers (M5.6): effects apply to the composite of every track below, opacity and
//! blend mode mix the adjusted picture over the original, Motion and masks restrict the region,
//! and nesting scopes them to their own sequence.

use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{ClipId, Label, Mask, MaskPath, MediaClip, MediaRef, SequenceSettings, TrackKind, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

const R: FrameRate = FrameRate::FPS_24;

struct Scene {
    p: Project,
    seq: ItemId,
    map: SourceMap,
}

fn media(p: &mut Project, map: &mut SourceMap, g: GeneratorSource) -> ItemId {
    let info = g.info().clone();
    let generator = g.generator.clone();
    let id = p.add_item(
        &info.name.clone(),
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(generator),
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
    map.0.insert(id, Arc::new(g) as SharedSource);
    id
}

fn matte(p: &mut Project, map: &mut SourceMap, c: [f32; 4]) -> ItemId {
    media(p, map, GeneratorSource::new(Generator::ColorMatte { color: c }, 320, 180, R, Tick(10 * TICKS_PER_SECOND)))
}

fn place(p: &mut Project, seq: ItemId, track: usize, item: ItemId) -> ClipId {
    let ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, R.tick_of(48)), R).unwrap();
    let id = ti.id;
    let mut ti = ti;
    ti.scale_to_frame = true;
    let s = p.sequence_mut(seq).unwrap();
    s.video_tracks[track].items.push(ti);
    s.video_tracks[track].sort();
    id
}

fn adjustment(p: &mut Project, seq: ItemId, track: usize) -> ClipId {
    let a = p.add_item("Adjustment Layer", Label::Lavender, ItemKind::AdjustmentLayer { width: 320, height: 180, rate: R, duration: R.tick_of(48) }, None);
    place(p, seq, track, a)
}

fn scene(tracks: usize) -> Scene {
    let mut p = Project::new("adj");
    let seq = p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: R, ..Default::default() }, tracks, 0, None);
    Scene { p, seq, map: SourceMap::default() }
}

fn clip(s: &mut Scene, c: ClipId) -> &mut TrackItem {
    s.p.sequence_mut(s.seq).unwrap().find_item_mut(c).unwrap().1
}

fn fx(s: &mut Scene, c: ClipId, id: &str) {
    let e = find_effect(id).unwrap().instance();
    let it = clip(s, c);
    let pos = it.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(it.effects.len());
    it.effects.insert(pos, e);
}

fn render(s: &Scene) -> Image {
    render_sequence(&s.p, s.seq, R.tick_of(12), RenderOptions::default(), &s.map)
}

fn near(a: [f32; 4], b: [f32; 3], tol: f32) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < tol)
}

#[test]
fn applies_to_everything_below_but_not_above() {
    let mut s = scene(3);
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    let blue = matte(&mut s.p, &mut s.map, [0.0, 0.0, 1.0, 1.0]);
    place(&mut s.p, s.seq, 0, red);
    let adj = adjustment(&mut s.p, s.seq, 1);
    fx(&mut s, adj, "invert");
    let img = render(&s);
    assert!(near(img.get(160, 90), [0.0, 1.0, 1.0], 1e-4), "red inverted to cyan: {:?}", img.get(160, 90));
    // a small blue layer above the adjustment layer is untouched
    let top = place(&mut s.p, s.seq, 2, blue);
    clip(&mut s, top).effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(25.0);
    let img = render(&s);
    assert!(near(img.get(160, 90), [0.0, 0.0, 1.0], 1e-4), "{:?}", img.get(160, 90));
    assert!(near(img.get(10, 10), [0.0, 1.0, 1.0], 1e-4));
}

#[test]
fn opacity_and_blend_mode_mix_over_the_original() {
    let mut s = scene(2);
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    place(&mut s.p, s.seq, 0, red);
    let adj = adjustment(&mut s.p, s.seq, 1);
    fx(&mut s, adj, "invert");
    clip(&mut s, adj).effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(25.0);
    let c = render(&s).get(100, 100);
    assert!(near(c, [0.75, 0.25, 0.25], 1e-3), "{c:?}");
    // Multiply: red × cyan = black at 100 %
    let op = clip(&mut s, adj).effect_mut("opacity").unwrap();
    op.params.get_mut("opacity").unwrap().value = ParamValue::Float(100.0);
    op.params.get_mut("blend").unwrap().value =
        ParamValue::Choice(filmcraft_project::effect::BLEND_MODES.iter().position(|b| *b == "Multiply").unwrap() as u32);
    let c = render(&s).get(100, 100);
    assert!(near(c, [0.0, 0.0, 0.0], 1e-3), "{c:?}");
}

#[test]
fn motion_and_masks_restrict_the_region() {
    let mut s = scene(2);
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    place(&mut s.p, s.seq, 0, red);
    let adj = adjustment(&mut s.p, s.seq, 1);
    fx(&mut s, adj, "invert");
    // Motion: scaled to 50 % around the centre → only the middle is inverted
    clip(&mut s, adj).effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(50.0);
    let img = render(&s);
    assert!(near(img.get(160, 90), [0.0, 1.0, 1.0], 1e-4), "{:?}", img.get(160, 90));
    assert!(near(img.get(10, 10), [1.0, 0.0, 0.0], 1e-4), "{:?}", img.get(10, 10));
    clip(&mut s, adj).effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(100.0);
    // a mask on the adjustment layer's effect (sequence pixels): left half
    let inv = clip(&mut s, adj).effects.iter_mut().find(|e| e.effect == "invert").unwrap();
    let mut m = Mask::new("m", MaskPath::rect(0.0, 0.0, 160.0, 180.0));
    m.feather.value = ParamValue::Float(0.0);
    inv.masks.push(m);
    let img = render(&s);
    assert!(near(img.get(40, 90), [0.0, 1.0, 1.0], 1e-4));
    assert!(near(img.get(280, 90), [1.0, 0.0, 0.0], 1e-4));
    // the same at half resolution (mask in sequence pixels scales with the output)
    let half = render_sequence(&s.p, s.seq, R.tick_of(12), RenderOptions { scale: 0.5, ..Default::default() }, &s.map);
    assert!(near(half.get(20, 45), [0.0, 1.0, 1.0], 1e-4));
    assert!(near(half.get(140, 45), [1.0, 0.0, 0.0], 1e-4));
    // an opacity mask on the adjustment layer: right half only (with the effect mask → nothing)
    let op = clip(&mut s, adj).effect_mut("opacity").unwrap();
    let mut m = Mask::new("o", MaskPath::rect(160.0, 0.0, 320.0, 180.0));
    m.feather.value = ParamValue::Float(0.0);
    op.masks.push(m);
    let img = render(&s);
    assert!(near(img.get(40, 90), [1.0, 0.0, 0.0], 1e-4), "{:?}", img.get(40, 90));
    assert!(near(img.get(280, 90), [1.0, 0.0, 0.0], 1e-4));
}

#[test]
fn a_transform_replaces_the_picture_below() {
    // Premiere: Transform on an adjustment layer moves the composite below; it is not a copy
    // drawn over the untouched original (#1524).
    let mut s = scene(2);
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    place(&mut s.p, s.seq, 0, red);
    let adj = adjustment(&mut s.p, s.seq, 1);
    fx(&mut s, adj, "transform");
    let tf = clip(&mut s, adj).effects.iter_mut().find(|e| e.effect == "transform").unwrap();
    tf.params.get_mut("scale_height").unwrap().value = ParamValue::Float(50.0);
    let img = render(&s);
    assert!(near(img.get(160, 90), [1.0, 0.0, 0.0], 1e-4), "{:?}", img.get(160, 90));
    assert_eq!(img.get(10, 10), [0.0; 4], "the original is gone outside the scaled picture");
    // at 50 % opacity: half the original, half the (empty) transformed picture
    clip(&mut s, adj).effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    let c = render(&s).get(10, 10);
    assert!((c[0] - 0.5).abs() < 1e-3 && (c[3] - 0.5).abs() < 1e-3, "{c:?}");
}

#[test]
fn nested_adjustment_layers_stay_in_their_sequence() {
    let mut s = scene(2);
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    let ocean = media(&mut s.p, &mut s.map, GeneratorSource::demo(DemoScene::OceanSunset));
    // inner sequence: red + adjustment (invert) → cyan
    let inner = s.p.new_sequence("inner", SequenceSettings { width: 320, height: 180, frame_rate: R, ..Default::default() }, 2, 0, None);
    place(&mut s.p, inner, 0, red);
    let a = s.p.add_item("Adjustment Layer", Label::Lavender, ItemKind::AdjustmentLayer { width: 320, height: 180, rate: R, duration: R.tick_of(48) }, None);
    let ac = place(&mut s.p, inner, 1, a);
    {
        let it = s.p.sequence_mut(inner).unwrap().find_item_mut(ac).unwrap().1;
        it.effects.insert(0, find_effect("invert").unwrap().instance());
    }
    // outer: ocean on V1, the nested sequence (scaled to 50 %) on V2
    place(&mut s.p, s.seq, 0, ocean);
    let nest = place(&mut s.p, s.seq, 1, inner);
    clip(&mut s, nest).effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(50.0);
    let img = render(&s);
    assert!(near(img.get(160, 90), [0.0, 1.0, 1.0], 1e-4), "inner adjusted: {:?}", img.get(160, 90));
    // the outer V1 (outside the nest) is untouched
    let plain = {
        let mut p2 = s.p.clone();
        p2.sequence_mut(s.seq).unwrap().video_tracks[1].items.clear();
        render_sequence(&p2, s.seq, R.tick_of(12), RenderOptions::default(), &s.map)
    };
    assert_eq!(img.get(5, 5), plain.get(5, 5));
}

#[test]
fn transform_shutter_on_an_adjustment_layer_spans_one_sequence_frame() {
    // A 60 fps sequence: at 360° the shutter is one 60 fps frame, so the frames just outside
    // the move stay sharp (they were blurred with a 24 fps shutter, #1088).
    use filmcraft_project::{Interpolation, Keyframe};
    let r60 = FrameRate::FPS_60;
    let mut s = scene(2);
    s.p.sequence_mut(s.seq).unwrap().settings.frame_rate = r60;
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    let below = place(&mut s.p, s.seq, 0, red);
    clip(&mut s, below).effect_mut("motion").unwrap().params.get_mut("scale").unwrap().value = ParamValue::Float(25.0);
    let adj = adjustment(&mut s.p, s.seq, 1);
    fx(&mut s, adj, "transform");
    let key = |sec: f64, x: f64| {
        let mut k = Keyframe::new(Tick::from_seconds_f64(sec), ParamValue::Vec2(filmcraft_geom::Vec2::new(x, 90.0)));
        k.interp = Interpolation::Linear;
        k
    };
    let e = clip(&mut s, adj).effect_mut("transform").unwrap();
    e.params.get_mut("shutter_angle").unwrap().value = ParamValue::Float(360.0);
    e.params.get_mut("position").unwrap().keyframes = vec![key(0.5, 160.0), key(1.0, 260.0)];
    let mut diff = |frame: i64| {
        let mut at = |shutter: bool| {
            clip(&mut s, adj).effect_mut("transform").unwrap().params.get_mut("shutter_override").unwrap().value = ParamValue::Bool(shutter);
            render_sequence(&s.p, s.seq, r60.tick_of(frame), RenderOptions::default(), &s.map)
        };
        let (sharp, blurred) = (at(false), at(true));
        sharp.px.iter().zip(&blurred.px).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max)
    };
    assert!(diff(29) < 1e-4, "frame before the move is blurred");
    assert!(diff(61) < 1e-4, "frame after the move is blurred");
    assert!(diff(45) > 1e-2, "moving frame is sharp");
}

fn dissolve(s: &mut Scene, track: usize, from: Option<ClipId>, to: Option<ClipId>, start: i64) {
    let tr = filmcraft_project::Transition {
        id: filmcraft_project::TransitionId(900 + start as u64),
        effect: find_effect("cross_dissolve").unwrap().instance(),
        start: R.tick_of(start),
        duration: R.tick_of(12),
        from,
        to,
        align: Default::default(),
        reverse: false,
    };
    s.p.sequence_mut(s.seq).unwrap().video_tracks[track].transitions.push(tr);
}

fn at(s: &Scene, frame: i64) -> [f32; 4] {
    render_sequence(&s.p, s.seq, R.tick_of(frame), RenderOptions::default(), &s.map).get(160, 90)
}

/// A dissolve at the start of an adjustment layer (Cmd+D) fades its effects in, and one at its
/// end fades them out, instead of leaving them off for the transition and cutting in after it.
#[test]
fn a_dissolve_fades_the_adjustment_layers_effects_in_and_out() {
    let mut s = scene(2);
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    place(&mut s.p, s.seq, 0, red);
    let adj = adjustment(&mut s.p, s.seq, 1);
    fx(&mut s, adj, "invert");
    dissolve(&mut s, 1, None, Some(adj), 0);
    dissolve(&mut s, 1, Some(adj), None, 36);
    // fade in over frames 0..12: red → halfway → cyan
    assert!(near(at(&s, 0), [1.0, 0.0, 0.0], 1e-3), "{:?}", at(&s, 0));
    assert!(near(at(&s, 3), [0.75, 0.25, 0.25], 1e-3), "a quarter in: {:?}", at(&s, 3));
    assert!(near(at(&s, 6), [0.5, 0.5, 0.5], 1e-3), "halfway in: {:?}", at(&s, 6));
    assert!(near(at(&s, 24), [0.0, 1.0, 1.0], 1e-3), "{:?}", at(&s, 24));
    // fade out over frames 36..48
    assert!(near(at(&s, 42), [0.5, 0.5, 0.5], 1e-3), "halfway out: {:?}", at(&s, 42));
    assert!(near(at(&s, 45), [0.75, 0.25, 0.25], 1e-3), "three quarters out: {:?}", at(&s, 45));
}

/// The layer's own opacity and blend mode still count during the fade.
#[test]
fn a_dissolve_on_an_adjustment_layer_keeps_its_opacity_and_blend_mode() {
    let mut s = scene(2);
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    place(&mut s.p, s.seq, 0, red);
    let adj = adjustment(&mut s.p, s.seq, 1);
    fx(&mut s, adj, "invert");
    clip(&mut s, adj).effect_mut("opacity").unwrap().params.get_mut("opacity").unwrap().value = ParamValue::Float(50.0);
    dissolve(&mut s, 1, None, Some(adj), 0);
    // halfway through the fade, at 50 % opacity: a quarter of the way to cyan
    assert!(near(at(&s, 6), [0.75, 0.25, 0.25], 1e-3), "{:?}", at(&s, 6));
    // Multiply (red × cyan = black) fades from red to black
    let op = clip(&mut s, adj).effect_mut("opacity").unwrap();
    op.params.get_mut("opacity").unwrap().value = ParamValue::Float(100.0);
    op.params.get_mut("blend").unwrap().value =
        ParamValue::Choice(filmcraft_project::effect::BLEND_MODES.iter().position(|b| *b == "Multiply").unwrap() as u32);
    assert!(near(at(&s, 6), [0.5, 0.0, 0.0], 1e-3), "{:?}", at(&s, 6));
    assert!(near(at(&s, 24), [0.0, 0.0, 0.0], 1e-3), "{:?}", at(&s, 24));
}

/// A frame where a transition touches an adjustment layer is composited on the CPU, which knows
/// how to fade one; the layered (GPU) plan does not.
#[test]
fn a_transition_with_an_adjustment_layer_is_not_planned_as_layers() {
    let mut s = scene(2);
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    let blue = matte(&mut s.p, &mut s.map, [0.0, 0.0, 1.0, 1.0]);
    place(&mut s.p, s.seq, 0, red);
    // V2: a blue clip from 0 to 24, then an adjustment layer from 24, a dissolve centred on the cut
    let b = place(&mut s.p, s.seq, 1, blue);
    clip(&mut s, b).duration = R.tick_of(24);
    let a = s.p.add_item("Adjustment Layer", Label::Lavender, ItemKind::AdjustmentLayer { width: 320, height: 180, rate: R, duration: R.tick_of(48) }, None);
    let mut ti = s.p.make_track_item(a, TrackKind::Video, R.tick_of(24), TimeRange::new(Tick::ZERO, R.tick_of(24)), R).unwrap();
    ti.effects.insert(0, find_effect("invert").unwrap().instance());
    let ac = ti.id;
    s.p.sequence_mut(s.seq).unwrap().video_tracks[1].items.push(ti);
    dissolve(&mut s, 1, Some(b), Some(ac), 18);
    let q = s.p.sequence(s.seq).unwrap();
    // frame 20 is still on the blue clip, but the dissolve into the adjustment layer shows
    assert!(!crate::plan::layered_at(&s.p, q, R.tick_of(20)));
    // halfway (frame 24): half blue, half the inverted red (cyan)
    assert!(near(at(&s, 24), [0.0, 0.5, 1.0], 1e-3), "{:?}", at(&s, 24));
}

/// The picture's bounds (in sequence pixels) and the share of the frame it covers.
fn coverage(img: &Image, scale: f32) -> ([f32; 4], f32) {
    let (mut b, mut sum) = ([f32::MAX, f32::MAX, f32::MIN, f32::MIN], 0.0);
    for y in 0..img.h {
        for x in 0..img.w {
            let a = img.get(x, y)[3];
            sum += a;
            if a > 0.5 {
                let (sx, sy) = (x as f32 / scale, y as f32 / scale);
                b = [b[0].min(sx), b[1].min(sy), b[2].max(sx), b[3].max(sy)];
            }
        }
    }
    (b, sum / (img.w * img.h) as f32)
}

/// Basic 3D's Distance to Image is in sequence pixels: zooming the Program monitor out (a lower
/// render scale) shrinks the picture, it does not push the plane further away (#1920).
#[test]
fn basic_3d_keeps_its_perspective_at_every_render_scale() {
    let mut s = scene(2);
    let red = matte(&mut s.p, &mut s.map, [1.0, 0.0, 0.0, 1.0]);
    place(&mut s.p, s.seq, 0, red);
    let adj = adjustment(&mut s.p, s.seq, 1);
    fx(&mut s, adj, "basic_3d");
    let b3 = clip(&mut s, adj).effects.iter_mut().find(|e| e.effect == "basic_3d").unwrap();
    for (id, v) in [("swivel", -12.0), ("tilt", -15.0), ("distance", 15.0)] {
        b3.params.get_mut(id).unwrap().value = ParamValue::Float(v);
    }
    let at = |scale: f32| coverage(&render_sequence(&s.p, s.seq, R.tick_of(12), RenderOptions { scale, ..Default::default() }, &s.map), scale);
    let (full, full_cov) = at(1.0);
    assert!(full_cov < 0.9, "the plane is turned and moved away: {full_cov}");
    for scale in [0.5, 0.25] {
        let (b, cov) = at(scale);
        assert!((cov - full_cov).abs() < 0.02, "at {scale}: covers {cov}, {full_cov} at full resolution");
        for k in 0..4 {
            assert!((b[k] - full[k]).abs() < 6.0, "at {scale}: bounds {b:?}, {full:?} at full resolution");
        }
    }
}
