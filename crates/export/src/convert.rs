use super::*;

/// Limited-range 10-bit 4:4:4 with matrix (Kr, Kb) from straight RGBA8, or from encoded R'G'B'
/// floats (3 per pixel) when `hdr` is given. `alpha` (full range, 0 = transparent) takes the
/// RGBA8 alpha; it stays opaque for float input, which carries none.
#[allow(clippy::too_many_arguments)]
pub fn to_yuv444_10(
    rgba: &[u8],
    hdr: Option<&[f32]>,
    w: usize,
    h: usize,
    kr: f32,
    kb: f32,
    y: &mut [u16],
    cb: &mut [u16],
    cr: &mut [u16],
    alpha: Option<&mut [u16]>,
) {
    let (w, n) = (w.max(1), w.saturating_mul(h));
    let kg = 1.0 - kr - kb;
    let (sb, sr) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
    let rgb_at = |i: usize| -> [f32; 3] {
        match hdr {
            Some(f) => f.get(i * 3..i * 3 + 3).map_or([0.0; 3], |p| [p[0], p[1], p[2]]),
            None => rgba.get(i * 4..i * 4 + 3).map_or([0.0; 3], |p| [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]),
        }
    };
    y.par_chunks_mut(w).zip(cb.par_chunks_mut(w).zip(cr.par_chunks_mut(w))).enumerate().for_each(|(row, (yr, (cbr, crr)))| {
        for (x, (yv, (cbv, crv))) in yr.iter_mut().zip(cbr.iter_mut().zip(crr.iter_mut())).enumerate() {
            let [r, g, b] = rgb_at(row * w + x);
            let yy = kr * r + kg * g + kb * b;
            *yv = (64.0 + 876.0 * yy).round().clamp(4.0, 1019.0) as u16;
            *cbv = (512.0 + 896.0 * (b - yy) / sb).round().clamp(4.0, 1019.0) as u16;
            *crv = (512.0 + 896.0 * (r - yy) / sr).round().clamp(4.0, 1019.0) as u16;
        }
    });
    if let (Some(a), None) = (alpha, hdr) {
        for (i, dst) in a.iter_mut().enumerate().take(n) {
            *dst = rgba.get(i * 4 + 3).map_or(1023, |&v| ((v as u32 * 1023 + 127) / 255) as u16);
        }
    }
}

/// BT.709 limited-range 10-bit 4:2:2 from straight RGBA8 (chroma averaged horizontally).
pub fn rgba_to_yuv422_10(rgba: &[u8], w: usize, h: usize, y: &mut [u16], cb: &mut [u16], cr: &mut [u16]) {
    let cw = w.div_ceil(2);
    debug_assert!(y.len() >= w * h && rgba.len() >= w * h * 4);
    y.par_chunks_mut(w).zip(cb.par_chunks_mut(cw).zip(cr.par_chunks_mut(cw))).enumerate().for_each(|(row, (yr, (cbr, crr)))| {
        let src = &rgba[row * w * 4..(row + 1) * w * 4];
        let mut us = vec![0f32; w];
        let mut vs = vec![0f32; w];
        for x in 0..w {
            let (r, g, b) = (src[x * 4] as f32 / 255.0, src[x * 4 + 1] as f32 / 255.0, src[x * 4 + 2] as f32 / 255.0);
            let yy = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            yr[x] = (64.0 + 876.0 * yy).round().clamp(4.0, 1019.0) as u16;
            us[x] = (b - yy) / 1.8556;
            vs[x] = (r - yy) / 1.5748;
        }
        for cx in 0..cw {
            let a = cx * 2;
            let b2 = (a + 1).min(w - 1);
            let u = (us[a] + us[b2]) * 0.5;
            let v = (vs[a] + vs[b2]) * 0.5;
            cbr[cx] = (512.0 + 896.0 * u).round().clamp(4.0, 1019.0) as u16;
            crr[cx] = (512.0 + 896.0 * v).round().clamp(4.0, 1019.0) as u16;
        }
    });
}

/// Limited-range 10-bit 4:2:2 from encoded R'G'B' floats with matrix (Kr, Kb).
#[allow(clippy::too_many_arguments)]
pub fn rgbf_to_yuv422_10(rgb: &[f32], w: usize, h: usize, kr: f32, kb: f32, y: &mut [u16], cb: &mut [u16], cr: &mut [u16]) {
    let cw = w.div_ceil(2);
    debug_assert!(rgb.len() >= w * h * 3 && y.len() >= w * h);
    let kg = 1.0 - kr - kb;
    let (sb, sr) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
    y.par_chunks_mut(w).zip(cb.par_chunks_mut(cw).zip(cr.par_chunks_mut(cw))).enumerate().for_each(|(row, (yr, (cbr, crr)))| {
        let src = &rgb[row * w * 3..(row + 1) * w * 3];
        let mut us = vec![0f32; w];
        let mut vs = vec![0f32; w];
        for x in 0..w {
            let (r, g, b) = (src[x * 3], src[x * 3 + 1], src[x * 3 + 2]);
            let yy = kr * r + kg * g + kb * b;
            yr[x] = (64.0 + 876.0 * yy).round().clamp(4.0, 1019.0) as u16;
            us[x] = (b - yy) / sb;
            vs[x] = (r - yy) / sr;
        }
        for cx in 0..cw {
            let a = cx * 2;
            let b2 = (a + 1).min(w - 1);
            cbr[cx] = (512.0 + 896.0 * (us[a] + us[b2]) * 0.5).round().clamp(4.0, 1019.0) as u16;
            crr[cx] = (512.0 + 896.0 * (vs[a] + vs[b2]) * 0.5).round().clamp(4.0, 1019.0) as u16;
        }
    });
}

/// `x.round().clamp(1.0, 254.0) as u8` for the finite values the conversions produce, without
/// the `roundf` call. For `0.5 <= x < 2^23` the sum `x + 0.5` is a multiple of the spacing of
/// `x` and never rounds up across an integer, so truncating it rounds half away from zero
/// exactly as `round` does; anything below 0.5 clamps to 1 either way. NaN gives 0, as the
/// `round`/`clamp`/`as` chain does.
#[inline(always)]
fn code8(x: f32) -> u8 {
    if x.is_nan() {
        return 0;
    }
    ((x + 0.5) as i32).clamp(1, 254) as u8
}

/// BT.709 limited-range 8-bit 4:2:0 from straight RGBA8 (2×2 chroma average).
pub fn rgba_to_yuv420_8(rgba: &[u8], w: usize, h: usize, y: &mut Vec<u8>, u: &mut Vec<u8>, v: &mut Vec<u8>) {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    y.resize(w * h, 0);
    u.resize(cw * ch, 0);
    v.resize(cw * ch, 0);
    // per-code tables of exactly the products the formula takes: `c / 255.0` and `k * (c / 255.0)`
    let unit: [f32; 256] = std::array::from_fn(|c| c as f32 / 255.0);
    let (kr, kg, kb): ([f32; 256], [f32; 256], [f32; 256]) =
        (std::array::from_fn(|c| 0.2126 * unit[c]), std::array::from_fn(|c| 0.7152 * unit[c]), std::array::from_fn(|c| 0.0722 * unit[c]));
    // Per row: gather the luma sums, then the vectorisable part (codes, chroma differences) over
    // whole rows, then add the differences pairwise in the order the per-pixel loop added them.
    let rows_init = || (vec![0f32; w], vec![0f32; w], vec![0f32; w], vec![0f32; cw], vec![0f32; cw]);
    y.par_chunks_mut(w * 2).zip(u.par_chunks_mut(cw).zip(v.par_chunks_mut(cw))).enumerate().for_each_init(
        rows_init,
        |(ty, tb, tr, us, vs), (cy, (yr, (ur, vr)))| {
            let rows = yr.len() / w;
            us.fill(0.0);
            vs.fill(0.0);
            for (dy, yrow) in yr.chunks_exact_mut(w).enumerate() {
                let row = cy * 2 + dy;
                let Some(src) = rgba.get(row * w * 4..(row + 1) * w * 4) else { continue };
                for (p, (yy, (b, r))) in src.as_chunks::<4>().0.iter().zip(ty.iter_mut().zip(tb.iter_mut().zip(tr.iter_mut()))) {
                    let (pr, pg, pb) = (usize::from(p[0]), usize::from(p[1]), usize::from(p[2]));
                    *yy = kr[pr] + kg[pg] + kb[pb];
                    *b = unit[pb];
                    *r = unit[pr];
                }
                for (o, &yy) in yrow.iter_mut().zip(ty.iter()) {
                    *o = code8(16.0 + 219.0 * yy);
                }
                for (b, &yy) in tb.iter_mut().zip(ty.iter()) {
                    *b = (*b - yy) / 1.8556;
                }
                for (r, &yy) in tr.iter_mut().zip(ty.iter()) {
                    *r = (*r - yy) / 1.5748;
                }
                for (acc, pair) in us.iter_mut().zip(tb.as_chunks::<2>().0) {
                    *acc += pair[0];
                    *acc += pair[1];
                }
                for (acc, pair) in vs.iter_mut().zip(tr.as_chunks::<2>().0) {
                    *acc += pair[0];
                    *acc += pair[1];
                }
                if w % 2 == 1 {
                    // the odd last column has a chroma sample of its own
                    us[cw - 1] += tb[w - 1];
                    vs[cw - 1] += tr[w - 1];
                }
            }
            for (cx, (uo, vo)) in ur.iter_mut().zip(vr.iter_mut()).enumerate() {
                // pixels averaged into this chroma sample: 2 per row (1 in an odd last column), `rows` rows
                let cnt = ((w - 2 * cx).min(2) * rows) as f32;
                *uo = code8(128.0 + 224.0 * us[cx] / cnt);
                *vo = code8(128.0 + 224.0 * vs[cx] / cnt);
            }
        },
    );
}

/// Limited-range 8-bit 4:2:0 from encoded R'G'B' floats with matrix (Kr, Kb).
#[allow(clippy::too_many_arguments)]
pub fn rgbf_to_yuv420_8(rgb: &[f32], w: usize, h: usize, kr: f32, kb: f32, y: &mut Vec<u8>, u: &mut Vec<u8>, v: &mut Vec<u8>) {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let kg = 1.0 - kr - kb;
    let (sb, sr) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
    y.resize(w * h, 0);
    u.resize(cw * ch, 0);
    v.resize(cw * ch, 0);
    y.par_chunks_mut(w * 2).zip(u.par_chunks_mut(cw).zip(v.par_chunks_mut(cw))).enumerate().for_each(|(cy, (yr, (ur, vr)))| {
        let rows = yr.len() / w;
        let mut us = vec![0f32; cw];
        let mut vs = vec![0f32; cw];
        let mut cnt = vec![0f32; cw];
        for dy in 0..rows {
            let row = cy * 2 + dy;
            let src = &rgb[row * w * 3..(row + 1) * w * 3];
            for x in 0..w {
                let (r, g, b) = (src[x * 3], src[x * 3 + 1], src[x * 3 + 2]);
                let yy = kr * r + kg * g + kb * b;
                yr[dy * w + x] = (16.0 + 219.0 * yy).round().clamp(1.0, 254.0) as u8;
                us[x / 2] += (b - yy) / sb;
                vs[x / 2] += (r - yy) / sr;
                cnt[x / 2] += 1.0;
            }
        }
        for cx in 0..cw {
            ur[cx] = (128.0 + 224.0 * us[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
            vr[cx] = (128.0 + 224.0 * vs[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
        }
    });
}

/// Limited-range 10-bit 4:2:0 (code values 0..1023, not MSB-aligned) from encoded R'G'B' floats with
/// matrix (Kr, Kb): Y = 64 + 876·Y', C = 512 + 896·C, chroma averaged over each 2x2 block (one pixel
/// or pair at an odd edge). `rgb` is 3 floats per pixel; NaN counts as 0, infinities and
/// out-of-range values clamp to 0..1 first, and codes clamp to 4..1019 like [`rgbf_to_yuv422_10`]
/// (so black is 64, peak white 940 and the chroma of any colour stays within 64..960).
/// The planes are resized to `w * h` and `ceil(w / 2) * ceil(h / 2)`. A `rgb` slice of the wrong
/// length, a zero size or a size whose arithmetic overflows is an error.
#[allow(clippy::too_many_arguments)]
pub fn rgbf_to_yuv420_10(rgb: &[f32], w: usize, h: usize, kr: f32, kb: f32, y: &mut Vec<u16>, u: &mut Vec<u16>, v: &mut Vec<u16>) -> Result<()> {
    let bad = || ExportError::Encode(format!("a {w}x{h} HDR picture needs w*h*3 floats, got {}", rgb.len()));
    let px = w.checked_mul(h).ok_or_else(bad)?;
    if w == 0 || h == 0 || px.checked_mul(3) != Some(rgb.len()) {
        return Err(bad());
    }
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let kg = 1.0 - kr - kb;
    let (sb, sr) = (2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
    y.clear();
    y.resize(px, 0);
    u.clear();
    u.resize(cw * ch, 0);
    v.clear();
    v.resize(cw * ch, 0);
    let unit = |x: f32| if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) };
    let code = |x: f32| x.round().clamp(4.0, 1019.0) as u16;
    y.par_chunks_mut(w * 2).zip(u.par_chunks_mut(cw).zip(v.par_chunks_mut(cw))).enumerate().for_each(|(cy, (yr, (ur, vr)))| {
        let rows = yr.len() / w;
        let mut us = vec![0f32; cw];
        let mut vs = vec![0f32; cw];
        let mut cnt = vec![0f32; cw];
        for (dy, yrow) in yr.chunks_exact_mut(w).enumerate().take(rows) {
            let row = cy * 2 + dy;
            let Some(src) = rgb.get(row * w * 3..(row + 1) * w * 3) else { continue };
            for (x, (&[r, g, b], yo)) in src.as_chunks::<3>().0.iter().zip(yrow.iter_mut()).enumerate() {
                let (r, g, b) = (unit(r), unit(g), unit(b));
                let yy = kr * r + kg * g + kb * b;
                *yo = code(64.0 + 876.0 * yy);
                if let (Some(a), Some(c), Some(n)) = (us.get_mut(x / 2), vs.get_mut(x / 2), cnt.get_mut(x / 2)) {
                    *a += (b - yy) / sb;
                    *c += (r - yy) / sr;
                    *n += 1.0;
                }
            }
        }
        for (cx, (uo, vo)) in ur.iter_mut().zip(vr.iter_mut()).enumerate() {
            let n = cnt.get(cx).copied().unwrap_or(1.0).max(1.0);
            *uo = code(512.0 + 896.0 * us.get(cx).copied().unwrap_or(0.0) / n);
            *vo = code(512.0 + 896.0 * vs.get(cx).copied().unwrap_or(0.0) / n);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The straightforward float formula `rgba_to_yuv420_8` must reproduce bit for bit.
    fn reference_420_8(rgba: &[u8], w: usize, h: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let (mut y, mut u, mut v) = (vec![0u8; w * h], vec![0u8; cw * ch], vec![0u8; cw * ch]);
        for cy in 0..ch {
            let mut us = vec![0f32; cw];
            let mut vs = vec![0f32; cw];
            let mut cnt = vec![0f32; cw];
            for row in cy * 2..(cy * 2 + 2).min(h) {
                for x in 0..w {
                    let p = &rgba[(row * w + x) * 4..];
                    let (r, g, b) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
                    let yy = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                    y[row * w + x] = (16.0 + 219.0 * yy).round().clamp(1.0, 254.0) as u8;
                    us[x / 2] += (b - yy) / 1.8556;
                    vs[x / 2] += (r - yy) / 1.5748;
                    cnt[x / 2] += 1.0;
                }
            }
            for cx in 0..cw {
                u[cy * cw + cx] = (128.0 + 224.0 * us[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
                v[cy * cw + cx] = (128.0 + 224.0 * vs[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
            }
        }
        (y, u, v)
    }

    fn check(rgba: &[u8], w: usize, h: usize) {
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        rgba_to_yuv420_8(rgba, w, h, &mut y, &mut u, &mut v);
        let (ry, ru, rv) = reference_420_8(rgba, w, h);
        assert!(y == ry, "{w}x{h}: luma differs");
        assert!(u == ru, "{w}x{h}: Cb differs");
        assert!(v == rv, "{w}x{h}: Cr differs");
    }

    #[test]
    fn code8_rounds_like_round_around_every_half() {
        for n in 0..300 {
            let half = n as f32 + 0.5;
            let mut x = half;
            let mut down = half;
            for _ in 0..64 {
                for v in [x, down] {
                    assert_eq!(code8(v), v.round().clamp(1.0, 254.0) as u8, "{v}");
                }
                x = f32::from_bits(x.to_bits() + 1);
                down = f32::from_bits(down.to_bits() - 1);
            }
        }
        for v in [-3.0f32, -0.5, -0.0, 0.0, 0.25, 0.49999997, 0.5, 1.0, 253.5, 254.49998, 1e9, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(code8(v), v.round().clamp(1.0, 254.0) as u8, "{v}");
        }
    }

    #[test]
    fn yuv420_8_matches_the_float_formula_for_every_colour() {
        // 4096 x 4096 holds each of the 2^24 colours once; neighbouring colours share chroma
        let (w, h) = (4096, 4096);
        let mut rgba = vec![255u8; w * h * 4];
        for (i, p) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            p[0] = (i >> 16) as u8;
            p[1] = (i >> 8) as u8;
            p[2] = i as u8;
        }
        check(&rgba, w, h);
    }

    #[test]
    fn yuv420_8_matches_the_float_formula_at_odd_sizes() {
        let mut seed = 0x9e37_79b9u32;
        for (w, h) in [(1, 1), (2, 2), (3, 5), (17, 9), (64, 33), (1921, 3)] {
            let rgba: Vec<u8> = (0..w * h * 4)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed >> 24) as u8
                })
                .collect();
            check(&rgba, w, h);
        }
        // the extremes, where rounding and clamping meet
        for c in [[0u8, 0, 0], [255, 255, 255], [255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [0, 255, 255], [255, 0, 255]] {
            let rgba: Vec<u8> = (0..6 * 4).flat_map(|_| [c[0], c[1], c[2], 255]).collect();
            check(&rgba, 6, 4);
        }
    }
}
