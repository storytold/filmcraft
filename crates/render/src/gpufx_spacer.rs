//! Spacer on the multi-pass GPU route.
//!
//! The CPU effect (`vfx::distort::spacer`) insets the layer inside margins, scales it to fit while
//! keeping its proportions, centres it, rounds its corners and optionally fills the background. This
//! module evaluates those numbers once ([`spacer_params`]) so the GPU kernel `cs_spacer`
//! (`crates/gpu/src/fx_spacer.wgsl`) and the CPU mirror [`spacer_cpu_pixel`] apply the same rule.
//!
//! Restrictions:
//! - The kernel samples bilinearly with no mip chain. The CPU path pre-filters with a box mip when
//!   the layer is minified by two or more, that is when the scale is at most 1/2. Route to the GPU
//!   only when [`SpacerParams::gpu_route_ok`] holds (scale above 1/2).
//! - Margins and radius are already in destination pixels: the caller multiplies them by the px scale,
//!   as the CPU effect does. The fill colour is linear RGB (the caller decodes the sRGB parameter).
//! - Negative or non-finite inputs are refused (`None`), not clamped. The CPU effect is not affected.
//!
//! Nothing here panics; the kernel name and the plan are plain data validated by `gpufx_multipass`.

use crate::gpufx_multipass::{MultipassPlan, Pass, Slot};
use crate::vfx::round_rect_sdf;

/// Entry point of the spacer kernel in `fx_spacer.wgsl`.
pub const SPACER_KERNEL: &str = "cs_spacer";

/// Smallest scale the GPU kernel handles. At or below it the CPU path uses its mip pre-filter.
pub const GPU_MIN_SCALE: f64 = 0.5;

/// Number of `f32` words in the uniform the kernel reads at binding 3 (four `vec4<f32>`).
pub const SPACER_UNIFORM_WORDS: usize = 16;

/// The spacer numbers, evaluated for one frame size. Build with [`spacer_params`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpacerParams {
    /// Destination width and height in pixels; the source has the same size.
    pub width: u32,
    pub height: u32,
    /// Fit scale `s = min(bw / w, bh / h)`.
    pub scale: f64,
    /// Fitted layer rectangle in destination pixels (`x1 = x0 + w * s`, `y1 = y0 + h * s`).
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    /// Corner radius in destination pixels, clamped to half the smaller fitted side. 0 = square.
    pub radius: f64,
    /// Background fill colour (linear RGB), or `None` for a transparent background.
    pub fill: Option<[f32; 3]>,
}

impl SpacerParams {
    /// True when the GPU kernel may draw this spacer: the scale must stay above [`GPU_MIN_SCALE`].
    pub fn gpu_route_ok(&self) -> bool {
        self.scale > GPU_MIN_SCALE
    }

    /// The uniform words for binding 3, in the `SpacerUniform` layout of `fx_spacer.wgsl`:
    /// `rect`, `src` (x = 1 / scale), `shape` (x = radius, y = fill flag), `fill_color`.
    pub fn uniform_words(&self) -> [f32; SPACER_UNIFORM_WORDS] {
        let inv_scale = if self.scale > 0.0 { 1.0 / self.scale } else { 0.0 };
        let (fill_flag, fill) = match self.fill {
            Some(c) => (1.0_f32, c),
            None => (0.0_f32, [0.0_f32; 3]),
        };
        [
            self.x0 as f32,
            self.y0 as f32,
            self.x1 as f32,
            self.y1 as f32,
            inv_scale as f32,
            0.0,
            0.0,
            0.0,
            self.radius as f32,
            fill_flag,
            0.0,
            0.0,
            fill[0],
            fill[1],
            fill[2],
            0.0,
        ]
    }
}

/// Evaluates the spacer geometry for a `width` x `height` frame, as `vfx::distort::spacer` does.
///
/// `uniform` uses `left` for all four margins. Returns `None` when a used margin or the radius is
/// negative or not finite, when the fill colour is not finite, or when a dimension is zero.
#[allow(clippy::too_many_arguments)]
pub fn spacer_params(
    width: u32,
    height: u32,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    uniform: bool,
    radius: f64,
    fill: Option<[f32; 3]>,
) -> Option<SpacerParams> {
    if width == 0 || height == 0 {
        return None;
    }
    let (t, r, b) = if uniform { (left, left, left) } else { (top, right, bottom) };
    if [left, t, r, b, radius].iter().any(|v| !v.is_finite() || *v < 0.0) {
        return None;
    }
    if fill.is_some_and(|c| c.iter().any(|v| !v.is_finite())) {
        return None;
    }
    let (w, h) = (f64::from(width), f64::from(height));
    let (bw, bh) = ((w - left - r).max(1.0), (h - t - b).max(1.0));
    let s = (bw / w).min(bh / h);
    if !s.is_finite() || s <= 0.0 {
        return None;
    }
    let (dw, dh) = (w * s, h * s);
    let (x0, y0) = (left + (bw - dw) / 2.0, t + (bh - dh) / 2.0);
    let rad = radius.min(dw.min(dh) / 2.0);
    Some(SpacerParams { width, height, scale: s, x0, y0, x1: x0 + dw, y1: y0 + dh, radius: rad.max(0.0), fill })
}

/// The one-pass plan: `cs_spacer` reads slot 0 (the source) and writes slot 1, which is the result.
pub fn spacer_plan(params: &SpacerParams) -> MultipassPlan {
    let block = params.uniform_words().iter().flat_map(|w| w.to_le_bytes()).collect();
    MultipassPlan { passes: vec![Pass { kernel: SPACER_KERNEL, inputs: vec![Slot(0)], output: Slot(1), params: block }], result: Slot(1) }
}

/// CPU mirror of the kernel for one destination pixel. `src` fetches a premultiplied source pixel by
/// integer coordinates; it is only called for coordinates inside the `width` x `height` source.
pub fn spacer_cpu_pixel(params: &SpacerParams, x: u32, y: u32, src: impl Fn(u32, u32) -> [f32; 4]) -> [f32; 4] {
    if params.scale.is_nan() || params.scale <= 0.0 {
        return [0.0; 4];
    }
    let (px, py) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
    let inv = 1.0 / params.scale;
    let (u, v) = ((px - params.x0) * inv, (py - params.y0) * inv);
    let (wf, hf) = (f64::from(params.width), f64::from(params.height));
    let mut color = [0.0_f32; 4];
    if u >= -1.0 && v >= -1.0 && u <= wf + 1.0 && v <= hf + 1.0 {
        color = bilinear(&src, params.width, params.height, u as f32, v as f32);
    }
    if params.radius > 0.0 {
        let d = round_rect_sdf(px as f32, py as f32, params.x0 as f32, params.y0 as f32, params.x1 as f32, params.y1 as f32, params.radius as f32);
        let a = (0.5 - d).clamp(0.0, 1.0);
        for c in &mut color {
            *c *= a;
        }
    }
    if let Some(fc) = params.fill {
        let k = 1.0 - color[3];
        color = [color[0] + fc[0] * k, color[1] + fc[1] * k, color[2] + fc[2] * k, 1.0];
    }
    color
}

/// Bilinear sample with transparent borders, as `Image::sample_bilinear`.
fn bilinear<F: Fn(u32, u32) -> [f32; 4]>(src: &F, w: u32, h: u32, u: f32, v: f32) -> [f32; 4] {
    let (fx, fy) = (u - 0.5, v - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let fetch = |xi: f32, yi: f32| -> [f32; 4] { if xi < 0.0 || yi < 0.0 || xi >= w as f32 || yi >= h as f32 { [0.0; 4] } else { src(xi as u32, yi as u32) } };
    let (a, b, c, d) = (fetch(x0, y0), fetch(x0 + 1.0, y0), fetch(x0, y0 + 1.0), fetch(x0 + 1.0, y0 + 1.0));
    let mut out = [0.0_f32; 4];
    for k in 0..4 {
        let top = a[k] + (b[k] - a[k]) * tx;
        let bot = c[k] + (d[k] - c[k]) * tx;
        out[k] = top + (bot - top) * ty;
    }
    out
}

/// The spacer route for `e` on a `width` x `height` frame, or None when the CPU must render it
/// (scale at or below one half: the mip pre-filter route, or invalid parameters).
pub fn spacer_effect(
    e: &filmcraft_project::EffectInstance,
    cx: &crate::effects::FxCtx,
    width: u32,
    height: u32,
) -> Option<crate::gpufx_multipass::MultipassEffect> {
    use crate::vfx::{bv, cv, fv, lin};
    let k = f64::from(cx.px_scale);
    let side = |id: &str| f64::from(fv(e, id, cx)) * k;
    let fill = bv(e, "fill").then(|| lin(cv(e, "color", cx)));
    let params = spacer_params(width, height, side("left"), side("top"), side("right"), side("bottom"), bv(e, "uniform"), side("radius"), fill)?;
    if !params.gpu_route_ok() {
        return None;
    }
    Some(crate::gpufx_multipass::MultipassEffect {
        id: "spacer",
        wgsl: include_str!("../../gpu/src/fx_spacer.wgsl"),
        kernels: &[SPACER_KERNEL],
        plan: spacer_plan(&params),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Image;
    use filmcraft_geom::Affine;

    /// The CPU `spacer` pipeline on an `Image`: `transformed` resampling, corner mask, fill. The
    /// effect itself needs an `EffectInstance` and a context, so the test rebuilds its steps here.
    fn reference(p: &SpacerParams, src: &Image) -> Image {
        let m = Affine::translate(p.x0, p.y0).then_apply(&Affine::scale(p.scale, p.scale));
        let mut out = src.transformed(src.w, src.h, &m);
        if p.radius > 0.0 {
            for y in 0..out.h {
                for x in 0..out.w {
                    let d = round_rect_sdf(x as f32 + 0.5, y as f32 + 0.5, p.x0 as f32, p.y0 as f32, p.x1 as f32, p.y1 as f32, p.radius as f32);
                    let a = (0.5 - d).clamp(0.0, 1.0);
                    let i = (y * out.w + x) * 4;
                    for v in &mut out.px[i..i + 4] {
                        *v *= a;
                    }
                }
            }
        }
        if let Some(c) = p.fill {
            for px in out.px.chunks_mut(4) {
                let k = 1.0 - px[3];
                px[0] += c[0] * k;
                px[1] += c[1] * k;
                px[2] += c[2] * k;
                px[3] = 1.0;
            }
        }
        out
    }

    fn gradient(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                img.px[i] = x as f32 / w as f32;
                img.px[i + 1] = y as f32 / h as f32;
                img.px[i + 2] = 0.25;
                img.px[i + 3] = 1.0;
            }
        }
        img
    }

    #[test]
    fn plan_is_valid_and_reads_slot_zero_writes_slot_one() {
        let params = spacer_params(64, 48, 8.0, 8.0, 8.0, 8.0, true, 4.0, None).expect("finite params");
        let plan = spacer_plan(&params);
        assert_eq!(plan.validate(), Ok(1));
        assert_eq!(plan.passes[0].params.len(), 64);
        assert_eq!(plan.passes[0].kernel, "cs_spacer");
        assert_eq!(plan.passes[0].inputs, vec![Slot(0)]);
        assert_eq!(plan.passes[0].output, Slot(1));
        assert_eq!(plan.result, Slot(1));
    }

    #[test]
    fn non_finite_or_invalid_inputs_are_refused() {
        assert!(spacer_params(64, 48, f64::NAN, 0.0, 0.0, 0.0, true, 0.0, None).is_none());
        assert!(spacer_params(64, 48, f64::INFINITY, 0.0, 0.0, 0.0, true, 0.0, None).is_none());
        assert!(spacer_params(64, 48, 4.0, f64::NAN, 4.0, 4.0, false, 0.0, None).is_none());
        assert!(spacer_params(64, 48, 4.0, 4.0, 4.0, 4.0, false, f64::NAN, None).is_none());
        assert!(spacer_params(64, 48, -1.0, 0.0, 0.0, 0.0, true, 0.0, None).is_none());
        assert!(spacer_params(64, 48, 4.0, 4.0, 4.0, 4.0, false, -2.0, None).is_none());
        assert!(spacer_params(64, 48, 4.0, 4.0, 4.0, 4.0, false, 0.0, Some([f32::NAN, 0.0, 0.0])).is_none());
        assert!(spacer_params(0, 48, 4.0, 4.0, 4.0, 4.0, false, 0.0, None).is_none());
        assert!(spacer_params(64, 0, 4.0, 4.0, 4.0, 4.0, false, 0.0, None).is_none());
    }

    #[test]
    fn uniform_margins_fit_keeps_proportion_and_centres() {
        // 100x50 frame, 10 px on every side: the box is 80x30, so s = min(0.8, 0.6) = 0.6.
        let p = spacer_params(100, 50, 10.0, 0.0, 0.0, 0.0, true, 0.0, None).expect("valid params");
        assert!((p.scale - 0.6).abs() < 1e-12);
        assert!((p.x0 - 20.0).abs() < 1e-9 && (p.y0 - 10.0).abs() < 1e-9);
        assert!((p.x1 - 80.0).abs() < 1e-9 && (p.y1 - 40.0).abs() < 1e-9);
        assert!(p.gpu_route_ok());
    }

    #[test]
    fn heavy_margins_fall_back_to_cpu_route() {
        // Box of 20 px wide for a 100 px frame: scale 0.2, minified below 1/2, so no GPU route.
        let p = spacer_params(100, 50, 40.0, 0.0, 0.0, 0.0, true, 0.0, None).expect("valid params");
        assert!(p.scale < GPU_MIN_SCALE);
        assert!(!p.gpu_route_ok());
    }

    #[test]
    fn radius_is_clamped_to_half_the_fitted_side() {
        let p = spacer_params(100, 50, 10.0, 0.0, 0.0, 0.0, true, 1000.0, None).expect("valid params");
        assert!((p.radius - 0.5 * 30.0).abs() < 1e-9);
    }

    #[test]
    fn cpu_pixel_rule_matches_the_reference_pipeline() {
        let src = gradient(100, 50);
        let cases = [
            spacer_params(100, 50, 10.0, 0.0, 0.0, 0.0, true, 0.0, None),
            spacer_params(100, 50, 10.0, 0.0, 0.0, 0.0, true, 8.0, Some([0.2, 0.3, 0.4])),
            spacer_params(100, 50, 6.0, 4.0, 12.0, 2.0, false, 5.0, None),
        ];
        for p in cases.into_iter().flatten() {
            let refimg = reference(&p, &src);
            let fetch = |x: u32, y: u32| src.get(x as usize, y as usize);
            // Corners, edges, the centre, and points inside the fitted layer.
            for &(x, y) in &[(0u32, 0u32), (99, 49), (50, 25), (20, 10), (21, 11), (79, 39), (10, 25), (5, 45)] {
                let got = spacer_cpu_pixel(&p, x, y, fetch);
                let want = refimg.get(x as usize, y as usize);
                for k in 0..4 {
                    assert!((got[k] - want[k]).abs() < 1e-4, "pixel ({x},{y}) channel {k}: got {got:?}, want {want:?}");
                }
            }
        }
    }

    #[test]
    fn uniform_words_follow_the_shader_layout() {
        let p = spacer_params(100, 50, 10.0, 0.0, 0.0, 0.0, true, 8.0, Some([0.1, 0.2, 0.3])).expect("valid params");
        let w = p.uniform_words();
        assert!((w[0] - 20.0).abs() < 1e-6 && (w[2] - 80.0).abs() < 1e-6);
        assert!((w[4] - (1.0 / 0.6) as f32).abs() < 1e-5);
        assert!((w[8] - 8.0).abs() < 1e-6);
        assert_eq!(w[9], 1.0);
        assert_eq!(&w[12..15], &[0.1, 0.2, 0.3]);
    }
}
