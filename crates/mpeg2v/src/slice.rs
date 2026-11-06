//! Slice, macroblock and block decoding (H.262 §6.2.4-§6.2.6, §7.2-§7.6; ISO/IEC 11172-2
//! §2.4.2.7-§2.4.4).

use crate::bits::Bits;
use crate::headers::{PictureCodingExtension, PictureType, SCAN};
use crate::idct::idct;
use crate::mc::{Block, Frame, predict};
use crate::vlc::{DCT_EOB, DCT_ESCAPE, MB_BWD, MB_FWD, MB_INTRA, MB_PATTERN, MB_QUANT, MBA_ESCAPE, MBA_STUFFING, tables};

/// Non-linear quantiser_scale (Table 7-6), indexed by quantiser_scale_code.
pub(crate) const NON_LINEAR_Q: [u8; 32] =
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 104, 112];

/// Motion types (frame_motion_type / field_motion_type values).
const MT_FIELD: u8 = 1;
const MT_FRAME: u8 = 2; // frame pictures; 16x8 MC in field pictures
const MT_16X8: u8 = 2;
const MT_DUAL: u8 = 3;

/// Everything slices of one picture share (read-only while slices decode in parallel).
pub(crate) struct PicParams<'a> {
    pub mpeg2: bool,
    pub ptype: PictureType,
    pub pce: PictureCodingExtension,
    pub full_pel: [bool; 2],
    /// Chroma subsampling shifts (4:2:0: 1, 1; 4:2:2: 1, 0).
    pub shx: u32,
    pub shy: u32,
    pub mb_width: usize,
    /// Macroblock rows of this picture (field rows for field pictures).
    pub mb_rows: usize,
    /// Intra luma, non-intra luma, intra chroma, non-intra chroma (raster order).
    pub matrices: [[u8; 64]; 4],
    /// Forward reference (P: the latest anchor; B: the older anchor) and backward reference.
    pub fwd: Option<&'a Frame>,
    pub bwd: Option<&'a Frame>,
    /// The first field of this frame (when decoding its second field).
    pub first_field: Option<&'a Frame>,
    /// Field pictures: parity of this field (0 top, 1 bottom).
    pub parity: usize,
    pub vertical_size_extension: bool,
}

impl PicParams<'_> {
    fn frame_picture(&self) -> bool {
        self.pce.picture_structure == 3
    }
    fn chroma_blocks(&self) -> usize {
        if self.shy == 1 { 2 } else { 4 }
    }
    /// Chroma macroblock size.
    fn cmb(&self) -> (usize, usize) {
        (16 >> self.shx, 16 >> self.shy)
    }
}

/// Where a row (or, for MPEG-1, the whole picture) of macroblocks is written.
pub(crate) struct Target<'a> {
    pub planes: [&'a mut [u8]; 3],
    pub strides: [usize; 2],
    /// First luma / chroma frame line held by `planes`.
    pub line0: [usize; 2],
    /// Field pictures: parity written.
    pub field: Option<usize>,
}

impl Target<'_> {
    fn put(&mut self, mbx: usize, mby: usize, mb: &MbBuf, cmb: (usize, usize)) {
        for (c, plane) in self.planes.iter_mut().enumerate() {
            let k = (c > 0) as usize;
            let (bw, bh) = if c == 0 { (16, 16) } else { cmb };
            let stride = self.strides[k];
            let src = if c == 0 { &mb.y[..] } else { &mb.c[c - 1][..] };
            for l in 0..bh {
                let line = match self.field {
                    None => bh * mby + l,
                    Some(p) => 2 * (bh * mby + l) + p,
                };
                let Some(local) = line.checked_sub(self.line0[k]) else { continue };
                let at = local * stride + bw * mbx;
                if at + bw <= plane.len() {
                    plane[at..at + bw].copy_from_slice(&src[l * bw..l * bw + bw]);
                }
            }
        }
    }
}

/// One macroblock's samples (luma 16×16; chroma rows of `16 >> shx` samples).
pub(crate) struct MbBuf {
    y: [u8; 256],
    c: [[u8; 256]; 2],
}

#[derive(Clone, Copy, Default)]
struct Motion {
    flags: i16,
    mtype: u8,
    /// vectors[r][s][t] in half-sample units (vertical in field units for field prediction).
    mv: [[[i32; 2]; 2]; 2],
    select: [[usize; 2]; 2],
    dmv: [i32; 2],
}

struct SliceState {
    qscale: i32,
    dc_pred: [i32; 3],
    pmv: [[[i32; 2]; 2]; 2],
    last: Motion,
}

fn dc_reset(p: &PicParams) -> i32 {
    1 << (7 + p.pce.intra_dc_precision)
}

#[derive(Debug)]
pub(crate) struct SliceError(pub &'static str);

type R<T> = std::result::Result<T, SliceError>;

/// Decode one slice (`data`: the bytes after its start code) into `t`. `row`: the slice's
/// macroblock row; `single_row`: MPEG-2 slices may not leave their row.
pub(crate) fn decode_slice(p: &PicParams, data: &[u8], row: usize, single_row: bool, t: &mut Target) -> R<()> {
    let tb = tables();
    let mut b = Bits::new(data);
    if p.vertical_size_extension {
        b.skip(3); // already applied to `row` by the caller
    }
    let qcode = b.read(5) as usize;
    if p.mpeg2 && b.peek(1) == 1 {
        b.skip(9); // intra_slice_flag, intra_slice, reserved_bits
    }
    while b.bit() {
        b.skip(8); // extra_information_slice
    }
    let mut st = SliceState { qscale: qscale(p, qcode), dc_pred: [dc_reset(p); 3], pmv: [[[0; 2]; 2]; 2], last: Motion::default() };
    let total = p.mb_width * p.mb_rows;
    let row_end = if single_row { (row + 1) * p.mb_width } else { total };
    let mut addr: isize = (row * p.mb_width) as isize - 1;
    let mut first = true;
    let mut buf = MbBuf { y: [0; 256], c: [[0; 256]; 2] };
    let mut blk = [0i32; 64];
    loop {
        // macroblock_address_increment
        let mut inc = 0isize;
        loop {
            match tb.mba.decode(&mut b) {
                Some(MBA_ESCAPE) => inc += 33,
                Some(MBA_STUFFING) => {}
                Some(n) => {
                    inc += n as isize;
                    break;
                }
                None => return Err(SliceError("bad macroblock_address_increment")),
            }
            if b.overrun() {
                return Err(SliceError("truncated slice"));
            }
        }
        if !first && inc > 1 {
            // skipped macroblocks
            for _ in 1..inc {
                addr += 1;
                if addr as usize >= row_end {
                    return Err(SliceError("macroblock address past the row"));
                }
                skipped_mb(p, &mut st, addr as usize, &mut buf, t)?;
            }
            addr += 1;
        } else {
            addr += inc;
        }
        first = false;
        if addr < 0 || addr as usize >= row_end {
            return Err(SliceError("macroblock address out of range"));
        }
        let a = addr as usize;
        macroblock(p, &mut st, &mut b, a, &mut buf, &mut blk, t)?;
        if b.overrun() {
            return Err(SliceError("truncated macroblock"));
        }
        // next_start_code: 23 zero bits end the slice
        if b.bits_left() <= 0 || b.peek(23) == 0 {
            return Ok(());
        }
    }
}

fn qscale(p: &PicParams, code: usize) -> i32 {
    if !p.mpeg2 {
        code as i32
    } else if p.pce.q_scale_type {
        NON_LINEAR_Q[code & 31] as i32
    } else {
        2 * code as i32
    }
}

/// A skipped macroblock (§7.6.6): P pictures predict from the same position with a zero vector;
/// B pictures repeat the previous macroblock's prediction.
fn skipped_mb(p: &PicParams, st: &mut SliceState, addr: usize, buf: &mut MbBuf, t: &mut Target) -> R<()> {
    st.dc_pred = [dc_reset(p); 3];
    let m = match p.ptype {
        PictureType::P => {
            st.pmv = [[[0; 2]; 2]; 2];
            let mut m = Motion { flags: MB_FWD, mtype: if p.frame_picture() { MT_FRAME } else { MT_FIELD }, ..Default::default() };
            m.select[0][0] = p.parity;
            m
        }
        PictureType::B => {
            let last = st.last;
            if last.flags & MB_INTRA != 0 {
                return Err(SliceError("skipped macroblock after an intra macroblock"));
            }
            // the previous macroblock's direction, one vector per direction (PMV[0][s]); field
            // pictures predict from the field of the same parity (§7.6.6.4)
            let mut m = Motion { flags: last.flags & (MB_FWD | MB_BWD), mtype: if p.frame_picture() { MT_FRAME } else { MT_FIELD }, ..Default::default() };
            for s in 0..2 {
                m.mv[0][s] = st.pmv[0][s];
                m.select[0][s] = p.parity;
            }
            m
        }
        _ => return Err(SliceError("skipped macroblock in an intra picture")),
    };
    let (mbx, mby) = (addr % p.mb_width, addr / p.mb_width);
    predict_mb(p, &m, mbx, mby, buf);
    t.put(mbx, mby, buf, p.cmb());
    if p.ptype == PictureType::B {
        st.last = Motion { flags: m.flags, ..st.last };
    }
    Ok(())
}

fn macroblock(p: &PicParams, st: &mut SliceState, b: &mut Bits, addr: usize, buf: &mut MbBuf, blk: &mut [i32; 64], t: &mut Target) -> R<()> {
    let tb = tables();
    let ti = match p.ptype {
        PictureType::I => 0,
        PictureType::P => 1,
        PictureType::B => 2,
        PictureType::D => 3,
    };
    let flags = tb.mbtype[ti].decode(b).ok_or(SliceError("bad macroblock_type"))?;
    let intra = flags & MB_INTRA != 0;
    let frame_pic = p.frame_picture();
    let mut m = Motion { flags, ..Default::default() };
    if flags & (MB_FWD | MB_BWD) != 0 {
        m.mtype = if frame_pic && (p.pce.frame_pred_frame_dct || !p.mpeg2) { MT_FRAME } else { b.read(2) as u8 };
        if m.mtype == 0 {
            return Err(SliceError("reserved motion type"));
        }
    } else if intra && p.pce.concealment_motion_vectors {
        m.mtype = if frame_pic { MT_FRAME } else { MT_FIELD };
    }
    let dct_type = p.mpeg2 && frame_pic && !p.pce.frame_pred_frame_dct && (intra || flags & MB_PATTERN != 0) && b.bit();
    if flags & MB_QUANT != 0 {
        st.qscale = qscale(p, b.read(5) as usize);
    }
    let concealment = intra && p.pce.concealment_motion_vectors;
    read_motion_vectors(p, st, b, &mut m, concealment)?;
    if concealment {
        b.skip(1); // marker_bit
    }
    // coded_block_pattern
    let nblocks = 4 + p.chroma_blocks();
    let cbp = read_cbp(p, b, flags, intra)?;
    let (mbx, mby) = (addr % p.mb_width, addr / p.mb_width);
    if intra {
        if !concealment {
            st.pmv = [[[0; 2]; 2]; 2];
        }
    } else {
        st.dc_pred = [dc_reset(p); 3];
        if p.ptype == PictureType::P && flags & MB_FWD == 0 {
            // "No MC": zero vector from the same position (same-parity field in field pictures)
            st.pmv = [[[0; 2]; 2]; 2];
            m.flags |= MB_FWD;
            m.mtype = if frame_pic { MT_FRAME } else { MT_FIELD };
            m.select[0][0] = p.parity;
        }
        predict_mb(p, &m, mbx, mby, buf);
    }
    // blocks
    let (cbw, cbh) = p.cmb();
    for i in 0..nblocks {
        if cbp & (1 << (nblocks - 1 - i)) == 0 {
            continue;
        }
        let cc = block_component(i);
        let dc_only = block(p, st, b, cc, intra, blk)?;
        idct(blk, dc_only);
        place_block(buf, blk, i, (cbw, cbh), dct_type, intra);
    }
    if p.ptype == PictureType::D {
        b.skip(1); // end_of_macroblock
    }
    t.put(mbx, mby, buf, (cbw, cbh));
    st.last = m;
    Ok(())
}

/// Colour component (0 luma, 1 Cb, 2 Cr) of block `i` of a macroblock.
fn block_component(i: usize) -> usize {
    if i < 4 { 0 } else { 1 + (i & 1) }
}

/// How a macroblock's motion vectors are sent.
struct MvLayout {
    /// Vectors per direction: 1, or 2 for field/16x8 prediction.
    count: usize,
    field_fmt: bool,
    dual: bool,
}

impl MvLayout {
    fn of(frame_pic: bool, mtype: u8) -> MvLayout {
        let (count, field_fmt, dual) = match (frame_pic, mtype) {
            (true, MT_FIELD) => (2, true, false),
            (true, MT_DUAL) | (false, MT_DUAL) => (1, true, true),
            (true, _) => (1, false, false),
            (false, MT_16X8) => (2, true, false),
            (false, _) => (1, true, false),
        };
        MvLayout { count, field_fmt, dual }
    }
}

/// Read the motion vectors (and dual-prime vectors) of a macroblock, updating the predictors.
fn read_motion_vectors(p: &PicParams, st: &mut SliceState, b: &mut Bits, m: &mut Motion, concealment: bool) -> R<()> {
    let tb = tables();
    let frame_pic = p.frame_picture();
    let MvLayout { count, field_fmt, dual } = MvLayout::of(frame_pic, m.mtype);
    let flags = m.flags;
    for s in 0..2 {
        let present = if s == 0 { flags & MB_FWD != 0 || concealment } else { flags & MB_BWD != 0 };
        if !present {
            continue;
        }
        for r in 0..count {
            if count == 2 || (field_fmt && !dual) {
                m.select[r][s] = b.read(1) as usize;
            }
            for c in 0..2 {
                let fc = p.pce.f_code[s][c] as i32;
                let code = tb.motion.decode(b).ok_or(SliceError("bad motion_code"))? as i32;
                let r_size = (fc - 1).max(0);
                let residual = if fc != 1 && code != 0 { b.read(r_size as u32) as i32 } else { 0 };
                if dual {
                    m.dmv[c] = tb.dmv.decode(b).ok_or(SliceError("bad dmvector"))? as i32;
                }
                let f = 1 << r_size;
                let delta = if f == 1 || code == 0 { code } else { (((code.abs() - 1) * f) + residual + 1) * code.signum() };
                let halve = p.mpeg2 && field_fmt && c == 1 && frame_pic;
                let pred = if halve { st.pmv[r][s][c] >> 1 } else { st.pmv[r][s][c] };
                let v = wrap_vector(pred, delta, f);
                st.pmv[r][s][c] = if halve { v * 2 } else { v };
                m.mv[r][s][c] = v;
            }
        }
        if count == 1 {
            st.pmv[1][s] = st.pmv[0][s];
        }
        if !p.mpeg2 && p.full_pel[s] {
            for c in 0..2 {
                m.mv[0][s][c] *= 2;
            }
        }
    }
    Ok(())
}

/// coded_block_pattern: one bit per block, first block in the most significant bit.
fn read_cbp(p: &PicParams, b: &mut Bits, flags: i16, intra: bool) -> R<u32> {
    let nblocks = 4 + p.chroma_blocks();
    Ok(if flags & MB_PATTERN != 0 {
        let v = tables().cbp.decode(b).ok_or(SliceError("bad coded_block_pattern"))? as u32;
        if nblocks == 8 { (v << 2) | b.read(2) } else { v }
    } else if intra {
        (1 << nblocks) - 1
    } else {
        0
    })
}

/// Write (intra) or add (inter) the decoded block `i` into the macroblock buffer.
fn place_block(buf: &mut MbBuf, blk: &[i32; 64], i: usize, cmb: (usize, usize), dct_type: bool, intra: bool) {
    let (cbw, cbh) = cmb;
    let cc = block_component(i);
    let (dst, stride, x0, y0, step) = if i < 4 {
        let x0 = (i & 1) * 8;
        let (y0, step) = if dct_type { (i >> 1, 2) } else { ((i >> 1) * 8, 1) };
        (&mut buf.y[..], 16, x0, y0, step)
    } else {
        let k = (i - 4) >> 1; // 0 for 4:2:0; 0/1 (top/bottom) for 4:2:2
        let (y0, step) = if cbh == 16 && dct_type { (k, 2) } else { (k * 8, 1) };
        (&mut buf.c[cc - 1][..], cbw, 0, y0, step)
    };
    for r in 0..8 {
        let at = (y0 + r * step) * stride + x0;
        let row = &mut dst[at..at + 8];
        let src = &blk[r * 8..r * 8 + 8];
        if intra {
            for (o, &v) in row.iter_mut().zip(src) {
                *o = v.clamp(0, 255) as u8;
            }
        } else {
            for (o, &v) in row.iter_mut().zip(src) {
                *o = (*o as i32 + v).clamp(0, 255) as u8;
            }
        }
    }
}

/// Decode and dequantise one block into `blk` (raster order). Returns whether only the DC
/// coefficient is non-zero.
fn block(p: &PicParams, st: &mut SliceState, b: &mut Bits, cc: usize, intra: bool, blk: &mut [i32; 64]) -> R<bool> {
    let tb = tables();
    blk.fill(0);
    let scan = &SCAN[p.pce.alternate_scan as usize];
    let w = &p.matrices[(cc > 0 && p.shy == 0) as usize * 2 + (!intra) as usize];
    let qs = st.qscale;
    let mut sum: i32 = 0;
    let mut i: usize;
    let mut ac = false;
    if intra {
        let size = if cc == 0 { tb.dc_luma.decode(b) } else { tb.dc_chroma.decode(b) }.ok_or(SliceError("bad dct_dc_size"))? as u32;
        let diff = if size == 0 {
            0
        } else {
            let v = b.read(size) as i32;
            if v & (1 << (size - 1)) == 0 { v - (1 << size) + 1 } else { v }
        };
        st.dc_pred[cc] += diff;
        let dc = if p.mpeg2 { st.dc_pred[cc] * (8 >> p.pce.intra_dc_precision) } else { st.dc_pred[cc] * 8 };
        blk[0] = dc;
        sum = dc;
        if p.ptype == PictureType::D {
            return Ok(true);
        }
        i = 0;
    } else {
        i = usize::MAX; // the first coefficient lands at run
        // "1s": run 0, level ±1 as the first coefficient of a non-intra block
        if b.peek(1) == 1 {
            b.skip(1);
            let level = if b.bit() { -1 } else { 1 };
            i = 0;
            let v = dequant(p, level, qs, w[scan[0] as usize] as i32, false);
            blk[scan[0] as usize] = v;
            sum += v;
            ac = scan[0] != 0;
        }
    }
    let table = if intra && p.pce.intra_vlc_format { &tb.dct_one } else { &tb.dct_zero };
    loop {
        let code = table.decode(b).ok_or(SliceError("bad DCT coefficient code"))?;
        if code == DCT_EOB {
            break;
        }
        let (run, level) = if code == DCT_ESCAPE {
            let run = b.read(6) as usize;
            let level = if p.mpeg2 {
                b.read_signed(12)
            } else {
                match b.read(8) as i32 {
                    0 => b.read(8) as i32,
                    128 => b.read(8) as i32 - 256,
                    v => (v << 24) >> 24,
                }
            };
            if level == 0 {
                return Err(SliceError("zero escape level"));
            }
            (run, level)
        } else {
            let run = (code & 63) as usize;
            let level = (code >> 6) as i32;
            (run, if b.bit() { -level } else { level })
        };
        i = i.wrapping_add(run + 1);
        if i > 63 {
            return Err(SliceError("coefficient index past 63"));
        }
        let pos = scan[i] as usize;
        let v = dequant(p, level, qs, w[pos] as i32, intra);
        blk[pos] = v;
        sum += v;
        ac = true;
        if b.overrun() {
            return Err(SliceError("truncated block"));
        }
    }
    if p.mpeg2 && sum & 1 == 0 {
        // mismatch control (§7.4.4)
        blk[63] ^= 1;
        ac = true;
    }
    Ok(!ac && blk[1..].iter().all(|&v| v == 0))
}

/// `prediction + delta`, brought back into [-16f, 16f-1] (§7.6.3.1). The range is restored on
/// the side the delta moves the vector, and a zero delta keeps the prediction, as the MPEG
/// Software Simulation Group reference decoder (ISO/IEC TR 13818-5) does. This only matters when
/// the prediction itself is out of range: a field vector's doubled vertical predictor before a
/// frame vector.
#[inline(always)]
pub(crate) fn wrap_vector(pred: i32, delta: i32, f: i32) -> i32 {
    let v = pred + delta;
    if delta > 0 && v > 16 * f - 1 {
        v - 32 * f
    } else if delta < 0 && v < -16 * f {
        v + 32 * f
    } else {
        v
    }
}

#[inline(always)]
fn dequant(p: &PicParams, level: i32, qs: i32, w: i32, intra: bool) -> i32 {
    if p.mpeg2 {
        let k = if intra { 0 } else { level.signum() };
        ((2 * level + k) * w * qs / 32).clamp(-2048, 2047)
    } else {
        let mut v = if intra { 2 * level * qs * w / 16 } else { (2 * level + level.signum()) * qs * w / 16 };
        if v & 1 == 0 {
            v -= v.signum();
        }
        v.clamp(-2048, 2047)
    }
}

/// The reference field (or frame) for direction `s` and field `sel` (field prediction).
fn reference<'a>(p: &PicParams<'a>, s: usize, sel: usize) -> Option<&'a Frame> {
    if s == 1 {
        return p.bwd.or(p.fwd);
    }
    if p.ptype == PictureType::P && !p.frame_picture() && sel != p.parity && p.first_field.is_some() {
        return p.first_field;
    }
    p.fwd.or(p.bwd)
}

/// Form the prediction of a non-intra macroblock in `buf` (§7.6).
fn predict_mb(p: &PicParams, m: &Motion, mbx: usize, mby: usize, buf: &mut MbBuf) {
    let (cbw, cbh) = p.cmb();
    let mb = Pred { mv: [0; 2], mbx, mby, cbw, cbh, avg: false };
    let frame_pic = p.frame_picture();
    let mut avg = false;
    for s in 0..2 {
        let dir = if s == 0 { MB_FWD } else { MB_BWD };
        if m.flags & dir == 0 {
            continue;
        }
        if frame_pic {
            match m.mtype {
                MT_FIELD => {
                    for r in 0..2 {
                        let Some(f) = reference(p, s, m.select[r][s]) else { return gray(buf) };
                        pred_block(p, buf, f, Some(m.select[r][s]), Pred { mv: m.mv[r][s], avg, ..mb }, (8, r, 2));
                    }
                }
                MT_DUAL => {
                    let Some(f) = reference(p, s, 0) else { return gray(buf) };
                    for par in 0..2 {
                        let opp = 1 - par;
                        let (mm, e) = if par == 0 { (if p.pce.top_field_first { 1 } else { 3 }, -1) } else { (if p.pce.top_field_first { 3 } else { 1 }, 1) };
                        let d = dual_vector(m.mv[0][s], m.dmv, mm, e);
                        pred_block(p, buf, f, Some(par), Pred { mv: m.mv[0][s], avg, ..mb }, (8, par, 2));
                        pred_block(p, buf, f, Some(opp), Pred { mv: d, avg: true, ..mb }, (8, par, 2));
                    }
                }
                _ => {
                    let Some(f) = reference(p, s, 0) else { return gray(buf) };
                    pred_block(p, buf, f, None, Pred { mv: m.mv[0][s], avg, ..mb }, (16, 0, 1));
                }
            }
        } else {
            match m.mtype {
                MT_16X8 => {
                    for r in 0..2 {
                        let Some(f) = reference(p, s, m.select[r][s]) else { return gray(buf) };
                        pred_half(p, buf, f, m.select[r][s], Pred { mv: m.mv[r][s], avg, ..mb }, r);
                    }
                }
                MT_DUAL => {
                    let same = p.parity;
                    let opp = 1 - same;
                    let Some(fs) = reference(p, s, same) else { return gray(buf) };
                    let Some(fo) = reference(p, s, opp) else { return gray(buf) };
                    let e = if same == 1 { 1 } else { -1 };
                    let d = dual_vector(m.mv[0][s], m.dmv, 1, e);
                    pred_block(p, buf, fs, Some(same), Pred { mv: m.mv[0][s], avg, ..mb }, (16, 0, 1));
                    pred_block(p, buf, fo, Some(opp), Pred { mv: d, avg: true, ..mb }, (16, 0, 1));
                }
                _ => {
                    let Some(f) = reference(p, s, m.select[0][s]) else { return gray(buf) };
                    pred_block(p, buf, f, Some(m.select[0][s]), Pred { mv: m.mv[0][s], avg, ..mb }, (16, 0, 1));
                }
            }
        }
        avg = true;
    }
}

fn gray(buf: &mut MbBuf) {
    buf.y.fill(128);
    buf.c[0].fill(128);
    buf.c[1].fill(128);
}

/// Dual-prime opposite-parity vector (§7.6.3.6).
fn dual_vector(v: [i32; 2], dmv: [i32; 2], m: i32, e: i32) -> [i32; 2] {
    let scale = |x: i32| (x * m + (x > 0) as i32) >> 1;
    [scale(v[0]) + dmv[0], scale(v[1]) + e + dmv[1]]
}

/// One motion-compensated prediction: vector, macroblock position, chroma block size, and
/// whether to average with what the buffer already holds.
#[derive(Clone, Copy)]
struct Pred {
    mv: [i32; 2],
    mbx: usize,
    mby: usize,
    cbw: usize,
    cbh: usize,
    avg: bool,
}

/// Predict a 16-wide luma block of `lh` lines (plus chroma) from `f` (one field when `field`),
/// into `buf` lines `first + k·step`; `lines` is `(lh, first, step)`. `lh` is 16 (frame or
/// field-picture prediction) or 8 (one field of a frame macroblock).
fn pred_block(p: &PicParams, buf: &mut MbBuf, f: &Frame, field: Option<usize>, pr: Pred, lines: (usize, usize, usize)) {
    let Pred { mv, mbx, mby, cbw, cbh, avg } = pr;
    let (lh, first, step) = lines;
    // luma: base position in the reference plane's own (frame or field) lines
    let by = if lh == 8 { mby * 8 } else { mby * 16 };
    let (x, y) = ((mbx * 16) as i32 + (mv[0] >> 1), by as i32 + (mv[1] >> 1));
    predict(&mut buf.y, first * 16, 16 * step, f.plane(0, field), Block { x, y, hx: mv[0] & 1 != 0, hy: mv[1] & 1 != 0, bw: 16, bh: lh }, avg);
    let cmv = [if p.shx == 1 { mv[0] / 2 } else { mv[0] }, if p.shy == 1 { mv[1] / 2 } else { mv[1] }];
    let clh = if lh == 8 { cbh / 2 } else { cbh };
    let cby = mby * clh;
    let (cx, cy) = ((mbx * cbw) as i32 + (cmv[0] >> 1), cby as i32 + (cmv[1] >> 1));
    let cb = Block { x: cx, y: cy, hx: cmv[0] & 1 != 0, hy: cmv[1] & 1 != 0, bw: cbw, bh: clh };
    for c in 0..2 {
        predict(&mut buf.c[c], first * cbw, cbw * step, f.plane(1 + c, field), cb, avg);
    }
}

/// 16x8 motion compensation in a field picture: half `r` (0 upper, 1 lower) of the macroblock.
fn pred_half(p: &PicParams, buf: &mut MbBuf, f: &Frame, field: usize, pr: Pred, r: usize) {
    let Pred { mv, mbx, mby, cbw, cbh, avg } = pr;
    let (x, y) = ((mbx * 16) as i32 + (mv[0] >> 1), (mby * 16 + r * 8) as i32 + (mv[1] >> 1));
    predict(&mut buf.y, r * 8 * 16, 16, f.plane(0, Some(field)), Block { x, y, hx: mv[0] & 1 != 0, hy: mv[1] & 1 != 0, bw: 16, bh: 8 }, avg);
    let cmv = [if p.shx == 1 { mv[0] / 2 } else { mv[0] }, if p.shy == 1 { mv[1] / 2 } else { mv[1] }];
    let ch = cbh / 2;
    let (cx, cy) = ((mbx * cbw) as i32 + (cmv[0] >> 1), (mby * cbh + r * ch) as i32 + (cmv[1] >> 1));
    let cb = Block { x: cx, y: cy, hx: cmv[0] & 1 != 0, hy: cmv[1] & 1 != 0, bw: cbw, bh: ch };
    for c in 0..2 {
        predict(&mut buf.c[c], r * ch * cbw, cbw, f.plane(1 + c, Some(field)), cb, avg);
    }
}

/// The macroblock row a slice starts in (slice_vertical_position and its extension).
pub(crate) fn slice_row(vertical_size_extension: bool, code: u8, payload: &[u8]) -> usize {
    let mut row = code as usize - 1;
    if vertical_size_extension && !payload.is_empty() {
        row += ((payload[0] >> 5) as usize) << 7;
    }
    row
}
