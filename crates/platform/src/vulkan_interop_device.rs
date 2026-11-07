//! AMD Vulkan device creation for VA-API DMA-BUF interoperability.
//!
//! This opens a device from a caller-selected adapter. It does not select a GPU or negotiate a
//! VA-API session, and enabling these Vulkan capabilities does not establish interop ownership or
//! synchronization.

use std::ffi::CStr;

use crate::{vaapi::DrmNodeId, vulkan_import::render_node_id};
use wgpu::{Adapter, Device, DeviceDescriptor, DeviceType, Features, Queue};
use wgpu_hal::api::Vulkan;

const AMD_VENDOR_ID: u32 = 0x1002;
const DMA_BUF_FEATURE: Features = Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;

fn strict_features(available: Features, requested: Features, foreign_supported: bool) -> Result<Features, String> {
    if !available.contains(requested) {
        return Err("Vulkan adapter does not support all requested wgpu features".into());
    }
    if !available.contains(DMA_BUF_FEATURE) {
        return Err("Vulkan adapter lacks VULKAN_EXTERNAL_MEMORY_DMA_BUF".into());
    }
    if !foreign_supported {
        return Err("Vulkan adapter lacks VK_EXT_queue_family_foreign".into());
    }
    Ok(requested | DMA_BUF_FEATURE)
}

fn validate_limits(requested: &wgpu::Limits, allowed: &wgpu::Limits) -> Result<(), String> {
    if requested.check_limits(allowed) { Ok(()) } else { Err("Vulkan adapter does not support all requested wgpu limits".into()) }
}

fn validate_experimental_features(requested: Features, opt_in: bool) -> Result<(), String> {
    if requested.intersects(Features::all_experimental_mask()) && !opt_in {
        return Err("experimental wgpu features require explicit descriptor opt-in".into());
    }
    Ok(())
}

fn append_foreign_extension(extensions: &mut Vec<&'static CStr>) {
    let foreign = ash::ext::queue_family_foreign::NAME;
    if !extensions.contains(&foreign) {
        extensions.push(foreign);
    }
}

fn supports_foreign_queue_family(adapter: &wgpu_hal::vulkan::Adapter) -> Result<bool, String> {
    let instance = adapter.shared_instance().raw_instance();
    // SAFETY: adapter guard retains its Vulkan instance and physical device for this query.
    let extensions = unsafe { instance.enumerate_device_extension_properties(adapter.raw_physical_device()) }
        .map_err(|error| format!("enumerate Vulkan device extensions: {error:?}"))?;
    Ok(extensions.iter().any(|extension| extension.extension_name_as_c_str() == Ok(ash::ext::queue_family_foreign::NAME)))
}

/// Create a wgpu device and queue from this exact AMD Vulkan adapter, with DMA-BUF and foreign
/// queue-family support enabled. Caller remains responsible for VA-API/Vulkan layout and sync.
pub fn create_amd_interop_device(adapter: &Adapter, descriptor: &DeviceDescriptor<'_>, expected_node: DrmNodeId) -> Result<(Device, Queue), String> {
    let info = adapter.get_info();
    if info.backend != wgpu::Backend::Vulkan || info.vendor != AMD_VENDOR_ID {
        return Err("interop device requires an AMD Vulkan adapter".into());
    }
    if !matches!(info.device_type, DeviceType::IntegratedGpu | DeviceType::DiscreteGpu) {
        return Err("interop device requires a physical AMD GPU".into());
    }
    validate_limits(&descriptor.required_limits, &adapter.limits())?;
    validate_experimental_features(descriptor.required_features, descriptor.experimental_features.is_enabled())?;
    if render_node_id(adapter)? != expected_node {
        return Err("VA and Vulkan render nodes differ".into());
    }

    // SAFETY: this guard retains the supplied adapter's Vulkan backend handle.
    let hal_adapter = unsafe { adapter.as_hal::<Vulkan>() }.ok_or("interop adapter is not Vulkan")?;
    let features = strict_features(adapter.features(), descriptor.required_features, supports_foreign_queue_family(&hal_adapter)?)?;
    let device_descriptor = DeviceDescriptor { required_features: features, ..descriptor.clone() };
    // wgpu-hal's normal feature mapping enables external_memory_fd, external_memory_dma_buf, and
    // image_drm_format_modifier. Its callback is the narrow public seam for adding foreign ownership.
    // SAFETY: only a supported device extension is added; HAL retains and owns the resulting device.
    let open_device = unsafe {
        hal_adapter.open_with_callback(
            device_descriptor.required_features,
            &device_descriptor.required_limits,
            &device_descriptor.memory_hints,
            Some(Box::new(|args| {
                append_foreign_extension(args.extensions);
            })),
        )
    }
    .map_err(|error| format!("open AMD Vulkan interop device: {error:?}"))?;

    if open_device.device.raw_physical_device() != hal_adapter.raw_physical_device() {
        return Err("opened Vulkan device does not match selected adapter".into());
    }
    // SAFETY: HAL device was opened from this exact adapter, and descriptor features equal the
    // features used to open it. wgpu takes ownership of this device and its queue.
    let (device, queue) = unsafe { adapter.create_device_from_hal::<Vulkan>(open_device, &device_descriptor) }
        .map_err(|error| format!("wrap AMD Vulkan interop device: {error:?}"))?;

    // SAFETY: guard retains the newly wrapped device while checking its immutable capabilities.
    let hal_device = unsafe { device.as_hal::<Vulkan>() }.ok_or("created interop device is not Vulkan")?;
    if !device.features().contains(DMA_BUF_FEATURE)
        || !hal_device.enabled_device_extensions().contains(&ash::khr::external_memory_fd::NAME)
        || !hal_device.enabled_device_extensions().contains(&ash::ext::external_memory_dma_buf::NAME)
        || !hal_device.enabled_device_extensions().contains(&ash::ext::image_drm_format_modifier::NAME)
        || !hal_device.enabled_device_extensions().contains(&ash::ext::queue_family_foreign::NAME)
        || hal_device.raw_physical_device() != hal_adapter.raw_physical_device()
    {
        return Err("created Vulkan device is missing required DMA-BUF or foreign ownership capability".into());
    }
    drop(hal_device);
    Ok((device, queue))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_features_require_dma_buf_and_foreign_extension() {
        let available = DMA_BUF_FEATURE | Features::FLOAT32_FILTERABLE;
        assert_eq!(strict_features(available, Features::FLOAT32_FILTERABLE, true), Ok(available));
        assert!(strict_features(Features::FLOAT32_FILTERABLE, Features::empty(), true).is_err());
        assert!(strict_features(available, Features::empty(), false).is_err());
        assert!(strict_features(available, Features::FLOAT32_BLENDABLE, true).is_err());
    }

    #[test]
    fn foreign_extension_is_added_once() {
        let mut extensions = Vec::new();
        append_foreign_extension(&mut extensions);
        append_foreign_extension(&mut extensions);
        assert_eq!(extensions, vec![ash::ext::queue_family_foreign::NAME]);
    }

    #[test]
    fn requested_limits_must_fit_adapter_limits() {
        let allowed = wgpu::Limits::default();
        let mut requested = allowed.clone();
        requested.max_buffer_size = allowed.max_buffer_size.saturating_add(1);
        assert!(validate_limits(&allowed, &allowed).is_ok());
        assert!(validate_limits(&requested, &allowed).is_err());
    }

    #[test]
    fn experimental_features_require_descriptor_opt_in() {
        let requested = Features::EXPERIMENTAL_MESH_SHADER;
        assert!(validate_experimental_features(Features::empty(), false).is_ok());
        assert!(validate_experimental_features(requested, false).is_err());
        assert!(validate_experimental_features(requested, true).is_ok());
    }
}
