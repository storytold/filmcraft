//! Pulse cache and bit allocation (RFC 6716 §4.3.3, §4.3.4.1).

use super::tables::{ALLOC, EBANDS, LOG2_FRAC_TABLE, MAX_LM, NB_EBANDS, OVERLAP};
use crate::range::{BITRES, RangeDecoder};

const MAX_PSEUDO: i32 = 40;
const LOG_MAX_PSEUDO: i32 = 6;
pub const MAX_FINE_BITS: i32 = 8;
const FINE_OFFSET: i32 = 21;
pub const QTHETA_OFFSET: i32 = 4;
pub const QTHETA_OFFSET_TWOPHASE: i32 = 16;
const ALLOC_STEPS: i32 = 6;

/// Number of pulses represented by pseudo-pulse index `i`.
pub fn get_pulses(i: i32) -> i32 {
    if i < 8 { i } else { (8 + (i & 7)) << ((i >> 3) - 1) }
}

/// Conservative (rounded up) log2 of `val` with `frac` fractional bits.
pub fn log2_frac(mut val: u32, mut frac: u32) -> i32 {
    let mut l = crate::range::ilog(val);
    if val & val.wrapping_sub(1) != 0 {
        if l > 16 {
            val = ((val - 1) >> (l - 16)) + 1;
        } else {
            val <<= 16 - l;
        }
        let mut l = (l - 1) << frac;
        loop {
            let b = (val >> 16) as i32;
            l += b << frac;
            val = (val + b as u32) >> b;
            val = (val * val + 0x7FFF) >> 15;
            if frac == 0 {
                break;
            }
            frac -= 1;
        }
        l + (val > 0x8000) as i32
    } else {
        l -= 1;
        l << frac
    }
}

/// PVQ codebook size V(N, K) as u128 (saturating far above 2^32).
pub fn pvq_v(n: usize, k: usize) -> u128 {
    // V(N,K) = V(N-1,K) + V(N,K-1) + V(N-1,K-1); computed row by row.
    if k == 0 {
        return 1;
    }
    if n == 0 {
        return 0;
    }
    let mut row = vec![0u128; k + 1]; // V(0, k)
    row[0] = 1;
    for _ in 0..n {
        let mut prev_diag = row[0]; // V(n-1, 0)
        // V(n, 0) = 1
        for kk in 1..=k {
            let up = row[kk]; // V(n-1, kk)
            let v = up.saturating_add(row[kk - 1]).saturating_add(prev_diag);
            prev_diag = up;
            row[kk] = v.min(u128::MAX >> 8);
        }
        row[0] = 1;
    }
    row[k]
}

fn fits_in32(n: usize, k: i32) -> bool {
    pvq_v(n, k as usize) <= u32::MAX as u128
}

/// Precomputed per-mode data shared by all CELT decoders.
pub struct Mode {
    /// log2 of band width in 1/8 bits.
    pub log_n: [i32; NB_EBANDS],
    /// Index into `cache_bits` for `(LM+1)*NB_EBANDS + band` (-1 when unused).
    pub cache_index: Vec<i16>,
    /// `cache[0]` = max pseudo-pulses; `cache[k]` = bits(k) - 1 in 1/8 bits.
    pub cache_bits: Vec<u8>,
    /// Per-band maximum allocation: `[(2*LM + C-1)*NB_EBANDS + band]`.
    pub caps: Vec<u8>,
    pub window: [f32; OVERLAP],
    pub mdct: [super::mdct::Imdct; MAX_LM + 1],
}

impl Mode {
    pub fn new() -> Mode {
        let mut log_n = [0i32; NB_EBANDS];
        for (i, l) in log_n.iter_mut().enumerate() {
            *l = log2_frac((EBANDS[i + 1] - EBANDS[i]) as u32, BITRES);
        }
        let (cache_index, cache_bits) = compute_pulse_cache(MAX_LM as i32);
        let mut m = Mode {
            log_n,
            cache_index,
            cache_bits,
            caps: Vec::new(),
            window: [0.0; OVERLAP],
            mdct: [super::mdct::Imdct::new(1920), super::mdct::Imdct::new(960), super::mdct::Imdct::new(480), super::mdct::Imdct::new(240)],
        };
        m.caps = compute_caps(&m);
        for i in 0..OVERLAP {
            let x = std::f64::consts::FRAC_PI_2 * (i as f64 + 0.5) / OVERLAP as f64;
            let s = x.sin();
            m.window[i] = (std::f64::consts::FRAC_PI_2 * s * s).sin() as f32;
        }
        m
    }

    #[inline]
    pub fn cache(&self, lm: i32, band: usize) -> &[u8] {
        let idx = self.cache_index[((lm + 1) as usize) * NB_EBANDS + band];
        &self.cache_bits[idx.max(0) as usize..]
    }

    pub fn bits2pulses(&self, band: usize, lm: i32, bits: i32) -> i32 {
        let cache = self.cache(lm, band);
        let mut lo = 0i32;
        let mut hi = cache[0] as i32;
        let bits = bits - 1;
        for _ in 0..LOG_MAX_PSEUDO {
            let mid = (lo + hi + 1) >> 1;
            if cache[mid as usize] as i32 >= bits {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let lo_bits = if lo == 0 { -1 } else { cache[lo as usize] as i32 };
        if bits - lo_bits <= cache[hi as usize] as i32 - bits { lo } else { hi }
    }

    pub fn pulses2bits(&self, band: usize, lm: i32, pulses: i32) -> i32 {
        if pulses == 0 { 0 } else { self.cache(lm, band)[pulses as usize] as i32 + 1 }
    }

    /// Per-band allocation caps for this frame (`init_caps`).
    pub fn init_caps(&self, lm: usize, c: usize) -> [i32; NB_EBANDS] {
        let mut cap = [0i32; NB_EBANDS];
        for (i, cp) in cap.iter_mut().enumerate() {
            let n = (EBANDS[i + 1] - EBANDS[i]) << lm;
            *cp = ((self.caps[NB_EBANDS * (2 * lm + c - 1) + i] as i32 + 64) * c as i32 * n) >> 2;
        }
        cap
    }
}

fn compute_pulse_cache(lm_max: i32) -> (Vec<i16>, Vec<u8>) {
    let nb = NB_EBANDS;
    let rows = (lm_max + 2) as usize;
    let mut index = vec![-1i16; nb * rows];
    let mut entries: Vec<(usize, i32, usize)> = Vec::new(); // (N, K, offset)
    let mut curr = 0usize;
    for i in 0..rows {
        for j in 0..nb {
            let n = (((EBANDS[j + 1] - EBANDS[j]) << i) >> 1) as usize;
            index[i * nb + j] = -1;
            // Reuse the entry of an earlier band with the same size.
            'search: for k in 0..=i {
                for nn in 0..nb {
                    if k == i && nn >= j {
                        break;
                    }
                    if n == (((EBANDS[nn + 1] - EBANDS[nn]) << k) >> 1) as usize {
                        index[i * nb + j] = index[k * nb + nn];
                        break 'search;
                    }
                }
            }
            if index[i * nb + j] == -1 && n != 0 {
                let mut k = 0;
                while k < MAX_PSEUDO && fits_in32(n, get_pulses(k + 1)) {
                    k += 1;
                }
                index[i * nb + j] = curr as i16;
                entries.push((n, k, curr));
                curr += (k + 1) as usize;
            }
        }
    }
    let mut bits = vec![0u8; curr];
    for &(n, k, off) in &entries {
        let maxk = get_pulses(k) as usize;
        let tmp = required_bits(n, maxk, BITRES);
        for j in 1..=k {
            bits[off + j as usize] = (tmp[get_pulses(j) as usize] - 1) as u8;
        }
        bits[off] = k as u8;
    }
    (index, bits)
}

/// Bits (1/2^frac units, rounded up) needed for each K in `0..=maxk` for dimension `n`.
fn required_bits(n: usize, maxk: usize, frac: u32) -> Vec<i32> {
    let mut out = vec![0i32; maxk + 1];
    if n == 1 {
        for o in out.iter_mut().skip(1) {
            *o = 1 << frac;
        }
    } else {
        for (k, o) in out.iter_mut().enumerate().skip(1) {
            let v = pvq_v(n, k);
            *o = log2_frac(v as u32, frac);
        }
    }
    out
}

fn compute_caps(m: &Mode) -> Vec<u8> {
    let nb = NB_EBANDS;
    let mut caps = vec![0u8; (MAX_LM + 1) * 2 * nb];
    for i in 0..=MAX_LM as i32 {
        for c in 1..=2i32 {
            for j in 0..nb {
                let mut n0 = EBANDS[j + 1] - EBANDS[j];
                let max_bits = if n0 << i == 1 {
                    (c * (1 + MAX_FINE_BITS)) << BITRES
                } else {
                    let mut lm0 = 0;
                    if n0 > 2 {
                        n0 >>= 1;
                        lm0 -= 1;
                    } else if n0 <= 1 {
                        lm0 = i.min(1);
                        n0 <<= lm0;
                    }
                    let pcache = m.cache(lm0, j);
                    let mut mb = pcache[pcache[0] as usize] as i32 + 1;
                    let mut n = n0;
                    for k in 0..(i - lm0) {
                        mb <<= 1;
                        let offset = ((m.log_n[j] + ((lm0 + k) << BITRES)) >> 1) - QTHETA_OFFSET;
                        let num = 459 * ((2 * n - 1) * offset + mb);
                        let den = ((2 * n - 1) << 9) - 459;
                        let qb = ((num + (den >> 1)) / den).min(57);
                        mb += qb;
                        n <<= 1;
                    }
                    if c == 2 {
                        mb <<= 1;
                        let offset = ((m.log_n[j] + (i << BITRES)) >> 1) - if n == 2 { QTHETA_OFFSET_TWOPHASE } else { QTHETA_OFFSET };
                        let ndof = 2 * n - 1 - (n == 2) as i32;
                        let p = if n == 2 { 512 } else { 487 };
                        let num = p * (mb + ndof * offset);
                        let den = (ndof << 9) - p;
                        let qb = ((num + (den >> 1)) / den).min(if n == 2 { 64 } else { 61 });
                        mb += qb;
                    }
                    let ndof = c * n + if c == 2 && n > 2 { 1 } else { 0 };
                    let mut offset = ((m.log_n[j] + (i << BITRES)) >> 1) - FINE_OFFSET;
                    if n == 2 {
                        offset += (1 << BITRES) >> 2;
                    }
                    let num = mb + ndof * offset;
                    let den = (ndof - 1) << BITRES;
                    let qb = ((num + (den >> 1)) / den).min(MAX_FINE_BITS);
                    mb += (c * qb) << BITRES;
                    mb
                };
                let w = c * ((EBANDS[j + 1] - EBANDS[j]) << i);
                let v = (4 * max_bits / w) - 64;
                caps[((i * 2 + c - 1) as usize) * nb + j] = v.clamp(0, 255) as u8;
            }
        }
    }
    caps
}

/// Result of the allocation process.
pub struct Allocation {
    pub coded_bands: usize,
    pub intensity: usize,
    pub dual_stereo: bool,
    pub balance: i32,
    pub pulses: [i32; NB_EBANDS],
    pub fine_quant: [i32; NB_EBANDS],
    pub fine_priority: [i32; NB_EBANDS],
}

/// `clt_compute_allocation` (decoder side).
pub struct AllocationInput<'a, 'd> {
    pub mode: &'a Mode,
    pub start: usize,
    pub end: usize,
    pub offsets: &'a [i32; NB_EBANDS],
    pub cap: &'a [i32; NB_EBANDS],
    pub trim: i32,
    pub total: i32,
    pub channels: usize,
    pub lm: usize,
    pub decoder: &'a mut RangeDecoder<'d>,
}

pub fn compute_allocation(input: AllocationInput<'_, '_>) -> Allocation {
    let AllocationInput { mode: m, start, end, offsets, cap, trim: alloc_trim, total, channels: c, lm, decoder: dec } = input;
    let ci = c as i32;
    let lmi = lm as i32;
    let mut total = total.max(0);
    let mut skip_start = start;
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let mut intensity_rsv = 0;
    let mut dual_stereo_rsv = 0;
    if c == 2 {
        intensity_rsv = LOG2_FRAC_TABLE[end - start];
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }
    let mut bits1 = [0i32; NB_EBANDS];
    let mut bits2 = [0i32; NB_EBANDS];
    let mut thresh = [0i32; NB_EBANDS];
    let mut trim_offset = [0i32; NB_EBANDS];
    for j in start..end {
        let w = EBANDS[j + 1] - EBANDS[j];
        thresh[j] = (ci << BITRES).max(((3 * w) << lmi << BITRES) >> 4);
        trim_offset[j] = (ci * w * (alloc_trim - 5 - lmi) * (end as i32 - j as i32 - 1) * (1 << (lmi + BITRES as i32))) >> 6;
        if w << lmi == 1 {
            trim_offset[j] -= ci << BITRES;
        }
    }
    let nb_alloc = ALLOC.len() as i32;
    let mut lo = 1;
    let mut hi = nb_alloc - 1;
    loop {
        let mut done = false;
        let mut psum = 0;
        let mid = (lo + hi) >> 1;
        for j in (start..end).rev() {
            let n = EBANDS[j + 1] - EBANDS[j];
            let mut bitsj = (ci * n * (ALLOC[mid as usize][j] as i32)) << lmi >> 2;
            if bitsj > 0 {
                bitsj = (bitsj + trim_offset[j]).max(0);
            }
            bitsj += offsets[j];
            if bitsj >= thresh[j] || done {
                done = true;
                psum += bitsj.min(cap[j]);
            } else if bitsj >= ci << BITRES {
                psum += ci << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
        if lo > hi {
            break;
        }
    }
    hi = lo;
    lo -= 1;
    for j in start..end {
        let n = EBANDS[j + 1] - EBANDS[j];
        let mut bits1j = (ci * n * (ALLOC[lo as usize][j] as i32)) << lmi >> 2;
        let mut bits2j = if hi >= nb_alloc { cap[j] } else { (ci * n * (ALLOC[hi as usize][j] as i32)) << lmi >> 2 };
        if bits1j > 0 {
            bits1j = (bits1j + trim_offset[j]).max(0);
        }
        if bits2j > 0 {
            bits2j = (bits2j + trim_offset[j]).max(0);
        }
        if lo > 0 {
            bits1j += offsets[j];
        }
        bits2j += offsets[j];
        if offsets[j] > 0 {
            skip_start = j;
        }
        bits2j = (bits2j - bits1j).max(0);
        bits1[j] = bits1j;
        bits2[j] = bits2j;
    }
    interp_bits2pulses(InterpPulsesInput {
        mode: m,
        start,
        end,
        skip_start,
        bits1: &bits1,
        bits2: &bits2,
        threshold: &thresh,
        cap,
        total,
        skip_reservation: skip_rsv,
        intensity_reservation: intensity_rsv,
        stereo_reservation: dual_stereo_rsv,
        channels: c,
        lm,
        decoder: dec,
    })
}

struct InterpPulsesInput<'a, 'd> {
    mode: &'a Mode,
    start: usize,
    end: usize,
    skip_start: usize,
    bits1: &'a [i32; NB_EBANDS],
    bits2: &'a [i32; NB_EBANDS],
    threshold: &'a [i32; NB_EBANDS],
    cap: &'a [i32; NB_EBANDS],
    total: i32,
    skip_reservation: i32,
    intensity_reservation: i32,
    stereo_reservation: i32,
    channels: usize,
    lm: usize,
    decoder: &'a mut RangeDecoder<'d>,
}

fn interp_bits2pulses(input: InterpPulsesInput<'_, '_>) -> Allocation {
    let InterpPulsesInput {
        mode: m,
        start,
        end,
        skip_start,
        bits1,
        bits2,
        threshold: thresh,
        cap,
        mut total,
        skip_reservation: skip_rsv,
        intensity_reservation: mut intensity_rsv,
        stereo_reservation: mut dual_stereo_rsv,
        channels: c,
        lm,
        decoder: dec,
    } = input;
    let ci = c as i32;
    let alloc_floor = ci << BITRES;
    let stereo = (c > 1) as i32;
    let log_m = (lm as i32) << BITRES;
    let mut lo = 0;
    let mut hi = 1 << ALLOC_STEPS;
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + ((mid * bits2[j]) >> ALLOC_STEPS);
            if tmp >= thresh[j] || done {
                done = true;
                psum += tmp.min(cap[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut bits = [0i32; NB_EBANDS];
    let mut psum = 0;
    let mut done = false;
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + ((lo * bits2[j]) >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        tmp = tmp.min(cap[j]);
        bits[j] = tmp;
        psum += tmp;
    }
    let s = start;
    let mut coded_bands = end;
    loop {
        let j = coded_bands - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }
        let mut left = total - psum;
        let span = EBANDS[coded_bands] - EBANDS[s];
        let percoeff = left / span;
        left -= span * percoeff;
        let rem = (left - (EBANDS[j] - EBANDS[s])).max(0);
        let band_width = EBANDS[coded_bands] - EBANDS[j];
        let mut band_bits = bits[j] + percoeff * band_width + rem;
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if dec.bit_logp(1) {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= bits[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = LOG2_FRAC_TABLE[j - s];
        }
        psum += intensity_rsv;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            bits[j] = alloc_floor;
        } else {
            bits[j] = 0;
        }
        coded_bands -= 1;
    }
    let intensity = if intensity_rsv > 0 { s + dec.uint((coded_bands + 1 - s) as u32) as usize } else { 0 };
    if intensity <= s {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    let dual_stereo = if dual_stereo_rsv > 0 { dec.bit_logp(1) } else { false };
    let mut left = total - psum;
    let span = EBANDS[coded_bands] - EBANDS[s];
    let percoeff = left / span;
    left -= span * percoeff;
    for j in s..coded_bands {
        bits[j] += percoeff * (EBANDS[j + 1] - EBANDS[j]);
    }
    for j in s..coded_bands {
        let tmp = left.min(EBANDS[j + 1] - EBANDS[j]);
        bits[j] += tmp;
        left -= tmp;
    }
    let mut ebits = [0i32; NB_EBANDS];
    let mut fine_priority = [0i32; NB_EBANDS];
    let mut balance = 0;
    for j in s..coded_bands {
        let n0 = EBANDS[j + 1] - EBANDS[j];
        let n = n0 << lm;
        let bit = bits[j] + balance;
        let mut excess;
        if n > 1 {
            excess = (bit - cap[j]).max(0);
            bits[j] = bit - excess;
            let den = ci * n + if c == 2 && n > 2 && !dual_stereo && j < intensity { 1 } else { 0 };
            let nclogn = den * (m.log_n[j] + log_m);
            let mut offset = (nclogn >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += (den << BITRES) >> 2;
            }
            if bits[j] + offset < (den * 2) << BITRES {
                offset += nclogn >> 2;
            } else if bits[j] + offset < (den * 3) << BITRES {
                offset += nclogn >> 3;
            }
            let mut eb = (bits[j] + offset + (den << (BITRES - 1))).max(0);
            eb = (eb / den) >> BITRES;
            if ci * eb > (bits[j] >> BITRES) {
                eb = bits[j] >> stereo >> BITRES;
            }
            eb = eb.min(MAX_FINE_BITS);
            ebits[j] = eb;
            fine_priority[j] = (eb * (den << BITRES) >= bits[j] + offset) as i32;
            bits[j] -= (ci * eb) << BITRES;
        } else {
            excess = (bit - (ci << BITRES)).max(0);
            bits[j] = bit - excess;
            ebits[j] = 0;
            fine_priority[j] = 1;
        }
        if excess > 0 {
            let extra_fine = (excess >> (stereo + BITRES as i32)).min(MAX_FINE_BITS - ebits[j]);
            ebits[j] += extra_fine;
            let extra_bits = (extra_fine * ci) << BITRES;
            fine_priority[j] = (extra_bits >= excess - balance) as i32;
            excess -= extra_bits;
        }
        balance = excess;
    }
    for j in coded_bands..end {
        ebits[j] = bits[j] >> stereo >> BITRES;
        bits[j] = 0;
        fine_priority[j] = (ebits[j] < 1) as i32;
    }
    Allocation { coded_bands, intensity, dual_stereo, balance, pulses: bits, fine_quant: ebits, fine_priority }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log2_frac_values() {
        assert_eq!(log2_frac(1, 3), 0);
        assert_eq!(log2_frac(2, 3), 8);
        assert_eq!(log2_frac(4, 3), 16);
        assert_eq!(log2_frac(6, 3), 21);
        assert_eq!(log2_frac(18, 3), 34);
        assert_eq!(log2_frac(22, 3), 36);
    }

    #[test]
    fn pvq_counts() {
        // V(N,1) = 2N, V(1,K) = 2 (K>0), V(2,K) = 4K.
        assert_eq!(pvq_v(5, 1), 10);
        assert_eq!(pvq_v(1, 7), 2);
        assert_eq!(pvq_v(2, 9), 36);
        assert_eq!(pvq_v(3, 2), 18);
        assert_eq!(pvq_v(0, 0), 1);
        assert_eq!(pvq_v(0, 3), 0);
    }

    #[test]
    fn caps_match_reference_shape() {
        let m = Mode::new();
        // 1-bin bands at LM=0 (mono and stereo) are capped at 224.
        assert!(m.caps[..8].iter().all(|&c| c == 224));
        assert!(m.caps[21..29].iter().all(|&c| c == 224));
        let first_rows: [u8; 42] = [
            224, 224, 224, 224, 224, 224, 224, 224, 160, 160, 160, 160, 185, 185, 185, 178, 178, 168, 134, 61, 37, 224, 224, 224, 224, 224, 224, 224, 224, 240,
            240, 240, 240, 207, 207, 207, 198, 198, 183, 144, 66, 40,
        ];
        assert_eq!(&m.caps[..42], &first_rows[..]);
    }
}
