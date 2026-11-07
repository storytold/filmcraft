//! Random streams written from the syntax alone: everything ffmpeg's encoder never emits (every
//! table entry, all three escapes, DC in the AC table, macroblock stuffing, header extension
//! codes, direct vectors with deltas) and a seed for the mutation tests that needs no ffmpeg.
//! With ffmpeg, its decode of the same streams is the reference.

use crate::vlc::{self, Event};
use crate::{Decoder, Picture};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        (self.next() >> 8) % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

#[derive(Default)]
struct Bw {
    bytes: Vec<u8>,
    n: usize,
}

impl Bw {
    fn put(&mut self, v: u32, n: u32) {
        for k in (0..n).rev() {
            if self.n.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if let Some(b) = self.bytes.last_mut() {
                *b |= (((v >> k) & 1) as u8) << (7 - self.n % 8);
            }
            self.n += 1;
        }
    }
    fn code(&mut self, s: &str) {
        for c in s.bytes() {
            self.put(u32::from(c - b'0'), 1);
        }
    }
    /// next_start_code: a 0, then 1s to the byte boundary.
    fn stuff(&mut self) {
        self.put(0, 1);
        while !self.n.is_multiple_of(8) {
            self.put(1, 1);
        }
    }
    fn start(&mut self, code: u8) {
        for b in [0, 0, 1, code] {
            self.put(u32::from(b), 8);
        }
    }
    /// The code of `value` in a lookup table on `bits` bits.
    fn table(&mut self, t: &[u16], bits: u32, value: u8) {
        let (i, e) = t.iter().enumerate().find(|(_, e)| **e != 0 && **e as u8 == value).unwrap();
        let len = u32::from(*e >> 8);
        self.put((i >> (bits - len)) as u32, len);
    }
}

#[derive(Clone, Copy, Default)]
struct Cfg {
    w: u32,
    h: u32,
    mpeg_quant: bool,
    qpel: bool,
    b_frames: u32,
    resync: bool,
    dc_thr: u32,
    fcode: u32,
    frames: u32,
    max_q: u64,
    no_four: bool,
}

struct Gen {
    r: Rng,
    c: Cfg,
    q: i32,
    /// Not-coded macroblocks of the last anchor.
    skipped: Vec<bool>,
    /// Table entries used, for the test's claim: intra then inter.
    used: Vec<bool>,
}

fn time_bits() -> u32 {
    5 // resolution 25
}

impl Gen {
    fn vol(&self, w: &mut Bw) {
        let c = &self.c;
        w.start(0x20);
        w.put(0, 1);
        w.put(if c.qpel { 17 } else { 1 }, 8);
        w.put(u32::from(c.qpel), 1); // is_object_layer_identifier
        if c.qpel {
            w.put(2, 4);
            w.put(1, 3);
        }
        w.put(1, 4); // aspect
        w.put(1, 1); // vol_control
        w.put(1, 2);
        w.put(u32::from(c.b_frames == 0), 1);
        w.put(0, 1);
        w.put(0, 2); // rectangular
        w.put(1, 1);
        w.put(25, 16);
        w.put(1, 1);
        w.put(0, 1);
        w.put(1, 1);
        w.put(c.w, 13);
        w.put(1, 1);
        w.put(c.h, 13);
        w.put(1, 1);
        w.put(0, 1); // progressive
        w.put(1, 1); // obmc_disable
        w.put(0, if c.qpel { 2 } else { 1 }); // sprite_enable
        w.put(0, 1); // 8 bit
        w.put(u32::from(c.mpeg_quant), 1);
        if c.mpeg_quant {
            w.put(1, 1); // load intra
            for k in 0..64 {
                w.put(8 + k / 2, 8);
            }
            w.put(1, 1); // load non-intra, terminated early
            for k in 0..20 {
                w.put(16 + k, 8);
            }
            w.put(0, 8);
        }
        if c.qpel {
            w.put(1, 1);
        }
        w.put(1, 1); // complexity estimation disable
        w.put(u32::from(!c.resync), 1);
        w.put(0, 1); // not partitioned
        if c.qpel {
            w.put(0, 1); // newpred
            w.put(0, 1); // reduced resolution
        }
        w.put(0, 1); // scalability
        w.stuff();
    }

    fn mvd(&mut self, w: &mut Bw, fcode: u32) {
        let m = match self.r.below(4) {
            0 => 0,
            1 => self.r.below(3) as u8,
            _ => self.r.below(33) as u8,
        };
        w.table(&vlc::MVD, 12, m);
        if m != 0 {
            w.put(self.r.below(2) as u32, 1);
            if fcode > 1 {
                w.put(self.r.below(1 << (fcode - 1)) as u32, fcode - 1);
            }
        }
    }

    fn event<'a>(&mut self, events: &'a [Event; 102], last: bool, room: usize) -> &'a Event {
        loop {
            let e = &events[self.r.below(102) as usize];
            // coarse quantisers keep to small levels: past the range of the transform, IDCTs differ
            if (e.0 == 1) == last && usize::from(e.1) < room && (self.q <= 8 || e.2 <= 2) {
                return e;
            }
        }
    }

    /// The coefficients of a block from coefficient `i`, up to the last.
    fn block(&mut self, w: &mut Bw, intra: bool, mut i: usize) {
        let (events, t) = if intra { (&vlc::INTRA, &vlc::TCOEF_INTRA) } else { (&vlc::INTER, &vlc::TCOEF_INTER) };
        let n = 1 + self.r.below(5);
        for k in 0..n {
            // a coefficient other than the last leaves room for one more
            let room = 64 - i;
            let last = k + 1 == n || room <= 2;
            let room = if last { room } else { room - 1 };
            let e = *self.event(events, last, room);
            if let Some(k) = events.iter().position(|x| *x == e) {
                self.used[k + if intra { 0 } else { 102 }] = true;
            }
            let kind = if self.q > 4 { 15 } else { self.r.below(16) };
            if kind == 0 {
                // third escape: fixed-length
                w.code("000001111");
                let run = self.r.below((room as u64).min(30)) as u32;
                let level = 1 + self.r.below(40) as i32;
                let level = if self.r.chance(50) { -level } else { level };
                w.put(u32::from(last), 1);
                w.put(run, 6);
                w.put(1, 1);
                w.put((level & 0xFFF) as u32, 12);
                w.put(1, 1);
                i += run as usize + 1;
                if last {
                    break;
                }
                continue;
            }
            let extra = if kind == 1 {
                w.code("00000110"); // level + LMAX
                0
            } else if kind == 2 {
                let add = usize::from(t.rmax[usize::from(e.0)][usize::from(e.2)]) + 1;
                if usize::from(e.1) + add >= room {
                    0
                } else {
                    w.code("000001110"); // run + RMAX + 1
                    add
                }
            } else {
                0
            };
            w.code(e.3);
            w.put(self.r.below(2) as u32, 1);
            i += usize::from(e.1) + extra + 1;
            if last {
                break;
            }
        }
    }

    fn dc(&mut self, w: &mut Bw, luma: bool) {
        // small: the DCs of random blocks stay positive, as those of pictures are
        let size = self.r.below(3) as u8;
        if luma {
            w.table(&vlc::DC_LUMA, 11, size);
        } else {
            w.table(&vlc::DC_CHROMA, 12, size);
        }
        if size > 0 {
            w.put(self.r.below(1 << size) as u32, u32::from(size));
        }
    }

    /// The quantiser before the macroblock's dquant decides.
    fn dc_vlc(&self) -> bool {
        const BELOW: [i32; 8] = [32, 13, 15, 17, 19, 21, 23, 0];
        self.q < BELOW[self.c.dc_thr as usize]
    }

    fn intra_blocks(&mut self, w: &mut Bw, cbp: u8, dc_vlc: bool) {
        for n in 0..6 {
            if dc_vlc {
                self.dc(w, n < 4);
            }
            if cbp & (32 >> n) != 0 {
                if !dc_vlc {
                    // the DC as a coefficient, kept small for the same reason
                    w.code("10");
                    w.put(self.r.below(2) as u32, 1);
                }
                self.block(w, true, 1);
            }
        }
    }

    fn dquant(&mut self, w: &mut Bw) {
        let d = self.r.below(4) as i32;
        w.put(d as u32, 2);
        self.q = (self.q + [-1, -2, 1, 2][d as usize]).clamp(1, self.c.max_q as i32);
    }

    fn mb_i(&mut self, w: &mut Bw) {
        if self.r.chance(3) {
            w.table(&vlc::MCBPC_I, 9, vlc::STUFFING);
        }
        let quant = self.r.chance(20);
        let cbpc = self.r.below(4) as u8;
        w.table(&vlc::MCBPC_I, 9, (if quant { 4 } else { 3 }) << 2 | cbpc);
        w.put(self.r.below(2) as u32, 1);
        let cbpy = self.r.below(16) as u8;
        w.table(&vlc::CBPY, 6, cbpy);
        let dc_vlc = self.dc_vlc();
        if quant {
            self.dquant(w);
        }
        self.intra_blocks(w, cbpy << 2 | cbpc, dc_vlc);
    }

    fn mb_p(&mut self, w: &mut Bw, fcode: u32, mb: usize) {
        self.skipped[mb] = false;
        if self.r.chance(3) {
            w.put(0, 1);
            w.table(&vlc::MCBPC_P, 9, vlc::STUFFING);
        }
        if self.r.chance(25) {
            w.put(1, 1);
            self.skipped[mb] = true;
            return;
        }
        w.put(0, 1);
        let kind = [0, 0, 0, 1, 2, 2, 3, 4][self.r.below(8) as usize];
        let kind = if kind == 2 && self.c.no_four { 0 } else { kind };
        let cbpc = self.r.below(4) as u8;
        w.table(&vlc::MCBPC_P, 9, kind << 2 | cbpc);
        if kind >= 3 {
            w.put(self.r.below(2) as u32, 1);
        }
        let cbpy = self.r.below(16) as u8;
        w.table(&vlc::CBPY, 6, cbpy);
        let dc_vlc = self.dc_vlc();
        if kind == 1 || kind == 4 {
            self.dquant(w);
        }
        if kind >= 3 {
            self.intra_blocks(w, cbpy << 2 | cbpc, dc_vlc);
            return;
        }
        for _ in 0..if kind == 2 { 8 } else { 2 } {
            self.mvd(w, fcode);
        }
        let cbp = (15 - cbpy) << 2 | cbpc;
        for n in 0..6 {
            if cbp & (32 >> n) != 0 {
                self.block(w, false, 0);
            }
        }
    }

    fn mb_b(&mut self, w: &mut Bw, fcode: u32, mb: usize) {
        if self.skipped[mb] {
            return;
        }
        let modb = self.r.below(3);
        if modb == 0 {
            w.put(1, 1);
            return;
        }
        w.put(u32::from(modb == 1), 2); // 01 no cbpb, 00 cbpb
        let kind = 1 + self.r.below(4) as u32;
        w.put(1, kind);
        let cbp = if modb == 2 { self.r.below(64) as u8 } else { 0 };
        if modb == 2 {
            w.put(u32::from(cbp), 6);
        }
        if kind != 1 && cbp != 0 {
            let d = self.r.below(3);
            match d {
                0 => w.put(0, 1),
                1 => w.put(0b10, 2),
                _ => w.put(0b11, 2),
            }
            if d != 0 {
                self.q = (self.q + if d == 1 { -2 } else { 2 }).clamp(1, self.c.max_q as i32);
            }
        }
        match kind {
            1 => {
                self.mvd(w, 1);
                self.mvd(w, 1);
            }
            2 => {
                for _ in 0..4 {
                    self.mvd(w, fcode);
                }
            }
            _ => {
                for _ in 0..2 {
                    self.mvd(w, fcode);
                }
            }
        }
        for n in 0..6 {
            if cbp & (32 >> n) != 0 {
                self.block(w, false, 0);
            }
        }
    }

    /// One VOP at display time `t`; kind 0 I, 1 P, 2 B.
    fn vop(&mut self, kind: u32, t: u32) -> Vec<u8> {
        let mut w = Bw::default();
        if kind == 0 {
            self.vol(&mut w);
        }
        let c = self.c;
        let (mb_w, mb_h) = (c.w.div_ceil(16) as usize, c.h.div_ceil(16) as usize);
        let total = mb_w * mb_h;
        let mb_bits = (total as u32).next_power_of_two().trailing_zeros();
        w.start(0xB6);
        w.put(kind, 2);
        w.put(0, 1); // modulo_time_base
        w.put(1, 1);
        w.put(t, time_bits());
        w.put(1, 1);
        w.put(1, 1); // coded
        if kind == 1 {
            w.put(self.r.below(2) as u32, 1); // rounding
        }
        w.put(c.dc_thr, 3);
        self.q = 1 + self.r.below(c.max_q) as i32;
        w.put(self.q as u32, 5);
        if kind != 0 {
            w.put(c.fcode, 3);
        }
        if kind == 2 {
            w.put(c.fcode, 3);
        }
        let zeros = match kind {
            0 => 16,
            1 => 15 + c.fcode,
            _ => 15 + c.fcode.max(2),
        };
        for mb in 0..total {
            match kind {
                0 => self.mb_i(&mut w),
                1 => self.mb_p(&mut w, c.fcode, mb),
                _ => self.mb_b(&mut w, c.fcode, mb),
            }
            if kind == 0 {
                self.skipped[mb] = false;
            }
            if c.resync && mb + 1 < total && self.r.chance(12) {
                w.stuff();
                w.put(0, zeros);
                w.put(1, 1);
                w.put(mb as u32 + 1, mb_bits);
                self.q = 1 + self.r.below(c.max_q) as i32;
                w.put(self.q as u32, 5);
                let hec = self.r.chance(30);
                w.put(u32::from(hec), 1);
                if hec {
                    w.put(0, 1);
                    w.put(1, 1);
                    w.put(t, time_bits());
                    w.put(1, 1);
                    w.put(kind, 2);
                    w.put(c.dc_thr, 3);
                    if kind != 0 {
                        w.put(c.fcode, 3);
                    }
                    if kind == 2 {
                        w.put(c.fcode, 3);
                    }
                }
            }
        }
        w.stuff();
        w.bytes
    }

    fn stream(&mut self) -> Vec<Vec<u8>> {
        let c = self.c;
        let mut out = Vec::new();
        let (mut t, mut left) = (0, c.frames);
        let mut kind = 0;
        while left > 0 {
            let bs = c.b_frames.min(left.saturating_sub(1));
            let anchor = t + if kind == 0 { 0 } else { bs };
            out.push(self.vop(kind, anchor));
            left -= 1;
            if kind != 0 {
                for k in 0..bs {
                    out.push(self.vop(2, t + k));
                    left -= 1;
                }
                t += bs;
            }
            t += 1;
            kind = 1;
        }
        out
    }
}

fn generator(c: Cfg, seed: u64) -> Gen {
    let total = (c.w.div_ceil(16) * c.h.div_ceil(16)) as usize;
    Gen { r: Rng(seed | 1), c, q: 4, skipped: vec![false; total], used: vec![false; 204] }
}

fn generate(c: Cfg, seed: u64) -> Vec<Vec<u8>> {
    generator(c, seed).stream()
}

fn decode(chunks: &[Vec<u8>]) -> Vec<Picture> {
    let mut d = Decoder::new();
    let mut out = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        out.extend(d.decode(c, i as i64).unwrap());
    }
    out.extend(d.flush());
    out
}

fn configs() -> Vec<(&'static str, Cfg)> {
    let base = Cfg { w: 96, h: 80, frames: 12, fcode: 1, max_q: 8, ..Cfg::default() };
    vec![
        ("plain", base),
        ("odd size", Cfg { w: 71, h: 53, no_four: true, ..base }),
        ("wide vectors", Cfg { fcode: 4, ..base }),
        ("dc in the ac table", Cfg { dc_thr: 7, ..base }),
        ("dc threshold", Cfg { dc_thr: 1, max_q: 26, ..base }),
        ("mpeg quant", Cfg { mpeg_quant: true, ..base }),
        ("packets", Cfg { resync: true, fcode: 2, ..base }),
        ("b1", Cfg { b_frames: 1, fcode: 2, ..base }),
        ("b3", Cfg { b_frames: 3, fcode: 3, frames: 17, ..base }),
        ("b2 packets", Cfg { b_frames: 2, resync: true, fcode: 1, ..base }),
        ("qpel", Cfg { qpel: true, fcode: 2, ..base }),
        ("qpel b2", Cfg { qpel: true, b_frames: 2, fcode: 2, ..base }),
    ]
}

/// Streams of random syntax decode as ffmpeg decodes them: every table entry, escape and
/// macroblock kind is used. Random coefficients are not images: they are compared by largest
/// difference, ±3 being the two IDCTs on noise and the drift P-VOPs make of it.
#[test]
fn random_streams_match_ffmpeg() {
    let Some(ff) = filmcraft_testkit::ffmpeg_or_skip("mpeg4v synth") else { return };
    let dir = filmcraft_testkit::fixtures_dir("mpeg4v");
    let mut used = vec![false; 204];
    for (name, c) in configs() {
        for seed in [0x9E37_79B9_7F4A_7C15u64, 0xD1B5_4A32_D192_ED03, 0x8CB9_2BA7_2F3D_8DD7, 0xFF51_AFD7_ED55_8CCD, 0xC4CE_B9FE_1A85_EC53] {
            let mut g = generator(c, seed);
            let chunks = g.stream();
            used.iter_mut().zip(&g.used).for_each(|(a, b)| *a |= *b);
            let pics = decode(&chunks);
            assert_eq!(pics.len(), c.frames as usize, "{name}");
            let path = dir.join(format!("synth-{}-{seed:x}.m4v", name.replace(' ', "_")));
            std::fs::write(&path, chunks.concat()).unwrap();
            let o = std::process::Command::new(&ff)
                .args(["-hide_banner", "-loglevel", "error", "-f", "m4v", "-i"])
                .arg(&path)
                .args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
                .output()
                .unwrap();
            assert!(o.stderr.is_empty(), "{name}: ffmpeg says {}", String::from_utf8_lossy(&o.stderr));
            let size = (c.w * c.h) as usize + 2 * (c.w.div_ceil(2) * c.h.div_ceil(2)) as usize;
            assert_eq!(o.stdout.len(), size * c.frames as usize, "{name}: frames");
            for (k, (p, r)) in pics.iter().zip(o.stdout.chunks_exact(size)).enumerate() {
                let ours: Vec<u8> = [&p.y[..], &p.cb, &p.cr].concat();
                let worst = ours.iter().zip(r).map(|(a, b)| a.abs_diff(*b)).max().unwrap();
                let pos = ours.iter().zip(r).position(|(a, b)| a.abs_diff(*b) == worst).unwrap();
                assert!(worst <= 4, "{name} {seed:x}: frame {k} {:?} differs by {worst} at sample {pos}", p.vop_type);
            }
            let _ = std::fs::remove_file(&path);
        }
    }
    assert!(used.iter().all(|u| *u), "table entries never used: {:?}", used.iter().enumerate().filter(|u| !u.1).map(|u| u.0).collect::<Vec<_>>());
}

/// Damage never panics, whatever it hits: truncation, bit flips, random bytes, start codes
/// inserted, and it is reported or concealed.
#[test]
fn damaged_streams_never_panic() {
    let rounds = if cfg!(debug_assertions) { 300 } else { 1500 };
    let mut r = Rng(0xA076_1D64_78BD_642F);
    let (mut reported, mut errors) = (0u64, 0u64);
    for (name, c) in configs() {
        let chunks = generate(Cfg { frames: 6, ..c }, 0x2545_F491_4F6C_DD1D);
        for round in 0..rounds {
            let mut d = Decoder::new();
            for (i, chunk) in chunks.iter().enumerate() {
                let mut data = chunk.clone();
                match r.below(6) {
                    0 => data.truncate(r.below(data.len() as u64 + 1) as usize),
                    1 | 2 => {
                        for _ in 0..1 + r.below(8) {
                            let at = r.below(data.len() as u64) as usize;
                            data[at] ^= 1 << r.below(8);
                        }
                    }
                    3 => {
                        for _ in 0..1 + r.below(4) {
                            let at = r.below(data.len() as u64) as usize;
                            data[at] = r.below(256) as u8;
                        }
                    }
                    4 => {
                        let at = r.below(data.len() as u64) as usize;
                        data.splice(at..at, [0, 0, 1, [0xB6, 0x20, 0xB3, 0xB0, 0xB5][r.below(5) as usize]]);
                    }
                    _ => {}
                }
                // an error is an answer; the point is that it returns
                let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.decode(&data, i as i64).map(|p| p.len())));
                assert!(got.is_ok(), "{name}, round {round}, chunk {i}: panicked");
                errors += u64::from(matches!(got, Ok(Err(_))));
            }
            let _ = d.flush();
            d.reset();
            reported += d.errors();
        }
    }
    assert!(reported > 0 && errors > 0, "damage goes unnoticed: {reported} concealed, {errors} errors");
}

/// Damage to a macroblock is concealed, counted, and leaves the picture count alone.
#[test]
fn a_damaged_vop_still_shows_a_picture() {
    let c = Cfg { w: 96, h: 80, frames: 4, fcode: 1, max_q: 8, ..Cfg::default() };
    let mut chunks = generate(c, 0x1357_9BDF_0246_8ACE);
    let clean = decode(&chunks);
    let n = chunks[2].len();
    chunks[2].truncate(n / 2);
    let mut d = Decoder::new();
    let mut pics = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        pics.extend(d.decode(c, i as i64).unwrap());
    }
    pics.extend(d.flush());
    assert_eq!(pics.len(), clean.len());
    assert_eq!(d.errors(), 1);
    assert!(d.last_error().is_some_and(|e| e.contains("P-VOP")), "{:?}", d.last_error());
    assert!(pics[1].y == clean[1].y, "the VOP before is untouched");
}

#[test]
fn garbage_is_an_error_and_a_vop_needs_a_layer() {
    let mut d = Decoder::new();
    assert!(d.decode(&[0x47, 0x11, 0x22, 0x33, 0x44], 0).is_err());
    assert!(d.decode(&[0, 0, 1, 0xB6, 0x10, 0x20, 0x30], 0).is_err());
    assert!(d.decode(&[], 0).unwrap().is_empty());
    assert!(d.flush().is_empty());
}

/// Features the decoder lacks name themselves in the error.
#[test]
fn unsupported_layers_say_what_is_missing() {
    let c = Cfg { w: 48, h: 48, frames: 2, fcode: 1, max_q: 8, ..Cfg::default() };
    let g = generator(c, 5);
    let mut w = Bw::default();
    g.vol(&mut w);
    // the interlaced bit: 1 + 8 + 1 + 4 + 1 + 2 + 1 + 1 + 2 + 1 + 16 + 1 + 1 + 1 + 13 + 1 + 13 + 1 bits in
    let bit = 32 + 69;
    w.bytes[bit / 8] |= 1 << (7 - bit % 8);
    let mut d = Decoder::new();
    let mut s = w.bytes;
    s.extend([0, 0, 1, 0xB6, 0x10, 0x20, 0x30, 0x40]);
    let e = d.decode(&s, 0).unwrap_err().to_string();
    assert!(e.contains("interlaced"), "{e}");
    assert_eq!(crate::probe(&s).and_then(|i| i.unsupported).as_deref(), Some("interlaced coding"));
}
