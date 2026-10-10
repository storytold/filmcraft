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
    use std::marker::PhantomData;
    use std::rc::Rc;
    type Init = unsafe extern "C" fn(u32) -> i32;
    type GetDevice = unsafe extern "C" fn(*mut i32, i32) -> i32;
    type Retain = unsafe extern "C" fn(*mut *mut c_void, i32) -> i32;
    type Release = unsafe extern "C" fn(i32) -> i32;
    type Push = unsafe extern "C" fn(*mut c_void) -> i32;
    type Pop = unsafe extern "C" fn(*mut *mut c_void) -> i32;
    type CopyToHost = unsafe extern "C" fn(*mut c_void, u64, usize) -> i32;

    /// A thread-local context stack entry; it cannot move to another thread.
    pub struct Current<'a> {
        _device: &'a Device,
        pop: Pop,
        _thread: PhantomData<Rc<()>>,
    }

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

        pub fn push_current(&self) -> Result<Current<'_>, String> {
            // SAFETY: signatures match the CUDA driver headers; the retained context and
            // library outlive the guard, whose Drop balances the push on this same thread.
            unsafe {
                let push: Push = *self._library.get(b"cuCtxPushCurrent_v2\0").map_err(|e| e.to_string())?;
                let pop: Pop = *self._library.get(b"cuCtxPopCurrent_v2\0").map_err(|e| e.to_string())?;
                let st = push(self.context);
                if st != 0 {
                    return Err(format!("cuCtxPushCurrent failed ({st})"));
                }
                Ok(Current { _device: self, pop, _thread: PhantomData })
            }
        }

        /// Copy a driver-owned device address into a bounded host slice. CUDA validates
        /// the source device range; the destination is always a live writable Rust slice.
        pub fn copy_to_host(&self, source: u64, destination: &mut [u8]) -> Result<(), String> {
            let _current = self.push_current()?;
            source.checked_add(u64::try_from(destination.len()).map_err(|_| "CUDA copy length overflows")?).ok_or("CUDA copy address overflows")?;
            // SAFETY: the synchronous copy writes at most destination.len() bytes to its
            // live buffer. The CUDA context and library remain live through the call.
            let st = unsafe {
                let copy: CopyToHost = *self._library.get(b"cuMemcpyDtoH_v2\0").map_err(|e| e.to_string())?;
                copy(destination.as_mut_ptr().cast(), source, destination.len())
            };
            if st != 0 {
                return Err(format!("cuMemcpyDtoH failed ({st})"));
            }
            Ok(())
        }
    }

    impl Drop for Current<'_> {
        fn drop(&mut self) {
            let mut previous = std::ptr::null_mut();
            // SAFETY: this guard owns one successful push on this thread; previous is
            // a valid out-pointer and the library is retained by the borrowed device.
            let st = unsafe { (self.pop)(&mut previous) };
            if st != 0 {
                log::warn!("cuCtxPopCurrent failed ({st})");
            }
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            // SAFETY: balances this object's successful retain, after the NVENC session is
            // destroyed. Release drops our reference, not other users' primary-context references.
            unsafe {
                let st = (self.release)(self.ordinal);
                if st != 0 {
                    log::warn!("cuDevicePrimaryCtxRelease failed ({st})");
                }
            }
        }
    }
}
