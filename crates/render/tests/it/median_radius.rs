//! Median Radius above 4 must not render as radius 4 at full resolution (`px_scale` 1).
//!
//! An 8-pixel-wide green stripe on black: window 9 (r=4) keeps the centre green; window 17 (r=8)
//! is majority black, so the stripe disappears. A 4-pixel cap would leave both centres green.

use filmcraft_project::{ParamValue, find_effect};
use filmcraft_render::effects::{FxCtx, apply};
use filmcraft_render::image::Image;
use filmcraft_time::Tick;

fn cx() -> FxCtx<'static> {
    FxCtx {
        t: Tick::ZERO,
        px_scale: 1.0,
        seconds: 0.0,
        timecode: "00:00:01:00",
        clip_name: "x",
        project: None,
        env: None,
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
    }
}

fn green_stripe() -> Image {
    let (w, h) = (33usize, 17usize);
    let mut img = Image::filled(w, h, [0.0, 0.0, 0.0, 1.0]);
    // 8-pixel-wide vertical stripe, centred.
    for y in 0..h {
        for x in 12..20 {
            let i = (y * w + x) * 4;
            img.px[i..i + 4].copy_from_slice(&[0.0, 1.0, 0.0, 1.0]);
        }
    }
    img
}

fn apply_radius(radius: f64) -> Image {
    let mut img = green_stripe();
    let mut e = find_effect("median").expect("median effect").instance();
    e.params.get_mut("radius").expect("radius").value = ParamValue::Float(radius);
    apply(&mut img, &e, &cx());
    img
}

#[test]
fn median_radius_8_is_stronger_than_4_at_full_res() {
    let r4 = apply_radius(4.0);
    let r8 = apply_radius(8.0);
    let cx_px = r4.w / 2;
    let cy = r4.h / 2;
    let g4 = r4.get(cx_px, cy)[1];
    let g8 = r8.get(cx_px, cy)[1];
    assert!(g4 > 0.9, "radius 4 should keep the 8px stripe (got g={g4})");
    assert!(g8 < 0.1, "radius 8 should erase the 8px stripe (got g={g8}); a 4px cap would leave it green");
}
