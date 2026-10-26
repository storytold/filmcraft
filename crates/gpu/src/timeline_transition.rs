use filmcraft_color::srgb_to_linear;
use filmcraft_project::{EffectInstance, ParamValue};
use filmcraft_time::Tick;
use wgpu::util::DeviceExt;

pub(crate) struct FlareStage {
    pipeline: wgpu::RenderPipeline,
}

impl FlareStage {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flare-transition"),
            source: wgpu::ShaderSource::Wgsl(include_str!("timeline_transition.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flare-transition"),
            layout: None,
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_flare"),
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

    pub(crate) fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &wgpu::TextureView,
        inputs: [&wgpu::TextureView; 2],
        size: (u32, u32),
        effect: &EffectInstance,
        progress: f32,
    ) {
        let progress = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
        let angle = number(effect, "angle") + 90.0;
        let (dx, dy) = angle.to_radians().sin_cos();
        let amount = number(effect, "amount") / 100.0;
        let color = color(effect, "color");
        let values = [size.0 as f32, size.1 as f32, progress, amount, dx, -dy, 0.0, 0.0, color[0], color[1], color[2], color[3]];
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("flare-transition-params"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flare-transition"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(inputs[0]) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(inputs[1]) },
                wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("flare-transition") });
        {
            let mut pass = super::accum_pass(&mut encoder, canvas, wgpu::LoadOp::Load);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }
}

fn number(effect: &EffectInstance, id: &str) -> f32 {
    let value = effect.f64_at(id, Tick::ZERO) as f32;
    if value.is_finite() { value } else { 0.0 }
}

fn color(effect: &EffectInstance, id: &str) -> [f32; 4] {
    let encoded = effect.param(id).map(|p| p.value.clone()).or_else(|| effect.def().and_then(|d| d.param(id)).map(|p| p.default.clone()));
    let encoded = match encoded {
        Some(ParamValue::Color(c)) => c,
        _ => [1.0; 4],
    };
    [srgb_to_linear(encoded[0].clamp(0.0, 1.0)), srgb_to_linear(encoded[1].clamp(0.0, 1.0)), srgb_to_linear(encoded[2].clamp(0.0, 1.0)), 1.0]
}
