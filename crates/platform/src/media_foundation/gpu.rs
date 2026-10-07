//! The Direct3D 11 side of the Windows decoder: the video-capable device shared by every decoder
//! (with the `IMFDXGIDeviceManager` the decoder MFTs get it through), what DXVA can decode on it,
//! and the readback of a decoded surface into system memory (GPU to staging texture to bytes).
//!
//! FFI module (docs/adr/0001-platform-ffi.md): every `unsafe` block has a `// SAFETY:` comment,
//! the public items are safe and return `Result<_, String>`, no COM pointer leaves this module
//! except as an opaque handle for the MFT wrapper.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use windows::Win32::Foundation::{HMODULE, RPC_E_CHANGED_MODE};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11_VIDEO_DECODER_DESC, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext,
    ID3D11Multithread, ID3D11Query, ID3D11Texture2D, ID3D11VideoDevice,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010};
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::Win32::Media::MediaFoundation::{IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSample};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::{BOOL, GUID, Interface};

use super::mft::MfApi;
use crate::biplanar::Biplanar;

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

/// A Direct3D 11 device created for video decoding, with its DXGI device manager.
pub struct Gpu {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    manager: IMFDXGIDeviceManager,
    name: String,
    /// The adapter's LUID (low part, high part): what a Direct3D 12 device must match to share
    /// memory with this one.
    luid: Option<(u32, i32)>,
    /// Set when the device was removed or reset: new decoders get a new device.
    lost: AtomicBool,
    /// Serialises our use of the immediate context (the device is also multithread protected,
    /// which covers the decoder MFTs' own use of it).
    gate: Mutex<()>,
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
        let (name, luid) = adapter_identity(&device);
        let name = name.unwrap_or_else(|| "an unknown adapter".into());
        Ok(Gpu { device, context, video, manager, name, luid, lost: AtomicBool::new(false), gate: Mutex::new(()) })
    }

    fn usable(&self) -> bool {
        // SAFETY: a plain COM call on a live interface.
        !self.lost.load(Ordering::Relaxed) && unsafe { self.device.GetDeviceRemovedReason() }.is_ok()
    }

    pub(super) fn device(&self) -> &ID3D11Device {
        &self.device
    }

    #[cfg(test)]
    pub(super) fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    pub(super) fn luid(&self) -> Option<(u32, i32)> {
        self.luid
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

    /// GPU to GPU copy of the rectangle `crop` (x, y, width, height; even coordinates) of slice
    /// `src_index` of `src` into `dst` (the top left corner of a texture of that size): the decoder
    /// output into a surface the renderer can open. Nothing crosses to the CPU.
    pub fn copy_picture(&self, dst: &ID3D11Texture2D, src: &ID3D11Texture2D, src_index: u32, crop: (u32, u32, u32, u32)) -> Result<(), String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a plain COM call writing a descriptor struct.
        unsafe { src.GetDesc(&mut desc) };
        if crop.0.saturating_add(crop.2) > desc.Width || crop.1.saturating_add(crop.3) > desc.Height || src_index >= desc.ArraySize.max(1) {
            return Err(format!(
                "the decoder's {}x{} surface does not hold the {}x{} picture at {},{}",
                desc.Width, desc.Height, crop.2, crop.3, crop.0, crop.1
            ));
        }
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let region = D3D11_BOX { left: crop.0, top: crop.1, front: 0, right: crop.0.saturating_add(crop.2), bottom: crop.1.saturating_add(crop.3), back: 1 };
        // SAFETY: both textures belong to this device; the context is used under the gate and the
        // box lies inside the source (the decoder output is at least the coded size).
        unsafe { self.context.CopySubresourceRegion(dst, 0, 0, 0, 0, src, src_index, Some(&region)) };
        Ok(())
    }

    /// Block until every command given to the Direct3D 11 device so far has finished on the GPU
    /// (an event query): what hands a surface written by Direct3D 11 over to another device. It
    /// waits for the GPU only; no pixel is copied.
    pub fn wait_for_gpu(&self) -> Result<(), String> {
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let desc = D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 };
        let mut query: Option<ID3D11Query> = None;
        // SAFETY: a valid descriptor and out-pointer; the query and the context are used under the
        // gate. `GetData` writes one BOOL (the size passed) into `done`.
        unsafe {
            self.device.CreateQuery(&desc, Some(&mut query)).map_err(|e| format!("cannot create a GPU query: {e}"))?;
            let query = query.ok_or("no GPU query")?;
            self.context.End(&query);
            self.context.Flush();
            let start = std::time::Instant::now();
            loop {
                let mut done = BOOL(0);
                let hr = self.context.GetData(&query, Some((&mut done as *mut BOOL).cast()), std::mem::size_of::<BOOL>() as u32, 0);
                if hr.is_ok() && done.as_bool() {
                    return Ok(());
                }
                if hr.is_err() || start.elapsed() > std::time::Duration::from_secs(2) {
                    return Err("the GPU did not finish in time".into());
                }
                std::thread::yield_now();
            }
        }
    }

    /// A failed call may mean the device is gone: if so, mark it so new decoders get a fresh one.
    pub fn note_failure(&self) {
        if !self.usable() {
            log::warn!("Direct3D 11 device lost ({}); new decoders will create another", self.name);
            self.lost.store(true, Ordering::Relaxed);
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
}

fn adapter_identity(device: &ID3D11Device) -> (Option<String>, Option<(u32, i32)>) {
    let Ok(dxgi) = device.cast::<IDXGIDevice>() else { return (None, None) };
    // SAFETY: plain COM calls on live interfaces; the description is a plain struct filled by DXGI.
    let Some(desc) = (unsafe { dxgi.GetAdapter().ok().and_then(|a| a.GetDesc().ok()) }) else { return (None, None) };
    let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
    (desc.Description.get(..len).map(String::from_utf16_lossy), Some((desc.AdapterLuid.LowPart, desc.AdapterLuid.HighPart)))
}

/// The Direct3D 11 texture (and array slice) a decoder output sample is backed by. Errors when the
/// sample is in system memory, which means the decoder did not use the GPU.
pub fn sample_texture(sample: &IMFSample) -> Result<(ID3D11Texture2D, u32), String> {
    // SAFETY: plain COM calls on live interfaces; `GetResource` writes one interface pointer
    // (a new reference) into `tex`, which `Option<ID3D11Texture2D>` owns and releases.
    unsafe {
        let buffer = sample.GetBufferByIndex(0).map_err(|e| format!("decoder output has no buffer: {e}"))?;
        let dxgi: IMFDXGIBuffer = buffer.cast().map_err(|_| "decoder output is in system memory: the decoder is not using the GPU".to_string())?;
        let mut tex: Option<ID3D11Texture2D> = None;
        dxgi.GetResource(&ID3D11Texture2D::IID, &mut tex as *mut Option<ID3D11Texture2D> as *mut *mut c_void)
            .map_err(|e| format!("decoder output is not a 2D texture: {e}"))?;
        Ok((tex.ok_or("decoder output has no texture")?, dxgi.GetSubresourceIndex().map_err(|e| format!("no subresource index: {e}"))?))
    }
}

/// Reads decoded surfaces back into system memory through a staging texture it keeps between
/// pictures (recreated when the surface format or size changes). This is the GPU to CPU copy of
/// the backend: a zero-copy path would share the surface with the renderer instead.
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
        let (tex, index) = sample_texture(sample)?;
        self.read_texture(gpu, &tex, index, format, f)
    }

    /// As [`Readback::read`] for a texture (and array slice) already in hand: the staging copy,
    /// the mapping, and `f` over the mapped planes.
    pub fn read_texture<R>(
        &mut self,
        gpu: &Gpu,
        tex: &ID3D11Texture2D,
        index: u32,
        format: SurfaceFormat,
        f: impl FnOnce(&Biplanar) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a plain COM call writing a descriptor struct.
        unsafe { tex.GetDesc(&mut desc) };
        if desc.Format != format.dxgi() {
            return Err(format!("decoded surface has DXGI format {} instead of {format:?}", desc.Format.0));
        }
        if desc.Width == 0 || desc.Height == 0 || desc.Width > 16384 || desc.Height > 16384 {
            return Err(format!("decoded surface is {}x{}", desc.Width, desc.Height));
        }
        let staging = self.staging_for(gpu, &desc)?;
        let _gate = gpu.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: both textures belong to this device; the copy and the mapping happen under the
        // context gate. After a successful `Map` the memory at `pData` holds `RowPitch` bytes per
        // row for `Height` luma rows followed by `Height / 2` chroma rows (the NV12 / P010 staging
        // layout), valid until `Unmap`, which runs before this block ends.
        unsafe {
            gpu.context.CopySubresourceRegion(staging, 0, 0, 0, 0, tex, index, None);
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
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
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
