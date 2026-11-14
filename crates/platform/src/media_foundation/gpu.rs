//! The Direct3D 11 side of the Windows decoder: the video-capable device shared by every decoder
//! (with the `IMFDXGIDeviceManager` the decoder MFTs get it through), what DXVA can decode on it,
//! and the readback of a decoded surface into system memory (GPU to staging texture to bytes).
//!
//! FFI module (docs/adr/0001-platform-ffi.md): every `unsafe` block has a `// SAFETY:` comment,
//! the public items are safe and return `Result<_, String>`, no COM pointer leaves this module
//! except as an opaque handle for the MFT wrapper.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use filmcraft_frame::{Chroma, GpuSurface, PixelData, VideoFrame, pool};
use filmcraft_gpu::ImportedPlanes;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D11_SRV_DIMENSION_TEXTURE2D, ID3DBlob,
    ID3DInclude,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BUFFER_DESC,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_CULL_NONE, D3D11_FILL_SOLID, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_RASTERIZER_DESC, D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION,
    D3D11_SHADER_RESOURCE_VIEW_DESC, D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING, D3D11_VIDEO_DECODER_DESC, D3D11_VIEWPORT, D3D11CreateDevice, ID3D11Buffer, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11PixelShader, ID3D11Query, ID3D11RasterizerState, ID3D11RenderTargetView, ID3D11ShaderResourceView, ID3D11Texture2D,
    ID3D11VertexShader, ID3D11VideoDevice,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, IDXGIAdapter, IDXGIDevice, IDXGIResource1};
use windows::Win32::Media::MediaFoundation::{IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSample};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::{GUID, Interface, s};

use super::mft::MfApi;
use crate::biplanar::{Biplanar, Geometry};

const SHARED_POOL_BUDGET_BYTES: u64 = 128 * 1024 * 1024;
const EVENT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_SURFACE_ID: AtomicUsize = AtomicUsize::new(1);

/// What a surface format holds: 8-bit NV12 or 10-bit P010 (both 4:2:0, chroma interleaved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceFormat {
    Nv12,
    P010,
}

impl SurfaceFormat {
    pub fn bits(self) -> u32 {
        match self {
            Self::Nv12 => 8,
            Self::P010 => 10,
        }
    }

    fn dxgi(self) -> DXGI_FORMAT {
        match self {
            Self::Nv12 => DXGI_FORMAT_NV12,
            Self::P010 => DXGI_FORMAT_P010,
        }
    }

    fn plane_dxgi(self) -> (DXGI_FORMAT, DXGI_FORMAT) {
        match self {
            Self::Nv12 => (DXGI_FORMAT_R8_UNORM, DXGI_FORMAT_R8G8_UNORM),
            Self::P010 => (DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_R16G16_UNORM),
        }
    }

    fn wgpu_formats(self) -> (wgpu::TextureFormat, wgpu::TextureFormat, f32) {
        match self {
            Self::Nv12 => (wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm, 255.0),
            Self::P010 => (wgpu::TextureFormat::R16Unorm, wgpu::TextureFormat::Rg16Unorm, 65535.0 / 64.0),
        }
    }
}

thread_local! {
    /// COM is initialised (multithreaded) once per thread that touches a decoder; decoders move
    /// between frame-worker threads. Never uninitialised: the threads end with the process.
    static COM: Result<(), String> = init_com();
}

fn init_com() -> Result<(), String> {
    // SAFETY: CoInitializeEx has no pointer arguments; calling it again on a thread that already
    // initialised COM (in either apartment model) is allowed and reported through the HRESULT.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    // A thread already in a single-threaded apartment (the UI thread) keeps it: MF decoders and
    // D3D11 are free-threaded and work from there too.
    if hr.is_ok() || hr == RPC_E_CHANGED_MODE { Ok(()) } else { Err(format!("COM initialisation failed ({hr:?})")) }
}

/// Make sure the calling thread can use COM objects.
pub fn ensure_com() -> Result<(), String> {
    COM.with(Clone::clone)
}

struct ScratchSurface {
    tex: ID3D11Texture2D,
    srv_y: ID3D11ShaderResourceView,
    srv_uv: ID3D11ShaderResourceView,
    w: u32,
    h: u32,
    format: SurfaceFormat,
}

struct CopyPipeline {
    vs: ID3D11VertexShader,
    ps_y: ID3D11PixelShader,
    ps_uv: ID3D11PixelShader,
    cb: ID3D11Buffer,
    rs: ID3D11RasterizerState,
    scratch: Option<ScratchSurface>,
}

struct SharedPlanePair {
    tex_y: ID3D11Texture2D,
    rtv_y: ID3D11RenderTargetView,
    handle_y: HANDLE,
    tex_uv: ID3D11Texture2D,
    rtv_uv: ID3D11RenderTargetView,
    handle_uv: HANDLE,
    w: u32,
    h: u32,
    cw: u32,
    ch: u32,
    format: SurfaceFormat,
    bytes: u64,
}

impl Drop for SharedPlanePair {
    fn drop(&mut self) {
        // SAFETY: `handle_y` and `handle_uv` are valid Win32 NT handles created via
        // `IDXGIResource1::CreateSharedHandle` and owned uniquely by `SharedPlanePair`.
        unsafe {
            if !self.handle_y.is_invalid() {
                let _ = CloseHandle(self.handle_y);
            }
            if !self.handle_uv.is_invalid() {
                let _ = CloseHandle(self.handle_uv);
            }
        }
    }
}

#[derive(Default)]
struct SharedPool {
    pairs: VecDeque<SharedPlanePair>,
    bytes: u64,
}

#[derive(Default)]
struct CpuFallbackStaging {
    y: Option<(ID3D11Texture2D, u32, u32, DXGI_FORMAT)>,
    uv: Option<(ID3D11Texture2D, u32, u32, DXGI_FORMAT)>,
}

/// A Direct3D 11 device created for video decoding, with its DXGI device manager.
pub struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    manager: IMFDXGIDeviceManager,
    name: String,
    /// Set when the device was removed or reset: new decoders get a new device.
    lost: AtomicBool,
    /// Serialises our use of the immediate context (the device is also multithread protected,
    /// which covers the decoder MFTs' own use of it).
    gate: Mutex<()>,
    copy: Mutex<Option<CopyPipeline>>,
    query_pool: Mutex<Vec<ID3D11Query>>,
    shared_pool: Mutex<SharedPool>,
    fallback_staging: Mutex<CpuFallbackStaging>,
}

// SAFETY: the device and its immediate context are used from several threads on purpose: the
// device is created multithread-protected (`ID3D11Multithread::SetMultithreadProtected`), the
// DXGI device manager is a free-threaded object, and our own use of the context is behind `gate`.
unsafe impl Send for Gpu {}
// SAFETY: see above.
unsafe impl Sync for Gpu {}

static SHARED: Mutex<Option<Arc<Gpu>>> = Mutex::new(None);

impl Gpu {
    /// The process-wide device (created on first use, replaced after a device loss).
    pub fn shared(api: &MfApi) -> Result<Arc<Gpu>, String> {
        ensure_com()?;
        let mut slot = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(g) = slot.as_ref().filter(|g| g.usable()) {
            return Ok(g.clone());
        }
        let g = Arc::new(Gpu::create(api)?);
        log::info!("hardware decoding: Direct3D 11 device on {}", g.name);
        *slot = Some(g.clone());
        Ok(g)
    }

    fn create(api: &MfApi) -> Result<Gpu, String> {
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let (mut device, mut context) = (None, None);
        // SAFETY: the out-pointers refer to live locals; no adapter is passed (the default one),
        // and the feature level slice outlives the call.
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| format!("no Direct3D 11 video device: {e}"))?;
        let (device, context) = device.zip(context).ok_or("Direct3D 11 returned no device")?;
        // Decoder MFTs on other threads use this device while we copy surfaces out of it.
        let mt: ID3D11Multithread = device.cast().map_err(|e| format!("no ID3D11Multithread: {e}"))?;
        // SAFETY: a plain COM call on a live interface (it returns the previous setting).
        let _ = unsafe { mt.SetMultithreadProtected(true) };
        let video: ID3D11VideoDevice = device.cast().map_err(|e| format!("the device has no video decoding: {e}"))?;
        let manager = api.create_device_manager(&device)?;
        let name = adapter_name(&device).unwrap_or_else(|| "an unknown adapter".into());
        Ok(Gpu {
            device,
            context,
            video,
            manager,
            name,
            lost: AtomicBool::new(false),
            gate: Mutex::new(()),
            copy: Mutex::new(None),
            query_pool: Mutex::new(Vec::new()),
            shared_pool: Mutex::new(SharedPool::default()),
            fallback_staging: Mutex::new(CpuFallbackStaging::default()),
        })
    }

    fn usable(&self) -> bool {
        // SAFETY: a plain COM call on a live interface.
        !self.lost.load(Ordering::Relaxed) && unsafe { self.device.GetDeviceRemovedReason() }.is_ok()
    }

    /// The adapter's description (for logs).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The device manager decoder MFTs are given (`MFT_MESSAGE_SET_D3D_MANAGER`), as the integer
    /// that message carries. Valid while this `Gpu` lives.
    pub fn manager_param(&self) -> usize {
        self.manager.as_raw() as usize
    }

    /// A failed call may mean the device is gone: if so, mark it so new decoders get a fresh one.
    pub fn note_failure(&self) {
        if !self.usable() {
            log::warn!("Direct3D 11 device lost ({}); new decoders will create another", self.name);
            self.lost.store(true, Ordering::Relaxed);
        }
    }

    /// Every decoder profile the device's DXVA lists.
    pub fn profiles(&self) -> Vec<GUID> {
        // SAFETY: plain COM calls on a live interface; each index below the count is valid.
        unsafe {
            let n = self.video.GetVideoDecoderProfileCount();
            (0..n).filter_map(|i| self.video.GetVideoDecoderProfile(i).ok()).collect()
        }
    }

    /// Whether DXVA on this device decodes `profile` into `format` at `size` (so a stream is
    /// declined up front instead of failing on its first sample).
    pub fn supports(&self, profile: GUID, format: SurfaceFormat, size: (u32, u32)) -> Result<(), String> {
        // SAFETY: plain COM calls on a live interface with a valid descriptor.
        unsafe {
            let ok = self.video.CheckVideoDecoderFormat(&profile, format.dxgi()).map_err(|e| format!("decoder format check failed: {e}"))?;
            if !ok.as_bool() {
                return Err(format!("the GPU has no DXVA decoder for this profile in {format:?}"));
            }
            let desc = D3D11_VIDEO_DECODER_DESC { Guid: profile, SampleWidth: size.0, SampleHeight: size.1, OutputFormat: format.dxgi() };
            let configs = self.video.GetVideoDecoderConfigCount(&desc).map_err(|e| format!("decoder configuration query failed: {e}"))?;
            if configs == 0 {
                return Err(format!("the GPU's DXVA decoder does not take {}x{} pictures", size.0, size.1));
            }
        }
        Ok(())
    }

    fn take_shared_pair(&self, format: SurfaceFormat, w: u32, h: u32) -> Result<SharedPlanePair, String> {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        {
            let mut pool = self.shared_pool.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(idx) = pool.pairs.iter().position(|p| p.format == format && p.w == w && p.h == h && p.cw == cw && p.ch == ch)
                && let Some(pair) = pool.pairs.remove(idx)
            {
                pool.bytes = pool.bytes.saturating_sub(pair.bytes);
                return Ok(pair);
            }
        }
        create_shared_pair(&self.device, format, w, h, cw, ch)
    }

    fn recycle_shared_pair(&self, pair: SharedPlanePair) {
        if self.lost.load(Ordering::Relaxed) {
            return;
        }
        let mut pool = self.shared_pool.lock().unwrap_or_else(PoisonError::into_inner);
        while pool.bytes.saturating_add(pair.bytes) > SHARED_POOL_BUDGET_BYTES {
            let Some(evicted) = pool.pairs.pop_front() else { break };
            pool.bytes = pool.bytes.saturating_sub(evicted.bytes);
        }
        if pool.bytes.saturating_add(pair.bytes) <= SHARED_POOL_BUDGET_BYTES {
            pool.bytes += pair.bytes;
            pool.pairs.push_back(pair);
        }
    }

    /// Copy and crop a DXVA decoded surface on the GPU into pooled shared NT-handle Y + CbCr
    /// textures wrapped in a lazy [`MfGpuSurface`] (zero CPU↔GPU copies in the hot path).
    pub fn export_surface(self: &Arc<Self>, sample: &IMFSample, format: SurfaceFormat, geometry: Geometry) -> Result<VideoFrame, String> {
        let r = self.export_surface_inner(sample, format, geometry);
        if r.is_err() {
            self.note_failure();
        }
        r
    }

    fn export_surface_inner(self: &Arc<Self>, sample: &IMFSample, format: SurfaceFormat, geometry: Geometry) -> Result<VideoFrame, String> {
        let (ox, oy, w, h) = geometry.crop;
        if w == 0 || h == 0 || !matches!(geometry.bits, 8 | 10) {
            return Err(format!("cannot export a {w}x{h} picture of {} bits", geometry.bits));
        }
        let (tex, index, desc) = extract_sample_texture(sample, format)?;
        if u64::from(ox) + u64::from(w) > u64::from(desc.Width) || u64::from(oy) + u64::from(h) > u64::from(desc.Height) {
            return Err(format!("decoded surface {}x{} is smaller than crop ({ox},{oy})+{w}x{h}", desc.Width, desc.Height));
        }
        let pair = self.take_shared_pair(format, w, h)?;
        let query = self.take_event_query()?;
        {
            let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
            let mut copy_slot = self.copy.lock().unwrap_or_else(PoisonError::into_inner);
            let copy = match copy_slot.as_mut() {
                Some(c) => c,
                None => copy_slot.insert(CopyPipeline::new(&self.device)?),
            };
            copy.ensure_scratch(&self.device, desc.Width, desc.Height, format)?;
            copy.dispatch(&self.context, &tex, index, &pair, (ox, oy), &query);
        }
        self.wait_event_query(&query)?;
        self.recycle_event_query(query);
        let surf = MfGpuSurface {
            id: (1usize << (usize::BITS - 1)) | NEXT_SURFACE_ID.fetch_add(1, Ordering::Relaxed),
            gpu: Arc::clone(self),
            format,
            w,
            h,
            cw: pair.cw,
            ch: pair.ch,
            pair: Mutex::new(Some(pair)),
        };
        Ok(VideoFrame {
            width: w,
            height: h,
            data: PixelData::GpuSurface(Arc::new(surf)),
            color: geometry.color,
            par: geometry.par,
            pts: filmcraft_time::Tick::ZERO,
        })
    }

    fn take_event_query(&self) -> Result<ID3D11Query, String> {
        if let Some(q) = self.query_pool.lock().unwrap_or_else(PoisonError::into_inner).pop() {
            return Ok(q);
        }
        let q_desc = D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 };
        let mut query: Option<ID3D11Query> = None;
        // SAFETY: valid event query descriptor and out-pointer on live D3D11 device.
        unsafe { self.device.CreateQuery(&q_desc, Some(&mut query)) }.map_err(|e| format!("CreateQuery failed: {e}"))?;
        query.ok_or_else(|| "no event query".to_string())
    }

    fn recycle_event_query(&self, query: ID3D11Query) {
        if self.lost.load(Ordering::Relaxed) {
            return;
        }
        let mut pool = self.query_pool.lock().unwrap_or_else(PoisonError::into_inner);
        if pool.len() < 16 {
            pool.push(query);
        }
    }

    fn wait_event_query(&self, query: &ID3D11Query) -> Result<(), String> {
        let start = Instant::now();
        loop {
            let mut done: u32 = 0;
            let res = {
                let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
                // SAFETY: `query` is a live `D3D11_QUERY_EVENT` owned by this caller until returned to
                // the pool; `done` is a 4-byte `BOOL` and immediate-context access is behind `gate`.
                unsafe {
                    self.context.GetData(
                        query,
                        Some(&mut done as *mut u32 as *mut c_void),
                        std::mem::size_of::<u32>() as u32,
                        D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
                    )
                }
            };
            res.map_err(|e| format!("D3D11 event query failed: {e}"))?;
            if done != 0 {
                return Ok(());
            }
            if start.elapsed() > EVENT_QUERY_TIMEOUT {
                return Err("D3D11 event query timed out".to_string());
            }
            std::thread::yield_now();
        }
    }

    fn read_shared_pair(&self, tex_y: &ID3D11Texture2D, tex_uv: &ID3D11Texture2D, format: SurfaceFormat, size: (u32, u32), chroma: (u32, u32)) -> Result<PixelData, String> {
        let (w, h) = size;
        let (cw, ch) = chroma;
        let (fmt_y, fmt_uv) = format.plane_dxgi();
        let bps = if format == SurfaceFormat::P010 { 2 } else { 1 };
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let mut staging = self.fallback_staging.lock().unwrap_or_else(PoisonError::into_inner);
        let st_y = ensure_staging_plane(&self.device, &mut staging.y, w, h, fmt_y)?;
        let (raw_y, stride_y) = map_staging_bytes(&self.context, tex_y, st_y, w * bps, h)?;
        let st_uv = ensure_staging_plane(&self.device, &mut staging.uv, cw, ch, fmt_uv)?;
        let (raw_uv, stride_uv) = map_staging_bytes(&self.context, tex_uv, st_uv, cw * 2 * bps, ch)?;
        filmcraft_frame::transfers::record_decode_readback((stride_y * h as usize + stride_uv * ch as usize) as u64);
        Ok(deinterleave_planes(
            format,
            (w as usize, h as usize),
            (cw as usize, ch as usize),
            RawPlane { bytes: &raw_y, stride: stride_y },
            RawPlane { bytes: &raw_uv, stride: stride_uv },
        ))
    }
}

/// GPU-resident NV12/P010 decoded video surface backed by D3D11 shared textures (`tex_y`, `tex_uv`).
pub struct MfGpuSurface {
    id: usize,
    gpu: Arc<Gpu>,
    format: SurfaceFormat,
    w: u32,
    h: u32,
    cw: u32,
    ch: u32,
    pair: Mutex<Option<SharedPlanePair>>,
}

// SAFETY: `SharedPlanePair` holds COM texture interfaces from a multithread-protected D3D11 device
// and Win32 NT handles protected by `Mutex`; reads are immutable after creation and pooling is mutex-guarded.
unsafe impl Send for MfGpuSurface {}
// SAFETY: see above.
unsafe impl Sync for MfGpuSurface {}

impl std::fmt::Debug for MfGpuSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfGpuSurface").field("id", &self.id).field("format", &self.format).field("w", &self.w).field("h", &self.h).finish()
    }
}

impl Drop for MfGpuSurface {
    fn drop(&mut self) {
        if let Some(pair) = self.pair.get_mut().unwrap_or_else(PoisonError::into_inner).take() {
            self.gpu.recycle_shared_pair(pair);
        }
    }
}

impl GpuSurface for MfGpuSurface {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn id(&self) -> usize {
        self.id
    }

    fn gpu_bytes(&self) -> u64 {
        let bps = if self.format == SurfaceFormat::P010 { 2u64 } else { 1u64 };
        u64::from(self.w) * u64::from(self.h) * bps + u64::from(self.cw) * u64::from(self.ch) * 2 * bps
    }

    fn bits(&self) -> u32 {
        self.format.bits()
    }

    fn chroma(&self) -> Chroma {
        Chroma::C420
    }

    fn to_cpu(&self) -> Result<PixelData, String> {
        let (tex_y, tex_uv) = {
            let guard = self.pair.lock().unwrap_or_else(PoisonError::into_inner);
            let p = guard.as_ref().ok_or("GPU surface already released")?;
            (p.tex_y.clone(), p.tex_uv.clone())
        };
        self.gpu.read_shared_pair(&tex_y, &tex_uv, self.format, (self.w, self.h), (self.cw, self.ch))
    }
}

/// Imports an [`MfGpuSurface`] directly into a Vulkan-backed `wgpu::Device` via D3D11 NT shared handles.
pub fn import_wgpu_surface(device: &wgpu::Device, dummy: &wgpu::TextureView, surf: &dyn GpuSurface, w: u32, h: u32) -> Option<ImportedPlanes> {
    let mf = surf.as_any().downcast_ref::<MfGpuSurface>()?;
    if w != mf.w || h != mf.h {
        return None;
    }
    if !device.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32) {
        return None;
    }
    if mf.format.bits() > 8 && !device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) {
        return None;
    }
    let (handle_y, handle_uv) = {
        let guard = mf.pair.lock().unwrap_or_else(PoisonError::into_inner);
        let pair = guard.as_ref()?;
        (pair.handle_y, pair.handle_uv)
    };
    let (fmt_y, fmt_uv, code_scale) = mf.format.wgpu_formats();
    let tex_y = import_d3d11_plane(device, handle_y, mf.w, mf.h, fmt_y, "mf-zero-copy-y")?;
    let tex_uv = import_d3d11_plane(device, handle_uv, mf.cw, mf.ch, fmt_uv, "mf-zero-copy-uv")?;
    let view_y = tex_y.create_view(&wgpu::TextureViewDescriptor::default());
    let view_uv = tex_uv.create_view(&wgpu::TextureViewDescriptor::default());
    Some(ImportedPlanes {
        views: [view_y, view_uv, dummy.clone(), dummy.clone()],
        textures: vec![tex_y, tex_uv],
        gpu_bytes: mf.gpu_bytes(),
        kind: 3,
        code_scale,
        chroma: (mf.cw, mf.ch),
    })
}

fn import_d3d11_plane(
    device: &wgpu::Device,
    handle: HANDLE,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> Option<wgpu::Texture> {
    let hal_desc = wgpu::hal::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUses::RESOURCE,
        memory_flags: wgpu::hal::MemoryFlags::empty(),
        view_formats: Vec::new(),
    };
    let hal_tex = {
        // SAFETY: `device` is active; only `Vulkan` devices return `Some`.
        let hal_dev = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }?;
        // SAFETY: `handle` is a valid NT shared handle for a D3D11 2D texture matching `hal_desc`
        // (`R8Unorm`/`Rg8Unorm` or `R16Unorm`/`Rg16Unorm` single-plane texture), and the GPU copy
        // writing to it completed before `MfGpuSurface` was constructed.
        match unsafe { hal_dev.texture_from_d3d11_shared_handle(handle, &hal_desc) } {
            Ok(t) => t,
            Err(why) => {
                static LOGGED: AtomicBool = AtomicBool::new(false);
                if !LOGGED.swap(true, Ordering::Relaxed) {
                    log::warn!("Vulkan D3D11 shared handle import unavailable ({why:?}); falling back to CPU readback");
                }
                return None;
            }
        }
    };
    let wgpu_desc = wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    // SAFETY: `hal_tex` was just imported on `device` with matching `wgpu_desc` and holds valid
    // decoded image contents.
    Some(unsafe { device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(hal_tex, &wgpu_desc, wgpu::TextureUses::UNINITIALIZED) })
}

fn extract_sample_texture(sample: &IMFSample, format: SurfaceFormat) -> Result<(ID3D11Texture2D, u32, D3D11_TEXTURE2D_DESC), String> {
    // SAFETY: plain COM calls on live interfaces; `GetResource` writes one interface pointer
    // into `tex`, which `Option<ID3D11Texture2D>` owns and releases.
    let (tex, index) = unsafe {
        let buffer = sample.GetBufferByIndex(0).map_err(|e| format!("decoder output has no buffer: {e}"))?;
        let dxgi: IMFDXGIBuffer = buffer.cast().map_err(|_| "decoder output is in system memory: the decoder is not using the GPU".to_string())?;
        let mut tex: Option<ID3D11Texture2D> = None;
        dxgi.GetResource(&ID3D11Texture2D::IID, &mut tex as *mut Option<ID3D11Texture2D> as *mut *mut c_void)
            .map_err(|e| format!("decoder output is not a 2D texture: {e}"))?;
        (tex.ok_or("decoder output has no texture")?, dxgi.GetSubresourceIndex().map_err(|e| format!("no subresource index: {e}"))?)
    };
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: a plain COM call writing a descriptor struct.
    unsafe { tex.GetDesc(&mut desc) };
    if desc.Format != format.dxgi() {
        return Err(format!("decoded surface has DXGI format {} instead of {format:?}", desc.Format.0));
    }
    if desc.Width == 0 || desc.Height == 0 || desc.Width > 16384 || desc.Height > 16384 {
        return Err(format!("decoded surface is {}x{}", desc.Width, desc.Height));
    }
    Ok((tex, index, desc))
}

fn create_shared_pair(device: &ID3D11Device, format: SurfaceFormat, w: u32, h: u32, cw: u32, ch: u32) -> Result<SharedPlanePair, String> {
    let (fmt_y, fmt_uv) = format.plane_dxgi();
    let (tex_y, rtv_y, handle_y) = create_shared_plane(device, w, h, fmt_y)?;
    let (tex_uv, rtv_uv, handle_uv) = match create_shared_plane(device, cw, ch, fmt_uv) {
        Ok(v) => v,
        Err(e) => {
            // SAFETY: `handle_y` was created above and is not yet owned by `SharedPlanePair`.
            unsafe { let _ = CloseHandle(handle_y); }
            return Err(e);
        }
    };
    let bps = if format == SurfaceFormat::P010 { 2u64 } else { 1u64 };
    let bytes = u64::from(w) * u64::from(h) * bps + u64::from(cw) * u64::from(ch) * 2 * bps;
    Ok(SharedPlanePair { tex_y, rtv_y, handle_y, tex_uv, rtv_uv, handle_uv, w, h, cw, ch, format, bytes })
}

fn create_shared_plane(device: &ID3D11Device, w: u32, h: u32, format: DXGI_FORMAT) -> Result<(ID3D11Texture2D, ID3D11RenderTargetView, HANDLE), String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    // SAFETY: valid descriptor and out-pointer on live D3D11 device.
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(|e| format!("cannot create shared D3D11 plane texture: {e}"))?;
    let tex = tex.ok_or("no shared D3D11 texture")?;
    let mut rtv: Option<ID3D11RenderTargetView> = None;
    // SAFETY: `tex` was created with `D3D11_BIND_RENDER_TARGET`.
    unsafe { device.CreateRenderTargetView(&tex, None, Some(&mut rtv)) }.map_err(|e| format!("cannot create D3D11 RTV: {e}"))?;
    let rtv = rtv.ok_or("no D3D11 RTV")?;
    let res: IDXGIResource1 = tex.cast().map_err(|e| format!("no IDXGIResource1 on shared texture: {e}"))?;
    // SAFETY: `tex` has `D3D11_RESOURCE_MISC_SHARED_NTHANDLE`, anonymous handle with RW access.
    let handle = unsafe { res.CreateSharedHandle(None, (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0, None) }
        .map_err(|e| format!("CreateSharedHandle failed: {e}"))?;
    Ok((tex, rtv, handle))
}

const COPY_HLSL: &str = r#"
cbuffer Offset : register(b0) {
    int4 offset;
};
Texture2D<float> src_y : register(t0);
Texture2D<float2> src_uv : register(t0);

float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2.0, -2.0) + float2(-1.0, 1.0), 0.0, 1.0);
}

float ps_y(float4 pos : SV_Position) : SV_Target {
    return src_y.Load(int3(int2(pos.xy) + offset.xy, 0));
}

float2 ps_uv(float4 pos : SV_Position) : SV_Target {
    return src_uv.Load(int3(int2(pos.xy) + offset.xy, 0));
}
"#;

fn compile_hlsl(entry: windows::core::PCSTR, target: windows::core::PCSTR) -> Result<Vec<u8>, String> {
    let mut code: Option<ID3DBlob> = None;
    let mut err: Option<ID3DBlob> = None;
    // SAFETY: `COPY_HLSL` points to a valid immutable ASCII byte slice for the call duration;
    // `code` and `err` are stack locals.
    let res = unsafe {
        D3DCompile(
            COPY_HLSL.as_ptr() as *const c_void,
            COPY_HLSL.len(),
            s!("mf_copy.hlsl"),
            None,
            None::<&ID3DInclude>,
            entry,
            target,
            0,
            0,
            &mut code,
            Some(&mut err),
        )
    };
    if let Err(e) = res {
        let msg = err
            .map(|b| {
                // SAFETY: `ID3DBlob` buffer pointer is valid for `GetBufferSize()` bytes.
                let bytes = unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) };
                String::from_utf8_lossy(bytes).into_owned()
            })
            .unwrap_or_default();
        return Err(format!("D3DCompile failed ({e}): {msg}"));
    }
    let blob = code.ok_or("D3DCompile produced no bytecode")?;
    // SAFETY: `ID3DBlob` buffer pointer is valid for `GetBufferSize()` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()) };
    Ok(bytes.to_vec())
}

impl CopyPipeline {
    fn new(device: &ID3D11Device) -> Result<Self, String> {
        let vs_bin = compile_hlsl(s!("vs_main"), s!("vs_5_0"))?;
        let ps_y_bin = compile_hlsl(s!("ps_y"), s!("ps_5_0"))?;
        let ps_uv_bin = compile_hlsl(s!("ps_uv"), s!("ps_5_0"))?;
        let mut vs: Option<ID3D11VertexShader> = None;
        let mut ps_y: Option<ID3D11PixelShader> = None;
        let mut ps_uv: Option<ID3D11PixelShader> = None;
        // SAFETY: shader bytecodes were compiled by `D3DCompile` above and outlive the calls.
        unsafe {
            device.CreateVertexShader(&vs_bin, None, Some(&mut vs)).map_err(|e| format!("CreateVertexShader failed: {e}"))?;
            device.CreatePixelShader(&ps_y_bin, None, Some(&mut ps_y)).map_err(|e| format!("CreatePixelShader(Y) failed: {e}"))?;
            device.CreatePixelShader(&ps_uv_bin, None, Some(&mut ps_uv)).map_err(|e| format!("CreatePixelShader(UV) failed: {e}"))?;
        }
        let cb_desc = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        let mut cb: Option<ID3D11Buffer> = None;
        // SAFETY: valid constant buffer descriptor and out-pointer.
        unsafe { device.CreateBuffer(&cb_desc, None, Some(&mut cb)) }.map_err(|e| format!("CreateBuffer failed: {e}"))?;
        let rs_desc = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: true.into(),
            ..Default::default()
        };
        let mut rs: Option<ID3D11RasterizerState> = None;
        // SAFETY: valid rasterizer descriptor and out-pointer.
        unsafe { device.CreateRasterizerState(&rs_desc, Some(&mut rs)) }.map_err(|e| format!("CreateRasterizerState failed: {e}"))?;
        Ok(Self {
            vs: vs.ok_or("no vertex shader")?,
            ps_y: ps_y.ok_or("no Y pixel shader")?,
            ps_uv: ps_uv.ok_or("no UV pixel shader")?,
            cb: cb.ok_or("no constant buffer")?,
            rs: rs.ok_or("no rasterizer state")?,
            scratch: None,
        })
    }

    fn ensure_scratch(&mut self, device: &ID3D11Device, w: u32, h: u32, format: SurfaceFormat) -> Result<(), String> {
        let fits = self.scratch.as_ref().is_some_and(|s| s.w == w && s.h == h && s.format == format);
        if !fits {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: format.dxgi(),
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            // SAFETY: valid descriptor and out-pointer on live device.
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(|e| format!("cannot create scratch YUV texture: {e}"))?;
            let tex = tex.ok_or("no scratch YUV texture")?;
            let (fmt_y, fmt_uv) = format.plane_dxgi();
            let srv_y = create_tex2d_srv(device, &tex, fmt_y)?;
            let srv_uv = create_tex2d_srv(device, &tex, fmt_uv)?;
            self.scratch = Some(ScratchSurface { tex, srv_y, srv_uv, w, h, format });
        }
        Ok(())
    }

    fn dispatch(&self, context: &ID3D11DeviceContext, src: &ID3D11Texture2D, index: u32, pair: &SharedPlanePair, (ox, oy): (u32, u32), query: &ID3D11Query) {
        let Some(scratch) = self.scratch.as_ref() else { return };
        let cb_y: [i32; 4] = [ox as i32, oy as i32, 0, 0];
        let cb_uv: [i32; 4] = [(ox / 2) as i32, (oy / 2) as i32, 0, 0];
        let vp_y = [D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: pair.w as f32, Height: pair.h as f32, MinDepth: 0.0, MaxDepth: 1.0 }];
        let vp_uv = [D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: pair.cw as f32, Height: pair.ch as f32, MinDepth: 0.0, MaxDepth: 1.0 }];
        // SAFETY: `src` and `scratch.tex` share format and dimensions; shaders, RTVs, SRVs, and
        // constant buffer belong to the same device and are accessed under `Gpu::gate`. All
        // bound SRVs/RTVs are cleared before returning and an event query confirms GPU completion
        // before exporting the shared handles to Vulkan.
        unsafe {
            context.CopySubresourceRegion(&scratch.tex, 0, 0, 0, 0, src, index, None);
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            context.RSSetState(&self.rs);
            context.OMSetBlendState(None, None, u32::MAX);
            context.OMSetDepthStencilState(None, 0);
            context.VSSetShader(&self.vs, None);
            context.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));

            context.UpdateSubresource(&self.cb, 0, None, cb_y.as_ptr() as *const c_void, 0, 0);
            context.RSSetViewports(Some(&vp_y));
            context.OMSetRenderTargets(Some(&[Some(pair.rtv_y.clone())]), None);
            context.PSSetShader(&self.ps_y, None);
            context.PSSetShaderResources(0, Some(&[Some(scratch.srv_y.clone())]));
            context.Draw(3, 0);

            context.UpdateSubresource(&self.cb, 0, None, cb_uv.as_ptr() as *const c_void, 0, 0);
            context.RSSetViewports(Some(&vp_uv));
            context.OMSetRenderTargets(Some(&[Some(pair.rtv_uv.clone())]), None);
            context.PSSetShader(&self.ps_uv, None);
            context.PSSetShaderResources(0, Some(&[Some(scratch.srv_uv.clone())]));
            context.Draw(3, 0);

            context.PSSetShaderResources(0, Some(&[None]));
            context.OMSetRenderTargets(None, None);
            context.End(query);
            context.Flush();
        }
    }
}

fn create_tex2d_srv(device: &ID3D11Device, tex: &ID3D11Texture2D, format: DXGI_FORMAT) -> Result<ID3D11ShaderResourceView, String> {
    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 } },
    };
    let mut srv: Option<ID3D11ShaderResourceView> = None;
    // SAFETY: `tex` was created with `D3D11_BIND_SHADER_RESOURCE` and `format` is a valid plane view format.
    unsafe { device.CreateShaderResourceView(tex, Some(&desc), Some(&mut srv)) }.map_err(|e| format!("CreateShaderResourceView failed: {e}"))?;
    srv.ok_or_else(|| "no SRV".to_string())
}

fn ensure_staging_plane<'a>(
    device: &ID3D11Device,
    slot: &'a mut Option<(ID3D11Texture2D, u32, u32, DXGI_FORMAT)>,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
) -> Result<&'a ID3D11Texture2D, String> {
    if !slot.as_ref().is_some_and(|(_, sw, sh, sf)| *sw == w && *sh == h && *sf == format) {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        // SAFETY: valid staging descriptor and out-pointer on live device.
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(|e| format!("cannot create staging fallback texture: {e}"))?;
        *slot = Some((tex.ok_or("no staging texture")?, w, h, format));
    }
    slot.as_ref().map(|(t, _, _, _)| t).ok_or_else(|| "no staging texture".to_string())
}

fn map_staging_bytes(
    context: &ID3D11DeviceContext,
    src: &ID3D11Texture2D,
    staging: &ID3D11Texture2D,
    row_bytes: u32,
    rows: u32,
) -> Result<(Vec<u8>, usize), String> {
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: `src` and `staging` match in size and format on the same device under `Gpu::gate`.
    unsafe {
        context.CopyResource(staging, src);
        context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| format!("cannot map shared surface: {e}"))?;
    }
    let res = (|| {
        let stride = mapped.RowPitch as usize;
        if mapped.pData.is_null() || stride < row_bytes as usize {
            return Err("mapped shared surface has invalid stride".to_string());
        }
        let len = stride.checked_mul(rows as usize).ok_or("mapped surface size overflows")?;
        // SAFETY: `Map` succeeded and `pData` spans `stride * rows` bytes until `Unmap` below.
        let src_bytes = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, len) };
        Ok((src_bytes.to_vec(), stride))
    })();
    // SAFETY: subresource 0 was mapped above and `src_bytes` is dropped before `Unmap`.
    unsafe { context.Unmap(staging, 0) };
    res
}

struct RawPlane<'a> {
    bytes: &'a [u8],
    stride: usize,
}

fn deinterleave_planes(format: SurfaceFormat, (w, h): (usize, usize), (cw, ch): (usize, usize), y_plane: RawPlane<'_>, uv_plane: RawPlane<'_>) -> PixelData {
    let (raw_y, stride_y) = (y_plane.bytes, y_plane.stride);
    let (raw_uv, stride_uv) = (uv_plane.bytes, uv_plane.stride);
    if format == SurfaceFormat::Nv12 {
        let mut yp = pool::take_u8(w * h);
        for y in 0..h {
            yp.extend_from_slice(&raw_y[y * stride_y..y * stride_y + w]);
        }
        let (mut u, mut v) = (pool::take_u8(cw * ch), pool::take_u8(cw * ch));
        for y in 0..ch {
            let pairs = raw_uv[y * stride_uv..y * stride_uv + cw * 2].as_chunks::<2>().0;
            u.extend(pairs.iter().map(|p| p[0]));
            v.extend(pairs.iter().map(|p| p[1]));
        }
        PixelData::Yuv8 { planes: [Arc::new(yp), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None }
    } else {
        let mut yp = pool::take_u16(w * h);
        for y in 0..h {
            yp.extend(raw_y[y * stride_y..y * stride_y + w * 2].as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b) >> 6));
        }
        let (mut u, mut v) = (pool::take_u16(cw * ch), pool::take_u16(cw * ch));
        for y in 0..ch {
            let quads = raw_uv[y * stride_uv..y * stride_uv + cw * 4].as_chunks::<4>().0;
            u.extend(quads.iter().map(|q| u16::from_le_bytes([q[0], q[1]]) >> 6));
            v.extend(quads.iter().map(|q| u16::from_le_bytes([q[2], q[3]]) >> 6));
        }
        PixelData::Yuv16 { planes: [Arc::new(yp), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, bits: 10, alpha: None }
    }
}

fn adapter_name(device: &ID3D11Device) -> Option<String> {
    let dxgi: IDXGIDevice = device.cast().ok()?;
    // SAFETY: plain COM calls on live interfaces; the description is a plain struct filled by DXGI.
    let desc = unsafe { dxgi.GetAdapter().ok()?.GetDesc().ok()? };
    let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
    Some(String::from_utf16_lossy(desc.Description.get(..len)?))
}

/// Reads decoded surfaces back into system memory through a staging texture it keeps between
/// pictures (recreated when the surface format or size changes).
#[derive(Default)]
pub struct Readback {
    staging: Option<(ID3D11Texture2D, D3D11_TEXTURE2D_DESC)>,
}

// SAFETY: the staging texture belongs to the device `Gpu` makes thread-safe; a `Readback` is used
// by one thread at a time (`&mut self`) and every use of the context is behind the `Gpu` gate.
unsafe impl Send for Readback {}

impl Readback {
    /// Copy the picture in `sample` (a decoder output sample backed by a Direct3D 11 texture) into
    /// system memory and hand it to `f` as a mapped biplanar surface. Errors when the sample is
    /// not texture-backed, which means the decoder did not use the GPU.
    pub fn read<R>(&mut self, gpu: &Gpu, sample: &IMFSample, format: SurfaceFormat, f: impl FnOnce(&Biplanar) -> Result<R, String>) -> Result<R, String> {
        let r = self.read_inner(gpu, sample, format, f);
        if r.is_err() {
            gpu.note_failure();
        }
        r
    }

    fn read_inner<R>(&mut self, gpu: &Gpu, sample: &IMFSample, format: SurfaceFormat, f: impl FnOnce(&Biplanar) -> Result<R, String>) -> Result<R, String> {
        let (tex, index, desc) = extract_sample_texture(sample, format)?;
        let staging = self.staging_for(gpu, &desc)?;
        let _gate = gpu.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures belong to this device; the copy and the mapping happen under the
        // context gate. After a successful `Map` the memory at `pData` holds `RowPitch` bytes per
        // row for `Height` luma rows followed by `Height / 2` chroma rows (the NV12 / P010 staging
        // layout), valid until `Unmap`, which runs before this block ends.
        unsafe {
            gpu.context.CopySubresourceRegion(staging, 0, 0, 0, 0, &tex, index, None);
            gpu.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| format!("cannot map the decoded surface: {e}"))?;
        }
        let result = (|| {
            let stride = mapped.RowPitch as usize;
            let rows = desc.Height as usize;
            let luma_len = stride.checked_mul(rows).ok_or("surface size overflows")?;
            let chroma_len = stride.checked_mul(rows.div_ceil(2)).ok_or("surface size overflows")?;
            if mapped.pData.is_null() || stride == 0 {
                return Err("the decoded surface is not mapped".to_string());
            }
            // SAFETY: see the `Map` call: `luma_len + chroma_len` bytes are mapped at `pData`.
            let all = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, luma_len + chroma_len) };
            let (luma, chroma) = all.split_at(luma_len);
            filmcraft_frame::transfers::record_decode_readback((luma_len + chroma_len) as u64);
            f(&Biplanar { luma, chroma, stride, rows })
        })();
        // SAFETY: the subresource was mapped above and nothing borrowed from it outlives `result`.
        unsafe { gpu.context.Unmap(staging, 0) };
        result
    }

    fn staging_for(&mut self, gpu: &Gpu, src: &D3D11_TEXTURE2D_DESC) -> Result<&ID3D11Texture2D, String> {
        let fits = self.staging.as_ref().is_some_and(|(_, d)| d.Width == src.Width && d.Height == src.Height && d.Format == src.Format);
        if !fits {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: src.Width,
                Height: src.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: src.Format,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            // SAFETY: a valid descriptor and out-pointer; no initial data.
            unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(|e| format!("cannot create the readback texture: {e}"))?;
            self.staging = Some((tex.ok_or("no readback texture")?, desc));
        }
        self.staging.as_ref().map(|(t, _)| t).ok_or_else(|| "no readback texture".to_string())
    }
}
