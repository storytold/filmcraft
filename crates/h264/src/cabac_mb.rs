//! CABAC slice data and macroblock layer parsing (7.3.4, 7.3.5 with 9.3 binarisations and context
//! index derivations).

use crate::cabac::Cabac;
use crate::error::{Result, ensure, invalid};
use crate::mbtypes::*;
use crate::picture::MbKind;
use crate::slice::SliceType;
use crate::slicedec::{MbCur, SliceDecoder};

/// Table 9-43: significant_coeff_flag ctxIdxInc for 8x8 frame blocks.
#[rustfmt::skip]
const SIG8_FRAME: [u8; 63] = [
    0, 1, 2, 3, 4, 5, 5, 4, 4, 3, 3, 4, 4, 4, 5, 5, 4, 4, 4, 4, 3, 3, 6, 7, 7, 7, 8, 9, 10, 9, 8, 7,
    7, 6, 11, 12, 13, 11, 6, 7, 8, 9, 14, 10, 9, 8, 6, 11, 12, 13, 11, 6, 9, 14, 10, 9, 11, 12, 13, 11, 14, 10, 12,
];
/// Table 9-43: last_significant_coeff_flag ctxIdxInc for 8x8 blocks.
#[rustfmt::skip]
const LAST8: [u8; 63] = [
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8,
];

/// ctxIdxInc = levelListIdx (ctxBlockCat 0, 1, 2, 4).
const IDENT_CTX: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = i as u8;
        i += 1;
    }
    t
};
/// ctxIdxInc = Min(levelListIdx / NumC8x8, 2) for chroma DC (ctxBlockCat 3), 4:2:0 (NumC8x8 = 1).
const CHROMA_DC_CTX: [u8; 4] = [0, 1, 2, 2];
/// The same derivation (9-22) for 4:2:2 (NumC8x8 = 2): 8 coefficients, 7 flagged positions.
const CHROMA_DC_CTX_422: [u8; 7] = [0, 0, 1, 1, 2, 2, 2];

/// Table 9-40 ctxIdxBlockCatOffset for ctxBlockCat 0..=5: (coded_block_flag, significant/last, abs level).
const CAT_OFFSET: [(usize, usize, usize); 6] = [(0, 0, 0), (4, 15, 10), (8, 29, 20), (12, 44, 30), (16, 47, 39), (0, 0, 0)];

impl SliceDecoder<'_> {
    /// Decode slice_data() with CABAC. `rbsp` is the whole slice RBSP.
    pub fn decode_cabac(&mut self, rbsp: &[u8]) -> Result<()> {
        let start = self.sh.header_bits.div_ceil(8) * 8;
        let init = if self.sh.slice_type.is_intra() { 3 } else { self.sh.cabac_init_idc as usize };
        let mut c = Cabac::new(rbsp, start, self.sh.qp(self.pps), init)?;
        let total = self.mb_w * self.mb_h;
        let mut addr = self.sh.first_mb_in_slice as usize;
        let intra_slice = self.sh.slice_type.is_intra();
        loop {
            ensure!(addr < total, "macroblock address {addr} out of range");
            self.start_mb(addr);
            let skip = !intra_slice && self.decode_skip_flag(&mut c);
            if skip {
                self.decode_skip()?;
            } else {
                self.decode_mb_cabac(&mut c)?;
            }
            self.pic.mb_done(addr);
            ensure!(!c.overrun(), "CABAC read past end of slice data");
            if c.decode_terminate() == 1 {
                break;
            }
            addr += 1;
        }
        Ok(())
    }

    fn decode_skip_flag(&mut self, c: &mut Cabac) -> bool {
        let cond = |n: Option<usize>| n.map(|m| !self.pic.mbs[m].kind_is_skip()).unwrap_or(false) as usize;
        let inc = cond(self.nb[0]) + cond(self.nb[1]);
        let base = if self.sh.slice_type.is_b() { 24 } else { 11 };
        c.decode_decision(base + inc) == 1
    }

    /// Intra mb_type suffix / I-slice mb_type (Table 9-36). `base` is 3 (I slice) or 17/32 (prefix
    /// offsets in P/B slices). Returns the I-slice mb_type value 0..=25.
    fn decode_mb_type_i(&mut self, c: &mut Cabac, base: usize) -> u32 {
        let islice = base == 3;
        let b0 = if islice {
            let cond = |n: Option<usize>| n.map(|m| !matches!(self.pic.mbs[m].kind, MbKind::I4x4 | MbKind::I8x8)).unwrap_or(false) as usize;
            let inc = cond(self.nb[0]) + cond(self.nb[1]);
            c.decode_decision(base + inc)
        } else {
            c.decode_decision(base)
        };
        if b0 == 0 {
            return 0;
        }
        if c.decode_terminate() == 1 {
            return 25;
        }
        let (c_luma, c_chroma, c_chroma2, c_pred0, c_pred1) =
            if islice { (base + 3, base + 4, base + 5, base + 6, base + 7) } else { (base + 1, base + 2, base + 2, base + 3, base + 3) };
        let luma = c.decode_decision(c_luma);
        let mut chroma = c.decode_decision(c_chroma);
        if chroma != 0 {
            chroma += c.decode_decision(c_chroma2);
        }
        let p0 = c.decode_decision(c_pred0);
        let p1 = c.decode_decision(c_pred1);
        1 + (p0 * 2 + p1) + 4 * chroma + 12 * luma
    }

    fn decode_mb_type(&mut self, c: &mut Cabac) -> Result<MbTypeInfo> {
        let info = match self.sh.slice_type {
            SliceType::I | SliceType::Si => intra_mb_type(self.decode_mb_type_i(c, 3)),
            SliceType::P | SliceType::Sp => {
                if c.decode_decision(14) == 1 {
                    intra_mb_type(self.decode_mb_type_i(c, 17))
                } else if c.decode_decision(15) == 0 {
                    p_mb_type(if c.decode_decision(16) == 0 { 0 } else { 3 })
                } else {
                    p_mb_type(if c.decode_decision(17) == 1 { 1 } else { 2 })
                }
            }
            SliceType::B => {
                let cond = |n: Option<usize>| n.map(|m| !matches!(self.pic.mbs[m].kind, MbKind::BSkip | MbKind::BDirect16x16)).unwrap_or(false) as usize;
                let inc = cond(self.nb[0]) + cond(self.nb[1]);
                if c.decode_decision(27 + inc) == 0 {
                    b_mb_type(0)
                } else if c.decode_decision(27 + 3) == 0 {
                    b_mb_type(1 + c.decode_decision(27 + 5))
                } else {
                    let b2 = c.decode_decision(27 + 4);
                    let b3 = c.decode_decision(27 + 5);
                    let b4 = c.decode_decision(27 + 5);
                    let b5 = c.decode_decision(27 + 5);
                    let v = (b2 << 3) | (b3 << 2) | (b4 << 1) | b5;
                    match v {
                        0b1101 => intra_mb_type(self.decode_mb_type_i(c, 32)),
                        0b1111 => b_mb_type(22),
                        0b1110 => b_mb_type(11),
                        _ if b2 == 0 => b_mb_type(3 + v),
                        _ => {
                            let b6 = c.decode_decision(27 + 5);
                            b_mb_type(12 + (((v & 7) << 1) | b6))
                        }
                    }
                }
            }
        };
        match info {
            Some(i) => Ok(i),
            None => invalid("invalid CABAC mb_type"),
        }
    }

    fn decode_sub_mb_type(&mut self, c: &mut Cabac) -> Result<SubMbInfo> {
        let r = if self.sh.slice_type.is_b() {
            if c.decode_decision(36) == 0 {
                b_sub_mb_type(0)
            } else if c.decode_decision(37) == 0 {
                b_sub_mb_type(1 + c.decode_decision(39))
            } else if c.decode_decision(38) == 1 {
                if c.decode_decision(39) == 1 {
                    b_sub_mb_type(11 + c.decode_decision(39))
                } else {
                    let b4 = c.decode_decision(39);
                    let b5 = c.decode_decision(39);
                    b_sub_mb_type(7 + (b4 << 1) + b5)
                }
            } else {
                let b3 = c.decode_decision(39);
                let b4 = c.decode_decision(39);
                b_sub_mb_type(3 + (b3 << 1) + b4)
            }
        } else if c.decode_decision(21) == 1 {
            p_sub_mb_type(0)
        } else if c.decode_decision(22) == 0 {
            p_sub_mb_type(1)
        } else {
            p_sub_mb_type(if c.decode_decision(23) == 1 { 2 } else { 3 })
        };
        match r {
            Some(s) => Ok(s),
            None => invalid("invalid CABAC sub_mb_type"),
        }
    }

    /// ref_idx_lX for the partition whose top-left 4x4 block is (x, y) (9.3.3.1.1.6).
    fn decode_ref_idx(&mut self, c: &mut Cabac, list: usize, x: usize, y: usize) -> u32 {
        let cond = |xx: i32, yy: i32| -> usize {
            let (mb, bx, by) = if xx < 0 {
                (self.nb[0], 3usize, yy as usize)
            } else if yy < 0 {
                (self.nb[1], xx as usize, 3usize)
            } else {
                (Some(self.mb_addr), xx as usize, yy as usize)
            };
            let Some(m) = mb else { return 0 };
            let st = &self.pic.mbs[m];
            let b8 = (by >> 1) * 2 + (bx >> 1);
            if st.kind.is_intra() || st.kind_is_skip() || st.direct8x8 & (1 << b8) != 0 {
                return 0;
            }
            (st.ref_idx[list][b8] > 0) as usize
        };
        let inc = cond(x as i32 - 1, y as i32) + 2 * cond(x as i32, y as i32 - 1);
        let mut v = 0u32;
        if c.decode_decision(54 + inc) == 1 {
            v = 1;
            if c.decode_decision(54 + 4) == 1 {
                v = 2;
                while c.decode_decision(54 + 5) == 1 {
                    v += 1;
                    if v > 32 {
                        break;
                    }
                }
            }
        }
        v
    }

    /// One mvd component for the (sub-)partition with top-left 4x4 block (x, y) (9.3.3.1.1.7, UEG3).
    fn decode_mvd(&mut self, c: &mut Cabac, list: usize, comp: usize, x: usize, y: usize) -> i32 {
        let absn = |xx: i32, yy: i32| -> u32 {
            let (mb, bx, by) = if xx < 0 {
                (self.nb[0], 3usize, yy as usize)
            } else if yy < 0 {
                (self.nb[1], xx as usize, 3usize)
            } else {
                (Some(self.mb_addr), xx as usize, yy as usize)
            };
            match mb {
                Some(m) => self.pic.mbs[m].mvd[list][by * 4 + bx][comp] as u32,
                None => 0,
            }
        };
        let sum = absn(x as i32 - 1, y as i32) + absn(x as i32, y as i32 - 1);
        let inc = if sum > 32 {
            2
        } else if sum > 2 {
            1
        } else {
            0
        };
        let base = if comp == 0 { 40 } else { 47 };
        if c.decode_decision(base + inc) == 0 {
            return 0;
        }
        let mut prefix = 1;
        while prefix < 9 && c.decode_decision(base + (prefix + 2).min(6)) == 1 {
            prefix += 1;
        }
        let mut val = prefix as i32;
        if prefix >= 9 {
            // EG3 suffix
            let mut k = 3;
            while c.decode_bypass() == 1 {
                val += 1 << k;
                k += 1;
                if k > 24 {
                    break;
                }
            }
            while k > 0 {
                k -= 1;
                val += (c.decode_bypass() as i32) << k;
            }
        }
        if c.decode_bypass() == 1 { -val } else { val }
    }

    fn set_cur_ref(&mut self, list: usize, x: usize, y: usize, w: usize, h: usize, r: i8) {
        let st = self.mb_mut();
        for yy in (y..y + h).step_by(2) {
            for xx in (x..x + w).step_by(2) {
                st.ref_idx[list][(yy >> 1) * 2 + (xx >> 1)] = r;
            }
        }
    }

    fn set_cur_mvd(&mut self, list: usize, x: usize, y: usize, w: usize, h: usize, d: [i32; 2]) {
        let a = [d[0].unsigned_abs().min(127) as u8, d[1].unsigned_abs().min(127) as u8];
        let st = self.mb_mut();
        for yy in y..y + h {
            for xx in x..x + w {
                st.mvd[list][yy * 4 + xx] = a;
            }
        }
    }

    fn decode_cbp(&mut self, c: &mut Cabac) -> u8 {
        let mut luma = 0u8;
        for b8 in 0..4usize {
            let (bx, by) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            // A
            let ca = if bx > 0 { ((luma >> (b8 - 1)) & 1 == 0) as usize } else { self.cbp_luma_cond(self.nb[0], b8 + 1) };
            let cb = if by > 0 { ((luma >> (b8 - 2)) & 1 == 0) as usize } else { self.cbp_luma_cond(self.nb[1], b8 + 2) };
            let _ = (bx, by);
            let bin = c.decode_decision(73 + ca + 2 * cb);
            luma |= (bin as u8) << b8;
        }
        let cond = |n: Option<usize>, bin: usize| -> usize {
            let Some(m) = n else { return 0 };
            let st = &self.pic.mbs[m];
            if st.kind == MbKind::IPcm {
                return 1;
            }
            if st.kind_is_skip() {
                return 0;
            }
            let ch = st.cbp >> 4;
            if bin == 0 { (ch != 0) as usize } else { (ch == 2) as usize }
        };
        let inc0 = cond(self.nb[0], 0) + 2 * cond(self.nb[1], 0);
        let mut chroma = 0u8;
        if c.decode_decision(77 + inc0) == 1 {
            let inc1 = cond(self.nb[0], 1) + 2 * cond(self.nb[1], 1) + 4;
            chroma = 1 + c.decode_decision(77 + inc1) as u8;
        }
        luma | (chroma << 4)
    }

    /// condTermFlagN for coded_block_pattern luma bin from a neighbouring MB; `b8n` is the neighbouring
    /// 8x8 block index within that MB.
    fn cbp_luma_cond(&self, n: Option<usize>, b8n: usize) -> usize {
        let Some(m) = n else { return 0 };
        let st = &self.pic.mbs[m];
        if st.kind == MbKind::IPcm {
            return 0;
        }
        if st.kind_is_skip() {
            return 1;
        }
        ((st.cbp >> (b8n & 3)) & 1 == 0) as usize
    }

    fn decode_qp_delta(&mut self, c: &mut Cabac) -> Result<i32> {
        let inc = self.prev_qp_delta_nz as usize;
        if c.decode_decision(60 + inc) == 0 {
            return Ok(0);
        }
        let mut k = 1u32;
        let mut ctx = 62;
        while c.decode_decision(ctx) == 1 {
            k += 1;
            ctx = 63;
            ensure!(k <= 128, "mb_qp_delta too large");
        }
        let v = k.div_ceil(2) as i32;
        Ok(if k % 2 == 1 { v } else { -v })
    }

    /// coded_block_flag ctxIdxInc condition for a neighbouring luma 4x4 block (cat 0..2) or chroma.
    fn cbf_luma_cond(&self, bx: i32, by: i32, intra_cur: bool) -> usize {
        if bx >= 0 && by >= 0 {
            return (self.mb().nnz[(by * 4 + bx) as usize] != 0) as usize;
        }
        let n = if bx < 0 { self.nb[0] } else { self.nb[1] };
        let Some(m) = n else { return intra_cur as usize };
        let st = &self.pic.mbs[m];
        let (x, y) = if bx < 0 { (3, by as usize) } else { (bx as usize, 3) };
        (st.nnz[y * 4 + x] != 0) as usize
    }

    fn cbf_dc_cond(&self, n: Option<usize>, bit: u8, intra_cur: bool) -> usize {
        let Some(m) = n else { return intra_cur as usize };
        let st = &self.pic.mbs[m];
        if st.kind == MbKind::IPcm {
            return 1;
        }
        (st.cbf_dc & (1 << bit) != 0) as usize
    }

    fn cbf_chroma_ac_cond(&self, comp: usize, bx: i32, by: i32, intra_cur: bool) -> usize {
        if bx >= 0 && by >= 0 {
            return (self.mb().nnz_c[comp][(by * 2 + bx) as usize] != 0) as usize;
        }
        let n = if bx < 0 { self.nb[0] } else { self.nb[1] };
        let Some(m) = n else { return intra_cur as usize };
        let st = &self.pic.mbs[m];
        // right column of the left MB / bottom row of the MB above (2 rows in 4:2:0, 4 in 4:2:2)
        let bottom = 2usize << usize::from(self.pic.planes.fmt.chroma_y_shift == 0);
        let (x, y) = if bx < 0 { (1, by as usize) } else { (bx as usize, bottom - 1) };
        (st.nnz_c[comp][y * 2 + x] != 0) as usize
    }

    /// residual_block_cabac for ctxBlockCat `cat` (0..=5). Coefficients are written to `coeff`
    /// in list order (indices 0..max_num). `cbf_inc` is the coded_block_flag ctxIdxInc (ignored for cat 5).
    /// Returns the number of non-zero coefficients.
    fn residual_block_cabac(&mut self, c: &mut Cabac, coeff: &mut [i32], cat: usize, max_num: usize, cbf_inc: usize) -> u8 {
        if cat != 5 && c.decode_decision(85 + CAT_OFFSET[cat].0 + cbf_inc) == 0 {
            return 0;
        }
        let (sig_base, last_base, abs_base) =
            if cat == 5 { (402, 417, 426) } else { (105 + CAT_OFFSET[cat].1, 166 + CAT_OFFSET[cat].1, 227 + CAT_OFFSET[cat].2) };
        let (sig_tab, last_tab): (&[u8], &[u8]) = match cat {
            3 if max_num > 4 => (&CHROMA_DC_CTX_422, &CHROMA_DC_CTX_422),
            3 => (&CHROMA_DC_CTX, &CHROMA_DC_CTX),
            5 => (&SIG8_FRAME, &LAST8),
            _ => (&IDENT_CTX, &IDENT_CTX),
        };
        // significance map: positions of the significant coefficients in scan order
        let mut pos = [0u8; 64];
        let mut n = 0usize;
        let last_i = max_num - 1;
        let mut found_last = false;
        for i in 0..last_i {
            if c.decode_decision(sig_base + sig_tab[i] as usize) == 1 {
                pos[n] = i as u8;
                n += 1;
                if c.decode_decision(last_base + last_tab[i] as usize) == 1 {
                    found_last = true;
                    break;
                }
            }
        }
        if !found_last {
            pos[n] = last_i as u8;
            n += 1;
        }
        let mut eq1 = 0usize;
        let mut gt1 = 0usize;
        let gt1_cap = if cat == 3 { 3 } else { 4 };
        for &p in pos[..n].iter().rev() {
            let inc0 = if gt1 != 0 { 0 } else { (1 + eq1).min(4) };
            let mut abs = 1i32;
            if c.decode_decision(abs_base + inc0) == 1 {
                let ctx = abs_base + 5 + gt1.min(gt1_cap);
                let mut prefix = 1;
                while prefix < 14 && c.decode_decision(ctx) == 1 {
                    prefix += 1;
                }
                abs = prefix + 1;
                if prefix == 14 {
                    let mut k = 0;
                    let mut suf = 0i32;
                    while c.decode_bypass() == 1 {
                        suf += 1 << k;
                        k += 1;
                        if k > 24 {
                            break;
                        }
                    }
                    while k > 0 {
                        k -= 1;
                        suf += (c.decode_bypass() as i32) << k;
                    }
                    abs += suf;
                }
                gt1 += 1;
            } else {
                eq1 += 1;
            }
            coeff[p as usize] = if c.decode_bypass() == 1 { -abs } else { abs };
        }
        n as u8
    }

    fn residual_cabac(&mut self, c: &mut Cabac, cbp: u8) -> Result<()> {
        let kind = self.mb().kind;
        let intra = kind.is_intra();
        let t8 = self.mb().transform_8x8;
        if kind == MbKind::I16x16 {
            let inc = self.cbf_dc_cond(self.nb[0], 0, intra) + 2 * self.cbf_dc_cond(self.nb[1], 0, intra);
            let mut lv = [0i32; 16];
            if self.residual_block_cabac(c, &mut lv, 0, 16, inc) > 0 {
                self.mb_mut().cbf_dc |= 1;
                self.put_luma_dc(&lv);
            }
        }
        for b8 in 0..4 {
            let (bx8, by8) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            if cbp & (1 << b8) == 0 {
                continue;
            }
            if t8 {
                let mut lv = [0i32; 64];
                self.residual_block_cabac(c, &mut lv, 5, 64, 0);
                let st = self.mb_mut();
                for r in [by8 * 4 + bx8, by8 * 4 + bx8 + 1, (by8 + 1) * 4 + bx8, (by8 + 1) * 4 + bx8 + 1] {
                    st.nnz[r] = 1;
                }
                self.put_luma8(b8, &lv);
            } else {
                for i4 in 0..4 {
                    let (bx, by) = (bx8 + (i4 & 1), by8 + (i4 >> 1));
                    let raster = by * 4 + bx;
                    let inc = self.cbf_luma_cond(bx as i32 - 1, by as i32, intra) + 2 * self.cbf_luma_cond(bx as i32, by as i32 - 1, intra);
                    let mut lv = [0i32; 16];
                    let n = if kind == MbKind::I16x16 {
                        self.residual_block_cabac(c, &mut lv[1..], 1, 15, inc)
                    } else {
                        self.residual_block_cabac(c, &mut lv, 2, 16, inc)
                    };
                    self.mb_mut().nnz[raster] = n;
                    if n > 0 {
                        self.put_luma4(raster, &lv, kind == MbKind::I16x16);
                    }
                }
            }
        }
        let cbp_c = cbp >> 4;
        if cbp_c != 0 {
            // 4:2:2 chroma DC blocks hold 8 coefficients (4 * NumC8x8, NumC8x8 = 2).
            let dc_len = 4usize << usize::from(self.pic.planes.fmt.chroma_y_shift == 0);
            for comp in 0..2 {
                let bit = 1 + comp as u8;
                let inc = self.cbf_dc_cond(self.nb[0], bit, intra) + 2 * self.cbf_dc_cond(self.nb[1], bit, intra);
                let mut lv = [0i32; 8];
                if self.residual_block_cabac(c, &mut lv[..dc_len], 3, dc_len, inc) > 0 {
                    self.mb_mut().cbf_dc |= 1 << bit;
                    self.put_chroma_dc(comp, &lv[..dc_len]);
                }
            }
        }
        if cbp_c == 2 {
            let nblk = 4usize << usize::from(self.pic.planes.fmt.chroma_y_shift == 0);
            for comp in 0..2 {
                for b in 0..nblk {
                    let (bx, by) = ((b & 1) as i32, (b >> 1) as i32);
                    let inc = self.cbf_chroma_ac_cond(comp, bx - 1, by, intra) + 2 * self.cbf_chroma_ac_cond(comp, bx, by - 1, intra);
                    let mut lv = [0i32; 16];
                    let n = self.residual_block_cabac(c, &mut lv[1..], 4, 15, inc);
                    self.mb_mut().nnz_c[comp][b] = n;
                    if n > 0 {
                        self.put_chroma_ac(comp, b, &lv);
                    }
                }
            }
        }
        Ok(())
    }

    /// Parse and reconstruct one macroblock_layer() with CABAC.
    fn decode_mb_cabac(&mut self, c: &mut Cabac) -> Result<()> {
        let mut info = self.decode_mb_type(c)?;
        self.cur = MbCur { info, ..Default::default() };
        if info.kind == MbKind::IPcm {
            // pcm alignment + samples (u(BitDepth) each), then re-initialise the arithmetic decoder
            let bit_pos = c.bit_pos().div_ceil(8) * 8;
            let (samples, end) = crate::slicedec::read_pcm_samples(c.data(), bit_pos, self.pic.planes.fmt)?;
            c.set_bit_pos(end);
            c.init_engine()?;
            self.finish_pcm();
            self.write_pcm(&samples);
            self.prev_qp_delta_nz = false;
            return Ok(());
        }
        let intra = info.kind.is_intra();
        if info.kind == MbKind::I4x4 && self.pps.transform_8x8_mode && self.decode_t8x8_flag(c) {
            info.kind = MbKind::I8x8;
            self.cur.info.kind = MbKind::I8x8;
            self.mb_mut().transform_8x8 = true;
        }
        self.mb_mut().kind = info.kind;
        let n_ref = self.sh.num_ref_idx_active;
        match info.kind {
            MbKind::I4x4 | MbKind::I8x8 => {
                let n = if info.kind == MbKind::I8x8 { 4 } else { 16 };
                for i in 0..n {
                    self.cur.rem_mode[i] = if c.decode_decision(68) == 1 {
                        -1
                    } else {
                        let b0 = c.decode_decision(69);
                        let b1 = c.decode_decision(69);
                        let b2 = c.decode_decision(69);
                        (b0 | (b1 << 1) | (b2 << 2)) as i8
                    };
                }
                self.derive_intra_modes(info.kind == MbKind::I8x8);
                let m = self.decode_chroma_mode(c);
                self.mb_mut().intra_chroma_mode = m;
            }
            MbKind::I16x16 => {
                let m = self.decode_chroma_mode(c);
                self.mb_mut().intra_chroma_mode = m;
            }
            MbKind::BDirect16x16 => {
                self.mb_mut().direct8x8 = 0xf;
            }
            _ => {
                if info.part == Part::P8x8 {
                    for p in 0..4 {
                        self.cur.sub[p] = self.decode_sub_mb_type(c)?;
                    }
                    let direct_mask: u8 = (0..4).filter(|&p| self.cur.sub[p].direct).map(|p| 1u8 << p).sum();
                    self.mb_mut().direct8x8 = direct_mask;
                    for l in 0..2 {
                        for p in 0..4 {
                            let sub = self.cur.sub[p];
                            if sub.direct || sub.pred & (1 << l) == 0 {
                                continue;
                            }
                            let (x, y) = ((p & 1) * 2, (p >> 1) * 2);
                            let r = if n_ref[l] > 1 && !info.ref0 { self.decode_ref_idx(c, l, x, y) as i8 } else { 0 };
                            self.cur.ref_idx[l][p] = r;
                            self.set_cur_ref(l, x, y, 2, 2, r);
                        }
                    }
                    for l in 0..2 {
                        for p in 0..4 {
                            let sub = self.cur.sub[p];
                            if sub.direct || sub.pred & (1 << l) == 0 {
                                continue;
                            }
                            let (x8, y8) = ((p & 1) * 2, (p >> 1) * 2);
                            for j in 0..sub.shape.num_parts() {
                                let (sx, sy, w, h) = sub.shape.rect(j);
                                let (x, y) = (x8 + sx, y8 + sy);
                                let dx = self.decode_mvd(c, l, 0, x, y);
                                let dy = self.decode_mvd(c, l, 1, x, y);
                                self.set_cur_mvd(l, x, y, w, h, [dx, dy]);
                                self.cur.mvd[l][p * 4 + j] = [dx as i16, dy as i16];
                            }
                        }
                    }
                } else {
                    let np = info.part.num_parts();
                    for l in 0..2 {
                        for p in 0..np {
                            if info.pred[p] & (1 << l) == 0 {
                                continue;
                            }
                            let (x, y, w, h) = info.part.rect(p);
                            let r = if n_ref[l] > 1 { self.decode_ref_idx(c, l, x, y) as i8 } else { 0 };
                            self.cur.ref_idx[l][p] = r;
                            self.set_cur_ref(l, x, y, w, h, r);
                        }
                    }
                    for l in 0..2 {
                        for p in 0..np {
                            if info.pred[p] & (1 << l) == 0 {
                                continue;
                            }
                            let (x, y, w, h) = info.part.rect(p);
                            let dx = self.decode_mvd(c, l, 0, x, y);
                            let dy = self.decode_mvd(c, l, 1, x, y);
                            self.set_cur_mvd(l, x, y, w, h, [dx, dy]);
                            self.cur.mvd[l][p * 4] = [dx as i16, dy as i16];
                        }
                    }
                }
            }
        }
        let cbp = if info.kind == MbKind::I16x16 { info.i16_cbp } else { self.decode_cbp(c) };
        self.cur.cbp = cbp;
        self.mb_mut().cbp = cbp;
        if !intra && cbp & 15 != 0 && self.pps.transform_8x8_mode && self.no_sub_8x8_lt() {
            let t = self.decode_t8x8_flag(c);
            self.mb_mut().transform_8x8 = t;
        }
        if !intra {
            // ref_idx written during parsing is replaced by the derived motion
            self.derive_inter_motion()?;
        }
        if cbp != 0 || info.kind == MbKind::I16x16 {
            let d = self.decode_qp_delta(c)?;
            self.apply_qp_delta(d)?;
            self.prev_qp_delta_nz = d != 0;
        } else {
            self.prev_qp_delta_nz = false;
        }
        self.store_qp();
        self.residual_cabac(c, cbp)?;
        if intra {
            self.reconstruct_intra();
        } else {
            self.predict_inter();
            self.reconstruct_inter_residual();
        }
        Ok(())
    }

    fn decode_t8x8_flag(&mut self, c: &mut Cabac) -> bool {
        let cond = |n: Option<usize>| n.map(|m| self.pic.mbs[m].transform_8x8).unwrap_or(false) as usize;
        let inc = cond(self.nb[0]) + cond(self.nb[1]);
        c.decode_decision(399 + inc) == 1
    }

    fn decode_chroma_mode(&mut self, c: &mut Cabac) -> u8 {
        let cond = |n: Option<usize>| {
            n.map(|m| {
                let st = &self.pic.mbs[m];
                st.kind.is_intra() && st.kind != MbKind::IPcm && st.intra_chroma_mode != 0
            })
            .unwrap_or(false) as usize
        };
        let inc = cond(self.nb[0]) + cond(self.nb[1]);
        if c.decode_decision(64 + inc) == 0 {
            return 0;
        }
        if c.decode_decision(67) == 0 {
            return 1;
        }
        if c.decode_decision(67) == 0 { 2 } else { 3 }
    }
}
