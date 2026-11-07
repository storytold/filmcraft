//! DNxHR encoding on the GPU: [`DnxGpuEncoder`] is a [`MacroblockBackend`] of the CPU encoder
//! (`filmcraft_dnx::Encoder::encode_with`) that runs the per-macroblock work in compute shaders
//! (`dnx_enc.wgsl`): forward DCT and weighting, bit counting at a quantiser, entropy coding, and
//! alpha (DCT-coded like luma, or lossless run-length coding), and the rate control (`choose`, the
//! CPU's algorithm step for step). The CPU encoder keeps the headers and the frame assembly, checks
//! every answer, and encodes a frame itself whenever this backend declines or fails.
//!
//! `load` uploads the planes, transforms every block once and counts the lossless alpha; `choose`
//! runs the whole rate control in one submission and reads back only the quantisers (and the bits
//! measured at the two it chose, for `bits`); `bits(q)`
//! measures every macroblock at `q` and its two neighbours in one dispatch and keeps the answers for
//! the frame; `write` codes every macroblock part into its own word-aligned range and reads back
//! only those ranges. Field-coded (interlaced) frames are declined: the CPU encoder writes
//! progressive frames only.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use filmcraft_dnx::layout::tables;
use filmcraft_dnx::{AlphaCoding, ChromaFormat, EncoderConfig, Frame, Interlace, MacroblockBackend};

use crate::intra::{Pooled, bind, dispatch, groups, storage, uniform};
use crate::intra_enc::{Fail, Gpu, fits, le_bytes, max_binding, own_device, pack_planes, words_of};

/// Rounds of measurement the rate control runs on the GPU: the most distinct quantisers the CPU's
/// `rate_control` measures. Its search measures 1, the hint, half the hint and then bisects (at
/// most 11 steps over 1..=2047); the refinement reads the uniform quantiser (not measured when the
/// search ends at 2047 without having asked) and the one finer. A hint at 0 against a frame that
/// first fits at 2047 takes 14; `the_rate_control_rounds_cover_every_cpu_search` checks hints
/// across the range against every place the frame can start to fit.
pub(crate) const RC_ROUNDS: usize = 14;

/// Words of the constants (see `dnx_enc.wgsl`).
const C_ZIG: usize = 64;
const C_WEIGHT: usize = 128;
const C_LAYOUT: usize = 256;
const C_AMP: usize = 272;
const C_EOB: usize = 528;
const C_RUN: usize = 529;
const C_DC: usize = 592;
const C_NDC: usize = 608;
const CONST_WORDS: usize = 612;
/// The quantiser scale factors a macroblock header holds.
const Q_RANGE: std::ops::RangeInclusive<u32> = 1..=2047;
/// Words `write` takes per macroblock in `aux`.
const AUX_WORDS: usize = 5;
/// Words of the rate control's buffer (`dnx_enc.wgsl`): the state, the uniform quantiser, the
/// done flag, the kept rows' quantisers, then from `R_QS` the answer.
const R_Q: usize = 1;
const R_QG: usize = 5;
const R_DONE: usize = 6;
const R_ROW0_Q: usize = 8;
const R_ROW1_Q: usize = 10;
const R_QS: usize = 64;
/// Slots of the rate control's table: one per quantiser it measures.
const RC_SLOTS: u64 = 16;

/// The rate control's answer: every macroblock's quantiser and the rows it measured at the
/// uniform quantiser and the finer one (for the cache of `bits`).
type Choice = (Vec<u32>, Vec<(u32, Vec<[u32; 2]>)>);

/// `Params` in `dnx_enc.wgsl` (32 words).
#[derive(Clone, Copy, Default)]
struct Params {
    qv: [u32; 4],
    nq: u32,
    nw: u32,
    ns: u32,
    nmb: u32,
    nblk: u32,
    ncol: u32,
    width: u32,
    height: u32,
    cwidth: u32,
    cheight: u32,
    cmbw: u32,
    off_cb: u32,
    off_cr: u32,
    off_a: u32,
    shift: i32,
    depth: u32,
    mid: f32,
    inv_scale: f32,
    p: f32,
    dc_max: f32,
    total_blocks: u32,
    p_bits: u32,
    max_amp: u32,
    ycbcr444: u32,
    alpha_mode: u32,
    rle_base: u32,
}

impl Params {
    fn bytes(&self) -> Vec<u8> {
        let [q0, q1, q2, q3] = self.qv;
        le_bytes(&[
            q0,
            q1,
            q2,
            q3,
            self.nq,
            self.nw,
            self.ns,
            self.nmb,
            self.nblk,
            self.ncol,
            self.width,
            self.height,
            self.cwidth,
            self.cheight,
            self.cmbw,
            self.off_cb,
            self.off_cr,
            self.off_a,
            self.shift as u32,
            self.depth,
            self.mid.to_bits(),
            self.inv_scale.to_bits(),
            self.p.to_bits(),
            self.dc_max.to_bits(),
            0,
            self.total_blocks,
            self.p_bits,
            self.max_amp,
            self.ycbcr444,
            self.alpha_mode,
            self.rle_base,
            0,
        ])
    }
}

/// The frame geometry, constants and buffer sizes of one `load`, checked against the device's
/// limits.
struct Plan {
    params: Params,
    consts: Vec<u32>,
    alpha: bool,
    plane_bytes: u64,
    coeff_bytes: u64,
}

impl Plan {
    /// `None` when the frame is not one this backend codes, or does not fit `limits`.
    fn new(cfg: &EncoderConfig, frame: &Frame, limits: &wgpu::Limits) -> Option<Plan> {
        let (w, h) = (cfg.width, cfg.height);
        let chroma = cfg.chroma();
        let progressive = frame.interlace == Interlace::Progressive;
        if frame.width != w || frame.height != h || frame.chroma != chroma || !progressive || !(8..=16).contains(&frame.bit_depth) {
            return None;
        }
        if !matches!(cfg.bit_depth, 8 | 10 | 12) || !matches!(chroma, ChromaFormat::Yuv422 | ChromaFormat::Yuv444) {
            return None;
        }
        let (cw, ch) = (frame.chroma_width(), frame.chroma_height());
        let alpha = cfg.alpha.is_some();
        let (luma, croma) = (u64::from(w) * u64::from(h), u64::from(cw) * u64::from(ch));
        let short = |p: &[u16], n: u64| (p.len() as u64) < n;
        if short(&frame.y, luma) || short(&frame.cb, croma) || short(&frame.cr, croma) || (alpha && frame.alpha.as_deref().is_none_or(|a| short(a, luma))) {
            return None;
        }
        let consts = consts(cfg)?;
        let (nw, ns) = (w.div_ceil(16), h.div_ceil(16));
        let nmb = u64::from(nw) * u64::from(ns);
        let ncol = tables::blocks(chroma).len() as u64;
        let nblk = if cfg.alpha == Some(AlphaCoding::Dct) { ncol + 4 } else { ncol };
        let total_blocks = nmb * nblk;
        let samples = luma + 2 * croma + if alpha { luma } else { 0 };
        let plane_bytes = (samples * 2).next_multiple_of(4);
        let rle_base = total_blocks * 64;
        let coeff_bytes = (rle_base + nmb) * 4;
        let maxg = limits.max_compute_workgroups_per_dimension;
        if !fits(limits, coeff_bytes, plane_bytes, w, h)
            || samples > u64::from(u32::MAX)
            || groups(total_blocks, maxg).is_none()
            || groups(nmb * 4, maxg).is_none()
        {
            return None;
        }
        let depth = u32::from(cfg.bit_depth);
        let cid = tables::cid(cfg.profile.cid())?;
        let params = Params {
            nw,
            ns,
            nmb: u32::try_from(nmb).ok()?,
            nblk: nblk as u32,
            ncol: ncol as u32,
            width: w,
            height: h,
            cwidth: cw,
            cheight: ch,
            cmbw: if chroma == ChromaFormat::Yuv444 { 16 } else { 8 },
            off_cb: u32::try_from(luma).ok()?,
            off_cr: u32::try_from(luma + croma).ok()?,
            off_a: u32::try_from(luma + 2 * croma).ok()?,
            shift: i32::from(cfg.bit_depth) - i32::from(frame.bit_depth),
            depth,
            mid: (1u32 << (depth - 1)) as f32,
            inv_scale: if depth == 12 { 0.25 } else { 1.0 },
            p: cid.p as f32,
            dc_max: if depth == 8 { 1023.0 } else { 4095.0 },
            total_blocks: u32::try_from(total_blocks).ok()?,
            p_bits: if depth == 8 { 4 } else { 6 },
            max_amp: if depth == 8 { 1024 } else { 4096 },
            ycbcr444: u32::from(chroma == ChromaFormat::Yuv444 && !frame.rgb),
            alpha_mode: match cfg.alpha {
                None => 0,
                Some(AlphaCoding::Dct) => 1,
                Some(AlphaCoding::Lossless) => 2,
            },
            rle_base: u32::try_from(rle_base).ok()?,
            ..Params::default()
        };
        Some(Plan { params, consts, alpha, plane_bytes, coeff_bytes })
    }
}

/// The constants `dnx_enc.wgsl` reads for `cfg` (see the shader).
fn consts(cfg: &EncoderConfig) -> Option<Vec<u32>> {
    let cid = tables::cid(cfg.profile.cid())?;
    let vlc = tables::encode_vlc(cfg.profile.cid())?;
    let mut c = vec![0u32; CONST_WORDS];
    for (d, b) in c.iter_mut().zip(tables::basis().iter().flatten()) {
        *d = b.to_bits();
    }
    for (d, z) in c.iter_mut().skip(C_ZIG).zip(tables::zigzag()) {
        *d = u32::from(z);
    }
    for (i, wt) in cid.weights.iter().enumerate() {
        for (d, &v) in c.iter_mut().skip(C_WEIGHT + 64 * i).zip(wt.iter()) {
            *d = u32::from(v);
        }
    }
    let alpha: &[(u8, u8, u8)] = if cfg.alpha == Some(AlphaCoding::Dct) { tables::alpha_blocks() } else { &[] };
    for (d, &(pl, x, y)) in c.iter_mut().skip(C_LAYOUT).zip(tables::blocks(cfg.chroma()).iter().chain(alpha)) {
        *d = u32::from(pl) | u32::from(x) << 8 | u32::from(y) << 16;
    }
    let code = |(c, l): (u16, u8)| u32::from(c) | u32::from(l) << 16;
    for (d, &v) in c.iter_mut().skip(C_AMP).zip(vlc.amp_code.iter().flatten().flatten()) {
        *d = code(v);
    }
    *c.get_mut(C_EOB)? = code(vlc.eob);
    for (d, &v) in c.iter_mut().skip(C_RUN).zip(vlc.run_code.iter()) {
        *d = code(v);
    }
    let ndc = vlc.dc_code.len().min(C_NDC - C_DC);
    for (d, &v) in c.iter_mut().skip(C_DC).zip(vlc.dc_code.iter().take(ndc)) {
        *d = code(v);
    }
    *c.get_mut(C_NDC)? = ndc as u32;
    Some(c)
}

/// A quantiser scale factor the rate control wrote, checked.
fn q_of(w: u32) -> Result<u32, Fail> {
    Some(w).filter(|q| Q_RANGE.contains(q)).ok_or(Fail::Decline)
}

/// The rate control's answer from the read-back (the status words, then its buffer up to the kept
/// rows); `Decline` unless the shaders finished without a fault and every quantiser is in range.
fn parse_choice(words: &[u32], nmb: usize) -> Result<Choice, Fail> {
    let rc = words.get(4..).ok_or(Fail::Decline)?;
    // (word 1: the lossless alpha counted at load)
    if words.get(..2) != Some(&[0, 0]) || rc.get(R_DONE) != Some(&1) {
        return Err(Fail::Decline);
    }
    q_of(*rc.get(R_QG).ok_or(Fail::Decline)?)?;
    let qs = rc.get(R_QS..R_QS + nmb).ok_or(Fail::Decline)?.iter().map(|&q| q_of(q)).collect::<Result<Vec<u32>, Fail>>()?;
    let rows = rc.get(R_QS + nmb..R_QS + 5 * nmb).ok_or(Fail::Decline)?;
    let mut kept = Vec::with_capacity(2);
    for (r, at) in [(R_ROW0_Q, 0), (R_ROW1_Q, 2 * nmb)] {
        let q = *rc.get(r).ok_or(Fail::Decline)?;
        if q != 0 {
            let row = rows.get(at..at + 2 * nmb).ok_or(Fail::Decline)?;
            kept.push((q_of(q)?, row.as_chunks::<2>().0.to_vec()));
        }
    }
    Ok((qs, kept))
}

struct Pipes {
    transform: wgpu::ComputePipeline,
    rc: [wgpu::ComputePipeline; 5],
    rle: wgpu::ComputePipeline,
    bits: wgpu::ComputePipeline,
    write: wgpu::ComputePipeline,
    bgl: wgpu::BindGroupLayout,
}

impl Pipes {
    fn new(device: &wgpu::Device) -> Pipes {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("filmcraft-dnx-enc"),
            source: wgpu::ShaderSource::Wgsl(include_str!("dnx_enc.wgsl").into()),
        });
        let uniform_entry = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dnx-enc"),
            entries: &[uniform_entry, storage(1, true), storage(2, true), storage(3, false), storage(4, false), storage(5, true), storage(6, false)],
        });
        let layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("dnx-enc"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("dnx-enc"),
                layout: Some(&layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let rc = [pipeline("rc_bits"), pipeline("rc_lines"), pipeline("rc_pick"), pipeline("rc_refine"), pipeline("rc_final")];
        Pipes { transform: pipeline("transform"), rc, rle: pipeline("rle"), bits: pipeline("bits"), write: pipeline("write"), bgl }
    }
}

/// The buffers of the loaded frame.
struct Loaded {
    params: Params,
    planes: wgpu::Buffer,
    consts: wgpu::Buffer,
    coeffs: wgpu::Buffer,
    /// Every macroblock's (colour, alpha) bits at every quantiser measured for this frame.
    measured: Vec<(u32, Vec<[u32; 2]>)>,
}

impl Loaded {
    fn measured(&self, q: u32) -> Option<&Vec<[u32; 2]>> {
        self.measured.iter().find(|(mq, _)| *mq == q).map(|(_, v)| v)
    }

    fn nmb(&self) -> usize {
        self.params.nmb as usize
    }
}

struct Pools {
    planes: Pooled,
    consts: Pooled,
    coeffs: Pooled,
    out: Pooled,
    aux: Pooled,
    rc: Pooled,
}

/// DNxHR encoding's per-macroblock work in compute shaders (a [`MacroblockBackend`]).
pub struct DnxGpuEncoder {
    gpu: Gpu,
    pipes: Pipes,
    status: wgpu::Buffer,
    pools: Pools,
    frame: Option<Loaded>,
    /// The packed planes of the last frame (reused).
    staging: Vec<u8>,
}

impl DnxGpuEncoder {
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
        if l.max_storage_buffers_per_shader_stage < 6
            || l.max_compute_invocations_per_workgroup < 64
            || l.max_compute_workgroup_size_x < 64
            || l.max_compute_workgroups_per_dimension < 1024
        {
            return None;
        }
        let mut gpu = Gpu::new(device, queue, lost);
        let scopes = gpu.scopes();
        let made = Self::make(&gpu.device);
        let (pipes, status) = gpu.settle("creating the DNxHR encode pipelines", scopes, Ok(made))?;
        let storage_dst = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
        let pools = Pools {
            planes: Pooled::new("dnx-enc-planes", storage_dst),
            consts: Pooled::new("dnx-enc-consts", storage_dst),
            coeffs: Pooled::new("dnx-enc-coeffs", wgpu::BufferUsages::STORAGE),
            out: Pooled::new("dnx-enc-out", wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC),
            aux: Pooled::new("dnx-enc-aux", storage_dst),
            rc: Pooled::new("dnx-enc-rc", storage_dst | wgpu::BufferUsages::COPY_SRC),
        };
        Some(DnxGpuEncoder { gpu, pipes, status, pools, frame: None, staging: Vec::new() })
    }

    /// The pipelines and the status words.
    fn make(device: &wgpu::Device) -> (Pipes, wgpu::Buffer) {
        let pipes = Pipes::new(device);
        let status = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dnx-enc-status"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        (pipes, status)
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

    fn bind_group(&self, f: &Loaded, params: &Params, out: &wgpu::Buffer, aux: &wgpu::Buffer) -> wgpu::BindGroup {
        let ubuf = uniform(&self.gpu.device, &self.gpu.queue, &params.bytes());
        self.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dnx-enc"),
            layout: &self.pipes.bgl,
            entries: &[
                bind(&ubuf, 0, None),
                bind(&f.planes, 1, None),
                bind(&f.consts, 2, None),
                bind(&f.coeffs, 3, None),
                bind(out, 4, None),
                bind(aux, 5, None),
                bind(&self.status, 6, None),
            ],
        })
    }

    fn maxg(&self) -> u32 {
        self.gpu.device.limits().max_compute_workgroups_per_dimension
    }

    fn encoder(&self) -> wgpu::CommandEncoder {
        self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("dnx-enc") })
    }

    /// Upload the planes and constants, run `transform` (and `rle`): the loaded frame.
    fn upload(&mut self, plan: Plan, frame: &Frame) -> Result<Loaded, Fail> {
        let Plan { params, consts, alpha, plane_bytes, coeff_bytes } = plan;
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
            planes: self.pools.planes.get(device, plane_bytes),
            consts: self.pools.consts.get(device, consts.len() as u64 * 4),
            coeffs: self.pools.coeffs.get(device, coeff_bytes),
            measured: Vec::new(),
        };
        let queue = &self.gpu.queue;
        queue.write_buffer(&f.planes, 0, &self.staging);
        queue.write_buffer(&f.consts, 0, &le_bytes(&consts));
        let out = self.pools.out.get(device, 16);
        let aux = self.pools.aux.get(device, 16);
        let maxg = self.maxg();
        let mut enc = self.encoder();
        enc.clear_buffer(&self.status, 0, None);
        let bg = self.bind_group(&f, &params, &out, &aux);
        let mut passes = vec![(&self.pipes.transform, "dnx-enc-transform", groups(u64::from(params.total_blocks), maxg).ok_or(Fail::Decline)?)];
        if params.alpha_mode == 2 {
            passes.push((&self.pipes.rle, "dnx-enc-rle", groups(u64::from(params.nmb), maxg).ok_or(Fail::Decline)?));
        }
        dispatch(&mut enc, &passes, &bg);
        self.gpu.queue.submit([enc.finish()]);
        Ok(f)
    }

    /// Every macroblock's (colour, alpha) bits at each of `qs` (one dispatch, one read-back).
    fn measure(&mut self, f: &Loaded, qs: &[u32]) -> Result<Vec<Vec<[u32; 2]>>, Fail> {
        let per = f.nmb() * 2;
        let bytes = (per * qs.len()) as u64 * 4;
        if bytes > max_binding(&self.gpu.device.limits()) || qs.is_empty() || qs.len() > 4 {
            return Err(Fail::Decline);
        }
        let mut params = Params { nq: qs.len() as u32, ..f.params };
        for (v, &q) in params.qv.iter_mut().zip(qs) {
            *v = q;
        }
        let device = &self.gpu.device;
        let out = self.pools.out.get(device, bytes);
        let aux = self.pools.aux.get(device, 16);
        let g = groups((f.nmb() * qs.len()) as u64, self.maxg()).ok_or(Fail::Decline)?;
        let mut enc = self.encoder();
        enc.clear_buffer(&self.status, 0, Some(4));
        let bg = self.bind_group(f, &params, &out, &aux);
        dispatch(&mut enc, &[(&self.pipes.bits, "dnx-enc-bits", g)], &bg);
        let data = self.gpu.read(enc, &[(&self.status, 0, 16), (&out, 0, bytes)])?;
        let words = words_of(&data);
        // (word 1: the lossless alpha counted at load)
        if words.get(..2) != Some(&[0, 0]) {
            return Err(Fail::Decline);
        }
        let bits = words.get(4..).ok_or(Fail::Decline)?;
        Ok(bits.chunks_exact(per).map(|v| v.as_chunks::<2>().0.to_vec()).collect())
    }

    /// Every macroblock part coded at its quantiser into its word-aligned range; the parts in the
    /// order the stream holds them, with their bits.
    fn code(&mut self, f: &Loaded, qs: &[u32], want: &[[u32; 2]]) -> Result<Vec<(Vec<u8>, u32)>, Fail> {
        let (nw, ns, alpha) = (f.params.nw as usize, f.params.ns as usize, f.params.alpha_mode != 0);
        let parts = if alpha { 2 } else { 1 };
        let mut aux = vec![0u32; f.nmb() * AUX_WORDS];
        // the parts in stream order: (word offset, bits)
        let mut order: Vec<(u32, u32)> = Vec::with_capacity(f.nmb() * parts);
        let mut at = 0u64;
        for s in 0..ns {
            for part in 0..parts {
                for mb in 0..nw {
                    let m = s * nw + mb;
                    let bits = *want.get(m).and_then(|w| w.get(part)).ok_or(Fail::Decline)?;
                    let off = u32::try_from(at).map_err(|_| Fail::Decline)?;
                    let q = *qs.get(m).ok_or(Fail::Decline)?;
                    let a = aux.get_mut(m * AUX_WORDS..(m + 1) * AUX_WORDS).ok_or(Fail::Decline)?;
                    for (i, v) in [(part * 2, off), (part * 2 + 1, bits), (4, q)] {
                        *a.get_mut(i).ok_or(Fail::Decline)? = v;
                    }
                    order.push((off, bits));
                    at += u64::from(bits).div_ceil(32);
                }
            }
        }
        let (bytes, aux_bytes) = (at * 4, aux.len() as u64 * 4);
        let max = max_binding(&self.gpu.device.limits());
        if bytes == 0 || bytes > max || aux_bytes > max {
            return Err(Fail::Decline);
        }
        let device = &self.gpu.device;
        let out = self.pools.out.get(device, bytes);
        let aux_buf = self.pools.aux.get(device, aux_bytes);
        self.gpu.queue.write_buffer(&aux_buf, 0, &le_bytes(&aux));
        let g = groups(f.nmb() as u64, self.maxg()).ok_or(Fail::Decline)?;
        let mut enc = self.encoder();
        enc.clear_buffer(&self.status, 0, Some(4));
        let bg = self.bind_group(f, &f.params, &out, &aux_buf);
        dispatch(&mut enc, &[(&self.pipes.write, "dnx-enc-write", g)], &bg);
        let data = self.gpu.read(enc, &[(&self.status, 0, 16), (&out, 0, bytes)])?;
        if data.get(..4) != Some(&[0; 4]) {
            return Err(Fail::Decline);
        }
        let data = data.get(16..).ok_or(Fail::Decline)?;
        order
            .into_iter()
            .map(|(off, bits)| {
                let start = off as usize * 4;
                let end = start + (bits as usize).div_ceil(8);
                data.get(start..end).map(|b| (b.to_vec(), bits)).ok_or(Fail::Decline)
            })
            .collect()
    }

    /// Bytes of the rate control's buffer for `p`'s frame, `None` beyond the device's limits.
    fn rc_bytes(&self, p: &Params) -> Option<u64> {
        let (nmb, ns) = (u64::from(p.nmb), u64::from(p.ns));
        let bytes = (R_QS as u64 + nmb * (5 + 2 * RC_SLOTS) + ns * 2 * RC_SLOTS) * 4;
        // (the shaders address the buffer with 32-bit word indices; the refinement keeps the scan
        // lines in workgroup memory and walks with 32-bit products)
        let addressable = bytes <= max_binding(&self.gpu.device.limits()) && bytes / 4 <= u64::from(u32::MAX);
        let ok = addressable && groups(nmb, self.maxg()).is_some() && ns <= 1024 && nmb <= 1 << 20;
        ok.then_some(bytes)
    }

    /// `rate_control` in compute passes: `RC_ROUNDS` rounds of measuring, adding up and picking, and the
    /// refinement, in one submission with one read-back.
    fn rate_control(&mut self, f: &Loaded, hint: u32, budget: u64) -> Result<Choice, Fail> {
        let nmb = f.nmb();
        let bytes = self.rc_bytes(&f.params).ok_or(Fail::Decline)?;
        let mut state = [0u32; R_QS];
        state[R_Q] = 1;
        let device = &self.gpu.device;
        let rc = self.pools.rc.get(device, bytes);
        let aux = self.pools.aux.get(device, 16);
        self.gpu.queue.write_buffer(&rc, 0, &le_bytes(&state));
        self.gpu.queue.write_buffer(&aux, 0, &le_bytes(&[budget as u32, (budget >> 32) as u32, hint, 0]));
        let mbs = groups(nmb as u64, self.maxg()).ok_or(Fail::Decline)?;
        // (one workgroup per scan line: `rc_bytes` keeps them within a dimension's 1024)
        let lines = (f.params.ns, 1);
        let [bits, sums, pick, refine, last] = &self.pipes.rc;
        let mut passes = Vec::with_capacity(RC_ROUNDS * 3 + 2);
        for _ in 0..RC_ROUNDS {
            passes.extend([(bits, "dnx-enc-rc-bits", mbs), (sums, "dnx-enc-rc-lines", lines), (pick, "dnx-enc-rc-pick", (1, 1))]);
        }
        passes.extend([(refine, "dnx-enc-rc-refine", (1, 1)), (last, "dnx-enc-rc-final", mbs)]);
        let mut enc = self.encoder();
        enc.clear_buffer(&self.status, 0, Some(4));
        let bg = self.bind_group(f, &f.params, &rc, &aux);
        dispatch(&mut enc, &passes, &bg);
        let data = self.gpu.read(enc, &[(&self.status, 0, 16), (&rc, 0, (R_QS + 5 * nmb) as u64 * 4)])?;
        parse_choice(&words_of(&data), nmb)
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

    /// Every macroblock's (colour, alpha) bits at `q`, measured (with its neighbours) if needed.
    fn parts_at(&mut self, q: u32) -> Option<Vec<[u32; 2]>> {
        if !Q_RANGE.contains(&q) {
            return None;
        }
        let f = self.frame.as_ref()?;
        if let Some(v) = f.measured(q) {
            return Some(v.clone());
        }
        // q and its neighbours, those not measured yet
        let qs: Vec<u32> =
            [Some(q), q.checked_sub(1), q.checked_add(1)].into_iter().flatten().filter(|p| Q_RANGE.contains(p) && f.measured(*p).is_none()).collect();
        let got = self.step("measuring DNxHR bits", |me, f| me.measure(f, &qs))?;
        let f = self.frame.as_mut()?;
        let nmb = f.nmb();
        for (p, v) in qs.into_iter().zip(got) {
            if v.len() == nmb {
                f.measured.push((p, v));
            }
        }
        f.measured(q).cloned()
    }
}

impl MacroblockBackend for DnxGpuEncoder {
    fn load(&mut self, cfg: &EncoderConfig, frame: &Frame) -> bool {
        self.frame = None;
        if self.gpu.disabled() {
            return false;
        }
        let Some(plan) = Plan::new(cfg, frame, &self.gpu.device.limits()) else { return false };
        let scopes = self.gpu.scopes();
        let r = self.upload(plan, frame);
        self.frame = self.gpu.settle("loading a DNxHR frame", scopes, r);
        self.frame.is_some()
    }

    fn bits(&mut self, q: u32) -> Option<Vec<u32>> {
        let parts = self.parts_at(q)?;
        parts.iter().map(|[c, a]| c.checked_add(*a)).collect()
    }

    fn choose(&mut self, hint: u32, budget: u64) -> Option<Vec<u32>> {
        // (a frame too large for the rate control's buffer keeps its rate control on the CPU)
        self.rc_bytes(&self.frame.as_ref()?.params)?;
        let (qs, kept) = self.step("DNxHR rate control", |me, f| me.rate_control(f, hint, budget))?;
        // the rows the encoder asks for next, measured on the way
        let f = self.frame.as_mut()?;
        let nmb = f.nmb();
        for (q, v) in kept {
            if v.len() == nmb && f.measured(q).is_none() {
                f.measured.push((q, v));
            }
        }
        Some(qs)
    }

    fn write(&mut self, qs: &[u32]) -> Option<Vec<(Vec<u8>, u32)>> {
        if qs.len() != self.frame.as_ref()?.nmb() {
            return None;
        }
        let mut want = Vec::with_capacity(qs.len());
        for (i, &q) in qs.iter().enumerate() {
            if self.frame.as_ref()?.measured(q).is_none() {
                self.parts_at(q)?;
            }
            want.push(*self.frame.as_ref()?.measured(q)?.get(i)?);
        }
        self.step("writing DNxHR macroblocks", |me, f| me.code(f, qs, &want))
    }
}
