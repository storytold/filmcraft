//! Exact multi-pass GPU routes for transition lights that need intermediate images.

use filmcraft_render::transitions::gpu_plan::gpu_lights::{LightKind, LightOp};
use wgpu::util::DeviceExt;

use crate::transition_catalog::TransitionCatalogError;

type Target = (wgpu::Texture, wgpu::TextureView, (u32, u32));

/// Bytes of the two full-frame Rgba32Float scratch slots at `size`, or `None` when the arithmetic
/// overflows. No artificial cap: the device decides whether it can allocate them (see `draw`).
fn temp_bytes(size: (u32, u32)) -> Option<u64> {
    u64::from(size.0).checked_mul(u64::from(size.1)).and_then(|n| n.checked_mul(16)).and_then(|n| n.checked_mul(2))
}

pub(crate) struct TransitionLightMultipassStage {
    mix: wgpu::ComputePipeline,
    ray_bright: wgpu::ComputePipeline,
    box_blur: wgpu::ComputePipeline,
    ray_zoom: wgpu::ComputePipeline,
    cross_zoom_legacy: wgpu::ComputePipeline,
    finish: wgpu::RenderPipeline,
    targets: [Option<Target>; 2],
}

impl TransitionLightMultipassStage {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("transition-light-multipass"),
            source: wgpu::ShaderSource::Wgsl(include_str!("transition_light_multipass.wgsl").into()),
        });
        let pipeline = |label, entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: None,
                module: &shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let finish = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("transition-light-multipass-finish"),
            layout: None,
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_light"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            mix: pipeline("transition-light-mix", "cs_mix"),
            ray_bright: pipeline("transition-ray-bright", "cs_ray_bright"),
            box_blur: pipeline("transition-light-box", "cs_box"),
            ray_zoom: pipeline("transition-ray-zoom", "cs_ray_zoom"),
            cross_zoom_legacy: pipeline("transition-cross-zoom-legacy", "cs_cross_zoom_legacy"),
            finish,
            targets: [None, None],
        }
    }

    fn target(&mut self, device: &wgpu::Device, slot: usize, size: (u32, u32)) -> wgpu::TextureView {
        if let Some((_, view, _)) = self.targets[slot].as_ref().filter(|t| t.2 == size) {
            return view.clone();
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("transition-light-rgba32-temp"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        self.targets[slot] = Some((texture, view.clone(), size));
        view
    }

    fn params(device: &wgpu::Device, size: (u32, u32), kernel: u32, radius: u32, axis: u32, p: f32, f0: [f32; 3], f1: [f32; 4]) -> wgpu::Buffer {
        let mut words = [0u32; 20];
        words[0] = size.0;
        words[1] = size.1;
        words[2] = radius;
        words[3] = axis;
        words[4] = kernel;
        words[8] = p.to_bits();
        words[9..12].copy_from_slice(&f0.map(f32::to_bits));
        words[12..16].copy_from_slice(&f1.map(f32::to_bits));
        let bytes: Vec<u8> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("transition-light-multipass-params"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        })
    }

    fn compute_bind(
        device: &wgpu::Device,
        pipeline: &wgpu::ComputePipeline,
        params: &wgpu::Buffer,
        a: &wgpu::TextureView,
        b: Option<&wgpu::TextureView>,
        output: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        let mut entries = vec![
            wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(a) },
        ];
        if let Some(b) = b {
            entries.push(wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(b) });
        }
        entries.push(wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(output) });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transition-light-multipass-bind"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        })
    }

    fn dispatch(encoder: &mut wgpu::CommandEncoder, pipeline: &wgpu::ComputePipeline, bind: &wgpu::BindGroup, groups: (u32, u32)) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("transition-light-multipass"), timestamp_writes: None });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.dispatch_workgroups(groups.0, groups.1, 1);
    }

    fn run_box(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        source_slot: usize,
        radius: u32,
        axis: u32,
        size: (u32, u32),
        kernel: u32,
        p: f32,
    ) -> (wgpu::TextureView, usize) {
        if radius == 0 {
            return (source.clone(), source_slot);
        }
        let dest_slot = 1 - source_slot;
        let dest = self.target(device, dest_slot, size);
        let uniform = Self::params(device, size, kernel, radius, axis, p, [0.0; 3], [0.0; 4]);
        let bind = Self::compute_bind(device, &self.box_blur, &uniform, source, None, &dest);
        let lines = if axis == 0 { size.1 } else { size.0 };
        Self::dispatch(encoder, &self.box_blur, &bind, (lines.div_ceil(64), 1));
        (dest, dest_slot)
    }

    fn blur(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        mut view: wgpu::TextureView,
        mut slot: usize,
        radii: &(Vec<u32>, Vec<u32>),
        size: (u32, u32),
        kernel: u32,
        p: f32,
    ) -> (wgpu::TextureView, usize) {
        for (axis, radii) in [(0u32, &radii.0), (1u32, &radii.1)] {
            for &radius in radii {
                (view, slot) = self.run_box(device, encoder, &view, slot, radius, axis, size, kernel, p);
            }
        }
        (view, slot)
    }

    pub(crate) fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &wgpu::TextureView,
        inputs: [&wgpu::TextureView; 2],
        size: (u32, u32),
        op: &LightOp,
    ) -> Result<(), TransitionCatalogError> {
        let id = match op.kind {
            LightKind::Glow => "glow",
            LightKind::Ray => "ray",
            LightKind::CrossZoomLegacy => "cross_zoom_legacy",
            _ => return Err(TransitionCatalogError::Unsupported { id: "non-multipass light".into() }),
        };
        if !op.gpu_ok() {
            return Err(TransitionCatalogError::InvalidPlan { id: id.into() });
        }
        let limits = device.limits();
        if size.0 == 0 || size.1 == 0 || size.0 > limits.max_texture_dimension_2d || size.1 > limits.max_texture_dimension_2d || temp_bytes(size).is_none() {
            return Err(TransitionCatalogError::InvalidSize);
        }
        if self.targets.iter().flatten().any(|target| target.2 != size) {
            self.targets = [None, None];
        }
        // Allocation failures are captured and returned as an error: no CPU fallback, no panic through
        // the device's default uncaptured-error handler.
        let oom_scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("transition-light-multipass") });
        let p = op.progress;
        let aux = match op.kind {
            LightKind::Glow => {
                let first = self.target(device, 0, size);
                let uniform = Self::params(device, size, op.kind as u32, 0, 0, p, [0.0; 3], [0.0; 4]);
                let bind = Self::compute_bind(device, &self.mix, &uniform, inputs[0], Some(inputs[1]), &first);
                Self::dispatch(&mut encoder, &self.mix, &bind, (size.0.div_ceil(8), size.1.div_ceil(8)));
                let sigma = op.radius * 0.5;
                let radii = filmcraft_render::effects::gaussian_boxes(size.0 as usize, size.1 as usize, sigma, sigma);
                self.blur(device, &mut encoder, first, 0, &radii, size, op.kind as u32, p).0
            }
            LightKind::Ray => {
                let bright = self.target(device, 0, size);
                let uniform = Self::params(device, size, op.kind as u32, 0, 0, p, [0.0; 3], [0.0; 4]);
                let bind = Self::compute_bind(device, &self.ray_bright, &uniform, inputs[0], Some(inputs[1]), &bright);
                Self::dispatch(&mut encoder, &self.ray_bright, &bind, (size.0.div_ceil(8), size.1.div_ceil(8)));
                let rays = self.target(device, 1, size);
                let center =
                    [if op.center_set[0] { op.center[0] } else { size.0 as f32 * 0.5 }, if op.center_set[1] { op.center[1] } else { size.1 as f32 * 0.5 }];
                let uniform = Self::params(device, size, op.kind as u32, 0, 0, p, [op.length * 0.9, 0.0, 0.0], [center[0], center[1], 0.0, 0.0]);
                let bind = Self::compute_bind(device, &self.ray_zoom, &uniform, &bright, None, &rays);
                Self::dispatch(&mut encoder, &self.ray_zoom, &bind, (size.0.div_ceil(8), size.1.div_ceil(8)));
                rays
            }
            LightKind::CrossZoomLegacy => {
                let transformed = self.target(device, 0, size);
                let source = if p < 0.5 { inputs[0] } else { inputs[1] };
                let uniform = Self::params(device, size, op.kind as u32, 0, 0, p, [0.0; 3], [0.0; 4]);
                let bind = Self::compute_bind(device, &self.cross_zoom_legacy, &uniform, source, None, &transformed);
                Self::dispatch(&mut encoder, &self.cross_zoom_legacy, &bind, (size.0.div_ceil(8), size.1.div_ceil(8)));
                let z = 1.0 + 3.0 * if p < 0.5 { p * 2.0 } else { (1.0 - p) * 2.0 };
                let sigma = (z - 1.0) * 6.0 * (size.0 as f32 / 1920.0);
                let radii = filmcraft_render::effects::gaussian_boxes(size.0 as usize, size.1 as usize, sigma, sigma);
                self.blur(device, &mut encoder, transformed, 0, &radii, size, op.kind as u32, p).0
            }
            _ => {
                // `id` already admits only the three kinds above; close the scopes before refusing.
                pollster::block_on(validation_scope.pop());
                pollster::block_on(oom_scope.pop());
                return Err(TransitionCatalogError::Unsupported { id: id.into() });
            }
        };
        let final_params = Self::params(device, size, op.kind as u32, 0, 0, p, [0.0, op.amount, op.intensity], [0.0; 4]);
        let final_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transition-light-multipass-finish"),
            layout: &self.finish.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: final_params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(inputs[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(inputs[1]) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&aux) },
            ],
        });
        let mut pass = crate::accum_pass(&mut encoder, canvas, wgpu::LoadOp::Load);
        pass.set_pipeline(&self.finish);
        pass.set_bind_group(0, &final_bind, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        queue.submit([encoder.finish()]);
        let validation_error = pollster::block_on(validation_scope.pop());
        let oom_error = pollster::block_on(oom_scope.pop());
        if let Some(error) = oom_error.or(validation_error) {
            // The cached targets may be invalid textures now; rebuild them on the next draw.
            self.targets = [None, None];
            return Err(TransitionCatalogError::AllocationFailed { detail: error.to_string() });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_bytes_counts_two_full_frame_slots_at_4k_and_8k() {
        assert_eq!(temp_bytes((3840, 2160)), Some(265_420_800));
        assert!(temp_bytes((7680, 4320)).is_some());
        assert_eq!(temp_bytes((u32::MAX, u32::MAX)), None);
        assert_eq!(temp_bytes((0, 100)), Some(0));
    }
}
