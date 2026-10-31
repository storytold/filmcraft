//! Hand-written SIMD kernels for the codecs' hot loops.
//!
//! Each kernel has a scalar version and one per architecture, chosen when the crate is built:
//!
//! - `aarch64` (Apple silicon, Windows and Linux on ARM): NEON, which every ARMv8-A core has;
//! - `x86_64`: SSE2, which every x86-64 core has;
//! - anything else (wasm32 among them): the scalar version.
//!
//! No path needs a runtime CPU check, so there is no dispatch cost and no code that only some
//! machines run. Every version gives the same bits as the scalar one; the tests check each
//! available version against it, and `FILMCRAFT_SIMD=scalar` forces the scalar one at run time
//! (to compare speed, or to rule the vector code out when chasing a bug).
//!
//! `unsafe` is confined to the architecture modules: entering a kernel compiled for the target's
//! baseline vector set, and vector loads and stores of fixed-size arrays. Kernels that are long
//! enough to share between architectures (the loop filter) are written once as a macro over a
//! handful of vector operations each module defines.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

mod deblock;
mod h264;
#[cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
mod hevc_itx;
mod scalar;
mod vp9;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod av1_itx_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod av1_mc_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod h264_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod hevc_dbk_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod hevc_itx_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod hevc_sao_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod hevc_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod vp8_lf_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod vp8_mc_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod vp8_itx_neon;

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
mod vp9_neon;

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
mod sse2;

use std::sync::OnceLock;

pub use deblock::{LfSize, LfThresh};

/// The instruction set a kernel runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Scalar,
    Neon,
    Sse2,
}

impl Level {
    /// The vector instruction set of this build's target, if the crate has kernels for it.
    pub const NATIVE: Level = if cfg!(target_arch = "aarch64") {
        Level::Neon
    } else if cfg!(target_arch = "x86_64") {
        Level::Sse2
    } else {
        Level::Scalar
    };
}

/// The level the kernels use: [`Level::NATIVE`], or scalar when `FILMCRAFT_SIMD=scalar`.
pub fn level() -> Level {
    static LEVEL: OnceLock<Level> = OnceLock::new();
    *LEVEL.get_or_init(|| if std::env::var("FILMCRAFT_SIMD").is_ok_and(|v| v.eq_ignore_ascii_case("scalar")) { Level::Scalar } else { Level::NATIVE })
}

/// The levels this build can run: scalar, and the native one when it differs.
pub fn levels() -> &'static [Level] {
    if Level::NATIVE == Level::Scalar { &[Level::Scalar] } else { &[Level::Scalar, Level::NATIVE] }
}

/// AV1 CDEF direction search (7.15.2) of an 8x8 block of samples of a `bit_depth - 8` = `shift`
/// bit deeper picture: the direction (0..8) and the variance term `(best - opposite) >> 10`.
pub fn cdef_direction(rows: &[&[u16; 8]; 8], shift: u32) -> (usize, i32) {
    cdef_direction_at(level(), rows, shift)
}

/// [`cdef_direction`] on a given level; a level this build has no kernels for runs scalar.
pub fn cdef_direction_at(level: Level, rows: &[&[u16; 8]; 8], shift: u32) -> (usize, i32) {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => neon::cdef_direction(rows, shift),
        #[cfg(target_arch = "x86_64")]
        Level::Sse2 => sse2::cdef_direction(rows, shift),
        _ => scalar::cdef_direction(rows, shift),
    }
}

/// AV1 loop filter (7.14.6) of one line across an edge: `d[base]` is q0 and `step` the distance
/// between samples across the edge. Scalar on every level (it filters lines the vector kernels
/// can't reach, such as those copied out across a band edge).
pub fn deblock_line(d: &mut [u16], base: usize, step: usize, size: LfSize, t: &LfThresh) {
    scalar::deblock_line(d, base, step, size, t);
}

/// AV1 loop filter (7.14.6) of `lines` (a multiple of 4) lines across a vertical edge with the
/// same filter: `d[base]` is q0 of the first line, and each next line starts `stride` further.
pub fn deblock_vertical(d: &mut [u16], base: usize, stride: usize, lines: usize, size: LfSize, t: &LfThresh) {
    deblock_vertical_at(level(), d, base, stride, lines, size, t);
}

/// [`deblock_vertical`] on a given level.
pub fn deblock_vertical_at(level: Level, d: &mut [u16], base: usize, stride: usize, lines: usize, size: LfSize, t: &LfThresh) {
    deblock_lines(lines, |first, n| {
        // An offset past `usize` is no line of `d`: nothing to filter.
        let Some(at) = first.checked_mul(stride).and_then(|o| base.checked_add(o)) else { return };
        let done = match level {
            #[cfg(target_arch = "aarch64")]
            Level::Neon => neon::deblock_vertical(d, at, stride, n, 0xFF, size, t),
            #[cfg(target_arch = "x86_64")]
            Level::Sse2 => sse2::deblock_vertical(d, at, stride, n, 0xFF, size, t),
            _ => false,
        };
        // Scalar, or a vector kernel's 8-sample rows don't all fit in `d`.
        if !done {
            for line in (0..n).filter_map(|i| i.checked_mul(stride).and_then(|o| at.checked_add(o))) {
                scalar::deblock_line(d, line, 1, size, t);
            }
        }
    });
}

/// AV1 loop filter (7.14.6) of `lines` (a multiple of 4) lines across a horizontal edge with
/// the same filter: `d[base]` is q0 of the first line, the others follow it, and `stride` is
/// the distance between rows.
pub fn deblock_horizontal(d: &mut [u16], base: usize, stride: usize, lines: usize, size: LfSize, t: &LfThresh) {
    deblock_horizontal_at(level(), d, base, stride, lines, size, t);
}

/// [`deblock_horizontal`] on a given level.
pub fn deblock_horizontal_at(level: Level, d: &mut [u16], base: usize, stride: usize, lines: usize, size: LfSize, t: &LfThresh) {
    deblock_lines(lines, |first, n| {
        let Some(at) = base.checked_add(first) else { return };
        let done = match level {
            #[cfg(target_arch = "aarch64")]
            Level::Neon => neon::deblock_horizontal(d, at, stride, n, 0xFF, size, t),
            #[cfg(target_arch = "x86_64")]
            Level::Sse2 => sse2::deblock_horizontal(d, at, stride, n, 0xFF, size, t),
            _ => false,
        };
        if !done {
            for line in (0..n).filter_map(|i| at.checked_add(i)) {
                scalar::deblock_line(d, line, stride, size, t);
            }
        }
    });
}

/// VP9 loop filter (8.8.5) of a group of `n` (4 or 8) lines across one edge with the same filter
/// (`size` one of the `Vp9` ones): `d[base]` is q0 of the first line, the next lines follow it
/// (`vertical`: one `stride` below each other; otherwise one sample apart, rows `stride` apart),
/// and line l is filtered where bit l of `on` is set.
pub fn vp9_deblock(d: &mut [u16], base: usize, stride: usize, vertical: bool, (n, on): (usize, u8), (size, t): (LfSize, &LfThresh)) {
    vp9_deblock_at(level(), d, base, stride, vertical, (n, on), (size, t));
}

/// [`vp9_deblock`] on a given level.
pub fn vp9_deblock_at(level: Level, d: &mut [u16], base: usize, stride: usize, vertical: bool, (n, on): (usize, u8), (size, t): (LfSize, &LfThresh)) {
    let n: usize = if n >= 8 { 8 } else { 4 };
    let done = match (level, vertical) {
        #[cfg(target_arch = "aarch64")]
        (Level::Neon, true) => neon::deblock_vertical(d, base, stride, n, on, size, t),
        #[cfg(target_arch = "aarch64")]
        (Level::Neon, false) => neon::deblock_horizontal(d, base, stride, n, on, size, t),
        #[cfg(target_arch = "x86_64")]
        (Level::Sse2, true) => sse2::deblock_vertical(d, base, stride, n, on, size, t),
        #[cfg(target_arch = "x86_64")]
        (Level::Sse2, false) => sse2::deblock_horizontal(d, base, stride, n, on, size, t),
        _ => false,
    };
    if !done {
        let (along, across) = if vertical { (stride, 1) } else { (1, stride) };
        for line in (0..n).filter(|l| on >> l & 1 == 1).filter_map(|l| l.checked_mul(along).and_then(|o| base.checked_add(o))) {
            scalar::deblock_line(d, line, across, size, t);
        }
    }
}

/// `lines` in chunks for the vector kernels: (first line, 8 or 4 lines).
#[inline(always)]
fn deblock_lines(lines: usize, mut chunk: impl FnMut(usize, usize)) {
    let mut first = 0;
    while first + 4 <= lines {
        let n = if first + 8 <= lines { 8 } else { 4 };
        chunk(first, n);
        first += n;
    }
}

/// Row stride of the padded block CDEF filters from: a block of at most 8x8 samples with 2
/// more on every side.
pub const CDEF_STRIDE: usize = 12;

/// One CDEF tap (7.15.3): where its sample is (the offset from the filtered sample in the
/// padded block), its weight, its strength and its damping shift.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CdefTap {
    pub off: isize,
    pub weight: i16,
    pub strength: i16,
    pub shift: u32,
}

/// CDEF (7.15.3) of the `w` x `h` block at row 2, column 2 of the padded block `buf`, with
/// every tap available: `x + Round(sum of weight * constrain(tap - x, strength, shift) / 16)`,
/// clamped to the range of `x` and its taps when `clamp`. `out` gets the samples row after
/// row (stride `w`). Samples are at most 12 bits and the weighted strengths of all taps sum
/// to less than 2^15 (AV1's reach 11520). A tap outside `buf` counts as `x`. The vector
/// kernels take AV1's 8x8, 8x4, 4x8 and 4x4 blocks (4-wide ones two rows to a vector).
pub fn cdef_filter(buf: &[u16; CDEF_STRIDE * CDEF_STRIDE], w: usize, h: usize, taps: &[CdefTap], clamp: bool, out: &mut [u16; 64]) {
    cdef_filter_at(level(), buf, w, h, taps, clamp, out);
}

/// [`cdef_filter`] on a given level.
pub fn cdef_filter_at(level: Level, buf: &[u16; CDEF_STRIDE * CDEF_STRIDE], w: usize, h: usize, taps: &[CdefTap], clamp: bool, out: &mut [u16; 64]) {
    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    let vector = matches!((w, h), (8, 8) | (8, 4) | (4, 8) | (4, 4));
    let done = match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon if vector => neon::cdef_filter(buf, w, h, taps, clamp, out),
        #[cfg(target_arch = "x86_64")]
        Level::Sse2 if vector => sse2::cdef_filter(buf, w, h, taps, clamp, out),
        _ => false,
    };
    // Scalar, or a vector kernel found a tap outside `buf`.
    if !done {
        scalar::cdef_filter(buf, w, h, taps, clamp, out);
    }
}

/// One row of the AV1 subpel filter's horizontal pass (7.11.3.4):
/// `out[j] = Round2(sum of taps[t] * src[j + t], shift)`, for as many `j` as `out` holds and
/// `src` has 7 more samples for. Samples are at most 12 bits, and the results must fit an i16
/// (true of AV1's filters after InterRound0).
pub fn subpel_h(src: &[u16], taps: &[i16; 8], out: &mut [i16], shift: u32) {
    subpel_h_at(level(), src, taps, out, shift);
}

/// [`subpel_h`] on a given level.
pub fn subpel_h_at(level: Level, src: &[u16], taps: &[i16; 8], out: &mut [i16], shift: u32) {
    // The kernels may then rely on `src` having `out.len() + 7` samples.
    let n = out.len().min(src.len().saturating_sub(7));
    let (Some(src), Some(out)) = (src.get(..(n + 7).min(src.len())), out.get_mut(..n)) else {
        return;
    };
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => neon::subpel_h(src, taps, out, shift),
        #[cfg(target_arch = "x86_64")]
        Level::Sse2 => sse2::subpel_h(src, taps, out, shift),
        _ => scalar::subpel_h(src, taps, out, shift),
    }
}

/// One row of the AV1 subpel filter's vertical pass (7.11.3.4):
/// `out[j] = Round2(sum of taps[t] * rows[t * stride + j], shift)`; rows past the end of `rows`
/// count as zero.
pub fn subpel_v(rows: &[i16], stride: usize, taps: &[i16; 8], out: &mut [i32], shift: u32) {
    subpel_v_at(level(), rows, stride, taps, out, shift);
}

/// The AV1 inverse transform's row pass input, 8 rows at a time: `out[j][l]` = `src[l * stride
/// + j]` narrowed to 16 bits (wrapping; dequantised 8-bit coefficients fit), for the `cols`
/// (a multiple of 8) columns. Returns false, writing nothing, when `src` or `out` is too short.
pub fn av1_rows_to_lanes(src: &[i32], stride: usize, cols: usize, out: &mut [[i16; 8]]) -> bool {
    av1_rows_to_lanes_at(level(), src, stride, cols, out)
}

/// [`av1_rows_to_lanes`] on a given level.
pub fn av1_rows_to_lanes_at(level: Level, src: &[i32], stride: usize, cols: usize, out: &mut [[i16; 8]]) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => av1_itx_neon::rows_to_lanes(src, stride, cols, out),
        _ => scalar::av1_rows_to_lanes(src, stride, cols, out),
    }
}

/// The AV1 inverse transform's row pass output, 8 rows at a time: `dst[l * stride + j]` =
/// Round2(`t[j][l]`, `shift`) for the `cols` (a multiple of 8) columns. Returns false, writing
/// nothing, when `t` or `dst` is too short or `shift` > 16.
pub fn av1_lanes_to_rows(t: &[[i16; 8]], cols: usize, shift: u32, dst: &mut [i32], stride: usize) -> bool {
    av1_lanes_to_rows_at(level(), t, cols, shift, dst, stride)
}

/// [`av1_lanes_to_rows`] on a given level.
pub fn av1_lanes_to_rows_at(level: Level, t: &[[i16; 8]], cols: usize, shift: u32, dst: &mut [i32], stride: usize) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => av1_itx_neon::lanes_to_rows(t, cols, shift, dst, stride),
        _ => scalar::av1_lanes_to_rows(t, cols, shift, dst, stride),
    }
}

/// Where [`av1_mc`] writes a block's prediction.
pub enum Av1McOut<'a> {
    /// A single prediction, into the picture: Round2(sum, InterRound1) clamped to 0..=max.
    Put { dst: &'a mut [u16], ds: usize, max: u16 },
    /// One of a compound's predictions: Round2(sum, InterRound1), rows `os` apart.
    Pred { out: &'a mut [i32], os: usize },
    /// The second prediction of a compound average, into the picture: Round2(first + this
    /// prediction, `shift`) clamped to 0..=max, `first` the first prediction (rows `fs` apart).
    Avg { first: &'a [i32], fs: usize, dst: &'a mut [u16], ds: usize, max: u16, shift: u32 },
}

/// AV1 block inter prediction without reference scaling (7.11.3.4) of a `w` x `h` block (`w` 4
/// to 128, a power of two; `h` 1 to 128; `size` (`w`, `h`)) of `bit_depth` samples: the
/// horizontal filter `hf` rounded by InterRound0 = `r0` (<= 7), then the vertical filter `vf`
/// rounded by InterRound1 = `r1` (`filters` (`hf`, `vf`), `rnd` (`r0`, `r1`)), written as `out`
/// says. `src` is (the reference from (x - 3, y - 3), its row stride `ss`), every
/// sample the filters read inside the reference (no edge clamping). The vector version reads 16
/// samples from each strip of 8 columns (one of 4 for 4-wide blocks), h + 7 rows, so it needs
/// `(h + 6) * ss + w + 8` samples (`+ 12` for 4-wide). Returns false, writing nothing, when no
/// vector version runs (on the scalar level, for other sizes, or when the loads or stores do not
/// fit); the caller then runs its scalar version.
pub fn av1_mc(src: (&[u16], usize), filters: (&[i16; 8], &[i16; 8]), size: (usize, usize), bit_depth: u32, rnd: (u32, u32), out: Av1McOut) -> bool {
    av1_mc_at(level(), src, filters, size, bit_depth, rnd, out)
}

/// [`av1_mc`] on a given level.
pub fn av1_mc_at(
    level: Level,
    (src, ss): (&[u16], usize),
    (hf, vf): (&[i16; 8], &[i16; 8]),
    (w, h): (usize, usize),
    bit_depth: u32,
    (r0, r1): (u32, u32),
    out: Av1McOut,
) -> bool {
    let wrap = bit_depth == 8 && r0 == 3 && vp9_wraps(hf);
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => av1_mc_neon::mc((src, ss), (hf, vf), (w, h), wrap, (r0, r1), out),
        _ => {
            let _ = (src, ss, vf, w, h, r1, out, wrap);
            false
        }
    }
}

/// Whether `rows` holds 8 rows of `n` samples `stride` apart: the vector kernels need them all
/// (the scalar one reads missing rows as zero).
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
fn whole_rows(rows: &[i16], stride: usize, n: usize) -> bool {
    n <= stride && stride.checked_mul(7).and_then(|s| s.checked_add(n)).is_some_and(|end| end <= rows.len())
}

/// [`subpel_v`] on a given level.
pub fn subpel_v_at(level: Level, rows: &[i16], stride: usize, taps: &[i16; 8], out: &mut [i32], shift: u32) {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon if whole_rows(rows, stride, out.len()) => neon::subpel_v(rows, stride, taps, out, shift),
        #[cfg(target_arch = "x86_64")]
        Level::Sse2 if whole_rows(rows, stride, out.len()) => sse2::subpel_v(rows, stride, taps, out, shift),
        _ => scalar::subpel_v(rows, stride, taps, out, shift),
    }
}

/// Weight of each line sum's square in its direction's cost (7.15.2), lane by lane: 840 / n for
/// a line of n samples (`DIV_TABLE`), zero past the direction's last line.
const fn cdef_cost_weights() -> [[i32; 16]; 8] {
    const DIV: [i32; 9] = [0, 840, 420, 280, 210, 168, 140, 120, 105];
    let mut w = [[0i32; 16]; 8];
    let mut k = 0;
    while k < 8 {
        w[2][k] = DIV[8];
        w[6][k] = DIV[8];
        k += 1;
    }
    let mut i = 0;
    while i < 7 {
        w[0][i] = DIV[i + 1];
        w[0][14 - i] = DIV[i + 1];
        w[4][i] = DIV[i + 1];
        w[4][14 - i] = DIV[i + 1];
        i += 1;
    }
    w[0][7] = DIV[8];
    w[4][7] = DIV[8];
    let mut d = 1;
    while d < 8 {
        let mut j = 0;
        while j < 5 {
            w[d][3 + j] = DIV[8];
            j += 1;
        }
        let mut j = 0;
        while j < 3 {
            w[d][j] = DIV[2 * j + 2];
            w[d][10 - j] = DIV[2 * j + 2];
            j += 1;
        }
        d += 2;
    }
    w
}

static CDEF_COST_WEIGHTS: [[i32; 16]; 8] = cdef_cost_weights();

/// The best direction and its variance term from the eight costs (first maximum wins).
#[inline]
fn cdef_pick(cost: &[i32; 8]) -> (usize, i32) {
    let mut best = 0;
    let mut dir = 0;
    for (i, &c) in cost.iter().enumerate() {
        if c > best {
            best = c;
            dir = i;
        }
    }
    (dir, (best - cost[(dir + 4) & 7]) >> 10)
}

/// How an H.264 prediction is written into the picture (8.4.2.3), for 8-bit video: weights
/// -128..=128 (128 is the default weight for logWD 7 and an implicit one), offsets -128..=127,
/// `log_wd` (logWD) up to 7. The prediction functions leave `dst` unchanged for other values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum H264Out {
    /// `dst = p`.
    Put,
    /// Default bi-prediction, `dst` holding the first prediction: `dst = (dst + p + 1) >> 1`.
    Avg,
    /// A weighted single prediction: `Clip1(((p * w + 2^(log_wd - 1)) >> log_wd) + o)`
    /// (`Clip1(p * w + o)` for `log_wd` 0).
    Weight { w: i32, o: i32, log_wd: u32 },
    /// Weighted bi-prediction, `dst` holding the first prediction:
    /// `Clip1(((dst * w0 + p * w1 + 2^log_wd) >> (log_wd + 1)) + o)`, `o` = (o0 + o1 + 1) >> 1.
    WeightBi { w0: i32, w1: i32, o: i32, log_wd: u32 },
}

/// Whether `out`'s weights, offset and `log_wd` are in 8-bit H.264's range (see [`H264Out`]).
fn h264_out_valid(out: H264Out) -> bool {
    let weight = |v: i32| (-128..=128).contains(&v);
    let offset = |v: i32| (-128..=127).contains(&v);
    match out {
        H264Out::Put | H264Out::Avg => true,
        H264Out::Weight { w, o, log_wd } => weight(w) && offset(o) && log_wd <= 7,
        H264Out::WeightBi { w0, w1, o, log_wd } => weight(w0) && weight(w1) && offset(o) && log_wd <= 7,
    }
}

/// Whether a `ww` x `wh` window (rows `ws` apart) fits `win` and the `w` x `h` block (rows `ds`
/// apart) fits `dst`.
fn block_fits<T>((win, ws): (&[T], usize), (ww, wh): (usize, usize), (dst, ds): (&[T], usize), (w, h): (usize, usize)) -> bool {
    let need = |len: usize, stride: usize, cols: usize, rows: usize| {
        rows == 0 || (rows - 1).checked_mul(stride).and_then(|n| n.checked_add(cols)).is_some_and(|n| n <= len)
    };
    need(win.len(), ws, ww, wh) && need(dst.len(), ds, w, h)
}

/// H.264 luma prediction (8.4.2.2.1, 8.4.2.3) of a `w` x `h` block (`w` and `h` 4, 8 or 16),
/// written into `dst` (rows `ds` apart) as `out` says. `win` holds the (w + 5) x (h + 5)
/// reference window from (x - 2, y - 2), rows `ws` apart; (`fx`, `fy`) is the quarter-sample
/// fraction. The vector version reads 16 (8 for blocks up to 8 wide) samples from up to 5 past
/// each window row's start, so it needs `(h + 4) * ws + 21` (`+ 13`) of them and otherwise the
/// scalar one runs. Blocks that do not fit leave `dst` unchanged.
pub fn h264_luma((win, ws): (&[u8], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): (&mut [u8], usize), out: H264Out) {
    h264_luma_at(level(), (win, ws), (fx, fy), (w, h), (dst, ds), out);
}

/// [`h264_luma`] on a given level.
pub fn h264_luma_at(level: Level, (win, ws): (&[u8], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): (&mut [u8], usize), out: H264Out) {
    if !h264_out_valid(out) || !matches!(w, 4 | 8 | 16) || !matches!(h, 4 | 8 | 16) || fx > 3 || fy > 3 || !block_fits((win, ws), (w + 5, h + 5), (&*dst, ds), (w, h)) {
        return;
    }
    let done = match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => h264_neon::luma((win, ws), (fx, fy), (w, h), (dst, ds), out),
        _ => false,
    };
    if !done {
        h264::luma_out((win, ws), (fx, fy), (w, h), (dst, ds), out);
    }
}

/// H.264 chroma prediction (8.4.2.2.2, 8.4.2.3) of a `w` x `h` block (`w` and `h` 2, 4 or 8)
/// into `dst` as `out` says. `win` holds the (w + 1) x (h + 1) window from the block's integer
/// position, rows `ws` apart (the vector version reads 9 samples from each row's start: it needs
/// `h * ws + 9`); (`fx`, `fy`) is the eighth-sample fraction.
pub fn h264_chroma((win, ws): (&[u8], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): (&mut [u8], usize), out: H264Out) {
    h264_chroma_at(level(), (win, ws), (fx, fy), (w, h), (dst, ds), out);
}

/// [`h264_chroma`] on a given level.
pub fn h264_chroma_at(level: Level, (win, ws): (&[u8], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): (&mut [u8], usize), out: H264Out) {
    if !h264_out_valid(out) || !matches!(w, 2 | 4 | 8) || !matches!(h, 2 | 4 | 8) || fx > 7 || fy > 7 || !block_fits((win, ws), (w + 1, h + 1), (&*dst, ds), (w, h)) {
        return;
    }
    let done = match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => h264_neon::chroma((win, ws), (fx, fy), (w, h), (dst, ds), out),
        _ => false,
    };
    if !done {
        h264::chroma_out((win, ws), (fx, fy), (w, h), (dst, ds), out);
    }
}

/// One H.264 deblocking edge (8.7.2): bS and tC0 for each quarter of its lines, and alpha /
/// beta (8-bit video: alpha < 256, beta < 19, tC0 < 26). bS 4 applies to a whole edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct H264Edge {
    pub bs: [u8; 4],
    pub tc0: [u8; 4],
    pub alpha: u8,
    pub beta: u8,
}

/// [`h264_chroma`] of the same block of both chroma planes, which share the motion vector, so the
/// fraction, window stride and destination stride: `win[0]` / `dst[0]` Cb, `win[1]` / `dst[1]`
/// Cr, both written as `out` (the vector version runs them as one).
pub fn h264_chroma_pair((win, ws): ([&[u8]; 2], usize), (fx, fy): (u32, u32), (w, h): (usize, usize), (dst, ds): ([&mut [u8]; 2], usize), out: H264Out) {
    h264_chroma_pair_at(level(), (win, ws), (fx, fy), (w, h), (dst, ds), out);
}

/// [`h264_chroma_pair`] on a given level.
pub fn h264_chroma_pair_at(
    level: Level,
    (win, ws): ([&[u8]; 2], usize),
    (fx, fy): (u32, u32),
    (w, h): (usize, usize),
    (dst, ds): ([&mut [u8]; 2], usize),
    out: H264Out,
) {
    let [db, dr] = dst;
    let fit = |win: &[u8], dst: &[u8]| block_fits((win, ws), (w + 1, h + 1), (dst, ds), (w, h));
    if !h264_out_valid(out) || !matches!(w, 2 | 4 | 8) || !matches!(h, 2 | 4 | 8) || fx > 7 || fy > 7 || !fit(win[0], db) || !fit(win[1], dr) {
        return;
    }
    let done = match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => h264_neon::chroma_pair((win, ws), (fx, fy), (w, h), ([&mut *db, &mut *dr], ds), out),
        _ => false,
    };
    if !done {
        h264::chroma_out((win[0], ws), (fx, fy), (w, h), (db, ds), out);
        h264::chroma_out((win[1], ws), (fx, fy), (w, h), (dr, ds), out);
    }
}

/// H.264 deblocking (8.7.2) of one edge of 16 luma or 8 chroma (`chroma`) lines of the 8-bit
/// plane `pix` (rows `stride` apart): a vertical edge (`vertical`) whose q0 column is `x`, for
/// the lines from row `y`, or a horizontal edge whose q0 row is `y`, for the columns from `x`.
/// Edges whose samples do not fit `pix` are left unchanged.
pub fn h264_deblock(pix: &mut [u8], stride: usize, (x, y): (usize, usize), vertical: bool, chroma: bool, e: &H264Edge) {
    h264_deblock_at(level(), pix, stride, (x, y), vertical, chroma, e);
}

/// [`h264_deblock`] on a given level.
pub fn h264_deblock_at(level: Level, pix: &mut [u8], stride: usize, (x, y): (usize, usize), vertical: bool, chroma: bool, e: &H264Edge) {
    let done = match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => h264_neon::deblock(pix, stride, x, y, vertical, chroma, e),
        _ => false,
    };
    if done {
        return;
    }
    match (vertical, chroma) {
        (true, false) => h264::filter_vertical::<16>(pix, stride, x, y, e, false),
        (true, true) => h264::filter_vertical::<8>(pix, stride, x, y, e, true),
        (false, false) => h264::filter_horizontal::<16>(pix, stride, x, y, e, false),
        (false, true) => h264::filter_horizontal::<8>(pix, stride, x, y, e, true),
    }
}

/// [`h264_deblock`] of the same chroma edge of the Cb plane `cb` and the Cr plane `cr`, which
/// share `stride` and the edge parameters `e` (their chroma QPs are equal): one vector pass for
/// both.
pub fn h264_deblock_chroma_pair(cb: &mut [u8], cr: &mut [u8], stride: usize, (x, y): (usize, usize), vertical: bool, e: &H264Edge) {
    h264_deblock_chroma_pair_at(level(), cb, cr, stride, (x, y), vertical, e);
}

/// [`h264_deblock_chroma_pair`] on a given level.
pub fn h264_deblock_chroma_pair_at(level: Level, cb: &mut [u8], cr: &mut [u8], stride: usize, (x, y): (usize, usize), vertical: bool, e: &H264Edge) {
    let done = match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => h264_neon::deblock_pair(cb, cr, stride, x, y, vertical, e),
        _ => false,
    };
    if !done {
        h264_deblock_at(level, cb, stride, (x, y), vertical, true, e);
        h264_deblock_at(level, cr, stride, (x, y), vertical, true, e);
    }
}

#[cfg(test)]
mod tests;

/// Where an HEVC inter prediction of 8-bit samples goes (8.5.3.3.4.2, default weighting).
pub enum HevcDst<'a> {
    /// The 14-bit prediction samples themselves (predSamplesLX of 8.5.3.3.3), rows `w` apart:
    /// the first prediction of a bi-predicted block, or the input of explicit weighting.
    Pred(&'a mut [i16]),
    /// Uni-prediction into the picture (rows `.1` apart): Clip3(0, 255, (p + 32) >> 6) (8-262).
    Put(&'a mut [u8], usize),
    /// Bi-prediction into the picture (rows `.2` apart) with the first prediction `.0` (rows `w`
    /// apart): Clip3(0, 255, (p0 + p + 64) >> 7) (8-263).
    Bi(&'a [i16], &'a mut [u8], usize),
}

/// HEVC luma sample interpolation (8.5.3.3.3.1) of a `w` x `h` block of 8-bit samples, written
/// as `dst` says. `rows` are the h + 7 window rows from (x - 3, y - 3), each starting at the
/// window's first column (the reference picture's own lines, or an edge-replicated copy); `fh`
/// and `fv` are the filters of the horizontal and vertical fractions (None at whole samples).
/// Returns false, writing nothing, when no vector version runs: on the scalar level, for widths
/// that are not a multiple of 4, or when a row holds fewer than 16 samples past the start of the
/// block's last strip of 8 (or 4) columns; the caller then runs its scalar version.
pub fn hevc_luma8(rows: &[&[u8]], fh: Option<&[i8; 8]>, fv: Option<&[i8; 8]>, w: usize, h: usize, dst: HevcDst) -> bool {
    hevc_luma8_at(level(), rows, fh, fv, w, h, dst)
}

/// [`hevc_luma8`] on a given level.
pub fn hevc_luma8_at(level: Level, rows: &[&[u8]], fh: Option<&[i8; 8]>, fv: Option<&[i8; 8]>, w: usize, h: usize, dst: HevcDst) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => hevc_neon::predict::<8>(rows, fh, fv, w, h, dst),
        _ => {
            let _ = (rows, fh, fv, w, h, dst);
            false
        }
    }
}

/// The two 4-line segments of an 8-line HEVC luma edge (8.7.2.5): whether each is filtered
/// (bS > 0 and the edge enabled), its tC and beta (8.7.2.5.3, for 8-bit samples), and whether its
/// p or q samples are left as they are (pcm_loop_filter_disabled_flag, cu_transquant_bypass).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HevcLumaEdge {
    pub on: [bool; 2],
    pub tc: [i32; 2],
    pub beta: [i32; 2],
    pub no_p: [bool; 2],
    pub no_q: [bool; 2],
}

/// HEVC luma deblocking (8.7.2.5.3 decisions, 8.7.2.5.7 filtering) of an 8-line edge of 8-bit
/// samples: q0 of the first line at `d[o]`, the lines `stride` apart down a vertical edge
/// (`vertical`, p0 at `o - 1`) or 1 apart along a horizontal one (p0 at `o - stride`). Returns
/// false, writing nothing, when no vector version runs (on the scalar level, or when p3..q3 of
/// the lines are not all in `d`); the caller then runs its scalar version.
pub fn hevc_deblock_luma8(d: &mut [u8], o: usize, stride: usize, vertical: bool, e: &HevcLumaEdge) -> bool {
    hevc_deblock_luma8_at(level(), d, o, stride, vertical, e)
}

/// [`hevc_deblock_luma8`] on a given level.
pub fn hevc_deblock_luma8_at(level: Level, d: &mut [u8], o: usize, stride: usize, vertical: bool, e: &HevcLumaEdge) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => hevc_dbk_neon::luma(d, o, stride, vertical, e),
        _ => {
            let _ = (d, o, stride, vertical, e);
            false
        }
    }
}

/// HEVC SAO band offset (8.7.3, SaoTypeIdx 1) of a run of 8-bit samples `src` into `dst`:
/// `table[band]` (band = sample >> 3; the four offsets at their bands, 0 elsewhere) added with
/// Clip1. Returns false, writing nothing, when no vector version runs (on the scalar level, runs
/// shorter than 16, offsets outside -128..=127, or `dst` shorter than `src`).
pub fn hevc_sao_band8(src: &[u8], dst: &mut [u8], table: &[i32; 32]) -> bool {
    hevc_sao_band8_at(level(), src, dst, table)
}

/// [`hevc_sao_band8`] on a given level.
pub fn hevc_sao_band8_at(level: Level, src: &[u8], dst: &mut [u8], table: &[i32; 32]) -> bool {
    let mut t = [0i8; 32];
    for (t, &v) in t.iter_mut().zip(table) {
        let Ok(v) = i8::try_from(v) else { return false };
        *t = v;
    }
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => hevc_sao_neon::band(src, dst, &t),
        _ => {
            let _ = (src, dst, t);
            false
        }
    }
}

/// HEVC SAO edge offset (8.7.3, SaoTypeIdx 2) of a run of 8-bit samples `cur` with their two
/// neighbours along the edge class `a` and `b` (all usable) into `dst`: `offsets[2 + sum]` added
/// with Clip1, sum the signs of `cur - a` and `cur - b` (`offsets[2]`, edgeIdx 0, is 0). Returns
/// false, writing nothing, as [`hevc_sao_band8`] does.
pub fn hevc_sao_edge8(cur: &[u8], a: &[u8], b: &[u8], dst: &mut [u8], offsets: &[i32; 5]) -> bool {
    hevc_sao_edge8_at(level(), cur, a, b, dst, offsets)
}

/// [`hevc_sao_edge8`] on a given level.
pub fn hevc_sao_edge8_at(level: Level, cur: &[u8], a: &[u8], b: &[u8], dst: &mut [u8], offsets: &[i32; 5]) -> bool {
    let mut o = [0i8; 5];
    for (o, &v) in o.iter_mut().zip(offsets) {
        let Ok(v) = i8::try_from(v) else { return false };
        *o = v;
    }
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => hevc_sao_neon::edge(cur, a, b, dst, &o),
        _ => {
            let _ = (cur, a, b, dst, o);
            false
        }
    }
}

/// VP8 inverse DCT (14.4) of the 4x4 block `coefs` (raster order), added to the 8-bit
/// prediction at `dst[pos..]` (rows `stride` apart) saturating (14.5). Returns false, writing
/// nothing, when no vector version runs (on the scalar level, or when the block is not all in
/// `dst`); the caller then runs its scalar version.
pub fn vp8_idct_add(coefs: &[i16; 16], dst: &mut [u8], pos: usize, stride: usize) -> bool {
    vp8_idct_add_at(level(), coefs, dst, pos, stride)
}

/// [`vp8_idct_add`] on a given level.
pub fn vp8_idct_add_at(level: Level, coefs: &[i16; 16], dst: &mut [u8], pos: usize, stride: usize) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => vp8_itx_neon::idct_add(coefs, dst, pos, stride),
        _ => {
            let _ = (coefs, dst, pos, stride);
            false
        }
    }
}

/// VP8 inter prediction (18.3) of a `w` x `h` block (`size`; `w` 16, 8 or 4, `h` 1..=16) of 8-bit
/// pixels into `dst` (`dst`, rows `ds` apart). `src` is (`src`, `ss`): the reference window from
/// (x - 2, y - 2), rows `ss` apart (the reference itself, read in place); `filters` (`fh`, `fv`)
/// are the six-tap (or bilinear, as six taps) filters of the horizontal and vertical fractions
/// (None at whole pixels). Each pass is
/// (sum + 64) >> 7 clipped to 0..=255. Returns false, writing nothing, when no vector version runs:
/// on the scalar level, for filters whose sums could leave 16 bits ([`vp8_wraps`]), or when the
/// vector loads (16 pixels from each strip of 8 columns, h + 5 rows) do not fit `src`; the caller
/// then runs its scalar version.
pub fn vp8_predict(src: (&[u8], usize), filters: (Option<&[i16; 6]>, Option<&[i16; 6]>), size: (usize, usize), dst: (&mut [u8], usize)) -> bool {
    vp8_predict_at(level(), src, filters, size, dst)
}

/// [`vp8_predict`] on a given level.
pub fn vp8_predict_at(
    level: Level,
    (src, ss): (&[u8], usize),
    (fh, fv): (Option<&[i16; 6]>, Option<&[i16; 6]>),
    (w, h): (usize, usize),
    (dst, ds): (&mut [u8], usize),
) -> bool {
    if !fh.is_none_or(vp8_wraps) || !fv.is_none_or(vp8_wraps) {
        return false;
    }
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => vp8_mc_neon::predict((src, ss), (fh, fv), (w, h), (dst, ds)),
        _ => {
            let _ = (src, ss, w, h, dst, ds);
            false
        }
    }
}

/// Whether 8-bit sums of the six-tap filter `f` may be kept modulo 2^16: its positive taps add up
/// to at most 192 and its negative ones to at least -64 (VP8's six-tap filters: 160 and -32; the
/// bilinear ones: 128 and 0).
pub fn vp8_wraps(f: &[i16; 6]) -> bool {
    let pos: i32 = f.iter().map(|&c| (c as i32).max(0)).sum();
    let neg: i32 = f.iter().map(|&c| (c as i32).min(0)).sum();
    pos <= 192 && neg >= -64
}

/// A VP8 loop filter (15.2, 15.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vp8Filter {
    /// The simple filter (15.2): p0 and q0 only, no interior or high edge variance test.
    Simple,
    /// The normal subblock (inner) edge filter (15.3 `subblock_filter`).
    Subblock,
    /// The normal macroblock edge filter (15.3 `MBfilter`).
    Macroblock,
}

/// The limits of a VP8 edge (15.2, 15.4): the edge limit (the macroblock or subblock one), and
/// for the normal filters the interior limit and the high edge variance threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vp8Limits {
    pub edge: i32,
    pub interior: i32,
    pub hev_threshold: i32,
}

/// VP8 loop filter of a 16-line luma edge of 8-bit pixels: q0 of the first line at `d[start]`,
/// the lines `stride` apart down a vertical edge (`vertical`, p0 at `start - 1`) or 1 apart
/// along a horizontal one (p0 at `start - stride`). Returns false, writing nothing, when no
/// vector version runs (on the scalar level, or when p3..q3 of the lines are not all in `d`);
/// the caller then runs its scalar version.
pub fn vp8_loop_filter(d: &mut [u8], start: usize, stride: usize, vertical: bool, f: Vp8Filter, l: &Vp8Limits) -> bool {
    vp8_loop_filter_at(level(), d, start, stride, vertical, f, l)
}

/// [`vp8_loop_filter`] on a given level.
pub fn vp8_loop_filter_at(level: Level, d: &mut [u8], start: usize, stride: usize, vertical: bool, f: Vp8Filter, l: &Vp8Limits) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => vp8_lf_neon::luma(d, start, stride, vertical, f, l),
        _ => {
            let _ = (d, start, stride, vertical, f, l);
            false
        }
    }
}

/// [`vp8_loop_filter`] of the same 8-line edge of both chroma planes `u` and `v` (same layout,
/// same limits: VP8 filters them alike).
pub fn vp8_loop_filter_uv(u: &mut [u8], v: &mut [u8], (start, stride): (usize, usize), vertical: bool, f: Vp8Filter, l: &Vp8Limits) -> bool {
    vp8_loop_filter_uv_at(level(), u, v, (start, stride), vertical, f, l)
}

/// [`vp8_loop_filter_uv`] on a given level.
pub fn vp8_loop_filter_uv_at(level: Level, u: &mut [u8], v: &mut [u8], (start, stride): (usize, usize), vertical: bool, f: Vp8Filter, l: &Vp8Limits) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => vp8_lf_neon::chroma(u, v, start, stride, vertical, f, l),
        _ => {
            let _ = (u, v, start, stride, vertical, f, l);
            false
        }
    }
}

/// HEVC inverse transform (8.6.4.2) of an `n` x `n` block (4, 8, 16 or 32; `dst_type`: the
/// 4x4 DST-VII) of scaled coefficients `coefs` (`coefs[y * n + x]`, within 16 bits; larger
/// values saturate, as the scaling process clips them), added to the 8-bit prediction samples
/// in `dst` (rows `ds` apart) with Clip1 (8.6.7). Only coefficient rows up to `max_y` and columns
/// up to `max_x` (`max` (`max_x`, `max_y`)) rounded up to a multiple of 8 (of 4 for 4x4 blocks) are read: the others must be
/// zero, as must those read past `max_x`. Returns false, writing nothing, when no vector version
/// runs (on the scalar level, or for arguments that do not fit); the caller then runs its scalar
/// version.
pub fn hevc_idct_add8(coefs: &[i32], n: usize, dst_type: bool, max: (usize, usize), dst: &mut [u8], ds: usize) -> bool {
    hevc_idct_add8_at(level(), coefs, n, dst_type, max, dst, ds)
}

/// [`hevc_idct_add8`] on a given level.
pub fn hevc_idct_add8_at(level: Level, coefs: &[i32], n: usize, dst_type: bool, (max_x, max_y): (usize, usize), dst: &mut [u8], ds: usize) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => hevc_itx_neon::idct_add(coefs, n, dst_type, max_x, max_y, dst, ds),
        _ => {
            let _ = (coefs, n, dst_type, max_x, max_y, dst, ds);
            false
        }
    }
}

/// Add the residual `r` (of a DC-only block, every sample the same) to the `n` x `n` 8-bit
/// prediction samples in `dst` (rows `ds` apart) with Clip1. Returns false, writing nothing, as
/// [`hevc_idct_add8`] does.
pub fn hevc_dc_add8(r: i32, n: usize, dst: &mut [u8], ds: usize) -> bool {
    hevc_dc_add8_at(level(), r, n, dst, ds)
}

/// [`hevc_dc_add8`] on a given level.
pub fn hevc_dc_add8_at(level: Level, r: i32, n: usize, dst: &mut [u8], ds: usize) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => hevc_itx_neon::dc_add(r, n, dst, ds),
        _ => {
            let _ = (r, n, dst, ds);
            false
        }
    }
}

/// HEVC chroma sample interpolation (8.5.3.3.3.2) of a `w` x `h` block of 8-bit samples, as
/// [`hevc_luma8`] with 4-tap filters: `rows` are the h + 3 window rows from (x - 1, y - 1).
pub fn hevc_chroma8(rows: &[&[u8]], fh: Option<&[i8; 4]>, fv: Option<&[i8; 4]>, w: usize, h: usize, dst: HevcDst) -> bool {
    hevc_chroma8_at(level(), rows, fh, fv, w, h, dst)
}

/// [`hevc_chroma8`] on a given level.
pub fn hevc_chroma8_at(level: Level, rows: &[&[u8]], fh: Option<&[i8; 4]>, fv: Option<&[i8; 4]>, w: usize, h: usize, dst: HevcDst) -> bool {
    match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => hevc_neon::predict::<4>(rows, fh, fv, w, h, dst),
        _ => {
            let _ = (rows, fh, fv, w, h, dst);
            false
        }
    }
}

/// How a VP9 inter prediction is written into the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vp9Out {
    /// `dst = p`.
    Put,
    /// Compound prediction, `dst` holding the first one: `dst = Round2(dst + p, 1)`.
    Avg,
}

/// VP9 inter prediction of a `w` x `h` block (`size`) from an unscaled reference (8.5.2.4 with
/// steps of 16; `w` 4, 8, 16, 32 or 64, `h` 1..=64), written into `dst` (`dst`, rows `ds` apart)
/// as `out` says. `src` (`src`, `ss`) holds the (w + 7) x (h + 7) reference window from
/// (x - 3, y - 3), rows `ss` apart; `filters` (`fh`, `fv`) are the 8-tap filters of the horizontal and vertical sub-sample positions (None at
/// whole-sample positions), and samples are at most `max` (255, 1023 or 4095). Each pass is
/// Round2(sum, 7) clipped to 0..=max. The vector version loads one sample past each window row,
/// so it needs `(h + 6) * ss + w + 8` samples in `src` and otherwise the scalar one runs. Blocks
/// that do not fit leave `dst` unchanged.
pub fn vp9_predict(
    src: (&[u16], usize),
    filters: (Option<&[i16; 8]>, Option<&[i16; 8]>),
    size: (usize, usize),
    max: u16,
    dst: (&mut [u16], usize),
    out: Vp9Out,
) {
    vp9_predict_at(level(), src, filters, size, max, dst, out);
}

/// [`vp9_predict`] on a given level.
pub fn vp9_predict_at(
    level: Level,
    (src, ss): (&[u16], usize),
    (fh, fv): (Option<&[i16; 8]>, Option<&[i16; 8]>),
    (w, h): (usize, usize),
    max: u16,
    (dst, ds): (&mut [u16], usize),
    out: Vp9Out,
) {
    if !matches!(w, 4 | 8 | 16 | 32 | 64) || !(1..=64).contains(&h) || !block_fits((src, ss), (w + 7, h + 7), (&*dst, ds), (w, h)) {
        return;
    }
    let wrap = max == 255 && fh.is_none_or(vp9_wraps) && fv.is_none_or(vp9_wraps);
    let done = match level {
        #[cfg(target_arch = "aarch64")]
        Level::Neon => vp9_neon::predict((src, ss), (fh, fv), (w, h), max, wrap, (&mut *dst, ds), out),
        _ => false,
    };
    if !done {
        vp9::predict((src, ss), (fh, fv), (w, h), max, wrap, (dst, ds), out);
    }
}

/// Whether 8-bit sums of filter `f` may be kept modulo 2^16: its positive taps add up to at most
/// 192 and its negative ones to at least -64, so that each sum plus 64 lies in [-16256, 49024].
fn vp9_wraps(f: &[i16; 8]) -> bool {
    let pos: i32 = f.iter().map(|&c| (c as i32).max(0)).sum();
    let neg: i32 = f.iter().map(|&c| (c as i32).min(0)).sum();
    pos <= 192 && neg >= -64
}
