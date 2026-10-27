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

/// BT.709 limited-range 8-bit 4:2:0 from straight RGBA8 (2×2 chroma average).
pub fn rgba_to_yuv420_8(rgba: &[u8], w: usize, h: usize, y: &mut Vec<u8>, u: &mut Vec<u8>, v: &mut Vec<u8>) {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
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
            let src = &rgba[row * w * 4..(row + 1) * w * 4];
            for x in 0..w {
                let (r, g, b) = (src[x * 4] as f32 / 255.0, src[x * 4 + 1] as f32 / 255.0, src[x * 4 + 2] as f32 / 255.0);
                let yy = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                yr[dy * w + x] = (16.0 + 219.0 * yy).round().clamp(1.0, 254.0) as u8;
                us[x / 2] += (b - yy) / 1.8556;
                vs[x / 2] += (r - yy) / 1.5748;
                cnt[x / 2] += 1.0;
            }
        }
        for cx in 0..cw {
            ur[cx] = (128.0 + 224.0 * us[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
            vr[cx] = (128.0 + 224.0 * vs[cx] / cnt[cx]).round().clamp(1.0, 254.0) as u8;
        }
    });
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
