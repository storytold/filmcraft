//! GPU-only Gaussian transition stages for typed blur operations.

use filmcraft_render::transitions::gpu_plan::{BlurKind, BlurOp};
use wgpu::util::DeviceExt;

use crate::transition_catalog::TransitionCatalogError;

type Target = (wgpu::Texture, wgpu::TextureView, (u32, u32));
/// Full-frame Rgba32Float scratch slots `draw` uses (indices 0..=4). Slot sizes scale with the frame.
const TEMP_TEXTURES: u64 = 5;

/// Bytes of scratch textures one blur needs at `size`, or `None` when the arithmetic overflows.
/// No artificial cap: the device decides whether it can allocate them (see `draw`).
fn temp_bytes(size: (u32, u32)) -> Option<u64> {
    u64::from(size.0).checked_mul(u64::from(size.1))?.checked_mul(16)?.checked_mul(TEMP_TEXTURES)
}

pub(crate) struct TransitionBlurStage {
    box_pipeline: wgpu::ComputePipeline,
    mix_pipeline: wgpu::ComputePipeline,
    whip_pipeline: wgpu::ComputePipeline,
    finish_pipeline: wgpu::RenderPipeline,
    targets: [Option<Target>; 6],
}

impl TransitionBlurStage {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("transition-blur"),
            source: wgpu::ShaderSource::Wgsl(include_str!("transition_blur.wgsl").into()),
        });
        let box_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("transition-blur-box"),
            layout: None,
            module: &shader,
            entry_point: Some("cs_box"),
            compilation_options: Default::default(),
            cache: None,
        });
        let mix_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("transition-blur-mix"),
            layout: None,
            module: &shader,
            entry_point: Some("cs_mix"),
            compilation_options: Default::default(),
            cache: None,
        });
        let whip_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("transition-whip-prepare"),
            layout: None,
            module: &shader,
            entry_point: Some("cs_whip_prepare"),
            compilation_options: Default::default(),
            cache: None,
        });
        let finish_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("transition-blur-finish"),
            layout: None,
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_spherical"),
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
        Self { box_pipeline, mix_pipeline, whip_pipeline, finish_pipeline, targets: [None, None, None, None, None, None] }
    }

    fn target(&mut self, device: &wgpu::Device, index: usize, size: (u32, u32)) -> wgpu::TextureView {
        if let Some(target) = self.targets[index].as_ref().filter(|target| target.2 == size) {
            return target.1.clone();
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("transition-blur-temp"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        self.targets[index] = Some((texture, view.clone(), size));
        view
    }

    fn params(size: (u32, u32), op: BlurOp, radius: u32, axis: u32) -> [u32; 32] {
        let mut words = [0; 32];
        words[0] = size.0;
        words[1] = size.1;
        words[2] = radius;
        words[3] = axis;
        words[4] = op.kind as u32;
        words[8] = op.progress.to_bits();
        words[9] = op.blur_px.to_bits();
        words
    }

    fn uniform(device: &wgpu::Device, words: &[u32; 32]) -> wgpu::Buffer {
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_le_bytes()).collect();
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("transition-blur-params"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        })
    }

    fn box_pass(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output_slot: usize,
        radius: u32,
        axis: u32,
        size: (u32, u32),
        op: BlurOp,
    ) -> wgpu::TextureView {
        let output = self.target(device, output_slot, size);
        let params = Self::uniform(device, &Self::params(size, op, radius, axis));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transition-blur-box"),
            layout: &self.box_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(input) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&output) },
            ],
        });
        let lines = if axis == 0 { size.1 } else { size.0 };
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("transition-blur-box"), timestamp_writes: None });
        pass.set_pipeline(&self.box_pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(lines.div_ceil(64), 1, 1);
        drop(pass);
        output
    }

    fn mix_pass(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        a: &wgpu::TextureView,
        b: &wgpu::TextureView,
        output_slot: usize,
        size: (u32, u32),
        op: BlurOp,
    ) -> wgpu::TextureView {
        let output = self.target(device, output_slot, size);
        let params = Self::uniform(device, &Self::params(size, op, 0, 0));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transition-blur-mix"),
            layout: &self.mix_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(a) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(b) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&output) },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("transition-blur-mix"), timestamp_writes: None });
        pass.set_pipeline(&self.mix_pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(size.0.div_ceil(8), size.1.div_ceil(8), 1);
        drop(pass);
        output
    }

    fn whip_prepare(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        inputs: [&wgpu::TextureView; 2],
        size: (u32, u32),
        op: BlurOp,
    ) -> wgpu::TextureView {
        let output = self.target(device, 4, size);
        let mut words = [0; 32];
        words[0] = size.0;
        words[1] = size.1;
        words[4] = op.kind as u32;
        words[5] = op.direction;
        words[8] = op.progress.to_bits();
        let params = Self::uniform(device, &words);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transition-whip-prepare"),
            layout: &self.whip_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(inputs[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(inputs[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&output) },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("transition-whip-prepare"), timestamp_writes: None });
        pass.set_pipeline(&self.whip_pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(size.0.div_ceil(8), size.1.div_ceil(8), 1);
        drop(pass);
        output
    }

    fn blur_chain(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        mut input: wgpu::TextureView,
        scratch: [usize; 2],
        radii: &(Vec<u32>, Vec<u32>),
        size: (u32, u32),
        op: BlurOp,
    ) -> wgpu::TextureView {
        let mut next = 0;
        for (axis, values) in [(0, &radii.0), (1, &radii.1)] {
            for &radius in values {
                input = self.box_pass(device, encoder, &input, scratch[next], radius, axis, size, op);
                next ^= 1;
            }
        }
        input
    }

    fn finish(&self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, source: &wgpu::TextureView, canvas: &wgpu::TextureView) {
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transition-blur-finish"),
            layout: &self.finish_pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(source) }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("transition-blur-finish"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: canvas,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.finish_pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    pub(crate) fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &wgpu::TextureView,
        inputs: [&wgpu::TextureView; 2],
        size: (u32, u32),
        op: BlurOp,
    ) -> Result<(), TransitionCatalogError> {
        let limits = device.limits();
        if size.0 == 0 || size.1 == 0 || size.0 > limits.max_texture_dimension_2d || size.1 > limits.max_texture_dimension_2d || temp_bytes(size).is_none() {
            return Err(TransitionCatalogError::InvalidSize);
        }
        if !op.gpu_ok() {
            return Err(TransitionCatalogError::InvalidPlan {
                id: match op.kind {
                    BlurKind::Dissolve => "blur_dissolve",
                    BlurKind::VrSpherical => "vr_spherical_blur",
                    BlurKind::WhipLegacy => "whip_legacy",
                }
                .into(),
            });
        }
        let p = op.progress;
        // Allocation failures (scratch textures, bind groups) are captured here and returned as an error:
        // no CPU fallback, and no panic through the device's default uncaptured-error handler.
        let oom_scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("transition-blur") });
        let final_view = match op.kind {
            BlurKind::Dissolve => {
                let sigma = op.blur_px * 0.5;
                let smooth = |lo: f32, hi: f32, x: f32| {
                    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
                    t * t * (3.0 - 2.0 * t)
                };
                let radii_a =
                    filmcraft_render::effects::gaussian_boxes(size.0 as usize, size.1 as usize, sigma * smooth(0.0, 0.6, p), sigma * smooth(0.0, 0.6, p));
                let radii_b = filmcraft_render::effects::gaussian_boxes(
                    size.0 as usize,
                    size.1 as usize,
                    sigma * (1.0 - smooth(0.4, 1.0, p)),
                    sigma * (1.0 - smooth(0.4, 1.0, p)),
                );
                let a = self.blur_chain(device, &mut encoder, inputs[0].clone(), [0, 1], &radii_a, size, op);
                let b = self.blur_chain(device, &mut encoder, inputs[1].clone(), [2, 3], &radii_b, size, op);
                self.mix_pass(device, &mut encoder, &a, &b, 4, size, op)
            }
            BlurKind::VrSpherical => {
                let mixed = self.mix_pass(device, &mut encoder, inputs[0], inputs[1], 4, size, op);
                let t = 4.0 * p * (1.0 - p);
                let env = t * t * (3.0 - 2.0 * t);
                let sigma = op.blur_px * 0.5 * env;
                let radii = filmcraft_render::effects::gaussian_boxes(size.0 as usize, size.1 as usize, sigma, sigma);
                self.blur_chain(device, &mut encoder, mixed, [0, 1], &radii, size, op)
            }
            BlurKind::WhipLegacy => {
                let pushed = self.whip_prepare(device, &mut encoder, inputs, size, op);
                let sigma = (1.0 - (2.0 * p - 1.0).abs()) * 40.0 * (size.0 as f32 / 1920.0);
                if sigma > 0.5 {
                    let radii = filmcraft_render::effects::gaussian_boxes(size.0 as usize, size.1 as usize, sigma, 0.0);
                    self.blur_chain(device, &mut encoder, pushed, [0, 1], &radii, size, op)
                } else {
                    pushed
                }
            }
        };
        self.finish(device, &mut encoder, &final_view, canvas);
        queue.submit([encoder.finish()]);
        let validation_error = pollster::block_on(validation_scope.pop());
        let oom_error = pollster::block_on(oom_scope.pop());
        if let Some(error) = oom_error.or(validation_error) {
            // The cached targets may be invalid textures now; rebuild them on the next draw.
            self.targets = [None, None, None, None, None, None];
            return Err(TransitionCatalogError::AllocationFailed { detail: error.to_string() });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_bytes_counts_five_full_frame_slots_at_4k_and_8k() {
        // 3840 x 2160 x 16 bytes x 5 slots = 663_552_000 bytes (about 633 MiB), above the old 512 MiB cap.
        // The size checks (zero, max texture side) are in draw(); this only pins the arithmetic.
        assert_eq!(temp_bytes((3840, 2160)), Some(663_552_000));
        assert!(temp_bytes((7680, 4320)).is_some());
    }

    #[test]
    fn an_overflowing_blur_size_is_refused_by_the_arithmetic() {
        // (2^32 - 1)^2 x 16 x 5 overflows u64, so the request is refused without any allocation.
        assert_eq!(temp_bytes((u32::MAX, u32::MAX)), None);
        assert_eq!(temp_bytes((0, 0)), Some(0));
    }
}
