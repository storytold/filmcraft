//! macOS encoder bridge: the completed wgpu Metal accumulator is converted directly into
//! IOSurface-backed NV12 planes. No CPU mapping, readback, RGB array or CPU RGB→YUV conversion.
//! All unsafe access is contained in this OS media FFI module; unsupported frames fall back.
use filmcraft_export::{FrameRenderer, NativeFrame};
use filmcraft_project::{ItemId, Project};
use filmcraft_render::{RenderOptions, SourceProvider};
use filmcraft_time::Tick;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_video::{CVImageBuffer, CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBufferPool, kCVPixelBufferIOSurfacePropertiesKey};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder, MTLCommandQueue, MTLComputeCommandEncoder, MTLComputePipelineState, MTLDevice, MTLLibrary,
    MTLPixelFormat, MTLSize, MTLTextureUsage,
};
use std::ptr::NonNull;

pub const NATIVE_FORMAT: &str = "videotoolbox.nv12.709";
const NV12: u32 = u32::from_be_bytes(*b"420v");

pub struct Surface {
    pub(crate) buffer: CFRetained<CVImageBuffer>,
}
// SAFETY: CoreVideo pixel buffers may be retained/released across threads. The GPU writes
// finish before this object is returned, and consumers only submit its immutable image to VT.
unsafe impl Send for Surface {}

struct Converter {
    cache: CFRetained<CVMetalTextureCache>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLComputePipelineState>>,
    pool: Option<(CFRetained<CVPixelBufferPool>, (u32, u32))>,
}
// SAFETY: Metal objects and CVMetalTextureCache support use from any thread. This converter
// is accessed exclusively through &mut Renderer; all commands complete before returning.
unsafe impl Send for Converter {}

impl Converter {
    fn new(device: &wgpu::Device) -> Option<Self> {
        // SAFETY: backend guard lives throughout access; no wgpu resources are mutated. Only
        // Metal devices are accepted, and the retained device outlives newly created objects.
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?;
        let metal = hal.raw_device();
        let options = objc2_metal::MTLCompileOptions::new();
        #[allow(deprecated, reason = "supports macOS versions before Metal mathMode was introduced")]
        options.setFastMathEnabled(false);
        let lib = metal.newLibraryWithSource_options_error(&NSString::from_str(include_str!("gpu_export.metal")), Some(&options)).ok()?;
        let function = lib.newFunctionWithName(&NSString::from_str("nv12"))?;
        let pipeline = metal.newComputePipelineStateWithFunction_error(&function).ok()?;
        let queue = metal.newCommandQueue()?;
        let mut raw = std::ptr::null_mut();
        // SAFETY: valid retained Metal device and initialized out-pointer; CoreVideo retains
        // its device. A successful create returns a +1 cache reference.
        let status = unsafe { CVMetalTextureCache::create(None, None, metal, None, NonNull::from(&mut raw)) };
        if status != 0 {
            return None;
        }
        let ptr = NonNull::new(raw)?;
        // SAFETY: successful create returned this live cache at +1.
        let cache = unsafe { CFRetained::from_raw(ptr) };
        Some(Self { cache, queue, pipeline, pool: None })
    }

    fn convert(&mut self, source: &wgpu::Texture, w: u32, h: u32) -> Option<Surface> {
        if w == 0 || h == 0 || w > 8192 || h > 8192 || !w.is_multiple_of(2) || !h.is_multiple_of(2) {
            return None;
        }
        if self.pool.as_ref().is_none_or(|(_, size)| *size != (w, h)) {
            let pf = CFNumber::new_i32(NV12 as i32);
            let cw = CFNumber::new_i32(w as i32);
            let ch = CFNumber::new_i32(h as i32);
            let io = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
            let yes = objc2_core_foundation::CFBoolean::new(true);
            // SAFETY: immutable framework constants.
            let keys = unsafe {
                [
                    objc2_core_video::kCVPixelBufferPixelFormatTypeKey,
                    kCVPixelBufferIOSurfacePropertiesKey,
                    objc2_core_video::kCVPixelBufferMetalCompatibilityKey,
                    objc2_core_video::kCVPixelBufferWidthKey,
                    objc2_core_video::kCVPixelBufferHeightKey,
                ]
            };
            let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &[pf.as_ref(), io.as_ref(), yes.as_ref(), cw.as_ref(), ch.as_ref()]);
            let mut raw = std::ptr::null_mut();
            // SAFETY: live attribute dictionary and initialized out-pointer; bounded dimensions.
            let status = unsafe { CVPixelBufferPool::create(None, None, Some(attrs.as_ref()), NonNull::from(&mut raw)) };
            if status != 0 {
                return None;
            }
            // SAFETY: successful create returned a +1 pool reference.
            self.pool = Some((unsafe { CFRetained::from_raw(NonNull::new(raw)?) }, (w, h)));
        }
        let pool = &self.pool.as_ref()?.0;
        let mut raw = std::ptr::null_mut();
        // SAFETY: live pool with validated NV12 attributes and initialized out-pointer.
        let status = unsafe { CVPixelBufferPool::create_pixel_buffer(None, pool, NonNull::from(&mut raw)) };
        if status != 0 {
            return None;
        }
        // SAFETY: successful allocation returned a +1 buffer.
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw)?) };
        let usage = CFNumber::new_i64(MTLTextureUsage::ShaderWrite.0 as i64);
        // SAFETY: immutable framework constant.
        let usage_key = unsafe { objc2_core_video::kCVMetalTextureUsage };
        let tex_attrs = CFDictionary::<CFString, CFType>::from_slices(&[usage_key], &[usage.as_ref()]);
        let plane = |format, width, height, index| -> Option<CFRetained<CVMetalTexture>> {
            let mut raw = std::ptr::null_mut();
            // SAFETY: the live IOSurface-backed NV12 buffer has exactly these two planes;
            // cache/device, dimensions and pixel formats match. The out-pointer is initialized.
            let status = unsafe {
                CVMetalTextureCache::create_texture_from_image(
                    None,
                    &self.cache,
                    &buffer,
                    Some(tex_attrs.as_ref()),
                    format,
                    width,
                    height,
                    index,
                    NonNull::from(&mut raw),
                )
            };
            if status != 0 {
                return None;
            }
            // SAFETY: successful create returned a +1 CVMetalTexture; retained through GPU work.
            Some(unsafe { CFRetained::from_raw(NonNull::new(raw)?) })
        };
        let y = plane(MTLPixelFormat::R8Unorm, w as usize, h as usize, 0)?;
        let uv = plane(MTLPixelFormat::RG8Unorm, w as usize / 2, h as usize / 2, 1)?;
        let yt = CVMetalTextureGetTexture(&y)?;
        let uvt = CVMetalTextureGetTexture(&uv)?;
        // SAFETY: wgpu has completed all writes before convert; this guard retains backend
        // resource access throughout encoding. The source is read-only and never altered.
        let hal = unsafe { source.as_hal::<wgpu::hal::api::Metal>() }?;
        let command = self.queue.commandBuffer()?;
        let encoder = command.computeCommandEncoder()?;
        encoder.setComputePipelineState(&self.pipeline);
        // SAFETY: all three textures are valid on this same device, have matching bounded
        // dimensions and the shader's required read/write usage. They outlive command completion.
        unsafe {
            encoder.setTexture_atIndex(Some(hal.raw_handle()), 0);
            encoder.setTexture_atIndex(Some(&yt), 1);
            encoder.setTexture_atIndex(Some(&uvt), 2);
        }
        encoder.dispatchThreads_threadsPerThreadgroup(
            MTLSize { width: w as usize / 2, height: h as usize / 2, depth: 1 },
            MTLSize { width: 8, height: 8, depth: 1 },
        );
        encoder.endEncoding();
        command.commit();
        command.waitUntilCompleted();
        if command.status() != MTLCommandBufferStatus::Completed {
            return None;
        }
        Some(Surface { buffer })
    }
}

pub struct Renderer {
    gpu: filmcraft_gpu::ExportRenderer,
    converter: Option<Converter>,
}
impl Renderer {
    pub fn new() -> Option<Self> {
        Some(Self { gpu: filmcraft_gpu::ExportRenderer::new()?, converter: None })
    }
}
impl FrameRenderer for Renderer {
    fn render(&mut self, p: &Project, seq: ItemId, t: Tick, opts: RenderOptions, sources: &dyn SourceProvider) -> Option<filmcraft_render::Image> {
        self.gpu.render(p, seq, t, opts, sources)
    }
    fn render_native(
        &mut self,
        p: &Project,
        seq: ItemId,
        t: Tick,
        opts: RenderOptions,
        sources: &dyn SourceProvider,
        format: &'static str,
    ) -> Option<NativeFrame> {
        if format != NATIVE_FORMAT {
            return None;
        }
        let (device, texture, (w, h)) = self.gpu.render_texture(p, seq, t, opts, sources)?;
        if self.converter.is_none() {
            self.converter = Converter::new(&device);
        }
        let surface = self.converter.as_mut()?.convert(&texture, w, h)?;
        Some(NativeFrame { width: w, height: h, format: NATIVE_FORMAT, surface: Box::new(surface) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_media::{Generator, MediaSource, generators::GeneratorSource};
    use filmcraft_project::{ItemKind, Label, MediaClip, MediaRef, SequenceSettings, TrackKind};
    use filmcraft_time::{FrameRate, TimeRange};
    use std::sync::Arc;

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn project() -> (Arc<Project>, ItemId, filmcraft_render::SourceMap) {
        let mut p = Project::new("native");
        let g = GeneratorSource::new(Generator::BarsAndTone, 128, 72, FrameRate::FPS_24, Tick::from_units(24, 24));
        let info = g.info().clone();
        let media = p.add_item(
            "bars",
            Label::Iris,
            ItemKind::Media(MediaClip {
                media: MediaRef::Generator(g.generator.clone()),
                info,
                interpret: Default::default(),
                mark_in: None,
                mark_out: None,
                markers: vec![],
                offline: false,
                proxy: None,
                identity: None,
            }),
            None,
        );
        let seq = p.new_sequence("native", SequenceSettings { width: 128, height: 72, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
        let clip =
            p.make_track_item(media, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, FrameRate::FPS_24.tick_of(24)), FrameRate::FPS_24).unwrap();
        p.sequence_mut(seq).unwrap().video_tracks[0].items.push(clip);
        let mut map = filmcraft_render::SourceMap::default();
        map.0.insert(media, Arc::new(g));
        (Arc::new(p), seq, map)
    }

    fn planes(buffer: &CVImageBuffer) -> Vec<Vec<u8>> {
        use objc2_core_video::*;
        struct Lock<'a>(&'a CVImageBuffer);
        impl Drop for Lock<'_> {
            fn drop(&mut self) {
                // SAFETY: this guard owns the matching read-only lock on a still-live buffer.
                unsafe { CVPixelBufferUnlockBaseAddress(self.0, CVPixelBufferLockFlags::ReadOnly) };
            }
        }
        // SAFETY: live buffer, immutable CPU test readback only, after GPU conversion completes.
        assert_eq!(unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) }, 0);
        let _lock = Lock(buffer);
        (0..2)
            .map(|i| {
                let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, i);
                let h = CVPixelBufferGetHeightOfPlane(buffer, i);
                let w = CVPixelBufferGetWidthOfPlane(buffer, i) * if i == 0 { 1 } else { 2 };
                let base = CVPixelBufferGetBaseAddressOfPlane(buffer, i).cast::<u8>();
                assert!(!base.is_null() && w <= stride);
                // SAFETY: locked buffer maps stride*height readable bytes; metadata dimensions are
                // validated above and the slice is consumed before the guard unlocks.
                let bytes = unsafe { std::slice::from_raw_parts(base, stride * h) };
                bytes.chunks(stride).flat_map(|row| row[..w].iter().copied()).collect()
            })
            .collect()
    }

    #[test]
    fn native_nv12_matches_portable_color_conversion_and_encodes() {
        let _guard = TEST_LOCK.lock().unwrap();
        let Some(mut renderer) = Renderer::new() else {
            eprintln!("no Metal adapter; skipping");
            return;
        };
        let (p, seq, map) = project();
        let opts = RenderOptions::default();
        let linear = renderer.render(&p, seq, Tick::ZERO, opts, &map).unwrap();
        let rgba = linear.over_black_rgba8();
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        filmcraft_export::rgba_to_yuv420_8(&rgba, 128, 72, &mut y, &mut u, &mut v);
        let native = renderer.render_native(&p, seq, Tick::ZERO, opts, &map, NATIVE_FORMAT).expect("direct Metal NV12 path");
        let surface = native.surface.downcast_ref::<Surface>().unwrap();
        let actual = planes(&surface.buffer);
        let expected_uv: Vec<_> = u.iter().zip(&v).flat_map(|(&u, &v)| [u, v]).collect();
        assert_eq!(actual[0], y, "luma codes");
        assert_eq!(actual[1], expected_uv, "chroma codes");
        for format in [filmcraft_export::Format::H264, filmcraft_export::Format::Hevc] {
            let settings = filmcraft_export::ExportSettings { hardware_encoding: filmcraft_export::HardwareEncoding::Auto, format, ..Default::default() };
            let config = crate::hardware_encode::config_for(128, 72, FrameRate::FPS_24, &settings).unwrap();
            let mut vt = crate::videotoolbox_encode::VtEncoder::new(config).expect("hardware encoder");
            let mut packets = Vec::new();
            for index in 0..8 {
                packets.extend(vt.encode_buffer(index, &surface.buffer).unwrap());
            }
            packets.extend(vt.flush().unwrap());
            assert_eq!(packets.len(), 8, "{format:?}");
            assert!(vt.parameter_sets().is_some());
        }
        assert!(renderer.render_native(&p, seq, Tick::ZERO, opts, &map, "unknown").is_none());
    }

    #[test]
    fn exporter_uses_native_frames_and_preserves_fallback_for_overlays() {
        let _guard = TEST_LOCK.lock().unwrap();
        if Renderer::new().is_none() {
            eprintln!("no Metal adapter; skipping");
            return;
        }
        filmcraft_export::register_frame_renderer(|| Renderer::new().map(|r| Box::new(r) as Box<dyn FrameRenderer>));
        filmcraft_export::register_encoder(crate::hardware_encode::videotoolbox_encoder_factory);
        let (p, seq, map) = project();
        let dir = std::env::temp_dir().join(format!("filmcraft-native-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for overlay in [false, true] {
            let mut settings = filmcraft_export::ExportSettings {
                path: dir.join(format!("{overlay}.mp4")).to_string_lossy().into_owned(),
                format: filmcraft_export::Format::H264,
                hardware_encoding: filmcraft_export::HardwareEncoding::Auto,
                gpu_rendering: filmcraft_export::GpuRendering::Auto,
                ..Default::default()
            };
            settings.effects.name_overlay.enabled = overlay;
            let progress = filmcraft_export::Progress::default();
            let before = filmcraft_gpu::export_renderer::timing::get();
            let stats = filmcraft_export::hw_encode_stats();
            let report = filmcraft_export::export(&p, seq, &settings, &map, &progress).unwrap();
            assert_eq!(report.frames, 24);
            let after = filmcraft_gpu::export_renderer::timing::get();
            if !overlay {
                assert_eq!(after.frames, before.frames, "native path never reads back a float image");
            } else {
                assert!(after.frames > before.frames, "overlay retains portable path");
            }
            assert!(filmcraft_export::hw_encode_stats().frames > stats.frames);
            let bytes: Arc<[u8]> = std::fs::read(&settings.path).unwrap().into();
            let source = filmcraft_codecs::open_bytes("native.mp4", bytes).unwrap();
            assert_eq!(source.info().video.as_ref().unwrap().width, 128);
            let frame = source.video_frame(filmcraft_media::FrameRequest::full(Tick::ZERO)).unwrap();
            assert_eq!((frame.width, frame.height), (128, 72));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
