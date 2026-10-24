//! A simple psychoacoustic model and the quantization loops (after ISO/IEC 11172-3 Annex C.1.5.4).
//!
//! The model gives every scalefactor band an allowed noise energy: the band energies spread to
//! their neighbours (10 dB per band upwards, 25 dB per band downwards) lowered by a fixed
//! signal-to-mask offset, but never below the absolute threshold of hearing. The inner loop picks
//! the finest global gain whose Huffman code fits the granule's bit budget; the outer loop
//! amplifies (raises the scalefactors of) the bands whose quantization noise exceeds the allowed
//! noise and keeps the result with the lowest worst noise-to-mask ratio. When every band is under
//! its threshold and the bit reservoir can take the savings, the global gain is coarsened again
//! as far as the thresholds allow.

use filmcraft_bitstream::BitWriter;

use crate::filterbank::GRANULE;
use crate::huffman::{self, Coding};
use crate::tables::{LSF_PARTITIONS, MAX_QUANT, SLEN, pow43};

/// Outer-loop iterations per granule (each raises at least one scalefactor).
const MAX_OUTER: usize = 24;
/// Signal-to-mask offset of the spread band energies, dB.
const MASK_OFFSET_DB: f32 = 12.0;

/// The fixed per-stream parameters of the model and the loops.
pub(crate) struct Model {
    pub lsf: bool,
    pub bands: [u16; 23],
    /// Allowed noise energy from the threshold in quiet, per band.
    pub ath: [f32; 22],
    /// Lines at and above this are not coded (the low-pass).
    pub cutoff: usize,
    /// `spread[b][j]`: how much of band `j`'s energy masks band `b`.
    spread: [[f32; 22]; 22],
}

/// One coded granule of one channel.
#[derive(Clone)]
pub(crate) struct Granule {
    pub ix: [u16; GRANULE],
    pub negative: [bool; GRANULE],
    pub global_gain: u32,
    pub scalefac: [u8; 22],
    pub scalefac_scale: bool,
    pub scalefac_compress: u32,
    /// Bits per scalefactor of each band group (MPEG-1: bands 0-10, 11-20; MPEG-2: four partitions).
    pub slen: [u32; 4],
    pub part2_bits: usize,
    pub coding: Coding,
}

impl Granule {
    fn silent() -> Granule {
        Granule {
            ix: [0; GRANULE],
            negative: [false; GRANULE],
            global_gain: 210,
            scalefac: [0; 22],
            scalefac_scale: false,
            scalefac_compress: 0,
            slen: [0; 4],
            part2_bits: 0,
            coding: Coding::default(),
        }
    }

    /// `part2_3_length`: scalefactor and Huffman bits.
    pub fn bits(&self) -> usize {
        self.part2_bits + self.coding.bits
    }

    /// Write the scalefactors (no scfsi reuse) and the Huffman code.
    pub fn write(&self, w: &mut BitWriter, model: &Model) {
        let groups: [(usize, usize, u32); 4] = if model.lsf {
            let mut start = 0;
            let mut g = [(0, 0, 0); 4];
            for (p, (&n, gi)) in LSF_PARTITIONS.iter().zip(g.iter_mut()).enumerate() {
                *gi = (start, start + n, self.slen[p]);
                start += n;
            }
            g
        } else {
            [(0, 11, self.slen[0]), (11, 21, self.slen[1]), (21, 21, 0), (21, 21, 0)]
        };
        for (a, b, bits) in groups {
            for &sf in self.scalefac.get(a..b).unwrap_or(&[]) {
                w.write_bits(u32::from(sf), bits);
            }
        }
        huffman::write(w, &self.ix, &self.negative, &self.coding, &model.bands);
    }
}

/// Bits needed for `v` (0 for 0).
fn bit_width(v: u32) -> u32 {
    32 - v.leading_zeros()
}

impl Model {
    pub fn new(lsf: bool, bands: [u16; 23], ath: [f32; 22], cutoff: usize) -> Model {
        let mut spread = [[0f32; 22]; 22];
        for (b, row) in spread.iter_mut().enumerate() {
            for (j, s) in row.iter_mut().enumerate() {
                let db = if b >= j { (b - j) as f32 * 10.0 } else { (j - b) as f32 * 25.0 };
                *s = 10f32.powf(-(db + MASK_OFFSET_DB) / 10.0);
            }
        }
        Model { lsf, bands, ath, cutoff: cutoff.min(GRANULE), spread }
    }

    fn band(&self, b: usize) -> std::ops::Range<usize> {
        let lo = self.bands.get(b).map_or(GRANULE, |&v| usize::from(v));
        let hi = self.bands.get(b + 1).map_or(GRANULE, |&v| usize::from(v));
        lo..hi.max(lo)
    }

    /// The largest scalefactor of band `b` the side information can carry.
    fn sf_limit(&self, b: usize) -> u8 {
        // MPEG-1 slen1 / MPEG-2 slen1-2 reach 4 bits, slen2 / slen3-4 3 bits; band 21 has none
        match b {
            0..=10 => 15,
            11..=20 => 7,
            _ => 0,
        }
    }

    /// Allowed noise energy per band for the lines `xr` (zero above the cutoff).
    pub fn thresholds(&self, xr: &[f32; GRANULE]) -> [f32; 22] {
        let mut en = [0f32; 22];
        for (b, e) in en.iter_mut().enumerate() {
            *e = xr.get(self.band(b)).unwrap_or(&[]).iter().map(|v| v * v).sum();
        }
        let mut xmin = [0f32; 22];
        for (b, x) in xmin.iter_mut().enumerate() {
            let r = self.band(b);
            if r.start >= self.cutoff {
                *x = f32::MAX;
                continue;
            }
            let mask: f32 = self.spread[b].iter().zip(&en).map(|(s, e)| s * e).sum();
            *x = mask.max(self.ath[b]);
        }
        xmin
    }

    /// Scalefactor bits and `scalefac_compress` (and the slen per group) for `sf`.
    fn part2(&self, sf: &[u8; 22]) -> (usize, u32, [u32; 4]) {
        let max_in = |a: usize, b: usize| bit_width(u32::from(sf.get(a..b).and_then(|s| s.iter().copied().max()).unwrap_or(0)));
        if self.lsf {
            let mut slen = [0u32; 4];
            let mut start = 0;
            let mut bits = 0usize;
            for (p, &n) in LSF_PARTITIONS.iter().enumerate() {
                slen[p] = max_in(start, start + n);
                bits += n * slen[p] as usize;
                start += n;
            }
            // scalefac_compress < 400: slen1, slen2 ≤ 4; slen3, slen4 ≤ 3 (13818-3 §2.4.3.2)
            let sfc = ((slen[0] * 5 + slen[1]) << 4) + (slen[2] << 2) + slen[3];
            (bits, sfc, slen)
        } else {
            let (n1, n2) = (max_in(0, 11), max_in(11, 21));
            let mut best = (usize::MAX, 15u32, [4, 3, 0, 0]);
            for (i, &(s1, s2)) in SLEN.iter().enumerate() {
                let bits = 11 * s1 as usize + 10 * s2 as usize;
                if s1 >= n1 && s2 >= n2 && bits < best.0 {
                    best = (bits, i as u32, [s1, s2, 0, 0]);
                }
            }
            best
        }
    }

    /// Code one granule's lines `xr` with at most `max_bits` and, when the thresholds `xmin` are
    /// met, preferably no fewer than `min_bits`.
    pub fn encode(&self, xr: &[f32; GRANULE], xmin: &[f32; 22], max_bits: usize, min_bits: usize) -> Granule {
        let mut q = Quantizer { model: self, xr, xr34: [0.0; GRANULE] };
        let mut peak = 0f32;
        for (o, v) in q.xr34.iter_mut().zip(xr.iter()).take(self.cutoff) {
            let a = v.abs();
            *o = a.sqrt() * a.sqrt().sqrt();
            peak = peak.max(a);
        }
        let mut silent = Granule::silent();
        if peak <= 1e-9 || max_bits == 0 {
            return silent;
        }
        silent.negative.iter_mut().zip(xr.iter()).for_each(|(n, v)| *n = *v < 0.0);
        let mut sf = [0u8; 22];
        let mut scale = false;
        let mut hint: Option<u32> = None;
        let mut best: Option<(f32, Granule)> = None;
        for _ in 0..MAX_OUTER {
            let (p2, sfc, slen) = self.part2(&sf);
            if p2 >= max_bits {
                break;
            }
            let Some((gg, ix, coding)) = q.inner(&sf, scale, max_bits - p2, hint) else { break };
            hint = Some(gg);
            let nmr = q.nmr(&ix, gg, &sf, scale, xmin);
            let worst = nmr.iter().copied().fold(0f32, f32::max);
            if best.as_ref().is_none_or(|b| worst < b.0) {
                let mut g = silent.clone();
                g.ix = ix;
                g.global_gain = gg;
                g.scalefac = sf;
                g.scalefac_scale = scale;
                g.scalefac_compress = sfc;
                g.slen = slen;
                g.part2_bits = p2;
                g.coding = coding;
                best = Some((worst, g));
            }
            // amplify the bands over their threshold
            let over: Vec<usize> = (0..21).filter(|&b| nmr[b] > 1.0).collect();
            if over.is_empty() || over.len() == 21 {
                break;
            }
            for &b in &over {
                sf[b] = sf[b].saturating_add(1);
            }
            if (0..21).any(|b| sf[b] > self.sf_limit(b)) {
                if scale {
                    break;
                }
                scale = true;
                sf.iter_mut().for_each(|s| *s = s.div_ceil(2));
            }
        }
        let Some((worst, mut g)) = best else { return silent };
        if worst <= 1.0 && g.bits() > min_bits {
            q.coarsen(&mut g, xmin, min_bits);
        }
        g
    }
}

struct Quantizer<'a> {
    model: &'a Model,
    xr: &'a [f32; GRANULE],
    /// `|xr|^(3/4)`.
    xr34: [f32; GRANULE],
}

impl Quantizer<'_> {
    /// Quantize with global gain `gg` and the scalefactors; `None` when a line exceeds the
    /// largest escape value.
    fn quantize(&self, gg: u32, sf: &[u8; 22], scale: bool) -> Option<[u16; GRANULE]> {
        let mut ix = [0u16; GRANULE];
        let sfm = if scale { 1.0 } else { 0.5 };
        for b in 0..22 {
            let r = self.model.band(b);
            let e = -0.1875 * (gg as f32 - 210.0) + 0.75 * sfm * f32::from(sf[b]);
            let f = 2f32.powf(e);
            for i in r.start..r.end.min(self.model.cutoff) {
                let v = self.xr34[i] * f + 0.4054;
                if v.is_nan() || v >= (MAX_QUANT + 1) as f32 {
                    return None;
                }
                ix[i] = v as u16;
            }
        }
        Some(ix)
    }

    /// Noise-to-mask ratio per band.
    fn nmr(&self, ix: &[u16; GRANULE], gg: u32, sf: &[u8; 22], scale: bool, xmin: &[f32; 22]) -> [f32; 22] {
        let p43 = pow43();
        let sfm = if scale { 1.0 } else { 0.5 };
        let mut out = [0f32; 22];
        for (b, o) in out.iter_mut().enumerate() {
            let r = self.model.band(b);
            let step = 2f32.powf(0.25 * (gg as f32 - 210.0) - sfm * f32::from(sf[b]));
            let mut noise = 0f32;
            for i in r.start..r.end.min(self.model.cutoff) {
                let rec = p43.get(usize::from(ix[i])).copied().unwrap_or(0.0) * step;
                let d = self.xr[i].abs() - rec;
                noise += d * d;
            }
            *o = if xmin[b] > 0.0 {
                noise / xmin[b]
            } else if noise > 0.0 {
                f32::MAX
            } else {
                0.0
            };
        }
        out
    }

    /// The code of `gg` if it fits `budget` bits.
    fn fits(&self, gg: u32, sf: &[u8; 22], scale: bool, budget: usize) -> Option<([u16; GRANULE], Coding)> {
        let ix = self.quantize(gg, sf, scale)?;
        let c = huffman::choose(&ix, &self.model.bands);
        (c.bits <= budget).then_some((ix, c))
    }

    /// The inner loop: the smallest global gain whose code fits `budget` bits.
    fn inner(&self, sf: &[u8; 22], scale: bool, budget: usize, hint: Option<u32>) -> Option<(u32, [u16; GRANULE], Coding)> {
        match hint {
            None => {
                let mut hi = 255u32;
                let mut found = self.fits(hi, sf, scale, budget)?;
                let mut lo = 0u32;
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    match self.fits(mid, sf, scale, budget) {
                        Some(r) => {
                            hi = mid;
                            found = r;
                        }
                        None => lo = mid + 1,
                    }
                }
                Some((hi, found.0, found.1))
            }
            Some(start) => {
                let mut g = start.min(255);
                if let Some(mut found) = self.fits(g, sf, scale, budget) {
                    while g > 0 {
                        match self.fits(g - 1, sf, scale, budget) {
                            Some(r) => {
                                g -= 1;
                                found = r;
                            }
                            None => break,
                        }
                    }
                    Some((g, found.0, found.1))
                } else {
                    while g < 255 {
                        g += 1;
                        if let Some(r) = self.fits(g, sf, scale, budget) {
                            return Some((g, r.0, r.1));
                        }
                    }
                    None
                }
            }
        }
    }

    /// Raise the global gain of `g` as far as every band stays under its threshold and the code
    /// keeps at least `min_bits`.
    fn coarsen(&self, g: &mut Granule, xmin: &[f32; 22], min_bits: usize) {
        let ok = |gg: u32| -> Option<([u16; GRANULE], Coding)> {
            let ix = self.quantize(gg, &g.scalefac, g.scalefac_scale)?;
            let c = huffman::choose(&ix, &self.model.bands);
            let nmr = self.nmr(&ix, gg, &g.scalefac, g.scalefac_scale, xmin);
            (nmr.iter().all(|&v| v <= 1.0) && g.part2_bits + c.bits >= min_bits).then_some((ix, c))
        };
        let (mut lo, mut hi) = (g.global_gain, 255u32);
        let mut found = None;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            match ok(mid) {
                Some(r) => {
                    lo = mid;
                    found = Some(r);
                }
                None => hi = mid - 1,
            }
        }
        if let Some((ix, c)) = found
            && lo > g.global_gain
        {
            g.global_gain = lo;
            g.ix = ix;
            g.coding = c;
        }
    }
}
