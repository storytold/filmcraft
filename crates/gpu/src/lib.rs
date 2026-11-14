//! The wgpu compositor.
//!
//! Executes a [`FramePlan`](filmcraft_render::plan::FramePlan) on the GPU: every layer's source is
//! uploaded as textures (Y/Cb/Cr planes stay YUV — conversion to linear RGB happens per pixel in
//! the shader), drawn as a transformed quad with premultiplied "over" blending into a Rgba16Float
//! accumulator, then resolved over black into an sRGB `Rgba8UnormSrgb` texture that the UI
//! registers as a native texture. Uploads are cached by pixel-buffer identity, so a paused frame or
//! a still costs nothing.
//!
//! Every blend mode of [`filmcraft_render::Blend`] runs here. Normal and Dissolve (whose per-pixel
//! pattern is the CPU's 64-bit hash, reproduced in WGSL) use fixed-function "over" blending. The
//! other 25 modes need the colour under the layer: before such a layer is drawn, the accumulator
//! region under its quad is copied into a backdrop texture, and the fragment shader composites
//! with the CPU reference's math (`filmcraft_render::blend::composite`: sRGB-encoded straight
//! colour, W3C formulas, back to linear) and writes the result. Only those layers pay the copy.
//!
//! A layer carrying standard effects ([`filmcraft_render::plan::LayerFx`]) goes through the
//! effect stage ([`fx`]) first: its source is drawn into an `Rgba32Float` working image, the
//! effects run as compute passes in WGSL with the CPU reference's math
//! (`filmcraft_render::gpufx::FxOp::apply`), and the result is drawn like any RGBA layer. Layers
//! without effects take the single draw above.
//!
//! Layers that need converting before upload (linear f32 RGBA from CPU-rendered layers, 16-bit
//! YUV such as ProRes) are converted to half floats by [`prepare`], which frame workers run off the
//! UI thread; [`GpuCompositor::composite_prepared`] then only copies bytes into textures.
//!
//! The CPU plan executor (`filmcraft_render::plan::execute_cpu`) is the oracle; tests compare.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;

use filmcraft_color::{Matrix, Range, Transfer};
use filmcraft_frame::{Chroma, PixelData, VideoFrame};
use filmcraft_render::Blend;
use filmcraft_render::plan::{DeviceCaps, FramePlan, PlanLayer, PlanStep};

pub mod adapter;
pub mod export_renderer;
pub mod fx;
pub mod lut;
pub mod mask;
mod transition;
pub use export_renderer::ExportRenderer;
pub use lut::GpuLut;
pub use mask::GpuMask;

/// Output texture format: gamma-encoded RGBA8 (what egui expects of native textures); the resolve
/// shader applies the sRGB encoding.
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

const UPLOAD_CACHE_BUDGET_BYTES: u64 = 512 * 1024 * 1024;
const UPLOAD_CACHE_MAX_ENTRIES: usize = 256;
const TEXTURE_POOL_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) struct UniformPool {
    bufs: Vec<wgpu::Buffer>,
    cursor: usize,
    size: u64,
    label: &'static str,
}

impl UniformPool {
    pub(crate) fn new(label: &'static str, size: u64) -> Self {
        Self { bufs: Vec::new(), cursor: 0, size, label }
    }

    pub(crate) fn begin_frame(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, bytes: &[u8]) -> &wgpu::Buffer {
        if self.cursor >= self.bufs.len() {
            self.bufs.push(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: self.size.max(bytes.len() as u64),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let idx = self.cursor;
        self.cursor += 1;
        let buf = &self.bufs[idx];
        queue.write_buffer(buf, 0, bytes);
        buf
    }
}

struct PooledTexture {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    bytes: u64,
}

struct Uploaded {
    /// Y, Cb, Cr (or the RGBA texture) and the alpha plane (the dummy texture when there is none).
    views: [wgpu::TextureView; 4],
    planes: Vec<PooledTexture>,
    #[expect(dead_code, reason = "keeps externally imported HAL textures alive alongside views")]
    owned_textures: Vec<wgpu::Texture>,
    gpu_bytes: u64,
    kind: u32,
    code_scale: f32,
    /// Multiplier from the alpha texture's sample to alpha 0..1; 0 when the frame has no alpha plane.
    alpha_scale: f32,
    chroma: (u32, u32),
    last_used: u64,
    /// The uploaded pixel buffers, kept alive while cached: the cache key is the buffer address,
    /// and a freed buffer's address can be reused by a different frame (stale texture).
    _pixels: PixelData,
}

pub struct GpuCompositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layer_pipeline: wgpu::RenderPipeline,
    /// Blend modes that read the destination: `fs_blend`, no fixed-function blending.
    blend_pipeline: wgpu::RenderPipeline,
    final_pipeline: wgpu::RenderPipeline,
    layer_bgl: wgpu::BindGroupLayout,
    blend_bgl: wgpu::BindGroupLayout,
    final_bgl: wgpu::BindGroupLayout,
    accum: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    /// Copy of the accumulator under a blend-mode layer (allocated on first use).
    backdrop: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    output: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    accum_readback_buf: Option<(wgpu::Buffer, u64)>,
    uploads: HashMap<(usize, u32, u32), Uploaded>,
    upload_cache_bytes: u64,
    texture_pool: HashMap<(u32, u32, wgpu::TextureFormat), Vec<PooledTexture>>,
    texture_pool_bytes: u64,
    uniform_pool: UniformPool,
    frame_depth: u32,
    clock: u64,
    dummy: wgpu::TextureView,
    /// The effect stage (None on devices without compute shaders or storage textures: layers
    /// with effects are then rendered on the CPU here).
    fx: Option<fx::FxStage>,
    transition: transition::PushStage,
    transition_inputs: [Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>; 2],
    pub gpu_transitions: u64,
    pub cpu_transitions: u64,
    /// Adjustment layers ([`PlanStep::Adjust`]) run by the effect stage, and frames with one that
    /// were composited on the CPU here instead: a plan made for another device ([`Self::caps`]), or
    /// the effect stage declining it. Planning with this device's caps keeps the second at zero.
    pub gpu_adjustments: u64,
    pub cpu_adjustments: u64,
    /// Total bytes uploaded (stats).
    pub uploaded_bytes: u64,
    r16_unorm: bool,
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

/// f32 → IEEE half (round to nearest even), no dependency needed.
pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let mant = x & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if mant != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = (mant | 0x80_0000) >> (1 - e);
        let round = (m >> 12) & 1;
        return sign | (((m >> 13) + round) as u16);
    }
    let m = mant >> 13;
    let round_bits = mant & 0x1fff;
    let mut h = (sign as u32) | ((e as u32) << 10) | m;
    if round_bits > 0x1000 || (round_bits == 0x1000 && (m & 1) == 1) {
        h += 1;
    }
    h as u16
}

/// IEEE half → f32 (the inverse of [`f32_to_f16`]), for reading `Rgba16Float` textures back.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let mant = (h & 0x03ff) as u32;
    let bits = match exp {
        0 => {
            if mant == 0 {
                sign
            } else {
                // Half subnormal: value = mant × 2⁻²⁴ = (1 + frac) × 2^(b−24) with b the mantissa's
                // highest set bit — renormalise into an f32.
                let b = 31 - mant.leading_zeros();
                let frac = (mant ^ (1 << b)) << (23 - b);
                sign | ((103 + b) << 23) | frac
            }
        }
        0x1f => sign | 0x7f80_0000 | (mant << 13),
        _ => sign | ((exp + 112) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

/// [`f16_to_f32`] with a branch-light path for normal numbers and zeros (the bulk of read-back
/// data); subnormals, infinities and NaNs take the general path. Same result for every input.
#[inline(always)]
pub fn f16_to_f32_fast(h: u16) -> f32 {
    let u = u32::from(h);
    let sign = (u & 0x8000) << 16;
    let abs = u & 0x7fff;
    if (0x0400..0x7c00).contains(&abs) {
        // rebias the exponent (127 - 15 = 112) and widen the mantissa
        f32::from_bits(sign | ((abs + (112 << 10)) << 13))
    } else if abs == 0 {
        f32::from_bits(sign)
    } else {
        f16_to_f32(h)
    }
}

/// Half-float texel data for one frame (one byte vector per plane), converted off the UI thread.
/// 8-bit frames need no conversion and have none.
#[derive(Clone, Debug, Default)]
pub struct Prepared {
    planes: Vec<Vec<u8>>,
}

impl Prepared {
    pub fn bytes(&self) -> usize {
        self.planes.iter().map(Vec::len).sum()
    }
}

/// [`Prepared`] data for every layer of a plan (index-aligned with its layers), or for the
/// fallback image of a [`FramePlan::Image`].
#[derive(Clone, Debug, Default)]
pub struct PreparedPlan {
    layers: Vec<Option<Prepared>>,
    image: Option<Prepared>,
    children: Vec<PreparedPlan>,
}

impl PreparedPlan {
    pub fn bytes(&self) -> usize {
        self.layers.iter().flatten().chain(&self.image).map(Prepared::bytes).sum::<usize>() + self.children.iter().map(Self::bytes).sum::<usize>()
    }
}

/// Values per parallel chunk when converting.
const CHUNK: usize = 1 << 15;

fn f32_to_f16_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = vec![0u8; v.len() * 2];
    out.par_chunks_mut(CHUNK * 2).zip(v.par_chunks(CHUNK)).for_each(|(o, s)| {
        for (o, x) in o.as_chunks_mut::<2>().0.iter_mut().zip(s) {
            o.copy_from_slice(&f32_to_f16(*x).to_le_bytes());
        }
    });
    out
}

/// Half floats of `code / 2^bits` for every 16-bit code (built once per bit depth).
fn code_table(bits: u32) -> &'static [[u8; 2]] {
    static TABLES: [std::sync::OnceLock<Vec<[u8; 2]>>; 17] = [const { std::sync::OnceLock::new() }; 17];
    let bits = bits.min(16);
    TABLES[bits as usize].get_or_init(|| {
        let scale = (1u32 << bits) as f32;
        (0..=u16::MAX as u32).map(|c| f32_to_f16(c as f32 / scale).to_le_bytes()).collect()
    })
}

/// `code / 2^bits` as half floats, through a per-code table (same values as converting each sample).
fn codes_to_f16_bytes(v: &[u16], bits: u32) -> Vec<u8> {
    let table = code_table(bits);
    let mut out = vec![0u8; v.len() * 2];
    out.par_chunks_mut(CHUNK * 2).zip(v.par_chunks(CHUNK)).for_each(|(o, s)| {
        for (o, c) in o.as_chunks_mut::<2>().0.iter_mut().zip(s) {
            o.copy_from_slice(&table[*c as usize]);
        }
    });
    out
}

/// Externally imported zero-copy planes for a [`filmcraft_frame::GpuSurface`].
pub struct ImportedPlanes {
    pub views: [wgpu::TextureView; 4],
    pub textures: Vec<wgpu::Texture>,
    pub gpu_bytes: u64,
    pub kind: u32,
    pub code_scale: f32,
    pub chroma: (u32, u32),
}

/// Platform hook importing a [`filmcraft_frame::GpuSurface`] into a `wgpu::Device` without CPU copies.
pub type GpuSurfaceImporter = fn(&wgpu::Device, &wgpu::TextureView, &dyn filmcraft_frame::GpuSurface, u32, u32) -> Option<ImportedPlanes>;

static SURFACE_IMPORTER: std::sync::OnceLock<GpuSurfaceImporter> = std::sync::OnceLock::new();

/// Register the platform zero-copy importer once at startup (`filmcraft_platform::register`).
pub fn register_gpu_surface_importer(importer: GpuSurfaceImporter) {
    let _ = SURFACE_IMPORTER.set(importer);
}

/// Optional wgpu device features that accelerate FilmCraft's GPU compositor when supported by
/// `adapter` (safe to bitwise-OR into `DeviceDescriptor::required_features`).
pub fn requested_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features() & (wgpu::Features::TEXTURE_FORMAT_16BIT_NORM | wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32)
}

/// Convert a frame's texels for upload (None when it uploads as it is).
pub fn prepare_frame(f: &VideoFrame) -> Option<Prepared> {
    prepare_frame_for(f, false)
}

/// [`prepare_frame`] tuned for a device: when `r16_unorm` is true, 16-bit YUV planes upload
/// directly as `R16Unorm` without any CPU float conversion.
pub fn prepare_frame_for(f: &VideoFrame, r16_unorm: bool) -> Option<Prepared> {
    match &f.data {
        PixelData::RgbaF32(d) => Some(Prepared { planes: vec![f32_to_f16_bytes(d)] }),
        PixelData::Yuv16 { .. } if r16_unorm && cfg!(target_endian = "little") => None,
        PixelData::Yuv16 { planes, bits, alpha, .. } => {
            Some(Prepared { planes: planes.iter().chain(alpha.as_ref()).map(|p| codes_to_f16_bytes(p, *bits)).collect() })
        }
        PixelData::Rgba8(_) | PixelData::Yuv8 { .. } | PixelData::GpuSurface(_) => None,
    }
}

/// Convert every layer of a plan for upload. Thread-safe and GPU-free: run it on a worker.
pub fn prepare(plan: &FramePlan) -> PreparedPlan {
    prepare_for(plan, false)
}

/// [`prepare`] tuned for a device that may support native 16-bit normalized textures (`R16Unorm`).
pub fn prepare_for(plan: &FramePlan, r16_unorm: bool) -> PreparedPlan {
    match plan {
        FramePlan::Layers { layers, .. } => prepare_layers(layers, r16_unorm),
        FramePlan::Image(img) => PreparedPlan { image: Some(Prepared { planes: vec![f32_to_f16_bytes(&img.px)] }), ..Default::default() },
        FramePlan::Composite { steps, .. } => PreparedPlan {
            children: steps
                .iter()
                .flat_map(|s| match s {
                    PlanStep::Layers(layers) => vec![prepare_layers(layers, r16_unorm)],
                    PlanStep::Transition { inputs, .. } => inputs.iter().map(|ls| prepare_layers(ls, r16_unorm)).collect(),
                    PlanStep::Adjust { .. } => Vec::new(),
                })
                .collect(),
            ..Default::default()
        },
    }
}

fn prepare_layers(layers: &[PlanLayer], r16_unorm: bool) -> PreparedPlan {
    PreparedPlan { layers: layers.iter().map(|l| prepare_frame_for(&l.frame, r16_unorm)).collect(), ..Default::default() }
}

impl GpuCompositor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
        });
        let layer_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                tex_entry(3),
                tex_entry(5),
            ],
        });
        let blend_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blend-layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                tex_entry(3),
                tex_entry(4),
                tex_entry(5),
            ],
        });
        let final_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("final"),
            entries: &[wgpu::BindGroupLayoutEntry { binding: 0, ..tex_entry(0) }],
        });
        let pl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("layer"), bind_group_layouts: &[Some(&layer_bgl)], immediate_size: 0 });
        let layer_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("layer"),
            layout: Some(&pl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: ACCUM_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let bpl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blend-layer"),
            bind_group_layouts: &[Some(&blend_bgl)],
            immediate_size: 0,
        });
        let blend_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blend-layer"),
            layout: Some(&bpl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_blend"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: ACCUM_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let fpl =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("final"), bind_group_layouts: &[Some(&final_bgl)], immediate_size: 0 });
        let final_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("final"),
            layout: Some(&fpl),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_full"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_full"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: OUTPUT_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dummy"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy = dummy_tex.create_view(&Default::default());
        // The effect stage's pipelines can still be invalid on a device that passes the limits
        // check (Rgba32Float is not renderable everywhere, e.g. software Vulkan/GL). Validation
        // errors would panic, so they are captured: without the stage, effects render on the CPU.
        let fx = fx::FxStage::supported(device)
            .then(|| {
                let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
                let stage = fx::FxStage::new(device, &shader, &layer_bgl);
                match poll_now(scope.pop()) {
                    Some(None) => Some(stage),
                    Some(Some(e)) => {
                        log::warn!("GPU effects unavailable on this device, rendering them on the CPU: {e}");
                        None
                    }
                    None => None,
                }
            })
            .flatten();
        Self {
            device: device.clone(),
            queue: queue.clone(),
            layer_pipeline,
            blend_pipeline,
            final_pipeline,
            layer_bgl,
            blend_bgl,
            final_bgl,
            accum: None,
            backdrop: None,
            output: None,
            accum_readback_buf: None,
            uploads: HashMap::new(),
            upload_cache_bytes: 0,
            texture_pool: HashMap::new(),
            texture_pool_bytes: 0,
            uniform_pool: UniformPool::new("layer-u", (UNIFORM_FLOATS * 4) as u64),
            frame_depth: 0,
            clock: 0,
            dummy,
            fx,
            transition: transition::PushStage::new(device),
            transition_inputs: [None, None],
            gpu_transitions: 0,
            cpu_transitions: 0,
            gpu_adjustments: 0,
            cpu_adjustments: 0,
            uploaded_bytes: 0,
            r16_unorm: device.features().contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM) && cfg!(target_endian = "little"),
        }
    }

    fn begin_frame(&mut self) {
        if self.frame_depth == 0 {
            self.uniform_pool.begin_frame();
            self.transition.begin_frame();
        }
        self.frame_depth = self.frame_depth.saturating_add(1);
    }

    fn end_frame(&mut self) {
        self.frame_depth = self.frame_depth.saturating_sub(1);
    }

    /// Whether this device supports uploading 10/12/16-bit YUV planes natively as `R16Unorm`.
    pub fn supports_r16_unorm(&self) -> bool {
        self.r16_unorm
    }

    fn target(
        device: &wgpu::Device,
        slot: &mut Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
        w: u32,
        h: u32,
        format: wgpu::TextureFormat,
        extra: wgpu::TextureUsages,
    ) -> wgpu::TextureView {
        if let Some(s) = slot.as_ref().filter(|s| s.2 == (w, h)) {
            return s.1.clone();
        }
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("filmcraft-target"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | extra,
            view_formats: &[],
        });
        let v = t.create_view(&Default::default());
        *slot = Some((t, v.clone(), (w, h)));
        v
    }

    fn plane_texture(&mut self, w: u32, h: u32, format: wgpu::TextureFormat, bytes: &[u8], bpp: u32) -> PooledTexture {
        let gpu_bytes = u64::from(w) * u64::from(h) * u64::from(bpp);
        let pooled = self.texture_pool.get_mut(&(w, h, format)).and_then(Vec::pop);
        let entry = match pooled {
            Some(p) => {
                self.texture_pool_bytes = self.texture_pool_bytes.saturating_sub(p.bytes);
                p
            }
            None => {
                let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("filmcraft-plane"),
                    size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = tex.create_view(&Default::default());
                PooledTexture { tex, view, w, h, format, bytes: gpu_bytes }
            }
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &entry.tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bpp), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.uploaded_bytes += bytes.len() as u64;
        filmcraft_frame::transfers::record_compositor_upload(bytes.len() as u64);
        entry
    }

    fn recycle_uploaded(&mut self, up: Uploaded) {
        self.upload_cache_bytes = self.upload_cache_bytes.saturating_sub(up.gpu_bytes);
        for p in up.planes {
            if self.texture_pool_bytes.saturating_add(p.bytes) <= TEXTURE_POOL_BUDGET_BYTES {
                self.texture_pool_bytes += p.bytes;
                self.texture_pool.entry((p.w, p.h, p.format)).or_default().push(p);
            }
        }
    }

    /// Upload (or reuse) the textures of a frame, using `prep` when it was converted beforehand.
    fn upload(&mut self, f: &VideoFrame, prep: Option<&Prepared>) -> (usize, u32, u32) {
        let id = match &f.data {
            PixelData::Rgba8(d) => Arc::as_ptr(d) as *const u8 as usize,
            PixelData::RgbaF32(d) => Arc::as_ptr(d) as *const u8 as usize,
            PixelData::Yuv8 { planes, .. } => Arc::as_ptr(&planes[0]) as *const u8 as usize,
            PixelData::Yuv16 { planes, .. } => Arc::as_ptr(&planes[0]) as *const u8 as usize,
            PixelData::GpuSurface(s) => s.id(),
        };
        let key = (id, f.width, f.height);
        self.clock += 1;
        if let Some(u) = self.uploads.get_mut(&key) {
            u.last_used = self.clock;
            return key;
        }
        let up = match &f.data {
            PixelData::GpuSurface(surf) => self.upload_gpu_surface(f, surf),
            _ => self.upload_cpu_data(f.width, f.height, &f.data, &f.data, prep),
        };
        self.upload_cache_bytes = self.upload_cache_bytes.saturating_add(up.gpu_bytes);
        if let Some(prev) = self.uploads.insert(key, up) {
            self.recycle_uploaded(prev);
        }
        // Keep recent uploads up to the VRAM budget and entry cap, recycling evicted textures.
        if self.uploads.len() > UPLOAD_CACHE_MAX_ENTRIES || self.upload_cache_bytes > UPLOAD_CACHE_BUDGET_BYTES {
            let mut v: Vec<(u64, (usize, u32, u32))> = self.uploads.iter().map(|(k, u)| (u.last_used, *k)).collect();
            v.sort_unstable();
            for (_, k) in v {
                if self.uploads.len() <= UPLOAD_CACHE_MAX_ENTRIES && self.upload_cache_bytes <= UPLOAD_CACHE_BUDGET_BYTES {
                    break;
                }
                if k == key {
                    continue;
                }
                if let Some(evicted) = self.uploads.remove(&k) {
                    self.recycle_uploaded(evicted);
                }
            }
        }
        key
    }

    fn upload_gpu_surface(&mut self, f: &VideoFrame, surf: &Arc<dyn filmcraft_frame::GpuSurface>) -> Uploaded {
        if let Some(importer) = SURFACE_IMPORTER.get()
            && let Some(imported) = importer(&self.device, &self.dummy, surf.as_ref(), f.width, f.height)
        {
            return Uploaded {
                views: imported.views,
                planes: Vec::new(),
                owned_textures: imported.textures,
                gpu_bytes: imported.gpu_bytes,
                kind: imported.kind,
                code_scale: imported.code_scale,
                alpha_scale: 0.0,
                chroma: imported.chroma,
                last_used: self.clock,
                _pixels: f.data.clone(),
            };
        }
        match surf.to_cpu() {
            Ok(cpu_data) => self.upload_cpu_data(f.width, f.height, &cpu_data, &f.data, None),
            Err(why) => {
                log::warn!("GPU surface fallback readback failed: {why}");
                let blank = PixelData::Rgba8(Arc::new(vec![0u8; (f.width as usize).saturating_mul(f.height as usize).saturating_mul(4)]));
                self.upload_cpu_data(f.width, f.height, &blank, &f.data, None)
            }
        }
    }

    fn upload_cpu_data(&mut self, w: u32, h: u32, data: &PixelData, keep: &PixelData, prep: Option<&Prepared>) -> Uploaded {
        let d0 = self.dummy.clone();
        let dummy = || d0.clone();
        match data {
            PixelData::Rgba8(d) => {
                let p0 = self.plane_texture(w, h, wgpu::TextureFormat::Rgba8UnormSrgb, d, 4);
                let v = p0.view.clone();
                let gpu_bytes = p0.bytes;
                Uploaded {
                    views: [v, dummy(), dummy(), dummy()],
                    planes: vec![p0],
                    owned_textures: Vec::new(),
                    gpu_bytes,
                    kind: 0,
                    code_scale: 1.0,
                    alpha_scale: 0.0,
                    chroma: (w, h),
                    last_used: self.clock,
                    _pixels: keep.clone(),
                }
            }
            PixelData::RgbaF32(d) => {
                let owned;
                let half = match prep {
                    Some(p) => &p.planes[0],
                    None => {
                        owned = f32_to_f16_bytes(d);
                        &owned
                    }
                };
                let p0 = self.plane_texture(w, h, wgpu::TextureFormat::Rgba16Float, half, 8);
                let v = p0.view.clone();
                let gpu_bytes = p0.bytes;
                Uploaded {
                    views: [v, dummy(), dummy(), dummy()],
                    planes: vec![p0],
                    owned_textures: Vec::new(),
                    gpu_bytes,
                    kind: 1,
                    code_scale: 1.0,
                    alpha_scale: 0.0,
                    chroma: (w, h),
                    last_used: self.clock,
                    _pixels: keep.clone(),
                }
            }
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let py = self.plane_texture(w, h, wgpu::TextureFormat::R8Unorm, &planes[0], 1);
                let pu = self.plane_texture(cw, ch, wgpu::TextureFormat::R8Unorm, &planes[1], 1);
                let pv = self.plane_texture(cw, ch, wgpu::TextureFormat::R8Unorm, &planes[2], 1);
                let pa = alpha.as_ref().map(|a| self.plane_texture(w, h, wgpu::TextureFormat::R8Unorm, a, 1));
                let y = py.view.clone();
                let u = pu.view.clone();
                let v = pv.view.clone();
                let a = pa.as_ref().map(|p| p.view.clone());
                let alpha_scale = if a.is_some() { 1.0 } else { 0.0 };
                let mut owned_planes = Vec::with_capacity(4);
                owned_planes.push(py);
                owned_planes.push(pu);
                owned_planes.push(pv);
                if let Some(pa) = pa {
                    owned_planes.push(pa);
                }
                let gpu_bytes = owned_planes.iter().map(|p| p.bytes).sum();
                Uploaded {
                    views: [y, u, v, a.unwrap_or_else(dummy)],
                    planes: owned_planes,
                    owned_textures: Vec::new(),
                    gpu_bytes,
                    kind: 2,
                    code_scale: 255.0,
                    alpha_scale,
                    chroma: (cw, ch),
                    last_used: self.clock,
                    _pixels: keep.clone(),
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let _ = Chroma::C420;
                if self.r16_unorm {
                    let py = self.plane_texture(w, h, wgpu::TextureFormat::R16Unorm, bytemuck::cast_slice(&planes[0]), 2);
                    let pu = self.plane_texture(cw, ch, wgpu::TextureFormat::R16Unorm, bytemuck::cast_slice(&planes[1]), 2);
                    let pv = self.plane_texture(cw, ch, wgpu::TextureFormat::R16Unorm, bytemuck::cast_slice(&planes[2]), 2);
                    let pa = alpha.as_ref().map(|a| self.plane_texture(w, h, wgpu::TextureFormat::R16Unorm, bytemuck::cast_slice(a), 2));
                    let y = py.view.clone();
                    let u = pu.view.clone();
                    let v = pv.view.clone();
                    let a = pa.as_ref().map(|p| p.view.clone());
                    let max_code = ((1u32 << bits) - 1).max(1) as f32;
                    let alpha_scale = if a.is_some() { 65535.0 / max_code } else { 0.0 };
                    let mut owned_planes = Vec::with_capacity(4);
                    owned_planes.push(py);
                    owned_planes.push(pu);
                    owned_planes.push(pv);
                    if let Some(pa) = pa {
                        owned_planes.push(pa);
                    }
                    let gpu_bytes = owned_planes.iter().map(|p| p.bytes).sum();
                    Uploaded {
                        views: [y, u, v, a.unwrap_or_else(dummy)],
                        planes: owned_planes,
                        owned_textures: Vec::new(),
                        gpu_bytes,
                        kind: 2,
                        code_scale: 65535.0,
                        alpha_scale,
                        chroma: (cw, ch),
                        last_used: self.clock,
                        _pixels: keep.clone(),
                    }
                } else {
                    let scale = (1u32 << bits) as f32;
                    let owned;
                    let p = match prep {
                        Some(p) => p,
                        None => {
                            owned = Prepared { planes: planes.iter().chain(alpha.as_ref()).map(|p| codes_to_f16_bytes(p, *bits)).collect() };
                            &owned
                        }
                    };
                    let py = self.plane_texture(w, h, wgpu::TextureFormat::R16Float, &p.planes[0], 2);
                    let pu = self.plane_texture(cw, ch, wgpu::TextureFormat::R16Float, &p.planes[1], 2);
                    let pv = self.plane_texture(cw, ch, wgpu::TextureFormat::R16Float, &p.planes[2], 2);
                    // (full-resolution alpha plane: its codes span 0..2^bits - 1, hence the scale)
                    let pa = alpha.as_ref().and_then(|_| p.planes.get(3)).map(|b| self.plane_texture(w, h, wgpu::TextureFormat::R16Float, b, 2));
                    let y = py.view.clone();
                    let u = pu.view.clone();
                    let v = pv.view.clone();
                    let a = pa.as_ref().map(|p| p.view.clone());
                    let alpha_scale = if a.is_some() { scale / (scale - 1.0) } else { 0.0 };
                    let mut owned_planes = Vec::with_capacity(4);
                    owned_planes.push(py);
                    owned_planes.push(pu);
                    owned_planes.push(pv);
                    if let Some(pa) = pa {
                        owned_planes.push(pa);
                    }
                    let gpu_bytes = owned_planes.iter().map(|p| p.bytes).sum();
                    Uploaded {
                        views: [y, u, v, a.unwrap_or_else(dummy)],
                        planes: owned_planes,
                        owned_textures: Vec::new(),
                        gpu_bytes,
                        kind: 2,
                        code_scale: scale,
                        alpha_scale,
                        chroma: (cw, ch),
                        last_used: self.clock,
                        _pixels: keep.clone(),
                    }
                }
            }
            PixelData::GpuSurface(_) => Uploaded {
                views: [dummy(), dummy(), dummy(), dummy()],
                planes: Vec::new(),
                owned_textures: Vec::new(),
                gpu_bytes: 0,
                kind: 1,
                code_scale: 1.0,
                alpha_scale: 0.0,
                chroma: (w, h),
                last_used: self.clock,
                _pixels: keep.clone(),
            },
        }
    }

    /// The source description of an uploaded frame.
    fn src_info(&self, f: &VideoFrame, key: (usize, u32, u32)) -> Option<SrcInfo> {
        let up = self.uploads.get(&key)?;
        let code_levels = match &f.data {
            PixelData::Yuv8 { .. } => 256.0,
            PixelData::Yuv16 { bits, .. } => (*bits as f32).exp2(),
            PixelData::GpuSurface(s) => (s.bits() as f32).exp2(),
            _ => 1.0,
        };
        Some(SrcInfo {
            kind: up.kind,
            code_scale: up.code_scale,
            code_levels,
            alpha: up.alpha_scale,
            chroma: up.chroma,
            size: (f.width, f.height),
            color: f.color,
            input: None,
        })
    }

    /// The uniforms drawing `frame` onto `fx`'s working image: box-decimated, then turned.
    fn fx_source_uniforms(src: &SrcInfo, frame: &VideoFrame, fx: &filmcraft_render::plan::LayerFx) -> [f32; UNIFORM_FLOATS] {
        let n = fx.decimation.max(1) as f64;
        let m = filmcraft_geom::Affine::scale(1.0 / n, 1.0 / n).then_apply(&filmcraft_render::plan::quarter_turns(
            fx.turns,
            frame.width as f64,
            frame.height as f64,
        ));
        let mut su = Self::uniforms(src, &m, 1.0, Blend::Normal, (fx.size.0.max(1), fx.size.1.max(1)));
        // Effect sources use the working pixel center, avoiding interpolated vertex coordinates.
        su[26] = n as f32;
        su[27] = (fx.turns % 4) as f32;
        su
    }

    fn uniforms(src: &SrcInfo, m: &filmcraft_geom::Affine, opacity: f32, blend: Blend, out: (u32, u32)) -> [f32; UNIFORM_FLOATS] {
        let (kr, kb) = src.color.matrix.kr_kb();
        let bits_scale = src.code_levels / 256.0; // 2^(bits - 8), independent of texture encoding
        let (yo, ys, co, cs) = match (src.color.range, src.kind) {
            (Range::Limited, 2 | 3) => (16.0 * bits_scale, 219.0 * bits_scale, 128.0 * bits_scale, 224.0 * bits_scale),
            (Range::Full, 2 | 3) => (0.0, src.code_levels - 1.0, src.code_levels / 2.0, src.code_levels - 1.0),
            _ => (0.0, 1.0, 0.0, 1.0),
        };
        let transfer = match src.color.transfer {
            Transfer::Linear => 1.0,
            Transfer::Pq => 2.0,
            Transfer::Hlg => 3.0,
            _ => 0.0,
        };
        // source pixels per output pixel → supersampling taps
        let sx = (m.a * m.a + m.b * m.b).sqrt();
        let sy = (m.c * m.c + m.d * m.d).sqrt();
        let footprint = (1.0 / sx.min(sy).max(1e-6)) as f32;
        let taps = if footprint > 1.25 { footprint.ceil().min(4.0) } else { 1.0 };
        let _ = Matrix::Bt709;
        // colour management (q0..q5), stage by stage as `InputTransform::apply`
        let q = src.input.map_or([0.0; 24], |p| {
            let curve = match p.curve {
                filmcraft_color::GpuCurve::Sdr => 0.0,
                filmcraft_color::GpuCurve::Linear => 1.0,
                filmcraft_color::GpuCurve::Pq => 2.0,
                filmcraft_color::GpuCurve::Hlg => 3.0,
            };
            let (hl, hk) = p.hlg.unwrap_or(([0.0; 3], 0.0));
            let mx = p.matrix.unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
            let (sp, dp) = p.tone.unwrap_or((0.0, 0.0));
            let dl = p.desaturate.unwrap_or([0.0; 3]);
            let on = |b: bool| if b { 1.0 } else { 0.0 };
            #[rustfmt::skip]
            let q = [
                1.0, curve, hk, filmcraft_color::REFERENCE_WHITE_NITS as f32,
                hl[0], hl[1], hl[2], on(p.matrix.is_some()),
                mx[0][0], mx[0][1], mx[0][2], on(p.tone.is_some()),
                mx[1][0], mx[1][1], mx[1][2], sp,
                mx[2][0], mx[2][1], mx[2][2], dp,
                dl[0], dl[1], dl[2], on(p.desaturate.is_some()),
            ];
            q
        });
        let base = [
            m.a as f32,
            m.b as f32,
            m.c as f32,
            m.d as f32,
            m.e as f32,
            m.f as f32,
            out.0 as f32,
            out.1 as f32,
            src.size.0 as f32,
            src.size.1 as f32,
            src.chroma.0 as f32,
            src.chroma.1 as f32,
            opacity,
            src.kind as f32,
            taps,
            transfer,
            yo,
            ys,
            co,
            cs,
            kr,
            kb,
            src.code_scale,
            footprint.max(1.0),
            blend.index() as f32,
            src.alpha,
            0.0,
            0.0,
        ];
        let mut u = [0.0; UNIFORM_FLOATS];
        u[..28].copy_from_slice(&base);
        u[28..].copy_from_slice(&q);
        u
    }

    fn write_uniform<'a>(device: &wgpu::Device, queue: &wgpu::Queue, pool: &'a mut UniformPool, u: &[f32; UNIFORM_FLOATS]) -> &'a wgpu::Buffer {
        let mut bytes = [0u8; UNIFORM_FLOATS * 4];
        for (dst, v) in bytes.chunks_exact_mut(4).zip(u) {
            dst.copy_from_slice(&v.to_le_bytes());
        }
        pool.write(device, queue, &bytes)
    }

    /// Composite a plan; returns the output view (sRGB, over black) and its size.
    pub fn composite(&mut self, plan: &FramePlan) -> (wgpu::TextureView, (u32, u32)) {
        self.composite_prepared(plan, None)
    }

    /// [`composite`](Self::composite) with texel conversions already done by [`prepare`] (the
    /// result is identical; only the upload work on this thread differs).
    pub fn composite_prepared(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>) -> (wgpu::TextureView, (u32, u32)) {
        self.render_plan(plan, prep, true, true)
    }

    fn render_plan(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>, clear: bool, resolve: bool) -> (wgpu::TextureView, (u32, u32)) {
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-composite") });
        self.begin_frame();
        let out = self.record_plan(&mut enc, plan, prep, clear, resolve);
        self.end_frame();
        self.queue.submit([enc.finish()]);
        out
    }

    fn record_plan(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        plan: &FramePlan,
        prep: Option<&PreparedPlan>,
        clear: bool,
        resolve: bool,
    ) -> (wgpu::TextureView, (u32, u32)) {
        let owned;
        let (w, h, layers): (u32, u32, &[PlanLayer]) = match plan {
            FramePlan::Layers { width, height, layers } => (*width as u32, *height as u32, layers.as_slice()),
            FramePlan::Image(img) => {
                let frame = match prep.and_then(|p| p.image.as_ref()) {
                    // The texels come from `prep`; the frame only carries size and colour.
                    Some(_) => VideoFrame {
                        width: img.w as u32,
                        height: img.h as u32,
                        data: PixelData::RgbaF32(Arc::new(Vec::new())),
                        ..VideoFrame::rgba_f32(1, 1, vec![0.0; 4])
                    },
                    None => VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px.clone()),
                };
                owned =
                    [PlanLayer { frame: Arc::new(frame), matrix: filmcraft_geom::Affine::IDENTITY, opacity: 1.0, blend: Blend::Normal, fx: None, input: None }];
                (img.w as u32, img.h as u32, &owned[..])
            }
            FramePlan::Composite { width, height, steps } => return self.composite_steps(enc, *width, *height, steps, prep, resolve),
        };
        let (w, h) = (w.max(1), h.max(1));
        let accum_view = Self::target(&self.device, &mut self.accum, w, h, ACCUM_FORMAT, wgpu::TextureUsages::COPY_SRC);
        let backdrop_view = if layers.iter().any(|l| l.blend.reads_destination()) {
            Some(Self::target(&self.device, &mut self.backdrop, w, h, ACCUM_FORMAT, wgpu::TextureUsages::COPY_DST))
        } else {
            None
        };
        let out_view = Self::target(&self.device, &mut self.output, w, h, OUTPUT_FORMAT, wgpu::TextureUsages::COPY_SRC);
        // Layers whose effects the GPU stage can't run (no stage on this device, or a working image
        // larger than a texture) are rendered on the CPU here and drawn as plain RGBA layers.
        let max_side = self.device.limits().max_texture_dimension_2d;
        let cpu_fx: Vec<Option<PlanLayer>> = layers
            .iter()
            .map(|l| {
                let lfx = l.fx.as_ref()?;
                if self.fx.is_some() && lfx.size.0.max(lfx.size.1) <= max_side {
                    return None;
                }
                let img = filmcraft_render::plan::effect_image(&l.frame, lfx);
                Some(PlanLayer::new(Arc::new(VideoFrame::rgba_f32(img.w as u32, img.h as u32, img.px)), l.matrix, l.opacity, l.blend))
            })
            .collect();
        let resolved: Vec<&PlanLayer> = layers.iter().zip(&cpu_fx).map(|(l, c)| c.as_ref().unwrap_or(l)).collect();
        let layers = resolved;
        let keys: Vec<(usize, u32, u32)> = layers
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let p = prep.and_then(|p| if matches!(plan, FramePlan::Image(_)) { p.image.as_ref() } else { p.layers.get(i).and_then(Option::as_ref) });
                // (texels converted beforehand belong to the plan's own frame)
                let p = if cpu_fx.get(i).is_some_and(Option::is_some) { None } else { p };
                self.upload(&l.frame, p)
            })
            .collect();
        // (bind group, None for a fixed-function layer or Some(region of the accumulator to copy
        // into the backdrop) for a layer that reads the destination; Some(None): off the output)
        let mut bind_groups = Vec::with_capacity(layers.len());
        // the effect stage of each layer with effects (recorded right before its draw)
        let mut jobs: Vec<Option<fx::FxJob>> = Vec::with_capacity(layers.len());
        if let Some(f) = self.fx.as_mut() {
            f.begin_frame();
        }
        for (l, k) in layers.iter().zip(&keys) {
            // (every frame was just uploaded; a missing upload draws transparent texels)
            let src = self.src_info(&l.frame, *k).unwrap_or(SrcInfo {
                kind: 1,
                code_scale: 1.0,
                code_levels: 1.0,
                alpha: 0.0,
                chroma: (1, 1),
                size: (l.frame.width, l.frame.height),
                color: l.frame.color,
                input: None,
            });
            // HDR / wide-gamut Y'CbCr: the planner's input transform, in the shader (#965)
            let src = SrcInfo { input: l.input.as_ref().filter(|_| matches!(src.kind, 2 | 3)).and_then(|t| t.gpu_params()), ..src };
            let views = self.uploads.get(k).map_or_else(|| std::array::from_fn(|_| self.dummy.clone()), |up| up.views.clone());
            let job = match &l.fx {
                Some(lfx) => {
                    // the source drawn onto the working image (box-decimated by n, as the CPU decodes)
                    let su = Self::fx_source_uniforms(&src, &l.frame, lfx);
                    let sbuf = Self::write_uniform(&self.device, &self.queue, &mut self.uniform_pool, &su);
                    let sbg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fx-source"),
                        layout: &self.layer_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: sbuf.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[0]) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[1]) },
                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&views[2]) },
                            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&views[3]) },
                        ],
                    });
                    match self.fx.as_mut() {
                        Some(f) => f.job(&self.device, &self.queue, lfx, sbg, &self.dummy),
                        None => None,
                    }
                }
                None => None,
            };
            // what the layer draws: the effect result (linear premultiplied RGBA) or the frame
            let (u, tex) = match &job {
                Some(j) => {
                    let s =
                        SrcInfo { kind: 1, code_scale: 1.0, code_levels: 1.0, alpha: 0.0, chroma: l.size(), size: l.size(), color: l.frame.color, input: None };
                    (Self::uniforms(&s, &l.matrix, l.opacity, l.blend, (w, h)), [j.result.clone(), self.dummy.clone(), self.dummy.clone(), self.dummy.clone()])
                }
                None => {
                    // (a layer with effects only gets here if its job failed, which the CPU
                    // fallback above rules out; it is then drawn without them, where the working
                    // image would be)
                    let m = match &l.fx {
                        Some(lfx) => {
                            let n = 1.0 / lfx.decimation.max(1) as f64;
                            l.matrix.then_apply(&filmcraft_geom::Affine::scale(n, n)).then_apply(&filmcraft_render::plan::quarter_turns(
                                lfx.turns,
                                l.frame.width as f64,
                                l.frame.height as f64,
                            ))
                        }
                        None => l.matrix,
                    };
                    (Self::uniforms(&src, &m, l.opacity, l.blend, (w, h)), views)
                }
            };
            jobs.push(job);
            let buf = Self::write_uniform(&self.device, &self.queue, &mut self.uniform_pool, &u);
            let mut entries = [
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tex[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&tex[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex[2]) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&tex[3]) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&self.dummy) },
            ];
            let (layout, entry_count, region) = match (&backdrop_view, l.blend.reads_destination()) {
                (Some(b), true) => {
                    entries[5] = wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(b) };
                    (&self.blend_bgl, 6, Some(quad_bounds(l, w, h)))
                }
                _ => (&self.layer_bgl, 5, None),
            };
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("layer"), layout, entries: &entries[..entry_count] });
            bind_groups.push((bg, region));
        }
        if let Some(f) = self.fx.as_mut() {
            f.end_frame();
        }
        // Runs of fixed-function layers draw in one pass. A layer that reads the destination
        // ends it, copies the accumulator under its quad into the backdrop and draws on its own.
        let accum_tex = self.accum.as_ref().map(|a| a.0.clone());
        let backdrop_tex = self.backdrop.as_ref().map(|b| b.0.clone());
        let mut cleared = !clear;
        let mut i = 0;
        while i < bind_groups.len() || !cleared {
            // a layer's effects run right before it is drawn (pooled working textures are reused
            // by the next layer with effects)
            if let (Some(Some(job)), Some(f)) = (jobs.get(i), self.fx.as_ref()) {
                f.record(enc, job);
            }
            if let Some((bg, Some(region))) = bind_groups.get(i) {
                i += 1;
                let (Some(rect), Some(src), Some(dst)) = (region, &accum_tex, &backdrop_tex) else { continue };
                if !cleared {
                    accum_pass(enc, &accum_view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                    cleared = true;
                }
                let origin = wgpu::Origin3d { x: rect.0, y: rect.1, z: 0 };
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo { texture: src, mip_level: 0, origin, aspect: wgpu::TextureAspect::All },
                    wgpu::TexelCopyTextureInfo { texture: dst, mip_level: 0, origin, aspect: wgpu::TextureAspect::All },
                    wgpu::Extent3d { width: rect.2, height: rect.3, depth_or_array_layers: 1 },
                );
                let mut pass = accum_pass(enc, &accum_view, wgpu::LoadOp::Load);
                pass.set_pipeline(&self.blend_pipeline);
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..6, 0..1);
                continue;
            }
            // a run of fixed-function layers, up to the next one that reads the destination or
            // has effects to run first
            let breaks = |j: &usize| bind_groups.get(*j).is_some_and(|(_, r)| r.is_some()) || jobs.get(*j).is_some_and(Option::is_some);
            let end = (i + 1..bind_groups.len()).find(breaks).unwrap_or(bind_groups.len()).max(i);
            let load = if cleared { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) };
            let mut pass = accum_pass(enc, &accum_view, load);
            cleared = true;
            pass.set_pipeline(&self.layer_pipeline);
            for (bg, _) in bind_groups.get(i..end).unwrap_or_default() {
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..6, 0..1);
            }
            i = end;
        }
        if resolve {
            let final_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("final"),
                layout: &self.final_bgl,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&accum_view) }],
            });
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("resolve"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.final_pipeline);
            pass.set_bind_group(0, &final_bg, &[]);
            pass.draw(0..3, 0..1);
        }
        (if resolve { out_view } else { accum_view }, (w, h))
    }

    fn composite_steps(
        &mut self,
        enc: &mut wgpu::CommandEncoder,
        width: usize,
        height: usize,
        steps: &[PlanStep],
        prep: Option<&PreparedPlan>,
        resolve: bool,
    ) -> (wgpu::TextureView, (u32, u32)) {
        let cpu = |c: &mut Self, enc: &mut wgpu::CommandEncoder| {
            let plan = FramePlan::Composite { width, height, steps: steps.to_vec() };
            c.record_plan(enc, &FramePlan::Image(filmcraft_render::plan::execute_cpu(&plan)), None, true, resolve)
        };
        if steps.iter().any(|s| matches!(s, PlanStep::Transition { effect, .. } if effect.effect != "push")) {
            self.cpu_transitions += 1;
            return cpu(self, enc);
        }
        // plan_frame makes Adjust steps only for this device's caps, so the monitor and the export
        // never get here; a plan made for another device still gets the right picture
        if steps.iter().any(|s| matches!(s, PlanStep::Adjust { .. })) && !self.caps().adjusts(width, height) {
            self.cpu_adjustments += 1;
            return cpu(self, enc);
        }
        // one frame for the effect stage's pool: the adjustment steps' working textures stay
        // pooled with those of the layers between them
        if let Some(f) = self.fx.as_mut() {
            f.begin_frame();
        }
        let empty = FramePlan::Layers { width, height, layers: Vec::new() };
        let (canvas, dims) = if matches!(steps.first(), Some(PlanStep::Layers(_))) {
            let w = (width as u32).max(1);
            let h = (height as u32).max(1);
            let accum_view = Self::target(&self.device, &mut self.accum, w, h, ACCUM_FORMAT, wgpu::TextureUsages::COPY_SRC);
            (accum_view, (w, h))
        } else {
            self.record_plan(enc, &empty, None, true, false)
        };
        let mut cleared = !matches!(steps.first(), Some(PlanStep::Layers(_)));
        let mut children = prep.map(|p| p.children.iter());
        // counted once the frame is done on the GPU: a frame that falls back to the CPU half way
        // throws away the steps drawn so far
        let (mut transitions, mut adjustments) = (0u64, 0u64);
        for step in steps {
            match step {
                PlanStep::Layers(layers) => {
                    let p = children.as_mut().and_then(Iterator::next);
                    self.record_plan(enc, &FramePlan::Layers { width, height, layers: layers.clone() }, p, !cleared, false);
                    cleared = true;
                }
                PlanStep::Transition { inputs, effect, progress, .. } => {
                    let mut views = [self.dummy.clone(), self.dummy.clone()];
                    for (index, layers) in inputs.iter().enumerate() {
                        let p = children.as_mut().and_then(Iterator::next);
                        // Isolate each clip; queued draws keep the parent accumulator and blend order.
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        let (view, _) = self.record_plan(enc, &FramePlan::Layers { width, height, layers: layers.clone() }, p, true, false);
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        views[index] = view;
                    }
                    self.transition.record((&self.device, &self.queue), enc, &canvas, [&views[0], &views[1]], dims, effect, *progress);
                    transitions += 1;
                }
                PlanStep::Adjust { ops } => {
                    if self.adjust(enc, &canvas, dims, ops).is_none() {
                        if let Some(f) = self.fx.as_mut() {
                            f.end_frame();
                        }
                        self.cpu_adjustments += 1;
                        return cpu(self, enc);
                    }
                    adjustments += 1;
                }
            }
        }
        self.gpu_transitions += transitions;
        self.gpu_adjustments += adjustments;
        let out = if resolve { self.record_plan(enc, &empty, None, false, true) } else { (canvas, dims) };
        if let Some(f) = self.fx.as_mut() {
            f.end_frame();
        }
        out
    }

    /// What this device runs, for planning its frames ([`filmcraft_render::plan::plan_frame`]):
    /// adjustment layers up to its texture limit when it has an effect stage.
    pub fn caps(&self) -> DeviceCaps {
        DeviceCaps { adjust_side: if self.fx.is_some() { self.device.limits().max_texture_dimension_2d } else { 0 } }
    }

    /// The adjustment layer `ops` on the accumulator (`canvas`, of size `w`×`h`): it is drawn
    /// into the effect stage's working image as a linear premultiplied RGBA source, the effects
    /// run there, and the result replaces the accumulator. None when the stage can't run them
    /// (nothing is drawn then).
    fn adjust(&mut self, enc: &mut wgpu::CommandEncoder, canvas: &wgpu::TextureView, (w, h): (u32, u32), ops: &[filmcraft_render::gpufx::FxOp]) -> Option<()> {
        let rgba = SrcInfo {
            kind: 1,
            code_scale: 1.0,
            code_levels: 1.0,
            alpha: 0.0,
            chroma: (w, h),
            size: (w, h),
            color: filmcraft_color::ColorInfo::SRGB_FULL,
            input: None,
        };
        let identity = filmcraft_geom::Affine::IDENTITY;
        let mut su = Self::uniforms(&rgba, &identity, 1.0, Blend::Normal, (w, h));
        // (the effect source draw's integer decimation: the canvas is the working image)
        su[26] = 1.0;
        let sbuf = Self::write_uniform(&self.device, &self.queue, &mut self.uniform_pool, &su);
        let sbg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("adjust-source"),
            layout: &self.layer_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: sbuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(canvas) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.dummy) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.dummy) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&self.dummy) },
            ],
        });
        let lfx = filmcraft_render::plan::LayerFx { size: (w, h), decimation: 1, turns: 0, ops: ops.to_vec() };
        let job = self.fx.as_mut()?.job(&self.device, &self.queue, &lfx, sbg, &self.dummy)?;
        let u = Self::uniforms(&rgba, &identity, 1.0, Blend::Normal, (w, h));
        let buf = Self::write_uniform(&self.device, &self.queue, &mut self.uniform_pool, &u);
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("adjust-result"),
            layout: &self.layer_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&job.result) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.dummy) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.dummy) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&self.dummy) },
            ],
        });
        self.fx.as_ref()?.record(enc, &job);
        {
            // cleared, then drawn "over" nothing: the adjusted picture replaces the canvas
            let mut pass = accum_pass(enc, canvas, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(&self.layer_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.draw(0..6, 0..1);
        }
        Some(())
    }

    /// Export rendering: `plan` composited into the linear float accumulator and read back as
    /// premultiplied RGBA f32 (the sRGB resolve is skipped). Layer plans and composites
    /// (`plan_frame` composites only the transitions and adjustment layers the device draws);
    /// None on a failed read-back.
    pub fn render_export_prepared(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>) -> Option<(u32, u32, Vec<f32>)> {
        let t = std::time::Instant::now();
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-composite-export") });
        self.begin_frame();
        self.record_plan(&mut enc, plan, prep, true, false);
        self.end_frame();
        let size = self.copy_accumulator(&mut enc)?;
        self.queue.submit([enc.finish()]);
        export_renderer::timing::add_submit(t.elapsed());
        self.map_accumulator(size)
    }

    /// Run the effect stage of a layer alone: `frame` decoded into its working image and `fx`
    /// applied, read back as linear premultiplied RGBA f32 (what `LayerFx` ops produce on the
    /// CPU from the same frame). None when the working image does not fit in a texture.
    pub fn effect_image(&mut self, frame: &VideoFrame, fx: &filmcraft_render::plan::LayerFx) -> Option<(u32, u32, Vec<f32>)> {
        self.begin_frame();
        let key = self.upload(frame, None);
        let Some(src) = self.src_info(frame, key) else {
            self.end_frame();
            return None;
        };
        let Some(views) = self.uploads.get(&key).map(|u| u.views.clone()) else {
            self.end_frame();
            return None;
        };
        let (w, h) = (fx.size.0.max(1), fx.size.1.max(1));
        let su = Self::fx_source_uniforms(&src, frame, fx);
        let sbuf = Self::write_uniform(&self.device, &self.queue, &mut self.uniform_pool, &su);
        let sbg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fx-source"),
            layout: &self.layer_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: sbuf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&views[2]) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&views[3]) },
            ],
        });
        let Some(stage) = self.fx.as_mut() else {
            self.end_frame();
            return None;
        };
        stage.begin_frame();
        let Some(job) = stage.job(&self.device, &self.queue, fx, sbg, &self.dummy) else {
            stage.end_frame();
            self.end_frame();
            return None;
        };
        let row = (w * 16).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx-readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        stage.record(&mut enc, &job);
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &job.result_texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        stage.end_frame();
        self.end_frame();
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        wait_mapped(&self.device, &rx)?;
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        let mut line = Vec::with_capacity((w * 16) as usize);
        for y in 0..h {
            let r = data.get((y * row) as usize..(y * row + w * 16) as usize)?;
            copy_mapped(r, &mut line);
            out.extend(line.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        }
        drop(data);
        buf.unmap();
        Some((w, h, out))
    }

    /// Read the **linear float accumulator** back as premultiplied RGBA f32 (export rendering):
    /// the display texture this crate's [`read_output`](Self::read_output) resolves is sRGB 8-bit
    /// and would clip the working-space data HDR exports need. The accumulator must have been
    /// composited first ([`composite`](Self::composite) / [`composite_prepared`](Self::composite_prepared)).
    pub fn read_accumulator(&mut self) -> Option<(u32, u32, Vec<f32>)> {
        let mut enc = self.device.create_command_encoder(&Default::default());
        let size = self.copy_accumulator(&mut enc)?;
        self.queue.submit([enc.finish()]);
        self.map_accumulator(size)
    }

    /// Record a copy of the accumulator into the staging buffer (grown when too small, reused
    /// otherwise); returns its width, height and row pitch in bytes.
    fn copy_accumulator(&mut self, enc: &mut wgpu::CommandEncoder) -> Option<(u32, u32, u32)> {
        let (_, _, (w, h)) = self.accum.as_ref()?;
        let (w, h) = (*w, *h);
        let row = w.checked_mul(8)?.div_ceil(256).checked_mul(256)?;
        let needed = u64::from(row) * u64::from(h);
        if self.accum_readback_buf.as_ref().is_none_or(|(_, size)| *size < needed) {
            let b = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("accum-readback"),
                size: needed,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.accum_readback_buf = Some((b, needed));
        }
        let (tex, _, _) = self.accum.as_ref()?;
        let (buf, _) = self.accum_readback_buf.as_ref()?;
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        Some((w, h, row))
    }

    /// Wait for the staging buffer filled by [`copy_accumulator`](Self::copy_accumulator) and
    /// convert its half floats to f32.
    fn map_accumulator(&mut self, dimensions: (u32, u32, u32)) -> Option<(u32, u32, Vec<f32>)> {
        self.map_accumulator_with_wait(dimensions, wait_mapped)
    }

    /// Keep the wait injectable so failure cleanup can be checked without stalling a driver.
    fn map_accumulator_with_wait(
        &mut self,
        (w, h, row): (u32, u32, u32),
        wait: impl FnOnce(&wgpu::Device, &std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>) -> Option<()>,
    ) -> Option<(u32, u32, Vec<f32>)> {
        if w == 0 || h == 0 {
            return None;
        }
        let t = std::time::Instant::now();
        // A failed map may already be unmapped; public unmap would report another error.
        // Own the staging buffer for this attempt and cache it again only after success.
        // Otherwise drop it so the next copy creates fresh storage instead of reusing it.
        let (buf, capacity) = self.accum_readback_buf.take()?;
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        wait(&self.device, &rx)?;
        export_renderer::timing::add_map_wait(t.elapsed());
        let t = std::time::Instant::now();
        let (width, row) = (w as usize, row as usize);
        let mut out = filmcraft_frame::pool::take_f32_overwritten(width.checked_mul(h as usize)?.checked_mul(4)?);
        let data = slice.get_mapped_range().ok()?;
        let mut line = Vec::with_capacity(width * 8);
        // every row must be there: the pooled output holds stale values until written
        let converted = out.chunks_exact_mut(width * 4).enumerate().all(|(y, dst)| {
            let Some(src) = data.get(y * row..y * row + width * 8) else { return false };
            copy_mapped(src, &mut line);
            for (d, s) in dst.iter_mut().zip(line.as_chunks::<2>().0) {
                *d = f16_to_f32_fast(u16::from_le_bytes(*s));
            }
            true
        });
        drop(data);
        if !converted {
            return None;
        }
        buf.unmap();
        self.accum_readback_buf = Some((buf, capacity));
        filmcraft_frame::transfers::record_export_readback(u64::from(w) * u64::from(h) * 8);
        export_renderer::timing::add_convert(t.elapsed());
        Some((w, h, out))
    }

    /// Read the output back as RGBA8 (tests / screenshots / thumbnails).
    pub fn read_output(&self) -> Option<(u32, u32, Vec<u8>)> {
        let (tex, _, (w, h)) = self.output.as_ref()?;
        let row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(*h) } },
            wgpu::Extent3d { width: *w, height: *h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        wait_mapped(&self.device, &rx)?;
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..*h {
            out.extend_from_slice(&data[(y * row) as usize..(y * row + w * 4) as usize]);
        }
        drop(data);
        buf.unmap();
        Some((*w, *h, out))
    }
}

/// How long a GPU read-back waits, for the poll and then for the callback. Eight seconds is far
/// longer than a healthy map.
const MAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Wait for the last submission and take its `map_async` callback, within a bound.
///
/// Export readback used `PollType::wait_indefinitely`. On radv that can never return after a failed
/// hardware-encode attempt (#1058), so the export never finishes. Giving the poll an eight second
/// timeout is not a bound on its own, though, and that is why an export could still stop for good: a
/// fence that has not signalled inside the timeout is reported to wgpu as `Ok(false)`, and wgpu
/// deliberately ignores it (`Device::maintain`: "we do not care if the wait succeeded or not, and
/// the `Ok(bool)` variant is ignored"), so `poll` returns `Ok(…)` and a GPU that has not finished
/// looks exactly like one that has. The callback then never arrives, and the blocking `recv` that
/// follows parks the export thread forever — no progress, no CPU, no error.
///
/// So the callback is taken with a timeout of our own. Anything late is `None`: the CPU renderer
/// takes the frame, which is what the poll's timeout was always meant to arrange.
fn wait_mapped<T>(device: &wgpu::Device, rx: &std::sync::mpsc::Receiver<Result<(), T>>) -> Option<()> {
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(MAP_TIMEOUT) }).ok()?;
    rx.recv_timeout(MAP_TIMEOUT).ok()?.ok()
}

/// Copy a row of a mapped read-back buffer into `line` (ordinary memory) in one go, before
/// anything reads it sample by sample. Mapped staging memory can be uncached (Mesa's Intel driver
/// maps it so): reading a 1080p half-float frame there two bytes at a time took 650 ms, a bulk copy
/// of it 85 ms; on cached memory the copy of a row that stays in the CPU cache costs next to nothing.
pub(crate) fn copy_mapped(src: &[u8], line: &mut Vec<u8>) {
    line.clear();
    line.extend_from_slice(src);
}

/// Shader source: kind (0 RGBA8 sRGB, 1 linear premultiplied RGBA, 2 YUV), sampling scales, dimensions, color.
struct SrcInfo {
    kind: u32,
    /// Texture sample -> code units: 255 for R8Unorm, 2^bits for R16Float.
    code_scale: f32,
    /// YUV quantization levels: 2^bits; R8Unorm's decoding scale is one less.
    code_levels: f32,
    /// Alpha texture sample → alpha (0: the frame has no alpha plane).
    alpha: f32,
    chroma: (u32, u32),
    size: (u32, u32),
    color: filmcraft_color::ColorInfo,
    /// Colour management beyond the default decode (a Y'CbCr layer's [`PlanLayer::input`]).
    input: Option<filmcraft_color::GpuInputParams>,
}

/// Floats in a layer's uniform block (`U` in `composite.wgsl`: 13 vec4).
const UNIFORM_FLOATS: usize = 52;

/// Poll a future once: wgpu's native error scopes resolve immediately. `None` if it is still
/// pending (then the caller assumes the worst).
fn poll_now<F: std::future::Future>(f: F) -> Option<F::Output> {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
        std::task::Poll::Ready(v) => Some(v),
        std::task::Poll::Pending => None,
    }
}

/// A render pass drawing into the accumulator.
fn accum_pass<'e>(enc: &'e mut wgpu::CommandEncoder, view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>) -> wgpu::RenderPass<'e> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("layers"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// The output pixels a layer's quad can touch, as (x, y, width, height) clamped to the `w`×`h`
/// output with a one-pixel margin; None when it touches none. A non-finite matrix covers it all.
fn quad_bounds(l: &PlanLayer, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let m = &l.matrix;
    let (fw, fh) = (l.size().0 as f64, l.size().1 as f64);
    let pts = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)].map(|(x, y)| (m.a * x + m.c * y + m.e, m.b * x + m.d * y + m.f));
    if pts.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
        return Some((0, 0, w, h));
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (x, y) in pts {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    // in range before the cast (`as` saturates anyway; NaN is excluded above)
    let px = |v: f64, hi: u32| v.max(0.0).min(hi as f64) as u32;
    let (x0, y0) = (px(x0.floor() - 1.0, w), px(y0.floor() - 1.0, h));
    let (x1, y1) = (px(x1.ceil() + 1.0, w), px(y1.ceil() + 1.0, h));
    (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
}

#[cfg(test)]
mod blend_tests;
#[cfg(test)]
mod fx_tests;
#[cfg(test)]
mod tests;
