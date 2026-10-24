//! Pure AMD encode-route selection; capability discovery lives outside this module.

use std::fmt;

use filmcraft_export::Format;
use wgpu::DeviceType;

use crate::vulkan_import::DrmNodeId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AmdVideoBackend {
    Vaapi,
}

#[derive(Clone, Debug)]
pub(crate) struct VulkanVideoCapabilities {
    pub codec: Format,
    pub extensions: bool,
    pub encode_queue: bool,
    pub codec_profile: bool,
    pub input_format: bool,
    pub required_usages: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct VaapiVideoCapabilities {
    pub drm_node: Option<DrmNodeId>,
    pub vendor_id: Option<u32>,
    pub device_id: Option<u32>,
    pub profile: Option<i32>,
    pub rate_control: u32,
    pub max_size: Option<(u32, u32)>,
    pub entrypoint: i32,
    pub packed_headers: u32,
    pub codec: Format,
    pub encode: bool,
    pub codec_profile: bool,
    pub input_format: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct AmdBackendCandidate {
    pub vendor_id: u32,
    pub device_id: u32,
    pub device_type: DeviceType,
    pub vulkan_drm_node: Option<DrmNodeId>,
    pub vulkan_video: VulkanVideoCapabilities,
    pub vaapi_drm_node: Option<DrmNodeId>,
    pub vaapi_video: VaapiVideoCapabilities,
    pub vulkan_diagnostic: String,
    pub vaapi_diagnostic: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AmdBackendError {
    pub request: Format,
    pub vulkan_reason: String,
    pub vaapi_reason: String,
}

impl fmt::Display for AmdBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AMD {:?}: Vulkan Video: {}; VA-API: {}", self.request, self.vulkan_reason, self.vaapi_reason)
    }
}

impl std::error::Error for AmdBackendError {}

/// Encode through same-device AMD VA-API only; Vulkan Video remains diagnostic discovery.
pub(crate) fn choose_amd_video_backend(request: Format, candidates: &[AmdBackendCandidate]) -> Result<AmdVideoBackend, AmdBackendError> {
    let mut vulkan_reasons = Vec::new();
    let mut vaapi_reasons = Vec::new();
    let mut vaapi_complete = false;
    for candidate in candidates {
        let identity = format!("device {:#06x}", candidate.device_id);
        let rejection = if candidate.vendor_id != 0x1002 {
            Some(format!("vendor {:#06x} is not AMD", candidate.vendor_id))
        } else if !matches!(candidate.device_type, DeviceType::IntegratedGpu | DeviceType::DiscreteGpu) {
            Some(format!("device type {:?} is not an integrated/discrete GPU", candidate.device_type))
        } else {
            None
        };
        if let Some(reason) = rejection {
            vulkan_reasons.push(format!("{identity}: {reason}"));
            vaapi_reasons.push(format!("{identity}: {reason}"));
            continue;
        }

        let vulkan = &candidate.vulkan_video;
        let mut missing = Vec::new();
        if candidate.vulkan_drm_node.is_none() {
            missing.push("missing Vulkan DRM node identity");
        }
        if vulkan.codec != request {
            missing.push("requested codec mismatch");
        }
        if !vulkan.extensions {
            missing.push("missing required video extensions");
        }
        if !vulkan.encode_queue {
            missing.push("missing encode queue");
        }
        if !vulkan.codec_profile {
            missing.push("missing codec profile");
        }
        if !vulkan.input_format {
            missing.push("missing input format");
        }
        if !vulkan.required_usages {
            missing.push("missing required image usages");
        }
        missing.push("Vulkan Video encode disabled by explicit user policy");
        vulkan_reasons.push(format!("{identity}: {}; {}", missing.join(", "), candidate.vulkan_diagnostic));

        let vaapi = &candidate.vaapi_video;
        missing.clear();
        match (candidate.vulkan_drm_node, candidate.vaapi_drm_node) {
            (Some(vulkan_node), Some(vaapi_node)) if vulkan_node == vaapi_node => {}
            (Some(_), Some(_)) => missing.push("DRM node mismatch"),
            _ => missing.push("missing Vulkan or VA-API DRM node identity"),
        }
        if vaapi.drm_node.is_none() || vaapi.drm_node != candidate.vulkan_drm_node || vaapi.drm_node != candidate.vaapi_drm_node {
            missing.push("VA-API record DRM node missing or mismatched");
        }
        if vaapi.vendor_id != Some(0x1002) {
            missing.push("VA-API record vendor is non-AMD or unknown");
        }
        if vaapi.device_id.is_some_and(|id| id != candidate.device_id) {
            missing.push("VA-API record device ID mismatch");
        }
        let profile_matches = match request {
            Format::H264 => matches!(vaapi.profile, Some(crate::vaapi::ffi::VAProfileH264Main | crate::vaapi::ffi::VAProfileH264High)),
            Format::Hevc => vaapi.profile == Some(crate::vaapi::ffi::VAProfileHEVCMain),
            _ => false,
        };
        if !profile_matches {
            missing.push("VA-API record requested codec profile mismatch");
        }
        if vaapi.codec != request {
            missing.push("requested codec mismatch");
        }
        if !vaapi.encode {
            missing.push("missing encode capability");
        }
        if !vaapi.codec_profile {
            missing.push("missing codec profile");
        }
        if !vaapi.input_format {
            missing.push("missing input format");
        }
        if missing.is_empty() {
            vaapi_complete = true;
        } else {
            vaapi_reasons.push(format!("{identity}: {}; {}", missing.join(", "), candidate.vaapi_diagnostic));
        }
    }
    if vaapi_complete {
        return Ok(AmdVideoBackend::Vaapi);
    }
    if candidates.is_empty() {
        vulkan_reasons.push("no AMD GPU candidates".into());
        vaapi_reasons.push("no AMD GPU candidates".into());
    }
    Err(AmdBackendError { request, vulkan_reason: vulkan_reasons.join("; "), vaapi_reason: vaapi_reasons.join("; ") })
}

pub(crate) const VIDEO_NV12_USAGES: ash::vk::ImageUsageFlags =
    ash::vk::ImageUsageFlags::from_raw(ash::vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR.as_raw() | ash::vk::ImageUsageFlags::TRANSFER_DST.as_raw());
pub(crate) const VIDEO_RGBA_USAGES: ash::vk::ImageUsageFlags = ash::vk::ImageUsageFlags::from_raw(
    ash::vk::ImageUsageFlags::COLOR_ATTACHMENT.as_raw() | ash::vk::ImageUsageFlags::STORAGE.as_raw() | ash::vk::ImageUsageFlags::TRANSFER_SRC.as_raw(),
);

pub(crate) fn video_image_usages_supported(nv12: ash::vk::ImageUsageFlags, rgba: ash::vk::ImageUsageFlags) -> bool {
    nv12.contains(VIDEO_NV12_USAGES) && rgba.contains(VIDEO_RGBA_USAGES)
}

/// Validated discovery input; no driver calls are needed to exercise eligibility.
pub(crate) struct VulkanVideoProbe {
    pub api_version: u32,
    pub synchronization2_extension: bool,
    pub drm_node: Option<DrmNodeId>,
    pub missing_extension: Option<String>,
    pub encode_queue: bool,
    pub codec_profile: bool,
    pub input_format: bool,
    pub required_usages: bool,
}

pub(crate) fn validate_vulkan_video_dependencies(api_version: u32, synchronization2_extension: bool) -> Result<(), String> {
    if api_version < ash::vk::API_VERSION_1_1 {
        return Err("Vulkan Video requires Vulkan 1.1".into());
    }
    if api_version < ash::vk::API_VERSION_1_3 && !synchronization2_extension {
        return Err("missing required extension VK_KHR_synchronization2 before Vulkan 1.3".into());
    }
    Ok(())
}

pub(crate) fn validate_vulkan_video_probe(codec: Format, probe: VulkanVideoProbe) -> Result<VulkanVideoCapabilities, String> {
    validate_vulkan_video_dependencies(probe.api_version, probe.synchronization2_extension)?;
    if let Some(extension) = probe.missing_extension {
        return Err(format!("missing required extension {extension}"));
    }
    if probe.drm_node.is_none() {
        return Err("missing Vulkan DRM render node identity".into());
    }
    for (supported, name) in [
        (probe.encode_queue, "encode queue"),
        (probe.codec_profile, "codec profile"),
        (probe.input_format, "input format"),
        (probe.required_usages, "required image usages"),
    ] {
        if !supported {
            return Err(format!("missing Vulkan Video {name}"));
        }
    }
    Ok(VulkanVideoCapabilities { codec, extensions: true, encode_queue: true, codec_profile: true, input_format: true, required_usages: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video_probe() -> VulkanVideoProbe {
        VulkanVideoProbe {
            api_version: ash::vk::API_VERSION_1_3,
            synchronization2_extension: false,
            drm_node: Some(DrmNodeId { major: 226, minor: 128 }),
            missing_extension: None,
            encode_queue: true,
            codec_profile: true,
            input_format: true,
            required_usages: true,
        }
    }

    #[test]
    fn vulkan_video_requires_queue_profile_and_all_image_usages() {
        use ash::vk::ImageUsageFlags as Usage;
        assert!(video_image_usages_supported(VIDEO_NV12_USAGES, VIDEO_RGBA_USAGES));
        for usage in [Usage::VIDEO_ENCODE_SRC_KHR, Usage::TRANSFER_DST] {
            assert!(!video_image_usages_supported(VIDEO_NV12_USAGES & !usage, VIDEO_RGBA_USAGES));
        }
        for usage in [Usage::COLOR_ATTACHMENT, Usage::STORAGE, Usage::TRANSFER_SRC] {
            assert!(!video_image_usages_supported(VIDEO_NV12_USAGES, VIDEO_RGBA_USAGES & !usage));
        }
        for codec in [Format::H264, Format::Hevc] {
            assert!(validate_vulkan_video_probe(codec, video_probe()).is_ok());
            for missing in 0..4 {
                let mut probe = video_probe();
                let expected = match missing {
                    0 => {
                        probe.encode_queue = false;
                        "encode queue"
                    }
                    1 => {
                        probe.codec_profile = false;
                        "codec profile"
                    }
                    2 => {
                        probe.input_format = false;
                        "input format"
                    }
                    _ => {
                        probe.required_usages = false;
                        "image usages"
                    }
                };
                assert!(validate_vulkan_video_probe(codec, probe).unwrap_err().contains(expected));
            }
        }
    }

    #[test]
    fn vulkan_video_requires_synchronization2_before_vulkan_1_3() {
        for version in [ash::vk::API_VERSION_1_1, ash::vk::API_VERSION_1_2] {
            let mut probe = video_probe();
            probe.api_version = version;
            let rejected = validate_vulkan_video_probe(Format::H264, probe);
            assert!(rejected.is_err(), "pre-1.3 Vulkan without synchronization2 must be ineligible");
            let reason = rejected.unwrap_err();
            assert!(reason.contains("VK_KHR_synchronization2"), "{reason}");
            let mut adapter = candidate();
            adapter.vulkan_video.extensions = false;
            adapter.vulkan_diagnostic = reason;
            assert_eq!(choose_amd_video_backend(Format::H264, &[adapter]), Ok(AmdVideoBackend::Vaapi));
            let mut probe = video_probe();
            probe.api_version = version;
            probe.synchronization2_extension = true;
            assert!(validate_vulkan_video_probe(Format::H264, probe).is_ok());
        }
        assert!(validate_vulkan_video_probe(Format::Hevc, video_probe()).is_ok());
        let mut probe = video_probe();
        probe.api_version = ash::vk::API_VERSION_1_0;
        probe.synchronization2_extension = true;
        assert!(validate_vulkan_video_probe(Format::H264, probe).unwrap_err().contains("Vulkan 1.1"));
    }

    #[test]
    fn vulkan_video_reports_missing_extension_or_drm_identity() {
        for extension in
            ["VK_KHR_video_queue", "VK_KHR_video_encode_queue", "VK_KHR_video_encode_h264", "VK_KHR_video_encode_h265", "VK_EXT_physical_device_drm"]
        {
            let mut probe = video_probe();
            probe.missing_extension = Some(extension.into());
            assert!(validate_vulkan_video_probe(Format::H264, probe).unwrap_err().contains(extension));
        }
        let mut probe = video_probe();
        probe.drm_node = None;
        assert!(validate_vulkan_video_probe(Format::Hevc, probe).unwrap_err().contains("DRM"));
    }

    fn candidate() -> AmdBackendCandidate {
        AmdBackendCandidate {
            vendor_id: 0x1002,
            device_id: 0x164e,
            device_type: DeviceType::IntegratedGpu,
            vulkan_drm_node: Some(DrmNodeId { major: 226, minor: 128 }),
            vulkan_video: VulkanVideoCapabilities {
                codec: Format::H264,
                extensions: true,
                encode_queue: true,
                codec_profile: true,
                input_format: true,
                required_usages: true,
            },
            vaapi_drm_node: Some(DrmNodeId { major: 226, minor: 128 }),
            vaapi_video: VaapiVideoCapabilities {
                drm_node: Some(DrmNodeId { major: 226, minor: 128 }),
                vendor_id: Some(0x1002),
                device_id: Some(0x164e),
                profile: Some(crate::vaapi::ffi::VAProfileH264High),
                rate_control: crate::vaapi::ffi::VA_RC_CBR | crate::vaapi::ffi::VA_RC_VBR,
                max_size: Some((4096, 4096)),
                entrypoint: crate::vaapi::ffi::VAEntrypointEncSlice,
                packed_headers: 0,
                codec: Format::H264,
                encode: true,
                codec_profile: true,
                input_format: true,
            },
            vulkan_diagnostic: "driver lacks video encoding".into(),
            vaapi_diagnostic: "driver lacks requested profile".into(),
        }
    }

    #[test]
    fn vaapi_is_only_encode_route_even_with_complete_vulkan_video() {
        assert_eq!(choose_amd_video_backend(Format::H264, &[candidate()]), Ok(AmdVideoBackend::Vaapi));
        let mut adapter = candidate();
        adapter.vaapi_video.encode = false;
        assert!(choose_amd_video_backend(Format::H264, &[adapter]).is_err());
    }

    #[test]
    fn vaapi_rejects_inconsistent_probe_identity_and_profile() {
        for invalid in 0..7 {
            let mut adapter = candidate();
            match invalid {
                0 => adapter.vaapi_video.drm_node = None,
                1 => adapter.vaapi_video.drm_node = Some(DrmNodeId { major: 226, minor: 129 }),
                2 => adapter.vaapi_video.vendor_id = None,
                3 => adapter.vaapi_video.vendor_id = Some(0x8086),
                4 => adapter.vaapi_video.device_id = Some(0xffff),
                5 => adapter.vaapi_video.profile = None,
                _ => adapter.vaapi_video.profile = Some(crate::vaapi::ffi::VAProfileHEVCMain),
            }
            assert!(choose_amd_video_backend(Format::H264, &[adapter]).is_err(), "inconsistent record {invalid}");
        }
        let mut adapter = candidate();
        adapter.vaapi_video.device_id = None;
        assert_eq!(choose_amd_video_backend(Format::H264, &[adapter]), Ok(AmdVideoBackend::Vaapi));
    }

    fn assert_error(request: Format, candidates: &[AmdBackendCandidate], fragments: &[&str]) {
        let result = choose_amd_video_backend(request, candidates);
        assert!(result.is_err(), "unexpected backend: {result:?}");
        if let Err(error) = result {
            let text = error.to_string();
            for fragment in fragments {
                assert!(text.contains(fragment), "missing {fragment:?} in {text:?}");
            }
        }
    }

    #[test]
    fn amd_integrated_adapter_is_eligible_without_dedicated_vram() {
        let integrated = candidate();
        assert_eq!(choose_amd_video_backend(Format::H264, std::slice::from_ref(&integrated)), Ok(AmdVideoBackend::Vaapi));
        let mut discrete = integrated.clone();
        discrete.device_type = DeviceType::DiscreteGpu;
        let mut vaapi_first = integrated;
        vaapi_first.vulkan_video.encode_queue = false;
        assert_eq!(choose_amd_video_backend(Format::H264, &[vaapi_first, discrete]), Ok(AmdVideoBackend::Vaapi));
    }

    #[test]
    fn negotiation_rejects_cpu_software_and_non_amd_adapters() {
        for device_type in [DeviceType::Cpu, DeviceType::Other, DeviceType::VirtualGpu] {
            let mut adapter = candidate();
            adapter.device_type = device_type;
            assert_error(Format::H264, &[adapter], &["AMD", "H264", "Vulkan Video", "VA-API", "device type"]);
        }
        let mut intel = candidate();
        intel.vendor_id = 0x8086;
        assert_error(Format::H264, &[intel], &["AMD", "H264", "Vulkan Video", "VA-API", "vendor"]);
    }

    #[test]
    fn incomplete_vulkan_video_capabilities_fall_back_to_matching_vaapi() {
        for missing in 0..7 {
            let mut adapter = candidate();
            match missing {
                0 => adapter.vulkan_video.extensions = false,
                1 => adapter.vulkan_video.encode_queue = false,
                2 => adapter.vulkan_video.codec_profile = false,
                3 => adapter.vulkan_video.input_format = false,
                4 => adapter.vulkan_video.required_usages = false,
                5 => adapter.vulkan_video.codec = Format::Hevc,
                _ => adapter.vulkan_drm_node = None,
            }
            if missing == 6 {
                assert_error(Format::H264, &[adapter], &["AMD", "H264", "Vulkan Video", "VA-API", "DRM"]);
            } else {
                assert_eq!(choose_amd_video_backend(Format::H264, &[adapter]), Ok(AmdVideoBackend::Vaapi));
            }
        }
        let mut hevc = candidate();
        hevc.vulkan_video.codec = Format::Hevc;
        hevc.vaapi_video.codec = Format::Hevc;
        hevc.vaapi_video.profile = Some(crate::vaapi::ffi::VAProfileHEVCMain);
        assert_eq!(choose_amd_video_backend(Format::Hevc, &[hevc]), Ok(AmdVideoBackend::Vaapi));
    }

    #[test]
    fn vaapi_on_different_drm_node_is_rejected() {
        let mut adapter = candidate();
        adapter.vulkan_video.encode_queue = false;
        adapter.vaapi_drm_node = Some(DrmNodeId { major: 226, minor: 129 });
        assert_error(Format::H264, &[adapter], &["AMD", "H264", "Vulkan Video", "VA-API", "encode queue", "DRM node mismatch"]);
    }

    #[test]
    fn no_complete_backend_returns_actionable_error_without_cpu_fallback() {
        let mut adapter = candidate();
        adapter.vulkan_video.encode_queue = false;
        adapter.vaapi_video.codec_profile = false;
        assert_error(
            Format::H264,
            &[adapter.clone()],
            &["AMD", "H264", "Vulkan Video", "VA-API", "encode queue", "codec profile", "driver lacks video encoding", "driver lacks requested profile"],
        );
        for missing in 0..4 {
            let mut invalid = adapter.clone();
            invalid.vaapi_video.codec_profile = true;
            let fragment = match missing {
                0 => {
                    invalid.vaapi_video.codec = Format::Hevc;
                    "requested codec"
                }
                1 => {
                    invalid.vaapi_video.encode = false;
                    "encode capability"
                }
                2 => {
                    invalid.vaapi_video.input_format = false;
                    "input format"
                }
                _ => {
                    invalid.vaapi_drm_node = None;
                    "DRM"
                }
            };
            assert_error(Format::H264, &[invalid], &["AMD", "H264", "Vulkan Video", "VA-API", fragment]);
        }
        assert_error(Format::Hevc, &[], &["AMD", "Hevc", "Vulkan Video", "VA-API", "no AMD GPU candidates"]);
    }
}
