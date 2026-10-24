//! Linux Vulkan helpers for VA-API DMA-BUF interop.

use std::{
    os::fd::OwnedFd,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

pub(crate) use crate::vaapi::DrmNodeId;
use crate::vaapi::ffi;
use crate::vaapi::va::{EncodeSession, ExportedDmaBuf};
use wgpu::{Adapter, CommandEncoder, Device, Queue, SubmissionIndex, Texture, TextureDescriptor};
use wgpu_hal::api::Vulkan;

const DRM_FORMAT_NV12: u32 = u32::from_le_bytes(*b"NV12");
const DRM_FORMAT_R8: u32 = u32::from_le_bytes(*b"R8  ");
const DRM_FORMAT_GR88: u32 = u32::from_le_bytes(*b"GR88");

/// Immutable object metadata. Object position is its identity within the exported descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmaBufObjectLayout {
    pub size: u64,
    pub modifier: u64,
}

/// One memory plane, explicitly mapped to its exported object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmaBufPlaneLayout {
    pub object_index: usize,
    pub format: u32,
    pub offset: u64,
    pub pitch: u64,
    pub width: u32,
    pub height: u32,
}

/// Validated NV12 layout. No FD or GPU resource is manufactured from this metadata alone.
#[derive(Debug)]
pub struct DmaBufLayout {
    node: DrmNodeId,
    size: (u32, u32),
    objects: Vec<DmaBufObjectLayout>,
    planes: Vec<DmaBufPlaneLayout>,
}

impl DmaBufLayout {
    /// Validate two separately exported NV12 planes against the queried format/modifier intersection.
    pub fn validate(
        expected_node: DrmNodeId,
        node: DrmNodeId,
        width: u32,
        height: u32,
        fourcc: u32,
        objects: Vec<DmaBufObjectLayout>,
        planes: Vec<DmaBufPlaneLayout>,
        supported: &[(u32, u64)],
    ) -> Result<Self, String> {
        if node != expected_node {
            return Err("VA and Vulkan render nodes differ".into());
        }
        if fourcc != DRM_FORMAT_NV12 || width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err("resident NV12 requires nonzero even coded dimensions".into());
        }
        if objects.is_empty() || objects.len() > 4 || planes.len() != 2 || objects.iter().any(|o| o.size == 0 || o.modifier == u64::MAX) {
            return Err("invalid DMA-BUF object/plane metadata".into());
        }
        for (index, plane) in planes.iter().enumerate() {
            let object = objects.get(plane.object_index).ok_or("DMA-BUF plane object index out of range")?;
            let (format, w, h, pixel_bytes) = if index == 0 { (DRM_FORMAT_R8, width, height, 1u64) } else { (DRM_FORMAT_GR88, width / 2, height / 2, 2) };
            let row_bytes = u64::from(w).checked_mul(pixel_bytes).ok_or("DMA-BUF row overflow")?;
            if plane.format != format
                || plane.width != w
                || plane.height != h
                || plane.pitch < row_bytes
                || plane.pitch % pixel_bytes != 0
                || plane.offset % pixel_bytes != 0
            {
                return Err("unsupported DMA-BUF plane dimensions/format/alignment".into());
            }
            let end = plane
                .pitch
                .checked_mul(u64::from(h - 1))
                .and_then(|n| n.checked_add(plane.offset))
                .and_then(|n| n.checked_add(row_bytes))
                .ok_or("DMA-BUF plane range overflow")?;
            if end > object.size {
                return Err("DMA-BUF plane exceeds its object".into());
            }
            if !supported.contains(&(format, object.modifier)) {
                return Err("DMA-BUF format/modifier is not importable for requested usage".into());
            }
        }
        if planes[0].object_index == planes[1].object_index {
            let y_end = planes[0]
                .offset
                .checked_add(planes[0].pitch.checked_mul(u64::from(height - 1)).ok_or("DMA-BUF range overflow")?)
                .and_then(|n| n.checked_add(u64::from(width)))
                .ok_or("DMA-BUF range overflow")?;
            let uv_end = planes[1]
                .offset
                .checked_add(planes[1].pitch.checked_mul(u64::from(height / 2 - 1)).ok_or("DMA-BUF range overflow")?)
                .and_then(|n| n.checked_add(u64::from(width)))
                .ok_or("DMA-BUF range overflow")?;
            if planes[0].offset < uv_end && planes[1].offset < y_end {
                return Err("DMA-BUF NV12 planes overlap".into());
            }
        }
        Ok(Self { node, size: (width, height), objects, planes })
    }

    pub fn drm_node_id(&self) -> DrmNodeId {
        self.node
    }
    pub fn size(&self) -> (u32, u32) {
        self.size
    }
    pub fn objects(&self) -> &[DmaBufObjectLayout] {
        &self.objects
    }
    pub fn planes(&self) -> &[DmaBufPlaneLayout] {
        &self.planes
    }
}

/// Extract and validate the exact VA export layout against Vulkan's queried copy-import support.
pub fn validated_nv12_layout(adapter: &Adapter, device: &Device, surface: &ExportedDmaBuf<'_>) -> Result<DmaBufLayout, String> {
    let (width, height) = surface.size();
    if surface.layer_count() != 2 || surface.plane_count(0) != Some(1) || surface.plane_count(1) != Some(1) {
        return Err("VA NV12 export is not two single-plane layers".into());
    }
    let objects = (0..surface.object_count())
        .map(|i| surface.object_layout(i).map(|(size, modifier)| DmaBufObjectLayout { size, modifier }).ok_or_else(|| "VA object metadata missing".to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut planes = Vec::with_capacity(2);
    let mut supported = Vec::with_capacity(2);
    for i in 0..2 {
        let plane = surface.plane(i, 0).ok_or("VA plane metadata missing")?;
        let (w, h, format) = if i == 0 { (width, height, ash::vk::Format::R8_UNORM) } else { (width / 2, height / 2, ash::vk::Format::R8G8_UNORM) };
        query_plane_import(adapter, device, format, plane.modifier, w, h, ash::vk::ImageUsageFlags::TRANSFER_DST)?;
        supported.push((plane.layer_format, plane.modifier));
        planes.push(DmaBufPlaneLayout {
            object_index: plane.object_index,
            format: plane.layer_format,
            offset: u64::from(plane.offset),
            pitch: u64::from(plane.pitch),
            width: w,
            height: h,
        });
    }
    DmaBufLayout::validate(render_node_id(adapter)?, surface.drm_node_id(), width, height, surface.fourcc(), objects, planes, &supported)
}

/// Transfer leases, including quarantine, share this hard bound. Codec DPB is budgeted separately.
pub const MAX_IN_FLIGHT_TRANSFERS: usize = 3;

pub(crate) struct TransferPermit(Arc<AtomicUsize>);
impl TransferPermit {
    pub(crate) fn acquire(count: Arc<AtomicUsize>) -> Result<Self, String> {
        count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| if n < MAX_IN_FLIGHT_TRANSFERS { Some(n + 1) } else { None })
            .map_err(|_| "three transfer leases are still in flight or quarantined")?;
        Ok(Self(count))
    }
}
impl Drop for TransferPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

type CompletionWait = Box<dyn FnMut(Duration) -> Result<(), String> + Send>;
struct QuarantinedTransfer {
    producer_ready: bool,
    producer: CompletionWait,
    consumer: CompletionWait,
    // Retained until both exact milestones complete, including cancellation/device loss.
    #[expect(dead_code, reason = "RAII owner retained until retirement")]
    resources: Box<dyn Send>,
    _permit: TransferPermit,
}
fn protected_completion(wait: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(wait)) {
        Ok(result) => result,
        Err(_) => Err("GPU/VA completion panicked; device loss or retirement remains unproven".into()),
    }
}

impl QuarantinedTransfer {
    fn wait_producer_ready(&mut self, timeout: Duration) -> Result<(), String> {
        if !self.producer_ready {
            protected_completion(|| (self.producer)(timeout))?;
            self.producer_ready = true;
        }
        Ok(())
    }

    fn wait_transfer_retired(&mut self, timeout: Duration) -> Result<(), String> {
        self.wait_producer_ready(timeout)?;
        protected_completion(|| (self.consumer)(timeout))
    }
}

/// A worker unwind/cancellation transfers ownership to quarantine instead of dropping in-flight
/// resources. Successful completion consumes the entry before this destructor runs.
pub(crate) struct PendingTransfer<'a> {
    transfer: Option<QuarantinedTransfer>,
    on_quarantine: Option<Box<dyn FnOnce() + 'a>>,
}
impl PendingTransfer<'_> {
    pub(crate) fn producer_ready(&mut self, timeout: Duration) -> Result<(), String> {
        self.transfer.as_mut().ok_or("transfer already retired")?.wait_producer_ready(timeout)
    }

    pub(crate) fn retired(&mut self, timeout: Duration) -> Result<(), String> {
        self.wait_transfer_retired(timeout)
    }

    fn wait_transfer_retired(&mut self, timeout: Duration) -> Result<(), String> {
        self.transfer.as_mut().ok_or("transfer already retired")?.wait_transfer_retired(timeout)?;
        self.transfer.take();
        self.on_quarantine.take();
        Ok(())
    }
}
fn guarded_submission<'a>(transfer: QuarantinedTransfer, on_quarantine: Box<dyn FnOnce() + 'a>, submit: impl FnOnce()) -> Result<PendingTransfer<'a>, String> {
    let pending = PendingTransfer { transfer: Some(transfer), on_quarantine: Some(on_quarantine) };
    // Install ownership before finish/submit: a driver can submit work and then unwind.
    protected_completion(|| {
        submit();
        Ok(())
    })?;
    Ok(pending)
}

impl Drop for PendingTransfer<'_> {
    fn drop(&mut self) {
        if let Some(transfer) = self.transfer.take() {
            if let Some(poison) = self.on_quarantine.take() {
                poison();
            }
            quarantine().lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(transfer);
        }
    }
}

fn quarantine() -> &'static Mutex<Vec<QuarantinedTransfer>> {
    static QUARANTINE: OnceLock<Mutex<Vec<QuarantinedTransfer>>> = OnceLock::new();
    QUARANTINE.get_or_init(|| Mutex::new(Vec::with_capacity(MAX_IN_FLIGHT_TRANSFERS)))
}
pub(crate) fn transfer_count() -> Arc<AtomicUsize> {
    static COUNT: OnceLock<Arc<AtomicUsize>> = OnceLock::new();
    COUNT.get_or_init(|| Arc::new(AtomicUsize::new(0))).clone()
}

/// Retry only the exact submissions/surfaces held in quarantine. A timeout keeps all owners and
/// their transfer reservations; full quarantine prevents a fourth GPU submission. Device loss
/// never implies retirement. Shutdown retains unresolved owners until process teardown.
// shortcut: unresolved device loss retains at most three transfers until process teardown;
// reclaim earlier only after a trusted device-teardown signal is available.
pub fn retry_quarantined_transfers(timeout: Duration) -> Result<usize, String> {
    let mut pending = quarantine().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = pending.len();
    let mut first_error = None;
    pending.retain_mut(|entry| match entry.wait_transfer_retired(timeout) {
        Ok(()) => false,
        Err(error) => {
            if first_error.is_none() {
                first_error = Some(error);
            }
            true
        }
    });
    match first_error {
        Some(error) => Err(format!("{} GPU/VA transfers remain quarantined: {error}", pending.len())),
        None => Ok(before - pending.len()),
    }
}

struct HeldTransfer {
    _imported: ImportedNv12Surface,
    _source: wgpu::Buffer,
    _surface: ExportedDmaBuf<'static>,
}

/// Imported writable VA-API NV12 input planes. Keep alive until the GPU copy submission completes.
struct ImportedNv12Surface {
    _y: Texture,
    _uv: Texture,
}

#[derive(Clone, Copy)]
enum ExternalImageAccess {
    // Constructed only by the fail-closed gate test: no production decoder requests preserved contents yet.
    #[cfg_attr(not(test), allow(dead_code))]
    PreserveContents,
    FullOverwrite,
}

fn external_sync_gate(foreign_enabled: bool, linear_modifier: bool, access: ExternalImageAccess) -> Result<(), String> {
    if !foreign_enabled {
        return Err("resident VA interop requires enabled VK_EXT_queue_family_foreign for external media ownership".into());
    }
    match access {
        ExternalImageAccess::PreserveContents => Err("preserve-content VA/Vulkan import remains unavailable until decoder acquire layout is specified".into()),
        ExternalImageAccess::FullOverwrite if !linear_modifier => {
            Err("VA/Vulkan write-only interop requires validated DRM_FORMAT_MOD_LINEAR modifier 0".into())
        }
        ExternalImageAccess::FullOverwrite => Ok(()),
    }
}

fn require_linear_nv12_layout(objects: &[DmaBufObjectLayout], y_modifier: u64, uv_modifier: u64) -> Result<(), String> {
    if y_modifier != ffi::DRM_FORMAT_MOD_LINEAR
        || uv_modifier != ffi::DRM_FORMAT_MOD_LINEAR
        || objects.iter().any(|object| object.modifier != ffi::DRM_FORMAT_MOD_LINEAR)
    {
        return Err("VA/Vulkan interop is limited to validated DRM_FORMAT_MOD_LINEAR modifier 0".into());
    }
    Ok(())
}

fn require_external_sync(device: &Device, linear_modifier: bool, access: ExternalImageAccess) -> Result<(), String> {
    require_foreign_extension(device)?;
    external_sync_gate(true, linear_modifier, access)
}

fn require_foreign_extension(device: &Device) -> Result<(), String> {
    // SAFETY: the borrowed guard retains the device; this only reads its enabled extensions.
    let hal = unsafe { device.as_hal::<Vulkan>() }.ok_or("interop device is not Vulkan")?;
    if hal.enabled_device_extensions().contains(&ash::ext::queue_family_foreign::NAME) {
        Ok(())
    } else {
        Err("resident VA interop requires enabled VK_EXT_queue_family_foreign for external media ownership".into())
    }
}

fn validation_scope_result(scope: wgpu::ErrorScopeGuard, device: &Device) -> Result<(), String> {
    use std::future::Future;
    let mut result = Box::pin(scope.pop());
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match result.as_mut().poll(&mut context) {
        std::task::Poll::Ready(Some(error)) => return Err(format!("wgpu source-buffer validation failed: {error}")),
        std::task::Poll::Ready(None) => return Ok(()),
        std::task::Poll::Pending => {}
    }
    device.poll(wgpu::PollType::Poll).map_err(|error| format!("wgpu validation scope poll failed: {error:?}"))?;
    match result.as_mut().poll(&mut context) {
        std::task::Poll::Ready(Some(error)) => Err(format!("wgpu source-buffer validation failed: {error}")),
        std::task::Poll::Ready(None) => Ok(()),
        std::task::Poll::Pending => Err("wgpu source-buffer validation scope did not resolve".into()),
    }
}

/// Use wgpu's own resource validation before exposing the buffer's raw Vulkan handle. The
/// validation command buffer is discarded; no pixel bytes are submitted or copied.
fn validate_buffer_on_device(device: &Device, source: &wgpu::Buffer, size: u64) -> Result<(), String> {
    if size < 4 || !size.is_multiple_of(4) || size > source.size() {
        return Err("NV12 validation copy size is invalid".into());
    }
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let validation = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("filmcraft-interop-source-validation"),
        size: 4,
        usage: wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-interop-source-validation") });
    encoder.copy_buffer_to_buffer(source, 0, &validation, 0, 4);
    drop(encoder.finish());
    drop(validation);
    validation_scope_result(scope, device)
}

/// Record copies from a coded-size NV12 buffer into the full exported VA surface.
#[allow(clippy::too_many_arguments)]
fn record_coded_nv12_buffer_to_surface(
    adapter: &Adapter,
    device: &Device,
    queue: &Queue,
    encoder: &mut CommandEncoder,
    source: &wgpu::Buffer,
    surface: &ExportedDmaBuf<'_>,
    width: u32,
    height: u32,
    y_stride: u32,
    uv_stride: u32,
    uv_offset: u64,
    buffer_size: u64,
) -> Result<ImportedNv12Surface, String> {
    require_foreign_extension(device)?;
    let (surface_width, surface_height) = surface.size();
    if surface.fourcc() != DRM_FORMAT_NV12 || width == 0 || height == 0 || width != surface_width || height != surface_height {
        return Err("VA encode surface is not a matching NV12 picture".into());
    }
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err("coded NV12 surface dimensions must be even".into());
    }
    if render_node_id(adapter)? != surface.drm_node_id() {
        return Err("VA and Vulkan render nodes differ".into());
    }
    if surface.layer_count() != 2 || surface.plane_count(0) != Some(1) || surface.plane_count(1) != Some(1) {
        return Err("VA NV12 export is not two single-plane layers".into());
    }

    let uv_width = surface_width.div_ceil(2);
    let uv_height = surface_height.div_ceil(2);
    let uv_row_bytes = uv_width.checked_mul(2).ok_or("NV12 chroma row overflow")?;
    let y_bytes = u64::from(y_stride).checked_mul(u64::from(surface_height)).ok_or("NV12 luma size overflow")?;
    if uv_offset != y_bytes {
        return Err("NV12 chroma offset does not follow the full coded luma plane".into());
    }
    let required = uv_offset
        .checked_add(u64::from(uv_stride).checked_mul(u64::from(uv_height)).ok_or("NV12 chroma size overflow")?)
        .ok_or("NV12 storage-buffer size overflow")?;
    if y_stride < surface_width
        || uv_stride < uv_row_bytes
        || !y_stride.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        || !uv_stride.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        || uv_offset != y_bytes
        || !uv_offset.is_multiple_of(2)
        || required > buffer_size
        || required > source.size()
    {
        return Err("NV12 storage-buffer layout is invalid for wgpu buffer-to-texture copies".into());
    }

    let y_plane = surface.plane(0, 0).ok_or("VA export has no luma plane")?;
    let uv_plane = surface.plane(1, 0).ok_or("VA export has no chroma plane")?;
    validate_plane(y_plane.layer_format, DRM_FORMAT_R8, y_plane.pitch, surface_width, surface_height, y_plane.offset, y_plane.object_size)?;
    validate_plane(
        uv_plane.layer_format,
        DRM_FORMAT_GR88,
        uv_plane.pitch,
        surface_width.div_ceil(2).checked_mul(2).ok_or("VA surface chroma width overflow")?,
        surface_height.div_ceil(2),
        uv_plane.offset,
        uv_plane.object_size,
    )?;

    let layout = validated_nv12_layout(adapter, device, surface)?;
    require_linear_nv12_layout(layout.objects(), y_plane.modifier, uv_plane.modifier)?;
    require_external_sync(device, true, ExternalImageAccess::FullOverwrite)?;
    validate_buffer_on_device(device, source, required)?;
    let y_desc = texture_desc("vaapi-nv12-y", surface_width, surface_height, wgpu::TextureFormat::R8Unorm);
    let uv_desc = texture_desc("vaapi-nv12-uv", surface_width.div_ceil(2), surface_height.div_ceil(2), wgpu::TextureFormat::Rg8Unorm);
    let y = import_dmabuf_texture(
        device,
        y_plane.fd.try_clone_to_owned().map_err(|e| format!("clone VA luma DMA-BUF: {e}"))?,
        &y_desc,
        y_plane.modifier,
        u64::from(y_plane.pitch),
        u64::from(y_plane.offset),
    )?;
    let uv = import_dmabuf_texture(
        device,
        uv_plane.fd.try_clone_to_owned().map_err(|e| format!("clone VA chroma DMA-BUF: {e}"))?,
        &uv_desc,
        uv_plane.modifier,
        u64::from(uv_plane.pitch),
        u64::from(uv_plane.offset),
    )?;

    record_linear_nv12_copy_with_foreign_ownership(
        device,
        queue,
        encoder,
        source,
        &y,
        &uv,
        surface_width,
        surface_height,
        y_stride,
        uv_stride,
        uv_offset,
        required,
    )?;
    Ok(ImportedNv12Surface { _y: y, _uv: uv })
}

#[allow(clippy::too_many_arguments)]
fn record_linear_nv12_copy_with_foreign_ownership(
    device: &Device,
    queue: &Queue,
    encoder: &mut CommandEncoder,
    source: &wgpu::Buffer,
    y: &Texture,
    uv: &Texture,
    width: u32,
    height: u32,
    y_stride: u32,
    uv_stride: u32,
    uv_offset: u64,
    source_bytes: u64,
) -> Result<(), String> {
    use ash::vk;

    // SAFETY: guards retain Vulkan objects for the device that created the encoder and imported planes.
    let hal_device = unsafe { device.as_hal::<Vulkan>() }.ok_or("interop device is not Vulkan")?;
    // SAFETY: the guard retains the queue while checking its exact raw handle.
    let hal_queue = unsafe { queue.as_hal::<Vulkan>() }.ok_or("interop queue is not Vulkan")?;
    if hal_queue.as_raw() != hal_device.raw_queue() {
        return Err("interop queue does not belong to the supplied Vulkan device".into());
    }
    // wgpu resource guards share one non-reentrant lock. Borrow handles separately; the caller
    // retains the source and these private imported textures through the completed transfer.
    let source_buffer = {
        // SAFETY: source was validated on this device and is retained through the transfer.
        let source = unsafe { source.as_hal::<Vulkan>() }.ok_or("validated NV12 source is not Vulkan")?;
        // SAFETY: the live source owns this borrowed handle.
        unsafe { source.raw_handle() }
    };
    let y_image = {
        // SAFETY: this private imported texture stays alive through the transfer.
        let y = unsafe { y.as_hal::<Vulkan>() }.ok_or("NV12 luma texture is not Vulkan")?;
        // SAFETY: the imported texture owns this borrowed handle.
        unsafe { y.raw_handle() }
    };
    let uv_image = {
        // SAFETY: this private imported texture stays alive through the transfer.
        let uv = unsafe { uv.as_hal::<Vulkan>() }.ok_or("NV12 chroma texture is not Vulkan")?;
        // SAFETY: the imported texture owns this borrowed handle.
        unsafe { uv.raw_handle() }
    };
    let family = hal_device.queue_family_index();
    let foreign = vk::QUEUE_FAMILY_FOREIGN_EXT;
    // UNDEFINED discards prior VA pixels; the two plane copies overwrite the full coded picture.
    let acquire = [
        foreign_image_barrier(y_image, foreign, family, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, true),
        foreign_image_barrier(uv_image, foreign, family, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, true),
    ];
    let release = [
        foreign_image_barrier(y_image, family, foreign, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::GENERAL, false),
        foreign_image_barrier(uv_image, family, foreign, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::GENERAL, false),
    ];
    let source_barrier = source_buffer_barrier(source_buffer, source_bytes);
    // shortcut: raw source use is not in wgpu-core's tracker; these barriers cover prior writes and future same-queue reuse.
    let source_return_barrier = source_buffer_return_barrier(source_buffer, source_bytes);
    let y_copy = vk::BufferImageCopy::default()
        .buffer_offset(0)
        .buffer_row_length(y_stride)
        .buffer_image_height(height)
        .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).mip_level(0).base_array_layer(0).layer_count(1))
        .image_extent(vk::Extent3D { width, height, depth: 1 });
    let uv_height = height / 2;
    let uv_copy = vk::BufferImageCopy::default()
        .buffer_offset(uv_offset)
        .buffer_row_length(uv_stride / 2)
        .buffer_image_height(uv_height)
        .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).mip_level(0).base_array_layer(0).layer_count(1))
        .image_extent(vk::Extent3D { width: width / 2, height: uv_height, depth: 1 });
    let mut result = Err("wgpu did not provide a Vulkan command encoder".to_string());
    // SAFETY: this path records Vulkan commands only; it does not call wgpu encoder methods on this encoder.
    unsafe {
        encoder.as_hal_mut::<Vulkan, _, _>(|hal_encoder| {
            let Some(hal_encoder) = hal_encoder else { return };
            // SAFETY: the HAL guard gives the active command buffer owned by wgpu.
            let command_buffer = hal_encoder.raw_handle();
            let raw = hal_device.raw_device();
            // SAFETY: exact same exclusive images cross from FOREIGN, are fully overwritten, then released to FOREIGN.
            raw.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[source_barrier],
                &[],
            );
            raw.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &acquire,
            );
            raw.cmd_copy_buffer_to_image(command_buffer, source_buffer, y_image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[y_copy]);
            raw.cmd_copy_buffer_to_image(command_buffer, source_buffer, uv_image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[uv_copy]);
            raw.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[source_return_barrier],
                &[],
            );
            raw.cmd_pipeline_barrier(
                command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &release,
            );
            result = Ok(());
        });
    }
    result
}

fn source_buffer_barrier(buffer: ash::vk::Buffer, size: u64) -> ash::vk::BufferMemoryBarrier<'static> {
    use ash::vk;
    vk::BufferMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .buffer(buffer)
        .offset(0)
        .size(size)
}

fn source_buffer_return_barrier(buffer: ash::vk::Buffer, size: u64) -> ash::vk::BufferMemoryBarrier<'static> {
    use ash::vk;
    vk::BufferMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::TRANSFER_READ)
        .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .buffer(buffer)
        .offset(0)
        .size(size)
}

fn foreign_image_barrier(
    image: ash::vk::Image,
    src_family: u32,
    dst_family: u32,
    old_layout: ash::vk::ImageLayout,
    new_layout: ash::vk::ImageLayout,
    acquire: bool,
) -> ash::vk::ImageMemoryBarrier<'static> {
    use ash::vk;
    vk::ImageMemoryBarrier::default()
        .src_access_mask(if acquire { vk::AccessFlags::empty() } else { vk::AccessFlags::TRANSFER_WRITE })
        .dst_access_mask(if acquire { vk::AccessFlags::TRANSFER_WRITE } else { vk::AccessFlags::empty() })
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(src_family)
        .dst_queue_family_index(dst_family)
        .image(image)
        .subresource_range(
            vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).base_mip_level(0).level_count(1).base_array_layer(0).layer_count(1),
        )
}

fn texture_desc(label: &'static str, width: u32, height: u32, format: wgpu::TextureFormat) -> TextureDescriptor<'static> {
    TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    }
}

fn validate_plane(format: u32, expected: u32, pitch: u32, row_bytes: u32, rows: u32, offset: u32, object_size: u32) -> Result<(), String> {
    let end = u64::from(offset)
        .checked_add(u64::from(pitch).checked_mul(u64::from(rows.saturating_sub(1))).ok_or("VA plane size overflow")?)
        .and_then(|end| end.checked_add(u64::from(row_bytes)))
        .ok_or("VA plane range overflow")?;
    if format != expected
        || row_bytes == 0
        || pitch < row_bytes
        || rows == 0
        || end > u64::from(object_size)
        || (expected == DRM_FORMAT_GR88 && (!pitch.is_multiple_of(2) || !offset.is_multiple_of(2) || !row_bytes.is_multiple_of(2)))
    {
        return Err("VA NV12 plane metadata or allocation bounds are invalid".into());
    }
    Ok(())
}

/// Wait for one specific GPU submission before VA begins using the shared surface. On timeout,
/// the submission may still be running; caller must quarantine resources and abort this export.
pub fn wait_for_submission(device: &Device, submission: SubmissionIndex, timeout: Duration) -> Result<(), String> {
    protected_completion(|| {
        device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: Some(timeout) })
            .map(|_| ())
            .map_err(|error| format!("GPU surface write did not complete before VA encode: {error:?}"))
    })
}

/// Copy coded-size NV12 to the VA input surface and return a lease after exact GPU readiness.
/// Caller must encode before `PendingTransfer::retired`; dropping it early quarantines ownership.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_coded_nv12_buffer_to_va_input(
    adapter: &Adapter,
    device: &Device,
    queue: &Queue,
    session: &mut EncodeSession,
    source: &wgpu::Buffer,
    coded_width: u32,
    coded_height: u32,
    y_stride: u32,
    uv_stride: u32,
    uv_offset: u64,
    buffer_size: u64,
    timeout: Duration,
) -> Result<PendingTransfer<'static>, String> {
    if timeout.is_zero() {
        return Err("GPU-to-VA wait timeout must be positive".into());
    }
    require_foreign_extension(device)?;
    session.require_bounded_sync()?;
    filmcraft_export::check_runtime_ram_ceiling().map_err(|error| error.to_string())?;
    let _ = retry_quarantined_transfers(Duration::ZERO);
    let permit = TransferPermit::acquire(transfer_count())?;
    let surface = session.export_input_surface()?;
    let consumer = surface.retirement_waiter()?;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-vaapi-nv12-copy") });
    let imported = record_coded_nv12_buffer_to_surface(
        adapter,
        device,
        queue,
        &mut encoder,
        source,
        &surface,
        coded_width,
        coded_height,
        y_stride,
        uv_stride,
        uv_offset,
        buffer_size,
    )?;
    let submission = Arc::new(Mutex::new(None::<wgpu::SubmissionIndex>));
    let producer_submission = submission.clone();
    let retained_device = device.clone();
    let transfer = QuarantinedTransfer {
        producer_ready: false,
        producer: Box::new(move |timeout| {
            let exact = producer_submission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .ok_or("exact SubmissionIndex unavailable; retirement remains unproven")?;
            wait_for_submission(&retained_device, exact, timeout)
        }),
        consumer,
        resources: Box::new(HeldTransfer { _surface: surface.into_owned(), _imported: imported, _source: source.clone() }),
        _permit: permit,
    };
    let poison = session.poison_token();
    let mut pending = guarded_submission(transfer, Box::new(move || poison.store(true, Ordering::Release)), || {
        let exact = queue.submit([encoder.finish()]);
        *submission.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(exact);
    })?;
    pending.producer_ready(timeout).map_err(|error| format!("GPU producer readiness failed; transfer quarantined: {error}"))?;
    Ok(pending)
}

/// DRM render-node identity shared by Vulkan and VA-API.
/// Vulkan adapter's DRM render node, when the driver exposes `VK_EXT_physical_device_drm`.
pub fn render_node_id(adapter: &Adapter) -> Result<DrmNodeId, String> {
    // SAFETY: the borrowed HAL guard retains the adapter and its Vulkan instance.
    let hal_adapter = unsafe { adapter.as_hal::<Vulkan>() }.ok_or_else(|| "wgpu adapter is not using Vulkan".to_string())?;
    let instance = hal_adapter.shared_instance();
    // SAFETY: the adapter physical device belongs to this live instance.
    let extensions = unsafe { instance.raw_instance().enumerate_device_extension_properties(hal_adapter.raw_physical_device()) }
        .map_err(|e| format!("enumerate Vulkan DRM extension: {e:?}"))?;
    if !extensions.iter().any(|extension| extension.extension_name_as_c_str().is_ok_and(|name| name == ash::ext::physical_device_drm::NAME)) {
        return Err("missing required extension VK_EXT_physical_device_drm".into());
    }
    if instance.instance_api_version() < ash::vk::API_VERSION_1_1 {
        return Err("Vulkan DRM discovery requires Vulkan instance 1.1".into());
    }
    let mut properties = ash::vk::PhysicalDeviceDrmPropertiesEXT::default();
    let mut properties2 = ash::vk::PhysicalDeviceProperties2::default().push_next(&mut properties);
    // SAFETY: adapter and instance guard keep the Vulkan physical device and instance alive.
    unsafe { instance.raw_instance().get_physical_device_properties2(hal_adapter.raw_physical_device(), &mut properties2) };
    if properties.has_render == 0 || properties.render_major < 0 || properties.render_minor < 0 {
        return Err("Vulkan adapter does not expose a DRM render node".into());
    }
    Ok(DrmNodeId {
        major: u32::try_from(properties.render_major).map_err(|_| "invalid Vulkan DRM render major")?,
        minor: u32::try_from(properties.render_minor).map_err(|_| "invalid Vulkan DRM render minor")?,
    })
}

/// Inspect the physical device owned by the exact offscreen renderer adapter. No device or
/// encode session is opened. NV12 is copied from GPU storage; RGBA16F is rendered/stored on GPU.
pub(crate) fn vulkan_video_capabilities(adapter: &Adapter, codec: filmcraft_export::Format) -> Result<crate::amd_backend::VulkanVideoCapabilities, String> {
    use crate::amd_backend::{
        VIDEO_NV12_USAGES, VIDEO_RGBA_USAGES, VulkanVideoProbe, validate_vulkan_video_dependencies, validate_vulkan_video_probe, video_image_usages_supported,
    };
    use ash::vk;
    use filmcraft_export::Format;
    let operation = match codec {
        Format::H264 => vk::VideoCodecOperationFlagsKHR::ENCODE_H264,
        Format::Hevc => vk::VideoCodecOperationFlagsKHR::ENCODE_H265,
        _ => return Err(format!("Vulkan Video codec {codec:?} is unsupported")),
    };
    let drm_node = render_node_id(adapter)?;
    // SAFETY: the borrowed guard retains this adapter's Vulkan physical device and instance.
    let hal = unsafe { adapter.as_hal::<Vulkan>() }.ok_or("wgpu adapter is not using Vulkan")?;
    let shared = hal.shared_instance();
    let instance = shared.raw_instance();
    let physical = hal.raw_physical_device();
    // SAFETY: physical belongs to the live instance held by hal.
    let extensions = unsafe { instance.enumerate_device_extension_properties(physical) }.map_err(|e| format!("enumerate Vulkan Video extensions: {e:?}"))?;
    let codec_extension = if codec == Format::H264 { ash::khr::video_encode_h264::NAME } else { ash::khr::video_encode_h265::NAME };
    for required in [ash::khr::video_queue::NAME, ash::khr::video_encode_queue::NAME, codec_extension, ash::ext::physical_device_drm::NAME] {
        if !extensions.iter().any(|extension| extension.extension_name_as_c_str().is_ok_and(|name| name == required)) {
            return Err(format!("missing required extension {}", required.to_string_lossy()));
        }
    }
    // SAFETY: the guarded physical device belongs to this live instance; metadata query only.
    let physical_api_version = unsafe { instance.get_physical_device_properties(physical) }.api_version;
    let api_version = shared.instance_api_version().min(physical_api_version);
    let synchronization2_extension =
        extensions.iter().any(|extension| extension.extension_name_as_c_str().is_ok_and(|name| name == ash::khr::synchronization2::NAME));
    validate_vulkan_video_dependencies(api_version, synchronization2_extension)?;
    // SAFETY: live instance; only queue family metadata is queried.
    let count = unsafe { instance.get_physical_device_queue_family_properties2_len(physical) };
    if count > 1024 {
        return Err("Vulkan queue family count exceeds discovery limit".into());
    }
    let mut video_queues = vec![vk::QueueFamilyVideoPropertiesKHR::default(); count];
    let mut queues: Vec<_> = video_queues.iter_mut().map(|video| vk::QueueFamilyProperties2::default().push_next(video)).collect();
    // SAFETY: each output chain owns a distinct live video_queues element, with queried length.
    unsafe { instance.get_physical_device_queue_family_properties2(physical, &mut queues) };
    let queue_flags: Vec<_> = queues.iter().map(|queue| queue.queue_family_properties).collect();
    drop(queues);
    let encode_queue = queue_flags.iter().zip(&video_queues).any(|(queue, video)| {
        queue.queue_count > 0 && queue.queue_flags.contains(vk::QueueFlags::VIDEO_ENCODE_KHR) && video.video_codec_operations.contains(operation)
    });
    let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(vk::native::StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH);
    let mut h265_profile = vk::VideoEncodeH265ProfileInfoKHR::default().std_profile_idc(vk::native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN);
    let profile = vk::VideoProfileInfoKHR::default()
        .video_codec_operation(operation)
        .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
        .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
        .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8);
    let profile = if codec == Format::H264 { profile.push_next(&mut h264_profile) } else { profile.push_next(&mut h265_profile) };
    // Resolve explicitly: ash's generated loaders substitute panicking stubs for absent functions.
    // SAFETY: names are static C strings and instance remains alive throughout discovery.
    let capabilities_fn = unsafe { shared.entry().get_instance_proc_addr(instance.handle(), c"vkGetPhysicalDeviceVideoCapabilitiesKHR".as_ptr()) }
        .ok_or("Vulkan Video capabilities entry point unavailable")?;
    // SAFETY: Vulkan defines this symbol with exactly PFN_vkGetPhysicalDeviceVideoCapabilitiesKHR.
    let capabilities_fn: vk::PFN_vkGetPhysicalDeviceVideoCapabilitiesKHR = unsafe { std::mem::transmute(capabilities_fn) };
    // SAFETY: names are static C strings and instance remains alive throughout discovery.
    let formats_fn = unsafe { shared.entry().get_instance_proc_addr(instance.handle(), c"vkGetPhysicalDeviceVideoFormatPropertiesKHR".as_ptr()) }
        .ok_or("Vulkan Video formats entry point unavailable")?;
    // SAFETY: Vulkan defines this symbol with exactly PFN_vkGetPhysicalDeviceVideoFormatPropertiesKHR.
    let formats_fn: vk::PFN_vkGetPhysicalDeviceVideoFormatPropertiesKHR = unsafe { std::mem::transmute(formats_fn) };
    let mut encode_caps = vk::VideoEncodeCapabilitiesKHR::default();
    let mut h264_caps = vk::VideoEncodeH264CapabilitiesKHR::default();
    let mut h265_caps = vk::VideoEncodeH265CapabilitiesKHR::default();
    let mut caps = vk::VideoCapabilitiesKHR::default().push_next(&mut encode_caps);
    caps = if codec == Format::H264 { caps.push_next(&mut h264_caps) } else { caps.push_next(&mut h265_caps) };
    // SAFETY: all profile and output pNext structures live until the synchronous call returns.
    let result = unsafe { capabilities_fn(physical, &profile, &mut caps) };
    if result != vk::Result::SUCCESS {
        return Err(format!("Vulkan Video requested codec profile unsupported: {result:?}"));
    }
    let profiles = [profile];
    let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(&profiles);
    let nv12_usage = VIDEO_NV12_USAGES;
    let info = vk::PhysicalDeviceVideoFormatInfoKHR::default().image_usage(nv12_usage).push_next(&mut profile_list);
    let mut format_count = 0;
    // SAFETY: valid profile chain; null output requests only the count.
    let result = unsafe { formats_fn(physical, &info, &mut format_count, std::ptr::null_mut()) };
    if result != vk::Result::SUCCESS || format_count > 4096 {
        return Err(format!("Vulkan Video format enumeration failed: {result:?}, count {format_count}"));
    }
    let mut formats = vec![vk::VideoFormatPropertiesKHR::default(); format_count as usize];
    // SAFETY: output allocation contains exactly format_count initialized records.
    let result = unsafe { formats_fn(physical, &info, &mut format_count, formats.as_mut_ptr()) };
    if result != vk::Result::SUCCESS {
        return Err(format!("Vulkan Video format enumeration failed: {result:?}"));
    }
    let input_format = formats.iter().take(format_count as usize).any(|f| {
        f.format == vk::Format::G8_B8R8_2PLANE_420_UNORM
            && f.image_type == vk::ImageType::TYPE_2D
            && f.image_tiling == vk::ImageTiling::OPTIMAL
            && f.image_usage_flags.contains(nv12_usage)
    });
    let rgba_usage = VIDEO_RGBA_USAGES;
    // SAFETY: physical belongs to the live instance; this query creates no image or allocation.
    let rgba_support = unsafe {
        instance.get_physical_device_image_format_properties(
            physical,
            vk::Format::R16G16B16A16_SFLOAT,
            vk::ImageType::TYPE_2D,
            vk::ImageTiling::OPTIMAL,
            rgba_usage,
            vk::ImageCreateFlags::empty(),
        )
    };
    validate_vulkan_video_probe(
        codec,
        VulkanVideoProbe {
            api_version,
            synchronization2_extension,
            drm_node: Some(drm_node),
            missing_extension: None,
            encode_queue,
            codec_profile: true,
            input_format,
            required_usages: input_format && rgba_support.is_ok() && video_image_usages_supported(nv12_usage, rgba_usage),
        },
    )
}

/// Query the exact adapter/device's single-plane DMA-BUF import parameters before HAL import.
fn query_plane_import(
    adapter: &Adapter,
    device: &Device,
    format: ash::vk::Format,
    modifier: u64,
    width: u32,
    height: u32,
    usage: ash::vk::ImageUsageFlags,
) -> Result<(), String> {
    use ash::vk;
    if adapter.get_info().vendor != 0x1002 || !device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF) {
        return Err("resident VA interop requires AMD and enabled VULKAN_EXTERNAL_MEMORY_DMA_BUF".into());
    }
    // SAFETY: guards retain the live Vulkan objects for all metadata queries below.
    let hal_adapter = unsafe { adapter.as_hal::<Vulkan>() }.ok_or("interop adapter is not Vulkan")?;
    // SAFETY: borrowed guard retains this device; no raw object is changed.
    let hal_device = unsafe { device.as_hal::<Vulkan>() }.ok_or("interop device is not Vulkan")?;
    if hal_adapter.raw_physical_device() != hal_device.raw_physical_device() {
        return Err("interop device does not belong to the renderer adapter".into());
    }
    let instance = hal_adapter.shared_instance().raw_instance();
    let physical = hal_adapter.raw_physical_device();
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut properties = vk::FormatProperties2::default().push_next(&mut list);
    // SAFETY: live physical device; valid writable output chain first queries the count.
    unsafe { instance.get_physical_device_format_properties2(physical, format, &mut properties) };
    if list.drm_format_modifier_count == 0 || list.drm_format_modifier_count > 4096 {
        return Err("Vulkan modifier list unavailable or exceeds discovery limit".into());
    }
    let mut modifiers = vec![vk::DrmFormatModifierPropertiesEXT::default(); list.drm_format_modifier_count as usize];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut modifiers);
    let mut properties = vk::FormatProperties2::default().push_next(&mut list);
    // SAFETY: output contains the queried number of live records and an exact output chain.
    unsafe { instance.get_physical_device_format_properties2(physical, format, &mut properties) };
    let returned_count = (list.drm_format_modifier_count as usize).min(modifiers.len());
    let properties =
        modifiers.iter().take(returned_count).find(|m| m.drm_format_modifier == modifier).ok_or("Vulkan does not support exported DRM modifier")?;
    if properties.drm_format_modifier_plane_count != 1 {
        return Err("wgpu HAL cannot import a modifier with auxiliary memory planes".into());
    }
    let required = if usage.contains(vk::ImageUsageFlags::SAMPLED) { vk::FormatFeatureFlags::SAMPLED_IMAGE } else { vk::FormatFeatureFlags::TRANSFER_DST };
    if !properties.drm_format_modifier_tiling_features.contains(required) {
        return Err("Vulkan DRM modifier does not support required image usage".into());
    }
    let mut modifier_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default().drm_format_modifier(modifier).sharing_mode(vk::SharingMode::EXCLUSIVE);
    let mut external_info = vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(usage)
        .flags(vk::ImageCreateFlags::empty())
        .push_next(&mut modifier_info)
        .push_next(&mut external_info);
    let mut external = vk::ExternalImageFormatProperties::default();
    let mut output = vk::ImageFormatProperties2::default().push_next(&mut external);
    // SAFETY: input/output chains remain live, exact query creates no allocations.
    unsafe { instance.get_physical_device_image_format_properties2(physical, &info, &mut output) }
        .map_err(|e| format!("Vulkan DMA-BUF format/modifier import unsupported: {e:?}"))?;
    let limits = output.image_format_properties;
    if !external.external_memory_properties.external_memory_features.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
        || !external.external_memory_properties.compatible_handle_types.contains(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        || width > limits.max_extent.width
        || height > limits.max_extent.height
        || !limits.sample_counts.contains(vk::SampleCountFlags::TYPE_1)
    {
        return Err("Vulkan DMA-BUF import feature or extent unsupported".into());
    }
    Ok(())
}

/// Import one single-plane DMA-BUF as a wgpu texture.
///
/// Call only with a device created with `VULKAN_EXTERNAL_MEMORY_DMA_BUF` and layout metadata
/// matching the exported object. HAL consumes/closes `fd` on both success and failure.
fn import_dmabuf_texture(device: &Device, fd: OwnedFd, desc: &TextureDescriptor<'_>, modifier: u64, stride: u64, offset: u64) -> Result<Texture, String> {
    // SAFETY: guard borrows the live device and retains it while the HAL texture is imported.
    let hal_device = unsafe { device.as_hal::<Vulkan>() }.ok_or_else(|| "wgpu device is not using Vulkan".to_string())?;
    let hal_desc = wgpu_hal::TextureDescriptor {
        label: desc.label,
        size: desc.size,
        mip_level_count: desc.mip_level_count,
        sample_count: desc.sample_count,
        dimension: desc.dimension,
        format: desc.format,
        usage: wgpu::TextureUses::COPY_DST,
        memory_flags: wgpu_hal::MemoryFlags::empty(),
        view_formats: desc.view_formats.to_vec(),
    };
    // SAFETY: caller provides the matching one-plane DMA-BUF layout and enabled device feature;
    // HAL owns and closes fd, and returned texture is wrapped against this same device/descriptor.
    let hal_texture = unsafe { hal_device.texture_from_dmabuf_fd(fd, &hal_desc, modifier, stride, offset) }
        .map_err(|error| format!("Vulkan DMA-BUF import failed: {error:?}"))?;
    // SAFETY: HAL texture belongs to this device, matches desc, and starts in UNINITIALIZED state.
    Ok(unsafe { device.create_texture_from_hal::<Vulkan>(hal_texture, desc, wgpu::TextureUses::UNINITIALIZED) })
}

#[cfg(test)]
mod interop_tests {
    use super::*;
    fn quarantine_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn review_fix_submit_unwind_retains_unknown_submission_until_exact_completion() {
        let _serial = quarantine_test_lock();
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let released = Arc::new(AtomicUsize::new(0));
        let poisoned = Arc::new(AtomicUsize::new(0));
        let count = Arc::new(AtomicUsize::new(0));
        let token = Arc::new(Mutex::new(None::<u64>));
        let read_token = token.clone();
        let consumer_calls = Arc::new(AtomicUsize::new(0));
        let calls = consumer_calls.clone();
        let transfer = QuarantinedTransfer {
            producer_ready: false,
            producer: Box::new(move |_| match *read_token.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
                Some(7) => Ok(()),
                _ => Err("exact submission identity/completion is unproven".into()),
            }),
            consumer: Box::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            resources: Box::new(Resource(released.clone())),
            _permit: TransferPermit::acquire(count.clone()).expect("permit"),
        };
        let poison = poisoned.clone();
        let submitted = std::cell::Cell::new(false);
        let result = guarded_submission(
            transfer,
            Box::new(move || {
                poison.fetch_add(1, Ordering::SeqCst);
            }),
            || {
                submitted.set(true); // Simulates a driver submitting work before wgpu unwinds.
                panic!("injected submit panic before SubmissionIndex is returned");
            },
        );
        assert!(result.is_err());
        assert!(submitted.get());
        assert_eq!(poisoned.load(Ordering::SeqCst), 1);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(retry_quarantined_transfers(Duration::ZERO).is_err());
        assert_eq!(consumer_calls.load(Ordering::SeqCst), 0);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        // Only this instrumented boundary can supply its exact late completion; production
        // cannot invent an index after submit unwinds and must retain that quarantine entry.
        *token.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(7);
        retry_quarantined_transfers(Duration::ZERO).expect("exact instrumented completion");
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn external_sync_requires_foreign_extension() {
        use ExternalImageAccess::{FullOverwrite, PreserveContents};
        assert!(external_sync_gate(false, true, FullOverwrite).expect_err("missing extension").contains("VK_EXT_queue_family_foreign"));
        assert!(external_sync_gate(true, true, PreserveContents).expect_err("decoder must preserve pixels").contains("preserve-content"));
        assert!(external_sync_gate(true, false, FullOverwrite).expect_err("nonlinear output").contains("modifier 0"));
        external_sync_gate(true, true, FullOverwrite).expect("validated linear output is fully overwritten");
    }

    #[test]
    fn external_memory_protocol_accepts_only_linear_modifier_zero() {
        let objects = [DmaBufObjectLayout { size: 4096, modifier: ffi::DRM_FORMAT_MOD_LINEAR }];
        require_linear_nv12_layout(&objects, ffi::DRM_FORMAT_MOD_LINEAR, ffi::DRM_FORMAT_MOD_LINEAR).expect("linear NV12");
        let nonlinear = [DmaBufObjectLayout { size: 4096, modifier: 1 }];
        assert!(require_linear_nv12_layout(&nonlinear, ffi::DRM_FORMAT_MOD_LINEAR, ffi::DRM_FORMAT_MOD_LINEAR).is_err());
    }

    #[test]
    fn invalid_nv12_object_plane_ranges_reject_before_import() {
        let node = DrmNodeId { major: 226, minor: 128 };
        let objects = vec![DmaBufObjectLayout { size: 4096, modifier: 0 }, DmaBufObjectLayout { size: 2048, modifier: 0 }];
        let planes = vec![
            DmaBufPlaneLayout { object_index: 0, format: DRM_FORMAT_R8, offset: 0, pitch: 64, width: 64, height: 32 },
            DmaBufPlaneLayout { object_index: 1, format: DRM_FORMAT_GR88, offset: 0, pitch: 64, width: 32, height: 16 },
        ];
        assert!(
            DmaBufLayout::validate(node, node, 64, 32, DRM_FORMAT_NV12, objects.clone(), planes.clone(), &[(DRM_FORMAT_R8, 0), (DRM_FORMAT_GR88, 0)]).is_ok()
        );
        let mut bad_planes = planes;
        bad_planes[1].offset = 2040;
        assert!(DmaBufLayout::validate(node, node, 64, 32, DRM_FORMAT_NV12, objects, bad_planes, &[(DRM_FORMAT_R8, 0), (DRM_FORMAT_GR88, 0)]).is_err());
    }

    #[test]
    fn linear_foreign_barriers_acquire_discard_and_release_general() {
        use ash::vk;
        let image = vk::Image::null();
        let acquire = foreign_image_barrier(image, vk::QUEUE_FAMILY_FOREIGN_EXT, 3, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, true);
        let release = foreign_image_barrier(image, 3, vk::QUEUE_FAMILY_FOREIGN_EXT, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::GENERAL, false);
        assert_eq!((acquire.src_queue_family_index, acquire.dst_queue_family_index), (vk::QUEUE_FAMILY_FOREIGN_EXT, 3));
        assert_eq!((release.src_queue_family_index, release.dst_queue_family_index), (3, vk::QUEUE_FAMILY_FOREIGN_EXT));
        assert_eq!((acquire.old_layout, acquire.new_layout), (vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL));
        assert_eq!((release.old_layout, release.new_layout), (vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::GENERAL));
    }

    #[test]
    fn producer_buffer_writes_are_visible_to_raw_transfer_read() {
        use ash::vk;
        let barrier = source_buffer_barrier(vk::Buffer::null(), 4096);
        assert_eq!(barrier.src_access_mask, vk::AccessFlags::MEMORY_WRITE);
        assert_eq!(barrier.dst_access_mask, vk::AccessFlags::TRANSFER_READ);
        assert_eq!((barrier.src_queue_family_index, barrier.dst_queue_family_index), (vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED));
        assert_eq!((barrier.offset, barrier.size), (0, 4096));
    }

    #[test]
    fn raw_transfer_read_completes_before_future_same_queue_buffer_access() {
        use ash::vk;
        let barrier = source_buffer_return_barrier(vk::Buffer::null(), 4096);
        assert_eq!(barrier.src_access_mask, vk::AccessFlags::TRANSFER_READ);
        assert_eq!(barrier.dst_access_mask, vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE);
        assert_eq!((barrier.src_queue_family_index, barrier.dst_queue_family_index), (vk::QUEUE_FAMILY_IGNORED, vk::QUEUE_FAMILY_IGNORED));
        assert_eq!((barrier.offset, barrier.size), (0, 4096));
    }

    #[test]
    fn cancellation_or_fatal_poll_quarantines_until_exact_producer_and_consumer_retire() {
        let _serial = quarantine_test_lock();
        use std::sync::atomic::AtomicBool;
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let poisoned = Arc::new(AtomicUsize::new(0));
        let ready = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let p = ready.clone();
        let c = finished.clone();
        let poison = poisoned.clone();
        let pending = QuarantinedTransfer {
            producer_ready: false,
            producer: Box::new(move |_| if p.load(Ordering::SeqCst) { Ok(()) } else { panic!("injected Device::poll fatal device loss") }),
            consumer: Box::new(move |_| if c.load(Ordering::SeqCst) { Ok(()) } else { Err("VA incomplete".into()) }),
            resources: Box::new(Resource(released.clone())),
            _permit: TransferPermit::acquire(count.clone()).expect("transfer"),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = PendingTransfer {
                transfer: Some(pending),
                on_quarantine: Some(Box::new(move || {
                    poison.fetch_add(1, Ordering::SeqCst);
                })),
            };
            panic!("injected worker cancellation");
        }));
        assert!(result.is_err());
        assert_eq!(poisoned.load(Ordering::SeqCst), 1);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(retry_quarantined_transfers(Duration::ZERO).is_err());
        ready.store(true, Ordering::SeqCst);
        assert!(retry_quarantined_transfers(Duration::ZERO).is_err());
        assert_eq!(released.load(Ordering::SeqCst), 0);
        finished.store(true, Ordering::SeqCst);
        retry_quarantined_transfers(Duration::ZERO).expect("both exact completions");
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn quarantine_requires_both_milestones_and_keeps_three_slot_reservation() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        struct Resource(Arc<AtomicUsize>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let permit = TransferPermit::acquire(count.clone()).expect("first transfer");
        let second = TransferPermit::acquire(count.clone()).expect("second transfer");
        let third = TransferPermit::acquire(count.clone()).expect("third transfer");
        assert!(TransferPermit::acquire(count.clone()).is_err());
        let ready = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let consumer_calls = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let p = ready.clone();
        let c = finished.clone();
        let calls = consumer_calls.clone();
        let mut pending = QuarantinedTransfer {
            producer_ready: false,
            producer: Box::new(move |_| if p.load(Ordering::SeqCst) { Ok(()) } else { Err("GPU timeout".into()) }),
            consumer: Box::new(move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                if c.load(Ordering::SeqCst) { Ok(()) } else { Err("VA timeout".into()) }
            }),
            resources: Box::new(Resource(released.clone())),
            _permit: permit,
        };
        assert!(pending.wait_transfer_retired(Duration::ZERO).is_err());
        assert_eq!(consumer_calls.load(Ordering::SeqCst), 0, "consumer never runs before producer_ready");
        ready.store(true, Ordering::SeqCst);
        assert!(pending.wait_transfer_retired(Duration::ZERO).is_err());
        assert_eq!(released.load(Ordering::SeqCst), 0, "producer_ready is not retirement");
        assert_eq!(count.load(Ordering::SeqCst), 3);
        finished.store(true, Ordering::SeqCst);
        pending.wait_transfer_retired(Duration::ZERO).expect("exact GPU and VA completion");
        drop(pending);
        assert_eq!(released.load(Ordering::SeqCst), 1);
        drop(second);
        drop(third);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    fn layout() -> (Vec<DmaBufObjectLayout>, Vec<DmaBufPlaneLayout>) {
        (
            vec![DmaBufObjectLayout { size: 16384, modifier: 0 }, DmaBufObjectLayout { size: 8192, modifier: 7 }],
            vec![
                DmaBufPlaneLayout { object_index: 0, format: DRM_FORMAT_R8, offset: 4096, pitch: 256, width: 64, height: 32 },
                DmaBufPlaneLayout { object_index: 1, format: DRM_FORMAT_GR88, offset: 1024, pitch: 256, width: 32, height: 16 },
            ],
        )
    }

    #[test]
    fn nv12_layout_validates_objects_planes_and_capability_intersection() {
        let (objects, planes) = layout();
        let node = DrmNodeId { major: 226, minor: 128 };
        let supported = [(DRM_FORMAT_R8, 0), (DRM_FORMAT_GR88, 7)];
        let valid =
            DmaBufLayout::validate(node, node, 64, 32, DRM_FORMAT_NV12, objects.clone(), planes.clone(), &supported).expect("valid padded multi-object layout");
        assert_eq!(valid.planes()[1].object_index, 1);
        assert_eq!(valid.objects()[1].modifier, 7);
        assert!(
            DmaBufLayout::validate(node, DrmNodeId { major: 226, minor: 129 }, 64, 32, DRM_FORMAT_NV12, objects.clone(), planes.clone(), &supported).is_err()
        );
        assert!(DmaBufLayout::validate(node, node, 64, 32, DRM_FORMAT_NV12, objects.clone(), planes.clone(), &supported[..1]).is_err());
        let mut bad = planes.clone();
        bad[1].object_index = 2;
        assert!(DmaBufLayout::validate(node, node, 64, 32, DRM_FORMAT_NV12, objects.clone(), bad, &supported).is_err());
        let mut bad = objects.clone();
        bad[0].size = 5000;
        assert!(DmaBufLayout::validate(node, node, 64, 32, DRM_FORMAT_NV12, bad, planes.clone(), &supported).is_err());
        assert!(DmaBufLayout::validate(node, node, 63, 32, DRM_FORMAT_NV12, objects.clone(), planes.clone(), &supported).is_err());
        let mut bad = planes.clone();
        bad[0].offset = u64::MAX;
        assert!(DmaBufLayout::validate(node, node, 64, 32, DRM_FORMAT_NV12, objects, bad, &supported).is_err());
    }

    #[test]
    fn nv12_plane_rejects_zero_width_and_misaligned_chroma() {
        assert!(validate_plane(DRM_FORMAT_R8, DRM_FORMAT_R8, 256, 0, 16, 0, 4096).is_err());
        assert!(validate_plane(DRM_FORMAT_GR88, DRM_FORMAT_GR88, 256, 64, 16, 1, 8192).is_err());
        assert!(validate_plane(DRM_FORMAT_GR88, DRM_FORMAT_GR88, 257, 64, 16, 0, 8192).is_err());
    }

    #[test]
    fn nv12_plane_accepts_padding_and_noncontiguous_offsets() {
        assert!(validate_plane(DRM_FORMAT_R8, DRM_FORMAT_R8, 256, 63, 17, 4096, 16384).is_ok());
        assert!(validate_plane(DRM_FORMAT_GR88, DRM_FORMAT_GR88, 256, 64, 9, 12288, 16384).is_ok());
        assert!(validate_plane(DRM_FORMAT_GR88, DRM_FORMAT_GR88, 256, 64, 9, 12288, 14000).is_err());
    }
}
