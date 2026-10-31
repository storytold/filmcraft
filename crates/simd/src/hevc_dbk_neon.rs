//! NEON kernel for the HEVC luma deblocking filter (8.7.2.5.3 decisions, 8.7.2.5.7 filtering)
//! of 8-bit samples: one 8-line edge (two 4-line decision segments) per call, one line per 16-bit
//! lane.
//!
//! A vertical edge's 8 rows of p3..q3 are transposed in registers (8x8 bytes: `vtrn` of bytes,
//! halfwords and words), so that both edge directions filter the same vectors x[0..8] = p3..q3.
//! The decisions of a segment come from its lines 0 and 3; a table lookup broadcasts those lanes
//! over the segment's four, so every decision is a lane mask. Both the strong and the weak filter
//! run on every lane and the masks pick the result; the stores narrow with `vqmovun`, which is the
//! weak filter's Clip1 (the strong filter's results stay within p +- 2 tC of samples in range).

use core::arch::aarch64::*;

use crate::HevcLumaEdge;

/// See [`crate::hevc_deblock_luma8`]: false (nothing written) when the samples do not fit `d`.
pub(crate) fn luma(d: &mut [u8], o: usize, stride: usize, vertical: bool, e: &HevcLumaEdge) -> bool {
    let fits = if vertical {
        o.checked_sub(4).and_then(|s| stride.checked_mul(7)?.checked_add(s)?.checked_add(8)).is_some_and(|end| end <= d.len())
    } else {
        stride.checked_mul(4).is_some_and(|b| b <= o) && stride.checked_mul(3).and_then(|r| r.checked_add(o)?.checked_add(8)).is_some_and(|end| end <= d.len())
    };
    if !fits {
        return false;
    }
    if !e.on[0] && !e.on[1] {
        return true;
    }
    // SAFETY: NEON is part of the aarch64 baseline: every core this build runs on has it.
    unsafe { edge(d, o, stride, vertical, e) };
    true
}

/// 8 samples from `d[o..]`; `luma` checked that every row the kernel touches is in `d`.
#[target_feature(enable = "neon")]
fn ld(d: &[u8], o: usize) -> uint8x8_t {
    debug_assert!(o + 8 <= d.len());
    // SAFETY: `o + 8 <= d.len()` (checked by `luma` for each row), the 8 bytes `vld1_u8` reads.
    unsafe { vld1_u8(d.as_ptr().add(o)) }
}

#[target_feature(enable = "neon")]
fn st(d: &mut [u8], o: usize, v: uint8x8_t) {
    debug_assert!(o + 8 <= d.len());
    // SAFETY: as for `ld`.
    unsafe { vst1_u8(d.as_mut_ptr().add(o), v) }
}

/// 8 rows of 8 bytes to 8 columns (its own inverse).
#[target_feature(enable = "neon")]
fn transpose(r: [uint8x8_t; 8]) -> [uint8x8_t; 8] {
    let b = [vtrn_u8(r[0], r[1]), vtrn_u8(r[2], r[3]), vtrn_u8(r[4], r[5]), vtrn_u8(r[6], r[7])];
    let h = vreinterpret_u16_u8;
    // Rows 0..4 and 4..8 of columns (0, 4), (2, 6), (1, 5) and (3, 7).
    let c = [vtrn_u16(h(b[0].0), h(b[1].0)), vtrn_u16(h(b[0].1), h(b[1].1)), vtrn_u16(h(b[2].0), h(b[3].0)), vtrn_u16(h(b[2].1), h(b[3].1))];
    let w = vreinterpret_u32_u16;
    let d = [vtrn_u32(w(c[0].0), w(c[2].0)), vtrn_u32(w(c[1].0), w(c[3].0)), vtrn_u32(w(c[0].1), w(c[2].1)), vtrn_u32(w(c[1].1), w(c[3].1))];
    let u = vreinterpret_u8_u32;
    [u(d[0].0), u(d[1].0), u(d[2].0), u(d[3].0), u(d[0].1), u(d[1].1), u(d[2].1), u(d[3].1)]
}

/// Lane l of the result: lane 0 (`LINE` 0) or 3 (`LINE` 3) of the segment of lane l.
#[target_feature(enable = "neon")]
fn seg<const LINE: u8>(v: int16x8_t) -> int16x8_t {
    let (a, b) = (2 * LINE, 2 * LINE + 1);
    let idx = [a, b, a, b, a, b, a, b, a + 8, b + 8, a + 8, b + 8, a + 8, b + 8, a + 8, b + 8];
    let t: uint8x16_t = bytemuck::must_cast(idx);
    vreinterpretq_s16_u8(vqtbl1q_u8(vreinterpretq_u8_s16(v), t))
}

/// Per-lane value of the segment parameters: `a[0]` in lanes 0..4, `a[1]` in lanes 4..8.
#[target_feature(enable = "neon")]
fn lanes(a: [i32; 2]) -> int16x8_t {
    vcombine_s16(vdup_n_s16(a[0] as i16), vdup_n_s16(a[1] as i16))
}

#[target_feature(enable = "neon")]
fn mask(a: [bool; 2]) -> int16x8_t {
    lanes([-(a[0] as i32), -(a[1] as i32)])
}

#[target_feature(enable = "neon")]
fn lt(a: int16x8_t, b: int16x8_t) -> int16x8_t {
    vreinterpretq_s16_u16(vcltq_s16(a, b))
}

#[target_feature(enable = "neon")]
fn sel(m: int16x8_t, a: int16x8_t, b: int16x8_t) -> int16x8_t {
    vbslq_s16(vreinterpretq_u16_s16(m), a, b)
}

#[target_feature(enable = "neon")]
fn clamp(v: int16x8_t, lo: int16x8_t, hi: int16x8_t) -> int16x8_t {
    vmaxq_s16(vminq_s16(v, hi), lo)
}

#[target_feature(enable = "neon")]
fn edge(d: &mut [u8], o: usize, stride: usize, vertical: bool, e: &HevcLumaEdge) {
    // x[i]: p3, p2, p1, p0, q0, q1, q2, q3 of the 8 lines.
    let raw: [uint8x8_t; 8] =
        if vertical { transpose(std::array::from_fn(|r| ld(d, o - 4 + r * stride))) } else { std::array::from_fn(|i| ld(d, o + i * stride - 4 * stride)) };
    let x: [int16x8_t; 8] = raw.map(|v| vreinterpretq_s16_u16(vmovl_u8(v)));
    let [p3, p2, p1, p0, q0, q1, q2, q3] = x;
    let (beta, tc) = (lanes(e.beta), lanes(e.tc));
    // decisions (8.7.2.5.3)
    let dp = vabsq_s16(vaddq_s16(vsubq_s16(p2, vshlq_n_s16::<1>(p1)), p0));
    let dq = vabsq_s16(vaddq_s16(vsubq_s16(q2, vshlq_n_s16::<1>(q1)), q0));
    let dpq = vaddq_s16(dp, dq);
    let on = vandq_s16(lt(vaddq_s16(seg::<0>(dpq), seg::<3>(dpq)), beta), mask(e.on));
    if vmaxvq_u16(vreinterpretq_u16_s16(on)) == 0 {
        return;
    }
    let tc5 = vshrq_n_s16::<1>(vaddq_s16(vmulq_n_s16(tc, 5), vdupq_n_s16(1)));
    let sam = vandq_s16(
        vandq_s16(lt(vshlq_n_s16::<1>(dpq), vshrq_n_s16::<2>(beta)), lt(vaddq_s16(vabdq_s16(p3, p0), vabdq_s16(q0, q3)), vshrq_n_s16::<3>(beta))),
        lt(vabdq_s16(p0, q0), tc5),
    );
    let strong = vandq_s16(seg::<0>(sam), seg::<3>(sam));
    let side = vshrq_n_s16::<3>(vaddq_s16(beta, vshrq_n_s16::<1>(beta)));
    let dep = lt(vaddq_s16(seg::<0>(dp), seg::<3>(dp)), side);
    let deq = lt(vaddq_s16(seg::<0>(dq), seg::<3>(dq)), side);
    // strong filter (8-353 .. 8-358)
    let t2 = vshlq_n_s16::<1>(tc);
    let around = |v: int16x8_t, c: int16x8_t| clamp(v, vsubq_s16(c, t2), vaddq_s16(c, t2));
    let p0q0 = vaddq_s16(p0, q0);
    let sp0 = around(vrshrq_n_s16::<3>(vaddq_s16(vaddq_s16(p2, q1), vshlq_n_s16::<1>(vaddq_s16(p1, p0q0)))), p0);
    let sp1 = around(vrshrq_n_s16::<2>(vaddq_s16(vaddq_s16(p2, p1), p0q0)), p1);
    let sp2 = around(vrshrq_n_s16::<3>(vaddq_s16(vaddq_s16(vshlq_n_s16::<1>(p3), vmulq_n_s16(p2, 3)), vaddq_s16(p1, p0q0))), p2);
    let sq0 = around(vrshrq_n_s16::<3>(vaddq_s16(vaddq_s16(p1, q2), vshlq_n_s16::<1>(vaddq_s16(q1, p0q0)))), q0);
    let sq1 = around(vrshrq_n_s16::<2>(vaddq_s16(vaddq_s16(q2, q1), p0q0)), q1);
    let sq2 = around(vrshrq_n_s16::<3>(vaddq_s16(vaddq_s16(vshlq_n_s16::<1>(q3), vmulq_n_s16(q2, 3)), vaddq_s16(q1, p0q0))), q2);
    // weak filter (8-359 .. 8-366)
    let raw_delta = vrshrq_n_s16::<4>(vsubq_s16(vmulq_n_s16(vsubq_s16(q0, p0), 9), vmulq_n_s16(vsubq_s16(q1, p1), 3)));
    let weak = vandq_s16(vbicq_s16(on, strong), lt(vabsq_s16(raw_delta), vmulq_n_s16(tc, 10)));
    let delta = clamp(raw_delta, vnegq_s16(tc), tc);
    let wp0 = vaddq_s16(p0, delta);
    let wq0 = vsubq_s16(q0, delta);
    let tc2 = vshrq_n_s16::<1>(tc);
    let wp1 = vaddq_s16(p1, clamp(vshrq_n_s16::<1>(vaddq_s16(vsubq_s16(vrhaddq_s16(p2, p0), p1), delta)), vnegq_s16(tc2), tc2));
    let wq1 = vaddq_s16(q1, clamp(vshrq_n_s16::<1>(vsubq_s16(vsubq_s16(vrhaddq_s16(q2, q0), q1), delta)), vnegq_s16(tc2), tc2));
    // lanes each side may change (pcm / bypass blocks keep their samples)
    let (mp, mq) = (vbicq_s16(on, mask(e.no_p)), vbicq_s16(on, mask(e.no_q)));
    let (sp, sq) = (vandq_s16(strong, mp), vandq_s16(strong, mq));
    let (wp, wq) = (vandq_s16(weak, mp), vandq_s16(weak, mq));
    let out = [
        p3,
        sel(sp, sp2, p2),
        sel(sp, sp1, sel(vandq_s16(wp, dep), wp1, p1)),
        sel(sp, sp0, sel(wp, wp0, p0)),
        sel(sq, sq0, sel(wq, wq0, q0)),
        sel(sq, sq1, sel(vandq_s16(wq, deq), wq1, q1)),
        sel(sq, sq2, q2),
        q3,
    ]
    .map(|v| vqmovun_s16(v));
    if vertical {
        for (r, v) in transpose(out).into_iter().enumerate() {
            st(d, o - 4 + r * stride, v);
        }
    } else {
        for (i, &v) in out.iter().enumerate().take(7).skip(1) {
            st(d, o + i * stride - 4 * stride, v);
        }
    }
}
