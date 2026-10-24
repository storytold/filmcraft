use filmcraft_project::EffectInstance;
use filmcraft_time::Tick;
use wgpu::util::DeviceExt;

type Target = (wgpu::Texture, wgpu::TextureView, (u32, u32));

pub(crate) struct BlurDissolveStage {
    convert: wgpu::RenderPipeline,
    run: wgpu::ComputePipeline,
    mix: wgpu::RenderPipeline,
    targets: [Option<Target>; 6],
}

impl BlurDissolveStage {
    pub(crate) fn new(device: &wgpu::Device) -> Option<Self> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blur-dissolve"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blur_transition.wgsl").into()),
        });
        let convert = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blur-dissolve-convert"),
            layout: None,
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_convert"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: super::fx::FX_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let run = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("blur-dissolve-box"),
            layout: None,
            module: &shader,
            entry_point: Some("cs_box"),
            compilation_options: Default::default(),
            cache: None,
        });
        let mix = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blur-dissolve-mix"),
            layout: None,
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_mix"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: super::ACCUM_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        match super::poll_now(scope.pop()) {
            Some(None) => Some(Self { convert, run, mix, targets: [None, None, None, None, None, None] }),
            Some(Some(error)) => {
                log::warn!("GPU Blur Dissolve unavailable: {error}");
                None
            }
            None => None,
        }
    }

    fn target(&mut self, device: &wgpu::Device, index: usize, size: (u32, u32)) -> wgpu::TextureView {
        if let Some(target) = self.targets[index].as_ref().filter(|t| t.2 == size) {
            return target.1.clone();
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blur-dissolve-temp"),
            size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: super::fx::FX_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        self.targets[index] = Some((texture, view.clone(), size));
        view
    }

    fn uniform(device: &wgpu::Device, values: &[u32], label: &'static str) -> wgpu::Buffer {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: &bytes, usage: wgpu::BufferUsages::UNIFORM })
    }

    fn convert(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        size: (u32, u32),
        slot: usize,
    ) -> wgpu::TextureView {
        let dest = self.target(device, slot, size);
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur-dissolve-convert"),
            layout: &self.convert.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(source) }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("blur-dissolve-convert"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &dest,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.convert);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        dest
    }

    fn box_pass(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        radius: u32,
        axis: u32,
        size: (u32, u32),
        slot: usize,
    ) -> wgpu::TextureView {
        if radius == 0 {
            return source.clone();
        }
        let dest = self.target(device, slot, size);
        let uniform = Self::uniform(device, &[size.0, size.1, radius, axis], "blur-dissolve-box-params");
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur-dissolve-box"),
            layout: &self.run.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(source) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&dest) },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("blur-dissolve-box"), timestamp_writes: None });
        pass.set_pipeline(&self.run);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups(if axis == 0 { size.1.div_ceil(64) } else { size.0.div_ceil(64) }, 1, 1);
        dest
    }

    fn blur_side(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        radii: &(Vec<u32>, Vec<u32>),
        size: (u32, u32),
        side: usize,
    ) -> wgpu::TextureView {
        let base = side * 3;
        let mut view = self.convert(device, encoder, input, size, base);
        for (axis, radius, _, dst) in box_plan(radii, base) {
            view = self.box_pass(device, encoder, &view, radius, axis, size, dst);
        }
        view
    }

    pub(crate) fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &wgpu::TextureView,
        inputs: [&wgpu::TextureView; 2],
        size: (u32, u32),
        effect: &EffectInstance,
        progress: f32,
        scale: f32,
    ) {
        let size = (size.0.max(1), size.1.max(1));
        let p = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
        let blur = effect.f64_at("blur", Tick::ZERO) as f32;
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let sigma = if blur.is_finite() { blur.max(0.0) * scale * 0.5 } else { 0.0 };
        let sa = sigma * smooth(0.0, 0.6, p);
        let sb = sigma * (1.0 - smooth(0.4, 1.0, p));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("blur-dissolve") });
        let (va, vb) = if p <= 0.0 {
            (inputs[0].clone(), inputs[0].clone())
        } else if p >= 1.0 {
            (inputs[1].clone(), inputs[1].clone())
        } else {
            let radii = filmcraft_render::effects::gaussian_boxes(size.0 as usize, size.1 as usize, sa, sa);
            let va = self.blur_side(device, &mut encoder, inputs[0], &radii, size, 0);
            let radii = filmcraft_render::effects::gaussian_boxes(size.0 as usize, size.1 as usize, sb, sb);
            let vb = self.blur_side(device, &mut encoder, inputs[1], &radii, size, 1);
            (va, vb)
        };
        let uniform = Self::uniform(device, &[(size.0 as f32).to_bits(), (size.1 as f32).to_bits(), p.to_bits(), 0], "blur-dissolve-mix-params");
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur-dissolve-mix"),
            layout: &self.mix.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&va) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&vb) },
                wgpu::BindGroupEntry { binding: 6, resource: uniform.as_entire_binding() },
            ],
        });
        let mut pass = super::accum_pass(&mut encoder, canvas, wgpu::LoadOp::Load);
        pass.set_pipeline(&self.mix);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        queue.submit([encoder.finish()]);
    }
}

fn smooth(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Box passes for one side as `(axis, radius, source slot, destination slot)`, starting from the
/// `base` slot. Radius-0 passes are skipped: they must not flip the ping-pong slot, or a later pass
/// would read and write the same texture.
fn box_plan(radii: &(Vec<u32>, Vec<u32>), base: usize) -> Vec<(u32, u32, usize, usize)> {
    let mut plan = Vec::new();
    let (mut src, mut slot) = (base, base + 1);
    for (axis, rs) in [(0u32, &radii.0), (1u32, &radii.1)] {
        for &radius in rs {
            if radius == 0 {
                continue;
            }
            plan.push((axis, radius, src, slot));
            src = slot;
            slot = if slot == base + 1 { base + 2 } else { base + 1 };
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::box_plan;

    #[test]
    fn zero_radius_pass_never_aliases_input_and_output() {
        let plan = box_plan(&(vec![5, 0, 5], vec![0, 3]), 0);
        assert_eq!(plan, vec![(0, 5, 0, 1), (0, 5, 1, 2), (1, 3, 2, 1)]);
        for (_, _, src, dst) in plan {
            assert_ne!(src, dst);
        }
        assert!(box_plan(&(vec![0, 0], vec![0]), 3).is_empty());
    }
}
