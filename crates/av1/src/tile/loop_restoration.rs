use super::coeffs::inverse_recenter;
use super::{
    NUM_4X4_BLOCKS_HIGH, NUM_4X4_BLOCKS_WIDE, RESTORE_NONE, RESTORE_SGRPROJ, RESTORE_WIENER, SGR_PARAMS, SGRPROJ_PARAMS_BITS, SGRPROJ_PRJ_BITS,
    SGRPROJ_PRJ_SUBEXP_K, SUPERRES_NUM, TileDecoder,
};

impl<'a, 'f> TileDecoder<'a, 'f> {
    // ---- loop restoration syntax -------------------------------------------------------------

    pub(super) fn read_lr(&mut self, r: usize, c: usize, bsize: usize) {
        if self.fs.fh.allow_intrabc {
            return;
        }
        let w = NUM_4X4_BLOCKS_WIDE[bsize] as usize;
        let h = NUM_4X4_BLOCKS_HIGH[bsize] as usize;
        for plane in 0..self.fs.num_planes {
            if self.fs.fh.lr.frame_restoration_type[plane] == RESTORE_NONE as u8 {
                continue;
            }
            let sub_x = if plane == 0 { 0 } else { self.fs.ssx };
            let sub_y = if plane == 0 { 0 } else { self.fs.ssy };
            let unit_size = self.fs.fh.lr.loop_restoration_size[plane] as usize;
            let (unit_rows, unit_cols) = (self.fs.lr_unit_rows[plane], self.fs.lr_unit_cols[plane]);
            let unit_row_start = (r * (4 >> sub_y)).div_ceil(unit_size);
            let unit_row_end = unit_rows.min(((r + h) * (4 >> sub_y)).div_ceil(unit_size));
            let (numerator, denominator) =
                if self.fs.fh.use_superres { ((4 >> sub_x) * self.fs.fh.superres_denom as usize, unit_size * SUPERRES_NUM) } else { (4 >> sub_x, unit_size) };
            let unit_col_start = (c * numerator).div_ceil(denominator);
            let unit_col_end = unit_cols.min(((c + w) * numerator).div_ceil(denominator));
            for unit_row in unit_row_start..unit_row_end {
                for unit_col in unit_col_start..unit_col_end {
                    self.read_lr_unit(plane, unit_row, unit_col);
                }
            }
        }
    }

    fn read_lr_unit(&mut self, plane: usize, unit_row: usize, unit_col: usize) {
        let frt = self.fs.fh.lr.frame_restoration_type[plane];
        let rtype = if frt == RESTORE_WIENER as u8 {
            if self.sd.read_symbol(&mut self.cdf.use_wiener) == 1 { RESTORE_WIENER as u8 } else { RESTORE_NONE as u8 }
        } else if frt == RESTORE_SGRPROJ as u8 {
            if self.sd.read_symbol(&mut self.cdf.use_sgrproj) == 1 { RESTORE_SGRPROJ as u8 } else { RESTORE_NONE as u8 }
        } else {
            // restoration_type symbol: 0 = NONE, 1 = WIENER, 2 = SGRPROJ
            match self.sd.read_symbol(&mut self.cdf.restoration_type) {
                0 => RESTORE_NONE as u8,
                1 => RESTORE_WIENER as u8,
                _ => RESTORE_SGRPROJ as u8,
            }
        };
        let idx = unit_row * self.fs.lr_unit_cols[plane] + unit_col;
        self.t.lr.lr_type[plane][idx] = rtype;
        self.t.lr_written[plane].push(idx);
        if rtype == RESTORE_WIENER as u8 {
            const MIN: [i32; 3] = [-5, -23, -17];
            const MAX: [i32; 3] = [10, 8, 46];
            const K: [u32; 3] = [1, 2, 3];
            for pass in 0..2 {
                let first = if plane > 0 {
                    self.t.lr.lr_wiener[plane][idx][pass][0] = 0;
                    1
                } else {
                    0
                };
                for j in first..3 {
                    let v = self.decode_signed_subexp_with_ref_bool(MIN[j], MAX[j] + 1, K[j], self.ref_lr_wiener[plane][pass][j]);
                    self.t.lr.lr_wiener[plane][idx][pass][j] = v as i8;
                    self.ref_lr_wiener[plane][pass][j] = v;
                }
            }
        } else if rtype == RESTORE_SGRPROJ as u8 {
            let set = self.sd.read_literal(SGRPROJ_PARAMS_BITS as u32) as usize;
            self.t.lr.lr_sgr_set[plane][idx] = set as u8;
            const MIN: [i32; 2] = [-96, -32];
            const MAX: [i32; 2] = [31, 95];
            for i in 0..2 {
                let radius = SGR_PARAMS[set][i * 2];
                let v = if radius != 0 {
                    self.decode_signed_subexp_with_ref_bool(MIN[i], MAX[i] + 1, SGRPROJ_PRJ_SUBEXP_K as u32, self.ref_sgr_xqd[plane][i])
                } else if i == 1 {
                    ((1 << SGRPROJ_PRJ_BITS) - self.ref_sgr_xqd[plane][0]).clamp(MIN[i], MAX[i])
                } else {
                    0
                };
                self.t.lr.lr_sgr_xqd[plane][idx][i] = v as i8;
                self.ref_sgr_xqd[plane][i] = v;
            }
        }
    }

    fn decode_signed_subexp_with_ref_bool(&mut self, low: i32, high: i32, k: u32, r: i32) -> i32 {
        let x = self.decode_unsigned_subexp_with_ref_bool(high - low, k, r - low);
        x + low
    }

    fn decode_unsigned_subexp_with_ref_bool(&mut self, mx: i32, k: u32, r: i32) -> i32 {
        let v = self.decode_subexp_bool(mx, k);
        if (r << 1) <= mx { inverse_recenter(r, v) } else { mx - 1 - inverse_recenter(mx - 1 - r, v) }
    }

    fn decode_subexp_bool(&mut self, num_syms: i32, k: u32) -> i32 {
        let mut i = 0u32;
        let mut mk = 0i32;
        loop {
            let b2 = if i > 0 { k + i - 1 } else { k };
            let a = 1i32 << b2;
            if num_syms <= mk + 3 * a {
                return self.sd.read_ns((num_syms - mk) as u32) as i32 + mk;
            } else if self.sd.read_literal(1) == 1 {
                i += 1;
                mk += a;
            } else {
                return self.sd.read_literal(b2) as i32 + mk;
            }
        }
    }
}
