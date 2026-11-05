//! ProRes frame decoding.

use crate::bits::Reader;
use crate::dct::idct8x8;
use crate::header::{FrameHeader, PictureHeader, row_slice_widths};
use crate::tables::{DC_CB, DC_CTX_INIT, FIRST_DC_CB, LEVEL_CB, LEVEL_CTX_INIT, RUN_CB, RUN_CTX_INIT, SCAN_INTERLACED, SCAN_PROGRESSIVE, qscale};
use crate::{AlphaType, ChromaFormat, Error, Frame, Interlace, Result};

/// Maximum aggregate decoded plane storage for one ProRes frame (256 MiB).
// 1 GiB: 8K ProRes 4444 with alpha is 283 MB of 16-bit planes
const MAX_FRAME_BYTES: usize = 1 << 30;

/// Decoder options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeOptions {
    /// Output bit depth (8..=16). `None`: 10 for 4:2:2, 12 for 4:4:4 (the precision ProRes
    /// codes each format at).
    pub bit_depth: Option<u8>,
    /// Decode slices in parallel (requires the `threads` feature).
    pub threads: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions { bit_depth: None, threads: true }
    }
}

/// Parse only the frame header (cheap; for probing geometry and format).
pub fn probe(data: &[u8]) -> Result<FrameHeader> {
    FrameHeader::parse(data).map(|(h, _, _)| h)
}

/// Decode one ProRes frame (the contents of one QuickTime sample) with default options.
pub fn decode_frame(data: &[u8]) -> Result<Frame> {
    decode_frame_with(data, &DecodeOptions::default())
}

/// Maps IDCT output (12-bit-domain signed samples) to the output bit depth.
#[derive(Clone, Copy)]
struct OutConv {
    scale: f32,
    bias: f32,
    min: i32,
    max: i32,
}

impl OutConv {
    fn new(depth: u8) -> OutConv {
        // The IDCT produces `f`, where the 12-bit sample is `f + 2048`.
        let scale = (2f32).powi(depth as i32 - 12);
        // Samples are clipped away from the extremes: the four lowest and highest codes are
        // excluded at 10 bits and above (0..=3 and 1020..=1023 are the SDI timing-reference codes
        // at 10 bits; the same margin is kept at 12 bits, matching common decoders), and the
        // single lowest/highest code at 8 and 9 bits.
        let reserved = if depth >= 10 { 4 } else { 1 };
        OutConv { scale, bias: (1u32 << (depth - 1)) as f32 + 0.5, min: reserved, max: (1i32 << depth) - 1 - reserved }
    }
    #[inline(always)]
    fn conv(&self, v: f32) -> u16 {
        // Truncation equals floor here: negative values clamp to `min` (> 0) either way.
        ((v * self.scale + self.bias) as i32).clamp(self.min, self.max) as u16
    }
}

struct Ctx {
    width: usize,
    cwidth: usize,
    chroma444: bool,
    qmat_luma: [u8; 64],
    qmat_chroma: [u8; 64],
    scan: &'static [u8; 64],
    alpha: AlphaType,
    conv: OutConv,
    depth: u8,
}

/// Output rows belonging to one macroblock row of every coded picture.
struct Band<'a> {
    y: &'a mut [u16],
    cb: &'a mut [u16],
    cr: &'a mut [u16],
    a: Option<&'a mut [u16]>,
    rows: usize,
}

struct Picture {
    /// Line parity within the frame (0 = even lines) for field pictures.
    parity: usize,
    /// Byte ranges of each slice, row-major.
    slices: Vec<(usize, usize)>,
}

struct Scratch {
    coeffs: Vec<[f32; 64]>,
    alpha: Vec<u16>,
}

impl Scratch {
    fn new() -> Scratch {
        Scratch { coeffs: vec![[0f32; 64]; 32], alpha: vec![0; 16 * 128] }
    }
}

/// Decode one ProRes frame.
pub fn decode_frame_with(data: &[u8], opts: &DecodeOptions) -> Result<Frame> {
    let mut f = Frame::new(0, 0, ChromaFormat::Yuv422, 10, false);
    decode_frame_into(data, opts, &mut f)?;
    Ok(f)
}

/// Decode one ProRes frame into `out`, reusing its plane allocations (the fastest way to decode
/// a sequence). On error the contents of `out` are unspecified.
pub fn decode_frame_into(data: &[u8], opts: &DecodeOptions, out: &mut Frame) -> Result<()> {
    let (hdr, mut off, frame_size) = FrameHeader::parse(data)?;
    let data = &data[..frame_size];
    let depth = opts.bit_depth.unwrap_or(match hdr.chroma {
        ChromaFormat::Yuv422 => 10,
        ChromaFormat::Yuv444 => 12,
    });
    if !(8..=16).contains(&depth) {
        return Err(Error::Unsupported("output bit depth"));
    }
    let width = hdr.width as usize;
    let height = hdr.height as usize;
    let cwidth = hdr.chroma.chroma_width(hdr.width as u32) as usize;
    let luma = width.checked_mul(height).ok_or(Error::Invalid("frame dimensions overflow"))?;
    let chroma = cwidth.checked_mul(height).ok_or(Error::Invalid("chroma dimensions overflow"))?;
    let samples = luma
        .checked_add(chroma.checked_mul(2).ok_or(Error::Invalid("chroma plane count overflows"))?)
        .and_then(|n| if hdr.alpha == AlphaType::None { Some(n) } else { n.checked_add(luma) })
        .ok_or(Error::Invalid("frame plane count overflows"))?;
    if samples.checked_mul(std::mem::size_of::<u16>()).is_none_or(|n| n > MAX_FRAME_BYTES) {
        return Err(Error::Invalid("frame dimensions exceed the decoder memory limit"));
    }
    let interlaced = hdr.interlace != Interlace::Progressive;
    let mb_width = width.div_ceil(16);
    let mb_height = if interlaced { height.div_ceil(32) } else { height.div_ceil(16) };

    let npics = if interlaced { 2 } else { 1 };
    let mut pics = Vec::with_capacity(npics);
    for p in 0..npics {
        let ph = PictureHeader::parse(data.get(off..).ok_or(Error::Truncated)?)?;
        let pic_end = off.checked_add(ph.picture_size).ok_or(Error::Truncated)?;
        if pic_end > data.len() || ph.picture_size < ph.header_size {
            return Err(Error::Truncated);
        }
        let widths = row_slice_widths(mb_width, ph.log2_slice_mbs);
        let nslices = widths.len() * mb_height;
        if ph.num_slices != 0 && ph.num_slices != nslices {
            return Err(Error::Invalid("slice count does not match frame geometry"));
        }
        let table = off + ph.header_size;
        let mut pos = table + 2 * nslices;
        if pos > pic_end {
            return Err(Error::Truncated);
        }
        let mut slices = Vec::with_capacity(nslices);
        for i in 0..nslices {
            let sz = u16::from_be_bytes([data[table + 2 * i], data[table + 2 * i + 1]]) as usize;
            // Every slice carries at least its 6-byte header.
            if sz < 6 || pos + sz > pic_end {
                return Err(Error::Truncated);
            }
            slices.push((pos, sz));
            pos += sz;
        }
        let parity = match (hdr.interlace, p) {
            (Interlace::BottomFieldFirst, 0) | (Interlace::TopFieldFirst, 1) => 1,
            _ => 0,
        };
        pics.push((Picture { parity, slices }, widths));
        off = pic_end;
    }

    let ctx = Ctx {
        width,
        cwidth,
        chroma444: hdr.chroma == ChromaFormat::Yuv444,
        qmat_luma: hdr.qmat_luma,
        qmat_chroma: hdr.qmat_chroma,
        scan: if interlaced { &SCAN_INTERLACED } else { &SCAN_PROGRESSIVE },
        alpha: hdr.alpha,
        conv: OutConv::new(depth),
        depth,
    };

    let take = |v: &mut Vec<u16>, n: usize| {
        let mut v = std::mem::take(v);
        v.resize(n, 0);
        v
    };
    let mut y = take(&mut out.y, width * height);
    let mut cb = take(&mut out.cb, cwidth * height);
    let mut cr = take(&mut out.cr, cwidth * height);
    let mut alpha = match (hdr.alpha, out.alpha.take()) {
        (AlphaType::None, _) => None,
        (_, a) => Some(take(&mut a.unwrap_or_default(), width * height)),
    };

    let band_h = if interlaced { 32 } else { 16 };
    let mut bands: Vec<Band> = Vec::with_capacity(mb_height);
    {
        let mut yc = y.chunks_mut(band_h * width);
        let mut cbc = cb.chunks_mut(band_h * cwidth);
        let mut crc = cr.chunks_mut(band_h * cwidth);
        let mut ac = alpha.as_mut().map(|a| a.chunks_mut(band_h * width));
        while let (Some(y), Some(cb), Some(cr)) = (yc.next(), cbc.next(), crc.next()) {
            let rows = y.len() / width;
            let a = ac.as_mut().and_then(|it| it.next());
            bands.push(Band { y, cb, cr, a, rows });
        }
    }

    let step = if interlaced { 2 } else { 1 };
    let run_band = |(row, band): (usize, &mut Band), scratch: &mut Scratch| -> Result<()> {
        for (pi, (pic, widths)) in pics.iter().enumerate() {
            let mut mb_x = 0;
            for (k, &w) in widths.iter().enumerate() {
                let si = row * widths.len() + k;
                let (start, len) = pic.slices[si];
                decode_slice(&ctx, &data[start..start + len], mb_x, w, band, Lines { step, parity: pic.parity }, scratch)
                    .map_err(|_| Error::Slice { picture: pi, slice: si })?;
                mb_x += w;
            }
        }
        Ok(())
    };

    #[cfg(feature = "threads")]
    if opts.threads {
        use rayon::prelude::*;
        bands.par_iter_mut().enumerate().try_for_each_init(Scratch::new, |s, b| run_band(b, s))?;
    } else {
        let mut s = Scratch::new();
        bands.iter_mut().enumerate().try_for_each(|b| run_band(b, &mut s))?;
    }
    #[cfg(not(feature = "threads"))]
    {
        let mut s = Scratch::new();
        bands.iter_mut().enumerate().try_for_each(|b| run_band(b, &mut s))?;
    }
    drop(bands);

    *out = Frame {
        width: hdr.width as u32,
        height: hdr.height as u32,
        chroma: hdr.chroma,
        bit_depth: depth,
        y,
        cb,
        cr,
        alpha,
        interlace: hdr.interlace,
        color: hdr.color,
        aspect_ratio: hdr.aspect_ratio,
        frame_rate_code: hdr.frame_rate_code,
    };
    Ok(())
}

#[inline]
fn be16(d: &[u8], i: usize) -> usize {
    u16::from_be_bytes([d[i], d[i + 1]]) as usize
}

/// Dequantisation factors (matrix × scale) for a slice.
fn qfactors(qmat: &[u8; 64], qs: u32) -> [f32; 64] {
    let mut q = [0f32; 64];
    for i in 0..64 {
        q[i] = (qmat[i] as u32 * qs) as f32;
    }
    q
}

/// Line stepping of the picture inside its frame: `step` is 2 for field pictures, `parity` the first line.
#[derive(Clone, Copy)]
struct Lines {
    step: usize,
    parity: usize,
}

fn decode_slice(ctx: &Ctx, d: &[u8], mb_x: usize, mbs: usize, band: &mut Band, il: Lines, s: &mut Scratch) -> std::result::Result<(), ()> {
    let Lines { step, parity } = il;
    if d.len() < 6 {
        return Err(());
    }
    let hsize = (d[0] >> 3) as usize;
    let has_alpha = ctx.alpha != AlphaType::None;
    if hsize < if has_alpha { 8 } else { 6 } || hsize > d.len() {
        return Err(());
    }
    let qs = qscale(d[1]);
    let ysize = be16(d, 2);
    let cbsize = be16(d, 4);
    let body = d.len() - hsize;
    let crsize = if has_alpha { be16(d, 6) } else { body.checked_sub(ysize + cbsize).ok_or(())? };
    if ysize + cbsize + crsize > body {
        return Err(());
    }
    let mut p = hsize;
    let ydata = &d[p..p + ysize];
    p += ysize;
    let cbdata = &d[p..p + cbsize];
    p += cbsize;
    let crdata = &d[p..p + crsize];
    p += crsize;
    let adata = &d[p..];

    let log2_mbs = mbs.trailing_zeros();
    let x0 = mb_x * 16;

    // Luma
    let ql = qfactors(&ctx.qmat_luma, qs);
    let nb = 4 * mbs;
    decode_component(ydata, log2_mbs + 2, ctx.scan, &ql, &mut s.coeffs[..nb])?;
    for (b, blk) in s.coeffs[..nb].iter_mut().enumerate() {
        idct8x8(blk);
        let (mb, i) = (b >> 2, b & 3);
        put_block(blk, band.y, ctx.width, (x0 + mb * 16 + (i & 1) * 8, (i >> 1) * 8), band.rows, il, ctx.conv);
    }

    // Chroma
    let qc = qfactors(&ctx.qmat_chroma, qs);
    let (cnb, clog2) = if ctx.chroma444 { (4 * mbs, log2_mbs + 2) } else { (2 * mbs, log2_mbs + 1) };
    for (data, plane) in [(cbdata, &mut *band.cb), (crdata, &mut *band.cr)] {
        decode_component(data, clog2, ctx.scan, &qc, &mut s.coeffs[..cnb])?;
        for (b, blk) in s.coeffs[..cnb].iter_mut().enumerate() {
            idct8x8(blk);
            let (bx, by) = if ctx.chroma444 {
                // 4:4:4 chroma blocks run down each 8-wide column pair: TL, BL, TR, BR.
                let (mb, i) = (b >> 2, b & 3);
                (x0 + mb * 16 + (i >> 1) * 8, (i & 1) * 8)
            } else {
                let (mb, i) = (b >> 1, b & 1);
                (mb_x * 8 + mb * 8, i * 8)
            };
            put_block(blk, plane, ctx.cwidth, (bx, by), band.rows, il, ctx.conv);
        }
    }

    // Alpha
    if let Some(a) = band.a.as_deref_mut() {
        let w = mbs * 16;
        let n = w * 16;
        let buf = &mut s.alpha[..n];
        decode_alpha(adata, ctx.alpha, buf)?;
        let depth = ctx.depth as u32;
        let conv = |v: u16| -> u16 {
            let v = v as u32;
            (match ctx.alpha {
                AlphaType::Bits8 => {
                    if depth >= 8 {
                        let r = (v << (depth - 8)) | (v >> (16 - depth).min(8));
                        r & ((1 << depth) - 1)
                    } else {
                        v
                    }
                }
                _ => v >> (16 - depth),
            }) as u16
        };
        for py in 0..16 {
            let line = py * step + parity;
            if line >= band.rows {
                break;
            }
            let row = &mut a[line * ctx.width..(line + 1) * ctx.width];
            let xe = (x0 + w).min(ctx.width);
            if x0 >= xe {
                continue;
            }
            for (o, &v) in row[x0..xe].iter_mut().zip(&buf[py * w..py * w + (xe - x0)]) {
                *o = conv(v);
            }
        }
    }
    Ok(())
}

#[inline]
fn to_signed(code: u32) -> i64 {
    let c = code as i64;
    (c >> 1) ^ -(c & 1)
}

/// Entropy-decode and dequantise one slice component into `blocks` (raster-order coefficients).
fn decode_component(data: &[u8], log2_blocks: u32, scan: &[u8; 64], q: &[f32; 64], blocks: &mut [[f32; 64]]) -> std::result::Result<(), ()> {
    for b in blocks.iter_mut() {
        *b = [0f32; 64];
    }
    let n = blocks.len();
    let mut r = Reader::new(data);

    // DC coefficients
    let code = r.read_cw(FIRST_DC_CB).ok_or(())?;
    let mut dc = to_signed(code);
    blocks[0][0] = dc as f32 * q[0];
    let mut code = DC_CTX_INIT;
    let mut sign = 0i64;
    for blk in blocks.iter_mut().skip(1) {
        code = r.read_cw(DC_CB[code.min(6) as usize]).ok_or(())?;
        if code == 0 {
            sign = 0;
        } else {
            sign ^= -((code & 1) as i64);
        }
        let mag = (code as i64 + 1) >> 1;
        dc = (dc + ((mag ^ sign) - sign)).clamp(-(1 << 24), 1 << 24);
        blk[0] = dc as f32 * q[0];
    }

    // AC coefficients, interleaved across blocks: position = scan_index * n + block
    let mask = n - 1;
    let total = 64 * n;
    let mut pos = mask;
    let mut run_ctx = RUN_CTX_INIT;
    let mut lev_ctx = LEVEL_CTX_INIT;
    while !r.only_padding_left() {
        let run = r.read_cw(RUN_CB[run_ctx.min(15) as usize]).ok_or(())?;
        run_ctx = run;
        pos += run as usize + 1;
        if pos >= total {
            return Err(());
        }
        let lev = r.read_cw(LEVEL_CB[lev_ctx.min(9) as usize]).ok_or(())?.saturating_add(1);
        lev_ctx = lev;
        let neg = r.read_bit();
        let i = scan[pos >> log2_blocks] as usize;
        let v = lev as f32 * q[i];
        blocks[pos & mask][i] = if neg { -v } else { v };
    }
    if r.overrun() {
        return Err(());
    }
    Ok(())
}

/// Decode a slice's alpha plane (raster order over the slice's 16 lines) into `out`
/// (native 8- or 16-bit values).
fn decode_alpha(data: &[u8], ty: AlphaType, out: &mut [u16]) -> std::result::Result<(), ()> {
    let bits = if ty == AlphaType::Bits16 { 16 } else { 8 };
    let short_bits = if bits == 16 { 7 } else { 4 };
    let mask: u32 = (1 << bits) - 1;
    let mut r = Reader::new(data);
    let mut val: u32 = mask;
    let n = out.len();
    let mut idx = 0;
    'outer: loop {
        loop {
            let d = if r.read_bit() {
                r.read(bits)
            } else {
                let c = r.read(short_bits);
                let m = (c + 2) >> 1;
                if c & 1 == 1 { m.wrapping_neg() } else { m }
            };
            val = val.wrapping_add(d) & mask;
            out[idx] = val as u16;
            idx += 1;
            if idx >= n {
                break 'outer;
            }
            if r.bits_left() <= 0 || !r.read_bit() {
                break;
            }
        }
        let mut run = r.read(4) as usize;
        if run == 0 {
            run = r.read(11) as usize;
        }
        let run = run.min(n - idx);
        out[idx..idx + run].fill(val as u16);
        idx += run;
        if idx >= n {
            break;
        }
        if r.bits_left() <= 0 {
            // Streams may end early; the remainder repeats the last value.
            out[idx..].fill(val as u16);
            break;
        }
    }
    if r.bits_left() < -64 {
        return Err(());
    }
    Ok(())
}

#[inline]
fn put_block(f: &[f32; 64], plane: &mut [u16], stride: usize, (x0, y0): (usize, usize), rows: usize, il: Lines, conv: OutConv) {
    let Lines { step, parity } = il;
    if x0 >= stride {
        return;
    }
    let xn = (stride - x0).min(8);
    if xn == 8 && (y0 + 7) * step + parity < rows {
        for yy in 0..8 {
            let o = ((y0 + yy) * step + parity) * stride + x0;
            let (Some(row), Some(src)) = (plane.get_mut(o..).and_then(|p| p.first_chunk_mut::<8>()), f.as_chunks::<8>().0.get(yy)) else {
                return;
            };
            for x in 0..8 {
                row[x] = conv.conv(src[x]);
            }
        }
        return;
    }
    for yy in 0..8 {
        let line = (y0 + yy) * step + parity;
        if line >= rows {
            break;
        }
        let row = &mut plane[line * stride + x0..line * stride + x0 + xn];
        let src = &f[yy * 8..yy * 8 + xn];
        for (o, &v) in row.iter_mut().zip(src) {
            *o = conv.conv(v);
        }
    }
}
