//! Diagnostic proof that a VA-API LINEAR NV12 allocation can be imported as a Vulkan buffer.
//!
//! This probe submits no work and does not establish Vulkan/VA ownership synchronization.
#![cfg(target_os = "linux")]

use std::{
    os::fd::{AsRawFd, BorrowedFd},
    sync::Arc,
    time::Duration,
};

use ash::vk;
use wgpu::{Adapter, Device, Queue};
use wgpu_hal::api::Vulkan;

use crate::{
    vaapi::{
        ffi,
        va::{ExportedDmaBuf, ResidentSurfaceLease},
    },
    vulkan_import::render_node_id,
};

const MAX_OBJECT_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_VA_DECODE_SURFACES: u64 = 18;
fn buffer_usage() -> vk::BufferUsageFlags {
    vk::BufferUsageFlags::TRANSFER_SRC
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VulkanBufferImportProbe {
    pub drm_node: crate::vaapi::DrmNodeId,
    pub object_size: u64,
    pub modifier: u64,
    pub plane_offsets: [u32; 2],
    pub plane_pitches: [u32; 2],
    pub memory_type_index: u32,
    pub memory_type_bits: u32,
    pub allocation_size: u64,
}

fn validate_export(surface: &ExportedDmaBuf<'_>, headroom_bytes: u64) -> Result<VulkanBufferImportProbe, String> {
    if surface.fourcc() != ffi::VA_FOURCC_NV12
        || surface.layer_count() != 2
        || surface.plane_count(0) != Some(1)
        || surface.plane_count(1) != Some(1)
        || surface.object_count() != 1
    {
        return Err("Vulkan buffer probe requires one-object, two-plane NV12 export".into());
    }
    let (width, height) = surface.size();
    if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err("Vulkan buffer probe requires nonzero even NV12 dimensions".into());
    }
    let (object_size, modifier) = surface.object_layout(0).ok_or("VA export object metadata missing")?;
    if object_size == 0 || object_size > MAX_OBJECT_BYTES || object_size > headroom_bytes {
        return Err("Vulkan buffer probe object exceeds bounded import budget".into());
    }
    if modifier != ffi::DRM_FORMAT_MOD_LINEAR {
        return Err("Vulkan buffer probe requires LINEAR modifier".into());
    }
    let y = surface.plane(0, 0).ok_or("VA luma plane metadata missing")?;
    let uv = surface.plane(1, 0).ok_or("VA chroma plane metadata missing")?;
    if y.object_index != 0 || uv.object_index != 0 || y.modifier != modifier || uv.modifier != modifier {
        return Err("Vulkan buffer probe planes must reference the same LINEAR object".into());
    }
    if y.layer_format != u32::from_le_bytes(*b"R8  ") || uv.layer_format != u32::from_le_bytes(*b"GR88") {
        return Err("Vulkan buffer probe requires R8 and GR88 NV12 planes".into());
    }
    validate_nv12_ranges(object_size, width, height, y.offset, y.pitch, uv.offset, uv.pitch)?;
    Ok(VulkanBufferImportProbe {
        drm_node: surface.drm_node_id(),
        object_size,
        modifier,
        plane_offsets: [y.offset, uv.offset],
        plane_pitches: [y.pitch, uv.pitch],
        memory_type_index: u32::MAX,
        memory_type_bits: 0,
        allocation_size: 0,
    })
}

fn validate_nv12_ranges(object_size: u64, width: u32, height: u32, y_offset: u32, y_pitch: u32, uv_offset: u32, uv_pitch: u32) -> Result<(), String> {
    let row = u64::from(width);
    if y_pitch == 0 || uv_pitch == 0 || u64::from(y_pitch) < row || u64::from(uv_pitch) < row {
        return Err("Vulkan buffer probe plane pitch is invalid".into());
    }
    let end = |offset: u32, pitch: u32, rows: u32| -> Result<u64, String> {
        u64::from(offset)
            .checked_add(u64::from(pitch).checked_mul(u64::from(rows - 1)).ok_or("NV12 plane range overflow")?)
            .and_then(|value| value.checked_add(row))
            .ok_or_else(|| "NV12 plane range overflow".to_string())
    };
    let y_end = end(y_offset, y_pitch, height)?;
    let uv_end = end(uv_offset, uv_pitch, height / 2)?;
    if y_end > object_size || uv_end > object_size || (u64::from(y_offset) < uv_end && u64::from(uv_offset) < y_end) {
        return Err("Vulkan buffer probe NV12 plane range exceeds or overlaps object".into());
    }
    Ok(())
}

fn choose_memory_type(bits: u32, count: u32) -> Option<u32> {
    (0..count.min(32)).find(|index| bits & (1 << index) != 0)
}

struct ImportedBuffer {
    device: ash::Device,
    buffer: Option<vk::Buffer>,
    memory: Option<vk::DeviceMemory>,
}

impl ImportedBuffer {
    fn into_hal(mut self) -> Result<wgpu_hal::vulkan::Buffer, String> {
        let Some(buffer) = self.buffer.as_ref().copied() else { return Err("imported VA buffer owner was already consumed".into()) };
        let Some(memory) = self.memory.as_ref().copied() else { return Err("imported VA allocation owner was already consumed".into()) };
        let device = self.device.clone();
        let destroy = Box::new(move || {
            // SAFETY: this callback is the sole owner after transfer; buffer dies before bound memory.
            unsafe { device.destroy_buffer(buffer, None) };
            // SAFETY: imported allocation belongs to this device and is freed after buffer destruction.
            unsafe { device.free_memory(memory, None) };
        });
        self.buffer.take();
        self.memory.take();
        // SAFETY: `from_raw_externally_owned` takes sole destruction ownership of both handles.
        Ok(unsafe { wgpu_hal::vulkan::Buffer::from_raw_externally_owned(buffer, destroy) })
    }
}

impl Drop for ImportedBuffer {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            // SAFETY: this guard owns the handle; Vulkan requires buffer destruction before bound memory is freed.
            unsafe { self.device.destroy_buffer(buffer, None) };
        }
        if let Some(memory) = self.memory.take() {
            // SAFETY: allocation belongs to this device and its buffer was destroyed above.
            unsafe { self.device.free_memory(memory, None) };
        }
    }
}

fn query_external_buffer(instance: &ash::Instance, physical: vk::PhysicalDevice) -> Result<(), String> {
    let info = vk::PhysicalDeviceExternalBufferInfo::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT).usage(buffer_usage());
    let mut properties = vk::ExternalBufferProperties::default();
    // SAFETY: physical is retained by the HAL adapter; output is a valid initialized Vulkan structure.
    unsafe { instance.get_physical_device_external_buffer_properties(physical, &info, &mut properties) };
    let external = properties.external_memory_properties;
    if !external.external_memory_features.contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
        || !external.compatible_handle_types.contains(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
    {
        return Err("Vulkan DMA-BUF buffer usage is not importable".into());
    }
    Ok(())
}

fn import_object<T>(
    instance: &ash::Instance,
    device: &ash::Device,
    fd: BorrowedFd<'_>,
    size: u64,
    reserve: impl FnOnce(u64) -> Result<T, String>,
) -> Result<(ImportedBuffer, u32, u32, u64, T), String> {
    let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let info = vk::BufferCreateInfo::default().size(size).usage(buffer_usage()).sharing_mode(vk::SharingMode::EXCLUSIVE).push_next(&mut external);
    // SAFETY: device is live; buffer uses the exact queried usage and bounded nonzero size.
    let buffer = unsafe { device.create_buffer(&info, None) }.map_err(|error| format!("create external Vulkan buffer: {error:?}"))?;
    let mut owner = ImportedBuffer { device: device.clone(), buffer: Some(buffer), memory: None };
    // SAFETY: buffer belongs to this live device.
    let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    if requirements.size > size {
        return Err("Vulkan buffer memory requirement exceeds VA DMA-BUF object".into());
    }
    let memory_fd = ash::khr::external_memory_fd::Device::new(instance, device);
    let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
    // SAFETY: imported fd is an open DMA-BUF borrowed from the live VA export; query does not consume it.
    unsafe { memory_fd.get_memory_fd_properties(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT, fd.as_raw_fd(), &mut fd_properties) }
        .map_err(|error| format!("query DMA-BUF memory types: {error:?}"))?;
    let bits = requirements.memory_type_bits & fd_properties.memory_type_bits;
    let memory_type = choose_memory_type(bits, 32).ok_or("DMA-BUF has no compatible Vulkan memory type")?;
    let duplicate = fd.try_clone_to_owned().map_err(|error| format!("duplicate VA DMA-BUF fd: {error}"))?;
    let allocation_hold = reserve(requirements.size)?;
    let mut import = vk::ImportMemoryFdInfoKHR::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT).fd(duplicate.as_raw_fd());
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
    let allocation =
        vk::MemoryAllocateInfo::default().allocation_size(requirements.size).memory_type_index(memory_type).push_next(&mut import).push_next(&mut dedicated);
    // SAFETY: imported fd remains owned on failure; Vulkan consumes it only when allocation succeeds.
    let memory = match unsafe { device.allocate_memory(&allocation, None) } {
        Ok(memory) => {
            std::mem::forget(duplicate);
            owner.memory = Some(memory);
            memory
        }
        Err(error) => return Err(format!("import DMA-BUF memory: {error:?}")),
    };
    // SAFETY: allocation imported the exported object's fd using a compatible memory type; offset zero is required for dma-buf import.
    unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|error| format!("bind imported DMA-BUF memory: {error:?}"))?;
    Ok((owner, memory_type, bits, requirements.size, allocation_hold))
}

/// Import one bounded VA DMA-BUF object as an unused Vulkan buffer, then destroy it.
///
/// This is diagnostic only. It records no ownership barrier and submits no Vulkan work.
pub fn probe_va_nv12_buffer_import(adapter: &Adapter, device: &Device, surface: &ExportedDmaBuf<'_>) -> Result<VulkanBufferImportProbe, String> {
    let headroom_kib = filmcraft_export::runtime_ram_headroom_kib().ok_or("runtime RAM headroom unavailable")?;
    let headroom_bytes = headroom_kib.checked_mul(1024).ok_or("runtime RAM headroom overflow")?;
    let mut report = validate_export(surface, headroom_bytes)?;
    if adapter.get_info().vendor != 0x1002 || adapter.get_info().backend != wgpu::Backend::Vulkan {
        return Err("Vulkan DMA-BUF buffer probe requires AMD Vulkan".into());
    }
    if render_node_id(adapter)? != report.drm_node {
        return Err("VA and Vulkan render nodes differ".into());
    }
    // SAFETY: guards retain the exact selected adapter and device while raw Vulkan handles are queried.
    let hal_adapter = unsafe { adapter.as_hal::<Vulkan>() }.ok_or("probe adapter is not Vulkan")?;
    // SAFETY: guard retains the exact wgpu device used for the import.
    let hal_device = unsafe { device.as_hal::<Vulkan>() }.ok_or("probe device is not Vulkan")?;
    if hal_adapter.raw_physical_device() != hal_device.raw_physical_device()
        || !device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
        || !hal_device.enabled_device_extensions().contains(&ash::ext::queue_family_foreign::NAME)
        || !hal_device.enabled_device_extensions().contains(&ash::khr::external_memory_fd::NAME)
        || !hal_device.enabled_device_extensions().contains(&ash::ext::external_memory_dma_buf::NAME)
    {
        return Err("probe requires same-device AMD DMA-BUF and FOREIGN capabilities".into());
    }
    let instance = hal_adapter.shared_instance().raw_instance();
    let physical = hal_adapter.raw_physical_device();
    let raw_device = hal_device.raw_device();
    let mut reservation = filmcraft_export::ResidentReservation::try_acquire(Some(headroom_bytes), None, 0, report.object_size)
        .map_err(|error| format!("reserve DMA-BUF probe allocation: {error}"))?;
    query_external_buffer(instance, physical)?;
    let plane = surface.plane(0, 0).ok_or("VA DMA-BUF fd missing")?;
    let (imported, memory_type, bits, allocation_size, allocation) = import_object(instance, raw_device, plane.fd, report.object_size, |bytes| {
        reservation.take(bytes).map_err(|error| format!("reserve DMA-BUF probe memory: {error}"))
    })?;
    report.memory_type_index = memory_type;
    report.memory_type_bits = bits;
    report.allocation_size = allocation_size;
    drop(imported);
    drop(allocation);
    drop(reservation);
    drop(hal_device);
    drop(hal_adapter);
    Ok(report)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Nv12BufferLayout {
    pub width: u32,
    pub height: u32,
    pub object_size: u64,
    pub y_offset: u32,
    pub uv_offset: u32,
    pub y_pitch: u32,
    pub uv_pitch: u32,
}

/// GPU-owned copy of a VA LINEAR NV12 object. Keep this owner alive through its last GPU consumer;
/// its allocation hold accounts for the buffer until that owner drops.
pub struct ResidentNv12Buffer {
    buffer: wgpu::Buffer,
    drm_node: crate::vaapi::DrmNodeId,
    layout: Nv12BufferLayout,
    _allocation: Arc<filmcraft_export::ResidentAllocation>,
}

impl ResidentNv12Buffer {
    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub fn drm_node_id(&self) -> crate::vaapi::DrmNodeId {
        self.drm_node
    }

    pub fn layout(&self) -> Nv12BufferLayout {
        self.layout
    }
}

fn validate_linear_nv12_layout(
    object_count: Option<usize>,
    object_size: u64,
    modifier: u64,
    width: u32,
    height: u32,
    y_offset: u32,
    y_pitch: u32,
    uv_offset: u32,
    uv_pitch: u32,
) -> Result<Nv12BufferLayout, String> {
    if object_count != Some(1)
        || object_size == 0
        || object_size > MAX_OBJECT_BYTES
        || !object_size.is_multiple_of(4)
        || modifier != ffi::DRM_FORMAT_MOD_LINEAR
        || width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
    {
        return Err("resident VA import requires one bounded nonzero even LINEAR NV12 object".into());
    }
    validate_nv12_ranges(object_size, width, height, y_offset, y_pitch, uv_offset, uv_pitch)?;
    Ok(Nv12BufferLayout { width, height, object_size, y_offset, uv_offset, y_pitch, uv_pitch })
}

/// Conservative pre-allocation object bound. A driver requiring larger pitch/alignment fails
/// closed at export before Vulkan import; row pitch allowance is 4096 bytes, plus 64 KiB tail.
pub(crate) fn linear_nv12_object_size_bound(width: u32, height: u32, coded_alignment: u32) -> Result<u64, String> {
    if width == 0 || height == 0 || coded_alignment == 0 || !coded_alignment.is_power_of_two() {
        return Err("resident NV12 bound requires nonzero dimensions and power-of-two alignment".into());
    }
    let align = |value: u32, to: u32| value.checked_add(to - 1).map(|value| value & !(to - 1)).ok_or_else(|| "resident NV12 size bound overflow".to_string());
    let coded_width = align(width, coded_alignment)?;
    let coded_height = align(height, coded_alignment)?;
    let pitch = u64::from(align(coded_width, 4096)?);
    let luma = pitch.checked_mul(u64::from(coded_height)).ok_or("resident NV12 size bound overflow")?;
    let chroma = pitch.checked_mul(u64::from(coded_height / 2)).ok_or("resident NV12 size bound overflow")?;
    let bytes = luma.checked_add(chroma).and_then(|bytes| bytes.checked_add(64 * 1024)).ok_or("resident NV12 size bound overflow")?;
    if bytes > MAX_OBJECT_BYTES {
        return Err("resident NV12 surface exceeds the 256 MiB import bound".into());
    }
    Ok(bytes)
}

struct ResidentNv12Budget {
    allocation: Arc<filmcraft_export::ResidentAllocation>,
    bytes: u64,
    taken: bool,
}

impl filmcraft_gpu::nv12_repack::Nv12AllocationBudget for ResidentNv12Budget {
    type Hold = Arc<filmcraft_export::ResidentAllocation>;

    fn take(&mut self, bytes: u64) -> Result<Self::Hold, String> {
        if self.taken || bytes != self.bytes {
            return Err("resident NV12 repack allocation differs from its aggregate reservation".into());
        }
        self.taken = true;
        Ok(self.allocation.clone())
    }
}

/// Imported and GPU-repacked decoded NV12 frame. Retain through the final consumer submission.
pub struct ResidentNv12GpuFrame {
    pub index: usize,
    pub pts: i64,
    pub crop: (u32, u32, u32, u32),
    pub color: filmcraft_color::ColorInfo,
    pub color_metadata: crate::vaapi::ResidentColorMetadata,
    pub par: (u32, u32),
    resources: Option<ResidentNv12GpuResources>,
    device: Device,
    queue: Queue,
    identity: Arc<()>,
    submission_generation: u64,
    repack_submission: wgpu::SubmissionIndex,
    retired: bool,
}

#[derive(Clone)]
pub struct ResidentNv12Completion {
    identity: Arc<()>,
    generation: u64,
    submission: wgpu::SubmissionIndex,
}

impl ResidentNv12Completion {
    pub fn submission_index(&self) -> wgpu::SubmissionIndex {
        self.submission.clone()
    }
}

struct ResidentNv12GpuResources {
    repacked: filmcraft_gpu::nv12_repack::RepackedNv12<Arc<filmcraft_export::ResidentAllocation>>,
    imported: ResidentNv12Buffer,
}

impl ResidentNv12GpuFrame {
    pub fn repack_submission(&self) -> wgpu::SubmissionIndex {
        self.repack_submission.clone()
    }

    pub fn submit_consumer_commands(&mut self, commands: impl IntoIterator<Item = wgpu::CommandBuffer>) -> Result<ResidentNv12Completion, String> {
        if self.retired || self.resources.is_none() {
            return Err("resident NV12 frame is already retired".into());
        }
        let generation = self.submission_generation.checked_add(1).ok_or("resident NV12 submission generation overflow")?;
        self.submission_generation = generation;
        let submission = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.queue.submit(commands)))
            .map_err(|_| "resident NV12 consumer submission panicked; GPU ownership remains unproven".to_string())?;
        Ok(ResidentNv12Completion { identity: self.identity.clone(), generation, submission })
    }

    /// Bind an exact later submission from this same wgpu queue to this frame. Used when the
    /// resident renderer owns command recording and returns its queue submission index.
    pub(crate) fn completion_for_queue_submission(&mut self, queue: &Queue, submission: wgpu::SubmissionIndex) -> Result<ResidentNv12Completion, String> {
        if self.retired || self.resources.is_none() {
            return Err("resident NV12 frame is already retired".into());
        }
        // SAFETY: retained device and caller queue guards keep these Vulkan handles live for identity checks.
        let hal_device = unsafe { self.device.as_hal::<Vulkan>() }.ok_or("resident NV12 completion device is not Vulkan")?;
        // SAFETY: the completion queue guard keeps the exact submitted queue live.
        let frame_queue = unsafe { self.queue.as_hal::<Vulkan>() }.ok_or("resident NV12 frame queue is not Vulkan")?;
        // SAFETY: caller queue guard keeps the producer queue live.
        let submitted_queue = unsafe { queue.as_hal::<Vulkan>() }.ok_or("resident NV12 completion queue is not Vulkan")?;
        if frame_queue.as_raw() != submitted_queue.as_raw() || submitted_queue.as_raw() != hal_device.raw_queue() {
            return Err("resident NV12 completion came from another queue".into());
        }
        let generation = self.submission_generation.checked_add(1).ok_or("resident NV12 submission generation overflow")?;
        self.submission_generation = generation;
        Ok(ResidentNv12Completion { identity: self.identity.clone(), generation, submission })
    }

    pub(crate) fn retire_after_queue_submission(&mut self, queue: &Queue, submission: wgpu::SubmissionIndex, timeout: Duration) -> Result<(), String> {
        let completion = self.completion_for_queue_submission(queue, submission)?;
        self.retire_after_gpu_submission(completion, timeout)
    }

    pub fn drm_node_id(&self) -> Option<crate::vaapi::DrmNodeId> {
        self.resources.as_ref().map(|resources| resources.imported.drm_node_id())
    }

    pub fn repacked(&self) -> Option<&filmcraft_gpu::nv12_repack::RepackedNv12<Arc<filmcraft_export::ResidentAllocation>>> {
        self.resources.as_ref().map(|resources| &resources.repacked)
    }

    /// Retire only after a completion from this frame's exact queue and latest consumer submit.
    pub fn retire_after_gpu_submission(&mut self, completion: ResidentNv12Completion, timeout: Duration) -> Result<(), String> {
        if self.retired {
            return Err("resident NV12 frame is already retired".into());
        }
        if !Arc::ptr_eq(&self.identity, &completion.identity) || completion.generation != self.submission_generation {
            return Err("resident NV12 completion does not cover the latest consumer submission".into());
        }
        if timeout.is_zero() {
            return Err("resident NV12 retirement timeout must be positive".into());
        }
        crate::vulkan_import::wait_for_submission(&self.device, completion.submission, timeout)?;
        self.retired = true;
        drop(self.resources.take());
        Ok(())
    }
}

impl Drop for ResidentNv12GpuFrame {
    fn drop(&mut self) {
        if !self.retired
            && let Some(resources) = self.resources.take()
        {
            // Keep uncertain consumer resources and their one-use reservation live; three slots cap quarantine.
            std::mem::forget(resources);
        }
    }
}

struct ResidentSurfaceImportGuard {
    source: Option<ResidentSurfaceLease>,
    device: Device,
    queue: Queue,
    timeout: Duration,
    gpu_submission_attempted: bool,
    imported: Option<wgpu::Buffer>,
    copied: Option<wgpu::Buffer>,
    allocation: Option<Arc<filmcraft_export::ResidentAllocation>>,
    source_allocation: Option<filmcraft_export::ResidentAllocation>,
}

impl ResidentSurfaceImportGuard {
    fn new(
        source: ResidentSurfaceLease,
        device: &Device,
        queue: &Queue,
        timeout: Duration,
        source_allocation: Option<filmcraft_export::ResidentAllocation>,
    ) -> Self {
        Self {
            source: Some(source),
            device: device.clone(),
            queue: queue.clone(),
            timeout,
            gpu_submission_attempted: false,
            imported: None,
            copied: None,
            allocation: None,
            source_allocation,
        }
    }

    fn source(&self) -> Result<&ResidentSurfaceLease, String> {
        self.source.as_ref().ok_or_else(|| "resident VA surface lease was retired".into())
    }

    fn take_source(&mut self) -> Result<ResidentSurfaceLease, String> {
        self.source.take().ok_or_else(|| "resident VA surface lease was retired".into())
    }

    fn take_source_allocation(&mut self) -> Option<filmcraft_export::ResidentAllocation> {
        self.source_allocation.take()
    }

    fn retire_after_gpu_submission(&mut self, submission: wgpu::SubmissionIndex) -> Result<(), String> {
        self.source.as_mut().ok_or("resident VA surface lease was retired")?.retire_after_gpu_submission(&self.device, submission, self.timeout)?;
        self.source.take();
        Ok(())
    }
}

impl Drop for ResidentSurfaceImportGuard {
    fn drop(&mut self) {
        if self.gpu_submission_attempted && self.source.is_some() {
            // A FOREIGN operation may still be in flight. Leak only this frame's resources and
            // allocation hold; ResidentSurfaceLease::drop quarantines the VA surface and permit.
            if let Some(resource) = self.imported.take() {
                std::mem::forget(resource);
            }
            if let Some(resource) = self.copied.take() {
                std::mem::forget(resource);
            }
            if let Some(allocation) = self.allocation.take() {
                std::mem::forget(allocation);
            }
            if let Some(allocation) = self.source_allocation.take() {
                std::mem::forget(allocation);
            }
            return;
        }
        if !self.gpu_submission_attempted && self.source.is_some() {
            // No imported buffer was submitted. An empty exact-queue submit plus bounded VA sync
            // retires the lease; any failure leaves ResidentSurfaceLease's quarantine as fallback.
            let retired = matches!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let submission = self.queue.submit(std::iter::empty::<wgpu::CommandBuffer>());
                    let retired = self.source.as_mut().is_some_and(|source| source.retire_after_gpu_submission(&self.device, submission, self.timeout).is_ok());
                    if retired {
                        self.source.take();
                    }
                    retired
                })),
                Ok(true)
            );
            if !retired && let Some(allocation) = self.source_allocation.take() {
                // VA surface owner will be quarantined by its lease; retain its budget too.
                std::mem::forget(allocation);
            }
        }
    }
}

fn aggregate_reservation_bytes(imported_bytes: u64, copied_bytes: u64, repack_bytes: u64) -> Result<u64, String> {
    imported_bytes
        .checked_add(copied_bytes)
        .and_then(|bytes| bytes.checked_add(repack_bytes))
        .ok_or_else(|| "resident NV12 aggregate reservation overflow".into())
}

/// Import one VA LINEAR NV12 frame through a FOREIGN-owned Vulkan buffer, then copy GPU-to-GPU
/// into wgpu-owned storage. Keep the returned owner through its final queue consumer. The aggregate
/// import, copy and repack reservation is taken before `vkAllocateMemory`; remaining frame budget
/// stays available for encoded packet bytes.
pub fn import_va_nv12_surface(
    adapter: &Adapter,
    device: &Device,
    queue: &Queue,
    source: ResidentSurfaceLease,
    reservation: &mut filmcraft_export::ResidentReservation,
    timeout: Duration,
) -> Result<ResidentNv12Buffer, String> {
    import_va_nv12_surface_with_allocation(adapter, device, queue, source, reservation, timeout, 0, None)
}

fn import_va_nv12_surface_with_allocation(
    adapter: &Adapter,
    device: &Device,
    queue: &Queue,
    source: ResidentSurfaceLease,
    reservation: &mut filmcraft_export::ResidentReservation,
    timeout: Duration,
    repack_bytes: u64,
    source_allocation: Option<filmcraft_export::ResidentAllocation>,
) -> Result<ResidentNv12Buffer, String> {
    if timeout.is_zero() {
        return Err("VA NV12 buffer import timeout must be positive".into());
    }
    let mut guard = ResidentSurfaceImportGuard::new(source, device, queue, timeout, source_allocation);
    let source = guard.source()?;
    let (width, height) = source.size().ok_or("resident VA surface metadata expired")?;
    if source.fourcc() != Some(ffi::VA_FOURCC_NV12)
        || source.layer_count() != Some(2)
        || source.plane_count(0) != Some(1)
        || source.plane_count(1) != Some(1)
        || width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
    {
        return Err("resident VA import requires two-plane nonzero even NV12".into());
    }
    let drm_node = source.drm_node_id().ok_or("resident VA surface lost DRM identity")?;
    if adapter.get_info().vendor != 0x1002 || adapter.get_info().backend != wgpu::Backend::Vulkan || render_node_id(adapter)? != drm_node {
        return Err("resident VA surface and Vulkan adapter must be AMD on the same DRM node".into());
    }
    let (object_size, modifier) = source.object_layout(0).ok_or("resident VA DMA-BUF object metadata missing")?;
    if !(4..=MAX_OBJECT_BYTES).contains(&object_size) || !object_size.is_multiple_of(4) {
        return Err("resident VA NV12 object exceeds the bounded aligned import budget".into());
    }
    if guard.source_allocation.as_ref().is_some_and(|hold| object_size > hold.bytes() / MAX_VA_DECODE_SURFACES) {
        return Err("resident VA surface exceeds the pre-reserved decoder DPB bound".into());
    }
    let limits = device.limits();
    if object_size > limits.max_buffer_size || object_size > limits.max_storage_buffer_binding_size {
        return Err("resident VA NV12 object exceeds wgpu storage-buffer limits".into());
    }
    let y = source.plane(0, 0).ok_or("resident VA luma plane metadata missing")?;
    let uv = source.plane(1, 0).ok_or("resident VA chroma plane metadata missing")?;
    if y.object_index != 0
        || uv.object_index != 0
        || u64::from(y.object_size) != object_size
        || u64::from(uv.object_size) != object_size
        || y.modifier != modifier
        || uv.modifier != modifier
        || y.layer_format != u32::from_le_bytes(*b"R8  ")
        || uv.layer_format != u32::from_le_bytes(*b"GR88")
    {
        return Err("resident VA planes do not match one LINEAR R8/GR88 NV12 object".into());
    }
    let layout = validate_linear_nv12_layout(source.object_count(), object_size, modifier, width, height, y.offset, y.pitch, uv.offset, uv.pitch)?;
    filmcraft_export::check_runtime_ram_ceiling().map_err(|error| error.to_string())?;
    // SAFETY: guards retain the exact adapter/device/queue while raw Vulkan handles are inspected.
    let hal_adapter = unsafe { adapter.as_hal::<Vulkan>() }.ok_or("resident VA import adapter is not Vulkan")?;
    // SAFETY: guard retains the exact wgpu device used for import and queue submissions.
    let hal_device = unsafe { device.as_hal::<Vulkan>() }.ok_or("resident VA import device is not Vulkan")?;
    // SAFETY: guard retains the caller queue while verifying its device ownership.
    let hal_queue = unsafe { queue.as_hal::<Vulkan>() }.ok_or("resident VA import queue is not Vulkan")?;
    if hal_adapter.raw_physical_device() != hal_device.raw_physical_device()
        || hal_queue.as_raw() != hal_device.raw_queue()
        || !device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF)
        || !hal_device.enabled_device_extensions().contains(&ash::ext::queue_family_foreign::NAME)
        || !hal_device.enabled_device_extensions().contains(&ash::khr::external_memory_fd::NAME)
        || !hal_device.enabled_device_extensions().contains(&ash::ext::external_memory_dma_buf::NAME)
    {
        return Err("resident VA import requires one AMD Vulkan device, exact queue and FOREIGN/DMA-BUF extensions".into());
    }
    let instance = hal_adapter.shared_instance().raw_instance();
    let physical = hal_adapter.raw_physical_device();
    let raw_device = hal_device.raw_device();
    query_external_buffer(instance, physical)?;
    let (imported, _, _, _, allocation) = import_object(instance, raw_device, y.fd, object_size, |imported_bytes| {
        let total = aggregate_reservation_bytes(imported_bytes, object_size, repack_bytes)?;
        reservation.take(total).map(Arc::new).map_err(|error| format!("reserve resident NV12 resources: {error}"))
    })?;
    guard.allocation = Some(allocation.clone());
    let imported = imported.into_hal()?;
    let imported_desc =
        wgpu::BufferDescriptor { label: Some("va-decoded-nv12-dmabuf"), size: object_size, usage: wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false };
    let imported_oom_scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let imported_validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    // SAFETY: imported HAL buffer belongs to this device; descriptor matches its bounded allocation.
    let imported_buffer = unsafe { device.create_buffer_from_hal::<Vulkan>(imported, &imported_desc) };
    guard.imported = Some(imported_buffer.clone());
    if let Some(error) = poll_error_scope(imported_validation_scope, device)? {
        return Err(format!("wgpu rejected imported VA NV12 buffer: {error}"));
    }
    if let Some(error) = poll_error_scope(imported_oom_scope, device)? {
        return Err(format!("wgpu could not wrap imported VA NV12 buffer: {error}"));
    }

    let output_scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let output_validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("resident-linear-nv12-copy"),
        size: object_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    guard.copied = Some(output.clone());
    if let Some(error) = poll_error_scope(output_validation_scope, device)? {
        return Err(format!("invalid resident NV12 wgpu buffer descriptor: {error}"));
    }
    if let Some(error) = poll_error_scope(output_scope, device)? {
        return Err(format!("allocate resident NV12 copy buffer: {error}"));
    }

    guard.gpu_submission_attempted = true;
    let acquired = submit_buffer_foreign_transfer(device, queue, &imported_buffer, true)?;
    crate::vulkan_import::wait_for_submission(device, acquired, timeout)?;

    let copy_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut copy_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("resident-linear-nv12-copy") });
    copy_encoder.copy_buffer_to_buffer(&imported_buffer, 0, &output, 0, object_size);
    let copied = queue.submit([copy_encoder.finish()]);
    if let Some(error) = poll_error_scope(copy_scope, device)? {
        let _ = crate::vulkan_import::wait_for_submission(device, copied, timeout);
        return Err(format!("wgpu rejected VA NV12 GPU copy: {error}"));
    }

    let released = submit_buffer_foreign_transfer(device, queue, &imported_buffer, false)?;
    guard.retire_after_gpu_submission(released)?;
    guard.imported.take();
    let buffer = guard.copied.take().ok_or("resident NV12 copy owner is missing")?;
    let allocation = guard.allocation.take().ok_or("resident NV12 allocation hold is missing")?;
    Ok(ResidentNv12Buffer { buffer, drm_node, layout, _allocation: allocation })
}

/// Import and GPU-repack one decoded frame. Keep its owner until the exact final consumer
/// submission has completed; dropping an unretired frame intentionally quarantines its resources.
pub fn import_and_repack_va_nv12_frame(
    adapter: &Adapter,
    device: &Device,
    queue: &Queue,
    repacker: &filmcraft_gpu::nv12_repack::Nv12Repack,
    frame: crate::vaapi::ResidentDecodedFrame,
    reservation: &mut filmcraft_export::ResidentReservation,
    timeout: Duration,
) -> Result<ResidentNv12GpuFrame, String> {
    import_and_repack_inner(adapter, device, queue, repacker, frame, reservation, timeout, None)
}

pub(crate) fn import_and_repack_va_nv12_frame_with_dpb(
    adapter: &Adapter,
    device: &Device,
    queue: &Queue,
    repacker: &filmcraft_gpu::nv12_repack::Nv12Repack,
    frame: crate::vaapi::ResidentDecodedFrame,
    reservation: &mut filmcraft_export::ResidentReservation,
    timeout: Duration,
    source_allocation: filmcraft_export::ResidentAllocation,
) -> Result<ResidentNv12GpuFrame, String> {
    import_and_repack_inner(adapter, device, queue, repacker, frame, reservation, timeout, Some(source_allocation))
}

fn import_and_repack_inner(
    adapter: &Adapter,
    device: &Device,
    queue: &Queue,
    repacker: &filmcraft_gpu::nv12_repack::Nv12Repack,
    frame: crate::vaapi::ResidentDecodedFrame,
    reservation: &mut filmcraft_export::ResidentReservation,
    timeout: Duration,
    source_allocation: Option<filmcraft_export::ResidentAllocation>,
) -> Result<ResidentNv12GpuFrame, String> {
    let crate::vaapi::ResidentDecodedFrame { index, pts, crop, color, color_metadata, par, lease } = frame;
    let mut source_guard = ResidentSurfaceImportGuard::new(lease, device, queue, timeout, source_allocation);
    if timeout.is_zero() {
        return Err("resident VA repack timeout must be positive".into());
    }
    if crop.2 == 0 || crop.3 == 0 || !crop.0.is_multiple_of(2) || !crop.1.is_multiple_of(2) {
        return Err("resident NV12 crop must be nonempty with chroma-aligned origin".into());
    }
    let (source_width, source_height) = source_guard.source()?.size().ok_or("resident VA source dimensions are unavailable")?;
    let right = crop.0.checked_add(crop.2).ok_or("resident VA crop horizontal range overflow")?;
    let bottom = crop.1.checked_add(crop.3).ok_or("resident VA crop vertical range overflow")?;
    if right > source_width || bottom > source_height {
        return Err("resident VA output crop exceeds decoded surface".into());
    }
    let source_lease = source_guard.source()?;
    if source_lease.object_count() != Some(1) || source_lease.fourcc() != Some(ffi::VA_FOURCC_NV12) {
        return Err("resident VA repack requires one NV12 DMA-BUF object".into());
    }
    let object_size = source_lease.object_layout(0).ok_or("resident VA DMA-BUF object metadata missing")?.0;
    let y = source_lease.plane(0, 0).ok_or("resident VA luma plane metadata missing")?;
    let uv = source_lease.plane(1, 0).ok_or("resident VA chroma plane metadata missing")?;
    let source_layout = filmcraft_gpu::nv12_repack::Nv12SourceLayout::new(source_width, source_height, object_size, y.offset, y.pitch, uv.offset, uv.pitch)?;
    let output_layout = filmcraft_gpu::nv12_repack::Nv12Repack::output_layout(source_layout, (crop.0, crop.1), (crop.2, crop.3))?;
    let repack_bytes = filmcraft_gpu::nv12_repack::Nv12Repack::allocation_bytes(&output_layout)?;
    let imported = import_va_nv12_surface_with_allocation(
        adapter,
        device,
        queue,
        source_guard.take_source()?,
        reservation,
        timeout,
        repack_bytes,
        source_guard.take_source_allocation(),
    )?;
    let allocation = imported._allocation.clone();
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("resident-va-nv12-repack") });
    let mut budget = ResidentNv12Budget { allocation, bytes: repack_bytes, taken: false };
    let repacked = repacker.record(device, &mut encoder, imported.buffer(), source_layout, (crop.0, crop.1), (crop.2, crop.3), &mut budget)?;
    let repack_submission = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| queue.submit([encoder.finish()]))) {
        Ok(submission) => submission,
        Err(_) => {
            // The driver may have submitted before unwinding; preserve this frame's allocations.
            std::mem::forget(ResidentNv12GpuResources { repacked, imported });
            return Err("resident NV12 repack submission panicked; GPU ownership remains unproven".into());
        }
    };
    Ok(ResidentNv12GpuFrame {
        index,
        pts,
        crop,
        color,
        color_metadata,
        par,
        resources: Some(ResidentNv12GpuResources { repacked, imported }),
        device: device.clone(),
        queue: queue.clone(),
        identity: Arc::new(()),
        submission_generation: 0,
        repack_submission,
        retired: false,
    })
}

pub(crate) fn poll_error_scope(scope: wgpu::ErrorScopeGuard, device: &Device) -> Result<Option<wgpu::Error>, String> {
    use std::future::Future;
    let mut future = Box::pin(scope.pop());
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(error) => Ok(error),
        std::task::Poll::Pending => {
            device.poll(wgpu::PollType::Poll).map_err(|error| format!("wgpu error-scope poll failed: {error:?}"))?;
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(error) => Ok(error),
                std::task::Poll::Pending => Err("wgpu error scope did not resolve".into()),
            }
        }
    }
}

fn submit_buffer_foreign_transfer(device: &Device, queue: &Queue, buffer: &wgpu::Buffer, acquire: bool) -> Result<wgpu::SubmissionIndex, String> {
    use ash::vk;
    // SAFETY: guards retain the exact device, queue and imported buffer during raw handle access.
    let hal_device = unsafe { device.as_hal::<Vulkan>() }.ok_or("resident VA transfer device is not Vulkan")?;
    // SAFETY: guard retains queue while verifying exact device binding.
    let hal_queue = unsafe { queue.as_hal::<Vulkan>() }.ok_or("resident VA transfer queue is not Vulkan")?;
    if hal_queue.as_raw() != hal_device.raw_queue() {
        return Err("resident VA transfer queue does not belong to its device".into());
    }
    let raw_buffer = {
        // SAFETY: the private import owner retains this buffer through the completed transfer.
        let buffer = unsafe { buffer.as_hal::<Vulkan>() }.ok_or("resident VA buffer is not Vulkan")?;
        // SAFETY: the owner retains this borrowed handle after the resource guard is released.
        unsafe { buffer.raw_handle() }
    };
    let family = hal_device.queue_family_index();
    let (barrier, src_stage, dst_stage) = foreign_buffer_barrier(raw_buffer, buffer.size(), family, acquire);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("va-nv12-foreign-buffer-transfer") });
    let mut recorded = false;
    // SAFETY: encoder records only the raw Vulkan ownership barrier below.
    unsafe {
        encoder.as_hal_mut::<Vulkan, _, _>(|hal_encoder| {
            let Some(hal_encoder) = hal_encoder else { return };
            // SAFETY: the HAL guard exposes wgpu's active Vulkan command buffer.
            let command_buffer = hal_encoder.raw_handle();
            // SAFETY: ownership barrier covers the complete imported LINEAR DMA-BUF range.
            hal_device.raw_device().cmd_pipeline_barrier(command_buffer, src_stage, dst_stage, vk::DependencyFlags::empty(), &[], &[barrier], &[]);
            recorded = true;
        });
    }
    if !recorded {
        return Err("wgpu did not provide a Vulkan command encoder".into());
    }
    Ok(queue.submit([encoder.finish()]))
}

fn foreign_buffer_barrier(
    buffer: vk::Buffer,
    size: u64,
    family: u32,
    acquire: bool,
) -> (vk::BufferMemoryBarrier<'static>, vk::PipelineStageFlags, vk::PipelineStageFlags) {
    let (src_family, dst_family, src_access, dst_access, src_stage, dst_stage) = if acquire {
        (
            vk::QUEUE_FAMILY_FOREIGN_EXT,
            family,
            vk::AccessFlags::empty(),
            vk::AccessFlags::TRANSFER_READ,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
        )
    } else {
        (
            family,
            vk::QUEUE_FAMILY_FOREIGN_EXT,
            vk::AccessFlags::TRANSFER_READ,
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
        )
    };
    (
        vk::BufferMemoryBarrier::default()
            .src_access_mask(src_access)
            .dst_access_mask(dst_access)
            .src_queue_family_index(src_family)
            .dst_queue_family_index(dst_family)
            .buffer(buffer)
            .offset(0)
            .size(size),
        src_stage,
        dst_stage,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_type_selection_uses_only_compatible_bits() {
        assert_eq!(choose_memory_type(0b1010, 4), Some(1));
        assert_eq!(choose_memory_type(0, 32), None);
        assert_eq!(choose_memory_type(u32::MAX, 0), None);
    }

    #[test]
    fn nv12_layout_validation_accepts_padding_and_rejects_overlap_or_truncation() {
        assert!(validate_nv12_ranges(423_936, 640, 368, 0, 768, 282_624, 768).is_ok());
        assert!(validate_nv12_ranges(423_936, 640, 368, 0, 768, 100, 768).is_err());
        assert!(validate_nv12_ranges(100, 8, 4, 0, 8, 64, 8).is_ok());
        assert!(validate_nv12_ranges(79, 8, 4, 0, 8, 64, 8).is_err());
    }

    #[test]
    fn resident_import_requires_one_bounded_linear_nv12_object() {
        let valid = validate_linear_nv12_layout(Some(1), 423_936, ffi::DRM_FORMAT_MOD_LINEAR, 640, 368, 0, 768, 282_624, 768);
        assert_eq!(valid, Ok(Nv12BufferLayout { width: 640, height: 368, object_size: 423_936, y_offset: 0, uv_offset: 282_624, y_pitch: 768, uv_pitch: 768 }));
        assert!(validate_linear_nv12_layout(Some(2), 423_936, ffi::DRM_FORMAT_MOD_LINEAR, 640, 368, 0, 768, 282_624, 768).is_err());
        assert!(validate_linear_nv12_layout(Some(1), 423_936, 1, 640, 368, 0, 768, 282_624, 768).is_err());
        assert!(validate_linear_nv12_layout(Some(1), 79, ffi::DRM_FORMAT_MOD_LINEAR, 8, 4, 0, 8, 64, 8).is_err());
    }

    #[test]
    fn foreign_buffer_barriers_transfer_the_complete_object_both_ways() {
        let buffer = vk::Buffer::null();
        let (acquire, acquire_src, acquire_dst) = foreign_buffer_barrier(buffer, 4096, 3, true);
        assert_eq!((acquire.src_queue_family_index, acquire.dst_queue_family_index), (vk::QUEUE_FAMILY_FOREIGN_EXT, 3));
        assert_eq!((acquire.src_access_mask, acquire.dst_access_mask), (vk::AccessFlags::empty(), vk::AccessFlags::TRANSFER_READ));
        assert_eq!((acquire_src, acquire_dst), (vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER));
        assert_eq!((acquire.offset, acquire.size), (0, 4096));

        let (release, release_src, release_dst) = foreign_buffer_barrier(buffer, 4096, 3, false);
        assert_eq!((release.src_queue_family_index, release.dst_queue_family_index), (3, vk::QUEUE_FAMILY_FOREIGN_EXT));
        assert_eq!((release.src_access_mask, release.dst_access_mask), (vk::AccessFlags::TRANSFER_READ, vk::AccessFlags::empty()));
        assert_eq!((release_src, release_dst), (vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::BOTTOM_OF_PIPE));
        assert_eq!((release.offset, release.size), (0, 4096));
    }

    #[test]
    fn aggregate_reservation_uses_checked_addition() {
        assert_eq!(aggregate_reservation_bytes(256, 256, 384), Ok(896));
        assert!(aggregate_reservation_bytes(u64::MAX, 1, 0).is_err());
    }
}
