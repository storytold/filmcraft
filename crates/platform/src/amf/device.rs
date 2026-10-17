//! Device discovery and context handle creation on an AMD GPU for AMF sessions.

#![allow(unsafe_code)]

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::c_void;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
    use windows::Win32::Graphics::Direct3D11::{D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
    use windows::core::Interface;

    use super::super::ffi::AMD_VENDOR_ID;

    pub struct Device(ID3D11Device);

    // SAFETY: ID3D11Device is free-threaded and used behind synchronization in AMF.
    unsafe impl Send for Device {}
    // SAFETY: see above.
    unsafe impl Sync for Device {}

    impl Device {
        pub fn new() -> Result<Self, String> {
            amd_device().map(Self)
        }

        pub fn as_raw(&self) -> *mut c_void {
            self.0.as_raw()
        }

        pub fn has_hardware_device() -> bool {
            amd_device().is_ok()
        }
    }

    fn amd_device() -> Result<ID3D11Device, String> {
        // SAFETY: COM and D3D calls with valid out-pointers; adapter outlives D3D11CreateDevice call.
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("no DXGI factory: {e}"))?;
            let mut i = 0;
            while let Ok(adapter) = factory.EnumAdapters1(i) {
                i += 1;
                let Ok(desc) = adapter.GetDesc1() else { continue };
                if desc.VendorId != AMD_VENDOR_ID {
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
        Err("no AMD adapter found".into())
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use std::ffi::c_void;

    pub struct Device;

    impl Device {
        pub fn new() -> Result<Self, String> {
            if Self::has_hardware_device() { Ok(Device) } else { Err("no AMD GPU adapter found on Linux".into()) }
        }

        pub fn as_raw(&self) -> *mut c_void {
            std::ptr::null_mut()
        }

        pub fn has_hardware_device() -> bool {
            if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
                for entry in entries.flatten() {
                    let vendor_path = entry.path().join("device/vendor");
                    if let Ok(vendor) = std::fs::read_to_string(vendor_path) {
                        if vendor.trim().eq_ignore_ascii_case("0x1002") {
                            return true;
                        }
                    }
                }
            }
            false
        }
    }
}

pub use imp::Device;
