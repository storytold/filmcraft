//! Lumetri Presets (Effects panel ▸ Lumetri Presets): built-in Lumetri Color looks, and their
//! thumbnails rendered by our own Lumetri on a generated preview picture.
//!
//! The presets are original work: parameter values chosen by FilmCraft contributors, named
//! descriptively (no third-party look names, LUTs or preview images). The preview picture
//! ([`preview_image`]) is procedural: a dusk sky with a sun, mountain ridges, a lake with its
//! reflection, a grey ramp down the left edge and six colour chips along the bottom, so a
//! thumbnail shows what a look does to sky, shadows, saturated colours and neutrals.

use filmcraft_color::srgb_to_linear;
use filmcraft_geom::Vec2;
use filmcraft_project::{EffectInstance, ParamValue, find_effect};
use filmcraft_time::Tick;

use crate::Image;
use crate::effects::{FxCtx, apply};

/// One preset: a Lumetri Color instance with these parameters (the rest at their defaults).
#[derive(Clone, Debug)]
pub struct LumetriPreset {
    /// Sub-folder of Lumetri Presets.
    pub folder: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub params: Vec<(&'static str, ParamValue)>,
}

impl LumetriPreset {
    /// A Lumetri Color effect instance configured as the preset.
    pub fn instance(&self) -> EffectInstance {
        let mut e = match find_effect("lumetri") {
            Some(def) => def.instance(),
            // A bare instance named after the effect; the preset's parameters are added below.
            None => EffectInstance {
                effect: "lumetri".into(),
                enabled: true,
                params: Default::default(),
                masks: Vec::new(),
                post_fader: false,
                essential: false,
                layer: None,
            },
        };
        for (k, v) in &self.params {
            if let Some(p) = e.params.get_mut(*k) {
                p.value = v.clone();
            }
        }
        e
    }
}

/// The preset folders in Effects panel order.
pub const FOLDERS: [&str; 4] = ["Cinematic", "Film Emulation", "Monochrome", "Technical"];

fn fl(v: f64) -> ParamValue {
    ParamValue::Float(v)
}
fn rgb(r: f32, g: f32, b: f32) -> ParamValue {
    ParamValue::Color([r, g, b, 1.0])
}
/// A colour-wheel offset towards hue `deg` (0 = red, 120 = green, 240 = blue) of length `len`.
fn wheel(deg: f64, len: f64) -> ParamValue {
    let a = deg.to_radians();
    ParamValue::Vec2(Vec2::new(a.cos() * len, a.sin() * len))
}
fn curve(points: &[[f32; 2]]) -> ParamValue {
    ParamValue::Curve(points.to_vec())
}

/// Every built-in preset, by folder.
pub fn presets() -> Vec<LumetriPreset> {
    let p = |folder, name, description, params| LumetriPreset { folder, name, description, params };
    vec![
        // ---- Cinematic
        p(
            "Cinematic",
            "Teal Shadows, Warm Skin",
            "Cyan-teal shadows and warm highlights with a little more contrast",
            vec![("shadow_tint", rgb(0.36, 0.52, 0.6)), ("highlight_tint", rgb(0.64, 0.53, 0.42)), ("contrast", fl(20.0)), ("creative_sat", fl(110.0))],
        ),
        p(
            "Cinematic",
            "Night Exterior",
            "Day for night: darker, cooler and less saturated",
            vec![("temperature", fl(-45.0)), ("exposure", fl(-0.9)), ("contrast", fl(15.0)), ("creative_sat", fl(70.0)), ("shadow_tint", rgb(0.4, 0.45, 0.62))],
        ),
        p(
            "Cinematic",
            "Golden Dusk",
            "Warm, soft late-afternoon light",
            vec![("temperature", fl(35.0)), ("tint", fl(8.0)), ("highlights", fl(-15.0)), ("vibrance", fl(20.0)), ("highlight_tint", rgb(0.66, 0.55, 0.4))],
        ),
        p("Cinematic", "Steel Blue", "Cool, desaturated and punchy", vec![("temperature", fl(-25.0)), ("saturation", fl(80.0)), ("contrast", fl(25.0))]),
        p(
            "Cinematic",
            "Hard Sun",
            "High contrast with held highlights and deep shadows",
            vec![("contrast", fl(45.0)), ("shadows", fl(-20.0)), ("highlights", fl(-25.0)), ("vibrance", fl(15.0))],
        ),
        // ---- Film Emulation
        p(
            "Film Emulation",
            "Faded Print",
            "Lifted blacks, softer contrast, muted colour",
            vec![("faded_film", fl(45.0)), ("contrast", fl(-10.0)), ("creative_sat", fl(85.0))],
        ),
        p(
            "Film Emulation",
            "Silver Retention",
            "Bleach-bypass style: desaturated with hard contrast",
            vec![("look", ParamValue::Choice(4)), ("look_intensity", fl(80.0))],
        ),
        p(
            "Film Emulation",
            "Cross Processed",
            "Yellow-green highlights and blue-lifted shadows from crossed channel curves",
            vec![
                ("curve_red", curve(&[[0.0, 0.0], [0.5, 0.56], [1.0, 1.0]])),
                ("curve_green", curve(&[[0.0, 0.02], [0.5, 0.54], [1.0, 0.98]])),
                ("curve_blue", curve(&[[0.0, 0.12], [1.0, 0.82]])),
            ],
        ),
        p("Film Emulation", "Warm Negative", "Warm print stock with gentle roll-off", vec![("look", ParamValue::Choice(2)), ("temperature", fl(10.0))]),
        // ---- Monochrome
        p("Monochrome", "Neutral Mono", "Black and white", vec![("saturation", fl(0.0))]),
        p(
            "Monochrome",
            "Hard Mono",
            "Black and white with crushed blacks and strong contrast",
            vec![("saturation", fl(0.0)), ("contrast", fl(60.0)), ("blacks", fl(-25.0))],
        ),
        p(
            "Monochrome",
            "Sepia Tone",
            "Monochrome toned warm brown",
            vec![("look", ParamValue::Choice(6)), ("wheel_midtones", wheel(30.0, 0.35)), ("wheel_highlights", wheel(45.0, 0.2))],
        ),
        p("Monochrome", "Cold Mono", "Monochrome toned cyan-blue", vec![("look", ParamValue::Choice(6)), ("wheel_midtones", wheel(210.0, 0.3))]),
        // ---- Technical
        p("Technical", "Lift Shadows", "Opens up the shadows", vec![("shadows", fl(40.0)), ("blacks", fl(10.0))]),
        p("Technical", "Protect Highlights", "Pulls highlights and whites down", vec![("highlights", fl(-40.0)), ("whites", fl(-15.0))]),
        p("Technical", "Half Saturation", "Saturation at 50 %", vec![("saturation", fl(50.0))]),
        p("Technical", "Boost Vibrance", "More colour in the less saturated areas", vec![("vibrance", fl(40.0))]),
        p(
            "Technical",
            "Legal Range Squeeze",
            "Maps full-range levels into 16–235 (video range) with a luma curve",
            vec![("curve_luma", curve(&[[0.0, 16.0 / 255.0], [1.0, 235.0 / 255.0]]))],
        ),
    ]
}

/// The preset called `name` (case-insensitive).
pub fn find(name: &str) -> Option<LumetriPreset> {
    presets().into_iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// The procedural preview picture (`w`×`h`, opaque, linear premultiplied like every [`Image`]).
pub fn preview_image(w: usize, h: usize) -> Image {
    let mut img = Image::new(w, h);
    let (wf, hf) = (w as f32, h as f32);
    let horizon = 0.56;
    let ramp_w = (wf * 0.05).max(2.0);
    let chips: [[f32; 3]; 6] = [[0.85, 0.12, 0.1], [0.95, 0.8, 0.1], [0.15, 0.7, 0.2], [0.1, 0.75, 0.8], [0.15, 0.25, 0.85], [0.8, 0.2, 0.75]];
    let chip_h = (hf * 0.12).max(2.0);
    let mix = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
    // mountain ridge heights (fraction of the height above the horizon), two layers
    let ridge = |u: f32, k: f32| 0.12 * k * (0.55 + 0.25 * (u * 7.3 + k).sin() + 0.15 * (u * 17.1 + 2.0 * k).sin() + 0.05 * (u * 41.0).sin());
    let sky = |v: f32| mix([0.98, 0.62, 0.32], [0.16, 0.3, 0.62], (1.0 - v / horizon).clamp(0.0, 1.0).powf(0.8));
    let (sun_x, sun_y, sun_r) = (0.68f32, 0.33f32, 0.07f32);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = ((x as f32 + 0.5) / wf, (y as f32 + 0.5) / hf);
            let mut c = if v < horizon {
                let mut c = sky(v);
                // the sun and its glow
                let d = (((u - sun_x) * wf / hf).powi(2) + (v - sun_y).powi(2)).sqrt();
                if d < sun_r {
                    c = [1.0, 0.93, 0.7];
                } else {
                    let g = (1.0 - (d - sun_r) / 0.25).clamp(0.0, 1.0).powi(2) * 0.35;
                    c = mix(c, [1.0, 0.85, 0.55], g);
                }
                // far ridge (blue-grey), near ridge (dark green-brown)
                if horizon - v < ridge(u, 1.6) {
                    c = mix([0.36, 0.4, 0.52], c, 0.2);
                }
                if horizon - v < ridge(u, 0.9) {
                    c = mix([0.16, 0.2, 0.12], [0.3, 0.28, 0.2], ((horizon - v) / 0.12).clamp(0.0, 1.0));
                }
                c
            } else {
                // the lake: a darker, bluer reflection of the sky, rippled
                let mirror = horizon - (v - horizon);
                let ripple = 0.04 * ((v * 140.0).sin() * (u * 9.0 + v * 30.0).cos());
                let r = sky(mirror.max(0.0));
                mix(mix(r, [0.05, 0.12, 0.22], 0.45 + (v - horizon) * 0.8), [0.0; 3], 0.1 + ripple)
            };
            // grey ramp down the left edge
            if (x as f32) < ramp_w {
                let g = 1.0 - v;
                c = [g, g, g];
            }
            // colour chips along the bottom
            if (y as f32) >= hf - chip_h && (x as f32) >= ramp_w {
                let k = (((x as f32 - ramp_w) / (wf - ramp_w)) * 6.0).floor().clamp(0.0, 5.0) as usize;
                c = chips[k];
            }
            let i = (y * w + x) * 4;
            img.px[i] = srgb_to_linear(c[0].clamp(0.0, 1.0));
            img.px[i + 1] = srgb_to_linear(c[1].clamp(0.0, 1.0));
            img.px[i + 2] = srgb_to_linear(c[2].clamp(0.0, 1.0));
            img.px[i + 3] = 1.0;
        }
    }
    img
}

/// A preset's thumbnail: the preview picture graded by the preset (Rec. 709).
pub fn thumbnail(preset: &LumetriPreset, w: usize, h: usize) -> crate::Result<Image> {
    let mut img = preview_image(w, h);
    let cx = FxCtx {
        t: Tick::ZERO,
        px_scale: w as f32 / 1920.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "",
        project: None,
        env: None,
        working: filmcraft_color::WorkingSpace::Rec709,
    };
    apply(&mut img, &preset.instance(), &cx)?;
    Ok(img)
}

/// A grid of thumbnails (`cols` columns of `w`×`h` cells, 4 px gaps over a dark background), in
/// the order given: the Effects panel's thumbnail view of a Lumetri Presets folder.
pub fn grid(presets: &[LumetriPreset], cols: usize, w: usize, h: usize) -> crate::Result<Image> {
    const GAP: usize = 4;
    let cols = cols.max(1);
    let rows = presets.len().div_ceil(cols).max(1);
    let (gw, gh) = (cols * (w + GAP) + GAP, rows * (h + GAP) + GAP);
    let bg = srgb_to_linear(0.11);
    let mut out = Image::filled(gw, gh, [bg, bg, bg, 1.0]);
    for (k, p) in presets.iter().enumerate() {
        let t = thumbnail(p, w, h)?;
        let (x0, y0) = (GAP + (k % cols) * (w + GAP), GAP + (k / cols) * (h + GAP));
        for y in 0..h {
            let src = &t.px[y * w * 4..(y + 1) * w * 4];
            let d = ((y0 + y) * gw + x0) * 4;
            out.px[d..d + w * 4].copy_from_slice(src);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid_and_change_the_picture() {
        let all = presets();
        assert!(all.len() >= 16);
        let mut names: Vec<&str> = all.iter().map(|p| p.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len(), "unique names");
        let def = find_effect("lumetri").unwrap();
        let base = preview_image(64, 36);
        for p in &all {
            assert!(FOLDERS.contains(&p.folder), "{}", p.name);
            for (k, _) in &p.params {
                assert!(def.param(k).is_some(), "{}: unknown param {k}", p.name);
            }
            let t = thumbnail(p, 64, 36).unwrap();
            let diff: f32 = t.px.iter().zip(&base.px).map(|(a, b)| (a - b).abs()).sum::<f32>() / base.px.len() as f32;
            assert!(diff > 0.004, "{} barely changes the picture ({diff})", p.name);
            assert!(t.px.iter().all(|v| v.is_finite()));
        }
        // Monochrome presets are monochrome (Sepia / Cold are toned: low but not zero chroma)
        let t = thumbnail(&find("Neutral Mono").unwrap(), 64, 36).unwrap();
        assert!(t.px.chunks(4).all(|p| (p[0] - p[1]).abs() < 1e-3 && (p[1] - p[2]).abs() < 1e-3));
        assert!(find("neutral mono").is_some() && find("nope").is_none());
    }

    #[test]
    fn preview_picture_has_ramp_chips_and_sky() {
        let img = preview_image(320, 180);
        let px = |x: usize, y: usize| img.get(x, y);
        assert!(px(4, 2)[0] > 0.9 && px(4, 177)[0] < 0.05, "grey ramp: white at the top, black at the bottom");
        let red = px(60, 176);
        assert!(red[0] > 0.5 && red[1] < 0.1, "a red chip: {red:?}");
        let sky = px(160, 10);
        assert!(sky[2] > sky[0], "blue sky at the top: {sky:?}");
        let grid = grid(&presets()[..5], 3, 32, 18).unwrap();
        assert_eq!((grid.w, grid.h), (3 * 36 + 4, 2 * 22 + 4));
    }
}
