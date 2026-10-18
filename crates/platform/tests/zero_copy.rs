//! Zero-copy decoding (Windows): decoded pictures stay in GPU memory as shareable Direct3D 11
//! textures, the compositor opens them on its DX12 device, and the result is the picture the
//! upload path gives. Its own test binary: the zero-copy switch is process-wide.
//! Skips without ffmpeg, a DX12 adapter that is the decoder's adapter, or a hardware decoder.
#![cfg(target_os = "windows")]

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_geom::Affine;
use filmcraft_gpu::GpuCompositor;
use filmcraft_render::Blend;
use filmcraft_render::plan::{FramePlan, PlanLayer};

// Tests toggle one process-wide decoder policy; serialize even under Cargo's default runner.
static ZERO_COPY_GATE: Mutex<()> = Mutex::new(());

/// A DX12 device compiling shaders with DXC (`FILMCRAFT_DXC_DIR`: a folder with `dxcompiler.dll` and
/// `dxil.dll`, e.g. the Windows SDK's `bin\<version>d`): FXC cannot compile the compositor's
/// effects shader. None skips the test.
fn dx12() -> Option<(wgpu::Device, wgpu::Queue)> {
    let dxc = std::path::Path::new(&std::env::var_os("FILMCRAFT_DXC_DIR")?).join("dxcompiler.dll");
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12,
        backend_options: wgpu::BackendOptions {
            dx12: wgpu::Dx12BackendOptions {
                shader_compiler: wgpu::Dx12Compiler::DynamicDxc { dxc_path: dxc.to_string_lossy().into_owned() },
                ..Default::default()
            },
            ..Default::default()
        },
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
    )
    .ok()?;
    eprintln!("DX12 adapter: {:?}", adapter.get_info());
    let features = adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_features: features, ..Default::default() })).ok()
}

fn plan(frame: &Arc<VideoFrame>) -> FramePlan {
    FramePlan::Layers {
        width: frame.width as usize,
        height: frame.height as usize,
        layers: vec![PlanLayer::new(frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal)],
    }
}

fn render(c: &mut GpuCompositor, frame: &Arc<VideoFrame>) -> Vec<u8> {
    c.composite(&plan(frame)).unwrap();
    c.read_output().expect("readback").2
}

#[test]
fn gpu_pictures_composite_like_uploaded_ones_and_read_back_exactly() {
    let _guard = ZERO_COPY_GATE.lock().unwrap();
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some((device, queue)) = dx12() else {
        eprintln!("SKIPPED: no DX12 device with DXC (set FILMCRAFT_DXC_DIR)");
        return;
    };
    filmcraft_platform::register();
    if !filmcraft_platform::media_foundation::enable_zero_copy(&device) {
        eprintln!("SKIPPED: zero-copy is not possible here");
        return;
    }
    let mut compositor = GpuCompositor::new(&device, &queue);
    let mut checked = 0;
    for (name, _) in FIXTURES {
        let Some(path) = named(&ff, name) else { continue };
        let s = read_stream(&path);
        let Some(mut hw) = filmcraft_platform::media_foundation_factory(&s.entry).and_then(|r| r.ok()) else {
            eprintln!("SKIPPED: no hardware decoder for {name}");
            continue;
        };
        let before = filmcraft_codecs::hw::hw_stats();
        let zero = decode_all(hw.as_mut(), &s.samples[..24]);
        let after = filmcraft_codecs::hw::hw_stats();
        eprintln!("GPU import fixture: {name}, frames={}, zero-copy delta={}", zero.len(), after.zero_copy_frames - before.zero_copy_frames);
        assert!(!zero.is_empty(), "{name}: decoder returned no frames");
        assert!(zero.iter().all(|f| matches!(f.frame.data, PixelData::Gpu(_))), "{name}: pictures are GPU surfaces");
        assert_eq!(after.zero_copy_frames - before.zero_copy_frames, zero.len() as u64, "{name}: counted");
        assert_eq!(zero.iter().map(|f| f.frame.byte_size()).min().map(|b| b > 0), Some(true));

        // the same pictures through the software decoder
        let soft = decode_all(filmcraft_codecs::software_video_decoder(&s.entry).unwrap().as_mut(), &s.samples[..24]);
        assert_eq!(zero.len(), soft.len(), "{name}: picture count");
        for (z, c) in zero.iter().zip(&soft) {
            assert_eq!(z.pts, c.pts, "{name}: order");
            // CPU consumers: the downloaded planes are the software decoder's exactly
            let down = z.frame.cpu().unwrap();
            assert_same(
                &format!("{name} pts {}", z.pts),
                &[filmcraft_codecs::DecodedFrame { pts: z.pts, frame: down.into_owned(), draft: false }],
                std::slice::from_ref(c),
            );
        }
        // the compositor: sampled from the decoder's memory vs uploaded from CPU planes
        let uploads = compositor.uploaded_bytes;
        for (z, c) in zero.iter().zip(&soft).take(6) {
            let (zf, cf) = (Arc::new(z.frame.clone()), Arc::new(c.frame.clone()));
            let before_gpu = compositor.uploaded_bytes;
            let a = render(&mut compositor, &zf);
            let uploaded = compositor.uploaded_bytes;
            assert_eq!(uploaded, before_gpu, "{name}: GPU import must not fall back to a CPU upload");
            let b = render(&mut compositor, &cf);
            assert!(compositor.uploaded_bytes > uploaded, "{name}: the CPU picture was uploaded");
            let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
            eprintln!("GPU parity: {name} pts={}, max error={worst}, GPU upload delta={}", z.pts, uploaded - before_gpu);
            assert!(worst <= 1, "{name} pts {}: zero-copy composite differs from the uploaded one by {worst}", z.pts);
            // second composite of the GPU picture: nothing uploaded, nothing imported again
            let up = compositor.uploaded_bytes;
            let _ = render(&mut compositor, &zf);
            assert_eq!(compositor.uploaded_bytes, up);
        }
        assert!(compositor.uploaded_bytes > uploads, "{name}: CPU upload control did not respond");
        checked += 1;
    }
    assert!(checked > 0, "no hardware fixture exercised GPU import");
    drop(compositor);
    // every surface was released with its picture: nothing leaks (handles, GPU memory)
    assert_eq!(filmcraft_platform::media_foundation::live_surfaces(), 0, "shareable surfaces left alive");
    // off again: the decoders hand out CPU planes
    filmcraft_platform::media_foundation::disable_zero_copy();
    let s = read_stream(&named(&ff, "h264_high.mp4").unwrap());
    let mut hw = filmcraft_platform::media_foundation_factory(&s.entry).unwrap().unwrap();
    assert!(decode_all(hw.as_mut(), &s.samples[..12]).iter().all(|f| matches!(f.frame.data, PixelData::Yuv8 { .. })));
}

/// Vulkan keeps new pictures on CPU; existing resident pictures must materialize and upload.
#[test]
fn vulkan_keeps_cpu_decode_and_materializes_resident_frames() {
    let _guard = ZERO_COPY_GATE.lock().unwrap();
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let Some((device, queue)) =
        pollster::block_on(instance.request_adapter(&Default::default())).ok().and_then(|a| pollster::block_on(a.request_device(&Default::default())).ok())
    else {
        eprintln!("SKIPPED: no Vulkan device");
        return;
    };
    assert!(!filmcraft_platform::media_foundation::enable_zero_copy(&device));
    assert!(!filmcraft_platform::media_foundation::zero_copy_enabled());
    let ff = filmcraft_testkit::require_ffmpeg!();
    filmcraft_platform::register();
    let s = read_stream(&named(&ff, "h264_high.mp4").unwrap());
    let mut cpu_decoder = filmcraft_platform::media_foundation_factory(&s.entry).unwrap().unwrap();
    let before = filmcraft_codecs::hw::hw_stats();
    // Fixture keyint=24; half a GOP exercises reordered pictures.
    const HALF_GOP_SAMPLES: usize = 24 / 2;
    let sample_count = s.samples.len().min(HALF_GOP_SAMPLES);
    let cpu = decode_all(cpu_decoder.as_mut(), &s.samples[..sample_count]);
    let after = filmcraft_codecs::hw::hw_stats();
    assert!(cpu.len() >= 2, "CPU decode must produce distinct-time pictures");
    assert!(cpu.iter().all(|f| matches!(f.frame.data, PixelData::Yuv8 { .. })));
    assert_eq!(after.zero_copy_frames, before.zero_copy_frames);

    let Some((dx_device, _dx_queue)) = dx12() else {
        eprintln!("SKIPPED: no DX12 device with DXC (set FILMCRAFT_DXC_DIR)");
        return;
    };
    assert!(filmcraft_platform::media_foundation::enable_zero_copy(&dx_device));
    let mut resident_decoder = filmcraft_platform::media_foundation_factory(&s.entry).unwrap().unwrap();
    let resident = decode_all(resident_decoder.as_mut(), &s.samples[..sample_count]);
    assert_eq!(resident.len(), cpu.len());
    assert!(resident.iter().all(|f| matches!(f.frame.data, PixelData::Gpu(_))));

    let mut compositor = GpuCompositor::new(&device, &queue);
    let reference = render(&mut compositor, &Arc::new(cpu[0].frame.clone()));
    let changed = render(&mut compositor, &Arc::new(cpu[1].frame.clone()));
    assert_ne!(reference, changed, "changed-time control must change the rendered picture");
    assert_eq!(resident[0].pts, cpu[0].pts);
    let frame = Arc::new(resident[0].frame.clone());
    let uploads = compositor.uploaded_bytes;
    assert_eq!(render(&mut compositor, &frame), reference);
    let fallback_uploads = compositor.uploaded_bytes;
    assert!(fallback_uploads > uploads, "Vulkan must materialize and upload the resident picture");
    assert_eq!(render(&mut compositor, &frame), reference);
    assert_eq!(compositor.uploaded_bytes, fallback_uploads, "cached CPU planes must reuse their upload");
    eprintln!(
        "Vulkan fallback: CPU frames={}, resident frames={}, upload bytes={}, repeat upload bytes=0, exact pixels, changed-time control passed",
        cpu.len(),
        resident.len(),
        fallback_uploads - uploads
    );
    drop(frame);
    drop(resident);
    drop(compositor);
    assert_eq!(filmcraft_platform::media_foundation::live_surfaces(), 0);
    filmcraft_platform::media_foundation::disable_zero_copy();
}

/// Decoder replacement must not accumulate imported surfaces in a persistent compositor.
#[test]
fn imported_surfaces_are_evicted_and_released_with_the_compositor() {
    let _guard = ZERO_COPY_GATE.lock().unwrap();
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some((device, queue)) = dx12() else {
        eprintln!("SKIPPED: no DX12 device with DXC (set FILMCRAFT_DXC_DIR)");
        return;
    };
    filmcraft_platform::register();
    assert!(filmcraft_platform::media_foundation::enable_zero_copy(&device));
    assert_eq!(filmcraft_platform::media_foundation::live_surfaces(), 0);
    let s = read_stream(&named(&ff, "h264_high.mp4").unwrap());
    let mut compositor = GpuCompositor::new(&device, &queue);
    let mut retained = None;
    // Cold fill + replacement: derive the observed bound without duplicating cache policy.
    for phase in ["cold", "replacement"] {
        let mut decoder = filmcraft_platform::media_foundation_factory(&s.entry).unwrap().unwrap();
        let before = filmcraft_codecs::hw::hw_stats();
        let pictures = decode_all(decoder.as_mut(), &s.samples);
        let after = filmcraft_codecs::hw::hw_stats();
        assert_eq!(after.zero_copy_frames - before.zero_copy_frames, pictures.len() as u64);
        assert!(!pictures.is_empty());
        let PixelData::Gpu(first) = &pictures[0].frame.data else { panic!("expected resident picture") };
        let first_surface = Arc::downgrade(first.surface());
        let mut first_output = None;
        let mut changed = false;
        for picture in &pictures {
            assert!(matches!(picture.frame.data, PixelData::Gpu(_)));
            let output = render(&mut compositor, &Arc::new(picture.frame.clone()));
            if let Some(first) = &first_output {
                changed |= first != &output;
            } else {
                first_output = Some(output);
            }
        }
        assert!(changed, "changed-time probe must respond");
        assert_eq!(compositor.uploaded_bytes, 0, "resident imports must not upload CPU planes");
        let produced = pictures.len();
        drop(pictures);
        drop(decoder);
        assert!(first_surface.upgrade().is_none(), "oldest imported surface must be evicted");
        let live = filmcraft_platform::media_foundation::live_surfaces();
        assert!(live > 0 && live < produced, "cache must retain some, but not all, decoded surfaces");
        if let Some(previous) = retained {
            assert_eq!(live, previous, "replacing the decoder must not accumulate cached surfaces");
        }
        retained = Some(live);
        eprintln!("Surface ownership: phase={phase}, produced={produced}, retained={live}, oldest evicted, CPU upload bytes=0");
    }
    drop(compositor);
    assert_eq!(filmcraft_platform::media_foundation::live_surfaces(), 0);
    filmcraft_platform::media_foundation::disable_zero_copy();
}

/// Controlled renderer destruction, not driver reset or application recovery.
#[test]
fn resident_pictures_survive_renderer_device_replacement() {
    let _guard = ZERO_COPY_GATE.lock().unwrap();
    let ff = filmcraft_testkit::require_ffmpeg!();
    let Some((device, queue)) = dx12() else {
        eprintln!("SKIPPED: no DX12 device with DXC (set FILMCRAFT_DXC_DIR)");
        return;
    };
    filmcraft_platform::register();
    assert!(filmcraft_platform::media_foundation::enable_zero_copy(&device));
    let s = read_stream(&named(&ff, "h264_high.mp4").unwrap());
    let mut decoder = filmcraft_platform::media_foundation_factory(&s.entry).unwrap().unwrap();
    // Fixture keyint=24; one GOP includes reordered output without retaining the whole clip.
    const GOP_SAMPLES: usize = 24;
    let pictures = decode_all(decoder.as_mut(), &s.samples[..s.samples.len().min(GOP_SAMPLES)]);
    assert!(pictures.len() >= 2);
    assert!(pictures.iter().all(|f| matches!(f.frame.data, PixelData::Gpu(_))));
    let first = Arc::new(pictures[0].frame.clone());
    let next = Arc::new(pictures[1].frame.clone());
    let mut compositor = GpuCompositor::new(&device, &queue);
    let reference = render(&mut compositor, &first);
    assert_ne!(reference, render(&mut compositor, &next), "changed-time control must respond");
    assert_eq!(compositor.uploaded_bytes, 0);
    let live = filmcraft_platform::media_foundation::live_surfaces();
    assert!(live > 0);
    drop(compositor);

    let (tx, rx) = std::sync::mpsc::channel();
    device.set_device_lost_callback(move |reason, _| {
        let _ = tx.send(reason);
    });
    device.destroy();
    // Readback above drained submissions; one poll dispatches the destruction callback.
    let _ = device.poll(wgpu::PollType::Poll);
    assert_eq!(rx.try_recv().unwrap(), wgpu::DeviceLostReason::Destroyed);
    assert_eq!(filmcraft_platform::media_foundation::live_surfaces(), live);
    drop(queue);
    drop(device);

    let (replacement, replacement_queue) = dx12().expect("replacement DX12 device");
    assert!(filmcraft_platform::media_foundation::enable_zero_copy(&replacement));
    let mut compositor = GpuCompositor::new(&replacement, &replacement_queue);
    assert_eq!(render(&mut compositor, &first), reference);
    assert_ne!(render(&mut compositor, &next), reference);
    assert_eq!(compositor.uploaded_bytes, 0, "replacement must reimport resident pictures");
    let cpu = Arc::new(first.cpu().unwrap().into_owned());
    assert_eq!(render(&mut compositor, &cpu), reference);
    assert!(compositor.uploaded_bytes > 0, "CPU download/upload control must respond");
    eprintln!("Renderer replacement: destroyed callback, retained surfaces={live}, exact reimport pixels, GPU upload bytes=0, CPU control passed");
    drop(cpu);
    drop(first);
    drop(next);
    drop(pictures);
    drop(compositor);
    assert_eq!(filmcraft_platform::media_foundation::live_surfaces(), 0);
    filmcraft_platform::media_foundation::disable_zero_copy();
}
