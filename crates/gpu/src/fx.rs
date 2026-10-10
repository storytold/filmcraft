//! The GPU effect stage: standard effects of a plan layer ([`LayerFx`]) run in WGSL (`fx.wgsl`).
//!
//! A layer with effects is first drawn from its source (YUV planes or RGBA, through the
//! compositor's own `fs` shader, so decoding is the same as for a plain layer) into a working
//! texture of the size the CPU decodes it at. Consecutive point operations share a compute pass;
//! spatial operations use separate passes and masks preserve their original in up to four textures;
//! the last one is drawn into the accumulator with the layer's matrix, opacity and blend mode
//! like any RGBA layer. Working textures are `Rgba32Float` (the CPU reference's precision) and
//! pooled per size; layers without effects never touch any of this.
//!
//! Box blurs (Gaussian Blur, Camera Blur, the blur inside Unsharp / Sharpen) run per pixel for
//! radii up to [`BOX_PER_PIXEL_MAX`] and as a running sum per row / column above, so a huge
//! radius costs O(1) per pixel like on the CPU (radii are capped by `gaussian_boxes`).

use std::collections::{HashMap, HashSet};

use filmcraft_render::gpufx::FxOp;
use filmcraft_render::plan::LayerFx;

/// Working texture format.
pub(crate) const FX_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
/// Box radii up to this many pixels are summed per output pixel; larger ones as a running sum.
pub const BOX_PER_PIXEL_MAX: u32 = 32;

const OP_BRIGHTNESS_CONTRAST: u32 = 1;
const OP_PROC_AMP: u32 = 2;
const OP_TINT: u32 = 3;
const OP_BLACK_WHITE: u32 = 4;
const OP_COLOR_BALANCE: u32 = 5;
const OP_LEAVE_COLOR: u32 = 6;
const OP_CHANGE_TO_COLOR: u32 = 7;
const OP_COLOR_PASS: u32 = 8;
const OP_GAMMA: u32 = 9;
const OP_LEVELS: u32 = 10;
const OP_EXTRACT: u32 = 11;
const OP_INVERT: u32 = 12;
const OP_INVERT_ALPHA: u32 = 13;
const OP_POSTERIZE: u32 = 14;
const OP_ASC_CDL: u32 = 15;
const OP_CHANNEL_MIX: u32 = 16;
const OP_COLOR_REPLACE: u32 = 17;
const OP_ALPHA_ADJUST: u32 = 18;
const OP_VIGNETTE: u32 = 19;
const OP_BOX: u32 = 20;
const OP_DIRECTIONAL: u32 = 22;
const OP_UNSHARP: u32 = 23;
const OP_CROP: u32 = 24;
const OP_RESAMPLE: u32 = 25;
const OP_HFLIP: u32 = 26;
const OP_VFLIP: u32 = 27;
const OP_MIRROR: u32 = 28;
const OP_OFFSET: u32 = 29;
const OP_VIDEO_LIMITER: u32 = 30;
const OP_LUMETRI: u32 = 31;
const OP_CHROMA_KEY: u32 = 32;
const OP_LUMA_KEY: u32 = 33;
const OP_ULTRA_KEY: u32 = 34;
const OP_MASK_MIX: u32 = 35;
const OP_MASK_ALPHA: u32 = 36;
const OP_LUT: u32 = 37;
const OP_GRADE: u32 = 38;
const OP_HSL: u32 = 39;
const OP_HSL_MASK: u32 = 40;
const OP_MEDIAN: u32 = 41;
const OP_HSL_COMBINE: u32 = 42;

type Target = (wgpu::Texture, wgpu::TextureView);

pub(crate) struct FxStage {
    px: wgpu::ComputePipeline,
    run: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
    #[cfg(not(target_arch = "wasm32"))]
    layout: wgpu::PipelineLayout,
    #[cfg(not(target_arch = "wasm32"))]
    specialised: HashMap<Vec<u32>, Specialised>,
    masks: crate::mask_cache::MaskCache,
    tiled: wgpu::ComputePipeline,
    #[cfg(test)]
    optimise: bool,
    /// Draws a layer's source into a working texture (`composite.wgsl` `fs`, no blending).
    source: wgpu::RenderPipeline,
    pool: HashMap<(u32, u32), Vec<Target>>,
    used: HashSet<(u32, u32)>,
    #[cfg(test)]
    fuse: bool,
}

#[cfg(not(target_arch = "wasm32"))]
enum Specialised {
    Pending(std::sync::mpsc::Receiver<Option<wgpu::ComputePipeline>>),
    Ready(wgpu::ComputePipeline),
    Failed,
}
#[cfg(not(target_arch = "wasm32"))]
static COMPILERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[cfg(not(target_arch = "wasm32"))]
struct CompilePermit;
#[cfg(not(target_arch = "wasm32"))]
impl Drop for CompilePermit {
    fn drop(&mut self) {
        COMPILERS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// One compute pass of a job.
struct Dispatch {
    run: bool,
    pipeline: Option<wgpu::ComputePipeline>,
    bg: wgpu::BindGroup,
    groups: (u32, u32),
}

/// The recorded work of one layer's effects (bind groups made; recorded into the frame's encoder
/// right before the layer is drawn, so layers can share pooled textures).
pub(crate) struct FxJob {
    source_bg: wgpu::BindGroup,
    source_target: wgpu::TextureView,
    dispatches: Vec<Dispatch>,
    /// The texture holding the result.
    pub(crate) result: wgpu::TextureView,
    pub(crate) result_texture: wgpu::Texture,
}

/// One pass before texture routing: entry point, op code, integer and float parameters.
struct Step {
    run: bool,
    code: u32,
    i1: [u32; 4],
    p: [f32; 24],
    /// Unsharp combine: reads the blurred image and the original.
    combine: bool,
    /// A blur pass of Unsharp Mask (the original stays untouched until the combine).
    unsharp_blur: bool,
    mask_start: bool,
    data: Vec<[f32; 4]>,
    masks: Vec<filmcraft_render::mask::FlatMask>,
}

fn step(code: u32, i1: [u32; 4], p: &[f32]) -> Step {
    let mut q = [0.0; 24];
    for (d, s) in q.iter_mut().zip(p) {
        *d = *s;
    }
    Step { run: false, code, i1, p: q, combine: false, unsharp_blur: false, mask_start: false, data: Vec::new(), masks: Vec::new() }
}

/// Box passes along x (vertical = false) or y for the radii (radius 0 is a no-op on the CPU).
fn boxes(radii: &[u32], vertical: bool, repeat: bool, out: &mut Vec<Step>) {
    for &r in radii.iter().filter(|r| **r > 0) {
        let mut s = step(OP_BOX, [r, repeat as u32, vertical as u32, 0], &[]);
        s.run = r > BOX_PER_PIXEL_MAX;
        out.push(s);
    }
}

/// The passes of an op (empty: no-op). Unsharp's blur passes come first, then the combine.
fn steps(op: &FxOp) -> Vec<Step> {
    let mut out = Vec::new();
    match op {
        FxOp::Chain(ops) => {
            for op in ops {
                out.extend(steps(op));
            }
        }
        FxOp::Lut { lut } => {
            let mut s = step(OP_LUT, [0; 4], &[]);
            pack_lut(lut, &mut s.data);
            out.push(s);
        }
        FxOp::Grade(grade) => {
            let mask = grade.curves.iter().enumerate().fold(0u32, |mask, (i, t)| mask | (u32::from(t.is_some()) << i));
            let wheels = grade.wheels.unwrap_or([[0.0; 3]; 3]);
            let mut s = step(
                OP_GRADE,
                [mask, grade.look, u32::from(grade.wheels.is_some()), 0],
                &[
                    grade.intensity,
                    0.0,
                    0.0,
                    0.0,
                    wheels[0][0],
                    wheels[0][1],
                    wheels[0][2],
                    0.0,
                    wheels[1][0],
                    wheels[1][1],
                    wheels[1][2],
                    0.0,
                    wheels[2][0],
                    wheels[2][1],
                    wheels[2][2],
                ],
            );
            for table in &grade.curves {
                for i in 0..filmcraft_render::grading::CURVE_SIZE {
                    s.data.push([table.as_ref().and_then(|t| t.get(i)).copied().unwrap_or(0.0), 0.0, 0.0, 0.0]);
                }
            }
            if let Some(lut) = &grade.look_lut {
                s.i1[1] = 9;
                pack_lut(lut, &mut s.data);
            }
            out.push(s);
        }
        FxOp::Hsl { params, output, radius, rx, ry } => {
            if params[11] > 0.0 || params[10] >= 0.3 {
                let mut mask = step(OP_HSL_MASK, [0; 4], params);
                mask.unsharp_blur = true;
                out.push(mask);
                if params[11] > 0.0 {
                    let mut median = step(OP_MEDIAN, [*radius, 0, 0, 0], &[params[11]]);
                    median.unsharp_blur = true;
                    out.push(median);
                }
                let start = out.len();
                boxes(rx, false, true, &mut out);
                boxes(ry, true, true, &mut out);
                for s in &mut out[start..] {
                    s.unsharp_blur = true;
                }
                let mut combine = step(OP_HSL_COMBINE, [*output, 0, 0, 0], params);
                combine.combine = true;
                out.push(combine);
            } else {
                out.push(step(OP_HSL, [*output, 0, 0, 0], params));
            }
        }
        FxOp::Masked { op, masks } => {
            out = steps(op);
            if let Some(first) = out.first_mut() {
                first.mask_start = true;
            }
            if !out.is_empty() {
                let mut mix = step(OP_MASK_MIX, [masks.len() as u32, 0, 0, 0], &[]);
                mix.masks = masks.clone();
                out.push(mix);
            }
        }
        FxOp::OpacityMask { masks } => {
            let mut alpha = step(OP_MASK_ALPHA, [masks.len() as u32, 0, 0, 0], &[]);
            alpha.masks = masks.clone();
            out.push(alpha);
        }
        FxOp::UltraKey { params, dominant, output } => out.push(step(OP_ULTRA_KEY, [*dominant, *output, 0, 0], params)),
        FxOp::ChromaKey { key_ycc, tolerance, softness, spill, dominant, output } => {
            out.push(step(OP_CHROMA_KEY, [*dominant, *output, 0, 0], &[key_ycc[0], key_ycc[1], key_ycc[2], *tolerance, *softness, *spill]));
        }
        FxOp::LumaKey { threshold, cutoff } => out.push(step(OP_LUMA_KEY, [0; 4], &[*threshold, *cutoff])),
        FxOp::BrightnessContrast { br, co } => out.push(step(OP_BRIGHTNESS_CONTRAST, [0; 4], &[*br, *co])),
        FxOp::ProcAmp { br, co, hue, sat } => out.push(step(OP_PROC_AMP, [0; 4], &[*br, *co, *hue, *sat])),
        FxOp::Tint { black, white, amount } => {
            out.push(step(OP_TINT, [0; 4], &[black[0], black[1], black[2], *amount, white[0], white[1], white[2]]));
        }
        FxOp::BlackWhite => out.push(step(OP_BLACK_WHITE, [0; 4], &[])),
        FxOp::ColorBalance { sh, md, hi, preserve } => {
            out.push(step(OP_COLOR_BALANCE, [*preserve as u32, 0, 0, 0], &[sh[0], sh[1], sh[2], 0.0, md[0], md[1], md[2], 0.0, hi[0], hi[1], hi[2]]))
        }
        FxOp::LeaveColor { amount, key_hue, tol, soft } => out.push(step(OP_LEAVE_COLOR, [0; 4], &[*amount, *key_hue, *tol, *soft])),
        FxOp::ChangeToColor { from_hue, to_hue, tol, soft } => out.push(step(OP_CHANGE_TO_COLOR, [0; 4], &[*from_hue, *to_hue, *tol, *soft])),
        FxOp::ColorPass { key, sim, reverse } => out.push(step(OP_COLOR_PASS, [*reverse as u32, 0, 0, 0], &[key[0], key[1], key[2], *sim])),
        FxOp::Gamma { g } => out.push(step(OP_GAMMA, [0; 4], &[*g])),
        FxOp::Levels { ib, iw, ob, ow, g } => out.push(step(OP_LEVELS, [0; 4], &[*ib, *iw, *ob, *ow, *g])),
        FxOp::Extract { lo, hi, soft, invert } => out.push(step(OP_EXTRACT, [*invert as u32, 0, 0, 0], &[*lo, *hi, *soft])),
        FxOp::Invert { channel: 4, blend } => out.push(step(OP_INVERT_ALPHA, [0; 4], &[*blend])),
        FxOp::Invert { channel, blend } => out.push(step(OP_INVERT, [*channel, 0, 0, 0], &[*blend])),
        FxOp::Posterize { n } => out.push(step(OP_POSTERIZE, [0; 4], &[*n])),
        FxOp::Gaussian { rx, ry, repeat } => {
            boxes(rx, false, *repeat, &mut out);
            boxes(ry, true, *repeat, &mut out);
        }
        FxOp::DirectionalBlur { dx, dy, steps } => {
            if *steps >= 2 {
                out.push(step(OP_DIRECTIONAL, [*steps, 0, 0, 0], &[*dx, *dy]));
            }
        }
        FxOp::Unsharp { rx, ry, amount, threshold } => {
            boxes(rx, false, true, &mut out);
            boxes(ry, true, true, &mut out);
            for s in &mut out {
                s.unsharp_blur = true;
            }
            let mut s = step(OP_UNSHARP, [0; 4], &[*amount, *threshold]);
            s.combine = true;
            out.push(s);
        }
        FxOp::Crop { x0, x1, y0, y1, feather } => out.push(step(OP_CROP, [0; 4], &[*x0, *x1, *y0, *y1, *feather])),
        FxOp::Resample(r) => {
            let op = if (r.opacity - 1.0).abs() < 1e-6 { 1.0 } else { r.opacity };
            match &r.inv {
                Some(m) => out.push(step(OP_RESAMPLE, r.rect, &[m.a as f32, m.b as f32, m.c as f32, m.d as f32, m.e as f32, m.f as f32, op, 1.0])),
                None => out.push(step(OP_RESAMPLE, [0; 4], &[0.0; 8])),
            }
        }
        FxOp::HFlip => out.push(step(OP_HFLIP, [0; 4], &[])),
        FxOp::VFlip => out.push(step(OP_VFLIP, [0; 4], &[])),
        FxOp::Mirror { cx, cy, nx, ny } => out.push(step(OP_MIRROR, [0; 4], &[*cx, *cy, *nx, *ny])),
        FxOp::Offset { dx, dy, blend } => out.push(step(OP_OFFSET, [0; 4], &[*dx, *dy, *blend])),
        FxOp::AscCdl { slope: s, offset: o, power: p, sat } => {
            out.push(step(OP_ASC_CDL, [0; 4], &[s[0], s[1], s[2], *sat, o[0], o[1], o[2], 0.0, p[0], p[1], p[2]]));
        }
        FxOp::ChannelMix { m } => {
            let p: Vec<f32> = m.iter().flatten().copied().collect();
            out.push(step(OP_CHANNEL_MIX, [0; 4], &p));
        }
        FxOp::ColorReplace { sim, solid, target: t, replace: r, replace_hsl: h } => {
            out.push(step(OP_COLOR_REPLACE, [*solid as u32, 0, 0, 0], &[t[0], t[1], t[2], *sim, r[0], r[1], r[2], 0.0, h[0], h[1], h[2]]))
        }
        FxOp::AlphaAdjust { opacity, ignore, invert, mask_only } => {
            out.push(step(OP_ALPHA_ADJUST, [*ignore as u32, *invert as u32, *mask_only as u32, 0], &[*opacity]));
        }
        FxOp::Vignette { amount, midpoint, roundness, feather, target } => {
            if amount.abs() >= 1e-5 {
                out.push(step(OP_VIGNETTE, [0; 4], &[*amount, *midpoint, *roundness, *feather, target[0], target[1], target[2]]));
            }
        }
        FxOp::VideoLimiter { max, comp, axis, warn, warning_color } => {
            out.push(step(OP_VIDEO_LIMITER, [*axis, *warn as u32, 0, 0], &[*max, *comp, warning_color[0], warning_color[1], warning_color[2]]));
        }
        FxOp::Lumetri {
            gains,
            exposure,
            contrast,
            hl,
            sh,
            wh,
            bl,
            sat,
            creative_on,
            faded,
            vib,
            st,
            ht,
            vignette_on,
            va,
            vmid,
            vround,
            vfeather,
            aspect,
            ..
        } => {
            let ge = [gains[0] * exposure, gains[1] * exposure, gains[2] * exposure];
            let b0 = -bl * 0.15;
            let w0 = 1.0 - wh * 0.15;
            let sh_k = sh * 0.35;
            let hl_k = hl * 0.35;
            let vround_aspect = if *vround < 0.0 { aspect.powf(-vround) } else { 1.0 };
            let st_k = [(st[0] - 0.5) * 0.3, (st[1] - 0.5) * 0.3, (st[2] - 0.5) * 0.3];
            let ht_k = [(ht[0] - 0.5) * 0.3, (ht[1] - 0.5) * 0.3, (ht[2] - 0.5) * 0.3];

            let p = [
                ge[0],
                ge[1],
                ge[2],
                b0,
                w0,
                sh_k,
                hl_k,
                *contrast,
                *faded,
                *sat,
                *vib,
                *va,
                *vmid,
                *vfeather,
                vround_aspect,
                0.0,
                st_k[0],
                st_k[1],
                st_k[2],
                0.0,
                ht_k[0],
                ht_k[1],
                ht_k[2],
                0.0,
            ];
            out.push(step(OP_LUMETRI, [*creative_on as u32, *vignette_on as u32, 0, 0], &p));
        }
    }
    out
}

fn pack_lut(lut: &filmcraft_color::Lut, data: &mut Vec<[f32; 4]>) {
    let shaper = lut.shaper.as_ref();
    let cube = lut.cube.as_ref();
    data.push([shaper.map_or(0, |l| l.data.len()) as f32, cube.map_or(0, |l| l.size) as f32, 0.0, 0.0]);
    for v in [
        shaper.map_or([0.0; 3], |l| l.domain_min),
        shaper.map_or([1.0; 3], |l| l.domain_max),
        cube.map_or([0.0; 3], |l| l.domain_min),
        cube.map_or([1.0; 3], |l| l.domain_max),
    ] {
        data.push([v[0], v[1], v[2], 0.0]);
    }
    for c in shaper.into_iter().flat_map(|l| &l.data).chain(cube.into_iter().flat_map(|l| &l.data)) {
        data.push([c[0], c[1], c[2], 0.0]);
    }
}

/// Uniform batches fit even the minimum WebGPU uniform binding limit. Spatial passes are
/// barriers: subsequent point operations must see their completed image, not the old source.
const MAX_FUSED: usize = 16;
#[cfg(not(target_arch = "wasm32"))]
const SPECIALISE_LOOP: &str = "for (var i = 0u; i < batch.header.x; i++) {\n        u = batch.ops[i];\n        value = pixel(u.i0.x, p, value);\n    }";

fn pointwise(s: &Step) -> bool {
    !s.mask_start
        && matches!(s.code, 1..=19 | OP_CROP | OP_VIDEO_LIMITER | OP_LUMETRI | OP_CHROMA_KEY | OP_LUMA_KEY | OP_ULTRA_KEY | OP_LUT | OP_GRADE | OP_HSL)
}

fn batches(steps: &[Step]) -> Vec<&[Step]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < steps.len() {
        let mut end = at + 1;
        if pointwise(&steps[at]) {
            while end < steps.len() && end - at < MAX_FUSED && pointwise(&steps[end]) {
                end += 1;
            }
        }
        out.push(&steps[at..end]);
        at = end;
    }
    out
}

impl FxStage {
    #[cfg(test)]
    pub(crate) fn set_fusion(&mut self, enabled: bool) {
        self.fuse = enabled;
    }

    #[cfg(test)]
    pub(crate) fn set_optimise(&mut self, enabled: bool) {
        self.optimise = enabled;
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn compiled_variants(&self) -> usize {
        self.specialised.values().filter(|v| matches!(v, Specialised::Ready(_))).count()
    }

    #[cfg(target_arch = "wasm32")]
    fn specialised(&mut self, _device: &wgpu::Device, _batch: &[Step]) -> Option<wgpu::ComputePipeline> {
        None
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn specialised(&mut self, device: &wgpu::Device, batch: &[Step]) -> Option<wgpu::ComputePipeline> {
        #[cfg(test)]
        if !self.optimise {
            return None;
        }
        if batch.len() < 2 || !batch.iter().all(pointwise) {
            return None;
        }
        let key: Vec<u32> = batch.iter().map(|s| s.code).collect();
        if let Some(entry) = self.specialised.get_mut(&key) {
            match entry {
                Specialised::Ready(p) => return Some(p.clone()),
                Specialised::Failed => return None,
                Specialised::Pending(rx) => match rx.try_recv() {
                    Ok(Some(p)) => {
                        *entry = Specialised::Ready(p.clone());
                        return Some(p);
                    }
                    Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => *entry = Specialised::Failed,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                },
            }
            return None;
        }
        // Bound both driver memory and concurrent compilation across playback/export devices.
        // Keyframes update uniforms; only a change of operation order creates a new variant.
        if self.specialised.len() >= 64
            || COMPILERS.fetch_update(std::sync::atomic::Ordering::Relaxed, std::sync::atomic::Ordering::Relaxed, |n| (n < 2).then_some(n + 1)).is_err()
        {
            return None;
        }
        let permit = CompilePermit;
        let body = key.iter().enumerate().map(|(i, code)| format!("u = batch.ops[{i}u]; value = pixel({code}u, p, value);\n")).collect::<String>();
        let source = include_str!("fx.wgsl").replace(SPECIALISE_LOOP, &body);
        let device = device.clone();
        let layout = self.layout.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new().name("gpu-fx-compile".into()).spawn(move || {
            let _permit = permit;
            let compiled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let shader = device
                    .create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("specialised-fx"), source: wgpu::ShaderSource::Wgsl(source.into()) });
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some("specialised-fx"),
                    layout: Some(&layout),
                    module: &shader,
                    entry_point: Some("fx_px"),
                    compilation_options: Default::default(),
                    cache: None,
                })
            }))
            .ok();
            if compiled.is_none() {
                log::warn!("effect shader compilation failed; using generic GPU pipeline");
            }
            let _ = tx.send(compiled);
        });
        if let Err(err) = worker {
            log::warn!("cannot start effect shader compiler: {err}");
            self.specialised.insert(key, Specialised::Failed);
            return None;
        }
        self.specialised.insert(key, Specialised::Pending(rx));
        // First frames keep using the existing fused shader: compilation never blocks playback.
        None
    }

    /// Whether `device` can run the stage: compute shaders with 16×16 workgroups and a storage
    /// texture (WebGL2-class devices can't; they render effects on the CPU).
    pub(crate) fn supported(device: &wgpu::Device) -> bool {
        let l = device.limits();
        l.max_storage_textures_per_shader_stage >= 1
            && l.max_storage_buffers_per_shader_stage >= 3
            && l.max_compute_workgroup_storage_size >= 4096
            && l.max_compute_invocations_per_workgroup >= 256
            && l.max_compute_workgroup_size_x >= 64
            && l.max_compute_workgroup_size_y >= 16
            && l.max_compute_workgroups_per_dimension >= 1
    }

    pub(crate) fn new(device: &wgpu::Device, composite: &wgpu::ShaderModule, layer_bgl: &wgpu::BindGroupLayout) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-fx"),
            source: wgpu::ShaderSource::Wgsl(include_str!("fx.wgsl").into()),
        });
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fx"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                tex(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: FX_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                tex(3),
                tex(7),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("fx"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let compute = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("fx"),
                layout: Some(&pl),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let spl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fx-source"),
            bind_group_layouts: &[Some(layer_bgl)],
            immediate_size: 0,
        });
        let source = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fx-source"),
            layout: Some(&spl),
            vertex: wgpu::VertexState { module: composite, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: composite,
                entry_point: Some("fs_fx_source"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: FX_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            px: compute("fx_px"),
            run: compute("fx_run"),
            tiled: compute("fx_tile"),
            #[cfg(not(target_arch = "wasm32"))]
            layout: pl.clone(),
            #[cfg(not(target_arch = "wasm32"))]
            specialised: HashMap::new(),
            masks: crate::mask_cache::MaskCache::new(device),
            bgl,
            #[cfg(test)]
            optimise: true,
            source,
            pool: HashMap::new(),
            used: HashSet::new(),
            #[cfg(test)]
            fuse: true,
        }
    }

    /// Start a frame: pooled textures of sizes no layer uses by [`end_frame`](Self::end_frame)
    /// are released then.
    pub(crate) fn begin_frame(&mut self) {
        self.masks.trim_to_budget();
        self.used.clear();
    }

    pub(crate) fn end_frame(&mut self) {
        let used = &self.used;
        self.pool.retain(|k, _| used.contains(k));
    }

    /// No working textures held.
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool {
        self.pool.is_empty()
    }

    fn targets(&mut self, device: &wgpu::Device, size: (u32, u32), n: usize) -> Vec<Target> {
        self.used.insert(size);
        let v = self.pool.entry(size).or_default();
        while v.len() < n {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("filmcraft-fx"),
                size: wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FX_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = t.create_view(&Default::default());
            v.push((t, view));
        }
        v.iter().take(n).cloned().collect()
    }

    /// Prepare the passes of `fx`, whose source draw uses `source_bg` (a layer bind group whose
    /// uniforms map the frame onto the working image). None when the working image does not fit
    /// in a texture (the layer is then drawn without its effects — never larger than the source
    /// frame, which would not have uploaded either).
    pub(crate) fn job(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        fx: &LayerFx,
        source_bg: wgpu::BindGroup,
        dummy: &wgpu::TextureView,
    ) -> Option<FxJob> {
        if !fx.ops.iter().all(FxOp::gpu_ok) {
            return None;
        }
        let (w, h) = (fx.size.0.max(1), fx.size.1.max(1));
        let max = device.limits().max_texture_dimension_2d;
        if w > max || h > max {
            return None;
        }
        let all: Vec<Step> = fx.ops.iter().flat_map(steps).collect();
        let n = if all.iter().any(|s| s.mask_start) {
            4
        } else if all.iter().any(|s| s.combine) {
            3
        } else {
            2
        };
        let targets = self.targets(device, (w, h), n).to_vec();
        let views: Vec<wgpu::TextureView> = targets.iter().map(|t| t.1.clone()).collect();
        let mut dispatches = Vec::with_capacity(all.len());
        let mut cur = 0usize;
        // Unsharp: the image its blur started from (kept while its blur passes run)
        let mut orig: Option<usize> = None;
        let mut mask_orig: Option<usize> = None;
        let groups = batches(&all);
        #[cfg(test)]
        let groups = if self.fuse { groups } else { all.chunks(1).collect() };
        for batch in groups {
            let s = batch.first()?;
            if s.mask_start {
                mask_orig = Some(cur);
            }
            // (an Unsharp without blur passes, radius 0, combines the original with itself)
            if (s.unsharp_blur || s.combine) && orig.is_none() {
                orig = Some(cur);
            }
            let keep = if s.unsharp_blur || s.combine { orig } else { None };
            let dst = (0..n).find(|k| *k != cur && Some(*k) != keep && Some(*k) != mask_orig)?;
            let aux = if s.code == OP_MASK_MIX {
                views.get(mask_orig?)?
            } else {
                match (s.combine, orig) {
                    (true, Some(o)) => views.get(o)?,
                    _ => dummy,
                }
            };
            let mut bytes = Vec::with_capacity(16 + MAX_FUSED * 128);
            let optimise = {
                #[cfg(test)]
                {
                    self.optimise
                }
                #[cfg(not(test))]
                {
                    true
                }
            };
            let cached_mask = if optimise && !s.masks.is_empty() { self.masks.coverage(device, queue, &s.masks, w, h) } else { None };
            for v in [batch.len() as u32, w, h, u32::from(cached_mask.is_some())] {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let mut data = Vec::new();
            for s in batch {
                let mut ints = s.i1;
                if !s.data.is_empty() {
                    ints[3] = (data.len() / 16) as u32;
                }
                for v in s.data.iter().flatten() {
                    data.extend_from_slice(&v.to_le_bytes());
                }
                for v in [s.code, w, h, 0].iter().chain(&ints) {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
                for v in &s.p {
                    bytes.extend_from_slice(&v.to_le_bytes());
                }
            }
            bytes.resize(16 + MAX_FUSED * 128, 0);
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fx-u"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buf, 0, &bytes);
            use wgpu::util::DeviceExt;
            let mut infos = Vec::new();
            let mut points = Vec::new();
            let mut start = 0u32;
            for mask in &s.masks {
                for v in [start, mask.pts.len() as u32, mask.mode.index(), u32::from(mask.inverted)] {
                    infos.extend_from_slice(&v.to_le_bytes());
                }
                for v in [mask.feather, mask.expansion, mask.opacity, 0.0] {
                    infos.extend_from_slice(&v.to_le_bytes());
                }
                for v in mask.pts.iter().flatten() {
                    points.extend_from_slice(&v.to_le_bytes());
                }
                start += mask.pts.len() as u32;
            }
            if infos.is_empty() {
                infos.resize(32, 0);
            }
            if points.is_empty() {
                points.resize(8, 0);
            }
            let mb =
                device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("fx-masks"), contents: &infos, usage: wgpu::BufferUsages::STORAGE });
            let pb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fx-mask-points"),
                contents: &points,
                usage: wgpu::BufferUsages::STORAGE,
            });
            if data.is_empty() {
                data.resize(16, 0);
            }
            if data.len() as u64 > device.limits().max_storage_buffer_binding_size {
                return None;
            }
            let db = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fx-grade-data"),
                contents: &data,
                usage: wgpu::BufferUsages::STORAGE,
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fx"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: buf.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(views.get(cur)?) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(views.get(dst)?) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(aux) },
                    wgpu::BindGroupEntry { binding: 4, resource: mb.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 5, resource: pb.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 6, resource: db.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(cached_mask.as_ref().unwrap_or(dummy)) },
                ],
            });
            let tiled = optimise && s.code == OP_BOX && (8..=BOX_PER_PIXEL_MAX).contains(&s.i1[0]);
            let pipeline = if tiled { Some(self.tiled.clone()) } else { self.specialised(device, batch) };
            let groups = if tiled {
                let (length, lines) = if s.i1[2] != 0 { (h, w) } else { (w, h) };
                (length.div_ceil(128), lines)
            } else if s.run {
                let lines = if s.i1[2] != 0 { w } else { h };
                (lines.div_ceil(64), 1)
            } else {
                (w.div_ceil(16), h.div_ceil(16))
            };
            if groups.0 > device.limits().max_compute_workgroups_per_dimension || groups.1 > device.limits().max_compute_workgroups_per_dimension {
                return None;
            }
            dispatches.push(Dispatch { run: s.run, pipeline, bg, groups });
            cur = dst;
            if s.combine {
                orig = None;
            }
            if s.code == OP_MASK_MIX {
                mask_orig = None;
            }
        }
        Some(FxJob {
            source_bg,
            source_target: views.first()?.clone(),
            dispatches,
            result: views.get(cur)?.clone(),
            result_texture: targets.get(cur)?.0.clone(),
        })
    }

    /// Record a job (source draw, then its compute passes) into `enc`.
    pub(crate) fn record(&self, enc: &mut wgpu::CommandEncoder, job: &FxJob) {
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fx-source"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &job.source_target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.source);
            pass.set_bind_group(0, &job.source_bg, &[]);
            pass.draw(0..6, 0..1);
        }
        if job.dispatches.is_empty() {
            return;
        }
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("fx"), timestamp_writes: None });
        for d in &job.dispatches {
            pass.set_pipeline(d.pipeline.as_ref().unwrap_or(if d.run { &self.run } else { &self.px }));
            pass.set_bind_group(0, &d.bg, &[]);
            pass.dispatch_workgroups(d.groups.0, d.groups.1, 1);
        }
    }
}

#[cfg(test)]
mod fusion_tests {
    use super::*;

    #[test]
    fn fusion_stops_at_spatial_operations_and_bounds_uniform_size() {
        let mut chain = vec![step(OP_TINT, [0; 4], &[])];
        chain.extend((0..18).map(|_| step(OP_BRIGHTNESS_CONTRAST, [0; 4], &[])));
        chain.push(step(OP_BOX, [1, 1, 0, 0], &[]));
        chain.push(step(OP_LUMETRI, [0; 4], &[]));
        chain.push(step(OP_ULTRA_KEY, [0; 4], &[]));
        assert_eq!(batches(&chain).iter().map(|b| b.len()).collect::<Vec<_>>(), [16, 3, 1, 2]);
    }
}
