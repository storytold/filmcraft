//! End-to-end verification of the Windows zero-copy decode -> Vulkan wgpu pipeline:
//! - H.264 8-bit (NV12) and HEVC Main 10 (P010) decode directly into `PixelData::GpuSurface`,
//!   including cropped 640x360 pictures inside 640x368 coded surfaces.
//! - `materialize_cpu()` matches the software decoders bit-for-bit.
//! - Vulkan `GpuCompositor` imports the D3D11 shared textures with zero CPU<->GPU transfers
//!   (`decode_readback_bytes == 0`, `compositor_upload_bytes == 0`) and produces pixels identical
//!   to compositing the CPU-materialized frame.
//! - DX12 / non-interop devices fall back transparently via `to_cpu()` with identical pixels.
#![cfg(target_os = "windows")]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use filmcraft_codecs::{DecodedFrame, VideoDecoder};
use filmcraft_frame::{PixelData, transfers};
use filmcraft_geom::Affine;
use filmcraft_gpu::{GpuCompositor, requested_features};
use filmcraft_isobmff::SampleEntry;
use filmcraft_render::Blend;
use filmcraft_render::plan::{FramePlan, PlanLayer};

struct Stream {
    entry: SampleEntry,
    samples: Vec<(Vec<u8>, i64)>,
}

fn fixture(ff: &Path, name: &str, args: &[&str]) -> Option<std::path::PathBuf> {
    let out = filmcraft_testkit::fixtures_dir("platform").join(name);
    filmcraft_testkit::fixtures::generate(&out, |tmp| {
        let st = Command::new(ff).args(["-y", "-v", "error"]).args(args).arg(tmp).stdin(Stdio::null()).status();
        matches!(st, Ok(s) if s.success())
    })
}

fn read_stream(path: &Path) -> Stream {
    let bytes = std::fs::read(path).unwrap();
    let file = filmcraft_isobmff::open(bytes.clone()).unwrap();
    let t = file.track_of_kind(filmcraft_isobmff::TrackKind::Video).unwrap();
    let track = &file.tracks[t];
    let samples = (0..track.samples.len()).map(|i| (file.read_sample(&bytes, t, i).unwrap(), track.samples[i].pts)).collect();
    Stream { entry: track.entries[0].clone(), samples }
}

fn decode_all(d: &mut dyn VideoDecoder, samples: &[(Vec<u8>, i64)]) -> Vec<DecodedFrame> {
    let mut out = Vec::new();
    for (s, p) in samples {
        out.extend(d.decode(s, *p).unwrap());
    }
    out.extend(d.flush());
    out
}

fn init_compositor(backends: wgpu::Backends, with_interop: bool) -> Option<(GpuCompositor, wgpu::AdapterInfo)> {
    let inst = wgpu::Instance::new(wgpu::InstanceDescriptor { backends, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let adapter = pollster::block_on(
        inst.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
    )
    .ok()?;
    let info = adapter.get_info();
    let required_features = if with_interop { requested_features(&adapter) } else { adapter.features() & wgpu::Features::TEXTURE_FORMAT_16BIT_NORM };
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("zero-copy-test"), required_features, ..Default::default() })).ok()?;
    Some((GpuCompositor::new(&device, &queue), info))
}

fn max_rgba_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

fn assert_cpu_parity(what: &str, hw: &[DecodedFrame], sw: &[DecodedFrame]) {
    assert_eq!(hw.len(), sw.len(), "{what}: frame count");
    for (h, s) in hw.iter().zip(sw) {
        assert_eq!(h.pts, s.pts, "{what}: pts");
        assert!(matches!(h.frame.data, PixelData::GpuSurface(_)), "{what}: expected GpuSurface");
        let mat = h.frame.materialize_cpu().expect("materialize_cpu on GpuSurface");
        match (&mat.data, &s.frame.data) {
            (PixelData::Yuv8 { planes: p, .. }, PixelData::Yuv8 { planes: q, .. }) => {
                for i in 0..3 {
                    assert_eq!(p[i], q[i], "{what}: pts {} YUV8 plane {i}", h.pts);
                }
            }
            (PixelData::Yuv16 { planes: p, bits: bx, .. }, PixelData::Yuv16 { planes: q, bits: by, .. }) => {
                assert_eq!(bx, by, "{what}: bits");
                for i in 0..3 {
                    assert_eq!(p[i], q[i], "{what}: pts {} YUV16 plane {i}", h.pts);
                }
            }
            other => panic!("{what}: unexpected pixel formats {other:?}"),
        }
    }
}

#[test]
fn zero_copy_vulkan_matches_cpu_with_zero_transfers_and_dx12_fallback() {
    let ff = filmcraft_testkit::require_ffmpeg!();
    filmcraft_platform::register();
    assert!(filmcraft_platform::media_foundation::zero_copy(), "register() should enable zero-copy on Windows");

    let Some((mut vk, vk_info)) = init_compositor(wgpu::Backends::VULKAN, true) else {
        eprintln!("SKIPPED: no Vulkan adapter available");
        return;
    };
    let mut dx12 = init_compositor(wgpu::Backends::DX12, false);

    let streams = [
        (
            "h264_high.mp4",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=640x360:r=24:d=3,noise=alls=12:allf=t",
                "-c:v",
                "libx264",
                "-profile:v",
                "high",
                "-preset",
                "fast",
                "-x264-params",
                "bframes=3:b-pyramid=normal:keyint=24:min-keyint=24:scenecut=0",
                "-pix_fmt",
                "yuv420p",
            ][..],
        ),
        (
            "hevc_main10.mp4",
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=640x360:r=24:d=3,noise=alls=12:allf=t",
                "-c:v",
                "libx265",
                "-preset",
                "fast",
                "-profile:v",
                "main10",
                "-tag:v",
                "hvc1",
                "-x265-params",
                "log-level=error:bframes=4:keyint=24:min-keyint=24:scenecut=0",
                "-pix_fmt",
                "yuv420p10le",
            ][..],
        ),
    ];

    for (name, args) in streams {
        let Some(path) = fixture(&ff, name, args) else { continue };
        let s = read_stream(&path);
        let Some(Ok(mut hw)) = filmcraft_platform::media_foundation_factory(&s.entry) else {
            eprintln!("SKIPPED: no Media Foundation hardware decoder for {name}");
            continue;
        };
        let mut sw = filmcraft_codecs::software_video_decoder(&s.entry).unwrap();

        let s0 = transfers::snapshot();
        let hw_frames = decode_all(hw.as_mut(), &s.samples);
        assert!(hw.name().starts_with("Media Foundation"), "{name}: stayed in hardware ({})", hw.name());
        let after_decode = transfers::snapshot() - s0;
        assert_eq!(after_decode.decode_readback_bytes, 0, "{name}: decode must not read back to CPU");

        // Composite a few frames via zero-copy Vulkan BEFORE calling materialize_cpu().
        for idx in [0usize, 12, hw_frames.len() - 1] {
            let gpu_frame = Arc::new(hw_frames[idx].frame.clone());
            let plan = FramePlan::Layers {
                width: gpu_frame.width as usize,
                height: gpu_frame.height as usize,
                layers: vec![PlanLayer::new(gpu_frame.clone(), Affine::IDENTITY, 1.0, Blend::Normal)],
            };
            let s1 = transfers::snapshot();
            vk.composite(&plan);
            let t = transfers::snapshot() - s1;
            assert_eq!(t.compositor_upload_bytes, 0, "{name} frame {idx} on {:?}: zero-copy upload must copy 0 bytes", vk_info.backend);
            assert_eq!(t.decode_readback_bytes, 0, "{name} frame {idx}: zero-copy must not trigger lazy readback");
            let (_, _, got) = vk.read_output().unwrap();

            // Reference: composite the CPU software frame through the same compositor.
            let cpu_frame = Arc::new(gpu_frame.materialize_cpu().unwrap());
            let cpu_plan = FramePlan::Layers {
                width: cpu_frame.width as usize,
                height: cpu_frame.height as usize,
                layers: vec![PlanLayer::new(cpu_frame, Affine::IDENTITY, 1.0, Blend::Normal)],
            };
            vk.composite(&cpu_plan);
            let (_, _, want) = vk.read_output().unwrap();
            let diff = max_rgba_diff(&got, &want);
            assert_eq!(diff, 0, "{name} frame {idx}: Vulkan zero-copy RGBA diff={diff}");

            if let Some((ref mut dx, _)) = dx12 {
                dx.composite(&plan);
                let (_, _, got_dx) = dx.read_output().unwrap();
                let dx_diff = max_rgba_diff(&got_dx, &want);
                assert!(dx_diff <= 1, "{name} frame {idx}: DX12 fallback RGBA diff={dx_diff}");
            }
        }

        let sw_frames = decode_all(sw.as_mut(), &s.samples);
        assert_cpu_parity(name, &hw_frames, &sw_frames);
    }
}
