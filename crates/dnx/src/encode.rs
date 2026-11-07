//! DNxHR encoder (RI profile, CBR, progressive).
//!
//! Every macroblock is transformed once (orthonormal 8×8 FDCT). Quantisation follows equation
//! 8.2 exactly (`X̂ = ⌊p·|X| / (qsf·W)⌋`, which the decoder reconstructs at the interval
//! midpoint). Rate control: a binary search finds the smallest uniform quantisation scale factor
//! whose frame fits the CBR payload, then macroblocks get one finer step in scan order while the
//! budget allows.
//!
//! Alpha (§7.3.1.2) follows a scan line's colour macroblocks: DCT-coded like luma with the
//! macroblock's quantiser, or lossless (differential RLE, or raw samples where those are
//! shorter); the frame size grows by half (equation 7.1's alpha factor).

use crate::bits::Writer;
use crate::dct::fdct8x8;
use crate::header::{self, EOF_SIGNATURE, FieldCode, FrameHeader, SCAN_INDEX_OFFSET};
use crate::tables::{CidInfo, VlcSet, ZIGZAG, cid_info, vlc};
use crate::{ChromaFormat, ColorVolume, Error, Frame, Profile, Result};

/// Encoder configuration.
#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub profile: Profile,
    pub width: u32,
    pub height: u32,
    /// Coded sample depth: 8 for LB/SQ/HQ; 10 or 12 for HQX and 444.
    pub bit_depth: u8,
    pub color_volume: ColorVolume,
    /// Pixel aspect ratio to signal (0/0 = not signalled).
    pub par: (u16, u16),
    /// Code the frame's alpha, and how (`None`: no alpha).
    pub alpha: Option<AlphaCoding>,
}

/// How alpha is coded (§7.3.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlphaCoding {
    /// DCT blocks like luma, quantised with the colour macroblock's scale.
    Dct,
    /// Lossless: differential run lengths, or raw samples.
    Lossless,
}

impl EncoderConfig {
    pub fn new(profile: Profile, width: u32, height: u32) -> EncoderConfig {
        let bit_depth = match profile {
            Profile::Hqx | Profile::R444 => 10,
            _ => 8,
        };
        EncoderConfig { profile, width, height, bit_depth, color_volume: ColorVolume::Bt709, par: (0, 0), alpha: None }
    }

    pub fn chroma(&self) -> ChromaFormat {
        if self.profile == Profile::R444 { ChromaFormat::Yuv444 } else { ChromaFormat::Yuv422 }
    }
}

/// DNxHR encoder.
pub struct Encoder {
    cfg: EncoderConfig,
    info: &'static CidInfo,
    frame_size: u32,
    header_size: u32,
    last_q: u32,
}

/// Transformed macroblock data: per block, the DC value and the AC magnitudes pre-divided by
/// the weights (`p·|X|/W`, sign in the float's sign) in bitstream (zig-zag) order.
struct Prepared {
    nw: usize,
    ns: usize,
    /// Blocks per macroblock: the colour blocks, then the alpha blocks of DCT-coded alpha.
    nblk: usize,
    ncol: usize,
    /// `[mb * nblk + k][r]`; `[..][0]` is the DC value.
    t: Vec<[f32; 64]>,
    /// DC bits of each macroblock, its lossless alpha included (independent of the quantiser).
    dc_bits: Vec<u32>,
    /// Lossless alpha of each macroblock: what to write (value, bits).
    rle: Vec<Vec<(u32, u32)>>,
}

impl Encoder {
    /// An encoder with the default configuration; fails on rasters the profile can't code.
    pub fn new(profile: Profile, width: u32, height: u32) -> Result<Encoder> {
        Encoder::with_config(EncoderConfig::new(profile, width, height))
    }

    pub fn with_config(cfg: EncoderConfig) -> Result<Encoder> {
        if cfg.width == 0 || cfg.height == 0 || cfg.width > 16384 || cfg.height > 16384 {
            return Err(Error::Input(format!("unsupported raster {}x{}", cfg.width, cfg.height)));
        }
        if cfg.chroma() == ChromaFormat::Yuv422 && !cfg.width.is_multiple_of(2) {
            return Err(Error::Input("4:2:2 needs an even width".into()));
        }
        let ok_depth = match cfg.profile {
            Profile::Hqx | Profile::R444 => matches!(cfg.bit_depth, 10 | 12),
            _ => cfg.bit_depth == 8,
        };
        if !ok_depth {
            return Err(Error::Input(format!("{} does not support {}-bit", cfg.profile.name(), cfg.bit_depth)));
        }
        let info = cid_info(cfg.profile.cid()).ok_or_else(|| Error::Input(format!("no compression table for {}", cfg.profile.name())))?;
        let frame_size = crate::tables::ri_frame_size(cfg.width, cfg.height, cfg.profile.c0(), cfg.alpha.is_some());
        let header_size = header::header_size_for(cfg.height);
        Ok(Encoder { cfg, info, frame_size, header_size, last_q: 8 })
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// Size of every encoded frame in bytes (CBR).
    pub fn frame_size(&self) -> u32 {
        self.frame_size
    }

    /// Encode one frame. Its geometry and chroma format must match the configuration; samples
    /// at another depth are rescaled to the coded depth.
    pub fn encode(&mut self, f: &Frame) -> Result<Vec<u8>> {
        self.encode_with(f, None).map(|(out, _)| out)
    }

    /// `encode`, with the per-macroblock work on `backend` when it takes the frame. Returns the
    /// frame and whether the backend coded it. When the backend declines, fails or answers
    /// inconsistently, the whole frame is coded here instead.
    pub fn encode_with(&mut self, f: &Frame, backend: Option<&mut dyn MacroblockBackend>) -> Result<(Vec<u8>, bool)> {
        self.validate(f)?;
        let budget_bits = (self.frame_size as u64 - self.header_size as u64 - 4) * 8;
        if let Some(backend) = backend
            && let Some(out) = self.encode_on_backend(f, backend, budget_bits)
        {
            return Ok((out, true));
        }
        let prep = self.prepare(f);
        let coder = Coder::new(&self.cfg, self.info, &prep, f.rgb);
        let (qs, q) = rate_control(self.last_q, prep.nw, prep.ns, budget_bits, &mut |q| Some(coder.all_bits(q)))
            .ok_or_else(|| Error::Input("rate control failed".into()))?;
        self.last_q = q;
        let rows = par_rows(prep.ns, |s| coder.write_row(s, &qs));
        Ok((self.assemble(rows, f.rgb)?, false))
    }

    /// Check the frame against the configuration.
    fn validate(&self, f: &Frame) -> Result<()> {
        let cfg = &self.cfg;
        if f.width != cfg.width || f.height != cfg.height {
            return Err(Error::Input(format!("frame is {}x{}, encoder is {}x{}", f.width, f.height, cfg.width, cfg.height)));
        }
        if f.chroma != cfg.chroma() {
            return Err(Error::Input(format!("{:?} input for {}", f.chroma, cfg.profile.name())));
        }
        if !(8..=16).contains(&f.bit_depth) {
            return Err(Error::Input(format!("unsupported input depth {}", f.bit_depth)));
        }
        let n = f.width as usize * f.height as usize;
        let cn = f.chroma_width() as usize * f.chroma_height() as usize;
        if f.y.len() != n || f.cb.len() != cn || f.cr.len() != cn {
            return Err(Error::Input("plane sizes do not match the frame geometry".into()));
        }
        if cfg.alpha.is_some() && f.alpha.as_ref().is_none_or(|a| a.len() != n) {
            return Err(Error::Input("alpha coding needs an alpha plane of the frame's size".into()));
        }
        Ok(())
    }

    /// The frame coded with the backend's per-macroblock work. `None` when the backend declines,
    /// fails, or its answers do not agree with each other (every length is checked against the
    /// bits it measured, and every macroblock header against its quantiser).
    fn encode_on_backend(&mut self, f: &Frame, backend: &mut dyn MacroblockBackend, budget_bits: u64) -> Option<Vec<u8>> {
        let nw = self.cfg.width.div_ceil(16) as usize;
        let ns = self.cfg.height.div_ceil(16) as usize;
        if !backend.load(&self.cfg, f) {
            return None;
        }
        let mut measured: Vec<(u32, Vec<u32>)> = Vec::new();
        let (qs, q) = backend_quantisers(backend, self.last_q, nw, ns, budget_bits, &mut measured)?;
        // the bits of every macroblock at the quantiser it will be written at
        let want = bits_at_qs(backend, &mut measured, &qs)?;
        if padded_frame_bits(&want, nw) > budget_bits {
            // the search found no quantiser that fits: the CPU reports it
            return None;
        }
        let written = backend.write(&qs)?;
        let alpha = self.cfg.alpha;
        let ycbcr444 = self.cfg.chroma() == ChromaFormat::Yuv444 && !f.rgb;
        let check = Stitch { nw, ns, parts: if alpha.is_some() { 2 } else { 1 }, dct_alpha: alpha == Some(AlphaCoding::Dct), ycbcr444 };
        let rows = check.rows(&written, &qs, &want)?;
        let out = self.assemble(rows, f.rgb).ok()?;
        self.last_q = q;
        Some(out)
    }

    fn prepare(&self, f: &Frame) -> Prepared {
        let cfg = &self.cfg;
        let nw = cfg.width.div_ceil(16) as usize;
        let ns = cfg.height.div_ceil(16) as usize;
        let ncol = mb_layout(cfg.chroma()).len();
        // DCT-coded alpha: four more blocks per macroblock (plane 3, coded after the colour)
        let layout: Vec<(u8, u8, u8)> = if cfg.alpha == Some(AlphaCoding::Dct) {
            mb_layout(cfg.chroma()).iter().chain(&crate::decode::LAYOUT_ALPHA).copied().collect()
        } else {
            mb_layout(cfg.chroma()).to_vec()
        };
        let nblk = layout.len();
        let depth = cfg.bit_depth as i32;
        let shift = depth - f.bit_depth as i32;
        let mid = (1i32 << (depth - 1)) as f32;
        let inv_scale = if depth == 12 { 0.25f32 } else { 1.0 };
        let p = self.info.p as f32;
        let w = self.info.weights;
        let cmbw = if cfg.chroma() == ChromaFormat::Yuv444 { 16 } else { 8 };
        let alpha: &[u16] = f.alpha.as_deref().unwrap_or(&[]);
        let planes: [(&[u16], usize, usize, usize); 4] = [
            (&f.y, f.width as usize, f.height as usize, 16),
            (&f.cb, f.chroma_width() as usize, f.chroma_height() as usize, cmbw),
            (&f.cr, f.chroma_width() as usize, f.chroma_height() as usize, cmbw),
            (alpha, f.width as usize, f.height as usize, 16),
        ];
        // a sample at the coded depth (edges repeated)
        let code = |pl: usize, x: usize, y: usize| -> i32 {
            let (d, pw, ph, _) = planes[pl];
            let v = d[y.min(ph - 1) * pw + x.min(pw - 1)] as i32;
            let v = if shift >= 0 { v << shift } else { (v + (1 << (-shift - 1))) >> -shift };
            v.min((1 << depth) - 1)
        };
        let sample = |pl: usize, x: usize, y: usize| -> f32 { code(pl, x, y) as f32 - mid };
        let do_row = |s: usize| -> Vec<[f32; 64]> {
            let mut out = Vec::with_capacity(nw * nblk);
            for mb in 0..nw {
                for &(pl, xo, yo) in &layout {
                    let pl = pl as usize;
                    let x0 = mb * planes[pl].3 + xo as usize;
                    let y0 = s * 16 + yo as usize;
                    let mut b = [0f32; 64];
                    for j in 0..8 {
                        for i in 0..8 {
                            b[j * 8 + i] = sample(pl, x0 + i, y0 + j);
                        }
                    }
                    fdct8x8(&mut b);
                    // (alpha is weighted like luma)
                    let wt = &w[(pl == 1 || pl == 2) as usize];
                    let mut t = [0f32; 64];
                    let dc_max = if depth == 8 { 1023.0 } else { 4095.0 };
                    t[0] = (b[0] * inv_scale).round().clamp(-dc_max - 1.0, dc_max);
                    for r in 1..64 {
                        let z = ZIGZAG[r] as usize;
                        t[r] = b[z] * inv_scale * p / wt[z - 1] as f32;
                    }
                    out.push(t);
                }
            }
            out
        };
        #[cfg(feature = "threads")]
        let rows: Vec<Vec<[f32; 64]>> = {
            use rayon::prelude::*;
            (0..ns).into_par_iter().map(do_row).collect()
        };
        #[cfg(not(feature = "threads"))]
        let rows: Vec<Vec<[f32; 64]>> = (0..ns).map(do_row).collect();
        let t: Vec<[f32; 64]> = rows.into_iter().flatten().collect();
        // lossless alpha, macroblock by macroblock
        let rle: Vec<Vec<(u32, u32)>> = if cfg.alpha == Some(AlphaCoding::Lossless) {
            (0..ns * nw)
                .map(|m| {
                    let (s, mb) = (m / nw, m % nw);
                    let mut px = [0u32; 256];
                    for (count, p) in px.iter_mut().enumerate() {
                        let y = count / 16;
                        // serpentine: odd lines right to left
                        let x = if y.is_multiple_of(2) { count % 16 } else { 15 - count % 16 };
                        *p = code(3, mb * 16 + x, s * 16 + y) as u32;
                    }
                    rle_alpha(&px, depth as u32)
                })
                .collect()
        } else {
            Vec::new()
        };
        // DC bits with per-row, per-component prediction
        let v = vlc(self.info.vlc);
        let mut dc_bits = vec![0u32; nw * ns];
        for s in 0..ns {
            let mut pred = [0i32; 4];
            for mb in 0..nw {
                let mut bits = rle.get(s * nw + mb).map_or(0, |r| r.iter().map(|t| t.1).sum());
                for (k, &(pl, _, _)) in layout.iter().enumerate() {
                    let dc = t[(s * nw + mb) * nblk + k][0] as i32;
                    let e = dc - pred[pl as usize];
                    pred[pl as usize] = dc;
                    let eta = dc_eta(e);
                    bits += v.dc_code[eta as usize].1 as u32 + eta;
                }
                dc_bits[s * nw + mb] = bits;
            }
        }
        Prepared { nw, ns, nblk, ncol, t, dc_bits, rle }
    }

    /// The frame header, scan index table and the scan lines (each padded to 4 bytes) of a frame.
    fn assemble(&self, rows: Vec<Vec<u8>>, rgb: bool) -> Result<Vec<u8>> {
        let cfg = &self.cfg;
        // 4:4:4 signalling as ffmpeg writes and reads it (see `FrameHeader::rgb_planes`): RGB is
        // CLF = 0 with ACF = 0; Y'CbCr is CLF = 1 with ACF = 1 on every macroblock.
        let ycbcr444 = cfg.chroma() == ChromaFormat::Yuv444 && !rgb;
        let h = FrameHeader {
            header_size: self.header_size,
            version: 3,
            cid: self.info.cid,
            vbr: false,
            field: FieldCode::Frame,
            macf: false,
            crc: false,
            alpha: cfg.alpha.is_some(),
            lossless_alpha: cfg.alpha == Some(AlphaCoding::Lossless),
            premultiplied_alpha: false,
            width: cfg.width,
            lines: cfg.height,
            par: cfg.par,
            bit_depth: cfg.bit_depth,
            interlaced: false,
            frame_encoding: true,
            chroma: cfg.chroma(),
            rgb: ycbcr444,
            color_volume: cfg.color_volume,
            timecode: None,
            mb_rows: rows.len() as u32,
        };
        let mut out = header::write(&h);
        out.reserve((self.frame_size as usize).saturating_sub(out.len()));
        let mut off = 0u32;
        for (s, r) in rows.iter().enumerate() {
            let o = SCAN_INDEX_OFFSET + 4 * s;
            out.get_mut(o..o + 4).ok_or_else(|| Error::Input("scan index outside the header".into()))?.copy_from_slice(&off.to_be_bytes());
            off += r.len() as u32;
        }
        for r in rows {
            out.extend_from_slice(&r);
        }
        let end = self.frame_size as usize - 4;
        if out.len() > end {
            return Err(Error::Input("rate control overflow".into()));
        }
        out.resize(end, 0);
        out.extend_from_slice(&EOF_SIGNATURE);
        Ok(out)
    }
}

/// The per-macroblock work of an encode on other hardware: forward DCT, bit counting at a
/// quantiser and entropy coding of every macroblock, exactly as this crate's encoder does them.
/// Macroblocks are in raster order (`mb_width` per scan line).
///
/// With alpha ([`EncoderConfig::alpha`]) a macroblock's coded bits are its colour part and its
/// alpha part: for DCT alpha a second macroblock header (11-bit qsf, a zero flag) and four alpha
/// blocks, for lossless alpha its run-length (or raw) codes.
pub trait MacroblockBackend: Send {
    /// Take `frame` (already validated by the encoder, alpha plane included when alpha is
    /// coded) for `cfg`. `false`: this backend cannot encode it (size, profile, a device error);
    /// the encoder then uses its own code.
    fn load(&mut self, cfg: &EncoderConfig, frame: &Frame) -> bool;
    /// Every macroblock's coded bits at quantiser scale factor `q`: the 11-bit qsf, the flag,
    /// DC and AC of its colour blocks (DC predicted along its scan line per component), plus its
    /// alpha part when alpha is coded.
    fn bits(&mut self, q: u32) -> Option<Vec<u32>>;
    /// Every macroblock part's bit string at the quantiser `qs[i]` of macroblock `i`, MSB first
    /// (the last byte zero-padded), and its length in bits. Without alpha: one entry per
    /// macroblock in raster order. With alpha: two entries per macroblock, scan line by scan
    /// line, each line's colour parts first and then its alpha parts, the order the stream
    /// holds them: `[line 0 colour mb 0..N, line 0 alpha mb 0..N, line 1 colour mb 0..N, ...]`.
    /// A macroblock's two lengths add up to its `bits`.
    fn write(&mut self, qs: &[u32]) -> Option<Vec<(Vec<u8>, u32)>>;
    /// The rate control (the quantiser of every macroblock for a frame of `budget` bits, given
    /// the previous frame's uniform quantiser as `hint`), run where the bits are: one value in
    /// `1..=2047` per macroblock. `None`: the encoder runs its search over `bits`. An answer is
    /// used only when it is in range, the frame then fits `budget`, and it uses at most 16
    /// distinct values (each distinct value costs the encoder one `bits` call to check the
    /// fit; the CPU algorithm and its GPU port use at most 2). Any other answer is ignored,
    /// however well it would fit, and the encoder runs its search.
    fn choose(&mut self, hint: u32, budget: u64) -> Option<Vec<u32>> {
        let _ = (hint, budget);
        None
    }
}

/// The most distinct quantisers a backend's `choose` answer may use (each costs a `bits` call).
const MAX_CHOSEN_LEVELS: usize = 16;

/// `bits(q)` of the backend, remembered in `measured` and checked for one entry per macroblock.
fn measure(backend: &mut dyn MacroblockBackend, measured: &mut Vec<(u32, Vec<u32>)>, q: u32, nmb: usize) -> Option<Vec<u32>> {
    if let Some((_, v)) = measured.iter().find(|(mq, _)| *mq == q) {
        return Some(v.clone());
    }
    let v = backend.bits(q)?;
    if v.len() != nmb {
        return None;
    }
    measured.push((q, v.clone()));
    Some(v)
}

/// Every macroblock's bits at its own quantiser.
fn bits_at_qs(backend: &mut dyn MacroblockBackend, measured: &mut Vec<(u32, Vec<u32>)>, qs: &[u32]) -> Option<Vec<u32>> {
    let mut levels: Vec<u32> = qs.to_vec();
    levels.sort_unstable();
    levels.dedup();
    for &q in &levels {
        measure(backend, measured, q, qs.len())?;
    }
    qs.iter().enumerate().map(|(i, q)| measured.iter().find(|(mq, _)| mq == q).and_then(|(_, v)| v.get(i).copied())).collect()
}

/// The quantisers of a frame from the backend: its own rate control when it has one and its
/// answer is in range and fits the budget, else the encoder's over the bits the backend
/// measures (kept in `measured`). With the uniform quantiser the frame was coded at.
fn backend_quantisers(
    backend: &mut dyn MacroblockBackend,
    hint: u32,
    nw: usize,
    ns: usize,
    budget_bits: u64,
    measured: &mut Vec<(u32, Vec<u32>)>,
) -> Option<(Vec<u32>, u32)> {
    let nmb = nw * ns;
    if let Some(qs) = backend.choose(hint, budget_bits)
        && qs.len() == nmb
        && qs.iter().all(|q| (1..=2047).contains(q))
        && let Some(want) =
            (qs.iter().collect::<std::collections::BTreeSet<_>>().len() <= MAX_CHOSEN_LEVELS).then(|| bits_at_qs(backend, measured, &qs)).flatten()
        && padded_frame_bits(&want, nw) <= budget_bits
    {
        let q = qs.iter().copied().max()?;
        return Some((qs, q));
    }
    rate_control(hint, nw, ns, budget_bits, &mut |q| measure(backend, measured, q, nmb))
}

/// What a backend's macroblock bit strings must look like to be stitched into scan lines.
struct Stitch {
    nw: usize,
    ns: usize,
    /// Entries per macroblock in the backend's answer: 1, or 2 with alpha.
    parts: usize,
    dct_alpha: bool,
    ycbcr444: bool,
}

impl Stitch {
    /// The scan lines from the backend's `written` strings, or `None` if anything disagrees with
    /// the measured `want` bits or the quantisers `qs`.
    fn rows(&self, written: &[(Vec<u8>, u32)], qs: &[u32], want: &[u32]) -> Option<Vec<Vec<u8>>> {
        let nmb = self.nw * self.ns;
        if written.len() != nmb * self.parts || qs.len() != nmb || want.len() != nmb {
            return None;
        }
        for s in 0..self.ns {
            for mb in 0..self.nw {
                let m = s * self.nw + mb;
                let (colour, alpha) = self.parts_of(written, s, mb)?;
                if !self.part_ok(colour, qs[m], Some(self.ycbcr444)) {
                    return None;
                }
                let mut bits = colour.1 as u64;
                if let Some(alpha) = alpha {
                    if !self.part_ok(alpha, qs[m], self.dct_alpha.then_some(false)) {
                        return None;
                    }
                    bits += alpha.1 as u64;
                }
                if bits != *want.get(m)? as u64 {
                    return None;
                }
            }
        }
        let row = |s: usize| -> Vec<u8> {
            let mut w = Writer::new();
            for part in 0..self.parts {
                for mb in 0..self.nw {
                    let entry = written.get((s * self.parts + part) * self.nw + mb);
                    if let Some((bytes, bits)) = entry {
                        put_bits(&mut w, bytes, *bits);
                    }
                }
            }
            w.finish_aligned(4)
        };
        Some(par_rows(self.ns, row))
    }

    /// The colour entry and, with alpha, the alpha entry of macroblock `mb` of line `s`.
    fn parts_of<'a>(&self, written: &'a [(Vec<u8>, u32)], s: usize, mb: usize) -> Option<(&'a (Vec<u8>, u32), Option<&'a (Vec<u8>, u32)>)> {
        let line = s * self.parts * self.nw;
        let colour = written.get(line + mb)?;
        let alpha = if self.parts == 2 { Some(written.get(line + self.nw + mb)?) } else { None };
        Some((colour, alpha))
    }

    /// A part has as many bytes as its bits need; when `flag` is set it starts with the
    /// macroblock header (11-bit qsf `q`, then that flag).
    fn part_ok(&self, (bytes, bits): &(Vec<u8>, u32), q: u32, flag: Option<bool>) -> bool {
        if bytes.len() as u64 != (*bits as u64).div_ceil(8) {
            return false;
        }
        let Some(flag) = flag else { return true };
        let (Some(&b0), Some(&b1)) = (bytes.first(), bytes.get(1)) else { return false };
        let head = (u32::from(b0) << 4) | u32::from(b1 >> 4);
        *bits >= 12 && head >> 1 == q && head & 1 == flag as u32
    }
}

/// Append the first `bits` bits of `bytes` (MSB first).
fn put_bits(w: &mut Writer, bytes: &[u8], bits: u32) {
    let full = (bits / 8) as usize;
    for &b in bytes.iter().take(full) {
        w.put(b as u32, 8);
    }
    let rem = bits % 8;
    if rem > 0
        && let Some(&b) = bytes.get(full)
    {
        w.put((b >> (8 - rem)) as u32, rem);
    }
}

/// `f(0..n)` in order, on all cores with the `threads` feature.
fn par_rows<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync + Send) -> Vec<T> {
    #[cfg(feature = "threads")]
    {
        use rayon::prelude::*;
        (0..n).into_par_iter().map(f).collect()
    }
    #[cfg(not(feature = "threads"))]
    {
        (0..n).map(f).collect()
    }
}

/// The blocks of a colour macroblock in coded order: (plane, x, y) inside the macroblock.
fn mb_layout(chroma: ChromaFormat) -> &'static [(u8, u8, u8)] {
    match chroma {
        ChromaFormat::Yuv444 => {
            &[(0, 0, 0), (0, 8, 0), (1, 0, 0), (1, 8, 0), (2, 0, 0), (2, 8, 0), (0, 0, 8), (0, 8, 8), (1, 0, 8), (1, 8, 8), (2, 0, 8), (2, 8, 8)]
        }
        _ => &[(0, 0, 0), (0, 8, 0), (1, 0, 0), (2, 0, 0), (0, 0, 8), (0, 8, 8), (1, 0, 8), (2, 0, 8)],
    }
}

/// Bits of every scan line (macroblock bits summed, padded to 4 bytes), added up.
fn padded_frame_bits(mb_bits: &[u32], nw: usize) -> u64 {
    mb_bits.chunks(nw.max(1)).map(|row| row_bytes(row.iter().map(|&b| b as u64).sum()) * 8).sum()
}

/// Smallest uniform quantiser whose frame fits `budget_bits` (bits are non-increasing in `q`),
/// searched from the previous frame's `hint`.
fn search_q(hint: u32, fits: &mut dyn FnMut(u32) -> Option<bool>) -> Option<u32> {
    let (mut lo, mut hi) = (1u32, 2047u32);
    if fits(1)? {
        return Some(1);
    }
    // bracket around the previous frame's choice first
    let g = hint.clamp(2, 2047);
    if fits(g)? {
        hi = g;
        let g2 = (g / 2).max(1);
        if g2 > 1 && !fits(g2)? {
            lo = g2;
        }
    } else {
        lo = g;
    }
    while hi - lo > 1 {
        let m = (lo + hi) / 2;
        if fits(m)? { hi = m } else { lo = m }
    }
    Some(hi)
}

/// Per-macroblock qsf values whose coded frame fits `budget_bits`, and the uniform quantiser
/// they refine: `mb_bits_at(q)` gives every macroblock's bits at `q` (`None` or a wrong count:
/// no answer).
fn rate_control(hint: u32, nw: usize, ns: usize, budget_bits: u64, mb_bits_at: &mut dyn FnMut(u32) -> Option<Vec<u32>>) -> Option<(Vec<u32>, u32)> {
    let nmb = nw * ns;
    let mut at = |q: u32| mb_bits_at(q).filter(|v| v.len() == nmb);
    let q = search_q(hint, &mut |q| Some(padded_frame_bits(&at(q)?, nw) <= budget_bits))?;
    let mut qs = vec![q; nmb];
    if q > 1 {
        // refine: one finer step per macroblock while the budget allows
        let cur = at(q)?;
        let finer = at(q - 1)?;
        let mut row_bits: Vec<u64> = cur.chunks(nw).map(|r| r.iter().map(|&b| b as u64).sum()).collect();
        let mut total: u64 = row_bits.iter().map(|&b| row_bytes(b) * 8).sum();
        // spread the refinement evenly over the frame: visit macroblocks in a strided order
        let stride = 7919usize;
        let mut idx = 0usize;
        for _ in 0..nmb {
            idx = (idx + stride) % nmb;
            let s = idx / nw;
            let extra = *finer.get(idx)? as u64 - (*cur.get(idx)?).min(*finer.get(idx)?) as u64;
            let nb = row_bits[s] + extra;
            let nt = total - row_bytes(row_bits[s]) * 8 + row_bytes(nb) * 8;
            if nt <= budget_bits {
                row_bits[s] = nb;
                total = nt;
                qs[idx] = q - 1;
            }
        }
    }
    Some((qs, q))
}

/// The encoder's rate control (`rate_control`) over `mb_bits_at`, for tests of a backend's
/// `choose` in other crates: its answer must be this one over the same measurements.
#[doc(hidden)]
pub fn rate_control_for_tests(
    hint: u32,
    nw: usize,
    ns: usize,
    budget_bits: u64,
    mb_bits_at: &mut dyn FnMut(u32) -> Option<Vec<u32>>,
) -> Option<(Vec<u32>, u32)> {
    rate_control(hint, nw, ns, budget_bits, mb_bits_at)
}

/// The entropy coding of macroblocks: bits and bit strings from the transformed blocks.
struct Coder<'a> {
    prep: &'a Prepared,
    vlc: &'static VlcSet,
    p_bits: u32,
    max_amp: u32,
    layout: &'static [(u8, u8, u8)],
    ycbcr444: bool,
}

impl<'a> Coder<'a> {
    fn new(cfg: &EncoderConfig, info: &'static CidInfo, prep: &'a Prepared, rgb: bool) -> Coder<'a> {
        let p_bits = if cfg.bit_depth == 8 { 4 } else { 6 };
        Coder {
            prep,
            vlc: vlc(info.vlc),
            p_bits,
            max_amp: if p_bits == 4 { 1024 } else { 4096 },
            layout: mb_layout(cfg.chroma()),
            ycbcr444: cfg.chroma() == ChromaFormat::Yuv444 && !rgb,
        }
    }

    /// Coded bits of macroblock `m` (colour and alpha) at quantiser `q`.
    fn mb_bits(&self, m: usize, q: u32) -> u32 {
        let prep = self.prep;
        // macroblock headers: colour, and DCT-coded alpha's own
        let headers = if prep.nblk > prep.ncol { 24 } else { 12 };
        let mut bits = headers + prep.dc_bits.get(m).copied().unwrap_or(0);
        for k in 0..prep.nblk {
            if let Some(t) = prep.t.get(m * prep.nblk + k) {
                bits += ac_bits(t, q as f32, self.vlc, self.p_bits, self.max_amp);
            }
        }
        bits
    }

    /// Every macroblock's bits at `q`, raster order.
    fn all_bits(&self, q: u32) -> Vec<u32> {
        let rows = par_rows(self.prep.ns, |s| (0..self.prep.nw).map(|mb| self.mb_bits(s * self.prep.nw + mb, q)).collect::<Vec<u32>>());
        rows.into_iter().flatten().collect()
    }

    /// One block: DC difference to the line's prediction for its plane, then the AC run/levels.
    fn put_block(&self, w: &mut Writer, m: usize, k: usize, pl: u8, q: u32, pred: &mut [i32; 4]) {
        let Some(t) = self.prep.t.get(m * self.prep.nblk + k) else { return };
        let dc = t[0] as i32;
        let e = dc - pred[pl as usize & 3];
        pred[pl as usize & 3] = dc;
        let eta = dc_eta(e);
        if let Some(&(c, l)) = self.vlc.dc_code.get(eta as usize) {
            w.put(c as u32, l as u32);
        }
        if eta > 0 {
            let rho = if e > 0 { e } else { e + (1 << eta) - 1 };
            w.put(rho as u32, eta);
        }
        write_ac(w, t, q as f32, self.vlc, self.p_bits, self.max_amp);
    }

    /// The colour part of macroblock `m`: header and colour blocks.
    fn put_colour(&self, w: &mut Writer, m: usize, q: u32, pred: &mut [i32; 4]) {
        w.put(q, 11);
        w.put(self.ycbcr444 as u32, 1);
        for (k, &(pl, _, _)) in self.layout.iter().enumerate() {
            self.put_block(w, m, k, pl, q, pred);
        }
    }

    /// The alpha part of macroblock `m`: DCT alpha's own header and blocks, or its lossless codes.
    fn put_alpha(&self, w: &mut Writer, m: usize, q: u32, pred: &mut [i32; 4]) {
        let prep = self.prep;
        if prep.nblk > prep.ncol {
            w.put(q, 11);
            w.put(0, 1);
            for k in prep.ncol..prep.nblk {
                self.put_block(w, m, k, 3, q, pred);
            }
        }
        for &(v, n) in prep.rle.get(m).map_or(&[][..], |r| r.as_slice()) {
            w.put(v, n);
        }
    }

    /// Macroblock scan line `s`: its colour macroblocks, then its alpha, padded to 4 bytes.
    fn write_row(&self, s: usize, qs: &[u32]) -> Vec<u8> {
        let prep = self.prep;
        let mut w = Writer::new();
        let mut pred = [0i32; 4];
        let q_of = |m: usize| qs.get(m).copied().unwrap_or(1);
        for mb in 0..prep.nw {
            let m = s * prep.nw + mb;
            self.put_colour(&mut w, m, q_of(m), &mut pred);
        }
        // the line's alpha after its colour
        for mb in 0..prep.nw {
            let m = s * prep.nw + mb;
            self.put_alpha(&mut w, m, q_of(m), &mut pred);
        }
        w.finish_aligned(4)
    }
}

/// Lossless alpha of one macroblock (its 256 samples in serpentine order, §7.3.1.2 and §8.3; the
/// inverse of the decoder's `decode_rle_alpha`): the run-length coding, or the raw samples where
/// that is shorter. Runs take the predictor (previous sample, or the linear continuation of the
/// last two) that extends furthest.
fn rle_alpha(px: &[u32; 256], d: u32) -> Vec<(u32, u32)> {
    let mask = (1u32 << d) - 1;
    let rice = d / 2 - 2;
    let mut out = vec![(1, 4)];
    let (mut d1, mut d2) = (mask >> 1, mask >> 1);
    let mut count = 0;
    while count < 256 {
        let predict = |p: u32, d1: u32, d2: u32| if p == 1 { (2 * d1).wrapping_sub(d2) } else { d1 };
        let mut best = (0usize, 0u32, 0u32);
        for p in 0..2 {
            let diff = px[count].wrapping_sub(predict(p, d1, d2)) & mask;
            let (mut a1, mut a2) = (d1, d2);
            let mut n = 0;
            // (a run is at most 511 long: Elias gamma with up to 8 leading zeros)
            while count + n < 256 && n < 511 {
                let v = predict(p, a1, a2).wrapping_add(diff) & mask;
                if v != px[count + n] {
                    break;
                }
                (a2, a1) = (a1, v);
                n += 1;
            }
            if n > best.0 {
                best = (n, p, diff);
            }
        }
        let (n, p, diff) = best;
        out.push((p, 1));
        // Elias gamma: g zeros, then n in g + 1 bits
        let g = 31 - (n as u32).leading_zeros();
        out.push((1, g + 1));
        out.push((n as u32 - (1 << g), g));
        // the difference folded to small codes for small magnitudes, Rice-coded when shorter
        let dcw = if diff <= mask >> 1 { 2 * diff } else { 2 * (mask - diff) + 1 };
        let q = dcw >> rice;
        if q + 1 + rice < d {
            out.push((1, 1));
            out.push((((1u32 << q) - 1) << 1, q + 1));
            out.push((dcw & ((1 << rice) - 1), rice));
        } else {
            out.push((0, 1));
            out.push((dcw, d));
        }
        (d1, d2) = if n >= 2 { (px[count + n - 1], px[count + n - 2]) } else { (px[count], d1) };
        count += n;
    }
    if out.iter().map(|t| t.1).sum::<u32>() > 4 + 256 * d {
        return std::iter::once((0, 4)).chain(px.iter().map(|&v| (v, d))).collect();
    }
    out
}

#[inline]
fn row_bytes(bits: u64) -> u64 {
    bits.div_ceil(32) * 4
}

#[inline]
fn dc_eta(e: i32) -> u32 {
    if e == 0 { 0 } else { 32 - e.unsigned_abs().leading_zeros() }
}

/// Quantised amplitude (≥ 1) of a pre-divided magnitude `|t| ≥ q`: the nearest reconstruction
/// level for decoders that reconstruct at `(A + 3/4)·step` (ffmpeg and ours by default).
#[inline(always)]
fn quant(t: f32, q: f32, max_amp: u32) -> u32 {
    ((t.abs() / q - 0.25) as u32).clamp(1, max_amp)
}

/// Bits of a block's AC coefficients plus EOB at quantiser `q`.
#[inline]
fn ac_bits(t: &[f32; 64], q: f32, v: &VlcSet, p_bits: u32, max_amp: u32) -> u32 {
    let mut bits = v.eob.1 as u32;
    let mut run = 0usize;
    for &x in &t[1..] {
        if x.abs() < q {
            run += 1;
            continue;
        }
        let a = quant(x, q, max_amp);
        let p = (a - 1) >> 6;
        let base = ((a - 1) & 63) as usize;
        let frun = (run > 0) as usize;
        let findex = (p > 0) as usize;
        bits += v.amp_code[frun][findex][base].1 as u32 + 1;
        if findex == 1 {
            bits += p_bits;
        }
        if frun == 1 {
            bits += v.run_code[run].1 as u32;
        }
        run = 0;
    }
    bits
}

fn write_ac(w: &mut Writer, t: &[f32; 64], q: f32, v: &VlcSet, p_bits: u32, max_amp: u32) {
    let mut run = 0usize;
    for &x in &t[1..] {
        if x.abs() < q {
            run += 1;
            continue;
        }
        let a = quant(x, q, max_amp);
        let p = (a - 1) >> 6;
        let base = ((a - 1) & 63) as usize;
        let frun = (run > 0) as usize;
        let findex = (p > 0) as usize;
        let (c, l) = v.amp_code[frun][findex][base];
        w.put(c as u32, l as u32);
        w.put((x < 0.0) as u32, 1);
        if findex == 1 {
            w.put(p, p_bits);
        }
        if frun == 1 {
            let (c, l) = v.run_code[run];
            w.put(c as u32, l as u32);
        }
        run = 0;
    }
    w.put(v.eob.0 as u32, v.eob.1 as u32);
}

#[cfg(test)]
mod tests {
    use super::rle_alpha;

    fn bits(t: &[(u32, u32)]) -> u32 {
        t.iter().map(|t| t.1).sum()
    }

    #[test]
    fn lossless_alpha_picks_runs_ramps_or_raw_samples() {
        // opaque: one run from the mid-grey start value
        let flat = rle_alpha(&[1023; 256], 10);
        assert_eq!(flat[0], (1, 4));
        assert!(bits(&flat) < 80, "{flat:?}");
        // a ramp along the serpentine: the linear predictor carries it
        let mut ramp = [0u32; 256];
        for (i, v) in ramp.iter_mut().enumerate() {
            *v = (100 + 3 * i as u32) & 1023;
        }
        let r = rle_alpha(&ramp, 10);
        assert!(r.iter().skip(1).any(|&t| t == (1, 1)) && bits(&r) < 80, "{r:?}");
        // noise: raw samples
        let mut noise = [0u32; 256];
        let mut x = 0x2545_f491u32;
        for v in &mut noise {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *v = x >> 24;
        }
        let n = rle_alpha(&noise, 8);
        assert_eq!((n[0], n.len(), bits(&n)), ((0, 4), 257, 4 + 256 * 8));
    }
}

#[cfg(test)]
mod backend_tests {
    use super::*;
    use crate::probe;

    const PROFILES: [Profile; 5] = [Profile::Lb, Profile::Sq, Profile::Hq, Profile::Hqx, Profile::R444];

    /// Busy planes (gradient, checkerboard, xorshift noise) at the coded depth; an alpha ramp
    /// with a noisy corner when alpha is coded.
    fn test_frame(cfg: &EncoderConfig, seed: u32) -> Frame {
        let (w, h) = (cfg.width as usize, cfg.height as usize);
        let mut f = Frame::new(cfg.width, cfg.height, cfg.chroma(), cfg.bit_depth, cfg.alpha.is_some());
        let mut s = seed.max(1);
        let mut noise = move || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        let max = (1u32 << cfg.bit_depth) - 1;
        let sh = cfg.bit_depth as u32 - 8;
        let cw = f.chroma_width() as usize;
        for y in 0..h {
            for x in 0..w {
                let check = if (x / 4 + y / 4) % 2 == 0 { 15 } else { 0 };
                f.y[y * w + x] = ((((16 + (x * 3 + y * 2) % 150 + check) as u32) << sh) + noise() % (10 << sh)).min(max) as u16;
                if let Some(a) = f.alpha.as_mut() {
                    let v = (x * max as usize) / w.max(1);
                    a[y * w + x] = if x > w * 3 / 4 && y > h * 3 / 4 { (noise() & max) as u16 } else { v as u16 };
                }
            }
            for x in 0..cw {
                f.cb[y * cw + x] = (((100 + (x + y) % 50) << sh) as u32 + noise() % (8 << sh)).min(max) as u16;
                f.cr[y * cw + x] = (((120 + (x * 5 + y) % 30) << sh) as u32 + noise() % (8 << sh)).min(max) as u16;
            }
        }
        f
    }

    /// The configurations under test: every profile at the depths it allows, with and without alpha.
    fn configs(w: u32, h: u32) -> Vec<EncoderConfig> {
        let mut v = Vec::new();
        for profile in PROFILES {
            let depths: &[u8] = match profile {
                Profile::Hqx | Profile::R444 => &[10, 12],
                _ => &[8],
            };
            for &bit_depth in depths {
                for alpha in [None, Some(AlphaCoding::Dct), Some(AlphaCoding::Lossless)] {
                    v.push(EncoderConfig { bit_depth, alpha, ..EncoderConfig::new(profile, w, h) });
                }
            }
        }
        v
    }

    /// How a test double misbehaves.
    #[derive(Default, Clone, Copy, PartialEq)]
    enum Lie {
        #[default]
        Honest,
        /// `write` answers with one more byte (and bit count) than `bits` measured, in this entry.
        ExtraByte(usize),
        /// `write`'s byte count does not fit its bit count.
        ShortBytes,
        /// The colour part of macroblock 1 starts with another quantiser.
        WrongQsf,
        /// Right lengths, nonsense bytes.
        Garbage,
        /// One entry missing.
        MissingEntry,
        /// `bits` answers one entry short.
        ShortBits,
        /// `bits` fails.
        FailBits,
        /// `write` fails.
        FailWrite,
        /// `load` declines.
        Decline,
    }

    #[derive(Default, Clone)]
    enum Choose {
        #[default]
        Default,
        /// The CPU algorithm's own answer over this backend's bits.
        Cpu,
        Fixed(Vec<u32>),
    }

    /// The crate's own CPU functions behind the backend trait.
    #[derive(Default)]
    struct CpuMbBackend {
        enc: Option<Encoder>,
        prep: Option<Prepared>,
        rgb: bool,
        lie: Lie,
        choose: Choose,
        choose_calls: usize,
        bits_calls: usize,
    }

    impl CpuMbBackend {
        fn lying(lie: Lie) -> Self {
            CpuMbBackend { lie, ..Default::default() }
        }
        fn with_choose(choose: Choose) -> Self {
            CpuMbBackend { choose, ..Default::default() }
        }
    }

    impl MacroblockBackend for CpuMbBackend {
        fn load(&mut self, cfg: &EncoderConfig, frame: &Frame) -> bool {
            if self.lie == Lie::Decline {
                return false;
            }
            let Ok(enc) = Encoder::with_config(cfg.clone()) else { return false };
            self.prep = Some(enc.prepare(frame));
            self.enc = Some(enc);
            self.rgb = frame.rgb;
            true
        }
        fn bits(&mut self, q: u32) -> Option<Vec<u32>> {
            self.bits_calls += 1;
            if self.lie == Lie::FailBits {
                return None;
            }
            let (enc, prep) = (self.enc.as_ref()?, self.prep.as_ref()?);
            let mut v = Coder::new(&enc.cfg, enc.info, prep, self.rgb).all_bits(q);
            if self.lie == Lie::ShortBits {
                v.pop();
            }
            Some(v)
        }
        fn write(&mut self, qs: &[u32]) -> Option<Vec<(Vec<u8>, u32)>> {
            if self.lie == Lie::FailWrite {
                return None;
            }
            let (enc, prep) = (self.enc.as_ref()?, self.prep.as_ref()?);
            let coder = Coder::new(&enc.cfg, enc.info, prep, self.rgb);
            let finish = |w: Writer| w.finish_bits();
            let mut out = Vec::new();
            for s in 0..prep.ns {
                let mut pred = [0i32; 4];
                for mb in 0..prep.nw {
                    let m = s * prep.nw + mb;
                    let mut w = Writer::new();
                    coder.put_colour(&mut w, m, *qs.get(m)?, &mut pred);
                    out.push(finish(w));
                }
                if enc.cfg.alpha.is_some() {
                    for mb in 0..prep.nw {
                        let m = s * prep.nw + mb;
                        let mut w = Writer::new();
                        coder.put_alpha(&mut w, m, *qs.get(m)?, &mut pred);
                        out.push(finish(w));
                    }
                }
            }
            match self.lie {
                Lie::ExtraByte(i) => {
                    let e = out.get_mut(i)?;
                    e.0.push(0);
                    e.1 += 8;
                }
                Lie::ShortBytes => out.get_mut(1)?.0.pop().map(|_| ())?,
                Lie::WrongQsf => {
                    let e = out.get_mut(1)?;
                    e.0[0] ^= 0x10;
                }
                Lie::MissingEntry => {
                    out.pop();
                }
                Lie::Garbage => {
                    for (bytes, _) in &mut out {
                        for (i, b) in bytes.iter_mut().enumerate().skip(2) {
                            *b = (i as u8).wrapping_mul(97) ^ 0x5a;
                        }
                    }
                }
                _ => {}
            }
            Some(out)
        }
        fn choose(&mut self, hint: u32, budget: u64) -> Option<Vec<u32>> {
            self.choose_calls += 1;
            match self.choose.clone() {
                Choose::Default => None,
                Choose::Fixed(qs) => Some(qs),
                Choose::Cpu => {
                    let (enc, prep) = (self.enc.as_ref()?, self.prep.as_ref()?);
                    let coder = Coder::new(&enc.cfg, enc.info, prep, self.rgb);
                    rate_control(hint, prep.nw, prep.ns, budget, &mut |q| Some(coder.all_bits(q))).map(|(qs, _)| qs)
                }
            }
        }
    }

    fn cpu_frame(cfg: &EncoderConfig, f: &Frame) -> Vec<u8> {
        Encoder::with_config(cfg.clone()).unwrap().encode(f).unwrap()
    }

    fn label(cfg: &EncoderConfig, w: u32, h: u32) -> String {
        format!("{:?} {}-bit {:?} {w}x{h}", cfg.profile, cfg.bit_depth, cfg.alpha)
    }

    #[test]
    fn a_backend_doing_the_cpus_work_gives_the_cpus_frame() {
        let (mut checked, mut refused) = (0, 0);
        for (w, h) in [(16, 16), (18, 13), (200, 120), (1920, 1080), (3840, 2160)] {
            for cfg in configs(w, h) {
                // the large sizes with the busiest combinations only, to keep the run short
                if w > 1920 && (cfg.alpha.is_some() || cfg.bit_depth == 12) {
                    continue;
                }
                if w == 1920 && cfg.alpha == Some(AlphaCoding::Lossless) {
                    continue;
                }
                let f = test_frame(&cfg, 7);
                let mut b = CpuMbBackend::default();
                let got = Encoder::with_config(cfg.clone()).unwrap().encode_with(&f, Some(&mut b));
                match (Encoder::with_config(cfg.clone()).unwrap().encode(&f), got) {
                    (Ok(cpu), Ok((out, used))) => {
                        assert!(used, "{}", label(&cfg, w, h));
                        assert!(out == cpu, "{}", label(&cfg, w, h));
                        checked += 1;
                    }
                    // lossless alpha is not rate controlled and can overflow: both refuse
                    (Err(_), Err(_)) => refused += 1,
                    (c, g) => panic!("{}: cpu {:?}, backend {:?}", label(&cfg, w, h), c.map(|v| v.len()), g.map(|v| v.0.len())),
                }
            }
        }
        assert!(checked > 60 && refused <= checked / 4, "{checked} compared, {refused} refused");
    }

    #[test]
    fn odd_sizes_and_444_widths_go_through_the_backend() {
        // 4:4:4 takes any width; 4:2:2 an even one
        for (w, h) in [(1, 1), (17, 13)] {
            let cfg = EncoderConfig::new(Profile::R444, w, h);
            for alpha in [None, Some(AlphaCoding::Dct), Some(AlphaCoding::Lossless)] {
                let cfg = EncoderConfig { alpha, ..cfg.clone() };
                let f = test_frame(&cfg, 3);
                let cpu = cpu_frame(&cfg, &f);
                let mut b = CpuMbBackend::default();
                let (out, used) = Encoder::with_config(cfg.clone()).unwrap().encode_with(&f, Some(&mut b)).unwrap();
                assert!(used && out == cpu, "{}", label(&cfg, w, h));
            }
        }
    }

    #[test]
    fn frames_keep_their_rgb_signalling_through_the_backend() {
        let cfg = EncoderConfig::new(Profile::R444, 48, 32);
        let mut f = test_frame(&cfg, 9);
        f.rgb = true;
        let cpu = cpu_frame(&cfg, &f);
        let mut b = CpuMbBackend::default();
        let (out, used) = Encoder::with_config(cfg).unwrap().encode_with(&f, Some(&mut b)).unwrap();
        assert!(used && out == cpu);
    }

    #[test]
    fn a_backend_that_declines_or_fails_leaves_the_frame_to_the_cpu() {
        for cfg in [EncoderConfig::new(Profile::Hq, 64, 48), EncoderConfig { alpha: Some(AlphaCoding::Dct), ..EncoderConfig::new(Profile::R444, 64, 48) }] {
            let f = test_frame(&cfg, 3);
            let cpu = cpu_frame(&cfg, &f);
            for (name, lie) in [("declining", Lie::Decline), ("fails bits", Lie::FailBits), ("fails write", Lie::FailWrite), ("short bits", Lie::ShortBits)] {
                let mut b = CpuMbBackend::lying(lie);
                let mut enc = Encoder::with_config(cfg.clone()).unwrap();
                let (out, used) = enc.encode_with(&f, Some(&mut b)).unwrap();
                assert!(!used, "{} {name}", label(&cfg, 64, 48));
                assert!(out == cpu, "{} {name}", label(&cfg, 64, 48));
                // and the next frame is coded as if nothing had happened
                let (again, _) = enc.encode_with(&f, None).unwrap();
                assert!(again == cpu, "{name}: next frame");
            }
        }
    }

    #[test]
    fn a_backend_whose_bytes_do_not_match_its_bits_is_not_trusted() {
        for cfg in [
            EncoderConfig::new(Profile::Hq, 128, 64),
            EncoderConfig { alpha: Some(AlphaCoding::Dct), ..EncoderConfig::new(Profile::R444, 128, 64) },
            EncoderConfig { alpha: Some(AlphaCoding::Lossless), ..EncoderConfig::new(Profile::Hqx, 128, 64) },
        ] {
            let f = test_frame(&cfg, 5);
            let cpu = cpu_frame(&cfg, &f);
            let parts = if cfg.alpha.is_some() { 2 } else { 1 };
            let nmb = 8 * 4;
            let lies = [
                ("one colour byte too many", Lie::ExtraByte(3)),
                ("one last-entry byte too many", Lie::ExtraByte(nmb * parts - 1)),
                ("bytes short of the bits", Lie::ShortBytes),
                ("wrong qsf in a header", Lie::WrongQsf),
                ("entry missing", Lie::MissingEntry),
            ];
            for (name, lie) in lies {
                let mut b = CpuMbBackend::lying(lie);
                let (out, used) = Encoder::with_config(cfg.clone()).unwrap().encode_with(&f, Some(&mut b)).unwrap();
                assert!(!used, "{} {name}", label(&cfg, 128, 64));
                assert!(out == cpu, "{} {name}", label(&cfg, 128, 64));
            }
            // Garbage of the right lengths (and headers) is the backend's own frame: it must
            // still parse and decode without a panic; only the shader tests can judge its content.
            let mut b = CpuMbBackend::lying(Lie::Garbage);
            let (out, _) = Encoder::with_config(cfg.clone()).unwrap().encode_with(&f, Some(&mut b)).unwrap();
            assert!(crate::frame_layout(&out).is_ok() && probe(&out).is_ok(), "{}: garbage frame parses", label(&cfg, 128, 64));
            let _ = crate::decode_frame(&out);
        }
    }

    #[test]
    fn a_valid_choose_answer_is_used() {
        for cfg in [EncoderConfig::new(Profile::Hq, 128, 64), EncoderConfig { alpha: Some(AlphaCoding::Dct), ..EncoderConfig::new(Profile::R444, 128, 64) }] {
            let f = test_frame(&cfg, 11);
            let cpu = cpu_frame(&cfg, &f);
            let mut b = CpuMbBackend::with_choose(Choose::Cpu);
            let (out, used) = Encoder::with_config(cfg.clone()).unwrap().encode_with(&f, Some(&mut b)).unwrap();
            assert_eq!(b.choose_calls, 1);
            assert!(used && out == cpu, "{}", label(&cfg, 128, 64));
            // no search ran: bits only at the (at most two) levels chosen
            assert!(b.bits_calls <= 2, "{} bits calls", b.bits_calls);
        }
    }

    #[test]
    fn a_choose_answer_that_does_not_fit_or_is_out_of_range_is_ignored() {
        let (w, h) = (128, 64);
        let cfg = EncoderConfig::new(Profile::Hq, w, h);
        let nmb = 8 * 4;
        let f = test_frame(&cfg, 13);
        let cpu = cpu_frame(&cfg, &f);
        // 32 distinct, fairly coarse levels: this answer fits the budget (checked below), so
        // only the cap on distinct levels can make the encoder ignore it
        let many: Vec<u32> = (0..nmb as u32).map(|i| 60 + i).collect();
        {
            let mut probe = CpuMbBackend::default();
            assert!(probe.load(&cfg, &f));
            let want = bits_at_qs(&mut probe, &mut Vec::new(), &many).unwrap();
            let budget = (Encoder::with_config(cfg.clone()).unwrap().frame_size() as u64 - header::header_size_for(h) as u64 - 4) * 8;
            assert!(padded_frame_bits(&want, 8) <= budget, "the many-levels answer must fit without the cap");
        }
        let cases = [
            ("q = 0", Choose::Fixed(vec![0; nmb])),
            ("q = 2048", Choose::Fixed(vec![2048; nmb])),
            ("one q = 0", Choose::Fixed((0..nmb).map(|i| if i == 5 { 0 } else { 40 }).collect())),
            ("too few", Choose::Fixed(vec![40; nmb - 1])),
            ("too many", Choose::Fixed(vec![40; nmb + 1])),
            ("empty", Choose::Fixed(Vec::new())),
            ("too fine for the budget", Choose::Fixed(vec![1; nmb])),
            ("too many distinct levels", Choose::Fixed(many)),
        ];
        for (name, choose) in cases {
            let mut b = CpuMbBackend::with_choose(choose);
            let (out, used) = Encoder::with_config(cfg.clone()).unwrap().encode_with(&f, Some(&mut b)).unwrap();
            assert_eq!(b.choose_calls, 1, "{name}");
            assert!(used, "{name}: the backend still codes it, over the CPU's rate control");
            assert!(out == cpu, "{name}");
        }
    }

    #[test]
    fn content_extremes_hold_through_the_backend() {
        // full-scale noise at LB, where the rate control hits its coarsest quantiser, and black
        for (profile, noise) in [(Profile::Lb, true), (Profile::Lb, false), (Profile::Hq, false)] {
            let cfg = EncoderConfig::new(profile, 96, 64);
            let mut f = Frame::new(96, 64, ChromaFormat::Yuv422, 8, false);
            let mut s = 12345u32;
            if noise {
                for v in f.y.iter_mut().chain(f.cb.iter_mut()).chain(f.cr.iter_mut()) {
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    *v = (s % 256) as u16;
                }
            }
            let cpu = Encoder::with_config(cfg.clone()).unwrap().encode(&f);
            let mut b = CpuMbBackend::default();
            let got = Encoder::with_config(cfg).unwrap().encode_with(&f, Some(&mut b));
            match (cpu, got) {
                (Ok(c), Ok((out, used))) => assert!(used && out == c, "{profile:?} noise={noise}"),
                (Err(_), Ok((_, used))) => assert!(!used, "an overflowing frame must not come out of the backend"),
                (c, g) => panic!("{profile:?} noise={noise}: cpu {:?} backend {:?}", c.map(|v| v.len()), g.map(|v| v.0.len())),
            }
        }
    }

    #[test]
    fn the_quantiser_hint_follows_a_backend_frame_like_a_cpu_frame() {
        let cfg = EncoderConfig::new(Profile::Sq, 128, 64);
        let (f1, f2) = (test_frame(&cfg, 1), test_frame(&cfg, 2));
        let mut cpu = Encoder::with_config(cfg.clone()).unwrap();
        let want = [cpu.encode(&f1).unwrap(), cpu.encode(&f2).unwrap()];
        let mut enc = Encoder::with_config(cfg).unwrap();
        let mut b = CpuMbBackend::default();
        let first = enc.encode_with(&f1, Some(&mut b)).unwrap().0;
        let second = enc.encode_with(&f2, None).unwrap().0;
        assert!(first == want[0] && second == want[1]);
        assert_eq!(enc.last_q, cpu.last_q);
    }

    #[test]
    fn stitching_strings_that_are_not_byte_aligned() {
        let mut w = Writer::new();
        put_bits(&mut w, &[0b1010_1100, 0b1110_0000], 11);
        put_bits(&mut w, &[0b1000_0000], 1);
        put_bits(&mut w, &[], 0);
        // 10101100 111 1 + padding to 4 bytes
        assert_eq!(w.finish_aligned(4), vec![0b1010_1100, 0b1111_0000, 0, 0]);
    }
}
