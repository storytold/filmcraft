//! AV1 and VP9 loop filter (deblocking) sample filters (AV1 7.14.6, VP9 8.8.5): the types the
//! API takes, and the vector kernels written once as a macro that each architecture module
//! expands over its own vector operations. VP9's filters are AV1's predecessors: the same narrow
//! filter and 8-wide flat filter, a 4-wide filter whose mask still checks p3..q3, and a 16-wide
//! filter over p7..q7 (15 taps) where AV1's reaches p6..q6 (13).

/// The sample filter an edge uses: AV1's 7.14.3 `filterSize` 4, 8 or 16 (on chroma planes 8 is
/// the 6-tap filter), or VP9's 8.8.2 filter size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfSize {
    Four,
    Six,
    Eight,
    Sixteen,
    /// VP9 TX_4X4: the narrow filter, with the mask over p3..q3.
    Vp9Four,
    /// VP9 TX_8X8: as AV1's 8.
    Vp9Eight,
    /// VP9 TX_16X16: flat2 and the wide filter over p7..q7.
    Vp9Sixteen,
}

impl LfSize {
    /// The filter for an edge of `filter_size` (4, 8 or 16) on a luma or chroma plane.
    pub fn new(filter_size: usize, chroma: bool) -> LfSize {
        match (filter_size, chroma) {
            (0..=4, _) => LfSize::Four,
            (_, true) => LfSize::Six,
            (5..=8, false) => LfSize::Eight,
            _ => LfSize::Sixteen,
        }
    }

    /// The size with discriminant `id` (the kernels' const parameter).
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    pub(crate) const fn from_id(id: u8) -> LfSize {
        match id {
            0 => LfSize::Four,
            1 => LfSize::Six,
            2 => LfSize::Eight,
            3 => LfSize::Sixteen,
            4 => LfSize::Vp9Four,
            5 => LfSize::Vp9Eight,
            _ => LfSize::Vp9Sixteen,
        }
    }

    /// Samples the filter reads on each side of the edge.
    pub(crate) const fn reach(self) -> usize {
        match self {
            LfSize::Four => 2,
            LfSize::Six => 3,
            LfSize::Eight | LfSize::Vp9Four | LfSize::Vp9Eight => 4,
            LfSize::Sixteen => 7,
            LfSize::Vp9Sixteen => 8,
        }
    }

    /// Samples the filter can change on each side of the edge.
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    pub(crate) const fn changes(self) -> usize {
        match self {
            LfSize::Four | LfSize::Six | LfSize::Vp9Four => 2,
            LfSize::Eight | LfSize::Vp9Eight => 3,
            LfSize::Sixteen => 6,
            LfSize::Vp9Sixteen => 7,
        }
    }
}

/// Per-edge thresholds of the sample filter, scaled to the bit depth.
#[derive(Clone, Copy, Debug)]
pub struct LfThresh {
    pub(crate) limit: i32,
    pub(crate) blimit: i32,
    pub(crate) thresh: i32,
    /// Flatness threshold 1 << (BitDepth - 8).
    pub(crate) flat: i32,
    /// 0x80 << (BitDepth - 8) and 1 << (BitDepth - 1).
    pub(crate) off: i32,
    pub(crate) half: i32,
}

impl LfThresh {
    /// The thresholds of an edge with filter strength `limit`, `blimit` and `thresh` (7.14.4)
    /// in a `bit_depth` (8 to 12) picture.
    pub fn new(limit: i32, blimit: i32, thresh: i32, bit_depth: u32) -> LfThresh {
        let sh = bit_depth.clamp(8, 12) - 8;
        LfThresh { limit: limit << sh, blimit: blimit << sh, thresh: thresh << sh, flat: 1 << sh, off: 0x80 << sh, half: 1 << (sh + 7) }
    }
}

/// The vector kernels over 8 lanes of i16, one line across the edge per lane (8 lines, or 4 in
/// the first lanes). Samples (at most 12 bits) fit an i16; the wide filters' weighted sums (at
/// most 16 * 4095 + 8) fit 16 bits unsigned, so they add with wrapping and shift right
/// logically.
///
/// The expanding module provides the type `V`, the target feature `$feature`, and these
/// functions, all compiled with that feature: `splat`, `add`, `sub` (wrapping), `abd` (absolute
/// difference of non-negative lanes), `gt` (all ones where a > b), `and`, `or`, `andnot`
/// (!a & b), `sel` (m ? a : b), `min`, `max`, `sra::<N>`, `srl::<N>`, `any` (a lane set),
/// `lanes` (lane l set where bit l of a mask is), `load4`/`store4` (4 samples into the first lanes),
/// `load8`/`store8`, and `transpose` (8 rows of 8 samples to 8 columns: its own inverse).
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
macro_rules! deblock_kernels {
    ($feature:literal) => {
        use $crate::deblock::{LfSize, LfThresh};

        /// The part of `d` from `before` samples before `base` to `len` samples after that, or
        /// None when it isn't all in `d`: the kernels check their reach once and then address
        /// rows with plain arithmetic.
        fn area(d: &mut [u16], base: usize, before: usize, len: usize) -> Option<&mut [u16]> {
            let first = base.checked_sub(before)?;
            d.get_mut(first..first.checked_add(len)?)
        }

        /// `n` (4 or 8) lines across a horizontal edge: q0 of the first at `d[base]`, the
        /// others after it, `stride` between rows; line l is filtered where bit l of `on` is
        /// set. `false` (nothing changed) when the samples it reads aren't all in `d`.
        #[target_feature(enable = $feature)]
        fn horizontal(d: &mut [u16], base: usize, stride: usize, n: usize, on: u8, size: LfSize, t: &LfThresh) -> bool {
            // The filter size as a const parameter: every loop over taps has constant bounds and
            // unrolls, so the taps stay in registers.
            match size {
                LfSize::Four => horizontal_s::<0>(d, base, stride, n, on, t),
                LfSize::Six => horizontal_s::<1>(d, base, stride, n, on, t),
                LfSize::Eight => horizontal_s::<2>(d, base, stride, n, on, t),
                LfSize::Sixteen => horizontal_s::<3>(d, base, stride, n, on, t),
                LfSize::Vp9Four => horizontal_s::<4>(d, base, stride, n, on, t),
                LfSize::Vp9Eight => horizontal_s::<5>(d, base, stride, n, on, t),
                LfSize::Vp9Sixteen => horizontal_s::<6>(d, base, stride, n, on, t),
            }
        }

        /// [`horizontal`] with the filter size [`LfSize::from_id`] `S`.
        #[target_feature(enable = $feature)]
        fn horizontal_s<const S: u8>(d: &mut [u16], base: usize, stride: usize, n: usize, on: u8, t: &LfThresh) -> bool {
            let size = const { LfSize::from_id(S) };
            let (r, w) = (size.reach(), size.changes());
            // Rows p_(r-1) .. q_(r-1), `n` samples of each: row k (offset k - 8 from q0) at (k + r - 8) * stride.
            let Some(d) = r.checked_mul(stride).zip((2 * r - 1).checked_mul(stride)).and_then(|(b, l)| area(d, base, b, l.checked_add(n)?)) else {
                return false;
            };
            let mut x = [splat(0); 16];
            for k in 8 - r..8 + r {
                let row = &d[(k + r - 8) * stride..];
                x[k] = match (n == 8, row.first_chunk::<8>(), row.first_chunk::<4>()) {
                    (true, Some(row), _) => load8(row),
                    (false, _, Some(row)) => load4(row),
                    _ => return false,
                };
            }
            filter::<S, false>(&mut x, t, lanes(if n == 8 { on } else { on & 15 }));
            for k in 8 - w..8 + w {
                let row = &mut d[(k + r - 8) * stride..];
                if n == 8 {
                    if let Some(row) = row.first_chunk_mut::<8>() {
                        store8(x[k], row);
                    }
                } else if let Some(row) = row.first_chunk_mut::<4>() {
                    store4(x[k], row);
                }
            }
            true
        }

        /// `n` (4 or 8) lines across a vertical edge: q0 of the first at `d[base]`, the others
        /// one `stride` below each other, line l filtered where bit l of `on` is set. Reads and
        /// writes 8 samples per line (16 for the 16-wide filters) centred on the edge, transposed
        /// in registers; `false` (nothing changed) when they aren't all in `d`.
        #[target_feature(enable = $feature)]
        fn vertical(d: &mut [u16], base: usize, stride: usize, n: usize, on: u8, size: LfSize, t: &LfThresh) -> bool {
            match size {
                LfSize::Four => vertical_s::<0>(d, base, stride, n, on, t),
                LfSize::Six => vertical_s::<1>(d, base, stride, n, on, t),
                LfSize::Eight => vertical_s::<2>(d, base, stride, n, on, t),
                LfSize::Sixteen => vertical_s::<3>(d, base, stride, n, on, t),
                LfSize::Vp9Four => vertical_s::<4>(d, base, stride, n, on, t),
                LfSize::Vp9Eight => vertical_s::<5>(d, base, stride, n, on, t),
                LfSize::Vp9Sixteen => vertical_s::<6>(d, base, stride, n, on, t),
            }
        }

        /// [`vertical`] with the filter size [`LfSize::from_id`] `S`.
        #[target_feature(enable = $feature)]
        fn vertical_s<const S: u8>(d: &mut [u16], base: usize, stride: usize, n: usize, on: u8, t: &LfThresh) -> bool {
            let size = const { LfSize::from_id(S) };
            // 8 samples before q0 and 8 from it for the 16-wide filters, else 4 and 4: runs of 8
            // samples, the first holding tap `8 - before`, line i of run j at i * stride + 8 * j.
            let before = if matches!(size, LfSize::Sixteen | LfSize::Vp9Sixteen) { 8 } else { 4 };
            let Some(d) = (n - 1).checked_mul(stride).and_then(|l| area(d, base, before, l.checked_add(2 * before)?)) else {
                return false;
            };
            let mut x = [splat(0); 16];
            for j in 0..before / 4 {
                let mut rows = [splat(0); 8];
                for (i, v) in rows[..n].iter_mut().enumerate() {
                    let Some(s) = d[i * stride + 8 * j..].first_chunk::<8>() else {
                        return false;
                    };
                    *v = load8(s);
                }
                let k = 8 - before + 8 * j;
                x[k..k + 8].copy_from_slice(&transpose(rows));
            }
            filter::<S, true>(&mut x, t, lanes(if n == 8 { on } else { on & 15 }));
            for j in 0..before / 4 {
                let k = 8 - before + 8 * j;
                let mut cols = [splat(0); 8];
                cols.copy_from_slice(&x[k..k + 8]);
                for (i, v) in transpose(cols).into_iter().take(n).enumerate() {
                    if let Some(s) = d[i * stride + 8 * j..].first_chunk_mut::<8>() {
                        store8(v, s);
                    }
                }
            }
            true
        }

        #[target_feature(enable = $feature)]
        fn clamp(v: V, lo: V, hi: V) -> V {
            max(min(v, hi), lo)
        }

        /// The sample filter (AV1 7.14.6, VP9 8.8.5) on taps `x[k]` at offset `k - 8` from q0, one
        /// line per lane (the lanes set in `lines`). Every lane runs every filter and keeps the
        /// result its masks pick. `VERTICAL` names the caller: each instance has one call site,
        /// so it is inlined and the taps stay in registers rather than going through memory.
        #[target_feature(enable = $feature)]
        fn filter<const S: u8, const VERTICAL: bool>(x: &mut [V; 16], t: &LfThresh, lines: V) {
            let size = const { LfSize::from_id(S) };
            let (lim, tf) = (splat(t.limit), splat(t.flat));
            let [p3, p2, p1, p0, q0, q1, q2, q3] = [x[4], x[5], x[6], x[7], x[8], x[9], x[10], x[11]];
            // filter mask (7.14.6.2)
            let (ap, aq, a0) = (abd(p1, p0), abd(q1, q0), abd(p0, q0));
            let mut fail = or(or(gt(ap, lim), gt(aq, lim)), gt(add(add(a0, a0), srl::<1>(abd(p1, q1))), splat(t.blimit)));
            let mut flat = splat(0);
            if size != LfSize::Four {
                fail = or(fail, or(gt(abd(p2, p1), lim), gt(abd(q2, q1), lim)));
                let mut rough = or(or(gt(ap, tf), gt(aq, tf)), or(gt(abd(p2, p0), tf), gt(abd(q2, q0), tf)));
                if size != LfSize::Six {
                    fail = or(fail, or(gt(abd(p3, p2), lim), gt(abd(q3, q2), lim)));
                    rough = or(rough, or(gt(abd(p3, p0), tf), gt(abd(q3, q0), tf)));
                }
                flat = if size == LfSize::Vp9Four { splat(0) } else { andnot(rough, splat(-1)) };
            }
            let on = andnot(fail, lines);
            if !any(on) {
                return;
            }
            let flat = and(flat, on);
            let narrow = andnot(flat, on);
            if any(narrow) {
                // narrow filter (7.14.6.3)
                let hev = or(gt(ap, splat(t.thresh)), gt(aq, splat(t.thresh)));
                let (lo, hi, off) = (splat(-t.half), splat(t.half - 1), splat(t.off));
                let (ps1, ps0, qs0, qs1) = (sub(p1, off), sub(p0, off), sub(q0, off), sub(q1, off));
                let mut f = and(hev, clamp(sub(ps1, qs1), lo, hi));
                let d0 = sub(qs0, ps0);
                f = clamp(add(f, add(d0, add(d0, d0))), lo, hi);
                let f1 = sra::<3>(clamp(add(f, splat(4)), lo, hi));
                let f2 = sra::<3>(clamp(add(f, splat(3)), lo, hi));
                x[8] = sel(narrow, add(clamp(sub(qs0, f1), lo, hi), off), q0);
                x[7] = sel(narrow, add(clamp(add(ps0, f2), lo, hi), off), p0);
                let outer = andnot(hev, narrow);
                let f = sra::<1>(add(f1, splat(1)));
                x[9] = sel(outer, add(clamp(sub(qs1, f), lo, hi), off), q1);
                x[6] = sel(outer, add(clamp(add(ps1, f), lo, hi), off), p1);
            }
            // The wide filters read the flat lanes, which the narrow filter left alone.
            if !any(flat) {
                return;
            }
            match size {
                LfSize::Four | LfSize::Vp9Four => {}
                LfSize::Six => put::<2>(x, flat, wide::<2, 1, 3>(x)),
                LfSize::Eight | LfSize::Vp9Eight => put::<3>(x, flat, wide::<3, 0, 3>(x)),
                LfSize::Sixteen | LfSize::Vp9Sixteen => {
                    // flat2: p4..p6 and q4..q6 (VP9: to p7 and q7) are as flat.
                    let far = if size == LfSize::Sixteen { 7 } else { 8 };
                    let mut rough = splat(0);
                    for k in 4..far {
                        rough = or(rough, or(gt(abd(x[8 + k], q0), tf), gt(abd(x[7 - k], p0), tf)));
                    }
                    let (flat2, flat1) = (andnot(rough, flat), and(rough, flat));
                    let eight = wide::<3, 0, 3>(x);
                    if any(flat2) {
                        if size == LfSize::Sixteen {
                            put::<6>(x, flat2, wide::<6, 1, 4>(x));
                        } else {
                            put::<7>(x, flat2, wide::<7, 0, 4>(x));
                        }
                    }
                    put::<3>(x, flat1, eight);
                }
            }
        }

        /// The `2 * N` outputs `o` of a wide filter into taps -N .. N where `m` is set.
        #[target_feature(enable = $feature)]
        fn put<const N: usize>(x: &mut [V; 16], m: V, o: [V; 14]) {
            for i in 0..2 * N {
                x[8 - N + i] = sel(m, o[i], x[8 - N + i]);
            }
        }

        /// Wide filter (AV1 7.14.6.4, VP9 8.8.5.3) with n = N, the 2-weight taps |j| <= N2 and
        /// log2Size = LOG2: the sliding window of the scalar version, over vectors.
        #[target_feature(enable = $feature)]
        fn wide<const N: usize, const N2: usize, const LOG2: i32>(x: &[V; 16]) -> [V; 14] {
            // e[m] = the tap at offset clamp(m - N, 0, 2N + 1) - (N + 1) from q0.
            let mut e = [splat(0); 29];
            for (m, v) in e[..4 * N + 1].iter_mut().enumerate() {
                *v = x[7 - N + m.saturating_sub(N).min(2 * N + 1)];
            }
            let mut f = [splat(0); 14];
            let mut win = splat(0);
            for v in &e[1..2 * N + 2] {
                win = add(win, *v);
            }
            for i in 0..2 * N {
                if i > 0 {
                    win = add(win, sub(e[i + 1 + 2 * N], e[i]));
                }
                let mut sum = add(win, splat(1 << (LOG2 - 1)));
                for v in &e[i + 1 + N - N2..=i + 1 + N + N2] {
                    sum = add(sum, *v);
                }
                f[i] = srl::<LOG2>(sum);
            }
            f
        }
    };
}

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
pub(crate) use deblock_kernels;
