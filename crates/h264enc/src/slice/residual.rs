use super::*;

/// The transform trial's bit estimate of a level `v`: `0.4` for zero, `3 + 2·log2|v|` otherwise.
/// Levels below 256 (nearly all) read it from a table of exactly those values.
#[inline]
fn level_bits(table: &[f64; 256], v: i16) -> f64 {
    let a = v.unsigned_abs();
    match table.get(usize::from(a)) {
        Some(&b) => b,
        None => 3.0 + 2.0 * (a as f64).log2(),
    }
}

fn level_bits_table() -> &'static [f64; 256] {
    static T: std::sync::OnceLock<[f64; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|a| if a == 0 { 0.4 } else { 3.0 + 2.0 * (a as f64).log2() }))
}

impl<'a, 'b> SliceEnc<'a, 'b> {
    // ------------------------------------------------------------------ residual coding helpers

    /// Code chroma residual for prediction `pu`, `pv`; fills code/info and reconstruction. Returns chroma cbp.
    pub(super) fn code_chroma(&self, src: &Src, pu: &[u8; 64], pv: &[u8; 64], qp: u8, intra: bool, c: &mut Cand) -> u8 {
        let qt = self.f.qt;
        let qpc = chroma_qp(qp as i32 + self.f.p.chroma_qp_offset);
        let dz = if intra { Deadzone::INTRA } else { Deadzone::INTER };
        let mut dc_any = false;
        let mut ac_any = false;
        let mut coefs = [[[0i32; 16]; 4]; 2];
        let mut dcl = [[0i32; 4]; 2];
        for comp in 0..2 {
            let (s, p) = if comp == 0 { (&src.u, pu) } else { (&src.v, pv) };
            let mut dc = [0i32; 4];
            let mut ac_score = 0;
            for b in 0..4 {
                let (bx, by) = ((b & 1) * 4, (b >> 1) * 4);
                let mut d = [0i32; 16];
                for y in 0..4 {
                    for x in 0..4 {
                        d[y * 4 + x] = s[(by + y) * 8 + bx + x] as i32 - p[(by + y) * 8 + bx + x] as i32;
                    }
                }
                let mut t = fdct4(&d);
                dc[b] = t[0];
                quant4(&mut t, qt, qpc, dz, true);
                if self.f.p.cavlc_clamp {
                    for v in t.iter_mut().skip(1) {
                        *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                    }
                }
                let mut scan = [0i16; 16];
                for k in 1..16 {
                    scan[k] = t[ZIGZAG4[k]] as i16;
                }
                ac_score += decimate_score(&scan[1..]);
                c.code.cac[comp * 4 + b] = scan;
                coefs[comp][b] = t;
            }
            if !intra && self.f.p.decimate && ac_score < 7 {
                for b in 0..4 {
                    c.code.cac[comp * 4 + b] = [0; 16];
                    for k in 1..16 {
                        coefs[comp][b][k] = 0;
                    }
                }
            }
            let (mut lv, _) = quant_chroma_dc(&dc, qt, qpc, dz);
            if self.f.p.cavlc_clamp {
                for v in lv.iter_mut() {
                    *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                }
            }
            dcl[comp] = lv;
            for b in 0..4 {
                c.code.cdc[comp][b] = lv[b] as i16;
            }
            dc_any |= lv.iter().any(|&v| v != 0);
            ac_any |= c.code.cac[comp * 4..comp * 4 + 4].iter().any(|b| b.iter().any(|&v| v != 0));
        }
        let cbp = if ac_any {
            2
        } else if dc_any {
            1
        } else {
            0
        };
        c.info.dc_cbf &= 1;
        for comp in 0..2 {
            let p = if comp == 0 { pu } else { pv };
            let dcq = dequant_chroma_dc(&dcl[comp], qt, qpc);
            if dcl[comp].iter().any(|&v| v != 0) {
                c.info.dc_cbf |= 2 << comp;
            }
            for b in 0..4 {
                let (bx, by) = ((b & 1) * 4, (b >> 1) * 4);
                let mut t = coefs[comp][b];
                let nz = if cbp == 2 { t.iter().skip(1).filter(|&&v| v != 0).count() as u8 } else { 0 };
                c.info.nnz_c[comp * 4 + b] = nz;
                if cbp != 2 {
                    t = [0; 16];
                } else {
                    dequant4(&mut t, qt, qpc, true);
                }
                t[0] = dcq[b];
                let rec = if comp == 0 { &mut c.ru } else { &mut c.rv };
                if nz == 0 {
                    let dcv = (t[0] + 32) >> 6;
                    for y in 0..4 {
                        for x in 0..4 {
                            let i = (by + y) * 8 + bx + x;
                            rec[i] = (p[i] as i32 + dcv).clamp(0, 255) as u8;
                        }
                    }
                } else {
                    let r = idct4(&t);
                    for y in 0..4 {
                        for x in 0..4 {
                            let i = (by + y) * 8 + bx + x;
                            rec[i] = (p[i] as i32 + r[y * 4 + x]).clamp(0, 255) as u8;
                        }
                    }
                }
            }
        }
        if cbp == 0 {
            c.code.cdc = [[0; 4]; 2];
        }
        if cbp != 2 {
            c.code.cac = [[0; 16]; 8];
        }
        cbp
    }

    /// Inter luma residual with the 4x4 transform. Returns luma cbp.
    fn code_luma4(&self, src: &[u8; 256], pred: &[u8; 256], qp: u8, dz: Deadzone, decimate: bool, c: &mut Cand) -> u8 {
        let qt = self.f.qt;
        let mut coefs = [[0i32; 16]; 16];
        let mut score8 = [0u32; 4];
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let mut d = [0i32; 16];
            for y in 0..4 {
                for x in 0..4 {
                    let i = (by * 4 + y) * 16 + bx * 4 + x;
                    d[y * 4 + x] = src[i] as i32 - pred[i] as i32;
                }
            }
            let mut t = fdct4(&d);
            quant4(&mut t, qt, qp, dz, false);
            if self.f.p.cavlc_clamp {
                for v in t.iter_mut() {
                    *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                }
            }
            let mut scan = [0i16; 16];
            for k in 0..16 {
                scan[k] = t[ZIGZAG4[k]] as i16;
            }
            score8[blk / 4] += decimate_score(&scan);
            c.code.luma4[blk] = scan;
            coefs[blk] = t;
        }
        let mut cbp = 0u8;
        let total: u32 = score8.iter().sum();
        for b8 in 0..4 {
            let kill = decimate && (score8[b8] < 4 || total < 6);
            for i in 0..4 {
                let blk = b8 * 4 + i;
                if kill {
                    c.code.luma4[blk] = [0; 16];
                    coefs[blk] = [0; 16];
                }
                if c.code.luma4[blk].iter().any(|&v| v != 0) {
                    cbp |= 1 << b8;
                }
            }
        }
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let nz = c.code.luma4[blk].iter().filter(|&&v| v != 0).count() as u8;
            c.info.nnz[by * 4 + bx] = nz;
            let off = by * 4 * 16 + bx * 4;
            if nz == 0 {
                for y in 0..4 {
                    c.ry[off + y * 16..off + y * 16 + 4].copy_from_slice(&pred[off + y * 16..off + y * 16 + 4]);
                }
            } else {
                let mut t = coefs[blk];
                dequant4(&mut t, qt, qp, false);
                let r = idct4(&t);
                for y in 0..4 {
                    for x in 0..4 {
                        let i = off + y * 16 + x;
                        c.ry[i] = (pred[i] as i32 + r[y * 4 + x]).clamp(0, 255) as u8;
                    }
                }
            }
        }
        cbp
    }

    /// Inter luma residual with the 8x8 transform. Returns luma cbp.
    fn code_luma8(&self, src: &[u8; 256], pred: &[u8; 256], qp: u8, dz: Deadzone, decimate: bool, c: &mut Cand) -> u8 {
        let qt = self.f.qt;
        let mut coefs = [[0i32; 64]; 4];
        let mut score = [0u32; 4];
        for b8 in 0..4 {
            let (ox, oy) = ((b8 & 1) * 8, (b8 >> 1) * 8);
            let mut d = [0i32; 64];
            for y in 0..8 {
                for x in 0..8 {
                    let i = (oy + y) * 16 + ox + x;
                    d[y * 8 + x] = src[i] as i32 - pred[i] as i32;
                }
            }
            let mut t = fdct8(&d);
            quant8(&mut t, qt, qp, dz);
            let mut scan = [0i16; 64];
            for k in 0..64 {
                scan[k] = t[ZIGZAG8[k]] as i16;
            }
            score[b8] = decimate_score(&scan);
            c.code.luma8[b8] = scan;
            coefs[b8] = t;
        }
        let total: u32 = score.iter().sum();
        let mut cbp = 0;
        for b8 in 0..4 {
            let (ox, oy) = ((b8 & 1) * 8, (b8 >> 1) * 8);
            if decimate && (score[b8] < 4 || total < 6) {
                c.code.luma8[b8] = [0; 64];
                coefs[b8] = [0; 64];
            }
            let nz = c.code.luma8[b8].iter().filter(|&&v| v != 0).count() as u8;
            let (bx, by) = (ox / 4, oy / 4);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                c.info.nnz[(by + dy) * 4 + bx + dx] = nz;
            }
            if nz == 0 {
                for y in 0..8 {
                    let o = (oy + y) * 16 + ox;
                    c.ry[o..o + 8].copy_from_slice(&pred[o..o + 8]);
                }
                continue;
            }
            cbp |= 1 << b8;
            let mut t = coefs[b8];
            dequant8(&mut t, qt, qp);
            let r = idct8(&t);
            for y in 0..8 {
                for x in 0..8 {
                    let i = (oy + y) * 16 + ox + x;
                    c.ry[i] = (pred[i] as i32 + r[y * 8 + x]).clamp(0, 255) as u8;
                }
            }
        }
        cbp
    }

    /// Code an inter candidate's residual given its luma/chroma prediction.
    pub(super) fn code_inter(&self, src: &Src, py: &[u8; 256], pu: &[u8; 64], pv: &[u8; 64], qp: u8, c: &mut Cand) {
        let t8_allowed = self.f.p.t8x8;
        let (use8, cbpl) = if t8_allowed {
            if self.f.p.adaptive_t8 {
                // Compare 4x4 and 8x8 transform by SSD + lambda * (approximate) bits. Neither
                // coder reads the fields it writes, so the 4x4 trial is coded into `c` itself
                // and the 8x8 trial into a copy that replaces `c` when it wins: the winner is
                // not coded a second time.
                let mut c8 = c.clone();
                let cbp8 = self.code_luma8(&src.y, py, qp, Deadzone::INTER, self.f.p.decimate, &mut c8);
                let cbp4 = self.code_luma4(&src.y, py, qp, Deadzone::INTER, self.f.p.decimate, c);
                let lam = self.f.lam.ssd[qp as usize];
                let lb = level_bits_table();
                let cost = |cc: &Cand, cbp: u8, eight: bool| {
                    let d = ssd(&src.y, 16, &cc.ry, 16, 16, 16) as f64;
                    let mut bits = 0f64;
                    if eight {
                        for b in 0..4 {
                            if (cbp >> b) & 1 != 0 {
                                bits += cc.code.luma8[b].iter().map(|&v| level_bits(lb, v)).sum::<f64>();
                            }
                        }
                    } else {
                        for b in 0..16 {
                            if (cbp >> (b / 4)) & 1 != 0 {
                                bits += 1.0 + cc.code.luma4[b].iter().map(|&v| level_bits(lb, v)).sum::<f64>();
                            }
                        }
                    }
                    d + lam * bits
                };
                if cost(&c8, cbp8, true) <= cost(c, cbp4, false) {
                    *c = c8;
                    (true, cbp8)
                } else {
                    (false, cbp4)
                }
            } else {
                (true, self.code_luma8(&src.y, py, qp, Deadzone::INTER, self.f.p.decimate, c))
            }
        } else {
            (false, self.code_luma4(&src.y, py, qp, Deadzone::INTER, self.f.p.decimate, c))
        };
        c.code.t8x8 = use8 && cbpl != 0;
        c.info.t8x8 = c.code.t8x8;
        let cbpc = self.code_chroma(src, pu, pv, qp, false, c);
        c.code.cbp = cbpl | (cbpc << 4);
    }
}
