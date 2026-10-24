//! GPU-only procedural FilmCraft demo footage source.
//!
//! Frame storage and uniforms are reserved through the caller's one-use allocation budget before
//! any per-frame GPU allocation is created. No CPU pixels, upload, or readback are involved.

use filmcraft_media::DemoScene;
use wgpu::util::DeviceExt;

const WORKGROUP: u32 = 8;
const PARAM_BYTES: u64 = 16;
const MAX_FRAME_BYTES: u64 = 512 * 1024 * 1024;

pub struct GeneratedDemoFrame<H> {
    pub texture: wgpu::Texture,
    pub size: (u32, u32),
    pub allocation: H,
}

pub struct DemoSource {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl DemoSource {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("filmcraft-demo-source"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-demo-source"),
            source: wgpu::ShaderSource::Wgsl(include_str!("demo_source.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("filmcraft-demo-source"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("filmcraft-demo-source"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline, layout }
    }

    pub fn allocation_bytes(width: u32, height: u32) -> Result<u64, String> {
        let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or("demo frame size overflow")?;
        let bytes = pixels.checked_mul(4).and_then(|bytes| bytes.checked_add(PARAM_BYTES)).ok_or("demo frame allocation overflow")?;
        if width == 0 || height == 0 || bytes > MAX_FRAME_BYTES {
            return Err("demo frame size is outside the supported allocation limit".into());
        }
        Ok(bytes)
    }

    pub fn record<H>(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        scene: DemoScene,
        width: u32,
        height: u32,
        time_seconds: f32,
        allocation: H,
    ) -> Result<GeneratedDemoFrame<H>, String> {
        let size = (width, height);
        if !time_seconds.is_finite() {
            return Err("demo source time must be finite".into());
        }
        if !(0.0..=18.0).contains(&time_seconds) {
            return Err("demo source time is outside its supported clip duration".into());
        }
        Self::allocation_bytes(width, height)?;
        let limits = device.limits();
        let groups = (size.0.div_ceil(WORKGROUP), size.1.div_ceil(WORKGROUP));
        if size.0 > limits.max_texture_dimension_2d
            || size.1 > limits.max_texture_dimension_2d
            || groups.0 > limits.max_compute_workgroups_per_dimension
            || groups.1 > limits.max_compute_workgroups_per_dimension
        {
            return Err("demo source exceeds device texture or compute limits".into());
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("filmcraft-demo-rgba8"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | if cfg!(test) { wgpu::TextureUsages::COPY_SRC } else { wgpu::TextureUsages::empty() },
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        let params = [scene_index(scene), size.0, size.1, time_seconds.to_bits()];
        let bytes: [u8; 16] = std::array::from_fn(|i| (params[i / 4] >> ((i % 4) * 8)) as u8);
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("filmcraft-demo-source-params"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("filmcraft-demo-source"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("filmcraft-demo-source"), timestamp_writes: None });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(groups.0, groups.1, 1);
        drop(pass);
        Ok(GeneratedDemoFrame { texture, size, allocation })
    }
}

fn scene_index(scene: DemoScene) -> u32 {
    match scene {
        DemoScene::Aurora => 0,
        DemoScene::OceanSunset => 1,
        DemoScene::CityNight => 2,
        DemoScene::Dunes => 3,
        DemoScene::Plasma => 4,
        DemoScene::Forest => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::DemoSource;

    #[test]
    fn demo_gpu_matches_independent_cpu_generator() {
        use filmcraft_media::{DemoScene, Generator};
        let renderer = match crate::ExportRenderer::new_required() {
            Ok(renderer) => renderer,
            Err(error) if matches!(error.reason, filmcraft_render::plan::EligibilityFailure::UnsupportedAdapter { .. }) => {
                eprintln!("SKIPPED demo GPU parity: {error}");
                return;
            }
            Err(error) => panic!("demo GPU device failed: {error}"),
        };
        let (_, device, queue) = renderer.wgpu_handles();
        let source = DemoSource::new(device);
        for scene in DemoScene::ALL {
            for time in [0.0, 1.5, 7.25] {
                for (w, h) in [(80_u32, 45_u32), (97, 53), (128, 72), (320, 180), (1920, 1080)] {
                    let row = (w * 4).div_ceil(256) * 256;
                    let mut encoder = device.create_command_encoder(&Default::default());
                    let frame = source.record(device, &mut encoder, scene, w, h, time, ()).unwrap();
                    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("demo-parity-readback-test-only"),
                        size: u64::from(row * h),
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    });
                    encoder.copy_texture_to_buffer(
                        wgpu::TexelCopyTextureInfo { texture: &frame.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                        wgpu::TexelCopyBufferInfo {
                            buffer: &buffer,
                            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
                        },
                        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    );
                    queue.submit([encoder.finish()]);
                    let slice = buffer.slice(..);
                    let (tx, rx) = std::sync::mpsc::channel();
                    slice.map_async(wgpu::MapMode::Read, move |result| {
                        let _ = tx.send(result);
                    });
                    assert!(crate::wait_mapped(device).is_some());
                    rx.recv_timeout(crate::MAP_TIMEOUT).unwrap().unwrap();
                    let data = slice.get_mapped_range().unwrap();
                    let gpu: Vec<u8> = data.chunks_exact(row as usize).flat_map(|line| line[..(w * 4) as usize].iter().copied()).collect();
                    drop(data);
                    buffer.unmap();
                    let cpu = filmcraft_media::generators::render(&Generator::Demo(scene), w, h, time, 0, filmcraft_time::FrameRate::FPS_24);
                    let (index, worst) = gpu.iter().zip(&cpu).enumerate().map(|(i, (a, b))| (i, a.abs_diff(*b))).max_by_key(|(_, d)| *d).unwrap();
                    let pixel = index / 4 * 4;
                    assert!(
                        worst <= 2,
                        "{scene:?} {w}x{h} t={time}: max channel difference {worst}/255 at ({}, {}) channel {}, GPU={:?} CPU={:?}",
                        index / 4 % w as usize,
                        index / 4 / w as usize,
                        index % 4,
                        &gpu[pixel..pixel + 4],
                        &cpu[pixel..pixel + 4]
                    );
                }
            }
        }
    }

    #[test]
    fn demo_frame_layout_checks_zero_overflow_and_hard_cap() {
        assert!(DemoSource::allocation_bytes(0, 1).is_err());
        assert!(DemoSource::allocation_bytes(u32::MAX, u32::MAX).is_err());
        assert!(DemoSource::allocation_bytes(16_384, 16_384).is_err());
        assert_eq!(DemoSource::allocation_bytes(2, 2), Ok(32));
    }
}
