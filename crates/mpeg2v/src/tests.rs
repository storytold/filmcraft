//! Unit tests over hand-built streams (headers, robustness). Decoding accuracy against ffmpeg is
//! in `tests/it/oracle.rs`.

use super::*;

/// A minimal bit writer for building streams.
pub(crate) struct W {
    pub(crate) out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl W {
    pub(crate) fn new() -> W {
        W { out: Vec::new(), acc: 0, n: 0 }
    }
    pub(crate) fn put(&mut self, v: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1) as u64;
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    pub(crate) fn code(&mut self, s: &str) {
        for c in s.chars().filter(|c| *c != ' ') {
            self.put((c == '1') as u32, 1);
        }
    }
    pub(crate) fn align(&mut self) {
        while self.n != 0 {
            self.put(0, 1);
        }
    }
    pub(crate) fn start(&mut self, code: u8) {
        self.align();
        self.out.extend_from_slice(&[0, 0, 1, code]);
    }
}

/// An MPEG-2 4:2:0 progressive 32×16 I picture whose macroblocks all have DC `dc` (luma) and
/// no AC: two macroblocks, one slice.
fn intra_stream(dc_luma: i32) -> Vec<u8> {
    let mut w = W::new();
    w.start(0xB3);
    w.put(32, 12);
    w.put(16, 12);
    w.put(1, 4); // square samples
    w.put(3, 4); // 25 fps
    w.put(1000, 18);
    w.put(1, 1);
    w.put(10, 10);
    w.put(0, 1);
    w.put(0, 1);
    w.put(0, 1);
    w.start(0xB5);
    w.put(1, 4);
    w.put(0x48, 8); // Main@Main
    w.put(1, 1); // progressive
    w.put(1, 2); // 4:2:0
    w.put(0, 2);
    w.put(0, 2);
    w.put(0, 12);
    w.put(1, 1);
    w.put(0, 8);
    w.put(0, 1);
    w.put(0, 2);
    w.put(0, 5);
    w.start(0x00);
    w.put(0, 10);
    w.put(1, 3); // I
    w.put(0xFFFF, 16);
    w.put(0, 1); // extra_bit_picture
    w.start(0xB5);
    w.put(8, 4);
    for _ in 0..4 {
        w.put(15, 4);
    }
    w.put(0, 2); // 8-bit DC
    w.put(3, 2); // frame picture
    w.put(0, 1);
    w.put(1, 1); // frame_pred_frame_dct
    w.put(0, 1);
    w.put(0, 1);
    w.put(0, 1);
    w.put(0, 1);
    w.put(0, 1);
    w.put(1, 1);
    w.put(1, 1); // progressive_frame
    w.put(0, 1);
    w.start(0x01);
    w.put(8, 5); // quantiser_scale_code
    w.put(0, 1); // extra_bit_slice
    for mb in 0..2 {
        w.code("1"); // address increment 1
        w.code("1"); // macroblock_type: intra
        for b in 0..6 {
            // DC differential: the first luma block of the slice carries dc - 128
            let diff = if b == 0 && mb == 0 { dc_luma - 128 } else { 0 };
            if b < 4 {
                if diff == 0 {
                    w.code("100");
                } else {
                    // size 6 covers |diff| < 64
                    w.code("1111 0");
                    let v = if diff > 0 { diff as u32 } else { (diff + 63) as u32 };
                    w.put(v, 6);
                }
            } else {
                w.code("00");
            }
            w.code("10"); // EOB (table zero)
        }
    }
    w.start(0xB7);
    w.out
}

#[test]
fn decodes_a_hand_built_intra_picture() {
    let s = intra_stream(160);
    let info = probe(&s).unwrap();
    assert_eq!((info.width, info.height, info.mpeg2, info.frame_rate), (32, 16, true, Some((25, 1))));
    assert_eq!(info.codec_name(), "MPEG-2 Video (Main@Main)");
    let mut d = Decoder::new();
    let mut pics = d.decode(&s, 7).unwrap();
    pics.extend(d.flush());
    assert_eq!(pics.len(), 1);
    let p = &pics[0];
    assert_eq!(d.errors(), 0);
    assert_eq!((p.width, p.height, p.pts, p.picture_type), (32, 16, 7, PictureType::I));
    // DC 160 everywhere (the mismatch-control toggle of F[7][7] is far below rounding)
    assert!(p.y.iter().all(|&v| v == 160), "{:?}", &p.y[..8]);
    assert!(p.cb.iter().chain(&p.cr).all(|&v| v == 128));
    assert_eq!(p.field_order(), None);
}

#[test]
fn truncation_and_mutation_never_panic() {
    let s = intra_stream(100);
    for cut in 0..s.len() {
        let mut d = Decoder::new();
        let _ = d.decode(&s[..cut], 0);
        let _ = d.flush();
    }
    let mut x = 0x1234_5678u32;
    for _ in 0..3000 {
        let mut m = s.clone();
        for _ in 0..3 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let i = x as usize % m.len();
            m[i] ^= (x >> 8) as u8;
        }
        let mut d = Decoder::new();
        let _ = d.decode(&m, 0);
        let _ = d.flush();
    }
}

#[test]
fn access_unit_scan() {
    let s = intra_stream(128);
    let au = scan_access_unit(&s);
    assert!(au.sequence_header && au.is_intra() && !au.is_disposable());
    assert_eq!(au.pictures[0].1.unwrap().picture_structure, 3);
}
