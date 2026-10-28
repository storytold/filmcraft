//! CAVLC residual block parsing (9.2).

use crate::cavlc_tables::{COEFF_TOKEN, RUN_BEFORE, TOTAL_ZEROS_2X2, TOTAL_ZEROS_2X4, TOTAL_ZEROS_4X4};
use crate::error::{Result, ensure};
use filmcraft_bitstream::BitReader;
use std::sync::OnceLock;

/// Prefix-code lookup keyed by (number of leading zeros, next 5 bits after the first one bit).
pub(crate) struct VlcTable {
    /// entry = (len << 8) | value, 0 = invalid.
    entries: Box<[u16; 17 * 32]>,
}

impl VlcTable {
    pub fn build(codes: &[(&str, u8)]) -> Self {
        let mut entries = Box::new([0u16; 17 * 32]);
        for &(code, value) in codes {
            if code.is_empty() {
                continue;
            }
            let len = code.len();
            let lz = code.bytes().take_while(|&b| b == b'0').count();
            let val = ((len as u16) << 8) | value as u16;
            if lz == len {
                // all-zero code: any continuation with at least `len` zeros
                for l in lz..=16 {
                    for s in 0..32 {
                        entries[l * 32 + s] = val;
                    }
                }
                continue;
            }
            let suffix = &code[lz + 1..];
            assert!(suffix.len() <= 5, "suffix too long in {code}");
            let sbits = suffix.bytes().fold(0u32, |a, b| (a << 1) | u32::from(b == b'1'));
            let free = 5 - suffix.len();
            for ext in 0..(1u32 << free) {
                let s = (sbits << free) | ext;
                entries[lz * 32 + s as usize] = val;
            }
        }
        Self { entries }
    }

    #[inline]
    pub fn decode(&self, r: &mut BitReader) -> Result<u8> {
        let bits = r.peek(32);
        let lz = bits.leading_zeros().min(16);
        let s = if lz >= 16 { 0 } else { ((bits << lz) << 1) >> 27 };
        let e = self.entries[(lz * 32 + s) as usize];
        ensure!(e != 0, "invalid VLC code");
        r.skip((e >> 8) as usize)?;
        Ok(e as u8)
    }
}

struct Tables {
    /// coeff_token per nC class; value = (TrailingOnes << 5) | TotalCoeff
    coeff_token: [VlcTable; 6],
    total_zeros_4x4: [VlcTable; 15],
    total_zeros_2x2: [VlcTable; 3],
    total_zeros_2x4: [VlcTable; 7],
    run_before: [VlcTable; 7],
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let coeff_token = std::array::from_fn(|col| {
            let codes: Vec<(&str, u8)> = COEFF_TOKEN.iter().map(|&(t1, tc, ref c)| (c[col], (t1 << 5) | tc)).collect();
            VlcTable::build(&codes)
        });
        fn simple<const N: usize>(row: &[&'static str; N]) -> VlcTable {
            let codes: Vec<(&str, u8)> = row.iter().enumerate().map(|(i, &c)| (c, i as u8)).collect();
            VlcTable::build(&codes)
        }
        Tables {
            coeff_token,
            total_zeros_4x4: std::array::from_fn(|i| simple(&TOTAL_ZEROS_4X4[i])),
            total_zeros_2x2: std::array::from_fn(|i| simple(&TOTAL_ZEROS_2X2[i])),
            total_zeros_2x4: std::array::from_fn(|i| simple(&TOTAL_ZEROS_2X4[i])),
            run_before: std::array::from_fn(|i| simple(&RUN_BEFORE[i])),
        }
    })
}

/// Force table construction (so the first slice does not pay for it).
pub(crate) fn init_tables() {
    let _ = tables();
}

/// Parse residual_block_cavlc(). Coefficients are written to `coeff[start_idx..=end_idx]` in scan order
/// (entries outside the coded range are left untouched; the caller zeroes them).
/// `nc` is the nC value (-1 for 4:2:0 chroma DC, -2 for 4:2:2 chroma DC). Returns TotalCoeff.
pub(crate) fn residual_block(r: &mut BitReader, coeff: &mut [i32], start_idx: usize, end_idx: usize, max_num_coeff: usize, nc: i32) -> Result<u8> {
    let t = tables();
    let col = match nc {
        -1 => 4,
        -2 => 5,
        0..=1 => 0,
        2..=3 => 1,
        4..=7 => 2,
        _ => 3,
    };
    let tok = t.coeff_token[col].decode(r)?;
    let total_coeff = (tok & 31) as usize;
    let trailing_ones = (tok >> 5) as usize;
    if total_coeff == 0 {
        return Ok(0);
    }
    ensure!(total_coeff <= max_num_coeff, "TotalCoeff exceeds maxNumCoeff");
    let mut level = [0i32; 16];
    let mut suffix_length: u32 = if total_coeff > 10 && trailing_ones < 3 { 1 } else { 0 };
    for (i, lv) in level.iter_mut().enumerate().take(total_coeff) {
        if i < trailing_ones {
            *lv = if r.read_bit()? { -1 } else { 1 };
            continue;
        }
        // level_prefix
        let bits = r.peek(32);
        let level_prefix = bits.leading_zeros();
        ensure!(level_prefix <= 25, "level_prefix too large");
        r.skip(level_prefix as usize + 1)?;
        let level_suffix_size = if level_prefix == 14 && suffix_length == 0 {
            4
        } else if level_prefix >= 15 {
            level_prefix - 3
        } else {
            suffix_length
        };
        let level_suffix = if level_suffix_size > 0 { r.read_bits(level_suffix_size)? as i32 } else { 0 };
        let mut level_code = ((level_prefix.min(15) as i32) << suffix_length) + level_suffix;
        if level_prefix >= 15 && suffix_length == 0 {
            level_code += 15;
        }
        if level_prefix >= 16 {
            level_code += (1 << (level_prefix - 3)) - 4096;
        }
        if i == trailing_ones && trailing_ones < 3 {
            level_code += 2;
        }
        *lv = if level_code % 2 == 0 { (level_code + 2) >> 1 } else { (-level_code - 1) >> 1 };
        if suffix_length == 0 {
            suffix_length = 1;
        }
        if lv.abs() > (3 << (suffix_length - 1)) && suffix_length < 6 {
            suffix_length += 1;
        }
    }
    let mut zeros_left = if total_coeff < end_idx - start_idx + 1 {
        let tz_idx = total_coeff - 1;
        let tz = match max_num_coeff {
            4 => t.total_zeros_2x2[tz_idx].decode(r)?,
            8 => t.total_zeros_2x4[tz_idx].decode(r)?,
            _ => t.total_zeros_4x4[tz_idx].decode(r)?,
        } as usize;
        ensure!(tz + total_coeff <= end_idx - start_idx + 1, "total_zeros out of range");
        tz
    } else {
        0
    };
    // runs and placement: highest-frequency coefficient first
    let mut pos = start_idx + total_coeff + zeros_left - 1;
    for (i, &lv) in level.iter().enumerate().take(total_coeff) {
        coeff[pos] = lv;
        if i + 1 == total_coeff {
            break;
        }
        let run = if zeros_left > 0 {
            let run = t.run_before[zeros_left.min(7) - 1].decode(r)? as usize;
            ensure!(run <= zeros_left, "run_before out of range");
            zeros_left -= run;
            run
        } else {
            0
        };
        pos -= 1 + run;
    }
    Ok(total_coeff as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_bitstream::BitWriter;

    fn kraft(codes: &[&str]) -> f64 {
        codes.iter().filter(|c| !c.is_empty()).map(|c| 0.5f64.powi(c.len() as i32)).sum()
    }

    fn prefix_free(codes: &[&str]) -> bool {
        let c: Vec<&&str> = codes.iter().filter(|c| !c.is_empty()).collect();
        for (i, a) in c.iter().enumerate() {
            for (j, b) in c.iter().enumerate() {
                if i != j && b.starts_with(**a) {
                    return false;
                }
            }
        }
        true
    }

    #[test]
    fn coeff_token_tables_are_prefix_codes() {
        for col in 0..6 {
            let codes: Vec<&str> = COEFF_TOKEN.iter().map(|e| e.2[col]).collect();
            assert!(prefix_free(&codes), "column {col}");
            let k = kraft(&codes);
            assert!(k <= 1.0 + 1e-12, "column {col} kraft {k}");
        }
        // nC>=8 is a complete 6-bit fixed code except the unused 000011 pattern
        let codes: Vec<&str> = COEFF_TOKEN.iter().map(|e| e.2[3]).collect();
        assert!(codes.iter().all(|c| c.len() == 6));
    }

    #[test]
    fn total_zeros_and_run_before_are_complete_prefix_codes() {
        for (i, row) in TOTAL_ZEROS_4X4.iter().enumerate() {
            assert!(prefix_free(row));
            // tzVlcIndex 1 leaves the all-zero 9-bit code unused; all others are complete
            let k = kraft(row);
            if i == 0 {
                assert!((k - (1.0 - 1.0 / 512.0)).abs() < 1e-12, "{row:?}");
            } else {
                assert!((k - 1.0).abs() < 1e-12, "{row:?}");
            }
        }
        for row in TOTAL_ZEROS_2X2.iter() {
            assert!(prefix_free(row));
            assert!((kraft(row) - 1.0).abs() < 1e-12);
        }
        for row in TOTAL_ZEROS_2X4.iter() {
            assert!(prefix_free(row));
            assert!((kraft(row) - 1.0).abs() < 1e-12);
        }
        for row in RUN_BEFORE.iter() {
            assert!(prefix_free(row));
        }
    }

    fn write_code(w: &mut BitWriter, code: &str) {
        for b in code.bytes() {
            w.write_bit(b == b'1');
        }
    }

    #[test]
    fn decodes_spec_style_example() {
        // Block: 0 3 -1 0 | 0 -1 1 0 | 1 0 0 0 | 0 0 0 0 (zig-zag order), nC = 0.
        // TotalCoeff 5, TrailingOnes 3: coeff_token 0000 100.
        // trailing ones (reverse order): +1, -1, -1  -> sign bits 0 1 1
        // level -1? No: remaining levels (reverse): 1 (idx 4?) ... use the classic example:
        // coefficients in scan order: [0, 3, -1, 0, 0, -1, 1, 0, 1, 0, ...]
        // reverse order non-zero: 1 (pos 8), 1 (pos 6), -1 (pos 5), -1 (pos 2), 3 (pos 1)
        // TotalCoeff 5, TrailingOnes 3 (1, 1, -1).
        let mut w = BitWriter::new();
        write_code(&mut w, "0000100"); // coeff_token TC=5 T1=3, nC 0..2
        write_code(&mut w, "0"); // +1
        write_code(&mut w, "0"); // +1
        write_code(&mut w, "1"); // -1
        // level -1 with suffixLength 0: levelCode = 1 (T1 == 3 so no +2) -> prefix 1 => "01"
        write_code(&mut w, "01");
        // level 3 with suffixLength 1: levelCode = 4 -> prefix 2, suffix 0 => "001" "0"
        write_code(&mut w, "0010");
        // total_zeros = 4 (positions 0, 3, 4, 7) with tzVlcIndex 5 -> "110"
        write_code(&mut w, "110");
        // run_before: zerosLeft 4 at pos 8: run 1 -> "10"; zerosLeft 3 at pos 6: run 0 -> "11";
        // zerosLeft 3 at pos 5: run 2 -> "01"; zerosLeft 1 at pos 2: run 0 -> "1"
        write_code(&mut w, "10");
        write_code(&mut w, "11");
        write_code(&mut w, "01");
        write_code(&mut w, "1");
        w.rbsp_trailing();
        let b = w.finish();
        let mut r = BitReader::new(&b);
        let mut c = [0i32; 16];
        let tc = residual_block(&mut r, &mut c, 0, 15, 16, 0).unwrap();
        assert_eq!(tc, 5);
        assert_eq!(&c[..10], &[0, 3, -1, 0, 0, -1, 1, 0, 1, 0]);
    }
}
