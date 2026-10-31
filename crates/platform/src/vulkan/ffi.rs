//! The Vulkan entry points and structures this crate uses (vulkan.h, Vulkan 1.1 and the DRM
//! extensions), transcribed and pinned against the header in `abi_tests.rs`. Only what the DMABUF
//! import probe needs: no device is created here, no image is made.
//!
//! The declarations follow the header's names, as vaapi's do.
#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_uint, c_void};

use libloading::Library;

/// `VkResult`: 0 is `VK_SUCCESS`.
pub const VK_SUCCESS: c_int = 0;
/// `VkResult`: an enumeration filled the array it was given and has more entries.
pub const VK_INCOMPLETE: c_int = 5;
pub const VK_STRUCTURE_TYPE_APPLICATION_INFO: c_int = 0;
pub const VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO: c_int = 1;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2: c_int = 1000059001;
pub const VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRM_PROPERTIES_EXT: c_int = 1000353000;
/// `VK_MAKE_API_VERSION(0, 1, 1, 0)`: Vulkan 1.1, where `vkGetPhysicalDeviceProperties2` is core.
pub const VK_API_VERSION_1_1: u32 = (1 << 22) | (1 << 12);
pub const VK_MAX_EXTENSION_NAME_SIZE: usize = 256;

/// `VK_VERSION_MAJOR` / `VK_VERSION_MINOR` of an `apiVersion` word.
pub const fn version_major(v: u32) -> u32 {
    v >> 22
}
pub const fn version_minor(v: u32) -> u32 {
    (v >> 12) & 0x3ff
}

/// The extensions the DMABUF import needs (and the one that says which GPU a device is).
pub const EXT_EXTERNAL_MEMORY_DMA_BUF: &str = "VK_EXT_external_memory_dma_buf";
pub const EXT_IMAGE_DRM_FORMAT_MODIFIER: &str = "VK_EXT_image_drm_format_modifier";
pub const EXT_PHYSICAL_DEVICE_DRM: &str = "VK_EXT_physical_device_drm";

/// `VkAllocationCallbacks`: the loader's default allocator (a null pointer) is what we pass.
pub type VkAllocationCallbacks = c_void;
pub type VkInstance = *mut c_void;
pub type VkPhysicalDevice = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkApplicationInfo {
    pub s_type: c_int,
    pub p_next: *const c_void,
    pub p_application_name: *const c_char,
    pub application_version: u32,
    pub p_engine_name: *const c_char,
    pub engine_version: u32,
    pub api_version: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkInstanceCreateInfo {
    pub s_type: c_int,
    pub p_next: *const c_void,
    pub flags: u32,
    pub p_application_info: *const VkApplicationInfo,
    pub enabled_layer_count: u32,
    pub pp_enabled_layer_names: *const *const c_char,
    pub enabled_extension_count: u32,
    pub pp_enabled_extension_names: *const *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkExtensionProperties {
    pub extension_name: [c_char; VK_MAX_EXTENSION_NAME_SIZE],
    pub spec_version: u32,
}

// [T; 256] has no `Default` (std stops at 32), and a zeroed structure is what the API wants.
impl Default for VkExtensionProperties {
    fn default() -> Self {
        VkExtensionProperties { extension_name: [0; VK_MAX_EXTENSION_NAME_SIZE], spec_version: 0 }
    }
}

/// `VkPhysicalDeviceProperties`: 824 bytes, of which only the first fields are read. The tail is
/// carried so the driver writes into a buffer of the size it expects (abi_tests.rs pins both).
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct VkPhysicalDeviceProperties {
    pub api_version: u32,
    pub driver_version: u32,
    pub vendor_id: u32,
    pub device_id: u32,
    /// `VK_PHYSICAL_DEVICE_TYPE_*`.
    pub device_type: u32,
    pub device_name: [c_char; VK_MAX_EXTENSION_NAME_SIZE],
    /// `limits` and `sparseProperties`: never read here.
    pub tail: [u8; 824 - 20 - VK_MAX_EXTENSION_NAME_SIZE],
}

impl Default for VkPhysicalDeviceProperties {
    fn default() -> Self {
        VkPhysicalDeviceProperties {
            api_version: 0,
            driver_version: 0,
            vendor_id: 0,
            device_id: 0,
            device_type: 0,
            device_name: [0; VK_MAX_EXTENSION_NAME_SIZE],
            tail: [0; 824 - 20 - VK_MAX_EXTENSION_NAME_SIZE],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct VkPhysicalDeviceProperties2 {
    pub s_type: c_int,
    pub p_next: *mut c_void,
    pub properties: VkPhysicalDeviceProperties,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VkPhysicalDeviceDrmPropertiesEXT {
    pub s_type: c_int,
    pub p_next: *mut c_void,
    pub has_primary: u32,
    pub has_render: u32,
    pub primary_major: u64,
    pub primary_minor: u64,
    pub render_major: u64,
    pub render_minor: u64,
}

pub type vkCreateInstance = unsafe extern "C" fn(*const VkInstanceCreateInfo, *const VkAllocationCallbacks, *mut VkInstance) -> c_int;
pub type vkDestroyInstance = unsafe extern "C" fn(VkInstance, *const VkAllocationCallbacks);
pub type vkEnumeratePhysicalDevices = unsafe extern "C" fn(VkInstance, *mut c_uint, *mut VkPhysicalDevice) -> c_int;
pub type vkGetPhysicalDeviceProperties = unsafe extern "C" fn(VkPhysicalDevice, *mut VkPhysicalDeviceProperties);
pub type vkGetPhysicalDeviceProperties2 = unsafe extern "C" fn(VkPhysicalDevice, *mut VkPhysicalDeviceProperties2);
pub type vkEnumerateDeviceExtensionProperties = unsafe extern "C" fn(VkPhysicalDevice, *const c_char, *mut c_uint, *mut VkExtensionProperties) -> c_int;

/// The loader and the entry points this crate uses (it stays loaded for the life of the process).
pub struct Api {
    pub create_instance: vkCreateInstance,
    pub destroy_instance: vkDestroyInstance,
    pub enumerate_physical_devices: vkEnumeratePhysicalDevices,
    pub get_physical_device_properties: vkGetPhysicalDeviceProperties,
    pub get_physical_device_properties2: vkGetPhysicalDeviceProperties2,
    pub enumerate_device_extension_properties: vkEnumerateDeviceExtensionProperties,
    pub _lib: Library,
}
