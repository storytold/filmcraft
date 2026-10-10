//! Bounded exact-geometry cache for GPU mask coverage. No CPU image or GPU readback.
use filmcraft_render::mask::FlatMask;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

struct Entry {
    view: wgpu::TextureView,
    bytes: usize,
    age: u64,
}
pub(crate) struct MaskCache {
    bgl: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    entries: HashMap<Vec<u8>, Entry>,
    bytes: usize,
    clock: u64,
}
impl MaskCache {
    pub fn new(device: &wgpu::Device) -> Self {
        let buffer = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask-coverage"),
            entries: &[
                buffer(0, wgpu::BufferBindingType::Uniform),
                buffer(1, wgpu::BufferBindingType::Storage { read_only: true }),
                buffer(2, wgpu::BufferBindingType::Storage { read_only: true }),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mask-coverage"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mask-coverage"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mask_cache.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mask-coverage"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { bgl, pipeline, entries: HashMap::new(), bytes: 0, clock: 0 }
    }
    fn trim(&mut self, budget: usize) {
        while self.bytes > budget {
            let Some(key) = self.entries.iter().min_by_key(|(_, e)| e.age).map(|(k, _)| k.clone()) else { break };
            if let Some(e) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(e.bytes);
            }
        }
    }
    pub fn trim_to_budget(&mut self) {
        self.trim((filmcraft_frame::memory::budgets().gpu_uploads / 4).min(64 << 20));
    }

    pub fn coverage(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, masks: &[FlatMask], w: u32, h: u32) -> Option<wgpu::TextureView> {
        let budget = (filmcraft_frame::memory::budgets().gpu_uploads / 4).min(64 << 20);
        self.trim(budget);
        let bytes = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
        if bytes > budget
            || masks.is_empty()
            || w.div_ceil(16) > device.limits().max_compute_workgroups_per_dimension
            || h.div_ceil(16) > device.limits().max_compute_workgroups_per_dimension
        {
            return None;
        }
        let params = [w, h, masks.len() as u32, 0].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>();
        let mut infos = Vec::new();
        let mut points = Vec::new();
        let mut start = 0u32;
        for mask in masks {
            for v in [start, mask.pts.len() as u32, mask.mode.index(), u32::from(mask.inverted)] {
                infos.extend_from_slice(&v.to_le_bytes());
            }
            for v in [mask.feather, mask.expansion, mask.opacity, 0.0] {
                infos.extend_from_slice(&v.to_le_bytes());
            }
            for v in mask.pts.iter().flatten() {
                points.extend_from_slice(&v.to_le_bytes());
            }
            start = start.saturating_add(mask.pts.len() as u32);
        }
        let mut key = params.clone();
        key.extend_from_slice(&infos);
        key.extend_from_slice(&points);
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.age = self.clock;
            return Some(entry.view.clone());
        }
        self.trim(budget.saturating_sub(bytes));
        if points.is_empty() {
            points.resize(8, 0);
        }
        let init = |label, contents: &[u8], usage| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents, usage });
        let ub = init("mask-params", &params, wgpu::BufferUsages::UNIFORM);
        let mb = init("mask-infos", &infos, wgpu::BufferUsages::STORAGE);
        let pb = init("mask-points", &points, wgpu::BufferUsages::STORAGE);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cached-mask"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask-coverage"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: mb.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: pb.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&view) },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("mask-coverage") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("mask-coverage"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(w.div_ceil(16), h.div_ceil(16), 1);
        }
        // Submit before publishing the entry: even if the caller abandons its effect job, this
        // texture will be initialized. Subsequent work on this same queue reads completed coverage.
        queue.submit([encoder.finish()]);
        self.entries.insert(key, Entry { view: view.clone(), bytes, age: self.clock });
        self.bytes = self.bytes.saturating_add(bytes);
        Some(view)
    }
}
