//! Devices passed to nvEncOpenEncodeSessionEx. Public APIs are safe; handles stay in this module.
use std::ffi::c_void;

#[cfg(target_os = "linux")]
pub use cuda::Device;
#[cfg(target_os = "windows")]
pub use directx::Device;

#[cfg(target_os = "windows")]
mod directx {
    use super::c_void;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::{D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
    use windows::core::Interface;

    /// NVIDIA's PCI vendor id.
    const NVIDIA: u32 = 0x10DE;

    pub struct Device(ID3D11Device);
    impl Device {
        pub const KIND: u32 = super::super::ffi::NV_ENC_DEVICE_TYPE_DIRECTX;
        pub fn new() -> Result<Self, String> {
            nvidia_device().map(Self)
        }
        pub fn as_raw(&self) -> *mut c_void {
            self.0.as_raw()
        }
    }
    /// A Direct3D 11 device on the first NVIDIA adapter (what an NVENC session is opened on).
    fn nvidia_device() -> Result<ID3D11Device, String> {
        // SAFETY: plain COM / D3D calls with valid out-pointers; the adapter outlives the call.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("no DXGI: {e}"))?;
            let mut i = 0;
            while let Ok(adapter) = factory.EnumAdapters1(i) {
                i += 1;
                let Ok(desc) = adapter.GetDesc1() else { continue };
                if desc.VendorId != NVIDIA {
                    continue;
                }
                let mut device = None;
                let adapter: IDXGIAdapter = adapter.cast().map_err(|e| e.to_string())?;
                if D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_FLAG(0),
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )
                .is_ok()
                    && let Some(d) = device
                {
                    return Ok(d);
                }
            }
        }
        Err("no NVIDIA adapter".into())
    }
}

#[cfg(target_os = "linux")]
mod cuda {
    use super::c_void;
    type Init = unsafe extern "C" fn(u32) -> i32;
    type GetDevice = unsafe extern "C" fn(*mut i32, i32) -> i32;
    type Retain = unsafe extern "C" fn(*mut *mut c_void, i32) -> i32;
    type Release = unsafe extern "C" fn(i32) -> i32;

    /// One retained primary CUDA context. Retaining does not change the calling thread's
    /// current context; NVENC receives the explicit context handle. No toolkit is needed.
    pub struct Device {
        context: *mut c_void,
        ordinal: i32,
        release: Release,
        _library: libloading::Library,
    }

    impl Device {
        pub const KIND: u32 = super::super::ffi::NV_ENC_DEVICE_TYPE_CUDA;
        pub fn new() -> Result<Self, String> {
            // SAFETY: CUDA driver API signatures from NVIDIA's public Driver API reference.
            // Resolve all symbols before retaining; keep the library alive until after release.
            unsafe {
                let library = libloading::Library::new("libcuda.so.1").map_err(|e| format!("no NVIDIA CUDA driver: {e}"))?;
                let init: libloading::Symbol<Init> = library.get(b"cuInit\0").map_err(|e| e.to_string())?;
                let get: libloading::Symbol<GetDevice> = library.get(b"cuDeviceGet\0").map_err(|e| e.to_string())?;
                let retain: libloading::Symbol<Retain> = library.get(b"cuDevicePrimaryCtxRetain\0").map_err(|e| e.to_string())?;
                let release: Release = *library.get(b"cuDevicePrimaryCtxRelease_v2\0").map_err(|e| e.to_string())?;
                let st = init(0);
                if st != 0 {
                    return Err(format!("cuInit failed ({st})"));
                }
                let mut ordinal = 0;
                let st = get(&mut ordinal, 0);
                if st != 0 {
                    return Err(format!("no CUDA device ({st})"));
                }
                let mut context = std::ptr::null_mut();
                let st = retain(&mut context, ordinal);
                if st != 0 {
                    return Err(format!("cuDevicePrimaryCtxRetain failed ({st})"));
                }
                if context.is_null() {
                    let _ = release(ordinal);
                    return Err("CUDA returned a null primary context".into());
                }
                Ok(Self { context, ordinal, release, _library: library })
            }
        }
        pub fn as_raw(&self) -> *mut c_void {
            self.context
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            // SAFETY: balances this object's successful retain, after the NVENC session is
            // destroyed. Release drops our reference, not other users' primary-context references.
            unsafe {
                (self.release)(self.ordinal);
            }
        }
    }
}
