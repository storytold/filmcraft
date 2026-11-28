//! Vulkan, for one question only: can a decoded VA-API surface's DMABUF be imported where the
//! compositor draws?
//!
//! The zero-copy path is: `vaapi::va::Display::export_surface` hands over the objects and plane
//! layout, and a Vulkan image made from them (`VK_EXT_external_memory_dma_buf` +
//! `VK_EXT_image_drm_format_modifier`) is what the compositor would sample instead of a readback.
//! Importing needs the *same* device the compositor uses, so this module also asks each device for
//! its DRM nodes (`VK_EXT_physical_device_drm`) and compares them with the VA display's render
//! node: a mismatch means zero-copy is not available on this machine (two GPUs, or a decoder
//! without a Vulkan device).
//!
//! Only the probe is here. Binding the image where wgpu draws needs raw Vulkan handles: wgpu's safe
//! API has no external-memory import, so the compositor would have to take the `hal` path
//! (`wgpu-hal`, `VkImage` + `VkDeviceMemory` from `vkAllocateMemory` with the imported fd, then a
//! `wgpu::Texture` around it). Until that lands, the readback stays the only path.
//!
//! FFI module (docs/adr/0001-platform-ffi.md): `libvulkan.so.1` is loaded at run time, every
//! `unsafe` block has a `// SAFETY:` comment, the public API is safe, and what the driver reports
//! is checked before it is used. The C layout of every structure is pinned in `abi_tests.rs`.

use std::ffi::{c_char, c_uint};
use std::sync::OnceLock;

use libloading::Library;

#[cfg(test)]
mod abi_tests;
mod ffi;

/// What one Vulkan device offers the DMABUF import, and whether it is the decoder's own device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportCapability {
    /// The Vulkan device's name (`VkPhysicalDeviceProperties::deviceName`).
    pub device: String,
    /// Its `apiVersion`, as (major, minor).
    pub api_version: (u32, u32),
    /// `VK_EXT_external_memory_dma_buf`: the device takes memory from a DMABUF file descriptor.
    pub dma_buf: bool,
    /// `VK_EXT_image_drm_format_modifier`: it takes the plane layout and the DRM format modifier
    /// the VA driver decoded with (what `SurfaceDmabuf` describes).
    pub format_modifiers: bool,
    /// The device's DRM render node, when it reports one (`VK_EXT_physical_device_drm`).
    pub drm_render_node: Option<(u32, u32)>,
    /// Whether that node is the VA display's own. None only when no VA node was given to
    /// [`import_capability`]; `Some(false)` when the device is on another node or reports none
    /// (it cannot be shown to be the same GPU, so [`ready`](Self::ready) declines rather than
    /// guessing).
    pub same_gpu: Option<bool>,
}

impl ImportCapability {
    /// Whether the compositor could take a VA-API DMABUF here on this device.
    pub fn ready(&self) -> bool {
        self.dma_buf && self.format_modifiers && self.same_gpu != Some(false)
    }
}

/// The device the probe chose and what it offers, or why there is none. `drm` is the VA display's
/// render node (`(major, minor)`, [`crate::vaapi::va::Display::render_node`]): pass None when there
/// is no display to match, and `same_gpu` stays None.
pub fn import_capability(drm: Option<(u32, u32)>) -> Result<ImportCapability, String> {
    let devices = devices().map_err(|e| format!("Vulkan: {e}"))?;
    // Prefer the device on the decoder's own node; otherwise the first one.
    let found = match drm {
        Some(node) => devices.iter().find(|d| d.drm_render_node == Some(node)).or(devices.first()),
        None => devices.first(),
    };
    let mut cap = found.ok_or("Vulkan: no physical device")?.clone();
    cap.same_gpu = drm.map(|node| cap.drm_render_node == Some(node));
    Ok(cap)
}

/// Whether the loaded Vulkan reports the two extensions the DMABUF import needs, on any device.
pub fn import_possible() -> bool {
    devices().is_ok_and(|d| d.iter().any(|c| c.dma_buf && c.format_modifiers))
}

fn devices() -> Result<&'static [ImportCapability], String> {
    static DEVICES: OnceLock<Result<Vec<ImportCapability>, String>> = OnceLock::new();
    DEVICES.get_or_init(enumerate).as_deref().map_err(Clone::clone)
}

/// Enumerate the physical devices once, with what each offers (probe and diagnostics).
fn enumerate() -> Result<Vec<ImportCapability>, String> {
    let api = api()?;
    let app = ffi::VkApplicationInfo {
        s_type: ffi::VK_STRUCTURE_TYPE_APPLICATION_INFO,
        p_next: std::ptr::null(),
        p_application_name: c"FilmCraft".as_ptr(),
        application_version: 0,
        p_engine_name: c"FilmCraft".as_ptr(),
        engine_version: 0,
        api_version: ffi::VK_API_VERSION_1_1,
    };
    let create = ffi::VkInstanceCreateInfo {
        s_type: ffi::VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        p_next: std::ptr::null(),
        flags: 0,
        p_application_info: &app,
        enabled_layer_count: 0,
        pp_enabled_layer_names: std::ptr::null(),
        enabled_extension_count: 0,
        pp_enabled_extension_names: std::ptr::null(),
    };
    let mut instance = std::ptr::null_mut();
    // SAFETY: `create` points at a live descriptor with an `app` info chain; no layers or instance
    // extensions are asked for, so nothing else is dereferenced. `instance` is a live output.
    let st = unsafe { (api.create_instance)(&create, std::ptr::null(), &mut instance) };
    if st != ffi::VK_SUCCESS {
        return Err(format!("vkCreateInstance: {st}"));
    }
    if instance.is_null() {
        return Err("vkCreateInstance returned a null instance".into());
    }
    // From here on the instance is destroyed on every path (an uninitialised one cannot be).
    let out = physical_devices(api, instance);
    // SAFETY: `instance` came from vkCreateInstance above and is destroyed exactly once, here.
    unsafe { (api.destroy_instance)(instance, std::ptr::null()) };
    out
}

/// Most physical devices read (a machine has a handful; the count is the driver's).
const MAX_DEVICES: c_uint = 64;
/// Most device extensions read (drivers list a few hundred).
const MAX_EXTENSIONS: c_uint = 4096;

fn physical_devices(api: &'static ffi::Api, instance: ffi::VkInstance) -> Result<Vec<ImportCapability>, String> {
    let mut count: c_uint = 0;
    // SAFETY: `instance` is a live instance; a null array with a live count is the documented way
    // to ask how many devices there are.
    let st = unsafe { (api.enumerate_physical_devices)(instance, &mut count, std::ptr::null_mut()) };
    if st != ffi::VK_SUCCESS {
        return Err(format!("vkEnumeratePhysicalDevices: {st}"));
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    // (the count is the driver's: the array is capped, and a capped one reads as VK_INCOMPLETE)
    let mut got = count.min(MAX_DEVICES);
    let n = got as usize;
    let mut raw = vec![std::ptr::null_mut(); n];
    // SAFETY: `raw` has room for `got` handles; the driver writes at most that many and reports
    // how many it wrote in `got`.
    let st = unsafe { (api.enumerate_physical_devices)(instance, &mut got, raw.as_mut_ptr()) };
    if st != ffi::VK_SUCCESS && st != ffi::VK_INCOMPLETE {
        return Err(format!("vkEnumeratePhysicalDevices: {st}"));
    }
    let got = usize::try_from(got).map_err(|_| "vkEnumeratePhysicalDevices: absurd device count")?.min(n);
    let mut out = Vec::with_capacity(got);
    for dev in raw.into_iter().take(got) {
        if dev.is_null() {
            continue;
        }
        out.push(one_device(api, dev));
    }
    Ok(out)
}

fn one_device(api: &'static ffi::Api, dev: ffi::VkPhysicalDevice) -> ImportCapability {
    let mut props: ffi::VkPhysicalDeviceProperties = Default::default();
    // SAFETY: `dev` is one of the handles just enumerated; `props` is a live, correctly sized
    // `VkPhysicalDeviceProperties` (abi_tests.rs pins its layout).
    unsafe { (api.get_physical_device_properties)(dev, &mut props) };
    let extensions = device_extensions(api, dev);
    let dma_buf = extensions.iter().any(|e| e == ffi::EXT_EXTERNAL_MEMORY_DMA_BUF);
    let format_modifiers = extensions.iter().any(|e| e == ffi::EXT_IMAGE_DRM_FORMAT_MODIFIER);
    let drm_render_node = if extensions.iter().any(|e| e == ffi::EXT_PHYSICAL_DEVICE_DRM) { render_node(api, dev) } else { None };
    ImportCapability {
        device: c_string(&props.device_name),
        api_version: (ffi::version_major(props.api_version), ffi::version_minor(props.api_version)),
        dma_buf,
        format_modifiers,
        drm_render_node,
        same_gpu: None,
    }
}

/// The device's DRM render node (`VK_EXT_physical_device_drm`), when it reports one.
fn render_node(api: &'static ffi::Api, dev: ffi::VkPhysicalDevice) -> Option<(u32, u32)> {
    let mut drm = ffi::VkPhysicalDeviceDrmPropertiesEXT {
        s_type: ffi::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRM_PROPERTIES_EXT,
        p_next: std::ptr::null_mut(),
        has_primary: 0,
        has_render: 0,
        primary_major: 0,
        primary_minor: 0,
        render_major: 0,
        render_minor: 0,
    };
    let mut props = ffi::VkPhysicalDeviceProperties2 {
        s_type: ffi::VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2,
        p_next: (&raw mut drm).cast(),
        properties: Default::default(),
    };
    // SAFETY: `dev` is a live physical device; `props` is a live `VkPhysicalDeviceProperties2`
    // whose `pNext` chain is one live `VkPhysicalDeviceDrmPropertiesEXT` with the structure type
    // the extension defines (abi_tests.rs pins both layouts).
    unsafe { (api.get_physical_device_properties2)(dev, &mut props) };
    if drm.has_render == 0 {
        return None;
    }
    let (major, minor) = (drm.render_major, drm.render_minor);
    // the driver reports the kernel's dev_t; a node that does not fit our range is not one
    Some((u32::try_from(major).ok()?, u32::try_from(minor).ok()?))
}

fn device_extensions(api: &'static ffi::Api, dev: ffi::VkPhysicalDevice) -> Vec<String> {
    let mut count: c_uint = 0;
    // SAFETY: `dev` is a live device; a null array with a live count asks how many there are.
    let st = unsafe { (api.enumerate_device_extension_properties)(dev, std::ptr::null(), &mut count, std::ptr::null_mut()) };
    if st != ffi::VK_SUCCESS || count == 0 {
        return Vec::new();
    }
    // (the count is the driver's: the array is capped, and a capped one reads as VK_INCOMPLETE)
    count = count.min(MAX_EXTENSIONS);
    let mut props = vec![ffi::VkExtensionProperties::default(); count as usize];
    // SAFETY: `props` has room for `count` entries; the driver writes at most that many and
    // reports how many it wrote in `count`.
    let st = unsafe { (api.enumerate_device_extension_properties)(dev, std::ptr::null(), &mut count, props.as_mut_ptr()) };
    if st != ffi::VK_SUCCESS && st != ffi::VK_INCOMPLETE {
        return Vec::new();
    }
    props.iter().take(count as usize).map(|p| c_string(&p.extension_name)).collect()
}

/// A NUL-terminated fixed-size C string, lossily decoded.
fn c_string(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().map(|c| *c as u8).collect();
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

static API: OnceLock<Result<ffi::Api, String>> = OnceLock::new();

fn api() -> Result<&'static ffi::Api, String> {
    API.get_or_init(load).as_ref().map_err(Clone::clone)
}

/// Load `libvulkan.so.1` and its entry points (the only ones this probe needs).
fn load() -> Result<ffi::Api, String> {
    // SAFETY: loading Vulkan runs its library initialisers, which set up Vulkan's own state; it is
    // the system's Vulkan loader, loaded the way every client loads it.
    let lib = unsafe { Library::new("libvulkan.so.1") }.map_err(|e| format!("libvulkan.so.1: {e}"))?;
    macro_rules! sym {
        ($name:ident) => {{
            // SAFETY: `ffi::$name` is the C signature of the Vulkan function of that name,
            // transcribed from vulkan.h and checked against it (abi_tests.rs). The pointer is
            // copied out; the library stays loaded in `ffi::Api` for the life of the process.
            let s = unsafe { lib.get::<ffi::$name>(concat!(stringify!($name), "\0").as_bytes()) };
            *s.map_err(|e| format!("{}: {e}", stringify!($name)))?
        }};
    }
    let api = ffi::Api {
        create_instance: sym!(vkCreateInstance),
        destroy_instance: sym!(vkDestroyInstance),
        enumerate_physical_devices: sym!(vkEnumeratePhysicalDevices),
        get_physical_device_properties: sym!(vkGetPhysicalDeviceProperties),
        enumerate_device_extension_properties: sym!(vkEnumerateDeviceExtensionProperties),
        // Vulkan 1.1, where this is core: without it a device cannot be shown to be the decoder's
        // own, so the probe declines rather than guessing (the readback stays the path).
        get_physical_device_properties2: sym!(vkGetPhysicalDeviceProperties2),
        _lib: lib,
    };
    Ok(api)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe finds a Vulkan device and says what it offers. Skips when there is no loader or
    /// no device (a container, a headless builder).
    #[test]
    fn probe_reports_the_devices_capability() {
        let Ok(caps) = devices() else {
            eprintln!("no Vulkan loader or device, skipping");
            return;
        };
        assert!(!caps.is_empty(), "a loader with no device");
        for c in caps {
            assert!(!c.device.is_empty(), "{c:?}");
            assert!(c.api_version.0 >= 1, "{c:?}");
            eprintln!(
                "vulkan: {} api {}.{} dma_buf={} modifiers={} node={:?}",
                c.device, c.api_version.0, c.api_version.1, c.dma_buf, c.format_modifiers, c.drm_render_node
            );
        }
        // the combined answer, which is what the compositor would ask
        let cap = import_capability(None).expect("a device was just enumerated");
        eprintln!("import capability: {cap:?} (ready {})", cap.ready());
        assert_eq!(cap.same_gpu, None, "no node to match without a display");
    }

    /// With a VA-API display, the device on its render node is the one the import would use. Skips
    /// without a driver.
    #[test]
    fn the_decoders_render_node_matches_a_vulkan_device() {
        let Ok(display) = crate::vaapi::va::Display::open() else {
            eprintln!("no VA-API driver, skipping");
            return;
        };
        let Some(node) = display.render_node() else {
            eprintln!("the display does not report its render node, skipping");
            return;
        };
        let Ok(caps) = devices() else {
            eprintln!("no Vulkan loader or device, skipping");
            return;
        };
        for c in caps {
            if c.drm_render_node == Some(node) {
                eprintln!("{} is the decoder's device (node {node:?})", c.device);
            }
        }
        let cap = import_capability(Some(node)).expect("a device was just enumerated");
        // the capability is reported for the matched device, whatever it is
        eprintln!("decoder node {node:?} → {cap:?}");
        assert!(cap.same_gpu.is_some(), "a node was asked for");
    }
}
