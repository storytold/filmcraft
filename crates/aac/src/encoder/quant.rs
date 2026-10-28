//! Quantisation, scalefactor selection, codebook/section selection, bit counting and writing of one
//! `individual_channel_stream`.
//!
//! Coefficients are kept in bitstream order ("interleaved": group → band → window → line), so every
//! (group, band) is a contiguous slice and short-window grouping needs no special casing.

use filmcraft_bitstream::BitWriter;

use crate::huffman::{self, ZERO_HCB};
use crate::ics::IcsInfo;
use crate::tables::pow43;
use crate::tns::TnsData;

/// Magic rounding offset for AAC quantisation (`0.5 - 0.0946`), minimising the error for the
/// `x^(3/4)` companding.
const ROUND: f32 = 0.4054;
const MAX_Q: i32 = 8191;

/// One (group, band) of a channel, in bitstream order.
#[derive(Clone, Copy, Debug)]
pub struct Band {
    /// Range in the interleaved coefficient array.
    pub start: usize,
    pub end: usize,
    /// Allowed noise energy at λ = 1.
    pub thr: f32,
    /// Coefficient energy.
    pub en: f32,
    /// Largest `|x|^(3/4)`.
    pub max34: f32,
}

/// Everything the rate loop needs for one channel.
pub struct ChannelInput {
    pub info: IcsInfo,
    /// Interleaved (bitstream-order) coefficients.
    pub x: Vec<f32>,
    pub xabs: Vec<f32>,
    pub x34: Vec<f32>,
    pub bands: Vec<Band>,
    pub tns: Option<TnsData>,
}

impl ChannelInput {
    /// `spec` is window-major (short windows at stride 128); `thr[g][sfb]` and the grouping come from
    /// `info`.
    pub fn new(info: IcsInfo, swb: &[u16], spec: &[f32], thr: &[Vec<f32>], tns: Option<TnsData>) -> ChannelInput {
        let mut x = Vec::with_capacity(1024);
        let mut bands = Vec::new();
        for g in 0..info.num_groups {
            let w0 = info.group_start(g);
            for sfb in 0..info.max_sfb {
                let start = x.len();
                for w in w0..w0 + info.group_len[g] as usize {
                    x.extend_from_slice(&spec[w * 128 + swb[sfb] as usize..w * 128 + swb[sfb + 1] as usize]);
                }
                bands.push(Band { start, end: x.len(), thr: thr[g][sfb], en: 0.0, max34: 0.0 });
            }
        }
        let xabs: Vec<f32> = x.iter().map(|v| v.abs()).collect();
        let x34: Vec<f32> = xabs.iter().map(|v| v.powf(0.75)).collect();
        for b in &mut bands {
            b.en = x[b.start..b.end].iter().map(|v| v * v).sum();
            b.max34 = x34[b.start..b.end].iter().cloned().fold(0.0, f32::max);
        }
        ChannelInput { info, x, xabs, x34, bands, tns }
    }
}

/// A quantised channel ready to be written.
#[derive(Clone, Default)]
pub struct QuantChannel {
    pub q: Vec<i32>,
    /// Scalefactor per band (only meaningful where `cb != 0`).
    pub sf: Vec<i32>,
    pub cb: Vec<u8>,
    /// Per group: (codebook, number of bands).
    pub sections: Vec<Vec<(u8, usize)>>,
    pub global_gain: i32,
    /// Bits of the ICS excluding `ics_info`.
    pub bits: u32,
}

#[inline]
fn gain34(sf: i32) -> f32 {
    (-0.1875 * (sf as f32 - 100.0)).exp2()
}
#[inline]
fn gain(sf: i32) -> f32 {
    (0.25 * (sf as f32 - 100.0)).exp2()
}

/// Quantise a slice at `sf`, returning the distortion; writes the (signed) values to `out` if given.
fn quantise(x: &[f32], xabs: &[f32], x34: &[f32], sf: i32, out: Option<&mut [i32]>) -> f32 {
    let g34 = gain34(sf);
    let g = gain(sf);
    let mut d = 0f32;
    match out {
        Some(out) => {
            for i in 0..x.len() {
                let q = ((x34[i] * g34 + ROUND) as i32).min(MAX_Q);
                let e = xabs[i] - pow43(q as u32) * g;
                d += e * e;
                out[i] = if x[i] < 0.0 { -q } else { q };
            }
        }
        None => {
            for i in 0..x.len() {
                let q = ((x34[i] * g34 + ROUND) as i32).min(MAX_Q);
                let e = xabs[i] - pow43(q as u32) * g;
                d += e * e;
            }
        }
    }
    d
}

/// Largest scalefactor whose distortion stays within `allowed`.
fn search_sf(ch: &ChannelInput, b: &Band, allowed: f32) -> i32 {
    let r = b.start..b.end;
    let (x, xa, x34) = (&ch.x[r.clone()], &ch.xabs[r.clone()], &ch.x34[r]);
    let l2 = b.max34.log2();
    let lo = ((100.0 - ((MAX_Q as f32 + 0.5).log2() - l2) / 0.1875).ceil() as i32).clamp(0, 255);
    let zero = ((100.0 - ((1.0 - ROUND).log2() - l2) / 0.1875).floor() as i32 + 1).clamp(0, 256);
    let mut a = lo;
    let mut hi = (zero - 1).clamp(lo, 255);
    if quantise(x, xa, x34, a, None) > allowed {
        return a;
    }
    while a < hi {
        let mid = (a + hi + 1) / 2;
        if quantise(x, xa, x34, mid, None) <= allowed {
            a = mid;
        } else {
            hi = mid - 1;
        }
    }
    a
}

/// Candidate codebooks for a band whose largest magnitude is `m` (0 for all-zero bands).
fn candidates(m: i32) -> &'static [u8] {
    match m {
        0 => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        1 => &[1, 2, 3, 4, 5, 6],
        2 => &[3, 4, 5, 6, 7, 8],
        3..=4 => &[5, 6, 7, 8, 9, 10],
        5..=7 => &[7, 8, 9, 10, 11],
        8..=12 => &[9, 10, 11],
        _ => &[11],
    }
}

/// Bring the scalefactor differences between coded bands (`sf >= 0`) within ±60 by lowering the
/// offenders, sweeping up and down until stable.
fn limit_sf_steps(sf: &mut [i32]) {
    let coded: Vec<usize> = (0..sf.len()).filter(|&i| sf[i] >= 0).collect();
    for _ in 0..8 {
        let mut changed = false;
        for w in coded.windows(2) {
            if sf[w[1]] > sf[w[0]] + 60 {
                sf[w[1]] = sf[w[0]] + 60;
                changed = true;
            }
        }
        for w in coded.windows(2).rev() {
            if sf[w[0]] > sf[w[1]] + 60 {
                sf[w[0]] = sf[w[1]] + 60;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// Quantise every band at its scalefactor into `q`, recording each band's largest magnitude in
/// `maxq`. A band that quantises to all zeros leaves the chain (`sf = -1`); returns whether any did.
fn quantise_bands(ch: &ChannelInput, sf: &mut [i32], q: &mut [i32], maxq: &mut [i32]) -> bool {
    let mut removed = false;
    for (i, b) in ch.bands.iter().enumerate() {
        let r = b.start..b.end;
        if sf[i] < 0 {
            q[r].fill(0);
            maxq[i] = 0;
            continue;
        }
        quantise(&ch.x[r.clone()], &ch.xabs[r.clone()], &ch.x34[r.clone()], sf[i], Some(&mut q[r.clone()]));
        maxq[i] = q[r].iter().map(|v| v.abs()).max().unwrap_or(0);
        if maxq[i] == 0 {
            sf[i] = -1;
            removed = true;
        }
    }
    removed
}

/// Cost sentinel for a codebook that cannot code a band.
const INF: u32 = u32::MAX / 4;

/// Bits to code each band of a group with each codebook (`INF` when the codebook cannot).
fn codebook_costs(ch: &ChannelInput, q: &[i32], maxq: &[i32], base: usize, n: usize) -> Vec<[u32; 12]> {
    let mut cost = vec![[INF; 12]; n];
    for i in 0..n {
        let bi = base + i;
        let b = &ch.bands[bi];
        let vals = &q[b.start..b.end];
        for &c in candidates(maxq[bi]) {
            cost[i][c as usize] = if c == 0 {
                0
            } else {
                // all-zero band inside a coded section also needs a (zero) scalefactor difference
                huffman::count_bits(c, vals) + if maxq[bi] == 0 { 1 } else { 0 }
            };
        }
    }
    cost
}

/// Dynamic programming over the codebook of each band of a group, charging `new_sec` bits for every
/// section start. Returns the chosen codebook per band.
fn best_codebooks(cost: &[[u32; 12]], new_sec: u32) -> Vec<u8> {
    let n = cost.len();
    let mut best = [INF; 12];
    let mut back = vec![[0u8; 12]; n];
    for c in 0..12 {
        if cost[0][c] < INF {
            best[c] = new_sec + cost[0][c];
        }
    }
    for i in 1..n {
        let (mut min_c, mut min_v) = (0usize, INF);
        for c in 0..12 {
            if best[c] < min_v {
                min_v = best[c];
                min_c = c;
            }
        }
        let mut nb_ = [INF; 12];
        for c in 0..12 {
            if cost[i][c] >= INF {
                continue;
            }
            let stay = best[c];
            let switch = min_v + new_sec;
            if stay <= switch {
                nb_[c] = stay + cost[i][c];
                back[i][c] = c as u8;
            } else {
                nb_[c] = switch + cost[i][c];
                back[i][c] = min_c as u8;
            }
        }
        best = nb_;
    }
    let mut cbs = vec![0u8; n];
    let mut c = (0..12).min_by_key(|&c| best[c]).unwrap_or(0);
    for i in (0..n).rev() {
        cbs[i] = c as u8;
        c = back[i][c] as usize;
    }
    cbs
}

/// Run-length encode a group's codebooks into `(codebook, length)` sections, and sum the spectral
/// bits of its non-zero sections. `base` is the index of the group's first band.
fn group_sections(ch: &ChannelInput, q: &[i32], cbs: &[u8], base: usize) -> (Vec<(u8, usize)>, u32) {
    let mut secs: Vec<(u8, usize)> = Vec::new();
    let mut spectral_bits = 0u32;
    for (i, &c) in cbs.iter().enumerate() {
        match secs.last_mut() {
            Some((lc, len)) if *lc == c => *len += 1,
            _ => secs.push((c, 1)),
        }
        if c != ZERO_HCB {
            let b = &ch.bands[base + i];
            spectral_bits += huffman::count_bits(c, &q[b.start..b.end]);
        }
    }
    (secs, spectral_bits)
}

/// Section selection: a codebook per band, the sections of each group and the bits they cost
/// `(codebooks, sections, spectral_bits, section_bits)`.
fn select_sections(ch: &ChannelInput, q: &[i32], maxq: &[i32]) -> (Vec<u8>, Vec<Vec<(u8, usize)>>, u32, u32) {
    let info = &ch.info;
    let (sect_bits, esc) = if info.is_short() { (3u32, 7usize) } else { (5u32, 31usize) };
    let mut cb = vec![ZERO_HCB; ch.bands.len()];
    let mut sections = Vec::with_capacity(info.num_groups);
    let mut spectral_bits = 0u32;
    let mut section_bits = 0u32;
    let n = info.max_sfb;
    for g in 0..info.num_groups {
        let base = g * n;
        if n == 0 {
            sections.push(Vec::new());
            continue;
        }
        let cost = codebook_costs(ch, q, maxq, base, n);
        cb[base..base + n].copy_from_slice(&best_codebooks(&cost, 4 + sect_bits));
        let (secs, bits) = group_sections(ch, q, &cb[base..base + n], base);
        spectral_bits += bits;
        for &(_, len) in &secs {
            section_bits += 4 + sect_bits * (len / esc + 1) as u32;
        }
        sections.push(secs);
    }
    (cb, sections, spectral_bits, section_bits)
}

/// Quantise one channel at rate-distortion multiplier `lambda` and select sections.
pub fn quantise_channel(ch: &ChannelInput, lambda: f32) -> QuantChannel {
    let nb = ch.bands.len();
    let mut sf = vec![-1i32; nb];
    for (i, b) in ch.bands.iter().enumerate() {
        let allowed = lambda * b.thr;
        if b.max34 <= 0.0 || b.en <= allowed {
            continue;
        }
        sf[i] = search_sf(ch, b, allowed);
    }
    // Scalefactor differences between coded bands must stay within ±60: lower (refine) offenders,
    // then quantise; repeat if a band unexpectedly quantised to all zeros (it leaves the chain).
    let mut q = vec![0i32; ch.x.len()];
    let mut maxq = vec![0i32; nb];
    loop {
        limit_sf_steps(&mut sf);
        if !quantise_bands(ch, &mut sf, &mut q, &mut maxq) {
            break;
        }
    }

    let (cb, sections, spectral_bits, section_bits) = select_sections(ch, &q, &maxq);

    // Scalefactors: global gain is the first coded band's value; bands coded with a non-zero
    // codebook but all-zero values reuse the previous value (difference 0).
    let first = (0..nb).find(|&i| sf[i] >= 0).map(|i| sf[i]).unwrap_or(100);
    let mut prev = first;
    let mut sf_bits = 0u32;
    for i in 0..nb {
        if cb[i] == ZERO_HCB {
            continue;
        }
        if sf[i] < 0 {
            sf[i] = prev;
        }
        sf_bits += huffman::sf_bits(sf[i] - prev);
        prev = sf[i];
    }
    let tns_bits = 1 + ch.tns.as_ref().map(|t| t.bit_count(&ch.info)).unwrap_or(0);
    let bits = 8 + section_bits + sf_bits + 1 + tns_bits + 1 + spectral_bits;
    QuantChannel { q, sf, cb, sections, global_gain: first, bits }
}

/// Write an `individual_channel_stream`; `write_info` is false inside a common-window CPE.
pub fn write_ics(bw: &mut BitWriter, ch: &ChannelInput, qc: &QuantChannel, write_info: bool) {
    let info = &ch.info;
    bw.write_bits(qc.global_gain as u32, 8);
    if write_info {
        info.write(bw);
    }
    let (sect_bits, esc) = if info.is_short() { (3u32, 7usize) } else { (5u32, 31usize) };
    for secs in &qc.sections {
        for &(c, len) in secs {
            bw.write_bits(c as u32, 4);
            let mut l = len;
            while l >= esc {
                bw.write_bits(esc as u32, sect_bits);
                l -= esc;
            }
            bw.write_bits(l as u32, sect_bits);
        }
    }
    let mut prev = qc.global_gain;
    for i in 0..ch.bands.len() {
        if qc.cb[i] == ZERO_HCB {
            continue;
        }
        huffman::write_sf(bw, qc.sf[i] - prev);
        prev = qc.sf[i];
    }
    bw.write_bits(0, 1); // pulse_data_present
    match &ch.tns {
        Some(t) => {
            bw.write_bits(1, 1);
            t.write(bw, info);
        }
        None => bw.write_bits(0, 1),
    }
    bw.write_bits(0, 1); // gain_control_data_present
    for (i, b) in ch.bands.iter().enumerate() {
        if qc.cb[i] != ZERO_HCB {
            huffman::write_spectral(bw, qc.cb[i], &qc.q[b.start..b.end]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ics::WindowSequence;
    use crate::mdct::WindowShape;
    use crate::tables::swb_offsets_long;
    use filmcraft_bitstream::BitWriter;

    #[test]
    fn bit_count_matches_written_bits() {
        let swb = swb_offsets_long(4);
        let info = IcsInfo { window_sequence: WindowSequence::OnlyLong, window_shape: WindowShape::Sine, max_sfb: 40, ..Default::default() };
        let spec: Vec<f32> = (0..1024).map(|i| ((i * 7919 % 1000) as f32 - 500.0) * (1000.0 / (i as f32 + 10.0))).collect();
        let thr = vec![vec![1000.0f32; 64]];
        let ch = ChannelInput::new(info, swb, &spec, &thr, None);
        for lambda in [0.01f32, 1.0, 100.0, 1e5] {
            let qc = quantise_channel(&ch, lambda);
            let mut bw = BitWriter::new();
            write_ics(&mut bw, &ch, &qc, false);
            assert_eq!(bw.bit_len() as u32, qc.bits, "lambda {lambda}");
        }
    }
}
