//! Synthetic MPEG-2 streams for coding tools ffmpeg's encoder never writes: field pictures (the
//! second field predicted from the first), 16x8 motion compensation, dual-prime prediction in
//! field and frame pictures, concealment motion vectors in every picture type, skipped
//! macroblocks of field pictures, 4:2:2 field DCT... A random but valid stream is written here
//! (random macroblock types, vectors, field selects and coefficients) and decoded by both our
//! decoder and ffmpeg (the oracle); the frames must agree up to IDCT rounding.

use std::collections::HashMap;

use crate::tests::W;
use crate::vlc::*;
use crate::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u32) -> u32 {
        (self.next() % n.max(1) as u64) as u32
    }
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u32) as i32
    }
    fn chance(&mut self, pct: u32) -> bool {
        self.below(100) < pct
    }
}

fn code_of(table: &[(&'static str, i16)], v: i16) -> &'static str {
    table.iter().find(|e| e.1 == v).map(|e| e.0).unwrap_or_else(|| panic!("no code for {v}"))
}

#[derive(Clone, Copy)]
struct Cfg {
    name: &'static str,
    seed: u64,
    chroma: u8,
    w: usize,
    h: usize,
    /// Field pictures (else interlaced frame pictures with frame_pred_frame_dct = 0).
    field_pics: bool,
    /// The first field / top_field_first.
    tff: bool,
    b_frames: bool,
    dual: bool,
    alt_scan: bool,
    intra_vlc: bool,
    q_scale_type: bool,
    dc_prec: u8,
    concealment: bool,
    frames: usize,
}

#[derive(Clone, Copy, Default)]
struct M {
    flags: i16,
    mtype: u8,
    mv: [[[i32; 2]; 2]; 2],
    select: [[usize; 2]; 2],
    dmv: [i32; 2],
}

/// A prediction block: plane (0 luma, 1 chroma), field, position, half flags, size.
struct Req {
    chroma: bool,
    field: bool,
    x: i32,
    y: i32,
    hx: i32,
    hy: i32,
    w: i32,
    h: i32,
}

struct Enc {
    w: W,
    rng: Rng,
    cfg: Cfg,
    mbw: usize,
    mbh: usize,
    shy: u32,
    dct_codes: [HashMap<(u32, u32), &'static str>; 2],
    /// The active quantiser_scale.
    qs: i32,
}

/// Picture-level coding state.
struct Pic {
    ptype: PictureType,
    structure: u8,
    parity: usize,
    fpfd: bool,
    /// The P second field of the first frame: only the first field (opposite parity) exists.
    lone: bool,
}

struct St {
    pmv: [[[i32; 2]; 2]; 2],
    dc: [i32; 3],
    last: M,
}

const F_CODE: i32 = 2;

impl Enc {
    fn new(cfg: Cfg) -> Enc {
        let mut dct_codes = [HashMap::new(), HashMap::new()];
        for (k, t) in [dct_table(DCT_ZERO, DCT_ZERO_LONG), dct_table(DCT_ONE, &[])].into_iter().enumerate() {
            for (c, v) in t {
                if v >= 0 {
                    dct_codes[k].insert(((v & 63) as u32, (v >> 6) as u32), c);
                }
            }
        }
        Enc { w: W::new(), rng: Rng(cfg.seed), cfg, mbw: cfg.w.div_ceil(16), mbh: 2 * cfg.h.div_ceil(32), shy: (cfg.chroma == 1) as u32, dct_codes, qs: 2 }
    }

    fn sequence(&mut self) {
        let c = self.cfg;
        let w = &mut self.w;
        w.start(0xB3);
        w.put(c.w as u32, 12);
        w.put(c.h as u32, 12);
        w.put(1, 4);
        w.put(3, 4);
        w.put(20000, 18);
        w.put(1, 1);
        w.put(112, 10);
        w.put(0, 1);
        w.put(0, 1);
        w.put(0, 1);
        w.start(0xB5);
        w.put(1, 4);
        w.put(if c.chroma == 2 { 0x85 } else { 0x48 }, 8);
        w.put(0, 1); // interlaced sequence
        w.put(c.chroma as u32, 2);
        w.put(0, 2);
        w.put(0, 2);
        w.put(0, 12);
        w.put(1, 1);
        w.put(0, 8);
        w.put(0, 1);
        w.put(0, 2);
        w.put(0, 5);
        w.start(0xB8);
        w.put(0, 12);
        w.put(1, 1); // marker_bit
        w.put(0, 12);
        w.put(1, 1); // closed GOP
        w.put(0, 1);
    }

    fn picture(&mut self, tref: u16, ptype: PictureType, structure: u8) {
        let c = self.cfg;
        let w = &mut self.w;
        w.start(0x00);
        w.put(tref as u32, 10);
        w.put(
            match ptype {
                PictureType::I => 1,
                PictureType::P => 2,
                _ => 3,
            },
            3,
        );
        w.put(0xFFFF, 16);
        if ptype != PictureType::I {
            w.put(0, 1);
            w.put(7, 3);
        }
        if ptype == PictureType::B {
            w.put(0, 1);
            w.put(7, 3);
        }
        w.put(0, 1);
        w.start(0xB5);
        w.put(8, 4);
        let fwd = if ptype == PictureType::I && !c.concealment { 15 } else { F_CODE as u32 };
        let bwd = if ptype == PictureType::B { F_CODE as u32 } else { 15 };
        for v in [fwd, fwd, bwd, bwd] {
            w.put(v, 4);
        }
        w.put(c.dc_prec as u32, 2);
        w.put(structure as u32, 2);
        w.put((structure == 3 && c.tff) as u32, 1);
        w.put(0, 1); // frame_pred_frame_dct
        w.put(c.concealment as u32, 1);
        w.put(c.q_scale_type as u32, 1);
        w.put(c.intra_vlc as u32, 1);
        w.put(c.alt_scan as u32, 1);
        w.put(0, 1); // repeat_first_field
        w.put(0, 1); // chroma_420_type
        w.put(0, 1); // progressive_frame
        w.put(0, 1);
        let lone = tref == 0 && ptype == PictureType::P;
        let pic = Pic { ptype, structure, parity: (structure == 2) as usize, fpfd: false, lone };
        let rows = if structure == 3 { self.mbh } else { self.mbh / 2 };
        for r in 0..rows {
            self.slice(&pic, r);
        }
    }

    fn set_q(&mut self, code: i32) {
        self.qs = if self.cfg.q_scale_type { crate::slice::NON_LINEAR_Q[code as usize] as i32 } else { 2 * code };
    }

    fn dc_reset(&self) -> i32 {
        1 << (7 + self.cfg.dc_prec)
    }

    fn slice(&mut self, p: &Pic, row: usize) {
        self.w.start(1 + row as u8);
        let q = self.rng.range(1, 31);
        self.set_q(q);
        self.w.put(q as u32, 5);
        self.w.put(0, 1);
        let mut st = St { pmv: [[[0; 2]; 2]; 2], dc: [self.dc_reset(); 3], last: M::default() };
        let mut skipped = 0;
        for mbx in 0..self.mbw {
            let can_skip = p.ptype != PictureType::I && !p.lone && mbx > 0 && mbx + 1 < self.mbw && self.rng.chance(15);
            if can_skip && self.try_skip(p, &mut st, mbx, row) {
                skipped += 1;
                continue;
            }
            self.address_increment(skipped + 1);
            skipped = 0;
            self.macroblock(p, &mut st, mbx, row);
        }
    }

    fn address_increment(&mut self, mut n: usize) {
        while n > 33 {
            self.w.code(code_of(MBA, MBA_ESCAPE));
            n -= 33;
        }
        self.w.code(code_of(MBA, n as i16));
    }

    /// Skip the macroblock when its implied prediction stays inside the picture.
    fn try_skip(&mut self, p: &Pic, st: &mut St, mbx: usize, mby: usize) -> bool {
        let mut m = M { mtype: if p.structure == 3 { 2 } else { 1 }, ..Default::default() };
        match p.ptype {
            PictureType::P => {
                m.flags = MB_FWD;
                m.select[0][0] = p.parity;
            }
            _ => {
                if st.last.flags & MB_INTRA != 0 || st.last.flags & (MB_FWD | MB_BWD) == 0 {
                    return false;
                }
                m.flags = st.last.flags & (MB_FWD | MB_BWD);
                for s in 0..2 {
                    m.mv[0][s] = st.pmv[0][s];
                    m.select[0][s] = p.parity;
                }
            }
        }
        if !self.fits(p, &m, mbx, mby) {
            return false;
        }
        st.dc = [self.dc_reset(); 3];
        if p.ptype == PictureType::P {
            st.pmv = [[[0; 2]; 2]; 2];
        }
        true
    }

    /// Every prediction block of `m` lies inside its reference plane.
    fn fits(&self, p: &Pic, m: &M, mbx: usize, mby: usize) -> bool {
        let (lw, lh) = (self.mbw as i32 * 16, self.mbh as i32 * 16);
        let (cw, ch) = (lw / 2, lh >> self.shy);
        self.requests(p, m, mbx, mby).iter().all(|r| {
            let (pw, ph) = if r.chroma { (cw, ch) } else { (lw, lh) };
            let ph = if r.field { ph / 2 } else { ph };
            r.x >= 0 && r.y >= 0 && r.x + r.w + r.hx <= pw && r.y + r.h + r.hy <= ph
        })
    }

    /// The prediction blocks of `m` (mirrors the decoder's geometry).
    fn requests(&self, p: &Pic, m: &M, mbx: usize, mby: usize) -> Vec<Req> {
        let mut out = Vec::new();
        let cbh = 16 >> self.shy;
        let mut add = |field: bool, mv: [i32; 2], lx: i32, ly: i32, lh: i32, cy: i32, ch: i32| {
            out.push(Req { chroma: false, field, x: lx + (mv[0] >> 1), y: ly + (mv[1] >> 1), hx: mv[0] & 1, hy: mv[1] & 1, w: 16, h: lh });
            let cmv = [mv[0] / 2, if self.shy == 1 { mv[1] / 2 } else { mv[1] }];
            out.push(Req { chroma: true, field, x: lx / 2 + (cmv[0] >> 1), y: cy + (cmv[1] >> 1), hx: cmv[0] & 1, hy: cmv[1] & 1, w: 8, h: ch });
        };
        let x = (mbx * 16) as i32;
        for s in 0..2 {
            let dir = if s == 0 { MB_FWD } else { MB_BWD };
            if m.flags & dir == 0 {
                continue;
            }
            let mby = mby as i32;
            if p.structure == 3 {
                match m.mtype {
                    1 => {
                        for r in 0..2 {
                            add(true, m.mv[r][s], x, mby * 8, 8, mby * cbh / 2, cbh / 2);
                        }
                    }
                    3 => {
                        for par in 0..2 {
                            let (mm, e) = if par == 0 { (if self.cfg.tff { 1 } else { 3 }, -1) } else { (if self.cfg.tff { 3 } else { 1 }, 1) };
                            add(true, m.mv[0][s], x, mby * 8, 8, mby * cbh / 2, cbh / 2);
                            add(true, dual(m.mv[0][s], m.dmv, mm, e), x, mby * 8, 8, mby * cbh / 2, cbh / 2);
                        }
                    }
                    _ => add(false, m.mv[0][s], x, mby * 16, 16, mby * cbh, cbh),
                }
            } else {
                match m.mtype {
                    2 => {
                        for r in 0..2 {
                            add(true, m.mv[r][s], x, mby * 16 + 8 * r as i32, 8, mby * cbh + r as i32 * cbh / 2, cbh / 2);
                        }
                    }
                    3 => {
                        let e = if p.parity == 1 { 1 } else { -1 };
                        add(true, m.mv[0][s], x, mby * 16, 16, mby * cbh, cbh);
                        add(true, dual(m.mv[0][s], m.dmv, 1, e), x, mby * 16, 16, mby * cbh, cbh);
                    }
                    _ => add(true, m.mv[0][s], x, mby * 16, 16, mby * cbh, cbh),
                }
            }
        }
        out
    }

    fn macroblock(&mut self, p: &Pic, st: &mut St, mbx: usize, mby: usize) {
        let frame_pic = p.structure == 3;
        let (table, flags) = self.pick_mb_type(p);
        let intra = flags & MB_INTRA != 0;
        let concealment = intra && self.cfg.concealment;
        let mut pmv = st.pmv;
        let moving = flags & (MB_FWD | MB_BWD) != 0;
        let mut m = self.pick_motion(p, flags, concealment, (mbx, mby), pmv);
        self.w.code(code_of(table, flags));
        if moving {
            self.w.put(m.mtype as u32, 2);
        }
        let dct_type = frame_pic && !p.fpfd && (intra || flags & MB_PATTERN != 0);
        if dct_type {
            self.w.put(self.rng.below(2), 1);
        }
        if flags & MB_QUANT != 0 {
            let q = self.rng.range(1, 31);
            self.set_q(q);
            self.w.put(q as u32, 5);
        }
        self.put_vectors(p, &m, concealment, &mut pmv);
        // predictor resets
        if intra {
            if !concealment {
                pmv = [[[0; 2]; 2]; 2];
            }
        } else {
            st.dc = [self.dc_reset(); 3];
            if p.ptype == PictureType::P && flags & MB_FWD == 0 {
                pmv = [[[0; 2]; 2]; 2];
                m.select[0][0] = p.parity;
            }
        }
        st.pmv = pmv;
        self.put_blocks(st, flags, intra);
        st.last = m;
    }

    fn pick_mb_type(&mut self, p: &Pic) -> (&'static [(&'static str, i16)], i16) {
        let quant = self.rng.chance(25);
        let (table, mut flags): (&'static [(&'static str, i16)], i16) = match p.ptype {
            PictureType::I => (MBTYPE_I, MB_INTRA),
            PictureType::P => (
                MBTYPE_P,
                match self.rng.below(100) {
                    0..10 => MB_INTRA,
                    10..55 => MB_FWD | MB_PATTERN,
                    55..70 if !p.lone => MB_PATTERN,
                    55..70 => MB_FWD | MB_PATTERN,
                    _ => MB_FWD,
                },
            ),
            _ => (
                MBTYPE_B,
                match self.rng.below(100) {
                    0..6 => MB_INTRA,
                    6..30 => MB_FWD | MB_BWD | MB_PATTERN,
                    30..45 => MB_FWD | MB_BWD,
                    45..60 => MB_FWD | MB_PATTERN,
                    60..70 => MB_FWD,
                    70..85 => MB_BWD | MB_PATTERN,
                    _ => MB_BWD,
                },
            ),
        };
        if quant && (flags & (MB_INTRA | MB_PATTERN) != 0) {
            flags |= MB_QUANT;
        }
        (table, flags)
    }

    /// Pick vectors until the prediction fits the picture and the predictors can reach them.
    fn pick_motion(&mut self, p: &Pic, flags: i16, concealment: bool, pos: (usize, usize), pmv: [[[i32; 2]; 2]; 2]) -> M {
        let (mbx, mby) = pos;
        let frame_pic = p.structure == 3;
        let mut m = M { flags, ..Default::default() };
        let moving = flags & (MB_FWD | MB_BWD) != 0;
        if moving || concealment {
            let mut ok = false;
            for _ in 0..40 {
                let mut cand = M { flags, ..Default::default() };
                cand.mtype = if concealment {
                    if frame_pic { 2 } else { 1 }
                } else {
                    let dual_ok = self.cfg.dual && p.ptype == PictureType::P && !p.lone;
                    match self.rng.below(if dual_ok { 3 } else { 2 }) {
                        0 => 1,
                        1 => 2,
                        _ => 3,
                    }
                };
                let spread = if self.rng.chance(20) { 0 } else { 31 };

                for r in 0..2 {
                    for s in 0..2 {
                        cand.select[r][s] = self.rng.below(2) as usize;
                        cand.mv[r][s] = [self.rng.range(-spread - 1, spread), self.rng.range(-spread - 1, spread)];
                    }
                }
                cand.dmv = [self.rng.range(-1, 1), self.rng.range(-1, 1)];
                if cand.mtype == 3 {
                    cand.select = [[p.parity; 2]; 2];
                }
                if p.lone {
                    cand.select = [[1 - p.parity; 2]; 2];
                }
                if (concealment || self.fits(p, &cand, mbx, mby)) && representable(p, &cand, concealment, pmv) {
                    m = cand;
                    ok = true;
                    break;
                }
            }
            if !ok && concealment {
                // a vector the (possibly out-of-range) predictor reaches unambiguously
                let mut c = M { flags, mtype: if frame_pic { 2 } else { 1 }, ..Default::default() };
                for t in 0..2 {
                    let pred = pmv[0][0][t];
                    c.mv[0][0][t] = if pred < -32 {
                        pred + 32
                    } else if pred > 31 {
                        pred - 32
                    } else {
                        0
                    };
                }
                m = c;
            } else if !ok {
                // zero vectors from the same parity always fit; field vectors (halved
                // predictors) are always representable
                let sel = if p.lone { 1 - p.parity } else { p.parity };
                m = if frame_pic {
                    M { flags, mtype: 1, select: [[0, 0], [1, 1]], ..Default::default() }
                } else {
                    M { flags, mtype: 1, select: [[sel; 2]; 2], ..Default::default() }
                };
                assert!(representable(p, &m, concealment, pmv));
            }
        }
        m
    }

    /// Vectors, coded differentially against the predictors as the decoder forms them.
    fn put_vectors(&mut self, p: &Pic, m: &M, concealment: bool, pmv: &mut [[[i32; 2]; 2]; 2]) {
        let frame_pic = p.structure == 3;
        let flags = m.flags;
        let (count, field_fmt, dualp) = match (frame_pic, m.mtype) {
            (true, 1) => (2, true, false),
            (_, 3) => (1, true, true),
            (true, _) => (1, false, false),
            (false, 2) => (2, true, false),
            (false, _) => (1, true, false),
        };
        for s in 0..2 {
            let present = if s == 0 { flags & MB_FWD != 0 || concealment } else { flags & MB_BWD != 0 };
            if !present {
                continue;
            }
            for r in 0..count {
                if count == 2 || (field_fmt && !dualp) {
                    self.w.put(m.select[r][s] as u32, 1);
                }
                for c in 0..2 {
                    let halve = field_fmt && c == 1 && frame_pic;
                    let pred = if halve { pmv[r][s][c] >> 1 } else { pmv[r][s][c] };
                    let v = m.mv[r][s][c];
                    self.motion_code(v - pred);
                    if dualp {
                        self.w.code(code_of(DMV, m.dmv[c] as i16));
                    }
                    pmv[r][s][c] = if halve { v * 2 } else { v };
                }
            }
            if count == 1 {
                pmv[1][s] = pmv[0][s];
            }
        }
        if concealment {
            self.w.put(1, 1);
        }
    }

    /// coded_block_pattern and the blocks it selects.
    fn put_blocks(&mut self, st: &mut St, flags: i16, intra: bool) {
        let nblocks = if self.cfg.chroma == 2 { 8 } else { 6 };
        let cbp: u32 = if flags & MB_PATTERN != 0 {
            // the coded_block_pattern code for 0 is only for 4:2:2 and 4:4:4 (Table B.9)
            let v = self.rng.range(1, (1 << nblocks) - 1) as u32;
            self.w.code(code_of(CBP, (v >> (nblocks - 6)) as i16));
            if nblocks == 8 {
                self.w.put(v & 3, 2);
            }
            v
        } else if intra {
            (1 << nblocks) - 1
        } else {
            0
        };
        for i in 0..nblocks {
            if cbp & (1 << (nblocks - 1 - i)) != 0 {
                let cc = if i < 4 { 0 } else { 1 + (i & 1) };
                self.block(st, cc, intra);
            }
        }
    }

    fn motion_code(&mut self, delta: i32) {
        let f = 1 << (F_CODE - 1);
        let range = 32 * f;
        let mut d = delta;
        if d < -16 * f {
            d += range;
        } else if d > 16 * f - 1 {
            d -= range;
        }
        if d == 0 {
            self.w.code(code_of(MOTION, 0));
            return;
        }
        let a = d.abs() - 1;
        let code = (a >> (F_CODE - 1)) + 1;
        self.w.code(code_of(MOTION, (code * d.signum()) as i16));
        self.w.put((a & (f - 1)) as u32, (F_CODE - 1) as u32);
    }

    fn block(&mut self, st: &mut St, cc: usize, intra: bool) {
        let tab = (intra && self.cfg.intra_vlc) as usize;
        let mut first = !intra;
        if intra {
            let max = (1 << (8 + self.cfg.dc_prec)) - 1;
            let target = (st.dc[cc] + self.rng.range(-40, 40)).clamp(0, max);
            let diff = target - st.dc[cc];
            st.dc[cc] = target;
            let size = 32 - diff.unsigned_abs().leading_zeros();
            self.w.code(code_of(if cc == 0 { DC_LUMA } else { DC_CHROMA }, size as i16));
            if size > 0 {
                let v = if diff > 0 { diff } else { diff + (1 << size) - 1 };
                self.w.put(v as u32, size);
            }
        }
        let n = if intra { self.rng.below(6) } else { 1 + self.rng.below(6) };
        let mut idx: i32 = if intra { 0 } else { -1 };
        for _ in 0..n {
            let run = self.rng.range(0, 6);
            if idx + run + 1 > 63 {
                break;
            }
            idx += run + 1;
            // keep dequantised values within ±300 (W ≤ 83 for the default intra matrix): realistic
            // residuals (extreme ones overflow ffmpeg's integer IDCT, which then differs)
            let cap = if intra { 300 * 16 / (self.qs * 83) } else { (300 * 32 / (self.qs * 16) - 1) / 2 }.clamp(1, 2047);
            let mag = if self.rng.chance(8) { self.rng.range(cap.min(20), cap.min(400)) } else { self.rng.range(1, cap.min(4)) };
            let neg = self.rng.chance(50);
            if first && run == 0 && mag == 1 {
                self.w.code("1");
                self.w.put(neg as u32, 1);
            } else if let Some(c) = self.dct_codes[tab].get(&(run as u32, mag as u32)).filter(|_| !self.rng.chance(5)) {
                self.w.code(c);
                self.w.put(neg as u32, 1);
            } else {
                self.w.code("0000 01");
                self.w.put(run as u32, 6);
                let l = if neg { -mag } else { mag };
                self.w.put((l & 0xFFF) as u32, 12);
            }
            first = false;
        }
        if first {
            // a coded non-intra block needs a coefficient
            self.w.code("1");
            self.w.put(0, 1);
        }
        self.w.code(if tab == 1 { "0110" } else { "10" });
    }
}

/// Whether every vector of `m` decodes to itself under both the literal §7.6.3.1 wrap and the
/// reference decoder's (they differ only for out-of-range predictors).
fn representable(p: &Pic, m: &M, concealment: bool, mut pmv: [[[i32; 2]; 2]; 2]) -> bool {
    let frame_pic = p.structure == 3;
    let f = 1 << (F_CODE - 1);
    let (count, field_fmt, _) = match (frame_pic, m.mtype) {
        (true, 1) => (2, true, false),
        (_, 3) => (1, true, true),
        (true, _) => (1, false, false),
        (false, 2) => (2, true, false),
        (false, _) => (1, true, false),
    };
    for s in 0..2 {
        let present = if s == 0 { m.flags & MB_FWD != 0 || concealment } else { m.flags & MB_BWD != 0 };
        if !present {
            continue;
        }
        for r in 0..count {
            for c in 0..2 {
                let halve = field_fmt && c == 1 && frame_pic;
                let pred = if halve { pmv[r][s][c] >> 1 } else { pmv[r][s][c] };
                let v = m.mv[r][s][c];
                let mut d = v - pred;
                if d < -16 * f {
                    d += 32 * f;
                } else if d > 16 * f - 1 {
                    d -= 32 * f;
                }
                let literal = {
                    let x = pred + d;
                    if x < -16 * f {
                        x + 32 * f
                    } else if x > 16 * f - 1 {
                        x - 32 * f
                    } else {
                        x
                    }
                };
                if crate::slice::wrap_vector(pred, d, f) != v || literal != v {
                    return false;
                }
                pmv[r][s][c] = if halve { v * 2 } else { v };
            }
        }
        if count == 1 {
            pmv[1][s] = pmv[0][s];
        }
    }
    true
}

fn dual(v: [i32; 2], dmv: [i32; 2], m: i32, e: i32) -> [i32; 2] {
    let s = |x: i32| (x * m + (x > 0) as i32) >> 1;
    [s(v[0]) + dmv[0], s(v[1]) + e + dmv[1]]
}

/// Write a stream: closed GOP, I/P/B frame order I0 P3 B1 B2 P6 B4 B5 … (or I0 P1 P2 … without
/// B pictures); field-coded frames are a pair of field pictures (I + P for the first frame).
fn stream(cfg: Cfg) -> Vec<u8> {
    let mut e = Enc::new(cfg);
    e.sequence();
    let mut order: Vec<(u16, PictureType)> = vec![(0, PictureType::I)];
    if cfg.b_frames {
        let mut a = 0u16;
        while (a as usize) + 3 < cfg.frames {
            order.push((a + 3, PictureType::P));
            order.push((a + 1, PictureType::B));
            order.push((a + 2, PictureType::B));
            a += 3;
        }
    } else {
        for k in 1..cfg.frames as u16 {
            order.push((k, PictureType::P));
        }
    }
    let (first, second) = if cfg.tff { (1, 2) } else { (2, 1) };
    for (tref, t) in order {
        if cfg.field_pics {
            e.picture(tref, t, first);
            e.picture(tref, if t == PictureType::I { PictureType::P } else { t }, second);
        } else {
            e.picture(tref, t, 3);
        }
    }
    e.w.start(0xB7);
    e.w.out
}

fn check(cfg: Cfg) {
    let es = stream(cfg);
    let (pics, errors) = {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for (i, r) in access_units(&es).into_iter().enumerate() {
            out.extend(d.decode(&es[r], i as i64).unwrap());
        }
        out.extend(d.flush());
        (out, d.errors())
    };
    assert_eq!(errors, 0, "{}: our decoder reports slice errors", cfg.name);
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip(cfg.name) else { return };
    let path = filmcraft_testkit::fixtures_dir("mpeg2v").join(format!("{}_{:x}.m2v", cfg.name, cfg.seed));
    std::fs::write(&path, &es).unwrap();
    let fmt = if cfg.chroma == 2 { "yuv422p" } else { "yuv420p" };
    let o = std::process::Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-xerror", "-i"])
        .arg(&path)
        .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", fmt, "-"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}: ffmpeg rejects the stream: {}", cfg.name, String::from_utf8_lossy(&o.stderr));
    let raw = o.stdout;
    let (w, h) = (cfg.w, cfg.h);
    let (cw, ch) = (w / 2, if cfg.chroma == 2 { h } else { h / 2 });
    let fs = w * h + 2 * cw * ch;
    assert_eq!(raw.len() / fs, pics.len(), "{}: frame count", cfg.name);
    let mut worst = 0u8;
    let (mut sse, mut n, mut differ) = (0f64, 0u64, 0u64);
    // (frame, type, macroblock x, y, line) of the first luma difference above 4
    let mut first_bad = None;
    for (i, p) in pics.iter().enumerate() {
        assert_eq!(p.temporal_reference as usize, i, "{}: display order", cfg.name);
        assert_eq!(p.top_field_first, cfg.tff);
        let f = &raw[i * fs..(i + 1) * fs];
        if first_bad.is_none()
            && let Some(k) = (0..w * h).find(|&k| p.y[k].abs_diff(f[k]) > 4)
        {
            first_bad = Some((i, p.picture_type, k % w / 16, k / w / 16, k / w % 16));
        }
        for (a, b) in p.y.iter().chain(&p.cb).chain(&p.cr).zip(f) {
            let d = a.abs_diff(*b);
            worst = worst.max(d);
            sse += (d as f64).powi(2);
            differ += (d != 0) as u64;
            n += 1;
        }
    }
    let psnr = 10.0 * (255f64 * 255.0 * n as f64 / sse.max(1e-9)).log10();
    println!("{}: {} frames, max diff {worst}, {:.3}% differ, PSNR {psnr:.2} dB", cfg.name, pics.len(), 100.0 * differ as f64 / n as f64);
    // the same criteria as the ffmpeg-encoded fixtures (tests/it/oracle.rs)
    assert!(worst <= 4 && psnr >= 58.0, "{} (seed {:#x}): max diff {worst}, PSNR {psnr:.2}, first at {first_bad:?}", cfg.name, cfg.seed);
}

const BASE: Cfg = Cfg {
    name: "",
    seed: 1,
    chroma: 1,
    w: 96,
    h: 64,
    field_pics: true,
    tff: true,
    b_frames: true,
    dual: false,
    alt_scan: false,
    intra_vlc: true,
    q_scale_type: false,
    dc_prec: 0,
    concealment: true,
    frames: 7,
};

/// Every configuration with more random streams.
#[test]
fn seed_sweep() {
    let cfgs = [
        Cfg { name: "sweep_field_ibp", ..BASE },
        Cfg { name: "sweep_field_dual_422", b_frames: false, dual: true, chroma: 2, tff: false, frames: 4, ..BASE },
        Cfg { name: "sweep_frame_dual", field_pics: false, b_frames: false, dual: true, frames: 4, dc_prec: 3, ..BASE },
        Cfg { name: "sweep_frame_ib_422", field_pics: false, chroma: 2, tff: false, q_scale_type: true, ..BASE },
    ];
    for seed in 1..=8u64 {
        for c in cfgs {
            check(Cfg { seed: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ c.w as u64, ..c });
        }
    }
}

#[test]
fn field_pictures_with_b_and_16x8() {
    check(Cfg { name: "synth_field_ibp_tff", seed: 0x1234_5678_9abc, ..BASE });
    check(Cfg {
        name: "synth_field_ibp_bff_422",
        seed: 0xfeed_beef_1234,
        tff: false,
        chroma: 2,
        alt_scan: true,
        q_scale_type: true,
        dc_prec: 2,
        w: 112,
        h: 96,
        ..BASE
    });
}

#[test]
fn dual_prime_in_field_pictures() {
    check(Cfg { name: "synth_field_dual_tff", seed: 0x0bad_cafe_0042, b_frames: false, dual: true, frames: 5, ..BASE });
    check(Cfg {
        name: "synth_field_dual_bff_422",
        seed: 0x1357_9bdf_2468,
        b_frames: false,
        dual: true,
        tff: false,
        chroma: 2,
        concealment: false,
        frames: 5,
        ..BASE
    });
}

#[test]
fn frame_pictures_field_mc_and_dual_prime() {
    check(Cfg { name: "synth_frame_dual_tff", seed: 0x5555_aaaa_1111, field_pics: false, b_frames: false, dual: true, frames: 6, ..BASE });
    check(Cfg {
        name: "synth_frame_dual_bff_422",
        seed: 0x7777_3333_9999,
        field_pics: false,
        b_frames: false,
        dual: true,
        tff: false,
        chroma: 2,
        dc_prec: 1,
        frames: 6,
        ..BASE
    });
    check(Cfg {
        name: "synth_frame_ib_422",
        seed: 0x2468_ace0_1357,
        field_pics: false,
        chroma: 2,
        alt_scan: true,
        intra_vlc: false,
        q_scale_type: true,
        w: 128,
        h: 96,
        ..BASE
    });
}
