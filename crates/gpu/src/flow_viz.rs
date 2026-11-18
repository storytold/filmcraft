//! Optical-flow overlay frames on the GPU (WGSL compute): the counterpart of
//! [`filmcraft_render::flow::encode_flow_viz_cpu`], registered with
//! [`filmcraft_render::flow::register_flow_viz`] by the apps. One pass draws the overlay and
//! encodes it to the `R'G'B'` floats the ProRes encoder takes. Only the flow grid goes up (and the
//! frame itself for Kinetic Motion Glow, which lights the picture). Tested for parity with the CPU
//! path in `flow_viz_tests.rs`.

use filmcraft_render::Image;
use filmcraft_render::flow::{FlowField, FlowVizMode, FlowVizRenderer};
use rayon::prelude::*;

use crate::export_renderer::{device_failed, shared_device};

/// Uniform block size (`Params` in `flow_viz.wgsl`: 12 four-byte fields).
const PARAMS_BYTES: usize = 48;
/// Sub-pixel noise floor below which nothing moves (`render_flow_viz`'s `deadzone`).
const DEADZONE: f32 = 0.25;

/// Buffers for one frame size, kept across frames of a clip.
struct Sized {
    out_w: u32,
    out_h: u32,
    out: wgpu::Buffer,
    read: wgpu::Buffer,
    src: wgpu::Texture,
    src_view: wgpu::TextureView,
    out_bytes: u64,
}

pub struct GpuFlowViz {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    /// The 1×1 texture bound when the mode does not read the frame.
    blank_view: wgpu::TextureView,
    sized: Option<Sized>,
    cells: Option<(wgpu::Buffer, u64)>,
    name: String,
}

impl GpuFlowViz {
    /// Open on the shared off-screen device, or say why there is none.
    pub fn new() -> Result<Self, String> {
        let (_, device, queue, adapter) = shared_device().ok_or_else(|| "no usable GPU".to_string())?;
        Self::with_device(&device, &queue, format!("GPU ({adapter})"))
    }

    /// Open on a given device. A pipeline the device rejects is an error here, caught by an error
    /// scope: through the device's error callback it would turn GPU export off for the session.
    pub fn with_device(device: &wgpu::Device, queue: &wgpu::Queue, name: String) -> Result<Self, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let made = Self::create(device, queue, name);
        match crate::poll_now(scope.pop()) {
            Some(None) => Ok(made),
            Some(Some(e)) => Err(format!("the overlay shader does not run on this GPU: {e}")),
            None => Err("the GPU did not answer".into()),
        }
    }

    fn create(device: &wgpu::Device, queue: &wgpu::Queue, name: String) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-flow-viz"),
            source: wgpu::ShaderSource::Wgsl(include_str!("flow_viz.wgsl").into()),
        });
        let buffer = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flow-viz"),
            entries: &[
                buffer(0, wgpu::BufferBindingType::Uniform),
                buffer(1, wgpu::BufferBindingType::Storage { read_only: true }),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                buffer(3, wgpu::BufferBindingType::Storage { read_only: false }),
            ],
        });
        let pl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("flow-viz"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("flow-viz"),
            layout: Some(&pl),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flow-viz-params"),
            size: PARAMS_BYTES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let blank = src_texture(device, 1, 1);
        let blank_view = blank.create_view(&Default::default());
        Self { device: device.clone(), queue: queue.clone(), pipeline, bgl, params, blank_view, sized: None, cells: None, name }
    }

    /// The output, read-back and frame buffers for a `w × h` frame (reused while the size stays).
    fn sized(&mut self, w: u32, h: u32) -> Result<&Sized, String> {
        if !self.sized.as_ref().is_some_and(|s| (s.out_w, s.out_h) == (w, h)) {
            let limits = self.device.limits();
            let out_bytes = u64::from(w).saturating_mul(u64::from(h)).saturating_mul(12);
            if out_bytes > limits.max_storage_buffer_binding_size || out_bytes > limits.max_buffer_size {
                return Err(format!("a {w}x{h} frame is larger than this GPU's buffers"));
            }
            if w > limits.max_texture_dimension_2d || h > limits.max_texture_dimension_2d {
                return Err(format!("a {w}x{h} frame is larger than this GPU's textures"));
            }
            let buf =
                |label, usage| self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: out_bytes, usage, mapped_at_creation: false });
            let out = buf("flow-viz-out", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC);
            let read = buf("flow-viz-read", wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ);
            let src = src_texture(&self.device, w, h);
            let src_view = src.create_view(&Default::default());
            self.sized = Some(Sized { out_w: w, out_h: h, out, read, src, src_view, out_bytes });
        }
        self.sized.as_ref().ok_or_else(|| "no output buffers".to_string())
    }

    /// The grid-cell buffer, grown when a larger grid arrives.
    fn cells(&mut self, bytes: u64) -> Result<&wgpu::Buffer, String> {
        if !self.cells.as_ref().is_some_and(|(_, have)| *have >= bytes) {
            let limits = self.device.limits();
            if bytes > limits.max_storage_buffer_binding_size || bytes > limits.max_buffer_size {
                return Err("the flow grid is larger than this GPU's buffers".into());
            }
            let b = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flow-viz-cells"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.cells = Some((b, bytes));
        }
        self.cells.as_ref().map(|(b, _)| b).ok_or_else(|| "no grid buffer".to_string())
    }
}

/// An `Rgba32Float` frame texture (read with `textureLoad`, so no filtering is needed).
fn src_texture(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("flow-viz-src"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// The flow grid as `(dx, dy, confidence, 0)` per cell, sampled as `FlowField::sample_px` sees it
/// (missing cells are still, cost becomes confidence; an empty grid is one still cell). Values
/// that are not finite become 0.
fn cell_bytes(field: &FlowField) -> (u32, u32, f32, f32, Vec<u8>) {
    let fin = |v: f32| if v.is_finite() { v } else { 0.0 };
    if field.w == 0 || field.h == 0 || field.frame_w == 0 || field.frame_h == 0 {
        let mut b = Vec::with_capacity(16);
        for v in [0.0f32, 0.0, 1.0, 0.0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        return (1, 1, 1.0, 1.0, b);
    }
    let n = field.w.saturating_mul(field.h);
    let mut b = vec![0u8; n.saturating_mul(16)];
    b.par_chunks_mut(16).enumerate().for_each(|(i, o)| {
        let uv = field.uv.get(i).copied().unwrap_or([0.0; 2]);
        let c = field.cost.get(i).copied().unwrap_or(0);
        let conf = 1.0 - (f32::from(c) / 255.0).clamp(0.0, 1.0);
        for (k, v) in [fin(uv[0]), fin(uv[1]), conf, 0.0].into_iter().enumerate() {
            if let Some(d) = o.get_mut(k * 4..k * 4 + 4) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
    });
    (field.w as u32, field.h as u32, field.frame_w as f32, field.frame_h as f32, b)
}

/// Linear RGBA floats as little-endian bytes (values that are not finite become 0).
fn rgba_bytes(px: &[f32]) -> Vec<u8> {
    let mut b = vec![0u8; px.len().saturating_mul(4)];
    b.par_chunks_mut(1 << 16).zip(px.par_chunks(1 << 14)).for_each(|(o, s)| {
        for (o, v) in o.as_chunks_mut::<4>().0.iter_mut().zip(s) {
            *o = (if v.is_finite() { *v } else { 0.0 }).to_le_bytes();
        }
    });
    b
}

impl FlowVizRenderer for GpuFlowViz {
    fn backend(&self) -> &str {
        &self.name
    }

    fn encode(&mut self, curr: &Image, field: &FlowField, mode: FlowVizMode, max_motion_px: f32) -> Result<Vec<f32>, String> {
        self.draw(curr, field, mode, max_motion_px, false)
    }

    fn linear_detail(&mut self, curr: &Image, field: &FlowField, max_motion_px: f32) -> Result<Vec<f32>, String> {
        self.draw(curr, field, FlowVizMode::DlssEnhance, max_motion_px, true)
    }
}

impl GpuFlowViz {
    fn draw(&mut self, curr: &Image, field: &FlowField, mode: FlowVizMode, max_motion_px: f32, linear: bool) -> Result<Vec<f32>, String> {
        // a frame the device rejects is this renderer's error (the overlay goes on on the CPU),
        // not one for the device's callback, which would turn GPU export off for the session
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let r = self.encode_frame(curr, field, mode, max_motion_px, linear);
        match (crate::poll_now(scope.pop()), r) {
            (Some(None), r) => r,
            (Some(Some(e)), _) => Err(format!("GPU error: {e}")),
            (None, _) => Err("the GPU did not answer".into()),
        }
    }
    fn encode_frame(&mut self, curr: &Image, field: &FlowField, mode: FlowVizMode, max_motion_px: f32, linear: bool) -> Result<Vec<f32>, String> {
        if device_failed() {
            return Err("the GPU device failed".into());
        }
        let (w, h) = (u32::try_from(curr.w).map_err(|e| e.to_string())?, u32::try_from(curr.h).map_err(|e| e.to_string())?);
        if w == 0 || h == 0 {
            return Ok(Vec::new());
        }
        let n = curr.w.saturating_mul(curr.h);
        let needs_src = matches!(mode, FlowVizMode::KineticGlow | FlowVizMode::DlssEnhance);
        if needs_src && curr.px.len() != n.saturating_mul(4) {
            return Err(format!("the {}x{} frame holds {} values", curr.w, curr.h, curr.px.len()));
        }
        let scale = if max_motion_px.is_finite() { max_motion_px.clamp(0.5, 512.0) } else { 16.0 };
        let mode_id: u32 = match mode {
            FlowVizMode::HsvWheel => 0,
            FlowVizMode::MotionVectors => 1,
            FlowVizMode::KineticGlow => 2,
            FlowVizMode::DlssEnhance => 3,
        };
        let (gw, gh, fw, fh, cells) = cell_bytes(field);
        let mut params = Vec::with_capacity(PARAMS_BYTES);
        for v in [w, h, gw, gh] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        for v in [fw, fh] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        for v in [mode_id, u32::from(linear)] {
            params.extend_from_slice(&v.to_le_bytes());
        }
        for v in [scale, DEADZONE, 0.0, 0.0] {
            params.extend_from_slice(&v.to_le_bytes());
        }

        let cells_buf = self.cells(cells.len() as u64)?.clone();
        let (out, read, out_bytes, src) = {
            let s = self.sized(w, h)?;
            (s.out.clone(), s.read.clone(), s.out_bytes, s.src.clone())
        };
        self.queue.write_buffer(&self.params, 0, &params);
        self.queue.write_buffer(&cells_buf, 0, &cells);
        if needs_src {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &src, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &rgba_bytes(&curr.px),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w.saturating_mul(16)), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        let src_view = if needs_src { self.sized.as_ref().map(|s| s.src_view.clone()) } else { None };
        let src_view = src_view.unwrap_or_else(|| self.blank_view.clone());
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flow-viz"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: cells_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&src_view) },
                wgpu::BindGroupEntry { binding: 3, resource: out.as_entire_binding() },
            ],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("flow-viz") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("flow-viz"), timestamp_writes: None });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(w.div_ceil(16), h.div_ceil(16), 1);
        }
        enc.copy_buffer_to_buffer(&out, 0, &read, 0, out_bytes);
        self.queue.submit([enc.finish()]);
        let slice = read.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        crate::wait_mapped(&self.device, &rx).ok_or_else(|| "the GPU did not finish the frame in time".to_string())?;
        let mut values = vec![0.0f32; n.saturating_mul(3)];
        {
            let data = slice.get_mapped_range().map_err(|e| e.to_string())?;
            // each chunk is copied out of mapped memory in one go first (it can be uncached), then
            // converted
            values.par_chunks_mut(1 << 14).zip(data.par_chunks(1 << 16)).for_each(|(o, b)| {
                let mut line = Vec::new();
                crate::copy_mapped(b, &mut line);
                for (o, b) in o.iter_mut().zip(line.as_chunks::<4>().0) {
                    *o = f32::from_le_bytes(*b);
                }
            });
        }
        read.unmap();
        // validation and out-of-memory errors arrive through the device's error callback
        if device_failed() {
            return Err("the GPU device failed".into());
        }
        Ok(values)
    }
}

/// The factory the apps register with [`filmcraft_render::flow::register_flow_viz`].
pub fn factory() -> Result<Box<dyn FlowVizRenderer>, String> {
    GpuFlowViz::new().map(|g| Box::new(g) as Box<dyn FlowVizRenderer>)
}
