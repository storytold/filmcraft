//! SSE2 kernels (every x86_64 core). The arithmetic intrinsics are safe to call here because
//! the kernels are compiled with `sse2` enabled; entering a kernel and loading or storing
//! through pointers need `unsafe`.

use core::arch::x86_64::*;

use crate::{CDEF_COST_WEIGHTS, CDEF_STRIDE, CdefTap, cdef_pick};

/// 16-bit lanes; each line sum is two vectors (lanes 0..8 and 8..16), and a row is added at its
/// offset with byte shifts that move it up across the two registers.
pub(crate) fn cdef_direction(rows: &[&[u16; 8]; 8], shift: u32) -> (usize, i32) {
    // SAFETY: SSE2 is part of the x86_64 baseline: every core this build runs on has it.
    unsafe { cdef_direction_sse2(rows, shift) }
}

#[target_feature(enable = "sse2")]
fn cdef_direction_sse2(rows: &[&[u16; 8]; 8], shift: u32) -> (usize, i32) {
    let z = _mm_setzero_si128();
    let down = _mm_cvtsi32_si128(shift as i32);
    let centre = _mm_set1_epi16(128);
    let ones = _mm_set1_epi16(1);
    let load = |r: &[u16; 8]| -> __m128i {
        let v: __m128i = bytemuck::must_cast(*r);
        _mm_sub_epi16(_mm_srl_epi16(v, down), centre)
    };
    // Add `v` to the line sum (lo, hi) starting at lane `o`.
    macro_rules! at {
        ($lo:ident, $hi:ident, $v:expr, 0) => {{
            $lo = _mm_add_epi16($lo, $v);
        }};
        ($lo:ident, $hi:ident, $v:expr, $o:tt) => {{
            $lo = _mm_add_epi16($lo, _mm_slli_si128::<{ 2 * $o }>($v));
            $hi = _mm_add_epi16($hi, _mm_srli_si128::<{ 16 - 2 * $o }>($v));
        }};
    }
    // The four lanes of 32-bit sums added together.
    let total = |m: __m128i| -> i32 {
        let s = _mm_add_epi32(m, _mm_shuffle_epi32::<0x4E>(m));
        _mm_cvtsi128_si32(_mm_add_epi32(s, _mm_shuffle_epi32::<0xB1>(s)))
    };
    let (mut l0, mut h0, mut l1, mut h1, mut l3, mut h3, mut l4, mut h4) = (z, z, z, z, z, z, z, z);
    let (mut l5, mut h5, mut l7, mut h7, mut l6, mut l2) = (z, z, z, z, z, z);
    // Row i: directions 0, 1, 3 and 4 at offset i, 5 at 3 - i / 2, 7 at i / 2.
    macro_rules! row {
        ($i:tt, $o5:tt, $o7:tt) => {{
            let x = load(rows[$i]);
            let sums = _mm_madd_epi16(x, ones); // x0+x1, x2+x3, x4+x5, x6+x7 as i32
            let pairs = _mm_packs_epi32(sums, z); // the pairs as i16, then zeros (|pair| <= 256)
            let rev_pairs = _mm_shufflelo_epi16::<0x1B>(pairs);
            let rev = _mm_shuffle_epi32::<0x4E>(_mm_shufflehi_epi16::<0x1B>(_mm_shufflelo_epi16::<0x1B>(x)));
            at!(l0, h0, x, $i);
            at!(l1, h1, pairs, $i);
            at!(l3, h3, rev_pairs, $i);
            at!(l4, h4, rev, $i);
            at!(l5, h5, x, $o5);
            at!(l7, h7, x, $o7);
            l6 = _mm_add_epi16(l6, x);
            l2 = _mm_insert_epi16::<$i>(l2, total(sums));
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
        let p: [i16; 16] = bytemuck::must_cast([lo, hi]);
        p.iter().zip(&CDEF_COST_WEIGHTS[d]).map(|(&v, &w)| v as i32 * v as i32 * w).sum()
    });
    cdef_pick(&cost)
}

// ---------------------------------------------------------------------------------------------
// Loop filter

/// 8 lanes of i16; the deblocking kernels use the first four.
type V = __m128i;

/// `n` (4 or 8) lines across a horizontal edge (see [`crate::deblock_horizontal`]); `false`
/// when the samples aren't all in `d`.
pub(crate) fn deblock_horizontal(d: &mut [u16], base: usize, stride: usize, n: usize, on: u8, size: LfSize, t: &LfThresh) -> bool {
    // SAFETY: SSE2 is part of the x86_64 baseline: every core this build runs on has it.
    unsafe { horizontal(d, base, stride, n, on, size, t) }
}

/// `n` (4 or 8) lines across a vertical edge (see [`crate::deblock_vertical`]); `false` when
/// the samples aren't all in `d`.
pub(crate) fn deblock_vertical(d: &mut [u16], base: usize, stride: usize, n: usize, on: u8, size: LfSize, t: &LfThresh) -> bool {
    // SAFETY: SSE2 is part of the x86_64 baseline: every core this build runs on has it.
    unsafe { vertical(d, base, stride, n, on, size, t) }
}

crate::deblock::deblock_kernels!("sse2");

#[target_feature(enable = "sse2")]
fn splat(v: i32) -> V {
    _mm_set1_epi16(v as i16)
}
#[target_feature(enable = "sse2")]
fn add(a: V, b: V) -> V {
    _mm_add_epi16(a, b)
}
#[target_feature(enable = "sse2")]
fn sub(a: V, b: V) -> V {
    _mm_sub_epi16(a, b)
}
/// |a - b| of non-negative lanes: one of the saturating unsigned differences is zero.
#[target_feature(enable = "sse2")]
fn abd(a: V, b: V) -> V {
    _mm_or_si128(_mm_subs_epu16(a, b), _mm_subs_epu16(b, a))
}
#[target_feature(enable = "sse2")]
fn gt(a: V, b: V) -> V {
    _mm_cmpgt_epi16(a, b)
}
#[target_feature(enable = "sse2")]
fn and(a: V, b: V) -> V {
    _mm_and_si128(a, b)
}
#[target_feature(enable = "sse2")]
fn or(a: V, b: V) -> V {
    _mm_or_si128(a, b)
}
#[target_feature(enable = "sse2")]
fn andnot(a: V, b: V) -> V {
    _mm_andnot_si128(a, b)
}
#[target_feature(enable = "sse2")]
fn sel(m: V, a: V, b: V) -> V {
    _mm_or_si128(_mm_and_si128(m, a), _mm_andnot_si128(m, b))
}
#[target_feature(enable = "sse2")]
fn min(a: V, b: V) -> V {
    _mm_min_epi16(a, b)
}
#[target_feature(enable = "sse2")]
fn max(a: V, b: V) -> V {
    _mm_max_epi16(a, b)
}
#[target_feature(enable = "sse2")]
fn sra<const N: i32>(a: V) -> V {
    _mm_srai_epi16::<N>(a)
}
#[target_feature(enable = "sse2")]
fn srl<const N: i32>(a: V) -> V {
    _mm_srli_epi16::<N>(a)
}
#[target_feature(enable = "sse2")]
fn any(m: V) -> bool {
    _mm_movemask_epi8(m) != 0
}
#[target_feature(enable = "sse2")]
fn lanes(on: u8) -> V {
    let bits = _mm_set_epi16(128, 64, 32, 16, 8, 4, 2, 1);
    _mm_cmpeq_epi16(_mm_and_si128(_mm_set1_epi16(on as i16), bits), bits)
}
#[target_feature(enable = "sse2")]
fn load4(s: &[u16; 4]) -> V {
    bytemuck::must_cast([s[0], s[1], s[2], s[3], 0, 0, 0, 0])
}
#[target_feature(enable = "sse2")]
fn store4(v: V, s: &mut [u16; 4]) {
    let a: [u16; 8] = bytemuck::must_cast(v);
    *s = [a[0], a[1], a[2], a[3]];
}
#[target_feature(enable = "sse2")]
fn load8(s: &[u16; 8]) -> V {
    bytemuck::must_cast(*s)
}
#[target_feature(enable = "sse2")]
fn store8(v: V, s: &mut [u16; 8]) {
    *s = bytemuck::must_cast(v);
}
/// 8 rows of 8 to 8 columns of 8 (and back: it is its own inverse).
#[target_feature(enable = "sse2")]
fn transpose(r: [V; 8]) -> [V; 8] {
    // Pairs of rows interleaved by 16, then 32, then 64 bits.
    let a: [V; 8] = std::array::from_fn(|i| if i % 2 == 0 { _mm_unpacklo_epi16(r[i], r[i + 1]) } else { _mm_unpackhi_epi16(r[i - 1], r[i]) });
    // b[0..4]: columns (0, 1), (2, 3), (4, 5), (6, 7) of rows 0..4; b[4..8]: of rows 4..8.
    let b = |x: usize, y: usize, hi: bool| if hi { _mm_unpackhi_epi32(a[x], a[y]) } else { _mm_unpacklo_epi32(a[x], a[y]) };
    let b = [b(0, 2, false), b(0, 2, true), b(1, 3, false), b(1, 3, true), b(4, 6, false), b(4, 6, true), b(5, 7, false), b(5, 7, true)];
    std::array::from_fn(|c| if c % 2 == 0 { _mm_unpacklo_epi64(b[c / 2], b[c / 2 + 4]) } else { _mm_unpackhi_epi64(b[c / 2], b[c / 2 + 4]) })
}

// ---------------------------------------------------------------------------------------------
// Subpel filters (motion compensation)

/// Taps `2k` and `2k + 1` as the i16 pair `pmaddwd` multiplies interleaved samples by.
#[target_feature(enable = "sse2")]
fn tap_pairs(taps: &[i16; 8]) -> [V; 4] {
    std::array::from_fn(|k| _mm_set1_epi32(((taps[2 * k + 1] as u16 as i32) << 16) | taps[2 * k] as u16 as i32))
}

/// See [`crate::subpel_h`]; `src` holds `out.len() + 7` samples.
pub(crate) fn subpel_h(src: &[u16], taps: &[i16; 8], out: &mut [i16], shift: u32) {
    // SAFETY: SSE2 is part of the x86_64 baseline: every core this build runs on has it.
    unsafe { subpel_h_sse2(src, taps, out, shift) }
}

/// 8 (then 4) columns at a time: the samples under taps 2k and 2k + 1 interleaved, so one
/// `pmaddwd` applies both taps and sums in 32 bits.
#[target_feature(enable = "sse2")]
fn subpel_h_sse2(src: &[u16], taps: &[i16; 8], out: &mut [i16], shift: u32) {
    let c = tap_pairs(taps);
    let half = _mm_set1_epi32((1 << shift) >> 1);
    let cnt = _mm_cvtsi32_si128(shift as i32);
    let n = out.len();
    let mut j = 0;
    while j + 8 <= n {
        let (mut lo, mut hi) = (half, half);
        for (k, &ck) in c.iter().enumerate() {
            let at = |o: usize| src.get(j + 2 * k + o..).and_then(|s| s.first_chunk::<8>());
            let (Some(a), Some(b)) = (at(0), at(1)) else {
                return;
            };
            let (a, b) = (load8(a), load8(b));
            lo = _mm_add_epi32(lo, _mm_madd_epi16(_mm_unpacklo_epi16(a, b), ck));
            hi = _mm_add_epi32(hi, _mm_madd_epi16(_mm_unpackhi_epi16(a, b), ck));
        }
        if let Some(o) = out.get_mut(j..).and_then(|o| o.first_chunk_mut::<8>()) {
            let r = _mm_packs_epi32(_mm_sra_epi32(lo, cnt), _mm_sra_epi32(hi, cnt));
            *o = bytemuck::must_cast(r);
        }
        j += 8;
    }
    if j + 4 <= n {
        let mut lo = half;
        for (k, &ck) in c.iter().enumerate() {
            let at = |o: usize| src.get(j + 2 * k + o..).and_then(|s| s.first_chunk::<4>());
            let (Some(a), Some(b)) = (at(0), at(1)) else {
                return;
            };
            lo = _mm_add_epi32(lo, _mm_madd_epi16(_mm_unpacklo_epi16(load4(a), load4(b)), ck));
        }
        if let Some(o) = out.get_mut(j..).and_then(|o| o.first_chunk_mut::<4>()) {
            let r = _mm_packs_epi32(_mm_sra_epi32(lo, cnt), lo);
            let a: [i16; 8] = bytemuck::must_cast(r);
            *o = [a[0], a[1], a[2], a[3]];
        }
        j += 4;
    }
    if let (Some(s), Some(o)) = (src.get(j..), out.get_mut(j..)) {
        crate::scalar::subpel_h(s, taps, o, shift);
    }
}

/// See [`crate::subpel_v`]; `rows` holds 8 rows of `stride >= out.len()`.
pub(crate) fn subpel_v(rows: &[i16], stride: usize, taps: &[i16; 8], out: &mut [i32], shift: u32) {
    // SAFETY: SSE2 is part of the x86_64 baseline: every core this build runs on has it.
    unsafe { subpel_v_sse2(rows, stride, taps, out, shift) }
}

#[target_feature(enable = "sse2")]
fn subpel_v_sse2(rows: &[i16], stride: usize, taps: &[i16; 8], out: &mut [i32], shift: u32) {
    let c = tap_pairs(taps);
    let half = _mm_set1_epi32((1 << shift) >> 1);
    let cnt = _mm_cvtsi32_si128(shift as i32);
    let n = out.len();
    let mut j = 0;
    while j + 4 <= n {
        let wide = j + 8 <= n;
        let (mut lo, mut hi) = (half, half);
        for (k, &ck) in c.iter().enumerate() {
            let at = |t: usize| rows.get(t * stride + j..);
            let (Some(a), Some(b)) = (at(2 * k), at(2 * k + 1)) else {
                return;
            };
            let (a, b) = match (a.first_chunk::<8>().filter(|_| wide), b.first_chunk::<8>().filter(|_| wide), a.first_chunk::<4>(), b.first_chunk::<4>()) {
                (Some(a), Some(b), _, _) => (load8_i16(a), load8_i16(b)),
                (_, _, Some(a), Some(b)) => (load4_i16(a), load4_i16(b)),
                _ => return,
            };
            lo = _mm_add_epi32(lo, _mm_madd_epi16(_mm_unpacklo_epi16(a, b), ck));
            hi = _mm_add_epi32(hi, _mm_madd_epi16(_mm_unpackhi_epi16(a, b), ck));
        }
        let Some(o) = out.get_mut(j..).and_then(|o| o.first_chunk_mut::<4>()) else {
            return;
        };
        *o = bytemuck::must_cast(_mm_sra_epi32(lo, cnt));
        if wide && let Some(o) = out.get_mut(j + 4..).and_then(|o| o.first_chunk_mut::<4>()) {
            *o = bytemuck::must_cast(_mm_sra_epi32(hi, cnt));
        }
        j += if wide { 8 } else { 4 };
    }
    if let (Some(r), Some(o)) = (rows.get(j..), out.get_mut(j..)) {
        crate::scalar::subpel_v(r, stride, taps, o, shift);
    }
}

#[target_feature(enable = "sse2")]
fn load8_i16(s: &[i16; 8]) -> V {
    bytemuck::must_cast(*s)
}

#[target_feature(enable = "sse2")]
fn load4_i16(s: &[i16; 4]) -> V {
    bytemuck::must_cast([s[0], s[1], s[2], s[3], 0, 0, 0, 0])
}

/// See [`crate::cdef_filter`]: 8-wide blocks a row per vector, 4-wide ones two rows; `false`
/// when a tap is outside `buf`.
pub(crate) fn cdef_filter(buf: &[u16; CDEF_STRIDE * CDEF_STRIDE], w: usize, h: usize, taps: &[CdefTap], clamp: bool, out: &mut [u16; 64]) -> bool {
    // SAFETY: SSE2 is part of the x86_64 baseline: every core this build runs on has it.
    unsafe {
        match (w, h) {
            (8, 8) => cdef_filter_sse2::<8, 8>(buf, taps, clamp, out),
            (8, 4) => cdef_filter_sse2::<8, 4>(buf, taps, clamp, out),
            (4, 8) => cdef_filter_sse2::<4, 4>(buf, taps, clamp, out),
            (4, 4) => cdef_filter_sse2::<4, 2>(buf, taps, clamp, out),
            _ => false,
        }
    }
}

/// The 8 samples from `start`: one row of 8, or 4 and the 4 below them.
#[target_feature(enable = "sse2")]
fn cdef_rows<const W: usize>(buf: &[u16; CDEF_STRIDE * CDEF_STRIDE], start: usize) -> Option<V> {
    if W == 8 {
        return Some(load8(buf.get(start..)?.first_chunk::<8>()?));
    }
    let a = buf.get(start..)?.first_chunk::<4>()?;
    let b = buf.get(start + CDEF_STRIDE..)?.first_chunk::<4>()?;
    Some(_mm_unpacklo_epi64(load4(a), load4(b)))
}

/// `N` vectors of `W`-wide rows; each tap's constants are set up once for all of them.
#[target_feature(enable = "sse2")]
fn cdef_filter_sse2<const W: usize, const N: usize>(buf: &[u16; CDEF_STRIDE * CDEF_STRIDE], taps: &[CdefTap], clamp: bool, out: &mut [u16; 64]) -> bool {
    let zero = _mm_setzero_si128();
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
        let (weight, strength, shift) = (_mm_set1_epi16(t.weight), _mm_set1_epi16(t.strength), _mm_cvtsi32_si128(t.shift.min(15) as i32));
        for k in 0..N {
            let Some(p) = at(k).checked_add_signed(t.off).and_then(|s| cdef_rows::<W>(buf, s)) else {
                return false;
            };
            let d = _mm_sub_epi16(p, x[k]);
            let ad = _mm_max_epi16(d, _mm_sub_epi16(zero, d));
            let mag = _mm_min_epi16(_mm_max_epi16(_mm_sub_epi16(strength, _mm_sra_epi16(ad, shift)), zero), ad);
            let sign = _mm_srai_epi16::<15>(d);
            sum[k] = _mm_add_epi16(sum[k], _mm_mullo_epi16(_mm_sub_epi16(_mm_xor_si128(mag, sign), sign), weight));
            if clamp {
                mx[k] = _mm_max_epi16(mx[k], p);
                mn[k] = _mm_min_epi16(mn[k], p);
            }
        }
    }
    for k in 0..N {
        // x + ((8 + sum - (sum < 0)) >> 4)
        let r = _mm_add_epi16(sum[k], _mm_add_epi16(_mm_set1_epi16(8), _mm_srai_epi16::<15>(sum[k])));
        let mut v = _mm_add_epi16(x[k], _mm_srai_epi16::<4>(r));
        if clamp {
            v = _mm_min_epi16(_mm_max_epi16(v, mn[k]), mx[k]);
        }
        let Some(o) = out.get_mut(8 * k..).and_then(|o| o.first_chunk_mut::<8>()) else {
            return false;
        };
        store8(v, o);
    }
    true
}
