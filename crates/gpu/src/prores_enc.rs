//! ProRes encoding on the GPU: [`ProResGpuEncoder`] is a [`SliceBackend`] of the CPU encoder
//! (`filmcraft_prores::Encoder::encode_with`) that runs the per-block work in compute shaders
//! (`prores_enc.wgsl`): forward DCT, quantisation, bit counting, entropy coding and the alpha
//! channel's run-length coding, and the rate control (`choose`, the CPU's algorithm step for step).
//! The CPU encoder keeps the headers and the frame assembly, checks every answer, and encodes a
//! frame itself whenever this backend declines or fails.
//!
//! `load` uploads the planes and transforms every block once; `choose` runs the whole rate control
//! in one submission and reads back only the quantisers (and the bits measured at the two it
//! chose, for `bits`); `bits(q)` measures every slice at `q` and its two neighbours in one dispatch
//! and keeps the answers for the frame; `write` codes every component into its own word-aligned
//! range and reads back only those ranges.
//!
//! An interlaced frame is two field pictures: the coefficients, slice records and alpha regions of
//! the second follow the first's as if its macroblock rows continued the frame's, `fdct` and `alpha`
//! read every second line of the field's parity, and every component is coded with the interlaced
//! scan. The CPU encoder writes the two picture headers.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use filmcraft_prores::layout::tables;
use filmcraft_prores::{ChromaFormat, EncoderConfig, Frame, Interlace, SliceBackend, SliceJob, picture_lines};

use std::any::Any;

use crate::export_post::ResidentPlanes;
use crate::intra::{Pooled, bind, dispatch, groups, storage, uniform};
use crate::intra_enc::{Fail, Gpu, fits, le_bytes, max_binding, own_device, pack_planes, words_of};

/// Rounds of measurement the rate control runs on the GPU: the most distinct quantisers the CPU's
/// `choose_quantisers` measures before its size guard. The search from a hint at 1 to a frame that
/// first fits at 130 measures 16 (the hint, eight widening steps, seven bisection steps). The
/// refinement adds none: it reads the finer quantiser only when the uniform one fits, and then the
/// finer one is the bracket's `lo`, already measured (the shader's `PH_FINER` stays as a guard);
/// `the_rate_control_rounds_cover_every_cpu_search` checks every hint against every place the frame
/// can start to fit. (The size guard raises each slice on its own, in one pass after the rounds.)
pub(crate) const RC_ROUNDS: usize = 16;

/// Words of the constants (see `prores_enc.wgsl`): basis, progressive scan, codebooks, AC rounding
/// and contexts, `1 / (4 qscale(q))` for every quantiser index from `C_INV`, then the interlaced
/// scan.
pub(crate) const C_SCAN: usize = 64;
const C_CB: usize = 128;
const C_AC_ROUND: usize = 162;
pub(crate) const C_INV: usize = 192;
pub(crate) const C_SCAN_INTERLACED: usize = C_INV + 225;
const CONST_WORDS: usize = C_SCAN_INTERLACED + 64;
/// Quantiser indices the encoder uses (RDD 36 allows 1..=224).
const Q_RANGE: std::ops::RangeInclusive<u8> = 1..=224;
/// Worst-case words of a macroblock's coded alpha: 256 values of at most 18 bits each.
const ALPHA_WORDS_PER_MB: u64 = 144;
/// Words of the rate control's buffer (`prores_enc.wgsl`): the state, the uniform quantiser, the
/// done flag, the kept rows' quantisers, then from `R_QS` the answer.
const R_Q: usize = 1;
const R_QG: usize = 5;
const R_DONE: usize = 6;
const R_ROW0_Q: usize = 8;
const R_ROW1_Q: usize = 10;
const R_QS: usize = 64;
/// Slots of the rate control's table: one per quantiser it measures.
const RC_SLOTS: u64 = 16;

/// The rate control's answer: every slice's quantiser, the uniform one, and the rows it measured
/// at the uniform quantiser and the finer one (for the cache of `bits`).
type Choice = (Vec<u8>, u8, Vec<(u8, Vec<[u32; 3]>)>);

/// `Params` in `prores_enc.wgsl` (28 words).
#[derive(Clone, Copy, Default)]
struct Params {
    n_slices: u32,
    mb_w: u32,
    mb_h: u32,
    cpm: u32,
    width: u32,
    height: u32,
    cwidth: u32,
    bpm: u32,
    scale: f32,
    off_cb: u32,
    off_cr: u32,
    off_a: u32,
    nq: u32,
    depth: u32,
    alpha_base: u32,
    qv: [u32; 4],
    total_blocks: u32,
    mbcol_base: u32,
    /// Frame lines between a picture's lines (1, or 2 for fields), each picture's first frame
    /// line and its number of lines.
    step: u32,
    first: [u32; 2],
    lines: [u32; 2],
    /// Word offset of the scan order in the constants.
    scan: u32,
}

impl Params {
    fn bytes(&self) -> Vec<u8> {
        let [q0, q1, q2, q3] = self.qv;
        le_bytes(&[
            self.n_slices,
            self.mb_w,
            self.mb_h,
            self.cpm,
            self.width,
            self.height,
            self.cwidth,
            self.bpm,
            self.scale.to_bits(),
            self.off_cb,
            self.off_cr,
            self.off_a,
            self.nq,
            self.depth,
            0,
            self.alpha_base,
            q0,
            q1,
            q2,
            q3,
            self.total_blocks,
            self.mbcol_base,
            self.step,
            self.first[0],
            self.first[1],
            self.lines[0],
            self.lines[1],
            self.scan,
        ])
    }
}

/// The frame geometry and buffer sizes of one `load`, checked against the device's limits.
struct Plan {
    params: Params,
    n: usize,
    alpha: bool,
    mbs: Vec<u32>,
    /// Slice records, then the macroblock-column table.
    slice_words: Vec<u32>,
    plane_bytes: u64,
    coeff_bytes: u64,
    alpha_bytes: u64,
}

/// What a frame to load is, whether its planes come from the CPU or are on the GPU already.
struct Shape {
    width: u32,
    height: u32,
    chroma: ChromaFormat,
    bit_depth: u8,
    /// It has an alpha plane.
    alpha: bool,
}

impl Shape {
    fn of(frame: &Frame) -> Shape {
        Shape { width: frame.width, height: frame.height, chroma: frame.chroma, bit_depth: frame.bit_depth, alpha: frame.alpha.is_some() }
    }

    fn of_resident(planes: &ResidentPlanes) -> Shape {
        let chroma = if planes.chroma_422 { ChromaFormat::Yuv422 } else { ChromaFormat::Yuv444 };
        Shape { width: planes.width, height: planes.height, chroma, bit_depth: planes.bit_depth, alpha: planes.alpha }
    }
}

/// Whether `frame`'s planes hold the samples `plan` reads.
fn planes_cover(plan: &Plan, frame: &Frame) -> bool {
    let (luma, croma) = (u64::from(plan.params.off_cb), u64::from(plan.params.off_cr - plan.params.off_cb));
    let short = |p: &[u16], n: u64| (p.len() as u64) < n;
    !(short(&frame.y, luma) || short(&frame.cb, croma) || short(&frame.cr, croma) || (plan.alpha && frame.alpha.as_deref().is_none_or(|a| short(a, luma))))
}

impl Plan {
    /// `None` when the frame is not one this backend codes, or does not fit `limits`.
    fn new(cfg: &EncoderConfig, frame: &Shape, jobs: &[SliceJob], limits: &wgpu::Limits) -> Option<Plan> {
        let (w, h) = (cfg.width, cfg.height);
        let chroma = cfg.profile.chroma();
        // (the frame's own interlace flag is metadata: the configuration says how it is coded)
        if frame.width != w || frame.height != h || frame.chroma != chroma || !(8..=16).contains(&frame.bit_depth) {
            return None;
        }
        let cw = chroma.chroma_width(w);
        let alpha = cfg.encode_alpha && chroma == ChromaFormat::Yuv444 && frame.alpha;
        let (luma, croma) = (u64::from(w) * u64::from(h), u64::from(cw) * u64::from(h));
        let pics = Pictures::new(cfg.interlace, h)?;
        let (mb_w, mb_h) = (w.div_ceil(16), pics.mb_rows);
        let cpm: u32 = if chroma == ChromaFormat::Yuv444 { 4 } else { 2 };
        let bpm = 4 + 2 * cpm;
        let (slice_words, mbcol_base) = slice_table(jobs, mb_w, mb_h, pics.count, bpm)?;
        // the pictures' macroblock rows, one after the other
        let rows = u64::from(mb_h) * u64::from(pics.count);
        let total_blocks = u64::from(mb_w) * rows * u64::from(bpm);
        let samples = luma + 2 * croma + if alpha { luma } else { 0 };
        let plane_bytes = (samples * 2).next_multiple_of(4);
        let coeff_bytes = total_blocks * 64 * 4;
        let n = jobs.len();
        let alpha_bytes = if alpha { (n as u64 + u64::from(mb_w) * rows * ALPHA_WORDS_PER_MB) * 4 } else { 16 };
        let biggest = plane_bytes.max(alpha_bytes).max(slice_words.len() as u64 * 4);
        let maxg = limits.max_compute_workgroups_per_dimension;
        if !fits(limits, coeff_bytes, biggest, w, h)
            || samples > u64::from(u32::MAX)
            || groups(total_blocks, maxg).is_none()
            || groups(n as u64 * 9, maxg).is_none()
        {
            return None;
        }
        let params = Params {
            n_slices: u32::try_from(n).ok()?,
            mb_w,
            mb_h,
            cpm,
            width: w,
            height: h,
            cwidth: cw,
            bpm,
            scale: (2f32).powi(12 - i32::from(frame.bit_depth)),
            off_cb: u32::try_from(luma).ok()?,
            off_cr: u32::try_from(luma + croma).ok()?,
            off_a: u32::try_from(luma + 2 * croma).ok()?,
            nq: 0,
            depth: u32::from(frame.bit_depth),
            alpha_base: u32::try_from(n).ok()?,
            qv: [0; 4],
            total_blocks: u32::try_from(total_blocks).ok()?,
            mbcol_base,
            step: pics.step,
            first: pics.first,
            lines: pics.lines,
            scan: u32::try_from(if pics.count == 2 { C_SCAN_INTERLACED } else { C_SCAN }).ok()?,
        };
        let mbs = jobs.iter().map(|j| j.mbs).collect();
        Some(Plan { params, n, alpha, mbs, slice_words, plane_bytes, coeff_bytes, alpha_bytes })
    }
}

/// The coded pictures of a frame: one, or an interlaced frame's two fields.
struct Pictures {
    count: u32,
    /// Macroblock rows of every picture.
    mb_rows: u32,
    step: u32,
    first: [u32; 2],
    lines: [u32; 2],
}

impl Pictures {
    /// `None` for an interlaced frame of fewer than two lines (the encoder refuses it).
    fn new(interlace: Interlace, h: u32) -> Option<Pictures> {
        let count = if interlace == Interlace::Progressive { 1 } else { 2 };
        let ((f0, step), (f1, _)) = (picture_lines(interlace, 0), picture_lines(interlace, 1));
        let lines = |first: u32| h.saturating_sub(first).div_ceil(step.max(1));
        let first = [f0, if count == 2 { f1 } else { 0 }];
        let lines = [lines(first[0]), if count == 2 { lines(first[1]) } else { 0 }];
        if lines.get(..count as usize)?.contains(&0) {
            return None;
        }
        Some(Pictures { count, mb_rows: h.div_ceil(16 * step), step, first, lines })
    }
}

/// The slice records (mb_x, macroblock row counted over the pictures one after the other, mbs,
/// first block) and the macroblock-column table (first column of the slice | its width << 16) with
/// its word offset. `None` unless `jobs` cover every picture in coded order with the same slices in
/// every row.
fn slice_table(jobs: &[SliceJob], mb_w: u32, mb_h: u32, pictures: u32, bpm: u32) -> Option<(Vec<u32>, u32)> {
    let mut words = Vec::with_capacity(jobs.len() * 4 + mb_w as usize);
    let mut cols = vec![u32::MAX; mb_w as usize];
    let (mut x, mut y, mut p) = (0u32, 0u32, 0u32);
    for j in jobs {
        let end = j.mb_x.checked_add(j.mbs)?;
        if j.mb_x != x || j.mb_y != y || j.picture != p || !matches!(j.mbs, 1 | 2 | 4 | 8) || end > mb_w || y >= mb_h || p >= pictures {
            return None;
        }
        let col = j.mb_x | j.mbs << 16;
        for c in cols.get_mut(j.mb_x as usize..end as usize)? {
            if *c != u32::MAX && *c != col {
                return None;
            }
            *c = col;
        }
        let row = p.checked_mul(mb_h)?.checked_add(y)?;
        let first = (row.checked_mul(mb_w)?.checked_add(x)?).checked_mul(bpm)?;
        words.extend_from_slice(&[j.mb_x, row, j.mbs, first]);
        (x, y) = if end == mb_w { (0, y + 1) } else { (end, y) };
        if y == mb_h {
            (y, p) = (0, p + 1);
        }
    }
    if (x, y, p) != (0, 0, pictures) {
        return None;
    }
    let base = u32::try_from(words.len()).ok()?;
    words.extend_from_slice(&cols);
    Some((words, base))
}

struct Pipes {
    fdct: wgpu::ComputePipeline,
    rc: [wgpu::ComputePipeline; 4],
    bits: wgpu::ComputePipeline,
    write: wgpu::ComputePipeline,
    alpha: wgpu::ComputePipeline,
    pack: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
}

impl Pipes {
    fn new(device: &wgpu::Device) -> Pipes {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-prores-enc"),
            source: wgpu::ShaderSource::Wgsl(include_str!("prores_enc.wgsl").into()),
        });
        let uniform_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("prores-enc"),
            entries: &[
                uniform_entry,
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, false),
                storage(5, false),
                storage(6, true),
                storage(7, false),
            ],
        });
        let layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("prores-enc"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("prores-enc"),
                layout: Some(&layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let rc = [pipeline("rc_bits"), pipeline("rc_pick"), pipeline("rc_refine"), pipeline("rc_final")];
        Pipes { fdct: pipeline("fdct"), rc, bits: pipeline("bits"), write: pipeline("write"), alpha: pipeline("alpha"), pack: pipeline("alpha_pack"), bgl }
    }
}

/// The buffers of the loaded frame.
struct Loaded {
    params: Params,
    n: usize,
    alpha: bool,
    /// Every slice's width in macroblocks.
    mbs: Vec<u32>,
    planes: wgpu::Buffer,
    slices: wgpu::Buffer,
    coeffs: wgpu::Buffer,
    alpha_buf: wgpu::Buffer,
    /// Per-slice Y, Cb, Cr bits at every quantiser measured for this frame.
    measured: Vec<(u8, Vec<[u32; 3]>)>,
}

impl Loaded {
    fn measured(&self, q: u8) -> Option<&Vec<[u32; 3]>> {
        self.measured.iter().find(|(mq, _)| *mq == q).map(|(_, v)| v)
    }
}

struct Pools {
    planes: Pooled,
    slices: Pooled,
    coeffs: Pooled,
    out: Pooled,
    aux: Pooled,
    alpha: Pooled,
    alpha_out: Pooled,
    rc: Pooled,
}

/// ProRes encoding's per-block work in compute shaders (a [`SliceBackend`]).
pub struct ProResGpuEncoder {
    gpu: Gpu,
    pipes: Pipes,
    consts: wgpu::Buffer,
    status: wgpu::Buffer,
    pools: Pools,
    frame: Option<Loaded>,
    /// The packed planes of the last frame (reused).
    staging: Vec<u8>,
}

impl ProResGpuEncoder {
    /// An encoder on its own device (as `ExportRenderer::new`), `None` without a compute-capable
    /// adapter.
    pub fn new() -> Option<Self> {
        let (device, queue, lost) = own_device()?;
        Self::build(device, queue, lost)
    }

    /// An encoder on `device`, `None` when the device lacks what the shaders need.
    pub fn with_device(device: wgpu::Device, queue: wgpu::Queue) -> Option<Self> {
        Self::build(device, queue, Arc::new(AtomicBool::new(false)))
    }

    /// An encoder on the export device, taking the frames a renderer on it leaves there.
    pub(crate) fn on(dev: &crate::intra_enc::ExportDevice) -> Option<Self> {
        Self::build(dev.device.clone(), dev.queue.clone(), dev.lost.clone())
    }

    fn build(device: wgpu::Device, queue: wgpu::Queue, lost: Arc<AtomicBool>) -> Option<Self> {
        let l = device.limits();
        if l.max_storage_buffers_per_shader_stage < 7
            || l.max_compute_invocations_per_workgroup < 64
            || l.max_compute_workgroup_size_x < 64
            || l.max_compute_workgroups_per_dimension < 1024
        {
            return None;
        }
        let mut gpu = Gpu::new(device, queue, lost);
        let scopes = gpu.scopes();
        let made = Self::make(&gpu.device, &gpu.queue);
        let (pipes, consts, status) = gpu.settle("creating the ProRes encode pipelines", scopes, Ok(made))?;
        let storage_dst = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let storage_src = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC;
        let pools = Pools {
            planes: Pooled::new("prores-enc-planes", storage_dst),
            slices: Pooled::new("prores-enc-slices", storage_dst),
            coeffs: Pooled::new("prores-enc-coeffs", wgpu::BufferUsages::STORAGE),
            out: Pooled::new("prores-enc-out", storage_src),
            aux: Pooled::new("prores-enc-aux", storage_dst),
            alpha: Pooled::new("prores-enc-alpha", storage_src),
            alpha_out: Pooled::new("prores-enc-alpha-out", storage_src),
            rc: Pooled::new("prores-enc-rc", storage_src | wgpu::BufferUsages::COPY_DST),
        };
        Some(ProResGpuEncoder { gpu, pipes, consts, status, pools, frame: None, staging: Vec::new() })
    }

    /// The pipelines, the constants and the status words.
    fn make(device: &wgpu::Device, queue: &wgpu::Queue) -> (Pipes, wgpu::Buffer, wgpu::Buffer) {
        let pipes = Pipes::new(device);
        let words = consts();
        let consts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("prores-enc-consts"),
            size: words.len() as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&consts, 0, &le_bytes(&words));
        let status = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("prores-enc-status"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        (pipes, consts, status)
    }

    /// The limits of the encoder's device.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn limits(&self) -> wgpu::Limits {
        self.gpu.device.limits()
    }

    /// Tests: count a device error, as a failed step does.
    #[cfg(test)]
    pub(crate) fn device_error(&mut self) {
        let scopes = self.gpu.scopes();
        let _ = self.gpu.settle::<()>("a test", scopes, Err(Fail::Device("injected".into())));
        self.frame = None;
    }

    fn bind_group(&self, f: &Loaded, params: &Params, work: &wgpu::Buffer, out: &wgpu::Buffer, aux: &wgpu::Buffer) -> wgpu::BindGroup {
        let ubuf = uniform(&self.gpu.device, &self.gpu.queue, &params.bytes());
        self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("prores-enc"),
            layout: &self.pipes.bgl,
            entries: &[
                bind(&ubuf, 0, None),
                bind(&f.planes, 1, None),
                bind(&f.slices, 2, None),
                bind(&self.consts, 3, None),
                bind(work, 4, None),
                bind(out, 5, None),
                bind(aux, 6, None),
                bind(&self.status, 7, None),
            ],
        })
    }

    fn maxg(&self) -> u32 {
        self.gpu.device.limits().max_compute_workgroups_per_dimension
    }

    fn encoder(&self) -> wgpu::CommandEncoder {
        self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("prores-enc") })
    }

    /// Upload the planes and run `fdct` (and `alpha`): the loaded frame.
    fn upload(&mut self, plan: Plan, frame: &Frame) -> Result<Loaded, Fail> {
        let (luma, croma) = (plan.params.off_cb as usize, (plan.params.off_cr - plan.params.off_cb) as usize);
        let mut planes = vec![frame.y.get(..luma), frame.cb.get(..croma), frame.cr.get(..croma)];
        if plan.alpha {
            planes.push(frame.alpha.as_deref().and_then(|a| a.get(..luma)));
        }
        let planes: Vec<&[u16]> = planes.into_iter().collect::<Option<_>>().ok_or(Fail::Decline)?;
        pack_planes(&mut self.staging, &planes, usize::try_from(plan.plane_bytes).map_err(|_| Fail::Decline)?).ok_or(Fail::Decline)?;
        let buffer = self.pools.planes.get(&self.gpu.device, plan.plane_bytes);
        self.gpu.queue.write_buffer(&buffer, 0, &self.staging);
        self.start(plan, buffer)
    }

    /// Run `fdct` (and `alpha`) over the planes in `planes`: the loaded frame.
    fn start(&mut self, plan: Plan, planes: wgpu::Buffer) -> Result<Loaded, Fail> {
        let Plan { params, n, alpha, mbs, slice_words, plane_bytes: _, coeff_bytes, alpha_bytes } = plan;
        let device = &self.gpu.device;
        let f = Loaded {
            params,
            n,
            alpha,
            mbs,
            planes,
            slices: self.pools.slices.get(device, slice_words.len() as u64 * 4),
            coeffs: self.pools.coeffs.get(device, coeff_bytes),
            alpha_buf: self.pools.alpha.get(device, alpha_bytes),
            measured: Vec::new(),
        };
        let queue = &self.gpu.queue;
        queue.write_buffer(&f.slices, 0, &le_bytes(&slice_words));
        let out = self.pools.out.get(device, 16);
        let aux = self.pools.aux.get(device, 16);
        let maxg = self.maxg();
        let mut enc = self.encoder();
        enc.clear_buffer(&self.status, 0, None);
        let bg = self.bind_group(&f, &params, &f.coeffs, &out, &aux);
        dispatch(&mut enc, &[(&self.pipes.fdct, "prores-enc-fdct", groups(u64::from(params.total_blocks), maxg).ok_or(Fail::Decline)?)], &bg);
        if alpha {
            let bg = self.bind_group(&f, &params, &f.coeffs, &f.alpha_buf, &aux);
            dispatch(&mut enc, &[(&self.pipes.alpha, "prores-enc-alpha", groups(n as u64, maxg).ok_or(Fail::Decline)?)], &bg);
        }
        self.gpu.queue.submit([enc.finish()]);
        Ok(f)
    }

    /// Every slice's bits at each of `qs` (one dispatch, one read-back).
    fn measure(&mut self, f: &Loaded, qs: &[u8]) -> Result<Vec<Vec<[u32; 3]>>, Fail> {
        let per = f.n * 3;
        let bytes = (per * qs.len()) as u64 * 4;
        if bytes > max_binding(&self.gpu.device.limits()) || qs.is_empty() || qs.len() > 4 {
            return Err(Fail::Decline);
        }
        let mut params = Params { nq: qs.len() as u32, ..f.params };
        for (v, &q) in params.qv.iter_mut().zip(qs) {
            *v = u32::from(q);
        }
        let device = &self.gpu.device;
        let out = self.pools.out.get(device, bytes);
        let aux = self.pools.aux.get(device, 16);
        let g = groups((per * qs.len()) as u64, self.maxg()).ok_or(Fail::Decline)?;
        let mut enc = self.encoder();
        enc.clear_buffer(&self.status, 0, Some(4));
        let bg = self.bind_group(f, &params, &f.coeffs, &out, &aux);
        dispatch(&mut enc, &[(&self.pipes.bits, "prores-enc-bits", g)], &bg);
        let data = self.gpu.read(enc, &[(&self.status, 0, 16), (&out, 0, bytes)])?;
        let words = words_of(&data);
        if words.first() != Some(&0) {
            return Err(Fail::Decline);
        }
        let bits = words.get(4..).ok_or(Fail::Decline)?;
        Ok(bits.chunks_exact(per).map(|v| v.as_chunks::<3>().0.to_vec()).collect())
    }

    /// Every component coded at its slice's quantiser into its word-aligned range; the bytes.
    fn code(&mut self, f: &Loaded, qs: &[u8], want: &[[u32; 3]]) -> Result<Vec<[Vec<u8>; 3]>, Fail> {
        let n = f.n;
        let mut aux = Vec::with_capacity(n * 7);
        let mut at = 0u64;
        for comps in want {
            for &b in comps {
                aux.extend_from_slice(&[u32::try_from(at).map_err(|_| Fail::Decline)?, b]);
                at += u64::from(b).div_ceil(32);
            }
        }
        aux.extend(qs.iter().map(|&q| u32::from(q)));
        let bytes = at * 4;
        if bytes == 0 || bytes > max_binding(&self.gpu.device.limits()) {
            return Err(Fail::Decline);
        }
        let device = &self.gpu.device;
        let out = self.pools.out.get(device, bytes);
        let aux_buf = self.pools.aux.get(device, aux.len() as u64 * 4);
        self.gpu.queue.write_buffer(&aux_buf, 0, &le_bytes(&aux));
        let g = groups(n as u64 * 3, self.maxg()).ok_or(Fail::Decline)?;
        let mut enc = self.encoder();
        enc.clear_buffer(&self.status, 0, Some(4));
        let bg = self.bind_group(f, &f.params, &f.coeffs, &out, &aux_buf);
        dispatch(&mut enc, &[(&self.pipes.write, "prores-enc-write", g)], &bg);
        let data = self.gpu.read(enc, &[(&self.status, 0, 16), (&out, 0, bytes)])?;
        if data.get(..4) != Some(&[0; 4]) {
            return Err(Fail::Decline);
        }
        split_components(data.get(16..).ok_or(Fail::Decline)?, &aux, want)
    }

    /// The alpha lengths `alpha` wrote, then the packed coded alpha of every slice.
    fn pack_alpha(&mut self, f: &Loaded) -> Result<Vec<Vec<u8>>, Fail> {
        let n = f.n;
        let lens = self.gpu.read(self.encoder(), &[(&self.status, 0, 16), (&f.alpha_buf, 0, n as u64 * 4)])?;
        let words = words_of(&lens);
        if words.get(1) != Some(&0) {
            return Err(Fail::Decline);
        }
        let lens = words.get(4..4 + n).ok_or(Fail::Decline)?;
        let mut dst = Vec::with_capacity(n);
        let mut at = 0u64;
        for (len, &mbs) in lens.iter().zip(&f.mbs) {
            if u64::from(*len) > u64::from(mbs) * ALPHA_WORDS_PER_MB * 4 {
                return Err(Fail::Decline);
            }
            dst.push(u32::try_from(at).map_err(|_| Fail::Decline)?);
            at += u64::from(*len).div_ceil(4);
        }
        let bytes = at * 4;
        if bytes == 0 || bytes > max_binding(&self.gpu.device.limits()) {
            return Err(Fail::Decline);
        }
        let device = &self.gpu.device;
        let out = self.pools.alpha_out.get(device, bytes);
        let aux = self.pools.aux.get(device, n as u64 * 4);
        self.gpu.queue.write_buffer(&aux, 0, &le_bytes(&dst));
        let g = groups(n as u64, self.maxg()).ok_or(Fail::Decline)?;
        let mut enc = self.encoder();
        let bg = self.bind_group(f, &f.params, &f.alpha_buf, &out, &aux);
        dispatch(&mut enc, &[(&self.pipes.pack, "prores-enc-alpha-pack", g)], &bg);
        let data = self.gpu.read(enc, &[(&out, 0, bytes)])?;
        lens.iter().zip(&dst).map(|(&len, &o)| data.get(o as usize * 4..o as usize * 4 + len as usize).map(<[u8]>::to_vec).ok_or(Fail::Decline)).collect()
    }

    /// Bytes of the rate control's buffer for `n` slices, `None` beyond the device's limits.
    fn rc_bytes(&self, n: usize) -> Option<u64> {
        let n = n as u64;
        let bytes = (R_QS as u64 + n * (8 + 3 * RC_SLOTS)) * 4;
        let aux = (4 + n) * 4;
        let max = max_binding(&self.gpu.device.limits());
        // (the shaders address the buffer with 32-bit word indices)
        let ok = bytes <= max && bytes / 4 <= u64::from(u32::MAX) && aux <= max && groups(n * 3, self.maxg()).is_some();
        ok.then_some(bytes)
    }

    /// `choose_quantisers` in compute passes: `RC_ROUNDS` rounds of measuring and picking, the
    /// refinement and the size guard, in one submission with one read-back.
    fn rate_control(&mut self, f: &Loaded, hint: u8, budget: u64, limits: &[u64]) -> Result<Choice, Fail> {
        let n = f.n;
        let bytes = self.rc_bytes(n).ok_or(Fail::Decline)?;
        let mut aux = vec![budget as u32, (budget >> 32) as u32, u32::from(hint), 0];
        // (a slice's bits never pass 32 bits on the GPU, so a larger limit compares the same)
        aux.extend(limits.iter().map(|&l| u32::try_from(l).unwrap_or(u32::MAX)));
        let mut state = [0u32; R_QS];
        state[R_Q] = u32::from(hint.clamp(*Q_RANGE.start(), *Q_RANGE.end()));
        let device = &self.gpu.device;
        let rc = self.pools.rc.get(device, bytes);
        let aux_buf = self.pools.aux.get(device, aux.len() as u64 * 4);
        self.gpu.queue.write_buffer(&rc, 0, &le_bytes(&state));
        self.gpu.queue.write_buffer(&aux_buf, 0, &le_bytes(&aux));
        let maxg = self.maxg();
        let (comps, slices) = (groups(n as u64 * 3, maxg).ok_or(Fail::Decline)?, groups(n as u64, maxg).ok_or(Fail::Decline)?);
        let [bits, pick, refine, last] = &self.pipes.rc;
        let mut passes = Vec::with_capacity(RC_ROUNDS * 2 + 2);
        for _ in 0..RC_ROUNDS {
            passes.extend([(bits, "prores-enc-rc-bits", comps), (pick, "prores-enc-rc-pick", (1, 1))]);
        }
        passes.extend([(refine, "prores-enc-rc-refine", (1, 1)), (last, "prores-enc-rc-final", slices)]);
        let mut enc = self.encoder();
        enc.clear_buffer(&self.status, 0, Some(4));
        let bg = self.bind_group(f, &f.params, &f.coeffs, &rc, &aux_buf);
        dispatch(&mut enc, &passes, &bg);
        let data = self.gpu.read(enc, &[(&self.status, 0, 16), (&rc, 0, (R_QS + 7 * n) as u64 * 4)])?;
        parse_choice(&words_of(&data), n)
    }

    /// Run `step` on the loaded frame inside error scopes; any failure drops the frame.
    fn step<T>(&mut self, what: &str, step: impl FnOnce(&mut Self, &Loaded) -> Result<T, Fail>) -> Option<T> {
        let f = self.frame.take()?;
        let scopes = self.gpu.scopes();
        let r = step(self, &f);
        let r = self.gpu.settle(what, scopes, r);
        if r.is_some() {
            self.frame = Some(f);
        }
        r
    }
}

/// The coded bytes of every component from the read-back `data` (laid out by `aux`'s word
/// offsets), each as long as its measured bits say.
fn split_components(data: &[u8], aux: &[u32], want: &[[u32; 3]]) -> Result<Vec<[Vec<u8>; 3]>, Fail> {
    let comp = |i: usize, bits: u32| -> Result<Vec<u8>, Fail> {
        let at = *aux.get(i * 2).ok_or(Fail::Decline)? as usize * 4;
        data.get(at..at + bits as usize / 8).map(<[u8]>::to_vec).ok_or(Fail::Decline)
    };
    want.iter().enumerate().map(|(s, [y, cb, cr])| Ok([comp(s * 3, *y)?, comp(s * 3 + 1, *cb)?, comp(s * 3 + 2, *cr)?])).collect()
}

/// A quantiser index the rate control wrote, checked.
fn q_of(w: u32) -> Result<u8, Fail> {
    u8::try_from(w).ok().filter(|q| Q_RANGE.contains(q)).ok_or(Fail::Decline)
}

/// The rate control's answer from the read-back (the status words, then its buffer up to the kept
/// rows); `Decline` unless the shaders finished without a fault and every quantiser is in range.
fn parse_choice(words: &[u32], n: usize) -> Result<Choice, Fail> {
    let rc = words.get(4..).ok_or(Fail::Decline)?;
    if words.first() != Some(&0) || rc.get(R_DONE) != Some(&1) {
        return Err(Fail::Decline);
    }
    let qg = q_of(*rc.get(R_QG).ok_or(Fail::Decline)?)?;
    let qs = rc.get(R_QS..R_QS + n).ok_or(Fail::Decline)?.iter().map(|&q| q_of(q)).collect::<Result<Vec<u8>, Fail>>()?;
    let rows = rc.get(R_QS + n..R_QS + 7 * n).ok_or(Fail::Decline)?;
    let mut kept = Vec::with_capacity(2);
    for (r, at) in [(R_ROW0_Q, 0), (R_ROW1_Q, 3 * n)] {
        let q = *rc.get(r).ok_or(Fail::Decline)?;
        if q != 0 {
            let row = rows.get(at..at + 3 * n).ok_or(Fail::Decline)?;
            kept.push((q_of(q)?, row.as_chunks::<3>().0.to_vec()));
        }
    }
    Ok((qs, qg, kept))
}

/// The constants `prores_enc.wgsl` reads (see the shader).
pub(crate) fn consts() -> Vec<u32> {
    let mut c = vec![0u32; CONST_WORDS];
    for (d, b) in c.iter_mut().zip(tables::basis().iter().flatten()) {
        *d = b.to_bits();
    }
    for (d, &s) in c.iter_mut().skip(C_SCAN).zip(tables::SCAN_PROGRESSIVE.iter()) {
        *d = u32::from(s);
    }
    for (d, &s) in c.iter_mut().skip(C_SCAN_INTERLACED).zip(tables::SCAN_INTERLACED.iter()) {
        *d = u32::from(s);
    }
    let cb = |(rice, exp, rq): (u8, u8, u8)| u32::from(rice) | u32::from(exp) << 8 | u32::from(rq) << 16;
    let books = std::iter::once(tables::first_dc()).chain(tables::dc()).chain(tables::run()).chain(tables::level());
    for (d, b) in c.iter_mut().skip(C_CB).zip(books) {
        *d = cb(b);
    }
    for (d, v) in c.iter_mut().skip(C_AC_ROUND).zip([tables::AC_ROUND.to_bits(), tables::DC_CTX_INIT, tables::RUN_CTX_INIT, tables::LEVEL_CTX_INIT]) {
        *d = v;
    }
    // the CPU encoder's own expression, so the quantisers agree bit for bit
    for (q, d) in c.iter_mut().skip(C_INV).take(C_SCAN_INTERLACED - C_INV).enumerate() {
        *d = (1.0 / (4.0 * tables::qscale(q as u8) as f32)).to_bits();
    }
    c
}

impl SliceBackend for ProResGpuEncoder {
    fn load(&mut self, cfg: &EncoderConfig, frame: &Frame, slices: &[SliceJob]) -> bool {
        self.frame = None;
        if self.gpu.disabled() || slices.is_empty() {
            return false;
        }
        let Some(plan) = Plan::new(cfg, &Shape::of(frame), slices, &self.gpu.device.limits()).filter(|p| planes_cover(p, frame)) else { return false };
        let scopes = self.gpu.scopes();
        let r = self.upload(plan, frame);
        self.frame = self.gpu.settle("loading a ProRes frame", scopes, r);
        self.frame.is_some()
    }

    /// Planes a renderer on this device left there ([`ResidentPlanes`]): coded from that buffer,
    /// without an upload. Planes of another device, size, chroma format or alpha are declined.
    fn load_resident(&mut self, cfg: &EncoderConfig, frame: &dyn Any, slices: &[SliceJob]) -> bool {
        self.frame = None;
        let Some(planes) = frame.downcast_ref::<ResidentPlanes>() else { return false };
        if self.gpu.disabled() || slices.is_empty() {
            return false;
        }
        let Some(plan) = Plan::new(cfg, &Shape::of_resident(planes), slices, &self.gpu.device.limits()) else { return false };
        let chroma_422 = cfg.profile.chroma() == ChromaFormat::Yuv422;
        // (the planes carry exactly the alpha the frame codes: the layout has no room for more)
        if !planes.fits(&self.gpu.device, (cfg.width, cfg.height), chroma_422, plan.alpha, plan.plane_bytes) {
            return false;
        }
        let scopes = self.gpu.scopes();
        let r = self.start(plan, planes.buffer.clone());
        self.frame = self.gpu.settle("loading a resident ProRes frame", scopes, r);
        self.frame.is_some()
    }

    fn bits(&mut self, q: u8) -> Option<Vec<[u32; 3]>> {
        if !Q_RANGE.contains(&q) {
            return None;
        }
        let f = self.frame.as_ref()?;
        if let Some(v) = f.measured(q) {
            return Some(v.clone());
        }
        // q and its neighbours, those not measured yet
        let qs: Vec<u8> =
            [Some(q), q.checked_sub(1), q.checked_add(1)].into_iter().flatten().filter(|p| Q_RANGE.contains(p) && f.measured(*p).is_none()).collect();
        let got = self.step("measuring ProRes bits", |me, f| me.measure(f, &qs))?;
        let f = self.frame.as_mut()?;
        for (p, v) in qs.into_iter().zip(got) {
            if v.len() == f.n {
                f.measured.push((p, v));
            }
        }
        f.measured(q).cloned()
    }

    fn write(&mut self, qs: &[u8]) -> Option<Vec<[Vec<u8>; 3]>> {
        if qs.len() != self.frame.as_ref()?.n {
            return None;
        }
        let mut want = Vec::with_capacity(qs.len());
        for (i, &q) in qs.iter().enumerate() {
            if self.frame.as_ref()?.measured(q).is_none() {
                self.bits(q)?;
            }
            want.push(*self.frame.as_ref()?.measured(q)?.get(i)?);
        }
        self.step("writing ProRes slices", |me, f| me.code(f, qs, &want))
    }

    fn choose(&mut self, hint: u8, budget: u64, limits: &[u64]) -> Option<(Vec<u8>, u8)> {
        let n = self.frame.as_ref()?.n;
        // (a frame too large for the rate control's buffer keeps its rate control on the CPU)
        if limits.len() != n || self.rc_bytes(n).is_none() {
            return None;
        }
        let (qs, qg, kept) = self.step("ProRes rate control", |me, f| me.rate_control(f, hint, budget, limits))?;
        // the rows the encoder asks for next, measured on the way
        let f = self.frame.as_mut()?;
        for (q, v) in kept {
            if v.len() == f.n && f.measured(q).is_none() {
                f.measured.push((q, v));
            }
        }
        Some((qs, qg))
    }

    fn alpha(&mut self) -> Option<Vec<Vec<u8>>> {
        if !self.frame.as_ref()?.alpha {
            return None;
        }
        self.step("coding ProRes alpha", |me, f| me.pack_alpha(f))
    }
}
