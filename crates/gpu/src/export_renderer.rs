//! GPU export renderer. The required API returns a resident texture view and rejects unsupported
//! plans. A separate compatibility method exists for CPU-parity tests, not GPU-selected export.
//!
//! Renderers share one off-screen wgpu device (no window), created by the first and kept for the
//! process, so an export can run several (one per export worker; each has its own compositor and
//! textures) without opening a device each. `new_required` reports adapter and device failures.
//! `new` remains optional for legacy availability probes; GPU-selected export remains gated until
//! the resident encoder path exists.

use crate::{GpuCompositor, prepare, resident::ResidentNv12Binding, yuv_output::Nv12Layout};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::plan::{
    DeviceRequirement, EligibilityFailure, FramePlan, GpuPlanError, PlannedGpuOp, RequiredFramePlan, ResidentRequiredFramePlan, TextureFormatRequirement,
    materialize_frame_required, plan_frame, preflight_frame_required,
};
use filmcraft_render::{Image, RenderOptions, SourceProvider};
use filmcraft_time::Tick;

/// Where export rendering spends its time (process-wide totals, for the bench).
pub mod timing {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static SUBMIT_NS: AtomicU64 = AtomicU64::new(0);
    static MAP_WAIT_NS: AtomicU64 = AtomicU64::new(0);
    static CONVERT_NS: AtomicU64 = AtomicU64::new(0);
    static FRAMES: AtomicU64 = AtomicU64::new(0);

    /// Totals since the last [`reset`].
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct GpuTimings {
        /// Recording and submitting the frame's commands.
        pub submit: Duration,
        /// Waiting for the GPU and the staging buffer's mapping.
        pub map_wait: Duration,
        /// Half float → f32 conversion of the read-back pixels.
        pub convert: Duration,
        /// Frames composited by the GPU.
        pub frames: u64,
    }

    fn add(counter: &AtomicU64, d: Duration) {
        counter.fetch_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    pub(crate) fn add_submit(d: Duration) {
        add(&SUBMIT_NS, d);
    }

    pub(crate) fn add_map_wait(d: Duration) {
        add(&MAP_WAIT_NS, d);
    }

    pub(crate) fn add_convert(d: Duration) {
        add(&CONVERT_NS, d);
    }

    pub(crate) fn add_frame() {
        FRAMES.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get() -> GpuTimings {
        let d = |c: &AtomicU64| Duration::from_nanos(c.load(Ordering::Relaxed));
        GpuTimings { submit: d(&SUBMIT_NS), map_wait: d(&MAP_WAIT_NS), convert: d(&CONVERT_NS), frames: FRAMES.load(Ordering::Relaxed) }
    }

    pub fn reset() {
        for c in [&SUBMIT_NS, &MAP_WAIT_NS, &CONVERT_NS, &FRAMES] {
            c.store(0, Ordering::Relaxed);
        }
    }
}

/// Set once the legacy shared export device reported an uncaptured error or was lost. The device
/// is shared by every legacy renderer and lives in a `OnceLock`, so it is not recreated here.
static DEVICE_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Retire the shared export device, logging the first reason only.
fn mark_device_failed(why: &str) {
    if !DEVICE_FAILED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        log::error!("GPU export device retired for this session ({why})");
    }
}

fn device_failed() -> bool {
    DEVICE_FAILED.load(std::sync::atomic::Ordering::SeqCst)
}

struct AmdDeviceAdmission {
    failed: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

fn strict_error(reason: EligibilityFailure) -> GpuPlanError {
    GpuPlanError { item_id: None, id: None, reason }
}

const REQUIRED_RENDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

fn poll_required_device(device: &wgpu::Device, poll: wgpu::PollType, stage: &'static str) -> Result<wgpu::PollStatus, GpuPlanError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| device.poll(poll)))
        .map_err(|_| strict_error(EligibilityFailure::RuntimeFailure { stage, detail: "device poll panicked".into() }))?
        .map_err(|error| strict_error(EligibilityFailure::RuntimeFailure { stage, detail: error.to_string() }))
}

fn validate_required_adapter(info: &wgpu::AdapterInfo) -> Result<(), GpuPlanError> {
    if info.vendor != 0x1002 {
        return Err(strict_error(EligibilityFailure::UnsupportedAdapter { vendor: info.vendor }));
    }
    if !matches!(info.device_type, wgpu::DeviceType::IntegratedGpu | wgpu::DeviceType::DiscreteGpu) {
        return Err(strict_error(EligibilityFailure::UnsupportedDeviceRequirement { requirement: DeviceRequirement::PhysicalGpu }));
    }
    Ok(())
}

pub struct ExportRenderer {
    _instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    compositor: GpuCompositor,
    admission: Option<AmdDeviceAdmission>,
}

impl ExportRenderer {
    /// Optional device for legacy availability probes. GPU-required callers must use
    /// [`Self::new_required`] to receive an actionable error.
    pub fn new() -> Option<Self> {
        // `None` is kept too: a machine without an adapter does not search again on every export
        static SHARED: std::sync::OnceLock<Option<(wgpu::Instance, wgpu::Adapter, wgpu::Device, wgpu::Queue)>> = std::sync::OnceLock::new();
        let (instance, adapter, device, queue) = SHARED
            .get_or_init(|| {
                // A build without a wgpu backend for this platform (`cargo build -p filmcraft-cli`
                // alone: this crate asks for none) has no GPU to offer, and `Instance::default()`
                // panics there.
                if wgpu::Instance::enabled_backend_features().is_empty() {
                    return None;
                }
                let instance = wgpu::Instance::default();
                let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
                let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
                // wgpu's default handler panics on a validation or out-of-memory error; retire
                // this optional legacy device so required callers cannot mistake it for healthy.
                device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| mark_device_failed(&format!("GPU error: {e}"))));
                device.set_device_lost_callback(|reason, msg| mark_device_failed(&format!("GPU device lost ({reason:?}): {msg}")));
                Some((instance, adapter, device, queue))
            })
            .as_ref()?
            .clone();
        if device_failed() {
            return None;
        }
        let compositor = GpuCompositor::new(&device, &queue);
        Some(Self { _instance: instance, adapter, device, queue, compositor, admission: None })
    }

    /// Admit the actual physical AMD adapter and create the strict device from that same adapter.
    /// This factory does not satisfy the separate resident VA-API export gate.
    pub fn new_required() -> Result<Self, GpuPlanError> {
        if wgpu::Instance::enabled_backend_features().is_empty() {
            return Err(strict_error(EligibilityFailure::UnsupportedDeviceRequirement { requirement: DeviceRequirement::DeviceAvailable }));
        }
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
            .into_iter()
            .find(|adapter| validate_required_adapter(&adapter.get_info()).is_ok())
            .ok_or_else(|| strict_error(EligibilityFailure::UnsupportedAdapter { vendor: 0 }))?;
        validate_required_adapter(&adapter.get_info())?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|error| strict_error(EligibilityFailure::RuntimeFailure { stage: "AMD device request", detail: error.to_string() }))?;
        Self::from_required_device(instance, adapter, device, queue)
    }

    /// Attach the host's admitted interop device so composition and VA transfers share one queue.
    pub fn from_required_device(instance: wgpu::Instance, adapter: wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, GpuPlanError> {
        validate_required_adapter(&adapter.get_info())?;
        let failed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let errors = failed.clone();
        device.on_uncaptured_error(std::sync::Arc::new(move |error: wgpu::Error| {
            *errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_string());
        }));
        let errors = failed.clone();
        device.set_device_lost_callback(move |reason, message| {
            *errors.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format!("device lost ({reason:?}): {message}"));
        });
        let mut compositor = GpuCompositor::new(&device, &queue);
        compositor.strict_transition = Some(crate::transition_catalog::TransitionCatalogStage::new(&device, crate::ACCUM_FORMAT));
        let renderer = Self { _instance: instance, adapter, device, queue, compositor, admission: Some(AmdDeviceAdmission { failed }) };
        poll_required_device(&renderer.device, wgpu::PollType::Poll, "AMD pipeline creation")?;
        renderer.check_required_admission()?;
        Ok(renderer)
    }

    fn check_required_admission(&self) -> Result<(), GpuPlanError> {
        let admission = self
            .admission
            .as_ref()
            .ok_or_else(|| strict_error(EligibilityFailure::UnsupportedDeviceRequirement { requirement: DeviceRequirement::StrictAdmission }))?;
        validate_required_adapter(&self.adapter.get_info())?;
        if let Some(detail) = admission.failed.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() {
            return Err(strict_error(EligibilityFailure::RuntimeFailure { stage: "AMD device", detail }));
        }
        Ok(())
    }

    /// Metadata preflight precedes every source-provider call. The resulting receipt remains
    /// attached to every materialized leaf for the strict compositor's upload boundary.
    pub fn plan_required(
        &self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: RenderOptions,
        sources: &dyn SourceProvider,
    ) -> Result<RequiredFramePlan, GpuPlanError> {
        let receipt = preflight_frame_required(project, seq, t, opts)?;
        self.check_required_admission()?;
        GpuCompositor::validate_required_routes(receipt.nodes())?;
        let plan = materialize_frame_required(receipt, sources)?;
        self.preflight_required_device(&plan)?;
        Ok(plan)
    }

    /// Render a validated chain into a resident linear GPU texture, without pixel readback.
    /// Initial decoded-source upload is permitted here; this is not resident AmdRequired export.
    pub fn render_required(
        &mut self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: RenderOptions,
        sources: &dyn SourceProvider,
    ) -> Result<(wgpu::TextureView, (u32, u32)), GpuPlanError> {
        let plan = self.plan_required(project, seq, t, opts, sources)?;
        self.check_required_admission()?;
        let (view, size, submission) = self.compositor.composite_required(&plan)?;
        match poll_required_device(
            &self.device,
            wgpu::PollType::Wait { submission_index: Some(submission), timeout: Some(REQUIRED_RENDER_TIMEOUT) },
            "AMD required render completion",
        )? {
            wgpu::PollStatus::QueueEmpty | wgpu::PollStatus::WaitSucceeded => {}
            wgpu::PollStatus::Poll => {
                return Err(strict_error(EligibilityFailure::RuntimeFailure {
                    stage: "AMD required render completion",
                    detail: "bounded wait returned without completing submission".into(),
                }));
            }
        }
        self.check_required_admission()?;
        Ok((view, size))
    }

    /// Check materialized extents and every operation against this exact adapter/device.
    pub fn preflight_required_device(&self, plan: &RequiredFramePlan) -> Result<(), GpuPlanError> {
        self.check_required_admission()?;
        GpuCompositor::validate_required_routes(plan.nodes())?;
        let limits = self.device.limits();
        let (w, h) = plan.size();
        let max_side = plan
            .layers()
            .map(|layer| {
                let frame = layer.layer().frame.width.max(layer.layer().frame.height);
                let effect = layer.layer().fx.as_ref().map_or(0, |fx| fx.size.0.max(fx.size.1));
                frame.max(effect) as usize
            })
            .fold(w.max(h), usize::max);
        if max_side > limits.max_texture_dimension_2d as usize {
            return Err(strict_error(EligibilityFailure::UnsupportedDeviceRequirement {
                requirement: DeviceRequirement::TextureDimension { required: u32::try_from(max_side).unwrap_or(u32::MAX) },
            }));
        }
        // These formats back the shared compositor even when an operation has no extra resources.
        for (format, requirement, usages) in [
            (
                wgpu::TextureFormat::Rgba16Float,
                TextureFormatRequirement::Rgba16Float,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            ),
            (
                wgpu::TextureFormat::Rgba32Float,
                TextureFormatRequirement::Rgba32Float,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            ),
        ] {
            if (format != wgpu::TextureFormat::Rgba32Float || plan.layers().any(|layer| layer.layer().fx.is_some()))
                && !self.adapter.get_texture_format_features(format).allowed_usages.contains(usages)
            {
                return Err(strict_error(EligibilityFailure::UnsupportedDeviceRequirement {
                    requirement: DeviceRequirement::TextureFormat { format: requirement },
                }));
            }
        }
        for node in plan.nodes() {
            for operation in node.operations() {
                let operation_id = match operation {
                    PlannedGpuOp::Effect { id, .. } | PlannedGpuOp::Transition { id, .. } => id,
                };
                if matches!(operation, PlannedGpuOp::Effect { .. }) && self.compositor.fx.is_none() {
                    return Err(GpuPlanError {
                        item_id: Some(node.item_id()),
                        id: Some(operation_id.clone()),
                        reason: EligibilityFailure::UnsupportedOperation {
                            id: match operation {
                                PlannedGpuOp::Effect { id, .. } | PlannedGpuOp::Transition { id, .. } => id.clone(),
                            },
                        },
                    });
                }
                let requirements = match operation {
                    PlannedGpuOp::Effect { requirements, .. } | PlannedGpuOp::Transition { requirements, .. } => requirements,
                };
                let reason = if u32::from(requirements.sampled_inputs) > limits.max_sampled_textures_per_shader_stage {
                    Some(DeviceRequirement::SampledTextures { required: requirements.sampled_inputs.into() })
                } else if requirements.compute.as_ref().is_some_and(|compute| {
                    let [x, y, z] = compute.workgroup_size;
                    x > limits.max_compute_workgroup_size_x
                        || y > limits.max_compute_workgroup_size_y
                        || z > limits.max_compute_workgroup_size_z
                        || x.checked_mul(y).and_then(|v| v.checked_mul(z)).is_none_or(|v| v > limits.max_compute_invocations_per_workgroup)
                        || compute.storage_buffers > limits.max_storage_buffers_per_shader_stage
                        || compute.storage_bytes > limits.max_storage_buffer_binding_size
                }) {
                    Some(DeviceRequirement::ComputeLimits)
                } else {
                    None
                };
                if let Some(requirement) = reason {
                    return Err(GpuPlanError {
                        item_id: Some(node.item_id()),
                        id: Some(operation_id.clone()),
                        reason: EligibilityFailure::UnsupportedDeviceRequirement { requirement },
                    });
                }
                for texture in &requirements.textures {
                    let format = match texture.format {
                        TextureFormatRequirement::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
                        TextureFormatRequirement::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
                        TextureFormatRequirement::R8Unorm => wgpu::TextureFormat::R8Unorm,
                        TextureFormatRequirement::Rg8Unorm => wgpu::TextureFormat::Rg8Unorm,
                        TextureFormatRequirement::R16Unorm => wgpu::TextureFormat::R16Unorm,
                        TextureFormatRequirement::Rg16Unorm => wgpu::TextureFormat::Rg16Unorm,
                    };
                    let mut usages = wgpu::TextureUsages::empty();
                    if texture.sampled {
                        usages |= wgpu::TextureUsages::TEXTURE_BINDING;
                    }
                    if texture.storage {
                        usages |= wgpu::TextureUsages::STORAGE_BINDING;
                    }
                    if texture.render_attachment {
                        usages |= wgpu::TextureUsages::RENDER_ATTACHMENT;
                    }
                    if !self.adapter.get_texture_format_features(format).allowed_usages.contains(usages)
                        || texture.extent.0.max(texture.extent.1) > limits.max_texture_dimension_2d
                    {
                        return Err(GpuPlanError {
                            item_id: Some(node.item_id()),
                            id: Some(operation_id.clone()),
                            reason: EligibilityFailure::UnsupportedDeviceRequirement {
                                requirement: DeviceRequirement::TextureFormat { format: texture.format },
                            },
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Handles from the export device; external GPU interop must use this same device and queue.
    pub fn wgpu_handles(&self) -> (&wgpu::Adapter, &wgpu::Device, &wgpu::Queue) {
        (&self.adapter, &self.device, &self.queue)
    }

    /// CPU-compatible parity helper. May return a CPU-planned image or `None` after GPU failure.
    /// Never use from GPU-selected or required export; use [`Self::render_required`].
    pub fn render_cpu_compatible(&mut self, project: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Option<Image> {
        if self.admission.is_some() {
            return None;
        }
        let plan = plan_frame(project, seq, t, opts, sources);
        match &plan {
            // Already the CPU image: return it unchanged instead of rendering it twice.
            FramePlan::Image(img) => Some(img.clone()),
            FramePlan::Layers { .. } | FramePlan::Composite { .. } => {
                if device_failed() {
                    return None;
                }
                let prep = prepare(&plan);
                let (w, h, px) = self.compositor.render_export_prepared(&plan, Some(&prep))?;
                // an error reported while this frame was drawn leaves its pixels untrusted
                if device_failed() {
                    return None;
                }
                timing::add_frame();
                Some(Image { w: w as usize, h: h as usize, px })
            }
        }
    }

    /// CPU-source NV12 export remains unsupported; strict resident export accepts GPU bindings.
    pub fn render_nv12_buffer(
        &mut self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: RenderOptions,
        sources: &dyn SourceProvider,
        output: wgpu::Buffer,
    ) -> Result<(wgpu::Buffer, Nv12Layout, wgpu::SubmissionIndex), GpuPlanError> {
        self.render_nv12_buffer_coded(project, seq, t, opts, sources, output, None)
    }

    /// CPU-source NV12 export remains unsupported; strict resident export accepts GPU bindings.
    pub fn render_nv12_buffer_coded(
        &mut self,
        project: &Project,
        seq: ItemId,
        t: Tick,
        opts: RenderOptions,
        sources: &dyn SourceProvider,
        output: wgpu::Buffer,
        coded_size: Option<(u32, u32)>,
    ) -> Result<(wgpu::Buffer, Nv12Layout, wgpu::SubmissionIndex), GpuPlanError> {
        let _ = (project, seq, t, opts, sources, output, coded_size);
        self.check_required_admission()?;
        Err(strict_error(EligibilityFailure::UnsupportedOperation { id: "resident NV12 export".into() }))
    }

    /// Render resident NV12 layers, typed effects and typed transitions directly into a caller-owned
    /// NV12 buffer. Anything without a resident route fails closed.
    pub fn render_resident_nv12_buffer_coded(
        &mut self,
        plan: &ResidentRequiredFramePlan,
        sources: &[ResidentNv12Binding<'_>],
        output: wgpu::Buffer,
        coded_size: Option<(u32, u32)>,
    ) -> Result<(wgpu::Buffer, Nv12Layout, wgpu::SubmissionIndex), GpuPlanError> {
        self.check_required_admission()?;
        if !output.usage().contains(wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC) {
            return Err(strict_error(EligibilityFailure::InvalidParameter {
                id: "resident NV12 output".into(),
                parameter: "buffer requires STORAGE and COPY_SRC usage".into(),
            }));
        }
        for (format, requirement, usages) in [
            (wgpu::TextureFormat::R8Unorm, TextureFormatRequirement::R8Unorm, wgpu::TextureUsages::TEXTURE_BINDING),
            (wgpu::TextureFormat::Rg8Unorm, TextureFormatRequirement::Rg8Unorm, wgpu::TextureUsages::TEXTURE_BINDING),
            (
                wgpu::TextureFormat::Rgba16Float,
                TextureFormatRequirement::Rgba16Float,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
            ),
        ] {
            if !self.adapter.get_texture_format_features(format).allowed_usages.contains(usages) {
                return Err(strict_error(EligibilityFailure::UnsupportedDeviceRequirement {
                    requirement: DeviceRequirement::TextureFormat { format: requirement },
                }));
            }
        }
        let (visible_width, visible_height) = plan.size();
        let visible_width = u32::try_from(visible_width).map_err(|_| {
            strict_error(EligibilityFailure::InvalidParameter { id: "resident NV12 output".into(), parameter: "visible width exceeds GPU range".into() })
        })?;
        let visible_height = u32::try_from(visible_height).map_err(|_| {
            strict_error(EligibilityFailure::InvalidParameter { id: "resident NV12 output".into(), parameter: "visible height exceeds GPU range".into() })
        })?;
        let (coded_width, coded_height) = coded_size.unwrap_or((visible_width, visible_height));
        let layout = Nv12Layout::with_coded_size(visible_width, visible_height, coded_width, coded_height).ok_or_else(|| {
            strict_error(EligibilityFailure::InvalidParameter { id: "resident NV12 output".into(), parameter: "invalid checked NV12 output layout".into() })
        })?;
        let limits = self.device.limits();
        let groups_x = coded_width.div_ceil(8);
        let groups_y = coded_height.div_ceil(2);
        if layout.size > output.size()
            || layout.size > limits.max_storage_buffer_binding_size
            || layout.size > limits.max_buffer_size
            || groups_x.div_ceil(8) > limits.max_compute_workgroups_per_dimension
            || groups_y.div_ceil(8) > limits.max_compute_workgroups_per_dimension
        {
            return Err(strict_error(EligibilityFailure::UnsupportedDeviceRequirement { requirement: DeviceRequirement::ComputeLimits }));
        }

        let (accum, (width, height), mut encoder) = self.compositor.composite_resident_flat(plan, sources)?;
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let recorded = self.compositor.nv12_output.record_coded(&self.device, &mut encoder, &accum, &output, width, height, coded_width, coded_height);
        let output_layout = match (recorded, crate::poll_now(scope.pop())) {
            (_, Some(Some(error))) => {
                return Err(strict_error(EligibilityFailure::RuntimeFailure { stage: "resident NV12 output recording", detail: error.to_string() }));
            }
            (_, None) => {
                return Err(strict_error(EligibilityFailure::RuntimeFailure {
                    stage: "resident NV12 output recording",
                    detail: "validation scope remained pending".into(),
                }));
            }
            (None, Some(None)) => {
                return Err(strict_error(EligibilityFailure::RuntimeFailure {
                    stage: "resident NV12 output recording",
                    detail: "prevalidated NV12 output pass was rejected".into(),
                }));
            }
            (Some(layout), Some(None)) => layout,
        };
        let submission = self.queue.submit([encoder.finish()]);
        Ok((output, output_layout, submission))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{f16_to_f32, f32_to_f16};
    use filmcraft_export::FrameRenderer;
    use filmcraft_media::generators::GeneratorSource;
    use filmcraft_media::{Generator, MediaSource};
    use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, Project, SequenceSettings, TrackKind};
    use filmcraft_render::SourceMap;
    use filmcraft_time::{TICKS_PER_SECOND, Tick, TimeRange};
    use std::sync::Arc;

    #[test]
    fn required_adapter_admission_checks_vendor_and_physical_device() {
        let mut info = wgpu::AdapterInfo::new(wgpu::DeviceType::Other, wgpu::Backend::Vulkan);
        for (vendor, device_type) in [
            (0x8086, wgpu::DeviceType::IntegratedGpu),
            (0, wgpu::DeviceType::DiscreteGpu),
            (0x1002, wgpu::DeviceType::Cpu),
            (0x1002, wgpu::DeviceType::VirtualGpu),
        ] {
            info.vendor = vendor;
            info.device_type = device_type;
            assert!(validate_required_adapter(&info).is_err());
        }
        info.vendor = 0x1002;
        info.device_type = wgpu::DeviceType::IntegratedGpu;
        assert!(validate_required_adapter(&info).is_ok());
    }

    const EPS: f32 = 1e-4;

    /// A solid colour generator at 320×180 plus a tone generator (mirrors the export tests'
    /// fixture-free project helper).
    fn project(transform: bool) -> (Arc<Project>, ItemId, SourceMap) {
        let mut p = Project::new("x");
        let g = GeneratorSource::new(
            Generator::ColorMatte { color: [0.2, 0.5, 0.9, 1.0] },
            320,
            180,
            filmcraft_time::FrameRate::FPS_24,
            Tick(2 * TICKS_PER_SECOND),
        );
        let tone = GeneratorSource::new(Generator::Tone { hz: 440.0, db: -6.0 }, 320, 180, filmcraft_time::FrameRate::FPS_24, Tick(2 * TICKS_PER_SECOND));
        let add = |p: &mut Project, g: &GeneratorSource| {
            let info = g.info().clone();
            p.add_item(
                &info.name.clone(),
                Label::Iris,
                ItemKind::Media(MediaClip {
                    media: MediaRef::Generator(g.generator.clone()),
                    info,
                    interpret: Default::default(),
                    mark_in: None,
                    mark_out: None,
                    markers: vec![],
                    offline: false,
                    proxy: None,
                    identity: None,
                }),
                None,
            )
        };
        let red = add(&mut p, &g);
        let t = add(&mut p, &tone);
        let seq =
            p.new_sequence("s", SequenceSettings { width: 320, height: 180, frame_rate: filmcraft_time::FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
        let r = filmcraft_time::FrameRate::FPS_24;
        let _v = p.make_track_item(red, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
        let a = p.make_track_item(t, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
        if transform {
            // Motion + opacity as fixed effects (the way the golden transform scene does it).
            if let Some(clip) = p.sequence_mut(seq).unwrap().video_tracks[0].items.last_mut() {
                let m = clip.effect_mut("motion").unwrap();
                if let Some(pm) = m.param_mut("scale") {
                    pm.value = filmcraft_project::ParamValue::Float(45.0);
                }
                if let Some(pm) = m.param_mut("rotation") {
                    pm.value = filmcraft_project::ParamValue::Float(20.0);
                }
                if let Some(pm) = m.param_mut("position") {
                    pm.value = filmcraft_project::ParamValue::Vec2(filmcraft_geom::Vec2::new(215.0, 70.0));
                }
                if let Some(o) = clip.effect_mut("opacity")
                    && let Some(pm) = o.param_mut("opacity")
                {
                    pm.value = filmcraft_project::ParamValue::Float(70.0);
                }
            }
        }
        p.sequence_mut(seq).unwrap().audio_tracks[0].items.push(a);
        let mut m = SourceMap::default();
        m.0.insert(red, Arc::new(g));
        m.0.insert(t, Arc::new(tone));
        (Arc::new(p), seq, m)
    }

    /// GPU parity: p99 of the per-pixel max RGB difference (over black, 8-bit) ≤ 6 and mean < 1.5
    /// — the same criterion the golden GPU-vs-CPU parity tests use.
    fn assert_parity(cpu: &Image, gpu: &Image, what: &str) {
        assert_eq!((cpu.w, cpu.h), (gpu.w, gpu.h), "{what}: size mismatch");
        let to8 = |img: &Image| -> Vec<u8> { img.over_black_rgba8() };
        let (a, b) = (to8(cpu), to8(gpu));
        let mut d: Vec<u32> = a
            .as_chunks::<4>()
            .0
            .iter()
            .zip(b.as_chunks::<4>().0)
            .map(|(x, y)| (0..3).map(|k| (x[k] as u32).abs_diff(y[k] as u32)).max().unwrap_or(0))
            .collect();
        d.sort_unstable();
        let p99 = d[d.len() * 99 / 100];
        let mean = d.iter().sum::<u32>() as f64 / d.len() as f64;
        eprintln!("{what}: GPU vs CPU over-black p99 {p99}, mean {mean:.3}");
        assert!(p99 <= 6 && mean < 1.5, "{what}: GPU parity p99 {p99}, mean {mean:.3}");
    }

    #[test]
    fn solid_frame_matches_cpu() {
        let Some(mut r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu export parity): no adapter");
            return;
        };
        let (p, seq, m) = project(false);
        let t = Tick::ZERO;
        let cpu = filmcraft_render::render_sequence(&p, seq, t, RenderOptions::default(), &m);
        let gpu = r.render_cpu_compatible(&p, seq, t, RenderOptions::default(), &m).expect("GPU rendered the frame");
        // The linear premultiplied accumulator should be very close to the CPU float image.
        assert_eq!((cpu.w, cpu.h), (gpu.w, gpu.h));
        let max: f32 = cpu.px.iter().zip(&gpu.px).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        eprintln!("solid: max |GPU-CPU| linear {max:.6}");
        assert!(max < 0.02, "solid frame differs by {max}");
        assert_parity(&cpu, &gpu, "solid");
    }

    #[test]
    fn transformed_frame_matches_cpu() {
        let Some(mut r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu export parity): no adapter");
            return;
        };
        let (p, seq, m) = project(true);
        let t = filmcraft_time::FrameRate::FPS_24.tick_of(10);
        let cpu = filmcraft_render::render_sequence(&p, seq, t, RenderOptions::default(), &m);
        let gpu = r.render_cpu_compatible(&p, seq, t, RenderOptions::default(), &m).expect("GPU rendered the frame");
        assert_parity(&cpu, &gpu, "transformed");
    }

    #[test]
    fn gpu_render_impls_frame_renderer() {
        // dev-dependency: the same wrapper the app and CLI register.
        struct R(ExportRenderer);
        impl FrameRenderer for R {
            fn render(&mut self, project: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Option<Image> {
                self.0.render_cpu_compatible(project, seq, t, opts, sources)
            }
        }
        let Some(r) = ExportRenderer::new() else {
            eprintln!("SKIPPED (gpu export parity): no adapter");
            return;
        };
        let (p, seq, m) = project(false);
        let gpu = R(r).render(&p, seq, Tick::ZERO, RenderOptions::default(), &m).expect("rendered");
        let cpu = filmcraft_render::render_sequence(&p, seq, Tick::ZERO, RenderOptions::default(), &m);
        assert_parity(&cpu, &gpu, "frame renderer");
    }

    #[test]
    fn f16_roundtrip() {
        for v in [0.0f32, 1.0, 0.5, 2.0, 1e-3, 65504.0, -1.0, -0.25] {
            assert!((f16_to_f32(f32_to_f16(v)) - v).abs() <= EPS * v.abs().max(1.0), "f16 {v}");
        }
    }
}
