//! ProRes frame encoding (progressive) with per-slice quantiser rate control.

use crate::bits::{Counter, Sink, Writer};
use crate::dct::fdct8x8;
use crate::header::{FRAME_ID, FrameHeader, PictureHeader, row_slice_widths};
use crate::tables::{DC_CB, DC_CTX_INIT, FIRST_DC_CB, LEVEL_CB, LEVEL_CTX_INIT, RUN_CB, RUN_CTX_INIT, SCAN_PROGRESSIVE, qscale};
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
}

/// Quantiser search range (RDD 36 allows 1..=224).
const Q_MIN: u8 = 1;
const Q_MAX: u8 = 224;

impl Encoder {
    pub fn new(profile: Profile, width: u32, height: u32) -> Encoder {
        Encoder { cfg: EncoderConfig::new(profile, width, height) }
    }

    pub fn with_config(cfg: EncoderConfig) -> Encoder {
        Encoder { cfg }
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

        let mb_width = w.div_ceil(16);
        let mb_height = h.div_ceil(16);
        let widths = row_slice_widths(mb_width, cfg.log2_slice_mbs);
        let mut jobs = Vec::with_capacity(widths.len() * mb_height);
        for row in 0..mb_height {
            let mut x = 0;
            for &sw in &widths {
                jobs.push((x, row, sw));
                x += sw;
            }
        }
        let src = Source {
            frame,
            w,
            h,
            cw,
            chroma444: chroma == ChromaFormat::Yuv444,
            scale: (2f32).powi(12 - frame.bit_depth as i32),
            alpha: with_alpha,
            bits_per_mb: self.bits_per_mb(),
        };
        let threads = cfg.threads;
        let prepared: Vec<SliceData> = par_map(threads, &jobs, |&(x, row, sw)| prepare_slice(&src, x, row, sw));
        let qs = choose_quantisers(&prepared, &src, mb_width * mb_height, threads);
        let items: Vec<(&SliceData, u8)> = prepared.iter().zip(qs).collect();
        let slices: Vec<Vec<u8>> = par_map(threads, &items, |&(sd, q)| write_slice(sd, q));

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
        for s in &slices {
            out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        }
        for s in &slices {
            out.extend_from_slice(s);
        }
        debug_assert_eq!(out.len(), total);
        Ok(out)
    }
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

/// Rounding offsets for quantisation: nearest for DC, a slight dead zone for AC.
const AC_ROUND: f32 = 0.45;

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

/// Coded DCT payload bits of a slice at quantiser `q` (components padded to bytes).
fn slice_bits(sd: &SliceData, q: u8) -> u64 {
    let mut lv = [Vec::new(), Vec::new(), Vec::new()];
    quantise_all(sd, q, &mut lv);
    lv.iter().map(|l| component_bits(l)).sum()
}

/// Frame-level rate control: the smallest uniform quantiser whose frame fits the budget, then
/// one step finer on as many slices as the remaining budget allows. Uniform quantisation
/// approximately minimises frame MSE for a given size.
fn choose_quantisers(slices: &[SliceData], src: &Source, total_mbs: usize, threads: bool) -> Vec<u8> {
    let n = slices.len();
    let hsize = slices.first().map_or(6, SliceData::header_size);
    let fixed = 8 * (8 + 20 + 8 + n * (2 + hsize)) as u64;
    let budget = (src.bits_per_mb as u64 * total_mbs as u64).saturating_sub(fixed);
    let bits_at = |q: u8| -> Vec<u64> { par_map(threads, slices, |sd| slice_bits(sd, q)) };
    let sum = |v: &[u64]| v.iter().sum::<u64>();

    let mut best = bits_at(Q_MIN);
    let mut qg = Q_MIN;
    if sum(&best) > budget {
        let (mut lo, mut hi) = (Q_MIN + 1, Q_MAX);
        let mut hi_bits = None;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let v = bits_at(mid);
            if sum(&v) <= budget {
                hi = mid;
                hi_bits = Some(v);
            } else {
                lo = mid + 1;
            }
        }
        qg = lo;
        best = hi_bits.unwrap_or_else(|| bits_at(qg));
    }
    let mut qs = vec![qg; n];
    let mut cur = best;
    let total = sum(&cur);
    if qg > Q_MIN && total < budget {
        let finer = bits_at(qg - 1);
        let mut left = budget - total;
        for i in 0..n {
            let d = finer[i].saturating_sub(cur[i]);
            if d <= left && finer[i] <= slices[i].limit_bits() {
                left -= d;
                qs[i] = qg - 1;
                cur[i] = finer[i];
            }
        }
    }
    // Keep every slice within its 16-bit size fields.
    for i in 0..n {
        while cur[i] > slices[i].limit_bits() && qs[i] < Q_MAX {
            qs[i] += 1;
            cur[i] = slice_bits(&slices[i], qs[i]);
        }
    }
    qs
}

fn write_slice(sd: &SliceData, q: u8) -> Vec<u8> {
    let mut lv = [Vec::new(), Vec::new(), Vec::new()];
    quantise_all(sd, q, &mut lv);
    let comps: Vec<Vec<u8>> = lv
        .iter()
        .map(|l| {
            let mut w = Writer::new();
            encode_component(&mut w, l, &SCAN_PROGRESSIVE);
            w.finish()
        })
        .collect();
    let hsize = sd.header_size();
    let alpha = sd.alpha.as_deref().unwrap_or(&[]);
    let mut out = Vec::with_capacity(hsize + comps.iter().map(Vec::len).sum::<usize>() + alpha.len());
    out.push((hsize as u8) << 3);
    out.push(q);
    out.extend_from_slice(&(comps[0].len() as u16).to_be_bytes());
    out.extend_from_slice(&(comps[1].len() as u16).to_be_bytes());
    if sd.alpha.is_some() {
        out.extend_from_slice(&(comps[2].len() as u16).to_be_bytes());
    }
    for c in &comps {
        out.extend_from_slice(c);
    }
    out.extend_from_slice(alpha);
    out
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
