//! Scalar VP9 unscaled inter prediction (8.5.2.4 with a step of 16): the reference for the vector
//! versions, and the version other targets run. Block widths are const parameters so that each
//! row's loops vectorize.

use crate::Vp9Out;

/// See [`crate::vp9_predict`]; the caller checked the arguments, the window and the block fit.
/// `wrap`: 8-bit samples and taps for which [`narrow8`] is exact.
pub(crate) fn predict(
    (src, ss): (&[u16], usize),
    (fh, fv): (Option<&[i16; 8]>, Option<&[i16; 8]>),
    (w, h): (usize, usize),
    max: u16,
    wrap: bool,
    (dst, ds): (&mut [u16], usize),
    out: Vp9Out,
) {
    match (w, out) {
        (4, Vp9Out::Put) => predict_w::<4, false>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
        (8, Vp9Out::Put) => predict_w::<8, false>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
        (16, Vp9Out::Put) => predict_w::<16, false>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
        (32, Vp9Out::Put) => predict_w::<32, false>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
        (_, Vp9Out::Put) => predict_w::<64, false>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
        (4, Vp9Out::Avg) => predict_w::<4, true>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
        (8, Vp9Out::Avg) => predict_w::<8, true>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
        (16, Vp9Out::Avg) => predict_w::<16, true>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
        (32, Vp9Out::Avg) => predict_w::<32, true>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
        (_, Vp9Out::Avg) => predict_w::<64, true>((src, ss), (fh, fv), h, max, wrap, (dst, ds)),
    }
}

/// The prediction of a block `W` wide (`AVG`: averaged into `dst`).
fn predict_w<const W: usize, const AVG: bool>(
    (src, ss): (&[u16], usize),
    (fh, fv): (Option<&[i16; 8]>, Option<&[i16; 8]>),
    h: usize,
    max: u16,
    wrap: bool,
    (dst, ds): (&mut [u16], usize),
) {
    let mut p = [0u16; W];
    match (fh, fv) {
        (None, None) => {
            for i in 0..h {
                let Some(s) = src.get((i + 3) * ss + 3..).and_then(|s| s.first_chunk::<W>()) else { return };
                store::<W, AVG>(dst, i * ds, s);
            }
        }
        (Some(f), None) => {
            for i in 0..h {
                filter::<W>(src, (i + 3) * ss, 1, f, max, wrap, &mut p);
                store::<W, AVG>(dst, i * ds, &p);
            }
        }
        (None, Some(f)) => {
            for i in 0..h {
                filter::<W>(src, i * ss + 3, ss, f, max, wrap, &mut p);
                store::<W, AVG>(dst, i * ds, &p);
            }
        }
        (Some(f), Some(g)) => {
            let mut tmp = [0u16; 71 * 64];
            for i in 0..h + 7 {
                if let Some(t) = tmp.get_mut(i * W..).and_then(|t| t.first_chunk_mut::<W>()) {
                    filter::<W>(src, i * ss, 1, f, max, wrap, t);
                }
            }
            for i in 0..h {
                filter::<W>(&tmp, i * W, W, g, max, wrap, &mut p);
                store::<W, AVG>(dst, i * ds, &p);
            }
        }
    }
}

/// `o[j]` = Round2(sum over t of f[t] * s[o0 + j + t * step], 7) clipped to 0..=max: a
/// horizontal (`step` 1) or vertical (`step` the stride) 8-tap filter of one row. The taps are
/// the outer loop so that the row vectorizes.
#[inline(always)]
fn filter<const W: usize>(s: &[u16], o0: usize, step: usize, f: &[i16; 8], max: u16, wrap: bool, o: &mut [u16; W]) {
    if wrap {
        let mut acc = [64u16; W];
        for t in 0..8 {
            let c = f[t] as u16;
            let Some(p) = s.get(o0 + t * step..).and_then(|s| s.first_chunk::<W>()) else { return };
            for j in 0..W {
                acc[j] = acc[j].wrapping_add(c.wrapping_mul(p[j]));
            }
        }
        narrow8(&acc, o);
        return;
    }
    let mut acc = [64i32; W];
    for t in 0..8 {
        let c = f[t] as i32;
        let Some(p) = s.get(o0 + t * step..).and_then(|s| s.first_chunk::<W>()) else { return };
        for j in 0..W {
            acc[j] += c * p[j] as i32;
        }
    }
    for j in 0..W {
        o[j] = (acc[j] >> 7).clamp(0, max as i32) as u16;
    }
}

/// Round2(sum, 7) clipped to 8 bits, from 8-bit filter sums kept modulo 2^16 (8 lanes per
/// vector instead of 4). With 8-bit samples and taps whose positive ones add up to at most 192
/// and negative ones to at least -64 ([`crate::vp9_wraps`]; VP9's filters: 182 and -54), every sum
/// plus the rounding term 64 lies in [-16256, 49024]: wrapped, the non-negative ones stay below
/// 0xC000 and the negative ones (which clip to 0) land at or above it.
#[inline(always)]
fn narrow8<const W: usize>(acc: &[u16; W], o: &mut [u16; W]) {
    for j in 0..W {
        o[j] = if acc[j] < 0xC000 { (acc[j] >> 7).min(255) } else { 0 };
    }
}

#[inline(always)]
fn store<const W: usize, const AVG: bool>(dst: &mut [u16], o: usize, p: &[u16; W]) {
    let Some(d) = dst.get_mut(o..).and_then(|d| d.first_chunk_mut::<W>()) else { return };
    if AVG {
        for j in 0..W {
            d[j] = ((d[j] as u32 + p[j] as u32 + 1) >> 1) as u16;
        }
    } else {
        *d = *p;
    }
}
