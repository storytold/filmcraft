//! Graphic text with strikethrough (letter o, 60 px): the stroke crosses the letter.
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::graphic::{new_text_layer, new_vertical_text_layer};
use filmcraft_project::{EffectInstance, ParamValue};
use filmcraft_render::graphic_clip::render_graphic_layers;
use filmcraft_render::image::Image;
use filmcraft_time::Tick;

fn set(e: &mut EffectInstance, k: &str, v: ParamValue) {
    e.params.get_mut(k).unwrap_or_else(|| panic!("{k}")).value = v;
}

fn draw(layer: EffectInstance) -> Image {
    let mut img = Image::new(320, 200);
    render_graphic_layers(&[layer], Tick::ZERO, (320, 200), &Affine::IDENTITY, &mut img);
    img
}

fn ink(img: &Image) -> Vec<(usize, usize)> {
    (0..img.h).flat_map(|y| (0..img.w).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[3] > 0.4).collect()
}

fn ink_box(img: &Image) -> [usize; 4] {
    ink(img).into_iter().fold([usize::MAX, usize::MAX, 0, 0], |b, (x, y)| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)])
}

#[test]
fn strikethrough_crosses_the_letter() {
    let mut h = new_text_layer("o", Vec2::new(80.0, 100.0), 60.0);
    let h_plain = draw(h.clone());
    set(&mut h, "strikethrough", ParamValue::Bool(true));
    let h_st = draw(h);
    let mut v = new_vertical_text_layer("o", Vec2::new(200.0, 100.0), 60.0);
    let v_plain = draw(v.clone());
    set(&mut v, "strikethrough", ParamValue::Bool(true));
    let v_st = draw(v);

    let (hp, hs) = (ink_box(&h_plain), ink_box(&h_st));
    assert!(ink(&h_st).len() > ink(&h_plain).len() + 20, "horizontal strikethrough adds ink");
    assert!(hs[1] >= hp[1] && hs[3] <= hp[3], "the stroke stays within the letter's height: plain {hp:?} struck {hs:?}");
    // the counter of the o is empty in its middle row; the stroke fills it
    let cx = (hp[0] + hp[2]) / 2;
    let row = (hp[1]..=hp[3]).filter(|&y| h_st.get(cx, y)[3] > 0.4 && h_plain.get(cx, y)[3] <= 0.4).count();
    assert!(row > 0, "ink inside the counter");

    let (vp, vs) = (ink_box(&v_plain), ink_box(&v_st));
    assert!(ink(&v_st).len() > ink(&v_plain).len() + 20, "vertical strikethrough adds ink");
    assert!(vs[0] >= vp[0] && vs[2] <= vp[2], "the vertical stroke stays within the letter's width: plain {vp:?} struck {vs:?}");
}
