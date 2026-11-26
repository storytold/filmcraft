//! Software MJPEG packets use the shared baseline JPEG encoder. FilmCraft keeps its existing
//! full-resolution chroma (4:4:4), quality scale and RGBA-to-RGB admission policy.

pub(super) fn encode(rgba: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>, String> {
    // The former image encoder required exactly one RGB sample per declared pixel. The shared
    // encoder permits a larger source buffer, so keep this check rather than silently crop it.
    let expected = usize::try_from(width).ok().and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h))).and_then(|n| n.checked_mul(3));
    if expected != rgba.as_chunks::<4>().0.len().checked_mul(3) {
        return Err("MJPEG frame dimensions do not match its pixel buffer".into());
    }
    let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    craft_jpeg::encode_baseline_low_memory(&rgb, width as usize, height as usize, 3, quality, craft_jpeg::ChromaSubsampling::S444, &[])
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(rgba: &[u8], width: u32, height: u32, quality: u8) -> Vec<u8> {
        let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode(&rgb, width, height, image::ExtendedColorType::Rgb8).unwrap();
        out
    }

    fn mae(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(&a, &b)| f64::from(a.abs_diff(b))).sum::<f64>() / a.len() as f64
    }

    #[test]
    fn mjpeg_shared_preserves_sampling_quality_and_decoded_fidelity() {
        for (w, h) in [(1u32, 1u32), (8, 8), (31, 17), (64, 32)] {
            for pattern in 0..4 {
                let rgba: Vec<u8> = (0..w * h)
                    .flat_map(|i| {
                        let x = i % w;
                        let y = i / w;
                        let rgb = match pattern {
                            0 => [93, 93, 93],
                            1 => [(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) * 255 / (w + h)) as u8],
                            2 => {
                                if (x / 4 + y / 4) % 2 == 0 {
                                    [255, 0, 0]
                                } else {
                                    [0, 0, 255]
                                }
                            }
                            _ => [(i.wrapping_mul(73) % 256) as u8, (i.wrapping_mul(137) % 256) as u8, (i.wrapping_mul(191) % 256) as u8],
                        };
                        [rgb[0], rgb[1], rgb[2], i as u8]
                    })
                    .collect();
                let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| p[..3].iter().copied()).collect();
                for q in [1, 25, 50, 75, 90, 100] {
                    let old = legacy(&rgba, w, h, q);
                    let new = encode(&rgba, w, h, q).unwrap();
                    let sof = |b: &[u8]| {
                        let at = b.windows(2).position(|p| p == [255, 192]).unwrap();
                        b[at..at + 19].to_vec()
                    };
                    // Same baseline precision, dimensions, component ids and 4:4:4 sampling.
                    assert_eq!(sof(&old), sof(&new));
                    let a = image::load_from_memory(&old).unwrap().into_rgb8();
                    let b = image::load_from_memory(&new).unwrap().into_rgb8();
                    assert_eq!(a.dimensions(), (w, h));
                    assert_eq!(b.dimensions(), (w, h));
                    // Lossy implementations need not produce the same bytes/pixels. Keep the
                    // mean decoded difference within 4/255 and source-error regression within
                    // 1/255 across flat fields, ramps, saturated edges and deterministic noise.
                    let delta = mae(&a, &b);
                    let regression = mae(&rgb, &b) - mae(&rgb, &a);
                    assert!(
                        delta <= 4.0 && regression <= 1.0,
                        "{w}x{h} pattern={pattern} quality={q}: decoded delta={delta}, source MAE increase={regression}"
                    );
                }
            }
        }
    }

    #[test]
    fn mjpeg_shared_preserves_alpha_ignoring_and_rejects_bad_dimensions() {
        let a = [10, 20, 30, 0];
        let b = [10, 20, 30, 255];
        assert_eq!(encode(&a, 1, 1, 75).unwrap(), encode(&b, 1, 1, 75).unwrap());
        assert_eq!(encode(&[10, 20, 30, 0, 99], 1, 1, 75).unwrap(), encode(&a, 1, 1, 75).unwrap());
        for (w, h, rgba) in
            [(0, 1, &[][..]), (1, 0, &[][..]), (1, 1, &[][..]), (2, 1, &a[..]), (1, 1, &[0; 8][..]), (u32::MAX, u32::MAX, &a[..]), (65_536, 1, &a[..])]
        {
            assert!(encode(rgba, w, h, 75).is_err());
        }
    }

    #[test]
    fn mjpeg_low_memory_matches_retained_encoder_for_high_entropy_frames() {
        let (width, height) = (1025u32, 513u32);
        let mut state = 0x1234_5678u32;
        let rgba: Vec<u8> = (0..width * height * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let expected = craft_jpeg::encode_baseline(&rgb, width as usize, height as usize, 3, 100, craft_jpeg::ChromaSubsampling::S444, &[]).unwrap();
        assert_eq!(encode(&rgba, width, height, 100).unwrap(), expected);
    }
}
