//! Planar 8-bit 4:2:0 pictures into an NV12 image of a (macroblock-padded) encode surface. Safe
//! code, compiled everywhere so it is tested everywhere; `va::EncodeSession::upload` maps the
//! image and hands its bytes here.

/// Write planar 4:2:0 (`w`×`h`, tight planes, chroma `ceil(w/2)`×`ceil(h/2)`) into an NV12 image of
/// `pw`×`ph` with the given plane pitches and offsets, replicating the right / bottom edges into
/// the padding.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fill(
    data: &mut [u8],
    pitches: [u32; 2],
    offsets: [u32; 2],
    y: &[u8],
    u: &[u8],
    v: &[u8],
    w: usize,
    h: usize,
    pw: usize,
    ph: usize,
) -> Result<(), String> {
    let (p0, p1) = (pitches[0] as usize, pitches[1] as usize);
    let (o0, o1) = (offsets[0] as usize, offsets[1] as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    if w == 0 || h == 0 || pw < w || ph < h {
        return Err(format!("a {w}x{h} picture for a {pw}x{ph} surface"));
    }
    if p0 < pw || p1 < pw {
        return Err("image pitch smaller than the surface".into());
    }
    if y.len() < w.saturating_mul(h) || u.len() < cw.saturating_mul(ch) || v.len() < cw.saturating_mul(ch) {
        return Err("picture planes shorter than the picture".into());
    }
    for row in 0..ph {
        let r = row.min(h - 1);
        let src = y.get(r * w..r * w + w).ok_or("luma row")?;
        let at = o0.saturating_add(row.saturating_mul(p0));
        let dst = data.get_mut(at..at.saturating_add(pw)).ok_or("image too small (luma)")?;
        let (left, right) = dst.split_at_mut(w);
        left.copy_from_slice(src);
        right.fill(src.last().copied().unwrap_or(16));
    }
    for row in 0..ph.div_ceil(2) {
        let r = row.min(ch - 1);
        let (su, sv) = (u.get(r * cw..r * cw + cw).ok_or("cb row")?, v.get(r * cw..r * cw + cw).ok_or("cr row")?);
        let at = o1.saturating_add(row.saturating_mul(p1));
        let dst = data.get_mut(at..at.saturating_add(pw)).ok_or("image too small (chroma)")?;
        for (i, pair) in dst.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let c = i.min(cw - 1);
            *pair = [su.get(c).copied().unwrap_or(128), sv.get(c).copied().unwrap_or(128)];
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn pads_by_replicating_edges() {
        // 3x2 picture into a 4x4 surface, pitch 4, chroma at offset 16
        let (y, u, v) = ([1u8, 2, 3, 4, 5, 6], [10u8, 11], [20u8, 21]);
        let mut data = vec![0u8; 24];
        fill(&mut data, [4, 4], [0, 16], &y, &u, &v, 3, 2, 4, 4).unwrap();
        assert_eq!(&data[0..16], &[1, 2, 3, 3, 4, 5, 6, 6, 4, 5, 6, 6, 4, 5, 6, 6]);
        assert_eq!(&data[16..24], &[10, 20, 11, 21, 10, 20, 11, 21]);
    }

    #[test]
    fn rejects_what_does_not_fit() {
        let mut data = vec![0u8; 24];
        assert!(fill(&mut data, [4, 4], [0, 16], &[], &[], &[], 0, 0, 4, 4).is_err(), "empty picture");
        assert!(fill(&mut data, [4, 4], [0, 16], &[0; 10], &[0; 3], &[0; 3], 5, 2, 4, 4).is_err(), "wider than the surface");
        assert!(fill(&mut data, [2, 2], [0, 16], &[0; 4], &[0; 1], &[0; 1], 2, 2, 4, 4).is_err(), "pitch below the surface width");
        assert!(fill(&mut data[..8], [4, 4], [0, 16], &[0; 4], &[0; 1], &[0; 1], 2, 2, 4, 4).is_err(), "image smaller than its layout");
        assert!(fill(&mut data, [4, 4], [0, 16], &[0; 3], &[0; 1], &[0; 1], 2, 2, 4, 4).is_err(), "short luma plane");
    }
}
