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
        let prep = self.prepare(f);
        let vlc = vlc(self.info.vlc);
        let budget_bits = (self.frame_size as u64 - self.header_size as u64 - 4) * 8;
        let q = self.rate_control(&prep, vlc, budget_bits);
        self.write(&prep, vlc, &q, f.rgb)
    }

    fn mb_layout(&self) -> &'static [(u8, u8, u8)] {
        match self.cfg.chroma() {
            ChromaFormat::Yuv444 => {
                &[(0, 0, 0), (0, 8, 0), (1, 0, 0), (1, 8, 0), (2, 0, 0), (2, 8, 0), (0, 0, 8), (0, 8, 8), (1, 0, 8), (1, 8, 8), (2, 0, 8), (2, 8, 8)]
            }
            _ => &[(0, 0, 0), (0, 8, 0), (1, 0, 0), (2, 0, 0), (0, 0, 8), (0, 8, 8), (1, 0, 8), (2, 0, 8)],
        }
    }

    fn prepare(&self, f: &Frame) -> Prepared {
        let cfg = &self.cfg;
        let nw = cfg.width.div_ceil(16) as usize;
        let ns = cfg.height.div_ceil(16) as usize;
        let ncol = self.mb_layout().len();
        // DCT-coded alpha: four more blocks per macroblock (plane 3, coded after the colour)
        let layout: Vec<(u8, u8, u8)> = if cfg.alpha == Some(AlphaCoding::Dct) {
            self.mb_layout().iter().chain(&crate::decode::LAYOUT_ALPHA).copied().collect()
        } else {
            self.mb_layout().to_vec()
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

    /// Per-macroblock qsf values whose coded frame fits `budget_bits`.
    fn rate_control(&mut self, prep: &Prepared, vlc: &VlcSet, budget_bits: u64) -> Vec<u32> {
        let p_bits = if self.cfg.bit_depth == 8 { 4 } else { 6 };
        let max_amp = if p_bits == 4 { 1024 } else { 4096 };
        let nmb = prep.nw * prep.ns;
        // macroblock headers: colour, and DCT-coded alpha's own
        let headers = if prep.nblk > prep.ncol { 24 } else { 12 };
        let mb_bits = |mb: usize, q: u32| -> u32 {
            let mut bits = headers + prep.dc_bits[mb];
            for k in 0..prep.nblk {
                bits += ac_bits(&prep.t[mb * prep.nblk + k], q as f32, vlc, p_bits, max_amp);
            }
            bits
        };
        let frame_bits = |q: u32| -> u64 {
            let row = |s: usize| -> u64 { row_bytes((0..prep.nw).map(|m| mb_bits(s * prep.nw + m, q) as u64).sum()) * 8 };
            #[cfg(feature = "threads")]
            {
                use rayon::prelude::*;
                (0..prep.ns).into_par_iter().map(row).sum()
            }
            #[cfg(not(feature = "threads"))]
            {
                (0..prep.ns).map(row).sum()
            }
        };
        // smallest feasible uniform q (bits are non-increasing in q)
        let (mut lo, mut hi) = (1u32, 2047u32);
        if frame_bits(1) <= budget_bits {
            hi = 1;
        } else {
            // bracket around the previous frame's choice first
            let g = self.last_q.clamp(2, 2047);
            if frame_bits(g) <= budget_bits {
                hi = g;
                let g2 = (g / 2).max(1);
                if g2 > 1 && frame_bits(g2) > budget_bits {
                    lo = g2;
                }
            } else {
                lo = g;
            }
            while hi - lo > 1 {
                let m = (lo + hi) / 2;
                if frame_bits(m) <= budget_bits { hi = m } else { lo = m }
            }
        }
        let q = hi;
        self.last_q = q;
        let mut qs = vec![q; nmb];
        if q > 1 {
            // refine: one finer step per macroblock while the budget allows
            let mut row_bits: Vec<u64> = (0..prep.ns).map(|s| (0..prep.nw).map(|m| mb_bits(s * prep.nw + m, q) as u64).sum()).collect();
            let mut total: u64 = row_bits.iter().map(|&b| row_bytes(b) * 8).sum();
            let cur: Vec<u32> = (0..nmb).map(|mb| mb_bits(mb, q)).collect();
            let finer: Vec<u32> = (0..nmb).map(|mb| mb_bits(mb, q - 1)).collect();
            // spread the refinement evenly over the frame: visit macroblocks in a strided order
            let stride = 7919usize;
            let mut idx = 0usize;
            for _ in 0..nmb {
                idx = (idx + stride) % nmb;
                let s = idx / prep.nw;
                let extra = finer[idx] as u64 - cur[idx].min(finer[idx]) as u64;
                let nb = row_bits[s] + extra;
                let nt = total - row_bytes(row_bits[s]) * 8 + row_bytes(nb) * 8;
                if nt <= budget_bits {
                    row_bits[s] = nb;
                    total = nt;
                    qs[idx] = q - 1;
                }
            }
        }
        qs
    }

    fn write(&self, prep: &Prepared, vlc: &VlcSet, qs: &[u32], rgb: bool) -> Result<Vec<u8>> {
        let cfg = &self.cfg;
        let p_bits = if cfg.bit_depth == 8 { 4 } else { 6 };
        let max_amp = if p_bits == 4 { 1024 } else { 4096 };
        let layout = self.mb_layout();
        // 4:4:4 signalling as ffmpeg writes and reads it (see `FrameHeader::rgb_planes`): RGB is
        // CLF = 0 with ACF = 0; Y'CbCr is CLF = 1 with ACF = 1 on every macroblock.
        let ycbcr444 = cfg.chroma() == ChromaFormat::Yuv444 && !rgb;
        let write_row = |s: usize| -> Vec<u8> {
            let mut w = Writer::new();
            let mut pred = [0i32; 4];
            let mut block = |w: &mut Writer, m: usize, k: usize, pl: u8, q: u32| {
                let t = &prep.t[m * prep.nblk + k];
                let dc = t[0] as i32;
                let e = dc - pred[pl as usize];
                pred[pl as usize] = dc;
                let eta = dc_eta(e);
                let (c, l) = vlc.dc_code[eta as usize];
                w.put(c as u32, l as u32);
                if eta > 0 {
                    let rho = if e > 0 { e } else { e + (1 << eta) - 1 };
                    w.put(rho as u32, eta);
                }
                write_ac(w, t, q as f32, vlc, p_bits, max_amp);
            };
            for mb in 0..prep.nw {
                let m = s * prep.nw + mb;
                let q = qs[m];
                w.put(q, 11);
                w.put(ycbcr444 as u32, 1);
                for (k, &(pl, _, _)) in layout.iter().enumerate() {
                    block(&mut w, m, k, pl, q);
                }
            }
            // the line's alpha after its colour
            for mb in 0..prep.nw {
                let m = s * prep.nw + mb;
                if prep.nblk > prep.ncol {
                    w.put(qs[m], 11);
                    w.put(0, 1);
                    for k in prep.ncol..prep.nblk {
                        block(&mut w, m, k, 3, qs[m]);
                    }
                }
                for &(v, n) in prep.rle.get(m).map_or(&[][..], |r| r.as_slice()) {
                    w.put(v, n);
                }
            }
            w.finish_aligned(4)
        };
        #[cfg(feature = "threads")]
        let rows: Vec<Vec<u8>> = {
            use rayon::prelude::*;
            (0..prep.ns).into_par_iter().map(write_row).collect()
        };
        #[cfg(not(feature = "threads"))]
        let rows: Vec<Vec<u8>> = (0..prep.ns).map(write_row).collect();

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
            mb_rows: prep.ns as u32,
        };
        let mut out = header::write(&h);
        out.reserve(self.frame_size as usize - out.len());
        let mut off = 0u32;
        for (s, r) in rows.iter().enumerate() {
            let o = SCAN_INDEX_OFFSET + 4 * s;
            out[o..o + 4].copy_from_slice(&off.to_be_bytes());
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
