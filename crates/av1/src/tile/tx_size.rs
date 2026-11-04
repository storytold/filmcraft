use super::{
    BLOCK_4X4, MAX_TX_DEPTH_TABLE, MAX_TX_SIZE_RECT, MAX_VARTX_DEPTH, SPLIT_TX_SIZE, TX_4X4, TX_HEIGHT, TX_MODE_SELECT, TX_SIZE_SQR_UP, TX_SIZES, TX_SIZES_ALL,
    TX_WIDTH, TileDecoder, block_height, block_width,
};

impl<'a, 'f> TileDecoder<'a, 'f> {
    // ---- transform size ---------------------------------------------------------------------

    pub(super) fn read_block_tx_size(&mut self) {
        let b = &self.b;
        let (bw4, bh4) = (b.bw4, b.bh4);
        let fh = &self.fs.fh;
        if fh.tx_mode == TX_MODE_SELECT as u8 && b.mi_size > BLOCK_4X4 && b.is_inter && !b.skip && !b.lossless {
            let max_tx = MAX_TX_SIZE_RECT[b.mi_size] as usize;
            let tw4 = TX_WIDTH[max_tx] as usize / 4;
            let th4 = TX_HEIGHT[max_tx] as usize / 4;
            let (r0, c0) = (b.mi_row, b.mi_col);
            let mut row = r0;
            while row < r0 + bh4 {
                let mut col = c0;
                while col < c0 + bw4 {
                    self.read_var_tx_size(row, col, max_tx, 0);
                    col += tw4;
                }
                row += th4;
            }
        } else {
            let allow = !b.skip || !b.is_inter;
            self.read_tx_size(allow);
            let (r0, c0, ts) = (self.b.mi_row, self.b.mi_col, self.b.tx_size as u8);
            let mi = &mut self.t.mi;
            for row in r0..(r0 + bh4).min(mi.rows) {
                for col in c0..(c0 + bw4).min(mi.cols) {
                    let i = mi.idx(row, col);
                    mi.inter_tx_size[i] = ts;
                }
            }
        }
    }

    fn read_tx_size(&mut self, allow_select: bool) {
        if self.b.lossless {
            self.b.tx_size = TX_4X4;
            return;
        }
        let ms = self.b.mi_size;
        let max_rect = MAX_TX_SIZE_RECT[ms] as usize;
        let max_depth = MAX_TX_DEPTH_TABLE[ms] as usize;
        self.b.tx_size = max_rect;
        if ms > BLOCK_4X4 && allow_select && self.fs.fh.tx_mode == TX_MODE_SELECT as u8 {
            let ctx = self.tx_depth_ctx(max_rect);
            let depth = match max_depth {
                4 => self.sd.read_symbol(&mut self.cdf.tx_64x64[ctx]),
                3 => self.sd.read_symbol(&mut self.cdf.tx_32x32[ctx]),
                2 => self.sd.read_symbol(&mut self.cdf.tx_16x16[ctx]),
                _ => self.sd.read_symbol(&mut self.cdf.tx_8x8[ctx]),
            };
            for _ in 0..depth {
                self.b.tx_size = SPLIT_TX_SIZE[self.b.tx_size] as usize;
            }
        }
    }

    fn tx_depth_ctx(&self, max_rect: usize) -> usize {
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let mi = &self.t.mi;
        let max_w = TX_WIDTH[max_rect] as usize;
        let max_h = TX_HEIGHT[max_rect] as usize;
        let above_w = if self.b.avail_u && mi.is_inter[mi.idx(r - 1, c)] {
            block_width(mi.mi_size[mi.idx(r - 1, c)] as usize)
        } else if self.b.avail_u {
            self.get_above_tx_width(r, c)
        } else {
            0
        };
        let left_h = if self.b.avail_l && mi.is_inter[mi.idx(r, c - 1)] {
            block_height(mi.mi_size[mi.idx(r, c - 1)] as usize)
        } else if self.b.avail_l {
            self.get_left_tx_height(r, c)
        } else {
            0
        };
        (above_w >= max_w) as usize + (left_h >= max_h) as usize
    }

    fn get_above_tx_width(&self, row: usize, col: usize) -> usize {
        let mi = &self.t.mi;
        if row == self.b.mi_row {
            if !self.b.avail_u {
                return 64;
            } else if mi.skip[mi.idx(row - 1, col)] && mi.is_inter[mi.idx(row - 1, col)] {
                return block_width(mi.mi_size[mi.idx(row - 1, col)] as usize);
            }
        }
        TX_WIDTH[mi.inter_tx_size[mi.idx(row - 1, col)] as usize] as usize
    }

    fn get_left_tx_height(&self, row: usize, col: usize) -> usize {
        let mi = &self.t.mi;
        if col == self.b.mi_col {
            if !self.b.avail_l {
                return 64;
            } else if mi.skip[mi.idx(row, col - 1)] && mi.is_inter[mi.idx(row, col - 1)] {
                return block_height(mi.mi_size[mi.idx(row, col - 1)] as usize);
            }
        }
        TX_HEIGHT[mi.inter_tx_size[mi.idx(row, col - 1)] as usize] as usize
    }

    fn read_var_tx_size(&mut self, row: usize, col: usize, tx_sz: usize, depth: usize) {
        let (mi_rows, mi_cols) = (self.fs.fh.mi_rows as usize, self.fs.fh.mi_cols as usize);
        if row >= mi_rows || col >= mi_cols {
            return;
        }
        let split = if tx_sz == TX_4X4 || depth == MAX_VARTX_DEPTH {
            false
        } else {
            let above = self.get_above_tx_width(row, col) < TX_WIDTH[tx_sz] as usize;
            let left = self.get_left_tx_height(row, col) < TX_HEIGHT[tx_sz] as usize;
            let ms = self.b.mi_size;
            let size = 64.min(block_width(ms).max(block_height(ms)));
            let max_tx = find_tx_size(size, size);
            let sqr_up = TX_SIZE_SQR_UP[tx_sz] as usize;
            let ctx = (sqr_up != max_tx) as usize * 3 + (TX_SIZES - 1 - max_tx) * 6 + above as usize + left as usize;
            self.sd.read_symbol(&mut self.cdf.txfm_split[ctx]) == 1
        };
        let w4 = TX_WIDTH[tx_sz] as usize / 4;
        let h4 = TX_HEIGHT[tx_sz] as usize / 4;
        if split {
            let sub = SPLIT_TX_SIZE[tx_sz] as usize;
            let sw = TX_WIDTH[sub] as usize / 4;
            let sh = TX_HEIGHT[sub] as usize / 4;
            let mut i = 0;
            while i < h4 {
                let mut j = 0;
                while j < w4 {
                    self.read_var_tx_size(row + i, col + j, sub, depth + 1);
                    j += sw;
                }
                i += sh;
            }
        } else {
            let mi = &mut self.t.mi;
            for i in 0..h4 {
                for j in 0..w4 {
                    if row + i < mi.rows && col + j < mi.cols {
                        let k = mi.idx(row + i, col + j);
                        mi.inter_tx_size[k] = tx_sz as u8;
                    }
                }
            }
            self.b.tx_size = tx_sz;
        }
    }
}

pub(crate) fn find_tx_size(w: usize, h: usize) -> usize {
    (0..TX_SIZES_ALL).find(|&t| TX_WIDTH[t] as usize == w && TX_HEIGHT[t] as usize == h).unwrap_or(TX_4X4)
}
