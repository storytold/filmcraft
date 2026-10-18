use super::*;
use filmcraft_geom::{Affine, Vec2};
use filmcraft_render::Blend;
use filmcraft_render::plan::execute_cpu;

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_surface_owners_remain_send_sync() {
    fn check<T: Send + Sync>() {}
    check::<SurfaceKeepalive>();
    check::<ImportedSurface>();
    check::<GpuCompositor>();
}

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}

#[derive(Debug)]
struct CacheSurface {
    id: u64,
    luma: wgpu::Texture,
    chroma: wgpu::Texture,
}

impl filmcraft_frame::GpuSurface for CacheSurface {
    fn size(&self) -> (u32, u32) {
        (2, 2)
    }
    fn byte_len(&self) -> usize {
        2 * 2 + 2
    }
    fn id(&self) -> u64 {
        self.id
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn prepare_cpu(&self, _: filmcraft_frame::CpuTransfer) {
        panic!("cache-only GPU import must not download pixels");
    }
}

fn import_cache_surface(surface: &dyn filmcraft_frame::GpuSurface, _: &wgpu::Device, _: &wgpu::Queue) -> Result<ImportedSurface, String> {
    let s = surface.as_any().downcast_ref::<CacheSurface>().ok_or("wrong test surface")?;
    if s.luma.format() == wgpu::TextureFormat::Rgba8UnormSrgb {
        return Ok(ImportedSurface::Rgba { texture: s.luma.clone(), keepalive: Box::new(()) });
    }
    Ok(ImportedSurface::Yuv { luma: s.luma.clone(), chroma: s.chroma.clone(), keepalive: Box::new(()) })
}

#[test]
fn imported_rgba_matches_uploaded_rgba_without_cpu_download() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    set_surface_importer(import_cache_surface);
    let pixels = vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 255, 255, 255, 0];
    let texture = dev.create_texture(&wgpu::TextureDescriptor {
        label: Some("imported-rgba-test"),
        size: wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    q.write_texture(
        texture.as_image_copy(),
        &pixels,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(2 * 4), rows_per_image: Some(2) },
        texture.size(),
    );
    let surface = Arc::new(CacheSurface { id: u64::MAX, luma: texture.clone(), chroma: texture });
    let cpu = VideoFrame {
        width: 2,
        height: 2,
        data: PixelData::Rgba8(Arc::new(pixels)),
        color: filmcraft_color::ColorInfo::SRGB_FULL,
        par: (1, 1),
        pts: Default::default(),
    };
    let gpu = VideoFrame { data: PixelData::Gpu(filmcraft_frame::GpuPixels::new(surface, filmcraft_frame::GpuFormat::Rgba8)), ..cpu.clone() };
    let plan = |frame| FramePlan::Layers { width: 5, height: 5, layers: vec![PlanLayer::new(Arc::new(frame), Affine::IDENTITY, 1.0, Blend::Normal)] };
    let mut compositor = GpuCompositor::new(&dev, &q);
    compositor.composite(&plan(gpu)).unwrap();
    let imported = compositor.read_output().unwrap();
    assert_eq!(compositor.uploaded_bytes, 0);
    compositor.composite(&plan(cpu)).unwrap();
    assert_eq!(imported, compositor.read_output().unwrap());
}

#[test]
fn imported_sources_obey_cache_budget_and_release_evicted_owners() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    set_surface_importer(import_cache_surface);
    let texture = |size, format| {
        dev.create_texture(&wgpu::TextureDescriptor {
            label: Some("cache-lifetime-test"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    };
    let luma = texture(wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 }, wgpu::TextureFormat::R8Unorm);
    let chroma = texture(wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 }, wgpu::TextureFormat::Rg8Unorm);
    let mut c = GpuCompositor::new(&dev, &q);
    let mut owners = Vec::new();
    for id in 1..=(SOURCE_CACHE_CAPACITY * 2) {
        let surface = Arc::new(CacheSurface { id: id as u64, luma: luma.clone(), chroma: chroma.clone() });
        owners.push(Arc::downgrade(&surface));
        let f = VideoFrame {
            width: 2,
            height: 2,
            data: PixelData::Gpu(filmcraft_frame::GpuPixels::new(surface, filmcraft_frame::GpuFormat::Yuv { chroma: Chroma::C420, bits: 8 })),
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: Default::default(),
        };
        c.upload(&f, None).unwrap();
        assert!(c.uploads.len() <= SOURCE_CACHE_CAPACITY);
    }
    assert_eq!(owners.iter().filter(|w| w.strong_count() > 0).count(), SOURCE_CACHE_CAPACITY);
    assert!(owners[..SOURCE_CACHE_CAPACITY].iter().all(|w| w.strong_count() == 0));
    assert_eq!(c.uploaded_bytes, 0);
    drop(c);
    assert!(owners.iter().all(|w| w.strong_count() == 0));
}

#[test]
fn yuv_code_range_uniforms() {
    let mut mismatches = Vec::new();
    for bits in [8, 10, 12, 16] {
        let levels = (1u32 << bits) as f32;
        for range in [Range::Full, Range::Limited] {
            let mut src = SrcInfo {
                kind: 2,
                code_scale: if bits == 8 { 255.0 } else { levels },
                code_levels: levels,
                alpha: 0.0,
                chroma: (1, 1),
                size: (1, 1),
                color: filmcraft_color::ColorInfo { range, ..filmcraft_color::ColorInfo::REC709 },
                input: None,
            };
            let u = GpuCompositor::uniforms(&src, &Affine::IDENTITY, 1.0, Blend::Normal, (1, 1));
            // Full: 0..2^bits-1, neutral 2^(bits-1). Limited: BT.709 8-bit codes * 2^(bits-8).
            let expected = match range {
                Range::Full => [0.0, levels - 1.0, levels / 2.0, levels - 1.0],
                Range::Limited => [16.0, 219.0, 128.0, 224.0].map(|v| v * (1u32 << (bits - 8)) as f32),
            };
            if u[16..20] != expected {
                mismatches.push(format!("{bits}-bit {range:?}: {:?} != {expected:?}", &u[16..20]));
            }
            src.kind = 3;
            let imported = GpuCompositor::uniforms(&src, &Affine::IDENTITY, 1.0, Blend::Normal, (1, 1));
            assert_eq!(&imported[16..20], &expected, "biplanar {bits}-bit {range:?}");
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn imported_yuv_preserves_managed_input_transform() {
    use filmcraft_color::{ColorInfo, ColorPipeline, ColorSpace, InputTransform, Matrix, Primaries, Transfer};
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    set_surface_importer(import_cache_surface);
    let texture = |width, height, format, bytes: &[u8], bytes_per_row| {
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = dev.create_texture(&wgpu::TextureDescriptor {
            label: Some("managed-import-test"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        q.write_texture(
            texture.as_image_copy(),
            bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bytes_per_row), rows_per_image: None },
            size,
        );
        texture
    };
    // Limited-range luma ramp + non-neutral chroma; 2x2 NV12 matches one planar 4:2:0 cell.
    let y = vec![64, 144, 200, 235];
    let uv = [90, 200];
    let surface = Arc::new(CacheSurface {
        id: u64::MAX - 2,
        luma: texture(2, 2, wgpu::TextureFormat::R8Unorm, &y, 2),
        chroma: texture(1, 1, wgpu::TextureFormat::Rg8Unorm, &uv, 2),
    });
    let color = ColorInfo { matrix: Matrix::Bt2020Ncl, transfer: Transfer::Hlg, primaries: Primaries::Bt2020, range: Range::Limited };
    let cpu_frame = VideoFrame {
        width: 2,
        height: 2,
        data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(vec![uv[0]]), Arc::new(vec![uv[1]])], chroma: Chroma::C420, alpha: None },
        color,
        par: (1, 1),
        pts: Default::default(),
    };
    let gpu_frame = VideoFrame {
        data: PixelData::Gpu(filmcraft_frame::GpuPixels::new(surface, filmcraft_frame::GpuFormat::Yuv { chroma: Chroma::C420, bits: 8 })),
        ..cpu_frame.clone()
    };
    let transform = Arc::new(InputTransform::new(ColorSpace::from_info(&color), color.range, &ColorPipeline::REC709, None));
    assert!(transform.gpu_params().is_some());
    let plan = |frame, input| FramePlan::Layers {
        width: 2,
        height: 2,
        layers: vec![PlanLayer { input, ..PlanLayer::new(Arc::new(frame), Affine::IDENTITY, 1.0, Blend::Normal) }],
    };
    let cpu = execute_cpu(&plan(cpu_frame, Some(transform.clone()))).unwrap().over_black_rgba8();
    let mut compositor = GpuCompositor::new(&dev, &q);
    compositor.composite(&plan(gpu_frame.clone(), Some(transform))).unwrap();
    let (_, _, managed) = compositor.read_output().expect("managed readback");
    compositor.composite(&plan(gpu_frame, None)).unwrap();
    let (_, _, unmanaged) = compositor.read_output().expect("unmanaged readback");
    let max_error = |pixels: &[u8]| cpu.iter().zip(pixels).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
    eprintln!("imported HLG: managed error {}, unmanaged error {}", max_error(&managed), max_error(&unmanaged));
    // Match the existing HDR parity allowance: two 8-bit output codes for GPU rounding.
    assert!(max_error(&managed) <= 2);
    // More than twenty codes distinguishes the unmanaged control from the two-code rounding allowance.
    assert!(max_error(&unmanaged) > 20, "fixture must respond to input color management");
    assert_eq!(compositor.uploaded_bytes, 0, "resident import uploaded CPU pixels");
}

#[derive(Debug)]
struct CpuFallbackSurface(std::sync::atomic::AtomicUsize);

impl filmcraft_frame::GpuSurface for CpuFallbackSurface {
    fn size(&self) -> (u32, u32) {
        (2, 2)
    }
    fn byte_len(&self) -> usize {
        2 * 2 + 2
    }
    fn id(&self) -> u64 {
        u64::MAX
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn prepare_cpu(&self, transfer: filmcraft_frame::CpuTransfer) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        transfer.complete(Ok(PixelData::Yuv8 { planes: [Arc::new(vec![16; 4]), Arc::new(vec![128]), Arc::new(vec![128])], chroma: Chroma::C420, alpha: None }));
    }
}

#[test]
fn failed_import_fallback_never_caches_or_uploads_substitute_pixels() {
    #[derive(Debug)]
    struct Lost;
    impl filmcraft_frame::GpuSurface for Lost {
        fn size(&self) -> (u32, u32) {
            (2, 2)
        }
        fn byte_len(&self) -> usize {
            6
        }
        fn id(&self) -> u64 {
            u64::MAX - 1
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn prepare_cpu(&self, transfer: filmcraft_frame::CpuTransfer) {
            transfer.complete(Err("injected readback failure".into()));
        }
    }
    let Some((dev, queue)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let frame = Arc::new(VideoFrame {
        data: PixelData::Gpu(filmcraft_frame::GpuPixels::new(Arc::new(Lost), filmcraft_frame::GpuFormat::Yuv { chroma: Chroma::C420, bits: 8 })),
        ..VideoFrame::rgba8(2, 2, vec![0; 2 * 2 * 4])
    });
    let plan = FramePlan::Layers { width: 2, height: 2, layers: vec![PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal)] };
    let mut compositor = GpuCompositor::new(&dev, &queue);
    assert_eq!(compositor.composite(&plan).unwrap_err(), "injected readback failure");
    assert_eq!(compositor.render_export_prepared(&plan, None).unwrap_err(), "injected readback failure");
    assert_eq!(compositor.uploaded_bytes, 0);
    assert!(compositor.uploads.is_empty());

    let good = PlanLayer::new(Arc::new(VideoFrame::rgba8(2, 2, vec![255; 2 * 2 * 4])), Affine::IDENTITY, 1.0, Blend::Normal);
    let valid = FramePlan::Layers { width: 2, height: 2, layers: vec![good.clone()] };
    compositor.composite(&valid).unwrap();
    let expected = compositor.read_output().unwrap();
    let parent = compositor.accum.as_ref().unwrap().0.clone();
    for failed_input in 0..2 {
        let bad = PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal);
        let mut inputs = [vec![good.clone()], vec![good.clone()]];
        inputs[failed_input] = vec![bad];
        let transition = FramePlan::Composite {
            width: 2,
            height: 2,
            steps: vec![PlanStep::Transition { inputs, effect: filmcraft_project::find_effect("push").unwrap().instance(), progress: 0.5, scale: 1.0 }],
        };
        assert_eq!(compositor.composite(&transition).unwrap_err(), "injected readback failure");
        assert_eq!(compositor.accum.as_ref().unwrap().0, parent, "parent accumulator restored after input {failed_input}");
        assert_eq!(compositor.gpu_transitions, 0);
        compositor.composite(&valid).unwrap();
        assert_eq!(compositor.read_output().unwrap(), expected);
    }
}

#[test]
fn unsupported_surface_import_downloads_once_and_reuses_cpu_upload() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    set_surface_importer(import_cache_surface);
    for format in [filmcraft_frame::GpuFormat::Yuv { chroma: Chroma::C420, bits: 8 }, filmcraft_frame::GpuFormat::Rgba8] {
        let surface = Arc::new(CpuFallbackSurface(std::sync::atomic::AtomicUsize::new(0)));
        let frame = Arc::new(VideoFrame {
            width: 2,
            height: 2,
            data: PixelData::Gpu(filmcraft_frame::GpuPixels::new(surface.clone(), format)),
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            pts: Default::default(),
        });
        let plan = FramePlan::Layers { width: 2, height: 2, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] };
        let mut c = GpuCompositor::new(&dev, &q);
        let expected = [0, 0, 0, 255].repeat(2 * 2); // Limited-range Y16/C128 -> opaque black.
        let expected_bytes = 2 * 2 + 2; // 2x2 luma + one sample per chroma plane, all u8.
        c.composite(&plan).unwrap();
        assert_eq!(c.read_output().expect("fallback pixels").2, expected, "{format:?}");
        assert_eq!(c.uploaded_bytes, expected_bytes);
        c.composite(&plan).unwrap();
        assert_eq!(c.read_output().expect("reused fallback pixels").2, expected, "{format:?}");
        assert_eq!(c.uploaded_bytes, expected_bytes);
        assert_eq!(surface.0.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_stage_validation_rejects_invalid_shader() {
    let Some((dev, _)) = device() else { return };
    assert_eq!(validated_stage(&dev, "valid control", || ()), Some(()));
    let invalid = validated_stage(&dev, "injected shader failure", || {
        dev.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("invalid-stage-test"), source: wgpu::ShaderSource::Wgsl("not WGSL".into()) })
    });
    assert!(invalid.is_none(), "validation error must select fallback");
}

#[test]
fn yuv_code_range_gpu_matches_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let mut prepared_compositor = GpuCompositor::new(&dev, &q);
    for bits in [8, 10, 12, 16] {
        let scale = 1u32 << (bits - 8);
        for range in [Range::Full, Range::Limited] {
            // Constant chroma isolates range normalization from CPU/GPU chroma reconstruction.
            for (u, v) in [(128, 128), (64, 192), (192, 64)] {
                let y = (0..=255).map(|code| (code * scale) as u16).collect::<Vec<_>>();
                let planes = [Arc::new(y), Arc::new(vec![(u * scale) as u16; 256]), Arc::new(vec![(v * scale) as u16; 256])];
                let data = if bits == 8 {
                    PixelData::Yuv8 { planes: planes.map(|p| Arc::new(p.iter().map(|v| *v as u8).collect())), chroma: Chroma::C444, alpha: None }
                } else {
                    PixelData::Yuv16 { planes, chroma: Chroma::C444, bits, alpha: None }
                };
                let frame = Arc::new(VideoFrame {
                    width: 256,
                    height: 1,
                    data,
                    color: filmcraft_color::ColorInfo { range, ..filmcraft_color::ColorInfo::REC709 },
                    par: (1, 1),
                    pts: Default::default(),
                });
                let cpu = frame.to_rgba8().unwrap();
                let plan = FramePlan::Layers { width: 256, height: 1, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] };
                c.composite(&plan).unwrap();
                let (w, h, gpu) = c.read_output().expect("readback");
                assert_eq!((w, h, gpu.len()), (256, 1, cpu.len()));
                let error = cpu.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().expect("pixels");
                // One output-code tolerance: CPU f32 vs GPU f16 accumulator quantization.
                assert!(error <= 1, "{bits}-bit {range:?} U={u} V={v}: max error {error}");
                if u == 128 && v == 128 {
                    assert!(gpu.as_chunks::<4>().0.iter().all(|p| p[0] == p[1] && p[1] == p[2]), "neutral tint: {bits}-bit {range:?}");
                }
                let prepared = prepare(&plan);
                prepared_compositor.composite_prepared(&plan, Some(&prepared)).unwrap();
                assert_eq!(prepared_compositor.read_output().expect("prepared readback").2, gpu, "prepared: {bits}-bit {range:?}");
                eprintln!("range gate: {bits}-bit {range:?} U={u} V={v}, max error={error}, prepared=inline");
            }
        }
    }
}

fn yuv_frame(w: u32, h: u32) -> Arc<VideoFrame> {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y: Vec<u8> = (0..w * h).map(|i| (16 + ((i % w) * 219 / w)) as u8).collect();
    let u: Vec<u8> = (0..cw * ch).map(|i| (64 + (i / cw) * 128 / ch) as u8).collect();
    let v: Vec<u8> = (0..cw * ch).map(|i| (200 - (i % cw) * 100 / cw) as u8).collect();
    Arc::new(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Yuv8 { planes: [Arc::new(y), Arc::new(u), Arc::new(v)], chroma: Chroma::C420, alpha: None },
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: Default::default(),
    })
}

#[test]
fn gpu_matches_cpu_plan() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let pixels = filmcraft_media::generators::render(
        &filmcraft_media::Generator::Demo(filmcraft_media::DemoScene::OceanSunset),
        320,
        180,
        1.0,
        24,
        filmcraft_time::FrameRate::FPS_24,
    );
    let rgba = Arc::new(VideoFrame::rgba8(320, 180, pixels));
    let (w, h) = (320usize, 180usize);
    let plan = FramePlan::Layers {
        width: w,
        height: h,
        layers: vec![
            PlanLayer { frame: yuv_frame(640, 360), matrix: Affine::scale(0.5, 0.5), opacity: 1.0, blend: Blend::Normal, fx: None, input: None },
            PlanLayer {
                frame: rgba,
                matrix: Affine::motion(Vec2::new(200.0, 100.0), Vec2::new(0.4, 0.4), 12.0, Vec2::new(160.0, 90.0)),
                opacity: 0.7,
                blend: Blend::Normal,
                fx: None,
                input: None,
            },
        ],
    };
    let cpu = execute_cpu(&plan).unwrap().over_black_rgba8();
    c.composite(&plan).unwrap();
    let (gw, gh, gpu) = c.read_output().expect("readback");
    assert_eq!((gw as usize, gh as usize), (w, h));
    // compare away from antialiased edges: mean abs error and 99th percentile
    let mut diffs: Vec<u32> =
        cpu.chunks(4).zip(gpu.chunks(4)).map(|(a, b)| (0..3).map(|k| (a[k] as i32 - b[k] as i32).unsigned_abs()).max().unwrap_or(0)).collect();
    diffs.sort_unstable();
    let p99 = diffs[diffs.len() * 99 / 100];
    let mean = diffs.iter().sum::<u32>() as f64 / diffs.len() as f64;
    assert!(p99 <= 6 && mean < 1.5, "p99 {p99}, mean {mean}");
    // cache: compositing the same plan again uploads nothing
    let before = c.uploaded_bytes;
    c.composite(&plan).unwrap();
    assert_eq!(c.uploaded_bytes, before);
}

#[test]
fn half_float_conversion() {
    for v in [0.0f32, 1.0, 0.5, 0.123, 65504.0, -2.0, 1e-5] {
        let h = f32_to_f16(v);
        // decode
        let s = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
        let e = ((h >> 10) & 0x1f) as i32;
        let m = (h & 0x3ff) as f32;
        let d = if e == 0 { s * m * 2f32.powi(-24) } else { s * (1.0 + m / 1024.0) * 2f32.powi(e - 15) };
        assert!((d - v).abs() <= v.abs() * 1e-3 + 1e-6, "{v} → {d}");
    }
}

#[test]
fn gpu_push_matches_cpu_with_alpha_and_surrounding_blends() {
    use filmcraft_project::ParamValue;
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let (w, h) = (128usize, 72usize);
    let layer = |side: usize| {
        let px = (0..w * h)
            .flat_map(|i| {
                let u = (i % w % 2) as f32;
                let v = (i / w % 2) as f32;
                let a = if side == 0 { (u + v) / 2.0 } else { 1.0 - (u + v) / 2.0 };
                [u * a, v * a, 0.5 * a, a]
            })
            .collect();
        PlanLayer::new(Arc::new(VideoFrame::rgba_f32(w as u32, h as u32, px)), Affine::IDENTITY, 0.7, Blend::Normal)
    };
    let mut worst = (0, 0.0f64);
    for direction in 0..4 {
        for blur in [0.0, 50.0, 100.0] {
            for progress in [0.0, 0.25, 0.37, 0.5, 0.75, 1.0] {
                let mut effect = filmcraft_project::find_effect("push").unwrap().instance();
                effect.param_mut("direction").unwrap().value = ParamValue::Choice(direction);
                effect.param_mut("motion_blur").unwrap().value = ParamValue::Float(blur);
                let mut above = layer(1);
                above.blend = Blend::Multiply;
                let plan = FramePlan::Composite {
                    width: w,
                    height: h,
                    steps: vec![
                        PlanStep::Layers(vec![layer(1)]),
                        PlanStep::Transition { inputs: [vec![layer(0)], vec![layer(1)]], effect, progress, scale: 1.0 },
                        PlanStep::Layers(vec![above]),
                    ],
                };
                let cpu = execute_cpu(&plan).unwrap().over_black_rgba8();
                c.composite_prepared(&plan, Some(&prepare(&plan))).unwrap();
                let (_, _, gpu) = c.read_output().expect("readback");
                let mut diffs: Vec<u32> =
                    cpu.as_chunks::<4>().0.iter().zip(gpu.as_chunks::<4>().0).map(|(a, b)| (0..3).map(|k| a[k].abs_diff(b[k]) as u32).max().unwrap()).collect();
                diffs.sort_unstable();
                let p99 = diffs[diffs.len() * 99 / 100];
                let mean = diffs.iter().sum::<u32>() as f64 / diffs.len() as f64;
                worst = (worst.0.max(p99), worst.1.max(mean));
                // Existing compositor acceptance band; moving edges included.
                assert!(p99 <= 6 && mean < 1.5, "direction {direction}, blur {blur}, progress {progress}: p99 {p99}, mean {mean}");
            }
        }
    }
    assert_eq!(c.gpu_transitions, 4 * 3 * 6);
    assert_eq!(c.cpu_transitions, 0);
    eprintln!("Push 72 cases: worst p99 {}, mean {}", worst.0, worst.1);
    let fallback = FramePlan::Composite {
        width: w,
        height: h,
        steps: vec![PlanStep::Transition {
            inputs: [vec![layer(0)], vec![layer(1)]],
            effect: filmcraft_project::find_effect("cross_dissolve").unwrap().instance(),
            progress: 0.37,
            scale: 1.0,
        }],
    };
    c.composite(&fallback).unwrap();
    let (_, _, got) = c.read_output().expect("fallback readback");
    let expected = execute_cpu(&fallback).unwrap().over_black_rgba8();
    assert!(got.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1));
    assert_eq!(c.cpu_transitions, 1);
    assert_eq!(c.gpu_transitions, 4 * 3 * 6);

    // Reuse both input targets across ordered transitions; preparation advances past intervening layers.
    let transition = |progress| PlanStep::Transition {
        inputs: [vec![layer(0)], vec![layer(1)]],
        effect: filmcraft_project::find_effect("push").unwrap().instance(),
        progress,
        scale: 1.0,
    };
    let mut middle = layer(0);
    middle.blend = Blend::Screen;
    let stacked = FramePlan::Composite {
        width: w,
        height: h,
        steps: vec![PlanStep::Layers(vec![layer(1)]), transition(0.25), PlanStep::Layers(vec![middle]), transition(0.75)],
    };
    c.composite_prepared(&stacked, Some(&prepare(&stacked))).unwrap();
    let (_, _, gpu) = c.read_output().expect("stacked readback");
    let cpu = execute_cpu(&stacked).unwrap().over_black_rgba8();
    // One byte level tolerates half-float resolve rounding.
    assert!(gpu.iter().zip(cpu).all(|(a, b)| a.abs_diff(b) <= 1), "stacked transitions exceed one byte level");
    assert_eq!(c.gpu_transitions, 4 * 3 * 6 + 2);
    assert_eq!(c.cpu_transitions, 1);
}

#[test]
fn prepared_upload_matches_inline_conversion() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (64u32, 36u32);
    let yuv16 = Arc::new(VideoFrame {
        width: w,
        height: h,
        data: PixelData::Yuv16 {
            planes: [
                Arc::new((0..w * h).map(|i| (64 + (i * 7) % 876) as u16).collect()),
                Arc::new((0..w * h / 2).map(|i| (64 + (i * 13) % 896) as u16).collect()),
                Arc::new((0..w * h / 2).map(|i| (960 - (i * 5) % 896) as u16).collect()),
            ],
            chroma: Chroma::C422,
            bits: 10,
            alpha: None,
        },
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: Default::default(),
    });
    let f32_layer: Vec<f32> = (0..w * h * 4).map(|i| ((i * 37) % 1000) as f32 / 1000.0 * if i % 4 == 3 { 1.0 } else { 0.6 }).collect();
    let rgbaf = Arc::new(VideoFrame::rgba_f32(w, h, f32_layer.clone()));
    let layers = FramePlan::Layers {
        width: w as usize,
        height: h as usize,
        layers: vec![
            PlanLayer { frame: yuv16, matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None },
            PlanLayer { frame: rgbaf, matrix: Affine::scale(0.5, 0.5), opacity: 0.8, blend: Blend::Normal, fx: None, input: None },
        ],
    };
    let image = FramePlan::Image(filmcraft_render::Image { w: w as usize, h: h as usize, px: f32_layer });
    for plan in [layers, image] {
        let mut a = GpuCompositor::new(&dev, &q);
        a.composite(&plan).unwrap();
        let inline = a.read_output().expect("readback");
        let mut b = GpuCompositor::new(&dev, &q);
        let prep = prepare(&plan);
        assert!(prep.bytes() > 0);
        b.composite_prepared(&plan, Some(&prep)).unwrap();
        let prepared = b.read_output().expect("readback");
        assert!(inline == prepared, "prepared upload differs");
    }
}

/// The upload cache is keyed by pixel-buffer address, so it must keep the buffer alive: otherwise
/// a new frame allocated at a freed frame's address is drawn with the stale texture (seen as
/// whole frames from a previous composite in `crates/golden` GPU parity).
#[test]
fn upload_cache_keeps_buffers_alive() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    let px = Arc::new(vec![200u8; 16 * 8 * 4]);
    let frame = Arc::new(VideoFrame { width: 16, height: 8, data: PixelData::Rgba8(px.clone()), ..(*yuv_frame(16, 8)).clone() });
    let plan = FramePlan::Layers {
        width: 16,
        height: 8,
        layers: vec![PlanLayer { frame, matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None }],
    };
    c.composite(&plan).unwrap();
    drop(plan);
    assert!(Arc::strong_count(&px) > 1, "cached upload must own its pixel buffer");
}

/// The WGSL tetrahedral LUT matches `Lut3d::apply` on the CPU.
#[test]
fn gpu_lut_matches_cpu_tetrahedral() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let g = GpuLut::new(&dev, &q);
    for size in [2usize, 17, 33, 65] {
        let mut lut = filmcraft_color::Lut3d::from_fn(size, |c| {
            [(c[0] * c[1] + c[2] * c[2]).sin(), c[1].powf(0.45) * (1.0 - 0.3 * c[0]), (c[0] - c[2]).abs() + 0.1 * c[1]]
        });
        if size == 17 {
            lut.domain_min = [-0.1, 0.0, 0.0];
            lut.domain_max = [1.2, 1.0, 2.0];
        }
        let mut s = 99u64;
        let mut px = Vec::new();
        for _ in 0..70_000 {
            for k in 0..4 {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                px.push(if k == 3 { 0.5 } else { (s % 10_001) as f32 / 10_000.0 * 1.2 - 0.1 });
            }
        }
        let gpu = g.apply(&lut, &px).expect("gpu lut");
        let mut worst = 0f32;
        for (i, p) in px.as_chunks::<4>().0.iter().enumerate() {
            let c = lut.apply([p[0], p[1], p[2]]);
            for k in 0..3 {
                worst = worst.max((c[k] - gpu[i * 4 + k]).abs());
            }
            assert_eq!(gpu[i * 4 + 3], 0.5);
        }
        eprintln!("LUT {size}³: max |cpu − gpu| = {worst:e}");
        assert!(worst < 1e-4, "size {size}: max |cpu - gpu| = {worst}");
    }
}

fn test_masks(scale: f32) -> Vec<filmcraft_render::mask::FlatMask> {
    use filmcraft_project::{Mask, MaskMode, MaskPath, ParamValue};
    let mut a = Mask::new("a", MaskPath::ellipse(Vec2::new(150.0, 90.0), Vec2::new(90.0, 55.0)));
    a.feather.value = ParamValue::Float(24.0);
    a.expansion.value = ParamValue::Float(6.0);
    let mut b = Mask::new("b", MaskPath::polygon(&[Vec2::new(40.0, 20.0), Vec2::new(260.0, 50.0), Vec2::new(120.0, 170.0)]));
    b.mode = MaskMode::Subtract;
    b.feather.value = ParamValue::Float(0.0);
    b.opacity.value = ParamValue::Float(70.0);
    let mut c = Mask::new("c", MaskPath::rect(200.0, 100.0, 300.0, 175.0));
    c.mode = MaskMode::Difference;
    c.inverted = true;
    c.feather.value = ParamValue::Float(3.5);
    filmcraft_render::mask::prepare(&[a, b, c], filmcraft_time::Tick(0), scale)
}

#[test]
fn gpu_mask_coverage_matches_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let g = crate::GpuMask::new(&dev, &q);
    for scale in [1.0f32, 0.5] {
        let masks = test_masks(scale);
        let (w, h) = ((320.0 * scale) as usize, (180.0 * scale) as usize);
        let cpu = filmcraft_render::mask::coverage(&masks, w, h).unwrap();
        let gpu = g.coverage(&masks, w, h).expect("gpu coverage");
        let worst = cpu.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        eprintln!("mask coverage @{scale}: max |cpu − gpu| = {worst:e}");
        assert!(worst < 2e-4, "scale {scale}: {worst}");
        assert!(cpu.iter().any(|v| *v > 0.99) && cpu.iter().any(|v| *v < 0.01) && cpu.iter().any(|v| *v > 0.2 && *v < 0.8));
    }
}

#[test]
fn gpu_masked_mix_matches_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let g = crate::GpuMask::new(&dev, &q);
    let masks = test_masks(1.0);
    let (w, h) = (320usize, 180usize);
    let original = filmcraft_render::Image { w, h, px: (0..w * h * 4).map(|i| ((i * 37) % 101) as f32 / 100.0).collect() };
    let mut effected = original.clone();
    effected.px.iter_mut().for_each(|v| *v = 1.0 - *v * 0.5);
    let gpu = g.mix(&masks, w, h, &original.px, &effected.px).expect("gpu mix");
    let cov = filmcraft_render::mask::coverage(&masks, w, h).unwrap();
    let mut cpu = effected.clone();
    filmcraft_render::mask::mix(&mut cpu, &original, &cov);
    let worst = cpu.px.iter().zip(&gpu).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    eprintln!("masked mix: max |cpu − gpu| = {worst:e}");
    assert!(worst < 2e-4, "{worst}");
}

/// A 4:4:4 grey picture with an alpha plane that ramps from transparent (left) to opaque (right).
fn yuv_alpha_frame(w: u32, h: u32, bits: u32) -> Arc<VideoFrame> {
    let n = (w * h) as usize;
    let max = (1u32 << bits) - 1;
    let ramp = |i: usize| ((i % w as usize) as u32 * max / (w - 1)) as u16;
    let data = if bits == 8 {
        PixelData::Yuv8 {
            planes: [Arc::new(vec![180u8; n]), Arc::new(vec![100u8; n]), Arc::new(vec![170u8; n])],
            chroma: Chroma::C444,
            alpha: Some(Arc::new((0..n).map(|i| ramp(i) as u8).collect())),
        }
    } else {
        let s = |v: u32| (v << (bits - 8)) as u16;
        PixelData::Yuv16 {
            planes: [Arc::new(vec![s(180); n]), Arc::new(vec![s(100); n]), Arc::new(vec![s(170); n])],
            chroma: Chroma::C444,
            bits,
            alpha: Some(Arc::new((0..n).map(ramp).collect())),
        }
    };
    Arc::new(VideoFrame { width: w, height: h, data, color: filmcraft_color::ColorInfo::REC709, par: (1, 1), pts: Default::default() })
}

/// A Y'CbCr layer with an alpha plane (ProRes 4444) is drawn with its alpha: the GPU result matches
/// the CPU compositor over an opaque background. (It used to draw such a layer opaque.)
#[test]
fn gpu_draws_yuv_alpha_plane_like_the_cpu() {
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let (w, h) = (320usize, 180usize);
    let pixels = filmcraft_media::generators::render(
        &filmcraft_media::Generator::Demo(filmcraft_media::DemoScene::OceanSunset),
        320,
        180,
        1.0,
        24,
        filmcraft_time::FrameRate::FPS_24,
    );
    let background = Arc::new(VideoFrame::rgba8(320, 180, pixels));
    for bits in [8, 10, 12] {
        let mut c = GpuCompositor::new(&dev, &q);
        let plan = FramePlan::Layers {
            width: w,
            height: h,
            layers: vec![
                PlanLayer { frame: background.clone(), matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None },
                PlanLayer { frame: yuv_alpha_frame(320, 180, bits), matrix: Affine::IDENTITY, opacity: 0.9, blend: Blend::Normal, fx: None, input: None },
            ],
        };
        let cpu = execute_cpu(&plan).unwrap().over_black_rgba8();
        c.composite(&plan).unwrap();
        let (_, _, gpu) = c.read_output().expect("readback");
        let mut diffs: Vec<u32> =
            cpu.chunks(4).zip(gpu.chunks(4)).map(|(a, b)| (0..3).map(|k| (a[k] as i32 - b[k] as i32).unsigned_abs()).max().unwrap_or(0)).collect();
        diffs.sort_unstable();
        let (p99, max) = (diffs[diffs.len() * 99 / 100], diffs[diffs.len() - 1]);
        assert!(p99 <= 3 && max <= 8, "{bits}-bit: p99 {p99}, max {max}");
        // the transparent left edge shows the background, the opaque right edge the layer
        let at = |img: &[u8], x: usize| (img[(90 * w + x) * 4], img[(90 * w + x) * 4 + 1], img[(90 * w + x) * 4 + 2]);
        let bg = execute_cpu(&FramePlan::Layers {
            width: w,
            height: h,
            layers: vec![PlanLayer { frame: background.clone(), matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None }],
        })
        .unwrap()
        .over_black_rgba8();
        let (l, r, b0) = (at(&gpu, 1), at(&gpu, w - 2), at(&bg, 1));
        assert!((l.0 as i32 - b0.0 as i32).abs() <= 6 && (l.1 as i32 - b0.1 as i32).abs() <= 6, "{bits}-bit: left edge {l:?} vs background {b0:?}");
        assert!(r != at(&bg, w - 2), "{bits}-bit: right edge {r:?} should differ from the background");
    }
}

#[test]
fn fast_half_float_conversion_matches_the_reference_for_every_value() {
    for h in 0..=u16::MAX {
        let (fast, slow) = (f16_to_f32_fast(h), f16_to_f32(h));
        assert!(fast.to_bits() == slow.to_bits() || (fast.is_nan() && slow.is_nan()), "{h:#06x}: {fast} vs {slow}");
    }
}

/// HDR and wide-gamut Y'CbCr on a Rec. 709 sequence (#965): the shader's colour management
/// (curve, HLG OOTF, BT.2020 → 709 matrix, BT.2390 tone mapping, desaturation) matches the CPU
/// running the same `InputTransform`.
#[test]
fn hdr_input_gpu_matches_cpu() {
    use filmcraft_color::{ColorInfo, ColorPipeline, ColorSpace, InputTransform, Matrix, Primaries, Transfer};
    let Some((dev, q)) = device() else {
        eprintln!("no GPU adapter; skipping");
        return;
    };
    let mut c = GpuCompositor::new(&dev, &q);
    // a 10-bit luma ramp (to the top code) under neutral, saturated and extreme chroma, 4:4:4
    let (w, h) = (256u32, 6u32);
    let chroma = [(512, 512), (300, 760), (760, 300), (64, 512), (960, 960), (512, 64)];
    let y: Vec<u16> = (0..h).flat_map(|_| (0..w).map(|x| (64 + x * 876 / 255) as u16)).collect();
    let cb: Vec<u16> = (0..h).flat_map(|r| std::iter::repeat_n(chroma[r as usize].0, w as usize)).collect();
    let cr: Vec<u16> = (0..h).flat_map(|r| std::iter::repeat_n(chroma[r as usize].1, w as usize)).collect();
    let data = PixelData::Yuv16 { planes: [Arc::new(y), Arc::new(cb), Arc::new(cr)], chroma: Chroma::C444, bits: 10, alpha: None };
    let hdr = |transfer| ColorInfo { matrix: Matrix::Bt2020Ncl, transfer, primaries: Primaries::Bt2020, range: Range::Limited };
    let cases = [
        ("HLG", hdr(Transfer::Hlg), None),
        ("PQ", hdr(Transfer::Pq), None),
        ("PQ 4000 cd/m²", hdr(Transfer::Pq), Some(4000.0)),
        ("BT.2020 SDR", ColorInfo { primaries: Primaries::Bt2020, ..ColorInfo::REC709 }, None),
    ];
    for (name, color, peak) in cases {
        let frame = Arc::new(VideoFrame { width: w, height: h, data: data.clone(), color, par: (1, 1), pts: Default::default() });
        let cs = ColorSpace::from_info(&color);
        let t = Arc::new(InputTransform::new(cs, color.range, &ColorPipeline::REC709, peak));
        assert!(t.gpu_params().is_some(), "{name}");
        let layer = PlanLayer { input: Some(t), ..PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal) };
        let plan = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![layer] };
        let cpu = execute_cpu(&plan).unwrap().over_black_rgba8();
        c.composite(&plan).unwrap();
        let (_, _, gpu) = c.read_output().expect("readback");
        let error = cpu.iter().zip(&gpu).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        // and the stage does something: the default decode of the same frame is far off
        let plain = FramePlan::Layers { width: w as usize, height: h as usize, layers: vec![PlanLayer::new(frame, Affine::IDENTITY, 1.0, Blend::Normal)] };
        c.composite(&plain).unwrap();
        let (_, _, unmanaged) = c.read_output().expect("readback");
        let apart = cpu.iter().zip(&unmanaged).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        eprintln!("{name}: max error {error}, unmanaged differs by up to {apart}");
        assert!(error <= 2, "{name}: GPU differs from the CPU by up to {error} codes");
        assert!(apart > 20, "{name}: colour management changed nothing ({apart})");
    }
}
