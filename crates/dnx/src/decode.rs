//! VC-3 decoding (SMPTE ST 2019-1 §8).

use crate::bits::Reader;
use crate::dct::idct8x8;
use crate::header::{self, FieldCode, FrameHeader, SCAN_INDEX_OFFSET};
use crate::tables::{AMP_EOB, AMP_FINDEX, AMP_FRUN, CidInfo, VlcSet, ZIGZAG, cid_info, vlc};
use crate::{ChromaFormat, Error, Frame, Interlace, Result};

/// Decoder options.
#[derive(Debug, Clone, Copy)]
pub struct DecodeOptions {
    /// Decode macroblock scan lines in parallel (needs the `threads` feature).
    pub threads: bool,
    /// Use the standard's integer inverse quantisation (equation 8.1) instead of the
    /// ffmpeg-compatible reconstruction (default false; see the README).
    pub spec_dequant: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions { threads: true, spec_dequant: false }
    }
}

/// Parse the header of the first coding unit of a frame.
pub fn probe(data: &[u8]) -> Result<FrameHeader> {
    header::parse(data)
}

/// Decode one compressed frame (one or two coding units).
pub fn decode_frame(data: &[u8]) -> Result<Frame> {
    decode_frame_with(data, &DecodeOptions::default())
}

/// Decode one compressed frame with options.
pub fn decode_frame_with(data: &[u8], opts: &DecodeOptions) -> Result<Frame> {
    let h1 = header::parse(data)?;
    let field_coded = h1.interlaced && !h1.frame_encoding;
    if !field_coded {
        let planes = decode_unit(&h1, data, opts)?;
        let mut f = Frame::new_like(&h1, h1.width, h1.lines);
        f.interlace = if h1.interlaced { Interlace::TopFieldFirst } else { Interlace::Progressive };
        planes.copy_into(&mut f, 0, 1);
        return Ok(f);
    }
    // field encoding: two coding units, field 1 (top) then field 2
    let off = second_unit_offset(&h1, data)?;
    let h2 = header::parse(&data[off..])?;
    if h2.cid != h1.cid || h2.width != h1.width || h2.lines != h1.lines || h2.chroma != h1.chroma || h2.alpha != h1.alpha {
        return Err(Error::Invalid("field coding units differ"));
    }
    let (p1, p2) = if opts.threads {
        join(|| decode_unit(&h1, &data[..off], opts), || decode_unit(&h2, &data[off..], opts))
    } else {
        (decode_unit(&h1, &data[..off], opts), decode_unit(&h2, &data[off..], opts))
    };
    let (top, bottom) = if h1.field == FieldCode::Field2 { (p2?, p1?) } else { (p1?, p2?) };
    // Both field payloads must be valid before committing the full-frame
    // allocation; a damaged header alone must not allocate an output picture.
    let mut f = Frame::new_like(&h1, h1.width, h1.lines * 2);
    f.interlace = Interlace::TopFieldFirst;
    top.copy_into(&mut f, 0, 2);
    bottom.copy_into(&mut f, 1, 2);
    Ok(f)
}

#[cfg(feature = "threads")]
fn join<A: Send, B: Send>(a: impl FnOnce() -> A + Send, b: impl FnOnce() -> B + Send) -> (A, B) {
    rayon::join(a, b)
}

#[cfg(not(feature = "threads"))]
fn join<A, B>(a: impl FnOnce() -> A, b: impl FnOnce() -> B) -> (A, B) {
    (a(), b())
}

/// Byte offset of the second coding unit of a field-coded frame.
fn second_unit_offset(h: &FrameHeader, data: &[u8]) -> Result<usize> {
    if let Some(sz) = h.coding_unit_size() {
        let sz = sz as usize;
        if data.len() >= sz + 0x280 && data[sz..sz + 4] == data[0..4] && data[sz + 4] == data[4] {
            return Ok(sz);
        }
    }
    // VBR (or unexpected size): find the next header prefix after the first unit's scan lines
    let prefix = &data[0..5];
    let start = h.header_size as usize;
    data[start..]
        .windows(5)
        .position(|w| w == prefix)
        .map(|p| p + start)
        .filter(|&p| header::parse(&data[p..]).is_ok())
        .ok_or(Error::Invalid("second field coding unit not found"))
}

/// Decoded planes of one coding unit at the coded (macroblock-aligned) size.
struct CodedPlanes {
    /// Y / Cb / Cr / A (or R / G / B / A).
    planes: [Vec<u16>; 4],
    strides: [usize; 4],
    /// An RGB-signalled stream whose macroblocks are all coded as Y'CbCr.
    ycbcr: bool,
}

impl CodedPlanes {
    /// Crop into `f`, writing coded line `i` to frame line `first + i·step`.
    fn copy_into(&self, f: &mut Frame, first: usize, step: usize) {
        if self.ycbcr {
            f.rgb = false;
        }
        let cw = f.chroma_width() as usize;
        let w = f.width as usize;
        let ch = f.chroma_height() as usize;
        let h = f.height as usize;
        let cfirst = first;
        let mut targets: Vec<(&mut Vec<u16>, usize, usize, usize, usize)> =
            vec![(&mut f.y, w, h, first, 0), (&mut f.cb, cw, ch, cfirst, 1), (&mut f.cr, cw, ch, cfirst, 2)];
        if let Some(a) = f.alpha.as_mut() {
            targets.push((a, w, h, first, 3));
        }
        for (dst, pw, ph, first, pi) in targets {
            let src = &self.planes[pi];
            let ss = self.strides[pi];
            let mut line = 0;
            let mut y = first;
            while y < ph {
                let s = &src[line * ss..line * ss + pw];
                dst[y * pw..y * pw + pw].copy_from_slice(s);
                line += 1;
                y += step;
            }
        }
    }
}

/// Block layout entry: plane, x offset and y offset (0 or 8) inside the macroblock.
type BlockPos = (u8, u8, u8);

const LAYOUT_422: [BlockPos; 8] = [(0, 0, 0), (0, 8, 0), (1, 0, 0), (2, 0, 0), (0, 0, 8), (0, 8, 8), (1, 0, 8), (2, 0, 8)];
const LAYOUT_420: [BlockPos; 6] = [(0, 0, 0), (0, 8, 0), (1, 0, 0), (2, 0, 0), (0, 0, 8), (0, 8, 8)];
const LAYOUT_444: [BlockPos; 12] =
    [(0, 0, 0), (0, 8, 0), (1, 0, 0), (1, 8, 0), (2, 0, 0), (2, 8, 0), (0, 0, 8), (0, 8, 8), (1, 0, 8), (1, 8, 8), (2, 0, 8), (2, 8, 8)];
const LAYOUT_ALPHA: [BlockPos; 4] = [(3, 0, 0), (3, 8, 0), (3, 0, 8), (3, 8, 8)];

/// Per-coding-unit decoding context.
struct Ctx<'a> {
    h: &'a FrameHeader,
    info: &'static CidInfo,
    vlc: &'static VlcSet,
    nw: usize,
    layout: &'static [BlockPos],
    /// Plane macroblock sizes (width, height) for Y/Cb/Cr/A.
    mb_size: [(usize, usize); 4],
    strides: [usize; 4],
    p_bits: u32,
    p_shift: u32,
    p_half: u32,
    spec_dequant: bool,
    /// `1 / (4p)`
    inv_4p: f32,
    /// IDCT output scale (4 at 12 bits, see README) and level offset.
    scale: f32,
    mid: f32,
    max: f32,
}

fn decode_unit(h: &FrameHeader, data: &[u8], opts: &DecodeOptions) -> Result<CodedPlanes> {
    let info = cid_info(h.cid).ok_or(Error::UnknownCid(h.cid))?;
    let nw = h.mb_width() as usize;
    let ns = h.mb_rows as usize;
    let (layout, cmb): (&'static [BlockPos], (usize, usize)) = match h.chroma {
        ChromaFormat::Yuv422 => (&LAYOUT_422, (8, 16)),
        ChromaFormat::Yuv420 => (&LAYOUT_420, (8, 8)),
        ChromaFormat::Yuv444 => (&LAYOUT_444, (16, 16)),
    };
    let mb_size = [(16, 16), cmb, cmb, (16, 16)];
    let strides = [nw * 16, nw * cmb.0, nw * cmb.0, nw * 16];
    let ctx = Ctx {
        h,
        info,
        vlc: vlc(info.vlc),
        nw,
        layout,
        mb_size,
        strides,
        p_bits: if h.bit_depth == 8 { 4 } else { 6 },
        p_shift: info.p.trailing_zeros(),
        p_half: info.p / 2,
        spec_dequant: opts.spec_dequant,
        inv_4p: 1.0 / (4 * info.p) as f32,
        scale: if h.bit_depth == 12 { 4.0 } else { 1.0 },
        mid: (1u32 << (h.bit_depth - 1)) as f32,
        max: ((1u32 << h.bit_depth) - 1) as f32,
    };
    // scan line byte ranges
    let hs = h.header_size as usize;
    let mut starts = Vec::with_capacity(ns);
    for s in 0..ns {
        let o = SCAN_INDEX_OFFSET + 4 * s;
        let idx = u32::from_be_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]) as usize;
        let st = hs.checked_add(idx).filter(|&v| v < data.len()).ok_or(Error::Truncated)?;
        starts.push(st);
    }
    let ranges: Vec<(usize, usize)> = (0..ns)
        .map(|s| {
            let st = starts[s];
            let end = starts.get(s + 1).copied().filter(|&e| e > st).unwrap_or(data.len());
            (st, end)
        })
        .collect();

    let alpha = h.alpha;
    let mut planes: [Vec<u16>; 4] = [
        vec![0; strides[0] * 16 * ns],
        vec![0; strides[1] * cmb.1 * ns],
        vec![0; strides[2] * cmb.1 * ns],
        if alpha { vec![0; strides[3] * 16 * ns] } else { Vec::new() },
    ];
    let [py, pcb, pcr, pa] = &mut planes;
    let band = [strides[0] * 16, strides[1] * cmb.1, strides[2] * cmb.1, strides[3] * 16];
    let mut alpha_bands: Vec<&mut [u16]> = if alpha { pa.chunks_mut(band[3]).collect() } else { (0..ns).map(|_| &mut [][..]).collect() };
    let rows: Vec<(usize, &mut [u16], &mut [u16], &mut [u16], &mut [u16])> = py
        .chunks_mut(band[0])
        .zip(pcb.chunks_mut(band[1]))
        .zip(pcr.chunks_mut(band[2]))
        .zip(alpha_bands.drain(..))
        .enumerate()
        .map(|(i, (((y, cb), cr), a))| (i, y, cb, cr, a))
        .collect();
    let run = |(i, y, cb, cr, a): (usize, &mut [u16], &mut [u16], &mut [u16], &mut [u16])| -> Result<Vec<bool>> {
        let (st, end) = ranges[i];
        decode_row(&ctx, &data[st..end], [y, cb, cr, a]).map_err(|e| match e {
            Error::Row(_) => Error::Row(i),
            e => e,
        })
    };
    #[cfg(feature = "threads")]
    let acf: Vec<Vec<bool>> = if opts.threads {
        use rayon::prelude::*;
        rows.into_par_iter().map(run).collect::<Result<_>>()?
    } else {
        rows.into_iter().map(run).collect::<Result<_>>()?
    };
    #[cfg(not(feature = "threads"))]
    let acf: Vec<Vec<bool>> = {
        let _ = opts;
        rows.into_iter().map(run).collect::<Result<_>>()?
    };
    // RGB streams (CLF = 1) choose RGB or Y'CbCr per macroblock (ACF). A stream whose
    // macroblocks are all Y'CbCr is returned as Y'CbCr 4:4:4; mixed streams are converted to RGB
    // macroblock by macroblock (ConvertMacroblockColor, 8.1.1).
    let n_acf: usize = acf.iter().map(|r| r.iter().filter(|&&a| a).count()).sum();
    let ycbcr = h.rgb_planes() && n_acf == nw * ns;
    if h.rgb_planes() && n_acf > 0 && !ycbcr && h.color_volume != crate::ColorVolume::OutOfBand {
        let [py, pcb, pcr, _] = &mut planes;
        for (s, row) in acf.iter().enumerate() {
            for (mb, &a) in row.iter().enumerate() {
                if a {
                    let o = s * band[0];
                    convert_mb_to_rgb(&ctx, [&mut py[o..o + band[0]], &mut pcb[o..o + band[0]], &mut pcr[o..o + band[0]]], mb);
                }
            }
        }
    }
    Ok(CodedPlanes { planes, strides, ycbcr })
}

/// Decode one macroblock scan line into the 16-line bands of each plane.
fn decode_row(ctx: &Ctx, data: &[u8], bands: [&mut [u16]; 4]) -> Result<Vec<bool>> {
    let h = ctx.h;
    let mut r = Reader::new(data);
    // DC predictors: Y/Ch1, Cb, Cr, R, G, B, A
    let mut pred = [0i32; 7];
    let rgb = h.rgb_planes();
    let mff_cid = ctx.info.cid == 1260;
    let mut block = [0f32; 64];
    let mut acfs = Vec::new();
    for mb in 0..ctx.nw {
        let (qsf, flag, mff) = if mff_cid {
            let mff = r.read(1) == 1;
            (r.read(10), r.read(1) == 1, mff)
        } else {
            (r.read(11), r.read(1) == 1, false)
        };
        let acf = rgb && flag;
        let wq = scaled_weights(ctx.info.weights, qsf);
        for &(plane, xo, yo) in ctx.layout {
            let comp = plane as usize;
            let slot = if rgb && !acf { 3 + comp } else { comp };
            let w = &wq[(comp != 0) as usize];
            decode_block(ctx, &mut r, &mut pred[slot], w, &mut block)?;
            store(ctx, bands[comp], comp, mb, xo as usize, yo as usize, mff, &mut block);
        }
        if rgb {
            acfs.push(acf);
        }
        if r.overrun() {
            return Err(Error::Row(0));
        }
    }
    if h.alpha {
        if h.lossless_alpha {
            for mb in 0..ctx.nw {
                decode_rle_alpha(ctx, &mut r, bands[3], mb)?;
            }
        } else {
            for mb in 0..ctx.nw {
                let qsf = if mff_cid {
                    r.read(1);
                    let q = r.read(10);
                    r.read(1);
                    q
                } else {
                    let q = r.read(11);
                    r.read(1);
                    q
                };
                let wq = scaled_weights(ctx.info.weights, qsf);
                for &(plane, xo, yo) in &LAYOUT_ALPHA {
                    decode_block(ctx, &mut r, &mut pred[6], &wq[0], &mut block)?;
                    store(ctx, bands[3], plane as usize, mb, xo as usize, yo as usize, false, &mut block);
                }
            }
        }
        if r.overrun() {
            return Err(Error::Row(0));
        }
    }
    Ok(acfs)
}

/// `W(u,v)·qsf` for every AC raster position (index 0 unused), luma and chroma.
#[inline]
fn scaled_weights(w: &[[u8; 63]; 2], qsf: u32) -> [[(u32, u32); 64]; 2] {
    let mut out = [[(0u32, 0u32); 64]; 2];
    for c in 0..2 {
        for i in 0..63 {
            let wi = w[c][i] as u32;
            out[c][i + 1] = (wi * qsf, wi);
        }
    }
    out
}

/// Decode one DCT block into dequantised coefficients (raster order).
#[inline]
fn decode_block(ctx: &Ctx, r: &mut Reader, pred: &mut i32, wq: &[(u32, u32); 64], block: &mut [f32; 64]) -> Result<()> {
    let v = ctx.vlc;
    *block = [0.0; 64];
    // DC (8.2.4)
    let eta = r.vlc(&v.dc);
    let eps = if eta == 0 {
        0
    } else {
        let rho = r.read(eta) as i32;
        if rho >= 1 << (eta - 1) { rho } else { rho + 1 - (1 << eta) }
    };
    let dc = pred.wrapping_add(eps);
    *pred = dc;
    block[0] = dc as f32;
    // AC (8.2.5)
    let mut pos = 0usize;
    loop {
        let s = r.vlc(&v.amp);
        if s & AMP_EOB != 0 {
            break;
        }
        let neg = r.read(1) == 1;
        let mut a = s & 0xff;
        if s & AMP_FINDEX != 0 {
            a += r.read(ctx.p_bits) * 64;
        }
        pos += 1;
        if s & AMP_FRUN != 0 {
            pos += r.vlc(&v.run) as usize;
        }
        if pos > 63 {
            return Err(Error::Row(0));
        }
        let z = ZIGZAG[pos] as usize;
        let (wqs, w) = wq[z];
        // inverse quantisation (8.2.7)
        // Reconstruction at (|X̂| + 3/4)·W·qsf/p, as ffmpeg's decoder does (see README); the
        // standard's equation 8.1 rounds to an integer near (|X̂| + 1/2)·W·qsf/p.
        let mag = if ctx.spec_dequant {
            let add = if w != ctx.info.p { ctx.p_half } else { 0 };
            ((a as u64 * wqs as u64 + (wqs >> 1) as u64 + add as u64) >> ctx.p_shift) as f32
        } else {
            ((4 * a as u64 + 3) * wqs as u64) as f32 * ctx.inv_4p
        };
        block[z] = if neg { -mag } else { mag };
    }
    Ok(())
}

/// IDCT a block and store it (with level adjustment and clipping) into a plane band.
#[inline]
fn store(ctx: &Ctx, band: &mut [u16], plane: usize, mb: usize, xo: usize, yo: usize, field: bool, block: &mut [f32; 64]) {
    idct8x8(block);
    let stride = ctx.strides[plane];
    let x0 = mb * ctx.mb_size[plane].0 + xo;
    let (first, step) = if field { (yo / 8, 2) } else { (yo, 1) };
    let (scale, mid, max) = (ctx.scale, ctx.mid + 0.5, ctx.max);
    for j in 0..8 {
        let row = &mut band[(first + j * step) * stride + x0..][..8];
        let src = &block[j * 8..j * 8 + 8];
        for i in 0..8 {
            row[i] = (src[i] * scale + mid).floor().clamp(0.0, max) as u16;
        }
    }
}

/// `ConvertMacroblockColor` (8.1.1): BT.709 Y′CbCr → R′G′B′ on the quantised (video-range)
/// signals, in place over the macroblock's three planes.
fn convert_mb_to_rgb(ctx: &Ctx, bands: [&mut [u16]; 3], mb: usize) {
    let stride = ctx.strides[0];
    let s = (1u32 << (ctx.h.bit_depth - 8)) as f32;
    let (kr, kb) = match ctx.h.color_volume {
        crate::ColorVolume::Bt709 => (0.2126f32, 0.0722f32),
        _ => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    let c = 219.0 / 224.0;
    for y in 0..16 {
        for x in 0..16 {
            let i = y * stride + mb * 16 + x;
            let yy = bands[0][i] as f32;
            let cb = bands[1][i] as f32 - 128.0 * s;
            let cr = bands[2][i] as f32 - 128.0 * s;
            let r = yy + 2.0 * (1.0 - kr) * c * cr;
            let b = yy + 2.0 * (1.0 - kb) * c * cb;
            let g = (yy - kr * r - kb * b) / kg;
            // RGB planes are stored in coded channel order G, B, R
            bands[0][i] = (g + 0.5).floor().clamp(0.0, ctx.max) as u16;
            bands[1][i] = (b + 0.5).floor().clamp(0.0, ctx.max) as u16;
            bands[2][i] = (r + 0.5).floor().clamp(0.0, ctx.max) as u16;
        }
    }
}

/// Lossless (differential RLE) alpha macroblock (§7.3.1.2, §8.3).
fn decode_rle_alpha(ctx: &Ctx, r: &mut Reader, band: &mut [u16], mb: usize) -> Result<()> {
    let d = ctx.h.bit_depth as u32;
    let mask = (1u32 << d) - 1;
    let stride = ctx.strides[3];
    let pos = |count: usize| {
        let y = count / 16;
        let x = if y.is_multiple_of(2) { count % 16 } else { 15 - count % 16 };
        y * stride + mb * 16 + x
    };
    // RL = 1: RLE coded; RL = 0: 256 raw samples (§7.3.1.2 prose; see README)
    let rl = r.read(4) & 1;
    if rl == 0 {
        for count in 0..256 {
            band[pos(count)] = r.read(d) as u16;
        }
        return Ok(());
    }
    let rice = d / 2 - 2;
    let (mut d1, mut d2) = (mask >> 1, mask >> 1);
    let mut count = 0usize;
    while count < 256 {
        let p = r.read(1);
        // Elias gamma
        let mut n = 0;
        while r.read(1) == 0 {
            n += 1;
            if n > 8 || r.overrun() {
                return Err(Error::Row(0));
            }
        }
        let nrl = (1usize << n) + r.read(n) as usize;
        let dcw = if r.read(1) == 1 {
            let mut q = 0u32;
            while r.read(1) == 1 {
                q += 1;
                if q > 1 << d || r.overrun() {
                    return Err(Error::Row(0));
                }
            }
            (q << rice) + r.read(rice)
        } else {
            r.read(d)
        };
        let diff = if dcw & 1 == 1 { mask.wrapping_sub(dcw >> 1) } else { dcw >> 1 };
        if count + nrl > 256 {
            return Err(Error::Row(0));
        }
        for _ in 0..nrl {
            let v = if p == 1 { (2 * d1).wrapping_sub(d2).wrapping_add(diff) & mask } else { d1.wrapping_add(diff) & mask };
            band[pos(count)] = v as u16;
            d2 = d1;
            d1 = v;
            count += 1;
        }
    }
    Ok(())
}
