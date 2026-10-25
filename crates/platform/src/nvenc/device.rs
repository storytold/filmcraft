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
    use std::cell::{Cell, RefCell};
    use std::marker::PhantomData;
    use std::ptr::NonNull;
    use std::rc::Rc;
    type Init = unsafe extern "C" fn(u32) -> i32;
    type GetDevice = unsafe extern "C" fn(*mut i32, i32) -> i32;
    type Retain = unsafe extern "C" fn(*mut *mut c_void, i32) -> i32;
    type Release = unsafe extern "C" fn(i32) -> i32;
    type Push = unsafe extern "C" fn(*mut c_void) -> i32;
    type Pop = unsafe extern "C" fn(*mut *mut c_void) -> i32;
    type CopyToHost = unsafe extern "C" fn(*mut c_void, u64, usize) -> i32;
    type AllocHost = unsafe extern "C" fn(*mut *mut c_void, usize) -> i32;
    type FreeHost = unsafe extern "C" fn(*mut c_void) -> i32;
    type StreamCreate = unsafe extern "C" fn(*mut *mut c_void, u32) -> i32;
    type StreamDestroy = unsafe extern "C" fn(*mut c_void) -> i32;
    type StreamSynchronize = unsafe extern "C" fn(*mut c_void) -> i32;
    type CopyToHostAsync = unsafe extern "C" fn(*mut c_void, u64, usize, *mut c_void) -> i32;

    /// A thread-local context stack entry; it cannot move to another thread.
    pub struct Current<'a> {
        _device: &'a Device,
        pop: Pop,
        _thread: PhantomData<Rc<()>>,
    }

    /// Page-locked host memory (`cuMemAllocHost`). The GPU copies into it directly; a copy into
    /// ordinary memory goes through the driver's own staging buffer and pins the destination's
    /// pages on the way, which took most of a 1080p picture's readback time. Zeroed when
    /// allocated, so every byte is initialised.
    struct Pinned {
        ptr: NonNull<u8>,
        len: usize,
    }

    /// `cuStreamCreate`, `cuStreamDestroy_v2`, `cuStreamSynchronize`, `cuMemcpyDtoHAsync_v2`.
    #[derive(Clone, Copy)]
    struct StreamApi {
        create: StreamCreate,
        destroy: StreamDestroy,
        synchronize: StreamSynchronize,
        copy_async: CopyToHostAsync,
    }

    /// `CU_STREAM_NON_BLOCKING`: the stream does not wait for work on the legacy default stream.
    const STREAM_NON_BLOCKING: u32 = 1;

    /// One retained primary CUDA context. Retaining does not change the calling thread's
    /// current context; NVENC receives the explicit context handle. No toolkit is needed.
    pub struct Device {
        context: *mut c_void,
        ordinal: i32,
        release: Release,
        push: Push,
        pop: Pop,
        copy: CopyToHost,
        /// `cuMemAllocHost_v2` / `cuMemFreeHost`, when the driver has them.
        host: Option<(AllocHost, FreeHost)>,
        /// The page-locked buffer [`Device::with_host_copy`] copies into, kept from one copy to the next.
        staging: RefCell<Option<Pinned>>,
        /// Page-locked memory could not be had: copies go to ordinary memory from then on.
        no_pinned: Cell<bool>,
        streams: Option<StreamApi>,
        /// This device's non-blocking stream ([`Device::readback_stream`]), made on first use.
        stream: Cell<Option<NonNull<c_void>>>,
        /// The stream could not be made: the legacy default stream from then on.
        no_stream: Cell<bool>,
        _library: libloading::Library,
    }

    impl Device {
        pub const KIND: u32 = super::super::ffi::NV_ENC_DEVICE_TYPE_CUDA;
        /// Load the CUDA driver, resolve the functions used here and retain device 0's primary
        /// context; an error without a driver or device.
        pub fn new() -> Result<Self, String> {
            // SAFETY: CUDA driver API signatures from NVIDIA's public Driver API reference.
            // Resolve all symbols before retaining; keep the library alive until after release
            // (the function pointers are only called while `_library` is loaded).
            unsafe {
                let library = libloading::Library::new("libcuda.so.1").map_err(|e| format!("no NVIDIA CUDA driver: {e}"))?;
                let init: libloading::Symbol<Init> = library.get(b"cuInit\0").map_err(|e| e.to_string())?;
                let get: libloading::Symbol<GetDevice> = library.get(b"cuDeviceGet\0").map_err(|e| e.to_string())?;
                let retain: libloading::Symbol<Retain> = library.get(b"cuDevicePrimaryCtxRetain\0").map_err(|e| e.to_string())?;
                let release: Release = *library.get(b"cuDevicePrimaryCtxRelease_v2\0").map_err(|e| e.to_string())?;
                let push: Push = *library.get(b"cuCtxPushCurrent_v2\0").map_err(|e| e.to_string())?;
                let pop: Pop = *library.get(b"cuCtxPopCurrent_v2\0").map_err(|e| e.to_string())?;
                let copy: CopyToHost = *library.get(b"cuMemcpyDtoH_v2\0").map_err(|e| e.to_string())?;
                let alloc_host = library.get::<AllocHost>(b"cuMemAllocHost_v2\0").ok().map(|s| *s);
                let free_host = library.get::<FreeHost>(b"cuMemFreeHost\0").ok().map(|s| *s);
                let streams = (|| {
                    Some(StreamApi {
                        create: *library.get::<StreamCreate>(b"cuStreamCreate\0").ok()?,
                        destroy: *library.get::<StreamDestroy>(b"cuStreamDestroy_v2\0").ok()?,
                        synchronize: *library.get::<StreamSynchronize>(b"cuStreamSynchronize\0").ok()?,
                        copy_async: *library.get::<CopyToHostAsync>(b"cuMemcpyDtoHAsync_v2\0").ok()?,
                    })
                })();
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
                Ok(Self {
                    context,
                    ordinal,
                    release,
                    push,
                    pop,
                    copy,
                    host: alloc_host.zip(free_host),
                    staging: RefCell::new(None),
                    no_pinned: Cell::new(false),
                    streams,
                    stream: Cell::new(None),
                    no_stream: Cell::new(false),
                    _library: library,
                })
            }
        }
        /// The retained context, the handle NVENC sessions are opened with.
        pub fn as_raw(&self) -> *mut c_void {
            self.context
        }

        /// Make this context current on the calling thread until the guard is dropped.
        pub fn push_current(&self) -> Result<Current<'_>, String> {
            // SAFETY: signatures match the CUDA driver headers; the retained context and
            // library outlive the guard, whose Drop balances the push on this same thread.
            let st = unsafe { (self.push)(self.context) };
            if st != 0 {
                return Err(format!("cuCtxPushCurrent failed ({st})"));
            }
            Ok(Current { _device: self, pop: self.pop, _thread: PhantomData })
        }

        /// This device's non-blocking stream (made on first use), or null, the legacy default
        /// stream, where it cannot be made. Work on the default stream waits for the work of every
        /// blocking stream of the context, which on the primary context NVDEC shares with NVENC
        /// includes the encoder's: a decoded picture's post-processing and readback on this
        /// stream do not wait for pictures being encoded. The context must be current.
        pub fn readback_stream(&self) -> *mut c_void {
            if let Some(stream) = self.stream.get() {
                return stream.as_ptr();
            }
            let Some(api) = self.streams.filter(|_| !self.no_stream.get()) else { return std::ptr::null_mut() };
            let mut stream = std::ptr::null_mut();
            // SAFETY: a valid out-pointer and flag; the caller made the context current.
            let st = unsafe { (api.create)(&mut stream, STREAM_NON_BLOCKING) };
            match NonNull::new(stream).filter(|_| st == 0) {
                Some(stream) => {
                    self.stream.set(Some(stream));
                    stream.as_ptr()
                }
                None => {
                    log::debug!("cuStreamCreate failed ({st}): reading back on the default stream");
                    self.no_stream.set(true);
                    std::ptr::null_mut()
                }
            }
        }

        /// Copy into ordinary memory from now on, as where the driver gives no page-locked memory
        /// (a test hook of the decoders).
        pub fn read_back_without_page_locked_memory(&self) {
            self.no_pinned.set(true);
            if let Some(pinned) = self.staging.try_borrow_mut().ok().and_then(|mut s| s.take()) {
                self.free_pinned(pinned);
            }
        }

        /// Wait for the readback stream (if there is one): every picture post-processing and copy
        /// queued on it is done when this returns Ok. An error means that could not be established:
        /// the GPU may still be reading a mapped picture or writing the staging buffer, so neither
        /// may be released or reused. The context must be current.
        pub fn settle_readback(&self) -> Result<(), String> {
            let (Some(stream), Some(api)) = (self.stream.get(), self.streams) else { return Ok(()) };
            // SAFETY: a stream this device created; the context is current and the library loaded.
            let st = unsafe { (api.synchronize)(stream.as_ptr()) };
            if st != 0 {
                return Err(format!("cuStreamSynchronize failed ({st})"));
            }
            Ok(())
        }

        /// Copy `len` bytes at the driver-owned device address `source` to the host and hand
        /// them to `read`. The bytes land in this device's page-locked staging buffer (grown as
        /// needed and kept for the next copy), on its [`Device::readback_stream`], or in ordinary
        /// memory where page-locked memory cannot be had. CUDA validates the source device range.
        /// Whatever the copy did, the readback stream is waited for before it is read; an error
        /// from that wait ([`Device::settle_readback`]) means the caller must not release the picture.
        pub fn with_host_copy<R>(&self, source: u64, len: usize, read: impl FnOnce(&[u8]) -> R) -> Result<R, String> {
            source.checked_add(u64::try_from(len).map_err(|_| "CUDA copy length overflows")?).ok_or("CUDA copy address overflows")?;
            let _current = self.push_current()?;
            let mut staging = self.staging.try_borrow_mut().map_err(|_| "CUDA staging buffer is already in use")?;
            if staging.as_ref().is_none_or(|p| p.len < len) && !self.no_pinned.get() {
                if let Some(old) = staging.take() {
                    self.free_pinned(old);
                }
                *staging = self.alloc_pinned(len);
                self.no_pinned.set(staging.is_none());
            }
            if let Some(pinned) = staging.as_ref().filter(|p| p.len >= len) {
                let stream = self.readback_stream();
                let copied = match self.streams.filter(|_| !stream.is_null()) {
                    // SAFETY: the copy writes `len <= pinned.len` bytes into the live page-locked
                    // allocation and is waited for (`settle_readback`) before anything reads, reuses
                    // or frees it; the stream is this device's, the context is current and the library loaded.
                    Some(api) => unsafe { (api.copy_async)(pinned.ptr.as_ptr().cast(), source, len, stream) },
                    // SAFETY: as above, a synchronous copy.
                    None => unsafe { (self.copy)(pinned.ptr.as_ptr().cast(), source, len) },
                };
                // wait even when the copy could not be queued: the stream holds the picture's
                // post-processing, and nothing may release the picture or the buffer under it
                if let Err(e) = self.settle_readback() {
                    // the GPU may still be writing into the buffer: never read, reuse or free it
                    // (forgetting the pointer leaks it); the next readback allocates another
                    *staging = None;
                    return Err(if copied != 0 { format!("CUDA readback failed ({copied}); {e}") } else { e });
                }
                if copied != 0 {
                    return Err(format!("CUDA readback failed ({copied})"));
                }
                // SAFETY: `pinned.ptr` addresses `pinned.len >= len` initialised bytes (zeroed when
                // allocated, overwritten by copies, the last one finished above) that this device
                // owns until it frees them in `with_host_copy` or `Drop`; the `RefCell` borrow keeps
                // them from being freed or written while `read` holds the slice.
                let bytes = unsafe { std::slice::from_raw_parts(pinned.ptr.as_ptr(), len) };
                return Ok(read(bytes));
            }
            // The picture's post-processing ran on the readback stream if there is one, and the
            // synchronous copy below (default stream) does not wait for a non-blocking stream: wait
            // for it before anything else, so no error below returns with it still running.
            self.settle_readback()?;
            let mut host = Vec::new();
            host.try_reserve_exact(len).map_err(|e| format!("CUDA readback allocation failed: {e}"))?;
            host.resize(len, 0);
            // SAFETY: the synchronous copy writes at most `host.len()` bytes into the live
            // vector; the context is current and the library loaded.
            let st = unsafe { (self.copy)(host.as_mut_ptr().cast(), source, host.len()) };
            if st != 0 {
                return Err(format!("cuMemcpyDtoH failed ({st})"));
            }
            Ok(read(&host))
        }

        /// `len` bytes of zeroed page-locked memory, or None (no driver support, out of memory,
        /// a zero length). The context must be current.
        fn alloc_pinned(&self, len: usize) -> Option<Pinned> {
            let (alloc, _) = self.host?;
            let mut ptr = std::ptr::null_mut();
            // SAFETY: a valid out-pointer and size; the context is current (the caller pushed it).
            let st = unsafe { alloc(&mut ptr, len) };
            let ptr = NonNull::new(ptr.cast::<u8>()).filter(|_| st == 0 && len > 0);
            let Some(ptr) = ptr else {
                log::debug!("cuMemAllocHost of {len} bytes failed ({st}): reading back into ordinary memory");
                return None;
            };
            // SAFETY: the driver returned `len` writable bytes at `ptr`.
            unsafe { std::ptr::write_bytes(ptr.as_ptr(), 0, len) };
            Some(Pinned { ptr, len })
        }

        /// Give a page-locked buffer back to the driver, with this context current; a failure is
        /// logged and the buffer leaked.
        fn free_pinned(&self, pinned: Pinned) {
            let Some((_, free)) = self.host else { return };
            match self.push_current() {
                Ok(_current) => {
                    // SAFETY: `pinned` came from `cuMemAllocHost` on this device's context and no
                    // slice of it is alive (the staging borrow is exclusive or the device is dropping).
                    let st = unsafe { free(pinned.ptr.as_ptr().cast()) };
                    if st != 0 {
                        log::warn!("cuMemFreeHost failed ({st})");
                    }
                }
                Err(e) => log::warn!("page-locked buffer not freed: {e}"),
            }
        }
    }

    impl Drop for Current<'_> {
        /// Pop the context this guard pushed.
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
        /// Wait for the readback stream, then destroy it and free the staging buffer while the
        /// context is still retained, then release the context. If the wait (or making the context
        /// current) fails, the GPU may still write the buffer or read a mapped picture: the stream,
        /// the buffer and the context reference are kept (leaked) instead.
        fn drop(&mut self) {
            if self.stream.get().is_some()
                && let Err(e) = self.push_current().and_then(|_current| self.settle_readback())
            {
                log::warn!("CUDA device kept: {e}");
                return;
            }
            // the stream and the page-locked buffer belong to the context: release them while the
            // context is retained (every copy into the buffer was waited for)
            if let (Some(stream), Some(api)) = (self.stream.take(), self.streams) {
                match self.push_current() {
                    Ok(_current) => {
                        // SAFETY: a stream this device created, with no work left on it (each copy
                        // was synchronised); the context is current.
                        let st = unsafe { (api.destroy)(stream.as_ptr()) };
                        if st != 0 {
                            log::warn!("cuStreamDestroy failed ({st})");
                        }
                    }
                    Err(e) => log::warn!("CUDA stream not destroyed: {e}"),
                }
            }
            if let Some(pinned) = self.staging.get_mut().take() {
                self.free_pinned(pinned);
            }
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
