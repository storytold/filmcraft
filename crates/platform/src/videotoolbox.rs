//! VideoToolbox (macOS) hardware H.264 / HEVC / VP9 / AV1 decoding.
//!
//! The only module of the workspace with `unsafe` code besides the other FFI modules of this crate
//! (docs/adr/0001-platform-ffi.md). Rules: every `unsafe` block has a `// SAFETY:` comment; no
//! panic may unwind into VideoToolbox (the output callback runs under `catch_unwind`); every
//! failure is a `Result` the caller ([`crate::hybrid::HybridDecoder`]) answers by switching to the
//! software decoder.
//!
//! A session is created from the parameter sets of the sample entry (`avcC` / `hvcC`; for VP9 and
//! AV1 the `vpcC` / `av1C` box, with Apple's supplemental decoder for the codec registered first)
//! with hardware decoding required, so streams the hardware cannot take are declined up front. Samples
//! are fed as `CMSampleBuffer`s with asynchronous decompression (a couple in flight); the output
//! callback (on a VideoToolbox thread) copies each decoded
//! `CVPixelBuffer` (NV12 / P010-style biplanar) into planar [`PixelData::Yuv8`] /
//! [`PixelData::Yuv16`]. VideoToolbox emits pictures in decoding order, so a reorder buffer of the
//! stream's own depth (`max_num_reorder_frames` / `sps_max_num_reorder_pics`) puts them in
//! presentation order, as the software decoders do. VP9 and AV1 have no reordering: WebM / MP4 pack
//! hidden alt-ref frames into superframes / temporal units, so every sample is one shown picture.
//! An AV1 temporal unit goes in as it is (its OBUs, no temporal delimiter), with the `av1C`
//! configuration OBUs in front of a random-access unit that carries no sequence header of its own.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use filmcraft_codecs::hw::{FrameCodec, FrameStreamInfo, NalCodec, NalStreamInfo};
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};
use filmcraft_frame::{Chroma, NativeSurface, PixelData, VideoFrame, pool};
use filmcraft_isobmff::{Av1Config, VpcConfig};
use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags, CMVideoFormatDescriptionCreate,
    CMVideoFormatDescriptionCreateFromH264ParameterSets, CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
    kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms,
};
use objc2_core_video::{
    CVImageBuffer, CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount, CVPixelBufferGetWidth,
    CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey,
};
use objc2_metal::{MTLPixelFormat, MTLTextureType};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
    kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
};

/// `CVPixelBuffer` formats we ask VideoToolbox for: biplanar Y + interleaved CbCr, 8-bit or
/// 10-bit in the high bits of 16-bit words.
const NV12_VIDEO: u32 = u32::from_be_bytes(*b"420v");
const NV12_FULL: u32 = u32::from_be_bytes(*b"420f");
const P010_VIDEO: u32 = u32::from_be_bytes(*b"x420");
const P010_FULL: u32 = u32::from_be_bytes(*b"xf20");
const NV16_VIDEO: u32 = u32::from_be_bytes(*b"422v");
const NV16_FULL: u32 = u32::from_be_bytes(*b"422f");
const P210_VIDEO: u32 = u32::from_be_bytes(*b"x422");
const P210_FULL: u32 = u32::from_be_bytes(*b"xf22");
/// `kCMVideoCodecType_VP9`.
const VP9: u32 = u32::from_be_bytes(*b"vp09");
/// `kCMVideoCodecType_AV1`.
const AV1: u32 = u32::from_be_bytes(*b"av01");

/// Maximum number of `IOSurface`-backed `CVPixelBuffer`s retained across decoded frames before
/// older buffers are materialised into pooled CPU planes and released back to VideoToolbox's pool.
const MAX_RETAINED_SURFACES: usize = 8;

/// How decoded buffers are laid out and what each output picture carries.
#[derive(Clone, Debug)]
struct Layout {
    pixel_format: u32,
    /// Coded size and output rectangle (luma samples).
    coded: (u32, u32),
    crop: (u32, u32, u32, u32),
    chroma: Chroma,
    /// Significant bits per sample (8 = `Yuv8`).
    bits: u32,
    color: filmcraft_color::ColorInfo,
    par: (u32, u32),
    alpha: bool,
}

/// One output-callback result.
enum Output {
    Frame(DecodedFrame),
    Failed(String),
}

/// A decoded biplanar `CVImageBuffer` kept alive for zero-copy Metal import, with lazy CPU
/// plane materialisation when CPU paths or pool eviction need the texels.
pub(crate) struct VtSurface {
    pb: Mutex<Option<CFRetained<CVImageBuffer>>>,
    layout: Layout,
    ox: usize,
    oy: usize,
    w: usize,
    h: usize,
    u8_planes: Mutex<Option<[Arc<Vec<u8>>; 2]>>,
    u16_planes: Mutex<Option<[Arc<Vec<u16>>; 2]>>,
}

// SAFETY: `CVImageBuffer` (`CVPixelBuffer`) is a CoreFoundation / IOSurface-backed object safe
// to retain, release, and lock for read-only access or Metal texture creation across threads;
// interior state is synchronized by `Mutex`.
unsafe impl Send for VtSurface {}
// SAFETY: see `Send` implementation above; all mutable access goes through `Mutex`.
unsafe impl Sync for VtSurface {}

impl std::fmt::Debug for VtSurface {
    /// Describe the retained surface dimensions and bit depth without reading its pixels.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtSurface").field("w", &self.w).field("h", &self.h).field("bits", &self.layout.bits).finish()
    }
}

impl VtSurface {
    /// Materialise the CPU planes (if not already done) and release the `CVImageBuffer` back to
    /// VideoToolbox's pool. A failed copy keeps the original buffer available for another read.
    fn evict_to_cpu(&self) -> std::result::Result<(), String> {
        if self.layout.bits <= 8 {
            self.read_planes_u8()?;
        } else {
            self.read_planes_u16()?;
        }
        *self.pb.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }

    /// Copy and cache 8-bit biplanar pixels; retain the native buffer if copying fails.
    fn read_planes_u8(&self) -> std::result::Result<[Arc<Vec<u8>>; 2], String> {
        let mut cache = self.u8_planes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(planes) = cache.as_ref() {
            return Ok(planes.clone());
        }
        let pb_guard = self.pb.lock().unwrap_or_else(PoisonError::into_inner);
        let pb = pb_guard.as_ref().ok_or("decoded picture has no backing buffer or CPU planes")?;
        let planes = copy_biplanar_u8(pb, &self.layout, self.ox, self.oy, self.w, self.h)?;
        *cache = Some(planes.clone());
        Ok(planes)
    }

    /// Copy and cache high-aligned 16-bit biplanar storage without discarding a failed source.
    fn read_planes_u16(&self) -> std::result::Result<[Arc<Vec<u16>>; 2], String> {
        let mut cache = self.u16_planes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(planes) = cache.as_ref() {
            return Ok(planes.clone());
        }
        let pb_guard = self.pb.lock().unwrap_or_else(PoisonError::into_inner);
        let pb = pb_guard.as_ref().ok_or("decoded picture has no backing buffer or CPU planes")?;
        let planes = copy_biplanar_u16(pb, &self.layout, self.ox, self.oy, self.w, self.h)?;
        *cache = Some(planes.clone());
        Ok(planes)
    }
}

impl NativeSurface for VtSurface {
    /// Resolve 8-bit CPU planes, logging a readback failure before returning the legacy absent-data result.
    fn planes_u8(&self) -> Option<[Arc<Vec<u8>>; 2]> {
        if self.layout.bits > 8 {
            return None;
        }
        self.read_planes_u8().inspect_err(|e| log::error!("VideoToolbox {}x{} 8-bit CPU plane readback failed: {e}", self.w, self.h)).ok()
    }

    /// Resolve 16-bit CPU planes, logging a readback failure before returning the legacy absent-data result.
    fn planes_u16(&self) -> Option<[Arc<Vec<u16>>; 2]> {
        if self.layout.bits <= 8 {
            return None;
        }
        self.read_planes_u16().inspect_err(|e| log::error!("VideoToolbox {}x{} {}-bit CPU plane readback failed: {e}", self.w, self.h, self.layout.bits)).ok()
    }

    /// Take cached 8-bit planes for recycling and drop this surface's native buffer reference.
    fn take_planes_u8(&self) -> Option<[Arc<Vec<u8>>; 2]> {
        *self.pb.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.u8_planes.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    /// Take cached 16-bit planes for recycling and drop this surface's native buffer reference.
    fn take_planes_u16(&self) -> Option<[Arc<Vec<u16>>; 2]> {
        *self.pb.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.u16_planes.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

struct MetalCache {
    device_addr: usize,
    cache: CFRetained<CVMetalTextureCache>,
}

// SAFETY: `CVMetalTextureCache` is a CoreFoundation object whose access is serialized by
// `METAL_CACHE`'s `Mutex`.
unsafe impl Send for MetalCache {}

static METAL_CACHE: Mutex<Option<MetalCache>> = Mutex::new(None);

/// Import a `VtSurface`'s `IOSurface`-backed `CVImageBuffer` directly into [`wgpu::TextureView`]s
/// for plane 0 (`R8Unorm` / `R16Unorm`) and plane 1 (`Rg8Unorm` / `Rg16Unorm`) without copying
/// texel data through CPU memory.
pub fn import_metal_surface(device: &wgpu::Device, surface: &dyn NativeSurface, w: u32, h: u32, chroma: Chroma, bits: u32) -> Option<[wgpu::TextureView; 2]> {
    let vt = (surface as &dyn std::any::Any).downcast_ref::<VtSurface>()?;
    if (vt.ox, vt.oy) != (0, 0) || (vt.w, vt.h) != (w as usize, h as usize) || vt.layout.chroma != chroma || vt.layout.bits != bits {
        return None;
    }
    if bits > 8 && !device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) {
        return None;
    }
    let pb = vt.pb.lock().unwrap_or_else(PoisonError::into_inner).clone()?;
    let raw_mtl_dev = {
        // SAFETY: accessing the underlying Metal HAL device purely to clone its `MTLDevice` handle
        // before dropping the HAL guard.
        let hal_dev = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?;
        hal_dev.raw_device().clone()
    };
    let dev_addr = objc2::rc::Retained::as_ptr(&raw_mtl_dev) as usize;
    let mut guard = METAL_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.as_ref().is_none_or(|c| c.device_addr != dev_addr) {
        let mut cache_ptr: *mut CVMetalTextureCache = std::ptr::null_mut();
        // SAFETY: `raw_mtl_dev` is a valid Metal device; `cache_ptr` is a valid output pointer.
        let status = unsafe { CVMetalTextureCache::create(None, None, &raw_mtl_dev, None, NonNull::from(&mut cache_ptr)) };
        let cache_ptr = NonNull::new(cache_ptr).filter(|_| status == 0)?;
        // SAFETY: `CVMetalTextureCache::create` returns a retained (+1) cache on status 0.
        let cache = unsafe { CFRetained::from_raw(cache_ptr) };
        *guard = Some(MetalCache { device_addr: dev_addr, cache });
    }
    let cache = &guard.as_ref()?.cache;
    let (sx, sy) = chroma.shifts();
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    let (y_mtl, uv_mtl, y_wgpu, uv_wgpu) = if bits > 8 {
        (MTLPixelFormat::R16Unorm, MTLPixelFormat::RG16Unorm, wgpu::TextureFormat::R16Unorm, wgpu::TextureFormat::Rg16Unorm)
    } else {
        (MTLPixelFormat::R8Unorm, MTLPixelFormat::RG8Unorm, wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm)
    };
    let y_view = import_plane(device, cache, &pb, 0, w, h, y_mtl, y_wgpu)?;
    let uv_view = import_plane(device, cache, &pb, 1, cw, ch, uv_mtl, uv_wgpu)?;
    Some([y_view, uv_view])
}

#[expect(dead_code, reason = "held alive until the wgpu HAL texture is dropped")]
struct RetainedPlaneHolders(CFRetained<CVMetalTexture>, CFRetained<CVImageBuffer>);

// SAFETY: `CFRetained` reference-counting (`CFRetain` / `CFRelease`) on `CVMetalTexture` and
// `CVImageBuffer` is thread-safe in CoreFoundation / CoreVideo.
unsafe impl Send for RetainedPlaneHolders {}
// SAFETY: `RetainedPlaneHolders` is only dropped once by the `FnOnce` drop callback; no interior
// mutation occurs across threads.
unsafe impl Sync for RetainedPlaneHolders {}

/// Wrap one CoreVideo plane as a Metal view, retaining its native owners until the GPU texture is dropped.
#[allow(clippy::too_many_arguments)]
fn import_plane(
    device: &wgpu::Device,
    cache: &CVMetalTextureCache,
    pb: &CFRetained<CVImageBuffer>,
    plane: usize,
    w: u32,
    h: u32,
    mtl_format: MTLPixelFormat,
    wgpu_format: wgpu::TextureFormat,
) -> Option<wgpu::TextureView> {
    let mut cv_tex_ptr: *mut CVMetalTexture = std::ptr::null_mut();
    // SAFETY: `cache` and `pb` are live CoreVideo objects; plane index and dimensions were
    // validated against the `CVPixelBuffer` planes at decode time.
    let status = unsafe {
        CVMetalTextureCache::create_texture_from_image(None, cache, pb, None, mtl_format, w as usize, h as usize, plane, NonNull::from(&mut cv_tex_ptr))
    };
    let cv_tex_ptr = NonNull::new(cv_tex_ptr).filter(|_| status == 0)?;
    // SAFETY: `create_texture_from_image` returns a retained (+1) `CVMetalTexture` on status 0.
    let cv_tex = unsafe { CFRetained::from_raw(cv_tex_ptr) };
    let mtl_tex = CVMetalTextureGetTexture(&cv_tex)?;
    let holders = RetainedPlaneHolders(cv_tex, pb.clone());
    // SAFETY: `mtl_tex` is a valid 2D `MTLTexture` backed by `pb`'s `IOSurface` with matching
    // format and dimensions; the drop callback retains `holders` until `wgpu` finishes using the
    // texture.
    let hal_tex = unsafe {
        wgpu::hal::metal::Device::texture_from_raw(
            mtl_tex,
            wgpu_format,
            MTLTextureType::Type2D,
            1,
            1,
            wgpu::hal::CopyExtent { width: w, height: h, depth: 1 },
            Some(Box::new(move || {
                drop(holders);
            })),
        )
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("vt-iosurface-plane"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu_format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    // SAFETY: `hal_tex` was created from the same Metal device as `device` and matches `desc`.
    let tex = unsafe { device.create_texture_from_hal::<wgpu::hal::api::Metal>(hal_tex, &desc, wgpu::TextureUses::RESOURCE) };
    Some(tex.create_view(&Default::default()))
}

/// State shared with the output callback (its `decompressionOutputRefCon`). Boxed by the decoder
/// and kept alive until the session is invalidated.
struct Shared {
    /// Behind a lock: a VP9 key frame sets the colour of the pictures after it.
    layout: Mutex<Layout>,
    out: Mutex<Vec<Output>>,
    surfaces: Mutex<VecDeque<Weak<VtSurface>>>,
}

impl Shared {
    fn push(&self, o: Output) {
        self.out.lock().unwrap_or_else(PoisonError::into_inner).push(o);
    }

    /// Bound decoder-owned native buffers by copying the oldest live surface before removing its tracking entry.
    fn track_surface(&self, surface: &Arc<VtSurface>) -> std::result::Result<(), String> {
        let mut q = self.surfaces.lock().unwrap_or_else(PoisonError::into_inner);
        q.retain(|w| w.strong_count() > 0);
        while q.len() >= MAX_RETAINED_SURFACES {
            if let Some(old) = q.front().and_then(|w| w.upgrade()) {
                old.evict_to_cpu().map_err(|e| format!("cannot evict retained VideoToolbox surface: {e}"))?;
            }
            q.pop_front();
        }
        q.push_back(Arc::downgrade(surface));
        Ok(())
    }
}

/// A VideoToolbox session with its callback state. Dropping it invalidates the session before the
/// callback state is freed.
struct Session {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    /// Pointed to by the session's callback record: must outlive `session`.
    shared: Box<Shared>,
}

// SAFETY: a VTDecompressionSession and the CoreMedia objects it uses may be used from any thread
// (Apple: "thread safe"); the decoder owning a `Session` is used by one thread at a time (`&mut
// self`), and the callback state is behind a `Mutex`.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the session is valid until released; invalidating it guarantees no callback runs
        // afterwards, so `shared` can be freed when this function returns.
        unsafe {
            self.session.wait_for_asynchronous_frames();
            self.session.invalidate();
        }
    }
}

/// The output callback. Never unwinds: panics are caught and reported as a failed frame.
unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    frame_refcon: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: CMTime,
    _duration: CMTime,
) {
    if refcon.is_null() {
        return;
    }
    // SAFETY: `refcon` is the `Shared` the session was created with; it lives in a `Box` owned by
    // the `Session`, which invalidates the session (no more callbacks) before freeing it.
    let shared = unsafe { &*(refcon as *const Shared) };
    let pts = frame_refcon as isize as i64;
    let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if status != 0 {
            return Output::Failed(format!("VideoToolbox decode error {status}"));
        }
        let Some(image) = NonNull::new(image) else {
            return Output::Failed("VideoToolbox dropped a picture".into());
        };
        // SAFETY: VideoToolbox passes a valid image buffer that stays alive for the duration of
        // the callback; we borrow it here (and retain it if wrapped in a `VtSurface`).
        let pb = unsafe { image.as_ref() };
        let layout = shared.layout.lock().unwrap_or_else(PoisonError::into_inner).clone();
        match copy_out(pb, &layout, shared) {
            Ok(frame) => Output::Frame(DecodedFrame { pts, frame, draft: false }),
            Err(e) => Output::Failed(e),
        }
    }));
    shared.push(r.unwrap_or_else(|_| Output::Failed("panic while reading a decoded picture".into())));
}

/// Unlocks a pixel buffer's base address when dropped.
struct Locked<'a>(&'a CVImageBuffer);

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: the caller locked the buffer with `ReadOnly`, and it is still alive (borrowed).
        unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
    }
}

/// One plane of a locked pixel buffer.
struct PlaneView<'a> {
    data: &'a [u8],
    stride: usize,
    width: usize,
    height: usize,
}

impl<'a> PlaneView<'a> {
    /// `len` bytes of row `y` from byte `x`.
    fn row(&self, y: usize, x: usize, len: usize) -> std::result::Result<&'a [u8], String> {
        let start = y * self.stride + x;
        self.data.get(start..start + len).ok_or_else(|| "plane row out of range".to_string())
    }
}

/// The returned view borrows the lock guard, so it cannot outlive the locked base address.
fn plane<'l>(pb: &CVImageBuffer, _lock: &'l Locked<'_>, i: usize) -> std::result::Result<PlaneView<'l>, String> {
    let base = CVPixelBufferGetBaseAddressOfPlane(pb, i) as *const u8;
    let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, i);
    let (width, height) = (CVPixelBufferGetWidthOfPlane(pb, i), CVPixelBufferGetHeightOfPlane(pb, i));
    if base.is_null() || stride == 0 || height == 0 {
        return Err(format!("decoded picture plane {i} is not mapped"));
    }
    let len = stride.checked_mul(height).ok_or("plane size overflows")?;
    // SAFETY: the buffer's base address is locked while `_lock` lives, and the returned slice
    // borrows `_lock` ('l), so it cannot be used after the unlock; CoreVideo maps
    // `bytes_per_row * height` bytes from each plane's base address.
    let data = unsafe { std::slice::from_raw_parts(base, len) };
    Ok(PlaneView { data, stride, width, height })
}

/// Lock, validate and copy cropped NV12/NV16 luma and interleaved chroma into pooled CPU planes.
fn copy_biplanar_u8(pb: &CVImageBuffer, l: &Layout, ox: usize, oy: usize, w: usize, h: usize) -> std::result::Result<[Arc<Vec<u8>>; 2], String> {
    // SAFETY: `pb` is a valid pixel buffer for the duration of this call; `Locked` unlocks it on drop.
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return Err("cannot lock the decoded picture".into());
    }
    let lock = Locked(pb);
    let (ys, cs) = (plane(pb, &lock, 0)?, plane(pb, &lock, 1)?);
    let (sx, sy) = l.chroma.shifts();
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    let (cox, coy) = (ox >> sx, oy >> sy);
    if ys.width < ox + w || ys.height < oy + h || ys.stride < ys.width || cs.width < cox + cw || cs.height < coy + ch || cs.stride < cs.width * 2 {
        return Err("decoded picture planes are smaller than the picture".into());
    }
    let mut yp = pool::take_u8(w * h);
    if ox == 0 && oy == 0 && ys.stride == w {
        yp.extend_from_slice(ys.data.get(..w * h).ok_or_else(|| "plane row out of range".to_string())?);
    } else {
        for y in 0..h {
            yp.extend_from_slice(ys.row(oy + y, ox, w)?);
        }
    }
    let mut uvp = pool::take_u8(cw * ch * 2);
    if cox == 0 && coy == 0 && cs.stride == cw * 2 {
        uvp.extend_from_slice(cs.data.get(..cw * ch * 2).ok_or_else(|| "plane row out of range".to_string())?);
    } else {
        for y in 0..ch {
            uvp.extend_from_slice(cs.row(coy + y, cox * 2, cw * 2)?);
        }
    }
    drop(lock);
    Ok([Arc::new(yp), Arc::new(uvp)])
}

/// Lock, validate and copy cropped P010/P210 planes while preserving their high-aligned sample codes.
fn copy_biplanar_u16(pb: &CVImageBuffer, l: &Layout, ox: usize, oy: usize, w: usize, h: usize) -> std::result::Result<[Arc<Vec<u16>>; 2], String> {
    // SAFETY: `pb` is a valid pixel buffer for the duration of this call; `Locked` unlocks it on drop.
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return Err("cannot lock the decoded picture".into());
    }
    let lock = Locked(pb);
    let (ys, cs) = (plane(pb, &lock, 0)?, plane(pb, &lock, 1)?);
    let (sx, sy) = l.chroma.shifts();
    let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
    let (cox, coy) = (ox >> sx, oy >> sy);
    if ys.width < ox + w || ys.height < oy + h || ys.stride < ys.width * 2 || cs.width < cox + cw || cs.height < coy + ch || cs.stride < cs.width * 4 {
        return Err("decoded picture planes are smaller than the picture".into());
    }
    let mut yp = pool::take_u16(w * h);
    for y in 0..h {
        let row = ys.row(oy + y, ox * 2, w * 2)?;
        yp.extend(row.as_chunks::<2>().0.iter().map(|b| u16::from_ne_bytes([b[0], b[1]])));
    }
    let mut uvp = pool::take_u16(cw * ch * 2);
    for y in 0..ch {
        let row = cs.row(coy + y, cox * 4, cw * 4)?;
        uvp.extend(row.as_chunks::<2>().0.iter().map(|b| u16::from_ne_bytes([b[0], b[1]])));
    }
    drop(lock);
    Ok([Arc::new(yp), Arc::new(uvp)])
}

/// Wrap a decoded biplanar buffer in a [`VtSurface`] (`PixelData::Biplanar8` / `Biplanar16`) so
/// the GPU compositor can import its `IOSurface` directly without a CPU copy, while CPU consumers
/// materialise its planes on demand.
fn copy_out(pb: &CVImageBuffer, l: &Layout, shared: &Shared) -> std::result::Result<VideoFrame, String> {
    let fmt = CVPixelBufferGetPixelFormatType(pb);
    if fmt != l.pixel_format {
        return Err(format!("unexpected decoded pixel format {fmt:#x}"));
    }
    if matches!(fmt, AYUV16 | BGRA) {
        return copy_packed(pb, l);
    }
    if CVPixelBufferGetPlaneCount(pb) != 2 {
        return Err(format!("unexpected decoded pixel format {fmt:#x}"));
    }
    let (bw, bh) = (CVPixelBufferGetWidth(pb) as u32, CVPixelBufferGetHeight(pb) as u32);
    let (cx, cy, w, h) = l.crop;
    // VideoToolbox outputs the cropped picture; a buffer of the coded size is cropped here.
    let (ox, oy) = if (bw, bh) == (w, h) {
        (0, 0)
    } else if (bw, bh) == l.coded && cx + w <= bw && cy + h <= bh {
        (cx, cy)
    } else {
        return Err(format!("decoded picture is {bw}x{bh}, expected {w}x{h}"));
    };
    let (w_us, h_us, ox_us, oy_us) = (w as usize, h as usize, ox as usize, oy as usize);
    let (sx, sy) = l.chroma.shifts();
    let (cw, ch) = (w_us.div_ceil(1 << sx), h_us.div_ceil(1 << sy));
    let (cox, coy) = (ox_us >> sx, oy_us >> sy);
    let bps = if l.bits > 8 { 2 } else { 1 };
    let (yw, yh, ys) = (CVPixelBufferGetWidthOfPlane(pb, 0), CVPixelBufferGetHeightOfPlane(pb, 0), CVPixelBufferGetBytesPerRowOfPlane(pb, 0));
    let (uvw, uvh, uvs) = (CVPixelBufferGetWidthOfPlane(pb, 1), CVPixelBufferGetHeightOfPlane(pb, 1), CVPixelBufferGetBytesPerRowOfPlane(pb, 1));
    if yw < ox_us + w_us || yh < oy_us + h_us || ys < yw * bps || uvw < cox + cw || uvh < coy + ch || uvs < uvw * 2 * bps {
        return Err("decoded picture planes are smaller than the picture".into());
    }
    // SAFETY: `pb` is a valid CoreFoundation object for the duration of the callback.
    let retained = unsafe { CFRetained::retain(NonNull::from(pb)) };
    let surface = Arc::new(VtSurface {
        pb: Mutex::new(Some(retained)),
        layout: l.clone(),
        ox: ox_us,
        oy: oy_us,
        w: w_us,
        h: h_us,
        u8_planes: Mutex::new(None),
        u16_planes: Mutex::new(None),
    });
    shared.track_surface(&surface)?;
    let surface_dyn: Arc<dyn NativeSurface> = surface;
    let data = if l.bits <= 8 {
        PixelData::Biplanar8 { planes: None, chroma: l.chroma, surface: Some(surface_dyn) }
    } else {
        PixelData::Biplanar16 { planes: None, chroma: l.chroma, bits: l.bits, surface: Some(surface_dyn) }
    };
    Ok(VideoFrame { width: w, height: h, data, color: l.color, par: l.par, pts: filmcraft_time::Tick::ZERO })
}

/// The layout for a VP9 stream (profile 0 8-bit, profile 2 10-bit, 4:2:0), or why VideoToolbox is
/// not used for it. The colour is the key frame's ([`VtDecoder::decode`]).
fn vp9_layout(info: &FrameStreamInfo, c: &VpcConfig) -> std::result::Result<Layout, String> {
    if info.codec != FrameCodec::Vp9 {
        return Err("AV1 is not decoded through VideoToolbox".into());
    }
    if info.subsampling != (1, 1) || info.mono {
        return Err(format!("VP9 profile {} chroma {:?}", info.profile, info.subsampling));
    }
    if !matches!((info.profile, info.bit_depth), (0, 8) | (2, 10)) {
        return Err(format!("VP9 profile {} at {} bits", info.profile, info.bit_depth));
    }
    let pixel_format = vp9_pixel_format(info.bit_depth, c.full_range);
    let (w, h) = info.size;
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err(format!("picture size {w}x{h}"));
    }
    let mut color = filmcraft_color::ColorInfo::REC709;
    if c.full_range {
        color.range = filmcraft_color::Range::Full;
    }
    Ok(Layout { alpha: false, pixel_format, coded: info.size, crop: (0, 0, w, h), chroma: Chroma::C420, bits: info.bit_depth, color, par: (1, 1) })
}

/// The layout for an AV1 stream (main profile: 8- or 10-bit 4:2:0), or why VideoToolbox is not
/// used for it. The colour is the sequence header's (`av1C`'s now, a later in-band one's from the
/// sample that carries it, [`VtDecoder::decode`]).
fn av1_layout(info: &FrameStreamInfo) -> std::result::Result<Layout, String> {
    if info.codec != FrameCodec::Av1 {
        return Err("VP9 is not an AV1 stream".into());
    }
    if info.profile != 0 || info.mono || info.subsampling != (1, 1) {
        return Err(format!("AV1 profile {} chroma {:?}{}", info.profile, info.subsampling, if info.mono { " monochrome" } else { "" }));
    }
    if !matches!(info.bit_depth, 8 | 10) {
        return Err(format!("AV1 at {} bits", info.bit_depth));
    }
    let (w, h) = info.size;
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err(format!("picture size {w}x{h}"));
    }
    let color = info.color.unwrap_or(filmcraft_color::ColorInfo::REC709);
    let pixel_format = vp9_pixel_format(info.bit_depth, color.range == filmcraft_color::Range::Full);
    Ok(Layout { alpha: false, pixel_format, coded: info.size, crop: (0, 0, w, h), chroma: Chroma::C420, bits: info.bit_depth, color, par: (1, 1) })
}

/// The buffer format for 8- / 10-bit 4:2:0 VP9 / AV1 pictures of a range.
fn vp9_pixel_format(bits: u32, full: bool) -> u32 {
    match (bits > 8, full) {
        (false, false) => NV12_VIDEO,
        (false, true) => NV12_FULL,
        (true, false) => P010_VIDEO,
        (true, true) => P010_FULL,
    }
}

/// The `vpcC` box payload (version 1) VideoToolbox reads the VP9 configuration from.
fn vpcc_atom(c: &VpcConfig) -> Vec<u8> {
    let packed = (c.bit_depth << 4) | ((c.chroma_subsampling & 7) << 1) | u8::from(c.full_range);
    vec![1, 0, 0, 0, c.profile, c.level, packed, c.colour_primaries, c.transfer_characteristics, c.matrix_coefficients, 0, 0]
}

/// The layout for a stream, or why VideoToolbox is not used for it.
fn layout(info: &NalStreamInfo) -> std::result::Result<Layout, String> {
    if info.interlaced {
        return Err("field-coded H.264".into());
    }
    if info.bit_depth_luma != info.bit_depth_chroma || !matches!(info.bit_depth_luma, 8 | 10) {
        return Err(format!("{}-bit luma / {}-bit chroma", info.bit_depth_luma, info.bit_depth_chroma));
    }
    let full = info.color.range == filmcraft_color::Range::Full;
    let ten = info.bit_depth_luma == 10;
    let (chroma, pixel_format) = match (info.chroma_format_idc, ten, full) {
        (1, false, false) => (Chroma::C420, NV12_VIDEO),
        (1, false, true) => (Chroma::C420, NV12_FULL),
        (1, true, false) => (Chroma::C420, P010_VIDEO),
        (1, true, true) => (Chroma::C420, P010_FULL),
        (2, false, false) => (Chroma::C422, NV16_VIDEO),
        (2, false, true) => (Chroma::C422, NV16_FULL),
        (2, true, false) => (Chroma::C422, P210_VIDEO),
        (2, true, true) => (Chroma::C422, P210_FULL),
        (3, false, false) => (Chroma::C444, u32::from_be_bytes(*b"444v")),
        (3, false, true) => (Chroma::C444, u32::from_be_bytes(*b"444f")),
        (3, true, false) => (Chroma::C444, u32::from_be_bytes(*b"x444")),
        (3, true, true) => (Chroma::C444, u32::from_be_bytes(*b"xf44")),
        (c, ..) => return Err(format!("chroma_format_idc {c}")),
    };
    let (cx, cy, w, h) = info.crop;
    if w == 0 || h == 0 || w > 8192 || h > 8192 || cx.saturating_add(w) > info.coded.0 || cy.saturating_add(h) > info.coded.1 {
        return Err(format!("picture size {w}x{h}"));
    }
    Ok(Layout { alpha: false, pixel_format, coded: info.coded, crop: info.crop, chroma, bits: info.bit_depth_luma, color: info.color, par: info.par })
}

/// What a session decodes: an `avcC` / `hvcC` stream, a VP9 stream with its `vpcC` box, or an
/// AV1 stream with its `av1C` box.
#[derive(Clone)]
enum Stream {
    Nal(NalStreamInfo),
    Vp9(FrameStreamInfo, Vec<u8>),
    Av1(FrameStreamInfo, Vec<u8>),
    Intra(Box<filmcraft_isobmff::SampleEntry>),
}

impl Stream {
    /// Build the CoreMedia description from parameter sets, a codec configuration atom or an intra sample entry.
    fn format_description(&self) -> std::result::Result<CFRetained<CMFormatDescription>, String> {
        match self {
            Self::Nal(info) => format_description(info),
            Self::Intra(entry) => intra_format_description(entry),
            Self::Vp9(info, vpcc) => atom_format_description(VP9, "vpcC", info.size, vpcc),
            Self::Av1(info, av1c) => atom_format_description(AV1, "av1C", info.size, av1c),
        }
    }

    /// The frame-coded stream's info (VP9 / AV1), if it is one.
    fn frame_info(&self) -> Option<&FrameStreamInfo> {
        match self {
            Self::Nal(_) | Self::Intra(_) => None,
            Self::Vp9(info, _) | Self::Av1(info, _) => Some(info),
        }
    }
}

impl Session {
    /// Create a hardware-required decompression session and its synchronized callback state for the requested layout.
    fn new(stream: &Stream, layout: Layout) -> std::result::Result<Session, String> {
        filmcraft_gpu::register_surface_importer(import_metal_surface);
        let format = stream.format_description()?;
        // SAFETY: reading an immutable framework constant.
        let key: &CFString = unsafe { kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder };
        let spec = CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true).as_ref()]);
        let pf = CFNumber::new_i32(layout.pixel_format as i32);
        // SAFETY: reading immutable framework constants.
        let pf_key: &CFString = unsafe { kCVPixelBufferPixelFormatTypeKey };
        // SAFETY: reading immutable framework constants.
        let metal_key: &CFString = unsafe { kCVPixelBufferMetalCompatibilityKey };
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&[pf_key, metal_key], &[pf.as_ref(), CFBoolean::new(true).as_ref()]);
        let shared = Box::new(Shared { layout: Mutex::new(layout), out: Mutex::new(Vec::new()), surfaces: Mutex::new(VecDeque::new()) });
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: &*shared as *const Shared as *mut c_void,
        };
        let mut out: *mut VTDecompressionSession = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call (VideoToolbox copies the callback record);
        // the refcon points into `shared`, which the returned `Session` keeps alive until the
        // session is invalidated.
        let status = unsafe { VTDecompressionSession::create(None, &format, Some(spec.as_ref()), Some(attrs.as_ref()), &record, NonNull::from(&mut out)) };
        let session = NonNull::new(out).filter(|_| status == 0).ok_or_else(|| format!("VTDecompressionSessionCreate failed ({status})"))?;
        // SAFETY: a created session is returned retained (+1); `CFRetained` takes that reference over.
        let session = unsafe { CFRetained::from_raw(session) };
        Ok(Session { session, format, shared })
    }

    /// Submit one length-prefixed access unit (asynchronous decompression: its picture arrives
    /// through the callback, possibly after this returns).
    fn submit(&self, prefix: &[u8], sample: &[u8], pts: i64) -> std::result::Result<(), String> {
        let buffer = sample_buffer(prefix, sample, pts, &self.format)?;
        let mut info = VTDecodeInfoFlags(0);
        // SAFETY: the session and sample buffer are valid (VideoToolbox retains the sample buffer
        // for as long as it needs it); the frame refcon is not a pointer, it carries the pts.
        let status =
            unsafe { self.session.decode_frame(&buffer, VTDecodeFrameFlags::Frame_EnableAsynchronousDecompression, pts as isize as *mut c_void, &mut info) };
        if status != 0 {
            return Err(format!("VTDecompressionSessionDecodeFrame failed ({status})"));
        }
        Ok(())
    }

    /// Wait until every submitted picture has been emitted.
    fn wait(&self) {
        // SAFETY: the session is valid.
        unsafe { self.session.wait_for_asynchronous_frames() };
    }

    /// The layout of pictures decoded from now on.
    fn set_layout(&self, layout: Layout) {
        *self.shared.layout.lock().unwrap_or_else(PoisonError::into_inner) = layout;
    }

    /// What the callback produced so far.
    fn take(&self) -> Vec<Output> {
        std::mem::take(&mut *self.shared.out.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Access units decoding at once (asynchronous decompression overlaps decoding a picture with
/// copying out the previous one).
const MAX_IN_FLIGHT: usize = 2;

/// A `CMVideoFormatDescription` from the record's parameter sets.
fn format_description(info: &NalStreamInfo) -> std::result::Result<CFRetained<CMFormatDescription>, String> {
    let mut ptrs: Vec<NonNull<u8>> = Vec::with_capacity(info.parameter_sets.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(info.parameter_sets.len());
    for p in &info.parameter_sets {
        ptrs.push(NonNull::new(p.as_ptr() as *mut u8).ok_or("empty parameter set")?);
        sizes.push(p.len());
    }
    let ptr_array = NonNull::new(ptrs.as_mut_ptr()).ok_or("no parameter sets")?;
    let size_array = NonNull::new(sizes.as_mut_ptr()).ok_or("no parameter sets")?;
    let mut out: *const CMFormatDescription = std::ptr::null();
    let ls = info.length_size as i32;
    // SAFETY: `ptrs` / `sizes` hold `len` valid (pointer, size) pairs into `info.parameter_sets`,
    // which outlive the call; CoreMedia copies the parameter sets.
    let status = unsafe {
        match info.codec {
            NalCodec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(None, ptrs.len(), ptr_array, size_array, ls, NonNull::from(&mut out)),
            NalCodec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(None, ptrs.len(), ptr_array, size_array, ls, None, NonNull::from(&mut out)),
        }
    };
    let fd = NonNull::new(out as *mut CMFormatDescription).filter(|_| status == 0).ok_or_else(|| format!("bad parameter sets for VideoToolbox ({status})"))?;
    // SAFETY: a "Create" function returns the description retained (+1); `CFRetained` takes it over.
    Ok(unsafe { CFRetained::from_raw(fd) })
}

/// A VP9 / AV1 `CMVideoFormatDescription`: the codec type, the picture size and the `vpcC` /
/// `av1C` box payload as a sample description extension atom.
fn atom_format_description(codec: u32, atom: &'static str, (w, h): (u32, u32), payload: &[u8]) -> std::result::Result<CFRetained<CMFormatDescription>, String> {
    let atom_key = CFString::from_static_str(atom);
    let data = CFData::from_bytes(payload);
    let atoms = CFDictionary::<CFString, CFType>::from_slices(&[&*atom_key], &[data.as_ref()]);
    // SAFETY: reading an immutable framework constant.
    let key: &CFString = unsafe { kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms };
    let ext = CFDictionary::<CFString, CFType>::from_slices(&[key], &[atoms.as_ref()]);
    let mut out: *const CMFormatDescription = std::ptr::null();
    // SAFETY: the extensions dictionary is valid for the call (CoreMedia deep-copies it) and `out`
    // is a valid pointer for the result.
    let status = unsafe { CMVideoFormatDescriptionCreate(None, codec, w as i32, h as i32, Some(ext.as_opaque()), NonNull::from(&mut out)) };
    let fd = NonNull::new(out as *mut CMFormatDescription).filter(|_| status == 0).ok_or_else(|| format!("bad {atom} for VideoToolbox ({status})"))?;
    // SAFETY: a "Create" function returns the description retained (+1); `CFRetained` takes it over.
    Ok(unsafe { CFRetained::from_raw(fd) })
}

/// A ready `CMSampleBuffer` holding `prefix` (if any) followed by `sample`.
fn sample_buffer(prefix: &[u8], sample: &[u8], pts: i64, format: &CMFormatDescription) -> std::result::Result<CFRetained<CMSampleBuffer>, String> {
    let total = prefix.len().saturating_add(sample.len());
    if sample.is_empty() || total > 256 << 20 {
        return Err("empty or oversized sample".into());
    }
    let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: CoreMedia allocates (AssureMemoryNow) a block of `total` bytes with the default
    // allocator; no external memory is referenced.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            total,
            None,
            std::ptr::null(),
            0,
            total,
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block),
        )
    };
    let block = NonNull::new(block).filter(|_| status == 0).ok_or_else(|| format!("CMBlockBufferCreateWithMemoryBlock failed ({status})"))?;
    // SAFETY: created retained (+1).
    let block = unsafe { CFRetained::from_raw(block) };
    if let Some(p) = NonNull::new(prefix.as_ptr() as *mut c_void).filter(|_| !prefix.is_empty()) {
        // SAFETY: copies `prefix.len()` bytes from `prefix` into the start of the `total`-byte block.
        let status = unsafe { CMBlockBuffer::replace_data_bytes(p, &block, 0, prefix.len()) };
        if status != 0 {
            return Err(format!("CMBlockBufferReplaceDataBytes failed ({status})"));
        }
    }
    let src = NonNull::new(sample.as_ptr() as *mut c_void).ok_or("empty sample")?;
    // SAFETY: copies `sample.len()` bytes from `sample` at offset `prefix.len()` into the `total`-byte block.
    let status = unsafe { CMBlockBuffer::replace_data_bytes(src, &block, prefix.len(), sample.len()) };
    if status != 0 {
        return Err(format!("CMBlockBufferReplaceDataBytes failed ({status})"));
    }
    let invalid = CMTime { value: 0, timescale: 0, flags: CMTimeFlags(0), epoch: 0 };
    let timing = CMSampleTimingInfo {
        duration: invalid,
        presentationTimeStamp: CMTime { value: pts, timescale: 1, flags: CMTimeFlags::Valid, epoch: 0 },
        decodeTimeStamp: invalid,
    };
    let size = total;
    let mut out: *mut CMSampleBuffer = std::ptr::null_mut();
    // SAFETY: one sample with one timing entry and one size entry, all pointing at locals that
    // outlive the call; the sample buffer retains the block buffer and format description.
    let status = unsafe { CMSampleBuffer::create_ready(None, Some(&block), Some(format), 1, 1, &timing, 1, &size, NonNull::from(&mut out)) };
    let out = NonNull::new(out).filter(|_| status == 0).ok_or_else(|| format!("CMSampleBufferCreateReady failed ({status})"))?;
    // SAFETY: created retained (+1).
    Ok(unsafe { CFRetained::from_raw(out) })
}

/// Whether VideoToolbox reports a hardware decoder for the codec at all.
pub fn hardware_available(codec: NalCodec) -> bool {
    let t = match codec {
        NalCodec::H264 => u32::from_be_bytes(*b"avc1"),
        NalCodec::Hevc => u32::from_be_bytes(*b"hvc1"),
    };
    // SAFETY: a pure query with no pointer arguments.
    unsafe { objc2_video_toolbox::VTIsHardwareDecodeSupported(t) }
}

/// Whether this Mac has a hardware VP9 decoder (Apple silicon). VP9 is a supplemental decoder:
/// it must be registered before VideoToolbox offers it.
pub fn vp9_hardware_available() -> bool {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    // SAFETY: pure calls with no pointer arguments; registering twice is harmless, done once anyway.
    REGISTER.call_once(|| unsafe { objc2_video_toolbox::VTRegisterSupplementalVideoDecoderIfAvailable(VP9) });
    // SAFETY: as above.
    unsafe { objc2_video_toolbox::VTIsHardwareDecodeSupported(VP9) }
}

/// Whether this Mac has a hardware AV1 decoder (the media engine of the M3 and later; macOS 14).
/// Like VP9, a supplemental decoder that must be registered before VideoToolbox offers it; on a
/// Mac without one the query says so and the stream stays with our decoder.
pub fn av1_hardware_available() -> bool {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    // SAFETY: pure calls with no pointer arguments; registering twice is harmless, done once anyway.
    REGISTER.call_once(|| unsafe { objc2_video_toolbox::VTRegisterSupplementalVideoDecoderIfAvailable(AV1) });
    // SAFETY: as above.
    unsafe { objc2_video_toolbox::VTIsHardwareDecodeSupported(AV1) }
}

/// The VideoToolbox decoder for one `avcC` / `hvcC` / `vpcC` / `av1C` stream.
pub struct VtDecoder {
    stream: Stream,
    /// The stream's reorder depth (0 for VP9 / AV1).
    reorder: usize,
    layout: Layout,
    /// `None` after a reset until the next sample (a fresh session per decoding run).
    session: Option<Session>,
    /// Decoded pictures waiting for presentation order (sorted by pts).
    pending: Vec<DecodedFrame>,
    /// Presentation times of submitted access units whose picture has not arrived yet.
    in_flight: Vec<i64>,
    /// The run started at a CRA / BLA picture: its RASL pictures are not decodable and are left
    /// out (as the software decoder does).
    skip_rasl: bool,
    first: bool,
    /// Test hook: fail every decode once this many samples were fed.
    fail_after: Option<u64>,
    fed: u64,
    name: &'static str,
}

impl VtDecoder {
    /// A decoder for the stream, or why VideoToolbox does not take it (unsupported format, no
    /// hardware decoder, session creation failed).
    /// Keep prefix/suffix SEI (including the alpha-channel description) alongside VPS/SPS/PPS.
    pub fn new_hevc(mut info: NalStreamInfo, config: &filmcraft_isobmff::HevcConfig) -> std::result::Result<Self, String> {
        for array in config.arrays.iter().filter(|a| matches!(a.nal_type, 39 | 40)) {
            info.parameter_sets.extend(array.nalus.clone());
        }
        Self::new(info)
    }

    pub fn new(info: NalStreamInfo) -> std::result::Result<Self, String> {
        let mut layout = layout(&info)?;
        if info.codec == NalCodec::Hevc {
            let format = format_description(&info)?;
            // SAFETY: an immutable framework key and a live format description.
            let alpha = unsafe { format.extension(objc2_core_media::kCMFormatDescriptionExtension_ContainsAlphaChannel) };
            if alpha.as_ref().and_then(|a| a.downcast_ref::<CFBoolean>()).is_some_and(CFBoolean::as_bool) {
                if layout.bits > 8 {
                    return Err("VideoToolbox does not decode HEVC alpha above 8 bits on the validated macOS path".into());
                }
                layout.pixel_format = BGRA;
                layout.alpha = true;
                layout.chroma = Chroma::C444;
            }
        }
        if !hardware_available(info.codec) {
            return Err("no hardware decoder".into());
        }
        let name = match info.codec {
            NalCodec::H264 => "VideoToolbox H.264",
            NalCodec::Hevc => "VideoToolbox HEVC",
        };
        let reorder = info.reorder;
        Self::with(Stream::Nal(info), reorder, layout, name)
    }

    /// A decoder for a VP9 stream with its `vpcC` configuration, or why VideoToolbox does not take
    /// it (profile 1 / 3, 12-bit, no hardware VP9 decoder, session creation failed).
    pub fn new_vp9(info: FrameStreamInfo, config: &VpcConfig) -> std::result::Result<Self, String> {
        let layout = vp9_layout(&info, config)?;
        if !vp9_hardware_available() {
            return Err("no hardware VP9 decoder".into());
        }
        Self::with(Stream::Vp9(info, vpcc_atom(config)), 0, layout, "VideoToolbox VP9")
    }

    /// A decoder for an AV1 stream with its `av1C` configuration, or why VideoToolbox does not
    /// take it (high / professional profile, 12-bit, monochrome, no hardware AV1 decoder, session
    /// creation failed).
    pub fn new_av1(info: FrameStreamInfo, config: &Av1Config) -> std::result::Result<Self, String> {
        let layout = av1_layout(&info)?;
        if !av1_hardware_available() {
            return Err("no hardware AV1 decoder".into());
        }
        Self::with(Stream::Av1(info, config.to_bytes()), 0, layout, "VideoToolbox AV1")
    }

    /// Create the initial session eagerly so unsupported stream/layout combinations fail before decoding starts.
    fn with(stream: Stream, reorder: usize, layout: Layout, name: &'static str) -> std::result::Result<Self, String> {
        // Create the session now so unsupported streams are declined up front.
        let session = Session::new(&stream, layout.clone())?;
        Ok(Self {
            stream,
            reorder,
            layout,
            session: Some(session),
            pending: Vec::new(),
            in_flight: Vec::new(),
            skip_rasl: false,
            first: true,
            fail_after: None,
            fed: 0,
            name,
        })
    }

    /// A hardware-only ProRes (all six profiles) or Motion JPEG decoder. Session creation
    /// establishes availability; unsupported systems keep the software decoder.
    pub fn new_intra(entry: filmcraft_isobmff::SampleEntry) -> std::result::Result<Self, String> {
        let layout = intra_layout(&entry)?;
        let name = if matches!(entry.codec, filmcraft_isobmff::CodecConfig::ProRes { .. }) { "VideoToolbox ProRes" } else { "VideoToolbox Motion JPEG" };
        Self::with(Stream::Intra(Box::new(entry)), 0, layout, name)
    }

    /// HEVC alpha has no software counterpart; its fallback must fail without discarding alpha.
    pub fn requires_hardware(&self) -> bool {
        self.layout.alpha && matches!(self.stream, Stream::Nal(_))
    }

    /// Whether VideoToolbox confirms the active hardware decoder (diagnostics).
    pub fn uses_hardware(&self) -> bool {
        let Some(session) = &self.session else { return false };
        let mut value: *mut CFType = std::ptr::null_mut();
        // SAFETY: immutable framework key, live session and valid output pointer.
        let status = unsafe {
            objc2_video_toolbox::VTSessionCopyProperty(
                session.session.as_ref(),
                objc2_video_toolbox::kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                None,
                (&raw mut value).cast(),
            )
        };
        let Some(value) = NonNull::new(value).filter(|_| status == 0) else { return false };
        // SAFETY: Copy transfers a retained reference.
        let value = unsafe { CFRetained::from_raw(value) };
        value.downcast_ref::<CFBoolean>().is_some_and(CFBoolean::as_bool)
    }

    /// Test hook: from the `n`-th sample on, every decode fails as a lost session would.
    pub fn fail_after(&mut self, n: u64) {
        self.fail_after = Some(n);
    }

    fn insert(&mut self, f: DecodedFrame) {
        let at = self.pending.partition_point(|p| p.pts <= f.pts);
        self.pending.insert(at, f);
    }

    /// Pictures that are due: everything beyond the reorder depth, smallest pts first.
    fn bump(&mut self) -> Vec<DecodedFrame> {
        let mut out = Vec::new();
        // A picture still decoding counts towards the depth and may come before the waiting ones.
        while self.pending.len() + self.in_flight.len() > self.reorder {
            let first = self.pending.first().map(|f| f.pts);
            match (first, self.in_flight.iter().min()) {
                (Some(p), Some(&q)) if q < p => break,
                (Some(_), _) => out.push(self.pending.remove(0)),
                (None, _) => break,
            }
        }
        out
    }

    /// Move the callback's pictures into the reorder buffer.
    fn collect(&mut self) -> std::result::Result<(), String> {
        let Some(session) = self.session.as_ref() else { return Ok(()) };
        for o in session.take() {
            match o {
                Output::Frame(f) => {
                    if let Some(i) = self.in_flight.iter().position(|&p| p == f.pts) {
                        self.in_flight.swap_remove(i);
                    }
                    self.insert(f);
                }
                Output::Failed(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl VideoDecoder for VtDecoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        self.fed += 1;
        if self.fail_after.is_some_and(|n| self.fed > n) {
            return Err(CodecError::Decode("VideoToolbox session failed (test hook)".into()));
        }
        if let Stream::Nal(info) = &self.stream
            && info.codec == NalCodec::Hevc
        {
            let types = info.nal_types(sample);
            let irap = types.iter().any(|t| (16..=23).contains(t));
            if irap {
                // CRA / BLA starting a run: its RASL pictures reference pictures we never decoded.
                self.skip_rasl = self.first && types.iter().any(|t| (16..=21).contains(t) && !(19..=20).contains(t));
            } else if self.skip_rasl && types.iter().any(|t| matches!(t, 8 | 9)) {
                self.first = false;
                return Ok(Vec::new());
            }
        }
        if matches!(self.stream, Stream::Intra(_)) {
            if let Some(session) = &self.session {
                session.wait();
            }
            self.collect().map_err(CodecError::Decode)?;
        }
        if let Stream::Intra(entry) = &self.stream {
            // ProRes carries colour/alpha information in every frame. Drain the previous
            // callback before changing its layout, so it cannot inherit the next frame's metadata.
            if matches!(entry.codec, filmcraft_isobmff::CodecConfig::ProRes { .. }) {
                let (header, _, _) = filmcraft_prores::header::FrameHeader::parse(sample).map_err(|e| CodecError::Decode(e.to_string()))?;
                let expected = self.layout.coded;
                if (u32::from(header.width), u32::from(header.height)) != expected
                    || (header.chroma == filmcraft_prores::ChromaFormat::Yuv444) != (self.layout.chroma == Chroma::C444)
                {
                    return Err(CodecError::Decode("ProRes frame layout differs from the sample entry".into()));
                }
                self.layout.alpha = header.alpha != filmcraft_prores::AlphaType::None;
                if let Some(m) = filmcraft_color::Matrix::from_code(header.color.matrix) {
                    self.layout.color.matrix = m;
                }
                if let Some(t) = filmcraft_color::Transfer::from_code(header.color.transfer) {
                    self.layout.color.transfer = t;
                }
                self.layout.color.primaries = match header.color.primaries {
                    9 => filmcraft_color::Primaries::Bt2020,
                    12 => filmcraft_color::Primaries::P3D65,
                    5 => filmcraft_color::Primaries::Bt601_625,
                    6 => filmcraft_color::Primaries::Bt601_525,
                    _ => filmcraft_color::Primaries::Bt709,
                };
                if let Some(session) = &self.session {
                    session.set_layout(self.layout.clone());
                }
            }
        }
        let first = std::mem::replace(&mut self.first, false);
        if let Some(info) = self.stream.frame_info()
            && let Some(p) = info.picture_params(sample)
        {
            // a VP9 key frame / an AV1 sequence header: its colour is what the software decoder
            // reports from here on. The buffer format stays the one of the configuration box's
            // range, which is the range VideoToolbox assumes: asking for the other one makes it
            // convert the samples (PS5 recordings say limited in the container and full in the
            // bitstream).
            self.layout.color = p.color;
            if let Some(s) = self.session.as_ref() {
                s.set_layout(self.layout.clone());
            }
        }
        // An AV1 run starting at a key frame unit without a sequence header of its own (encoders
        // may leave it to `av1C`): put the configuration OBUs in front, as the decoder needs the
        // sequence header before the first frame header after a reset.
        let prefix: &[u8] = match &self.stream {
            Stream::Av1(info, _) if first && !info.config_obus.is_empty() && !info.carries_sequence_header(sample) => info.config_obus.as_slice(),
            _ => &[],
        };
        if self.session.is_none() {
            self.session = Some(Session::new(&self.stream, self.layout.clone()).map_err(CodecError::Decode)?);
        }
        let Some(session) = self.session.as_ref() else {
            return Err(CodecError::Decode("no VideoToolbox session".into()));
        };
        session.submit(prefix, sample, pts).map_err(CodecError::Decode)?;
        self.in_flight.push(pts);
        if self.in_flight.len() > MAX_IN_FLIGHT {
            session.wait();
        }
        self.collect().map_err(CodecError::Decode)?;
        Ok(self.bump())
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        if let Some(s) = self.session.as_ref() {
            s.wait();
        }
        if let Err(e) = self.collect() {
            log::warn!("{}: {e} while flushing", self.name);
        }
        self.in_flight.clear();
        std::mem::take(&mut self.pending)
    }

    fn reset(&mut self) {
        // A fresh session per run (made on the next sample): no reference pictures survive a seek.
        self.session = None;
        self.pending.clear();
        self.in_flight.clear();
        self.skip_rasl = false;
        self.first = true;
    }

    fn name(&self) -> &str {
        self.name
    }

    /// Report whether every compressed sample can be decoded without earlier reference pictures.
    fn intra_only(&self) -> bool {
        matches!(self.stream, Stream::Intra(_))
    }

    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        match &self.stream {
            Stream::Nal(info) => info.is_random_access(sample),
            Stream::Intra(_) => Some(true),
            Stream::Vp9(info, _) | Stream::Av1(info, _) => Some(info.is_random_access(sample)),
        }
    }

    fn is_disposable(&self, sample: &[u8]) -> bool {
        match &self.stream {
            Stream::Nal(info) => info.is_disposable(sample),
            // as the software VP9 / AV1 decoders: no sample is marked disposable
            Stream::Vp9(..) | Stream::Av1(..) | Stream::Intra(_) => false,
        }
    }
}

const AYUV16: u32 = u32::from_be_bytes(*b"y416");
const BGRA: u32 = u32::from_be_bytes(*b"BGRA");

/// Validate a ProRes/JPEG sample entry and select biplanar or packed storage appropriate to its profile.
fn intra_layout(entry: &filmcraft_isobmff::SampleEntry) -> std::result::Result<Layout, String> {
    let v = entry.video.as_ref().ok_or("no video dimensions")?;
    let (w, h) = (u32::from(v.width), u32::from(v.height));
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err("invalid picture size".into());
    }
    let (pixel_format, chroma, bits) = match &entry.codec {
        filmcraft_isobmff::CodecConfig::ProRes { fourcc } => match &fourcc.0 {
            b"ap4h" | b"ap4x" => (AYUV16, Chroma::C444, 12),
            b"apco" | b"apcs" | b"apcn" | b"apch" => (P210_VIDEO, Chroma::C422, 10),
            _ => return Err("invalid ProRes profile".into()),
        },
        filmcraft_isobmff::CodecConfig::Jpeg { fourcc } if fourcc.0 == *b"jpeg" => (BGRA, Chroma::C444, 8),
        _ => return Err("not a supported intra codec".into()),
    };
    Ok(Layout {
        pixel_format,
        coded: (w, h),
        crop: (0, 0, w, h),
        chroma,
        bits,
        color: if pixel_format == BGRA { filmcraft_color::ColorInfo::SRGB_FULL } else { filmcraft_color::ColorInfo::REC709 },
        par: v.pixel_aspect.filter(|&(n, d)| n > 0 && d > 0).unwrap_or((1, 1)),
        alpha: false,
    })
}

/// Create a CoreMedia video description for the intra codec and its sample-entry dimensions.
fn intra_format_description(entry: &filmcraft_isobmff::SampleEntry) -> std::result::Result<CFRetained<CMFormatDescription>, String> {
    let v = entry.video.as_ref().ok_or("no video dimensions")?;
    let mut out: *const CMFormatDescription = std::ptr::null();
    // SAFETY: entry and its validated video dimensions are live; out is a valid output pointer.
    let status = unsafe {
        CMVideoFormatDescriptionCreate(None, u32::from_be_bytes(entry.format.0), i32::from(v.width), i32::from(v.height), None, NonNull::from(&mut out))
    };
    let raw = NonNull::new(out.cast_mut()).filter(|_| status == 0).ok_or_else(|| format!("intra format description failed ({status})"))?;
    // SAFETY: Create returned a retained object (+1).
    Ok(unsafe { CFRetained::from_raw(raw) })
}

/// Copy packed BGRA or AYUV16 output into CPU pixels, preserving alpha and the declared color metadata.
fn copy_packed(pb: &CVImageBuffer, l: &Layout) -> std::result::Result<VideoFrame, String> {
    let (cx, cy, w, h) = l.crop;
    let (bw, bh) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb));
    let (ox, oy) = if (bw, bh) == (w as usize, h as usize) {
        (0, 0)
    } else if (bw, bh) == (l.coded.0 as usize, l.coded.1 as usize) {
        (cx as usize, cy as usize)
    } else {
        return Err(format!("unexpected packed dimensions {bw}x{bh}"));
    };
    let (w, h) = (w as usize, h as usize);
    // SAFETY: the callback holds a live buffer and this guard pairs the lock with its unlock.
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return Err("cannot lock decoded buffer".into());
    }
    let _lock = Locked(pb);
    let stride = objc2_core_video::CVPixelBufferGetBytesPerRow(pb);
    let bpp = if l.pixel_format == BGRA { 4 } else { 8 };
    let base = objc2_core_video::CVPixelBufferGetBaseAddress(pb).cast::<u8>();
    let len = stride.checked_mul(bh).ok_or("decoded buffer overflow")?;
    if base.is_null() || stride < (ox + w) * bpp {
        return Err("short packed buffer".into());
    }
    // SAFETY: CoreVideo maps stride * height bytes while the base address is locked; both
    // dimensions are validated and the borrow ends before the guard unlocks the buffer.
    let data = unsafe { std::slice::from_raw_parts(base, len) };
    let pixels = if bpp == 4 {
        let mut rgba = pool::take_u8(w * h * 4);
        let premultiplied = premultiplied_alpha(pb);
        for row in data.chunks(stride).skip(oy).take(h) {
            for p in row.as_chunks::<4>().0.iter().skip(ox).take(w) {
                let component = |v: u8| {
                    if !premultiplied {
                        v
                    } else if p[3] == 0 {
                        0
                    } else {
                        ((u32::from(v) * 255 + u32::from(p[3]) / 2) / u32::from(p[3])).min(255) as u8
                    }
                };
                rgba.extend_from_slice(&[component(p[2]), component(p[1]), component(p[0]), p[3]]);
            }
        }
        PixelData::Rgba8(Arc::new(rgba))
    } else {
        let [mut y, mut u, mut v, mut a] = std::array::from_fn(|_| pool::take_u16(w * h));
        for row in data.chunks(stride).skip(oy).take(h) {
            for p in row.as_chunks::<8>().0.iter().skip(ox).take(w) {
                let sample = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
                y.push(sample(2) >> (16 - l.bits));
                u.push(sample(4) >> (16 - l.bits));
                v.push(sample(6) >> (16 - l.bits));
                if l.alpha {
                    a.push(((u32::from(sample(0)) * ((1 << l.bits) - 1) + 32767) / 65535) as u16);
                }
            }
        }
        PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C444, bits: l.bits, alpha: l.alpha.then(|| Arc::new(a)) }
    };
    Ok(VideoFrame { width: w as u32, height: h as u32, data: pixels, color: l.color, par: l.par, pts: filmcraft_time::Tick::ZERO })
}

/// CopyAttachment starts at macOS 12. Use the old getter to retain macOS 11 compatibility.
#[allow(deprecated)]
fn premultiplied_alpha(pb: &CVImageBuffer) -> bool {
    // SAFETY: live buffer, immutable 10.15+ framework key; optional output pointer is null.
    let value = unsafe { pb.get_attachment(objc2_core_video::kCVImageBufferAlphaChannelModeKey, std::ptr::null_mut()) };
    // SAFETY: reading an immutable framework constant.
    let premultiplied = unsafe { objc2_core_video::kCVImageBufferAlphaChannelMode_PremultipliedAlpha };
    value.as_ref().and_then(|v| v.downcast_ref::<CFString>()).is_some_and(|v| v == premultiplied)
}

#[cfg(test)]
#[path = "videotoolbox_surface_tests.rs"]
mod surface_tests;
