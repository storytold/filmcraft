use filmcraft_project::{EffectInstance, ParamValue};
use wgpu::util::DeviceExt;

pub(crate) struct PushStage {
    pipeline: wgpu::RenderPipeline,
}

impl PushStage {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("push"),
            source: wgpu::ShaderSource::Wgsl(include_str!("transition.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("push"),
            layout: None,
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
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
        Self { pipeline }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &wgpu::TextureView,
        inputs: [&wgpu::TextureView; 2],
        size: (u32, u32),
        effect: &EffectInstance,
        progress: f32,
    ) {
        let value = effect.param("direction").map(|p| p.value.clone()).or_else(|| effect.def().and_then(|d| d.param("direction")).map(|p| p.default.clone()));
        let direction = match value {
            Some(ParamValue::Choice(v)) => v,
            _ => 0,
        };
        let blur = effect.f64_at("motion_blur", Default::default()) as f32;
        let blur = if blur.is_finite() { (blur / 100.0).clamp(0.0, 1.0) } else { 0.0 };
        let progress = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
        let values = [size.0 as f32, size.1 as f32, progress, blur, direction as f32, 0.0, 0.0, 0.0];
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let uniform =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("push-params"), contents: &bytes, usage: wgpu::BufferUsages::UNIFORM });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("push"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(inputs[0]) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(inputs[1]) },
                wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("push") });
        {
            let mut pass = super::accum_pass(&mut encoder, canvas, wgpu::LoadOp::Load);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}
