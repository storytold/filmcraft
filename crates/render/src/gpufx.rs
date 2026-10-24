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

#[path = "gpufx_filters.rs"]
mod filters;
#[path = "gpufx_legacy_wipes.rs"]
mod legacy_wipes;
#[path = "gpufx_procedural.rs"]
mod procedural;
#[path = "gpufx_spatial.rs"]
mod spatial;
#[path = "gpufx_stylize.rs"]
mod stylize;

pub use filters::FilterOp;
pub use legacy_wipes::LegacyWipeOp;
pub use procedural::ProceduralOp;
pub use spatial::SpatialOp;
pub use stylize::StylizeOp;

/// Independent references for catalog operations whose original CPU implementation lives in
/// `effects.rs`, outside the `vfx` dispatcher. Keep these test-only so parity checks do not call
/// the `FxOp` implementation they are checking.
#[cfg(test)]
pub(crate) fn legacy_cpu_oracle(img: &mut Image, e: &EffectInstance, cx: &FxCtx<'_>) -> bool {
    match e.effect.as_str() {
        "replicate" => {
            let n = f(e, "count", cx).clamp(1.0, 16.0) as usize;
            let src = img.clone();
            for y in 0..img.h {
                for x in 0..img.w {
                    let u = (x * n) % img.w;
                    let v = (y * n) % img.h;
                    let sample = src.sample_bilinear_clamped((u as f32 + 0.5) + (n as f32 - 1.0) * 0.5 / n as f32, v as f32 + 0.5);
                    let i = (y * img.w + x) * 4;
                    img.px[i..i + 4].copy_from_slice(&sample);
                }
            }
            true
        }
        "twirl" | "twirl_legacy" => {
            let mut base = e.clone();
            base.effect = "twirl".into();
            let center = point(&base, "center", cx, img);
            let angle = (f(&base, "angle", cx) as f64).to_radians();
            let radius = f(&base, "radius", cx) as f64 / 100.0 * img.w.min(img.h) as f64;
            crate::effects::warp(img, |x, y| {
                let (dx, dy) = (x - center.x, y - center.y);
                let distance = (dx * dx + dy * dy).sqrt();
                if distance >= radius {
                    return (x, y);
                }
                let a = angle * (1.0 - distance / radius).powi(2);
                let (sin, cos) = a.sin_cos();
                (center.x + dx * cos - dy * sin, center.y + dx * sin + dy * cos)
            });
            true
        }
        "lens_distortion" => {
            let k = f(e, "curvature", cx) as f64 / 100.0;
            let (center_x, center_y) = (
                img.w as f64 / 2.0 + f(e, "h_decentering", cx) as f64 / 100.0 * img.w as f64 / 2.0,
                img.h as f64 / 2.0 + f(e, "v_decentering", cx) as f64 / 100.0 * img.h as f64 / 2.0,
            );
            let norm = (img.w as f64 / 2.0).hypot(img.h as f64 / 2.0);
            crate::effects::warp(img, |x, y| {
                let (dx, dy) = ((x - center_x) / norm, (y - center_y) / norm);
                let scale = 1.0 - k * (dx * dx + dy * dy);
                (center_x + dx * scale * norm, center_y + dy * scale * norm)
            });
            true
        }
        "wave_warp" => {
            let height = f(e, "height", cx) as f64 * cx.px_scale as f64;
            let width = (f(e, "width", cx) as f64 * cx.px_scale as f64).max(1.0);
            let direction = (f(e, "direction", cx) as f64).to_radians();
            let phase = cx.seconds * f(e, "speed", cx) as f64 * std::f64::consts::TAU;
            let (ux, uy) = (direction.sin(), -direction.cos());
            crate::effects::warp(img, |x, y| {
                let offset = height * ((x * ux + y * uy) / width * std::f64::consts::TAU - phase).sin();
                (x - uy * offset, y + ux * offset)
            });
            true
        }
        "grid" => {
            let size = (f(e, "size", cx) * cx.px_scale).max(1.0);
            let border = f(e, "border", cx) * cx.px_scale;
            let color = dec({
                let c = color(e, "color", cx);
                [c[0], c[1], c[2]]
            });
            let opacity = f(e, "opacity", cx) / 100.0;
            let (w, h) = (img.w as f32, img.h as f32);
            for y in 0..img.h {
                for x in 0..img.w {
                    let dx = (x as f32 - w / 2.0).rem_euclid(size);
                    let dy = (y as f32 - h / 2.0).rem_euclid(size);
                    let distance = dx.min(size - dx).min(dy.min(size - dy));
                    let a = (border * 0.5 - distance + 0.5).clamp(0.0, 1.0) * opacity;
                    if a > 0.0 {
                        let i = (y * img.w + x) * 4;
                        for c in 0..3 {
                            img.px[i + c] = color[c] * a + img.px[i + c] * (1.0 - a);
                        }
                        img.px[i + 3] = a + img.px[i + 3] * (1.0 - a);
                    }
                }
            }
            true
        }
        "circle" => {
            let center = point(e, "center", cx, img);
            let radius = f(e, "radius", cx) * cx.px_scale;
            let raw = color(e, "color", cx);
            let color = dec([raw[0], raw[1], raw[2]]);
            let opacity = f(e, "opacity", cx) / 100.0;
            for y in 0..img.h {
                for x in 0..img.w {
                    let distance = ((x as f64 + 0.5 - center.x).hypot(y as f64 + 0.5 - center.y)) as f32;
                    let a = (radius - distance + 0.5).clamp(0.0, 1.0) * opacity;
                    if a > 0.0 {
                        let i = (y * img.w + x) * 4;
                        for c in 0..3 {
                            img.px[i + c] = color[c] * a + img.px[i + c] * (1.0 - a);
                        }
                        img.px[i + 3] = a + img.px[i + 3] * (1.0 - a);
                    }
                }
            }
            true
        }
        "ramp" => {
            let start = point(e, "start", cx, img);
            let end = point(e, "end", cx, img);
            let start_color = color(e, "start_color", cx);
            let end_color = color(e, "end_color", cx);
            let radial = choice(e, "shape") == 1;
            let blend = f(e, "blend", cx) / 100.0;
            let (vx, vy) = (end.x - start.x, end.y - start.y);
            let len2 = (vx * vx + vy * vy).max(1e-9);
            crate::effects::fill_over(img, 1.0 - blend, |x, y| {
                let t = if radial {
                    ((x as f64 - start.x).hypot(y as f64 - start.y) / len2.sqrt()) as f32
                } else {
                    (((x as f64 - start.x) * vx + (y as f64 - start.y) * vy) / len2) as f32
                }
                .clamp(0.0, 1.0);
                dec([
                    start_color[0] + (end_color[0] - start_color[0]) * t,
                    start_color[1] + (end_color[1] - start_color[1]) * t,
                    start_color[2] + (end_color[2] - start_color[2]) * t,
                ])
            });
            true
        }
        "four_color_gradient" => {
            let colors = ["c1", "c2", "c3", "c4"].map(|id| color(e, id, cx));
            let opacity = f(e, "opacity", cx) / 100.0;
            let (w, h) = (img.w as f32, img.h as f32);
            let points = [(0.25 * w, 0.25 * h), (0.75 * w, 0.25 * h), (0.25 * w, 0.75 * h), (0.75 * w, 0.75 * h)];
            let exponent = 1.0 + f(e, "blend", cx) / 100.0;
            crate::effects::fill_over(img, opacity, |x, y| {
                let mut acc = [0.0; 3];
                let mut sum = 0.0;
                for (index, point) in points.iter().enumerate() {
                    let weight = 1.0 / ((x - point.0).powi(2) + (y - point.1).powi(2)).sqrt().max(1.0).powf(exponent);
                    for channel in 0..3 {
                        acc[channel] += colors[index][channel] * weight;
                    }
                    sum += weight;
                }
                dec(acc.map(|value| value / sum))
            });
            true
        }
        "emboss" => {
            let direction = (f(e, "direction", cx) as f64).to_radians();
            let relief = f(e, "relief", cx) * cx.px_scale.max(0.25);
            let contrast = f(e, "contrast", cx) / 100.0;
            let blend = f(e, "blend", cx) / 100.0;
            let (dx, dy) = ((direction.cos() as f32) * relief, (-direction.sin() as f32) * relief);
            let src = img.clone();
            for y in 0..img.h {
                for x in 0..img.w {
                    let a = src.sample_bilinear_clamped(x as f32 + 0.5 + dx, y as f32 + 0.5 + dy);
                    let b = src.sample_bilinear_clamped(x as f32 + 0.5 - dx, y as f32 + 0.5 - dy);
                    let value = filmcraft_color::srgb_to_linear(
                        (0.5 + (filmcraft_color::luma709(a[0], a[1], a[2]) - filmcraft_color::luma709(b[0], b[1], b[2])) * contrast * 2.0).clamp(0.0, 1.0),
                    );
                    let i = (y * img.w + x) * 4;
                    for c in 0..3 {
                        img.px[i + c] = value * src.px[i + 3] * (1.0 - blend) + src.px[i + c] * blend;
                    }
                }
            }
            true
        }
        "find_edges" => {
            let invert = b(e, "invert");
            let blend = f(e, "blend", cx) / 100.0;
            let src = img.clone();
            for y in 0..img.h {
                for x in 0..img.w {
                    let luma = |dx: isize, dy: isize| {
                        let p = src.get_clamped(x as isize + dx, y as isize + dy);
                        filmcraft_color::luma709(p[0], p[1], p[2])
                    };
                    let gx = luma(1, -1) + 2.0 * luma(1, 0) + luma(1, 1) - luma(-1, -1) - 2.0 * luma(-1, 0) - luma(-1, 1);
                    let gy = luma(-1, 1) + 2.0 * luma(0, 1) + luma(1, 1) - luma(-1, -1) - 2.0 * luma(0, -1) - luma(1, -1);
                    let mut m = (gx * gx + gy * gy).sqrt().min(1.0);
                    if !invert {
                        m = 1.0 - m;
                    }
                    let i = (y * img.w + x) * 4;
                    let alpha = src.px[i + 3];
                    for c in 0..3 {
                        img.px[i + c] = m * alpha * (1.0 - blend) + src.px[i + c] * blend;
                    }
                }
            }
            true
        }
        "strobe" => {
            let period = f(e, "period", cx).max(0.001) as f64;
            let duration = f(e, "duration", cx) as f64;
            if cx.seconds % period < duration {
                let raw = color(e, "color", cx);
                let color = dec([raw[0], raw[1], raw[2]]);
                let blend = f(e, "blend", cx) / 100.0;
                img.map_rgb(|orig, _, _| {
                    [color[0] + (orig[0] - color[0]) * blend, color[1] + (orig[1] - color[1]) * blend, color[2] + (orig[2] - color[2]) * blend]
                });
            }
            true
        }
        _ => false,
    }
}

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
    ColorKey {
        key_ycc: [f32; 3],
        tol: f32,
        soft: f32,
    },
    LogoCutout {
        background: [f32; 3],
        threshold: f32,
        softness: f32,
        unmultiply: bool,
        invert: bool,
    },
    LumaKey {
        threshold: f32,
        cutoff: f32,
    },
    Cineon {
        mode: u32,
        black: f32,
        white: f32,
        ib: f32,
        iw: f32,
        gamma: f32,
        roll: f32,
    },
    Noise {
        amount: f32,
        grain: f32,
        seed: u64,
        colored: bool,
        clip: bool,
    },
    Spatial(SpatialOp),
    Procedural(ProceduralOp),
    Filter(FilterOp),
    Stylize(StylizeOp),
    LegacyWipe(LegacyWipeOp),
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
    /// Independent per-channel Gaussian blur radii (red, green, blue, alpha).
    ChannelBlur {
        rx: [Vec<u32>; 4],
        ry: [Vec<u32>; 4],
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
    DropShadow {
        dx: f64,
        dy: f64,
        color: [f32; 3],
        opacity: f32,
        rx: Vec<u32>,
        ry: Vec<u32>,
        only: bool,
    },
    AlphaGlow {
        start: [f32; 3],
        end: [f32; 3],
        brightness: f32,
        use_end: bool,
        fade: bool,
        enabled: bool,
        rx: Vec<u32>,
        ry: Vec<u32>,
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
    "color_key",
    "luma_key",
    "cineon_converter",
    "noise",
    "noise_legacy",
    "mosaic",
    "mosaic_legacy",
    "replicate",
    "magnify",
    "magnify_legacy",
    "twirl",
    "twirl_legacy",
    "spherize",
    "corner_pin",
    "rotate_3d",
    "basic_3d",
    "turbulent_displace",
    "lens_distortion",
    "wave_warp",
    "rounded_crop",
    "rgb_split",
    "checkerboard",
    "grid",
    "circle",
    "ellipse",
    "ramp",
    "gradient",
    "four_color_gradient",
    "cell_pattern",
    "emboss",
    "color_emboss",
    "find_edges",
    "strobe",
    "lens_flare",
    "median",
    "grow",
    "shrink",
    "move",
    "spin",
    "wiggle",
    "camera_shake",
    "bevel_alpha",
    "block_dissolve",
    "linear_wipe_legacy",
    "logo_cutout",
    "vr_chromatic_aberrations",
    "vr_color_gradients",
    "vr_projection",
    "vr_rotate_sphere",
    "vr_plane_to_sphere",
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
    "reduce_interlace_flicker",
    "alpha_glow",
    "channel_blur",
    "drop_shadow",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectGpuFailure {
    UnknownId,
    UnsupportedOperation,
    InvalidParameter { parameter: String },
    MissingResource { resource: crate::plan::ResourceKind },
    UnsupportedCase,
}

/// An explicit classification; an absent operation never implies a successful no-op.
#[derive(Clone, Debug, PartialEq)]
pub enum EffectGpuOutcome {
    Disabled,
    Eligible(FxOp),
    Ineligible(EffectGpuFailure),
}

/// Dependencies required by the oracle, including effects without a GPU route yet.
pub fn effect_requirements(e: &EffectInstance) -> crate::plan::ResourceRequirements {
    if !e.enabled {
        return crate::plan::ResourceRequirements::default();
    }
    let history = matches!(e.effect.as_str(), "echo" | "posterize_time" | "auto_reframe" | "warp_stabilizer" | "time_remap");
    let track = matches!(e.effect.as_str(), "track_matte" | "compound_blur" | "gradient_wipe_legacy");
    let lut = e.effect == "lumetri" && (on(e, "basic_on") && !text(e, "input_lut").is_empty() || on(e, "creative_on") && !text(e, "look_lut").is_empty());
    crate::plan::ResourceRequirements {
        sampled_inputs: if track { 2 } else { 1 },
        history,
        mask: !e.masks.is_empty() || e.effect == "track_matte",
        lut,
        ..Default::default()
    }
}

/// Classifies every live video ID while retaining the exact existing evaluated operation.
/// Canonical identity stays in `e.effect`; unknown and non-video IDs fail closed.
pub fn evaluate_gpu_effect(e: &EffectInstance, cx: &FxCtx, w: usize, h: usize) -> EffectGpuOutcome {
    use EffectGpuFailure as Failure;
    use EffectGpuOutcome as Outcome;
    use filmcraft_project::{EffectKind, ParamValue};
    if !e.enabled {
        return Outcome::Disabled;
    }
    let Some(def) = e.def().filter(|d| d.kind == EffectKind::Video) else {
        return Outcome::Ineligible(Failure::UnknownId);
    };
    // Check evaluated inputs before the oracle's casts/clamps can erase NaN or infinity.
    for param in &def.params {
        let value = e.param(param.id).map(|p| p.value_at(cx.t)).unwrap_or_else(|| param.default.clone());
        let finite = match value {
            ParamValue::Float(v) => v.is_finite(),
            // NaN point coordinates are the model's documented automatic-centre sentinel.
            ParamValue::Vec2(v) => !v.x.is_infinite() && !v.y.is_infinite(),
            ParamValue::Color(v) => v.iter().all(|v| v.is_finite()),
            ParamValue::Curve(v) => v.iter().flatten().all(|v| v.is_finite()),
            _ => true,
        };
        if !finite {
            return Outcome::Ineligible(Failure::InvalidParameter { parameter: param.id.into() });
        }
    }
    if !e.masks.is_empty() {
        return Outcome::Ineligible(Failure::MissingResource { resource: crate::plan::ResourceKind::Mask });
    }
    // effects::apply dispatches the seven-sample shutter oracle before FxOp::eval.
    if e.effect == "transform" && crate::vfx::bv(e, "shutter_override") && crate::vfx::fv(e, "shutter_angle", cx) > 0.5 {
        return Outcome::Ineligible(Failure::UnsupportedOperation);
    }
    let Some(op) = FxOp::eval(e, cx, w, h) else {
        return Outcome::Ineligible(Failure::UnsupportedOperation);
    };
    if op.gpu_ok() { Outcome::Eligible(op) } else { Outcome::Ineligible(Failure::UnsupportedCase) }
}

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

fn cineon_value(v: f32, mode: u32, black: f32, white: f32, ib: f32, iw: f32, gamma: f32, roll: f32) -> f32 {
    let k = 0.002 / 0.6;
    let offset = 10f32.powf((black - white) * k);
    match mode {
        0 => {
            let code = v * 1023.0;
            let linv = ((10f32.powf((code - white) * k) - offset) / (1.0 - offset)).max(0.0);
            let mut out = ib + linv.powf(1.0 / gamma) * (iw - ib);
            let r0 = 1.0 - roll;
            if roll > 1e-4 && out > r0 {
                out = r0 + roll * (1.0 - (-(out - r0) / roll).exp());
            }
            out.clamp(0.0, 1.0)
        }
        1 => {
            let d = ((v - ib) / (iw - ib).max(1e-4)).clamp(0.0, 1.0);
            let linv = d.powf(gamma);
            let code = white + (linv * (1.0 - offset) + offset).max(1e-6).log10() / k;
            (code / 1023.0).clamp(0.0, 1.0)
        }
        _ => (((v * 1023.0 - black) / (white - black)) * 590.0 + 95.0) / 1023.0,
    }
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
            "extract" => {
                let softness = f(e, "softness", cx);
                FxOp::Extract {
                    lo: f(e, "black", cx) / 255.0,
                    hi: f(e, "white", cx) / 255.0,
                    // Zero softness is a hard cutoff. A tiny substitute fade amplifies
                    // CPU/GPU transfer-function rounding and introduces unwanted grey edges.
                    soft: if softness <= 0.0 { 0.0 } else { softness / 100.0 * 0.2 + 1e-4 },
                    invert: b(e, "invert"),
                }
            }
            "color_key" => {
                let key = color(e, "color", cx);
                FxOp::ColorKey {
                    key_ycc: filmcraft_color::rgb_to_ycbcr(key[0], key[1], key[2], filmcraft_color::Matrix::Bt709),
                    tol: f(e, "tolerance", cx) / 255.0 * 0.6 + 0.01,
                    soft: f(e, "feather", cx) / 50.0 * 0.2 + 0.01,
                }
            }
            "logo_cutout" => {
                let bg = match choice(e, "background") {
                    0 => [1.0; 3],
                    1 => [0.0; 3],
                    _ => {
                        let c = color(e, "color", cx);
                        [c[0], c[1], c[2]]
                    }
                };
                FxOp::LogoCutout {
                    background: bg,
                    threshold: f(e, "threshold", cx) / 100.0,
                    softness: (f(e, "softness", cx) / 100.0).max(1e-3),
                    unmultiply: b(e, "unmultiply"),
                    invert: b(e, "invert"),
                }
            }
            "luma_key" => FxOp::LumaKey { threshold: f(e, "threshold", cx) / 100.0, cutoff: f(e, "cutoff", cx) / 100.0 },
            "cineon_converter" => {
                use crate::vfx::fv;
                let black = fv(e, "black10", cx);
                FxOp::Cineon {
                    mode: choice(e, "conversion"),
                    black,
                    white: fv(e, "white10", cx).max(black + 1.0),
                    ib: fv(e, "black_internal", cx) / 255.0,
                    iw: fv(e, "white_internal", cx) / 255.0,
                    gamma: fv(e, "gamma", cx).max(0.1),
                    roll: fv(e, "rolloff", cx) / 255.0,
                }
            }
            "noise" | "noise_legacy" => {
                use crate::vfx::{bv, fv};
                let grain = (fv(e, "grain_size", cx).max(1.0) * cx.px_scale).max(1.0);
                FxOp::Noise {
                    amount: fv(e, "amount", cx) / 100.0,
                    grain,
                    seed: crate::effects::noise_frame_seed(cx),
                    colored: bv(e, "color"),
                    clip: bv(e, "clip"),
                }
            }
            "mosaic"
            | "mosaic_legacy"
            | "replicate"
            | "magnify"
            | "magnify_legacy"
            | "twirl"
            | "twirl_legacy"
            | "spherize"
            | "lens_distortion"
            | "wave_warp"
            | "corner_pin"
            | "rotate_3d"
            | "basic_3d"
            | "turbulent_displace"
            | "rounded_crop"
            | "rgb_split"
            | "vr_chromatic_aberrations"
            | "vr_color_gradients"
            | "vr_projection"
            | "vr_rotate_sphere"
            | "vr_plane_to_sphere" => {
                let op = SpatialOp::eval(e, cx, w, h)?;
                if !op.gpu_ok() {
                    return None;
                }
                FxOp::Spatial(op)
            }
            "checkerboard"
            | "grid"
            | "circle"
            | "ellipse"
            | "ramp"
            | "gradient"
            | "four_color_gradient"
            | "cell_pattern"
            | "emboss"
            | "color_emboss"
            | "find_edges"
            | "strobe"
            | "lens_flare" => {
                let op = ProceduralOp::eval(e, cx, w, h)?;
                if !op.gpu_ok() {
                    return None;
                }
                FxOp::Procedural(op)
            }
            "median" => FxOp::Filter(FilterOp::eval(e, cx, w, h)?),
            "block_dissolve" | "linear_wipe_legacy" => FxOp::LegacyWipe(LegacyWipeOp::eval(e, cx, w, h)?),
            "grow" | "shrink" | "move" | "spin" | "wiggle" | "camera_shake" | "bevel_alpha" => FxOp::Stylize(StylizeOp::eval(e, cx, w, h)?),
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
            "reduce_interlace_flicker" => {
                let sigma = f(e, "softness", cx) * cx.px_scale.max(0.25);
                let (_, ry) = gaussian_boxes(w, h, 0.0, sigma);
                FxOp::Gaussian { rx: Vec::new(), ry, repeat: true }
            }
            "channel_blur" => {
                let dimensions = choice(e, "dimensions");
                let radii = ["red", "green", "blue", "alpha"].map(|id| {
                    let sigma = f(e, id, cx) * cx.px_scale * 0.5;
                    gaussian_boxes(w, h, if dimensions == 2 { 0.0 } else { sigma }, if dimensions == 1 { 0.0 } else { sigma })
                });
                let [(red_x, red_y), (green_x, green_y), (blue_x, blue_y), (alpha_x, alpha_y)] = radii;
                FxOp::ChannelBlur { rx: [red_x, green_x, blue_x, alpha_x], ry: [red_y, green_y, blue_y, alpha_y], repeat: b(e, "repeat_edge") }
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
                // below a sigma of ~0.58 the three boxes all round to radius 0 and the mask is a
                // no-op, so Sharpen vanished at 1/2 playback resolution and below (#1351)
                let r = cx.px_scale.max(0.6);
                let (rx, ry) = gaussian_boxes(w, h, r, r);
                FxOp::Unsharp { rx, ry, amount: f(e, "amount", cx) / 100.0, threshold: 0.0 }
            }
            "unsharp_mask" => {
                let r = f(e, "radius", cx) * cx.px_scale;
                let (rx, ry) = gaussian_boxes(w, h, r, r);
                FxOp::Unsharp { rx, ry, amount: f(e, "amount", cx) / 100.0, threshold: f(e, "threshold", cx) / 255.0 }
            }
            "drop_shadow" => {
                let c = color(e, "color", cx);
                let direction = (f(e, "direction", cx) as f64).to_radians();
                let distance = f(e, "distance", cx) as f64 * cx.px_scale as f64;
                let softness = f(e, "softness", cx) * cx.px_scale * 0.5;
                let (rx, ry) = gaussian_boxes(w, h, softness, softness);
                FxOp::DropShadow {
                    dx: direction.sin() * distance,
                    dy: -direction.cos() * distance,
                    color: dec([c[0], c[1], c[2]]),
                    opacity: f(e, "opacity", cx) / 100.0,
                    rx,
                    ry,
                    only: b(e, "only"),
                }
            }
            "alpha_glow" => {
                let radius = f(e, "glow", cx) * cx.px_scale * 0.5;
                let (rx, ry) = if radius > 0.3 { gaussian_boxes(w, h, radius, radius) } else { (Vec::new(), Vec::new()) };
                let start = color(e, "start_color", cx);
                let end = color(e, "end_color", cx);
                FxOp::AlphaGlow {
                    start: dec([start[0], start[1], start[2]]),
                    end: dec([end[0], end[1], end[2]]),
                    brightness: f(e, "brightness", cx) / 255.0,
                    use_end: b(e, "use_end"),
                    fade: b(e, "fade_out"),
                    enabled: radius > 0.3,
                    rx,
                    ry,
                }
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
                use crate::effects::{curve_param, is_identity_curve, wheel_rgb};
                let (basic_on, creative_on, vignette_on) = (on(e, "basic_on"), on(e, "creative_on"), on(e, "vignette_on"));
                let gs = grade_space(e, cx, "hdr_white");
                let is_hdr = gs.is_hdr();
                let has_input_lut = basic_on && !text(e, "input_lut").is_empty();
                let has_look_lut = creative_on && !text(e, "look_lut").is_empty();
                let look = if creative_on && !has_look_lut { choice(e, "look") } else { 0 };
                let sharpen = if creative_on { f(e, "sharpen", cx) / 100.0 } else { 0.0 };

                let curves_on = on(e, "curves_on");
                let has_curves = curves_on && {
                    ["curve_luma", "curve_red", "curve_green", "curve_blue"].iter().any(|id| curve_param(e, id).is_some_and(|c| !is_identity_curve(&c)))
                        || ["hue_vs_sat", "hue_vs_hue", "hue_vs_luma", "luma_vs_sat", "sat_vs_sat"]
                            .iter()
                            .any(|id| curve_param(e, id).is_some_and(|c| !c.is_empty()))
                };

                let wheels_on = on(e, "wheels_on");
                let has_wheels = wheels_on && {
                    let v2 = |id: &str| e.param(id).map(|p| p.vec2_at(cx.t)).unwrap_or_default();
                    let (ws, wm, wh) = (wheel_rgb(v2("wheel_shadows")), wheel_rgb(v2("wheel_midtones")), wheel_rgb(v2("wheel_highlights")));
                    let (ls, lm, lh) = (f(e, "wheel_shadows_l", cx) / 100.0, f(e, "wheel_midtones_l", cx) / 100.0, f(e, "wheel_highlights_l", cx) / 100.0);
                    ws.iter().chain(&wm).chain(&wh).any(|v| v.abs() > 1e-5) || (ls.abs() + lm.abs() + lh.abs() > 1e-5)
                };

                let hsl_on = b(e, "hsl_on");

                let gpu_capable = !is_hdr && !has_input_lut && !has_look_lut && look == 0 && sharpen.abs() <= 1e-3 && !has_curves && !has_wheels && !hsl_on;

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
                    st: [st[0], st[1], st[2]],
                    ht: [ht[0], ht[1], ht[2]],
                    vignette_on,
                    va,
                    vmid,
                    vround,
                    vfeather,
                    aspect,
                    gpu_capable,
                }
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
            FxOp::ColorKey { key_ycc, tol, soft } => fin(key_ycc) && fin(&[*tol, *soft]),
            FxOp::LogoCutout { background, threshold, softness, .. } => fin(background) && fin(&[*threshold, *softness]),
            FxOp::LumaKey { threshold, cutoff } => fin(&[*threshold, *cutoff]),
            FxOp::Cineon { black, white, ib, iw, gamma, roll, .. } => fin(&[*black, *white, *ib, *iw, *gamma, *roll]),
            FxOp::Noise { amount, grain, .. } => amount.is_finite() && grain.is_finite() && *grain >= 1.0,
            FxOp::Spatial(op) => op.gpu_ok(),
            FxOp::Procedural(op) => op.gpu_ok(),
            FxOp::Filter(op) => op.gpu_ok(),
            FxOp::Stylize(op) => op.gpu_ok(),
            FxOp::LegacyWipe(op) => op.gpu_ok(),
            FxOp::Invert { blend, .. } => blend.is_finite(),
            FxOp::Posterize { n } => n.is_finite(),
            FxOp::Gaussian { .. } => true,
            FxOp::ChannelBlur { .. } => true,
            FxOp::DirectionalBlur { dx, dy, .. } => fin(&[*dx, *dy]),
            FxOp::Unsharp { amount, threshold, .. } => fin(&[*amount, *threshold]),
            FxOp::DropShadow { dx, dy, color, opacity, .. } => {
                dx.is_finite() && dy.is_finite() && dx.abs() <= f32::MAX as f64 && dy.abs() <= f32::MAX as f64 && opacity.is_finite() && fin(color)
            }
            FxOp::AlphaGlow { start, end, brightness, .. } => fin(start) && fin(end) && brightness.is_finite(),
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
                    let inside = if soft <= 0.0 {
                        // Match the feathered function's limiting value at the cutoffs.
                        if l > lo && l < hi { 1.0 } else { 0.0 }
                    } else {
                        ((l - lo) / soft).clamp(0.0, 1.0).min(((hi - l) / soft).clamp(0.0, 1.0))
                    };
                    let v = if inv { 1.0 - inside } else { inside };
                    [v, v, v]
                });
            }
            FxOp::ColorKey { key_ycc, tol, soft } => {
                let (key, tol, soft) = (*key_ycc, *tol, *soft);
                img.px.par_chunks_mut(4).for_each(|p| {
                    if p[3] <= 0.0 {
                        return;
                    }
                    let c = enc([p[0] / p[3], p[1] / p[3], p[2] / p[3]]);
                    let y = filmcraft_color::rgb_to_ycbcr(c[0], c[1], c[2], filmcraft_color::Matrix::Bt709);
                    let d = ((y[1] - key[1]).powi(2) + (y[2] - key[2]).powi(2)).sqrt() + (y[0] - key[0]).abs() * 0.15;
                    let a = ((d - tol) / soft).clamp(0.0, 1.0);
                    let lo = dec(c);
                    let a = p[3] * a;
                    p[0] = lo[0] * a;
                    p[1] = lo[1] * a;
                    p[2] = lo[2] * a;
                    p[3] = a;
                });
            }
            FxOp::LumaKey { threshold, cutoff } => {
                let (threshold, cutoff) = (*threshold, *cutoff);
                img.px.par_chunks_mut(4).for_each(|p| {
                    if p[3] <= 0.0 {
                        return;
                    }
                    let l = linear_to_srgb(luma709(p[0] / p[3], p[1] / p[3], p[2] / p[3]).max(0.0));
                    let a = if l <= cutoff {
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
            FxOp::Cineon { mode, black, white, ib, iw, gamma, roll } => {
                img.map_rgb(|c, _, _| dec(enc(c).map(|v| cineon_value(v, *mode, *black, *white, *ib, *iw, *gamma, *roll))));
            }
            FxOp::Noise { amount, grain, seed, colored, clip } => {
                if *amount <= 0.0 {
                    return;
                }
                let (amount, grain, seed, colored, clip) = (*amount, *grain, *seed, *colored, *clip);
                let w = img.w;
                img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
                    for x in 0..w {
                        let p = &mut row[x * 4..x * 4 + 4];
                        let a = p[3];
                        if a <= 0.0 {
                            continue;
                        }
                        let (gx, gy) = ((x as f32 / grain) as usize, (y as f32 / grain) as usize);
                        let hash = |z| crate::effects::hash3(gx, gy, z) - 0.5;
                        let n0 = hash(seed);
                        let noise = if colored { [n0, hash(seed.wrapping_add(7777)), hash(seed.wrapping_add(99_991))] } else { [n0; 3] };
                        let c = enc([p[0] / a, p[1] / a, p[2] / a]);
                        let mut out = [c[0] + noise[0] * amount, c[1] + noise[1] * amount, c[2] + noise[2] * amount];
                        if clip {
                            out = out.map(|v| v.clamp(0.0, 1.0));
                        }
                        let out = dec(out);
                        p[0] = out[0] * a;
                        p[1] = out[1] * a;
                        p[2] = out[2] * a;
                    }
                });
            }
            FxOp::Spatial(op) => op.apply(img),
            FxOp::Procedural(op) => op.apply(img),
            FxOp::Filter(op) => op.apply(img),
            FxOp::Stylize(op) => op.apply(img),
            FxOp::LegacyWipe(op) => op.apply(img),
            FxOp::LogoCutout { background, threshold, softness, unmultiply, invert } => {
                img.px.par_chunks_mut(4).for_each(|p| {
                    if p[3] <= 1e-6 {
                        return;
                    }
                    let v = enc(Image::unpremul([p[0], p[1], p[2], p[3]]));
                    let mut a0 = 0.0f32;
                    for k in 0..3 {
                        let d = v[k] - background[k];
                        let room = if d >= 0.0 { 1.0 - background[k] } else { background[k] };
                        if room > 1e-6 {
                            a0 = a0.max(d.abs() / room);
                        }
                    }
                    let a0 = a0.clamp(0.0, 1.0);
                    let mut a = ((a0 - threshold) / ((1.0 - threshold).max(1e-3) * softness)).clamp(0.0, 1.0);
                    if *invert {
                        a = 1.0 - a;
                    }
                    let col = if *unmultiply && a0 > 1e-4 && !*invert {
                        std::array::from_fn(|k| (background[k] + (v[k] - background[k]) / a0).clamp(0.0, 1.0))
                    } else {
                        v
                    };
                    let l = dec(col);
                    let na = a * p[3];
                    p.copy_from_slice(&[l[0] * na, l[1] * na, l[2] * na, na]);
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
            FxOp::ChannelBlur { rx, ry, repeat } => {
                for channel in 0..4 {
                    let mut plane = Image { w: img.w, h: img.h, px: img.px.as_chunks::<4>().0.iter().flat_map(|p| [p[channel], 0.0, 0.0, 0.0]).collect() };
                    crate::effects::box_blur(&mut plane, &rx[channel], &ry[channel], *repeat);
                    img.px.par_chunks_mut(4).zip(plane.px.par_chunks(4)).for_each(|(p, q)| p[channel] = q[0]);
                }
            }
            FxOp::DirectionalBlur { dx, dy, steps } => crate::effects::directional_taps(img, *dx, *dy, *steps as usize),
            FxOp::Unsharp { rx, ry, amount, threshold } => crate::effects::unsharp_boxes(img, rx, ry, *amount, *threshold),
            FxOp::DropShadow { dx, dy, color, opacity, rx, ry, only } => {
                let mut shadow = img.transformed(img.w, img.h, &Affine::translate(*dx, *dy));
                shadow.px.par_chunks_mut(4).for_each(|p| {
                    let alpha = p[3] * opacity;
                    p[..3].copy_from_slice(&[color[0] * alpha, color[1] * alpha, color[2] * alpha]);
                    p[3] = alpha;
                });
                crate::effects::box_blur(&mut shadow, rx, ry, false);
                if !only {
                    crate::blend::composite(&mut shadow, img, 1.0, crate::blend::Blend::Normal);
                }
                *img = shadow;
            }
            FxOp::AlphaGlow { start, end, brightness, use_end, fade, enabled, rx, ry } => {
                if !enabled {
                    return;
                }
                let mut plane = Image { w: img.w, h: img.h, px: img.px.as_chunks::<4>().0.iter().flat_map(|p| [0.0, 0.0, 0.0, p[3]]).collect() };
                crate::effects::box_blur(&mut plane, rx, ry, false);
                img.px.par_chunks_mut(4).zip(plane.px.as_chunks::<4>().0.par_iter()).for_each(|(p, q)| {
                    let gv = (q[3] * 2.0).min(1.0);
                    let k = if *fade {
                        gv * brightness
                    } else if gv > 0.02 {
                        *brightness
                    } else {
                        0.0
                    };
                    if k <= 0.0 || p[3] >= 1.0 {
                        return;
                    }
                    let col = if *use_end {
                        [end[0] + (start[0] - end[0]) * gv, end[1] + (start[1] - end[1]) * gv, end[2] + (start[2] - end[2]) * gv]
                    } else {
                        *start
                    };
                    let a = 1.0 - p[3];
                    p[0] += col[0] * k * a;
                    p[1] += col[1] * k * a;
                    p[2] += col[2] * k * a;
                    p[3] += k * a;
                });
            }
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
                        let nx = (x as f32 / w - 0.5) * 2.0 * lumetri_vignette_aspect(aspect, vround);
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

/// Horizontal scale of the Lumetri vignette's coordinates for a Roundness of `vround`
/// (−1 … 1) on a frame `aspect` wide. At 0 the vignette is an ellipse that follows the
/// frame; positive values move it towards a circle (1 = a true circle), negative values
/// stretch it into a wider oval. Non-finite or out-of-range input never yields NaN.
pub fn lumetri_vignette_aspect(aspect: f32, vround: f32) -> f32 {
    if !aspect.is_finite() || aspect <= 0.0 || !vround.is_finite() {
        return 1.0;
    }
    aspect.powf(vround.clamp(-1.0, 1.0))
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

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{ParamValue, find_effect};
    use filmcraft_time::Tick;

    fn catalog_context() -> FxCtx<'static> {
        FxCtx {
            t: Tick::ZERO,
            px_scale: 1.0,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            frame_rate: 24.0,
            working: filmcraft_color::WorkingSpace::Rec709,
        }
    }

    #[test]
    fn catalog_classifies_every_live_video_id_without_claiming_gpu_coverage() {
        // Test-only inventory: adding a registry ID requires a deliberate route review.
        let unsupported = [
            "auto_align",
            "auto_reframe",
            "bokeh_blur",
            "brush_strokes",
            "clip_name",
            "clone",
            "compound_blur",
            "echo",
            "echo_glow",
            "edge_glow",
            "graphic_shape",
            "graphic_text",
            "focus_blur",
            "glint",
            "gradient_wipe_legacy",
            "light_leaks",
            "lighting_effects",
            "lightning",
            "long_shadow",
            "metadata_burnin",
            "motion",
            "opacity",
            "paint_bucket",
            "posterize_time",
            "roughen_edges",
            "simple_text",
            "spacer",
            "stroke",
            "time_remap",
            "timecode",
            "track_matte",
            "ultra_key",
            "volumetric_rays",
            "vr_blur",
            "vr_denoise",
            "vr_digital_glitch",
            "vr_fractal_noise",
            "vr_glow",
            "vr_sharpen",
            "warp_stabilizer",
            "wonder_glow",
            "write_on",
        ];
        let expected: std::collections::BTreeSet<_> = GPU_EFFECTS.iter().copied().chain(unsupported).collect();
        let live: std::collections::BTreeSet<_> =
            filmcraft_project::effect_defs().iter().filter(|d| d.kind == filmcraft_project::EffectKind::Video).map(|d| d.id).collect();
        assert_eq!(live, expected, "review classification when the live video registry changes");
        for def in filmcraft_project::effect_defs().iter().filter(|d| d.kind == filmcraft_project::EffectKind::Video) {
            let outcome = evaluate_gpu_effect(&def.instance(), &catalog_context(), 9, 7);
            match outcome {
                EffectGpuOutcome::Eligible(op) => assert!(op.gpu_ok(), "{}", def.id),
                EffectGpuOutcome::Ineligible(EffectGpuFailure::UnsupportedOperation) => {
                    // the clip-timed ops need the clip envelope, which this context lacks: they refuse the GPU route
                    assert!(!GPU_EFFECTS.contains(&def.id) || matches!(def.id, "grow" | "shrink" | "spin" | "move"), "{} lost its existing GPU route", def.id);
                }
                EffectGpuOutcome::Ineligible(EffectGpuFailure::UnsupportedCase) => {
                    assert!(matches!(def.id, "lumetri" | "transform"), "{}", def.id);
                }
                other => panic!("{} default has unexpected classification: {other:?}", def.id),
            }
        }
    }

    #[test]
    fn cineon_fxop_matches_vfx_oracle_for_all_conversion_modes() {
        for mode in 0..3 {
            let mut e = find_effect("cineon_converter").unwrap().instance();
            e.params.get_mut("conversion").unwrap().value = ParamValue::Choice(mode);
            e.params.get_mut("white10").unwrap().value = ParamValue::Float(700.0);
            e.params.get_mut("black_internal").unwrap().value = ParamValue::Float(8.0);
            e.params.get_mut("white_internal").unwrap().value = ParamValue::Float(240.0);
            e.params.get_mut("rolloff").unwrap().value = ParamValue::Float(80.0);
            let cx = catalog_context();
            let mut expected = Image { w: 4, h: 1, px: vec![0.0, 0.0, 0.0, 1.0, 0.04, 0.15, 0.3, 0.5, 0.8, 0.4, 0.1, 1.0, 1.5, 1.2, 0.6, 1.0] };
            let mut actual = expected.clone();
            assert!(crate::vfx::apply(&mut expected, &e, &cx));
            let op = FxOp::eval(&e, &cx, 4, 1).unwrap();
            assert!(op.gpu_ok());
            op.apply(&mut actual);
            for (a, b) in expected.px.iter().zip(actual.px.iter()) {
                assert!((a - b).abs() <= 1e-6, "mode {mode}: {a} != {b}");
            }
        }
    }

    #[test]
    fn noise_fxop_matches_vfx_oracle_for_current_and_legacy_ids() {
        for id in ["noise", "noise_legacy"] {
            for colored in [false, true] {
                let mut e = find_effect(id).unwrap().instance();
                e.params.get_mut("amount").unwrap().value = ParamValue::Float(47.0);
                e.params.get_mut("color").unwrap().value = ParamValue::Bool(colored);
                e.params.get_mut("clip").unwrap().value = ParamValue::Bool(false);
                if id == "noise" {
                    e.params.get_mut("grain_size").unwrap().value = ParamValue::Float(2.5);
                }
                let cx = FxCtx { t: Tick(-7), px_scale: 0.5, ..catalog_context() };
                let input = Image {
                    w: 5,
                    h: 2,
                    px: vec![
                        0.0, 0.0, 0.0, 0.0, 0.01, 0.02, 0.03, 1e-8, 0.1, 0.2, 0.3, 0.5, 1.5, 0.9, 0.5, 1.0, 0.8, 0.2, 0.1, 1.0, 0.4, 0.6, 0.9, 0.25, 0.2, 0.1,
                        0.05, 0.75, 0.9, 0.9, 0.9, 1.0, 0.3, 0.6, 0.9, 0.4, 1.2, 0.8, 0.1, 1.0,
                    ],
                };
                let mut expected = input.clone();
                let mut actual = input;
                assert!(crate::vfx::apply(&mut expected, &e, &cx));
                let op = FxOp::eval(&e, &cx, 5, 2).unwrap();
                assert!(op.gpu_ok());
                op.apply(&mut actual);
                for (a, b) in expected.px.iter().zip(&actual.px) {
                    assert!((a - b).abs() <= 1e-6, "{id}, colored={colored}: {a} != {b}");
                }
            }
        }
    }

    #[test]
    fn spatial_and_procedural_fxops_match_vfx_oracle_defaults() {
        let ids = [
            "mosaic",
            "mosaic_legacy",
            "magnify",
            "magnify_legacy",
            "replicate",
            "twirl",
            "twirl_legacy",
            "spherize",
            "corner_pin",
            "turbulent_displace",
            "lens_distortion",
            "wave_warp",
            "rounded_crop",
            "rgb_split",
            "vr_chromatic_aberrations",
            "vr_color_gradients",
            "vr_projection",
            "vr_rotate_sphere",
            "vr_plane_to_sphere",
            "checkerboard",
            "grid",
            "circle",
            "ellipse",
            "ramp",
            "gradient",
            "four_color_gradient",
            "cell_pattern",
            "emboss",
            "color_emboss",
            "find_edges",
            "strobe",
        ];
        let mut pixels = Vec::new();
        for i in 0..(17 * 11) {
            let a = match i % 5 {
                0 => 0.0,
                1 => 0.5,
                2 => 1e-8,
                _ => 1.0,
            };
            let x = (i % 17) as f32 / 16.0;
            let y = (i / 17) as f32 / 10.0;
            pixels.extend_from_slice(&[x * a, y * a, (1.0 - x) * a, a]);
        }
        for id in ids {
            let e = find_effect(id).unwrap().instance();
            let cx = FxCtx { seconds: 0.0, ..catalog_context() };
            let mut expected = Image { w: 17, h: 11, px: pixels.clone() };
            let mut actual = expected.clone();
            if !legacy_cpu_oracle(&mut expected, &e, &cx) {
                assert!(crate::vfx::apply(&mut expected, &e, &cx), "{id}: missing CPU oracle");
            }
            let op = FxOp::eval(&e, &cx, 17, 11).unwrap_or_else(|| panic!("{id}: missing GPU op"));
            assert!(op.gpu_ok(), "{id}: default route disabled");
            op.apply(&mut actual);
            for (a, b) in expected.px.iter().zip(&actual.px) {
                assert!((a - b).abs() <= 1e-6, "{id}: {a} != {b}");
            }
        }
    }

    #[test]
    fn grid_cpu_op_matches_integer_coordinate_reference_for_default_and_custom_settings() {
        for custom in [false, true] {
            let mut effect = find_effect("grid").unwrap().instance();
            if custom {
                effect.params.get_mut("size").unwrap().value = ParamValue::Float(3.0);
                effect.params.get_mut("border").unwrap().value = ParamValue::Float(1.0);
                effect.params.get_mut("opacity").unwrap().value = ParamValue::Float(73.0);
            }
            let cx = catalog_context();
            let input = Image {
                w: 7,
                h: 5,
                px: (0..35)
                    .flat_map(|i| {
                        let alpha = if i % 3 == 0 { 0.35 } else { 1.0 };
                        [0.08 * alpha, 0.16 * alpha, 0.24 * alpha, alpha]
                    })
                    .collect(),
            };
            let mut expected = input.clone();
            let mut actual = input;
            assert!(legacy_cpu_oracle(&mut expected, &effect, &cx));
            let op = FxOp::eval(&effect, &cx, 7, 5).unwrap();
            op.apply(&mut actual);
            for (a, b) in expected.px.iter().zip(&actual.px) {
                assert!((a - b).abs() <= 1e-6, "custom={custom}: {a} != {b}");
            }
        }
    }

    #[test]
    fn catalog_distinguishes_disabled_unknown_and_unsupported() {
        let mut effect = find_effect("echo").unwrap().instance();
        assert_eq!(evaluate_gpu_effect(&effect, &catalog_context(), 9, 7), EffectGpuOutcome::Ineligible(EffectGpuFailure::UnsupportedOperation));
        effect.enabled = false;
        assert_eq!(evaluate_gpu_effect(&effect, &catalog_context(), 9, 7), EffectGpuOutcome::Disabled);
        effect.effect = "unknown_effect".into();
        effect.enabled = true;
        assert_eq!(evaluate_gpu_effect(&effect, &catalog_context(), 9, 7), EffectGpuOutcome::Ineligible(EffectGpuFailure::UnknownId));
    }

    #[test]
    fn vr_chromatic_op_matches_oracle_for_each_view_layout() {
        for layout in 0..=2 {
            let mut effect = find_effect("vr_chromatic_aberrations").unwrap().instance();
            for (key, value) in [
                ("red", ParamValue::Float(31.0)),
                ("green", ParamValue::Float(-23.0)),
                ("blue", ParamValue::Float(47.0)),
                ("center_x", ParamValue::Float(37.0)),
                ("center_y", ParamValue::Float(-19.0)),
                ("falloff", ParamValue::Float(73.0)),
                ("inverse", ParamValue::Bool(true)),
                ("frame_layout", ParamValue::Choice(layout)),
            ] {
                effect.params.get_mut(key).unwrap().value = value;
            }
            let cx = catalog_context();
            let mut expected = Image::new(17, 11);
            for y in 0..expected.h {
                for x in 0..expected.w {
                    let a = if (x + y) % 4 == 0 {
                        0.0
                    } else if x % 3 == 0 {
                        0.45
                    } else {
                        1.0
                    };
                    let i = (y * expected.w + x) * 4;
                    expected.px[i..i + 4].copy_from_slice(&[x as f32 / 17.0 * a, y as f32 / 11.0 * a, 0.8 * a, a]);
                }
            }
            let mut actual = expected.clone();
            assert!(crate::vfx::apply(&mut expected, &effect, &cx));
            let op = FxOp::eval(&effect, &cx, 17, 11).unwrap();
            assert!(op.gpu_ok());
            op.apply(&mut actual);
            for (a, b) in expected.px.iter().zip(actual.px.iter()) {
                assert!((a - b).abs() <= 2e-5, "layout={layout}: {a} != {b}");
            }
        }
    }

    #[test]
    fn vr_color_gradients_op_matches_oracle_for_all_modes_and_view_layouts() {
        for layout in 0..=2 {
            for mode in 0..=4 {
                let mut effect = find_effect("vr_color_gradients").unwrap().instance();
                for (key, value) in [
                    ("c1", ParamValue::Color([0.85, 0.2, 0.1, 1.0])),
                    ("p1_lon", ParamValue::Float(-115.0)),
                    ("p1_lat", ParamValue::Float(25.0)),
                    ("c2", ParamValue::Color([0.1, 0.8, 0.2, 1.0])),
                    ("p2_lon", ParamValue::Float(8.0)),
                    ("p2_lat", ParamValue::Float(-33.0)),
                    ("c3", ParamValue::Color([0.15, 0.3, 0.9, 1.0])),
                    ("p3_lon", ParamValue::Float(126.0)),
                    ("p3_lat", ParamValue::Float(18.0)),
                    ("blend", ParamValue::Float(73.0)),
                    ("opacity", ParamValue::Float(61.0)),
                    ("mode", ParamValue::Choice(mode)),
                    ("frame_layout", ParamValue::Choice(layout)),
                ] {
                    effect.params.get_mut(key).unwrap().value = value;
                }
                let cx = catalog_context();
                let mut expected = Image::new(17, 11);
                for y in 0..expected.h {
                    for x in 0..expected.w {
                        let a = if (x + y) % 4 == 0 {
                            0.0
                        } else if x % 3 == 0 {
                            0.45
                        } else {
                            1.0
                        };
                        let i = (y * expected.w + x) * 4;
                        expected.px[i..i + 4].copy_from_slice(&[x as f32 / 17.0 * a, y as f32 / 11.0 * a, 0.8 * a, a]);
                    }
                }
                let mut actual = expected.clone();
                assert!(crate::vfx::apply(&mut expected, &effect, &cx));
                let op = FxOp::eval(&effect, &cx, 17, 11).unwrap();
                assert!(op.gpu_ok());
                op.apply(&mut actual);
                for (a, b) in expected.px.iter().zip(actual.px.iter()) {
                    assert!((a - b).abs() <= 2e-5, "layout={layout}, mode={mode}: {a} != {b}");
                }
            }
        }
    }

    #[test]
    fn vr_projection_and_sphere_rotation_match_oracle_for_all_view_layouts() {
        for (id, stretch) in [("vr_projection", false), ("vr_projection", true), ("vr_rotate_sphere", false)] {
            for layout in 0..=2 {
                let mut effect = find_effect(id).unwrap().instance();
                for (key, value) in [
                    ("tilt", ParamValue::Float(-23.0)),
                    ("pan", ParamValue::Float(41.0)),
                    ("roll", ParamValue::Float(17.0)),
                    ("frame_layout", ParamValue::Choice(layout)),
                ] {
                    effect.params.get_mut(key).unwrap().value = value;
                }
                if id == "vr_projection" {
                    effect.params.get_mut("fov").unwrap().value = ParamValue::Float(235.0);
                    effect.params.get_mut("stretch").unwrap().value = ParamValue::Bool(stretch);
                }
                let cx = catalog_context();
                let mut expected = Image::new(17, 11);
                for y in 0..expected.h {
                    for x in 0..expected.w {
                        let a = if (x + y) % 4 == 0 {
                            0.0
                        } else if x % 3 == 0 {
                            0.45
                        } else {
                            1.0
                        };
                        let i = (y * expected.w + x) * 4;
                        expected.px[i..i + 4].copy_from_slice(&[x as f32 / 17.0 * a, y as f32 / 11.0 * a, 0.8 * a, a]);
                    }
                }
                let mut actual = expected.clone();
                assert!(crate::vfx::apply(&mut expected, &effect, &cx));
                let op = FxOp::eval(&effect, &cx, 17, 11).unwrap();
                assert!(op.gpu_ok());
                op.apply(&mut actual);
                for (a, b) in expected.px.iter().zip(actual.px.iter()) {
                    assert!((a - b).abs() <= 2e-5, "{id}, layout={layout}, stretch={stretch}: {a} != {b}");
                }
            }
        }
    }

    #[test]
    fn vr_plane_to_sphere_matches_oracle_for_all_view_layouts_and_feather() {
        for layout in 0..=2 {
            for feather in [0.0, 35.0] {
                let mut effect = find_effect("vr_plane_to_sphere").unwrap().instance();
                for (key, value) in [
                    ("scale", ParamValue::Float(105.0)),
                    ("tilt", ParamValue::Float(19.0)),
                    ("pan", ParamValue::Float(-31.0)),
                    ("roll", ParamValue::Float(12.0)),
                    ("feather", ParamValue::Float(feather)),
                    ("frame_layout", ParamValue::Choice(layout)),
                ] {
                    effect.params.get_mut(key).unwrap().value = value;
                }
                let cx = catalog_context();
                let mut expected = Image::new(17, 11);
                for y in 0..expected.h {
                    for x in 0..expected.w {
                        let a = if (x + y) % 4 == 0 {
                            0.0
                        } else if x % 3 == 0 {
                            0.45
                        } else {
                            1.0
                        };
                        let i = (y * expected.w + x) * 4;
                        expected.px[i..i + 4].copy_from_slice(&[x as f32 / 17.0 * a, y as f32 / 11.0 * a, 0.8 * a, a]);
                    }
                }
                let mut actual = expected.clone();
                assert!(crate::vfx::apply(&mut expected, &effect, &cx));
                let op = FxOp::eval(&effect, &cx, 17, 11).unwrap();
                assert!(op.gpu_ok());
                op.apply(&mut actual);
                for (a, b) in expected.px.iter().zip(actual.px.iter()) {
                    assert!((a - b).abs() <= 2e-5, "layout={layout}, feather={feather}: {a} != {b}");
                }
            }
        }
    }

    #[test]
    fn median_radius_above_exact_gpu_kernel_is_ineligible() {
        let mut effect = find_effect("median").unwrap().instance();
        effect.params.get_mut("radius").unwrap().value = ParamValue::Float(2.0);
        assert_eq!(evaluate_gpu_effect(&effect, &catalog_context(), 9, 7), EffectGpuOutcome::Ineligible(EffectGpuFailure::UnsupportedCase));
    }

    #[test]
    fn catalog_rejects_nonfinite_evaluated_parameters_before_clamping() {
        let mut effect = find_effect("gaussian_blur").unwrap().instance();
        effect.params.get_mut("blurriness").unwrap().value = ParamValue::Float(f64::NAN);
        assert_eq!(
            evaluate_gpu_effect(&effect, &catalog_context(), 9, 7),
            EffectGpuOutcome::Ineligible(EffectGpuFailure::InvalidParameter { parameter: "blurriness".into() }),
        );
    }

    #[test]
    fn catalog_requirements_describe_history_track_mask_and_lut_inputs() {
        let echo = find_effect("echo").unwrap().instance();
        assert!(effect_requirements(&echo).history);
        let matte = find_effect("track_matte").unwrap().instance();
        assert_eq!(effect_requirements(&matte).sampled_inputs, 2);
        assert!(effect_requirements(&matte).mask);
        let mut lumetri = find_effect("lumetri").unwrap().instance();
        lumetri.params.get_mut("input_lut").unwrap().value = ParamValue::Text("input.cube".into());
        assert!(effect_requirements(&lumetri).lut);
        lumetri.params.get_mut("basic_on").unwrap().value = ParamValue::Bool(false);
        assert!(!effect_requirements(&lumetri).lut);
    }

    #[test]
    fn catalog_preserves_the_existing_evaluated_operation() {
        let effect = find_effect("brightness_contrast").unwrap().instance();
        let expected = FxOp::eval(&effect, &catalog_context(), 9, 7).unwrap();
        assert_eq!(evaluate_gpu_effect(&effect, &catalog_context(), 9, 7), EffectGpuOutcome::Eligible(expected));
    }

    #[test]
    fn catalog_transform_shutter_does_not_claim_plain_resample_parity() {
        let mut effect = find_effect("transform").unwrap().instance();
        effect.params.get_mut("shutter_override").unwrap().value = ParamValue::Bool(true);
        effect.params.get_mut("shutter_angle").unwrap().value = ParamValue::Float(180.0);
        assert_eq!(evaluate_gpu_effect(&effect, &catalog_context(), 9, 7), EffectGpuOutcome::Ineligible(EffectGpuFailure::UnsupportedOperation));
    }

    #[test]
    fn extract_zero_softness_has_no_fractional_edge() {
        let cx = FxCtx {
            t: Tick::ZERO,
            px_scale: 1.0,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            frame_rate: 24.0,
            working: filmcraft_color::WorkingSpace::Rec709,
        };
        // Both sides of each cutoff, including samples inside the former artificial fade.
        let lo = 90.0 / 255.0;
        let hi = 160.0 / 255.0;
        let samples = [lo - 0.00005, lo + 0.00005, hi - 0.00005, hi + 0.00005];
        for softness in [0.0, -1.0] {
            for invert in [false, true] {
                for alpha in [1.0, 0.5] {
                    let mut e = find_effect("extract").unwrap().instance();
                    for (key, value) in [("black", 90.0), ("white", 160.0), ("softness", softness)] {
                        e.params.get_mut(key).unwrap().value = ParamValue::Float(value);
                    }
                    e.params.get_mut("invert").unwrap().value = ParamValue::Bool(invert);
                    let mut img = Image {
                        w: 4,
                        h: 1,
                        px: samples
                            .iter()
                            .flat_map(|v| {
                                let c = filmcraft_color::srgb_to_linear(*v) * alpha;
                                [c, c, c, alpha]
                            })
                            .collect(),
                    };
                    FxOp::eval(&e, &cx, 4, 1).unwrap().apply(&mut img);
                    for (pixel, inside) in img.px.as_chunks::<4>().0.iter().zip([false, true, true, false]) {
                        let value = if inside != invert { alpha } else { 0.0 };
                        assert_eq!(*pixel, [value, value, value, alpha], "softness={softness}, invert={invert}");
                    }
                }
            }
        }
    }

    #[test]
    fn color_key_clamps_hdr_and_negative_rgb_like_legacy_oracle() {
        let cx = catalog_context();
        let e = find_effect("color_key").unwrap().instance();
        let input = Image { w: 2, h: 1, px: vec![2.0, -0.1, 0.5, 0.5, 0.125, 0.25, 0.375, 0.0] };
        let mut expected = input.clone();
        crate::effects::key(&mut expected, &e, &cx);
        let mut actual = input;
        FxOp::eval(&e, &cx, 2, 1).unwrap().apply(&mut actual);
        assert_eq!(actual.px, expected.px);
    }
}
