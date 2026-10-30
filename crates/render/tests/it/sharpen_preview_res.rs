//! Sharpen must stay visible at reduced playback resolution (#1351).
//!
//! Playback at 1/2 renders with `px_scale` 0.5. Sharpen's blur radius scaled with it to a sigma the
//! three-box Gaussian rounds to zero-width boxes, so the unsharp mask subtracted the image from
//! itself and the effect vanished while playing, though it showed on the paused (Full) frame.

use filmcraft_project::{ParamValue, find_effect};
use filmcraft_render::effects::{FxCtx, apply};
use filmcraft_render::image::Image;
use filmcraft_time::Tick;

fn cx(px_scale: f32) -> FxCtx<'static> {
    FxCtx {
        t: Tick::ZERO,
        px_scale,
        seconds: 0.0,
        timecode: "00:00:01:00",
        clip_name: "x",
        project: None,
        env: None,
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
    }
}

/// Dark left half, light right half: the edge sits between x = 7 and x = 8.
fn edge() -> Image {
    let (w, h) = (16usize, 8usize);
    let mut img = Image::filled(w, h, [0.25, 0.25, 0.25, 1.0]);
    for y in 0..h {
        for x in w / 2..w {
            let i = (y * w + x) * 4;
            img.px[i..i + 3].copy_from_slice(&[0.75, 0.75, 0.75]);
        }
    }
    img
}

fn sharpened(px_scale: f32) -> Image {
    let mut img = edge();
    let mut e = find_effect("sharpen").expect("sharpen effect").instance();
    e.params.get_mut("amount").expect("amount").value = ParamValue::Float(100.0);
    apply(&mut img, &e, &cx(px_scale));
    img
}

#[test]
fn sharpen_shows_at_half_and_quarter_playback_resolution() {
    let plain = edge();
    for px_scale in [0.5, 0.25] {
        let img = sharpened(px_scale);
        let (dark, light) = (img.get(7, 4)[0], img.get(8, 4)[0]);
        assert!(dark < plain.get(7, 4)[0] - 0.01, "px_scale {px_scale}: the dark side of the edge should darken (got {dark})");
        assert!(light > plain.get(8, 4)[0] + 0.01, "px_scale {px_scale}: the light side of the edge should brighten (got {light})");
    }
}

#[test]
fn sharpen_at_full_resolution_is_unchanged() {
    // values rendered before the fix: the Full (paused) frame must not change
    let img = sharpened(1.0);
    for (x, want) in [(6, 0.25), (7, 0.25 - 1.0 / 6.0), (8, 0.75 + 1.0 / 6.0), (9, 0.75)] {
        let got = img.get(x, 4)[0];
        assert!((got - want).abs() < 1e-5, "x {x}: got {got}, want {want}");
    }
}
