//! GPU 3D LUT application (WGSL compute, tetrahedral interpolation), the counterpart of
//! [`filmcraft_color::Lut3d::apply`]. Tested for parity with the CPU path.

use filmcraft_color::Lut3d;

pub struct GpuLut {
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

impl GpuLut {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-lut"),
            source: wgpu::ShaderSource::Wgsl(include_str!("lut.wgsl").into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lut"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage(1, true),
                storage(2, true),
                storage(3, false),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("lut"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("lut"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        GpuLut { device: device.clone(), queue: queue.clone(), pipeline, bgl }
    }

    /// Apply `lut` to straight RGBA f32 pixels (alpha passes through). Blocks until done.
    pub fn apply(&self, lut: &Lut3d, rgba: &[f32]) -> Option<Vec<f32>> {
        use wgpu::util::DeviceExt;
        let count = (rgba.len() / 4) as u32;
        if count == 0 {
            return Some(Vec::new());
        }
        let mut params = Vec::with_capacity(48);
        for v in [lut.size as u32, count, 0, 0] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        for v in lut.domain_min.iter().chain(&[0.0]).chain(lut.domain_max.iter()).chain(&[1.0]) {
            params.extend_from_slice(&v.to_le_bytes());
        }
        let lut_bytes: Vec<u8> = lut.data.iter().flat_map(|c| [c[0], c[1], c[2], 0.0]).flat_map(f32::to_le_bytes).collect();
        let src_bytes: Vec<u8> = rgba.iter().flat_map(|v| v.to_le_bytes()).collect();
        let d = &self.device;
        let ub = d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("lut-params"), contents: &params, usage: wgpu::BufferUsages::UNIFORM });
        let lb = d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("lut-data"), contents: &lut_bytes, usage: wgpu::BufferUsages::STORAGE });
        let sb = d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("lut-src"), contents: &src_bytes, usage: wgpu::BufferUsages::STORAGE });
        let size = src_bytes.len() as u64;
        let ob = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lut-dst"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let rb = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lut-read"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let bg = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: lb.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: sb.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: ob.as_entire_binding() },
            ],
        });
        let mut enc = d.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("lut") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("lut"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            let groups = count.div_ceil(64);
            pass.dispatch_workgroups(groups.min(65535), groups.div_ceil(65535), 1);
        }
        enc.copy_buffer_to_buffer(&ob, 0, &rb, 0, size);
        self.queue.submit([enc.finish()]);
        let slice = rb.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        d.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(8)) }).ok()?;
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        let out = data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        drop(data);
        rb.unmap();
        Some(out)
    }
}
