use std::f32::consts::{FRAC_PI_2, PI, TAU};

use filmcraft_project::{EffectInstance, ParamValue};
use filmcraft_time::Tick;
use wgpu::util::DeviceExt;

#[derive(Clone, Copy)]
enum Field {
    Linear { dx: f32, dy: f32 },
    Clock { start: f32 },
    Radial { corner: u32 },
    IrisRound,
    IrisBox,
    IrisDiamond,
    IrisCross,
    BarnDoors { vertical: bool },
    Inset { corner: u32 },
    Wipe { direction: u32 },
}

impl Field {
    fn of(effect: &EffectInstance) -> Option<Self> {
        let get_choice = |id: &str| choice(effect, id);
        Some(match effect.effect.as_str() {
            "linear_wipe" | "soft_wipe" => {
                let (s, c) = number(effect, "angle").to_radians().sin_cos();
                Self::Linear { dx: s, dy: -c }
            }
            "clock_wipe" => Self::Clock { start: number(effect, "start").to_radians() },
            "radial_wipe" => Self::Radial { corner: get_choice("corner").min(3) },
            "iris_round" => Self::IrisRound,
            "iris_box" => Self::IrisBox,
            "iris_diamond" => Self::IrisDiamond,
            "iris_cross" => Self::IrisCross,
            "barn_doors" => Self::BarnDoors { vertical: get_choice("orientation") == 1 },
            "inset" => Self::Inset { corner: get_choice("corner").min(3) },
            "wipe" => {
                let direction = match effect.param("direction").map(|p| &p.value) {
                    Some(ParamValue::Choice(v)) => *v,
                    _ => 3,
                };
                Self::Wipe { direction: direction.min(3) }
            }
            _ => return None,
        })
    }

    fn value(self, x: f32, y: f32, w: f32, h: f32, cx: f32, cy: f32, aspect: f32) -> f32 {
        match self {
            Self::Linear { dx, dy } => x * dx + y * dy,
            Self::Wipe { .. } => 0.0,
            Self::Clock { start } => ((x - cx).atan2(-(y - cy)) - start).rem_euclid(TAU) / TAU * (w + h) * 0.5,
            Self::Radial { corner } => {
                let (ox, oy, a0) = corner_point(corner, w, h);
                let a = ((y - oy).atan2(x - ox) - a0).rem_euclid(TAU);
                let a = if a > PI * 1.5 { 0.0 } else { a.min(FRAC_PI_2) };
                a / FRAC_PI_2 * (w + h) * 0.5
            }
            Self::IrisRound => ((x - cx).powi(2) + (y - cy).powi(2)).sqrt(),
            Self::IrisBox => (x - cx).abs().max((y - cy).abs() * aspect),
            Self::IrisDiamond => (x - cx).abs() + (y - cy).abs() * aspect,
            Self::IrisCross => {
                let (u, v) = ((x - cx).abs(), (y - cy).abs() * aspect);
                (u / 0.3).max(v).min(u.max(v / 0.3))
            }
            Self::BarnDoors { vertical } => {
                if vertical {
                    (x - cx).abs()
                } else {
                    (y - cy).abs() * aspect
                }
            }
            Self::Inset { corner } => {
                let (ox, oy, _) = corner_point(corner, w, h);
                (x - ox).abs().max((y - oy).abs() * aspect)
            }
        }
    }

    fn uniforms(self, cx: f32, cy: f32, aspect: f32) -> ([f32; 4], [f32; 4]) {
        let (mut sweep, mut shape) = ([0.0; 4], [0.0; 4]);
        shape[1] = cx;
        shape[2] = cy;
        shape[3] = aspect;
        match self {
            Self::Linear { dx, dy } => {
                sweep[3] = dx;
                shape[0] = dy;
            }
            Self::Clock { start } => sweep[3] = start,
            _ => {}
        }
        (sweep, shape)
    }
}

fn corner_point(corner: u32, w: f32, h: f32) -> (f32, f32, f32) {
    match corner {
        1 => (w, 0.0, FRAC_PI_2),
        2 => (w, h, PI),
        3 => (0.0, h, -FRAC_PI_2),
        _ => (0.0, 0.0, 0.0),
    }
}

fn value_param(effect: &EffectInstance, id: &str) -> Option<ParamValue> {
    effect.param(id).map(|p| p.value.clone()).or_else(|| effect.def().and_then(|d| d.param(id)).map(|p| p.default.clone()))
}

fn number(effect: &EffectInstance, id: &str) -> f32 {
    let value = effect.f64_at(id, Tick::ZERO) as f32;
    if value.is_finite() { value } else { 0.0 }
}

fn choice(effect: &EffectInstance, id: &str) -> u32 {
    match value_param(effect, id) {
        Some(ParamValue::Choice(v)) => v,
        _ => 0,
    }
}

fn color(effect: &EffectInstance) -> [f32; 4] {
    let value = match value_param(effect, "border_color") {
        Some(ParamValue::Color(v)) => v,
        _ => [1.0; 4],
    };
    let linear = |v: f32| filmcraft_color::srgb_to_linear(if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
    [linear(value[0]), linear(value[1]), linear(value[2]), 1.0]
}

fn center(effect: &EffectInstance, scale: f32, w: f32, h: f32) -> (f32, f32) {
    let value = value_param(effect, "center");
    let (x, y) = match value {
        Some(ParamValue::Vec2(v)) => (v.x as f32, v.y as f32),
        _ => (f32::NAN, f32::NAN),
    };
    (if x.is_finite() { x * scale } else { w * 0.5 }, if y.is_finite() { y * scale } else { h * 0.5 })
}

fn field_range(field: Field, w: f32, h: f32, cx: f32, cy: f32, aspect: f32) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for j in 0..=48 {
        for i in 0..=48 {
            let value = field.value(w * i as f32 / 48.0, h * j as f32 / 48.0, w, h, cx, cy, aspect);
            if value.is_finite() {
                lo = lo.min(value);
                hi = hi.max(value);
            }
        }
    }
    if lo.is_finite() && hi.is_finite() { (lo, hi) } else { (0.0, 1.0) }
}

fn uniforms(effect: &EffectInstance, size: (u32, u32), scale: f32, progress: f32) -> Option<[f32; 20]> {
    let field = Field::of(effect)?;
    let (w, h) = (size.0 as f32, size.1 as f32);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let (cx, cy) = center(effect, scale, w, h);
    let aspect = w / h.max(1.0);
    let (lo, hi) = field_range(field, w, h, cx, cy, aspect);
    let aa = [0.5, 1.0, 1.75, 2.5][choice(effect, "antialias").min(3) as usize] * scale.clamp(0.25, 1.0);
    let feather = number(effect, "feather").max(0.0) * scale;
    let border = number(effect, "border_width").max(0.0) * scale;
    let margin = border + feather + 2.0 * aa + 1.0;
    let p = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
    let selector = match field {
        Field::Linear { .. } => 0.0,
        Field::Clock { .. } => 1.0,
        Field::Radial { .. } => 2.0,
        Field::IrisRound => 3.0,
        Field::IrisBox => 4.0,
        Field::IrisDiamond => 5.0,
        Field::IrisCross => 6.0,
        Field::BarnDoors { .. } => 7.0,
        Field::Inset { .. } => 8.0,
        Field::Wipe { .. } => 9.0,
    };
    let (mut sweep, shape) = field.uniforms(cx, cy, aspect);
    sweep[0] = lo;
    sweep[1] = hi;
    sweep[2] = margin;
    let choice = match field {
        Field::Radial { corner } | Field::Inset { corner } => corner,
        Field::BarnDoors { vertical } => u32::from(vertical),
        Field::Wipe { direction } => direction,
        _ => 0,
    };
    let color = color(effect);
    Some([
        w,
        h,
        p,
        selector,
        aa,
        feather,
        border,
        choice as f32,
        color[0],
        color[1],
        color[2],
        color[3],
        sweep[0],
        sweep[1],
        sweep[2],
        sweep[3],
        shape[0],
        shape[1],
        shape[2],
        shape[3],
    ])
}

pub(crate) struct WipeStage {
    pipeline: wgpu::RenderPipeline,
}

impl WipeStage {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("field-wipe"),
            source: wgpu::ShaderSource::Wgsl(include_str!("wipe.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("field-wipe"),
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

    pub(crate) fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &wgpu::TextureView,
        inputs: [&wgpu::TextureView; 2],
        size: (u32, u32),
        effect: &EffectInstance,
        progress: f32,
        scale: f32,
    ) -> bool {
        let Some(values) = uniforms(effect, size, scale, progress) else { return false };
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("field-wipe-params"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("field-wipe"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(inputs[0]) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(inputs[1]) },
                wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("field-wipe") });
        {
            let mut pass = super::accum_pass(&mut encoder, canvas, wgpu::LoadOp::Load);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
        true
    }
}
