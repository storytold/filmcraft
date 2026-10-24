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
use filmcraft_render::plan::{
    EligibilityFailure, FramePlan, GpuPlanError, PixelOrigin, PlanLayer, PlanStep, RequiredFramePlan, RequiredStep, ResidentRequiredFramePlan,
    ResidentRequiredStep,
};

pub mod adapter;
mod blur_transition;
pub mod demo_source;
pub mod export_renderer;
pub mod fx;
pub mod lut;
pub mod mask;
pub mod nv12_repack;
pub mod resident;
mod timeline_transition;
mod transition;
mod transition_blur_stage;
pub mod transition_catalog;
pub mod yuv_output;
pub use export_renderer::ExportRenderer;
pub use lut::GpuLut;
pub use mask::GpuMask;

/// Output texture format: gamma-encoded RGBA8 (what egui expects of native textures); the resolve
/// shader applies the sRGB encoding.
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Transitions whose GPU result matched the CPU oracle on the AMD adapter (`transition_catalog`
/// `per_id_parity_report`, 23/10/2026: 82 of 108 pass; the f32 run allows 1e-3 and the f16 run
/// 2e-3, the compositor's own budget). Regenerate it from that report.
pub const TRANSITION_AMD_PARITY: &[&str] = &[
    "burn_alpha",
    "frame",
    "grunge",
    "radial_wipe",
    "spin_motion",
    "star_wipe",
    "vr_iris_wipe",
    "blur_dissolve",
    "whip_legacy",
    "spinback_3d",
    "cross_dissolve",
    "cross_dissolve_legacy",
    "additive_dissolve",
    "additive_dissolve_legacy",
    "film_dissolve",
    "film_dissolve_legacy",
    "dip_to_black",
    "dip_to_black_legacy",
    "dip_to_white",
    "dip_to_white_legacy",
    "push",
    "flare",
    "linear_wipe",
    "soft_wipe",
    "wipe",
    "dip_to_color",
    "luma_fade",
    "non_additive_dissolve",
    "venetian_blinds",
    "checker_wipe",
    "gradient_wipe",
    "panel_wipe",
    "barn_doors",
    "inset",
    "iris_round",
    "iris_box",
    "iris_diamond",
    "iris_cross",
    "stretch_wipe",
    "neon_wipe",
    "mosaic_transition",
    "shape_flow",
    "vr_gradient_wipe",
    "burn_chroma",
    "chroma_leak",
    "directional_blur_transition",
    "flash",
    "lens_blur",
    "light_leak",
    "light_sweep",
    "phosphor",
    "radial_blur",
    "solarize",
    "stripe",
    "glow",
    "ray",
    "motion_camera",
    "roll",
    "stretch",
    "slide",
    "split",
    "motion_tween",
    "spring_motion",
    "push_legacy",
    "slide_legacy",
    "page_turn",
    "wave",
    "louver",
    "mirror_transition",
    "typewriter",
    "pull_motion",
    "vr_random_blocks",
    "flicker",
    "kaleidoscope",
    "liquid_distortion",
    "earthquake",
    "glass",
    "glitch",
    "vhs_damage",
    "vr_chroma_leaks",
    "vr_light_leaks",
    "vr_spherical_blur",
];

/// Effects with recorded CPU parity on the AMD adapter (22/10/2026, 0.10): every case of
/// `fx_tests::gpu_effects_match_cpu_exactly` before its harness stops at `replicate`, plus the
/// `fx_tests::gpu_effect_catalog_family_parity_requires_adapter` families that match (block_dissolve
/// and the vr_chromatic_aberrations/vr_projection/vr_rotate_sphere cases still diverge), plus
/// gaussian_blur_legacy, directional_blur_legacy, reduce_interlace_flicker, wiggle, camera_shake,
/// rotate_3d, basic_3d and lens_flare (perspective > 0 only: perspective 0 puts the camera 1e7 px away and the f32
/// coefficients would lose precision, so it fails closed) (cases added to `gpu_effects_match_cpu_exactly` on 23/10/2026 against the real CPU effect: max rel
/// diff <= 6e-6; camera_shake only without motion blur, the blurred form fails closed).
/// The strict resident export has no CPU fallback, so it refuses every effect not listed here.
const RESIDENT_FX_PARITY: &[&str] = &[
    "alpha_adjust",
    "alpha_glow",
    "asc_cdl",
    "basic_3d",
    "bevel_alpha",
    "black_white",
    "brightness_contrast",
    "camera_blur",
    "camera_shake",
    "cell_pattern",
    "change_to_color",
    "channel_blur",
    "channel_mix",
    "checkerboard",
    "cineon_converter",
    "circle",
    "color_balance",
    "color_emboss",
    "color_key",
    "color_pass",
    "color_replace",
    "corner_pin",
    "crop",
    "directional_blur",
    "directional_blur_legacy",
    "drop_shadow",
    "edge_feather",
    "ellipse",
    "emboss",
    "extract",
    "find_edges",
    "four_color_gradient",
    "gamma_correction",
    "gaussian_blur",
    "gaussian_blur_legacy",
    "gradient",
    "grid",
    "grow",
    "horizontal_flip",
    "invert",
    "leave_color",
    "lens_distortion",
    "lens_flare",
    "levels",
    "linear_wipe_legacy",
    "logo_cutout",
    "luma_key",
    "lumetri",
    "magnify",
    "magnify_legacy",
    "median",
    "mirror",
    "mosaic",
    "mosaic_legacy",
    "move",
    "noise",
    "noise_legacy",
    "offset",
    "posterize",
    "proc_amp",
    "ramp",
    "reduce_interlace_flicker",
    "replicate",
    "rgb_split",
    "rotate_3d",
    "rounded_crop",
    "sharpen",
    "shrink",
    "spherize",
    "spin",
    "strobe",
    "tint",
    "transform",
    "turbulent_displace",
    "twirl",
    "twirl_legacy",
    "unsharp_mask",
    "vertical_flip",
    "video_limiter",
    "vignette",
    "vr_color_gradients",
    "vr_plane_to_sphere",
    "wave_warp",
    "wiggle",
];

struct Uploaded {
    /// Y, Cb, Cr (or the RGBA texture) and the alpha plane (the dummy texture when there is none).
    views: [wgpu::TextureView; 4],
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
    nv12_output: yuv_output::Nv12Output,
    demo_source: Option<demo_source::DemoSource>,
    uploads: HashMap<(usize, u32, u32), Uploaded>,
    clock: u64,
    dummy: wgpu::TextureView,
    /// The effect stage (None on devices without compute shaders or storage textures: layers
    /// with effects are then rendered on the CPU here).
    fx: Option<fx::FxStage>,
    fx_unavailable_reason: Option<String>,
    strict_transition: Option<transition_catalog::TransitionCatalogStage>,
    transition: transition::PushStage,
    flare_transition: timeline_transition::FlareStage,
    blur_transition: Option<blur_transition::BlurDissolveStage>,
    transition_inputs: [Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>; 2],
    pub gpu_transitions: u64,
    pub cpu_transitions: u64,
    /// Total bytes uploaded (stats).
    pub uploaded_bytes: u64,
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

/// Convert a frame's texels for upload (None when it uploads as it is).
pub fn prepare_frame(f: &VideoFrame) -> Option<Prepared> {
    match &f.data {
        PixelData::RgbaF32(d) => Some(Prepared { planes: vec![f32_to_f16_bytes(d)] }),
        PixelData::Yuv16 { planes, bits, alpha, .. } => {
            Some(Prepared { planes: planes.iter().chain(alpha.as_ref()).map(|p| codes_to_f16_bytes(p, *bits)).collect() })
        }
        PixelData::Rgba8(_) | PixelData::Yuv8 { .. } => None,
    }
}

/// Convert every layer of a plan for upload. Thread-safe and GPU-free: run it on a worker.
pub fn prepare(plan: &FramePlan) -> PreparedPlan {
    match plan {
        FramePlan::Layers { layers, .. } => prepare_layers(layers),
        FramePlan::Image(img) => PreparedPlan { image: Some(Prepared { planes: vec![f32_to_f16_bytes(&img.px)] }), ..Default::default() },
        FramePlan::Composite { steps, .. } => PreparedPlan {
            children: steps
                .iter()
                .flat_map(|s| match s {
                    PlanStep::Layers(layers) => vec![prepare_layers(layers)],
                    PlanStep::Transition { inputs, .. } => inputs.iter().map(|ls| prepare_layers(ls)).collect(),
                })
                .collect(),
            ..Default::default()
        },
    }
}

fn prepare_layers(layers: &[PlanLayer]) -> PreparedPlan {
    PreparedPlan { layers: layers.iter().map(|l| prepare_frame(&l.frame)).collect(), ..Default::default() }
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
        // check (Rgba32Float is not renderable everywhere, e.g. software Vulkan/GL). Capture
        // validation errors; legacy calls may render effects on CPU, while strict calls fail.
        let (fx, fx_unavailable_reason) = if fx::FxStage::supported(device) {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let stage = fx::FxStage::new(device, &shader, &layer_bgl);
            match poll_now(scope.pop()) {
                Some(None) => (Some(stage), None),
                Some(Some(e)) => {
                    log::warn!("GPU effect stage unavailable; legacy calls may use CPU rendering, strict calls fail: {e}");
                    (None, Some(e.to_string()))
                }
                None => {
                    let reason = "GPU FX validation scope remained pending after nonblocking poll".to_owned();
                    log::warn!("GPU effect stage unavailable; legacy calls may use CPU rendering, strict calls fail: {reason}");
                    (None, Some(reason))
                }
            }
        } else {
            let reason = "adapter lacks required GPU FX capabilities".to_owned();
            log::warn!("GPU effect stage unavailable; legacy calls may use CPU rendering, strict calls fail: {reason}");
            (None, Some(reason))
        };
        let strict_transition = if fx.is_some() {
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let stage = transition_catalog::TransitionCatalogStage::new(device, ACCUM_FORMAT);
            match poll_now(scope.pop()) {
                Some(None) => Some(stage),
                Some(Some(error)) => {
                    log::warn!("GPU transition stage unavailable: {error}");
                    None
                }
                None => {
                    log::warn!("GPU transition validation did not complete; transition rendering unavailable");
                    None
                }
            }
        } else {
            None
        };
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
            nv12_output: yuv_output::Nv12Output::new(device),
            demo_source: None,
            uploads: HashMap::new(),
            clock: 0,
            dummy,
            fx,
            fx_unavailable_reason,
            strict_transition,
            transition: transition::PushStage::new(device),
            flare_transition: timeline_transition::FlareStage::new(device),
            blur_transition: blur_transition::BlurDissolveStage::new(device),
            transition_inputs: [None, None],
            gpu_transitions: 0,
            cpu_transitions: 0,
            uploaded_bytes: 0,
        }
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

    fn plane_texture(&mut self, w: u32, h: u32, format: wgpu::TextureFormat, bytes: &[u8], bpp: u32) -> wgpu::TextureView {
        let t = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("filmcraft-plane"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bpp), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.uploaded_bytes += bytes.len() as u64;
        t.create_view(&Default::default())
    }

    /// Upload (or reuse) the textures of a frame, using `prep` when it was converted beforehand.
    fn upload(&mut self, f: &VideoFrame, prep: Option<&Prepared>) -> (usize, u32, u32) {
        let id = match &f.data {
            PixelData::Rgba8(d) => Arc::as_ptr(d) as *const u8 as usize,
            PixelData::RgbaF32(d) => Arc::as_ptr(d) as *const u8 as usize,
            PixelData::Yuv8 { planes, .. } => Arc::as_ptr(&planes[0]) as *const u8 as usize,
            PixelData::Yuv16 { planes, .. } => Arc::as_ptr(&planes[0]) as *const u8 as usize,
        };
        let key = (id, f.width, f.height);
        self.clock += 1;
        if let Some(u) = self.uploads.get_mut(&key) {
            u.last_used = self.clock;
            return key;
        }
        let (w, h) = (f.width, f.height);
        let d0 = self.dummy.clone();
        let dummy = || d0.clone();
        let up = match &f.data {
            PixelData::Rgba8(d) => {
                let v = self.plane_texture(w, h, wgpu::TextureFormat::Rgba8UnormSrgb, d, 4);
                Uploaded {
                    views: [v, dummy(), dummy(), dummy()],
                    kind: 0,
                    code_scale: 1.0,
                    alpha_scale: 0.0,
                    chroma: (w, h),
                    last_used: self.clock,
                    _pixels: f.data.clone(),
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
                let v = self.plane_texture(w, h, wgpu::TextureFormat::Rgba16Float, half, 8);
                Uploaded {
                    views: [v, dummy(), dummy(), dummy()],
                    kind: 1,
                    code_scale: 1.0,
                    alpha_scale: 0.0,
                    chroma: (w, h),
                    last_used: self.clock,
                    _pixels: f.data.clone(),
                }
            }
            PixelData::Yuv8 { planes, chroma, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let y = self.plane_texture(w, h, wgpu::TextureFormat::R8Unorm, &planes[0], 1);
                let u = self.plane_texture(cw, ch, wgpu::TextureFormat::R8Unorm, &planes[1], 1);
                let v = self.plane_texture(cw, ch, wgpu::TextureFormat::R8Unorm, &planes[2], 1);
                let a = alpha.as_ref().map(|a| self.plane_texture(w, h, wgpu::TextureFormat::R8Unorm, a, 1));
                let alpha_scale = if a.is_some() { 1.0 } else { 0.0 };
                Uploaded {
                    views: [y, u, v, a.unwrap_or_else(dummy)],
                    kind: 2,
                    code_scale: 255.0,
                    alpha_scale,
                    chroma: (cw, ch),
                    last_used: self.clock,
                    _pixels: f.data.clone(),
                }
            }
            PixelData::Yuv16 { planes, chroma, bits, alpha } => {
                let (sx, sy) = chroma.shifts();
                let (cw, ch) = (w.div_ceil(1 << sx), h.div_ceil(1 << sy));
                let scale = (1u32 << bits) as f32;
                let owned;
                let p = match prep {
                    Some(p) => p,
                    None => {
                        owned = Prepared { planes: planes.iter().chain(alpha.as_ref()).map(|p| codes_to_f16_bytes(p, *bits)).collect() };
                        &owned
                    }
                };
                let y = self.plane_texture(w, h, wgpu::TextureFormat::R16Float, &p.planes[0], 2);
                let u = self.plane_texture(cw, ch, wgpu::TextureFormat::R16Float, &p.planes[1], 2);
                let v = self.plane_texture(cw, ch, wgpu::TextureFormat::R16Float, &p.planes[2], 2);
                // (full-resolution alpha plane: its codes span 0..2^bits - 1, hence the scale)
                let a = alpha.as_ref().and_then(|_| p.planes.get(3)).map(|b| self.plane_texture(w, h, wgpu::TextureFormat::R16Float, b, 2));
                let alpha_scale = if a.is_some() { scale / (scale - 1.0) } else { 0.0 };
                let _ = Chroma::C420;
                Uploaded {
                    views: [y, u, v, a.unwrap_or_else(dummy)],
                    kind: 2,
                    code_scale: scale,
                    alpha_scale,
                    chroma: (cw, ch),
                    last_used: self.clock,
                    _pixels: f.data.clone(),
                }
            }
        };
        self.uploads.insert(key, up);
        // keep the most recent uploads (enough for several stacked layers + playback lookahead)
        if self.uploads.len() > 24 {
            let mut v: Vec<(u64, (usize, u32, u32))> = self.uploads.iter().map(|(k, u)| (u.last_used, *k)).collect();
            v.sort_unstable();
            for (_, k) in v.into_iter().take(self.uploads.len() - 24) {
                self.uploads.remove(&k);
            }
        }
        key
    }

    /// The source description of an uploaded frame.
    fn src_info(&self, f: &VideoFrame, key: (usize, u32, u32)) -> Option<SrcInfo> {
        let up = self.uploads.get(&key)?;
        let code_levels = match &f.data {
            PixelData::Yuv8 { .. } => 256.0,
            PixelData::Yuv16 { bits, .. } => (*bits as f32).exp2(),
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

    fn uniform_buffer(&self, u: &[f32; UNIFORM_FLOATS]) -> wgpu::Buffer {
        let bytes: Vec<u8> = u.iter().flat_map(|v| v.to_le_bytes()).collect();
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer-u"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&buf, 0, &bytes);
        buf
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
        // The legacy policy cannot produce a strict rejection.
        self.render_plan_checked(plan, prep, clear, resolve, false).map(|(view, size, _)| (view, size)).unwrap_or_else(|_| (self.dummy.clone(), (1, 1)))
    }

    fn render_plan_checked(
        &mut self,
        plan: &FramePlan,
        prep: Option<&PreparedPlan>,
        clear: bool,
        resolve: bool,
        strict: bool,
    ) -> Result<(wgpu::TextureView, (u32, u32), wgpu::SubmissionIndex), GpuPlanError> {
        let fx_failure_detail = self.fx_unavailable_reason.as_ref().map_or_else(
            || "GPU effect stage unavailable or failed to create the complete job".to_owned(),
            |reason| format!("GPU effect stage unavailable: {reason}"),
        );
        let failure = || GpuPlanError {
            item_id: None,
            id: None,
            reason: EligibilityFailure::RuntimeFailure { stage: "strict compositor", detail: fx_failure_detail.clone() },
        };
        let owned;
        let (w, h, layers): (u32, u32, &[PlanLayer]) = match plan {
            FramePlan::Layers { width, height, layers } => (*width as u32, *height as u32, layers.as_slice()),
            FramePlan::Image(img) => {
                if strict {
                    return Err(failure());
                }
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
            FramePlan::Composite { width, height, steps } => {
                if strict {
                    return Err(failure());
                }
                let (view, size) = self.composite_steps(*width, *height, steps, prep, clear, resolve);
                let submission = self.queue.submit([]);
                return Ok((view, size, submission));
            }
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
        if strict && layers.iter().any(|layer| layer.fx.as_ref().is_some_and(|fx| self.fx.is_none() || fx.size.0.max(fx.size.1) > max_side)) {
            return Err(failure());
        }
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
            // Strict rendering must never turn a failed upload into transparent pixels.
            let missing_upload = || GpuPlanError {
                item_id: None,
                id: None,
                reason: EligibilityFailure::RuntimeFailure { stage: "strict source upload", detail: "uploaded source texture metadata is missing".into() },
            };
            let fallback_src = SrcInfo {
                kind: 1,
                code_scale: 1.0,
                code_levels: 1.0,
                alpha: 0.0,
                chroma: (1, 1),
                size: (l.frame.width, l.frame.height),
                color: l.frame.color,
                input: None,
            };
            let src = match self.src_info(&l.frame, *k) {
                Some(src) => src,
                None if strict => return Err(missing_upload()),
                None => fallback_src,
            };
            // HDR / wide-gamut Y'CbCr: the planner's input transform, in the shader (#965)
            let src = SrcInfo { input: l.input.as_ref().filter(|_| src.kind == 2).and_then(|t| t.gpu_params()), ..src };
            let views = match self.uploads.get(k) {
                Some(up) => up.views.clone(),
                None if strict => return Err(missing_upload()),
                None => std::array::from_fn(|_| self.dummy.clone()),
            };
            let job = match &l.fx {
                Some(lfx) => {
                    // the source drawn onto the working image (box-decimated by n, as the CPU decodes)
                    let n = lfx.decimation.max(1) as f64;
                    let mut su =
                        Self::uniforms(&src, &filmcraft_geom::Affine::scale(1.0 / n, 1.0 / n), 1.0, Blend::Normal, (lfx.size.0.max(1), lfx.size.1.max(1)));
                    // Effect sources use the working pixel center, avoiding interpolated vertex coordinates.
                    su[26] = n as f32;
                    let sbuf = self.uniform_buffer(&su);
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
            if strict && l.fx.is_some() && job.is_none() {
                return Err(failure());
            }
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
                            l.matrix.then_apply(&filmcraft_geom::Affine::scale(n, n))
                        }
                        None => l.matrix,
                    };
                    (Self::uniforms(&src, &m, l.opacity, l.blend, (w, h)), views)
                }
            };
            jobs.push(job);
            let buf = self.uniform_buffer(&u);
            let mut entries = vec![
                wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&tex[0]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&tex[1]) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex[2]) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&tex[3]) },
            ];
            let (layout, region) = match (&backdrop_view, l.blend.reads_destination()) {
                (Some(b), true) => {
                    entries.push(wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(b) });
                    (&self.blend_bgl, Some(quad_bounds(l, w, h)))
                }
                _ => (&self.layer_bgl, None),
            };
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("layer"), layout, entries: &entries });
            bind_groups.push((bg, region));
        }
        if let Some(f) = self.fx.as_mut() {
            f.end_frame();
        }
        let final_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("final"),
            layout: &self.final_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&accum_view) }],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-composite") });
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
                f.record(&mut enc, job);
            }
            if let Some((bg, Some(region))) = bind_groups.get(i) {
                i += 1;
                let (Some(rect), Some(src), Some(dst)) = (region, &accum_tex, &backdrop_tex) else { continue };
                if !cleared {
                    accum_pass(&mut enc, &accum_view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                    cleared = true;
                }
                let origin = wgpu::Origin3d { x: rect.0, y: rect.1, z: 0 };
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo { texture: src, mip_level: 0, origin, aspect: wgpu::TextureAspect::All },
                    wgpu::TexelCopyTextureInfo { texture: dst, mip_level: 0, origin, aspect: wgpu::TextureAspect::All },
                    wgpu::Extent3d { width: rect.2, height: rect.3, depth_or_array_layers: 1 },
                );
                let mut pass = accum_pass(&mut enc, &accum_view, wgpu::LoadOp::Load);
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
            let mut pass = accum_pass(&mut enc, &accum_view, load);
            cleared = true;
            pass.set_pipeline(&self.layer_pipeline);
            for (bg, _) in bind_groups.get(i..end).unwrap_or_default() {
                pass.set_bind_group(0, bg, &[]);
                pass.draw(0..6, 0..1);
            }
            i = end;
        }
        if resolve {
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
        let submission = self.queue.submit([enc.finish()]);
        Ok((if resolve { out_view } else { accum_view }, (w, h), submission))
    }

    /// Composite resident layers directly from borrowed decoded NV12 views or GPU-generated demo textures. Per-layer typed
    /// effects run through the shared effect stage and typed transitions through the strict catalog
    /// stage; destination-reading blends remain rejected. Never reads pixels back. Work before a
    /// transition is submitted in order (the catalog stage submits its own), the rest is returned.
    pub(crate) fn composite_resident_flat(
        &mut self,
        plan: &ResidentRequiredFramePlan,
        sources: &[resident::ResidentNv12Binding<'_>],
    ) -> Result<(wgpu::TextureView, (u32, u32), wgpu::CommandEncoder), GpuPlanError> {
        Self::validate_resident_required_routes(plan.nodes())?;
        resident::validate_plan_bindings(plan, sources)?;
        let (width, height) = plan.size();
        let (w, h) = (
            u32::try_from(width).map_err(|_| Self::resident_runtime_error(None, "canvas width exceeds GPU range"))?,
            u32::try_from(height).map_err(|_| Self::resident_runtime_error(None, "canvas height exceeds GPU range"))?,
        );
        if w == 0 || h == 0 || w.max(h) > self.device.limits().max_texture_dimension_2d {
            return Err(Self::resident_runtime_error(None, "resident canvas exceeds GPU texture limits"));
        }
        for layer in plan.layers() {
            for operation in layer.evidence().operations() {
                let id = match operation {
                    filmcraft_render::plan::PlannedGpuOp::Effect { id, .. } if RESIDENT_FX_PARITY.contains(&id.as_str()) => continue,
                    filmcraft_render::plan::PlannedGpuOp::Effect { id, .. } | filmcraft_render::plan::PlannedGpuOp::Transition { id, .. } => id.clone(),
                };
                return Err(GpuPlanError {
                    item_id: Some(layer.evidence().item_id()),
                    id: Some(id.clone()),
                    reason: EligibilityFailure::UnsupportedOperation { id },
                });
            }
            if layer.blend().reads_destination() {
                let id = layer.evidence().registry_id().unwrap_or("resident blend").to_owned();
                return Err(GpuPlanError {
                    item_id: Some(layer.evidence().item_id()),
                    id: Some(id.clone()),
                    reason: EligibilityFailure::UnsupportedOperation { id },
                });
            }
        }
        let canvas = Self::target(&self.device, &mut self.accum, w, h, ACCUM_FORMAT, wgpu::TextureUsages::COPY_SRC);
        if let Some(fx) = self.fx.as_mut() {
            fx.begin_frame();
        }
        let encoded = self.composite_resident_steps(plan, sources, &canvas, (w, h));
        if let Some(fx) = self.fx.as_mut() {
            fx.end_frame();
        }
        Ok((canvas, (w, h), encoded?))
    }

    fn composite_resident_steps(
        &mut self,
        plan: &ResidentRequiredFramePlan,
        sources: &[resident::ResidentNv12Binding<'_>],
        canvas: &wgpu::TextureView,
        size: (u32, u32),
    ) -> Result<wgpu::CommandEncoder, GpuPlanError> {
        let new_encoder =
            |device: &wgpu::Device| device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-resident-nv12-composite") });
        let mut encoder = new_encoder(&self.device);
        let mut cleared = false;
        for step in plan.steps() {
            match step {
                ResidentRequiredStep::Layers(layers) => {
                    self.draw_resident_layers(&mut encoder, canvas, &layers.iter().collect::<Vec<_>>(), sources, size, !cleared)?;
                    cleared = true;
                }
                ResidentRequiredStep::Transition { inputs, evidence } => {
                    if !cleared {
                        drop(accum_pass(&mut encoder, canvas, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)));
                        cleared = true;
                    }
                    let mut views = [self.dummy.clone(), self.dummy.clone()];
                    for (index, input) in inputs.iter().enumerate() {
                        let slot = self
                            .transition_inputs
                            .get_mut(index)
                            .ok_or_else(|| Self::resident_runtime_error(Some(evidence.item_id()), "resident transition input slot is missing"))?;
                        let view = Self::target(&self.device, slot, size.0, size.1, ACCUM_FORMAT, wgpu::TextureUsages::COPY_SRC);
                        self.draw_resident_layers(&mut encoder, &view, &input.iter().collect::<Vec<_>>(), sources, size, true)?;
                        if let Some(slot) = views.get_mut(index) {
                            *slot = view;
                        }
                    }
                    // The catalog stage submits its own commands: everything recorded so far goes first.
                    self.queue.submit([std::mem::replace(&mut encoder, new_encoder(&self.device)).finish()]);
                    for operation in evidence.operations() {
                        if let filmcraft_render::plan::PlannedGpuOp::Transition { id, op, progress, scale, requirements } = operation {
                            let error = |detail: String| GpuPlanError {
                                item_id: Some(evidence.item_id()),
                                id: Some(id.clone()),
                                reason: EligibilityFailure::RuntimeFailure { stage: "typed transition", detail },
                            };
                            let stage = self.strict_transition.as_ref().ok_or_else(|| error("strict transition stage unavailable".into()))?;
                            let planned = filmcraft_render::transitions::gpu_plan::PlannedTransition {
                                id: id.clone(),
                                op: op.clone(),
                                progress: *progress,
                                scale: *scale,
                                requirements: requirements.clone(),
                            };
                            stage
                                .draw(&self.device, &self.queue, canvas, [&views[0], &views[1]], size, &planned)
                                .map_err(|reason| error(format!("{reason:?}")))?;
                            self.gpu_transitions += 1;
                        }
                    }
                }
            }
        }
        if !cleared {
            drop(accum_pass(&mut encoder, canvas, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)));
        }
        Ok(encoder)
    }

    /// Record `layers` into `target`: one pass per run of layers, with each layer's effects recorded
    /// right before it is drawn (pooled working textures are reused by the next layer with effects).
    fn draw_resident_layers(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        layers: &[&filmcraft_render::plan::ResidentRequiredLayer],
        sources: &[resident::ResidentNv12Binding<'_>],
        size: (u32, u32),
        clear: bool,
    ) -> Result<(), GpuPlanError> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let groups = self.resident_layer_groups(encoder, layers, sources, size);
        let groups = match (groups, poll_now(scope.pop())) {
            (Err(error), _) => return Err(error),
            (Ok(_), Some(Some(error))) => {
                return Err(Self::resident_runtime_error(None, &format!("resident source bind group rejected: {error}")));
            }
            (Ok(_), None) => return Err(Self::resident_runtime_error(None, "resident source validation scope remained pending")),
            (Ok(groups), Some(None)) => groups,
        };
        if groups.is_empty() && clear {
            drop(accum_pass(encoder, target, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)));
        }
        let mut cleared = !clear;
        let mut start = 0;
        while start < groups.len() {
            if let (Some((_, Some(job))), Some(fx)) = (groups.get(start), self.fx.as_ref()) {
                fx.record(encoder, job);
            }
            let end = (start + 1..groups.len()).find(|index| groups.get(*index).is_some_and(|(_, job)| job.is_some())).unwrap_or(groups.len());
            let load = if cleared { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) };
            cleared = true;
            let mut pass = accum_pass(encoder, target, load);
            pass.set_pipeline(&self.layer_pipeline);
            for (bind_group, _) in groups.get(start..end).unwrap_or_default() {
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
            drop(pass);
            start = end;
        }
        Ok(())
    }

    /// One bind group per resident layer, plus the effect job of a layer with effects: its source
    /// is drawn from the borrowed NV12 views onto the working image, and the layer then draws the
    /// effect result (linear premultiplied RGBA) instead of the frame.
    fn resident_layer_groups(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        layers: &[&filmcraft_render::plan::ResidentRequiredLayer],
        sources: &[resident::ResidentNv12Binding<'_>],
        (w, h): (u32, u32),
    ) -> Result<Vec<(wgpu::BindGroup, Option<fx::FxJob>)>, GpuPlanError> {
        let mut groups = Vec::with_capacity(layers.len());
        for layer in layers {
            let item_id = layer.evidence().item_id();
            let (source_w, source_h) = layer.size();
            let (source, views) = match layer.source_kind() {
                filmcraft_render::plan::ResidentSourceKind::Decoded => {
                    let binding = sources
                        .iter()
                        .find(|source| source.id == layer.source_id())
                        .ok_or_else(|| Self::resident_runtime_error(Some(item_id), "resident source binding disappeared after validation"))?;
                    let y_view = binding.y.create_view(&Default::default());
                    let uv_view = binding.uv.create_view(&Default::default());
                    let source = SrcInfo {
                        kind: 3,
                        code_scale: 255.0,
                        code_levels: 256.0,
                        alpha: 0.0,
                        chroma: (source_w.div_ceil(2), source_h.div_ceil(2)),
                        size: (source_w, source_h),
                        color: layer.color(),
                        input: None,
                    };
                    (source, [y_view, uv_view.clone(), uv_view.clone(), uv_view])
                }
                filmcraft_render::plan::ResidentSourceKind::Demo(scene) => {
                    let time = layer.demo_time_seconds().ok_or_else(|| Self::resident_runtime_error(Some(item_id), "resident demo source time is missing"))?;
                    if self.demo_source.is_none() {
                        self.demo_source = Some(demo_source::DemoSource::new(&self.device));
                    }
                    let source =
                        self.demo_source.as_ref().ok_or_else(|| Self::resident_runtime_error(Some(item_id), "resident demo source stage is unavailable"))?;
                    let frame = source
                        .record(&self.device, encoder, scene, source_w, source_h, time, ())
                        .map_err(|detail| Self::resident_runtime_error(Some(item_id), &detail))?;
                    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
                        format: Some(wgpu::TextureFormat::Rgba8UnormSrgb),
                        usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
                        ..Default::default()
                    });
                    let source = SrcInfo {
                        kind: 0,
                        code_scale: 1.0,
                        code_levels: 1.0,
                        alpha: 0.0,
                        chroma: (source_w, source_h),
                        size: (source_w, source_h),
                        color: layer.color(),
                        input: None,
                    };
                    (source, [view, self.dummy.clone(), self.dummy.clone(), self.dummy.clone()])
                }
            };
            let input = match layer.input() {
                Some(transform) => Some(transform.gpu_params().ok_or_else(|| GpuPlanError {
                    item_id: Some(item_id),
                    id: layer.evidence().registry_id().map(str::to_owned),
                    reason: EligibilityFailure::UnsupportedFrameStructure { detail: filmcraft_render::plan::FrameRequirement::ColorPipeline },
                })?),
                None => None,
            };
            let source = SrcInfo { input, ..source };
            let job = match layer.fx() {
                Some(lfx) => {
                    let n = f64::from(lfx.decimation.max(1));
                    let mut uniform =
                        Self::uniforms(&source, &filmcraft_geom::Affine::scale(1.0 / n, 1.0 / n), 1.0, Blend::Normal, (lfx.size.0.max(1), lfx.size.1.max(1)));
                    uniform[26] = n as f32;
                    let buffer = self.uniform_buffer(&uniform);
                    let source_bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("resident-fx-source"),
                        layout: &self.layer_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[0]) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[1]) },
                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&views[2]) },
                            wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&views[3]) },
                        ],
                    });
                    let job = match self.fx.as_mut() {
                        Some(stage) => stage.job(&self.device, &self.queue, lfx, source_bg, &self.dummy),
                        None => None,
                    };
                    Some(job.ok_or_else(|| {
                        Self::resident_runtime_error(Some(item_id), "resident effect stage unavailable or working image exceeds texture limits")
                    })?)
                }
                None => None,
            };
            let (uniform, views) = match (&job, layer.fx()) {
                (Some(job), Some(lfx)) => {
                    let placed =
                        SrcInfo { kind: 1, code_scale: 1.0, code_levels: 1.0, alpha: 0.0, chroma: lfx.size, size: lfx.size, color: layer.color(), input: None };
                    (
                        Self::uniforms(&placed, layer.matrix(), layer.opacity(), layer.blend(), (w, h)),
                        [job.result.clone(), self.dummy.clone(), self.dummy.clone(), self.dummy.clone()],
                    )
                }
                _ => (Self::uniforms(&source, layer.matrix(), layer.opacity(), layer.blend(), (w, h)), views),
            };
            let buffer = self.uniform_buffer(&uniform);
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("resident-nv12-layer"),
                layout: &self.layer_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[0]) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&views[1]) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&views[2]) },
                    wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&views[3]) },
                ],
            });
            groups.push((bind_group, job));
        }
        Ok(groups)
    }

    fn resident_runtime_error(item_id: Option<filmcraft_project::ItemId>, detail: &str) -> GpuPlanError {
        GpuPlanError {
            item_id,
            id: Some("resident NV12 compositor".into()),
            reason: EligibilityFailure::RuntimeFailure { stage: "resident compositor", detail: detail.into() },
        }
    }

    /// Validate all typed routes before dispatching any branch. (The resident export runs this after
    /// it has decoded its sources, so a refused route still costs that decode.)
    pub(crate) fn validate_required_routes<'a>(nodes: impl Iterator<Item = &'a filmcraft_render::plan::PlanNodeEvidence>) -> Result<(), GpuPlanError> {
        Self::validate_routes(nodes, false)
    }

    fn validate_resident_required_routes<'a>(nodes: impl Iterator<Item = &'a filmcraft_render::plan::PlanNodeEvidence>) -> Result<(), GpuPlanError> {
        Self::validate_routes(nodes, true)
    }

    fn validate_routes<'a>(nodes: impl Iterator<Item = &'a filmcraft_render::plan::PlanNodeEvidence>, allow_demo_source: bool) -> Result<(), GpuPlanError> {
        for node in nodes {
            let supported = matches!(node.origin(), PixelOrigin::DecodedMediaSource | PixelOrigin::GpuGenerated { route: "transition" })
                || (allow_demo_source && matches!(node.origin(), PixelOrigin::GpuGenerated { route: "demo_source" }));
            if !supported {
                return Err(GpuPlanError {
                    item_id: Some(node.item_id()),
                    id: node.registry_id().map(str::to_owned),
                    reason: EligibilityFailure::CpuFallbackRequired { id: node.registry_id().map(str::to_owned) },
                });
            }
            for operation in node.operations() {
                if let filmcraft_render::plan::PlannedGpuOp::Transition { id, op, progress, scale, requirements } = operation {
                    // Only transitions with recorded AMD parity are admitted by the strict compositors.
                    if !TRANSITION_AMD_PARITY.contains(&id.as_str()) {
                        return Err(GpuPlanError {
                            item_id: Some(node.item_id()),
                            id: Some(id.clone()),
                            reason: EligibilityFailure::UnsupportedOperation { id: id.clone() },
                        });
                    }
                    let planned = filmcraft_render::transitions::gpu_plan::PlannedTransition {
                        id: id.clone(),
                        op: op.clone(),
                        progress: *progress,
                        scale: *scale,
                        requirements: requirements.clone(),
                    };
                    transition_catalog::TransitionCatalogStage::validate(&planned).map_err(|error| GpuPlanError {
                        item_id: Some(node.item_id()),
                        id: Some(id.clone()),
                        reason: match error {
                            transition_catalog::TransitionCatalogError::Unsupported { .. } => EligibilityFailure::UnsupportedOperation { id: id.clone() },
                            _ => EligibilityFailure::InvalidParameter { id: id.clone(), parameter: "typed transition plan".into() },
                        },
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Dispatch opaque strict evidence into a resident accumulator; errors never become CPU pixels,
    /// transparent layers, or a dummy texture. Never reads pixels back.
    pub fn composite_required(&mut self, plan: &RequiredFramePlan) -> Result<(wgpu::TextureView, (u32, u32), wgpu::SubmissionIndex), GpuPlanError> {
        Self::validate_required_routes(plan.nodes())?;
        let (width, height) = plan.size();
        let empty = FramePlan::Layers { width, height, layers: Vec::new() };
        let (canvas, dims, _) = self.render_plan_checked(&empty, None, true, false, true)?;
        for step in plan.steps() {
            match step {
                RequiredStep::Layers(next) => {
                    let layers = next.iter().map(|layer| layer.layer().clone()).collect();
                    self.render_plan_checked(&FramePlan::Layers { width, height, layers }, None, false, false, true)
                        .map_err(|error| Self::required_layer_error(error, next))?;
                }
                RequiredStep::Transition { inputs, evidence } => {
                    let mut views = [self.dummy.clone(), self.dummy.clone()];
                    for (index, input) in inputs.iter().enumerate() {
                        let layers = input.iter().map(|layer| layer.layer().clone()).collect();
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        let rendered = self.render_plan_checked(&FramePlan::Layers { width, height, layers }, None, true, false, true);
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        views[index] = rendered.map_err(|error| Self::required_layer_error(error, input))?.0;
                    }
                    for operation in evidence.operations() {
                        if let filmcraft_render::plan::PlannedGpuOp::Transition { id, op, progress, scale, requirements } = operation {
                            let error = |detail: String| GpuPlanError {
                                item_id: Some(evidence.item_id()),
                                id: Some(id.clone()),
                                reason: EligibilityFailure::RuntimeFailure { stage: "typed transition", detail },
                            };
                            let stage = self.strict_transition.as_ref().ok_or_else(|| error("strict transition stage unavailable".into()))?;
                            let planned = filmcraft_render::transitions::gpu_plan::PlannedTransition {
                                id: id.clone(),
                                op: op.clone(),
                                progress: *progress,
                                scale: *scale,
                                requirements: requirements.clone(),
                            };
                            stage
                                .draw(&self.device, &self.queue, &canvas, [&views[0], &views[1]], dims, &planned)
                                .map_err(|reason| error(format!("{reason:?}")))?;
                            self.gpu_transitions += 1;
                        }
                    }
                }
            }
        }
        let (_, _, submission) = self.render_plan_checked(&empty, None, false, false, true)?;
        Ok((canvas, dims, submission))
    }

    /// Resolve a required preview to gamma-encoded RGBA8 for egui, keeping the resident API linear.
    pub fn composite_required_display(&mut self, plan: &RequiredFramePlan) -> Result<(wgpu::TextureView, (u32, u32), wgpu::SubmissionIndex), GpuPlanError> {
        self.composite_required(plan)?;
        let (width, height) = plan.size();
        self.render_plan_checked(&FramePlan::Layers { width, height, layers: Vec::new() }, None, false, true, true)
    }

    /// Resolve a strict resident plan that has no decoded sources (GPU-generated demo scenes only)
    /// to gamma-encoded RGBA8 for egui. The scenes are generated on the GPU; nothing is read back.
    pub fn composite_demo_display(&mut self, plan: &ResidentRequiredFramePlan) -> Result<(wgpu::TextureView, (u32, u32), wgpu::SubmissionIndex), GpuPlanError> {
        let (_, _, encoder) = self.composite_resident_flat(plan, &[])?;
        self.queue.submit([encoder.finish()]);
        let (width, height) = plan.size();
        self.render_plan_checked(&FramePlan::Layers { width, height, layers: Vec::new() }, None, false, true, true)
    }

    fn required_layer_error(mut error: GpuPlanError, layers: &[filmcraft_render::plan::RequiredLayer]) -> GpuPlanError {
        if error.item_id.is_none()
            && let Some(layer) = layers.iter().find(|layer| !layer.evidence().operations().is_empty())
        {
            error.item_id = Some(layer.evidence().item_id());
            error.id = layer.evidence().operations().first().map(|operation| match operation {
                filmcraft_render::plan::PlannedGpuOp::Effect { id, .. } | filmcraft_render::plan::PlannedGpuOp::Transition { id, .. } => id.clone(),
            });
        }
        error
    }

    fn composite_steps(
        &mut self,
        width: usize,
        height: usize,
        steps: &[PlanStep],
        prep: Option<&PreparedPlan>,
        clear: bool,
        resolve: bool,
    ) -> (wgpu::TextureView, (u32, u32)) {
        let unsupported =
            steps.iter().any(|s| matches!(s, PlanStep::Transition { effect, .. } if !matches!(effect.effect.as_str(), "push" | "flare" | "blur_dissolve")));
        let unavailable =
            steps.iter().any(|s| matches!(s, PlanStep::Transition { effect, .. } if effect.effect == "blur_dissolve") && self.blur_transition.is_none());
        if unsupported || unavailable {
            self.cpu_transitions += 1;
            let plan = FramePlan::Composite { width, height, steps: steps.to_vec() };
            return self.render_plan(&FramePlan::Image(filmcraft_render::plan::execute_cpu(&plan)), None, clear, resolve);
        }
        let empty = FramePlan::Layers { width, height, layers: Vec::new() };
        let (canvas, dims) = self.render_plan(&empty, None, clear, false);
        let mut children = prep.map(|p| p.children.iter());
        for step in steps {
            match step {
                PlanStep::Layers(layers) => {
                    let p = children.as_mut().and_then(Iterator::next);
                    self.render_plan(&FramePlan::Layers { width, height, layers: layers.clone() }, p, false, false);
                }
                PlanStep::Transition { inputs, effect, progress, scale } => {
                    let mut views = [self.dummy.clone(), self.dummy.clone()];
                    for (index, layers) in inputs.iter().enumerate() {
                        let p = children.as_mut().and_then(Iterator::next);
                        // Isolate each clip; queued draws keep the parent accumulator and blend order.
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        let (view, _) = self.render_plan(&FramePlan::Layers { width, height, layers: layers.clone() }, p, true, false);
                        std::mem::swap(&mut self.accum, &mut self.transition_inputs[index]);
                        views[index] = view;
                    }
                    match effect.effect.as_str() {
                        "push" => self.transition.draw(&self.device, &self.queue, &canvas, [&views[0], &views[1]], dims, effect, *progress),
                        "flare" => self.flare_transition.draw(&self.device, &self.queue, &canvas, [&views[0], &views[1]], dims, effect, *progress),
                        "blur_dissolve" => {
                            if let Some(stage) = self.blur_transition.as_mut() {
                                stage.draw(&self.device, &self.queue, &canvas, [&views[0], &views[1]], dims, effect, *progress, *scale);
                            }
                        }
                        _ => continue,
                    }
                    self.gpu_transitions += 1;
                }
            }
        }
        self.render_plan(&empty, None, false, resolve)
    }

    /// Export rendering: `plan` composited into the linear float accumulator and read back as
    /// premultiplied RGBA f32 (the sRGB resolve is skipped). None for a plan the export path does
    /// not run on the GPU (transitions: the caller renders those on the CPU) or a failed read-back.
    pub fn render_export_prepared(&mut self, plan: &FramePlan, prep: Option<&PreparedPlan>) -> Option<(u32, u32, Vec<f32>)> {
        let t = std::time::Instant::now();
        self.render_plan(plan, prep, true, false);
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-composite-export") });
        let size = self.copy_accumulator(&mut enc)?;
        self.queue.submit([enc.finish()]);
        export_renderer::timing::add_submit(t.elapsed());
        self.map_accumulator(size)
    }

    /// Convert a composited export frame into a caller-owned resident NV12 buffer.
    /// Returns the buffer, 256-byte-pitched layout, and conversion submission to wait on.
    pub fn render_nv12_buffer(
        &mut self,
        plan: &FramePlan,
        prep: Option<&PreparedPlan>,
        output: wgpu::Buffer,
    ) -> Option<(wgpu::Buffer, yuv_output::Nv12Layout, wgpu::SubmissionIndex)> {
        self.render_nv12_buffer_coded(plan, prep, output, None)
    }

    /// Convert an export frame to NV12 with an optional coded size, replicating visible edges.
    pub fn render_nv12_buffer_coded(
        &mut self,
        plan: &FramePlan,
        prep: Option<&PreparedPlan>,
        output: wgpu::Buffer,
        coded_size: Option<(u32, u32)>,
    ) -> Option<(wgpu::Buffer, yuv_output::Nv12Layout, wgpu::SubmissionIndex)> {
        if !output.usage().contains(wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC) {
            return None;
        }
        let (accum, (width, height)) = self.render_plan(plan, prep, true, false);
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("filmcraft-nv12-export") });
        let (coded_width, coded_height) = coded_size.unwrap_or((width, height));
        let layout = self.nv12_output.record_coded(&self.device, &mut encoder, &accum, &output, width, height, coded_width, coded_height)?;
        let submission = self.queue.submit([encoder.finish()]);
        Some((output, layout, submission))
    }

    /// Run the effect stage of a layer alone: `frame` decoded into its working image and `fx`
    /// applied, read back as linear premultiplied RGBA f32 (what `LayerFx` ops produce on the
    /// CPU from the same frame). None when the working image does not fit in a texture.
    pub fn effect_image(&mut self, frame: &VideoFrame, fx: &filmcraft_render::plan::LayerFx) -> Option<(u32, u32, Vec<f32>)> {
        let key = self.upload(frame, None);
        let src = self.src_info(frame, key)?;
        let views = self.uploads.get(&key)?.views.clone();
        let n = fx.decimation.max(1) as f64;
        let (w, h) = (fx.size.0.max(1), fx.size.1.max(1));
        let mut su = Self::uniforms(&src, &filmcraft_geom::Affine::scale(1.0 / n, 1.0 / n), 1.0, Blend::Normal, (w, h));
        // Same integer source-grid mapping as the composited effect path.
        su[26] = n as f32;
        let sbuf = self.uniform_buffer(&su);
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
        let stage = self.fx.as_mut()?;
        stage.begin_frame();
        let job = stage.job(&self.device, &self.queue, fx, sbg, &self.dummy)?;
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
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        if wait_mapped(&self.device).is_none() || rx.recv_timeout(MAP_TIMEOUT).ok().and_then(Result::ok).is_none() {
            buf.unmap();
            return None;
        }
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let r = data.get((y * row) as usize..(y * row + w * 16) as usize)?;
            out.extend(r.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
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
    fn map_accumulator(&self, (w, h, row): (u32, u32, u32)) -> Option<(u32, u32, Vec<f32>)> {
        if w == 0 || h == 0 {
            return None;
        }
        let t = std::time::Instant::now();
        let (buf, _) = self.accum_readback_buf.as_ref()?;
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        if wait_mapped(&self.device).is_none() || rx.recv_timeout(MAP_TIMEOUT).ok().and_then(Result::ok).is_none() {
            buf.unmap();
            return None;
        }
        export_renderer::timing::add_map_wait(t.elapsed());
        let t = std::time::Instant::now();
        let (width, row) = (w as usize, row as usize);
        let mut out = filmcraft_frame::pool::take_f32_overwritten(width.checked_mul(h as usize)?.checked_mul(4)?);
        let data = slice.get_mapped_range().ok()?;
        // every row must be there: the pooled output holds stale values until written
        let converted = out.chunks_exact_mut(width * 4).enumerate().all(|(y, dst)| {
            let Some(src) = data.get(y * row..y * row + width * 8) else { return false };
            for (d, s) in dst.iter_mut().zip(src.as_chunks::<2>().0) {
                *d = f16_to_f32_fast(u16::from_le_bytes(*s));
            }
            true
        });
        drop(data);
        buf.unmap();
        export_renderer::timing::add_convert(t.elapsed());
        converted.then_some((w, h, out))
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
        if wait_mapped(&self.device).is_none() || rx.recv_timeout(MAP_TIMEOUT).ok().and_then(Result::ok).is_none() {
            buf.unmap();
            return None;
        }
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

/// Wait for the last submission and its `map_async` callbacks.
///
/// Export readback used `PollType::wait_indefinitely`. On radv that can never return after a
/// failed hardware-encode attempt (#1058), so the export never finishes. Eight seconds is longer
/// than a healthy map; a timeout returns `None` and the CPU renderer takes the frame.
const MAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

fn wait_mapped(device: &wgpu::Device) -> Option<()> {
    device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(MAP_TIMEOUT) }).ok().map(|_| ())
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
