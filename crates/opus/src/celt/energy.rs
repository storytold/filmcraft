//! Band energy decoding: Laplace-coded coarse energy, fine energy and the final refinement
//! (RFC 6716 §4.3.2).

use super::rate::MAX_FINE_BITS;
use super::tables::{BETA_COEF, BETA_INTRA, E_PROB_MODEL, NB_EBANDS, PRED_COEF, SMALL_ENERGY_ICDF};
use crate::range::RangeDecoder;

const LAPLACE_MINP: u32 = 1;
const LAPLACE_NMIN: u32 = 16;

fn laplace_get_freq1(fs0: u32, decay: u32) -> u32 {
    let ft = 32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN) - fs0;
    (ft * (16384 - decay)) >> 15
}

/// Decodes a Laplace-distributed integer (`fs` = probability of 0 in Q15, `decay` in Q14).
pub fn laplace_decode(dec: &mut RangeDecoder, mut fs: u32, decay: u32) -> i32 {
    let mut val = 0i32;
    let fm = dec.decode_bin(15);
    let mut fl = 0u32;
    if fm >= fs {
        val += 1;
        fl = fs;
        fs = laplace_get_freq1(fs, decay) + LAPLACE_MINP;
        while fs > LAPLACE_MINP && fm >= fl + 2 * fs {
            fs *= 2;
            fl += fs;
            fs = (((fs - 2 * LAPLACE_MINP) * decay) >> 15) + LAPLACE_MINP;
            val += 1;
        }
        if fs <= LAPLACE_MINP {
            let di = (fm - fl) >> 1;
            val += di as i32;
            fl += 2 * di * LAPLACE_MINP;
        }
        if fm < fl + fs {
            val = -val;
        } else {
            fl += fs;
        }
    }
    dec.update_bin(fl, (fl + fs).min(32768), 15);
    val
}

/// `unquant_coarse_energy`.
pub fn unquant_coarse_energy(start: usize, end: usize, old_e: &mut [f32], intra: bool, dec: &mut RangeDecoder, c: usize, lm: usize) {
    let prob = &E_PROB_MODEL[lm][intra as usize];
    let (coef, beta) = if intra { (0.0, BETA_INTRA) } else { (PRED_COEF[lm], BETA_COEF[lm]) };
    let budget = dec.storage() as i32 * 8;
    let mut prev = [0f32; 2];
    for i in start..end {
        for ch in 0..c {
            let tell = dec.tell();
            let qi = if budget - tell >= 15 {
                let pi = 2 * i.min(20);
                laplace_decode(dec, (prob[pi] as u32) << 7, (prob[pi + 1] as u32) << 6)
            } else if budget - tell >= 2 {
                let q = dec.icdf(&SMALL_ENERGY_ICDF, 2) as i32;
                (q >> 1) ^ -(q & 1)
            } else if budget - tell >= 1 {
                -(dec.bit_logp(1) as i32)
            } else {
                -1
            };
            let q = qi as f32;
            let idx = i + ch * NB_EBANDS;
            old_e[idx] = old_e[idx].max(-9.0);
            let tmp = coef * old_e[idx] + prev[ch] + q;
            old_e[idx] = tmp;
            prev[ch] = prev[ch] + q - beta * q;
        }
    }
}

pub fn unquant_fine_energy(start: usize, end: usize, old_e: &mut [f32], fine_quant: &[i32; NB_EBANDS], dec: &mut RangeDecoder, c: usize) {
    for i in start..end {
        let fq = fine_quant[i];
        if fq <= 0 {
            continue;
        }
        for ch in 0..c {
            let q2 = dec.bits(fq as u32);
            let offset = (q2 as f32 + 0.5) * (1 << (14 - fq)) as f32 / 16384.0 - 0.5;
            old_e[i + ch * NB_EBANDS] += offset;
        }
    }
}

pub struct EnergyFinalise<'a, 'd> {
    pub start: usize,
    pub end: usize,
    pub old_e: &'a mut [f32],
    pub fine_quant: &'a [i32; NB_EBANDS],
    pub fine_priority: &'a [i32; NB_EBANDS],
    pub bits_left: i32,
    pub decoder: &'a mut RangeDecoder<'d>,
    pub channels: usize,
}

pub fn unquant_energy_finalise(input: EnergyFinalise<'_, '_>) {
    let EnergyFinalise { start, end, old_e, fine_quant, fine_priority, mut bits_left, decoder: dec, channels: c } = input;
    for prio in 0..2 {
        let mut i = start;
        while i < end && bits_left >= c as i32 {
            if fine_quant[i] >= MAX_FINE_BITS || fine_priority[i] != prio {
                i += 1;
                continue;
            }
            for ch in 0..c {
                let q2 = dec.bits(1);
                let offset = (q2 as f32 - 0.5) * (1 << (14 - fine_quant[i] - 1)) as f32 / 16384.0;
                old_e[i + ch * NB_EBANDS] += offset;
                bits_left -= 1;
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::enc::RangeEncoder;

    fn laplace_encode(e: &mut RangeEncoder, value: i32, mut fs: u32, decay: u32) {
        let mut fl = 0u32;
        let mut val = value;
        if val != 0 {
            let s: i32 = if val < 0 { -1 } else { 0 };
            val = val.abs();
            fl = fs;
            fs = laplace_get_freq1(fs, decay);
            let mut i = 1;
            while fs > 0 && i < val {
                fs *= 2;
                fl += fs + 2 * LAPLACE_MINP;
                fs = (fs * decay) >> 15;
                i += 1;
            }
            if fs == 0 {
                let ndi_max = (32768 - fl + LAPLACE_MINP - 1) as i32;
                let ndi_max = (ndi_max - s) >> 1;
                let di = (val - i).min(ndi_max - 1);
                fl = (fl as i32 + (2 * di + 1 + s) * LAPLACE_MINP as i32) as u32;
                fs = LAPLACE_MINP.min(32768 - fl);
            } else {
                fs += LAPLACE_MINP;
                if s == 0 {
                    fl += fs;
                }
            }
        }
        e.encode(fl, fl + fs, 32768);
    }

    #[test]
    fn laplace_roundtrip() {
        for &(fs, decay) in &[(72u32 << 7, 127u32 << 6), (24 << 7, 179 << 6), (177 << 7, 11 << 6)] {
            let vals: Vec<i32> = (-12..=12).collect();
            let mut e = RangeEncoder::new(400);
            for &v in &vals {
                laplace_encode(&mut e, v, fs, decay);
            }
            let buf = e.done();
            let mut d = RangeDecoder::new(&buf);
            for &v in &vals {
                assert_eq!(laplace_decode(&mut d, fs, decay), v, "fs={fs} v={v}");
            }
        }
    }
}
