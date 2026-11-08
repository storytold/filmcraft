//! ProRes frame encoding (progressive, or interlaced as two field pictures) with per-slice
//! quantiser rate control.

use crate::bits::{Counter, Sink, Writer};
use crate::dct::fdct8x8;
use crate::header::{FRAME_ID, FrameHeader, PictureHeader, row_slice_widths};
use crate::tables::{AC_ROUND, DC_CB, DC_CTX_INIT, FIRST_DC_CB, LEVEL_CB, LEVEL_CTX_INIT, RUN_CB, RUN_CTX_INIT, SCAN_INTERLACED, SCAN_PROGRESSIVE, qscale};
use crate::{AlphaType, ChromaFormat, ColorInfo, Error, Frame, Interlace, Profile, Result};
use std::any::Any;

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
    /// Progressive frames (the default), or interlaced: each frame coded as two field pictures,
    /// the first coded one the temporally first field (top: the even lines, bottom: the odd lines).
    pub interlace: Interlace,
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
            interlace: Interlace::Progressive,
        }
    }
}

/// ProRes encoder for one stream of same-sized frames.
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
        let srcs = Source::pictures(&self.cfg, frame, with_alpha);
        if let Some(backend) = backend
            && let Some((out, qg)) = self.encode_on_backend(Input::Planes(frame, &srcs), with_alpha, backend, &jobs)
        {
            self.last_q = qg;
            return Ok((out, true));
        }
        let threads = self.cfg.threads;
        let prepared = prepare_slices(threads, &srcs, &jobs).ok_or(Error::Invalid("slice outside the frame's pictures"))?;
        let limits: Vec<u64> = prepared.iter().map(SliceData::limit_bits).collect();
        let budget = frame_budget(prepared.len(), slice_header_size(with_alpha), self.bits_per_mb(), &jobs, srcs.len());
        let mut bits_at = |q: u8| Some(par_map(threads, &prepared, |sd| slice_bits(sd, q)));
        let (qs, qg) = choose_quantisers(budget, &limits, self.last_q, &mut bits_at).ok_or(Error::Invalid("rate control failed"))?;
        self.last_q = qg;
        let items: Vec<(&SliceData, u8)> = prepared.iter().zip(qs).collect();
        let slices: Option<Vec<Vec<u8>>> = par_map(threads, &items, |&(sd, q)| write_slice(sd, q)).into_iter().collect();
        let slices = slices.ok_or(Error::Invalid("slice larger than its 16-bit size field"))?;
        Ok((self.assemble_frame(&slices, with_alpha, srcs.len())?, false))
    }

    /// `encode_with` for a frame whose planes are already where `backend` works (a GPU export
    /// renders them there, see [`SliceBackend::load_resident`]): `frame` is that backend's handle.
    /// The planes must be as `encode_with` takes them (10-bit samples of the profile's chroma
    /// format, alpha when the configuration codes it). `None` when the backend does not take the
    /// handle, fails, or answers inconsistently: the planes are not here, so the CPU cannot code
    /// the frame; the caller makes them and calls `encode_with`.
    pub fn encode_resident(&mut self, frame: &dyn Any, backend: &mut dyn SliceBackend) -> Option<Vec<u8>> {
        self.validate_config().ok()?;
        let with_alpha = self.cfg.encode_alpha && self.cfg.profile.chroma() == ChromaFormat::Yuv444;
        let jobs = self.slice_jobs();
        let (out, qg) = self.encode_on_backend(Input::Resident(frame), with_alpha, backend, &jobs)?;
        self.last_q = qg;
        Some(out)
    }

    /// The coded pictures of a frame: one, or an interlaced frame's two fields.
    fn pictures(&self) -> usize {
        if self.cfg.interlace == Interlace::Progressive { 1 } else { 2 }
    }

    /// The slices of a frame of this encoder's size, in coded order: picture by picture (one, or
    /// an interlaced frame's two fields, each `ceil(height / 32)` macroblock rows), row by row.
    pub fn slice_jobs(&self) -> Vec<SliceJob> {
        let mb_width = (self.cfg.width as usize).div_ceil(16);
        let (pictures, mb_height) = match self.cfg.interlace {
            Interlace::Progressive => (1u32, (self.cfg.height as usize).div_ceil(16)),
            _ => (2, (self.cfg.height as usize).div_ceil(32)),
        };
        let widths = row_slice_widths(mb_width, self.cfg.log2_slice_mbs.min(3));
        let mut jobs = Vec::with_capacity(widths.len() * mb_height * pictures as usize);
        for picture in 0..pictures {
            for row in 0..mb_height {
                let mut x = 0usize;
                for &sw in &widths {
                    jobs.push(SliceJob { mb_x: x as u32, mb_y: row as u32, mbs: sw as u32, picture });
                    x += sw;
                }
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
        self.validate_config()?;
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
        Ok(with_alpha)
    }

    /// The checks of `validate` that do not look at a frame.
    fn validate_config(&self) -> Result<()> {
        let cfg = &self.cfg;
        if cfg.width == 0 || cfg.height == 0 || cfg.width > 65535 || cfg.height > 65535 {
            return Err(Error::Input("unsupported frame size".into()));
        }
        if cfg.log2_slice_mbs > 3 {
            return Err(Error::Input("log2_slice_mbs must be 0..=3".into()));
        }
        if cfg.interlace != Interlace::Progressive && cfg.height < 2 {
            return Err(Error::Input("an interlaced frame needs at least two lines".into()));
        }
        Ok(())
    }

    /// The frame coded with the backend's per-block work, and its uniform quantiser. `None` when
    /// the backend declines, fails, or its answers do not agree with each other.
    fn encode_on_backend(&self, input: Input, with_alpha: bool, backend: &mut dyn SliceBackend, jobs: &[SliceJob]) -> Option<(Vec<u8>, u8)> {
        let n = jobs.len();
        let loaded = match input {
            Input::Planes(frame, _) => backend.load(&self.cfg, frame, jobs),
            Input::Resident(frame) => backend.load_resident(&self.cfg, frame, jobs),
        };
        if n == 0 || !loaded {
            return None;
        }
        let alpha: Vec<Option<Vec<u8>>> = if with_alpha {
            match (backend.alpha(), input) {
                (Some(a), _) if a.len() == n => a.into_iter().map(Some).collect(),
                (_, Input::Planes(_, srcs)) => jobs
                    .iter()
                    .map(|j| srcs.get(j.picture as usize).map(|src| encode_alpha_slice(src, j.mb_x as usize * 16, j.mb_y as usize * 16, j.mbs as usize)))
                    .collect::<Option<Vec<Vec<u8>>>>()?
                    .into_iter()
                    .map(Some)
                    .collect(),
                // (the alpha plane is with the backend only)
                (_, Input::Resident(_)) => return None,
            }
        } else {
            vec![None; n]
        };
        let pictures = self.pictures();
        let limits: Vec<u64> = alpha.iter().map(|a| slice_limit_bits(with_alpha, a.as_ref().map_or(0, Vec::len))).collect();
        let budget = frame_budget(n, slice_header_size(with_alpha), self.bits_per_mb(), jobs, pictures);
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
        let hsize = slice_header_size(with_alpha);
        let mut slices = Vec::with_capacity(n);
        for (((comps, &q), bits), a) in written.iter().zip(&qs).zip(&want).zip(&alpha) {
            if comps.iter().zip(bits).any(|(c, &b)| c.len() as u64 * 8 != b as u64) {
                return None;
            }
            slices.push(assemble_slice(hsize, q, [&comps[0], &comps[1], &comps[2]], a.as_deref())?);
        }
        let out = self.assemble_frame(&slices, with_alpha, pictures).ok()?;
        Some((out, qg))
    }

    /// The frame header, then per picture (`pictures` of them, each with the same number of
    /// slices, in coded order) the picture header, slice table and slices.
    fn assemble_frame(&self, slices: &[Vec<u8>], with_alpha: bool, pictures: usize) -> Result<Vec<u8>> {
        let cfg = &self.cfg;
        let chroma = cfg.profile.chroma();
        if slices.iter().any(|s| s.len() > 65535) {
            return Err(Error::Invalid("slice larger than its 16-bit size field"));
        }
        if pictures == 0 || !slices.len().is_multiple_of(pictures) {
            return Err(Error::Invalid("pictures of unequal slice counts"));
        }
        let hdr = FrameHeader {
            header_size: 0,
            bitstream_version: if chroma == ChromaFormat::Yuv444 { 1 } else { 0 },
            encoder_id: cfg.encoder_id,
            width: cfg.width as u16,
            height: cfg.height as u16,
            chroma,
            interlace: cfg.interlace,
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
        let per_picture = (slices.len() / pictures).max(1);
        let picture_size = |ps: &[Vec<u8>]| 8 + 2 * ps.len() + ps.iter().map(Vec::len).sum::<usize>();
        let total = 8 + fh.len() + slices.chunks(per_picture).map(picture_size).sum::<usize>();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(&FRAME_ID);
        out.extend_from_slice(&fh);
        for ps in slices.chunks(per_picture) {
            PictureHeader { header_size: 8, picture_size: picture_size(ps), num_slices: ps.len(), log2_slice_mbs: cfg.log2_slice_mbs }.write(&mut out);
            for s in ps {
                out.extend_from_slice(&(s.len() as u16).to_be_bytes());
            }
            for s in ps {
                out.extend_from_slice(s);
            }
        }
        debug_assert_eq!(out.len(), total);
        Ok(out)
    }
}

/// Where one slice lies, in coded order (picture by picture, row-major, `row_slice_widths` per
/// row).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SliceJob {
    pub mb_x: u32,
    /// Macroblock row within the slice's picture.
    pub mb_y: u32,
    pub mbs: u32,
    /// The coded picture: 0, or 1 for an interlaced frame's second field. A picture's lines are
    /// [`picture_lines`] of the frame (an interlaced frame's fields: every second line), and its
    /// slices use the interlaced scan when the frame is interlaced.
    pub picture: u32,
}

/// The frame lines coded picture `picture` holds: the first one and the step to the next. An
/// interlaced frame's picture 0 is the temporally first field (top: the even lines from 0, bottom:
/// the odd lines from 1), picture 1 the other; a progressive frame's one picture is every line.
pub fn picture_lines(interlace: Interlace, picture: u32) -> (u32, u32) {
    let first = picture & 1;
    match interlace {
        Interlace::Progressive => (0, 1),
        Interlace::TopFieldFirst => (first, 2),
        Interlace::BottomFieldFirst => (first ^ 1, 2),
    }
}

/// Where the planes of the frame being coded are: here, or already with the backend.
#[derive(Clone, Copy)]
enum Input<'a> {
    Planes(&'a Frame, &'a [Source<'a>]),
    Resident(&'a dyn Any),
}

/// The per-block work of an encode on other hardware: forward DCT, quantisation, entropy coding
/// of the Y, Cb and Cr components of every slice, exactly as this crate's encoder does them.
pub trait SliceBackend: Send {
    /// Take `frame` (already validated by the encoder) for `slices`. `false`: this backend cannot
    /// encode it (size, profile, a device error); the encoder then uses its own code.
    fn load(&mut self, cfg: &EncoderConfig, frame: &Frame, slices: &[SliceJob]) -> bool;
    /// [`SliceBackend::load`] for a frame whose planes this backend already holds: `frame` is a
    /// handle of its own making (a GPU buffer the export rendered into), opaque to the encoder
    /// ([`Encoder::encode_resident`]). `false` (the default): not a handle it takes.
    fn load_resident(&mut self, cfg: &EncoderConfig, frame: &dyn Any, slices: &[SliceJob]) -> bool {
        let _ = (cfg, frame, slices);
        false
    }
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
/// Unlike DNxHR's, the answer is not checked against `budget`: a ProRes frame has no fixed size,
/// so an answer that overshoots only makes a larger frame (each slice's 16-bit size fields are
/// still checked when it is written), while a DNxHR frame must fit its constant size.
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

/// The DCT payload budget of a frame in bits (after the fixed headers): `bits_per_mb` for every
/// macroblock `jobs` code (both fields of an interlaced frame), less the frame header, the
/// `pictures` picture headers and the slice tables and headers.
fn frame_budget(slices: usize, hsize: usize, bits_per_mb: u32, jobs: &[SliceJob], pictures: usize) -> u64 {
    let total_mbs: u64 = jobs.iter().map(|j| u64::from(j.mbs)).sum();
    let fixed = 8 * (8 + 20 + 8 * pictures + slices * (2 + hsize)) as u64;
    (u64::from(bits_per_mb).saturating_mul(total_mbs)).saturating_sub(fixed)
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

/// One coded picture of a frame: the frame, or one of its fields (every second line).
struct Source<'a> {
    frame: &'a Frame,
    w: usize,
    /// Lines of the picture.
    h: usize,
    /// The frame line of the picture's line 0, and the step between its lines.
    first: usize,
    step: usize,
    cw: usize,
    chroma444: bool,
    /// Multiplier from input sample values to the 12-bit domain.
    scale: f32,
    alpha: bool,
    /// The scan order of the picture's coefficients.
    scan: &'static [u8; 64],
}

impl<'a> Source<'a> {
    /// The coded pictures of `frame` (validated), in coded order.
    fn pictures(cfg: &EncoderConfig, frame: &'a Frame, alpha: bool) -> Vec<Source<'a>> {
        let chroma = cfg.profile.chroma();
        let count = if cfg.interlace == Interlace::Progressive { 1 } else { 2 };
        let scan = if count == 1 { &SCAN_PROGRESSIVE } else { &SCAN_INTERLACED };
        (0..count)
            .map(|picture| {
                let (first, step) = picture_lines(cfg.interlace, picture);
                let (first, step) = (first as usize, step as usize);
                Source {
                    frame,
                    w: cfg.width as usize,
                    h: (cfg.height as usize).saturating_sub(first).div_ceil(step).max(1),
                    first,
                    step,
                    cw: chroma.chroma_width(cfg.width) as usize,
                    chroma444: chroma == ChromaFormat::Yuv444,
                    scale: (2f32).powi(12 - frame.bit_depth as i32),
                    alpha,
                    scan,
                }
            })
            .collect()
    }

    /// The frame line of picture line `y`, edge-replicated below the picture's last line.
    fn line(&self, y: usize) -> usize {
        self.first + y.min(self.h - 1) * self.step
    }

    /// Load an 8×8 block of `plane` at picture position (`x0`, `y0`), edge-replicated, into the
    /// 12-bit signed domain and transform it.
    fn load_block(&self, plane: &[u16], stride: usize, x0: usize, y0: usize) -> [f32; 64] {
        let mut b = [0f32; 64];
        for yy in 0..8 {
            let y = self.line(y0 + yy);
            let row = &plane[y * stride..(y + 1) * stride];
            for xx in 0..8 {
                let x = (x0 + xx).min(stride - 1);
                b[yy * 8 + xx] = row[x] as f32 * self.scale - 2048.0;
            }
        }
        fdct8x8(&mut b);
        b
    }
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

fn component_bits(levels: &[[i32; 64]], scan: &[u8; 64]) -> u64 {
    let mut c = Counter::default();
    encode_component(&mut c, levels, scan);
    c.0.div_ceil(8) * 8
}

/// A slice's transformed blocks (Y, Cb, Cr), its coded alpha and its picture's scan order.
struct SliceData {
    comps: [Vec<[f32; 64]>; 3],
    alpha: Option<Vec<u8>>,
    scan: &'static [u8; 64],
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

/// Every job's slice, from its picture of `srcs`; `None` for a job outside them.
fn prepare_slices(threads: bool, srcs: &[Source], jobs: &[SliceJob]) -> Option<Vec<SliceData>> {
    par_map(threads, jobs, |j| srcs.get(j.picture as usize).map(|src| prepare_slice(src, j.mb_x as usize, j.mb_y as usize, j.mbs as usize)))
        .into_iter()
        .collect()
}

fn prepare_slice(src: &Source, mb_x: usize, mb_y: usize, mbs: usize) -> SliceData {
    let f = src.frame;
    let x0 = mb_x * 16;
    let y0 = mb_y * 16;
    let mut luma = Vec::with_capacity(4 * mbs);
    for mb in 0..mbs {
        for i in 0..4 {
            luma.push(src.load_block(&f.y, src.w, x0 + mb * 16 + (i & 1) * 8, y0 + (i >> 1) * 8));
        }
    }
    let mut chroma = [Vec::with_capacity(4 * mbs), Vec::with_capacity(4 * mbs)];
    for (k, plane) in [&f.cb, &f.cr].into_iter().enumerate() {
        for mb in 0..mbs {
            if src.chroma444 {
                // 4:4:4 chroma block order within a macroblock: TL, BL, TR, BR.
                for i in 0..4 {
                    chroma[k].push(src.load_block(plane, src.cw, x0 + mb * 16 + (i >> 1) * 8, y0 + (i & 1) * 8));
                }
            } else {
                for i in 0..2 {
                    chroma[k].push(src.load_block(plane, src.cw, mb_x * 8 + mb * 8, y0 + i * 8));
                }
            }
        }
    }
    let [cb, cr] = chroma;
    let alpha = src.alpha.then(|| encode_alpha_slice(src, x0, y0, mbs));
    SliceData { comps: [luma, cb, cr], alpha, scan: src.scan }
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
    lv.map(|l| component_bits(&l, sd.scan))
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

/// The encoder's rate control (`choose_quantisers`) over `bits_at`, for tests of a backend's
/// `choose` in other crates: its answer must be this one over the same measurements.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn rate_control_for_tests(budget: u64, limits: &[u64], hint: u8, bits_at: &mut dyn FnMut(u8) -> Option<Vec<u64>>) -> Option<(Vec<u8>, u8)> {
    choose_quantisers(budget, limits, hint, bits_at)
}

/// This crate's own CPU functions behind [`SliceBackend`]: what a correct backend must give,
/// for tests of the seam in other crates (a stand-in for a device that works).
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
#[derive(Default)]
pub struct ReferenceBackend {
    slices: Vec<SliceData>,
}

#[cfg(any(test, feature = "test-support"))]
impl SliceBackend for ReferenceBackend {
    /// A [`Frame`] passed as the handle loads as `load` would.
    fn load_resident(&mut self, cfg: &EncoderConfig, frame: &dyn Any, jobs: &[SliceJob]) -> bool {
        match frame.downcast_ref::<Frame>() {
            Some(frame) => self.load(cfg, frame, jobs),
            None => {
                self.slices.clear();
                false
            }
        }
    }
    /// `false` for a frame the encoder would refuse (it checks every frame before a backend sees
    /// it) or slices other than the encoder's own: nothing outside the planes is read.
    fn load(&mut self, cfg: &EncoderConfig, frame: &Frame, jobs: &[SliceJob]) -> bool {
        let enc = Encoder::with_config(cfg.clone());
        let alpha = match enc.validate(frame) {
            Ok(alpha) if jobs == enc.slice_jobs().as_slice() => alpha,
            _ => {
                self.slices.clear();
                return false;
            }
        };
        let srcs = Source::pictures(cfg, frame, alpha);
        match prepare_slices(false, &srcs, jobs) {
            Some(slices) => {
                self.slices = slices;
                true
            }
            None => {
                self.slices.clear();
                false
            }
        }
    }
    fn bits(&mut self, q: u8) -> Option<Vec<[u32; 3]>> {
        Some(self.slices.iter().map(|sd| slice_comp_bits(sd, q).map(|b| b as u32)).collect())
    }
    fn write(&mut self, qs: &[u8]) -> Option<Vec<[Vec<u8>; 3]>> {
        Some(self.slices.iter().zip(qs).map(|(sd, &q)| write_components(sd, q)).collect())
    }
    fn alpha(&mut self) -> Option<Vec<Vec<u8>>> {
        self.slices.iter().map(|sd| sd.alpha.clone()).collect()
    }
}

/// The Y, Cb and Cr payloads of a slice at quantiser `q`.
fn write_components(sd: &SliceData, q: u8) -> [Vec<u8>; 3] {
    let mut lv = [Vec::new(), Vec::new(), Vec::new()];
    quantise_all(sd, q, &mut lv);
    lv.map(|l| {
        let mut w = Writer::new();
        encode_component(&mut w, &l, sd.scan);
        w.finish()
    })
}

/// A whole slice; `None` when it does not fit its 16-bit size fields.
fn write_slice(sd: &SliceData, q: u8) -> Option<Vec<u8>> {
    let [y, cb, cr] = write_components(sd, q);
    assemble_slice(sd.header_size(), q, [&y, &cb, &cr], sd.alpha.as_deref())
}

/// Encode the slice's alpha samples (16-bit coding) in raster order over its 16 lines of its
/// picture.
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
        let y = src.line(y0 + yy);
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
        let slices: Vec<SliceData> = (1..=3)
            .map(|k| SliceData { comps: [vec![block(40.0 * k as f32); 4], vec![block(10.0); 2], vec![block(5.0); 2]], alpha: None, scan: &SCAN_PROGRESSIVE })
            .collect();
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
    use crate::{SliceBackend, SliceJob, decode_frame, picture_lines};

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
        inner: ReferenceBackend,
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
            !self.decline && self.inner.load(cfg, frame, jobs)
        }
        fn bits(&mut self, q: u8) -> Option<Vec<[u32; 3]>> {
            if self.fail_bits {
                return None;
            }
            self.inner.bits(q)
        }
        fn write(&mut self, qs: &[u8]) -> Option<Vec<[Vec<u8>; 3]>> {
            if self.fail_write {
                return None;
            }
            let mut out = self.inner.write(qs)?;
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
                AlphaMode::Slices => self.inner.alpha(),
                AlphaMode::Nothing => None,
                AlphaMode::WrongCount => {
                    let mut v = self.inner.alpha()?;
                    v.pop();
                    Some(v)
                }
            }
        }
        fn choose(&mut self, hint: u8, budget: u64, limits: &[u64]) -> Option<(Vec<u8>, u8)> {
            self.choose_calls += 1;
            let cpu = || {
                let slices = &self.inner.slices;
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

    /// A [`ReferenceBackend`] that never codes alpha (the encoder must then code it itself).
    struct NoAlpha(ReferenceBackend);

    impl SliceBackend for NoAlpha {
        fn load(&mut self, cfg: &EncoderConfig, frame: &Frame, slices: &[SliceJob]) -> bool {
            self.0.load(cfg, frame, slices)
        }
        fn load_resident(&mut self, cfg: &EncoderConfig, frame: &dyn Any, slices: &[SliceJob]) -> bool {
            self.0.load_resident(cfg, frame, slices)
        }
        fn bits(&mut self, q: u8) -> Option<Vec<[u32; 3]>> {
            self.0.bits(q)
        }
        fn write(&mut self, qs: &[u8]) -> Option<Vec<[Vec<u8>; 3]>> {
            self.0.write(qs)
        }
    }

    /// A frame whose planes the backend holds already (its handle: here the `Frame` itself) codes
    /// to the bytes of the same planes coded on the CPU, progressive and interlaced, with alpha. A
    /// handle the backend does not take, a backend that takes none, and a 4444 frame whose alpha
    /// the backend does not code give nothing (the planes are not with the encoder).
    #[test]
    fn a_resident_frame_codes_like_its_planes() {
        for profile in [Profile::Hq, Profile::P4444] {
            for interlace in [Interlace::Progressive, Interlace::TopFieldFirst] {
                for (w, h) in [(17, 13), (64, 48)] {
                    let f = test_frame(profile, w, h, 3);
                    let cfg = EncoderConfig { interlace, ..EncoderConfig::new(profile, w, h) };
                    let what = format!("{profile:?} {interlace:?} {w}x{h}");
                    let cpu = Encoder::with_config(cfg.clone()).encode(&f).unwrap();
                    let mut enc = Encoder::with_config(cfg.clone());
                    let resident = enc.encode_resident(&f, &mut ReferenceBackend::default());
                    assert_eq!(resident.as_deref(), Some(&cpu[..]), "{what}");
                    // the next frame's search starts where this one ended, as after `encode`
                    let mut cpu_enc = Encoder::with_config(cfg.clone());
                    cpu_enc.encode(&f).unwrap();
                    assert_eq!(enc.encode_resident(&f, &mut ReferenceBackend::default()), Some(cpu_enc.encode(&f).unwrap()), "{what}: second frame");
                    assert!(Encoder::with_config(cfg.clone()).encode_resident(&42u32, &mut ReferenceBackend::default()).is_none(), "{what}");
                    assert!(Encoder::with_config(cfg.clone()).encode_resident(&f, &mut CpuBackend::default()).is_none(), "{what}");
                    let no_alpha = Encoder::with_config(cfg).encode_resident(&f, &mut NoAlpha(ReferenceBackend::default()));
                    assert_eq!(no_alpha.is_some(), profile == Profile::Hq, "{what}: alpha only the backend could code");
                }
            }
        }
    }

    /// An interlaced frame's two field pictures go through the backend too, with the CPU's bytes.
    #[test]
    fn a_backend_doing_the_cpus_work_gives_the_cpus_interlaced_frame() {
        for interlace in [Interlace::TopFieldFirst, Interlace::BottomFieldFirst] {
            for profile in Profile::ALL {
                for (w, h) in [(16, 2), (17, 13), (64, 48), (1920, 1080)] {
                    let f = test_frame(profile, w, h, 7);
                    let cfg = EncoderConfig { interlace, ..EncoderConfig::new(profile, w, h) };
                    let cpu = Encoder::with_config(cfg.clone()).encode(&f).unwrap();
                    assert_eq!(frame_layout(&cpu).unwrap().header.interlace, interlace);
                    let mut b = CpuBackend::default();
                    let (gpu, used) = Encoder::with_config(cfg.clone()).encode_with(&f, Some(&mut b)).unwrap();
                    assert!(used, "{interlace:?} {profile:?} {w}x{h}");
                    assert_eq!(gpu, cpu, "{interlace:?} {profile:?} {w}x{h}");
                    let mut b = CpuBackend { choose: ChooseMode::Cpu, ..Default::default() };
                    let (gpu, used) = Encoder::with_config(cfg).encode_with(&f, Some(&mut b)).unwrap();
                    assert!(used && gpu == cpu, "{interlace:?} {profile:?} {w}x{h}: the backend's rate control");
                }
            }
        }
    }

    #[test]
    fn interlaced_slice_jobs_are_two_field_pictures() {
        assert_eq!(picture_lines(Interlace::Progressive, 0), (0, 1));
        assert_eq!((picture_lines(Interlace::TopFieldFirst, 0), picture_lines(Interlace::TopFieldFirst, 1)), ((0, 2), (1, 2)));
        assert_eq!((picture_lines(Interlace::BottomFieldFirst, 0), picture_lines(Interlace::BottomFieldFirst, 1)), ((1, 2), (0, 2)));
        let enc = Encoder::with_config(EncoderConfig { interlace: Interlace::BottomFieldFirst, ..EncoderConfig::new(Profile::Hq, 333, 201) });
        let jobs = enc.slice_jobs();
        let rows = 201u32.div_ceil(32);
        for picture in 0..2 {
            let pic: Vec<&SliceJob> = jobs.iter().filter(|j| j.picture == picture).collect();
            assert_eq!(pic.iter().map(|j| j.mbs).sum::<u32>(), 333u32.div_ceil(16) * rows);
            assert_eq!(pic.last().map(|j| j.mb_y), Some(rows - 1));
        }
        assert!(jobs.windows(2).all(|p| p[0].picture <= p[1].picture), "picture by picture");
    }

    #[test]
    fn slice_jobs_cover_the_frame_in_coded_order() {
        let enc = Encoder::new(Profile::Hq, 333, 201);
        let jobs = enc.slice_jobs();
        let mbs: u32 = jobs.iter().map(|j| j.mbs).sum();
        assert_eq!(mbs, 333u32.div_ceil(16) * 201u32.div_ceil(16));
        assert_eq!(jobs.first(), Some(&SliceJob { mb_x: 0, mb_y: 0, mbs: 8, picture: 0 }));
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

    /// The reference backend checks the frame as the encoder does: planes smaller than the
    /// configuration, a frame of another size, or an interlaced configuration of one line are
    /// refused, not indexed out of bounds.
    #[test]
    fn the_reference_backend_refuses_frames_that_do_not_match_the_configuration() {
        let load = |cfg: &EncoderConfig, f: &Frame| {
            let jobs = Encoder::with_config(cfg.clone()).slice_jobs();
            ReferenceBackend::default().load(cfg, f, &jobs)
        };
        let cfg = EncoderConfig::new(Profile::Hq, 64, 48);
        assert!(load(&cfg, &test_frame(Profile::Hq, 64, 48, 1)), "a matching frame loads");
        let mut small = test_frame(Profile::Hq, 64, 48, 1);
        small.y.truncate(100);
        assert!(!load(&cfg, &small), "a luma plane too small");
        let mut small = test_frame(Profile::Hq, 64, 48, 1);
        small.cr.truncate(100);
        assert!(!load(&cfg, &small), "a chroma plane too small");
        let mut other = test_frame(Profile::Hq, 32, 32, 1);
        other.width = 64;
        other.height = 48;
        assert!(!load(&cfg, &other), "planes of a smaller frame");
        for interlace in [Interlace::TopFieldFirst, Interlace::BottomFieldFirst] {
            let cfg = EncoderConfig { interlace, ..EncoderConfig::new(Profile::Hq, 16, 1) };
            assert!(!load(&cfg, &test_frame(Profile::Hq, 16, 1, 1)), "{interlace:?} with one line");
        }
        let cfg = EncoderConfig { encode_alpha: true, ..EncoderConfig::new(Profile::P4444, 64, 48) };
        let mut f = test_frame(Profile::P4444, 64, 48, 1);
        if let Some(a) = f.alpha.as_mut() {
            a.truncate(100);
        }
        assert!(!load(&cfg, &f), "an alpha plane too small");
    }
}
