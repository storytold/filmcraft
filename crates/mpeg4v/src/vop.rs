//! Macroblock layer of a VOP: motion vectors, AC/DC prediction, texture and reconstruction
//! (ISO/IEC 14496-2 §6.2.6, §7.3-§7.6), with the video packets of §6.2.5.2.
//!
//! Hostile streams: a macroblock that does not parse ends its packet. Decoding goes on at the next
//! resync marker; macroblocks nothing decoded are filled from the reference at the end of the VOP.

use crate::bits::Bits;
use crate::headers::{ALTERNATE_HORIZONTAL, ALTERNATE_VERTICAL, PacketHeader, Vol, VopHeader, VopType, ZIGZAG, parse_packet};
use crate::idct::idct;
use crate::mc::{Frame, Mc, Out, average};
use crate::vlc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mv {
    pub x: i16,
    pub y: i16,
}

impl Mv {
    #[inline]
    fn pair(self) -> (i32, i32) {
        (i32::from(self.x), i32::from(self.y))
    }
}

pub(crate) const MODE_INTER: u8 = 0;
pub(crate) const MODE_4V: u8 = 1;
pub(crate) const MODE_INTRA: u8 = 2;
pub(crate) const MODE_SKIP: u8 = 3;

/// What intra blocks keep for the prediction of their neighbours: reconstructed DC, and the first
/// row and column of quantised coefficients.
#[derive(Clone, Copy)]
pub(crate) struct IntraPred {
    dc: [i16; 6],
    row: [[i16; 7]; 6],
    col: [[i16; 7]; 6],
    qp: u8,
}

const NO_PRED: IntraPred = IntraPred { dc: [1024; 6], row: [[0; 7]; 6], col: [[0; 7]; 6], qp: 1 };

/// Per-macroblock state.
#[derive(Default)]
pub(crate) struct Side {
    /// Vectors and modes of the anchor being decoded.
    pub mvs: Vec<[Mv; 4]>,
    pub mode: Vec<u8>,
    /// Vectors and modes of the last anchor (the future reference of B-VOPs).
    pub col_mvs: Vec<[Mv; 4]>,
    pub col_mode: Vec<u8>,
    /// The packet each macroblock was last decoded in: only macroblocks of the current packet
    /// predict one another. Numbers never repeat, so old values need no clearing.
    pub packet: Vec<u32>,
    pub pred: Vec<IntraPred>,
    pub next_packet: u32,
}

impl Side {
    pub fn resize(&mut self, n: usize) {
        self.mvs = vec![[Mv::default(); 4]; n];
        self.col_mvs = vec![[Mv::default(); 4]; n];
        self.mode = vec![MODE_SKIP; n];
        self.col_mode = vec![MODE_INTRA; n];
        self.packet = vec![0; n];
        self.pred = vec![NO_PRED; n];
        self.next_packet = 1;
    }

    fn new_packet(&mut self) -> u32 {
        let p = self.next_packet;
        self.next_packet = p.wrapping_add(1).max(1);
        p
    }
}

/// `a // b`: division rounded to the nearest integer, halves away from zero. `b` > 0.
#[inline]
fn round_div(a: i32, b: i32) -> i32 {
    if a >= 0 { (a + (b >> 1)) / b } else { (a - (b >> 1)) / b }
}

#[inline]
fn dc_scaler(qp: i32, luma: bool) -> i32 {
    if luma {
        match qp {
            ..=4 => 8,
            5..=8 => 2 * qp,
            9..=24 => qp + 8,
            _ => 2 * qp - 16,
        }
    } else {
        match qp {
            ..=4 => 8,
            5..=24 => (qp + 13) / 2,
            _ => qp - 6,
        }
    }
}

#[inline]
fn median(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(a.min(b).max(c))
}

/// Where block `n` of macroblock `x`, `y` lies: plane, stride, offset.
#[inline]
fn block_place(cur: &mut Frame, x: usize, y: usize, n: usize) -> (&mut Vec<u8>, usize, usize) {
    let w = cur.w;
    match n {
        0..=3 => (&mut cur.y, w, (y * 16 + (n >> 1) * 8) * w + x * 16 + (n & 1) * 8),
        4 => (&mut cur.u, w / 2, y * 8 * (w / 2) + x * 8),
        _ => (&mut cur.v, w / 2, y * 8 * (w / 2) + x * 8),
    }
}

/// A macroblock or packet that does not parse.
pub(crate) struct Damaged;
type R<T = ()> = std::result::Result<T, Damaged>;

trait Ok<T> {
    fn ok(self) -> R<T>;
}
impl<T> Ok<T> for Option<T> {
    #[inline(always)]
    fn ok(self) -> R<T> {
        self.ok_or(Damaged)
    }
}

/// Table 6-21: the quantiser from which intra_dc_vlc_thr codes intra DCs with the AC table.
const DC_VLC_BELOW: [i32; 8] = [32, 13, 15, 17, 19, 21, 23, 0];

/// A macroblock's texture header: what the data-partitioned syntax reads apart from its blocks.
#[derive(Clone, Copy, Default)]
struct MbHead {
    intra: bool,
    four: bool,
    skip: bool,
    /// dquant follows.
    quant: bool,
    ac_pred: bool,
    cbp: u8,
    qp: u8,
    /// Differential DCs read in advance (data partitioning).
    dc: Option<[i32; 6]>,
    /// DCs are coded with their own VLC rather than as the first coefficient.
    dc_vlc: bool,
}

/// The decoding of one VOP.
pub(crate) struct Vop<'a> {
    pub br: Bits<'a>,
    pub hdr: &'a VopHeader,
    pub vol: &'a Vol,
    pub cur: &'a mut Frame,
    /// The older anchor (forward reference of B-VOPs).
    pub past: &'a Frame,
    /// The newest anchor: reference of P-VOPs, backward reference of B-VOPs.
    pub future: &'a Frame,
    pub side: &'a mut Side,
    pub mb_w: usize,
    pub mb_h: usize,
    /// Temporal distances for direct mode: from the past anchor to this VOP, and between anchors.
    pub trb: i32,
    pub trd: i32,
    pub packet: u32,
    pub first_packet: u32,
    pub qp: i32,
    /// B-VOP vector predictors.
    pub last_f: Mv,
    pub last_b: Mv,
    pub blk: [i32; 64],
    pub damaged: bool,
    pub tmp_y: [u8; 256],
    pub tmp_c: [[u8; 64]; 2],
    heads: Vec<MbHead>,
}

impl<'a> Vop<'a> {
    pub fn new(
        data: &'a [u8],
        hdr: &'a VopHeader,
        vol: &'a Vol,
        (cur, past, future): (&'a mut Frame, &'a Frame, &'a Frame),
        side: &'a mut Side,
        (mb_w, mb_h): (usize, usize),
        (trb, trd): (i32, i32),
    ) -> Vop<'a> {
        let packet = side.new_packet();
        let mut br = Bits::new(data);
        br.set_pos(hdr.data_pos);
        Vop {
            br,
            hdr,
            vol,
            cur,
            past,
            future,
            side,
            mb_w,
            mb_h,
            trb,
            trd,
            packet,
            first_packet: packet,
            qp: i32::from(hdr.quant),
            last_f: Mv::default(),
            last_b: Mv::default(),
            blk: [0; 64],
            damaged: false,
            tmp_y: [0; 256],
            tmp_c: [[0; 64]; 2],
            heads: Vec::new(),
        }
    }

    /// Zero bits of the resync marker (§6.3.5.2).
    fn marker_zeros(&self) -> u32 {
        match self.hdr.kind {
            VopType::I => 16,
            VopType::B => 15 + u32::from(self.hdr.fcode_f.max(self.hdr.fcode_b).max(2)),
            _ => 15 + u32::from(self.hdr.fcode_f),
        }
    }

    /// Stuffing bits before a resync marker at the current position, when there is one.
    #[inline]
    fn resync_ahead(&self, zeros: u32) -> Option<u32> {
        let n = 8 - (self.br.pos() & 7) as u32;
        let w = self.br.peek32();
        // stuffing: a 0 then 1s up to the byte boundary
        let stuffing = (1u32 << (n - 1)) - 1;
        if w >> (32 - n) != stuffing || (w << n) >> (32 - (zeros + 1)) != 1 {
            return None;
        }
        Some(n)
    }

    /// The next resync marker at a byte boundary from here on: leaves `br` just after it.
    fn find_resync(&mut self, zeros: u32) -> bool {
        let mut byte = self.br.pos().div_ceil(8);
        let n = self.br.len_bytes();
        while byte + 2 < n {
            self.br.set_pos(byte * 8);
            if self.br.peek(zeros + 1) == 1 {
                self.br.skip(zeros + 1);
                return true;
            }
            byte += 1;
        }
        false
    }

    pub fn run(&mut self) -> R {
        let total = self.mb_w * self.mb_h;
        let mb_bits = (total as u32).next_power_of_two().trailing_zeros();
        let zeros = self.marker_zeros();
        let mut mb = 0usize;
        let mut pending: Option<PacketHeader> = None;
        loop {
            if let Some(h) = pending.take() {
                // Macroblocks a B-VOP does not code take no bits: a packet may start before or
                // after a run of them, which were reconstructed on the way here.
                let behind = h.mb < mb && !(h.mb..mb).all(|m| self.col_skipped(m));
                if behind || h.mb >= total {
                    self.damaged = true;
                    return Err(Damaged);
                }
                mb = mb.max(h.mb);
                self.qp = i32::from(h.quant);
                self.packet = self.side.new_packet();
                self.last_f = Mv::default();
                self.last_b = Mv::default();
            }
            let r = if self.vol.data_partitioned { self.packet_partitioned(&mut mb, zeros) } else { self.packet_plain(&mut mb, zeros) };
            if r.is_err() {
                self.damaged = true;
                if !self.find_resync(zeros) {
                    return Ok(());
                }
            } else if mb >= total {
                // what the last macroblock left (nothing, in a whole VOP)
                self.damaged |= self.br.overrun();
                return Ok(());
            }
            let hdr = self.hdr;
            let h = parse_packet(&mut self.br, mb_bits, self.vol, hdr);
            match h {
                Some(h) => pending = Some(h),
                None => {
                    self.damaged = true;
                    return Err(Damaged);
                }
            }
        }
    }

    /// The macroblocks of one packet, up to the next resync marker (the bits after it are the
    /// packet header) or the end of the VOP.
    fn packet_plain(&mut self, mb: &mut usize, zeros: u32) -> R {
        let total = self.mb_w * self.mb_h;
        let resync = !self.vol.resync_disable;
        while *mb < total {
            let (x, y) = (*mb % self.mb_w, *mb / self.mb_w);
            if x == 0 {
                // B-VOP vector predictors start each row at zero
                self.last_f = Mv::default();
                self.last_b = Mv::default();
            }
            if self.col_skipped(*mb) {
                if let Some(p) = self.side.packet.get_mut(*mb) {
                    *p = self.packet;
                }
                self.copy_past(x, y);
                *mb += 1;
                continue;
            }
            // also at a packet's first macroblock: packets that hold only macroblocks taking no
            // bits leave one marker right after another
            if resync && let Some(stuff) = self.resync_ahead(zeros) {
                self.br.skip(stuff + zeros + 1);
                return Ok(());
            }
            if let Some(p) = self.side.packet.get_mut(*mb) {
                *p = self.packet;
            }
            let r = match self.hdr.kind {
                VopType::I => self.mb_i(*mb, x, y),
                VopType::B => self.mb_b(*mb, x, y),
                _ => self.mb_p(*mb, x, y),
            };
            if r.is_err() || self.br.overrun() {
                // left to the concealment
                if let Some(p) = self.side.packet.get_mut(*mb) {
                    *p = 0;
                }
                return Err(Damaged);
            }
            *mb += 1;
        }
        Ok(())
    }

    fn avail(&self, mb: Option<usize>) -> Option<usize> {
        let m = mb?;
        (self.side.packet.get(m) == Some(&self.packet)).then_some(m)
    }

    fn left(&self, mb: usize, x: usize) -> Option<usize> {
        self.avail((x > 0).then(|| mb - 1))
    }
    fn up(&self, mb: usize, y: usize) -> Option<usize> {
        self.avail((y > 0).then(|| mb - self.mb_w))
    }
    fn up_left(&self, mb: usize, x: usize, y: usize) -> Option<usize> {
        self.avail((x > 0 && y > 0).then(|| mb - self.mb_w - 1))
    }
    fn up_right(&self, mb: usize, x: usize, y: usize) -> Option<usize> {
        self.avail((y > 0 && x + 1 < self.mb_w).then(|| mb + 1 - self.mb_w))
    }

    /// Whether intra DCs have their own VLC: intra_dc_vlc_thr against the running quantiser, that
    /// of the macroblock before (§6.3.6), so this comes before the macroblock's dquant.
    #[inline]
    fn dc_vlc(&self) -> bool {
        self.qp < DC_VLC_BELOW.get(usize::from(self.hdr.dc_thr)).copied().unwrap_or(32)
    }

    /// Update the running quantiser with dquant.
    #[inline]
    fn dquant(&mut self) {
        const D: [i32; 4] = [-1, -2, 1, 2];
        let d = D.get(self.br.read(2) as usize).copied().unwrap_or(0);
        self.qp = (self.qp + d).clamp(1, 31);
    }

    fn mb_i(&mut self, mb: usize, x: usize, y: usize) -> R {
        let m = loop {
            let m = vlc::mcbpc_intra(&mut self.br).ok()?;
            if m != vlc::STUFFING {
                break m;
            }
            if self.br.overrun() {
                return Err(Damaged);
            }
        };
        let ac_pred = self.br.bit();
        let cbpy = vlc::cbpy(&mut self.br).ok()?;
        let dc_vlc = self.dc_vlc();
        if m >> 2 == 4 {
            self.dquant();
        }
        let qp = self.qp as u8;
        self.set_mb(mb, MODE_INTRA, [Mv::default(); 4]);
        self.intra(mb, x, y, MbHead { intra: true, ac_pred, cbp: cbpy << 2 | (m & 3), qp, dc_vlc, ..MbHead::default() })
    }

    #[inline]
    fn set_mb(&mut self, mb: usize, mode: u8, mvs: [Mv; 4]) {
        if let (Some(m), Some(v)) = (self.side.mode.get_mut(mb), self.side.mvs.get_mut(mb)) {
            *m = mode;
            *v = mvs;
        }
    }

    /// Vector candidates for block `k` (§7.6.5): left, above, above right.
    fn pred_mv(&self, mb: usize, x: usize, y: usize, k: usize, cur: &[Mv; 4]) -> (i32, i32) {
        let at = |m: Option<usize>, b: usize| m.and_then(|m| self.side.mvs.get(m)).and_then(|v| v.get(b)).copied();
        let here = |b: usize| cur.get(b).copied();
        let (c1, c2, c3) = match k {
            0 => (at(self.left(mb, x), 1), at(self.up(mb, y), 2), at(self.up_right(mb, x, y), 2)),
            1 => (here(0), at(self.up(mb, y), 3), at(self.up_right(mb, x, y), 2)),
            2 => (at(self.left(mb, x), 3), here(0), here(1)),
            _ => (here(2), here(0), here(1)),
        };
        let valid = c1.is_some() as u8 + c2.is_some() as u8 + c3.is_some() as u8;
        match valid {
            0 => (0, 0),
            1 => c1.or(c2).or(c3).map_or((0, 0), Mv::pair),
            _ => {
                let (a, b, c) = (c1.unwrap_or_default().pair(), c2.unwrap_or_default().pair(), c3.unwrap_or_default().pair());
                (median(a.0, b.0, c.0), median(a.1, b.1, c.1))
            }
        }
    }

    /// One vector: `pred` plus the coded difference, wrapped into the range of `fcode`.
    #[inline]
    fn read_mv(&mut self, pred: (i32, i32), fcode: u32) -> R<Mv> {
        let dx = vlc::mvd(&mut self.br, fcode).ok()?;
        let dy = vlc::mvd(&mut self.br, fcode).ok()?;
        let high = (32i32 << (fcode - 1)) - 1;
        let low = -(32i32 << (fcode - 1));
        let range = 64i32 << (fcode - 1);
        let wrap = |v: i32| {
            if v < low {
                v + range
            } else if v > high {
                v - range
            } else {
                v
            }
        };
        Ok(Mv { x: wrap(pred.0 + dx) as i16, y: wrap(pred.1 + dy) as i16 })
    }

    /// The motion part of a P macroblock; returns the vectors.
    fn read_p_vectors(&mut self, mb: usize, x: usize, y: usize, four: bool) -> R<[Mv; 4]> {
        let f = u32::from(self.hdr.fcode_f);
        let mut v = [Mv::default(); 4];
        if four {
            for k in 0..4 {
                let p = self.pred_mv(mb, x, y, k, &v);
                let m = self.read_mv(p, f)?;
                if let Some(o) = v.get_mut(k) {
                    *o = m;
                }
            }
        } else {
            let p = self.pred_mv(mb, x, y, 0, &v);
            v = [self.read_mv(p, f)?; 4];
        }
        Ok(v)
    }

    fn mb_p(&mut self, mb: usize, x: usize, y: usize) -> R {
        let m = loop {
            if self.br.bit() {
                self.set_mb(mb, MODE_SKIP, [Mv::default(); 4]);
                self.predict_forward(x, y, &[Mv::default(); 4], false);
                return Ok(());
            }
            let m = vlc::mcbpc_inter(&mut self.br).ok()?;
            if m != vlc::STUFFING {
                break m;
            }
            if self.br.overrun() {
                return Err(Damaged);
            }
        };
        let (kind, cbpc) = (m >> 2, m & 3);
        if kind >= 3 {
            let ac_pred = self.br.bit();
            let cbpy = vlc::cbpy(&mut self.br).ok()?;
            let dc_vlc = self.dc_vlc();
            if kind == 4 {
                self.dquant();
            }
            let qp = self.qp as u8;
            self.set_mb(mb, MODE_INTRA, [Mv::default(); 4]);
            return self.intra(mb, x, y, MbHead { intra: true, ac_pred, cbp: cbpy << 2 | cbpc, qp, dc_vlc, ..MbHead::default() });
        }
        let cbpy = 15 - vlc::cbpy(&mut self.br).ok()?;
        if kind == 1 {
            self.dquant();
        }
        let four = kind == 2;
        let v = self.read_p_vectors(mb, x, y, four)?;
        self.set_mb(mb, if four { MODE_4V } else { MODE_INTER }, v);
        self.predict_forward(x, y, &v, four);
        self.residual(x, y, cbpy << 2 | cbpc)
    }

    /// Macroblock `x`, `y` of the current frame as prediction destination.
    fn out(cur: &mut Frame, x: usize, y: usize) -> Option<Out<'_>> {
        let (ys, cs) = (cur.w, cur.w / 2);
        let (yo, co) = (y * 16 * ys + x * 16, y * 8 * cs + x * 8);
        let Frame { y: py, u: pu, v: pv, .. } = cur;
        Some(Out { y: py.get_mut(yo..)?, u: pu.get_mut(co..)?, v: pv.get_mut(co..)?, ys, cs })
    }

    /// Prediction from the reference of a P-VOP.
    fn predict_forward(&mut self, x: usize, y: usize, v: &[Mv; 4], four: bool) {
        let mc = Mc { rounding: self.hdr.rounding, quarter: self.vol.quarter_sample };
        let Some(mut o) = Self::out(self.cur, x, y) else { return };
        if four {
            mc.four(&mut o, self.future, x, y, &v.map(Mv::pair));
        } else {
            mc.one(&mut o, self.future, x, y, v[0].pair());
        }
    }

    /// Texture of an inter macroblock: blocks of `cbp` added to the prediction.
    fn residual(&mut self, x: usize, y: usize, cbp: u8) -> R {
        if cbp == 0 {
            return Ok(());
        }
        for n in 0..6 {
            if cbp & (32 >> n) != 0 {
                let dc_only = self.inter_block()?;
                idct(&mut self.blk, dc_only);
                self.add_block(x, y, n);
                self.blk = [0; 64];
            }
        }
        Ok(())
    }

    /// One inter block: coefficients inverse quantised into `blk` (§7.4.4); true when only the DC
    /// is set.
    #[inline]
    fn inter_block(&mut self) -> R<bool> {
        let q = self.qp;
        let mpeg = self.vol.mpeg_quant;
        let mut i = 0usize;
        let mut dc_only = true;
        let mut sum = 0i32;
        loop {
            let c = vlc::coef(&vlc::TCOEF_INTER, &mut self.br).ok()?;
            i += c.run;
            let pos = usize::from(*ZIGZAG.get(i).ok()?);
            let a = c.level.abs();
            let v = if mpeg {
                let w = i32::from(self.vol.nonintra_matrix[pos & 63]);
                ((2 * a + 1) * w * q) >> 4
            } else {
                (2 * a + 1) * q - ((q & 1) ^ 1)
            };
            let v = if c.level < 0 { -v } else { v }.clamp(-2048, 2047);
            self.blk[pos & 63] = v;
            sum += v;
            dc_only &= pos == 0;
            i += 1;
            if c.last {
                break;
            }
        }
        if mpeg && sum & 1 == 0 {
            self.blk[63] ^= 1;
            dc_only = false;
        }
        Ok(dc_only)
    }

    /// Add the spatial block in `blk` to block `n` of macroblock `x`, `y`.
    #[inline]
    fn add_block(&mut self, x: usize, y: usize, n: usize) {
        let (plane, stride, off) = block_place(self.cur, x, y, n);
        let Some(d) = plane.get_mut(off..) else { return };
        for (row, r) in d.chunks_mut(stride).zip(self.blk.as_chunks::<8>().0) {
            for (o, v) in row.iter_mut().zip(r) {
                *o = (i32::from(*o) + v).clamp(0, 255) as u8;
            }
        }
    }

    /// Store the spatial block in `blk` as block `n` of macroblock `x`, `y`.
    #[inline]
    fn put_block(&mut self, x: usize, y: usize, n: usize) {
        let (plane, stride, off) = block_place(self.cur, x, y, n);
        let Some(d) = plane.get_mut(off..) else { return };
        for (row, r) in d.chunks_mut(stride).zip(self.blk.as_chunks::<8>().0) {
            for (o, v) in row.iter_mut().zip(r) {
                *o = (*v).clamp(0, 255) as u8;
            }
        }
    }

    /// Neighbouring blocks of block `n` for intra prediction: left (A), above left (B), above (C),
    /// each as (macroblock, block) when it is an intra block of this packet.
    fn intra_neighbours(&self, mb: usize, x: usize, y: usize, n: usize) -> [Option<(usize, usize)>; 3] {
        let intra = |m: Option<usize>, b: usize| m.filter(|&m| self.side.mode.get(m) == Some(&MODE_INTRA)).map(|m| (m, b));
        let (l, u, ul) = (self.left(mb, x), self.up(mb, y), self.up_left(mb, x, y));
        match n {
            0 => [intra(l, 1), intra(ul, 3), intra(u, 2)],
            1 => [Some((mb, 0)), intra(u, 2), intra(u, 3)],
            2 => [intra(l, 3), intra(l, 1), Some((mb, 0))],
            3 => [Some((mb, 2)), Some((mb, 0)), Some((mb, 1))],
            _ => [intra(l, n), intra(ul, n), intra(u, n)],
        }
    }

    /// An intra macroblock (§7.3, §7.4.3, §7.4.4).
    fn intra(&mut self, mb: usize, x: usize, y: usize, h: MbHead) -> R {
        let qp = i32::from(h.qp);
        if let Some(p) = self.side.pred.get_mut(mb) {
            p.qp = h.qp;
        }
        for n in 0..6 {
            let luma = n < 4;
            let [a, b, c] = self.intra_neighbours(mb, x, y, n);
            let pred = |o: Option<(usize, usize)>| o.and_then(|(m, k)| self.side.pred.get(m)?.dc.get(k).copied()).map_or(1024, i32::from);
            let (fa, fb, fc) = (pred(a), pred(b), pred(c));
            let from_above = (fa - fb).abs() < (fb - fc).abs();
            let scan = match (h.ac_pred, from_above) {
                (false, _) => &ZIGZAG,
                (true, true) => &ALTERNATE_HORIZONTAL,
                (true, false) => &ALTERNATE_VERTICAL,
            };
            let mut qf = [0i32; 64];
            let mut i = 0usize;
            let mut dc = 0;
            if let Some(d) = h.dc {
                dc = d.get(n).copied().unwrap_or(0);
                i = 1;
            } else if h.dc_vlc {
                dc = vlc::intra_dc(&mut self.br, luma).ok()?;
                i = 1;
            }
            let dc_in_table = i == 0;
            if h.cbp & (32 >> n) != 0 {
                loop {
                    let e = vlc::coef(&vlc::TCOEF_INTRA, &mut self.br).ok()?;
                    i += e.run;
                    let pos = usize::from(*scan.get(i).ok()?);
                    qf[pos & 63] = e.level.clamp(-2048, 2047);
                    i += 1;
                    if e.last {
                        break;
                    }
                }
            }
            if dc_in_table {
                dc = qf[0];
            }
            let scale = dc_scaler(qp, luma);
            let fpred = if from_above { fc } else { fa };
            let dc_rec = ((dc + round_div(fpred, scale)) * scale).clamp(0, 2047);
            qf[0] = 0;
            if h.ac_pred {
                self.predict_ac(&mut qf, if from_above { c } else { a }, from_above, qp);
            }
            if let Some(p) = self.side.pred.get_mut(mb) {
                if let Some(d) = p.dc.get_mut(n) {
                    *d = dc_rec as i16;
                }
                if let Some(r) = p.row.get_mut(n) {
                    for (k, o) in r.iter_mut().enumerate() {
                        *o = qf[(k + 1) & 63] as i16;
                    }
                }
                if let Some(r) = p.col.get_mut(n) {
                    for (k, o) in r.iter_mut().enumerate() {
                        *o = qf[((k + 1) * 8) & 63] as i16;
                    }
                }
            }
            // inverse quantisation (§7.4.4)
            let mut sum = dc_rec;
            let mut ac = false;
            let mpeg = self.vol.mpeg_quant;
            for (pos, (&v, o)) in qf.iter().zip(self.blk.iter_mut()).enumerate() {
                if v == 0 {
                    continue;
                }
                ac = true;
                let a = v.abs();
                let f = if mpeg { (2 * a * i32::from(self.vol.intra_matrix[pos & 63]) * qp) >> 4 } else { (2 * a + 1) * qp - ((qp & 1) ^ 1) };
                let f = if v < 0 { -f } else { f }.clamp(-2048, 2047);
                *o = f;
                sum += f;
            }
            self.blk[0] = dc_rec;
            if mpeg && sum & 1 == 0 {
                self.blk[63] ^= 1;
                ac = true;
            }
            idct(&mut self.blk, !ac);
            self.put_block(x, y, n);
            self.blk = [0; 64];
        }
        Ok(())
    }

    /// Add the first row or column of the neighbour `from` to `qf`, scaled to this block's
    /// quantiser (§7.4.3.3).
    fn predict_ac(&self, qf: &mut [i32; 64], from: Option<(usize, usize)>, above: bool, qp: i32) {
        let Some((m, k)) = from else { return };
        let Some(p) = self.side.pred.get(m) else { return };
        let q = i32::from(p.qp);
        let src = if above { p.row.get(k) } else { p.col.get(k) };
        let Some(src) = src else { return };
        for (i, &v) in src.iter().enumerate() {
            let pos = if above { i + 1 } else { (i + 1) * 8 };
            let add = if q == qp { i32::from(v) } else { round_div(i32::from(v) * q, qp) };
            qf[pos & 63] = (qf[pos & 63] + add).clamp(-2048, 2047);
        }
    }

    /// A macroblock of a B-VOP not coded because the anchor after it did not code it either.
    #[inline]
    fn col_skipped(&self, mb: usize) -> bool {
        self.hdr.kind == VopType::B && self.side.col_mode.get(mb) == Some(&MODE_SKIP)
    }

    /// Such a macroblock: a copy from the past anchor.
    fn copy_past(&mut self, x: usize, y: usize) {
        let mc = Mc { rounding: 0, quarter: self.vol.quarter_sample };
        if let Some(mut o) = Self::out(self.cur, x, y) {
            mc.one(&mut o, self.past, x, y, (0, 0));
        }
    }

    /// A B-VOP macroblock (§6.2.6, §7.6.9).
    fn mb_b(&mut self, mb: usize, x: usize, y: usize) -> R {
        let col_mode = self.side.col_mode.get(mb).copied().unwrap_or(MODE_INTRA);
        let (mut kind, mut cbp) = (0u32, 0u8);
        let mut skip = false;
        if self.br.bit() {
            skip = true;
        } else {
            let no_cbp = self.br.bit();
            kind = match self.br.peek(4) {
                v if v >= 8 => 1,
                v if v >= 4 => 2,
                v if v >= 2 => 3,
                1 => 4,
                _ => return Err(Damaged),
            };
            self.br.skip(kind);
            if !no_cbp {
                cbp = self.br.read(6) as u8;
            }
            if kind != 1 && cbp != 0 {
                // dbquant: 0, 10 (-2), 11 (+2)
                if self.br.bit() {
                    let d = if self.br.bit() { 2 } else { -2 };
                    self.qp = (self.qp + d).clamp(1, 31);
                }
            }
        }
        let (fwd, back) = (kind == 2 || kind == 4, kind == 2 || kind == 3);
        // kind: 1 direct, 2 interpolate, 3 backward, 4 forward
        let (mut vf, mut vb) = ([Mv::default(); 4], [Mv::default(); 4]);
        let mut four = false;
        if skip || kind == 1 {
            let d = if skip { (0, 0) } else { self.read_mv((0, 0), 1)?.pair() };
            // direct mode predicts four 8x8 blocks; with half samples two equal vectors give the
            // same samples as one 16x16 block, with the quarter-sample filter they do not
            four = col_mode == MODE_4V || self.vol.quarter_sample;
            let col = if col_mode == MODE_INTRA { [Mv::default(); 4] } else { self.side.col_mvs.get(mb).copied().unwrap_or_default() };
            let (trb, trd) = (i64::from(self.trb), i64::from(self.trd.max(1)));
            for k in 0..4 {
                let c = col[k].pair();
                let scale = |v: i32, d: i32| {
                    let (v64, d64) = (i64::from(v), i64::from(d));
                    let f = trb * v64 / trd + d64;
                    let b = if d == 0 { (trb - trd) * v64 / trd } else { f - v64 };
                    (f.clamp(-32768, 32767) as i32, b.clamp(-32768, 32767) as i32)
                };
                let (fx, bx) = scale(c.0, d.0);
                let (fy, by) = scale(c.1, d.1);
                vf[k] = Mv { x: fx as i16, y: fy as i16 };
                vb[k] = Mv { x: bx as i16, y: by as i16 };
            }
        } else {
            if fwd {
                let p = self.last_f.pair();
                self.last_f = self.read_mv(p, u32::from(self.hdr.fcode_f))?;
                vf = [self.last_f; 4];
            }
            if back {
                let p = self.last_b.pair();
                self.last_b = self.read_mv(p, u32::from(self.hdr.fcode_b))?;
                vb = [self.last_b; 4];
            }
        }
        let direct = skip || kind == 1;
        let mc = Mc { rounding: 0, quarter: self.vol.quarter_sample };
        let Some(mut o) = Self::out(self.cur, x, y) else { return Err(Damaged) };
        let predict = |o: &mut Out, r: &Frame, v: &[Mv; 4]| {
            if four {
                mc.four(o, r, x, y, &v.map(Mv::pair));
            } else {
                mc.one(o, r, x, y, v[0].pair());
            }
        };
        match (direct || fwd, direct || back) {
            (true, false) => predict(&mut o, self.past, &vf),
            (false, true) => predict(&mut o, self.future, &vb),
            _ => {
                predict(&mut o, self.past, &vf);
                let Self { tmp_y, tmp_c, .. } = self;
                let [tmp_u, tmp_v] = tmp_c;
                let mut t = Out { y: tmp_y, u: tmp_u, v: tmp_v, ys: 16, cs: 8 };
                predict(&mut t, self.future, &vb);
                average::<16>(o.y, o.ys, &self.tmp_y, 16);
                average::<8>(o.u, o.cs, &self.tmp_c[0], 8);
                average::<8>(o.v, o.cs, &self.tmp_c[1], 8);
            }
        }
        self.residual(x, y, cbp)
    }

    /// Fill what no packet reached from the newest anchor (or leave grey without one), after a
    /// damaged VOP. Returns whether anything was missing.
    pub fn conceal(&mut self) -> bool {
        let total = self.mb_w * self.mb_h;
        let mut missing = false;
        for mb in 0..total {
            let done = self.side.packet.get(mb).is_some_and(|&p| p.wrapping_sub(self.first_packet) < 1 << 24);
            if done {
                continue;
            }
            missing = true;
            let (x, y) = (mb % self.mb_w, mb / self.mb_w);
            self.set_mb(mb, MODE_SKIP, [Mv::default(); 4]);
            if self.future.w == self.cur.w && self.future.h == self.cur.h {
                let mc = Mc { rounding: 0, quarter: false };
                if let Some(mut o) = Self::out(self.cur, x, y) {
                    mc.one(&mut o, self.future, x, y, (0, 0));
                }
            }
        }
        missing
    }
}

const DC_MARKER: u32 = 0x6B001;
const DC_MARKER_BITS: u32 = 19;
const MOTION_MARKER: u32 = 0x1F001;
const MOTION_MARKER_BITS: u32 = 17;

impl Vop<'_> {
    /// One packet of a data-partitioned I- or P-VOP (§6.2.5.3): motion or DC of all its
    /// macroblocks, a marker, their headers, then their blocks. B-VOPs are never partitioned.
    fn packet_partitioned(&mut self, mb: &mut usize, zeros: u32) -> R {
        if self.hdr.kind == VopType::B {
            return self.packet_plain(mb, zeros);
        }
        let mut heads = std::mem::take(&mut self.heads);
        heads.clear();
        let first = *mb;
        let r = self.partitions(first, &mut heads).and_then(|()| self.reconstruct(mb, &heads));
        // what stage reached is not reconstructed: leave it to the concealment
        if r.is_err() {
            for m in *mb..first + heads.len() {
                if let Some(p) = self.side.packet.get_mut(m) {
                    *p = 0;
                }
            }
        }
        self.heads = heads;
        r?;
        if *mb < self.mb_w * self.mb_h {
            let stuff = self.resync_ahead(zeros).ok()?;
            self.br.skip(stuff + zeros + 1);
        }
        Ok(())
    }

    fn partitions(&mut self, first: usize, heads: &mut Vec<MbHead>) -> R {
        let total = self.mb_w * self.mb_h;
        let intra_vop = self.hdr.kind == VopType::I;
        let (marker, bits) = if intra_vop { (DC_MARKER, DC_MARKER_BITS) } else { (MOTION_MARKER, MOTION_MARKER_BITS) };
        // first partition
        loop {
            if self.br.peek(bits) == marker {
                self.br.skip(bits);
                break;
            }
            let mb = first + heads.len();
            if mb >= total || self.br.overrun() {
                return Err(Damaged);
            }
            let (x, y) = (mb % self.mb_w, mb / self.mb_w);
            if let Some(p) = self.side.packet.get_mut(mb) {
                *p = self.packet;
            }
            let mut h = MbHead::default();
            if !intra_vop && self.br.bit() {
                h.skip = true;
                self.set_mb(mb, MODE_SKIP, [Mv::default(); 4]);
                heads.push(h);
                continue;
            }
            let m = if intra_vop { vlc::mcbpc_intra(&mut self.br) } else { vlc::mcbpc_inter(&mut self.br) }.ok()?;
            if m == vlc::STUFFING {
                continue;
            }
            let (kind, cbpc) = (m >> 2, m & 3);
            h.cbp = cbpc;
            if kind >= 3 {
                h.intra = true;
                h.quant = kind == 4;
                self.set_mb(mb, MODE_INTRA, [Mv::default(); 4]);
                if intra_vop {
                    h.dc_vlc = self.dc_vlc();
                    if h.quant {
                        self.dquant();
                    }
                    h.qp = self.qp as u8;
                    if h.dc_vlc {
                        h.dc = Some(self.read_dcs()?);
                    }
                }
            } else {
                h.quant = kind == 1;
                h.four = kind == 2;
                let v = self.read_p_vectors(mb, x, y, h.four)?;
                self.set_mb(mb, if h.four { MODE_4V } else { MODE_INTER }, v);
            }
            heads.push(h);
        }
        // second partition
        for h in heads.iter_mut().filter(|h| !h.skip) {
            if h.intra {
                h.ac_pred = self.br.bit();
            }
            let cbpy = vlc::cbpy(&mut self.br).ok()?;
            h.cbp |= (if h.intra { cbpy } else { 15 - cbpy }) << 2;
            if !intra_vop {
                h.dc_vlc = self.dc_vlc();
                if h.quant {
                    self.dquant();
                }
                h.qp = self.qp as u8;
                if h.intra && h.dc_vlc {
                    h.dc = Some(self.read_dcs()?);
                }
            }
        }
        Ok(())
    }

    fn read_dcs(&mut self) -> R<[i32; 6]> {
        let mut d = [0; 6];
        for (n, o) in d.iter_mut().enumerate() {
            *o = vlc::intra_dc(&mut self.br, n < 4).ok()?;
        }
        Ok(d)
    }

    /// The third partition: blocks, macroblock by macroblock from `mb`.
    fn reconstruct(&mut self, mb: &mut usize, heads: &[MbHead]) -> R {
        for h in heads {
            let (x, y) = (*mb % self.mb_w, *mb / self.mb_w);
            if h.skip {
                self.predict_forward(x, y, &[Mv::default(); 4], false);
            } else if h.intra {
                self.intra(*mb, x, y, *h)?;
            } else {
                let v = self.side.mvs.get(*mb).copied().unwrap_or_default();
                self.predict_forward(x, y, &v, h.four);
                self.qp = i32::from(h.qp);
                self.residual(x, y, h.cbp)?;
            }
            if self.br.overrun() {
                return Err(Damaged);
            }
            *mb += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_division_and_scalers() {
        assert_eq!([round_div(7, 2), round_div(-7, 2), round_div(8, 4), round_div(1, 3)], [4, -4, 2, 0]);
        assert_eq!([1, 4, 5, 8, 9, 24, 25, 31].map(|q| dc_scaler(q, true)), [8, 8, 10, 16, 17, 32, 34, 46]);
        assert_eq!([1, 4, 5, 8, 24, 25, 31].map(|q| dc_scaler(q, false)), [8, 8, 9, 10, 18, 19, 25]);
    }

    #[test]
    fn medians() {
        assert_eq!(median(1, 2, 3), 2);
        assert_eq!(median(3, 1, 2), 2);
        assert_eq!(median(-5, 9, 0), 0);
    }
}
