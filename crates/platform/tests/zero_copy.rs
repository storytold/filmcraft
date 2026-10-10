//! Zero-copy decoding (Windows): decoded pictures stay in GPU memory as shareable Direct3D 11
//! textures, the compositor opens them on its DX12 device, and the result is the picture the
//! upload path gives. Its own test binary: the zero-copy switch is process-wide.
//! Skips without ffmpeg, a DX12 adapter that is the decoder's adapter, or a hardware decoder.
#![cfg(target_os = "windows")]

mod common;

use std::sync::Arc;

use common::*;
use filmcraft_frame::{PixelData, VideoFrame};
use filmcraft_geom::Affine;
use filmcraft_gpu::GpuCompositor;
use filmcraft_render::Blend;
use filmcraft_render::plan::{FramePlan, PlanLayer};

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
        layers: vec![PlanLayer { frame: frame.clone(), matrix: Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None }],
    }
}

fn render(c: &mut GpuCompositor, frame: &Arc<VideoFrame>) -> Vec<u8> {
    c.composite(&plan(frame)).unwrap();
    c.read_output().expect("readback").2
}

#[test]
fn gpu_pictures_composite_like_uploaded_ones_and_read_back_exactly() {
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

/// A renderer that is not DX12 (wgpu's default on Windows is Vulkan) cannot open the decoder's
/// surfaces: zero-copy stays off and every picture is a CPU one.
#[test]
fn a_vulkan_renderer_keeps_cpu_pictures() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let Some((device, _q)) =
        pollster::block_on(instance.request_adapter(&Default::default())).ok().and_then(|a| pollster::block_on(a.request_device(&Default::default())).ok())
    else {
        eprintln!("SKIPPED: no Vulkan device");
        return;
    };
    assert!(!filmcraft_platform::media_foundation::enable_zero_copy(&device));
    assert!(!filmcraft_platform::media_foundation::zero_copy_enabled());
}
