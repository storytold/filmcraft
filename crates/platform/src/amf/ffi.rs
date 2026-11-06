//! AMD Advanced Media Framework (AMF) runtime FFI definitions.
//!
//! Clean-room implementation based on AMD's open-source AMF SDK headers (MIT license).
//! `amfrt64.dll` is loaded dynamically at runtime; no AMD SDK is linked.

#![allow(dead_code)]

use std::ffi::c_void;
use std::sync::OnceLock;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::HMODULE;
#[cfg(target_os = "windows")]
use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
#[cfg(target_os = "windows")]
use windows::core::{PCSTR, w};

#[cfg(not(target_os = "windows"))]
use libloading::Library;

pub const AMF_OK: i32 = 0;
pub const AMF_FAIL: i32 = 1;
pub const AMF_EOF: i32 = 23;
pub const AMF_REPEAT: i32 = 24;
pub const AMF_INPUT_FULL: i32 = 25;
pub const AMF_NOT_SUPPORTED: i32 = 10;

pub const AMF_MEMORY_HOST: u32 = 1;
pub const AMF_MEMORY_DX11: u32 = 3;

pub const AMF_SURFACE_NV12: u32 = 1;
pub const AMF_SURFACE_P010: u32 = 10;
pub const AMF_SURFACE_RGBA: u32 = 5;

pub const AMF_DX11_1: u32 = 111;

pub const AMF_VARIANT_BOOL: i32 = 1;
pub const AMF_VARIANT_INT64: i32 = 2;
pub const AMF_VARIANT_DOUBLE: i32 = 3;
pub const AMF_VARIANT_RECT: i32 = 4;
pub const AMF_VARIANT_SIZE: i32 = 5;
pub const AMF_VARIANT_POINT: i32 = 6;
pub const AMF_VARIANT_RATE: i32 = 7;
pub const AMF_VARIANT_RATIO: i32 = 8;

// H.264 Output Data Type (AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_ENUM)
pub const AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_IDR: i64 = 0;
pub const AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_I: i64 = 1;
pub const AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_P: i64 = 2;
pub const AMF_VIDEO_ENCODER_OUTPUT_DATA_TYPE_B: i64 = 3;

// HEVC Output Data Type (AMF_VIDEO_ENCODER_HEVC_OUTPUT_DATA_TYPE_ENUM)
pub const AMF_VIDEO_ENCODER_HEVC_OUTPUT_DATA_TYPE_IDR: i64 = 0;
pub const AMF_VIDEO_ENCODER_HEVC_OUTPUT_DATA_TYPE_I: i64 = 1;
pub const AMF_VIDEO_ENCODER_HEVC_OUTPUT_DATA_TYPE_P: i64 = 2;

// AV1 Output Frame Type (AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_ENUM)
pub const AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_KEY: i64 = 0;
pub const AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_INTRA_ONLY: i64 = 1;
pub const AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_INTER: i64 = 2;
pub const AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_SWITCH: i64 = 3;
pub const AMF_VIDEO_ENCODER_AV1_OUTPUT_FRAME_TYPE_SHOW_EXISTING: i64 = 4;

pub const AMD_VENDOR_ID: u32 = 0x1002;

pub const AMF_VIDEO_ENCODER_VCE_AVC: windows::core::PCWSTR = windows::core::w!("AMFVideoEncoderVCE_AVC");
pub const AMF_VIDEO_ENCODER_HW_HEVC: windows::core::PCWSTR = windows::core::w!("AMFVideoEncoderHW_HEVC");
pub const AMF_VIDEO_ENCODER_HW_AV1: windows::core::PCWSTR = windows::core::w!("AMFVideoEncoderHW_AV1");

pub type AmfInitFn = unsafe extern "C" fn(u64, *mut *mut AmfFactory) -> i32;
pub type AmfQueryVersionFn = unsafe extern "C" fn(*mut u64) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AmfVariant {
    pub var_type: i32,
    pub _pad: u32,
    pub val: [u8; 16],
}

impl AmfVariant {
    pub fn int64(v: i64) -> Self {
        let mut s = Self { var_type: AMF_VARIANT_INT64, _pad: 0, val: [0; 16] };
        s.val[..8].copy_from_slice(&v.to_ne_bytes());
        s
    }

    pub fn bool_val(v: bool) -> Self {
        let mut s = Self { var_type: AMF_VARIANT_BOOL, _pad: 0, val: [0; 16] };
        s.val[0] = if v { 1 } else { 0 };
        s
    }

    pub fn rate(num: u32, den: u32) -> Self {
        let mut s = Self { var_type: AMF_VARIANT_RATE, _pad: 0, val: [0; 16] };
        s.val[..4].copy_from_slice(&num.to_ne_bytes());
        s.val[4..8].copy_from_slice(&den.to_ne_bytes());
        s
    }

    pub fn ratio(num: u32, den: u32) -> Self {
        let mut s = Self { var_type: AMF_VARIANT_RATIO, _pad: 0, val: [0; 16] };
        s.val[..4].copy_from_slice(&num.to_ne_bytes());
        s.val[4..8].copy_from_slice(&den.to_ne_bytes());
        s
    }

    pub fn size(width: i32, height: i32) -> Self {
        let mut s = Self { var_type: AMF_VARIANT_SIZE, _pad: 0, val: [0; 16] };
        s.val[..4].copy_from_slice(&width.to_ne_bytes());
        s.val[4..8].copy_from_slice(&height.to_ne_bytes());
        s
    }

    pub fn as_int64(&self) -> Option<i64> {
        if self.var_type == AMF_VARIANT_INT64 {
            let bytes = <[u8; 8]>::try_from(&self.val[..8]).ok()?;
            Some(i64::from_ne_bytes(bytes))
        } else {
            None
        }
    }
}

#[repr(C)]
pub struct AmfFactoryVtbl {
    pub create_context: unsafe extern "system" fn(*mut AmfFactory, *mut *mut AmfContext) -> i32,
    pub create_component: unsafe extern "system" fn(*mut AmfFactory, *mut AmfContext, *const u16, *mut *mut AmfComponent) -> i32,
    pub set_cache_folder: *const c_void,
    pub get_cache_folder: *const c_void,
    pub get_debug: *const c_void,
    pub get_trace: *const c_void,
    pub get_programs: *const c_void,
}

#[repr(C)]
pub struct AmfFactory {
    pub vtbl: *const AmfFactoryVtbl,
}

#[repr(C)]
pub struct AmfContextVtbl {
    pub acquire: *const c_void,
    pub release: unsafe extern "system" fn(*mut AmfContext) -> i32,
    pub query_interface: *const c_void,

    pub set_property: *const c_void,
    pub get_property: *const c_void,
    pub has_property: *const c_void,
    pub get_property_count: *const c_void,
    pub get_property_at: *const c_void,
    pub clear: *const c_void,
    pub add_to: *const c_void,
    pub copy_to: *const c_void,
    pub add_observer: *const c_void,
    pub remove_observer: *const c_void,

    pub terminate: unsafe extern "system" fn(*mut AmfContext) -> i32,

    pub init_dx9: *const c_void,
    pub get_dx9_device: *const c_void,
    pub lock_dx9: *const c_void,
    pub unlock_dx9: *const c_void,

    pub init_dx11: unsafe extern "system" fn(*mut AmfContext, *mut c_void, u32) -> i32,
    pub get_dx11_device: *const c_void,
    pub lock_dx11: *const c_void,
    pub unlock_dx11: *const c_void,

    pub init_opencl: *const c_void,
    pub get_opencl_context: *const c_void,
    pub get_opencl_command_queue: *const c_void,
    pub get_opencl_device_id: *const c_void,
    pub get_opencl_compute_factory: *const c_void,
    pub init_opencl_ex: *const c_void,
    pub lock_opencl: *const c_void,
    pub unlock_opencl: *const c_void,

    pub init_opengl: *const c_void,
    pub get_opengl_context: *const c_void,
    pub get_opengl_drawable: *const c_void,
    pub lock_opengl: *const c_void,
    pub unlock_opengl: *const c_void,

    pub init_xv: *const c_void,
    pub get_xv_device: *const c_void,
    pub lock_xv: *const c_void,
    pub unlock_xv: *const c_void,

    pub init_gralloc: *const c_void,
    pub get_gralloc_device: *const c_void,
    pub lock_gralloc: *const c_void,
    pub unlock_gralloc: *const c_void,

    pub alloc_buffer: unsafe extern "system" fn(*mut AmfContext, u32, usize, *mut *mut AmfBuffer) -> i32,
    pub alloc_surface: unsafe extern "system" fn(*mut AmfContext, u32, u32, i32, i32, *mut *mut AmfSurface) -> i32,
}

#[repr(C)]
pub struct AmfContext {
    pub vtbl: *const AmfContextVtbl,
}

#[repr(C)]
pub struct AmfComponentVtbl {
    pub acquire: *const c_void,
    pub release: unsafe extern "system" fn(*mut AmfComponent) -> i32,
    pub query_interface: *const c_void,

    pub set_property: unsafe extern "system" fn(*mut AmfComponent, *const u16, AmfVariant) -> i32,
    pub get_property: unsafe extern "system" fn(*mut AmfComponent, *const u16, *mut AmfVariant) -> i32,
    pub has_property: *const c_void,
    pub get_property_count: *const c_void,
    pub get_property_at: *const c_void,
    pub clear: *const c_void,
    pub add_to: *const c_void,
    pub copy_to: *const c_void,
    pub add_observer: *const c_void,
    pub remove_observer: *const c_void,

    pub get_properties_info_count: *const c_void,
    pub get_property_info_at: *const c_void,
    pub get_property_info: *const c_void,
    pub validate_property: *const c_void,

    pub init: unsafe extern "system" fn(*mut AmfComponent, u32, i32, i32) -> i32,
    pub reinit: *const c_void,
    pub terminate: unsafe extern "system" fn(*mut AmfComponent) -> i32,
    pub drain: unsafe extern "system" fn(*mut AmfComponent) -> i32,
    pub flush: unsafe extern "system" fn(*mut AmfComponent) -> i32,
    pub submit_input: unsafe extern "system" fn(*mut AmfComponent, *mut c_void) -> i32,
    pub query_output: unsafe extern "system" fn(*mut AmfComponent, *mut *mut c_void) -> i32,
}

#[repr(C)]
pub struct AmfComponent {
    pub vtbl: *const AmfComponentVtbl,
}

#[repr(C)]
pub struct AmfPlaneVtbl {
    pub acquire: *const c_void,
    pub release: unsafe extern "system" fn(*mut AmfPlane) -> i32,
    pub query_interface: *const c_void,

    pub get_type: *const c_void,
    pub get_native: unsafe extern "system" fn(*mut AmfPlane) -> *mut u8,
    pub get_pixel_size_in_bytes: *const c_void,
    pub get_offset_x: *const c_void,
    pub get_offset_y: *const c_void,
    pub get_width: *const c_void,
    pub get_height: *const c_void,
    pub get_hpitch: unsafe extern "system" fn(*mut AmfPlane) -> i32,
    pub get_vpitch: unsafe extern "system" fn(*mut AmfPlane) -> i32,
    pub is_tiled: *const c_void,
}

#[repr(C)]
pub struct AmfPlane {
    pub vtbl: *const AmfPlaneVtbl,
}

#[repr(C)]
pub struct AmfSurfaceVtbl {
    pub acquire: *const c_void,
    pub release: unsafe extern "system" fn(*mut AmfSurface) -> i32,
    pub query_interface: *const c_void,

    pub set_property: *const c_void,
    pub get_property: *const c_void,
    pub has_property: *const c_void,
    pub get_property_count: *const c_void,
    pub get_property_at: *const c_void,
    pub clear: *const c_void,
    pub add_to: *const c_void,
    pub copy_to: *const c_void,
    pub add_observer: *const c_void,
    pub remove_observer: *const c_void,

    pub get_memory_type: *const c_void,
    pub duplicate: *const c_void,
    pub convert: *const c_void,
    pub interop: *const c_void,
    pub get_data_type: *const c_void,
    pub is_reusable: *const c_void,
    pub set_pts: unsafe extern "system" fn(*mut AmfSurface, i64),
    pub get_pts: unsafe extern "system" fn(*mut AmfSurface) -> i64,
    pub set_duration: unsafe extern "system" fn(*mut AmfSurface, i64),
    pub get_duration: unsafe extern "system" fn(*mut AmfSurface) -> i64,

    pub get_format: *const c_void,
    pub get_planes_count: *const c_void,
    pub get_plane_at: unsafe extern "system" fn(*mut AmfSurface, usize) -> *mut AmfPlane,
}

#[repr(C)]
pub struct AmfSurface {
    pub vtbl: *const AmfSurfaceVtbl,
}

#[repr(C)]
pub struct AmfBufferVtbl {
    pub acquire: *const c_void,
    pub release: unsafe extern "system" fn(*mut AmfBuffer) -> i32,
    pub query_interface: *const c_void,

    pub set_property: *const c_void,
    pub get_property: unsafe extern "system" fn(*mut AmfBuffer, *const u16, *mut AmfVariant) -> i32,
    pub has_property: *const c_void,
    pub get_property_count: *const c_void,
    pub get_property_at: *const c_void,
    pub clear: *const c_void,
    pub add_to: *const c_void,
    pub copy_to: *const c_void,
    pub add_observer: *const c_void,
    pub remove_observer: *const c_void,

    pub get_memory_type: *const c_void,
    pub duplicate: *const c_void,
    pub convert: *const c_void,
    pub interop: *const c_void,
    pub get_data_type: *const c_void,
    pub is_reusable: *const c_void,
    pub set_pts: unsafe extern "system" fn(*mut AmfBuffer, i64),
    pub get_pts: unsafe extern "system" fn(*mut AmfBuffer) -> i64,
    pub set_duration: unsafe extern "system" fn(*mut AmfBuffer, i64),
    pub get_duration: unsafe extern "system" fn(*mut AmfBuffer) -> i64,

    pub set_size: *const c_void,
    pub get_size: unsafe extern "system" fn(*mut AmfBuffer) -> usize,
    pub get_native: unsafe extern "system" fn(*mut AmfBuffer) -> *mut u8,
}

#[repr(C)]
pub struct AmfBuffer {
    pub vtbl: *const AmfBufferVtbl,
}

pub struct AmfRuntime {
    #[cfg(target_os = "windows")]
    _module: HMODULE,
    #[cfg(not(target_os = "windows"))]
    _lib: Library,
    factory: *mut AmfFactory,
}

// SAFETY: AMFFactory is thread-safe and free-threaded across worker threads.
unsafe impl Send for AmfRuntime {}
// SAFETY: see above.
unsafe impl Sync for AmfRuntime {}

static RUNTIME: OnceLock<Result<AmfRuntime, String>> = OnceLock::new();

pub fn runtime() -> Result<&'static AmfRuntime, String> {
    RUNTIME.get_or_init(load_runtime).as_ref().map_err(Clone::clone)
}

#[cfg(target_os = "windows")]
fn load_runtime() -> Result<AmfRuntime, String> {
    // SAFETY: loads amfrt64.dll securely from system32.
    let _module = unsafe { LoadLibraryExW(w!("amfrt64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|e| format!("amfrt64.dll not available: {e}"))?;

    // SAFETY: resolves AMFInit exported symbol with expected signature.
    let p_init = unsafe { GetProcAddress(_module, PCSTR(c"AMFInit".as_ptr().cast())) }.ok_or_else(|| "amfrt64.dll lacks AMFInit symbol".to_string())?;

    // SAFETY: transmuting function pointer of known signature.
    let init_fn: AmfInitFn = unsafe { std::mem::transmute(p_init) };

    let mut factory = std::ptr::null_mut();
    // AMF_FULL_VERSION for 1.4.0: (1 << 48) | (4 << 32)
    let full_version = (1u64 << 48) | (4u64 << 32);
    // SAFETY: calling AMFInit with valid out-pointer.
    let res = unsafe { init_fn(full_version, &mut factory) };
    if res != AMF_OK || factory.is_null() {
        return Err(format!("AMFInit failed with code {res}"));
    }

    Ok(AmfRuntime { _module, factory })
}

#[cfg(not(target_os = "windows"))]
fn load_runtime() -> Result<AmfRuntime, String> {
    // SAFETY: loads libamfrt64.so.1 securely from system library search path.
    let lib = unsafe { Library::new("libamfrt64.so.1").or_else(|_| Library::new("libamfrt64.so")) }.map_err(|e| format!("libamfrt64.so not available: {e}"))?;

    // SAFETY: dereferencing function pointer symbol loaded from libamfrt64.
    let init_fn: AmfInitFn = unsafe {
        let sym = lib.get::<AmfInitFn>(b"AMFInit\0").map_err(|e| format!("AMFInit not found: {e}"))?;
        *sym
    };

    let mut factory = std::ptr::null_mut();
    let full_version = (1u64 << 48) | (4u64 << 32);
    // SAFETY: calling AMFInit with valid out-pointer.
    let res = unsafe { init_fn(full_version, &mut factory) };
    if res != AMF_OK || factory.is_null() {
        return Err(format!("AMFInit failed with code {res}"));
    }

    Ok(AmfRuntime { _lib: lib, factory })
}

impl AmfRuntime {
    pub fn factory(&self) -> *mut AmfFactory {
        self.factory
    }
}
