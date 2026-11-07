//! Variable-length codes of ISO/IEC 14496-2 Annex B, as direct-lookup tables built at compile
//! time from the tables as printed (code strings without the sign bit).

use crate::bits::Bits;

/// A code of a lookup table: `(code, value)`.
type Code = (&'static str, u8);

/// A direct-lookup table on `N` = 2^bits entries: `len << 8 | value`, 0 for an invalid code.
const fn table<const N: usize>(codes: &[Code], bits: u32) -> [u16; N] {
    let mut t = [0u16; N];
    let mut i = 0;
    while i < codes.len() {
        let (code, value) = codes[i];
        let c = code.as_bytes();
        let len = c.len() as u32;
        let mut v = 0usize;
        let mut k = 0;
        while k < c.len() {
            v = v << 1 | (c[k] == b'1') as usize;
            k += 1;
        }
        let shift = bits - len;
        let mut e = 0;
        while e < 1usize << shift {
            t[(v << shift) | e] = (len as u16) << 8 | value as u16;
            e += 1;
        }
        i += 1;
    }
    t
}

/// Table B-6: `mb_type << 2 | cbpc`, [`STUFFING`] for macroblock stuffing.
pub(crate) const STUFFING: u8 = 0xFF;
pub(crate) static MCBPC_I: [u16; 512] = table(
    &[
        ("1", 3 << 2),
        ("001", 3 << 2 | 1),
        ("010", 3 << 2 | 2),
        ("011", 3 << 2 | 3),
        ("0001", 4 << 2),
        ("000001", 4 << 2 | 1),
        ("000010", 4 << 2 | 2),
        ("000011", 4 << 2 | 3),
        ("000000001", STUFFING),
    ],
    9,
);

/// Table B-7: `mb_type << 2 | cbpc`.
pub(crate) static MCBPC_P: [u16; 512] = table(
    &[
        ("1", 0),
        ("0011", 1),
        ("0010", 2),
        ("000101", 3),
        ("011", 1 << 2),
        ("0000111", 1 << 2 | 1),
        ("0000110", 1 << 2 | 2),
        ("000000101", 1 << 2 | 3),
        ("010", 2 << 2),
        ("0000101", 2 << 2 | 1),
        ("0000100", 2 << 2 | 2),
        ("00000101", 2 << 2 | 3),
        ("00011", 3 << 2),
        ("00000100", 3 << 2 | 1),
        ("00000011", 3 << 2 | 2),
        ("0000011", 3 << 2 | 3),
        ("000100", 4 << 2),
        ("000000100", 4 << 2 | 1),
        ("000000011", 4 << 2 | 2),
        ("000000010", 4 << 2 | 3),
        ("000000001", STUFFING),
    ],
    9,
);

/// Table B-8: CBPY for intra macroblocks (inter: 15 minus this).
pub(crate) static CBPY: [u16; 64] = table(
    &[
        ("0011", 0),
        ("00101", 1),
        ("00100", 2),
        ("1001", 3),
        ("00011", 4),
        ("0111", 5),
        ("000010", 6),
        ("1011", 7),
        ("00010", 8),
        ("000011", 9),
        ("0101", 10),
        ("1010", 11),
        ("0100", 12),
        ("1000", 13),
        ("0110", 14),
        ("11", 15),
    ],
    6,
);

/// Table B-12: magnitude of the motion vector data (a sign bit follows all but 0).
pub(crate) static MVD: [u16; 4096] = table(
    &[
        ("1", 0),
        ("01", 1),
        ("001", 2),
        ("0001", 3),
        ("000011", 4),
        ("0000101", 5),
        ("0000100", 6),
        ("0000011", 7),
        ("000001011", 8),
        ("000001010", 9),
        ("000001001", 10),
        ("0000010001", 11),
        ("0000010000", 12),
        ("0000001111", 13),
        ("0000001110", 14),
        ("0000001101", 15),
        ("0000001100", 16),
        ("0000001011", 17),
        ("0000001010", 18),
        ("0000001001", 19),
        ("0000001000", 20),
        ("0000000111", 21),
        ("0000000110", 22),
        ("0000000101", 23),
        ("0000000100", 24),
        ("00000000111", 25),
        ("00000000110", 26),
        ("00000000101", 27),
        ("00000000100", 28),
        ("00000000011", 29),
        ("00000000010", 30),
        ("000000000011", 31),
        ("000000000010", 32),
    ],
    12,
);

/// Table B-13: dct_dc_size_luminance.
pub(crate) static DC_LUMA: [u16; 2048] = table(
    &[
        ("011", 0),
        ("11", 1),
        ("10", 2),
        ("010", 3),
        ("001", 4),
        ("0001", 5),
        ("00001", 6),
        ("000001", 7),
        ("0000001", 8),
        ("00000001", 9),
        ("000000001", 10),
        ("0000000001", 11),
        ("00000000001", 12),
    ],
    11,
);

/// Table B-14: dct_dc_size_chrominance.
pub(crate) static DC_CHROMA: [u16; 4096] = table(
    &[
        ("11", 0),
        ("10", 1),
        ("01", 2),
        ("001", 3),
        ("0001", 4),
        ("00001", 5),
        ("000001", 6),
        ("0000001", 7),
        ("00000001", 8),
        ("000000001", 9),
        ("0000000001", 10),
        ("00000000001", 11),
        ("000000000001", 12),
    ],
    12,
);

/// `(last, run, level, code)`.
pub(crate) type Event = (u8, u8, u8, &'static str);

const ESCAPE: &str = "0000011";

/// Table B-16: intra AC coefficients.
pub(crate) const INTRA: [Event; 102] = [
    (0, 0, 1, "10"),
    (0, 0, 2, "110"),
    (0, 0, 3, "1111"),
    (0, 0, 4, "01101"),
    (0, 0, 5, "01100"),
    (0, 0, 6, "010101"),
    (0, 0, 7, "010011"),
    (0, 0, 8, "010010"),
    (0, 0, 9, "0010111"),
    (0, 0, 10, "00011111"),
    (0, 0, 11, "00011110"),
    (0, 0, 12, "00011101"),
    (0, 0, 13, "000100101"),
    (0, 0, 14, "000100100"),
    (0, 0, 15, "000100011"),
    (0, 0, 16, "000100001"),
    (0, 0, 17, "0000100001"),
    (0, 0, 18, "0000100000"),
    (0, 0, 19, "0000001111"),
    (0, 0, 20, "0000001110"),
    (0, 0, 21, "00000000111"),
    (0, 0, 22, "00000000110"),
    (0, 0, 23, "00000100000"),
    (0, 0, 24, "00000100001"),
    (0, 0, 25, "000001010000"),
    (0, 0, 26, "000001010001"),
    (0, 0, 27, "000001010010"),
    (0, 1, 1, "1110"),
    (0, 1, 2, "010100"),
    (0, 1, 3, "0010110"),
    (0, 1, 4, "00011100"),
    (0, 1, 5, "000100000"),
    (0, 1, 6, "000011111"),
    (0, 1, 7, "0000001101"),
    (0, 1, 8, "00000100010"),
    (0, 1, 9, "000001010011"),
    (0, 1, 10, "000001010101"),
    (0, 2, 1, "01011"),
    (0, 2, 2, "0010101"),
    (0, 2, 3, "000011110"),
    (0, 2, 4, "0000001100"),
    (0, 2, 5, "000001010110"),
    (0, 3, 1, "010001"),
    (0, 3, 2, "00011011"),
    (0, 3, 3, "000011101"),
    (0, 3, 4, "0000001011"),
    (0, 4, 1, "010000"),
    (0, 4, 2, "000100010"),
    (0, 4, 3, "0000001010"),
    (0, 5, 1, "001101"),
    (0, 5, 2, "000011100"),
    (0, 5, 3, "0000001000"),
    (0, 6, 1, "0010010"),
    (0, 6, 2, "000011011"),
    (0, 6, 3, "000001010100"),
    (0, 7, 1, "0010100"),
    (0, 7, 2, "000011010"),
    (0, 7, 3, "000001010111"),
    (0, 8, 1, "00011001"),
    (0, 8, 2, "0000001001"),
    (0, 9, 1, "00011000"),
    (0, 9, 2, "00000100011"),
    (0, 10, 1, "00010111"),
    (0, 11, 1, "000011001"),
    (0, 12, 1, "000011000"),
    (0, 13, 1, "0000000111"),
    (0, 14, 1, "000001011000"),
    (1, 0, 1, "0111"),
    (1, 0, 2, "001100"),
    (1, 0, 3, "00010110"),
    (1, 0, 4, "000010111"),
    (1, 0, 5, "0000000110"),
    (1, 0, 6, "00000000101"),
    (1, 0, 7, "00000000100"),
    (1, 0, 8, "000001011001"),
    (1, 1, 1, "001111"),
    (1, 1, 2, "000010110"),
    (1, 1, 3, "0000000101"),
    (1, 2, 1, "001110"),
    (1, 2, 2, "0000000100"),
    (1, 3, 1, "0010001"),
    (1, 3, 2, "00000100100"),
    (1, 4, 1, "0010000"),
    (1, 4, 2, "00000100101"),
    (1, 5, 1, "0010011"),
    (1, 5, 2, "000001011010"),
    (1, 6, 1, "00010101"),
    (1, 6, 2, "000001011011"),
    (1, 7, 1, "00010100"),
    (1, 8, 1, "00010011"),
    (1, 9, 1, "00011010"),
    (1, 10, 1, "000010101"),
    (1, 11, 1, "000010100"),
    (1, 12, 1, "000010011"),
    (1, 13, 1, "000010010"),
    (1, 14, 1, "000010001"),
    (1, 15, 1, "00000100110"),
    (1, 16, 1, "00000100111"),
    (1, 17, 1, "000001011100"),
    (1, 18, 1, "000001011101"),
    (1, 19, 1, "000001011110"),
    (1, 20, 1, "000001011111"),
];

/// Table B-17: inter AC coefficients.
pub(crate) const INTER: [Event; 102] = [
    (0, 0, 1, "10"),
    (0, 0, 2, "1111"),
    (0, 0, 3, "010101"),
    (0, 0, 4, "0010111"),
    (0, 0, 5, "00011111"),
    (0, 0, 6, "000100101"),
    (0, 0, 7, "000100100"),
    (0, 0, 8, "0000100001"),
    (0, 0, 9, "0000100000"),
    (0, 0, 10, "00000000111"),
    (0, 0, 11, "00000000110"),
    (0, 0, 12, "00000100000"),
    (0, 1, 1, "110"),
    (0, 1, 2, "010100"),
    (0, 1, 3, "00011110"),
    (0, 1, 4, "0000001111"),
    (0, 1, 5, "00000100001"),
    (0, 1, 6, "000001010000"),
    (0, 2, 1, "1110"),
    (0, 2, 2, "00011101"),
    (0, 2, 3, "0000001110"),
    (0, 2, 4, "000001010001"),
    (0, 3, 1, "01101"),
    (0, 3, 2, "000100011"),
    (0, 3, 3, "0000001101"),
    (0, 4, 1, "01100"),
    (0, 4, 2, "000100010"),
    (0, 4, 3, "000001010010"),
    (0, 5, 1, "01011"),
    (0, 5, 2, "0000001100"),
    (0, 5, 3, "000001010011"),
    (0, 6, 1, "010011"),
    (0, 6, 2, "0000001011"),
    (0, 6, 3, "000001010100"),
    (0, 7, 1, "010010"),
    (0, 7, 2, "0000001010"),
    (0, 8, 1, "010001"),
    (0, 8, 2, "0000001001"),
    (0, 9, 1, "010000"),
    (0, 9, 2, "0000001000"),
    (0, 10, 1, "0010110"),
    (0, 10, 2, "000001010101"),
    (0, 11, 1, "0010101"),
    (0, 12, 1, "0010100"),
    (0, 13, 1, "00011100"),
    (0, 14, 1, "00011011"),
    (0, 15, 1, "000100001"),
    (0, 16, 1, "000100000"),
    (0, 17, 1, "000011111"),
    (0, 18, 1, "000011110"),
    (0, 19, 1, "000011101"),
    (0, 20, 1, "000011100"),
    (0, 21, 1, "000011011"),
    (0, 22, 1, "000011010"),
    (0, 23, 1, "00000100010"),
    (0, 24, 1, "00000100011"),
    (0, 25, 1, "000001010110"),
    (0, 26, 1, "000001010111"),
    (1, 0, 1, "0111"),
    (1, 0, 2, "000011001"),
    (1, 0, 3, "00000000101"),
    (1, 1, 1, "001111"),
    (1, 1, 2, "00000000100"),
    (1, 2, 1, "001110"),
    (1, 3, 1, "001101"),
    (1, 4, 1, "001100"),
    (1, 5, 1, "0010011"),
    (1, 6, 1, "0010010"),
    (1, 7, 1, "0010001"),
    (1, 8, 1, "0010000"),
    (1, 9, 1, "00011010"),
    (1, 10, 1, "00011001"),
    (1, 11, 1, "00011000"),
    (1, 12, 1, "00010111"),
    (1, 13, 1, "00010110"),
    (1, 14, 1, "00010101"),
    (1, 15, 1, "00010100"),
    (1, 16, 1, "00010011"),
    (1, 17, 1, "000011000"),
    (1, 18, 1, "000010111"),
    (1, 19, 1, "000010110"),
    (1, 20, 1, "000010101"),
    (1, 21, 1, "000010100"),
    (1, 22, 1, "000010011"),
    (1, 23, 1, "000010010"),
    (1, 24, 1, "000010001"),
    (1, 25, 1, "0000000111"),
    (1, 26, 1, "0000000110"),
    (1, 27, 1, "0000000101"),
    (1, 28, 1, "0000000100"),
    (1, 29, 1, "00000100100"),
    (1, 30, 1, "00000100101"),
    (1, 31, 1, "00000100110"),
    (1, 32, 1, "00000100111"),
    (1, 33, 1, "000001011000"),
    (1, 34, 1, "000001011001"),
    (1, 35, 1, "000001011010"),
    (1, 36, 1, "000001011011"),
    (1, 37, 1, "000001011100"),
    (1, 38, 1, "000001011101"),
    (1, 39, 1, "000001011110"),
    (1, 40, 1, "000001011111"),
];

/// A coefficient table: a lookup on 12 bits (`len << 24 | last << 16 | run << 8 | level`; the
/// escape code has run [`ESC`]; 0: invalid) and the limits the first two escapes add to.
pub(crate) struct Tcoef {
    vlc: [u32; 4096],
    /// Table B-18/20: largest level for `[last][run]`.
    pub(crate) lmax: [[u8; 64]; 2],
    /// Table B-19/21: largest run for `[last][level]`.
    pub(crate) rmax: [[u8; 32]; 2],
}

const ESC: u32 = 0xFF;

const fn tcoef(events: &[Event; 102]) -> Tcoef {
    let mut t = Tcoef { vlc: [0; 4096], lmax: [[0; 64]; 2], rmax: [[0; 32]; 2] };
    let mut i = 0;
    while i <= events.len() {
        let (last, run, level, code) = if i < events.len() { events[i] } else { (0, 0xFF, 0, ESCAPE) };
        let c = code.as_bytes();
        let mut v = 0usize;
        let mut k = 0;
        while k < c.len() {
            v = v << 1 | (c[k] == b'1') as usize;
            k += 1;
        }
        let shift = 12 - c.len();
        let entry = (c.len() as u32) << 24 | (last as u32) << 16 | (run as u32) << 8 | level as u32;
        let mut e = 0;
        while e < 1usize << shift {
            t.vlc[(v << shift) | e] = entry;
            e += 1;
        }
        if i < events.len() {
            let (l, r, m) = (last as usize, run as usize, level as usize);
            if level > t.lmax[l][r] {
                t.lmax[l][r] = level;
            }
            if run > t.rmax[l][m] {
                t.rmax[l][m] = run;
            }
        }
        i += 1;
    }
    t
}

pub(crate) static TCOEF_INTRA: Tcoef = tcoef(&INTRA);
pub(crate) static TCOEF_INTER: Tcoef = tcoef(&INTER);

#[inline(always)]
fn lookup<const N: usize>(t: &[u16; N], index: usize, br: &mut Bits) -> Option<u8> {
    let e = *t.get(index)?;
    if e == 0 {
        return None;
    }
    br.skip(u32::from(e >> 8));
    Some(e as u8)
}

#[inline]
pub(crate) fn mcbpc_intra(br: &mut Bits) -> Option<u8> {
    let i = br.peek(9) as usize;
    lookup(&MCBPC_I, i, br)
}

#[inline]
pub(crate) fn mcbpc_inter(br: &mut Bits) -> Option<u8> {
    let i = br.peek(9) as usize;
    lookup(&MCBPC_P, i, br)
}

#[inline]
pub(crate) fn cbpy(br: &mut Bits) -> Option<u8> {
    let i = br.peek(6) as usize;
    lookup(&CBPY, i, br)
}

/// One motion vector component: the signed motion code and its residual, as a difference.
#[inline]
pub(crate) fn mvd(br: &mut Bits, fcode: u32) -> Option<i32> {
    let i = br.peek(12) as usize;
    let m = i32::from(lookup(&MVD, i, br)?);
    if m == 0 {
        return Some(0);
    }
    let neg = br.bit();
    let r_size = fcode.saturating_sub(1);
    let v = if r_size == 0 { m } else { ((m - 1) << r_size) + br.read(r_size) as i32 + 1 };
    Some(if neg { -v } else { v })
}

/// The differential intra DC of one block: dct_dc_size and its value.
#[inline]
pub(crate) fn intra_dc(br: &mut Bits, luma: bool) -> Option<i32> {
    let size = if luma {
        let i = br.peek(11) as usize;
        lookup(&DC_LUMA, i, br)?
    } else {
        let i = br.peek(12) as usize;
        lookup(&DC_CHROMA, i, br)?
    };
    if size == 0 {
        return Some(0);
    }
    let n = u32::from(size);
    let v = br.read(n) as i32;
    let d = if v >> (n - 1) == 0 { v - (1 << n) + 1 } else { v };
    if n > 8 {
        br.skip(1); // marker
    }
    Some(d)
}

/// One decoded (last, run, signed level) event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Coef {
    pub last: bool,
    pub run: usize,
    pub level: i32,
}

/// A table code with its sign. The shared entry is well-formed unless the code is invalid.
#[inline(always)]
fn event(t: &Tcoef, br: &mut Bits) -> Option<(usize, usize, i32)> {
    let w = br.peek32();
    let e = *t.vlc.get((w >> 20) as usize)?;
    if e == 0 || (e >> 8) & 0xFF == ESC {
        return None;
    }
    let len = e >> 24;
    br.skip(len + 1);
    let neg = (w << len) >> 31 == 1;
    let (last, run, level) = (((e >> 16) & 1) as usize, ((e >> 8) & 0xFF) as usize, (e & 0xFF) as i32);
    Some((last, run, if neg { -level } else { level }))
}

/// One coefficient event (§7.4.1.3): a table code, or an escape of one of three kinds.
#[inline]
pub(crate) fn coef(t: &Tcoef, br: &mut Bits) -> Option<Coef> {
    let w = br.peek32();
    let e = *t.vlc.get((w >> 20) as usize)?;
    if e == 0 {
        return None;
    }
    if (e >> 8) & 0xFF != ESC {
        let len = e >> 24;
        br.skip(len + 1);
        let neg = (w << len) >> 31 == 1;
        let level = (e & 0xFF) as i32;
        return Some(Coef { last: (e >> 16) & 1 == 1, run: ((e >> 8) & 0xFF) as usize, level: if neg { -level } else { level } });
    }
    // after the 7-bit escape: 0 / 10 / 11
    if (w >> 24) & 1 == 0 {
        br.skip(8);
        let (last, run, level) = event(t, br)?;
        let add = i32::from(*t.lmax.get(last)?.get(run)?);
        Some(Coef { last: last == 1, run, level: if level < 0 { level - add } else { level + add } })
    } else if (w >> 23) & 1 == 0 {
        br.skip(9);
        let (last, run, level) = event(t, br)?;
        let add = usize::from(*t.rmax.get(last)?.get(level.unsigned_abs() as usize)?) + 1;
        Some(Coef { last: last == 1, run: run + add, level })
    } else {
        br.skip(9);
        let last = br.bit();
        let run = br.read(6) as usize;
        br.skip(1); // marker
        let v = br.read(12) as i32;
        br.skip(1); // marker
        Some(Coef { last, run, level: (v << 20) >> 20 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prefix-free and, with the unused all-zero prefix, a complete code.
    fn kraft(codes: &[&str], unused: f64) {
        let sum: f64 = codes.iter().map(|c| 2f64.powi(-(c.len() as i32))).sum();
        assert!((sum + unused - 1.0).abs() < 1e-9, "Kraft sum {sum}");
        for a in codes {
            for b in codes {
                assert!(a == b || !b.starts_with(a), "{a} is a prefix of {b}");
            }
        }
    }

    #[test]
    fn coefficient_tables_are_complete_prefix_codes() {
        for t in [&INTRA, &INTER] {
            let mut c: Vec<&str> = t.iter().map(|e| e.3).collect();
            c.push(ESCAPE);
            kraft(&c, 1.0 / 512.0);
        }
    }

    /// The tables' limits as printed in Tables B-18 to B-21.
    #[test]
    fn level_and_run_limits() {
        let l0 = [27, 10, 5, 4, 3, 3, 3, 3, 2, 2, 1, 1, 1, 1, 1, 0];
        assert_eq!(TCOEF_INTRA.lmax[0][..16], l0);
        assert_eq!(TCOEF_INTRA.lmax[1][..8], [8, 3, 2, 2, 2, 2, 2, 1]);
        assert_eq!(TCOEF_INTRA.lmax[1][20..22], [1, 0]);
        assert_eq!(TCOEF_INTER.lmax[0][..12], [12, 6, 4, 3, 3, 3, 3, 2, 2, 2, 2, 1]);
        assert_eq!(TCOEF_INTER.lmax[0][26..28], [1, 0]);
        assert_eq!(TCOEF_INTER.lmax[1][..3], [3, 2, 1]);
        assert_eq!(TCOEF_INTER.lmax[1][40..42], [1, 0]);
        assert_eq!(TCOEF_INTRA.rmax[0][1..7], [14, 9, 7, 3, 2, 1]);
        assert_eq!(TCOEF_INTRA.rmax[1][1..5], [20, 6, 1, 0]);
        assert_eq!(TCOEF_INTER.rmax[0][1..8], [26, 10, 6, 2, 1, 1, 0]);
        assert_eq!(TCOEF_INTER.rmax[1][1..4], [40, 1, 0]);
    }

    #[test]
    fn every_event_reads_back_with_both_signs() {
        for (t, events) in [(&TCOEF_INTRA, &INTRA), (&TCOEF_INTER, &INTER)] {
            for &(last, run, level, code) in events.iter() {
                for sign in [0u8, 1] {
                    let mut bits: Vec<u8> = code.bytes().map(|b| b - b'0').collect();
                    bits.push(sign);
                    bits.resize(bits.len().next_multiple_of(8) + 32, 0);
                    let bytes: Vec<u8> = bits.chunks(8).map(|c| c.iter().fold(0, |a, b| a << 1 | b)).collect();
                    let mut br = Bits::new(&bytes);
                    let got = coef(t, &mut br).unwrap();
                    let want = if sign == 1 { -i32::from(level) } else { i32::from(level) };
                    assert_eq!(got, Coef { last: last == 1, run: run as usize, level: want }, "{code}");
                    assert_eq!(br.pos(), code.len() + 1);
                }
            }
        }
    }

    #[test]
    fn escapes_add_the_table_limits() {
        // bits of: escape, kind, the code for (0,0,2) in the inter table "1111" + sign 0
        let build = |s: &str| -> Vec<u8> {
            let mut b: Vec<u8> = s.bytes().filter(|c| *c != b' ').map(|c| c - b'0').collect();
            b.resize(b.len().next_multiple_of(8) + 32, 0);
            b.chunks(8).map(|c| c.iter().fold(0, |a, x| a << 1 | x)).collect()
        };
        let d = build("0000011 0 1111 1");
        assert_eq!(coef(&TCOEF_INTER, &mut Bits::new(&d)), Some(Coef { last: false, run: 0, level: -14 }));
        let d = build("0000011 10 1111 0");
        assert_eq!(coef(&TCOEF_INTER, &mut Bits::new(&d)), Some(Coef { last: false, run: 11, level: 2 }));
        let d = build("0000011 11 1 000101 1 111111111101 1");
        assert_eq!(coef(&TCOEF_INTER, &mut Bits::new(&d)), Some(Coef { last: true, run: 5, level: -3 }));
    }

    #[test]
    fn motion_differences() {
        let d = [0b0100_0000u8, 0, 0, 0];
        assert_eq!(mvd(&mut Bits::new(&d), 1), Some(1));
        let d = [0b0110_0000u8, 0, 0, 0];
        assert_eq!(mvd(&mut Bits::new(&d), 1), Some(-1));
        // motion code 2 with a 2-bit residual 3: (2 - 1) * 4 + 3 + 1
        let d = [0b0010_1100u8, 0, 0, 0];
        assert_eq!(mvd(&mut Bits::new(&d), 3), Some(8));
    }

    #[test]
    fn dc_differentials() {
        // size 2 luma ("10"), value 01 = -2
        let d = [0b1001_0000u8, 0, 0, 0];
        assert_eq!(intra_dc(&mut Bits::new(&d), true), Some(-2));
        // size 2 chroma ("01"), value 11 = 3
        let d = [0b0111_0000u8, 0, 0, 0];
        assert_eq!(intra_dc(&mut Bits::new(&d), false), Some(3));
    }
}
