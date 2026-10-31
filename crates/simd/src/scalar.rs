//! The scalar kernels: the reference every vector version must match, and the code wasm and
//! other targets run.

use crate::deblock::{LfSize, LfThresh};
use crate::{CDEF_COST_WEIGHTS, CDEF_STRIDE, CdefTap, cdef_pick};

/// Each line sum takes whole rows added at an offset; integer sums don't depend on the order.
pub(crate) fn cdef_direction(rows: &[&[u16; 8]; 8], shift: u32) -> (usize, i32) {
    fn add<const N: usize>(dst: &mut [i32], src: &[i32; N]) {
        for (d, s) in dst.iter_mut().zip(src) {
            *d += s;
        }
    }
    let mut partial = [[0i32; 16]; 8];
    for (i, row) in rows.iter().enumerate() {
        let x: [i32; 8] = std::array::from_fn(|j| (row[j] >> shift) as i32 - 128);
        // Pairs of neighbouring samples, for the directions that step two columns per row.
        let pairs: [i32; 4] = std::array::from_fn(|k| x[2 * k] + x[2 * k + 1]);
        let rev: [i32; 8] = std::array::from_fn(|m| x[7 - m]);
        let rev_pairs: [i32; 4] = std::array::from_fn(|m| pairs[3 - m]);
        add(&mut partial[0][i..], &x);
        add(&mut partial[1][i..], &pairs);
        partial[2][i] = x.iter().sum();
        add(&mut partial[3][i..], &rev_pairs);
        add(&mut partial[4][i..], &rev);
        add(&mut partial[5][3 - i / 2..], &x);
        add(&mut partial[6][..], &x);
        add(&mut partial[7][i / 2..], &x);
    }
    cdef_pick(&std::array::from_fn(|d| partial[d].iter().zip(&CDEF_COST_WEIGHTS[d]).map(|(&p, &w)| p * p * w).sum()))
}

/// Sample filtering process (AV1 7.14.6, VP9 8.8.5) for one line across a loop filter edge:
/// `base` indexes q0 and `step` is the distance between samples across the edge.
#[inline(always)]
pub(crate) fn deblock_line(d: &mut [u16], base: usize, step: usize, size: LfSize, t: &LfThresh) {
    // p(reach - 1)..q(reach - 1) must all be in `d`; otherwise the line is left as it is.
    let reach = size.reach();
    let fits = reach.checked_mul(step).is_some_and(|b| b <= base)
        && (reach - 1).checked_mul(step).and_then(|a| a.checked_add(base)).is_some_and(|last| last < d.len());
    if !fits {
        return;
    }
    let at = |d: &[u16], k: isize| -> i32 { d[(base as isize + k * step as isize) as usize] as i32 };
    let (q0, q1, p0, p1) = (at(d, 0), at(d, 1), at(d, -1), at(d, -2));
    let lim = t.limit;
    // filter mask (7.14.6.2)
    if (p1 - p0).abs() > lim || (q1 - q0).abs() > lim || (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 > t.blimit {
        return;
    }
    let hev = (p1 - p0).abs() > t.thresh || (q1 - q0).abs() > t.thresh;
    let mut flat = false;
    if size != LfSize::Four {
        let (q2, p2) = (at(d, 2), at(d, -3));
        if (p2 - p1).abs() > lim || (q2 - q1).abs() > lim {
            return;
        }
        let tf = t.flat;
        flat = (p1 - p0).abs() <= tf && (q1 - q0).abs() <= tf && (p2 - p0).abs() <= tf && (q2 - q0).abs() <= tf;
        if size != LfSize::Six {
            // filterLen >= 8
            let (q3, p3) = (at(d, 3), at(d, -4));
            if (p3 - p2).abs() > lim || (q3 - q2).abs() > lim {
                return;
            }
            flat = flat && (p3 - p0).abs() <= tf && (q3 - q0).abs() <= tf && size != LfSize::Vp9Four;
        }
    }
    if !flat {
        // narrow filter (7.14.6.3)
        let c = |v: i32| v.clamp(-t.half, t.half - 1);
        let off = t.off;
        let (ps1, ps0, qs0, qs1) = (p1 - off, p0 - off, q0 - off, q1 - off);
        let mut filter = if hev { c(ps1 - qs1) } else { 0 };
        filter = c(filter + 3 * (qs0 - ps0));
        let filter1 = c(filter + 4) >> 3;
        let filter2 = c(filter + 3) >> 3;
        d[base] = (c(qs0 - filter1) + off) as u16;
        d[base - step] = (c(ps0 + filter2) + off) as u16;
        if !hev {
            let f = (filter1 + 1) >> 1;
            d[base + step] = (c(qs1 - f) + off) as u16;
            d[base - 2 * step] = (c(ps1 + f) + off) as u16;
        }
        return;
    }
    match size {
        LfSize::Four | LfSize::Vp9Four => {}
        LfSize::Six => wide::<2, 1, 3>(d, base, step),
        LfSize::Eight | LfSize::Vp9Eight => wide::<3, 0, 3>(d, base, step),
        LfSize::Sixteen => {
            let tf = t.flat;
            let flat2 = (4..7).all(|k| (at(d, k) - q0).abs() <= tf && (at(d, -k - 1) - p0).abs() <= tf);
            if flat2 { wide::<6, 1, 4>(d, base, step) } else { wide::<3, 0, 3>(d, base, step) }
        }
        LfSize::Vp9Sixteen => {
            let tf = t.flat;
            let flat2 = (4..8).all(|k| (at(d, k) - q0).abs() <= tf && (at(d, -k - 1) - p0).abs() <= tf);
            if flat2 { wide::<7, 0, 4>(d, base, step) } else { wide::<3, 0, 3>(d, base, step) }
        }
    }
}

/// Wide filter process (AV1 7.14.6.4, VP9 8.8.5.3) with n = N, the 2-weight taps |j| <= N2 and
/// log2Size = LOG2.
#[inline(always)]
fn wide<const N: usize, const N2: usize, const LOG2: u32>(d: &mut [u16], base: usize, step: usize) {
    // v[k] = sample at offset k - (N + 1) from q0, k in 0 .. 2N + 2
    let mut v = [0i32; 16];
    let first = base - (N + 1) * step;
    for (k, x) in v[..2 * N + 2].iter_mut().enumerate() {
        *x = d[first + k * step] as i32;
    }
    // e[m] = v[clamp(m - N, 0, 2N + 1)]: the clamped tap positions as one array, so
    // F[i] = sum_{jj = 0}^{2N} e[i + 1 + jj] * (2 if |jj - N| <= N2 else 1), a sliding window.
    let mut e = [0i32; 29];
    for (m, x) in e[..4 * N + 1].iter_mut().enumerate() {
        *x = v[m.saturating_sub(N).min(2 * N + 1)];
    }
    let mut f = [0i32; 14];
    let mut win: i32 = e[1..2 * N + 2].iter().sum();
    for i in 0..2 * N {
        if i > 0 {
            win += e[i + 1 + 2 * N] - e[i];
        }
        let centre: i32 = e[i + 1 + N - N2..=i + 1 + N + N2].iter().sum();
        f[i] = (win + centre + (1 << (LOG2 - 1))) >> LOG2;
    }
    let out = base - N * step;
    for i in 0..2 * N {
        d[out + i * step] = f[i] as u16;
    }
}

/// One row of the AV1 subpel filter's horizontal pass (7.11.3.4).
pub(crate) fn subpel_h(src: &[u16], taps: &[i16; 8], out: &mut [i16], shift: u32) {
    let half = (1 << shift) >> 1;
    for (o, s) in out.iter_mut().zip(src.windows(8)) {
        let v: i32 = s.iter().zip(taps).map(|(&p, &t)| p as i32 * t as i32).sum();
        *o = ((v + half) >> shift) as i16;
    }
}

/// One row of the AV1 subpel filter's vertical pass (7.11.3.4).
pub(crate) fn subpel_v(rows: &[i16], stride: usize, taps: &[i16; 8], out: &mut [i32], shift: u32) {
    let half = (1 << shift) >> 1;
    for (j, o) in out.iter_mut().enumerate() {
        let v: i32 = taps.iter().enumerate().map(|(t, &f)| f as i32 * rows.get(t * stride + j).copied().unwrap_or(0) as i32).sum();
        *o = (v + half) >> shift;
    }
}

pub(crate) fn cdef_filter(buf: &[u16; CDEF_STRIDE * CDEF_STRIDE], w: usize, h: usize, taps: &[CdefTap], clamp: bool, out: &mut [u16; 64]) {
    for (i, row) in out.chunks_exact_mut(w.max(1)).take(h).enumerate() {
        for (j, o) in row.iter_mut().enumerate() {
            let ci = (i + 2) * CDEF_STRIDE + j + 2;
            let Some(&x) = buf.get(ci) else {
                return;
            };
            let x = x as i32;
            let (mut sum, mut max, mut min) = (0, x, x);
            for t in taps {
                let p = ci.checked_add_signed(t.off).and_then(|k| buf.get(k)).map_or(x, |&p| p as i32);
                let (d, strength) = (p - x, t.strength as i32);
                let mag = (strength - (d.abs() >> t.shift.min(31))).max(0).min(d.abs());
                sum += t.weight as i32 * if d < 0 { -mag } else { mag };
                (max, min) = (max.max(p), min.min(p));
            }
            let v = x + ((8 + sum - (sum < 0) as i32) >> 4);
            *o = if clamp { v.max(min).min(max) } else { v } as u16;
        }
    }
}

/// See [`crate::av1_rows_to_lanes`].
pub(crate) fn av1_rows_to_lanes(src: &[i32], stride: usize, cols: usize, out: &mut [[i16; 8]]) -> bool {
    let src_ok = stride >= cols && 7usize.checked_mul(stride).and_then(|n| n.checked_add(cols)).is_some_and(|n| n <= src.len());
    if !cols.is_multiple_of(8) || out.len() < cols || !src_ok {
        return false;
    }
    for (j, o) in out[..cols].iter_mut().enumerate() {
        for (l, v) in o.iter_mut().enumerate() {
            *v = src[l * stride + j] as i16;
        }
    }
    true
}

/// See [`crate::av1_lanes_to_rows`].
pub(crate) fn av1_lanes_to_rows(t: &[[i16; 8]], cols: usize, shift: u32, dst: &mut [i32], stride: usize) -> bool {
    let dst_ok = stride >= cols && 7usize.checked_mul(stride).and_then(|n| n.checked_add(cols)).is_some_and(|n| n <= dst.len());
    if !cols.is_multiple_of(8) || t.len() < cols || !dst_ok || shift > 16 {
        return false;
    }
    for (j, lanes) in t[..cols].iter().enumerate() {
        for (l, &v) in lanes.iter().enumerate() {
            dst[l * stride + j] = (v as i32 + (1 << shift >> 1)) >> shift;
        }
    }
    true
}
