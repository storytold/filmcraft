//! Typed catalog kernels. Unsupported plans fail before any GPU command is encoded.
use filmcraft_render::transitions::gpu_plan::{
    FieldWipeKind, IrisShape, LightKind, LightOp, PlannedTransition, PushDirection, TransitionOp, gpu_3d, gpu_blur, gpu_cards, gpu_grunge, gpu_motion,
};
use std::sync::Mutex;
use wgpu::util::DeviceExt;

#[path = "transition_light_multipass.rs"]
mod light_multipass;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransitionCatalogError {
    Unsupported {
        id: String,
    },
    InvalidPlan {
        id: String,
    },
    InvalidSize,
    /// The device could not allocate or validate the scratch resources of a kernel at this size.
    AllocationFailed {
        detail: String,
    },
}

const PI: f32 = std::f32::consts::PI;
const TAU: f32 = std::f32::consts::TAU;
const FRAC_PI_2: f32 = std::f32::consts::FRAC_PI_2;

fn direction(angle_degrees: f32) -> (f32, f32) {
    let radians = angle_degrees.to_radians();
    (radians.sin(), -radians.cos())
}

fn wipe_choice_direction(choice: u32) -> (f32, f32) {
    match choice {
        0 => (0.0, 1.0),
        1 => (-1.0, 0.0),
        2 => (0.0, -1.0),
        _ => (1.0, 0.0),
    }
}

fn resolve_center(center: [Option<f32>; 2], w: f32, h: f32) -> (f32, f32) {
    (center[0].unwrap_or(w / 2.0), center[1].unwrap_or(h / 2.0))
}

fn motion_kernel(id: &str) -> Option<u32> {
    Some(match id {
        "pop_motion" => 35,
        "spring_motion" => 38,
        "film_roll" => 41,
        "roll" => 42,
        "slide" => 43,
        "split" => 44,
        "stretch" => 45,
        "motion_camera" => 47,
        "motion_tween" => 48,
        "wave" => 50,
        "louver" => 51,
        "mirror_transition" => 52,
        "pull_motion" => 53,
        "travel_motion" => 54,
        "spin_motion" => 55,
        "roll_3d" => 56,
        "block_motion" => 57,
        "page_turn" => 59,
        "push_legacy" => 60,
        "slide_legacy" => 61,
        "split_legacy" => 62,
        _ => return None,
    })
}

fn grunge_kernel(id: &str) -> Option<u32> {
    Some(match id {
        "chaos" => 80,
        "earthquake" => 81,
        "glass" => 82,
        "glitch" => 83,
        "grunge" => 84,
        "tv_power" => 85,
        "vhs_damage" => 86,
        "flicker" => 104,
        "kaleidoscope" => 105,
        "liquid_distortion" => 106,
        _ => return None,
    })
}

fn vr_kernel(id: &str) -> Option<u32> {
    Some(match id {
        "vr_random_blocks" => 112,
        "vr_chroma_leaks" => 113,
        "vr_light_leaks" => 114,
        "vr_light_rays" => 115,
        "vr_mobius_zoom" => 116,
        _ => return None,
    })
}

fn card_kernel(id: &str) -> Option<u32> {
    Some(match id {
        "page_peel" => 94,
        "whip" => 95,
        _ => return None,
    })
}

fn light_kernel(id: &str) -> Option<u32> {
    Some(match id {
        "burn_chroma" => 64,
        "chroma_leak" => 65,
        "cross_zoom" => 66,
        "directional_blur_transition" => 67,
        "flash" => 68,
        "lens_blur" => 69,
        "light_leak" => 70,
        "light_sweep" => 71,
        "phosphor" => 72,
        "radial_blur" => 73,
        "solarize" => 74,
        "stripe" => 75,
        "zoom_blur" => 76,
        "glow" => 77,
        "ray" => 78,
        "cross_zoom_legacy" => 79,
        _ => return None,
    })
}

fn blur_kernel(id: &str) -> Option<u32> {
    Some(match id {
        "blur_dissolve" => 108,
        "vr_spherical_blur" => 109,
        "whip_legacy" => 111,
        _ => return None,
    })
}

fn star_distance(x: f32, y: f32, n: u32, inner: f32) -> f32 {
    let r = (x * x + y * y).sqrt();
    if r < 1e-6 {
        return 0.0;
    }
    let theta = (-y).atan2(x) - FRAC_PI_2;
    let seg = TAU / n.max(2) as f32;
    let a = (theta.rem_euclid(seg) - seg / 2.0).abs();
    let a = seg / 2.0 - a;
    let ri = inner.clamp(0.05, 1.0);
    let (p1x, p1y) = (ri * (seg / 2.0).cos(), ri * (seg / 2.0).sin());
    let (ex, ey) = (p1x - 1.0, p1y);
    let (dx, dy) = (a.cos(), a.sin());
    let denom = dx * ey - dy * ex;
    let tr = if denom.abs() < 1e-9 { 1.0 } else { ey / denom };
    r / tr.max(0.05)
}

fn shape_distance(shape: u32, x: f32, y: f32) -> f32 {
    let r = (x * x + y * y).sqrt();
    let theta = (-y).atan2(x);
    match shape {
        1 => x.abs().max(y.abs()),
        2 => (x.abs() + y.abs()) * std::f32::consts::FRAC_1_SQRT_2,
        3 => star_distance(x, y, 5, 0.45),
        4 => r / (0.5 * (1.0 - theta.sin()) + 0.08) * 0.5,
        5 | 6 => {
            let (n, rot) = if shape == 5 { (3.0, FRAC_PI_2 - PI / 3.0) } else { (6.0, 0.0) };
            let seg = TAU / n;
            let a = (theta - rot).rem_euclid(seg) - seg / 2.0;
            r * a.cos() / (PI / n).cos()
        }
        _ => r,
    }
}

fn shape_dissolve_field(shape: u32, size: f32, rotation: f32, order: u32, seed: u32, cols: f32, rows: f32, diag: f32, x: f32, y: f32) -> f32 {
    let (cx, cy) = ((x / size).floor(), (y / size).floor());
    let (lx, ly) = (x - (cx + 0.5) * size, y - (cy + 0.5) * size);
    let r = rotation.to_radians();
    let (s, c) = r.sin_cos();
    let (u, v) = (lx / (size * 0.5) * c + ly / (size * 0.5) * s, -lx / (size * 0.5) * s + ly / (size * 0.5) * c);
    let delay = match order {
        1 => cx / cols,
        2 => (((cx + 0.5) - cols / 2.0).powi(2) + ((cy + 0.5) - rows / 2.0).powi(2)).sqrt() / diag,
        _ => {
            let mut h = (cx as u32).wrapping_mul(0x8DA6_B343) ^ (cy as u32).wrapping_mul(0xD816_3841) ^ seed.wrapping_mul(0xCB1A_B31F) ^ 0x9E37_79B9;
            h ^= h >> 15;
            h = h.wrapping_mul(0x2C1B_3C6D);
            h ^= h >> 12;
            h = h.wrapping_mul(0x297A_2D39);
            h ^= h >> 15;
            (h >> 8) as f32 / 16_777_216.0
        }
    };
    (shape_distance(shape, u, v) + delay * 1.5) * size * 0.5
}

fn field_value(kind: &FieldWipeKind, x: f32, y: f32, w: f32, h: f32, plateau_range: (f32, f32)) -> f32 {
    let asp = w / h.max(1.0);
    match kind {
        FieldWipeKind::Clock { center, start_degrees } => {
            let (cx, cy) = resolve_center(*center, w, h);
            let scale = (w + h) * 0.5;
            ((x - cx).atan2(-(y - cy)) - start_degrees.to_radians()).rem_euclid(TAU) / TAU * scale
        }
        FieldWipeKind::Radial { corner } => {
            let (ox, oy, a0) = match *corner {
                1 => (w, 0.0, FRAC_PI_2),
                2 => (w, h, PI),
                3 => (0.0, h, -FRAC_PI_2),
                _ => (0.0, 0.0, 0.0),
            };
            let scale = (w + h) * 0.5;
            let a = (y - oy).atan2(x - ox) - a0;
            let a = a.rem_euclid(TAU);
            (if a > PI * 1.5 { 0.0 } else { a.min(FRAC_PI_2) }) / FRAC_PI_2 * scale
        }
        FieldWipeKind::Star { center, points, inner, rotation_degrees } => {
            let (cx, cy) = resolve_center(*center, w, h);
            let r = rotation_degrees.to_radians();
            let (s, c) = r.sin_cos();
            let (u, v) = ((x - cx) * c + (y - cy) * s, -(x - cx) * s + (y - cy) * c);
            star_distance(u, v, *points, *inner)
        }
        FieldWipeKind::Panel { direction, panels } => {
            let (dx, dy): (f32, f32) = match *direction {
                0 => (0.0, 1.0),
                1 => (-1.0, 0.0),
                2 => (0.0, -1.0),
                _ => (1.0, 0.0),
            };
            let n = *panels as f32;
            let travel = (dx.abs() * w + dy.abs() * h).max(1.0);
            let across_len = if dx != 0.0 { h } else { w };
            let along = x * dx + y * dy;
            let across = if dx != 0.0 { y } else { x };
            let idx = (across / across_len * n).floor().clamp(0.0, n - 1.0);
            along + idx * travel * 0.6 / n
        }
        FieldWipeKind::Plateau { angle_degrees, plateaus } => {
            let (dx, dy) = direction(*angle_degrees);
            let (lo, hi) = plateau_range;
            let span = (hi - lo).max(1.0);
            let n = *plateaus as f32;
            let step = w.max(h) * 0.35 / n.max(1.0);
            let along = x * dx + y * dy;
            let across = ((-x * dy + y * dx) - lo) / span;
            let idx = (across * n).floor().clamp(0.0, n - 1.0);
            along + (idx - (n - 1.0) / 2.0).abs() * step
        }
        FieldWipeKind::BarnDoors { center, orientation } => {
            let (cx, cy) = resolve_center(*center, w, h);
            if *orientation == 1 { (x - cx).abs() } else { (y - cy).abs() * asp }
        }
        FieldWipeKind::Inset { corner } => {
            let (ox, oy) = match *corner {
                1 => (w, 0.0),
                2 => (w, h),
                3 => (0.0, h),
                _ => (0.0, 0.0),
            };
            (x - ox).abs().max((y - oy).abs() * asp)
        }
        FieldWipeKind::Iris { center, shape } => {
            let (cx, cy) = resolve_center(*center, w, h);
            let (u, v) = ((x - cx).abs(), (y - cy).abs() * asp);
            match shape {
                IrisShape::Round => ((x - cx).powi(2) + (y - cy).powi(2)).sqrt(),
                IrisShape::Box => u.max(v),
                IrisShape::Diamond => u + v,
                IrisShape::Cross => (u / 0.3).max(v).min(u.max(v / 0.3)),
            }
        }
        FieldWipeKind::VrIris { center } => {
            let (cx, cy) = resolve_center(*center, w, h);
            let lon = |x: f32| x / w * TAU - PI;
            let lat = |y: f32| FRAC_PI_2 - y / h * PI;
            let (l0, p0) = (lon(cx), lat(cy));
            let (sp0, cp0) = p0.sin_cos();
            let (l1, p1) = (lon(x), lat(y));
            let c = sp0 * p1.sin() + cp0 * p1.cos() * (l1 - l0).cos();
            c.clamp(-1.0, 1.0).acos() * (w / TAU)
        }
    }
}

fn field_range(w: f32, h: f32, mut field: impl FnMut(f32, f32) -> f32) -> (f32, f32) {
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for j in 0..=48 {
        for i in 0..=48 {
            let v = field(w * i as f32 / 48.0, h * j as f32 / 48.0);
            if v.is_finite() {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
    }
    if lo.is_finite() { (lo, hi) } else { (0.0, 1.0) }
}

fn pack_field_wipe(words: &mut [u32; 32], p: &filmcraft_render::transitions::gpu_plan::FieldWipeParams, kernel: u32, size: (u32, u32)) {
    let (w, h) = (size.0 as f32, size.1 as f32);
    let zero_range = (0.0, 0.0);
    let mut plateau_range = zero_range;
    if let FieldWipeKind::Plateau { angle_degrees, .. } = p.kind {
        let (dx, dy) = direction(angle_degrees);
        plateau_range = field_range(w, h, |x, y| -x * dy + y * dx);
    }
    let (lo, hi) = field_range(w, h, |x, y| field_value(&p.kind, x, y, w, h, plateau_range));
    words[3] = p.style.antialias.to_bits();
    words[4] = p.style.feather.to_bits();
    words[5] = p.style.border_width.to_bits();
    for (word, color) in words[8..11].iter_mut().zip(p.style.border_color_linear) {
        *word = color.to_bits();
    }
    words[11] = 1.0f32.to_bits();
    words[12] = lo.to_bits();
    words[13] = hi.to_bits();
    words[17] = plateau_range.0.to_bits();
    words[18] = plateau_range.1.to_bits();
    match p.kind {
        FieldWipeKind::Clock { center, start_degrees } => {
            words[14] = resolve_center(center, w, h).0.to_bits();
            words[15] = resolve_center(center, w, h).1.to_bits();
            words[16] = start_degrees.to_bits();
        }
        FieldWipeKind::Radial { corner } => words[2] = corner,
        FieldWipeKind::Star { center, points, inner, rotation_degrees } => {
            words[14] = resolve_center(center, w, h).0.to_bits();
            words[15] = resolve_center(center, w, h).1.to_bits();
            words[19] = inner.to_bits();
            words[20] = rotation_degrees.to_bits();
            words[21] = (points as f32).to_bits();
        }
        FieldWipeKind::Panel { direction, panels } => {
            words[2] = direction;
            words[21] = (panels as f32).to_bits();
        }
        FieldWipeKind::Plateau { angle_degrees, .. } => {
            let (dx, dy) = direction(angle_degrees);
            words[6] = dx.to_bits();
            words[7] = dy.to_bits();
            words[16] = angle_degrees.to_bits();
        }
        FieldWipeKind::BarnDoors { center, orientation } => {
            words[2] = orientation;
            words[14] = resolve_center(center, w, h).0.to_bits();
            words[15] = resolve_center(center, w, h).1.to_bits();
        }
        FieldWipeKind::Inset { corner } => words[2] = corner,
        FieldWipeKind::Iris { center, .. } => {
            words[14] = resolve_center(center, w, h).0.to_bits();
            words[15] = resolve_center(center, w, h).1.to_bits();
        }
        FieldWipeKind::VrIris { center } => {
            words[14] = resolve_center(center, w, h).0.to_bits();
            words[15] = resolve_center(center, w, h).1.to_bits();
        }
    }
    words[1] = kernel;
}

pub struct TransitionCatalogStage {
    pipeline: wgpu::RenderPipeline,
    lights: Mutex<light_multipass::TransitionLightMultipassStage>,
    blur: Mutex<crate::transition_blur_stage::TransitionBlurStage>,
}

impl TransitionCatalogStage {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("transition-catalog"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}\n{}\n{}\n{}\n{}",
                    include_str!("transition_catalog.wgsl"),
                    include_str!("transition_lights.wgsl"),
                    include_str!("transition_motion.wgsl"),
                    include_str!("transition_grunge.wgsl"),
                    include_str!("transition_cards.wgsl"),
                    include_str!("transition_3d.wgsl")
                )
                .into(),
            ),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transition-catalog"),
            entries: &[
                texture(0),
                texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transition-catalog"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("transition-catalog"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(if format == wgpu::TextureFormat::Rgba16Float { "fs_half" } else { "fs" }),
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
            pipeline,
            lights: Mutex::new(light_multipass::TransitionLightMultipassStage::new(device, format)),
            blur: Mutex::new(crate::transition_blur_stage::TransitionBlurStage::new(device, format)),
        }
    }

    pub fn validate(plan: &PlannedTransition) -> Result<(), TransitionCatalogError> {
        Self::params(plan, None).map(|_| ())
    }

    fn params(plan: &PlannedTransition, size: Option<(u32, u32)>) -> Result<[u32; 32], TransitionCatalogError> {
        let mut words = [0; 32];
        let kernel = match (plan.id.as_str(), &plan.op) {
            (id, TransitionOp::Blur(op)) if blur_kernel(id) == Some(op.kind as u32) && gpu_blur::matches_id(id) && op.gpu_ok() => {
                let packed = op.pack();
                if packed[0] != plan.progress.to_bits() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words = packed;
                packed[1]
            }
            ("flip_motion", TransitionOp::ThreeD(op)) if op.kind == gpu_3d::ThreeDKind::FlipMotion && gpu_3d::gpu_ok(op) => {
                let packed = gpu_3d::pack(op).ok_or_else(|| TransitionCatalogError::InvalidPlan { id: plan.id.clone() })?;
                if packed[0] != plan.progress.to_bits() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words = packed;
                packed[1]
            }
            ("spin_3d", TransitionOp::ThreeD(op)) if op.kind == gpu_3d::ThreeDKind::Spin3d && gpu_3d::gpu_ok(op) => {
                let packed = gpu_3d::pack(op).ok_or_else(|| TransitionCatalogError::InvalidPlan { id: plan.id.clone() })?;
                if packed[0] != plan.progress.to_bits() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words = packed;
                packed[1]
            }
            ("slice", TransitionOp::ThreeD(op)) if op.kind == gpu_3d::ThreeDKind::Slice && gpu_3d::gpu_ok(op) => {
                let packed = gpu_3d::pack(op).ok_or_else(|| TransitionCatalogError::InvalidPlan { id: plan.id.clone() })?;
                if packed[0] != plan.progress.to_bits() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words = packed;
                packed[1]
            }
            ("spinback_3d", TransitionOp::ThreeD(op)) if op.kind == gpu_3d::ThreeDKind::Spinback3d && gpu_3d::gpu_ok(op) => {
                let packed = gpu_3d::pack(op).ok_or_else(|| TransitionCatalogError::InvalidPlan { id: plan.id.clone() })?;
                if packed[0] != plan.progress.to_bits() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words = packed;
                packed[1]
            }
            (id, TransitionOp::VrLeaks { amount, seed, chroma }) if vr_kernel(id) == Some(if *chroma { 113 } else { 114 }) => {
                words[2] = *seed;
                words[3] = u32::from(*chroma);
                words[12] = amount.to_bits();
                if *chroma { 113 } else { 114 }
            }
            ("vr_light_rays", TransitionOp::VrLightRays { center, amount }) => {
                words[3] = u32::from(center[0].is_some()) | (u32::from(center[1].is_some()) << 1);
                words[12] = center[0].unwrap_or(0.0).to_bits();
                words[13] = center[1].unwrap_or(0.0).to_bits();
                words[14] = amount.to_bits();
                115
            }
            ("vr_mobius_zoom", TransitionOp::VrMobiusZoom { zoom, twist_radians }) => {
                words[12] = zoom.to_bits();
                words[13] = twist_radians.to_bits();
                116
            }
            ("frame", TransitionOp::Frame { direction, frame_width_px, frame_color_linear, content_scale }) => {
                if *direction > 3 {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words[2] = *direction;
                words[4] = frame_width_px.to_bits();
                words[5] = content_scale.to_bits();
                for (word, color) in words[8..12].iter_mut().zip(frame_color_linear) {
                    *word = color.to_bits();
                }
                63
            }
            ("vr_random_blocks", TransitionOp::VrRandomBlocks { block_size_px, softness, seed }) => {
                words[2] = *seed;
                words[4] = block_size_px.to_bits();
                words[5] = softness.to_bits();
                112
            }
            ("text_animator", TransitionOp::TextCells { columns, rows, typewriter: false, .. }) => {
                words[4] = columns.to_bits();
                words[5] = rows.to_bits();
                102
            }
            ("typewriter", TransitionOp::TextCells { columns, rows, typewriter: true, cursor }) => {
                words[2] = u32::from(*cursor);
                words[4] = columns.to_bits();
                words[5] = rows.to_bits();
                103
            }
            ("center_split", TransitionOp::CenterSplit) => 100,
            ("band_slide", TransitionOp::BandSlide) => 101,
            ("cross_dissolve" | "cross_dissolve_legacy", TransitionOp::CrossDissolve) => 0,
            ("additive_dissolve" | "additive_dissolve_legacy", TransitionOp::AdditiveDissolve) => 1,
            ("film_dissolve" | "film_dissolve_legacy", TransitionOp::FilmDissolve) => 2,
            ("dip_to_black" | "dip_to_black_legacy", TransitionOp::DipToBlack) => 3,
            ("dip_to_white" | "dip_to_white_legacy", TransitionOp::DipToWhite) => 4,
            ("push", TransitionOp::Push(p)) => {
                words[2] = match p.direction {
                    PushDirection::FromNorth => 0,
                    PushDirection::FromEast => 1,
                    PushDirection::FromSouth => 2,
                    PushDirection::FromWest => 3,
                };
                words[4] = p.motion_blur.to_bits();
                5
            }
            ("flare", TransitionOp::Flare(p)) => {
                let (sin, cos) = (p.angle_degrees + 90.0).to_radians().sin_cos();
                words[5] = p.amount.to_bits();
                words[6] = sin.to_bits();
                words[7] = (-cos).to_bits();
                for (word, color) in words[8..11].iter_mut().zip(p.color_linear) {
                    *word = color.to_bits();
                }
                6
            }
            ("linear_wipe", TransitionOp::LinearWipe(p)) => {
                if p.antialias < 0.0 || !p.antialias.is_finite() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words[2] = p.antialias.to_bits();
                words[4] = p.feather.to_bits();
                words[5] = p.border_width.to_bits();
                let (dx, dy) = wipe_direction(p.angle_degrees);
                words[6] = dx.to_bits();
                words[7] = dy.to_bits();
                for (word, color) in words[8..11].iter_mut().zip(p.border_color_linear) {
                    *word = color.to_bits();
                }
                words[11] = 1.0f32.to_bits();
                7
            }
            ("soft_wipe", TransitionOp::SoftWipe(p)) => {
                if p.antialias < 0.0 || !p.antialias.is_finite() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words[2] = p.antialias.to_bits();
                words[4] = p.feather.to_bits();
                let (dx, dy) = wipe_direction(p.angle_degrees);
                words[6] = dx.to_bits();
                words[7] = dy.to_bits();
                8
            }
            ("wipe", TransitionOp::LegacyWipe { direction }) => {
                if *direction > 3 {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words[2] = *direction;
                9
            }
            ("dip_to_color", TransitionOp::DipToColor { color_linear, hold }) => {
                words[4] = hold.to_bits();
                for (word, color) in words[8..11].iter_mut().zip(color_linear) {
                    *word = color.to_bits();
                }
                words[11] = 1.0f32.to_bits();
                10
            }
            ("luma_fade", TransitionOp::LumaFade { source, softness, invert }) => {
                if *source > 1 {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words[2] = *source;
                words[3] = u32::from(*invert);
                words[4] = softness.to_bits();
                11
            }
            ("non_additive_dissolve", TransitionOp::NonAdditiveDissolve) => 12,
            ("clock_wipe_legacy" | "radial_wipe_legacy", TransitionOp::LegacyClockWipe) => 13,
            ("venetian_blinds", TransitionOp::VenetianBlinds) => 14,
            ("checker_wipe", TransitionOp::CheckerWipe) => 15,
            ("gradient_wipe", TransitionOp::GradientWipe { softness }) => {
                words[4] = softness.to_bits();
                16
            }
            ("burn_alpha", TransitionOp::BurnAlpha { softness, seed, color_linear }) => {
                words[2] = *seed;
                words[4] = softness.to_bits();
                for (word, color) in words[8..11].iter_mut().zip(color_linear) {
                    *word = color.to_bits();
                }
                words[11] = 1.0f32.to_bits();
                17
            }
            ("mosaic_transition", TransitionOp::Mosaic { block_size_px, dissolve }) => {
                words[2] = u32::from(*dissolve);
                words[4] = block_size_px.to_bits();
                18
            }
            ("shape_dissolve", TransitionOp::ShapeDissolve { shape, size_px, softness_px, rotation_degrees, order, seed }) => {
                let (w, h) = size.unwrap_or((1, 1));
                let cols = ((w as f32 / *size_px).ceil()).max(1.0);
                let rows = ((h as f32 / *size_px).ceil()).max(1.0);
                let diag = ((cols / 2.0).powi(2) + (rows / 2.0).powi(2)).sqrt().max(1.0);
                let field = |x, y| shape_dissolve_field(*shape, *size_px, *rotation_degrees, *order, *seed, cols, rows, diag, x, y);
                let (lo, hi) = field_range(w as f32, h as f32, field);
                words[2] = *shape;
                words[3] = 1.0f32.to_bits();
                words[4] = softness_px.to_bits();
                words[5] = *order;
                words[6] = rotation_degrees.to_bits();
                words[7] = *seed;
                words[12] = lo.to_bits();
                words[13] = hi.to_bits();
                words[14] = size_px.to_bits();
                words[15] = cols.to_bits();
                words[16] = rows.to_bits();
                words[17] = diag.to_bits();
                19
            }
            ("shape_flow", TransitionOp::ShapeFlow { shape, direction, softness_px }) => {
                if *shape > 6 || *direction > 3 {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words[2] = *shape;
                words[3] = 1.0f32.to_bits();
                words[4] = softness_px.to_bits();
                words[5] = *direction;
                97
            }
            (id, TransitionOp::Light(light)) if LightOp::matches_id(id) && light_kernel(id) == Some(light.kind as u32) && light.gpu_ok() => {
                let packed = light.pack();
                if packed[0] != plan.progress.to_bits() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words = packed;
                packed[1]
            }
            (id, TransitionOp::Motion(op)) if gpu_motion::matches_id(id) && motion_kernel(id) == Some(op.kind as u32) && gpu_motion::gpu_ok(op) => {
                let packed = op.pack();
                if packed[0] != plan.progress.to_bits() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words = packed;
                packed[1]
            }
            (id, TransitionOp::Grunge(op)) if gpu_grunge::matches_id(id) && grunge_kernel(id) == Some(op.kind as u32) && gpu_grunge::gpu_ok(op) => {
                let packed = op.pack();
                if packed[0] != plan.progress.to_bits() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words = packed;
                packed[1]
            }
            (id, TransitionOp::Card(op)) if card_kernel(id) == Some(op.kind as u32) && gpu_cards::gpu_ok(op) => {
                let packed = op.pack();
                if packed[0] != plan.progress.to_bits() {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                words = packed;
                packed[1]
            }
            ("clock_wipe", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 20, size.unwrap_or((1, 1)));
                20
            }
            ("radial_wipe", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 21, size.unwrap_or((1, 1)));
                21
            }
            ("star_wipe", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 22, size.unwrap_or((1, 1)));
                22
            }
            ("panel_wipe", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 23, size.unwrap_or((1, 1)));
                23
            }
            ("plateau_wipe", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 24, size.unwrap_or((1, 1)));
                24
            }
            ("barn_doors", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 25, size.unwrap_or((1, 1)));
                25
            }
            ("inset", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 26, size.unwrap_or((1, 1)));
                26
            }
            ("iris_round", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 27, size.unwrap_or((1, 1)));
                27
            }
            ("iris_box", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 28, size.unwrap_or((1, 1)));
                28
            }
            ("iris_diamond", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 29, size.unwrap_or((1, 1)));
                29
            }
            ("iris_cross", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 30, size.unwrap_or((1, 1)));
                30
            }
            ("vr_iris_wipe", TransitionOp::FieldWipe(p)) => {
                pack_field_wipe(&mut words, p, 98, size.unwrap_or((1, 1)));
                98
            }
            ("vr_gradient_wipe", TransitionOp::VrGradientWipe { softness, invert }) => {
                words[2] = u32::from(*invert);
                words[4] = softness.to_bits();
                99
            }
            ("stretch_wipe", TransitionOp::StretchWipe { direction, stretch_px }) => {
                if *direction > 3 {
                    return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
                }
                let (w, h) = size.unwrap_or((1, 1));
                let (dx, dy) = wipe_choice_direction(*direction);
                let (lo, hi) = field_range(w as f32, h as f32, |x, y| x * dx + y * dy);
                words[2] = *direction;
                words[4] = stretch_px.to_bits();
                words[6] = dx.to_bits();
                words[7] = dy.to_bits();
                words[12] = lo.to_bits();
                words[13] = hi.to_bits();
                31
            }
            ("neon_wipe", TransitionOp::NeonWipe { angle_degrees, glow_px, amount, color_linear, antialias }) => {
                let (w, h) = size.unwrap_or((1, 1));
                let (dx, dy) = direction(*angle_degrees);
                let (lo, hi) = field_range(w as f32, h as f32, |x, y| x * dx + y * dy);
                words[3] = antialias.to_bits();
                words[4] = glow_px.to_bits();
                words[5] = amount.to_bits();
                words[6] = dx.to_bits();
                words[7] = dy.to_bits();
                for (word, color) in words[8..11].iter_mut().zip(color_linear) {
                    *word = color.to_bits();
                }
                words[11] = 1.0f32.to_bits();
                words[12] = lo.to_bits();
                words[13] = hi.to_bits();
                96
            }
            (
                "push"
                | "flare"
                | "cross_dissolve"
                | "center_split"
                | "band_slide"
                | "text_animator"
                | "typewriter"
                | "vr_random_blocks"
                | "cross_dissolve_legacy"
                | "additive_dissolve"
                | "additive_dissolve_legacy"
                | "film_dissolve"
                | "film_dissolve_legacy"
                | "dip_to_black"
                | "dip_to_black_legacy"
                | "dip_to_white"
                | "dip_to_white_legacy"
                | "linear_wipe"
                | "soft_wipe"
                | "wipe"
                | "dip_to_color"
                | "luma_fade"
                | "non_additive_dissolve"
                | "clock_wipe_legacy"
                | "radial_wipe_legacy"
                | "venetian_blinds"
                | "checker_wipe"
                | "gradient_wipe"
                | "clock_wipe"
                | "radial_wipe"
                | "star_wipe"
                | "panel_wipe"
                | "plateau_wipe"
                | "barn_doors"
                | "inset"
                | "iris_round"
                | "iris_box"
                | "iris_diamond"
                | "iris_cross"
                | "vr_iris_wipe"
                | "vr_gradient_wipe"
                | "stretch_wipe"
                | "neon_wipe"
                | "burn_alpha"
                | "mosaic_transition"
                | "shape_dissolve"
                | "shape_flow"
                | "burn_chroma"
                | "chroma_leak"
                | "cross_zoom"
                | "directional_blur_transition"
                | "flash"
                | "lens_blur"
                | "light_leak"
                | "light_sweep"
                | "phosphor"
                | "radial_blur"
                | "solarize"
                | "stripe"
                | "zoom_blur"
                | "glow"
                | "ray"
                | "cross_zoom_legacy"
                | "push_legacy"
                | "slide_legacy"
                | "split_legacy"
                | "page_turn"
                | "motion_camera"
                | "roll"
                | "film_roll"
                | "stretch"
                | "slide"
                | "split"
                | "motion_tween"
                | "spring_motion"
                | "pop_motion",
                _,
            ) => {
                return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
            }
            _ => return Err(TransitionCatalogError::Unsupported { id: plan.id.clone() }),
        };
        words[0] = plan.progress.to_bits();
        words[1] = kernel;
        let extra_fields_valid = match &plan.op {
            TransitionOp::FieldWipe(_) => words[12..22].iter().all(|w| f32::from_bits(*w).is_finite()),
            TransitionOp::StretchWipe { .. } | TransitionOp::NeonWipe { .. } => words[12..14].iter().all(|w| f32::from_bits(*w).is_finite()),
            TransitionOp::ShapeDissolve { .. } => words[12..18].iter().all(|w| f32::from_bits(*w).is_finite()),
            TransitionOp::Light(_) => [0, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25].iter().all(|i| f32::from_bits(words[*i]).is_finite()),
            TransitionOp::Motion(_) => words[12..20].iter().all(|w| f32::from_bits(*w).is_finite()),
            TransitionOp::Grunge(_) => words[12..20].iter().all(|w| f32::from_bits(*w).is_finite()),
            TransitionOp::Card(_) => words[12..14].iter().all(|w| f32::from_bits(*w).is_finite()),
            TransitionOp::Blur(_) => [12].iter().all(|w| f32::from_bits(words[*w]).is_finite()),
            TransitionOp::ThreeD(_) => words[12..14].iter().all(|w| f32::from_bits(*w).is_finite()),
            TransitionOp::VrLeaks { .. } => f32::from_bits(words[12]).is_finite(),
            TransitionOp::VrLightRays { .. } | TransitionOp::VrMobiusZoom { .. } => words[12..15].iter().all(|w| f32::from_bits(*w).is_finite()),
            _ => true,
        };
        if !plan.progress.is_finite()
            || !(0.0..=1.0).contains(&plan.progress)
            || !plan.scale.is_finite()
            || plan.scale <= 0.0
            || words[4..12].iter().any(|w| !f32::from_bits(*w).is_finite())
            || !extra_fields_valid
        {
            return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
        }
        Ok(words)
    }

    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: &wgpu::TextureView,
        inputs: [&wgpu::TextureView; 2],
        size: (u32, u32),
        plan: &PlannedTransition,
    ) -> Result<(), TransitionCatalogError> {
        let params = Self::params(plan, Some(size))?;
        if size.0 == 0 || size.1 == 0 {
            return Err(TransitionCatalogError::InvalidSize);
        }
        if let TransitionOp::Blur(op) = &plan.op {
            if op.progress.to_bits() != plan.progress.to_bits() {
                return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
            }
            return self.blur.lock().unwrap_or_else(std::sync::PoisonError::into_inner).draw(device, queue, canvas, inputs, size, *op);
        }
        if let TransitionOp::Light(op) = &plan.op {
            if op.progress.to_bits() != plan.progress.to_bits() || !op.gpu_ok() {
                return Err(TransitionCatalogError::InvalidPlan { id: plan.id.clone() });
            }
            if matches!(op.kind, LightKind::Glow | LightKind::Ray | LightKind::CrossZoomLegacy) {
                return self.lights.lock().unwrap_or_else(std::sync::PoisonError::into_inner).draw(device, queue, canvas, inputs, size, op);
            }
        }
        let bytes: Vec<u8> = params.iter().flat_map(|v| v.to_le_bytes()).collect();
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("transition-catalog"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transition-catalog"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(inputs[0]) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(inputs[1]) },
                wgpu::BindGroupEntry { binding: 2, resource: uniform.as_entire_binding() },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("transition-catalog") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("transition-catalog"),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
        Ok(())
    }
}

fn wipe_direction(angle_degrees: f32) -> (f32, f32) {
    let radians = angle_degrees.to_radians();
    (radians.sin(), -radians.cos())
}
