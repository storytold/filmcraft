//! Intel GPU device discovery through DXGI on Windows and DRM sysfs on Linux.
//!
//! FFI module: every `unsafe` block has a `// SAFETY:` comment, and all public functions return `Result`.

#![allow(unsafe_code)]

/// Intel Corporation PCI vendor ID.
pub const INTEL_VENDOR_ID: u32 = 0x8086;

/// Information about a detected Intel GPU.
#[derive(Clone, Debug)]
pub struct IntelAdapterInfo {
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_video_memory: usize,
}

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};

    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    };
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIAdapter1, IDXGIFactory1};
    use windows::Win32::Media::MediaFoundation::IMFDXGIDeviceManager;
    use windows::core::Interface;

    use super::{INTEL_VENDOR_ID, IntelAdapterInfo};
    use crate::media_foundation::gpu::ensure_com;
    use crate::media_foundation::mft::{MfApi, api};

    /// Discovered Intel GPU adapter.
    pub struct IntelGpu {
        pub info: IntelAdapterInfo,
        pub adapter: IDXGIAdapter1,
        pub device: ID3D11Device,
        pub context: ID3D11DeviceContext,
        pub manager: IMFDXGIDeviceManager,
        lost: AtomicBool,
        _gate: Mutex<()>,
    }

    // SAFETY: ID3D11Device is multithread-protected via ID3D11Multithread,
    // IMFDXGIDeviceManager is free-threaded, and context access is gated.
    unsafe impl Send for IntelGpu {}
    // SAFETY: Access to D3D11 device and context is synchronized via ID3D11Multithread and _gate mutex.
    unsafe impl Sync for IntelGpu {}

    static INTEL_GPU: Mutex<Option<Arc<IntelGpu>>> = Mutex::new(None);

    impl IntelGpu {
        /// Retrieve or create the shared Intel GPU device.
        pub fn shared() -> Result<Arc<IntelGpu>, String> {
            ensure_com()?;
            let mut slot = INTEL_GPU.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(gpu) = slot.as_ref().filter(|g| g.usable()) {
                return Ok(gpu.clone());
            }
            let mf_api = api()?;
            let gpu = Arc::new(Self::create(mf_api)?);
            log::info!("Intel Quick Sync: using {} (device ID 0x{:04x})", gpu.info.name, gpu.info.device_id);
            *slot = Some(gpu.clone());
            Ok(gpu)
        }

        /// Check whether an Intel GPU is present on this system without allocating a full D3D device.
        pub fn probe() -> Result<Option<IntelAdapterInfo>, String> {
            ensure_com()?;
            // SAFETY: plain COM call to create the DXGI factory.
            let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| format!("cannot create DXGIFactory1: {e}"))?;
            let mut i = 0u32;
            loop {
                // SAFETY: EnumAdapters1 safely returns DXGI_ERROR_NOT_FOUND when index exceeds count.
                let adapter = match unsafe { factory.EnumAdapters1(i) } {
                    Ok(a) => a,
                    Err(_) => break,
                };
                i = i.saturating_add(1);
                // SAFETY: safe query on live IDXGIAdapter1.
                let desc = match unsafe { adapter.GetDesc1() } {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                if desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                    continue;
                }
                if desc.VendorId == INTEL_VENDOR_ID {
                    let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
                    let name = String::from_utf16_lossy(&desc.Description[..len]);
                    return Ok(Some(IntelAdapterInfo {
                        name,
                        vendor_id: desc.VendorId,
                        device_id: desc.DeviceId,
                        dedicated_video_memory: desc.DedicatedVideoMemory,
                    }));
                }
            }
            Ok(None)
        }

        fn create(api: &MfApi) -> Result<IntelGpu, String> {
            // SAFETY: plain COM call to create DXGIFactory1.
            let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| format!("cannot create DXGIFactory1: {e}"))?;
            let mut i = 0u32;
            let mut selected = None;
            loop {
                // SAFETY: EnumAdapters1 on a live factory.
                let adapter = match unsafe { factory.EnumAdapters1(i) } {
                    Ok(a) => a,
                    Err(_) => break,
                };
                i = i.saturating_add(1);
                // SAFETY: safe query on live IDXGIAdapter1.
                let desc = match unsafe { adapter.GetDesc1() } {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                if desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                    continue;
                }
                if desc.VendorId == INTEL_VENDOR_ID {
                    let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
                    let name = String::from_utf16_lossy(&desc.Description[..len]);
                    let info = IntelAdapterInfo { name, vendor_id: desc.VendorId, device_id: desc.DeviceId, dedicated_video_memory: desc.DedicatedVideoMemory };
                    selected = Some((adapter, info));
                    break;
                }
            }
            let (adapter, info) = selected.ok_or_else(|| "no Intel GPU adapter found".to_string())?;

            let adapter0: windows::Win32::Graphics::Dxgi::IDXGIAdapter = adapter.cast().map_err(|e| format!("cannot cast adapter: {e}"))?;
            let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
            let (mut device, mut context) = (None, None);
            // SAFETY: creating D3D11 device on an explicit Intel adapter requires D3D_DRIVER_TYPE_UNKNOWN.
            unsafe {
                D3D11CreateDevice(
                    Some(&adapter0),
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                    Some(&levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            }
            .map_err(|e| format!("failed to create D3D11 device on Intel adapter: {e}"))?;

            let (device, context) = device.zip(context).ok_or("D3D11CreateDevice returned no device")?;
            let mt: ID3D11Multithread = device.cast().map_err(|e| format!("device lacks ID3D11Multithread: {e}"))?;
            // SAFETY: set multithread protection on COM interface.
            let _ = unsafe { mt.SetMultithreadProtected(true) };
            let manager = api.create_device_manager(&device)?;

            Ok(IntelGpu { info, adapter, device, context, manager, lost: AtomicBool::new(false), _gate: Mutex::new(()) })
        }

        pub fn usable(&self) -> bool {
            // SAFETY: GetDeviceRemovedReason is a safe COM query on live device.
            !self.lost.load(Ordering::Relaxed) && unsafe { self.device.GetDeviceRemovedReason() }.is_ok()
        }

        pub fn manager_param(&self) -> usize {
            self.manager.as_raw() as usize
        }

        pub fn note_failure(&self) {
            if !self.usable() {
                log::warn!("Intel D3D11 device lost ({}); resetting", self.info.name);
                self.lost.store(true, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::{INTEL_VENDOR_ID, IntelAdapterInfo};
    use std::sync::Arc;

    pub struct IntelGpu {
        pub info: IntelAdapterInfo,
    }

    impl IntelGpu {
        pub fn shared() -> Result<Arc<IntelGpu>, String> {
            if let Some(info) = Self::probe()? { Ok(Arc::new(IntelGpu { info })) } else { Err("no Intel GPU adapter found on Linux".into()) }
        }

        pub fn probe() -> Result<Option<IntelAdapterInfo>, String> {
            if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let vendor_path = path.join("device/vendor");
                    if let Ok(vendor) = std::fs::read_to_string(vendor_path) {
                        if vendor.trim().eq_ignore_ascii_case("0x8086") {
                            let device_id = std::fs::read_to_string(path.join("device/device"))
                                .ok()
                                .and_then(|s| u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok())
                                .unwrap_or(0);
                            return Ok(Some(IntelAdapterInfo {
                                name: "Intel Graphics (Linux DRM/VA-API)".into(),
                                vendor_id: INTEL_VENDOR_ID,
                                device_id,
                                dedicated_video_memory: 0,
                            }));
                        }
                    }
                }
            }
            Ok(None)
        }

        pub fn usable(&self) -> bool {
            true
        }

        pub fn manager_param(&self) -> usize {
            0
        }

        pub fn note_failure(&self) {}
    }
}

pub use imp::IntelGpu;
