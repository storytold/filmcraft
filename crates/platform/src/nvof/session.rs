//! An NVIDIA Optical Flow (`nvofapi64.dll` / `libnvidia-opticalflow.so.1`) hardware session on the
//! primary CUDA context (`nvcuda.dll` / `libcuda.so.1`).
//!
//! FFI module (docs/adr/0001-platform-ffi.md): every `unsafe` block has a `// SAFETY:` comment, the
//! public items are safe and return `Result<_, String>`, and no raw driver pointer leaves this module.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::OnceLock;

use super::ffi::{
    CUcontext, CUdeviceptr, CreateInstanceCudaFn, MaxSupportedVersionFn, NV_OF_API_VERSION, NV_OF_BUFFER_DESCRIPTOR, NV_OF_BUFFER_FORMAT,
    NV_OF_BUFFER_FORMAT_GRAYSCALE8, NV_OF_BUFFER_FORMAT_SHORT2, NV_OF_BUFFER_FORMAT_UINT8, NV_OF_BUFFER_USAGE, NV_OF_BUFFER_USAGE_COST,
    NV_OF_BUFFER_USAGE_INPUT, NV_OF_BUFFER_USAGE_OUTPUT, NV_OF_CAPS, NV_OF_CAPS_HEIGHT_MAX, NV_OF_CAPS_HEIGHT_MIN, NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES,
    NV_OF_CAPS_WIDTH_MAX, NV_OF_CAPS_WIDTH_MIN, NV_OF_CUDA_API_FUNCTION_LIST, NV_OF_CUDA_BUFFER_STRIDE_INFO, NV_OF_CUDA_BUFFER_TYPE_CUDEVICEPTR,
    NV_OF_EXECUTE_INPUT_PARAMS, NV_OF_EXECUTE_OUTPUT_PARAMS, NV_OF_FALSE, NV_OF_HINT_VECTOR_GRID_SIZE_UNDEFINED, NV_OF_INIT_PARAMS, NV_OF_MODE_OPTICALFLOW,
    NV_OF_OUTPUT_VECTOR_GRID_SIZE_1, NV_OF_OUTPUT_VECTOR_GRID_SIZE_2, NV_OF_OUTPUT_VECTOR_GRID_SIZE_4, NV_OF_PERF_LEVEL_SLOW, NV_OF_STATUS,
    NV_OF_STEREO_DISPARITY_RANGE_UNDEFINED, NV_OF_SUCCESS, NV_OF_TRUE, NvOFGPUBufferHandle, NvOFHandle,
};

type CuInitFn = unsafe extern "system" fn(u32) -> i32;
type CuDeviceGetFn = unsafe extern "system" fn(*mut i32, i32) -> i32;
type CuCtxRetainFn = unsafe extern "system" fn(*mut CUcontext, i32) -> i32;
type CuCtxReleaseFn = unsafe extern "system" fn(i32) -> i32;
type CuCtxPushFn = unsafe extern "system" fn(CUcontext) -> i32;
type CuCtxPopFn = unsafe extern "system" fn(*mut CUcontext) -> i32;
type CuCtxSyncFn = unsafe extern "system" fn() -> i32;
type CuMemcpy2DFn = unsafe extern "system" fn(*const CudaMemcpy2D) -> i32;

/// `CUmemorytype`: host memory.
const CU_MEMORYTYPE_HOST: u32 = 1;
/// `CUmemorytype`: device memory.
const CU_MEMORYTYPE_DEVICE: u32 = 2;

/// `CUDA_MEMCPY2D_v2` as NVIDIA's public CUDA Driver API reference documents it
/// (<https://docs.nvidia.com/cuda/cuda-driver-api/structCUDA__MEMCPY2D__v2.html>): a pitched copy
/// of `Height` rows of `WidthInBytes` bytes. Only the fields this backend sets are non-zero.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct CudaMemcpy2D {
    pub src_x_in_bytes: usize,
    pub src_y: usize,
    pub src_memory_type: u32,
    pub src_host: *const c_void,
    pub src_device: CUdeviceptr,
    pub src_array: *mut c_void,
    pub src_pitch: usize,
    pub dst_x_in_bytes: usize,
    pub dst_y: usize,
    pub dst_memory_type: u32,
    pub dst_host: *mut c_void,
    pub dst_device: CUdeviceptr,
    pub dst_array: *mut c_void,
    pub dst_pitch: usize,
    pub width_in_bytes: usize,
    pub height: usize,
}

impl CudaMemcpy2D {
    /// Host rows (`host_pitch` bytes apart) → device rows (`dev_pitch` bytes apart).
    fn host_to_device(src: &[u8], host_pitch: usize, dst: CUdeviceptr, dev_pitch: usize, width_in_bytes: usize, height: usize) -> Self {
        Self {
            src_x_in_bytes: 0,
            src_y: 0,
            src_memory_type: CU_MEMORYTYPE_HOST,
            src_host: src.as_ptr().cast(),
            src_device: 0,
            src_array: std::ptr::null_mut(),
            src_pitch: host_pitch,
            dst_x_in_bytes: 0,
            dst_y: 0,
            dst_memory_type: CU_MEMORYTYPE_DEVICE,
            dst_host: std::ptr::null_mut(),
            dst_device: dst,
            dst_array: std::ptr::null_mut(),
            dst_pitch: dev_pitch,
            width_in_bytes,
            height,
        }
    }

    /// Device rows (`dev_pitch` bytes apart) → host rows (`host_pitch` bytes apart).
    fn device_to_host(src: CUdeviceptr, dev_pitch: usize, dst: &mut [u8], host_pitch: usize, width_in_bytes: usize, height: usize) -> Self {
        Self {
            src_x_in_bytes: 0,
            src_y: 0,
            src_memory_type: CU_MEMORYTYPE_DEVICE,
            src_host: std::ptr::null(),
            src_device: src,
            src_array: std::ptr::null_mut(),
            src_pitch: dev_pitch,
            dst_x_in_bytes: 0,
            dst_y: 0,
            dst_memory_type: CU_MEMORYTYPE_HOST,
            dst_host: dst.as_mut_ptr().cast(),
            dst_device: 0,
            dst_array: std::ptr::null_mut(),
            dst_pitch: host_pitch,
            width_in_bytes,
            height,
        }
    }
}

struct CudaApi {
    device_get: CuDeviceGetFn,
    retain: CuCtxRetainFn,
    release: CuCtxReleaseFn,
    push: CuCtxPushFn,
    pop: CuCtxPopFn,
    sync: CuCtxSyncFn,
    copy_2d: CuMemcpy2DFn,
    #[cfg(target_os = "linux")]
    _library: libloading::Library,
}

// SAFETY: plain function pointers into the process-lifetime CUDA driver library.
unsafe impl Send for CudaApi {}
// SAFETY: see above.
unsafe impl Sync for CudaApi {}

fn cuda_api() -> Result<&'static CudaApi, String> {
    static CUDA: OnceLock<Result<CudaApi, String>> = OnceLock::new();
    CUDA.get_or_init(load_cuda).as_ref().map_err(Clone::clone)
}

#[cfg(target_os = "windows")]
fn load_cuda() -> Result<CudaApi, String> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
    use windows::core::{PCSTR, w};
    // SAFETY: loads the NVIDIA CUDA driver DLL from System32 only.
    let module: HMODULE =
        unsafe { LoadLibraryExW(w!("nvcuda.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|_| "no NVIDIA CUDA driver (nvcuda.dll)".to_string())?;
    // SAFETY: symbols match NVIDIA's public CUDA Driver API headers (`cuda.h`).
    unsafe {
        let init: CuInitFn = std::mem::transmute(GetProcAddress(module, PCSTR(c"cuInit".as_ptr().cast())).ok_or("nvcuda.dll lacks cuInit")?);
        let device_get: CuDeviceGetFn =
            std::mem::transmute(GetProcAddress(module, PCSTR(c"cuDeviceGet".as_ptr().cast())).ok_or("nvcuda.dll lacks cuDeviceGet")?);
        let retain: CuCtxRetainFn =
            std::mem::transmute(GetProcAddress(module, PCSTR(c"cuDevicePrimaryCtxRetain".as_ptr().cast())).ok_or("nvcuda.dll lacks cuDevicePrimaryCtxRetain")?);
        let release: CuCtxReleaseFn = std::mem::transmute(
            GetProcAddress(module, PCSTR(c"cuDevicePrimaryCtxRelease_v2".as_ptr().cast()))
                .or_else(|| GetProcAddress(module, PCSTR(c"cuDevicePrimaryCtxRelease".as_ptr().cast())))
                .ok_or("nvcuda.dll lacks cuDevicePrimaryCtxRelease")?,
        );
        let push: CuCtxPushFn = std::mem::transmute(
            GetProcAddress(module, PCSTR(c"cuCtxPushCurrent_v2".as_ptr().cast()))
                .or_else(|| GetProcAddress(module, PCSTR(c"cuCtxPushCurrent".as_ptr().cast())))
                .ok_or("nvcuda.dll lacks cuCtxPushCurrent")?,
        );
        let pop: CuCtxPopFn = std::mem::transmute(
            GetProcAddress(module, PCSTR(c"cuCtxPopCurrent_v2".as_ptr().cast()))
                .or_else(|| GetProcAddress(module, PCSTR(c"cuCtxPopCurrent".as_ptr().cast())))
                .ok_or("nvcuda.dll lacks cuCtxPopCurrent")?,
        );
        let sync: CuCtxSyncFn =
            std::mem::transmute(GetProcAddress(module, PCSTR(c"cuCtxSynchronize".as_ptr().cast())).ok_or("nvcuda.dll lacks cuCtxSynchronize")?);
        let copy_2d: CuMemcpy2DFn =
            std::mem::transmute(GetProcAddress(module, PCSTR(c"cuMemcpy2D_v2".as_ptr().cast())).ok_or("nvcuda.dll lacks cuMemcpy2D_v2")?);
        let st = init(0);
        if st != 0 {
            return Err(format!("cuInit failed ({st})"));
        }
        Ok(CudaApi { device_get, retain, release, push, pop, sync, copy_2d })
    }
}

#[cfg(target_os = "linux")]
fn load_cuda() -> Result<CudaApi, String> {
    // SAFETY: loads the NVIDIA CUDA driver shared library and looks up Driver API entry points.
    unsafe {
        let library = libloading::Library::new("libcuda.so.1").map_err(|e| format!("no NVIDIA CUDA driver: {e}"))?;
        let init: CuInitFn = *library.get(b"cuInit\0").map_err(|e| e.to_string())?;
        let device_get: CuDeviceGetFn = *library.get(b"cuDeviceGet\0").map_err(|e| e.to_string())?;
        let retain: CuCtxRetainFn = *library.get(b"cuDevicePrimaryCtxRetain\0").map_err(|e| e.to_string())?;
        let release: CuCtxReleaseFn = *library.get(b"cuDevicePrimaryCtxRelease_v2\0").map_err(|e| e.to_string())?;
        let push: CuCtxPushFn = *library.get(b"cuCtxPushCurrent_v2\0").map_err(|e| e.to_string())?;
        let pop: CuCtxPopFn = *library.get(b"cuCtxPopCurrent_v2\0").map_err(|e| e.to_string())?;
        let sync: CuCtxSyncFn = *library.get(b"cuCtxSynchronize\0").map_err(|e| e.to_string())?;
        let copy_2d: CuMemcpy2DFn = *library.get(b"cuMemcpy2D_v2\0").map_err(|e| e.to_string())?;
        let st = init(0);
        if st != 0 {
            return Err(format!("cuInit failed ({st})"));
        }
        Ok(CudaApi { device_get, retain, release, push, pop, sync, copy_2d, _library: library })
    }
}

struct CudaContext {
    api: &'static CudaApi,
    context: CUcontext,
    ordinal: i32,
}

// SAFETY: the retained primary CUDA context handle is thread-safe when pushed/popped around calls
// and serialised by `&mut Session`.
unsafe impl Send for CudaContext {}

struct CurrentGuard<'a> {
    api: &'a CudaApi,
    _not_send: PhantomData<Rc<()>>,
}

impl Drop for CurrentGuard<'_> {
    fn drop(&mut self) {
        let mut prev = std::ptr::null_mut();
        // SAFETY: balances one successful `cuCtxPushCurrent` on the same thread.
        let _ = unsafe { (self.api.pop)(&mut prev) };
    }
}

impl CudaContext {
    fn new() -> Result<Self, String> {
        let api = cuda_api()?;
        let mut ordinal = 0;
        // SAFETY: `ordinal` is a valid out-pointer after `cuInit(0)` succeeded.
        let st = unsafe { (api.device_get)(&mut ordinal, 0) };
        if st != 0 {
            return Err(format!("no CUDA device ({st})"));
        }
        let mut context = std::ptr::null_mut();
        // SAFETY: retains the device's primary CUDA context; released in `Drop`.
        let st = unsafe { (api.retain)(&mut context, ordinal) };
        if st != 0 || context.is_null() {
            return Err(format!("cuDevicePrimaryCtxRetain failed ({st})"));
        }
        Ok(Self { api, context, ordinal })
    }

    fn push_current(&self) -> Result<CurrentGuard<'_>, String> {
        // SAFETY: `self.context` is a live retained primary CUDA context.
        let st = unsafe { (self.api.push)(self.context) };
        if st != 0 {
            return Err(format!("cuCtxPushCurrent failed ({st})"));
        }
        Ok(CurrentGuard { api: self.api, _not_send: PhantomData })
    }

    /// Copy `buf.rows` rows of `buf.row_bytes` bytes from packed host memory `src` into the pitched
    /// device buffer. Only those bytes are written: the buffer's padding is never touched, so
    /// nothing depends on the driver's vertical stride.
    fn copy_to_device(&self, buf: &GpuBuffer, src: &[u8]) -> Result<(), String> {
        let need = buf.row_bytes.checked_mul(buf.rows).ok_or("CUDA upload size overflows")?;
        if src.len() < need {
            return Err(format!("CUDA upload: {} bytes for a {}x{} buffer", src.len(), buf.row_bytes, buf.rows));
        }
        if need == 0 {
            return Ok(());
        }
        let copy = CudaMemcpy2D::host_to_device(src, buf.row_bytes, buf.dev_ptr, buf.pitch, buf.row_bytes, buf.rows);
        // SAFETY: `src` is readable for `rows × row_bytes` bytes (checked above); the device buffer
        // was allocated by the driver as `rows` rows `pitch >= row_bytes` bytes apart
        // (checked in `alloc_buffer`); the caller holds the current CUDA context.
        let st = unsafe { (self.api.copy_2d)(&copy) };
        if st != 0 {
            return Err(format!("cuMemcpy2D (upload) failed ({st})"));
        }
        Ok(())
    }

    /// Copy the pitched device buffer's `rows × row_bytes` bytes into packed host memory `dst`.
    fn copy_to_host(&self, buf: &GpuBuffer, dst: &mut [u8]) -> Result<(), String> {
        let need = buf.row_bytes.checked_mul(buf.rows).ok_or("CUDA download size overflows")?;
        if dst.len() < need {
            return Err(format!("CUDA download: {} bytes for a {}x{} buffer", dst.len(), buf.row_bytes, buf.rows));
        }
        if need == 0 {
            return Ok(());
        }
        let copy = CudaMemcpy2D::device_to_host(buf.dev_ptr, buf.pitch, dst, buf.row_bytes, buf.row_bytes, buf.rows);
        // SAFETY: `dst` is writable for `rows × row_bytes` bytes (checked above); the device buffer
        // holds `rows` rows `pitch >= row_bytes` bytes apart; the caller holds the current context.
        let st = unsafe { (self.api.copy_2d)(&copy) };
        if st != 0 {
            return Err(format!("cuMemcpy2D (download) failed ({st})"));
        }
        Ok(())
    }

    fn synchronize(&self) -> Result<(), String> {
        // SAFETY: synchronizes the calling thread's current CUDA context.
        let st = unsafe { (self.api.sync)() };
        if st != 0 {
            return Err(format!("cuCtxSynchronize failed ({st})"));
        }
        Ok(())
    }
}

impl Drop for CudaContext {
    fn drop(&mut self) {
        // SAFETY: balances the `cuDevicePrimaryCtxRetain` in `CudaContext::new`.
        let _ = unsafe { (self.api.release)(self.ordinal) };
    }
}

struct OfApi {
    list: NV_OF_CUDA_API_FUNCTION_LIST,
    #[cfg(target_os = "linux")]
    _library: libloading::Library,
}

// SAFETY: immutable function table loaded once from the NVIDIA Optical Flow driver library.
unsafe impl Send for OfApi {}
// SAFETY: see above.
unsafe impl Sync for OfApi {}

fn of_api() -> Result<&'static OfApi, String> {
    static OF: OnceLock<Result<OfApi, String>> = OnceLock::new();
    OF.get_or_init(load_of).as_ref().map_err(Clone::clone)
}

#[cfg(target_os = "windows")]
fn load_of() -> Result<OfApi, String> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
    use windows::core::{PCSTR, w};
    // SAFETY: loads the NVIDIA Optical Flow driver library from System32 only.
    let module: HMODULE = unsafe { LoadLibraryExW(w!("nvofapi64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
        .map_err(|_| "no NVIDIA Optical Flow driver (nvofapi64.dll)".to_string())?;
    // SAFETY: symbols match `nvOpticalFlowCommon.h` and `nvOpticalFlowCuda.h`.
    unsafe {
        let max_ver: MaxSupportedVersionFn = std::mem::transmute(
            GetProcAddress(module, PCSTR(c"NvOFGetMaxSupportedApiVersion".as_ptr().cast())).ok_or("nvofapi64.dll lacks NvOFGetMaxSupportedApiVersion")?,
        );
        let create: CreateInstanceCudaFn = std::mem::transmute(
            GetProcAddress(module, PCSTR(c"NvOFAPICreateInstanceCuda".as_ptr().cast())).ok_or("nvofapi64.dll lacks NvOFAPICreateInstanceCuda")?,
        );
        let mut v = 0u32;
        if max_ver(&mut v) != NV_OF_SUCCESS {
            return Err("NvOFGetMaxSupportedApiVersion failed".into());
        }
        if v < NV_OF_API_VERSION {
            return Err(format!("NVIDIA Optical Flow driver version {:#x} is older than {:#x}", v, NV_OF_API_VERSION));
        }
        let mut list: NV_OF_CUDA_API_FUNCTION_LIST = std::mem::zeroed();
        let st = create(NV_OF_API_VERSION, &mut list);
        if st != NV_OF_SUCCESS {
            return Err(format!("NvOFAPICreateInstanceCuda failed ({st})"));
        }
        Ok(OfApi { list })
    }
}

#[cfg(target_os = "linux")]
fn load_of() -> Result<OfApi, String> {
    // SAFETY: loads `libnvidia-opticalflow.so.1` and resolves the public entry points.
    unsafe {
        let library =
            libloading::Library::new("libnvidia-opticalflow.so.1").map_err(|e| format!("no NVIDIA Optical Flow driver (libnvidia-opticalflow.so.1): {e}"))?;
        let max_ver: MaxSupportedVersionFn = *library.get(b"NvOFGetMaxSupportedApiVersion\0").map_err(|e| e.to_string())?;
        let create: CreateInstanceCudaFn = *library.get(b"NvOFAPICreateInstanceCuda\0").map_err(|e| e.to_string())?;
        let mut v = 0u32;
        if max_ver(&mut v) != NV_OF_SUCCESS {
            return Err("NvOFGetMaxSupportedApiVersion failed".into());
        }
        if v < NV_OF_API_VERSION {
            return Err(format!("NVIDIA Optical Flow driver version {:#x} is older than {:#x}", v, NV_OF_API_VERSION));
        }
        let mut list: NV_OF_CUDA_API_FUNCTION_LIST = std::mem::zeroed();
        let st = create(NV_OF_API_VERSION, &mut list);
        if st != NV_OF_SUCCESS {
            return Err(format!("NvOFAPICreateInstanceCuda failed ({st})"));
        }
        Ok(OfApi { list, _library: library })
    }
}

/// A driver-allocated, pitched NVOF buffer of `rows` rows of `row_bytes` meaningful bytes.
struct GpuBuffer {
    handle: NvOFGPUBufferHandle,
    dev_ptr: CUdeviceptr,
    /// Bytes from one row to the next on the device (`strideXInBytes`), at least `row_bytes`.
    pitch: usize,
    /// Meaningful bytes per row (width × bytes per element).
    row_bytes: usize,
    rows: usize,
}

/// The session's driver buffers; destroys whatever it holds when dropped, so no early return during
/// setup can leak a buffer.
struct Buffers {
    of: &'static OfApi,
    /// The two input frames; which one holds the reference changes from call to call.
    input: [Option<GpuBuffer>; 2],
    flow: Option<GpuBuffer>,
    cost: Option<GpuBuffer>,
}

impl Drop for Buffers {
    fn drop(&mut self) {
        for b in self.input.iter_mut().chain([&mut self.flow, &mut self.cost]) {
            if let Some(b) = b.take() {
                destroy_buf_quiet(self.of, b.handle);
            }
        }
    }
}

/// The optical-flow handle; destroyed when dropped (after the buffers, which [`Session`] declares
/// first).
struct OfHandle {
    of: &'static OfApi,
    handle: NvOFHandle,
}

impl Drop for OfHandle {
    fn drop(&mut self) {
        if !self.handle.is_null()
            && let Some(destroy_of) = self.of.list.nvOFDestroy
        {
            // SAFETY: destroys the optical-flow session handle; its buffers were released first.
            let _ = unsafe { destroy_of(self.handle) };
        }
    }
}

/// One hardware optical-flow session bound to a fixed `(width, height)` frame size.
///
/// Field order is drop order: the buffers go first, then the optical-flow handle, then the CUDA
/// context they live in. [`Drop`] makes the context current around all of it.
pub struct Session {
    buffers: Buffers,
    of_handle: OfHandle,
    cuda: CudaContext,
    of: &'static OfApi,
    width: usize,
    height: usize,
    grid_size: usize,
    grid_w: usize,
    grid_h: usize,
    /// Which input buffer holds the `curr` frame of the last successful call (the next call's
    /// reference when it continues the sequence).
    last_curr: Option<usize>,
    flow_staging: Vec<u8>,
    cost_staging: Vec<u8>,
}

// SAFETY: `Session` owns its `NvOFHandle` and GPU buffers; every call pushes `self.cuda` on the
// calling thread and is serialised through `&mut self`.
unsafe impl Send for Session {}

fn check_status(st: NV_OF_STATUS, op: &str) -> Result<(), String> {
    if st == NV_OF_SUCCESS { Ok(()) } else { Err(format!("{op} failed (NV_OF_STATUS = {st})")) }
}

fn query_caps(of: &OfApi, handle: NvOFHandle, cap: NV_OF_CAPS) -> Result<Vec<u32>, String> {
    let get_caps = of.list.nvOFGetCaps.ok_or("driver lacks nvOFGetCaps")?;
    let mut count = 0u32;
    // SAFETY: first stage queries the element count with `capsVal = null`.
    let st = unsafe { get_caps(handle, cap, std::ptr::null_mut(), &mut count) };
    check_status(st, "nvOFGetCaps(count)")?;
    if count == 0 || count > 64 {
        return Ok(Vec::new());
    }
    let mut vals = vec![0u32; count as usize];
    // SAFETY: `vals` has room for `count` `u32` elements.
    let st = unsafe { get_caps(handle, cap, vals.as_mut_ptr(), &mut count) };
    check_status(st, "nvOFGetCaps(values)")?;
    vals.truncate((count as usize).min(64));
    Ok(vals)
}

/// Create a `width × height` buffer of `elem_bytes`-byte elements. The driver's pitch must hold a
/// whole row; otherwise the buffer is destroyed and this is an error.
#[allow(clippy::too_many_arguments)]
fn alloc_buffer(
    of: &OfApi,
    handle: NvOFHandle,
    width: u32,
    height: u32,
    elem_bytes: usize,
    usage: NV_OF_BUFFER_USAGE,
    format: NV_OF_BUFFER_FORMAT,
) -> Result<GpuBuffer, String> {
    let create_buf = of.list.nvOFCreateGPUBufferCuda.ok_or("driver lacks nvOFCreateGPUBufferCuda")?;
    let get_ptr = of.list.nvOFGPUBufferGetCUdeviceptr.ok_or("driver lacks nvOFGPUBufferGetCUdeviceptr")?;
    let get_stride = of.list.nvOFGPUBufferGetStrideInfo.ok_or("driver lacks nvOFGPUBufferGetStrideInfo")?;
    let row_bytes = (width as usize).checked_mul(elem_bytes).ok_or("NVOF buffer row overflows")?;
    let desc = NV_OF_BUFFER_DESCRIPTOR { width, height, bufferUsage: usage, bufferFormat: format };
    let mut buf_handle: NvOFGPUBufferHandle = std::ptr::null_mut();
    // SAFETY: `handle` is an initialized `NvOFHandle` and `desc` is valid.
    let st = unsafe { create_buf(handle, &desc, NV_OF_CUDA_BUFFER_TYPE_CUDEVICEPTR, &mut buf_handle) };
    check_status(st, "nvOFCreateGPUBufferCuda")?;
    if buf_handle.is_null() {
        return Err("nvOFCreateGPUBufferCuda returned null".into());
    }
    // SAFETY: `buf_handle` was created with `NV_OF_CUDA_BUFFER_TYPE_CUDEVICEPTR`.
    let dev_ptr = unsafe { get_ptr(buf_handle) };
    let mut stride_info = NV_OF_CUDA_BUFFER_STRIDE_INFO::default();
    // SAFETY: `buf_handle` is valid and `stride_info` is a writable out-struct.
    let st = unsafe { get_stride(buf_handle, &mut stride_info) };
    let pitch = stride_info.strideInfo[0].strideXInBytes as usize;
    if dev_ptr == 0 || st != NV_OF_SUCCESS || stride_info.numPlanes == 0 || pitch < row_bytes {
        destroy_buf_quiet(of, buf_handle);
        return Err(format!("nvOFGPUBufferGetStrideInfo failed ({st}, pitch {pitch} for {row_bytes}-byte rows)"));
    }
    Ok(GpuBuffer { handle: buf_handle, dev_ptr, pitch, row_bytes, rows: height as usize })
}

fn destroy_buf_quiet(of: &OfApi, buf: NvOFGPUBufferHandle) {
    if !buf.is_null()
        && let Some(destroy_buf) = of.list.nvOFDestroyGPUBufferCuda
    {
        // SAFETY: `buf` is a live buffer handle owned by this session.
        let _ = unsafe { destroy_buf(buf) };
    }
}

impl Session {
    /// Probe whether the system has a working NVIDIA Optical Flow hardware engine.
    pub fn probe() -> Result<(), String> {
        let _ = of_api()?;
        let _ = CudaContext::new()?;
        Ok(())
    }

    /// Open a hardware optical-flow session for frames of `width` × `height`.
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width < 64 || height < 64 || width > 8192 || height > 8192 {
            return Err(format!("frame size {width}x{height} is outside NVOFA bounds"));
        }
        let of = of_api()?;
        let cuda = CudaContext::new()?;
        let guard = cuda.push_current()?;

        let create_of = of.list.nvCreateOpticalFlowCuda.ok_or("driver lacks nvCreateOpticalFlowCuda")?;
        let init_of = of.list.nvOFInit.ok_or("driver lacks nvOFInit")?;

        let mut handle: NvOFHandle = std::ptr::null_mut();
        // SAFETY: `cuda.context` is the current retained primary CUDA context.
        check_status(unsafe { create_of(cuda.context, &mut handle) }, "nvCreateOpticalFlowCuda")?;
        if handle.is_null() {
            return Err("nvCreateOpticalFlowCuda returned null".into());
        }
        // from here on, every early return destroys the handle (and, below, the buffers)
        let of_handle = OfHandle { of, handle };

        let min_w = query_caps(of, handle, NV_OF_CAPS_WIDTH_MIN)?.first().copied().unwrap_or(160);
        let min_h = query_caps(of, handle, NV_OF_CAPS_HEIGHT_MIN)?.first().copied().unwrap_or(64);
        let max_w = query_caps(of, handle, NV_OF_CAPS_WIDTH_MAX)?.first().copied().unwrap_or(8192);
        let max_h = query_caps(of, handle, NV_OF_CAPS_HEIGHT_MAX)?.first().copied().unwrap_or(8192);
        if width < min_w || height < min_h || width > max_w || height > max_h {
            return Err(format!("frame size {width}x{height} outside GPU NVOFA limits ({min_w}x{min_h}..={max_w}x{max_h})"));
        }
        let grids = query_caps(of, handle, NV_OF_CAPS_SUPPORTED_OUTPUT_GRID_SIZES)?;
        // Prefer the 4x4 native block size, or 2x2 / 1x1 if those are all that is reported.
        let grid_size = [NV_OF_OUTPUT_VECTOR_GRID_SIZE_4, NV_OF_OUTPUT_VECTOR_GRID_SIZE_2, NV_OF_OUTPUT_VECTOR_GRID_SIZE_1]
            .into_iter()
            .find(|g| grids.contains(g))
            .unwrap_or(NV_OF_OUTPUT_VECTOR_GRID_SIZE_4);

        let mut init_params = NV_OF_INIT_PARAMS {
            width,
            height,
            outGridSize: grid_size,
            hintGridSize: NV_OF_HINT_VECTOR_GRID_SIZE_UNDEFINED,
            mode: NV_OF_MODE_OPTICALFLOW,
            perfLevel: NV_OF_PERF_LEVEL_SLOW,
            enableExternalHints: NV_OF_FALSE,
            enableOutputCost: NV_OF_TRUE,
            hPrivData: std::ptr::null_mut(),
            disparityRange: NV_OF_STEREO_DISPARITY_RANGE_UNDEFINED,
            enableRoi: NV_OF_FALSE,
        };
        // SAFETY: `handle` and `init_params` are valid; if cost output is unsupported on this GPU
        // generation, retry without `enableOutputCost`.
        let has_cost = if unsafe { init_of(handle, &init_params) } == NV_OF_SUCCESS {
            true
        } else {
            init_params.enableOutputCost = NV_OF_FALSE;
            // SAFETY: same as above, without the output cost buffer.
            check_status(unsafe { init_of(handle, &init_params) }, "nvOFInit")?;
            false
        };

        let gw = width.div_ceil(grid_size).max(1);
        let gh = height.div_ceil(grid_size).max(1);
        let mut buffers = Buffers { of, input: [None, None], flow: None, cost: None };
        for slot in &mut buffers.input {
            *slot = Some(alloc_buffer(of, handle, width, height, 1, NV_OF_BUFFER_USAGE_INPUT, NV_OF_BUFFER_FORMAT_GRAYSCALE8)?);
        }
        // `NV_OF_FLOW_VECTOR`: two `i16`
        buffers.flow = Some(alloc_buffer(of, handle, gw, gh, 4, NV_OF_BUFFER_USAGE_OUTPUT, NV_OF_BUFFER_FORMAT_SHORT2)?);
        if has_cost {
            buffers.cost = alloc_buffer(of, handle, gw, gh, 1, NV_OF_BUFFER_USAGE_COST, NV_OF_BUFFER_FORMAT_UINT8).ok();
        }
        drop(guard);

        let cells = (gw as usize).saturating_mul(gh as usize);
        Ok(Self {
            buffers,
            of_handle,
            cuda,
            of,
            width: width as usize,
            height: height as usize,
            grid_size: grid_size as usize,
            grid_w: gw as usize,
            grid_h: gh as usize,
            last_curr: None,
            flow_staging: vec![0u8; cells.saturating_mul(4)],
            cost_staging: vec![0u8; cells],
        })
    }

    /// `(grid width, grid height, grid size)` of the flow vectors.
    pub fn grid_dimensions(&self) -> (usize, usize, usize) {
        (self.grid_w, self.grid_h, self.grid_size)
    }

    /// Execute hardware optical flow between `prev_luma` and `curr_luma` (`width * height` bytes,
    /// rows packed). With `sequential`, the caller promises `prev_luma` is the last call's
    /// `curr_luma`: that frame is already on the GPU and is not uploaded again, and the engine uses
    /// the last call's vectors as hints.
    pub fn execute(&mut self, prev_luma: &[u8], curr_luma: &[u8], sequential: bool) -> Result<(Vec<[f32; 2]>, Vec<u8>), String> {
        let expected = self.width.checked_mul(self.height).ok_or("frame size overflows")?;
        if prev_luma.len() < expected || curr_luma.len() < expected {
            return Err("luma buffer is smaller than frame dimensions".into());
        }
        let exec_fn = self.of.list.nvOFExecute.ok_or("driver lacks nvOFExecute")?;
        let _guard = self.cuda.push_current()?;

        // Continue from the frame already uploaded, or upload both.
        let continues = if sequential { self.last_curr } else { None };
        self.last_curr = None;
        let prev_idx = continues.unwrap_or(0).min(1);
        let curr_idx = 1 - prev_idx;
        let [Some(b0), Some(b1)] = &self.buffers.input else { return Err("NVOF input buffers are missing".into()) };
        let (prev_buf, curr_buf) = if prev_idx == 0 { (b0, b1) } else { (b1, b0) };
        if continues.is_none() {
            self.cuda.copy_to_device(prev_buf, prev_luma)?;
        }
        self.cuda.copy_to_device(curr_buf, curr_luma)?;
        let flow_buf = self.buffers.flow.as_ref().ok_or("NVOF flow buffer is missing")?;

        let exec_in = NV_OF_EXECUTE_INPUT_PARAMS {
            inputFrame: prev_buf.handle,
            referenceFrame: curr_buf.handle,
            externalHints: std::ptr::null_mut(),
            disableTemporalHints: if continues.is_some() { NV_OF_FALSE } else { NV_OF_TRUE },
            padding: 0,
            hPrivData: std::ptr::null_mut(),
            padding2: 0,
            numRois: 0,
            roiData: std::ptr::null_mut(),
        };
        let mut exec_out = NV_OF_EXECUTE_OUTPUT_PARAMS {
            outputBuffer: flow_buf.handle,
            outputCostBuffer: self.buffers.cost.as_ref().map_or(std::ptr::null_mut(), |b| b.handle),
            hPrivData: std::ptr::null_mut(),
        };

        // SAFETY: the input and output buffer handles belong to `self.of_handle` and are live.
        check_status(unsafe { exec_fn(self.of_handle.handle, &exec_in, &mut exec_out) }, "nvOFExecute")?;
        self.cuda.synchronize()?;

        self.cuda.copy_to_host(flow_buf, &mut self.flow_staging)?;
        if let Some(cost_buf) = &self.buffers.cost {
            self.cuda.copy_to_host(cost_buf, &mut self.cost_staging)?;
        }

        // S10.5 fixed point: 1/32nd of a pixel
        const INV_S10_5: f32 = 1.0 / 32.0;
        let uv: Vec<[f32; 2]> = self
            .flow_staging
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[x0, x1, y0, y1]| [f32::from(i16::from_le_bytes([x0, x1])) * INV_S10_5, f32::from(i16::from_le_bytes([y0, y1])) * INV_S10_5])
            .collect();
        let cost = if self.buffers.cost.is_some() { self.cost_staging.clone() } else { vec![0u8; uv.len()] };
        self.last_curr = Some(curr_idx);
        Ok((uv, cost))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The buffers and the handle are released by their own `Drop`s right after this, in field
        // order, with this context current (`push_current` failing leaves them to the driver's
        // context teardown).
        if let Ok(guard) = self.cuda.push_current() {
            let buffers = std::mem::replace(&mut self.buffers, Buffers { of: self.of, input: [None, None], flow: None, cost: None });
            drop(buffers);
            let handle = std::mem::replace(&mut self.of_handle, OfHandle { of: self.of, handle: std::ptr::null_mut() });
            drop(handle);
            drop(guard);
        }
    }
}
