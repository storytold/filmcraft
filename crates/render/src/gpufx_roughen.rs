//! Roughen Edges on the GPU: the parameter evaluation and the three-pass plan for `fx_roughen.wgsl`.
//!
//! The CPU reference is `roughen_edges` in `vfx/stylize.rs`. Its depth blur is three box passes per
//! axis with the radii of `effects::gaussian_boxes`; the GPU runs the same boxes in the same order. Parameters arrive already evaluated (the caller reads
//! them with the same helpers the CPU uses) and every value is checked here: nothing non-finite
//! reaches the device, and nothing here panics.

use crate::gpufx_multipass::{MAX_PARAM_BYTES, MultipassPlan, Pass, Slot};

/// Bytes of the uniform block read by `cs_roughen_apply` in `fx_roughen.wgsl` (five `vec4`s).
pub const UNIFORM_BYTES: usize = 80;

/// Evaluated inputs, as the CPU reads them: `fv`/`chv`/`cv`/`offv` results of the effect instance.
#[derive(Clone, Debug, PartialEq)]
pub struct RoughenInput {
    /// `edge_type` choice (0..7). Values above 7 behave like 0, as on the CPU.
    pub edge_type: u32,
    /// `border` in effect units, before the pixel scale.
    pub border: f32,
    pub sharpness: f32,
    pub influence: f32,
    /// `scale` in percent, before the pixel scale.
    pub scale: f32,
    /// `stretch` as a power of two exponent (the CPU uses `2^stretch`).
    pub stretch: f32,
    /// `offset` as returned by `offv`, already multiplied by the pixel scale.
    pub offset: [f32; 2],
    pub complexity: f32,
    /// `evolution` in degrees.
    pub evolution: f32,
    /// `seed`; negative values clamp to 0, as on the CPU.
    pub seed: f32,
    /// `edge_color` as returned by `cv`: sRGB-encoded straight colour.
    pub edge_color: [f32; 4],
    /// Pixel scale of the render (`FxCtx::px_scale`).
    pub px_scale: f32,
}

/// The evaluated numbers the kernel needs. Built only by [`roughen_params`].
#[derive(Clone, Debug, PartialEq)]
pub struct RoughenParams {
    /// Border in pixels (`border * px_scale`); never below 0.5.
    pub border_px: f32,
    /// Blur sigma of the depth map (`border_px * 0.5`).
    pub sigma: f32,
    /// Sharpness, clamped to `>= 0`.
    pub sharpness: f32,
    /// Influence, clamped to `0..=1`.
    pub influence: f32,
    /// Noise scale in pixels (`max(scale / 100 * 30 * px_scale, 0.5)`).
    pub scale_px: f32,
    /// `2^stretch`, as the CPU computes it (no clamp; a non-finite result is refused).
    pub stretch: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    /// Fractal octaves, clamped to `1..=10`.
    pub complexity: f32,
    /// Evolution in turns (`evolution / 360`), the noise's third coordinate.
    pub evolution: f32,
    pub seed: u64,
    pub edge_type: u32,
    /// Edge colour, linear RGB.
    pub edge_color: [f32; 3],
}

/// Evaluates the parameters the way `roughen_edges` does.
///
/// * `Err` when any input or derived value is not finite (a bad parameter, never a valid effect).
/// * `Ok(None)` when the border is below half a pixel: the effect does nothing.
/// * `Ok(Some(_))` otherwise.
pub fn roughen_params(input: &RoughenInput) -> Result<Option<RoughenParams>, &'static str> {
    let scalars = [
        input.border,
        input.sharpness,
        input.influence,
        input.scale,
        input.stretch,
        input.offset[0],
        input.offset[1],
        input.complexity,
        input.evolution,
        input.seed,
        input.edge_color[0],
        input.edge_color[1],
        input.edge_color[2],
        input.edge_color[3],
        input.px_scale,
    ];
    if scalars.iter().any(|v| !v.is_finite()) {
        return Err("roughen: non-finite parameter");
    }
    let border_px = input.border * input.px_scale;
    if !border_px.is_finite() {
        return Err("roughen: border overflows");
    }
    if border_px < 0.5 {
        return Ok(None);
    }
    let sigma = border_px * 0.5;
    let scale_px = (input.scale / 100.0 * 30.0 * input.px_scale).max(0.5);
    let stretch = input.stretch.exp2();
    let evolution = input.evolution / 360.0;
    let span = scale_px * stretch;
    let inv_span = scale_px / stretch;
    if !(sigma.is_finite() && scale_px.is_finite() && stretch.is_finite() && stretch > 0.0 && evolution.is_finite())
        || !(span.is_finite() && inv_span.is_finite() && span >= f32::MIN_POSITIVE && inv_span >= f32::MIN_POSITIVE)
    {
        return Err("roughen: derived parameter overflows");
    }
    let [r, g, b] = crate::vfx::lin(input.edge_color);
    let edge_color = [r, g, b];
    if !edge_color.iter().all(|c| c.is_finite()) {
        return Err("roughen: edge colour is not finite");
    }
    Ok(Some(RoughenParams {
        border_px,
        sigma,
        sharpness: input.sharpness.max(0.0),
        influence: input.influence.clamp(0.0, 1.0),
        scale_px,
        stretch,
        offset_x: input.offset[0],
        offset_y: input.offset[1],
        complexity: input.complexity.clamp(1.0, 10.0),
        evolution,
        seed: input.seed.max(0.0) as u64,
        edge_type: input.edge_type,
        edge_color,
    }))
}

impl RoughenParams {
    /// The uniform block for `fx_roughen.wgsl` (`RoughenUniform`), little-endian, std140-style
    /// 16-byte rows: `a` (sigma, sharpness, influence, scale), `b` (stretch, offset x, offset y,
    /// complexity), `c` (evolution), `d` (seed lo, seed hi, edge type, unused), `e` (edge colour).
    pub fn uniform_bytes(&self) -> [u8; UNIFORM_BYTES] {
        let floats = [
            self.sigma,
            self.sharpness,
            self.influence,
            self.scale_px,
            self.stretch,
            self.offset_x,
            self.offset_y,
            self.complexity,
            self.evolution,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            self.edge_color[0],
            self.edge_color[1],
            self.edge_color[2],
            0.0,
        ];
        // Words 12..16 are integers (seed lo, seed hi, edge type, unused); the rest are f32 bit patterns.
        let ints = [self.seed as u32, (self.seed >> 32) as u32, self.edge_type, 0];
        let mut words = [0u32; 20];
        for (i, (word, value)) in words.iter_mut().zip(floats).enumerate() {
            *word = match i {
                12..=15 => ints.get(i - 12).copied().unwrap_or(0),
                _ => value.to_bits(),
            };
        }
        let mut out = [0u8; UNIFORM_BYTES];
        let (chunks, _) = out.as_chunks_mut::<4>();
        for (dst, word) in chunks.iter_mut().zip(words) {
            *dst = word.to_le_bytes();
        }
        out
    }
}

/// The three-pass plan: blur the alpha horizontally (slot 0 -> 1), vertically (1 -> 2), then apply
/// the roughening with the source (slot 0) and the final depth (slot 2) into slot 3. Every pass
/// carries the same parameter block (binding 3).
pub fn roughen_plan(params: &RoughenParams, width: u32, height: u32) -> MultipassPlan {
    // The CPU's depth map: box radii from effects::gaussian_boxes (same caps as the CPU). Sigma
    // at or below 0.3 means no blur there: all radii 0 (identity boxes, as box_rows does).
    let (rx, ry) = crate::effects::gaussian_boxes(width as usize, height as usize, params.sigma, params.sigma);
    let radius = |v: &[u32], i: usize| v.get(i).copied().unwrap_or(0);
    let box_block = |r: u32, alpha: u32, vertical: u32| {
        // Zero-padded to the shader's declared binding size (array<vec4<u32>, 8>).
        let mut b = Vec::with_capacity(MAX_PARAM_BYTES);
        for word in [r, alpha, vertical, 0] {
            b.extend_from_slice(&word.to_le_bytes());
        }
        b.resize(MAX_PARAM_BYTES, 0);
        b
    };
    let padded = |mut b: Vec<u8>| {
        b.resize(MAX_PARAM_BYTES, 0);
        b
    };
    let mut passes = Vec::with_capacity(7);
    // horizontal boxes: slot0 alpha -> 1 -> 2 -> 3
    for i in 0..3usize {
        let (input, output) = (Slot(i as u8), Slot(i as u8 + 1));
        passes.push(Pass { kernel: "cs_roughen_box", inputs: vec![input], output, params: box_block(radius(&rx, i), u32::from(i == 0), 0) });
    }
    // vertical boxes: slot3 -> 1 -> 2 -> 3 (slot 1 and 2 are free again once the horizontal chain is read)
    for i in 0..3usize {
        let input = if i == 0 { Slot(3) } else { Slot(i as u8) };
        passes.push(Pass { kernel: "cs_roughen_box", inputs: vec![input], output: Slot(i as u8 + 1), params: box_block(radius(&ry, i), 0, 1) });
    }
    passes.push(Pass { kernel: "cs_roughen_apply", inputs: vec![Slot(0), Slot(3)], output: Slot(4), params: padded(params.uniform_bytes().to_vec()) });
    MultipassPlan { passes, result: Slot(4) }
}

/// Evaluates the effect instance at the context's time, as `vfx::stylize::roughen_edges` reads it.
pub fn roughen_input(e: &filmcraft_project::EffectInstance, cx: &crate::effects::FxCtx) -> RoughenInput {
    use crate::vfx::{chv, cv, fv, offv};
    let off = offv(e, "offset", cx);
    RoughenInput {
        edge_type: chv(e, "edge_type"),
        border: fv(e, "border", cx),
        sharpness: fv(e, "sharpness", cx),
        influence: fv(e, "influence", cx),
        scale: fv(e, "scale", cx),
        stretch: fv(e, "stretch", cx),
        offset: [off.x as f32, off.y as f32],
        complexity: fv(e, "complexity", cx),
        evolution: fv(e, "evolution", cx),
        seed: fv(e, "seed", cx),
        edge_color: cv(e, "edge_color", cx),
        px_scale: cx.px_scale,
    }
}

/// WGSL of the roughen route: the u64 hash prepended to the kernels, as the kernels need it.
fn roughen_wgsl() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(|| format!("{}\n{}", include_str!("../../gpu/src/noise_u64.wgsl"), include_str!("../../gpu/src/fx_roughen.wgsl"))).as_str()
}

/// The roughen route for `e` on a `width` x `height` frame. None when the effect does nothing
/// (border below half a pixel) or a parameter cannot be used on the GPU; the CPU then renders it.
pub fn roughen_effect(
    e: &filmcraft_project::EffectInstance,
    cx: &crate::effects::FxCtx,
    width: u32,
    height: u32,
) -> Option<crate::gpufx_multipass::MultipassEffect> {
    let params = roughen_params(&roughen_input(e, cx)).ok().flatten()?;
    Some(crate::gpufx_multipass::MultipassEffect {
        id: "roughen_edges",
        wgsl: roughen_wgsl(),
        kernels: &["cs_roughen_box", "cs_roughen_apply"],
        plan: roughen_plan(&params, width, height),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> RoughenInput {
        RoughenInput {
            edge_type: 0,
            border: 10.0,
            sharpness: 0.5,
            influence: 0.5,
            scale: 50.0,
            stretch: 0.0,
            offset: [0.0, 0.0],
            complexity: 4.0,
            evolution: 90.0,
            seed: 7.0,
            edge_color: [1.0, 1.0, 1.0, 1.0],
            px_scale: 1.0,
        }
    }

    #[test]
    fn plan_is_valid_with_box_passes_and_four_intermediate_slots() {
        let Some(params) = roughen_params(&base()).ok().flatten() else {
            panic!("base parameters must produce an effect");
        };
        let plan = roughen_plan(&params, 64, 48);
        assert_eq!(plan.validate(), Ok(4));
        assert_eq!(plan.passes.len(), 7);
        assert_eq!(plan.passes.last().map(|p| p.inputs.clone()), Some(vec![Slot(0), Slot(3)]));
        assert_eq!(plan.passes.last().map(|p| p.params[..UNIFORM_BYTES].len()), Some(UNIFORM_BYTES));
        assert!(plan.passes.iter().all(|p| p.params.len() == MAX_PARAM_BYTES));
    }

    #[test]
    fn nan_parameters_are_refused() {
        let mut nan_border = base();
        nan_border.border = f32::NAN;
        assert!(roughen_params(&nan_border).is_err());

        let mut nan_colour = base();
        nan_colour.edge_color[1] = f32::NAN;
        assert!(roughen_params(&nan_colour).is_err());

        let mut inf_offset = base();
        inf_offset.offset[0] = f32::INFINITY;
        assert!(roughen_params(&inf_offset).is_err());
    }

    #[test]
    fn overflowing_border_is_an_error_not_a_panic() {
        let mut huge = base();
        huge.border = f32::MAX;
        huge.px_scale = 4.0;
        assert!(roughen_params(&huge).is_err());
    }

    #[test]
    fn tiny_border_means_no_effect() {
        let mut small = base();
        small.border = 0.4;
        assert_eq!(roughen_params(&small), Ok(None));
    }

    #[test]
    fn values_are_evaluated_like_the_cpu() {
        let p = roughen_params(&base()).ok().flatten();
        let p = p.as_ref();
        assert_eq!(p.map(|p| p.sigma), Some(5.0));
        assert_eq!(p.map(|p| p.scale_px), Some(15.0));
        assert_eq!(p.map(|p| p.evolution), Some(0.25));
        assert_eq!(p.map(|p| p.seed), Some(7));
    }

    #[test]
    fn uniform_block_has_the_declared_size_and_sigma_first() {
        let p = roughen_params(&base()).ok().flatten();
        let bytes = p.map(|p| p.uniform_bytes());
        assert_eq!(bytes.as_ref().map(|b| b.len()), Some(UNIFORM_BYTES));
        let first = bytes.and_then(|b| b.get(0..4).and_then(|s| <[u8; 4]>::try_from(s).ok()));
        assert_eq!(first.map(f32::from_le_bytes), Some(5.0));
    }
}

/// CPU noise3 and grad3 for the octave probe.
pub fn noise_reference(x: f32, y: f32, z: f32, seed: u64) -> f32 {
    crate::vfx::noise3(x, y, z, seed)
}

/// CPU grad3 of a cell hash for the octave probe.
pub fn grad_reference(h: u64, x: f32, y: f32, z: f32) -> f32 {
    crate::vfx::grad3(h, x, y, z)
}

/// CPU `hash_u` of cell (x, y, z) with `seed`, for the hash probe.
pub fn hash_cell_reference(x: i64, y: i64, z: i64, seed: u64) -> u64 {
    crate::vfx::hash_u(x, y, z, seed)
}

/// CPU fBm of the apply, for the probe: the value the kernel's `fbm` must reproduce.
pub fn roughen_noise_reference(x: f32, y: f32, z: f32, octaves: f32, seed: u64) -> f32 {
    crate::vfx::fbm(x, y, z, octaves, seed)
}

/// Measurement oracle for the AMD stage test: the depth plane after each of the six box passes of
/// the plan, in plan order (horizontal radii, then vertical), starting from `alpha` (`w` x `h`).
/// Each stage is a plane of `w * h` values in row-major order.
pub fn depth_stages(alpha: &[f32], width: usize, height: usize, sigma: f32) -> Vec<Vec<f32>> {
    let (rx, ry) = crate::effects::gaussian_boxes(width, height, sigma, sigma);
    let radii: Vec<(u32, bool)> = rx.iter().map(|&r| (r, true)).chain(ry.iter().map(|&r| (r, false))).collect();
    let mut stages = Vec::with_capacity(radii.len());
    let mut plane = alpha.to_vec();
    for (r, horizontal) in radii {
        plane = box_pass(&plane, width, height, r as usize, horizontal);
        stages.push(plane.clone());
    }
    stages
}

/// Replica of `cs_roughen_box`: mean of 2r+1 taps, zero outside the plane, along one axis.
fn box_pass(plane: &[f32], w: usize, h: usize, r: usize, horizontal: bool) -> Vec<f32> {
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0f32;
            for t in 0..=2 * r {
                let o = t as isize - r as isize;
                let (qx, qy) = if horizontal { (x as isize + o, y as isize) } else { (x as isize, y as isize + o) };
                if qx >= 0 && qy >= 0 && (qx as usize) < w && (qy as usize) < h {
                    acc += plane[qy as usize * w + qx as usize];
                }
            }
            out[y * w + x] = acc / (2 * r + 1) as f32;
        }
    }
    out
}

#[cfg(test)]
mod kernel_semantics_tests {
    fn kernel_depth(alpha: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
        super::depth_stages(alpha, w, h, sigma).pop().unwrap_or_default()
    }

    #[test]
    fn kernel_depth_passes_match_cpu_blur_plane() {
        let (w, h) = (67usize, 41usize);
        let alpha: Vec<f32> = (0..w * h).map(|i| if (i * 37 + i / w) % 11 < 6 { 1.0 } else { ((i * 13) % 17) as f32 / 17.0 }).collect();
        for sigma in [0.25f32, 0.5, 1.0, 2.5, 5.0, 12.0] {
            let cpu = crate::vfx::blur_plane(&alpha, w, h, sigma);
            let gpu = kernel_depth(&alpha, w, h, sigma);
            let worst = cpu.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            eprintln!("sigma {sigma}: worst depth diff {worst:e}");
            assert!(worst < 1e-4, "sigma {sigma}: depth differs by {worst}");
        }
    }

    /// The corner 001 of the origin cell at octave 0 (z = -1 after the offset): the CPU gradient is
    /// -1 because the cell hash has mod 12 = 4 (x + z). A GPU that returns +1 has a different hash or index.
    #[test]
    fn corner_001_gradient_sign_at_origin() {
        use super::{grad_reference, hash_cell_reference};
        let h = hash_cell_reference(0, 0, 1, 0);
        assert_eq!(h % 12, 4);
        assert_eq!(grad_reference(h, 0.0, 0.0, -1.0), -1.0);
    }
}
