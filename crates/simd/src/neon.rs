//! NEON kernels (every aarch64 core). The arithmetic intrinsics are safe to call here because
//! the kernels are compiled with `neon` enabled; entering a kernel and loading or storing
//! through pointers need `unsafe`.

use core::arch::aarch64::*;

use crate::{CDEF_COST_WEIGHTS, CDEF_STRIDE, CdefTap, cdef_pick};

/// [`CDEF_COST_WEIGHTS`] as vectors, cast once when the crate is compiled.
static CDEF_WEIGHT_VECTORS: [[int32x4_t; 4]; 8] = bytemuck::must_cast(CDEF_COST_WEIGHTS);

/// 16-bit lanes; each line sum is two vectors (lanes 0..8 and 8..16), and a row is added at its
/// offset with `ext`, a fixed shift of lanes across two registers.
pub(crate) fn cdef_direction(rows: &[&[u16; 8]; 8], shift: u32) -> (usize, i32) {
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { cdef_direction_neon(rows, shift) }
}

#[target_feature(enable = "neon")]
fn cdef_direction_neon(rows: &[&[u16; 8]; 8], shift: u32) -> (usize, i32) {
    let z = vdupq_n_s16(0);
    let down = vdupq_n_s16(-(shift as i16));
    let centre = vdupq_n_s16(128);
    let load = |r: &[u16; 8]| -> int16x8_t {
        let v: uint16x8_t = bytemuck::must_cast(*r);
        vsubq_s16(vreinterpretq_s16_u16(vshlq_u16(v, down)), centre)
    };
    // Add `v` to the line sum (lo, hi) starting at lane `o`.
    macro_rules! at {
        ($lo:ident, $hi:ident, $v:expr, 0) => {{
            $lo = vaddq_s16($lo, $v);
        }};
        ($lo:ident, $hi:ident, $v:expr, $o:tt) => {{
            $lo = vaddq_s16($lo, vextq_s16::<{ 8 - $o }>(z, $v));
            $hi = vaddq_s16($hi, vextq_s16::<{ 8 - $o }>($v, z));
        }};
    }
    let (mut l0, mut h0, mut l1, mut h1, mut l3, mut h3, mut l4, mut h4) = (z, z, z, z, z, z, z, z);
    let (mut l5, mut h5, mut l7, mut h7, mut l6, mut l2) = (z, z, z, z, z, z);
    // Row i: directions 0, 1, 3 and 4 at offset i, 5 at 3 - i / 2, 7 at i / 2.
    macro_rules! row {
        ($i:tt, $o5:tt, $o7:tt) => {{
            let x = load(rows[$i]);
            let pairs = vpaddq_s16(x, z); // x0+x1, x2+x3, x4+x5, x6+x7, then zeros
            let rev_pairs = vrev64q_s16(pairs); // the four pairs reversed, then zeros
            let r = vrev64q_s16(x);
            let rev = vextq_s16::<4>(r, r); // x7 .. x0
            at!(l0, h0, x, $i);
            at!(l1, h1, pairs, $i);
            at!(l3, h3, rev_pairs, $i);
            at!(l4, h4, rev, $i);
            at!(l5, h5, x, $o5);
            at!(l7, h7, x, $o7);
            l6 = vaddq_s16(l6, x);
            l2 = vsetq_lane_s16::<$i>(vaddvq_s16(x), l2);
        }};
    }
    row!(0, 3, 0);
    row!(1, 3, 0);
    row!(2, 2, 1);
    row!(3, 2, 1);
    row!(4, 1, 2);
    row!(5, 1, 2);
    row!(6, 0, 3);
    row!(7, 0, 3);
    let lines = [(l0, h0), (l1, h1), (l2, z), (l3, h3), (l4, h4), (l5, h5), (l6, z), (l7, h7)];
    let cost: [i32; 8] = std::array::from_fn(|d| {
        let (lo, hi) = lines[d];
        let [w0, w1, w2, w3] = CDEF_WEIGHT_VECTORS[d];
        // Squares widened to 32 bits, times the lane weights, summed.
        let mut s = vmulq_s32(vmull_s16(vget_low_s16(lo), vget_low_s16(lo)), w0);
        s = vmlaq_s32(s, vmull_high_s16(lo, lo), w1);
        s = vmlaq_s32(s, vmull_s16(vget_low_s16(hi), vget_low_s16(hi)), w2);
        s = vmlaq_s32(s, vmull_high_s16(hi, hi), w3);
        vaddvq_s32(s)
    });
    cdef_pick(&cost)
}

// ---------------------------------------------------------------------------------------------
// Loop filter

/// 8 lanes of i16; the deblocking kernels use the first four.
type V = int16x8_t;

/// `n` (4 or 8) lines across a horizontal edge (see [`crate::deblock_horizontal`]); `false`
/// when the samples aren't all in `d`.
pub(crate) fn deblock_horizontal(d: &mut [u16], base: usize, stride: usize, n: usize, on: u8, size: LfSize, t: &LfThresh) -> bool {
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { horizontal(d, base, stride, n, on, size, t) }
}

/// `n` (4 or 8) lines across a vertical edge (see [`crate::deblock_vertical`]); `false` when
/// the samples aren't all in `d`.
pub(crate) fn deblock_vertical(d: &mut [u16], base: usize, stride: usize, n: usize, on: u8, size: LfSize, t: &LfThresh) -> bool {
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { vertical(d, base, stride, n, on, size, t) }
}

crate::deblock::deblock_kernels!("neon");

#[target_feature(enable = "neon")]
fn splat(v: i32) -> V {
    vdupq_n_s16(v as i16)
}
#[target_feature(enable = "neon")]
fn add(a: V, b: V) -> V {
    vaddq_s16(a, b)
}
#[target_feature(enable = "neon")]
fn sub(a: V, b: V) -> V {
    vsubq_s16(a, b)
}
#[target_feature(enable = "neon")]
fn abd(a: V, b: V) -> V {
    vabdq_s16(a, b)
}
#[target_feature(enable = "neon")]
fn gt(a: V, b: V) -> V {
    vreinterpretq_s16_u16(vcgtq_s16(a, b))
}
#[target_feature(enable = "neon")]
fn and(a: V, b: V) -> V {
    vandq_s16(a, b)
}
#[target_feature(enable = "neon")]
fn or(a: V, b: V) -> V {
    vorrq_s16(a, b)
}
#[target_feature(enable = "neon")]
fn andnot(a: V, b: V) -> V {
    vbicq_s16(b, a)
}
#[target_feature(enable = "neon")]
fn sel(m: V, a: V, b: V) -> V {
    vbslq_s16(vreinterpretq_u16_s16(m), a, b)
}
#[target_feature(enable = "neon")]
fn min(a: V, b: V) -> V {
    vminq_s16(a, b)
}
#[target_feature(enable = "neon")]
fn max(a: V, b: V) -> V {
    vmaxq_s16(a, b)
}
#[target_feature(enable = "neon")]
fn sra<const N: i32>(a: V) -> V {
    vshrq_n_s16::<N>(a)
}
#[target_feature(enable = "neon")]
fn srl<const N: i32>(a: V) -> V {
    vreinterpretq_s16_u16(vshrq_n_u16::<N>(vreinterpretq_u16_s16(a)))
}
#[target_feature(enable = "neon")]
fn any(m: V) -> bool {
    vmaxvq_u16(vreinterpretq_u16_s16(m)) != 0
}
#[target_feature(enable = "neon")]
fn lanes(on: u8) -> V {
    const BITS: [i16; 8] = [1, 2, 4, 8, 16, 32, 64, 128];
    let bits: int16x8_t = bytemuck::must_cast(BITS);
    vreinterpretq_s16_u16(vtstq_s16(vdupq_n_s16(on as i16), bits))
}
#[target_feature(enable = "neon")]
fn load4(s: &[u16; 4]) -> V {
    let v: uint16x4_t = bytemuck::must_cast(*s);
    vreinterpretq_s16_u16(vcombine_u16(v, vdup_n_u16(0)))
}
#[target_feature(enable = "neon")]
fn store4(v: V, s: &mut [u16; 4]) {
    *s = bytemuck::must_cast(vget_low_u16(vreinterpretq_u16_s16(v)));
}
#[target_feature(enable = "neon")]
fn load8(s: &[u16; 8]) -> V {
    vreinterpretq_s16_u16(bytemuck::must_cast(*s))
}
#[target_feature(enable = "neon")]
fn store8(v: V, s: &mut [u16; 8]) {
    *s = bytemuck::must_cast(vreinterpretq_u16_s16(v));
}
/// 8 rows of 8 to 8 columns of 8 (and back: it is its own inverse).
#[target_feature(enable = "neon")]
fn transpose(r: [V; 8]) -> [V; 8] {
    let (w, h) = (vreinterpretq_s32_s16, vreinterpretq_s16_s32);
    let (l, s) = (vreinterpretq_s64_s16, vreinterpretq_s16_s64);
    // Pairs of rows interleaved by 16, then 32, then 64 bits.
    let t: [V; 8] = std::array::from_fn(|i| if i % 2 == 0 { vtrn1q_s16(r[i], r[i + 1]) } else { vtrn2q_s16(r[i - 1], r[i]) });
    let u = |a: usize, b: usize, hi: bool| if hi { h(vtrn2q_s32(w(t[a]), w(t[b]))) } else { h(vtrn1q_s32(w(t[a]), w(t[b]))) };
    // u[c]: column c (rows 0..4) and column c + 4 of rows 0..4; u[c + 4]: the same of rows 4..8.
    let u = [u(0, 2, false), u(1, 3, false), u(0, 2, true), u(1, 3, true), u(4, 6, false), u(5, 7, false), u(4, 6, true), u(5, 7, true)];
    std::array::from_fn(|c| if c < 4 { s(vtrn1q_s64(l(u[c]), l(u[c + 4]))) } else { s(vtrn2q_s64(l(u[c - 4]), l(u[c]))) })
}

// ---------------------------------------------------------------------------------------------
// Subpel filters (motion compensation)

/// See [`crate::subpel_h`]; `src` holds `out.len() + 7` samples.
pub(crate) fn subpel_h(src: &[u16], taps: &[i16; 8], out: &mut [i16], shift: u32) {
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { subpel_h_neon(src, taps, out, shift) }
}

/// 8 (then 4) columns at a time: each tap multiplies the source shifted by it and accumulates
/// in 32 bits.
#[target_feature(enable = "neon")]
fn subpel_h_neon(src: &[u16], taps: &[i16; 8], out: &mut [i16], shift: u32) {
    let sh = vdupq_n_s32(-(shift as i32));
    let n = out.len();
    let mut j = 0;
    while j + 8 <= n {
        let (mut lo, mut hi) = (vdupq_n_s32(0), vdupq_n_s32(0));
        for (t, &f) in taps.iter().enumerate() {
            let Some(s) = src.get(j + t..).and_then(|s| s.first_chunk::<8>()) else {
                return;
            };
            let v = vreinterpretq_s16_u16(bytemuck::must_cast(*s));
            lo = vmlal_n_s16(lo, vget_low_s16(v), f);
            hi = vmlal_high_n_s16(hi, v, f);
        }
        let r = vcombine_s16(vmovn_s32(vrshlq_s32(lo, sh)), vmovn_s32(vrshlq_s32(hi, sh)));
        if let Some(o) = out.get_mut(j..).and_then(|o| o.first_chunk_mut::<8>()) {
            *o = bytemuck::must_cast(r);
        }
        j += 8;
    }
    if j + 4 <= n {
        let mut lo = vdupq_n_s32(0);
        for (t, &f) in taps.iter().enumerate() {
            let Some(s) = src.get(j + t..).and_then(|s| s.first_chunk::<4>()) else {
                return;
            };
            lo = vmlal_n_s16(lo, vreinterpret_s16_u16(bytemuck::must_cast(*s)), f);
        }
        if let Some(o) = out.get_mut(j..).and_then(|o| o.first_chunk_mut::<4>()) {
            *o = bytemuck::must_cast(vmovn_s32(vrshlq_s32(lo, sh)));
        }
        j += 4;
    }
    if let (Some(s), Some(o)) = (src.get(j..), out.get_mut(j..)) {
        crate::scalar::subpel_h(s, taps, o, shift);
    }
}

/// See [`crate::subpel_v`]; `rows` holds 8 rows of `stride >= out.len()`.
pub(crate) fn subpel_v(rows: &[i16], stride: usize, taps: &[i16; 8], out: &mut [i32], shift: u32) {
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { subpel_v_neon(rows, stride, taps, out, shift) }
}

#[target_feature(enable = "neon")]
fn subpel_v_neon(rows: &[i16], stride: usize, taps: &[i16; 8], out: &mut [i32], shift: u32) {
    let sh = vdupq_n_s32(-(shift as i32));
    let n = out.len();
    let mut j = 0;
    while j + 8 <= n {
        let (mut lo, mut hi) = (vdupq_n_s32(0), vdupq_n_s32(0));
        for (t, &f) in taps.iter().enumerate() {
            let Some(s) = rows.get(t * stride + j..).and_then(|s| s.first_chunk::<8>()) else {
                return;
            };
            let v: int16x8_t = bytemuck::must_cast(*s);
            lo = vmlal_n_s16(lo, vget_low_s16(v), f);
            hi = vmlal_high_n_s16(hi, v, f);
        }
        if let Some(o) = out.get_mut(j..).and_then(|o| o.first_chunk_mut::<8>()) {
            *o = bytemuck::must_cast([vrshlq_s32(lo, sh), vrshlq_s32(hi, sh)]);
        }
        j += 8;
    }
    if j + 4 <= n {
        let mut lo = vdupq_n_s32(0);
        for (t, &f) in taps.iter().enumerate() {
            let Some(s) = rows.get(t * stride + j..).and_then(|s| s.first_chunk::<4>()) else {
                return;
            };
            lo = vmlal_n_s16(lo, bytemuck::must_cast(*s), f);
        }
        if let Some(o) = out.get_mut(j..).and_then(|o| o.first_chunk_mut::<4>()) {
            *o = bytemuck::must_cast(vrshlq_s32(lo, sh));
        }
        j += 4;
    }
    if let (Some(r), Some(o)) = (rows.get(j..), out.get_mut(j..)) {
        crate::scalar::subpel_v(r, stride, taps, o, shift);
    }
}

/// See [`crate::cdef_filter`]: 8-wide blocks a row per vector, 4-wide ones two rows; `false`
/// when a tap is outside `buf`.
pub(crate) fn cdef_filter(buf: &[u16; CDEF_STRIDE * CDEF_STRIDE], w: usize, h: usize, taps: &[CdefTap], clamp: bool, out: &mut [u16; 64]) -> bool {
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe {
        match (w, h) {
            (8, 8) => cdef_filter_neon::<8, 8>(buf, taps, clamp, out),
            (8, 4) => cdef_filter_neon::<8, 4>(buf, taps, clamp, out),
            (4, 8) => cdef_filter_neon::<4, 4>(buf, taps, clamp, out),
            (4, 4) => cdef_filter_neon::<4, 2>(buf, taps, clamp, out),
            _ => false,
        }
    }
}

/// The 8 samples from `start`: one row of 8, or 4 and the 4 below them.
#[target_feature(enable = "neon")]
fn cdef_rows<const W: usize>(buf: &[u16; CDEF_STRIDE * CDEF_STRIDE], start: usize) -> Option<V> {
    if W == 8 {
        return Some(load8(buf.get(start..)?.first_chunk::<8>()?));
    }
    let a = buf.get(start..)?.first_chunk::<4>()?;
    let b = buf.get(start + CDEF_STRIDE..)?.first_chunk::<4>()?;
    let (a, b): (uint16x4_t, uint16x4_t) = (bytemuck::must_cast(*a), bytemuck::must_cast(*b));
    Some(vreinterpretq_s16_u16(vcombine_u16(a, b)))
}

/// `N` vectors of `W`-wide rows; each tap's constants are set up once for all of them.
#[target_feature(enable = "neon")]
fn cdef_filter_neon<const W: usize, const N: usize>(buf: &[u16; CDEF_STRIDE * CDEF_STRIDE], taps: &[CdefTap], clamp: bool, out: &mut [u16; 64]) -> bool {
    let zero = vdupq_n_s16(0);
    // Vector k starts at row 2 + k * 8 / W, column 2.
    let at = |k: usize| (2 + k * (8 / W)) * CDEF_STRIDE + 2;
    let mut x = [zero; N];
    for (k, x) in x.iter_mut().enumerate() {
        let Some(v) = cdef_rows::<W>(buf, at(k)) else {
            return false;
        };
        *x = v;
    }
    let (mut sum, mut mx, mut mn) = ([zero; N], x, x);
    for t in taps {
        let (weight, strength, shift) = (vdupq_n_s16(t.weight), vdupq_n_s16(t.strength), vdupq_n_s16(-(t.shift.min(15) as i16)));
        for k in 0..N {
            let Some(p) = at(k).checked_add_signed(t.off).and_then(|s| cdef_rows::<W>(buf, s)) else {
                return false;
            };
            let d = vsubq_s16(p, x[k]);
            let ad = vabsq_s16(d);
            let mag = vminq_s16(vmaxq_s16(vsubq_s16(strength, vshlq_s16(ad, shift)), zero), ad);
            let sign = vshrq_n_s16::<15>(d);
            sum[k] = vmlaq_s16(sum[k], vsubq_s16(veorq_s16(mag, sign), sign), weight);
            if clamp {
                mx[k] = vmaxq_s16(mx[k], p);
                mn[k] = vminq_s16(mn[k], p);
            }
        }
    }
    for k in 0..N {
        // x + ((8 + sum - (sum < 0)) >> 4)
        let r = vaddq_s16(sum[k], vaddq_s16(vdupq_n_s16(8), vshrq_n_s16::<15>(sum[k])));
        let mut v = vaddq_s16(x[k], vshrq_n_s16::<4>(r));
        if clamp {
            v = vminq_s16(vmaxq_s16(v, mn[k]), mx[k]);
        }
        let Some(o) = out.get_mut(8 * k..).and_then(|o| o.first_chunk_mut::<8>()) else {
            return false;
        };
        store8(v, o);
    }
    true
}
