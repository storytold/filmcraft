//! Optical Flow time interpolation on the GPU (`flow.wgsl`): the passes of
//! [`filmcraft_render::flow::interpolate`], the CPU reference, for a layer's two working images.

use filmcraft_render::flow as cpu;

use crate::fx::{FX_FORMAT, Pipe};

const OP_LUMA: u32 = 1;
const OP_DOWN_H: u32 = 2;
const OP_DOWN_V: u32 = 3;
const OP_TENSOR_H: u32 = 4;
const OP_SUM_V: u32 = 5;
const OP_MISMATCH_H: u32 = 6;
const OP_UPDATE: u32 = 7;
const OP_STILL_H: u32 = 8;
const OP_STILL_V: u32 = 9;
const OP_MEDIAN: u32 = 10;
const OP_UPSAMPLE: u32 = 11;
const OP_ZERO: u32 = 12;
const OP_SPLAT_OUT: u32 = 15;
const OP_FILL: u32 = 16;
const OP_WARP: u32 = 17;

pub(crate) struct FlowStage {
    bgl: wgpu::BindGroupLayout,
    pub(crate) px: wgpu::ComputePipeline,
    pub(crate) score: wgpu::ComputePipeline,
    pub(crate) win: wgpu::ComputePipeline,
    /// Written by the passes that write no image (a texture may not be read and written at once).
    no_out: wgpu::TextureView,
}

/// One compute pass of the flow.
pub(crate) struct Pass {
    pub(crate) pipe: Pipe,
    pub(crate) bg: wgpu::BindGroup,
    pub(crate) groups: (u32, u32),
}

/// The working images of one pyramid level: the two frames' luma, a scratch image (horizontal
/// window sums), the structure tensor, and each direction's flow (two, written in turn).
pub(crate) struct Level {
    pub(crate) size: (u32, u32),
    pub(crate) a: wgpu::TextureView,
    pub(crate) b: wgpu::TextureView,
    pub(crate) scratch: wgpu::TextureView,
    pub(crate) tensor: wgpu::TextureView,
    pub(crate) f01: [wgpu::TextureView; 2],
    pub(crate) f10: [wgpu::TextureView; 2],
}

/// Images a level needs ([`Level`]); the finest also holds the in-between flow (two).
pub(crate) const LEVEL_IMAGES: usize = 8;
pub(crate) const FINEST_EXTRA: usize = 2;

/// The box reduction of a `w`×`h` working image and the sizes of its luma pyramid, finest first,
/// as `flow::interpolate` builds them.
pub(crate) fn levels(w: u32, h: u32) -> (u32, Vec<(u32, u32)>) {
    let s = w.div_ceil(cpu::WORK_W as u32).max(1);
    let mut v = vec![(w.div_ceil(s).max(1), h.div_ceil(s).max(1))];
    let min = 2 * cpu::MIN_LEVEL as u32;
    while v.len() < cpu::MAX_LEVELS {
        let Some(&(lw, lh)) = v.last() else { break };
        if lw < min || lh < min {
            break;
        }
        v.push((lw.div_ceil(2).max(1), lh.div_ceil(2).max(1)));
    }
    (s, v)
}

impl FlowStage {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-flow"),
            source: wgpu::ShaderSource::Wgsl(include_str!("flow.wgsl").into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty, count: None };
        let tex = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let storage =
            wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flow"),
            entries: &[
                entry(0, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }),
                entry(1, tex),
                entry(2, tex),
                entry(3, tex),
                entry(4, tex),
                entry(5, tex),
                entry(
                    6,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: FX_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
                entry(7, storage),
                entry(8, storage),
                entry(9, storage),
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("flow"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let compute = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("flow"),
                layout: Some(&pl),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let no_out = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("filmcraft-flow-no-out"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FX_FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Self { px: compute("flow_px"), score: compute("splat_score"), win: compute("splat_win"), bgl, no_out }
    }

    /// The passes that write the frame between working images `a` (time 0) and `b` (time 1) at
    /// `t` (0 < t < 1) into `dst`, all `size`; `levels` from [`levels`] for it, `between` the
    /// finest level's two extra images, `dummy` bound where a pass reads nothing.
    #[expect(clippy::too_many_arguments, reason = "the two frames, the result and the stage's images")]
    pub(crate) fn passes(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        (a, b, dst): (&wgpu::TextureView, &wgpu::TextureView, &wgpu::TextureView),
        size: (u32, u32),
        t: f32,
        s: u32,
        levels: &[Level],
        between: &[wgpu::TextureView; 2],
        dummy: &wgpu::TextureView,
    ) -> Option<Vec<Pass>> {
        let finest = levels.first()?;
        let n = u64::from(finest.size.0) * u64::from(finest.size.1);
        let buffer = |label, bytes: u64| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.max(4),
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            })
        };
        // (new buffers are zero: no best score and no winner yet)
        let bufs = [buffer("flow-best", n * 4), buffer("flow-winner", n * 4), buffer("flow-scores", n * 2 * 3 * 4)];
        let p0 = [t, s as f32, cpu::DAMP, cpu::STILL_SLACK];
        let mut out = Vec::new();
        let mut pass = |pipe: Pipe, op: u32, size: (u32, u32), ins: [Option<&wgpu::TextureView>; 5], dst: Option<&wgpu::TextureView>, i1: [u32; 4]| {
            let mut bytes = Vec::with_capacity(48);
            for v in [op, size.0, size.1, 0].iter().chain(&i1) {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            for v in &p0 {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
            let ub = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flow-u"),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&ub, 0, &bytes);
            let tex = |k: usize| wgpu::BindingResource::TextureView(ins.get(k).copied().flatten().unwrap_or(dummy));
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("flow"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: tex(0) },
                    wgpu::BindGroupEntry { binding: 2, resource: tex(1) },
                    wgpu::BindGroupEntry { binding: 3, resource: tex(2) },
                    wgpu::BindGroupEntry { binding: 4, resource: tex(3) },
                    wgpu::BindGroupEntry { binding: 5, resource: tex(4) },
                    wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(dst.unwrap_or(&self.no_out)) },
                    wgpu::BindGroupEntry { binding: 7, resource: bufs[0].as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 8, resource: bufs[1].as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 9, resource: bufs[2].as_entire_binding() },
                ],
            });
            let rows = if matches!(pipe, Pipe::FlowPx) { size.1 } else { 2 * size.1 };
            out.push(Pass { pipe, bg, groups: (size.0.div_ceil(16), rows.div_ceil(16)) });
        };
        let px = Pipe::FlowPx;
        // the working luma and its pyramid (`work_luma`, `pyramid`)
        pass(px, OP_LUMA, finest.size, [Some(a), None, None, None, None], Some(&finest.a), [s, 0, 0, 0]);
        pass(px, OP_LUMA, finest.size, [Some(b), None, None, None, None], Some(&finest.b), [s, 0, 0, 0]);
        for pair in levels.windows(2) {
            let [fine, coarse] = pair else { continue };
            for (src, dst) in [(&fine.a, &coarse.a), (&fine.b, &coarse.b)] {
                pass(px, OP_DOWN_H, fine.size, [Some(src), None, None, None, None], Some(&fine.scratch), [0; 4]);
                pass(px, OP_DOWN_V, coarse.size, [Some(&fine.scratch), None, None, None, None], Some(dst), [0; 4]);
            }
        }
        // the dense flow both ways (`dense_flow`): which of each level's two flows is current
        let mut finals = Vec::with_capacity(2);
        for forward in [true, false] {
            let mut cur = 0usize;
            let mut coarser: Option<&wgpu::TextureView> = None;
            for l in levels.iter().rev() {
                let (la, lb, f) = if forward { (&l.a, &l.b, &l.f01) } else { (&l.b, &l.a, &l.f10) };
                match coarser {
                    Some(c) => pass(px, OP_UPSAMPLE, l.size, [None, None, Some(c), None, None], Some(&f[0]), [0; 4]),
                    None => pass(px, OP_ZERO, l.size, [None; 5], Some(&f[0]), [0; 4]),
                }
                cur = 0;
                // `refine`
                pass(px, OP_TENSOR_H, l.size, [Some(la), None, None, None, None], Some(&l.scratch), [0; 4]);
                pass(px, OP_SUM_V, l.size, [Some(&l.scratch), None, None, None, None], Some(&l.tensor), [0; 4]);
                for _ in 0..cpu::ITERS {
                    pass(px, OP_MISMATCH_H, l.size, [Some(la), Some(lb), Some(&f[cur]), None, None], Some(&l.scratch), [0; 4]);
                    pass(px, OP_UPDATE, l.size, [Some(&l.scratch), Some(&l.tensor), Some(&f[cur]), None, None], Some(&f[1 - cur]), [0; 4]);
                    cur = 1 - cur;
                }
                // `prefer_still`, then `median3`
                pass(px, OP_STILL_H, l.size, [Some(la), Some(lb), Some(&f[cur]), None, None], Some(&l.scratch), [0; 4]);
                pass(px, OP_STILL_V, l.size, [Some(&l.scratch), None, Some(&f[cur]), None, None], Some(&f[1 - cur]), [0; 4]);
                cur = 1 - cur;
                pass(px, OP_MEDIAN, l.size, [None, None, Some(&f[cur]), None, None], Some(&f[1 - cur]), [0; 4]);
                cur = 1 - cur;
                coarser = Some(&f[cur]);
            }
            finals.push(if forward { &finest.f01[cur] } else { &finest.f10[cur] });
        }
        let (f01, f10) = (*finals.first()?, *finals.get(1)?);
        // the flow at time t (`flow_at`): splat, pick, fill
        let lum = [Some(&finest.a), Some(&finest.b), Some(f01), Some(f10), None];
        pass(Pipe::FlowScore, 0, finest.size, lum, None, [0; 4]);
        pass(Pipe::FlowWin, 0, finest.size, lum, None, [0; 4]);
        pass(px, OP_SPLAT_OUT, finest.size, [None, None, Some(f01), Some(f10), None], Some(&between[0]), [0; 4]);
        let mut cur = 0usize;
        for _ in 0..cpu::FILL_PASSES {
            pass(px, OP_FILL, finest.size, [Some(&between[cur]), None, None, None, None], Some(&between[1 - cur]), [0; 4]);
            cur = 1 - cur;
        }
        // warp and blend
        pass(px, OP_WARP, size, [Some(a), Some(b), Some(&between[cur]), Some(f01), Some(f10)], Some(dst), [0; 4]);
        Some(out)
    }
}
