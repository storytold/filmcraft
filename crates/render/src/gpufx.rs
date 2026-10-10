//! Standard effects with a GPU implementation (`filmcraft_gpu` runs them in WGSL).
//!
//! Each effect here is evaluated in two steps: [`FxOp::eval`] reads the effect's parameters at a
//! time (keyframes, defaults, `px_scale`, the working image size) into plain numbers, and
//! [`FxOp::apply`] runs the CPU reference on a working [`Image`]. `effects::apply` uses exactly
//! these two steps, so the CPU render and the GPU plan agree on every parameter (and on how
//! hostile values are clamped) by construction. The GPU compositor receives the evaluated
//! [`FxOp`]s in a [`crate::plan::LayerFx`] and reproduces `apply` per pixel; the plan only hands an
//! op to the GPU when [`FxOp::gpu_ok`] says the shader covers it (finite numbers, no minifying
//! resample), otherwise the clip is rendered on the CPU as before.

use filmcraft_color::{hsl_to_rgb, linear_to_srgb, luma709, rgb_to_hsl};
use filmcraft_geom::Affine;
use filmcraft_project::EffectInstance;
use rayon::prelude::*;

use crate::effects::{FxCtx, b, choice, color, dec, enc, f, gaussian_boxes, grade_space, on, point, text};
use crate::image::Image;

#[cfg(test)]
#[path = "keying_tests.rs"]
mod keying_tests;

/// A bilinear resample of the working image through an affine map (`Image::transformed` at a
/// magnification or mild minification: no mip pre-filter).
#[derive(Clone, Debug, PartialEq)]
pub struct Resample {
    /// Destination pixel → source pixel (None: singular map, the result is transparent).
    pub inv: Option<Affine>,
    /// Destination pixels written: x0..x1, y0..y1 (the rest becomes transparent).
    pub rect: [u32; 4],
    /// Alpha scale applied afterwards (Transform's Opacity; 1 = none).
    pub opacity: f32,
}

/// One evaluated effect.
#[derive(Clone, Debug, PartialEq)]
pub enum FxOp {
    Chain(Vec<FxOp>),
    Lut {
        lut: std::sync::Arc<filmcraft_color::Lut>,
    },
    Grade(Box<crate::grading::Grade>),
    /// HSL Secondary: key, denoise/blur, correction, display mode.
    Hsl {
        params: [f32; 12],
        output: u32,
        radius: u32,
        rx: Vec<u32>,
        ry: Vec<u32>,
    },
    BrightnessContrast {
        br: f32,
        co: f32,
    },
    ProcAmp {
        br: f32,
        co: f32,
        hue: f32,
        sat: f32,
    },
    Tint {
        black: [f32; 3],
        white: [f32; 3],
        amount: f32,
    },
    BlackWhite,
    ColorBalance {
        sh: [f32; 3],
        md: [f32; 3],
        hi: [f32; 3],
        preserve: bool,
    },
    LeaveColor {
        amount: f32,
        key_hue: f32,
        tol: f32,
        soft: f32,
    },
    ChangeToColor {
        from_hue: f32,
        to_hue: f32,
        tol: f32,
        soft: f32,
    },
    ColorPass {
        key: [f32; 3],
        sim: f32,
        reverse: bool,
    },
    /// Color Key: evaluated BT.709 key and matte thresholds.
    ChromaKey {
        key_ycc: [f32; 3],
        tolerance: f32,
        softness: f32,
        spill: f32,
        dominant: u32,
        output: u32,
    },
    LumaKey {
        threshold: f32,
        cutoff: f32,
    },
    /// Ultra Key without spatial choke/soften. Packed single-pixel matte, spill and correction
    /// parameters; layout is shared with `gpu::fx` / `fx.wgsl` (20 floats):
    /// p0 = key YCbCr, chroma magnitude²; p1 = tolerance, gain, pedestal, highlight;
    /// p2 = shadow, contrast, midpoint, spill; p3 = desaturate, range, spill luma, CC saturation;
    /// p4 = CC hue, CC luminance, unused, unused.
    UltraKey {
        params: [f32; 20],
        dominant: u32,
        output: u32,
    },
    Gamma {
        g: f32,
    },
    Levels {
        ib: f32,
        iw: f32,
        ob: f32,
        ow: f32,
        g: f32,
    },
    Extract {
        lo: f32,
        hi: f32,
        soft: f32,
        invert: bool,
    },
    Invert {
        channel: u32,
        blend: f32,
    },
    Posterize {
        n: f32,
    },
    /// Three box passes per axis approximating a Gaussian (radii per pass; empty: axis untouched).
    Gaussian {
        rx: Vec<u32>,
        ry: Vec<u32>,
        repeat: bool,
    },
    /// `steps` bilinear (edge-clamped) taps along `(dx, dy)` centred on the pixel.
    DirectionalBlur {
        dx: f32,
        dy: f32,
        steps: u32,
    },
    /// Unsharp mask over a repeat-edge Gaussian of the given box radii (per axis).
    Unsharp {
        rx: Vec<u32>,
        ry: Vec<u32>,
        amount: f32,
        threshold: f32,
    },
    /// Crop / Edge Feather: alpha falloff `feather` px inside the rectangle (0: half-pixel AA).
    Crop {
        x0: f32,
        x1: f32,
        y0: f32,
        y1: f32,
        feather: f32,
    },
    Resample(Resample),
    HFlip,
    VFlip,
    Mirror {
        cx: f32,
        cy: f32,
        nx: f32,
        ny: f32,
    },
    /// Wrap-around shift (dx, dy already reduced into 0..w, 0..h), mixed with the original.
    Offset {
        dx: f32,
        dy: f32,
        blend: f32,
    },
    /// ASC CDL on display-encoded colour (slope, offset, power, saturation).
    AscCdl {
        slope: [f32; 3],
        offset: [f32; 3],
        power: [f32; 3],
        sat: f32,
    },
    /// Channel Mixer: rows (r, g, b, constant) per output channel, on display-encoded colour.
    ChannelMix {
        m: [[f32; 4]; 3],
    },
    /// Color Replace: `sim` already ×1.2; `replace_hsl` the HSL of the replacement colour.
    ColorReplace {
        sim: f32,
        solid: bool,
        target: [f32; 3],
        replace: [f32; 3],
        replace_hsl: [f32; 3],
    },
    AlphaAdjust {
        opacity: f32,
        ignore: bool,
        invert: bool,
        mask_only: bool,
    },
    Vignette {
        amount: f32,
        midpoint: f32,
        roundness: f32,
        feather: f32,
        target: [f32; 3],
    },
    VideoLimiter {
        max: f32,
        comp: f32,
        axis: u32,
        warn: bool,
        warning_color: [f32; 3],
    },
    /// Effect-local masks, evaluated in working-image coordinates.
    Masked {
        op: Box<FxOp>,
        masks: Vec<crate::mask::FlatMask>,
    },
    OpacityMask {
        masks: Vec<crate::mask::FlatMask>,
    },
    Lumetri {
        gains: [f32; 3],
        exposure: f32,
        contrast: f32,
        hl: f32,
        sh: f32,
        wh: f32,
        bl: f32,
        sat: f32,
        creative_on: bool,
        faded: f32,
        vib: f32,
        st: [f32; 3],
        ht: [f32; 3],
        vignette_on: bool,
        va: f32,
        vmid: f32,
        vround: f32,
        vfeather: f32,
        aspect: f32,
        gpu_capable: bool,
    },
}

/// Effect ids [`FxOp::eval`] understands (the GPU-capable standard effects).
pub const GPU_EFFECTS: &[&str] = &[
    "brightness_contrast",
    "proc_amp",
    "tint",
    "black_white",
    "color_balance",
    "leave_color",
    "change_to_color",
    "color_pass",
    "gamma_correction",
    "levels",
    "extract",
    "invert",
    "posterize",
    "gaussian_blur",
    "gaussian_blur_legacy",
    "camera_blur",
    "directional_blur",
    "directional_blur_legacy",
    "sharpen",
    "unsharp_mask",
    "crop",
    "edge_feather",
    "transform",
    "horizontal_flip",
    "vertical_flip",
    "mirror",
    "offset",
    "asc_cdl",
    "channel_mix",
    "color_replace",
    "alpha_adjust",
    "vignette",
    "video_limiter",
    "lumetri",
    "ultra_key",
    "color_key",
    "luma_key",
];

/// `Image::transformed` without the mip path: the destination rectangle it writes and the inverse.
fn resample(w: usize, h: usize, m: &Affine, opacity: f32) -> Resample {
    let Some(inv) = m.inverse() else { return Resample { inv: None, rect: [0; 4], opacity } };
    let b = m.bounds(&filmcraft_geom::Rect::new(0.0, 0.0, w as f64, h as f64));
    let y0 = (b.y.floor().max(0.0) as usize).min(h);
    let y1 = (b.bottom().ceil().max(0.0) as usize).min(h);
    let x0 = (b.x.floor().max(0.0) as usize).min(w);
    let x1 = (b.right().ceil().max(0.0) as usize).min(w);
    Resample { inv: Some(inv), rect: [x0 as u32, x1 as u32, y0.min(y1) as u32, y1 as u32], opacity }
}

/// Whether `Image::transformed` would take its mip (downsample) path for `m` on a `w`×`h` image.
fn minifies(m: &Affine, w: usize, h: usize) -> bool {
    let sx = (m.a * m.a + m.b * m.b).sqrt();
    let sy = (m.c * m.c + m.d * m.d).sqrt();
    let minify = 1.0 / sx.min(sy).max(1e-6);
    // (a non-finite factor: keep such maps off the GPU; `apply` still hands them to the CPU's
    // `Image::transformed`, which decides as before)
    !minify.is_finite() || minify >= 2.0 && w >= 4 && h >= 4
}

impl FxOp {
    /// Evaluate `e` at `cx.t` for a `w`×`h` working image. None for effects without a GPU
    /// implementation (and for disabled effects).
    pub fn eval(e: &EffectInstance, cx: &FxCtx, w: usize, h: usize) -> Option<FxOp> {
        if !e.enabled {
            return None;
        }
        Some(match e.effect.as_str() {
            "ultra_key" => {
                use crate::vfx::{chv, cv, fv};
                // Spatial matte operations still use the complete CPU implementation.
                if fv(e, "choke", cx) != 0.0 || fv(e, "soften", cx) != 0.0 {
                    return None;
                }
                let kc = cv(e, "key_color", cx);
                let k = filmcraft_color::rgb_to_ycbcr(kc[0], kc[1], kc[2], filmcraft_color::Matrix::Bt709);
                let (gm, pm) = match chv(e, "setting") {
                    1 => (0.85, 0.7),
                    2 => (1.2, 1.5),
                    _ => (1.0, 1.0),
                };
                let dominant = if kc[1] >= kc[0] && kc[1] >= kc[2] {
                    1
                } else if kc[2] >= kc[0] {
                    2
                } else {
                    0
                };
                FxOp::UltraKey {
                    params: [
                        k[0],
                        k[1],
                        k[2],
                        (k[1] * k[1] + k[2] * k[2]).max(1e-6),
                        0.08 + fv(e, "tolerance", cx) / 100.0 * 0.5,
                        (1.0 + 2.0 * (fv(e, "transparency", cx) / 100.0)) * gm,
                        (fv(e, "pedestal", cx) / 100.0 * 0.3 * pm).min(0.9),
                        fv(e, "highlight", cx) / 100.0,
                        fv(e, "shadow", cx) / 100.0,
                        fv(e, "contrast", cx) / 100.0,
                        fv(e, "mid_point", cx) / 100.0,
                        fv(e, "spill", cx) / 100.0,
                        fv(e, "desaturate", cx) / 100.0,
                        fv(e, "range", cx) / 100.0,
                        fv(e, "spill_luma", cx) / 100.0,
                        fv(e, "cc_saturation", cx) / 100.0,
                        fv(e, "cc_hue", cx) / 360.0,
                        fv(e, "cc_luminance", cx) / 100.0,
                        0.0,
                        0.0,
                    ],
                    dominant,
                    output: chv(e, "output"),
                }
            }
            "color_key" => {
                let kc = color(e, "color", cx);
                let key_ycc = filmcraft_color::rgb_to_ycbcr(kc[0], kc[1], kc[2], filmcraft_color::Matrix::Bt709);
                let dominant = if kc[1] >= kc[0] && kc[1] >= kc[2] {
                    1
                } else if kc[2] >= kc[0] {
                    2
                } else {
                    0
                };
                FxOp::ChromaKey {
                    key_ycc,
                    tolerance: f(e, "tolerance", cx) / 255.0 * 0.6 + 0.01,
                    softness: f(e, "feather", cx) / 50.0 * 0.2 + 0.01,
                    spill: 0.0,
                    dominant,
                    output: 0,
                }
            }
            "luma_key" => FxOp::LumaKey { threshold: f(e, "threshold", cx) / 100.0, cutoff: f(e, "cutoff", cx) / 100.0 },
            "brightness_contrast" => FxOp::BrightnessContrast { br: f(e, "brightness", cx) / 100.0 * 0.4, co: 1.0 + f(e, "contrast", cx) / 100.0 },
            "proc_amp" => FxOp::ProcAmp {
                br: f(e, "brightness", cx) / 100.0 * 0.4,
                co: f(e, "contrast", cx) / 100.0,
                hue: f(e, "hue", cx) / 360.0,
                sat: f(e, "saturation", cx) / 100.0,
            },
            "tint" => {
                let (bl, wh) = (color(e, "black", cx), color(e, "white", cx));
                FxOp::Tint { black: [bl[0], bl[1], bl[2]], white: [wh[0], wh[1], wh[2]], amount: f(e, "amount", cx) / 100.0 }
            }
            "black_white" => FxOp::BlackWhite,
            "color_balance" => {
                let g = |k: &str| f(e, k, cx) / 100.0 * 0.25;
                FxOp::ColorBalance {
                    sh: [g("shadow_r"), g("shadow_g"), g("shadow_b")],
                    md: [g("mid_r"), g("mid_g"), g("mid_b")],
                    hi: [g("hi_r"), g("hi_g"), g("hi_b")],
                    preserve: b(e, "preserve"),
                }
            }
            "leave_color" => {
                let key = color(e, "color", cx);
                FxOp::LeaveColor {
                    amount: f(e, "amount", cx) / 100.0,
                    key_hue: rgb_to_hsl(key[0], key[1], key[2])[0],
                    tol: f(e, "tolerance", cx) / 100.0,
                    soft: f(e, "softness", cx) / 100.0 + 1e-4,
                }
            }
            "change_to_color" => {
                let (from, to) = (color(e, "from", cx), color(e, "to", cx));
                FxOp::ChangeToColor {
                    from_hue: rgb_to_hsl(from[0], from[1], from[2])[0],
                    to_hue: rgb_to_hsl(to[0], to[1], to[2])[0],
                    tol: f(e, "hue_tol", cx) / 100.0,
                    soft: f(e, "softness", cx) / 100.0 * 0.3 + 1e-4,
                }
            }
            "color_pass" => {
                let key = color(e, "color", cx);
                FxOp::ColorPass { key: [key[0], key[1], key[2]], sim: f(e, "similarity", cx) / 100.0, reverse: b(e, "reverse") }
            }
            "gamma_correction" => FxOp::Gamma { g: f(e, "gamma", cx) / 10.0 },
            "levels" => {
                let ib = f(e, "in_black", cx) / 255.0;
                FxOp::Levels {
                    ib,
                    iw: (f(e, "in_white", cx) / 255.0).max(ib + 1e-3),
                    ob: f(e, "out_black", cx) / 255.0,
                    ow: f(e, "out_white", cx) / 255.0,
                    g: 100.0 / f(e, "gamma", cx).max(1.0),
                }
            }
            "extract" => FxOp::Extract {
                lo: f(e, "black", cx) / 255.0,
                hi: f(e, "white", cx) / 255.0,
                soft: f(e, "softness", cx) / 100.0 * 0.2 + 1e-4,
                invert: b(e, "invert"),
            },
            "invert" => FxOp::Invert { channel: choice(e, "channel"), blend: f(e, "blend", cx) / 100.0 },
            "posterize" => FxOp::Posterize { n: f(e, "levels", cx).max(2.0) - 1.0 },
            "gaussian_blur" | "gaussian_blur_legacy" => {
                let r = f(e, "blurriness", cx) * cx.px_scale * 0.5;
                let dims = choice(e, "dimensions");
                let (rx, ry) = gaussian_boxes(w, h, if dims == 2 { 0.0 } else { r }, if dims == 1 { 0.0 } else { r });
                FxOp::Gaussian { rx, ry, repeat: b(e, "repeat_edge") }
            }
            "camera_blur" => {
                let r = f(e, "percent", cx) * cx.px_scale * 0.3;
                let (rx, ry) = gaussian_boxes(w, h, r, r);
                FxOp::Gaussian { rx, ry, repeat: true }
            }
            "directional_blur" | "directional_blur_legacy" => {
                let len = f(e, "length", cx) * cx.px_scale * 2.0;
                let dir = (f(e, "direction", cx) as f64).to_radians();
                if len < 0.5 {
                    FxOp::DirectionalBlur { dx: 0.0, dy: 0.0, steps: 0 }
                } else {
                    let steps = (len.ceil() as usize).clamp(2, 64) as u32;
                    FxOp::DirectionalBlur { dx: (dir.sin() as f32) * len, dy: (-dir.cos() as f32) * len, steps }
                }
            }
            "sharpen" => {
                let r = cx.px_scale.max(0.35);
                let (rx, ry) = gaussian_boxes(w, h, r, r);
                FxOp::Unsharp { rx, ry, amount: f(e, "amount", cx) / 100.0, threshold: 0.0 }
            }
            "unsharp_mask" => {
                let r = f(e, "radius", cx) * cx.px_scale;
                let (rx, ry) = gaussian_boxes(w, h, r, r);
                FxOp::Unsharp { rx, ry, amount: f(e, "amount", cx) / 100.0, threshold: f(e, "threshold", cx) / 255.0 }
            }
            "crop" => {
                let l = f(e, "left", cx) / 100.0;
                let t = f(e, "top", cx) / 100.0;
                let r = f(e, "right", cx) / 100.0;
                let bt = f(e, "bottom", cx) / 100.0;
                if b(e, "zoom") && l + r < 0.99 && t + bt < 0.99 {
                    let (fw, fh) = (w as f64, h as f64);
                    let m = Affine::scale(1.0 / (1.0 - (l + r) as f64), 1.0 / (1.0 - (t + bt) as f64))
                        .then_apply(&Affine::translate(-(l as f64) * fw, -(t as f64) * fh));
                    if minifies(&m, w, h) {
                        // negative crops zoom out: the CPU's mip path (see `transform`)
                        return Some(FxOp::Resample(Resample { inv: Some(m), rect: [u32::MAX; 4], opacity: 1.0 }));
                    }
                    FxOp::Resample(resample(w, h, &m, 1.0))
                } else {
                    FxOp::crop_rect(w, h, l, t, r, bt, f(e, "feather", cx) * cx.px_scale)
                }
            }
            "edge_feather" => {
                let amt = f(e, "amount", cx) / 100.0 * (w.min(h) as f32) * 0.5;
                FxOp::crop_rect(w, h, 0.0, 0.0, 0.0, 0.0, amt)
            }
            "transform" => {
                let img = Image { w, h, px: Vec::new() };
                let anchor = point(e, "anchor", cx, &img);
                let pos = point(e, "position", cx, &img);
                let sh = f(e, "scale_height", cx) as f64 / 100.0;
                let sw = if b(e, "uniform_scale") { sh } else { f(e, "scale_width", cx) as f64 / 100.0 };
                let rot = f(e, "rotation", cx) as f64;
                let skew = (f(e, "skew", cx) as f64).to_radians().tan();
                let skew_axis = f(e, "skew_axis", cx) as f64;
                let op = f(e, "opacity", cx) / 100.0;
                let sk = Affine::rotate_deg(skew_axis)
                    .then_apply(&Affine { a: 1.0, b: 0.0, c: skew, d: 1.0, e: 0.0, f: 0.0 })
                    .then_apply(&Affine::rotate_deg(-skew_axis));
                let m = Affine::translate(pos.x, pos.y)
                    .then_apply(&Affine::rotate_deg(rot))
                    .then_apply(&sk)
                    .then_apply(&Affine::scale(sw, sh))
                    .then_apply(&Affine::translate(-anchor.x, -anchor.y));
                if minifies(&m, w, h) {
                    // the CPU's mip path (`Image::transformed` with the forward map; `rect`
                    // u32::MAX marks it): CPU only
                    return Some(FxOp::Resample(Resample { inv: Some(m), rect: [u32::MAX; 4], opacity: op }));
                }
                FxOp::Resample(resample(w, h, &m, op))
            }
            "horizontal_flip" => FxOp::HFlip,
            "vertical_flip" => FxOp::VFlip,
            "mirror" => {
                let img = Image { w, h, px: Vec::new() };
                let c = point(e, "center", cx, &img);
                let ang = (f(e, "angle", cx) as f64).to_radians();
                FxOp::Mirror { cx: c.x as f32, cy: c.y as f32, nx: ang.cos() as f32, ny: ang.sin() as f32 }
            }
            "offset" => {
                let img = Image { w, h, px: Vec::new() };
                let s = point(e, "shift", cx, &img);
                let (dx, dy) = (s.x - w as f64 / 2.0, s.y - h as f64 / 2.0);
                FxOp::Offset { dx: dx.rem_euclid(w.max(1) as f64) as f32, dy: dy.rem_euclid(h.max(1) as f64) as f32, blend: f(e, "blend", cx) / 100.0 }
            }
            "asc_cdl" => {
                use crate::vfx::fv;
                FxOp::AscCdl {
                    slope: [fv(e, "r_slope", cx), fv(e, "g_slope", cx), fv(e, "b_slope", cx)],
                    offset: [fv(e, "r_offset", cx), fv(e, "g_offset", cx), fv(e, "b_offset", cx)],
                    power: [fv(e, "r_power", cx), fv(e, "g_power", cx), fv(e, "b_power", cx)],
                    sat: fv(e, "saturation", cx),
                }
            }
            "channel_mix" => {
                use crate::vfx::{bv, fv};
                let g = |k: &str| fv(e, k, cx) / 100.0;
                let mut m = [[g("rr"), g("rg"), g("rb"), g("rc")], [g("gr"), g("gg"), g("gb"), g("gc")], [g("br"), g("bg"), g("bb"), g("bc")]];
                if bv(e, "monochrome") {
                    m = [m[0]; 3];
                }
                FxOp::ChannelMix { m }
            }
            "color_replace" => {
                use crate::vfx::{bv, cv, fv};
                let t = cv(e, "target", cx);
                let r = cv(e, "replace", cx);
                FxOp::ColorReplace {
                    sim: fv(e, "similarity", cx) / 100.0 * 1.2,
                    solid: bv(e, "solid"),
                    target: [t[0], t[1], t[2]],
                    replace: [r[0], r[1], r[2]],
                    replace_hsl: rgb_to_hsl(r[0], r[1], r[2]),
                }
            }
            "alpha_adjust" => {
                use crate::vfx::{bv, fv};
                FxOp::AlphaAdjust { opacity: fv(e, "opacity", cx) / 100.0, ignore: bv(e, "ignore"), invert: bv(e, "invert"), mask_only: bv(e, "mask_only") }
            }
            "vignette" => {
                use crate::vfx::{cv, fv};
                let amt = fv(e, "amount", cx) / 100.0;
                let mid = fv(e, "midpoint", cx) / 100.0;
                let round = fv(e, "roundness", cx) / 100.0;
                let raw_feather = fv(e, "feather", cx);
                let feather = if raw_feather.is_finite() { (raw_feather / 100.0).max(0.01) } else { raw_feather };
                let col = cv(e, "color", cx);
                let target = if amt < 0.0 { [col[0], col[1], col[2]] } else { [1.0; 3] };
                FxOp::Vignette { amount: amt, midpoint: mid, roundness: round, feather, target }
            }
            "video_limiter" => {
                use crate::vfx::{bv, cv};
                use filmcraft_project::ParamValue;
                let clip_level = match e.param("clip_level").map(|p| p.value.clone()).or_else(|| crate::vfx::def_value(e, "clip_level")) {
                    Some(ParamValue::Choice(c)) => c as f32,
                    Some(ParamValue::Float(f)) => f as f32,
                    _ => 0.0,
                };
                let comp_val = match e.param("compression").map(|p| p.value.clone()).or_else(|| crate::vfx::def_value(e, "compression")) {
                    Some(ParamValue::Choice(c)) => [0.0, 0.03, 0.05, 0.10, 0.20][(c as usize).min(4)],
                    Some(ParamValue::Float(f)) => f as f32,
                    _ => 0.03,
                };
                let axis = match e.param("axis").map(|p| p.value.clone()).or_else(|| crate::vfx::def_value(e, "axis")) {
                    Some(ParamValue::Choice(c)) => c,
                    Some(ParamValue::Float(f)) => {
                        if f.is_nan() {
                            u32::MAX
                        } else {
                            f as u32
                        }
                    }
                    _ => 3,
                };
                let warn = bv(e, "gamut_warning");
                let c = cv(e, "warning_color", cx);
                let wc = dec([c[0], c[1], c[2]]);
                FxOp::VideoLimiter { max: 1.0 + clip_level / 100.0, comp: comp_val, axis, warn, warning_color: wc }
            }
            "lumetri" => {
                let (basic_on, creative_on, vignette_on) = (on(e, "basic_on"), on(e, "creative_on"), on(e, "vignette_on"));
                let is_hdr = grade_space(e, cx, "hdr_white").is_hdr();
                let input_lut = if basic_on { crate::luts::resolve(cx.project, text(e, "input_lut")) } else { None };
                let sharpen = if creative_on { f(e, "sharpen", cx) / 100.0 } else { 0.0 };
                let hsl_on = b(e, "hsl_on");
                let gpu_capable = !is_hdr;
                let bf = |id: &str| if basic_on { f(e, id, cx) } else { 0.0 };
                let temp = bf("temperature") / 100.0;
                let tint = bf("tint") / 100.0;
                let exposure = 2f32.powf(bf("exposure"));
                let contrast = bf("contrast") / 100.0;
                let hl = bf("highlights") / 100.0;
                let sh = bf("shadows") / 100.0;
                let wh = bf("whites") / 100.0;
                let bl = bf("blacks") / 100.0;
                let sat = if basic_on { f(e, "saturation", cx) / 100.0 } else { 1.0 } * if creative_on { f(e, "creative_sat", cx) / 100.0 } else { 1.0 };
                let vib = if creative_on { f(e, "vibrance", cx) / 100.0 } else { 0.0 };
                let faded = if creative_on { f(e, "faded_film", cx) / 100.0 } else { 0.0 };
                let st = color(e, "shadow_tint", cx);
                let ht = color(e, "highlight_tint", cx);
                let va = if vignette_on { f(e, "vignette_amount", cx) } else { 0.0 };
                let vmid = f(e, "vignette_midpoint", cx) / 100.0;
                let vround = f(e, "vignette_roundness", cx) / 100.0;
                let vfeather = f(e, "vignette_feather", cx) / 100.0;
                let gains = [1.0 + 0.35 * temp, 1.0 - 0.3 * tint, 1.0 - 0.35 * temp];
                let aspect = if h > 0 { (w as f32 / h as f32).max(1e-4) } else { 1.0 };

                let base = FxOp::Lumetri {
                    gains,
                    exposure,
                    contrast,
                    hl,
                    sh,
                    wh,
                    bl,
                    sat,
                    creative_on,
                    faded,
                    vib,
                    st: [st[0], st[1], st[2]],
                    ht: [ht[0], ht[1], ht[2]],
                    vignette_on,
                    va,
                    vmid,
                    vround,
                    vfeather,
                    aspect,
                    gpu_capable,
                };
                if !gpu_capable {
                    return Some(base);
                }
                let mut chain = Vec::new();
                if let Some(lut) = input_lut {
                    chain.push(FxOp::Lut { lut });
                }
                chain.push(base);
                if let Some(grade) = crate::grading::Grade::eval(e, cx) {
                    chain.push(FxOp::Grade(Box::new(grade)));
                }
                if hsl_on {
                    let params = [
                        f(e, "hsl_hue", cx) / 360.0,
                        (f(e, "hsl_hue_range", cx) / 360.0).max(1e-3),
                        f(e, "hsl_sat_min", cx) / 100.0,
                        f(e, "hsl_luma_min", cx) / 100.0,
                        f(e, "hsl_luma_max", cx) / 100.0,
                        (f(e, "hsl_soft", cx) / 100.0 * 0.3).max(0.01),
                        f(e, "hsl_temp", cx) / 100.0,
                        f(e, "hsl_tint", cx) / 100.0,
                        f(e, "hsl_sat", cx) / 100.0,
                        f(e, "hsl_hue_shift", cx) / 360.0,
                        f(e, "hsl_blur", cx).clamp(0.0, 100.0) / 100.0 * 20.0 * cx.px_scale,
                        f(e, "hsl_denoise", cx).clamp(0.0, 100.0) / 100.0,
                    ];
                    let radius = ((1.0 + 2.0 * params[11]) * cx.px_scale).round().max(1.0) as u32;
                    let (rx, ry) = if params[10] >= 0.3 { gaussian_boxes(w, h, params[10], params[10]) } else { (Vec::new(), Vec::new()) };
                    chain.push(FxOp::Hsl { params, output: choice(e, "hsl_show_mask"), radius, rx, ry });
                }
                if sharpen.abs() > 1e-3 {
                    let (rx, ry) = gaussian_boxes(w, h, 1.2 * cx.px_scale.max(0.35), 1.2 * cx.px_scale.max(0.35));
                    chain.push(FxOp::Unsharp { rx, ry, amount: sharpen.max(-1.0), threshold: 0.0 });
                }
                if chain.len() == 1 { chain.remove(0) } else { FxOp::Chain(chain) }
            }
            _ => return None,
        })
    }

    fn crop_rect(w: usize, h: usize, l: f32, t: f32, r: f32, b: f32, feather: f32) -> FxOp {
        let (w, h) = (w as f32, h as f32);
        FxOp::Crop { x0: l * w, x1: w * (1.0 - r), y0: t * h, y1: h * (1.0 - b), feather: feather.max(0.0) }
    }

    /// Whether the GPU shader reproduces [`apply`](Self::apply) for this op: every number finite
    /// (the CPU's NaN / infinity behaviour is left to the CPU) and resamples that the CPU does
    /// not pre-filter.
    pub fn gpu_ok(&self) -> bool {
        let fin = |v: &[f32]| v.iter().all(|x| x.is_finite());
        match self {
            FxOp::Chain(ops) => !ops.is_empty() && ops.len() <= 32 && ops.iter().all(|op| !matches!(op, FxOp::Chain(_) | FxOp::Masked { .. }) && op.gpu_ok()),
            FxOp::Lut { lut } => crate::grading::lut_ok(lut),
            FxOp::Grade(grade) => grade.gpu_ok(),
            FxOp::Hsl { params, output, radius, .. } => fin(params) && *output <= 3 && (params[11] <= 0.0 || (1..=3).contains(radius)),
            FxOp::Masked { op, masks } => !matches!(**op, FxOp::Masked { .. } | FxOp::OpacityMask { .. }) && op.gpu_ok() && masks_ok(masks),
            FxOp::OpacityMask { masks } => masks_ok(masks),
            FxOp::UltraKey { params, dominant, .. } => fin(params) && params[3] > 0.0 && params[6] < 1.0 && *dominant < 3,
            FxOp::ChromaKey { key_ycc, tolerance, softness, spill, dominant, .. } => {
                fin(key_ycc) && fin(&[*tolerance, *softness, *spill]) && *softness > 0.0 && *dominant < 3
            }
            FxOp::LumaKey { threshold, cutoff } => fin(&[*threshold, *cutoff]),
            FxOp::BrightnessContrast { br, co } => fin(&[*br, *co]),
            FxOp::ProcAmp { br, co, hue, sat } => fin(&[*br, *co, *hue, *sat]),
            FxOp::Tint { black, white, amount } => fin(black) && fin(white) && amount.is_finite(),
            FxOp::BlackWhite | FxOp::HFlip | FxOp::VFlip => true,
            FxOp::ColorBalance { sh, md, hi, .. } => fin(sh) && fin(md) && fin(hi),
            FxOp::LeaveColor { amount, key_hue, tol, soft } => fin(&[*amount, *key_hue, *tol, *soft]),
            FxOp::ChangeToColor { from_hue, to_hue, tol, soft } => fin(&[*from_hue, *to_hue, *tol, *soft]),
            FxOp::ColorPass { key, sim, .. } => fin(key) && sim.is_finite(),
            FxOp::Gamma { g } => g.is_finite(),
            FxOp::Levels { ib, iw, ob, ow, g } => fin(&[*ib, *iw, *ob, *ow, *g]),
            FxOp::Extract { lo, hi, soft, .. } => fin(&[*lo, *hi, *soft]),
            FxOp::Invert { blend, .. } => blend.is_finite(),
            FxOp::Posterize { n } => n.is_finite(),
            FxOp::Gaussian { .. } => true,
            FxOp::DirectionalBlur { dx, dy, .. } => fin(&[*dx, *dy]),
            FxOp::Unsharp { amount, threshold, .. } => fin(&[*amount, *threshold]),
            FxOp::Crop { x0, x1, y0, y1, feather } => fin(&[*x0, *x1, *y0, *y1, *feather]),
            FxOp::Resample(r) => {
                r.rect[0] != u32::MAX
                    && r.opacity.is_finite()
                    && r.inv.as_ref().is_none_or(|m| [m.a, m.b, m.c, m.d, m.e, m.f].iter().all(|v| v.is_finite() && v.abs() < 1e7))
            }
            FxOp::Mirror { cx, cy, nx, ny } => fin(&[*cx, *cy, *nx, *ny]),
            FxOp::Offset { dx, dy, blend } => fin(&[*dx, *dy, *blend]),
            FxOp::AscCdl { slope, offset, power, sat } => fin(slope) && fin(offset) && fin(power) && sat.is_finite(),
            FxOp::ChannelMix { m } => m.iter().all(|r| fin(r)),
            FxOp::ColorReplace { sim, target, replace, replace_hsl, .. } => sim.is_finite() && fin(target) && fin(replace) && fin(replace_hsl),
            FxOp::AlphaAdjust { opacity, .. } => opacity.is_finite(),
            FxOp::Vignette { amount, midpoint, roundness, feather, target } => fin(&[*amount, *midpoint, *roundness, *feather]) && fin(target),
            FxOp::VideoLimiter { max, comp, axis, warning_color, .. } => *axis <= 3 && max.is_finite() && comp.is_finite() && fin(warning_color),
            FxOp::Lumetri { gains, exposure, contrast, hl, sh, wh, bl, sat, faded, vib, st, ht, va, vmid, vround, vfeather, aspect, gpu_capable, .. } => {
                *gpu_capable
                    && fin(gains)
                    && fin(&[*exposure, *contrast, *hl, *sh, *wh, *bl, *sat, *faded, *vib])
                    && fin(st)
                    && fin(ht)
                    && fin(&[*va, *vmid, *vround, *vfeather, *aspect])
            }
        }
    }

    /// The CPU reference.
    pub fn apply(&self, img: &mut Image) {
        if img.w == 0 || img.h == 0 {
            return;
        }
        match self {
            FxOp::Chain(ops) => {
                for op in ops {
                    op.apply(img);
                }
            }
            FxOp::Lut { lut } => img.map_rgb(|c, _, _| dec(lut.apply(enc(c).map(|q| q.clamp(0.0, 1.0))))),
            FxOp::Grade(grade) => grade.apply(img),
            FxOp::Hsl { params: q, output, radius, .. } => {
                let mask = if q[11] > 0.0 || q[10] >= 0.3 {
                    let mut mask: Vec<f32> = img
                        .px
                        .par_chunks_exact(4)
                        .map(|p| {
                            if p[3] <= 1e-6 { 0.0 } else { crate::grading::hsl_key(enc([p[0] / p[3], p[1] / p[3], p[2] / p[3]]).map(|v| v.clamp(0.0, 1.0)), q) }
                        })
                        .collect();
                    if q[11] > 0.0 {
                        let median = crate::effects::median_filter(&mask, img.w, img.h, (*radius).min(3) as usize);
                        for (m, d) in mask.iter_mut().zip(median) {
                            *m += (d - *m) * q[11].min(1.0);
                        }
                    }
                    if q[10] >= 0.3 {
                        crate::effects::blur_plane(&mut mask, img.w, img.h, q[10]);
                    }
                    Some(mask)
                } else {
                    None
                };
                let w = img.w;
                img.map_rgb(|c, x, y| {
                    let v = enc(c);
                    let u = v.map(|v| v.clamp(0.0, 1.0));
                    let mut h = rgb_to_hsl(u[0], u[1], u[2]);
                    let m = mask.as_ref().and_then(|m| m.get(y * w + x)).copied().unwrap_or_else(|| crate::grading::hsl_key(u, q));
                    dec(match output {
                        1 => u.map(|v| h[2] + (v - h[2]) * m),
                        2 => u.map(|v| v * m),
                        3 => [m; 3],
                        _ => {
                            h[0] = (h[0] + q[9]).rem_euclid(1.0);
                            h[1] = (h[1] * q[8]).clamp(0.0, 1.0);
                            let mut c2 = hsl_to_rgb(h[0], h[1], h[2]);
                            c2[0] *= 1.0 + 0.25 * q[6];
                            c2[2] *= 1.0 - 0.25 * q[6];
                            c2[1] *= 1.0 - 0.2 * q[7];
                            [u[0] + (c2[0] - u[0]) * m, u[1] + (c2[1] - u[1]) * m, u[2] + (c2[2] - u[2]) * m]
                        }
                    })
                });
            }
            FxOp::Masked { op, masks } => {
                let original = img.clone();
                op.apply(img);
                if let Some(cov) = crate::mask::coverage(masks, img.w, img.h) {
                    crate::mask::mix(img, &original, &cov);
                }
            }
            FxOp::OpacityMask { masks } => {
                if let Some(cov) = crate::mask::coverage(masks, img.w, img.h) {
                    crate::mask::scale_by(img, &cov);
                }
            }
            FxOp::UltraKey { params: q, dominant, output } => {
                let dom = *dominant as usize;
                if dom >= 3 {
                    return;
                }
                let kmag2 = q[3].max(1e-6);
                let cc = (q[15] - 1.0).abs() > 1e-4 || q[16].abs() > 1e-6 || (q[17] - 1.0).abs() > 1e-4;
                img.px.par_chunks_exact_mut(4).for_each(|p| {
                    let src_a = p[3];
                    let mut v = enc(Image::unpremul([p[0], p[1], p[2], src_a]));
                    let mut a = 0.0;
                    if src_a > 1e-6 {
                        let y = filmcraft_color::rgb_to_ycbcr(v[0], v[1], v[2], filmcraft_color::Matrix::Bt709);
                        let proj = (y[1] * q[1] + y[2] * q[2]) / kmag2;
                        let perp = ((y[1] - proj * q[1]).powi(2) + (y[2] - proj * q[2]).powi(2)).sqrt() / kmag2.sqrt();
                        let keyness = proj.clamp(0.0, 1.5) * (1.0 - crate::vfx::smoothstep(0.0, q[4], perp));
                        let mut m = (1.0 - keyness * q[5]).clamp(0.0, 1.0);
                        m += (y[0] - q[0]).max(0.0) * q[7] * 2.0 * keyness.min(1.0);
                        m += (q[0] - y[0]).max(0.0) * q[8] * 2.0 * keyness.min(1.0);
                        a = ((m.clamp(0.0, 1.0) - q[6]) / (1.0 - q[6]).max(f32::MIN_POSITIVE)).clamp(0.0, 1.0);
                    }
                    if q[9] > 0.0 {
                        a = ((a - q[10]) * (1.0 + q[9] * 4.0) + q[10]).clamp(0.0, 1.0);
                    }
                    let (o1, o2) = (v[(dom + 1) % 3], v[(dom + 2) % 3]);
                    let limit = o1.max(o2) * (1.0 - q[13]) + (o1 + o2) * 0.5 * q[13];
                    let excess = (v[dom] - limit).max(0.0);
                    if excess > 0.0 && q[11] > 0.0 {
                        let l0 = luma709(v[0], v[1], v[2]);
                        v[dom] -= excess * q[11];
                        let l1 = luma709(v[0], v[1], v[2]);
                        v = v.map(|x| l1 + (x - l1) * (1.0 - q[12] * (excess * 4.0).min(1.0)));
                        v = v.map(|x| x + (l0 - l1) * q[14]);
                    }
                    if cc {
                        let mut h = rgb_to_hsl(v[0].clamp(0.0, 1.0), v[1].clamp(0.0, 1.0), v[2].clamp(0.0, 1.0));
                        h[0] = (h[0] + q[16]).rem_euclid(1.0);
                        h[1] = (h[1] * q[15]).clamp(0.0, 1.0);
                        h[2] = (h[2] * q[17]).clamp(0.0, 1.0);
                        v = hsl_to_rgb(h[0], h[1], h[2]);
                    }
                    let l = dec(v);
                    if *output == 1 {
                        let g = filmcraft_color::srgb_to_linear(a) * src_a;
                        p.copy_from_slice(&[g, g, g, src_a]);
                    } else {
                        let na = if *output == 2 { src_a } else { a * src_a };
                        p.copy_from_slice(&[l[0] * na, l[1] * na, l[2] * na, na]);
                    }
                });
            }
            FxOp::ChromaKey { key_ycc, tolerance, softness, spill, dominant, output } => {
                let dom = *dominant as usize;
                // Public ops can be constructed directly; malformed channel indices never panic.
                if dom >= 3 {
                    return;
                }
                let softness = softness.max(f32::MIN_POSITIVE);
                img.px.par_chunks_exact_mut(4).for_each(|p| {
                    if p[3] <= 0.0 {
                        return;
                    }
                    let mut c = enc([p[0] / p[3], p[1] / p[3], p[2] / p[3]]);
                    let ycc = filmcraft_color::rgb_to_ycbcr(c[0], c[1], c[2], filmcraft_color::Matrix::Bt709);
                    let d = ((ycc[1] - key_ycc[1]).powi(2) + (ycc[2] - key_ycc[2]).powi(2)).sqrt() + (ycc[0] - key_ycc[0]).abs() * 0.15;
                    let alpha = ((d - tolerance) / softness).clamp(0.0, 1.0);
                    if *spill > 0.0 {
                        let others = (c[(dom + 1) % 3] + c[(dom + 2) % 3]) / 2.0;
                        if c[dom] > others {
                            c[dom] -= (c[dom] - others) * spill;
                        }
                    }
                    let a = p[3] * alpha;
                    if *output == 1 {
                        p.copy_from_slice(&[a, a, a, p[3]]);
                    } else {
                        let lo = dec(c);
                        p.copy_from_slice(&[lo[0] * a, lo[1] * a, lo[2] * a, a]);
                    }
                });
            }
            FxOp::LumaKey { threshold, cutoff } => {
                img.px.par_chunks_exact_mut(4).for_each(|p| {
                    if p[3] <= 0.0 {
                        return;
                    }
                    let l = linear_to_srgb(luma709(p[0] / p[3], p[1] / p[3], p[2] / p[3]).max(0.0));
                    let a = if l <= *cutoff {
                        0.0
                    } else if l >= threshold.max(cutoff + 1e-3) {
                        1.0
                    } else {
                        (l - cutoff) / (threshold - cutoff).max(1e-3)
                    };
                    for v in p {
                        *v *= a;
                    }
                });
            }
            FxOp::BrightnessContrast { br, co } => {
                let (br, co) = (*br, *co);
                img.map_rgb(|c, _, _| {
                    let c = enc(c);
                    dec(c.map(|v| (v - 0.5) * co + 0.5 + br))
                });
            }
            FxOp::ProcAmp { br, co, hue, sat } => {
                let (br, co, hue, sat) = (*br, *co, *hue, *sat);
                img.map_rgb(|c, _, _| {
                    let c = enc(c);
                    let mut hsl = rgb_to_hsl(c[0], c[1], c[2]);
                    hsl[0] = (hsl[0] + hue).rem_euclid(1.0);
                    hsl[1] = (hsl[1] * sat).clamp(0.0, 1.0);
                    let c = hsl_to_rgb(hsl[0], hsl[1], hsl[2]);
                    dec(c.map(|v| (v - 0.5) * co + 0.5 + br))
                });
            }
            FxOp::Tint { black: bl, white: wh, amount } => {
                let amt = *amount;
                img.map_rgb(|c, _, _| {
                    let l = linear_to_srgb(luma709(c[0], c[1], c[2]).max(0.0));
                    let t = [bl[0] + (wh[0] - bl[0]) * l, bl[1] + (wh[1] - bl[1]) * l, bl[2] + (wh[2] - bl[2]) * l];
                    let e = enc(c);
                    dec([e[0] + (t[0] - e[0]) * amt, e[1] + (t[1] - e[1]) * amt, e[2] + (t[2] - e[2]) * amt])
                });
            }
            FxOp::BlackWhite => img.map_rgb(|c, _, _| {
                let l = luma709(c[0], c[1], c[2]);
                [l, l, l]
            }),
            FxOp::ColorBalance { sh, md, hi, preserve } => {
                let preserve = *preserve;
                img.map_rgb(|c, _, _| {
                    let c = enc(c);
                    let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                    let ws = (1.0 - l).powi(2);
                    let wh = l.powi(2);
                    let wm = 1.0 - ws - wh;
                    let mut o = [0.0; 3];
                    for k in 0..3 {
                        o[k] = c[k] + sh[k] * ws + md[k] * wm.max(0.0) + hi[k] * wh;
                    }
                    if preserve {
                        let l2 = 0.2126 * o[0] + 0.7152 * o[1] + 0.0722 * o[2];
                        let d = l - l2;
                        o = o.map(|v| v + d);
                    }
                    dec(o)
                });
            }
            FxOp::LeaveColor { amount, key_hue, tol, soft } => {
                let (amt, kh, tol, soft) = (*amount, *key_hue, *tol, *soft);
                img.map_rgb(|c, _, _| {
                    let ec = enc(c);
                    let h = rgb_to_hsl(ec[0], ec[1], ec[2])[0];
                    let d = (h - kh).abs().min(1.0 - (h - kh).abs()) * 2.0;
                    let keep = 1.0 - ((d - tol) / soft).clamp(0.0, 1.0);
                    let l = luma709(c[0], c[1], c[2]);
                    let k = amt * (1.0 - keep);
                    [c[0] + (l - c[0]) * k, c[1] + (l - c[1]) * k, c[2] + (l - c[2]) * k]
                });
            }
            FxOp::ChangeToColor { from_hue: fh, to_hue: th, tol, soft } => {
                let (fh, th, tol, soft) = (*fh, *th, *tol, *soft);
                img.map_rgb(|c, _, _| {
                    let ec = enc(c);
                    let mut hsl = rgb_to_hsl(ec[0], ec[1], ec[2]);
                    let d = (hsl[0] - fh).abs().min(1.0 - (hsl[0] - fh).abs());
                    let w = 1.0 - ((d - tol) / soft).clamp(0.0, 1.0);
                    hsl[0] = (hsl[0] + (th - fh) * w).rem_euclid(1.0);
                    dec(hsl_to_rgb(hsl[0], hsl[1], hsl[2]))
                });
            }
            FxOp::ColorPass { key, sim, reverse } => {
                let (sim, rev) = (*sim, *reverse);
                img.map_rgb(|c, _, _| {
                    let ec = enc(c);
                    let d = ((ec[0] - key[0]).powi(2) + (ec[1] - key[1]).powi(2) + (ec[2] - key[2]).powi(2)).sqrt();
                    let pass = (d <= sim * 1.2) != rev;
                    if pass {
                        c
                    } else {
                        let l = luma709(c[0], c[1], c[2]);
                        [l, l, l]
                    }
                });
            }
            FxOp::Gamma { g } => {
                let g = *g;
                img.map_rgb(|c, _, _| dec(enc(c).map(|v| v.max(0.0).powf(g))));
            }
            FxOp::Levels { ib, iw, ob, ow, g } => {
                let (ib, iw, ob, ow, g) = (*ib, *iw, *ob, *ow, *g);
                img.map_rgb(|c, _, _| dec(enc(c).map(|v| ob + (((v - ib) / (iw - ib)).clamp(0.0, 1.0)).powf(g) * (ow - ob))));
            }
            FxOp::Extract { lo, hi, soft, invert } => {
                let (lo, hi, soft, inv) = (*lo, *hi, *soft, *invert);
                img.map_rgb(|c, _, _| {
                    let l = linear_to_srgb(luma709(c[0], c[1], c[2]).max(0.0));
                    let inside = ((l - lo) / soft).clamp(0.0, 1.0).min(((hi - l) / soft).clamp(0.0, 1.0));
                    let v = if inv { 1.0 - inside } else { inside };
                    [v, v, v]
                });
            }
            FxOp::Invert { channel, blend } => {
                let (ch, blend) = (*channel, *blend);
                if ch == 4 {
                    img.px.par_chunks_mut(4).for_each(|p| {
                        let a = p[3];
                        let na = 1.0 - a;
                        let k = if a > 1e-6 { na / a } else { 0.0 };
                        for c in &mut p[..3] {
                            *c *= k;
                        }
                        p[3] = na * (1.0 - blend) + a * blend;
                    });
                } else {
                    img.map_rgb(|c, _, _| {
                        let ec = enc(c);
                        let mut o = ec;
                        for k in 0..3 {
                            if ch == 0 || ch as usize == k + 1 {
                                o[k] = 1.0 - ec[k];
                            }
                        }
                        dec([o[0] + (ec[0] - o[0]) * blend, o[1] + (ec[1] - o[1]) * blend, o[2] + (ec[2] - o[2]) * blend])
                    });
                }
            }
            FxOp::Posterize { n } => {
                let n = *n;
                img.map_rgb(|c, _, _| dec(enc(c).map(|v| (v * n).round() / n)));
            }
            FxOp::Gaussian { rx, ry, repeat } => crate::effects::box_blur(img, rx, ry, *repeat),
            FxOp::DirectionalBlur { dx, dy, steps } => crate::effects::directional_taps(img, *dx, *dy, *steps as usize),
            FxOp::Unsharp { rx, ry, amount, threshold } => crate::effects::unsharp_boxes(img, rx, ry, *amount, *threshold),
            FxOp::Crop { x0, x1, y0, y1, feather } => crate::effects::crop_px(img, *x0, *x1, *y0, *y1, *feather),
            FxOp::Resample(r) => {
                let out = match (&r.inv, r.rect[0]) {
                    (Some(m), u32::MAX) => img.transformed(img.w, img.h, m), // mip path (CPU only)
                    (Some(inv), _) => resample_cpu(img, inv, r.rect),
                    (None, _) => Image::new(img.w, img.h),
                };
                *img = out;
                img.scale_alpha(r.opacity);
            }
            FxOp::HFlip => {
                let w = img.w;
                img.px.par_chunks_mut(w * 4).for_each(|row| {
                    for x in 0..w / 2 {
                        for k in 0..4 {
                            row.swap(x * 4 + k, (w - 1 - x) * 4 + k);
                        }
                    }
                });
            }
            FxOp::VFlip => {
                let (w, h) = (img.w, img.h);
                for y in 0..h / 2 {
                    let (a, bb) = img.px.split_at_mut((h - 1 - y) * w * 4);
                    a[y * w * 4..(y + 1) * w * 4].swap_with_slice(&mut bb[..w * 4]);
                }
            }
            FxOp::Mirror { cx, cy, nx, ny } => {
                let (c, n) = ((*cx as f64, *cy as f64), (*nx as f64, *ny as f64));
                let src = img.clone();
                let w = img.w;
                img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..w {
                        let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                        let d = (px - c.0) * n.0 + (py - c.1) * n.1;
                        if d > 0.0 {
                            let (rx, ry) = (px - 2.0 * d * n.0, py - 2.0 * d * n.1);
                            row[x * 4..x * 4 + 4].copy_from_slice(&src.sample_bilinear(rx as f32, ry as f32));
                        }
                    }
                });
            }
            FxOp::Offset { dx, dy, blend } => {
                let (dx, dy, blend) = (*dx as f64, *dy as f64, *blend);
                let src = img.clone();
                let (w, h) = (img.w as f64, img.h as f64);
                let wi = img.w;
                img.px.par_chunks_mut(wi * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..wi {
                        let u = (x as f64 + 0.5 - dx).rem_euclid(w);
                        let v = (y as f64 + 0.5 - dy).rem_euclid(h);
                        let p = src.sample_bilinear_clamped(u as f32, v as f32);
                        let o = src.get(x, y);
                        for k in 0..4 {
                            row[x * 4 + k] = p[k] + (o[k] - p[k]) * blend;
                        }
                    }
                });
            }
            FxOp::AscCdl { slope, offset, power, sat } => {
                let (s, o, pw, sat) = (*slope, *offset, *power, *sat);
                img.map_rgb(|c, _, _| dec(crate::vfx::cdl(enc(c), s, o, pw, sat)));
            }
            FxOp::ChannelMix { m } => {
                let m = *m;
                img.map_rgb(|c, _, _| {
                    let v = enc(c);
                    dec([0, 1, 2].map(|k| (m[k][0] * v[0] + m[k][1] * v[1] + m[k][2] * v[2] + m[k][3]).clamp(0.0, 1.0)))
                });
            }
            FxOp::ColorReplace { sim, solid, target: t, replace: r, replace_hsl: rh } => {
                let (sim, solid) = (*sim, *solid);
                img.map_rgb(|c, _, _| {
                    let v = enc(c);
                    let d = ((v[0] - t[0]).powi(2) + (v[1] - t[1]).powi(2) + (v[2] - t[2]).powi(2)).sqrt();
                    let k = 1.0 - crate::vfx::smoothstep(sim * 0.85, sim.max(1e-4), d);
                    if k <= 0.0 {
                        return c;
                    }
                    let target = if solid {
                        [r[0], r[1], r[2]]
                    } else {
                        let l = rgb_to_hsl(v[0], v[1], v[2])[2];
                        hsl_to_rgb(rh[0], rh[1], l)
                    };
                    dec(crate::vfx::lerp3(v, target, k))
                });
            }
            FxOp::AlphaAdjust { opacity, ignore, invert, mask_only } => {
                let (op, ignore, invert, mask_only) = (*opacity, *ignore, *invert, *mask_only);
                img.px.par_chunks_mut(4).for_each(|p| {
                    let c = Image::unpremul([p[0], p[1], p[2], p[3]]);
                    let mut a = if ignore { 1.0 } else { p[3] };
                    if invert {
                        a = 1.0 - a;
                    }
                    a = (a * op).clamp(0.0, 1.0);
                    if mask_only {
                        let g = filmcraft_color::srgb_to_linear(a);
                        p.copy_from_slice(&[g, g, g, 1.0]);
                    } else {
                        p.copy_from_slice(&[c[0] * a, c[1] * a, c[2] * a, a]);
                    }
                });
            }
            FxOp::Vignette { amount, midpoint, roundness, feather, target } => {
                let (amt, mid, round, feather, target) = (*amount, *midpoint, *roundness, *feather, *target);
                if !amt.is_finite() || !mid.is_finite() || !round.is_finite() || !feather.is_finite() || !target.iter().all(|c| c.is_finite()) {
                    return;
                }
                if amt.abs() < 1e-5 {
                    return;
                }
                let (w, h) = (img.w as f32, img.h as f32);
                let aspect = w / h;
                img.map_rgb(|c, x, y| {
                    let mut nx = (x as f32 + 0.5) / w * 2.0 - 1.0;
                    let ny = (y as f32 + 0.5) / h * 2.0 - 1.0;
                    // roundness 100 = circle; 0 = an ellipse following the frame; −100 = squarer
                    if round > 0.0 {
                        nx *= 1.0 + (aspect - 1.0) * round;
                    }
                    let p = if round < 0.0 { 2.0 + (-round) * 6.0 } else { 2.0 };
                    // 0 at the centre, 1 at the corners
                    let d = (nx.abs().powf(p) + ny.abs().powf(p)).powf(1.0 / p) / 2f32.powf(1.0 / p);
                    let edge = crate::vfx::smoothstep(mid - feather * 0.5, mid + feather * 0.5, d);
                    dec(crate::vfx::lerp3(enc(c), target, edge * amt.abs()))
                });
            }
            FxOp::VideoLimiter { max, comp, axis, warn, warning_color } => {
                let (max, comp, axis, warn, wc) = (*max, *comp, *axis, *warn, *warning_color);
                if !max.is_finite() || !comp.is_finite() || !wc.iter().all(|c| c.is_finite()) {
                    return;
                }
                img.map_rgb(|c, _, _| {
                    let v = [linear_to_enc(c[0]), linear_to_enc(c[1]), linear_to_enc(c[2])];
                    let out = limit(v, max, comp, axis);
                    if warn && v.iter().zip(&out).any(|(a, b)| (a - b).abs() > 1e-4) {
                        return wc;
                    }
                    out.map(enc_to_linear)
                });
            }
            FxOp::Lumetri {
                gains,
                exposure,
                contrast,
                hl,
                sh,
                wh,
                bl,
                sat,
                creative_on,
                faded,
                vib,
                st,
                ht,
                vignette_on,
                va,
                vmid,
                vround,
                vfeather,
                aspect,
                ..
            } => {
                let gains = *gains;
                let (exposure, contrast, hl, sh, wh, bl, sat) = (*exposure, *contrast, *hl, *sh, *wh, *bl, *sat);
                let (creative_on, faded, vib, st, ht) = (*creative_on, *faded, *vib, *st, *ht);
                let (vignette_on, va, vmid, vround, vfeather, aspect) = (*vignette_on, *va, *vmid, *vround, *vfeather, *aspect);
                let (w, h) = (img.w as f32, img.h as f32);
                img.map_rgb(|c, x, y| {
                    // white balance + exposure in linear light
                    let lin = [c[0] * gains[0] * exposure, c[1] * gains[1] * exposure, c[2] * gains[2] * exposure];
                    let mut v = enc(lin);
                    // whites / blacks: endpoints
                    let b0 = -bl * 0.15;
                    let w0 = 1.0 - wh * 0.15;
                    v = v.map(|q| (q - b0) / (w0 - b0).max(1e-3));
                    // highlights / shadows: luma-weighted lift/compress, hue preserving
                    let l = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
                    let ws = (1.0 - l).clamp(0.0, 1.0).powi(3);
                    let whl = l.clamp(0.0, 1.0).powi(3);
                    let nl = (l + sh * 0.35 * ws + hl * 0.35 * whl).max(0.0);
                    if l > 1e-5 {
                        let k = nl / l;
                        v = v.map(|q| q * k);
                    }
                    // contrast: smooth S-curve around mid grey
                    if contrast.abs() > 1e-4 {
                        let k = 1.0 + contrast;
                        v = v.map(|q| {
                            let q = q.clamp(0.0, 1.0);
                            let s = q * q * (3.0 - 2.0 * q);
                            if k >= 1.0 { q + (s - q) * (k - 1.0) } else { 0.5 + (q - 0.5) * k }
                        });
                    }
                    // faded film: lift blacks and compress
                    if faded > 0.0 {
                        v = v.map(|q| q * (1.0 - 0.25 * faded) + 0.12 * faded);
                    }
                    // split tone
                    if creative_on {
                        let l2 = (0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2]).clamp(0.0, 1.0);
                        for k in 0..3 {
                            v[k] += (st[k] - 0.5) * 0.3 * (1.0 - l2) + (ht[k] - 0.5) * 0.3 * l2;
                        }
                    }
                    // saturation & vibrance
                    let l3 = 0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2];
                    let cur_sat = v[0].max(v[1]).max(v[2]) - v[0].min(v[1]).min(v[2]);
                    let s = sat * (1.0 + vib * (1.0 - cur_sat.clamp(0.0, 1.0)));
                    v = v.map(|q| l3 + (q - l3) * s);
                    // vignette
                    if vignette_on && va.abs() > 1e-4 {
                        let nx = (x as f32 / w - 0.5) * 2.0 * (1.0 + vround * 0.0) * if vround < 0.0 { aspect.powf(-vround) } else { 1.0 };
                        let ny = (y as f32 / h - 0.5) * 2.0;
                        let d = (nx * nx + ny * ny).sqrt() / std::f32::consts::SQRT_2;
                        let edge = ((d - vmid * 0.9) / (vfeather.max(0.01) * 0.9)).clamp(0.0, 1.0);
                        let e2 = edge * edge * (3.0 - 2.0 * edge);
                        let k = 1.0 + va * 0.2 * e2;
                        v = v.map(|q| if va < 0.0 { q * k.max(0.0) } else { q + (1.0 - q) * (k - 1.0) });
                    }
                    dec(v)
                });
            }
        }
    }
}

/// sRGB encoding that keeps values above 1 (super-whites) instead of clamping them.
#[inline]
fn linear_to_enc(v: f32) -> f32 {
    if v <= 1.0 { filmcraft_color::linear_to_srgb(v.max(0.0)) } else { 1.0 + (v - 1.0) / 2.4 }
}
#[inline]
fn enc_to_linear(v: f32) -> f32 {
    if v <= 1.0 { filmcraft_color::srgb_to_linear(v.max(0.0)) } else { 1.0 + (v - 1.0) * 2.4 }
}

#[inline]
fn knee(v: f32, max: f32, comp: f32) -> f32 {
    let k = max * (1.0 - comp);
    if comp <= 0.0 || v <= k {
        return v.min(max);
    }
    let r = max - k;
    k + r * (1.0 - (-(v - k) / r).exp())
}

pub(crate) fn limit(v: [f32; 3], max: f32, comp: f32, axis: u32) -> [f32; 3] {
    let m = filmcraft_color::Matrix::Bt709;
    let ycc = filmcraft_color::rgb_to_ycbcr(v[0], v[1], v[2], m);
    let (kr, kb) = (0.2126f32, 0.0722f32);
    let to_rgb = |y: f32, cb: f32, cr: f32| {
        let r = y + 2.0 * (1.0 - kr) * cr;
        let b = y + 2.0 * (1.0 - kb) * cb;
        let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
        [r, g, b]
    };
    let (mut y, mut cb, mut cr) = (ycc[0], ycc[1], ycc[2]);
    if axis != 1 {
        y = knee(y.max(0.0), max, comp);
    }
    if axis != 0 {
        // scale chroma so every channel fits 0…max (the gamut of legal R′G′B′)
        let rgb = to_rgb(y, cb, cr);
        let mut s = 1.0f32;
        for &ch in &rgb {
            let d = ch - y;
            if ch > max && d > 1e-6 {
                s = s.min(((max - y) / d).max(0.0));
            }
            if ch < 0.0 && d < -1e-6 {
                s = s.min((y / -d).max(0.0));
            }
        }
        if axis == 3 && s < 1.0 - 1e-4 {
            // smart limit: soft chroma compression rather than a hard cut
            s = knee(s, 1.0, comp.max(0.03)).min(1.0);
        }
        cb *= s;
        cr *= s;
    }
    to_rgb(y, cb, cr).map(|q| q.clamp(0.0, max))
}

/// The body of `Image::transformed` (no mip path) for a precomputed inverse and rectangle.
fn resample_cpu(src: &Image, inv: &Affine, rect: [u32; 4]) -> Image {
    let (w, h) = (src.w, src.h);
    let mut out = Image::new(w, h);
    let [x0, x1, y0, y1] = rect.map(|v| v as usize);
    let (x1, y1) = (x1.min(w), y1.min(h));
    let (x0, y0) = (x0.min(x1), y0.min(y1));
    let axis_aligned = inv.b == 0.0 && inv.c == 0.0;
    out.px.par_chunks_mut(w * 4).enumerate().skip(y0).take(y1 - y0).for_each(|(y, row)| {
        let py = y as f64 + 0.5;
        for x in x0..x1 {
            let px = x as f64 + 0.5;
            let (u, v) =
                if axis_aligned { (inv.a * px + inv.e, inv.d * py + inv.f) } else { (inv.a * px + inv.c * py + inv.e, inv.b * px + inv.d * py + inv.f) };
            if u < -1.0 || v < -1.0 || u > w as f64 + 1.0 || v > h as f64 + 1.0 {
                continue;
            }
            row[x * 4..x * 4 + 4].copy_from_slice(&src.sample_bilinear(u as f32, v as f32));
        }
    });
    out
}

/// Bound shader loops and reject non-finite flattened geometry before GPU upload.
fn masks_ok(masks: &[crate::mask::FlatMask]) -> bool {
    !masks.is_empty()
        && masks.len() <= 64
        && masks.iter().all(|m| {
            m.mode != filmcraft_project::MaskMode::None
                && m.pts.len() <= 4096
                && m.pts.iter().flatten().all(|v| v.is_finite())
                && [m.feather, m.expansion, m.opacity].iter().all(|v| v.is_finite())
        })
        && masks.iter().map(|m| m.pts.len()).sum::<usize>() <= 16384
}
