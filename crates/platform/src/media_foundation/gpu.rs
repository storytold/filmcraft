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
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_STAGING, D3D11_VIDEO_DECODER_DESC, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, ID3D11VideoDevice,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIDevice, IDXGIFactory1};
use windows::Win32::Media::MediaFoundation::{IMFDXGIBuffer, IMFDXGIDeviceManager, IMFSample};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::core::{GUID, Interface};

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
    /// The adapter's PCI vendor and device ids (None when DXGI does not say).
    ids: Option<(u32, u32)>,
    /// The preferred adapter this device was made for ([`set_preferred_adapter`]): a device made
    /// before a preference was set, or for another one, is replaced for new decoders.
    made_for: Option<(u32, u32)>,
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

/// The GPU decoding should run on, by PCI vendor and device id: the one the app renders with.
static PREFERRED: Mutex<Option<(u32, u32)>> = Mutex::new(None);

/// The environment variable that moves decoding onto the GPU the app renders with (`renderer`).
/// Off by default: until decoded surfaces are shared with the renderer (zero-copy), every picture
/// is read back to system memory, and an integrated GPU, whose memory is system memory, reads back
/// faster than a discrete one over PCIe. Measured on a Core Ultra 9 + RTX 5070 Ti laptop with 4K
/// H.264: 8.5 ms per sample on the integrated GPU against 7.6 to 10.3 ms on the RTX, and slower
/// frame fetches (docs/performance.md).
pub const DECODE_ADAPTER_ENV: &str = "FILMCRAFT_DECODE_ADAPTER";

/// Whether [`DECODE_ADAPTER_ENV`] asks for the renderer's GPU.
pub fn follow_renderer_requested(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim().eq_ignore_ascii_case("renderer"))
}

/// Decode on the GPU with these PCI ids (the renderer's) instead of Windows' default adapter, when
/// [`DECODE_ADAPTER_ENV`] is `renderer`; otherwise nothing changes. Decoders made before the call
/// keep their device; new ones use the preferred adapter, or the default one when no adapter
/// matches or a device cannot be made on it.
pub fn set_preferred_adapter(vendor: u32, device: u32) {
    if !follow_renderer_requested(std::env::var(DECODE_ADAPTER_ENV).ok().as_deref()) {
        return;
    }
    log::info!("hardware decoding follows the renderer ({DECODE_ADAPTER_ENV}=renderer): {vendor:04x}:{device:04x}");
    *PREFERRED.lock().unwrap_or_else(PoisonError::into_inner) = Some((vendor, device));
}

impl Gpu {
    /// The process-wide device (created on first use, replaced after a device loss or when the
    /// preferred adapter changed since it was made).
    pub fn shared(api: &MfApi) -> Result<Arc<Gpu>, String> {
        ensure_com()?;
        let want = *PREFERRED.lock().unwrap_or_else(PoisonError::into_inner);
        let mut slot = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(g) = slot.as_ref().filter(|g| g.usable() && g.made_for == want) {
            return Ok(g.clone());
        }
        let g = Arc::new(Gpu::create_preferred(api, want)?);
        match g.ids {
            Some((v, d)) => log::info!("hardware decoding: Direct3D 11 device on {} ({v:04x}:{d:04x})", g.name),
            None => log::info!("hardware decoding: Direct3D 11 device on {}", g.name),
        }
        *slot = Some(g.clone());
        Ok(g)
    }

    /// A device on the adapter with the `want` ids, else on the default adapter.
    fn create_preferred(api: &MfApi, want: Option<(u32, u32)>) -> Result<Gpu, String> {
        if let Some((vendor, device)) = want {
            match find_adapter(vendor, device).and_then(|a| Gpu::create(api, Some(&a))) {
                Ok(g) => return Ok(Gpu { made_for: want, ..g }),
                Err(e) => log::warn!("hardware decoding: no device on the renderer's GPU ({vendor:04x}:{device:04x}: {e}); using the default adapter"),
            }
        }
        Gpu::create(api, None).map(|g| Gpu { made_for: want, ..g })
    }

    fn create(api: &MfApi, adapter: Option<&IDXGIAdapter>) -> Result<Gpu, String> {
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let (mut device, mut context) = (None, None);
        // (an explicit adapter takes the "unknown" driver type; the hardware type means the default adapter)
        let driver = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };
        // SAFETY: the out-pointers refer to live locals; the adapter, when given, is a live
        // interface, and the feature level slice outlives the call.
        unsafe {
            D3D11CreateDevice(
                adapter,
                driver,
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
        let (name, ids) = adapter_desc(&device).map_or_else(|| ("an unknown adapter".into(), None), |(n, v, d)| (n, Some((v, d))));
        Ok(Gpu { device, context, video, manager, name, ids, made_for: None, lost: AtomicBool::new(false), gate: Mutex::new(()) })
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
}

/// The device's adapter: description, PCI vendor id, PCI device id.
fn adapter_desc(device: &ID3D11Device) -> Option<(String, u32, u32)> {
    let dxgi: IDXGIDevice = device.cast().ok()?;
    // SAFETY: plain COM calls on live interfaces; the description is a plain struct filled by DXGI.
    let desc = unsafe { dxgi.GetAdapter().ok()?.GetDesc().ok()? };
    let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
    Some((String::from_utf16_lossy(desc.Description.get(..len)?), desc.VendorId, desc.DeviceId))
}

/// The first adapter DXGI lists with these PCI ids.
fn find_adapter(vendor: u32, device: u32) -> Result<IDXGIAdapter, String> {
    // SAFETY: CreateDXGIFactory1 returns an owned interface; EnumAdapters1 is called with indices
    // from 0 up until it fails (DXGI_ERROR_NOT_FOUND past the last adapter); GetDesc1 fills a
    // plain struct. Every interface is released when dropped.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| format!("no DXGI factory: {e}"))?;
        // (bounded: a system lists a handful of adapters)
        for i in 0..64u32 {
            let Ok(adapter) = factory.EnumAdapters1(i) else { break };
            let Ok(desc) = adapter.GetDesc1() else { continue };
            if desc.VendorId == vendor && desc.DeviceId == device {
                return adapter.cast::<IDXGIAdapter>().map_err(|e| format!("adapter interface: {e}"));
            }
        }
    }
    Err("no DXGI adapter with those ids".into())
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
        // SAFETY: plain COM calls on live interfaces; `GetResource` writes one interface pointer
        // (a new reference) into `tex`, which `Option<ID3D11Texture2D>` owns and releases.
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

#[cfg(test)]
mod tests {
    use super::follow_renderer_requested;

    #[test]
    fn only_renderer_moves_decoding() {
        assert!(follow_renderer_requested(Some("renderer")));
        assert!(follow_renderer_requested(Some(" Renderer ")));
        for v in [None, Some(""), Some("default"), Some("1"), Some("rtx")] {
            assert!(!follow_renderer_requested(v), "{v:?}");
        }
    }
}
