//! Writing one frame (RFC 9639 §9): header with CRC-8, one subframe per channel, zero padding to a
//! byte, CRC-16.

use filmcraft_bitstream::BitWriter;

use crate::crc::{crc8, crc16};
use crate::predict::{Residual, Subframe};

/// Inter-channel decorrelation of a stereo frame (§9.1.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stereo {
    Independent,
    /// Left, then side (left − right).
    LeftSide,
    /// Side, then right.
    SideRight,
    /// Mid ((left + right) >> 1), then side.
    MidSide,
}

/// Frame header block size bits: 4096 has its own code, others are stored after the header.
fn block_size_code(n: u16) -> (u8, Option<(u32, u32)>) {
    match n {
        4096 => (0b1100, None),
        1..=256 => (0b0110, Some((u32::from(n - 1), 8))),
        _ => (0b0111, Some((u32::from(n.saturating_sub(1)), 16))),
    }
}

/// Frame header sample rate code; 0 refers to STREAMINFO.
fn sample_rate_code(rate: u32) -> u8 {
    match rate {
        88_200 => 1,
        176_400 => 2,
        192_000 => 3,
        8_000 => 4,
        16_000 => 5,
        22_050 => 6,
        24_000 => 7,
        32_000 => 8,
        44_100 => 9,
        48_000 => 10,
        96_000 => 11,
        _ => 0,
    }
}

/// Frame header sample size code; 0 refers to STREAMINFO.
fn sample_size_code(bps: u8) -> u8 {
    match bps {
        8 => 1,
        12 => 2,
        16 => 4,
        20 => 5,
        24 => 6,
        _ => 0,
    }
}

/// The frame number in the UTF-8-like coding of §9.1.5 (up to 36 bits).
fn coded_number(v: u64, out: &mut Vec<u8>) {
    if v < 0x80 {
        out.push(v as u8);
        return;
    }
    let tail = match v {
        0..0x800 => 1,
        0x800..0x1_0000 => 2,
        0x1_0000..0x20_0000 => 3,
        0x20_0000..0x400_0000 => 4,
        0x400_0000..0x8000_0000 => 5,
        _ => 6,
    };
    let lead_mask: u8 = !(0xFFu8 >> (tail + 1));
    out.push(lead_mask | (v >> (6 * tail)) as u8 & (0x7F >> (tail + 1)));
    for i in (0..tail).rev() {
        out.push(0x80 | ((v >> (6 * i)) & 0x3F) as u8);
    }
}

pub struct FrameSpec<'a> {
    pub number: u64,
    pub sample_rate: u32,
    pub bps: u8,
    pub stereo: Option<Stereo>,
    /// The signals as coded (mid/side already formed), with their subframes.
    pub channels: &'a [(&'a [i32], Subframe, u8)],
}

fn write_residual(w: &mut BitWriter, residual: &Residual, r: &[i64], block: usize, order: usize) {
    let wide = residual.wide();
    w.write_bits(u32::from(wide), 2);
    w.write_bits(u32::from(residual.partition_order), 4);
    let parts = 1usize << residual.partition_order;
    let len = block / parts;
    let mut start = 0usize;
    for (p, &k) in residual.params.iter().enumerate() {
        w.write_bits(u32::from(k), if wide { 5 } else { 4 });
        let n = if p == 0 { len.saturating_sub(order) } else { len };
        for &v in r.get(start..start + n).unwrap_or_default() {
            let u = if v >= 0 { (v as u64) << 1 } else { ((-v - 1) as u64) << 1 | 1 };
            let mut q = u >> k;
            while q >= 32 {
                w.write_bits(0, 32);
                q -= 32;
            }
            w.write_bits(1, q as u32 + 1);
            w.write_bits((u & ((1u64 << k) - 1)) as u32, u32::from(k));
        }
        start += n;
    }
}

fn write_subframe(w: &mut BitWriter, x: &[i32], sub: &Subframe, bps: u8) {
    let b = u32::from(bps);
    let sample = |w: &mut BitWriter, v: i32| w.write_bits(v as u32, b);
    w.write_bit(false);
    match sub {
        Subframe::Constant(v) => {
            w.write_bits(0, 6);
            w.write_bit(false);
            sample(w, *v);
        }
        Subframe::Verbatim => {
            w.write_bits(1, 6);
            w.write_bit(false);
            x.iter().for_each(|&v| sample(w, v));
        }
        Subframe::Fixed { order, residual } => {
            w.write_bits(0b001000 | u32::from(*order), 6);
            w.write_bit(false);
            let o = usize::from(*order);
            x.iter().take(o).for_each(|&v| sample(w, v));
            let r = crate::predict::fixed_residual(x, o).unwrap_or_default();
            write_residual(w, residual, &r, x.len(), o);
        }
        Subframe::Lpc { coefs, precision, shift, residual } => {
            let o = coefs.len();
            w.write_bits(0b100000 | (o as u32 - 1), 6);
            w.write_bit(false);
            x.iter().take(o).for_each(|&v| sample(w, v));
            w.write_bits(u32::from(*precision) - 1, 4);
            w.write_bits(u32::from(*shift), 5);
            coefs.iter().for_each(|&c| w.write_bits(c as u32, u32::from(*precision)));
            let r = crate::predict::lpc_residual(x, coefs, *shift).unwrap_or_default();
            write_residual(w, residual, &r, x.len(), o);
        }
    }
}

/// One complete frame.
pub fn write(spec: &FrameSpec) -> Vec<u8> {
    let block = spec.channels.first().map_or(0, |c| c.0.len());
    let (bs_code, bs_extra) = block_size_code(u16::try_from(block).unwrap_or(u16::MAX));
    let channel_code = match spec.stereo {
        Some(Stereo::LeftSide) => 8,
        Some(Stereo::SideRight) => 9,
        Some(Stereo::MidSide) => 10,
        _ => spec.channels.len().saturating_sub(1) as u8,
    };
    let mut h = vec![0xFF, 0xF8, bs_code << 4 | sample_rate_code(spec.sample_rate), channel_code << 4 | sample_size_code(spec.bps) << 1];
    coded_number(spec.number, &mut h);
    if let Some((v, n)) = bs_extra {
        if n == 8 {
            h.push(v as u8);
        } else {
            h.extend_from_slice(&(v as u16).to_be_bytes());
        }
    }
    h.push(crc8(&h));
    let mut w = BitWriter::new();
    w.write_bytes(&h);
    for (x, sub, bps) in spec.channels {
        write_subframe(&mut w, x, sub, *bps);
    }
    let mut out = w.finish();
    let crc = crc16(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coded_numbers() {
        let code = |v| {
            let mut o = Vec::new();
            coded_number(v, &mut o);
            o
        };
        assert_eq!(code(0), [0]);
        assert_eq!(code(0x7F), [0x7F]);
        assert_eq!(code(0x80), [0xC2, 0x80]);
        assert_eq!(code(0x7FF), [0xDF, 0xBF]);
        assert_eq!(code(0x800), [0xE0, 0xA0, 0x80]);
        assert_eq!(code(0xFFFF), [0xEF, 0xBF, 0xBF]);
        assert_eq!(code(0x1_0000), [0xF0, 0x90, 0x80, 0x80]);
        assert_eq!(code(0x7FFF_FFFF), [0xFD, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF]);
        assert_eq!(code(0xF_FFFF_FFFF), [0xFE, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF, 0xBF]);
    }
}
