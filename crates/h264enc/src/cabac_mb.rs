//! CABAC binarisation and context selection for macroblock-layer syntax elements (§9.3.2, §9.3.3.1).

use crate::cabac::BinSink;
use crate::mbinfo::{MbInfo, MbKind, Part};
use crate::nal::SliceType;
use crate::syntax::{MbCode, NbCtx};
use crate::tables::{BLK4_XY, LAST8X8, SIG8X8_FRAME};

pub fn write_skip_flag<S: BinSink>(s: &mut S, nb: &NbCtx, skip: bool) {
    let cond = |m: Option<&MbInfo>| m.is_some_and(|m| !m.kind.is_skip()) as usize;
    let base = if nb.slice_type == SliceType::B { 24 } else { 11 };
    s.decision(base + cond(nb.left) + cond(nb.top), skip as u32);
}

fn eg_bypass<S: BinSink>(s: &mut S, mut v: u32, mut k: u32) {
    loop {
        if v >= (1 << k) {
            s.bypass(1);
            v -= 1 << k;
            k += 1;
        } else {
            s.bypass(0);
            while k > 0 {
                k -= 1;
                s.bypass((v >> k) & 1);
            }
            break;
        }
    }
}

/// I16x16 / I_NxN mb_type bins. `off` = ctxIdxOffset of the (suffix) table; `i_slice` selects I-slice contexts.
fn write_intra_type<S: BinSink>(s: &mut S, nb: &NbCtx, c: &MbCode) {
    let i_slice = nb.slice_type == SliceType::I;
    let (off, b0ctx) = match nb.slice_type {
        SliceType::I => {
            let cond = |m: Option<&MbInfo>| m.is_some_and(|m| !m.kind.is_inxn()) as usize;
            (3, 3 + cond(nb.left) + cond(nb.top))
        }
        SliceType::P => {
            s.decision(14, 1); // prefix: intra
            (17, 17)
        }
        SliceType::B => {
            // prefix 111101
            write_b_bins(s, nb, &[1, 1, 1, 1, 0, 1]);
            (32, 32)
        }
    };
    if c.kind != MbKind::I16x16 {
        s.decision(b0ctx, 0);
        return;
    }
    s.decision(b0ctx, 1);
    s.terminate(0);
    let luma = (c.cbp & 15 != 0) as u32;
    let chroma = (c.cbp >> 4) as u32;
    let pred = c.i16_mode as u32;
    if i_slice {
        s.decision(off + 3, luma);
        s.decision(off + 4, (chroma != 0) as u32);
        if chroma != 0 {
            s.decision(off + 5, (chroma == 2) as u32);
            s.decision(off + 6, pred >> 1);
            s.decision(off + 7, pred & 1);
        } else {
            s.decision(off + 6, pred >> 1);
            s.decision(off + 7, pred & 1);
        }
    } else {
        s.decision(off + 1, luma);
        s.decision(off + 2, (chroma != 0) as u32);
        if chroma != 0 {
            s.decision(off + 2, (chroma == 2) as u32);
        }
        s.decision(off + 3, pred >> 1);
        s.decision(off + 3, pred & 1);
    }
}

fn write_b_bins<S: BinSink>(s: &mut S, nb: &NbCtx, bins: &[u32]) {
    for (i, &b) in bins.iter().enumerate() {
        let ctx = match i {
            0 => {
                let cond = |m: Option<&MbInfo>| m.is_some_and(|m| !matches!(m.kind, MbKind::BSkip | MbKind::BDirect)) as usize;
                27 + cond(nb.left) + cond(nb.top)
            }
            1 => 27 + 3,
            2 => 27 + if bins[1] != 0 { 4 } else { 5 },
            _ => 27 + 5,
        };
        s.decision(ctx, b);
    }
}

const B_BINS: [&[u32]; 23] = [
    &[0],
    &[1, 0, 0],
    &[1, 0, 1],
    &[1, 1, 0, 0, 0, 0],
    &[1, 1, 0, 0, 0, 1],
    &[1, 1, 0, 0, 1, 0],
    &[1, 1, 0, 0, 1, 1],
    &[1, 1, 0, 1, 0, 0],
    &[1, 1, 0, 1, 0, 1],
    &[1, 1, 0, 1, 1, 0],
    &[1, 1, 0, 1, 1, 1],
    &[1, 1, 1, 1, 1, 0],
    &[1, 1, 1, 0, 0, 0, 0],
    &[1, 1, 1, 0, 0, 0, 1],
    &[1, 1, 1, 0, 0, 1, 0],
    &[1, 1, 1, 0, 0, 1, 1],
    &[1, 1, 1, 0, 1, 0, 0],
    &[1, 1, 1, 0, 1, 0, 1],
    &[1, 1, 1, 0, 1, 1, 0],
    &[1, 1, 1, 0, 1, 1, 1],
    &[1, 1, 1, 1, 0, 0, 0],
    &[1, 1, 1, 1, 0, 0, 1],
    &[1, 1, 1, 1, 1, 1],
];

fn write_mvd<S: BinSink>(s: &mut S, comp: usize, v: i32, sum: u32) {
    let base = if comp == 0 { 40 } else { 47 };
    let a = v.unsigned_abs();
    let inc = if sum < 3 {
        0
    } else if sum > 32 {
        2
    } else {
        1
    };
    let u = a.min(9);
    s.decision(base + inc, (u > 0) as u32);
    if u > 0 {
        for k in 1..9u32 {
            let ctx = base + [3, 4, 5, 6, 6, 6, 6, 6][(k - 1) as usize];
            if k < u {
                s.decision(ctx, 1);
            } else {
                s.decision(ctx, 0);
                break;
            }
        }
        if a >= 9 {
            eg_bypass(s, a - 9, 3);
        }
        s.bypass((v < 0) as u32);
    }
}

/// Sum of |mvd| of neighbours A and B for the 4x4 block at raster `r` (current MB info already holds earlier partitions).
fn mvd_sum(nb: &NbCtx, r: usize, list: usize, comp: usize) -> u32 {
    let (x, y) = (r & 3, r >> 2);
    let a = if x > 0 { nb.cur.mvd[list][r - 1][comp] } else { nb.left.map_or(0, |m| m.mvd[list][y * 4 + 3][comp]) };
    let b = if y > 0 { nb.cur.mvd[list][r - 4][comp] } else { nb.top.map_or(0, |m| m.mvd[list][12 + x][comp]) };
    a as u32 + b as u32
}

fn write_cbp<S: BinSink>(s: &mut S, nb: &NbCtx, cbp: u8) {
    let luma_bit = |m: &MbInfo, b8: usize| (m.cbp >> b8) & 1;
    for b8 in 0..4usize {
        // (A, B) neighbour 8x8 blocks: (Some(mb), idx) where None = current MB
        let cond = |m: Option<&MbInfo>, idx: usize, in_cur: bool| -> usize {
            if in_cur {
                ((cbp >> idx) & 1 == 0) as usize
            } else {
                match m {
                    None => 0,
                    Some(m) => (luma_bit(m, idx) == 0) as usize,
                }
            }
        };
        let (ca, cb) = match b8 {
            0 => (cond(nb.left, 1, false), cond(nb.top, 2, false)),
            1 => (cond(None, 0, true), cond(nb.top, 3, false)),
            2 => (cond(nb.left, 3, false), cond(None, 0, true)),
            _ => (cond(None, 2, true), cond(None, 1, true)),
        };
        s.decision(73 + ca + 2 * cb, ((cbp >> b8) & 1) as u32);
    }
    let chroma = cbp >> 4;
    let cc = |m: Option<&MbInfo>, two: bool| -> usize {
        m.is_some_and(|m| {
            let c = m.cbp >> 4;
            !m.kind.is_skip() && if two { c == 2 } else { c != 0 }
        }) as usize
    };
    s.decision(77 + cc(nb.left, false) + 2 * cc(nb.top, false), (chroma != 0) as u32);
    if chroma != 0 {
        s.decision(77 + 4 + cc(nb.left, true) + 2 * cc(nb.top, true), (chroma == 2) as u32);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cat {
    LumaDc = 0,
    LumaAc = 1,
    Luma4x4 = 2,
    ChromaDc = 3,
    ChromaAc = 4,
    Luma8x8 = 5,
}

/// Encode one residual block; `coeffs` are the block's coefficients in scan order (length = maxNumCoeff).
pub fn write_residual<S: BinSink>(s: &mut S, cat: Cat, coeffs: &[i16], cbf_inc: u32) {
    const CBF_OFF: [usize; 5] = [0, 4, 8, 12, 16];
    const SIG_OFF: [usize; 5] = [0, 15, 29, 44, 47];
    const ABS_OFF: [usize; 5] = [0, 10, 20, 30, 39];
    let n = coeffs.len();
    let last = coeffs.iter().rposition(|&c| c != 0);
    let ci = cat as usize;
    if cat != Cat::Luma8x8 {
        s.decision(85 + CBF_OFF[ci] + cbf_inc as usize, last.is_some() as u32);
    }
    let Some(last) = last else { return };
    let (sig_base, last_base, abs_base) = if cat == Cat::Luma8x8 { (402, 417, 426) } else { (105 + SIG_OFF[ci], 166 + SIG_OFF[ci], 227 + ABS_OFF[ci]) };
    for i in 0..n - 1 {
        let (si, li) = match cat {
            Cat::ChromaDc => (i.min(2), i.min(2)),
            Cat::Luma8x8 => (SIG8X8_FRAME[i] as usize, LAST8X8[i] as usize),
            _ => (i, i),
        };
        let sig = coeffs[i] != 0;
        s.decision(sig_base + si, sig as u32);
        if sig {
            let is_last = i == last;
            s.decision(last_base + li, is_last as u32);
            if is_last {
                break;
            }
        }
    }
    let mut gt1 = 0usize;
    let mut eq1 = 0usize;
    let max_gt1 = if cat == Cat::ChromaDc { 3 } else { 4 };
    for i in (0..=last).rev() {
        let c = coeffs[i];
        if c == 0 {
            continue;
        }
        let a = c.unsigned_abs() as u32;
        let v = a - 1;
        let inc0 = if gt1 != 0 { 0 } else { (1 + eq1).min(4) };
        s.decision(abs_base + inc0, (v > 0) as u32);
        if v > 0 {
            let ctx = abs_base + 5 + gt1.min(max_gt1);
            let u = v.min(14);
            for _ in 1..u {
                s.decision(ctx, 1);
            }
            if u < 14 {
                s.decision(ctx, 0);
            } else {
                eg_bypass(s, v - 14, 0);
            }
        }
        s.bypass((c < 0) as u32);
        if a == 1 {
            eq1 += 1;
        } else {
            gt1 += 1;
        }
    }
}

fn cbf_luma(nb: &NbCtx, raster: usize) -> u32 {
    let f = |v: Option<u8>| v.map_or(nb.unavail_cbf(), |n| (n != 0) as u32);
    f(nb.luma_nnz(raster, true)) + 2 * f(nb.luma_nnz(raster, false))
}

fn cbf_chroma_ac(nb: &NbCtx, c: usize, b: usize) -> u32 {
    let f = |v: Option<u8>| v.map_or(nb.unavail_cbf(), |n| (n != 0) as u32);
    f(nb.chroma_nnz(c, b, true)) + 2 * f(nb.chroma_nnz(c, b, false))
}

fn cbf_dc(nb: &NbCtx, bit: u8) -> u32 {
    let f = |m: Option<&MbInfo>| m.map_or(nb.unavail_cbf(), |m| ((m.dc_cbf >> bit) & 1) as u32);
    f(nb.left) + 2 * f(nb.top)
}

/// mb_type (and the P_L0_8x8 sub_mb_types of a P_8x8 macroblock).
fn write_mb_type<S: BinSink>(s: &mut S, nb: &NbCtx, c: &MbCode) {
    match c.kind {
        MbKind::I4x4 | MbKind::I8x8 | MbKind::I16x16 => write_intra_type(s, nb, c),
        MbKind::PInter => {
            let bins: [u32; 3] = match c.part {
                Part::P16x16 => [0, 0, 0],
                Part::P16x8 => [0, 1, 1],
                Part::P8x16 => [0, 1, 0],
                Part::P8x8 => [0, 0, 1],
            };
            s.decision(14, bins[0]);
            s.decision(15, bins[1]);
            s.decision(if bins[1] != 1 { 16 } else { 17 }, bins[2]);
        }
        MbKind::BDirect | MbKind::BInter => write_b_bins(s, nb, B_BINS[c.b_type_number() as usize]),
        // Skip / none macroblocks are not written with write_mb.
        _ => debug_assert!(false, "write_mb on a skipped macroblock"),
    }
    if c.kind == MbKind::PInter && c.part == Part::P8x8 {
        for _ in 0..4 {
            s.decision(21, 1); // P_L0_8x8
        }
    }
}

/// transform_size_8x8_flag.
fn write_transform_size_flag<S: BinSink>(s: &mut S, nb: &NbCtx, c: &MbCode) {
    let cond = |m: Option<&MbInfo>| m.is_some_and(|m| m.t8x8) as usize;
    s.decision(399 + cond(nb.left) + cond(nb.top), c.t8x8 as u32);
}

/// prev_intra_NxN_pred_mode flags / rem_intra_NxN_pred_mode values.
fn write_intra_pred_modes<S: BinSink>(s: &mut S, c: &MbCode) {
    let n = if c.t8x8 { 4 } else { 16 };
    for i in 0..n {
        let p = c.ipred[i];
        if p < 0 {
            s.decision(68, 1);
        } else {
            s.decision(68, 0);
            s.decision(69, (p & 1) as u32);
            s.decision(69, ((p >> 1) & 1) as u32);
            s.decision(69, ((p >> 2) & 1) as u32);
        }
    }
}

/// intra_chroma_pred_mode.
fn write_chroma_pred_mode<S: BinSink>(s: &mut S, nb: &NbCtx, c: &MbCode) {
    let cond = |m: Option<&MbInfo>| m.is_some_and(|m| m.kind.is_intra() && m.chroma_mode != 0) as usize;
    let m = c.chroma_mode as u32;
    s.decision(64 + cond(nb.left) + cond(nb.top), (m > 0) as u32);
    if m > 0 {
        s.decision(64 + 3, (m > 1) as u32);
        if m > 1 {
            s.decision(64 + 3, (m > 2) as u32);
        }
    }
}

/// mvd_lX components of every partition that uses each list.
fn write_mvds<S: BinSink>(s: &mut S, nb: &NbCtx, c: &MbCode) {
    for list in 0..2 {
        for p in 0..c.part.count() {
            if !c.uses_list(p, list) {
                continue;
            }
            let (px, py, _, _) = c.part.rect(p);
            let r = (py / 4) * 4 + px / 4;
            let mvd = c.mvd[list][p];
            write_mvd(s, 0, mvd.x as i32, mvd_sum(nb, r, list, 0));
            write_mvd(s, 1, mvd.y as i32, mvd_sum(nb, r, list, 1));
        }
    }
}

/// mb_qp_delta.
fn write_qp_delta<S: BinSink>(s: &mut S, nb: &NbCtx, v: i32) {
    let m = if v > 0 { 2 * v as u32 - 1 } else { (-2 * v) as u32 };
    s.decision(60 + nb.prev_dqp_nz as usize, (m > 0) as u32);
    if m > 0 {
        for k in 1..=m {
            s.decision(if k == 1 { 62 } else { 63 }, (k < m) as u32);
        }
    }
}

/// Luma residual blocks of the macroblock.
fn write_luma_residual<S: BinSink>(s: &mut S, nb: &NbCtx, c: &MbCode) {
    if c.kind == MbKind::I16x16 {
        write_residual(s, Cat::LumaDc, &c.luma_dc, cbf_dc(nb, 0));
        if c.cbp & 15 != 0 {
            for blk in 0..16 {
                let (x, y) = BLK4_XY[blk];
                write_residual(s, Cat::LumaAc, &c.luma4[blk][1..16], cbf_luma(nb, y * 4 + x));
            }
        }
        return;
    }
    for b8 in 0..4 {
        if (c.cbp >> b8) & 1 == 0 {
            continue;
        }
        if c.t8x8 {
            write_residual(s, Cat::Luma8x8, &c.luma8[b8], 0);
        } else {
            for i in 0..4 {
                let blk = b8 * 4 + i;
                let (x, y) = BLK4_XY[blk];
                write_residual(s, Cat::Luma4x4, &c.luma4[blk], cbf_luma(nb, y * 4 + x));
            }
        }
    }
}

/// Chroma DC / AC residual blocks of the macroblock.
fn write_chroma_residual<S: BinSink>(s: &mut S, nb: &NbCtx, c: &MbCode) {
    let chroma = c.cbp >> 4;
    if chroma != 0 {
        for comp in 0..2 {
            write_residual(s, Cat::ChromaDc, &c.cdc[comp], cbf_dc(nb, 1 + comp as u8));
        }
    }
    if chroma == 2 {
        for comp in 0..2 {
            for b in 0..4 {
                write_residual(s, Cat::ChromaAc, &c.cac[comp * 4 + b][1..16], cbf_chroma_ac(nb, comp, b));
            }
        }
    }
}

/// Write the whole macroblock layer (after mb_skip_flag = 0 in P/B slices).
pub fn write_mb<S: BinSink>(s: &mut S, nb: &NbCtx, c: &MbCode) {
    write_mb_type(s, nb, c);
    let is_inxn = c.kind.is_inxn();
    // ---- mb_pred
    if is_inxn {
        if nb.t8x8_mode {
            write_transform_size_flag(s, nb, c);
        }
        write_intra_pred_modes(s, c);
    }
    if c.kind.is_intra() {
        write_chroma_pred_mode(s, nb, c);
    }
    if matches!(c.kind, MbKind::PInter | MbKind::BInter) {
        write_mvds(s, nb, c);
    }
    // ---- cbp, transform size, qp delta
    if c.kind != MbKind::I16x16 {
        write_cbp(s, nb, c.cbp);
        if c.cbp & 15 != 0 && nb.t8x8_mode && !is_inxn {
            write_transform_size_flag(s, nb, c);
        }
    }
    if c.cbp == 0 && c.kind != MbKind::I16x16 {
        return;
    }
    write_qp_delta(s, nb, c.qp_delta);
    // ---- residual
    write_luma_residual(s, nb, c);
    write_chroma_residual(s, nb, c);
}
