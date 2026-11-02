//! Horizontal vs vertical graphic text with underline (letter A, 60 px).
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

fn ink_box(img: &Image) -> [usize; 4] {
    let mut b: Option<[usize; 4]> = None;
    for y in 0..img.h {
        for x in 0..img.w {
            if img.get(x, y)[3] > 0.4 {
                b = Some(match b {
                    Some(a) => [a[0].min(x), a[1].min(y), a[2].max(x), a[3].max(y)],
                    None => [x, y, x, y],
                });
            }
        }
    }
    b.expect("ink")
}

fn count(img: &Image) -> usize {
    (0..img.h).flat_map(|y| (0..img.w).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[3] > 0.4).count()
}

#[test]
fn underline_horizontal_vs_vertical_letter_a() {
    let pos = Vec2::new(80.0, 100.0);
    let mut h = new_text_layer("A", pos, 60.0);
    let h_plain = draw(h.clone());
    set(&mut h, "underline", ParamValue::Bool(true));
    let h_ul = draw(h);
    let mut v = new_vertical_text_layer("A", Vec2::new(200.0, 100.0), 60.0);
    let v_plain = draw(v.clone());
    set(&mut v, "underline", ParamValue::Bool(true));
    let v_ul = draw(v);

    assert!(count(&h_ul) > count(&h_plain) + 20, "horizontal underline adds ink");
    assert!(count(&v_ul) > count(&v_plain) + 20, "vertical underline adds ink");

    let hb = ink_box(&h_ul);
    let hp = ink_box(&h_plain);
    let vb = ink_box(&v_ul);
    let vp = ink_box(&v_plain);
    assert!(hb[3] > hp[3] + 1, "horizontal underline sits below the letter: plain {hp:?} ul {hb:?}");
    assert!(vb[2] > vp[2] + 1, "vertical underline sits to the right of the letter: plain {vp:?} ul {vb:?}");
}
