//! Immutable VideoToolbox surfaces and safe Metal imports. OS media FFI stays in this crate.
use filmcraft_frame::{Chroma, NativePlanes, NativeYuv, PixelData, VideoFrame};
use objc2_core_foundation::CFRetained;
use objc2_core_video::{CVImageBuffer, CVPixelBufferLockFlags};
use std::{
    ptr::NonNull,
    sync::{Arc, OnceLock},
};

#[derive(Debug)]
struct Plane {
    base: NonNull<u8>,
    len: usize,
    stride: usize,
}
#[derive(Debug)]
pub(crate) struct Surface {
    pub(crate) buffer: CFRetained<CVImageBuffer>,
    planes: [Plane; 2],
    pub(crate) w: u32,
    pub(crate) h: u32,
    chroma: Chroma,
    pub(crate) bits: u32,
    cpu: OnceLock<NativePlanes>,
}
// SAFETY: the retained decoder image is complete and immutable. Its read-only lock remains
// held until drop, and validated plane pointers are read-only. CPU materialization is once-only.
unsafe impl Send for Surface {}
// SAFETY: simultaneous CPU/GPU reads of a retained immutable decoded image do not race.
unsafe impl Sync for Surface {}
impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: creation acquired this matching read-only lock; the buffer is still retained.
        unsafe {
            objc2_core_video::CVPixelBufferUnlockBaseAddress(&self.buffer, CVPixelBufferLockFlags::ReadOnly);
        }
    }
}
impl NativeYuv for Surface {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn chroma(&self) -> Chroma {
        self.chroma
    }
    fn bits(&self) -> u32 {
        self.bits
    }
    fn byte_size(&self) -> usize {
        // Reserve the optional CPU snapshot from the start. Cache byte accounting must remain
        // stable when a CPU consumer materializes the surface after it was inserted in the GOP.
        self.planes.iter().fold(0usize, |n, p| n.saturating_add(p.len)).saturating_mul(2)
    }

    fn planes(&self) -> NativePlanes {
        self.cpu
            .get_or_init(|| {
                let (w, h) = (self.w as usize, self.h as usize);
                let cw = w.div_ceil(2);
                let ch = h.div_ceil(if self.chroma == Chroma::C420 { 2 } else { 1 });
                // SAFETY: creation validates both mapped lengths, row spans and dimensions. The
                // read-only lock and retained buffer outlive these temporary views and CPU copying.
                let (y, uv) = unsafe {
                    (
                        std::slice::from_raw_parts(self.planes[0].base.as_ptr(), self.planes[0].len),
                        std::slice::from_raw_parts(self.planes[1].base.as_ptr(), self.planes[1].len),
                    )
                };
                if self.bits == 8 {
                    let mut yp = Vec::with_capacity(w * h);
                    for row in y.chunks_exact(self.planes[0].stride).take(h) {
                        yp.extend_from_slice(&row[..w]);
                    }
                    let (mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
                    for row in uv.chunks_exact(self.planes[1].stride).take(ch) {
                        crate::chroma::append_u8(row[..cw * 2].as_chunks::<2>().0, &mut u, &mut v);
                    }
                    NativePlanes::U8([Arc::new(yp), Arc::new(u), Arc::new(v)])
                } else {
                    let mut yp = Vec::with_capacity(w * h);
                    for row in y.chunks_exact(self.planes[0].stride).take(h) {
                        yp.extend(row[..w * 2].as_chunks::<2>().0.iter().map(|p| u16::from_ne_bytes(*p) >> 6));
                    }
                    let (mut u, mut v) = (Vec::with_capacity(cw * ch), Vec::with_capacity(cw * ch));
                    for row in uv.chunks_exact(self.planes[1].stride).take(ch) {
                        crate::chroma::append_u10(row[..cw * 4].as_chunks::<4>().0, &mut u, &mut v);
                    }
                    NativePlanes::U16([Arc::new(yp), Arc::new(u), Arc::new(v)])
                }
            })
            .clone()
    }
}

/// Uncropped 8/10-bit biplanar pictures only; other layouts use the existing decoder copy path.
pub(crate) fn frame(pb: &CVImageBuffer, w: u32, h: u32, chroma: Chroma, bits: u32) -> Option<VideoFrame> {
    use objc2_core_video::*;
    if w == 0
        || h == 0
        || w > 8192
        || h > 8192
        || !matches!(bits, 8 | 10)
        || !matches!(chroma, Chroma::C420 | Chroma::C422)
        || CVPixelBufferGetWidth(pb) != w as usize
        || CVPixelBufferGetHeight(pb) != h as usize
        || CVPixelBufferGetPlaneCount(pb) != 2
    {
        return None;
    }
    let formats = match (chroma, bits) {
        (Chroma::C420, 8) => [*b"420v", *b"420f"],
        (Chroma::C420, 10) => [*b"x420", *b"xf20"],
        (Chroma::C422, 8) => [*b"422v", *b"422f"],
        (Chroma::C422, 10) => [*b"x422", *b"xf22"],
        _ => return None,
    };
    if !formats.into_iter().any(|pf| u32::from_be_bytes(pf) == CVPixelBufferGetPixelFormatType(pb)) {
        return None;
    }
    // SAFETY: valid callback image; successful acquisition is either released on failure or
    // transferred to Surface. Keeping the read-only mapping permits infallible lazy CPU access.
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return None;
    }
    let build = || -> Option<[Plane; 2]> {
        let bps = if bits == 8 { 1 } else { 2 };
        let shapes = [(w as usize, h as usize, bps), ((w as usize).div_ceil(2), (h as usize).div_ceil(if chroma == Chroma::C420 { 2 } else { 1 }), bps * 2)];
        let mut planes = Vec::new();
        for (i, (pw, ph, bpp)) in shapes.into_iter().enumerate() {
            let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, i);
            if CVPixelBufferGetWidthOfPlane(pb, i) != pw || CVPixelBufferGetHeightOfPlane(pb, i) != ph || stride < pw.checked_mul(bpp)? {
                return None;
            }
            let len = stride.checked_mul(ph)?;
            if len > isize::MAX as usize {
                return None;
            }
            planes.push(Plane { base: NonNull::new(CVPixelBufferGetBaseAddressOfPlane(pb, i).cast::<u8>())?, stride, len });
        }
        planes.try_into().ok()
    };
    let Some(planes) = build() else {
        // SAFETY: balances this function's successful lock on validation failure.
        unsafe {
            CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
        }
        return None;
    };
    // SAFETY: callback image is live; retain keeps its backing storage and mapping alive.
    let buffer = unsafe { CFRetained::retain(NonNull::from(pb)) };
    let surface = Arc::new(Surface { buffer, planes, w, h, chroma, bits, cpu: OnceLock::new() });
    Some(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Native(surface),
        color: filmcraft_color::ColorInfo::SRGB_FULL,
        par: (1, 1),
        pts: filmcraft_time::Tick::ZERO,
    })
}

struct TextureOwner(CFRetained<objc2_core_video::CVMetalTexture>);
// SAFETY: this immutable retained CV texture is released only after GPU references complete;
// retaining/releasing CoreVideo objects is thread-safe. No mutable operations are exposed.
unsafe impl Send for TextureOwner {}
// SAFETY: sharing this owner performs no access to mutable CoreVideo state.
unsafe impl Sync for TextureOwner {}
impl TextureOwner {
    fn release(self) {
        drop(self.0);
    }
}

thread_local! {
    // The playback/export worker keeps its texture cache on its own thread. Switching devices
    // replaces it; imported textures retain their CV owners independently of this cache.
    static CACHE: std::cell::RefCell<Option<(wgpu::Device,CFRetained<objc2_core_video::CVMetalTextureCache>)>> = const { std::cell::RefCell::new(None) };
}
fn texture_cache(device: &wgpu::Device) -> Option<CFRetained<objc2_core_video::CVMetalTextureCache>> {
    CACHE.with(|slot| {
        let mut slot = slot.try_borrow_mut().ok()?;
        if let Some((previous, cache)) = &*slot
            && previous == device
        {
            return Some(cache.clone());
        }
        // SAFETY: guarded access to this device for matching CoreVideo cache creation.
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: live Metal device and initialized out-pointer.
        if unsafe { objc2_core_video::CVMetalTextureCache::create(None, None, hal.raw_device(), None, NonNull::from(&mut raw)) } != 0 {
            return None;
        }
        // SAFETY: successful create returned a +1 cache.
        let cache = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        *slot = Some((device.clone(), cache.clone()));
        Some(cache)
    })
}

pub(crate) fn import(device: &wgpu::Device, native: &Arc<dyn NativeYuv>) -> Option<filmcraft_gpu::NativeImport> {
    use objc2_core_video::*;
    use objc2_metal::{MTLPixelFormat, MTLTexture, MTLTextureType};
    let surface = native.as_any().downcast_ref::<Surface>()?;
    if surface.bits == 10 && !device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) {
        return None;
    }
    let cache = texture_cache(device)?;
    let (cw, ch) = ((surface.w as usize).div_ceil(2), (surface.h as usize).div_ceil(if surface.chroma == Chroma::C420 { 2 } else { 1 }));
    let ten = surface.bits == 10;
    let specs = [
        (
            surface.w as usize,
            surface.h as usize,
            if ten { MTLPixelFormat::R16Unorm } else { MTLPixelFormat::R8Unorm },
            if ten { wgpu::TextureFormat::R16Unorm } else { wgpu::TextureFormat::R8Unorm },
        ),
        (
            cw,
            ch,
            if ten { MTLPixelFormat::RG16Unorm } else { MTLPixelFormat::RG8Unorm },
            if ten { wgpu::TextureFormat::Rg16Unorm } else { wgpu::TextureFormat::Rg8Unorm },
        ),
    ];
    let mut textures = Vec::new();
    for (i, (w, h, pf, format)) in specs.into_iter().enumerate() {
        let mut raw = std::ptr::null_mut();
        // SAFETY: validated biplanar dimensions and matching formats, read-only device cache.
        if unsafe { CVMetalTextureCache::create_texture_from_image(None, &cache, &surface.buffer, None, pf, w, h, i, NonNull::from(&mut raw)) } != 0 {
            return None;
        }
        // SAFETY: successful creation returned a +1 texture reference.
        let cv = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        let texture = CVMetalTextureGetTexture(&cv)?;
        if texture.textureType() != MTLTextureType::Type2D
            || texture.pixelFormat() != pf
            || texture.width() != w
            || texture.height() != h
            || texture.arrayLength() != 1
            || texture.mipmapLevelCount() != 1
            || texture.sampleCount() != 1
        {
            return None;
        }
        let owner = native.clone();
        let cv = TextureOwner(cv);
        let size = wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 };
        // SAFETY: CoreVideo created this initialized texture on this exact Metal device. The
        // descriptor matches its actual plane and format. The HAL drop callback retains BOTH
        // the CVMetalTexture and native surface until all in-flight GPU references are released.
        let imported = unsafe {
            let texture = wgpu::hal::metal::Device::texture_from_raw(
                texture,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent { width: w as u32, height: h as u32, depth: 1 },
                Some(Box::new(move || {
                    cv.release();
                    drop(owner)
                })),
            );
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                texture,
                &wgpu::TextureDescriptor {
                    label: Some("VideoToolbox plane"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::TextureUses::RESOURCE,
            )
        };
        textures.push(imported.create_view(&Default::default()));
    }
    Some(filmcraft_gpu::NativeImport {
        views: textures.try_into().ok()?,
        chroma: (cw as u32, ch as u32),
        code_scale: if ten { 65535.0 / 64.0 } else { 255.0 },
        bits: surface.bits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_render::{
        Blend,
        plan::{FramePlan, PlanLayer},
    };
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFString, CFType};
    use objc2_core_video::*;
    fn buffer(w: u32, h: u32, bits: u32) -> CFRetained<CVImageBuffer> {
        let io = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let yes = CFBoolean::new(true);
        // SAFETY: immutable CoreVideo attribute keys.
        let keys = unsafe { [kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey] };
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &[io.as_ref(), yes.as_ref()]);
        let mut raw = std::ptr::null_mut();
        assert_eq!(
            // SAFETY: bounded test dimensions, supported pixel format and initialized out-pointer.
            unsafe {
                CVPixelBufferCreate(
                    None,
                    w as usize,
                    h as usize,
                    u32::from_be_bytes(if bits == 8 { *b"420v" } else { *b"x420" }),
                    Some(attrs.as_ref()),
                    NonNull::from(&mut raw),
                )
            },
            0
        );
        // SAFETY: successful create returns a +1 live buffer.
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) };
        // SAFETY: exclusive access to the newly allocated image while filling the test fixture.
        assert_eq!(unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) }, 0);
        for i in 0..2 {
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, i);
            let ph = CVPixelBufferGetHeightOfPlane(&buffer, i);
            let ptr = CVPixelBufferGetBaseAddressOfPlane(&buffer, i).cast::<u8>();
            assert!(!ptr.is_null());
            // SAFETY: successfully locked mapped plane, exactly its stride*height allocation.
            let bytes = unsafe { std::slice::from_raw_parts_mut(ptr, stride * ph) };
            for (y, row) in bytes.chunks_exact_mut(stride).enumerate() {
                if bits == 8 {
                    for (x, p) in row.iter_mut().enumerate() {
                        *p = if i == 0 { 16 + ((x + 3 * y) % 220) as u8 } else { 64 + ((x * 3 + y) % 128) as u8 };
                    }
                } else {
                    for (x, p) in row.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                        let code = if i == 0 { 64 + ((x + 3 * y) % 877) as u16 } else { 256 + ((x * 3 + y) % 512) as u16 };
                        *p = (code << 6).to_ne_bytes();
                    }
                }
            }
        }
        // SAFETY: balances the fixture write lock before publishing its immutable image.
        unsafe {
            CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty());
        }
        buffer
    }
    fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_features: adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM,
            ..Default::default()
        }))
        .ok()
    }
    #[test]
    fn cpu_snapshot_is_lazy_stable_and_exact_and_native_owners_survive_eviction() {
        let Some((device, queue)) = device() else { return };
        filmcraft_gpu::register_native_import(import);
        for bits in [8, 10] {
            let pb = buffer(130, 74, bits);
            assert!(frame(&pb, 0, 74, Chroma::C420, bits).is_none());
            assert!(frame(&pb, 130, 74, Chroma::C420, if bits == 8 { 10 } else { 8 }).is_none());
            assert!(frame(&pb, 130, 73, Chroma::C420, bits).is_none());
            let mut f = frame(&pb, 130, 74, Chroma::C420, bits).unwrap();
            f.color = filmcraft_color::ColorInfo::REC709;
            let PixelData::Native(owner) = &f.data else { panic!("native") };
            let surface = owner.as_any().downcast_ref::<Surface>().unwrap();
            assert!(surface.cpu.get().is_none());
            let charge = f.byte_size();
            let legacy = crate::videotoolbox::legacy_frame(&pb, 130, 74, bits).unwrap();
            let mut c = filmcraft_gpu::GpuCompositor::new(&device, &queue);
            let plan = FramePlan::Layers {
                width: 130,
                height: 74,
                layers: vec![PlanLayer::new(Arc::new(f.clone()), filmcraft_geom::Affine::IDENTITY, 1.0, Blend::Normal)],
            };
            c.composite(&plan);
            assert_eq!(c.uploaded_bytes, 0);
            assert!(surface.cpu.get().is_none());
            let cpu = f.materialized();
            assert_eq!(charge, f.byte_size());
            assert!(surface.cpu.get().is_some());
            assert_eq!(cpu.to_rgba8(), legacy.to_rgba8());
            c.clear_source_cache();
            drop(plan);
            drop(f);
            drop(pb);
            let (_, _, actual) = c.read_output().unwrap();
            let mut expected = filmcraft_gpu::GpuCompositor::new(&device, &queue);
            expected.composite(&FramePlan::Layers {
                width: 130,
                height: 74,
                layers: vec![PlanLayer::new(Arc::new(legacy), filmcraft_geom::Affine::IDENTITY, 1.0, Blend::Normal)],
            });
            let (_, _, reference) = expected.read_output().unwrap();
            assert!(actual.iter().zip(reference).all(|(&a, b)| a.abs_diff(b) <= 1));
        }
    }
    #[test]
    #[ignore = "Native decode/upload benchmark; actual Metal adapter, --nocapture"]
    fn bench_native_decode_upload() {
        let (device, queue) = device().expect("Metal adapter");
        filmcraft_gpu::register_native_import(import);
        let mut c = filmcraft_gpu::GpuCompositor::new(&device, &queue);
        for (w, h) in [(1920, 1080), (3840, 2160)] {
            for bits in [8, 10] {
                let pb = buffer(w, h, bits);
                let mut times = [Vec::new(), Vec::new()];
                for round in 0..5 {
                    for index in [round % 2, 1 - round % 2] {
                        let run = |c: &mut filmcraft_gpu::GpuCompositor| {
                            c.clear_source_cache();
                            let f = if index == 0 {
                                crate::videotoolbox::legacy_frame(&pb, w, h, bits).unwrap()
                            } else {
                                let mut f = frame(&pb, w, h, Chroma::C420, bits).unwrap();
                                f.color = filmcraft_color::ColorInfo::REC709;
                                f
                            };
                            let plan = FramePlan::Layers {
                                width: w as usize,
                                height: h as usize,
                                layers: vec![PlanLayer::new(Arc::new(f), filmcraft_geom::Affine::IDENTITY, 1.0, Blend::Normal)],
                            };
                            let before = c.uploaded_bytes;
                            c.composite(&plan);
                            if index == 1 {
                                assert_eq!(before, c.uploaded_bytes);
                            }
                            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                        };
                        for _ in 0..2 {
                            run(&mut c);
                        }
                        let start = std::time::Instant::now();
                        for _ in 0..6 {
                            run(&mut c);
                        }
                        times[index].push(start.elapsed().as_secs_f64() * 1000.0 / 6.0);
                    }
                }
                for t in &mut times {
                    t.sort_by(f64::total_cmp);
                }
                eprintln!("native {w}x{h} {bits}-bit: copy/upload {:.3}ms -> import {:.3}ms ({:.2}x)", times[0][2], times[1][2], times[0][2] / times[1][2]);
            }
        }
    }
}
