use super::*;

/// 1 when the NEON kernel call `$call` ran (returned true), 0 when it declined and on other
/// targets, where it is not compiled: for asserting that the vector path of a test ran.
macro_rules! neon_ran {
    ($call:expr) => {{
        #[cfg(target_arch = "aarch64")]
        let ran = $call;
        #[cfg(not(target_arch = "aarch64"))]
        let ran = false;
        ran as usize
    }};
}

/// 7.15.2 as the spec writes it: one sample at a time into the line sums, then the costs.
fn spec_direction(rows: &[&[u16; 8]; 8], shift: u32) -> (usize, i32) {
    const DIV: [i32; 9] = [0, 840, 420, 280, 210, 168, 140, 120, 105];
    let mut partial = [[0i32; 15]; 8];
    for i in 0..8 {
        for j in 0..8 {
            let x = (rows[i][j] >> shift) as i32 - 128;
            partial[0][i + j] += x;
            partial[1][i + j / 2] += x;
            partial[2][i] += x;
            partial[3][3 + i - j / 2] += x;
            partial[4][7 + i - j] += x;
            partial[5][3 - i / 2 + j] += x;
            partial[6][j] += x;
            partial[7][i / 2 + j] += x;
        }
    }
    let mut cost = [0i32; 8];
    for i in 0..8 {
        cost[2] += partial[2][i] * partial[2][i];
        cost[6] += partial[6][i] * partial[6][i];
    }
    cost[2] *= DIV[8];
    cost[6] *= DIV[8];
    for i in 0..7 {
        cost[0] += (partial[0][i] * partial[0][i] + partial[0][14 - i] * partial[0][14 - i]) * DIV[i + 1];
        cost[4] += (partial[4][i] * partial[4][i] + partial[4][14 - i] * partial[4][14 - i]) * DIV[i + 1];
    }
    cost[0] += partial[0][7] * partial[0][7] * DIV[8];
    cost[4] += partial[4][7] * partial[4][7] * DIV[8];
    for d in (1..8).step_by(2) {
        for j in 0..5 {
            cost[d] += partial[d][3 + j] * partial[d][3 + j];
        }
        cost[d] *= DIV[8];
        for j in 0..3 {
            cost[d] += (partial[d][j] * partial[d][j] + partial[d][10 - j] * partial[d][10 - j]) * DIV[2 * j + 2];
        }
    }
    cdef_pick(&cost)
}

/// Random blocks at 8, 10 and 12 bits, the extremes of each depth, and edges along every
/// direction.
fn blocks() -> Vec<([[u16; 8]; 8], u32)> {
    let mut seed = 0x2545_f491u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut out = Vec::new();
    for shift in [0, 2, 4] {
        let max = (256u32 << shift) - 1;
        for _ in 0..10_000 {
            out.push((std::array::from_fn(|_| std::array::from_fn(|_| (next() % (max + 1)) as u16)), shift));
        }
        out.push(([[0; 8]; 8], shift));
        out.push(([[max as u16; 8]; 8], shift));
        for d in 0..8 {
            out.push((
                std::array::from_fn(|i| {
                    std::array::from_fn(|j| {
                        let (i, j) = (i as i32, j as i32);
                        let t = [i + j, 2 * i + j, i, 2 * i - j, i - j, j - 2 * i, j, j + 2 * i][d];
                        if t.rem_euclid(4) < 2 { max as u16 } else { 0 }
                    })
                }),
                shift,
            ));
        }
    }
    out
}

#[test]
fn every_level_matches_the_spec_direction_search() {
    for (block, shift) in blocks() {
        let rows: [&[u16; 8]; 8] = std::array::from_fn(|i| &block[i]);
        let want = spec_direction(&rows, shift);
        for &level in levels() {
            assert_eq!(cdef_direction_at(level, &rows, shift), want, "{level:?}, shift {shift}: {block:?}");
        }
    }
}

#[test]
fn native_level_follows_the_target() {
    let native = if cfg!(target_arch = "aarch64") {
        Level::Neon
    } else if cfg!(target_arch = "x86_64") {
        Level::Sse2
    } else {
        Level::Scalar
    };
    assert_eq!(Level::NATIVE, native);
    assert!(levels().contains(&Level::Scalar) && levels().contains(&native));
}

/// Filter strengths as 7.14.4 derives them from a level and a sharpness.
fn lf_thresh(lvl: i32, sharp: i32, bit_depth: u32) -> LfThresh {
    let shift = if sharp > 4 { 2 } else { (sharp > 0) as i32 };
    let limit = if sharp > 0 { (lvl >> shift).clamp(1, 9 - sharp) } else { (lvl >> shift).max(1) };
    LfThresh::new(limit, 2 * (lvl + 2) + limit, lvl >> 4, bit_depth)
}

/// 32x32 pictures around an edge through the middle: smooth sides (to reach the flat and wide
/// paths) or noisy ones, a step of random height or a random walk across the edge, random
/// strengths.
fn deblock_cases() -> Vec<(Vec<u16>, LfThresh, LfSize, u32, usize)> {
    let mut seed = 0x9e37_79b9u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut out = Vec::new();
    for bd in [8, 10, 12] {
        let max = (1i32 << bd) - 1;
        let unit = 1i32 << (bd - 8);
        for size in [LfSize::Four, LfSize::Six, LfSize::Eight, LfSize::Sixteen, LfSize::Vp9Four, LfSize::Vp9Eight, LfSize::Vp9Sixteen] {
            for _ in 0..3000 {
                let noise = [0, 1, 2, 4, 16, 120][next() as usize % 6] * unit;
                let step = [0, 1, 2, 3, 8, 40, 100][next() as usize % 7] * unit;
                let level = (next() % (max as u32 + 1)) as i32;
                let vertical = next() % 2 == 0;
                // A third of the cases walk randomly across the edge (zigzags reach the narrow
                // filter's clamps); the rest are a step.
                let walk = next() % 3 == 0;
                let mut profile = [level; 32];
                for a in 1..32 {
                    let s = 70 * unit;
                    profile[a] =
                        if walk { (profile[a - 1] + (next() % (2 * s as u32 + 1)) as i32 - s).clamp(0, max) } else { level + if a >= 16 { step } else { 0 } };
                }
                // Lines differ in the size of the step, so lanes take different paths.
                let scale: [i32; 32] = std::array::from_fn(|_| [0, 1, 1, 2][next() as usize % 4]);
                let pic: Vec<u16> = (0..32 * 32)
                    .map(|i| {
                        let (across, line) = if vertical { (i % 32, i / 32) } else { (i / 32, i % 32) };
                        let n = if noise == 0 { 0 } else { (next() % (noise as u32 + 1)) as i32 - noise / 2 };
                        let p = if walk { profile[across] } else { level + if across >= 16 { step * scale[line] } else { 0 } };
                        (p + n).clamp(0, max) as u16
                    })
                    .collect();
                let t = lf_thresh(1 + (next() % 63) as i32, (next() % 8) as i32, bd);
                out.push((pic, t, size, vertical as u32, 4 * (1 + next() as usize % 4)));
            }
        }
    }
    out
}

#[test]
fn every_level_matches_the_scalar_loop_filter() {
    let (mut changed, mut total) = (0, 0);
    for (pic, t, size, vertical, lines) in deblock_cases() {
        let mut want = pic.clone();
        let base = if vertical == 1 { 8 * 32 + 16 } else { 16 * 32 + 8 };
        // Up to 16 lines from line 8: all inside the 32x32 picture.
        let run = |level: Level, d: &mut [u16]| {
            if vertical == 1 { deblock_vertical_at(level, d, base, 32, lines, size, &t) } else { deblock_horizontal_at(level, d, base, 32, lines, size, &t) }
        };
        run(Level::Scalar, &mut want);
        total += 1;
        changed += (want != pic) as u32;
        for &level in levels() {
            let mut got = pic.clone();
            run(level, &mut got);
            assert_eq!(got, want, "{level:?} {size:?} vertical {vertical} {lines} lines {t:?}");
        }
    }
    // The cases reach the filters, not only the masks that turn them off.
    assert!(changed > total / 3, "{changed} of {total}");
}

#[test]
fn every_level_matches_the_scalar_vp9_loop_filter_per_line() {
    let mut seed = 0x0009_10f1u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for (i, (pic, t, size, vertical, _)) in deblock_cases().into_iter().enumerate() {
        if !matches!(size, LfSize::Vp9Four | LfSize::Vp9Eight | LfSize::Vp9Sixteen) {
            continue;
        }
        let vertical = vertical == 1;
        let base = if vertical { 8 * 32 + 16 } else { 16 * 32 + 8 };
        let n = if i % 3 == 0 { 4 } else { 8 };
        let on = if i % 2 == 0 { 0xFF } else { next() as u8 };
        // Every line on its own, as the scalar fallback filters them.
        let mut want = pic.clone();
        let (along, across) = if vertical { (32, 1) } else { (1, 32) };
        for l in (0..n).filter(|l| on >> l & 1 == 1) {
            crate::scalar::deblock_line(&mut want, base + l * along, across, size, &t);
        }
        for &level in levels() {
            let mut got = pic.clone();
            vp9_deblock_at(level, &mut got, base, 32, vertical, (n, on), (size, &t));
            assert_eq!(got, want, "{level:?} {size:?} vertical {vertical} {n} lines on {on:#04x} {t:?}");
        }
    }
}

#[test]
fn loop_filter_falls_back_when_the_vector_rows_leave_the_buffer() {
    // A vertical edge 2 samples from the start: the 4-tap filter reads p1, but the vector
    // kernel's 8-sample rows would start before the buffer.
    let pic: Vec<u16> = (0..4 * 8).map(|i| if i % 8 >= 2 { 120 } else { 100 }).collect();
    let t = lf_thresh(40, 0, 8);
    let mut want = pic.clone();
    deblock_vertical_at(Level::Scalar, &mut want, 2, 8, 4, LfSize::Four, &t);
    assert_ne!(want, pic);
    for &level in levels() {
        let mut got = pic.clone();
        deblock_vertical_at(level, &mut got, 2, 8, 4, LfSize::Four, &t);
        assert_eq!(got, want, "{level:?}");
    }
}

#[test]
fn loop_filter_clamps_a_zigzag_at_full_strength() {
    // p1 - q1 is past the signed range the narrow filter clamps it to (7.14.6.3), while the
    // differences next to the edge stay within the strongest filter's limits.
    for bd in [8, 10, 12] {
        let u = 1 << (bd - 8);
        let line = [100, 100, 100, 100, 100, 100, 263, 200, 190, 127, 100, 100, 100, 100, 100, 100].map(|v: u16| v * u);
        let t = lf_thresh(63, 0, bd);
        for vertical in [false, true] {
            let pic: Vec<u16> = (0..16 * 16).map(|i| line[if vertical { i % 16 } else { i / 16 }]).collect();
            let base = if vertical { 4 * 16 + 8 } else { 8 * 16 + 4 }; // 8 lines from line 4
            let mut want = pic.clone();
            let run = |level, d: &mut [u16]| {
                if vertical {
                    deblock_vertical_at(level, d, base, 16, 8, LfSize::Four, &t)
                } else {
                    deblock_horizontal_at(level, d, base, 16, 8, LfSize::Four, &t)
                }
            };
            run(Level::Scalar, &mut want);
            assert_ne!(want, pic);
            for &level in levels() {
                let mut got = pic.clone();
                run(level, &mut got);
                assert_eq!(got, want, "{level:?} bd {bd} vertical {vertical}");
            }
        }
    }
}

#[test]
fn every_level_matches_the_scalar_subpel_filters() {
    let mut seed = 0x51ed_270bu32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    // AV1's filter shapes: taps summing to 128 with small negative side lobes.
    for case in 0..20_000 {
        let bd = [8, 10, 12][case % 3];
        let max = (1u32 << bd) - 1;
        let mut taps = [0i16; 8];
        for t in &mut taps {
            *t = (next() % 25) as i16 - 12;
        }
        taps[3] = 128 - taps.iter().enumerate().filter(|&(i, _)| i != 3).map(|(_, &t)| t).sum::<i16>();
        let w = [1, 2, 3, 4, 5, 8, 12, 16, 20, 32, 64, 128][next() as usize % 12];
        let shift = if bd == 12 { 5 } else { 3 };
        let src: Vec<u16> = (0..w + 7).map(|_| if next() % 4 == 0 { max as u16 } else { (next() % (max + 1)) as u16 }).collect();
        let mut want = vec![0i16; w];
        subpel_h_at(Level::Scalar, &src, &taps, &mut want, shift);
        for &level in levels() {
            let mut got = vec![0i16; w];
            subpel_h_at(level, &src, &taps, &mut got, shift);
            assert_eq!(got, want, "{level:?} h w {w} bd {bd} taps {taps:?}");
        }
        let stride = w + (next() % 3) as usize;
        let rows: Vec<i16> = (0..7 * stride + w).map(|_| ((next() % 51_000) as i32 - 25_500) as i16).collect();
        let vshift = [7, 9, 11][next() as usize % 3];
        let mut want = vec![0i32; w];
        subpel_v_at(Level::Scalar, &rows, stride, &taps, &mut want, vshift);
        for &level in levels() {
            let mut got = vec![0i32; w];
            subpel_v_at(level, &rows, stride, &taps, &mut got, vshift);
            assert_eq!(got, want, "{level:?} v w {w} stride {stride} taps {taps:?}");
        }
    }
}

#[test]
fn every_level_matches_the_scalar_cdef_filter() {
    let mut seed = 0x0c0f_fee5u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for case in 0..30_000 {
        let shift = [0, 2, 4][case % 3];
        let max = (256u32 << shift) - 1;
        let (w, h) = [(8, 8), (4, 4), (4, 8), (8, 4), (4, 2)][next() as usize % 5];
        let base = next() % (max + 1);
        let spread = [2, 16, 200, 8192][next() as usize % 4];
        let buf: [u16; CDEF_STRIDE * CDEF_STRIDE] = std::array::from_fn(|_| (base + next() % spread).saturating_sub(spread / 2).min(max) as u16);
        // AV1's taps: 2 rows and columns around the sample at most, weights up to 4.
        let n = 1 + next() as usize % 12;
        let taps: Vec<CdefTap> = (0..n)
            .map(|_| CdefTap {
                off: (next() % 5) as isize * CDEF_STRIDE as isize + (next() % 5) as isize - 2 * CDEF_STRIDE as isize - 2,
                weight: 1 + (next() % 4) as i16,
                strength: ((next() % 16) << shift) as i16,
                shift: next() % 11,
            })
            .collect();
        let clamp = next() % 2 == 0;
        let mut want = [0u16; 64];
        cdef_filter_at(Level::Scalar, &buf, w, h, &taps, clamp, &mut want);
        for &level in levels() {
            let mut got = [0u16; 64];
            cdef_filter_at(level, &buf, w, h, &taps, clamp, &mut got);
            assert_eq!(got, want, "{level:?} {w}x{h} clamp {clamp} taps {taps:?}");
        }
    }
}

#[test]
fn cdef_filter_falls_back_when_a_tap_leaves_the_buffer() {
    let buf: [u16; CDEF_STRIDE * CDEF_STRIDE] = std::array::from_fn(|i| (i * 7 % 50) as u16 + 100);
    // Three rows below the bottom row of an 8x8 block: past the end of the padded block.
    let taps = [CdefTap { off: 3 * CDEF_STRIDE as isize, weight: 4, strength: 12, shift: 1 }, CdefTap { off: -1, weight: 2, strength: 12, shift: 1 }];
    let mut want = [0u16; 64];
    cdef_filter_at(Level::Scalar, &buf, 8, 8, &taps, true, &mut want);
    for &level in levels() {
        let mut got = [0u16; 64];
        cdef_filter_at(level, &buf, 8, 8, &taps, true, &mut got);
        assert_eq!(got, want, "{level:?}");
    }
}

#[test]
fn every_level_matches_the_scalar_h264_prediction() {
    let mut seed = 0x0264_5eedu32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let (mut luma_ran, mut chroma_ran) = (0, 0);
    for case in 0..12_000 {
        // Windows of extremes as well as random samples, so clipping and saturation are hit.
        let sample = |n: u32| match n % 4 {
            0 => 0u8,
            1 => 255,
            _ => (n >> 3) as u8,
        };
        // Window strides and lengths: long enough for the vector loads or (some) not.
        let ws = [24, 32, 21, 16][case % 4];
        let win: Vec<u8> = (0..ws * 21 - [0, 0, 3, 8][case / 4 % 4]).map(|_| sample(next())).collect();
        let w8 = |n: u32| (n % 256) as i32 - 128;
        let out = match next() % 6 {
            0 | 1 => H264Out::Put,
            2 => H264Out::Avg,
            3 => H264Out::Weight { w: w8(next()), o: w8(next()), log_wd: next() % 8 },
            4 => H264Out::Weight { w: (next() % 65) as i32, o: 0, log_wd: 5 },
            _ => H264Out::WeightBi { w0: w8(next()), w1: w8(next()), o: w8(next()), log_wd: next() % 8 },
        };
        let dst0: Vec<u8> = (0..16 * 20).map(|_| sample(next())).collect();
        let (w, h) = [(16, 16), (16, 8), (8, 16), (8, 8), (8, 4), (4, 8), (4, 4)][case % 7];
        let (fx, fy) = (next() % 4, next() % 4);
        let mut want = dst0.clone();
        h264_luma_at(Level::Scalar, (&win, ws), (fx, fy), (w, h), (&mut want, 20), out);
        for &level in levels() {
            let mut got = dst0.clone();
            h264_luma_at(level, (&win, ws), (fx, fy), (w, h), (&mut got, 20), out);
            assert_eq!(got, want, "{level:?} luma {w}x{h} frac {fx},{fy} {out:?} window {ws} x {}", win.len());
        }
        luma_ran += neon_ran!({
            let mut got = dst0.clone();
            let done = h264_neon::luma((&win, ws), (fx, fy), (w, h), (&mut got, 20), out);
            assert!(!done || got == want, "NEON luma {w}x{h} frac {fx},{fy} {out:?}");
            done
        });
        let (cw, ch) = [(8, 8), (8, 4), (4, 8), (4, 4), (4, 2), (2, 4), (2, 2)][case % 7];
        let (cx, cy) = (next() % 8, next() % 8);
        let mut want = dst0.clone();
        h264_chroma_at(Level::Scalar, (&win, ws), (cx, cy), (cw, ch), (&mut want, 20), out);
        for &level in levels() {
            let mut got = dst0.clone();
            h264_chroma_at(level, (&win, ws), (cx, cy), (cw, ch), (&mut got, 20), out);
            assert_eq!(got, want, "{level:?} chroma {cw}x{ch} frac {cx},{cy} {out:?}");
        }
        chroma_ran += neon_ran!({
            let mut got = dst0.clone();
            let done = h264_neon::chroma((&win, ws), (cx, cy), (cw, ch), (&mut got, 20), out);
            assert!(!done || got == want, "NEON chroma {cw}x{ch} frac {cx},{cy} {out:?}");
            done
        });
    }
    assert!(luma_ran > 0 || !cfg!(target_arch = "aarch64"));
    assert!(chroma_ran > 0 || !cfg!(target_arch = "aarch64"));
}

#[test]
fn h264_prediction_leaves_dst_alone_for_weights_out_of_range() {
    let win = vec![100u8; 32 * 21];
    for out in [
        H264Out::Weight { w: 1, o: 0, log_wd: 40 },
        H264Out::Weight { w: i32::MAX, o: 0, log_wd: 0 },
        H264Out::Weight { w: 1, o: 128, log_wd: 0 },
        H264Out::WeightBi { w0: 129, w1: 1, o: 0, log_wd: 1 },
        H264Out::WeightBi { w0: 1, w1: 1, o: i32::MIN, log_wd: 1 },
        H264Out::WeightBi { w0: 1, w1: 1, o: 0, log_wd: u32::MAX },
    ] {
        for &level in levels() {
            let mut dst = vec![7u8; 16 * 20];
            h264_luma_at(level, (&win, 32), (1, 2), (16, 16), (&mut dst, 20), out);
            h264_chroma_at(level, (&win, 32), (3, 5), (8, 8), (&mut dst, 20), out);
            let mut cr = vec![7u8; 16 * 20];
            h264_chroma_pair_at(level, ([&win, &win], 32), (3, 5), (8, 8), ([&mut dst, &mut cr], 20), out);
            assert!(dst.iter().chain(&cr).all(|&v| v == 7), "{level:?} {out:?}");
        }
    }
    // The extreme weights H.264 does use still run: 128 (implicit, and the default for logWD 7).
    let mut dst = vec![7u8; 16 * 20];
    h264_luma_at(Level::Scalar, (&win, 32), (0, 0), (16, 16), (&mut dst, 20), H264Out::Weight { w: 128, o: 0, log_wd: 7 });
    assert!(dst.iter().step_by(20).all(|&v| v == 100));
}

#[test]
fn every_level_predicts_a_chroma_pair_as_two_blocks() {
    let mut seed = 0x0c4a_9a12u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut ran = 0;
    for case in 0..12_000 {
        let sample = |n: u32| match n % 4 {
            0 => 0u8,
            1 => 255,
            _ => (n >> 3) as u8,
        };
        // window strides and lengths: long enough for the vector loads or (some) not
        let ws = [16, 24, 9, 12][case % 4];
        let short = [0, 0, 3, 8][case / 4 % 4];
        let mut window = || -> Vec<u8> { (0..ws * 9 - short).map(|_| sample(next())).collect() };
        let (wb, wr) = (window(), window());
        let w8 = |n: u32| (n % 256) as i32 - 128;
        let out = match next() % 6 {
            0 | 1 => H264Out::Put,
            2 => H264Out::Avg,
            3 => H264Out::Weight { w: w8(next()), o: w8(next()), log_wd: next() % 8 },
            4 => H264Out::Weight { w: (next() % 65) as i32, o: 0, log_wd: 5 },
            _ => H264Out::WeightBi { w0: w8(next()), w1: w8(next()), o: w8(next()), log_wd: next() % 8 },
        };
        let (cw, ch) = [(8, 8), (8, 4), (4, 8), (4, 4), (4, 2), (2, 4), (2, 2)][case % 7];
        let (fx, fy) = (next() % 8, next() % 8);
        let mut dst = || -> Vec<u8> { (0..20 * 8).map(|_| sample(next())).collect() };
        let (db, dr) = (dst(), dst());
        let (mut want_b, mut want_r) = (db.clone(), dr.clone());
        h264_chroma_at(Level::Scalar, (&wb, ws), (fx, fy), (cw, ch), (&mut want_b, 20), out);
        h264_chroma_at(Level::Scalar, (&wr, ws), (fx, fy), (cw, ch), (&mut want_r, 20), out);
        for &level in levels() {
            let (mut got_b, mut got_r) = (db.clone(), dr.clone());
            h264_chroma_pair_at(level, ([&wb, &wr], ws), (fx, fy), (cw, ch), ([&mut got_b, &mut got_r], 20), out);
            assert_eq!((&got_b, &got_r), (&want_b, &want_r), "{level:?} {cw}x{ch} frac {fx},{fy} {out:?} window {ws} x {}", wb.len());
            ran += (level != Level::Scalar && got_b != db) as usize;
        }
    }
    assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
}

#[test]
fn every_level_matches_the_scalar_h264_deblocking() {
    let mut seed = 0x0de6_10c5u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let (w, h) = (40usize, 40usize);
    let mut ran = 0;
    for case in 0..20_000 {
        // Smooth planes with small steps, so that every filter branch is taken, and some extremes.
        let base = (next() % 256) as i32;
        let spread = [2, 6, 20, 255][case % 4];
        let pix: Vec<u8> = (0..w * h)
            .map(|_| {
                let v = base + (next() % (2 * spread + 1)) as i32 - spread as i32;
                v.clamp(0, 255) as u8
            })
            .collect();
        let strong = next() % 5 == 0;
        let bs: [u8; 4] = if strong { [4; 4] } else { std::array::from_fn(|_| (next() % 4) as u8) };
        let e = H264Edge { bs, tc0: std::array::from_fn(|_| (next() % 26) as u8), alpha: (next() % 256) as u8, beta: (next() % 19) as u8 };
        let (vertical, chroma) = (case % 2 == 0, case % 3 == 0);
        let (x, y) = (4 + (next() % 16) as usize, 4 + (next() % 16) as usize);
        let mut want = pix.clone();
        h264_deblock_at(Level::Scalar, &mut want, w, (x, y), vertical, chroma, &e);
        for &level in levels() {
            let mut got = pix.clone();
            h264_deblock_at(level, &mut got, w, (x, y), vertical, chroma, &e);
            assert_eq!(got, want, "{level:?} vertical {vertical} chroma {chroma} at ({x}, {y}) {e:?}");
        }
        ran += neon_ran!({
            let mut got = pix.clone();
            let done = h264_neon::deblock(&mut got, w, x, y, vertical, chroma, &e);
            assert!(!done || got == want, "NEON vertical {vertical} chroma {chroma} at ({x}, {y}) {e:?}");
            done
        });
    }
    assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
}

#[test]
fn every_level_deblocks_a_chroma_pair_as_two_edges() {
    let mut seed = 0x0c4b_c4c5u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let (w, h) = (24usize, 24usize);
    let mut ran = 0;
    for case in 0..20_000 {
        let base = (next() % 256) as i32;
        let spread = [2, 6, 20, 255][case % 4];
        let mut plane = || -> Vec<u8> { (0..w * h).map(|_| (base + (next() % (2 * spread + 1)) as i32 - spread as i32).clamp(0, 255) as u8).collect() };
        let (cb, cr) = (plane(), plane());
        let strong = next() % 5 == 0;
        let bs: [u8; 4] = if strong { [4; 4] } else { std::array::from_fn(|_| (next() % 4) as u8) };
        let e = H264Edge { bs, tc0: std::array::from_fn(|_| (next() % 26) as u8), alpha: (next() % 256) as u8, beta: (next() % 19) as u8 };
        let vertical = case % 2 == 0;
        // some edges too close to the plane's start or end for the vector loads
        let (x, y) = ((next() % 20) as usize, (next() % 20) as usize);
        let (mut want_b, mut want_r) = (cb.clone(), cr.clone());
        h264_deblock_at(Level::Scalar, &mut want_b, w, (x, y), vertical, true, &e);
        h264_deblock_at(Level::Scalar, &mut want_r, w, (x, y), vertical, true, &e);
        for &level in levels() {
            let (mut got_b, mut got_r) = (cb.clone(), cr.clone());
            h264_deblock_chroma_pair_at(level, &mut got_b, &mut got_r, w, (x, y), vertical, &e);
            assert_eq!((&got_b, &got_r), (&want_b, &want_r), "{level:?} vertical {vertical} at ({x}, {y}) {e:?}");
            ran += (level != Level::Scalar && got_b != cb) as usize;
        }
    }
    assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
}

/// 8.5.2.4 with a step of 16 as the spec writes it: 32-bit sums, Round2(sum, 7) clipped per pass.
fn spec_vp9_predict(
    (src, ss): (&[u16], usize),
    (fh, fv): (Option<&[i16; 8]>, Option<&[i16; 8]>),
    (w, h): (usize, usize),
    max: u16,
    (dst, ds): (&mut [u16], usize),
    out: Vp9Out,
) {
    let id = [0, 0, 0, 128, 0, 0, 0, 0];
    let (fh, fv) = (fh.unwrap_or(&id), fv.unwrap_or(&id));
    let round = |acc: i32| ((acc + 64) >> 7).clamp(0, max as i32);
    let tmp: Vec<i32> = (0..(h + 7) * w).map(|n| round((0..8).map(|t| fh[t] as i32 * src[(n / w) * ss + n % w + t] as i32).sum())).collect();
    for i in 0..h {
        for j in 0..w {
            let p = round((0..8).map(|t| fv[t] as i32 * tmp[(i + t) * w + j]).sum()) as u16;
            let d = &mut dst[i * ds + j];
            *d = if out == Vp9Out::Avg { ((*d as u32 + p as u32 + 1) >> 1) as u16 } else { p };
        }
    }
}

#[test]
fn every_level_matches_the_spec_vp9_prediction() {
    let mut seed = 0x0009_5eedu32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    // Two of VP9's filters (regular and sharp, at their extremes) besides random ones.
    let vp9 = [[0i16, 1, -5, 126, 8, -3, 1, 0], [-2, 5, -10, 79, 79, -10, 5, -2], [-1, 3, -7, 127, 8, -3, 1, 0], [-4, 11, -23, 80, 80, -23, 11, -4]];
    let (mut wrapped, mut exact, mut ran) = (0, 0, 0);
    for case in 0..8_000 {
        let max = [255u16, 255, 1023, 4095][case % 4];
        let sample = |n: u32| match n % 5 {
            0 => 0u16,
            1 => max,
            _ => ((n >> 3) % (max as u32 + 1)) as u16,
        };
        let taps = |next: &mut dyn FnMut() -> u32| -> [i16; 8] {
            let n = next();
            match n % 4 {
                0 => vp9[(n / 4) as usize % 4],
                1 => std::array::from_fn(|_| (next() % 256) as i16 - 128),
                _ => std::array::from_fn(|_| (next() % 90) as i16 - 30),
            }
        };
        let (fh, fv) = (taps(&mut next), taps(&mut next));
        let (fh, fv) = match next() % 4 {
            0 => (None, None),
            1 => (Some(fh), None),
            2 => (None, Some(fv)),
            _ => (Some(fh), Some(fv)),
        };
        let w = [4, 8, 16, 32, 64][case % 5];
        let h = [4, 8, 16, 32, 64, 1, 3, 12][next() as usize % 8].min(if w == 64 { 64 } else { 32 });
        let ss = w + 7 + [0, 1, 9][next() as usize % 3];
        // Windows exactly as large as the scalar version needs, one more (as the vector loads
        // need), or with room to spare.
        let len = (h + 6) * ss + w + 7 + [0, 1, 8][next() as usize % 3];
        let src: Vec<u16> = (0..len).map(|_| sample(next())).collect();
        let ds = w + [0, 5][next() as usize % 2];
        let dst0: Vec<u16> = (0..(h - 1) * ds + w).map(|_| sample(next())).collect();
        let out = if next() % 2 == 0 { Vp9Out::Put } else { Vp9Out::Avg };
        let wrap = max == 255 && fh.as_ref().is_none_or(vp9_wraps) && fv.as_ref().is_none_or(vp9_wraps);
        if wrap {
            wrapped += 1;
        } else {
            exact += 1;
        }
        let mut want = dst0.clone();
        spec_vp9_predict((&src, ss), (fh.as_ref(), fv.as_ref()), (w, h), max, (&mut want, ds), out);
        for &level in levels() {
            let mut got = dst0.clone();
            vp9_predict_at(level, (&src, ss), (fh.as_ref(), fv.as_ref()), (w, h), max, (&mut got, ds), out);
            assert_eq!(got, want, "{level:?} {w}x{h} max {max} taps {fh:?} {fv:?} {out:?} window {ss} x {len}");
        }
        ran += neon_ran!({
            let mut got = dst0.clone();
            let done = vp9_neon::predict((&src, ss), (fh.as_ref(), fv.as_ref()), (w, h), max, wrap, (&mut got, ds), out);
            assert!(!done || got == want, "NEON {w}x{h} max {max} taps {fh:?} {fv:?} {out:?} window {ss} x {len}");
            done
        });
    }
    assert!(wrapped > 1000 && exact > 1000, "{wrapped} {exact}");
    assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
}

#[test]
fn vp9_wraps_only_where_16_bit_sums_are_exact() {
    assert!(vp9_wraps(&[-4, 11, -23, 80, 80, -23, 11, -4]));
    assert!(vp9_wraps(&[0, 0, 0, 192, 0, 0, -64, 0]));
    assert!(!vp9_wraps(&[0, 0, 0, 193, 0, 0, 0, 0]));
    assert!(!vp9_wraps(&[0, 0, 0, 128, 0, 0, -65, 0]));
}

/// HEVC luma (8.5.3.3.3.1) and chroma (8.5.3.3.3.2) filters, as the specification lists them.
const HEVC_LUMA: [[i8; 8]; 4] = [[0, 0, 0, 64, 0, 0, 0, 0], [-1, 4, -10, 58, 17, -5, 1, 0], [-1, 4, -11, 40, 40, -11, 4, -1], [0, 1, -5, 17, 58, -10, 4, -1]];
const HEVC_CHROMA: [[i8; 4]; 8] =
    [[0, 64, 0, 0], [-2, 58, 10, -2], [-4, 54, 16, -2], [-6, 46, 28, -4], [-4, 36, 36, -4], [-4, 28, 46, -6], [-2, 16, 54, -4], [-2, 10, 58, -2]];

/// 8.5.3.3.3 for 8-bit samples as the specification writes it (shift1 = 0, shift2 = 6,
/// shift3 = 6): the predSamplesLX of the block at (m, m) of the window.
fn spec_hevc_pred<const T: usize>(rows: &[&[u8]], fh: Option<&[i8; T]>, fv: Option<&[i8; T]>, w: usize, h: usize) -> Vec<i16> {
    let m = T / 2 - 1;
    let s = |y: usize, x: usize| rows[y][x] as i32;
    let hf = |y: usize, x: usize, f: &[i8; T]| (0..T).map(|t| f[t] as i32 * s(y, x + t)).sum::<i32>();
    let mut p = vec![0i16; w * h];
    for y in 0..h {
        for x in 0..w {
            p[y * w + x] = match (fh, fv) {
                (None, None) => s(y + m, x + m) << 6,
                (Some(f), None) => hf(y + m, x, f),
                (None, Some(g)) => (0..T).map(|t| g[t] as i32 * s(y + t, x + m)).sum::<i32>(),
                (Some(f), Some(g)) => (0..T).map(|t| g[t] as i32 * hf(y + t, x, f)).sum::<i32>() >> 6,
            } as i16;
        }
    }
    p
}

/// The HEVC 8-bit prediction kernels against the specification's formulas: every fraction,
/// every width the vector version covers, uni-, bi- and 14-bit output, extreme samples, and
/// window rows exactly as long as the kernels may read.
#[test]
fn hevc_predict8_matches_the_formulas() {
    let mut seed = 0x4e5f_6071u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    fn check<const T: usize>(level: Level, filters: &[[i8; T]], next: &mut dyn FnMut() -> u32) {
        let run = |rows: &[&[u8]], fh: Option<&[i8; T]>, fv: Option<&[i8; T]>, w: usize, h: usize, dst: HevcDst| {
            if T == 8 {
                hevc_luma8_at(level, rows, fh.map(|f| f.first_chunk::<8>().unwrap()), fv.map(|f| f.first_chunk::<8>().unwrap()), w, h, dst)
            } else {
                hevc_chroma8_at(level, rows, fh.map(|f| f.first_chunk::<4>().unwrap()), fv.map(|f| f.first_chunk::<4>().unwrap()), w, h, dst)
            }
        };
        for w in (4usize..=64).step_by(4) {
            for h in [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64] {
                let (fx, fy) = (next() as usize % filters.len(), next() as usize % filters.len());
                let (fh, fv) = ((fx != 0).then(|| &filters[fx]), (fy != 0).then(|| &filters[fy]));
                let last = if w.is_multiple_of(8) { w - 8 } else { w - 4 };
                let len = last + 16;
                let data: Vec<Vec<u8>> = (0..h + T - 1)
                    .map(|_| {
                        (0..len)
                            .map(|_| match next() % 8 {
                                0 => 0,
                                1 => 255,
                                _ => next() as u8,
                            })
                            .collect()
                    })
                    .collect();
                let rows: Vec<&[u8]> = data.iter().map(|r| &r[..]).collect();
                let want = spec_hevc_pred(&rows, fh, fv, w, h);
                let ctx = format!("T {T} w {w} h {h} fx {fx} fy {fy}");
                let mut got = vec![0i16; w * h];
                // Levels without a vector version decline (and write nothing): the caller's
                // scalar version runs.
                if !cfg!(target_arch = "aarch64") || level != Level::Neon {
                    assert!(!run(&rows, fh, fv, w, h, HevcDst::Pred(&mut got)), "{ctx}");
                    assert!(got.iter().all(|&v| v == 0), "{ctx}");
                    continue;
                }
                assert!(run(&rows, fh, fv, w, h, HevcDst::Pred(&mut got)), "{ctx}");
                assert_eq!(got, want, "pred {ctx}");
                let ds = w + 5;
                let mut d = vec![7u8; ds * h];
                assert!(run(&rows, fh, fv, w, h, HevcDst::Put(&mut d, ds)), "{ctx}");
                let first: Vec<i16> = (0..w * h).map(|_| (next() % 22000) as i16 - 4000).collect();
                let mut b = vec![7u8; ds * h];
                assert!(run(&rows, fh, fv, w, h, HevcDst::Bi(&first, &mut b, ds)), "{ctx}");
                for y in 0..h {
                    for x in 0..ds {
                        let (put, bi) = if x < w {
                            let p = want[y * w + x] as i32;
                            (((p + 32) >> 6).clamp(0, 255) as u8, ((first[y * w + x] as i32 + p + 64) >> 7).clamp(0, 255) as u8)
                        } else {
                            (7, 7)
                        };
                        assert_eq!(d[y * ds + x], put, "put {ctx} at {x},{y}");
                        assert_eq!(b[y * ds + x], bi, "bi {ctx} at {x},{y}");
                    }
                }
                // One sample short of what the vector loads read: no vector version.
                let short: Vec<&[u8]> = data.iter().map(|r| &r[..len - 1]).collect();
                assert!(!run(&short, fh, fv, w, h, HevcDst::Pred(&mut got)), "short {ctx}");
            }
        }
    }
    for &level in levels() {
        check::<8>(level, &HEVC_LUMA, &mut next);
        check::<4>(level, &HEVC_CHROMA, &mut next);
    }
}

/// 7.11.3.4 sample by sample: the intermediate rows rounded by InterRound0, then the output
/// rounded by InterRound1.
fn spec_av1_mc((src, ss): (&[u16], usize), (hf, vf): (&[i16; 8], &[i16; 8]), (w, h): (usize, usize), (r0, r1): (u32, u32)) -> Vec<i32> {
    let round2 = |x: i32, n: u32| (x + (1 << n >> 1)) >> n;
    let mut tmp = vec![0i32; (h + 7) * w];
    for r in 0..h + 7 {
        for c in 0..w {
            let s: i32 = (0..8).map(|t| hf[t] as i32 * src[r * ss + c + t] as i32).sum();
            tmp[r * w + c] = round2(s, r0);
        }
    }
    let mut out = vec![0i32; h * w];
    for r in 0..h {
        for c in 0..w {
            let s: i32 = (0..8).map(|t| vf[t] as i32 * tmp[(r + t) * w + c]).sum();
            out[r * w + c] = round2(s, r1);
        }
    }
    out
}

/// Random taps adding up to 128 (the identity now and then), their positive ones at most
/// `pos` and negative ones at least `neg` in sum.
fn random_taps(pos: i32, neg: i32, next: &mut dyn FnMut() -> u32) -> [i16; 8] {
    if next().is_multiple_of(5) {
        return [0, 0, 0, 128, 0, 0, 0, 0];
    }
    loop {
        let mut f = [0i16; 8];
        let mut rest = 128i32;
        // all 8 taps, taps 1..7 (AV1's regular and smooth filters) or 2..6 (its 4-tap ones)
        let free: &[usize] = match next() % 3 {
            0 => &[0, 1, 2, 5, 6, 7],
            1 => &[1, 2, 5, 6],
            _ => &[2, 5],
        };
        for &t in free {
            let v = (next() % 49) as i32 - 24;
            f[t] = v as i16;
            rest -= v;
        }
        let split = (next() % 129) as i32;
        f[3] = split as i16;
        f[4] = (rest - split) as i16;
        let p: i32 = f.iter().map(|&c| (c as i32).max(0)).sum();
        let n: i32 = f.iter().map(|&c| (c as i32).min(0)).sum();
        if p <= pos && n >= neg {
            return f;
        }
    }
}

#[test]
fn every_level_matches_the_spec_av1_inter_prediction() {
    let mut seed = 0x0a1_3c5u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut ran = 0;
    for &level in levels() {
        for bd in [8u32, 10, 12] {
            for compound in [false, true] {
                let r0 = if bd == 12 { 5 } else { 3 };
                let r1 = match (compound, bd) {
                    (true, _) => 7,
                    (false, 12) => 9,
                    _ => 11,
                };
                let max = (1u32 << bd) - 1;
                for w in [2usize, 4, 8, 16, 32, 64, 128] {
                    for h in [1usize, 2, 4, 8, 16, 32, 64, 128] {
                        // 8-bit sometimes with taps past the 16-bit envelope (the 32-bit pass runs)
                        let (pos, neg) = if bd == 8 && next().is_multiple_of(4) { (300, -100) } else { (192, -64) };
                        let (hf, vf) = (random_taps(pos, neg, &mut next), random_taps(192, -64, &mut next));
                        let ss = w + 8 + (next() % 5) as usize;
                        let src: Vec<u16> = (0..(h + 7) * ss + 8)
                            .map(|_| match next() % 6 {
                                0 => 0,
                                1 => max as u16,
                                _ => (next() % (max + 1)) as u16,
                            })
                            .collect();
                        let want = spec_av1_mc((&src, ss), (&hf, &vf), (w, h), (r0, r1));
                        let ctx = format!("{level:?} bd {bd} compound {compound} {w}x{h} hf {hf:?} vf {vf:?}");
                        let ds = w + 3;
                        if compound && next().is_multiple_of(2) {
                            // the second prediction of an average, with a random first one
                            let shift = if bd == 12 { 3 } else { 5 };
                            let first: Vec<i32> = (0..ds * h).map(|_| (next() % 40000) as i32 - 8000).collect();
                            let mut got = vec![7u16; ds * h];
                            let out = Av1McOut::Avg { first: &first, fs: ds, dst: &mut got, ds, max: max as u16, shift };
                            if !av1_mc_at(level, (&src, ss), (&hf, &vf), (w, h), bd, (r0, r1), out) {
                                assert!(got.iter().all(|&v| v == 7), "{ctx}");
                                assert!(level != Level::Neon || w == 2, "{ctx}");
                                continue;
                            }
                            for y in 0..h {
                                let row: Vec<u16> =
                                    (0..w).map(|x| ((first[y * ds + x] + want[y * w + x] + (1 << (shift - 1))) >> shift).clamp(0, max as i32) as u16).collect();
                                assert_eq!(&got[y * ds..y * ds + w], &row[..], "avg {ctx} row {y}");
                                assert!(got[y * ds + w..y * ds + ds].iter().all(|&v| v == 7), "{ctx} row {y}");
                            }
                        } else if compound {
                            let mut got = vec![7i32; ds * h];
                            let done = av1_mc_at(level, (&src, ss), (&hf, &vf), (w, h), bd, (r0, r1), Av1McOut::Pred { out: &mut got, os: ds });
                            if !done {
                                assert!(got.iter().all(|&v| v == 7), "{ctx}");
                                assert!(level != Level::Neon || w == 2, "{ctx}");
                                continue;
                            }
                            for y in 0..h {
                                assert_eq!(&got[y * ds..y * ds + w], &want[y * w..y * w + w], "{ctx} row {y}");
                                assert!(got[y * ds + w..y * ds + ds].iter().all(|&v| v == 7), "{ctx} row {y}");
                            }
                        } else {
                            let mut got = vec![7u16; ds * h];
                            let done = av1_mc_at(level, (&src, ss), (&hf, &vf), (w, h), bd, (r0, r1), Av1McOut::Put { dst: &mut got, ds, max: max as u16 });
                            if !done {
                                assert!(got.iter().all(|&v| v == 7), "{ctx}");
                                assert!(level != Level::Neon || w == 2, "{ctx}");
                                continue;
                            }
                            for y in 0..h {
                                let row: Vec<u16> = want[y * w..y * w + w].iter().map(|&v| v.clamp(0, max as i32) as u16).collect();
                                assert_eq!(&got[y * ds..y * ds + w], &row[..], "{ctx} row {y}");
                                assert!(got[y * ds + w..y * ds + ds].iter().all(|&v| v == 7), "{ctx} row {y}");
                            }
                        }
                        ran += 1;
                    }
                }
            }
        }
    }
    assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
}

#[test]
fn av1_inter_prediction_declines_blocks_whose_loads_leave_the_source() {
    for &level in levels() {
        let (w, h, ss) = (16usize, 8usize, 24usize);
        let taps = [0, 2, -6, 126, 8, -2, 0, 0];
        // One sample short of the last row's 16-sample load.
        let src = vec![100u16; (h + 6) * ss + (w - 8) + 15];
        let mut dst = vec![7u16; w * h];
        assert!(!av1_mc_at(level, (&src, ss), (&taps, &taps), (w, h), 8, (3, 11), Av1McOut::Put { dst: &mut dst, ds: w, max: 255 }));
        assert!(dst.iter().all(|&v| v == 7));
        // A destination one sample short.
        let src = vec![100u16; (h + 6) * ss + w + 8];
        let mut dst = vec![7u16; w * h - 1];
        assert!(!av1_mc_at(level, (&src, ss), (&taps, &taps), (w, h), 8, (3, 11), Av1McOut::Put { dst: &mut dst, ds: w, max: 255 }));
        assert!(dst.iter().all(|&v| v == 7));
    }
}

#[test]
fn every_level_transposes_av1_transform_rows_like_the_definition() {
    let mut seed = 0x77a5_9e01u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for &level in levels() {
        for cols in [8usize, 16, 32, 64] {
            for shift in [0u32, 1, 2, 3, 16] {
                let stride = cols + (next() % 9) as usize;
                let src: Vec<i32> = (0..7 * stride + cols).map(|_| (next() % 65536) as i32 - 32768).collect();
                let mut lanes = vec![[0i16; 8]; cols];
                assert!(av1_rows_to_lanes_at(level, &src, stride, cols, &mut lanes));
                for j in 0..cols {
                    for l in 0..8 {
                        assert_eq!(lanes[j][l], src[l * stride + j] as i16, "{level:?} cols {cols} ({j}, {l})");
                    }
                }
                let mut dst = vec![7i32; 7 * stride + cols + 3];
                assert!(av1_lanes_to_rows_at(level, &lanes, cols, shift, &mut dst, stride));
                for l in 0..8 {
                    for j in 0..stride {
                        let want = if j < cols {
                            (lanes[j][l] as i32 + (1 << shift >> 1)) >> shift
                        } else if l < 7 {
                            7
                        } else {
                            continue;
                        };
                        assert_eq!(dst[l * stride + j], want, "{level:?} cols {cols} shift {shift} ({j}, {l})");
                    }
                }
                // too short a source or destination: nothing written
                let mut out = vec![[1i16; 8]; cols];
                assert!(!av1_rows_to_lanes_at(level, &src[..7 * stride + cols - 1], stride, cols, &mut out));
                assert!(out.iter().all(|o| *o == [1; 8]));
                let mut short = vec![9i32; 7 * stride + cols - 1];
                assert!(!av1_lanes_to_rows_at(level, &lanes, cols, shift, &mut short, stride));
                assert!(short.iter().all(|&v| v == 9));
            }
        }
    }
}

/// 8.7.3 band offset sample by sample: `table[sample >> 3]` added with Clip1 (8-bit).
fn spec_sao_band(src: &[u8], table: &[i32; 32]) -> Vec<u8> {
    src.iter().map(|&v| (v as i32 + table[v as usize >> 3]).clamp(0, 255) as u8).collect()
}

/// 8.7.3 edge offset sample by sample: `offsets[2 + Sign(cur - a) + Sign(cur - b)]` added with
/// Clip1 (8-bit).
fn spec_sao_edge(cur: &[u8], a: &[u8], b: &[u8], offsets: &[i32; 5]) -> Vec<u8> {
    let sign = |d: i32| d.signum();
    (0..cur.len())
        .map(|i| {
            let (v, p, q) = (cur[i] as i32, a[i] as i32, b[i] as i32);
            (v + offsets[(2 + sign(v - p) + sign(v - q)) as usize]).clamp(0, 255) as u8
        })
        .collect()
}

/// Random 8-bit samples, often at 0, 255 or equal to their neighbour (edge offset's Sign 0).
fn sao_samples(n: usize, next: &mut dyn FnMut() -> u32) -> Vec<u8> {
    (0..n)
        .map(|_| match next() % 6 {
            0 => 0,
            1 => 255,
            2 => 128,
            _ => next() as u8,
        })
        .collect()
}

#[test]
fn every_level_matches_the_spec_hevc_sao() {
    let mut seed = 0x5a0_ed9eu32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut ran = 0;
    for &level in levels() {
        for n in (16..=80).chain([128, 1920]) {
            // band offset: the four offsets at a random start band, the others 0
            let mut table = [0i32; 32];
            let start = next() as usize % 29;
            for k in 0..4 {
                table[start + k] = (next() % 256) as i32 - 128;
            }
            let src = sao_samples(n, &mut next);
            let mut dst = vec![7u8; n + 5];
            let done = hevc_sao_band8_at(level, &src, &mut dst, &table);
            if level != Level::Neon {
                assert!(!done && dst.iter().all(|&v| v == 7), "{level:?} band n {n}");
            } else {
                assert!(done, "band n {n}");
                assert_eq!(&dst[..n], &spec_sao_band(&src, &table)[..], "band n {n} table {table:?}");
                assert!(dst[n..].iter().all(|&v| v == 7), "band n {n}: wrote past the run");
                ran += 1;
            }
            // edge offset: neighbours often equal to the sample
            let cur = sao_samples(n, &mut next);
            let near = |next: &mut dyn FnMut() -> u32| -> Vec<u8> { cur.iter().map(|&v| if next().is_multiple_of(3) { v } else { next() as u8 }).collect() };
            let (a, b) = (near(&mut next), near(&mut next));
            let offsets: [i32; 5] = std::array::from_fn(|_| (next() % 256) as i32 - 128);
            let mut dst = vec![7u8; n + 5];
            let done = hevc_sao_edge8_at(level, &cur, &a, &b, &mut dst, &offsets);
            if level != Level::Neon {
                assert!(!done && dst.iter().all(|&v| v == 7), "{level:?} edge n {n}");
            } else {
                assert!(done, "edge n {n}");
                assert_eq!(&dst[..n], &spec_sao_edge(&cur, &a, &b, &offsets)[..], "edge n {n} offsets {offsets:?}");
                assert!(dst[n..].iter().all(|&v| v == 7), "edge n {n}: wrote past the run");
                ran += 1;
            }
        }
    }
    assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
}

#[test]
fn hevc_sao_declines_what_it_cannot_take() {
    for &level in levels() {
        let src = vec![100u8; 40];
        let table = [3i32; 32];
        // runs shorter than 16, a short destination, offsets outside i8
        for (src, dlen, table) in [(&src[..15], 40, table), (&src[..], 39, table), (&src[..], 40, [200i32; 32])] {
            let mut dst = vec![7u8; dlen];
            assert!(!hevc_sao_band8_at(level, src, &mut dst, &table), "{level:?}");
            assert!(dst.iter().all(|&v| v == 7));
        }
        let offsets = [1i32, 2, 0, -2, -1];
        let mut dst = vec![7u8; 40];
        assert!(!hevc_sao_edge8_at(level, &src[..15], &src[..15], &src[..15], &mut dst, &offsets), "{level:?}");
        assert!(!hevc_sao_edge8_at(level, &src, &src[..39], &src, &mut dst, &offsets), "{level:?}");
        assert!(!hevc_sao_edge8_at(level, &src, &src, &src, &mut dst, &[1, 2, 0, -2, -129]), "{level:?}");
        assert!(dst.iter().all(|&v| v == 7));
    }
}

/// 18.3 sample by sample: the horizontal six-tap pass over the rows the vertical one reads (or the
/// samples themselves without a horizontal filter), then the vertical pass; each pass is
/// (sum + 64) >> 7 clipped to 0..=255. `src` is the window from (x - 2, y - 2).
fn spec_vp8_predict(src: &[u8], ss: usize, fh: Option<&[i16; 6]>, fv: Option<&[i16; 6]>, w: usize, h: usize) -> Vec<u8> {
    let pass = |sum: i32| ((sum + 64) >> 7).clamp(0, 255);
    let first = |r: usize, c: usize| -> i32 {
        match fh {
            Some(f) => pass((0..6).map(|t| f[t] as i32 * src[r * ss + c + t] as i32).sum()),
            None => src[r * ss + c + 2] as i32,
        }
    };
    let mut out = vec![0u8; w * h];
    for r in 0..h {
        for c in 0..w {
            out[r * w + c] = match fv {
                Some(f) => pass((0..6).map(|t| f[t] as i32 * first(r + t, c)).sum()),
                None => first(r + 2, c),
            } as u8;
        }
    }
    out
}

#[test]
fn every_level_matches_the_spec_vp8_prediction() {
    // VP8's six-tap filters (sub-pixel positions 1..8 in eighths) and bilinear ones, as six taps.
    const SIXTAP: [[i16; 6]; 7] = [
        [0, -6, 123, 12, -1, 0],
        [2, -11, 108, 36, -8, 1],
        [0, -9, 93, 50, -6, 0],
        [3, -16, 77, 77, -16, 3],
        [0, -6, 50, 93, -9, 0],
        [1, -8, 36, 108, -11, 2],
        [0, -1, 12, 123, -6, 0],
    ];
    const BILINEAR: [[i16; 6]; 7] = [
        [0, 0, 112, 16, 0, 0],
        [0, 0, 96, 32, 0, 0],
        [0, 0, 80, 48, 0, 0],
        [0, 0, 64, 64, 0, 0],
        [0, 0, 48, 80, 0, 0],
        [0, 0, 32, 96, 0, 0],
        [0, 0, 16, 112, 0, 0],
    ];
    let mut seed = 0x0f8_5e7au32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut ran = 0;
    for &level in levels() {
        for set in [&SIXTAP, &BILINEAR] {
            for w in [4usize, 8, 16] {
                for h in 1..=16 {
                    let pick = |next: &mut dyn FnMut() -> u32| -> Option<&[i16; 6]> {
                        let k = next() as usize % (set.len() + 1);
                        (k < set.len()).then(|| &set[k])
                    };
                    let (fh, fv) = (pick(&mut next), pick(&mut next));
                    // the vector loads read 16 samples from each strip's first column, h + 5 rows
                    let ss = w + 8 + (next() % 7) as usize;
                    let last = if w == 16 { 8 } else { 0 };
                    let len = (h + 4) * ss + last + 16;
                    let src: Vec<u8> = (0..len)
                        .map(|_| match next() % 5 {
                            0 => 0,
                            1 => 255,
                            _ => next() as u8,
                        })
                        .collect();
                    let want = spec_vp8_predict(&src, ss, fh, fv, w, h);
                    let ds = w + 3;
                    let mut dst = vec![7u8; ds * h];
                    let ctx = format!("{level:?} {w}x{h} fh {fh:?} fv {fv:?}");
                    let done = vp8_predict_at(level, (&src, ss), (fh, fv), (w, h), (&mut dst, ds));
                    if level != Level::Neon {
                        assert!(!done && dst.iter().all(|&v| v == 7), "{ctx}");
                        continue;
                    }
                    assert!(done, "{ctx}");
                    for r in 0..h {
                        assert_eq!(&dst[r * ds..r * ds + w], &want[r * w..r * w + w], "{ctx} row {r}");
                        assert!(dst[r * ds + w..r * ds + ds].iter().all(|&v| v == 7), "{ctx} row {r}: wrote past the block");
                    }
                    ran += 1;
                }
            }
        }
    }
    assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
}

#[test]
fn vp8_prediction_declines_what_it_cannot_take() {
    let f: [i16; 6] = [3, -16, 77, 77, -16, 3];
    for &level in levels() {
        let (w, h, ss) = (16usize, 8usize, 24usize);
        let full = vec![100u8; (h + 4) * ss + 8 + 16];
        // one sample short of the last row's loads, a short destination, an unsupported width,
        // and taps whose sums leave 16 bits
        let cases: [(&[u8], usize, usize, [i16; 6]); 4] =
            [(&full[..full.len() - 1], w, w * h, f), (&full, w, w * h - 1, f), (&full, 32, 32 * h, f), (&full, w, w * h, [0, 0, 200, -72, 0, 0])];
        for (src, w, dlen, taps) in cases {
            let mut dst = vec![7u8; dlen];
            assert!(!vp8_predict_at(level, (src, ss.max(w + 8)), (Some(&taps), Some(&taps)), (w, h), (&mut dst, w)), "{level:?} w {w}");
            assert!(dst.iter().all(|&v| v == 7), "{level:?} w {w}");
        }
    }
}

#[test]
fn vp8_loop_filters_decline_edges_whose_lines_leave_the_buffer() {
    let l = Vp8Limits { edge: 255, interior: 255, hev_threshold: 0 };
    let stride = 40;
    // A step across every edge, so a filter that ran would change samples.
    let pic = |len: usize| -> Vec<u8> { (0..len).map(|i| if i % 2 == 0 { 60 } else { 90 }).collect() };
    let mut ran = 0;
    for &level in levels() {
        for vertical in [false, true] {
            for f in [Vp8Filter::Simple, Vp8Filter::Subblock, Vp8Filter::Macroblock] {
                for lines in [16usize, 8] {
                    let (step, along) = if vertical { (1, stride) } else { (stride, 1) };
                    // The smallest start, and the length that just holds q3 of the last line.
                    let start = 4 * step;
                    let len = start + (lines - 1) * along + 3 * step + 1;
                    let call = |d: &mut [u8], e: &mut [u8], start: usize, which: usize| -> bool {
                        if lines == 16 {
                            vp8_loop_filter_at(level, d, start, stride, vertical, f, &l)
                        } else if which == 0 {
                            vp8_loop_filter_uv_at(level, d, e, (start, stride), vertical, f, &l)
                        } else {
                            vp8_loop_filter_uv_at(level, e, d, (start, stride), vertical, f, &l)
                        }
                    };
                    // `which`: the plane that is short (chroma only; luma has one).
                    for which in 0..if lines == 16 { 1 } else { 2 } {
                        let (mut d, mut e) = (pic(len), pic(len));
                        ran += call(&mut d, &mut e, start, which) as usize;
                        for (s, n) in [(start - 1, len), (start, len - 1)] {
                            let (mut d, mut e) = (pic(n), pic(len));
                            let (d0, e0) = (d.clone(), e.clone());
                            assert!(!call(&mut d, &mut e, s, which), "{level:?} {f:?} vertical {vertical} lines {lines} start {s} len {n}");
                            assert!(d == d0 && e == e0, "{level:?} {f:?} vertical {vertical} lines {lines}");
                        }
                    }
                }
            }
        }
    }
    assert!(ran > 0 || !cfg!(target_arch = "aarch64"));
}

#[test]
fn kernels_decline_offsets_whose_reach_overflows() {
    let l = Vp8Limits { edge: 255, interior: 255, hev_threshold: 0 };
    let e = HevcLumaEdge { on: [true; 2], tc: [4; 2], beta: [40; 2], no_p: [false; 2], no_q: [false; 2] };
    let mut d = vec![0u8; 256];
    for &level in levels() {
        for o in [usize::MAX - 3, usize::MAX] {
            for vertical in [false, true] {
                assert!(!hevc_deblock_luma8_at(level, &mut d, o, 16, vertical, &e));
                assert!(!vp8_loop_filter_at(level, &mut d, o, 16, vertical, Vp8Filter::Macroblock, &l));
            }
            assert!(!vp8_idct_add_at(level, &[1; 16], &mut d, o, 16));
        }
        // A stride whose three steps past q0 overflow.
        assert!(!vp8_loop_filter_at(level, &mut d, 4, usize::MAX / 2, true, Vp8Filter::Macroblock, &l));
    }
    assert!(d.iter().all(|&v| v == 0));
    // The AV1 and VP9 loop filter entry points: line offsets past `usize` are skipped, not
    // computed (they panicked with overflow checks on).
    let t = lf_thresh(63, 0, 8);
    let mut w = vec![0u16; 256];
    for &level in levels() {
        for (base, stride) in [(usize::MAX - 3, 16), (usize::MAX, 1), (4, usize::MAX / 2), (4, usize::MAX)] {
            deblock_vertical_at(level, &mut w, base, stride, 16, LfSize::Eight, &t);
            deblock_horizontal_at(level, &mut w, base, stride, 16, LfSize::Eight, &t);
            for vertical in [false, true] {
                vp9_deblock_at(level, &mut w, base, stride, vertical, (8, 0xFF), (LfSize::Vp9Eight, &t));
            }
        }
    }
    assert!(w.iter().all(|&v| v == 0));
}

#[test]
fn scalar_loop_filter_leaves_lines_that_leave_the_buffer() {
    let t = lf_thresh(63, 0, 8);
    let step_edge = |len: usize| -> Vec<u16> { (0..len).map(|i| if i < len / 2 { 60 } else { 70 }).collect() };
    for size in [LfSize::Four, LfSize::Six, LfSize::Eight, LfSize::Sixteen, LfSize::Vp9Four, LfSize::Vp9Eight, LfSize::Vp9Sixteen] {
        let reach = size.reach();
        let len = 2 * reach;
        // Fits exactly: the line is filtered.
        let mut d = step_edge(len);
        deblock_line(&mut d, reach, 1, size, &t);
        assert_ne!(d, step_edge(len), "{size:?}");
        // One short on either side, and offsets whose reach overflows: left unchanged.
        for (base, n, step) in [(reach - 1, len - 1, 1), (reach, len - 1, 1), (usize::MAX, len, 1), (reach, len, usize::MAX)] {
            let mut d = step_edge(n);
            deblock_line(&mut d, base, step, size, &t);
            assert_eq!(d, step_edge(n), "{size:?} base {base}");
        }
    }
}
