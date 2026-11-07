//! ProRes frame encoding (progressive) with per-slice quantiser rate control.

use crate::bits::{Counter, Sink, Writer};
use crate::dct::fdct8x8;
use crate::header::{FRAME_ID, FrameHeader, PictureHeader, row_slice_widths};
use crate::tables::{AC_ROUND, DC_CB, DC_CTX_INIT, FIRST_DC_CB, LEVEL_CB, LEVEL_CTX_INIT, RUN_CB, RUN_CTX_INIT, SCAN_PROGRESSIVE, qscale};
use crate::{AlphaType, ChromaFormat, ColorInfo, Error, Frame, Interlace, Profile, Result};

/// Encoder configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderConfig {
    pub profile: Profile,
    pub width: u32,
    pub height: u32,
    /// Coded-bits budget per macroblock; `None` uses [`Profile::nominal_bits_per_mb`].
    pub bits_per_mb: Option<u32>,
    /// log2 of the macroblocks per slice (0..=3; 3 = 8 macroblocks, the common choice).
    pub log2_slice_mbs: u8,
    pub color: ColorInfo,
    pub aspect_ratio: u8,
    pub frame_rate_code: u8,
    /// Encode the frame's alpha plane (4444 profiles only; 16-bit alpha coding).
    pub encode_alpha: bool,
    /// Encode slices in parallel (requires the `threads` feature).
    pub threads: bool,
    /// Four-character encoder identifier written into the frame header.
    pub encoder_id: [u8; 4],
}

impl EncoderConfig {
    pub fn new(profile: Profile, width: u32, height: u32) -> EncoderConfig {
        EncoderConfig {
            profile,
            width,
            height,
            bits_per_mb: None,
            log2_slice_mbs: 3,
            color: ColorInfo::default(),
            aspect_ratio: 0,
            frame_rate_code: 0,
            encode_alpha: true,
            threads: true,
            encoder_id: *b"fmcr",
        }
    }
}

/// ProRes encoder for one stream of same-sized progressive frames.
#[derive(Debug, Clone)]
pub struct Encoder {
    cfg: EncoderConfig,
    /// The previous frame's uniform quantiser: where the next frame's search starts.
    last_q: u8,
}

/// Quantiser search range (RDD 36 allows 1..=224).
const Q_MIN: u8 = 1;
const Q_MAX: u8 = 224;

impl Encoder {
    pub fn new(profile: Profile, width: u32, height: u32) -> Encoder {
        Encoder::with_config(EncoderConfig::new(profile, width, height))
    }

    pub fn with_config(cfg: EncoderConfig) -> Encoder {
        Encoder { cfg, last_q: Q_MIN }
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    pub fn bits_per_mb(&self) -> u32 {
        self.cfg.bits_per_mb.unwrap_or_else(|| self.cfg.profile.nominal_bits_per_mb())
    }

    /// The rate-control target for one frame in bytes (excluding alpha).
    pub fn target_frame_bytes(&self) -> usize {
        let mbs = (self.cfg.width as usize).div_ceil(16) * (self.cfg.height as usize).div_ceil(16);
        mbs * self.bits_per_mb() as usize / 8
    }

    /// Encode one frame. Input planes may be at any bit depth 8..=16 (10 is typical); the frame's
    /// chroma format must match the profile.
    pub fn encode(&mut self, frame: &Frame) -> Result<Vec<u8>> {
        self.encode_with(frame, None).map(|(f, _)| f)
    }

    /// `encode`, with the per-block work on `backend` when it takes the frame. Returns the frame
    /// and whether the backend coded it; any inconsistency is re-encoded on the CPU.
    pub fn encode_with(&mut self, frame: &Frame, backend: Option<&mut dyn SliceBackend>) -> Result<(Vec<u8>, bool)> {
        let with_alpha = self.validate(frame)?;
        let jobs = self.slice_jobs();
        let src = Source::new(&self.cfg, frame, with_alpha);
        if let Some(backend) = backend
            && let Some((out, qg)) = self.encode_on_backend(&src, backend, &jobs)
        {
            self.last_q = qg;
            return Ok((out, true));
        }
        let threads = self.cfg.threads;
        let prepared: Vec<SliceData> = par_map(threads, &jobs, |j| prepare_slice(&src, j.mb_x as usize, j.mb_y as usize, j.mbs as usize));
        let limits: Vec<u64> = prepared.iter().map(SliceData::limit_bits).collect();
        let budget = frame_budget(prepared.len(), slice_header_size(with_alpha), src.bits_per_mb, src.mb_count());
        let mut bits_at = |q: u8| Some(par_map(threads, &prepared, |sd| slice_bits(sd, q)));
        let (qs, qg) = choose_quantisers(budget, &limits, self.last_q, &mut bits_at).ok_or(Error::Invalid("rate control failed"))?;
        self.last_q = qg;
        let items: Vec<(&SliceData, u8)> = prepared.iter().zip(qs).collect();
        let slices: Option<Vec<Vec<u8>>> = par_map(threads, &items, |&(sd, q)| write_slice(sd, q)).into_iter().collect();
        let slices = slices.ok_or(Error::Invalid("slice larger than its 16-bit size field"))?;
        Ok((self.assemble_frame(&slices, with_alpha)?, false))
    }

    /// The slices of a frame of this encoder's size, in coded order.
    pub fn slice_jobs(&self) -> Vec<SliceJob> {
        let mb_width = (self.cfg.width as usize).div_ceil(16);
        let mb_height = (self.cfg.height as usize).div_ceil(16);
        let widths = row_slice_widths(mb_width, self.cfg.log2_slice_mbs.min(3));
        let mut jobs = Vec::with_capacity(widths.len() * mb_height);
        for row in 0..mb_height {
            let mut x = 0usize;
            for &sw in &widths {
                jobs.push(SliceJob { mb_x: x as u32, mb_y: row as u32, mbs: sw as u32 });
                x += sw;
            }
        }
        jobs
    }

    /// Check the frame against the configuration; whether alpha is coded.
    fn validate(&self, frame: &Frame) -> Result<bool> {
        let cfg = &self.cfg;
        if frame.width != cfg.width || frame.height != cfg.height {
            return Err(Error::Input(format!("frame is {}x{}, encoder is {}x{}", frame.width, frame.height, cfg.width, cfg.height)));
        }
        if cfg.width == 0 || cfg.height == 0 || cfg.width > 65535 || cfg.height > 65535 {
            return Err(Error::Input("unsupported frame size".into()));
        }
        let chroma = cfg.profile.chroma();
        if frame.chroma != chroma {
            return Err(Error::Input(format!("{:?} input for a {:?} profile", frame.chroma, chroma)));
        }
        if !(8..=16).contains(&frame.bit_depth) {
            return Err(Error::Input("bit depth must be 8..=16".into()));
        }
        let (w, h) = (cfg.width as usize, cfg.height as usize);
        let cw = chroma.chroma_width(cfg.width) as usize;
        if frame.y.len() < w * h || frame.cb.len() < cw * h || frame.cr.len() < cw * h {
            return Err(Error::Input("plane too small".into()));
        }
        let with_alpha = cfg.encode_alpha && chroma == ChromaFormat::Yuv444 && frame.alpha.is_some();
        if let Some(a) = &frame.alpha
            && with_alpha
            && a.len() < w * h
        {
            return Err(Error::Input("alpha plane too small".into()));
        }
        if cfg.log2_slice_mbs > 3 {
            return Err(Error::Input("log2_slice_mbs must be 0..=3".into()));
        }
        Ok(with_alpha)
    }

    /// The frame coded with the backend's per-block work, and its uniform quantiser. `None` when
    /// the backend declines, fails, or its answers do not agree with each other.
    fn encode_on_backend(&self, src: &Source, backend: &mut dyn SliceBackend, jobs: &[SliceJob]) -> Option<(Vec<u8>, u8)> {
        let n = jobs.len();
        if n == 0 || !backend.load(&self.cfg, src.frame, jobs) {
            return None;
        }
        let alpha: Vec<Option<Vec<u8>>> = if src.alpha {
            match backend.alpha() {
                Some(a) if a.len() == n => a.into_iter().map(Some).collect(),
                _ => jobs.iter().map(|j| Some(encode_alpha_slice(src, j.mb_x as usize * 16, j.mb_y as usize * 16, j.mbs as usize))).collect(),
            }
        } else {
            vec![None; n]
        };
        let limits: Vec<u64> = alpha.iter().map(|a| slice_limit_bits(src.alpha, a.as_ref().map_or(0, Vec::len))).collect();
        let budget = frame_budget(n, slice_header_size(src.alpha), src.bits_per_mb, src.mb_count());
        let mut measured: Vec<(u8, Vec<[u32; 3]>)> = Vec::new();
        let (qs, qg) = backend_quantisers(backend, budget, &limits, self.last_q, &mut measured)?;
        if qs.len() != n {
            return None;
        }
        // The bits every component was measured at, at the quantiser it will be written at.
        let mut want: Vec<[u32; 3]> = Vec::with_capacity(n);
        for (i, &q) in qs.iter().enumerate() {
            if !measured.iter().any(|(mq, _)| *mq == q) {
                let v = backend.bits(q)?;
                if v.len() != n {
                    return None;
                }
                measured.push((q, v));
            }
            let (_, v) = measured.iter().find(|(mq, _)| *mq == q)?;
            want.push(*v.get(i)?);
        }
        let written = backend.write(&qs)?;
        if written.len() != n {
            return None;
        }
        let hsize = slice_header_size(src.alpha);
        let mut slices = Vec::with_capacity(n);
        for (((comps, &q), bits), a) in written.iter().zip(&qs).zip(&want).zip(&alpha) {
            if comps.iter().zip(bits).any(|(c, &b)| c.len() as u64 * 8 != b as u64) {
                return None;
            }
            slices.push(assemble_slice(hsize, q, [&comps[0], &comps[1], &comps[2]], a.as_deref())?);
        }
        let out = self.assemble_frame(&slices, src.alpha).ok()?;
        Some((out, qg))
    }

    /// The frame header, picture header, slice table and slices.
    fn assemble_frame(&self, slices: &[Vec<u8>], with_alpha: bool) -> Result<Vec<u8>> {
        let cfg = &self.cfg;
        let chroma = cfg.profile.chroma();
        if slices.iter().any(|s| s.len() > 65535) {
            return Err(Error::Invalid("slice larger than its 16-bit size field"));
        }
        let hdr = FrameHeader {
            header_size: 0,
            bitstream_version: if chroma == ChromaFormat::Yuv444 { 1 } else { 0 },
            encoder_id: cfg.encoder_id,
            width: cfg.width as u16,
            height: cfg.height as u16,
            chroma,
            interlace: Interlace::Progressive,
            aspect_ratio: cfg.aspect_ratio & 15,
            frame_rate_code: cfg.frame_rate_code & 15,
            color: cfg.color,
            alpha: if with_alpha { AlphaType::Bits16 } else { AlphaType::None },
            qmat_luma: [4; 64],
            qmat_chroma: [4; 64],
            luma_matrix_present: false,
            chroma_matrix_present: false,
        };
        let fh = hdr.write();
        let slice_bytes: usize = slices.iter().map(Vec::len).sum();
        let pic_size = 8 + 2 * slices.len() + slice_bytes;
        let total = 8 + fh.len() + pic_size;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(&FRAME_ID);
        out.extend_from_slice(&fh);
        PictureHeader { header_size: 8, picture_size: pic_size, num_slices: slices.len(), log2_slice_mbs: cfg.log2_slice_mbs }.write(&mut out);
        for s in slices {
            out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        }
        for s in slices {
            out.extend_from_slice(s);
        }
        debug_assert_eq!(out.len(), total);
        Ok(out)
    }
}

/// Where one slice lies, in coded order (row-major, `row_slice_widths` per row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SliceJob {
    pub mb_x: u32,
    pub mb_y: u32,
    pub mbs: u32,
}

/// The per-block work of an encode on other hardware: forward DCT, quantisation, entropy coding
/// of the Y, Cb and Cr components of every slice, exactly as this crate's encoder does them.
pub trait SliceBackend: Send {
    /// Take `frame` (already validated by the encoder) for `slices`. `false`: this backend cannot
    /// encode it (size, profile, a device error); the encoder then uses its own code.
    fn load(&mut self, cfg: &EncoderConfig, frame: &Frame, slices: &[SliceJob]) -> bool;
    /// The coded bits of every slice's Y, Cb, Cr at quantiser index `q`, each padded to whole
    /// bytes (as `component_bits`), in `slices` order.
    fn bits(&mut self, q: u8) -> Option<Vec<[u32; 3]>>;
    /// Every slice's Y, Cb, Cr payload bytes at its quantiser index `qs[i]`.
    fn write(&mut self, qs: &[u8]) -> Option<Vec<[Vec<u8>; 3]>>;
    /// Every slice's coded alpha (4444 with alpha; `encode_alpha_slice`'s bytes). `None`: the
    /// encoder codes alpha itself.
    fn alpha(&mut self) -> Option<Vec<Vec<u8>>> {
        None
    }
    /// The whole rate control (`choose_quantisers` with this frame's `hint`, `budget` bits and
    /// per-slice `limits` bits), run where the bits are. `None`: the encoder runs it over `bits`.
    fn choose(&mut self, hint: u8, budget: u64, limits: &[u64]) -> Option<(Vec<u8>, u8)> {
        let _ = (hint, budget, limits);
        None
    }
}

/// The quantisers of a frame from the backend: its own rate control when it has one and its
/// answer is in range, else the encoder's over the bits the backend measures (kept in `measured`).
fn backend_quantisers(backend: &mut dyn SliceBackend, budget: u64, limits: &[u64], hint: u8, measured: &mut Vec<(u8, Vec<[u32; 3]>)>) -> Option<(Vec<u8>, u8)> {
    let n = limits.len();
    if let Some((qs, qg)) = backend.choose(hint, budget, limits)
        && qs.len() == n
        && (Q_MIN..=Q_MAX).contains(&qg)
        && qs.iter().all(|q| (Q_MIN..=Q_MAX).contains(q))
    {
        return Some((qs, qg));
    }
    let mut bits_at = |q: u8| {
        let v = backend.bits(q)?;
        if v.len() != n {
            return None;
        }
        let sums = v.iter().map(|c| c.iter().map(|&b| b as u64).sum()).collect();
        measured.push((q, v));
        Some(sums)
    };
    choose_quantisers(budget, limits, hint, &mut bits_at)
}

/// Bytes of a slice header: with the alpha size field or without.
fn slice_header_size(alpha: bool) -> usize {
    if alpha { 8 } else { 6 }
}

/// Largest DCT payload (bits) that keeps every 16-bit size field valid.
fn slice_limit_bits(alpha: bool, alpha_len: usize) -> u64 {
    65535usize.saturating_sub(slice_header_size(alpha) + alpha_len) as u64 * 8
}

/// The DCT payload budget of a frame in bits (after the fixed headers).
fn frame_budget(slices: usize, hsize: usize, bits_per_mb: u32, total_mbs: usize) -> u64 {
    let fixed = 8 * (8 + 20 + 8 + slices * (2 + hsize)) as u64;
    (bits_per_mb as u64 * total_mbs as u64).saturating_sub(fixed)
}

/// A slice from its three payloads: header, payloads, alpha. `None` when a size does not fit its
/// 16-bit field.
fn assemble_slice(hsize: usize, q: u8, comps: [&[u8]; 3], alpha: Option<&[u8]>) -> Option<Vec<u8>> {
    let alpha_bytes = alpha.unwrap_or(&[]);
    let total = hsize + comps.iter().map(|c| c.len()).sum::<usize>() + alpha_bytes.len();
    if total > 65535 || comps.iter().any(|c| c.len() > 65535) {
        return None;
    }
    let mut out = Vec::with_capacity(total);
    out.push((hsize as u8) << 3);
    out.push(q);
    out.extend_from_slice(&(comps[0].len() as u16).to_be_bytes());
    out.extend_from_slice(&(comps[1].len() as u16).to_be_bytes());
    if alpha.is_some() {
        out.extend_from_slice(&(comps[2].len() as u16).to_be_bytes());
    }
    for c in comps {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(alpha_bytes);
    Some(out)
}

struct Source<'a> {
    frame: &'a Frame,
    w: usize,
    h: usize,
    cw: usize,
    chroma444: bool,
    /// Multiplier from input sample values to the 12-bit domain.
    scale: f32,
    alpha: bool,
    bits_per_mb: u32,
}

impl<'a> Source<'a> {
    fn new(cfg: &EncoderConfig, frame: &'a Frame, alpha: bool) -> Source<'a> {
        let chroma = cfg.profile.chroma();
        Source {
            frame,
            w: cfg.width as usize,
            h: cfg.height as usize,
            cw: chroma.chroma_width(cfg.width) as usize,
            chroma444: chroma == ChromaFormat::Yuv444,
            scale: (2f32).powi(12 - frame.bit_depth as i32),
            alpha,
            bits_per_mb: cfg.bits_per_mb.unwrap_or_else(|| cfg.profile.nominal_bits_per_mb()),
        }
    }

    fn mb_count(&self) -> usize {
        self.w.div_ceil(16) * self.h.div_ceil(16)
    }
}

/// Load an 8×8 block (edge-replicated) into the 12-bit signed domain and transform it.
fn load_block(plane: &[u16], stride: usize, height: usize, x0: usize, y0: usize, scale: f32) -> [f32; 64] {
    let mut b = [0f32; 64];
    for yy in 0..8 {
        let y = (y0 + yy).min(height - 1);
        let row = &plane[y * stride..(y + 1) * stride];
        for xx in 0..8 {
            let x = (x0 + xx).min(stride - 1);
            b[yy * 8 + xx] = row[x] as f32 * scale - 2048.0;
        }
    }
    fdct8x8(&mut b);
    b
}

fn quantise(coeffs: &[[f32; 64]], qmat_inv: &[f32; 64], out: &mut Vec<[i32; 64]>) {
    out.clear();
    for c in coeffs {
        let mut l = [0i32; 64];
        let dc = c[0] * qmat_inv[0];
        l[0] = dc.round() as i32;
        for i in 1..64 {
            let v = c[i] * qmat_inv[i];
            let a = (v.abs() + AC_ROUND) as i32;
            l[i] = if v < 0.0 { -a } else { a };
        }
        out.push(l);
    }
}

fn encode_component<S: Sink>(s: &mut S, levels: &[[i32; 64]], scan: &[u8; 64]) {
    let n = levels.len();
    // DC
    let first = levels[0][0];
    let zz = if first >= 0 { (first as u32) << 1 } else { ((-(first as i64)) as u32) * 2 - 1 };
    s.put_cw(FIRST_DC_CB, zz);
    let mut prev = first;
    let mut ctx = DC_CTX_INIT;
    let mut sign = false;
    for blk in &levels[1..] {
        let dc = blk[0];
        let delta = dc - prev;
        prev = dc;
        let code = if delta == 0 {
            sign = false;
            0
        } else {
            let neg = delta < 0;
            let mag = delta.unsigned_abs();
            let c = if neg != sign { 2 * mag - 1 } else { 2 * mag };
            sign = neg;
            c
        };
        s.put_cw(DC_CB[ctx.min(6) as usize], code);
        ctx = code;
    }
    // AC, interleaved across blocks
    let mut run = 0u32;
    let mut run_ctx = RUN_CTX_INIT;
    let mut lev_ctx = LEVEL_CTX_INIT;
    for &nat in &scan[1..] {
        let nat = nat as usize;
        for blk in levels.iter().take(n) {
            let l = blk[nat];
            if l == 0 {
                run += 1;
            } else {
                s.put_cw(RUN_CB[run_ctx.min(15) as usize], run);
                run_ctx = run;
                let a = l.unsigned_abs();
                s.put_cw(LEVEL_CB[lev_ctx.min(9) as usize], a - 1);
                lev_ctx = a;
                s.put((l < 0) as u32, 1);
                run = 0;
            }
        }
    }
}

fn component_bits(levels: &[[i32; 64]]) -> u64 {
    let mut c = Counter::default();
    encode_component(&mut c, levels, &SCAN_PROGRESSIVE);
    c.0.div_ceil(8) * 8
}

/// A slice's transformed blocks (Y, Cb, Cr) and its coded alpha.
struct SliceData {
    comps: [Vec<[f32; 64]>; 3],
    alpha: Option<Vec<u8>>,
}

impl SliceData {
    fn header_size(&self) -> usize {
        if self.alpha.is_some() { 8 } else { 6 }
    }
    /// Largest DCT payload (bits) that keeps every 16-bit size field valid.
    fn limit_bits(&self) -> u64 {
        (65535 - self.header_size() - self.alpha.as_ref().map_or(0, Vec::len)) as u64 * 8
    }
}

fn par_map<T: Sync, R: Send>(threads: bool, items: &[T], f: impl Fn(&T) -> R + Sync + Send) -> Vec<R> {
    #[cfg(feature = "threads")]
    if threads {
        use rayon::prelude::*;
        return items.par_iter().map(f).collect();
    }
    let _ = threads;
    items.iter().map(f).collect()
}

fn prepare_slice(src: &Source, mb_x: usize, mb_y: usize, mbs: usize) -> SliceData {
    let f = src.frame;
    let x0 = mb_x * 16;
    let y0 = mb_y * 16;
    let mut luma = Vec::with_capacity(4 * mbs);
    for mb in 0..mbs {
        for i in 0..4 {
            luma.push(load_block(&f.y, src.w, src.h, x0 + mb * 16 + (i & 1) * 8, y0 + (i >> 1) * 8, src.scale));
        }
    }
    let mut chroma = [Vec::with_capacity(4 * mbs), Vec::with_capacity(4 * mbs)];
    for (k, plane) in [&f.cb, &f.cr].into_iter().enumerate() {
        for mb in 0..mbs {
            if src.chroma444 {
                // 4:4:4 chroma block order within a macroblock: TL, BL, TR, BR.
                for i in 0..4 {
                    chroma[k].push(load_block(plane, src.cw, src.h, x0 + mb * 16 + (i >> 1) * 8, y0 + (i & 1) * 8, src.scale));
                }
            } else {
                for i in 0..2 {
                    chroma[k].push(load_block(plane, src.cw, src.h, mb_x * 8 + mb * 8, y0 + i * 8, src.scale));
                }
            }
        }
    }
    let [cb, cr] = chroma;
    let alpha = src.alpha.then(|| encode_alpha_slice(src, x0, y0, mbs));
    SliceData { comps: [luma, cb, cr], alpha }
}

fn quantise_all(sd: &SliceData, q: u8, lv: &mut [Vec<[i32; 64]>; 3]) {
    let inv = [1.0 / (4.0 * qscale(q) as f32); 64];
    for (c, l) in sd.comps.iter().zip(lv.iter_mut()) {
        quantise(c, &inv, l);
    }
}

/// Coded bits of a slice's Y, Cb and Cr at quantiser `q` (each padded to bytes).
fn slice_comp_bits(sd: &SliceData, q: u8) -> [u64; 3] {
    let mut lv = [Vec::new(), Vec::new(), Vec::new()];
    quantise_all(sd, q, &mut lv);
    lv.map(|l| component_bits(&l))
}

/// Coded DCT payload bits of a slice at quantiser `q` (components padded to bytes).
fn slice_bits(sd: &SliceData, q: u8) -> u64 {
    slice_comp_bits(sd, q).iter().sum()
}

/// Per-slice coded bits at the quantisers tried so far, so no quantiser is measured twice.
struct Probes<'a> {
    bits_at: &'a mut dyn FnMut(u8) -> Option<Vec<u64>>,
    /// Slices of the frame: every measurement must have this many entries.
    slices: usize,
    tried: Vec<(u8, Vec<u64>, u64)>,
}

impl Probes<'_> {
    /// The per-slice bits at `q`, measuring them if `q` was not tried yet. `None`: the
    /// measurement failed or has the wrong number of slices.
    fn at(&mut self, q: u8) -> Option<Vec<u64>> {
        if let Some((_, v, _)) = self.tried.iter().find(|(tq, _, _)| *tq == q) {
            return Some(v.clone());
        }
        let v = (self.bits_at)(q)?;
        if v.len() != self.slices {
            return None;
        }
        let t = v.iter().fold(0u64, |a, &b| a.saturating_add(b));
        self.tried.push((q, v.clone(), t));
        Some(v)
    }

    /// The frame's coded bits at `q` (the sum over slices).
    fn total(&mut self, q: u8) -> Option<u64> {
        self.at(q)?;
        self.tried.iter().find(|(tq, _, _)| *tq == q).map(|(_, _, t)| *t)
    }
}

/// The smallest quantiser in `Q_MIN..=Q_MAX` whose frame fits `budget` (`Q_MAX` when none does),
/// assuming bits fall as the quantiser grows. The search starts at `hint` (the previous frame's
/// quantiser) and widens its steps away from it until the answer is bracketed, then bisects: the
/// frames of a shot need the same quantiser or a neighbour, which takes two measurements instead of
/// the eight of a bisection over the whole range.
fn smallest_fitting(total: &mut impl FnMut(u8) -> u64, hint: u8, budget: u64) -> u8 {
    let h = hint.clamp(Q_MIN, Q_MAX);
    // `lo` does not fit, `hi` does: the answer is in `lo + 1..=hi`.
    let (mut lo, mut hi) = if total(h) <= budget {
        let (mut hi, mut step) = (h, 1u8);
        loop {
            if hi == Q_MIN {
                return Q_MIN;
            }
            let q = hi.saturating_sub(step).max(Q_MIN);
            if total(q) > budget {
                break (q, hi);
            }
            hi = q;
            step = step.saturating_mul(2);
        }
    } else {
        let (mut lo, mut step) = (h, 1u8);
        loop {
            if lo == Q_MAX {
                return Q_MAX;
            }
            let q = lo.saturating_add(step).min(Q_MAX);
            if total(q) <= budget {
                break (lo, q);
            }
            lo = q;
            step = step.saturating_mul(2);
        }
    };
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if total(mid) <= budget {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// Frame-level rate control: the smallest uniform quantiser whose frame fits the budget, then
/// one step finer on as many slices as the remaining budget allows. Uniform quantisation
/// approximately minimises frame MSE for a given size. Returns the slices' quantisers and the
/// uniform one (the next frame's `hint`). `limits` are the slices' largest payloads in bits;
/// `bits_at(q)` measures every slice at `q`. `None`: a measurement failed.
fn choose_quantisers(budget: u64, limits: &[u64], hint: u8, bits_at: &mut dyn FnMut(u8) -> Option<Vec<u64>>) -> Option<(Vec<u8>, u8)> {
    let n = limits.len();
    let mut probes = Probes { bits_at, slices: n, tried: Vec::new() };
    let mut failed = false;
    let qg = smallest_fitting(
        &mut |q| {
            probes.total(q).unwrap_or_else(|| {
                failed = true;
                u64::MAX
            })
        },
        hint,
        budget,
    );
    if failed {
        return None;
    }
    let mut qs = vec![qg; n];
    let mut cur = probes.at(qg)?;
    let total = cur.iter().sum::<u64>();
    if qg > Q_MIN && total < budget {
        // usually measured already: the search's last quantiser that did not fit
        let finer = probes.at(qg - 1)?;
        let mut left = budget - total;
        for (((&limit, f), c), q) in limits.iter().zip(&finer).zip(cur.iter_mut()).zip(qs.iter_mut()) {
            let d = f.saturating_sub(*c);
            if d <= left && *f <= limit {
                left -= d;
                *q = qg - 1;
                *c = *f;
            }
        }
    }
    // Keep every slice within its 16-bit size fields: raise the quantiser of those that do not
    // fit, one step per round, over the cached whole-frame measurements.
    loop {
        let mut raised = false;
        for ((&limit, q), c) in limits.iter().zip(qs.iter_mut()).zip(cur.iter()) {
            if *c > limit && *q < Q_MAX {
                *q += 1;
                raised = true;
            }
        }
        if !raised {
            break;
        }
        let mut by_q: Vec<(u8, Vec<u64>)> = Vec::new();
        for &q in &qs {
            if by_q.iter().all(|(bq, _)| *bq != q) {
                by_q.push((q, probes.at(q)?));
            }
        }
        for (i, (&q, c)) in qs.iter().zip(cur.iter_mut()).enumerate() {
            if let Some(b) = by_q.iter().find(|(bq, _)| *bq == q).and_then(|(_, v)| v.get(i)) {
                *c = *b;
            }
        }
    }
    Some((qs, qg))
}

/// The Y, Cb and Cr payloads of a slice at quantiser `q`.
fn write_components(sd: &SliceData, q: u8) -> [Vec<u8>; 3] {
    let mut lv = [Vec::new(), Vec::new(), Vec::new()];
    quantise_all(sd, q, &mut lv);
    lv.map(|l| {
        let mut w = Writer::new();
        encode_component(&mut w, &l, &SCAN_PROGRESSIVE);
        w.finish()
    })
}

/// A whole slice; `None` when it does not fit its 16-bit size fields.
fn write_slice(sd: &SliceData, q: u8) -> Option<Vec<u8>> {
    let [y, cb, cr] = write_components(sd, q);
    assemble_slice(sd.header_size(), q, [&y, &cb, &cr], sd.alpha.as_deref())
}

/// Encode the slice's alpha samples (16-bit coding) in raster order over its 16 lines.
fn encode_alpha_slice(src: &Source, x0: usize, y0: usize, mbs: usize) -> Vec<u8> {
    let Some(a) = src.frame.alpha.as_ref() else { return Vec::new() };
    let d = src.frame.bit_depth as u32;
    let to16 = |v: u16| -> u32 {
        let v = v as u32 & ((1 << d) - 1);
        if d >= 16 { v } else { ((v << (16 - d)) | (v >> (2 * d).saturating_sub(16).min(d))) & 0xffff }
    };
    let sw = mbs * 16;
    let mut vals = Vec::with_capacity(sw * 16);
    for yy in 0..16 {
        let y = (y0 + yy).min(src.h - 1);
        for xx in 0..sw {
            let x = (x0 + xx).min(src.w - 1);
            vals.push(to16(a[y * src.w + x]));
        }
    }
    let mut w = Writer::new();
    encode_alpha(&mut w, &vals, 16);
    w.finish()
}

/// Alpha plane coding: a value (difference from the previous value, short or full width), then
/// either a one bit (another value follows) or a zero bit and a run of repeats of the value.
pub(crate) fn encode_alpha(w: &mut Writer, vals: &[u32], bits: u32) {
    let mask = (1u32 << bits) - 1;
    let (short_bits, short_max) = if bits == 16 { (7, 64i32) } else { (4, 8i32) };
    let n = vals.len();
    let mut prev = mask;
    let mut idx = 0;
    while idx < n {
        let v = vals[idx];
        let diff = v.wrapping_sub(prev) & mask;
        // signed difference in (-2^(bits-1), 2^(bits-1)]
        let sd = if diff > mask >> 1 { diff as i32 - (mask as i32 + 1) } else { diff as i32 };
        if sd != 0 && sd.abs() <= short_max {
            w.put(0, 1);
            let c = (2 * sd.unsigned_abs() - 2) | (sd < 0) as u32;
            w.put(c, short_bits);
        } else {
            w.put(1, 1);
            w.put(diff, bits);
        }
        prev = v;
        idx += 1;
        // RDD 36:2022 §5.3.3 requires a run after every alpha difference,
        // including a singleton at the end of the slice. Do not replace that
        // final `1` with byte-alignment zeros: native decoders reject it.
        let mut run = 0;
        while idx + run < n && vals[idx + run] == prev && run < 2047 {
            run += 1;
        }
        if run == 0 {
            w.put(1, 1);
            continue;
        }
        w.put(0, 1);
        if run < 16 {
            w.put(run as u32, 4);
        } else {
            w.put(0, 4);
            w.put(run as u32, 11);
        }
        idx += run;
    }
}

#[cfg(test)]
mod rate_tests {
    use super::*;

    /// `at` gives a quantiser's measured per-slice bits whether or not `total` tried it: an
    /// empty list would skip the slices' 16-bit size checks in `choose_quantisers`.
    #[test]
    fn at_measures_an_untried_quantiser() {
        let block = |v: f32| std::array::from_fn::<f32, 64, _>(|i| v * (64 - i) as f32);
        let slices: Vec<SliceData> =
            (1..=3).map(|k| SliceData { comps: [vec![block(40.0 * k as f32); 4], vec![block(10.0); 2], vec![block(5.0); 2]], alpha: None }).collect();
        let want = |q| slices.iter().map(|sd| slice_bits(sd, q)).collect::<Vec<_>>();
        assert!(want(4).iter().all(|&b| b > 0));
        let mut bits_at = |q| Some(slices.iter().map(|sd| slice_bits(sd, q)).collect::<Vec<_>>());
        let mut probes = Probes { bits_at: &mut bits_at, slices: slices.len(), tried: Vec::new() };
        assert_eq!(probes.at(4), Some(want(4)), "untried");
        probes.total(6);
        assert_eq!(probes.at(6), Some(want(6)), "tried");
        assert_eq!(probes.tried.len(), 2, "each quantiser is measured once");
    }

    /// Frame bits falling with the quantiser (steeply at first, then flat), as real content does.
    fn bits(q: u8) -> u64 {
        4_000_000 / (q as u64 + 3) + 1_000
    }

    #[test]
    fn hinted_search_finds_the_smallest_fitting_quantiser() {
        for budget in (0..1_200_000).step_by(997).chain([0, 1_000, 2_000, bits(Q_MIN), bits(Q_MAX), u64::MAX]) {
            // the reference: a scan from the finest quantiser
            let want = (Q_MIN..=Q_MAX).find(|&q| bits(q) <= budget).unwrap_or(Q_MAX);
            for hint in [0, Q_MIN, 2, 7, 64, 128, 223, Q_MAX, 255] {
                let mut calls = 0;
                let got = smallest_fitting(
                    &mut |q| {
                        assert!((Q_MIN..=Q_MAX).contains(&q), "measured an invalid quantiser {q}");
                        calls += 1;
                        bits(q)
                    },
                    hint,
                    budget,
                );
                assert_eq!(got, want, "budget {budget}, hint {hint}");
                assert!(calls <= 16, "budget {budget}, hint {hint}: {calls} measurements");
            }
        }
    }

    /// The common case (the previous frame's quantiser, or a neighbour) takes two measurements.
    #[test]
    fn a_good_hint_takes_two_measurements() {
        for want in [Q_MIN + 1, 9, 40, 150, Q_MAX] {
            let budget = bits(want);
            for hint in [want - 1, want] {
                let mut calls = 0;
                let got = smallest_fitting(
                    &mut |q| {
                        calls += 1;
                        bits(q)
                    },
                    hint,
                    budget,
                );
                assert_eq!((got, calls), (want, 2), "hint {hint}");
            }
        }
    }

    /// Two frames, the second encoded after very different content: the encoder's memory of the
    /// previous quantiser changes the work, not the stream.
    #[test]
    fn previous_frames_do_not_change_the_stream() {
        let (w, h) = (256, 64);
        let frame = |seed: u32, detail: u32| {
            let mut f = Frame::new(w, h, ChromaFormat::Yuv422, 10, false);
            let mut s = seed;
            let cw = f.chroma_width() as usize;
            for y in 0..h as usize {
                for x in 0..w as usize {
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    f.y[y * w as usize + x] = (400 + (x * 3 + y) as u32 % 200 + s % detail) as u16;
                }
                for x in 0..cw {
                    f.cb[y * cw + x] = (500 + (x + y) as u32 % 40) as u16;
                    f.cr[y * cw + x] = (520 + (x * 5) as u32 % 30) as u16;
                }
            }
            f
        };
        let (busy, plain) = (frame(7, 400), frame(9, 2));
        for profile in [Profile::Proxy, Profile::Standard, Profile::Hq] {
            let alone = |f: &Frame| Encoder::new(profile, w, h).encode(f).unwrap();
            let mut enc = Encoder::new(profile, w, h);
            enc.encode(&plain).unwrap();
            assert!(enc.encode(&busy).unwrap() == alone(&busy), "{profile:?}: busy after plain");
            assert!(enc.encode(&plain).unwrap() == alone(&plain), "{profile:?}: plain after busy");
            assert!(enc.encode(&plain).unwrap() == alone(&plain), "{profile:?}: plain after plain");
        }
    }
}

#[cfg(test)]
mod alpha_tests {
    use super::*;

    #[test]
    fn alpha_final_singleton_has_a_run_code() {
        // RDD 36:2022 §5.3.3: every difference is followed by a run, including
        // the final one. Tables 13/14 encode -1; Table 12 encodes run 1 as `1`.
        // With 16-bit alpha the difference occupies exactly one byte, so the
        // required final run must start another byte, followed by zero padding.
        for (bits, expected) in [(16, vec![0x01, 0x80]), (8, vec![0x0c])] {
            let mut w = Writer::new();
            encode_alpha(&mut w, &[(1 << bits) - 2], bits);
            assert_eq!(w.finish(), expected, "{bits}-bit final singleton");
        }
    }
}

#[cfg(test)]
mod alpha_conformance_tests {
    use super::*;
    use filmcraft_bitstream::BitReader;

    /// RDD 36:2022 §5.3.3 and Tables 12–14, using a bounds-checked reader.
    /// Unlike the production decoder's zero-padded hot path, this checks every
    /// final run and all alignment bits, so a truncated run cannot be accepted.
    fn read_alpha(data: &[u8], bits: u32, count: usize) -> Vec<u32> {
        let mut r = BitReader::new(data);
        let mask = (1u32 << bits) - 1;
        let mut prev = mask;
        let mut out = Vec::new();
        while out.len() < count {
            let diff = if r.read_bits(1).unwrap() == 1 {
                r.read_bits(bits).unwrap()
            } else {
                let code = r.read_bits(if bits == 16 { 7 } else { 4 }).unwrap();
                let magnitude = (code >> 1) + 1;
                if code & 1 == 0 { magnitude } else { magnitude.wrapping_neg() }
            };
            prev = prev.wrapping_add(diff) & mask;
            let run = if r.read_bits(1).unwrap() == 1 {
                1
            } else {
                let short = r.read_bits(4).unwrap();
                if short == 0 { r.read_bits(11).unwrap() + 1 } else { short + 1 }
            } as usize;
            assert!(run <= count - out.len(), "alpha run exceeds the slice");
            out.extend(std::iter::repeat_n(prev, run));
        }
        assert!(r.bits_left() < 8, "only byte-alignment padding may remain");
        while r.bits_left() > 0 {
            assert_eq!(r.read_bits(1).unwrap(), 0, "alignment padding must be zero");
        }
        out
    }

    #[test]
    fn alpha_runs_cover_boundaries_and_final_values() {
        for bits in [8, 16] {
            let mask = (1 << bits) - 1;
            for run in [1, 2, 15, 16, 17, 2047, 2048, 2049, 4096] {
                for tail in 0..=8 {
                    let mut values = vec![mask - 1; run];
                    // Short negative/positive deltas, modulo transitions and
                    // escaped full-width deltas, ending on every bit alignment.
                    values.extend([0, mask, 1, 7, 8, 64, 65, mask / 2].into_iter().take(tail));
                    let mut w = Writer::new();
                    encode_alpha(&mut w, &values, bits);
                    assert_eq!(read_alpha(&w.finish(), bits, values.len()), values, "bits={bits}, run={run}, tail={tail}");
                }
            }
        }
    }

    #[test]
    fn encoded_alpha_slices_have_complete_final_runs() {
        for profile in [Profile::P4444, Profile::P4444Xq] {
            for log2_slice_mbs in 0..=3 {
                let (width, height) = (128, 32);
                let mut frame = Frame::new(width, height, ChromaFormat::Yuv444, 16, true);
                frame.y.fill(32768);
                frame.cb.fill(32768);
                frame.cr.fill(32768);
                let alpha = frame.alpha.as_mut().unwrap();
                for (i, a) in alpha.iter_mut().enumerate() {
                    *a = (i as u16).wrapping_mul(257);
                }
                let mut cfg = EncoderConfig::new(profile, width, height);
                cfg.log2_slice_mbs = log2_slice_mbs;
                let bytes = Encoder::with_config(cfg).encode(&frame).unwrap();
                let (_, start, _) = FrameHeader::parse(&bytes).unwrap();
                let picture = &bytes[start..];
                let header = PictureHeader::parse(picture).unwrap();
                let table = &picture[header.header_size..][..2 * header.num_slices];
                let mut offset = header.header_size + table.len();
                let sw = 16 << log2_slice_mbs;
                for (i, size) in table.as_chunks::<2>().0.iter().enumerate() {
                    let size = u16::from_be_bytes([size[0], size[1]]) as usize;
                    let slice = &picture[offset..offset + size];
                    offset += size;
                    let color_bytes: usize = slice[2..8].as_chunks::<2>().0.iter().map(|b| u16::from_be_bytes([b[0], b[1]]) as usize).sum();
                    let payload = &slice[(slice[0] >> 3) as usize + color_bytes..];
                    let actual = read_alpha(payload, 16, sw * 16);
                    let x0 = i % (width as usize / sw) * sw;
                    let y0 = i / (width as usize / sw) * 16;
                    let alpha = frame.alpha.as_ref().unwrap();
                    let expected: Vec<u32> = (y0..y0 + 16).flat_map(|y| (x0..x0 + sw).map(move |x| alpha[y * width as usize + x] as u32)).collect();
                    assert_eq!(actual, expected, "{profile:?}, slice {i}, log2={log2_slice_mbs}");
                }
                assert_eq!(offset, picture.len());
            }
        }
    }
}

#[cfg(test)]
mod backend_tests {
    use super::*;
    use crate::layout::frame_layout;
    use crate::{SliceBackend, SliceJob, decode_frame};

    /// Deterministic busy planes: gradient + checkerboard + xorshift noise; alpha ramp for 4444.
    fn test_frame(profile: Profile, w: u32, h: u32, seed: u32) -> Frame {
        let alpha = profile.chroma() == ChromaFormat::Yuv444;
        let mut f = Frame::new(w, h, profile.chroma(), 10, alpha);
        let mut s = seed.max(1);
        let mut noise = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        let cw = f.chroma_width() as usize;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let check = if (x / 4 + y / 4) % 2 == 0 { 60 } else { 0 };
                f.y[y * w as usize + x] = (64 + (x * 3 + y * 2) % 600 + check + noise() as usize % 40) as u16;
                if let Some(a) = f.alpha.as_mut() {
                    a[y * w as usize + x] = ((x * 1023) / w.max(1) as usize) as u16;
                }
            }
            for x in 0..cw {
                f.cb[y * cw + x] = (400 + (x + y) % 200 + noise() as usize % 30) as u16;
                f.cr[y * cw + x] = (500 + (x * 5 + y) % 100 + noise() as usize % 30) as u16;
            }
        }
        f
    }

    /// The crate's own CPU functions behind the backend trait.
    #[derive(Default)]
    struct CpuBackend {
        slices: Vec<SliceData>,
        /// Lie about a component: (slice, component, bytes to add to `write`'s answer).
        extra_byte: Option<(usize, usize)>,
        garbage: bool,
        fail_bits: bool,
        fail_write: bool,
        decline: bool,
        choose: ChooseMode,
        alpha_mode: AlphaMode,
        choose_calls: usize,
    }

    #[derive(Default)]
    enum ChooseMode {
        #[default]
        Default,
        /// The CPU algorithm's own answer over this backend's bits.
        Cpu,
        /// A fixed answer, whatever the frame.
        Fixed(Vec<u8>, u8),
        /// The CPU algorithm's quantisers with one entry too many.
        TooLong,
    }

    #[derive(Default)]
    enum AlphaMode {
        #[default]
        Slices,
        Nothing,
        /// One entry too few.
        WrongCount,
    }

    impl SliceBackend for CpuBackend {
        fn load(&mut self, cfg: &EncoderConfig, frame: &Frame, jobs: &[SliceJob]) -> bool {
            if self.decline {
                return false;
            }
            let alpha = cfg.encode_alpha && cfg.profile.chroma() == ChromaFormat::Yuv444 && frame.alpha.is_some();
            let src = Source::new(cfg, frame, alpha);
            self.slices = jobs.iter().map(|j| prepare_slice(&src, j.mb_x as usize, j.mb_y as usize, j.mbs as usize)).collect();
            true
        }
        fn bits(&mut self, q: u8) -> Option<Vec<[u32; 3]>> {
            if self.fail_bits {
                return None;
            }
            Some(self.slices.iter().map(|sd| slice_comp_bits(sd, q).map(|b| b as u32)).collect())
        }
        fn write(&mut self, qs: &[u8]) -> Option<Vec<[Vec<u8>; 3]>> {
            if self.fail_write {
                return None;
            }
            let mut out: Vec<[Vec<u8>; 3]> = self.slices.iter().zip(qs).map(|(sd, &q)| write_components(sd, q)).collect();
            if let Some((s, c)) = self.extra_byte
                && let Some(comp) = out.get_mut(s).and_then(|x| x.get_mut(c))
            {
                comp.push(0);
            }
            if self.garbage {
                for comp in out.iter_mut().flatten() {
                    for (i, b) in comp.iter_mut().enumerate() {
                        *b = (i as u8).wrapping_mul(97) ^ 0x5a;
                    }
                }
            }
            Some(out)
        }
        fn alpha(&mut self) -> Option<Vec<Vec<u8>>> {
            match self.alpha_mode {
                AlphaMode::Slices => self.slices.iter().map(|sd| sd.alpha.clone()).collect(),
                AlphaMode::Nothing => None,
                AlphaMode::WrongCount => {
                    let mut v: Vec<Vec<u8>> = self.slices.iter().map(|sd| sd.alpha.clone()).collect::<Option<_>>()?;
                    v.pop();
                    Some(v)
                }
            }
        }
        fn choose(&mut self, hint: u8, budget: u64, limits: &[u64]) -> Option<(Vec<u8>, u8)> {
            self.choose_calls += 1;
            let cpu = || {
                let slices = &self.slices;
                choose_quantisers(budget, limits, hint, &mut |q| Some(slices.iter().map(|sd| slice_bits(sd, q)).collect()))
            };
            match &self.choose {
                ChooseMode::Default => None,
                ChooseMode::Cpu => cpu(),
                ChooseMode::Fixed(qs, qg) => Some((qs.clone(), *qg)),
                ChooseMode::TooLong => {
                    let (mut qs, qg) = cpu()?;
                    qs.push(qg);
                    Some((qs, qg))
                }
            }
        }
    }

    #[test]
    fn a_backend_doing_the_cpus_work_gives_the_cpus_frame() {
        for profile in Profile::ALL {
            for (w, h) in [(16, 16), (17, 13), (64, 48), (1920, 1080)] {
                let f = test_frame(profile, w, h, 7);
                let cpu = Encoder::new(profile, w, h).encode(&f).unwrap();
                let mut b = CpuBackend::default();
                let (gpu, used) = Encoder::new(profile, w, h).encode_with(&f, Some(&mut b)).unwrap();
                assert!(used, "{profile:?} {w}x{h}");
                assert_eq!(gpu, cpu, "{profile:?} {w}x{h}");
            }
        }
    }

    #[test]
    fn slice_jobs_cover_the_frame_in_coded_order() {
        let enc = Encoder::new(Profile::Hq, 333, 201);
        let jobs = enc.slice_jobs();
        let mbs: u32 = jobs.iter().map(|j| j.mbs).sum();
        assert_eq!(mbs, 333u32.div_ceil(16) * 201u32.div_ceil(16));
        assert_eq!(jobs.first(), Some(&SliceJob { mb_x: 0, mb_y: 0, mbs: 8 }));
    }

    #[test]
    fn a_backend_that_declines_or_fails_leaves_the_frame_to_the_cpu() {
        let (w, h) = (64, 48);
        for profile in [Profile::Hq, Profile::P4444] {
            let f = test_frame(profile, w, h, 3);
            let cpu = Encoder::new(profile, w, h).encode(&f).unwrap();
            let cases = [
                ("declining", CpuBackend { decline: true, ..Default::default() }),
                ("fails bits", CpuBackend { fail_bits: true, ..Default::default() }),
                ("fails write", CpuBackend { fail_write: true, ..Default::default() }),
            ];
            for (name, mut b) in cases {
                let (out, used) = Encoder::new(profile, w, h).encode_with(&f, Some(&mut b)).unwrap();
                assert!(!used, "{profile:?} {name}");
                assert_eq!(out, cpu, "{profile:?} {name}");
            }
        }
    }

    #[test]
    fn a_backend_whose_bytes_do_not_match_its_bits_is_not_trusted() {
        let (w, h) = (128, 64);
        for profile in [Profile::Hq, Profile::P4444] {
            let f = test_frame(profile, w, h, 5);
            let cpu = Encoder::new(profile, w, h).encode(&f).unwrap();
            let mut b = CpuBackend { extra_byte: Some((3, 1)), ..Default::default() };
            let (out, used) = Encoder::new(profile, w, h).encode_with(&f, Some(&mut b)).unwrap();
            assert!(!used, "{profile:?}: one byte too many");
            assert_eq!(out, cpu, "{profile:?}: one byte too many");

            // Garbage of the right length is the backend's own frame: it must still parse and
            // decode without a panic (only the shader tests can judge its content).
            let mut b = CpuBackend { garbage: true, ..Default::default() };
            let (out, _) = Encoder::new(profile, w, h).encode_with(&f, Some(&mut b)).unwrap();
            assert!(frame_layout(&out).is_ok(), "{profile:?}: garbage frame parses");
            let _ = decode_frame(&out);
        }
    }

    #[test]
    fn the_slice_size_guard_holds_through_a_backend() {
        let (w, h) = (256, 64);
        let mut f = Frame::new(w, h, ChromaFormat::Yuv422, 10, false);
        let mut s = 12345u32;
        for v in f.y.iter_mut().chain(f.cb.iter_mut()).chain(f.cr.iter_mut()) {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *v = (s % 1024) as u16;
        }
        let mut cfg = EncoderConfig::new(Profile::Proxy, w, h);
        cfg.bits_per_mb = Some(u32::MAX / 4096);
        let cpu = Encoder::with_config(cfg.clone()).encode(&f).unwrap();
        let mut b = CpuBackend::default();
        let (out, used) = Encoder::with_config(cfg).encode_with(&f, Some(&mut b)).unwrap();
        assert!(used);
        assert_eq!(out, cpu);
        let layout = frame_layout(&out).unwrap();
        for sl in &layout.slices {
            let size: u32 = sl.comps.iter().map(|c| c.1).sum::<u32>() + 6;
            assert!(size <= 65535, "slice of {size} bytes");
        }
    }

    /// The size guard itself: slice 1 only fits its 16-bit fields from quantiser 10 on.
    #[test]
    fn the_guard_raises_a_slice_until_it_fits() {
        let limits = [1_000u64, 1_000, 1_000];
        let mut measured = Vec::new();
        let mut bits_at = |q: u8| {
            measured.push(q);
            Some(vec![500u64, if q < 10 { 5_000 } else { 900 }, 500])
        };
        let (qs, qg) = choose_quantisers(10_000, &limits, 3, &mut bits_at).unwrap();
        assert_eq!(qs, vec![qg, 10, qg]);
        assert!(measured.iter().all(|&q| (Q_MIN..=Q_MAX).contains(&q)));
        let distinct = measured.iter().collect::<std::collections::BTreeSet<_>>().len();
        assert_eq!(distinct, measured.len(), "no quantiser is measured twice: {measured:?}");
    }

    #[test]
    fn a_valid_choose_answer_is_used() {
        let (w, h) = (128, 64);
        for profile in [Profile::Hq, Profile::P4444] {
            let f = test_frame(profile, w, h, 11);
            let cpu = Encoder::new(profile, w, h).encode(&f).unwrap();
            let mut b = CpuBackend { choose: ChooseMode::Cpu, ..Default::default() };
            let (out, used) = Encoder::new(profile, w, h).encode_with(&f, Some(&mut b)).unwrap();
            assert_eq!(b.choose_calls, 1);
            assert!(used, "{profile:?}");
            assert_eq!(out, cpu, "{profile:?}");
        }
    }

    #[test]
    fn an_invalid_choose_answer_is_ignored() {
        let (w, h) = (128, 64);
        let n = Encoder::new(Profile::Hq, w, h).slice_jobs().len();
        let f = test_frame(Profile::Hq, w, h, 13);
        let cpu = Encoder::new(Profile::Hq, w, h).encode(&f).unwrap();
        let cases = [
            ("q = 0", ChooseMode::Fixed(vec![0; n], 4)),
            ("q = 225", ChooseMode::Fixed(vec![225; n], 4)),
            ("qg = 0", ChooseMode::Fixed(vec![4; n], 0)),
            ("too few", ChooseMode::Fixed(vec![4; n - 1], 4)),
            ("too many", ChooseMode::TooLong),
            ("empty", ChooseMode::Fixed(Vec::new(), 4)),
        ];
        for (name, choose) in cases {
            let mut b = CpuBackend { choose, ..Default::default() };
            let (out, used) = Encoder::new(Profile::Hq, w, h).encode_with(&f, Some(&mut b)).unwrap();
            assert_eq!(b.choose_calls, 1, "{name}");
            assert!(used, "{name}: the backend still codes it, over the CPU's rate control");
            assert_eq!(out, cpu, "{name}");
        }
    }

    #[test]
    fn missing_or_miscounted_alpha_is_coded_by_the_cpu() {
        let (w, h) = (128, 64);
        let f = test_frame(Profile::P4444, w, h, 17);
        let cpu = Encoder::new(Profile::P4444, w, h).encode(&f).unwrap();
        for (name, alpha_mode) in [("none", AlphaMode::Nothing), ("wrong count", AlphaMode::WrongCount)] {
            let mut b = CpuBackend { alpha_mode, ..Default::default() };
            let (out, used) = Encoder::new(Profile::P4444, w, h).encode_with(&f, Some(&mut b)).unwrap();
            assert!(used, "{name}");
            assert_eq!(out, cpu, "{name}");
        }
    }
}
