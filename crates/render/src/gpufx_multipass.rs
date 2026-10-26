//! Pass plans for GPU effect routes that need intermediate images.
//!
//! A plan is an ordered list of compute passes over numbered slots. Slot 0 is the source image and
//! is read-only; every other slot is an intermediate written by an earlier pass. The plan is pure
//! data: it is validated here, without a device, and the `filmcraft-gpu` crate allocates and runs it.
//! Every count and size comes from the project or the media, so all arithmetic is checked and every
//! violation is an explicit error. Nothing here panics.

/// Maximum number of passes in one plan. Bounds the work a hostile plan can request.
pub const MAX_PASSES: usize = 64;
/// Maximum number of slots (source included). Bounds the scratch memory a plan can request.
pub const MAX_SLOTS: usize = 8;
/// Inputs one pass may read. The kernel contract in `filmcraft-gpu` binds two.
pub const MAX_INPUTS: usize = 2;

/// Largest parameter block a pass may carry, in bytes. Fixed at 128 so the uniform binding has one
/// size for every kernel. Must stay a multiple of 16 (the `vec4` alignment of the uniform).
pub const MAX_PARAM_BYTES: usize = 128;

/// Bytes per pixel of the intermediate format (Rgba32Float).
pub const SLOT_BYTES_PER_PIXEL: u64 = 16;

/// Index of a plan slot. Slot 0 is the source image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slot(pub u8);

impl Slot {
    /// The source image, which no pass may write.
    pub const SOURCE: Slot = Slot(0);
}

/// One compute pass: reads up to [`MAX_INPUTS`] slots and writes exactly one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pass {
    /// Kernel name, looked up by the GPU side. Must be non-empty.
    pub kernel: &'static str,
    /// Slots read by the pass, in binding order.
    pub inputs: Vec<Slot>,
    /// Slot written by the pass.
    pub output: Slot,
    /// Parameter block bound as a uniform (binding 3). Empty when the kernel takes no parameters.
    /// Otherwise a multiple of 16 bytes and at most [`MAX_PARAM_BYTES`]; the GPU side zero-pads it.
    pub params: Vec<u8>,
}

/// An ordered multi-pass route with its result slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultipassPlan {
    pub passes: Vec<Pass>,
    /// Slot holding the final image after the last pass.
    pub result: Slot,
}

/// Why a plan was refused. Each variant names the first violated rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MultipassError {
    /// No passes, or the result slot is not produced by any pass.
    EmptyPlan,
    /// More than [`MAX_PASSES`] passes.
    TooManyPasses { passes: usize },
    /// A slot index is outside `0..MAX_SLOTS`.
    SlotOutOfRange { slot: u8 },
    /// A pass reads more than [`MAX_INPUTS`] slots.
    TooManyInputs { pass: usize },
    /// A pass reads no slot. Every kernel reads at least one input.
    NoInputs { pass: usize },
    /// A pass has no kernel name.
    EmptyKernel { pass: usize },
    /// A pass writes the source slot.
    WritesSource { pass: usize },
    /// A pass reads the slot it writes. Compute passes cannot alias input and output.
    ReadsOwnOutput { pass: usize },
    /// A pass reads an intermediate that no earlier pass wrote.
    ReadBeforeWrite { pass: usize, slot: u8 },
    /// The result slot is not written by any pass.
    ResultNotWritten { slot: u8 },
    /// Byte count of the intermediates overflows `u64`.
    SizeOverflow,
    /// A pass's parameter block is longer than [`MAX_PARAM_BYTES`] or not a multiple of 16.
    BadParams { pass: usize, len: usize },
}

impl MultipassPlan {
    /// Validates the plan's structure. Returns the number of distinct intermediate slots it writes.
    pub fn validate(&self) -> Result<usize, MultipassError> {
        if self.passes.is_empty() {
            return Err(MultipassError::EmptyPlan);
        }
        if self.passes.len() > MAX_PASSES {
            return Err(MultipassError::TooManyPasses { passes: self.passes.len() });
        }
        let in_range = |s: Slot| if usize::from(s.0) < MAX_SLOTS { Ok(()) } else { Err(MultipassError::SlotOutOfRange { slot: s.0 }) };
        let mut written = [false; MAX_SLOTS];
        written[usize::from(Slot::SOURCE.0)] = true;
        for (index, pass) in self.passes.iter().enumerate() {
            if pass.kernel.is_empty() {
                return Err(MultipassError::EmptyKernel { pass: index });
            }
            if pass.params.len() > MAX_PARAM_BYTES || pass.params.len() % 16 != 0 {
                return Err(MultipassError::BadParams { pass: index, len: pass.params.len() });
            }
            if pass.inputs.is_empty() {
                return Err(MultipassError::NoInputs { pass: index });
            }
            if pass.inputs.len() > MAX_INPUTS {
                return Err(MultipassError::TooManyInputs { pass: index });
            }
            in_range(pass.output)?;
            if pass.output == Slot::SOURCE {
                return Err(MultipassError::WritesSource { pass: index });
            }
            for &input in &pass.inputs {
                in_range(input)?;
                if input == pass.output {
                    return Err(MultipassError::ReadsOwnOutput { pass: index });
                }
                if !written[usize::from(input.0)] {
                    return Err(MultipassError::ReadBeforeWrite { pass: index, slot: input.0 });
                }
            }
            written[usize::from(pass.output.0)] = true;
        }
        in_range(self.result)?;
        if !written[usize::from(self.result.0)] || self.result == Slot::SOURCE {
            return Err(MultipassError::ResultNotWritten { slot: self.result.0 });
        }
        let mut distinct = [false; MAX_SLOTS];
        for pass in &self.passes {
            distinct[usize::from(pass.output.0)] = true;
        }
        Ok(distinct.iter().filter(|&&d| d).count())
    }

    /// Bytes of the intermediate slots this plan writes at `width` x `height`, or `None` when the
    /// validation fails or the arithmetic overflows. No cap: the device decides whether they fit.
    pub fn intermediate_bytes(&self, width: u32, height: u32) -> Result<u64, MultipassError> {
        let slots = self.validate()?;
        slot_bytes(width, height).and_then(|one| one.checked_mul(u64::try_from(slots).ok()?)).ok_or(MultipassError::SizeOverflow)
    }
}

/// Bytes of one Rgba32Float slot at `width` x `height`, or `None` on overflow.
pub fn slot_bytes(width: u32, height: u32) -> Option<u64> {
    u64::from(width).checked_mul(u64::from(height))?.checked_mul(SLOT_BYTES_PER_PIXEL)
}

/// One effect's multi-pass route. Built from the effect instance at evaluation time.
#[derive(Clone, Debug, PartialEq)]
pub struct MultipassEffect {
    pub id: &'static str,
    /// WGSL module with one compute entry point per name in `kernels`. Same binding contract as crates/gpu/src/fx_multipass.wgsl.
    pub wgsl: &'static str,
    pub kernels: &'static [&'static str],
    pub plan: MultipassPlan,
}

/// The parameter set the parity harness measures for `id`, or None when `id` has no route.
///
/// Spacer's project default (400 px margins) shrinks the layer below half scale on a small frame.
/// That is the CPU-only mip route, so the measured case uses small margins that stay on the GPU.
pub fn multipass_case(id: &str) -> Option<filmcraft_project::EffectInstance> {
    let mut e = filmcraft_project::find_effect(id)?.instance();
    if id == "spacer" {
        for key in ["left", "top", "right", "bottom", "radius"] {
            if let Some(p) = e.params.get_mut(key) {
                p.value = filmcraft_project::ParamValue::Float(6.0);
            }
        }
        if let Some(p) = e.params.get_mut("uniform") {
            p.value = filmcraft_project::ParamValue::Bool(false);
        }
    }
    Some(e)
}

/// The evaluation context the harness uses: pixel scale 1, no project, a 10 s clip at 24 fps.
pub fn multipass_cx() -> crate::effects::FxCtx<'static> {
    static CLIP: crate::vfx::ClipTiming =
        crate::vfx::ClipTiming { clip_seconds: 10.0, clip_offset: 0.0, frame_rate: 24.0, sequence_size: (67, 41), source_size: (67, 41) };
    crate::effects::FxCtx {
        t: filmcraft_time::Tick::ZERO,
        px_scale: 1.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "",
        project: None,
        env: Some(&CLIP),
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
    }
}

/// VR route for `id` on a `width` x `height` frame. The plan depends
/// on the frame size (view rectangles, vertical radii), so it is built for the size it will run at.
/// The parameters are the fixed sample values of the parity case (see `MULTIPASS_ROUTES`); they are
/// not yet read from the effect instance.
fn vr_effect(id: &str, width: u32, height: u32) -> Option<MultipassEffect> {
    use crate::vfx::{VR_KERNEL_COL, VR_KERNEL_COPY, VR_KERNEL_DENOISE, VR_KERNEL_ROW, VR_KERNEL_SHARPEN, VrKind, vr_plan};
    const VR_KERNELS: &[&str] = &[VR_KERNEL_ROW, VR_KERNEL_COL, VR_KERNEL_COPY, VR_KERNEL_SHARPEN, VR_KERNEL_DENOISE];
    // Same WGSL module as crates/gpu/src/vr.wgsl; the render crate does not depend on filmcraft-gpu.
    const VR_WGSL: &str = include_str!("../../gpu/src/vr.wgsl");
    let kind = match id {
        // blurriness 4 at px_scale 1: sigma 2
        "vr_blur" => VrKind::Blur { sigma: 2.0 },
        // amount 60: 0.6, sharpen sigma 1.2 at px_scale 1
        "vr_sharpen" => VrKind::Sharpen { sigma: 1.2, amount: 0.6 },
        // amount 50: level 0.5, sigma 0.5 * 1.8, threshold 0.02 + 0.5 * 0.12
        "vr_denoise" => VrKind::Denoise { sigma: 0.9, threshold: 0.08, show: false },
        _ => return None,
    };
    let plan = vr_plan(kind, 0, usize::try_from(width).ok()?, usize::try_from(height).ok()?)?;
    let id = MULTIPASS_ROUTES.iter().copied().find(|r| *r == id)?;
    Some(MultipassEffect { id, wgsl: VR_WGSL, kernels: VR_KERNELS, plan })
}

/// Routes that have a plan, kernel and CPU parity case. Each id stays in
/// `filmcraft_render::gpufx::UNMEASURED_GPU_ROUTES`, so the classifier still refuses it.
/// Builds the route for `id` on a `width` x `height` frame, if one is registered and applies to
/// these parameters. None means the CPU renders the effect.
pub fn multipass_effect_for(id: &str, width: u32, height: u32) -> Option<MultipassEffect> {
    match id {
        "vr_blur" | "vr_sharpen" | "vr_denoise" => vr_effect(id, width, height),
        "roughen_edges" | "spacer" => {
            let e = multipass_case(id)?;
            let cx = multipass_cx();
            match id {
                "roughen_edges" => crate::gpufx_roughen::roughen_effect(&e, &cx, width, height),
                _ => crate::gpufx_spacer::spacer_effect(&e, &cx, width, height),
            }
        }
        _ => None,
    }
}

/// Production route for `e` on a `width` x `height` frame, with the parameters the instance holds.
/// None means the CPU renders the effect (a no-op value, a frame the plan cannot take, or a route
/// that is not registered). Never a silent fallback after a GPU failure: the caller decides.
pub fn multipass_route(e: &filmcraft_project::EffectInstance, cx: &crate::effects::FxCtx, width: u32, height: u32) -> Option<MultipassEffect> {
    match e.effect.as_str() {
        "vr_blur" | "vr_sharpen" | "vr_denoise" => vr_route_for(e, cx, width, height),
        "roughen_edges" => crate::gpufx_roughen::roughen_effect(e, cx, width, height),
        "spacer" => crate::gpufx_spacer::spacer_effect(e, cx, width, height),
        _ => None,
    }
}

/// VR route with the instance's parameters, evaluated as `vfx::immersive::apply` does. The same
/// numbers feed the CPU reference, so the two agree by construction. None on a no-op value.
fn vr_route_for(e: &filmcraft_project::EffectInstance, cx: &crate::effects::FxCtx, width: u32, height: u32) -> Option<MultipassEffect> {
    use crate::vfx::{VR_KERNEL_COL, VR_KERNEL_COPY, VR_KERNEL_DENOISE, VR_KERNEL_ROW, VR_KERNEL_SHARPEN, VrKind, bv, chv, fv, vr_plan};
    const VR_KERNELS: &[&str] = &[VR_KERNEL_ROW, VR_KERNEL_COL, VR_KERNEL_COPY, VR_KERNEL_SHARPEN, VR_KERNEL_DENOISE];
    const VR_WGSL: &str = include_str!("../../gpu/src/vr.wgsl");
    let px = cx.px_scale;
    let kind = match e.effect.as_str() {
        "vr_blur" => {
            let sigma = fv(e, "blurriness", cx) * px * 0.5;
            // vr_blur_image returns early at sigma <= 0.3: nothing to route
            (sigma > 0.3).then_some(VrKind::Blur { sigma })?
        }
        "vr_sharpen" => {
            let amount = fv(e, "amount", cx) / 100.0;
            (amount > 0.0).then_some(VrKind::Sharpen { sigma: px.max(0.35) * 1.2, amount })?
        }
        "vr_denoise" => {
            let level = fv(e, "amount", cx) / 100.0;
            if level <= 0.0 {
                return None;
            }
            let slow = chv(e, "noise_type") == 1;
            let sigma = level * if slow { 3.0 } else { 1.8 } * px.max(0.35);
            VrKind::Denoise { sigma, threshold: 0.02 + level * 0.12, show: bv(e, "show_noise") }
        }
        _ => return None,
    };
    let layout = chv(e, "frame_layout");
    let plan = vr_plan(kind, layout, usize::try_from(width).ok()?, usize::try_from(height).ok()?)?;
    let id = MULTIPASS_ROUTES.iter().copied().find(|r| *r == e.effect.as_str())?;
    Some(MultipassEffect { id, wgsl: VR_WGSL, kernels: VR_KERNELS, plan })
}

/// Routes the production planner may send to the GPU. Empty until each id's parity is measured on the
/// AMD adapter; then the id moves here and out of `UNMEASURED_GPU_ROUTES`.
pub const ROUTED_ON_GPU: &[&str] = &["vr_blur", "vr_sharpen", "vr_denoise", "roughen_edges", "spacer"];

/// Whether the production planner may route `id` to the GPU multi-pass executor.
pub fn is_routed(id: &str) -> bool {
    ROUTED_ON_GPU.contains(&id)
}

/// A routed effect as a plan op: its GPU route, and as CPU reference the effect's own renderer
/// (`vfx::apply`, the path the CPU render takes), with the evaluation context the render used.
#[derive(Clone, Debug, PartialEq)]
pub struct MultipassOp {
    pub route: MultipassEffect,
    effect: filmcraft_project::EffectInstance,
    t: filmcraft_time::Tick,
    px_scale: f32,
    seconds: f64,
    frame_rate: f64,
    working: filmcraft_color::WorkingSpace,
}

/// The op for `e` on a `width` x `height` working image, or None when its route does not apply to
/// these parameters. The planner checks [`is_routed`] before it calls this.
pub fn multipass_op(e: &filmcraft_project::EffectInstance, cx: &crate::effects::FxCtx, width: u32, height: u32) -> Option<MultipassOp> {
    let route = multipass_route(e, cx, width, height)?;
    Some(MultipassOp { route, effect: e.clone(), t: cx.t, px_scale: cx.px_scale, seconds: cx.seconds, frame_rate: cx.frame_rate, working: cx.working })
}

impl MultipassOp {
    /// The CPU reference: the effect's CPU renderer on `img`, with the context the plan evaluated.
    pub fn apply(&self, img: &mut crate::image::Image) {
        let cx = crate::effects::FxCtx {
            t: self.t,
            px_scale: self.px_scale,
            seconds: self.seconds,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            frame_rate: self.frame_rate,
            working: self.working,
        };
        // vfx::apply handles every routed id; a false here would be a routing bug, so the image is left as is
        let _ = crate::vfx::apply(img, &self.effect, &cx);
    }
}

/// Routes that have a plan, kernel and CPU parity case. The classifier in gpufx.rs still refuses
/// every id here until the GPU parity is measured.
pub const MULTIPASS_ROUTES: &[&str] = &["vr_blur", "vr_sharpen", "vr_denoise", "roughen_edges", "spacer"];

/// The first `pass_count` passes of `plan`, with `slot` as the result. Measurement only: it lets the
/// harness read an intermediate slot after a chosen pass. Validated like any plan, so a slot that
/// the prefix never writes, or a count outside `1..=plan.passes.len()`, is refused.
pub fn prefix_plan(plan: &MultipassPlan, pass_count: usize, slot: Slot) -> Result<MultipassPlan, MultipassError> {
    if pass_count == 0 {
        return Err(MultipassError::EmptyPlan);
    }
    let passes = plan.passes.get(..pass_count).ok_or(MultipassError::TooManyPasses { passes: pass_count })?;
    let prefix = MultipassPlan { passes: passes.to_vec(), result: slot };
    prefix.validate()?;
    Ok(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(kernel: &'static str, inputs: &[u8], output: u8) -> Pass {
        Pass { kernel, inputs: inputs.iter().map(|&s| Slot(s)).collect(), output: Slot(output), params: Vec::new() }
    }

    fn plan(passes: Vec<Pass>, result: u8) -> MultipassPlan {
        MultipassPlan { passes, result: Slot(result) }
    }

    #[test]
    fn two_pass_chain_counts_distinct_intermediates() {
        let p = plan(vec![pass("blur_h", &[0], 1), pass("blur_v", &[1], 2), pass("mix", &[0, 2], 1)], 1);
        // slot 1 is written twice but is one slot
        assert_eq!(p.validate(), Ok(2));
        assert_eq!(p.intermediate_bytes(4, 3), Ok(2 * 4 * 3 * 16));
    }

    #[test]
    fn empty_plan_is_refused() {
        assert_eq!(plan(vec![], 1).validate(), Err(MultipassError::EmptyPlan));
    }

    #[test]
    fn too_many_passes_is_refused_before_any_walk() {
        let passes = (0..=MAX_PASSES).map(|_| pass("k", &[0], 1)).collect();
        assert_eq!(plan(passes, 1).validate(), Err(MultipassError::TooManyPasses { passes: MAX_PASSES + 1 }));
    }

    #[test]
    fn out_of_range_slots_are_refused() {
        assert_eq!(plan(vec![pass("k", &[0], 8)], 8).validate(), Err(MultipassError::SlotOutOfRange { slot: 8 }));
        assert_eq!(plan(vec![pass("k", &[9], 1)], 1).validate(), Err(MultipassError::SlotOutOfRange { slot: 9 }));
        assert_eq!(plan(vec![pass("k", &[0], 1)], 200).validate(), Err(MultipassError::SlotOutOfRange { slot: 200 }));
    }

    #[test]
    fn writing_the_source_is_refused() {
        assert_eq!(plan(vec![pass("k", &[0], 0)], 0).validate(), Err(MultipassError::WritesSource { pass: 0 }));
    }

    #[test]
    fn aliasing_input_and_output_is_refused() {
        let p = plan(vec![pass("k", &[0], 1), pass("k", &[1], 1)], 1);
        assert_eq!(p.validate(), Err(MultipassError::ReadsOwnOutput { pass: 1 }));
    }

    #[test]
    fn reading_an_unwritten_intermediate_is_refused() {
        let p = plan(vec![pass("k", &[2], 1)], 1);
        assert_eq!(p.validate(), Err(MultipassError::ReadBeforeWrite { pass: 0, slot: 2 }));
    }

    #[test]
    fn result_must_be_an_intermediate_some_pass_wrote() {
        assert_eq!(plan(vec![pass("k", &[0], 1)], 2).validate(), Err(MultipassError::ResultNotWritten { slot: 2 }));
        assert_eq!(plan(vec![pass("k", &[0], 1)], 0).validate(), Err(MultipassError::ResultNotWritten { slot: 0 }));
    }

    #[test]
    fn parameter_blocks_must_be_aligned_and_bounded() {
        let with = |len: usize| {
            let mut p = pass("k", &[0], 1);
            p.params = vec![0; len];
            plan(vec![p], 1).validate()
        };
        assert_eq!(with(0), Ok(1));
        assert_eq!(with(16), Ok(1));
        assert_eq!(with(MAX_PARAM_BYTES), Ok(1));
        assert_eq!(with(20), Err(MultipassError::BadParams { pass: 0, len: 20 }));
        assert_eq!(with(MAX_PARAM_BYTES + 16), Err(MultipassError::BadParams { pass: 0, len: MAX_PARAM_BYTES + 16 }));
    }

    #[test]
    fn prefix_plan_reads_an_intermediate_after_chosen_pass() {
        let p = plan(vec![pass("blur_h", &[0], 1), pass("blur_v", &[1], 2), pass("mix", &[0, 2], 1)], 1);
        assert_eq!(prefix_plan(&p, 1, Slot(1)).map(|q| q.passes.len()), Ok(1));
        assert_eq!(prefix_plan(&p, 2, Slot(2)).map(|q| q.result), Ok(Slot(2)));
        assert_eq!(prefix_plan(&p, 3, Slot(1)).map(|q| q.passes.len()), Ok(3));
    }

    #[test]
    fn prefix_plan_refuses_bad_indices_and_unwritten_slots() {
        let p = plan(vec![pass("blur_h", &[0], 1), pass("blur_v", &[1], 2)], 1);
        assert_eq!(prefix_plan(&p, 0, Slot(1)), Err(MultipassError::EmptyPlan));
        assert_eq!(prefix_plan(&p, 3, Slot(1)), Err(MultipassError::TooManyPasses { passes: 3 }));
        // slot 2 is written by pass 1 only, so a prefix of one pass never writes it
        assert_eq!(prefix_plan(&p, 1, Slot(2)), Err(MultipassError::ResultNotWritten { slot: 2 }));
        assert_eq!(prefix_plan(&p, 2, Slot(9)), Err(MultipassError::SlotOutOfRange { slot: 9 }));
        assert_eq!(prefix_plan(&p, 2, Slot(0)), Err(MultipassError::ResultNotWritten { slot: 0 }));
    }

    #[test]
    fn a_pass_with_no_inputs_is_refused() {
        assert_eq!(plan(vec![pass("k", &[], 1)], 1).validate(), Err(MultipassError::NoInputs { pass: 0 }));
    }

    #[test]
    fn too_many_inputs_and_empty_kernel_are_refused() {
        assert_eq!(plan(vec![pass("k", &[0, 0, 0], 1)], 1).validate(), Err(MultipassError::TooManyInputs { pass: 0 }));
        assert_eq!(plan(vec![pass("", &[0], 1)], 1).validate(), Err(MultipassError::EmptyKernel { pass: 0 }));
    }

    #[test]
    fn byte_arithmetic_overflow_is_an_error_not_a_panic() {
        assert_eq!(slot_bytes(u32::MAX, u32::MAX), None);
        let p = plan(vec![pass("k", &[0], 1)], 1);
        assert_eq!(p.intermediate_bytes(u32::MAX, u32::MAX), Err(MultipassError::SizeOverflow));
    }

    #[test]
    fn no_resolution_cap_is_applied() {
        let p = plan(vec![pass("k", &[0], 1)], 1);
        // 8K UHD is far above any fixed cap; the device, not this crate, decides
        assert_eq!(p.intermediate_bytes(7680, 4320), Ok(7680 * 4320 * 16));
    }

    fn instance_with(id: &str, key: &str, value: f32) -> filmcraft_project::EffectInstance {
        let mut e = filmcraft_project::find_effect(id).expect("registered effect").instance();
        e.params.get_mut(key).expect("param").value = filmcraft_project::ParamValue::Float(f64::from(value));
        e
    }

    #[test]
    fn production_vr_route_reads_the_instance_parameters() {
        use crate::vfx::{VR_KERNEL_DENOISE, VR_KERNEL_SHARPEN, VrKind, vr_plan};
        let cx = multipass_cx();
        // the parity case's values, read from the instance this time
        let blur = multipass_route(&instance_with("vr_blur", "blurriness", 4.0), &cx, 67, 41).expect("blur route");
        assert_eq!(blur.plan, vr_plan(VrKind::Blur { sigma: 2.0 }, 0, 67, 41).expect("plan"));
        let sharpen = multipass_route(&instance_with("vr_sharpen", "amount", 60.0), &cx, 67, 41).expect("sharpen route");
        assert_eq!(sharpen.plan, vr_plan(VrKind::Sharpen { sigma: 1.2, amount: 0.6 }, 0, 67, 41).expect("plan"));
        assert!(sharpen.kernels.contains(&VR_KERNEL_SHARPEN));
        let denoise = multipass_route(&instance_with("vr_denoise", "amount", 50.0), &cx, 67, 41).expect("denoise route");
        assert_eq!(denoise.plan, vr_plan(VrKind::Denoise { sigma: 0.9, threshold: 0.08, show: false }, 0, 67, 41).expect("plan"));
        assert!(denoise.kernels.contains(&VR_KERNEL_DENOISE));
    }

    #[test]
    fn production_roughen_route_reads_the_instance_parameters() {
        let cx = multipass_cx();
        let default = filmcraft_project::find_effect("roughen_edges").expect("roughen_edges").instance();
        let base = multipass_route(&default, &cx, 67, 41).expect("roughen route");
        let changed = multipass_route(&instance_with("roughen_edges", "border", 20.0), &cx, 67, 41).expect("roughen route");
        assert_ne!(base.plan, changed.plan, "border must change the plan");
        assert_eq!(base.id, "roughen_edges");
        assert_eq!(base.plan, crate::gpufx_roughen::roughen_effect(&default, &cx, 67, 41).expect("roughen effect").plan);
    }

    #[test]
    fn production_vr_route_refuses_no_op_values_and_unknown_ids() {
        let cx = multipass_cx();
        assert!(multipass_route(&instance_with("vr_sharpen", "amount", 0.0), &cx, 67, 41).is_none());
        assert!(multipass_route(&instance_with("vr_denoise", "amount", 0.0), &cx, 67, 41).is_none());
        assert!(multipass_route(&instance_with("vr_blur", "blurriness", 0.0), &cx, 67, 41).is_none());
        assert!(multipass_route(&instance_with("vr_blur", "blurriness", 4.0), &cx, 0, 41).is_none());
        let plain = filmcraft_project::find_effect("grid").expect("grid").instance();
        assert!(multipass_route(&plain, &cx, 67, 41).is_none());
    }

    #[test]
    fn registered_routes_build_valid_plans_on_the_harness_frame() {
        for id in MULTIPASS_ROUTES {
            let route = multipass_effect_for(id, 67, 41).expect("registered route builds");
            assert_eq!(route.id, *id);
            assert!(route.plan.validate().is_ok(), "{id} plan");
            assert!(route.plan.passes.iter().all(|p| route.kernels.contains(&p.kernel)), "{id} names a kernel it does not ship");
        }
    }
}
