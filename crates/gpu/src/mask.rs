//! GPU mask coverage and masked-effect mixing (WGSL compute), the counterpart of
//! [`filmcraft_render::mask`]. Tested for parity with the CPU path.

use filmcraft_render::mask::FlatMask;

pub struct GpuMask {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
}

fn storage(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

impl GpuMask {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-mask"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mask.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, true),
                storage(5, false),
                storage(6, false),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("mask"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mask"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        GpuMask { device: device.clone(), queue: queue.clone(), pipeline, bgl }
    }

    /// Combined coverage of `masks` over `w`×`h` pixels (`None` without active masks).
    pub fn coverage(&self, masks: &[FlatMask], w: usize, h: usize) -> Option<Vec<f32>> {
        self.run(masks, w, h, None).map(|(cov, _)| cov)
    }

    /// `lerp(original, effected, coverage)` on premultiplied RGBA f32 pixels (a masked effect).
    pub fn mix(&self, masks: &[FlatMask], w: usize, h: usize, original: &[f32], effected: &[f32]) -> Option<Vec<f32>> {
        self.run(masks, w, h, Some((original, effected))).map(|(_, px)| px)
    }

    /// Run the mask compute shader for `masks` over `w`×`h`, optionally mixing `original` and `effected`.
    fn run(&self, masks: &[FlatMask], w: usize, h: usize, mix: Option<(&[f32], &[f32])>) -> Option<(Vec<f32>, Vec<f32>)> {
        use wgpu::util::DeviceExt;
        let active: Vec<&FlatMask> = masks.iter().filter(|m| m.mode.index() != 0).collect();
        if active.is_empty() || w == 0 || h == 0 {
            return None;
        }
        let n = w * h;
        if let Some((o, e)) = mix
            && (o.len() != n * 4 || e.len() != n * 4)
        {
            return None;
        }
        let mut params = Vec::with_capacity(16);
        for v in [w as u32, h as u32, active.len() as u32, u32::from(mix.is_some())] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        let mut infos = Vec::new();
        let mut pts: Vec<f32> = Vec::new();
        for m in &active {
            let start = (pts.len() / 2) as u32;
            pts.extend(m.pts.iter().flatten());
            for v in [start, m.pts.len() as u32, m.mode.index(), u32::from(m.inverted)] {
                infos.extend_from_slice(&v.to_le_bytes());
            }
            for v in [m.feather, m.expansion, m.opacity, 0.0] {
                infos.extend_from_slice(&v.to_le_bytes());
            }
        }
        if pts.is_empty() {
            pts.extend([0.0, 0.0]);
        }
        let d = &self.device;
        let init = |label: &str, contents: &[u8], usage| d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents, usage });
        let ub = init("mask-params", &params, wgpu::BufferUsages::UNIFORM);
        let mb = init("mask-infos", &infos, wgpu::BufferUsages::STORAGE);
        let pb = init("mask-pts", &f32_bytes(&pts), wgpu::BufferUsages::STORAGE);
        let dummy = [0.0f32; 4];
        let (o, e) = mix.unwrap_or((&dummy, &dummy));
        let ob = init("mask-original", &f32_bytes(o), wgpu::BufferUsages::STORAGE);
        let eb = init("mask-effected", &f32_bytes(e), wgpu::BufferUsages::STORAGE);
        let px_size = if mix.is_some() { (n * 16) as u64 } else { 16 };
        let cov_size = (n * 4) as u64;
        let out = |label: &str, size: u64| {
            d.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let px_b = out("mask-out-px", px_size);
        let cov_b = out("mask-out-cov", cov_size);
        let bg = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: mb.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: pb.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: ob.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: eb.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: px_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 6, resource: cov_b.as_entire_binding() },
            ],
        });
        let read = |size: u64| {
            d.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mask-read"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        };
        let rc = read(cov_size);
        let rp = read(px_size);
        let mut enc = d.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("mask") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("mask"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((w as u32).div_ceil(8), (h as u32).div_ceil(8), 1);
        }
        enc.copy_buffer_to_buffer(&cov_b, 0, &rc, 0, cov_size);
        enc.copy_buffer_to_buffer(&px_b, 0, &rp, 0, px_size);
        let sub = self.queue.submit([enc.finish()]);
        let fetch = |b: &wgpu::Buffer| -> Option<Vec<f32>> {
            let slice = b.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            crate::wait_mapped(d, sub.clone(), &rx)?;
            let data = slice.get_mapped_range().ok()?;
            let v = data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
            drop(data);
            b.unmap();
            Some(v)
        };
        let cov = fetch(&rc)?;
        let px = if mix.is_some() { fetch(&rp)? } else { Vec::new() };
        Some((cov, px))
    }
}
