//! Band energy coding: Laplace-coded coarse energy, fine energy and the final refinement
//! (RFC 6716 §4.3.2). Each encoder function mirrors its decoder and updates the same predictor
//! state, so both sides hold identical quantised energies.

use super::rate::MAX_FINE_BITS;
use super::tables::{BETA_COEF, BETA_INTRA, E_PROB_MODEL, NB_EBANDS, PRED_COEF, SMALL_ENERGY_ICDF};
#[cfg(test)]
use crate::range::RangeDecoder;
use crate::range::RangeEncoder;

const LAPLACE_MINP: u32 = 1;
const LAPLACE_NMIN: u32 = 16;

fn laplace_get_freq1(fs0: u32, decay: u32) -> u32 {
    let ft = 32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN) - fs0;
    (ft * (16384 - decay)) >> 15
}

#[cfg(test)]
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

/// Encodes `value` with the Laplace model of [`laplace_decode`]; returns the value actually coded
/// (far tails are clamped to what the model can represent).
pub fn laplace_encode(e: &mut RangeEncoder, value: i32, fs0: u32, decay: u32) -> i32 {
    let mut fl = 0u32;
    let mut fs = fs0;
    let mut coded = value;
    if value != 0 {
        let s: i32 = if value < 0 { -1 } else { 0 };
        let mag = value.saturating_abs();
        fl = fs;
        fs = laplace_get_freq1(fs, decay);
        let mut i = 1;
        while fs > 0 && i < mag {
            fs *= 2;
            fl += fs + 2 * LAPLACE_MINP;
            fs = (fs * decay) >> 15;
            i += 1;
        }
        if fs == 0 {
            let ndi_max = (32768i64 - i64::from(fl) + i64::from(LAPLACE_MINP) - 1) as i32;
            let ndi_max = (ndi_max - s) >> 1;
            let di = (mag - i).min(ndi_max - 1).max(0);
            fl = (fl as i32 + (2 * di + 1 + s) * LAPLACE_MINP as i32).clamp(0, 32767) as u32;
            fs = LAPLACE_MINP.min(32768 - fl);
            coded = if s < 0 { -(i + di) } else { i + di };
        } else {
            fs += LAPLACE_MINP;
            if s == 0 {
                fl += fs;
            }
        }
    }
    e.encode(fl, (fl + fs).min(32768), 32768);
    coded
}

/// Encoder side of the decoder's `unquant_coarse_energy`: quantises the band energies `e` (log2 amplitude
/// minus the band mean, `[channel * NB_EBANDS + band]`) against the prediction from `old_e`,
/// which then holds the coarse energies the decoder reconstructs.
pub struct CoarseEncode<'a> {
    pub start: usize,
    pub end: usize,
    pub e: &'a [f32],
    pub old_e: &'a mut [f32],
    pub intra: bool,
    pub channels: usize,
    pub lm: usize,
}

pub fn quant_coarse_energy(enc: &mut RangeEncoder, input: CoarseEncode<'_>) {
    let CoarseEncode { start, end, e, old_e, intra, channels: c, lm } = input;
    let lm = lm.min(3);
    let prob = &E_PROB_MODEL[lm][intra as usize];
    let (coef, beta) = if intra { (0.0, BETA_INTRA) } else { (PRED_COEF[lm], BETA_COEF[lm]) };
    let budget = enc.storage() as i32 * 8;
    let mut prev = [0f32; 2];
    for i in start..end.min(NB_EBANDS) {
        for ch in 0..c.min(2) {
            let idx = i + ch * NB_EBANDS;
            let (Some(&target), Some(&old)) = (e.get(idx), old_e.get(idx)) else { continue };
            let old = old.max(-9.0);
            let f = coef * old + prev[ch];
            let x = if target.is_finite() { target.max(-28.0) } else { -28.0 };
            let mut qi = (x - f + 0.5).floor().clamp(-40.0, 40.0) as i32;
            let tell = enc.tell();
            if budget - tell >= 15 {
                let pi = 2 * i.min(20);
                qi = laplace_encode(enc, qi, (prob[pi] as u32) << 7, (prob[pi + 1] as u32) << 6);
            } else if budget - tell >= 2 {
                qi = qi.clamp(-1, 1);
                enc.icdf(
                    if qi == 0 {
                        0
                    } else if qi < 0 {
                        1
                    } else {
                        2
                    },
                    &SMALL_ENERGY_ICDF,
                    2,
                );
            } else if budget - tell >= 1 {
                qi = qi.clamp(-1, 0);
                enc.bit_logp(qi == -1, 1);
            } else {
                qi = -1;
            }
            let q = qi as f32;
            old_e[idx] = coef * old + prev[ch] + q;
            prev[ch] = prev[ch] + q - beta * q;
        }
    }
}

/// Encoder side of the decoder's `unquant_fine_energy`.
pub fn quant_fine_energy(enc: &mut RangeEncoder, start: usize, end: usize, e: &[f32], old_e: &mut [f32], fine_quant: &[i32; NB_EBANDS], c: usize) {
    for i in start..end.min(NB_EBANDS) {
        let fq = fine_quant[i];
        if fq <= 0 {
            continue;
        }
        let fq = fq.min(MAX_FINE_BITS);
        for ch in 0..c.min(2) {
            let idx = i + ch * NB_EBANDS;
            let (Some(&target), Some(old)) = (e.get(idx), old_e.get_mut(idx)) else { continue };
            let err = if target.is_finite() { target - *old } else { 0.0 };
            let q2 = ((err + 0.5) * (1 << fq) as f32).floor().clamp(0.0, ((1 << fq) - 1) as f32) as u32;
            enc.bits(q2, fq as u32);
            *old += (q2 as f32 + 0.5) * (1 << (14 - fq)) as f32 / 16384.0 - 0.5;
        }
    }
}

/// Encoder side of the decoder's `unquant_energy_finalise`: spends the frame's leftover bits on one more
/// bit of energy per band, in the decoder's priority order.
pub struct FinaliseEncode<'a> {
    pub start: usize,
    pub end: usize,
    pub e: &'a [f32],
    pub old_e: &'a mut [f32],
    pub fine_quant: &'a [i32; NB_EBANDS],
    pub fine_priority: &'a [i32; NB_EBANDS],
    pub bits_left: i32,
    pub channels: usize,
}

pub fn quant_energy_finalise(enc: &mut RangeEncoder, input: FinaliseEncode<'_>) {
    let FinaliseEncode { start, end, e, old_e, fine_quant, fine_priority, mut bits_left, channels: c } = input;
    let end = end.min(NB_EBANDS);
    for prio in 0..2 {
        let mut i = start;
        while i < end && bits_left >= c as i32 {
            if fine_quant[i] >= MAX_FINE_BITS || fine_priority[i] != prio {
                i += 1;
                continue;
            }
            for ch in 0..c {
                let idx = i + ch * NB_EBANDS;
                let target = e.get(idx).copied().unwrap_or(0.0);
                if let Some(old) = old_e.get_mut(idx) {
                    let q2 = (target.is_finite() && target - *old >= 0.0) as u32;
                    enc.bits(q2, 1);
                    *old += (q2 as f32 - 0.5) * (1 << (14 - fine_quant[i] - 1)) as f32 / 16384.0;
                }
                bits_left -= 1;
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Far tails are clamped, and the encoder reports the value the decoder will read.
    #[test]
    fn laplace_reports_clamped_values() {
        for v in [-4000, -300, -41, 41, 300, 4000] {
            let mut e = RangeEncoder::new(64);
            let coded = laplace_encode(&mut e, v, 72 << 7, 127 << 6);
            let buf = e.done().unwrap();
            assert_eq!(laplace_decode(&mut RangeDecoder::new(&buf), 72 << 7, 127 << 6), coded, "v={v}");
        }
    }

    #[test]
    fn laplace_roundtrip() {
        for &(fs, decay) in &[(72u32 << 7, 127u32 << 6), (24 << 7, 179 << 6), (177 << 7, 11 << 6)] {
            let vals: Vec<i32> = (-12..=12).collect();
            let mut e = RangeEncoder::new(400);
            for &v in &vals {
                assert_eq!(laplace_encode(&mut e, v, fs, decay), v);
            }
            let buf = e.done().unwrap();
            let mut d = RangeDecoder::new(&buf);
            for &v in &vals {
                assert_eq!(laplace_decode(&mut d, fs, decay), v, "fs={fs} v={v}");
            }
        }
    }
}
