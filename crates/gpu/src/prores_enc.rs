//! ProRes encoding on the GPU: [`ProResGpuEncoder`] is a [`SliceBackend`] of the CPU encoder
//! (`filmcraft_prores::Encoder::encode_with`) that runs the per-block work in compute shaders
//! (`prores_enc.wgsl`): forward DCT, quantisation, bit counting, entropy coding and the alpha
//! channel's run-length coding. The CPU encoder keeps the rate control (over the bits measured
//! here), the headers and the frame assembly, and encodes a frame itself whenever this backend
//! declines or fails.
//!
//! `load` uploads the planes and transforms every block once; `bits(q)` measures every slice at `q`
//! and its two neighbours in one dispatch (the rate control's next question is nearly always one
//! of them) and keeps the answers for the frame; `write` codes every component into its own
//! word-aligned range and reads back only those ranges.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use filmcraft_prores::layout::tables;
use filmcraft_prores::{ChromaFormat, EncoderConfig, Frame, Interlace, SliceBackend, SliceJob};

use crate::intra::{Pooled, bind, dispatch, groups, storage, uniform};
use crate::intra_enc::{Fail, Gpu, fits, le_bytes, max_binding, own_device, pack_planes, words_of};

/// Words of the constants (see `prores_enc.wgsl`): basis, scan, codebooks, AC rounding and
/// contexts, then `1 / (4 qscale(q))` for every quantiser index from `C_INV`.
const C_SCAN: usize = 64;
const C_CB: usize = 128;
const C_AC_ROUND: usize = 162;
const C_INV: usize = 192;
const CONST_WORDS: usize = C_INV + 225;
/// Quantiser indices the encoder uses (RDD 36 allows 1..=224).
const Q_RANGE: std::ops::RangeInclusive<u8> = 1..=224;
/// Worst-case words of a macroblock's coded alpha: 256 values of at most 18 bits each.
const ALPHA_WORDS_PER_MB: u64 = 144;

/// `Params` in `prores_enc.wgsl` (24 words).
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
            0,
            0,
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

impl Plan {
    /// `None` when the frame is not one this backend codes, or does not fit `limits`.
    fn new(cfg: &EncoderConfig, frame: &Frame, jobs: &[SliceJob], limits: &wgpu::Limits) -> Option<Plan> {
        let (w, h) = (cfg.width, cfg.height);
        let chroma = cfg.profile.chroma();
        if frame.width != w || frame.height != h || frame.chroma != chroma || frame.interlace != Interlace::Progressive || !(8..=16).contains(&frame.bit_depth)
        {
            return None;
        }
        let cw = chroma.chroma_width(w);
        let alpha = cfg.encode_alpha && chroma == ChromaFormat::Yuv444 && frame.alpha.is_some();
        let (luma, croma) = (u64::from(w) * u64::from(h), u64::from(cw) * u64::from(h));
        let short = |p: &[u16], n: u64| (p.len() as u64) < n;
        if short(&frame.y, luma) || short(&frame.cb, croma) || short(&frame.cr, croma) || (alpha && frame.alpha.as_deref().is_none_or(|a| short(a, luma))) {
            return None;
        }
        let (mb_w, mb_h) = (w.div_ceil(16), h.div_ceil(16));
        let cpm: u32 = if chroma == ChromaFormat::Yuv444 { 4 } else { 2 };
        let bpm = 4 + 2 * cpm;
        let (slice_words, mbcol_base) = slice_table(jobs, mb_w, mb_h, bpm)?;
        let total_blocks = u64::from(mb_w) * u64::from(mb_h) * u64::from(bpm);
        let samples = luma + 2 * croma + if alpha { luma } else { 0 };
        let plane_bytes = (samples * 2).next_multiple_of(4);
        let coeff_bytes = total_blocks * 64 * 4;
        let n = jobs.len();
        let alpha_bytes = if alpha { (n as u64 + u64::from(mb_w) * u64::from(mb_h) * ALPHA_WORDS_PER_MB) * 4 } else { 16 };
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
        };
        let mbs = jobs.iter().map(|j| j.mbs).collect();
        Some(Plan { params, n, alpha, mbs, slice_words, plane_bytes, coeff_bytes, alpha_bytes })
    }
}

/// The slice records (mb_x, mb_y, mbs, first block) and the macroblock-column table (first column
/// of the slice | its width << 16) with its word offset. `None` unless `jobs` cover the frame in
/// coded order with the same slices in every row.
fn slice_table(jobs: &[SliceJob], mb_w: u32, mb_h: u32, bpm: u32) -> Option<(Vec<u32>, u32)> {
    let mut words = Vec::with_capacity(jobs.len() * 4 + mb_w as usize);
    let mut cols = vec![u32::MAX; mb_w as usize];
    let (mut x, mut y) = (0u32, 0u32);
    for j in jobs {
        let end = j.mb_x.checked_add(j.mbs)?;
        if j.mb_x != x || j.mb_y != y || !matches!(j.mbs, 1 | 2 | 4 | 8) || end > mb_w || y >= mb_h {
            return None;
        }
        let col = j.mb_x | j.mbs << 16;
        for c in cols.get_mut(j.mb_x as usize..end as usize)? {
            if *c != u32::MAX && *c != col {
                return None;
            }
            *c = col;
        }
        let first = (y.checked_mul(mb_w)?.checked_add(x)?).checked_mul(bpm)?;
        words.extend_from_slice(&[j.mb_x, j.mb_y, j.mbs, first]);
        (x, y) = if end == mb_w { (0, y + 1) } else { (end, y) };
    }
    if (x, y) != (0, mb_h) {
        return None;
    }
    let base = u32::try_from(words.len()).ok()?;
    words.extend_from_slice(&cols);
    Some((words, base))
}

struct Pipes {
    fdct: wgpu::ComputePipeline,
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
        Pipes { fdct: pipeline("fdct"), bits: pipeline("bits"), write: pipeline("write"), alpha: pipeline("alpha"), pack: pipeline("alpha_pack"), bgl }
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
        let Plan { params, n, alpha, mbs, slice_words, plane_bytes, coeff_bytes, alpha_bytes } = plan;
        let (luma, croma) = (params.off_cb as usize, (params.off_cr - params.off_cb) as usize);
        let mut planes = vec![frame.y.get(..luma), frame.cb.get(..croma), frame.cr.get(..croma)];
        if alpha {
            planes.push(frame.alpha.as_deref().and_then(|a| a.get(..luma)));
        }
        let planes: Vec<&[u16]> = planes.into_iter().collect::<Option<_>>().ok_or(Fail::Decline)?;
        pack_planes(&mut self.staging, &planes, usize::try_from(plane_bytes).map_err(|_| Fail::Decline)?).ok_or(Fail::Decline)?;
        let device = &self.gpu.device;
        let f = Loaded {
            params,
            n,
            alpha,
            mbs,
            planes: self.pools.planes.get(device, plane_bytes),
            slices: self.pools.slices.get(device, slice_words.len() as u64 * 4),
            coeffs: self.pools.coeffs.get(device, coeff_bytes),
            alpha_buf: self.pools.alpha.get(device, alpha_bytes),
            measured: Vec::new(),
        };
        let queue = &self.gpu.queue;
        queue.write_buffer(&f.planes, 0, &self.staging);
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

/// The constants `prores_enc.wgsl` reads (see the shader).
fn consts() -> Vec<u32> {
    let mut c = vec![0u32; CONST_WORDS];
    for (d, b) in c.iter_mut().zip(tables::basis().iter().flatten()) {
        *d = b.to_bits();
    }
    for (d, &s) in c.iter_mut().skip(C_SCAN).zip(tables::SCAN_PROGRESSIVE.iter()) {
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
    for (q, d) in c.iter_mut().skip(C_INV).enumerate() {
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
        let Some(plan) = Plan::new(cfg, frame, slices, &self.gpu.device.limits()) else { return false };
        let scopes = self.gpu.scopes();
        let r = self.upload(plan, frame);
        self.frame = self.gpu.settle("loading a ProRes frame", scopes, r);
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

    fn alpha(&mut self) -> Option<Vec<Vec<u8>>> {
        if !self.frame.as_ref()?.alpha {
            return None;
        }
        self.step("coding ProRes alpha", |me, f| me.pack_alpha(f))
    }
}
