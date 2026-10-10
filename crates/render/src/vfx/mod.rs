//! CPU implementations of the Premiere 26 video-effect catalogue added in M5.11 (see
//! `filmcraft_project::effect::vfx` for the definitions), plus the rebuilt versions of a few core
//! effects (Ultra Key, Lens Flare, Noise, Mosaic, Simple Text, Track Matte Key).
//!
//! Conventions match [`crate::effects`]: images are premultiplied linear f32; spatial parameters
//! are full-resolution clip pixels scaled by `FxCtx::px_scale`; colour maths that artists expect
//! in display space (keys, limiters, CDL) runs on sRGB-encoded straight colour.
//!
//! Effects that need more than the current frame — Posterize Time and Echo (other frames), Track
//! Matte Key / Compound Blur / Gradient Wipe (another track), Auto Reframe and Warp Stabilizer
//! (frames for analysis, sequence geometry) — read it through [`FxEnv`]; without an environment
//! (isolated tests, adjustment layers) they leave the image unchanged.

mod blur;
mod color;
pub(crate) use color::cdl;
mod distort;
mod immersive;
mod lights;
mod stylize;
mod temporal;
mod text;

#[cfg(test)]
mod tests;

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::{EffectInstance, ParamValue, Project, Sequence, TrackItem};
use filmcraft_time::{Tick, TimeDisplay, format_time};
use rayon::prelude::*;

use crate::effects::FxCtx;
use crate::image::Image;
use crate::{RenderOptions, SourceProvider};

pub use temporal::{StabilizerPath, clear_stabilizer_cache, stabilizer_path};

/// What an effect may know about the clip beyond its own pixels.
pub trait FxEnv: Sync {
    /// The clip's layer `dt` timeline seconds from now, with the effects before `upto` applied,
    /// at the current working size. Temporal effects among those are skipped (no recursion).
    fn frame(&self, dt: f64, upto: &EffectInstance) -> Option<Image>;
    /// The clip's raw picture `dt` seconds from now at `scale` × its source size (analysis).
    fn source_frame(&self, dt: f64, scale: f32) -> Option<Image>;
    /// Video track `index` (0 = V1) rendered alone at this time, output-sized.
    fn track(&self, index: usize) -> Option<Image>;
    /// Maps working-layer pixels to output pixels (Motion included).
    fn layer_to_output(&self) -> Affine;
    /// Clip duration on the timeline, in seconds.
    fn clip_seconds(&self) -> f64;
    /// Seconds from the clip's start to now.
    fn clip_offset(&self) -> f64;
    fn frame_rate(&self) -> f64;
    fn media_timecode(&self) -> String;
    fn file_name(&self) -> String;
    fn sequence_name(&self) -> String;
    /// Full-resolution sequence frame size.
    fn sequence_size(&self) -> (u32, u32);
    /// Full-resolution clip source size.
    fn source_size(&self) -> (u32, u32);
    /// Identity of the clip and its source range (cache key for analyses).
    fn clip_key(&self) -> u64;
}

/// The environment of a track item being rendered by [`crate::item_layer`].
pub(crate) struct ItemEnv<'a> {
    pub project: &'a Project,
    pub seq: &'a Sequence,
    pub item: &'a TrackItem,
    pub t: Tick,
    pub opts: RenderOptions,
    pub sources: &'a dyn SourceProvider,
    pub want: f32,
    pub layer_size: (usize, usize),
    pub layer_to_output: Affine,
    pub tc: &'a str,
}

fn resize(img: Image, w: usize, h: usize) -> Image {
    if img.w == w && img.h == h {
        return img;
    }
    let m = Affine::scale(w as f64 / img.w.max(1) as f64, h as f64 / img.h.max(1) as f64);
    img.transformed(w, h, &m)
}

/// Effects that read other frames (skipped when rendering another frame for one of them).
pub fn is_temporal(id: &str) -> bool {
    matches!(id, "posterize_time" | "echo" | "warp_stabilizer" | "auto_reframe")
}

impl FxEnv for ItemEnv<'_> {
    fn frame(&self, dt: f64, upto: &EffectInstance) -> Option<Image> {
        let t2 = self.t + Tick::from_seconds_f64(dt);
        let base = crate::base_layer(self.project, self.seq, self.item, t2, self.opts, self.sources, self.want)?;
        let mut img = resize(base, self.layer_size.0, self.layer_size.1);
        let src = crate::source_size(self.project, self.item.item).unwrap_or((1, 1));
        let cx = FxCtx {
            t: self.item.effect_time_at(t2),
            px_scale: img.w as f32 / src.0.max(1) as f32,
            seconds: (t2 - self.item.start).seconds(),
            timecode: self.tc,
            clip_name: &self.item.name,
            project: Some(self.project),
            env: None,
            working: self.seq.settings.color.working,
        };
        for e in &self.item.effects {
            if std::ptr::eq(e, upto) {
                break;
            }
            if e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e) && !is_temporal(&e.effect) {
                crate::mask::apply_effect(&mut img, e, &cx);
            }
        }
        Some(img)
    }
    fn source_frame(&self, dt: f64, scale: f32) -> Option<Image> {
        let t2 = self.t + Tick::from_seconds_f64(dt);
        crate::base_layer(self.project, self.seq, self.item, t2, self.opts, self.sources, scale.clamp(1.0 / 64.0, 1.0))
    }
    fn track(&self, index: usize) -> Option<Image> {
        if index >= self.seq.video_tracks.len() {
            return None;
        }
        let o = RenderOptions { captions: false, working_output: true, depth: self.opts.depth + 1, ..self.opts };
        Some(crate::render_seq_tracks(self.project, self.seq, self.t, o, self.sources, Some(index)))
    }
    fn layer_to_output(&self) -> Affine {
        self.layer_to_output
    }
    fn clip_seconds(&self) -> f64 {
        self.item.duration.seconds()
    }
    fn clip_offset(&self) -> f64 {
        (self.t - self.item.start).seconds()
    }
    fn frame_rate(&self) -> f64 {
        self.seq.settings.frame_rate.as_f64()
    }
    fn media_timecode(&self) -> String {
        let s = &self.seq.settings;
        format_time(self.item.source_time_at(self.t), s.frame_rate, s.drop_frame, TimeDisplay::Timecode, s.sample_rate as i64)
    }
    fn file_name(&self) -> String {
        self.project.item(self.item.item).map(|p| p.name.clone()).unwrap_or_default()
    }
    fn sequence_name(&self) -> String {
        self.project
            .items
            .values()
            .find(|p| matches!(&p.kind, filmcraft_project::ItemKind::Sequence(s) if std::ptr::eq(&**s, self.seq)))
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }
    fn sequence_size(&self) -> (u32, u32) {
        (self.seq.settings.width, self.seq.settings.height)
    }
    fn source_size(&self) -> (u32, u32) {
        crate::source_size(self.project, self.item.item).unwrap_or((self.seq.settings.width, self.seq.settings.height))
    }
    fn clip_key(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for v in [
            self.item.item.0,
            self.item.start.0 as u64,
            self.item.duration.0 as u64,
            self.item.source_in.0 as u64,
            self.item.speed.to_bits(),
            self.item.reverse as u64,
        ] {
            h = (h ^ v).wrapping_mul(0x0100_0000_01b3);
        }
        h
    }
}

// ------------------------------------------------------------------ parameters

pub(crate) fn def_value(e: &EffectInstance, id: &str) -> Option<ParamValue> {
    e.def().and_then(|d| d.param(id)).map(|p| p.default.clone())
}
pub(crate) fn fv(e: &EffectInstance, id: &str, cx: &FxCtx) -> f32 {
    e.f64_at(id, cx.t) as f32
}
pub(crate) fn bv(e: &EffectInstance, id: &str) -> bool {
    e.param(id).and_then(|p| p.value.as_bool()).or_else(|| def_value(e, id).and_then(|v| v.as_bool())).unwrap_or(false)
}
pub(crate) fn chv(e: &EffectInstance, id: &str) -> u32 {
    match e.param(id).map(|p| p.value.clone()).or_else(|| def_value(e, id)) {
        Some(ParamValue::Choice(c)) => c,
        _ => 0,
    }
}
pub(crate) fn cv(e: &EffectInstance, id: &str, cx: &FxCtx) -> [f32; 4] {
    e.param(id).and_then(|p| p.value_at(cx.t).as_color()).or_else(|| def_value(e, id).and_then(|v| v.as_color())).unwrap_or([1.0; 4])
}
pub(crate) fn tv(e: &EffectInstance, id: &str) -> String {
    match e.param(id).map(|p| p.value.clone()).or_else(|| def_value(e, id)) {
        Some(ParamValue::Text(s)) => s,
        _ => String::new(),
    }
}
/// A point parameter in working pixels. Auto (NaN) components use the effect's auto position
/// (`filmcraft_project::effect::auto_point`) or the layer centre.
pub(crate) fn pv(e: &EffectInstance, id: &str, cx: &FxCtx, img: &Image) -> Vec2 {
    let v = e.param(id).map(|p| p.vec2_at(cx.t)).or_else(|| def_value(e, id).and_then(|v| v.as_vec2())).unwrap_or(Vec2::new(f64::NAN, f64::NAN));
    let (fx, fy) = filmcraft_project::effect::auto_point(&e.effect, id).unwrap_or((0.5, 0.5));
    Vec2::new(if v.x.is_nan() { img.w as f64 * fx } else { v.x * cx.px_scale as f64 }, if v.y.is_nan() { img.h as f64 * fy } else { v.y * cx.px_scale as f64 })
}
/// A point offset parameter (no auto position) in working pixels.
pub(crate) fn offv(e: &EffectInstance, id: &str, cx: &FxCtx) -> Vec2 {
    let v = e.param(id).map(|p| p.vec2_at(cx.t)).or_else(|| def_value(e, id).and_then(|v| v.as_vec2())).unwrap_or_default();
    let k = cx.px_scale as f64;
    Vec2::new(if v.x.is_nan() { 0.0 } else { v.x * k }, if v.y.is_nan() { 0.0 } else { v.y * k })
}

// ------------------------------------------------------------------ colour helpers

pub(crate) use crate::effects::{dec, enc};

#[inline]
pub(crate) fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
#[inline]
pub(crate) fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}
#[inline]
pub(crate) fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// sRGB-encoded straight colour param → linear RGB.
#[inline]
pub(crate) fn lin(c: [f32; 4]) -> [f32; 3] {
    dec([c[0], c[1], c[2]])
}

/// Blend `src` (straight linear RGB) onto `base` (straight linear RGB) with one of
/// [`filmcraft_project::effect::SIMPLE_BLEND`] (Normal, Add, Screen, Multiply, Overlay).
#[inline]
pub(crate) fn blend_simple(mode: u32, base: [f32; 3], src: [f32; 3]) -> [f32; 3] {
    let mut o = [0.0; 3];
    for k in 0..3 {
        let (b, s) = (base[k], src[k]);
        o[k] = match mode {
            1 => b + s,
            2 => 1.0 - (1.0 - b.min(1.0)) * (1.0 - s.min(1.0)),
            3 => b * s,
            4 => {
                if b < 0.5 {
                    2.0 * b * s
                } else {
                    1.0 - 2.0 * (1.0 - b.min(1.0)) * (1.0 - s.min(1.0))
                }
            }
            _ => s,
        };
    }
    o
}

/// Premultiplied `src` over premultiplied `dst`.
#[inline]
pub(crate) fn over(dst: &mut [f32], src: [f32; 4]) {
    let k = 1.0 - src[3];
    for c in 0..4 {
        dst[c] = src[c] + dst[c] * k;
    }
}

/// Add light `add` (linear RGB) to a premultiplied pixel, keeping it opaque where light lands on
/// transparency (glows extend past alpha).
#[inline]
pub(crate) fn add_light(p: &mut [f32], add: [f32; 3]) {
    let m = add[0].max(add[1]).max(add[2]).clamp(0.0, 1.0);
    for k in 0..3 {
        p[k] += add[k];
    }
    p[3] = p[3] + (1.0 - p[3]) * m;
}

// ------------------------------------------------------------------ noise

#[inline]
pub(crate) fn hash_u(x: i64, y: i64, z: i64, seed: u64) -> u64 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (z as u64).wrapping_mul(0x1656_67B1_9E37_79F9)
        ^ seed.wrapping_mul(0x27D4_EB2F_1656_67C5);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^ (h >> 32)
}
/// Uniform 0..1 from integer coordinates.
#[inline]
pub(crate) fn hash01(x: i64, y: i64, z: i64, seed: u64) -> f32 {
    (hash_u(x, y, z, seed) >> 40) as f32 / (1u64 << 24) as f32
}

#[inline]
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}
#[inline]
fn grad3(h: u64, x: f32, y: f32, z: f32) -> f32 {
    // 12 edge directions of a cube (Perlin 2002)
    match h % 12 {
        0 => x + y,
        1 => -x + y,
        2 => x - y,
        3 => -x - y,
        4 => x + z,
        5 => -x + z,
        6 => x - z,
        7 => -x - z,
        8 => y + z,
        9 => -y + z,
        10 => y - z,
        _ => -y - z,
    }
}

/// 3-D gradient noise in about −1..1 (our own hash; Perlin's interpolant and gradient set).
pub(crate) fn noise3(x: f32, y: f32, z: f32, seed: u64) -> f32 {
    let (xi, yi, zi) = (x.floor(), y.floor(), z.floor());
    let (xf, yf, zf) = (x - xi, y - yi, z - zi);
    let (xi, yi, zi) = (xi as i64, yi as i64, zi as i64);
    let (u, v, w) = (fade(xf), fade(yf), fade(zf));
    let g = |dx: i64, dy: i64, dz: i64| grad3(hash_u(xi + dx, yi + dy, zi + dz, seed), xf - dx as f32, yf - dy as f32, zf - dz as f32);
    let l = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let x00 = l(g(0, 0, 0), g(1, 0, 0), u);
    let x10 = l(g(0, 1, 0), g(1, 1, 0), u);
    let x01 = l(g(0, 0, 1), g(1, 0, 1), u);
    let x11 = l(g(0, 1, 1), g(1, 1, 1), u);
    l(l(x00, x10, v), l(x01, x11, v), w) * 0.9
}

/// Fractal (fBm) noise with `octaves` (fractional octaves fade in), about −1..1.
pub(crate) fn fbm(x: f32, y: f32, z: f32, octaves: f32, seed: u64) -> f32 {
    let n = octaves.clamp(1.0, 10.0);
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    let mut i = 0;
    while (i as f32) < n {
        let k = (n - i as f32).min(1.0);
        sum += noise3(x * freq, y * freq, z * freq + i as f32 * 17.13, seed.wrapping_add(i as u64)) * amp * k;
        norm += amp * k;
        amp *= 0.5;
        freq *= 2.0;
        i += 1;
    }
    sum / norm.max(1e-6)
}

/// Smooth 1-D noise over time (−1..1): for wiggle/shake channels.
pub(crate) fn noise1(t: f64, channel: u64, seed: u64) -> f32 {
    noise3(t as f32, channel as f32 * 7.31 + 0.5, 0.37, seed)
}

// ------------------------------------------------------------------ image helpers

/// The alpha channel as its own buffer.
pub(crate) fn alpha_of(img: &Image) -> Vec<f32> {
    img.px.par_chunks(4).map(|p| p[3]).collect()
}

/// Gaussian-blur a single-channel buffer (via a temporary image).
pub(crate) fn blur_plane(plane: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if sigma <= 0.3 {
        return plane.to_vec();
    }
    let mut im = Image { w, h, px: plane.iter().flat_map(|&v| [v, 0.0, 0.0, 0.0]).collect() };
    crate::effects::gaussian(&mut im, sigma, sigma, false);
    im.px.as_chunks::<4>().0.iter().map(|p| p[0]).collect()
}

/// Bright parts of an image above `threshold` (display luma), premultiplied linear, for glows.
pub(crate) fn highlights(img: &Image, threshold: f32, soft: f32) -> Image {
    let mut out = img.clone();
    out.px.par_chunks_mut(4).for_each(|p| {
        let a = p[3];
        if a <= 1e-6 {
            p.fill(0.0);
            return;
        }
        let l = crate::effects::enc([p[0] / a, p[1] / a, p[2] / a]);
        let k = smoothstep(threshold, threshold + soft.max(1e-3), luma(l));
        for v in p.iter_mut() {
            *v *= k;
        }
    });
    out
}

/// Fill an image from a per-pixel premultiplied colour function (x, y at pixel centres).
pub(crate) fn generate(img: &mut Image, f: impl Fn(f32, f32, [f32; 4]) -> [f32; 4] + Sync) {
    let w = img.w;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let p = &mut row[x * 4..x * 4 + 4];
            let o = f(x as f32 + 0.5, y as f32 + 0.5, [p[0], p[1], p[2], p[3]]);
            p.copy_from_slice(&o);
        }
    });
}

/// Inverse-map warp with an affine (destination → source sampling).
pub(crate) fn affine_warp(img: &mut Image, m: &Affine) {
    if m.is_identity() {
        return;
    }
    *img = img.transformed(img.w, img.h, m);
}

/// Shift/rotate/scale a layer about `c`: the transform shared by the animated Transform presets.
pub(crate) fn place(img: &mut Image, c: Vec2, offset: Vec2, scale: f64, rot_deg: f64) {
    let m = Affine::translate(c.x + offset.x, c.y + offset.y)
        .then_apply(&Affine::rotate_deg(rot_deg))
        .then_apply(&Affine::scale(scale, scale))
        .then_apply(&Affine::translate(-c.x, -c.y));
    affine_warp(img, &m);
}

/// Easing curves for [`filmcraft_project::effect::EASINGS`].
pub(crate) fn ease(kind: u32, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    match kind {
        1 => t * t,
        2 => 1.0 - (1.0 - t) * (1.0 - t),
        3 => t * t * (3.0 - 2.0 * t),
        _ => t,
    }
}

/// Progress through the clip (0 at its first frame, 1 at its last) when an environment is
/// available, else 0.
pub(crate) fn clip_progress(cx: &FxCtx) -> f64 {
    match cx.env {
        Some(env) => {
            let d = env.clip_seconds();
            if d <= 1e-9 { 0.0 } else { (cx.seconds / d).clamp(0.0, 1.0) }
        }
        None => 0.0,
    }
}

// ------------------------------------------------------------------ dispatch

/// Apply `e` if it is one of the effects implemented here; returns `false` for anything else.
pub fn apply(img: &mut Image, e: &EffectInstance, cx: &FxCtx) -> bool {
    let alias = |img: &mut Image, base: &str| {
        let mut a = e.clone();
        a.effect = base.to_string();
        crate::effects::apply(img, &a, cx);
    };
    match e.effect.as_str() {
        // Adjust / Color / Image Control / Keying / Utility colour
        "lighting_effects" => color::lighting(img, e, cx),
        "logo_cutout" => color::logo_cutout(img, e, cx),
        "ultra_key" => color::ultra_key(img, e, cx),
        "track_matte" => color::track_matte(img, e, cx),
        "cineon_converter" => color::cineon(img, e, cx),
        "noise" | "noise_legacy" => color::noise(img, e, cx),
        // Blur & Sharpen
        "bokeh_blur" => blur::bokeh(img, e, cx),
        "channel_blur" => blur::channel_blur(img, e, cx),
        "compound_blur" => blur::compound(img, e, cx),
        "focus_blur" => blur::focus(img, e, cx),
        "reduce_interlace_flicker" => blur::interlace_flicker(img, e, cx),
        "gaussian_blur_legacy" => alias(img, "gaussian_blur"),
        "directional_blur_legacy" => alias(img, "directional_blur"),
        // Distort / Transform / Utility geometry
        "corner_pin" => distort::corner_pin(img, e, cx),
        "magnify" | "magnify_legacy" => distort::magnify(img, e, cx),
        "spherize" => distort::spherize(img, e, cx),
        "turbulent_displace" => distort::turbulent_displace(img, e, cx),
        "rotate_3d" => distort::rotate_3d(img, e, cx),
        "grow" | "shrink" => distort::grow(img, e, cx),
        "move" => distort::move_fx(img, e, cx),
        "spin" => distort::spin(img, e, cx),
        "wiggle" => distort::wiggle(img, e, cx),
        "camera_shake" => distort::camera_shake(img, e, cx),
        "spacer" => distort::spacer(img, e, cx),
        "clone" => distort::clone_fx(img, e, cx),
        "auto_align" => distort::auto_align(img, e, cx),
        "rounded_crop" => distort::rounded_crop(img, e, cx),
        "mosaic" | "mosaic_legacy" => distort::mosaic(img, e, cx),
        "twirl_legacy" => alias(img, "twirl"),
        // Lights & Glows
        "echo_glow" => lights::echo_glow(img, e, cx),
        "edge_glow" => lights::edge_glow(img, e, cx),
        "glint" => lights::glint(img, e, cx),
        "light_leaks" => lights::light_leaks(img, e, cx),
        "rgb_split" => lights::rgb_split(img, e, cx),
        "volumetric_rays" => lights::volumetric_rays(img, e, cx),
        "wonder_glow" => lights::wonder_glow(img, e, cx),
        "lens_flare" => lights::lens_flare(img, e, cx),
        "alpha_glow" => lights::alpha_glow(img, e, cx),
        // Stylize / Perspective / Generate / Legacy & obsolete generators
        "brush_strokes" => stylize::brush_strokes(img, e, cx),
        "color_emboss" => stylize::color_emboss(img, e, cx),
        "roughen_edges" => stylize::roughen_edges(img, e, cx),
        "long_shadow" => stylize::long_shadow(img, e, cx),
        "stroke" => stylize::stroke(img, e, cx),
        "gradient" => stylize::gradient(img, e, cx),
        "block_dissolve" => stylize::block_dissolve(img, e, cx),
        "gradient_wipe_legacy" => stylize::gradient_wipe(img, e, cx),
        "linear_wipe_legacy" => stylize::linear_wipe(img, e, cx),
        "lightning" => stylize::lightning(img, e, cx),
        "cell_pattern" => stylize::cell_pattern(img, e, cx),
        "checkerboard" => stylize::checkerboard(img, e, cx),
        "ellipse" => stylize::ellipse(img, e, cx),
        "paint_bucket" => stylize::paint_bucket(img, e, cx),
        "write_on" => stylize::write_on(img, e, cx),
        // Time / analysis
        "posterize_time" => temporal::posterize_time(img, e, cx),
        "echo" => temporal::echo(img, e, cx),
        "warp_stabilizer" => temporal::warp_stabilizer(img, e, cx),
        "auto_reframe" => temporal::auto_reframe(img, e, cx),
        // Text
        "simple_text" => text::simple_text(img, e, cx),
        "metadata_burnin" => text::metadata_burnin(img, e, cx),
        id if id.starts_with("vr_") => immersive::apply(img, e, cx),
        _ => return false,
    }
    true
}
