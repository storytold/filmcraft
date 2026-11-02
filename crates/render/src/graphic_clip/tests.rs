use super::*;
use filmcraft_project::graphic::{new_shape_layer, new_text_layer};
use filmcraft_project::{ParamValue, find_effect};

fn set(e: &mut EffectInstance, k: &str, v: ParamValue) {
    e.params.get_mut(k).unwrap_or_else(|| panic!("{k}")).value = v;
}

/// A 5 s graphic clip holding `layers` (canvas 400×300 at 25 fps).
fn graphic_item(layers: Vec<EffectInstance>) -> TrackItem {
    let mut p = filmcraft_project::Project::new("t");
    let rate = filmcraft_time::FrameRate::FPS_25;
    let g = p.add_item("G", filmcraft_project::Label::Rose, filmcraft_project::ItemKind::Graphic { width: 400, height: 300, rate }, None);
    let mut it = p
        .make_track_item(g, filmcraft_project::TrackKind::Video, Tick::ZERO, filmcraft_time::TimeRange::new(Tick::ZERO, Tick::from_seconds_f64(5.0)), rate)
        .unwrap();
    it.effects.extend(layers);
    it
}

fn ink_box(img: &Image) -> Option<[usize; 4]> {
    let mut b: Option<[usize; 4]> = None;
    for y in 0..img.h {
        for x in 0..img.w {
            if img.get(x, y)[3] > 0.5 {
                b = Some(match b {
                    Some(a) => [a[0].min(x), a[1].min(y), a[2].max(x), a[3].max(y)],
                    None => [x, y, x, y],
                });
            }
        }
    }
    b
}

#[test]
fn per_character_fill_and_size() {
    use filmcraft_project::graphic_design::{CharStyle, LayerExtra, StyleRun};
    let mut e = new_text_layer("AAAA", Vec2::new(20.0, 100.0), 60.0);
    set(&mut e, "fill_color", ParamValue::Color([1.0, 1.0, 1.0, 1.0]));
    let red = CharStyle { fill: Some([1.0, 0.0, 0.0, 1.0]), ..Default::default() };
    e.layer = Some(Box::new(LayerExtra { runs: vec![StyleRun { start: 2, end: 4, style: red }], ..Default::default() }));
    let mut img = Image::new(400, 200);
    render_graphic_layers(&[e.clone()], Tick::ZERO, (400, 200), &Affine::IDENTITY, &mut img);
    let l = text_layout(match &eval_layer(&e, Tick::ZERO, (400, 200)).unwrap().content {
        LayerContent::Text(t) => t,
        _ => unreachable!(),
    });
    let split = 20.0 + l.caret(2).0;
    let (mut white, mut red_px) = (0, 0);
    for y in 0..200 {
        for x in 0..400 {
            let p = img.get(x, y);
            if p[3] > 0.9 {
                if p[1] > 0.9 {
                    white += 1;
                    assert!((x as f32) < split + 1.0, "white only in the first half at {x}");
                } else if p[0] > 0.9 && p[1] < 0.05 {
                    red_px += 1;
                    assert!((x as f32) > split - 1.0, "red only in the second half at {x}");
                }
            }
        }
    }
    assert!(white > 200 && red_px > 200, "{white} {red_px}");
    // a bigger run is taller and sits on the same baseline
    let big = CharStyle { size: Some(120.0), ..Default::default() };
    e.layer = Some(Box::new(LayerExtra { runs: vec![StyleRun { start: 3, end: 4, style: big }], ..Default::default() }));
    let mut img2 = Image::new(400, 200);
    render_graphic_layers(&[e], Tick::ZERO, (400, 200), &Affine::IDENTITY, &mut img2);
    let (b1, b2) = (ink_box(&img).unwrap(), ink_box(&img2).unwrap());
    assert!(b2[1] + 30 < b1[1], "the big letter rises higher: {b1:?} {b2:?}");
    assert!(b2[3].abs_diff(b1[3]) <= 1, "same baseline: {b1:?} {b2:?}");
}

#[test]
fn pinned_box_follows_text_growth() {
    use filmcraft_project::graphic_design::{LayerExtra, Pin, PinTarget};
    let mut t = new_text_layer("Hi", Vec2::new(50.0, 100.0), 40.0);
    t.layer = Some(Box::new(LayerExtra { uid: 1, ..Default::default() }));
    let mut bx = new_shape_layer(0, Vec2::new(60.0, 90.0), Vec2::new(40.0, 40.0), vec![]);
    let tb = layer_bounds(&eval_layer(&t, Tick::ZERO, (400, 300)).unwrap());
    let padded = [tb[0] - 10.0, tb[1] - 10.0, tb[2] + 10.0, tb[3] + 10.0];
    let pin = Pin::new(PinTarget::Layer(1), [true; 4], padded, tb);
    bx.layer = Some(Box::new(LayerExtra { uid: 2, pin: Some(pin), ..Default::default() }));
    for text in ["Hi", "Hello there, world"] {
        set(&mut t, "text", ParamValue::Text(text.into()));
        let specs = graphic_specs(&[bx.clone(), t.clone()], Tick::ZERO, (400, 300));
        let (b, tt) = (layer_bounds(&specs[0]), layer_bounds(&specs[1]));
        for k in 0..4 {
            let want = tt[k] + if k < 2 { -10.0 } else { 10.0 };
            assert!((b[k] - want).abs() < 1e-3, "{text}: edge {k}: {b:?} vs {tt:?}");
        }
    }
}

#[test]
fn roll_moves_the_whole_graphic() {
    use filmcraft_project::graphic_design::{GraphicMeta, Roll, RollMode};
    let e = new_shape_layer(0, Vec2::new(200.0, 150.0), Vec2::new(100.0, 100.0), vec![]);
    let mut item = graphic_item(vec![e]);
    item.graphic = Some(Box::new(GraphicMeta {
        roll: Roll { mode: RollMode::Roll, start_off_screen: true, end_off_screen: true, ..Default::default() },
        ..Default::default()
    }));
    let at = |item: &TrackItem, s: f64| {
        let sp = item_layer_specs(item, Tick::from_seconds_f64(s), (400, 300));
        layer_bounds(&sp[0].1)
    };
    // the box (y 100..200) starts with its top at the frame bottom and ends above the frame
    assert!((at(&item, 0.0)[1] - 300.0).abs() < 1e-6);
    assert!(at(&item, 5.0)[3].abs() < 1e-6);
    // half way: offset (200 + (-200)) / 2 = 0 → back where it was laid out
    assert!((at(&item, 2.5)[1] - 100.0).abs() < 1e-6, "{:?}", at(&item, 2.5));
    let mut img = Image::new(400, 300);
    render_graphic(&item, Tick::from_seconds_f64(2.5), (400, 300), &Affine::IDENTITY, &mut img);
    assert!(img.get(200, 150)[3] > 0.9, "centre at mid-roll");
    let mut img0 = Image::new(400, 300);
    render_graphic(&item, Tick::ZERO, (400, 300), &Affine::IDENTITY, &mut img0);
    assert!(ink_box(&img0).is_none(), "off screen at the start");
}

#[test]
fn text_layer_renders_white_text_at_position() {
    let e = new_text_layer("HELLO", Vec2::new(100.0, 150.0), 60.0);
    let mut img = Image::new(400, 200);
    render_graphic_layers(&[e], Tick::ZERO, (400, 200), &Affine::IDENTITY, &mut img);
    let ink: Vec<(usize, usize)> = (0..200).flat_map(|y| (0..400).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[3] > 0.5).collect();
    assert!(ink.len() > 500);
    let minx = ink.iter().map(|p| p.0).min().unwrap();
    let maxy = ink.iter().map(|p| p.1).max().unwrap();
    assert!((98..=106).contains(&minx), "starts at the position: {minx}");
    assert!((145..=152).contains(&maxy), "sits on the baseline: {maxy}");
    assert!(img.get(ink[0].0, ink[0].1)[0] > 0.4, "white");
}

#[test]
fn shape_appearance_layers() {
    let mut e = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(80.0, 40.0), vec![]);
    set(&mut e, "fill_color", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
    set(&mut e, "stroke", ParamValue::Bool(true));
    set(&mut e, "stroke_width", ParamValue::Float(6.0));
    set(&mut e, "stroke_color", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
    set(&mut e, "shadow", ParamValue::Bool(true));
    set(&mut e, "shadow_distance", ParamValue::Float(20.0));
    set(&mut e, "shadow_blur", ParamValue::Float(0.0));
    let mut img = Image::new(200, 200);
    render_graphic_layers(&[e], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
    let p = img.get(100, 100);
    assert!(p[2] > 0.9 && p[0] < 0.05, "blue fill {p:?}");
    let s = img.get(100, 78);
    assert!(s[0] > 0.9 && s[2] < 0.05, "red outer stroke above the box {s:?}");
    // shadow: below-right of the box, outside the stroke
    let sh = img.get(150, 135);
    assert!(sh[3] > 0.5 && sh[0] < 0.05 && sh[2] < 0.05, "black shadow {sh:?}");
    assert_eq!(img.get(20, 20)[3], 0.0);
}

#[test]
fn linear_gradient_runs_left_to_right_on_a_shape_and_on_text() {
    let mut shape = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(80.0, 40.0), vec![]);
    set(&mut shape, "fill_kind", ParamValue::Choice(1));
    set(&mut shape, "gradient_start", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
    set(&mut shape, "gradient_end", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
    set(&mut shape, "gradient_angle", ParamValue::Float(0.0));
    let mut img = Image::new(200, 200);
    render_graphic_layers(&[shape], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
    let left = img.get(70, 100);
    let right = img.get(130, 100);
    assert!(left[3] > 0.9 && left[0] > left[2], "redder on the left {left:?}");
    assert!(right[3] > 0.9 && right[2] > right[0], "bluer on the right {right:?}");

    let mut text = new_text_layer("HHHHHHHH", Vec2::new(16.0, 90.0), 64.0);
    set(&mut text, "fill_kind", ParamValue::Choice(1));
    set(&mut text, "gradient_start", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
    set(&mut text, "gradient_end", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
    let mut img = Image::new(500, 180);
    render_graphic_layers(&[text], Tick::ZERO, (500, 180), &Affine::IDENTITY, &mut img);
    let ink: Vec<(usize, usize)> = (0..img.h).flat_map(|y| (0..img.w).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[3] > 0.8).collect();
    let minx = ink.iter().map(|p| p.0).min().unwrap();
    let maxx = ink.iter().map(|p| p.0).max().unwrap();
    let span = (maxx - minx).max(1);
    let (mut ln, mut rn, mut ls, mut rs) = (0.0_f32, 0.0, 0.0, 0.0);
    for &(x, y) in &ink {
        let p = img.get(x, y)[0];
        if x < minx + span / 3 {
            ls += p;
            ln += 1.0;
        } else if x > maxx - span / 3 {
            rs += p;
            rn += 1.0;
        }
    }
    assert!(ln > 30.0 && rn > 30.0, "ink on both ends of the title: {ln} {rn} x {minx}..{maxx}");
    assert!(ls / ln > rs / rn + 0.15, "text ramps from red to blue: {} {}", ls / ln, rs / rn);
}

#[test]
fn linear_gradient_blends_in_linear_light_and_turns_with_the_layer() {
    let mut shape = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(80.0, 40.0), vec![]);
    set(&mut shape, "fill_kind", ParamValue::Choice(1));
    set(&mut shape, "gradient_start", ParamValue::Color([0.0, 0.0, 0.0, 1.0]));
    set(&mut shape, "gradient_end", ParamValue::Color([1.0, 1.0, 1.0, 1.0]));
    let mut img = Image::new(200, 200);
    render_graphic_layers(&[shape.clone()], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
    let mid = img.get(100, 100)[0];
    // Linear-light midpoint is ~0.5. An sRGB midpoint converted to linear is ~0.21.
    assert!(mid > 0.4 && mid < 0.6, "centre of black→white is linear 0.5, got {mid}");
    assert!(img.get(70, 100)[0] < mid && mid < img.get(130, 100)[0]);

    set(&mut shape, "gradient_start", ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
    set(&mut shape, "gradient_end", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
    set(&mut shape, "rotation", ParamValue::Float(90.0));
    let mut turned = Image::new(200, 200);
    render_graphic_layers(&[shape], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut turned);
    let top = turned.get(100, 70);
    let bot = turned.get(100, 130);
    assert!(top[3] > 0.9 && bot[3] > 0.9, "the rotated ramp is inside the shape: {top:?} {bot:?}");
    assert!(top[0] > top[2], "clockwise 90° carries the start colour to the top {top:?}");
    assert!(bot[2] > bot[0], "and the end colour to the bottom {bot:?}");
}

#[test]
fn linear_gradient_keeps_a_per_character_fill_solid() {
    use filmcraft_project::graphic_design::{CharStyle, LayerExtra, StyleRun};
    let mut e = new_text_layer("AAAA", Vec2::new(20.0, 100.0), 60.0);
    set(&mut e, "fill_kind", ParamValue::Choice(1));
    set(&mut e, "gradient_start", ParamValue::Color([0.0, 1.0, 0.0, 1.0]));
    set(&mut e, "gradient_end", ParamValue::Color([0.0, 0.0, 1.0, 1.0]));
    let red = CharStyle { fill: Some([1.0, 0.0, 0.0, 1.0]), ..Default::default() };
    e.layer = Some(Box::new(LayerExtra { runs: vec![StyleRun { start: 2, end: 4, style: red }], ..Default::default() }));
    let mut img = Image::new(400, 200);
    render_graphic_layers(&[e.clone()], Tick::ZERO, (400, 200), &Affine::IDENTITY, &mut img);
    let l = text_layout(match &eval_layer(&e, Tick::ZERO, (400, 200)).unwrap().content {
        LayerContent::Text(t) => t,
        _ => unreachable!(),
    });
    let split = 20.0 + l.caret(2).0;
    let (mut green, mut red_px) = (0, 0);
    for y in 0..200 {
        for x in 0..400 {
            let p = img.get(x, y);
            if p[3] < 0.8 {
                continue;
            }
            if (x as f32) < split - 1.0 && p[1] > p[0] && p[1] > p[2] {
                green += 1;
            } else if (x as f32) > split + 1.0 && p[0] > 0.9 && p[1] < 0.05 {
                red_px += 1;
            }
        }
    }
    assert!(green > 80 && red_px > 80, "gradient on the plain letters, solid red on the styled ones: {green} {red_px}");
}

#[test]
fn inner_and_centre_strokes() {
    for (kind, inside, outside) in [(2u32, true, false), (1, true, true)] {
        let mut e = new_shape_layer(2, Vec2::new(100.0, 100.0), Vec2::new(120.0, 120.0), vec![]);
        set(&mut e, "sides", ParamValue::Float(4.0));
        set(&mut e, "stroke", ParamValue::Bool(true));
        set(&mut e, "stroke_width", ParamValue::Float(6.0));
        set(&mut e, "stroke_type", ParamValue::Choice(kind));
        set(&mut e, "stroke_color", ParamValue::Color([0.0, 1.0, 0.0, 1.0]));
        let mut img = Image::new(200, 200);
        render_graphic_layers(&[e], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
        // diamond: top vertex at y = 40; probe straight below/above the right vertex (160, 100)
        let green = |x: usize| {
            let p = img.get(x, 100);
            p[1] > 0.9 && p[0] < 0.1
        };
        assert_eq!(green(157), inside, "kind {kind} inside");
        assert_eq!(green(161), outside, "kind {kind} outside");
        assert!(!green(120), "fill in the middle");
    }
}

#[test]
fn outer_stroke_without_fill_leaves_the_inside_empty() {
    let mut e = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(100.0, 100.0), vec![]);
    set(&mut e, "fill", ParamValue::Bool(false));
    set(&mut e, "stroke", ParamValue::Bool(true));
    set(&mut e, "stroke_width", ParamValue::Float(6.0));
    set(&mut e, "stroke_type", ParamValue::Choice(0));
    set(&mut e, "stroke_color", ParamValue::Color([0.0, 1.0, 0.0, 1.0]));
    let mut img = Image::new(200, 200);
    render_graphic_layers(&[e], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
    // rectangle spans 50..150: the ring is 150..156, everything left of the edge stays clear
    let p = img.get(153, 100);
    assert!(p[1] > 0.9 && p[0] < 0.1 && p[3] > 0.9, "ring is stroke-coloured: {p:?}");
    for x in [100, 140, 148] {
        assert_eq!(img.get(x, 100)[3], 0.0, "inside at x = {x}");
    }
}

#[test]
fn rotation_scale_and_opacity() {
    let mut e = new_shape_layer(0, Vec2::new(100.0, 100.0), Vec2::new(100.0, 10.0), vec![]);
    set(&mut e, "rotation", ParamValue::Float(90.0));
    set(&mut e, "opacity", ParamValue::Float(50.0));
    let mut img = Image::new(200, 200);
    render_graphic_layers(&[e.clone()], Tick::ZERO, (200, 200), &Affine::IDENTITY, &mut img);
    assert!((img.get(100, 60)[3] - 0.5).abs() < 0.02, "vertical after 90°");
    assert_eq!(img.get(60, 100)[3], 0.0);
    // a 2× output transform (e.g. Motion scale) draws vectors crisply at the larger size
    let mut big = Image::new(400, 400);
    render_graphic_layers(&[e], Tick::ZERO, (200, 200), &Affine::scale(2.0, 2.0), &mut big);
    assert!((big.get(200, 110)[3] - 0.5).abs() < 0.02);
    let q = layer_quad(&eval_layer(&find_effect("graphic_shape").unwrap().instance(), Tick::ZERO, (200, 200)).unwrap());
    assert!((q[0].x - (100.0 - 200.0)).abs() < 1e-6 && (q[2].y - (100.0 + 100.0)).abs() < 1e-6);
}

#[test]
fn tight_image_matches_full_render() {
    let a = new_text_layer("Tight", Vec2::new(50.0, 80.0), 40.0);
    let b = new_shape_layer(1, Vec2::new(150.0, 120.0), Vec2::new(60.0, 60.0), vec![]);
    let fx = vec![b, a];
    let mut full = Image::new(220, 160);
    let item = graphic_item(fx);
    render_graphic(&item, Tick::ZERO, (220, 160), &Affine::IDENTITY, &mut full);
    let (tight, x, y) = render_graphic_tight(&item, Tick::ZERO, (220, 160), &Affine::IDENTITY, 220, 160).unwrap();
    assert!(tight.w < 220 && tight.h < 160);
    for yy in 0..tight.h {
        for xx in 0..tight.w {
            let (p, q) = (tight.get(xx, yy), full.get(xx + x as usize, yy + y as usize));
            assert!((p[3] - q[3]).abs() < 1e-5);
        }
    }
}

/// #3361: Responsive Design – Time remaps the keyframes of the graphic's *layers* only. The clip's
/// own effects and masks (Opacity, Motion, ...) are read at plain media time, like on any clip,
/// so trimming a graphic with an outro moves a layer's outro animation with the clip end but not a
/// keyframe on the clip's own effect. Documented in docs/graphics.md.
#[test]
fn responsive_time_remaps_layer_keyframes_not_the_clips_own_effects() {
    use filmcraft_project::graphic_design::GraphicMeta;
    let sec = Tick::from_seconds_f64;
    let mut layer = new_shape_layer(0, Vec2::new(200.0, 150.0), Vec2::new(100.0, 100.0), vec![]);
    // layer opacity fades 100 → 0 over the designed outro (4 s .. 5 s)
    let p = layer.params.get_mut("opacity").expect("opacity");
    p.put_keyframe(sec(4.0), ParamValue::Float(100.0));
    p.put_keyframe(sec(5.0), ParamValue::Float(0.0));
    let mut item = graphic_item(vec![layer]);
    // the same fade on the clip's own (intrinsic) Opacity effect, at media times
    let own = item.effect_mut("opacity").expect("intrinsic opacity");
    let p = own.params.get_mut("opacity").expect("opacity");
    p.put_keyframe(sec(4.0), ParamValue::Float(100.0));
    p.put_keyframe(sec(5.0), ParamValue::Float(0.0));
    // designed 5 s with a 1 s outro, then trimmed to 3 s
    item.graphic = Some(Box::new(GraphicMeta { outro: sec(1.0), design_duration: sec(5.0), ..Default::default() }));
    item.duration = sec(3.0);
    // half way through the trimmed clip's outro (clip time 2.5 s of 3 s)
    let mt = sec(2.5);
    let (t, _, _) = graphic_time(&item, mt);
    assert_eq!(t, sec(4.5), "the layer reads its keyframes half way through the designed outro");
    let layer_opacity = item_layer_specs(&item, mt, (400, 300))[0].1.transform.opacity;
    assert!((layer_opacity - 0.5).abs() < 1e-4, "layer fade is half done: {layer_opacity}");
    // the clip's own Opacity is read at media time: 2.5 s is before its keyframes, so still 100 %
    assert_eq!(crate::opacity_blend(&item, item.effect_time_at(mt)).0, 1.0);
    // a head trim advances source_in; the clip's keyframes stay at their media times (4.5 s: half)
    item.source_in = sec(2.0);
    let own_at = |item: &TrackItem, clip_t: f64| crate::opacity_blend(item, item.effect_time_at(sec(clip_t))).0;
    assert!((own_at(&item, 2.5) - 0.5).abs() < 1e-4, "media 4.5 s: {}", own_at(&item, 2.5));
    assert!((own_at(&item, 0.0) - 1.0).abs() < 1e-4, "media 2 s: {}", own_at(&item, 0.0));
}
