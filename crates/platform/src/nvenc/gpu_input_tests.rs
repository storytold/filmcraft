//! NVENC reading its input pictures from GPU memory ([`super::target`]) writes the stream it writes
//! from the same pixels in CPU memory, picture for picture (ignored: needs an NVIDIA GPU whose
//! Vulkan driver imports Direct3D 11 textures; run by hand with
//! `cargo test -p filmcraft-platform --lib nvenc::gpu_input_tests -- --ignored --nocapture`).
//! The frames are written into the shared textures through a wgpu Vulkan device and the
//! registered importer, as the export's compositor does.

use std::sync::Arc;

use super::{Config, Nvenc, Packet, Profile, Signal, target};
use filmcraft_gpu::TargetImporter;

const NVIDIA: u32 = 0x10DE;

/// A picture that changes from frame to frame (motion for the B-frames to predict).
fn pattern(w: u32, h: u32, i: u32) -> Vec<u8> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = (x + i * 5, y + i * 3);
            px.extend_from_slice(&[(u ^ v) as u8, (u.wrapping_mul(3) + v) as u8, (v.wrapping_mul(2) ^ (i * 17)) as u8, 255]);
        }
    }
    px
}

fn config(profile: Profile, bframes: bool) -> Config {
    Config {
        width: 640,
        height: 360,
        fps: (30, 1),
        bitrate_kbps: 4000,
        max_bitrate_kbps: 6000,
        cbr: false,
        keyint: 30,
        profile,
        level: None,
        sar: None,
        bframes,
    }
}

fn summary(ps: &[Packet]) -> Vec<(Vec<u8>, bool, i64, i64)> {
    ps.iter().map(|p| (p.data.clone(), p.key, p.pts, p.dts)).collect()
}

fn encode_cpu(cfg: &Config, frames: &[Vec<u8>]) -> Vec<Packet> {
    let mut enc = Nvenc::with_rgba_input(cfg, &Signal::default()).expect("an RGBA-input NVENC session");
    let mut out = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        out.extend(enc.encode_rgba(f, i as u64).expect("encode"));
    }
    out.extend(enc.flush().expect("flush"));
    out
}

fn vulkan_device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let adapter = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
        .into_iter()
        .find(|a| a.get_info().vendor == NVIDIA && a.features().contains(wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32))
        .expect("an NVIDIA Vulkan adapter that imports Direct3D 11 textures");
    pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor { required_features: wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32, ..Default::default() }),
    )
    .expect("device")
}

fn encode_gpu(cfg: &Config, frames: &[Vec<u8>], device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<Packet> {
    let (w, h) = (cfg.width, cfg.height);
    let mut enc = Nvenc::with_rgba_input(cfg, &Signal::default()).expect("an RGBA-input NVENC session");
    // the encoder holds its ring (8) and the B-frame delay: 12 pictures never run out here
    let pool = enc.make_targets(12).expect("input pictures in GPU memory");
    let importer = target::Importer;
    let mut out = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let t = target::take(&pool).expect("a free input picture");
        let tex = importer.import(device, &t).expect("the input picture opened on the Vulkan device");
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            f,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let index = queue.submit([]);
        device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(std::time::Duration::from_secs(8)) }).expect("the write finished");
        out.extend(enc.encode_target(Arc::new(t), i as u64).expect("encode from GPU memory"));
    }
    out.extend(enc.flush().expect("flush"));
    // every picture went back to the pool
    let back: Vec<_> = (0..pool.len()).map_while(|_| target::take(&pool)).collect();
    assert_eq!(back.len(), pool.len(), "pictures still held after the flush");
    out
}

#[test]
#[ignore]
fn gpu_pictures_encode_like_cpu_pictures() {
    let (device, queue) = vulkan_device();
    for (profile, bframes) in [(Profile::High, true), (Profile::High, false), (Profile::HevcMain, true)] {
        let cfg = config(profile, bframes);
        let frames: Vec<Vec<u8>> = (0..75).map(|i| pattern(cfg.width, cfg.height, i)).collect();
        let cpu = summary(&encode_cpu(&cfg, &frames));
        // NVENC writes the same stream for the same pixels: the reference for the GPU pictures
        assert_eq!(cpu, summary(&encode_cpu(&cfg, &frames)), "NVENC is not deterministic here ({profile:?}, B-frames {bframes})");
        let gpu = summary(&encode_gpu(&cfg, &frames, &device, &queue));
        assert_eq!(gpu.len(), cpu.len(), "{profile:?}, B-frames {bframes}");
        for (k, (g, c)) in gpu.iter().zip(&cpu).enumerate() {
            assert!(g == c, "picture {k} differs ({profile:?}, B-frames {bframes}): {} bytes vs {}", g.0.len(), c.0.len());
        }
        println!("{profile:?}, B-frames {bframes}: {} pictures identical", gpu.len());
    }
}
