//! Length-prefixed (`avcC` / `hvcC`) access units to Annex B byte streams, the input format of
//! Windows' H.264 and HEVC decoder MFTs. Safe code, tested on every target.

use filmcraft_codecs::hw::NalStreamInfo;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// `sample` (NAL units behind `info.length_size`-byte lengths) as an Annex B access unit. With
/// `headers` the stream's parameter sets (from the sample entry) go first, unless the sample
/// carries parameter sets of its own: a decoder starting (or restarting after a flush) at this
/// sample then has what it needs. Errors on a malformed sample: lengths that run past its end,
/// empty NAL units or bytes after the last unit.
pub fn to_annex_b(info: &NalStreamInfo, sample: &[u8], headers: bool) -> Result<Vec<u8>, String> {
    let nals = info.nals(sample);
    let used = nals.iter().try_fold(0usize, |a, n| a.checked_add(n.len())?.checked_add(info.length_size)).ok_or("sample size overflows")?;
    if nals.is_empty() || used != sample.len() || nals.iter().any(|n| n.is_empty()) {
        return Err("malformed length-prefixed sample".into());
    }
    let in_band = nals.iter().any(|n| n.first().is_some_and(|&h| info.is_parameter_set(info.nal_type(h))));
    let add_headers = headers && !in_band;
    let prefix: usize = if add_headers { info.parameter_sets.iter().map(|p| p.len() + START_CODE.len()).sum() } else { 0 };
    let mut out = Vec::with_capacity(prefix + sample.len() + nals.len() * START_CODE.len());
    if add_headers {
        for p in &info.parameter_sets {
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(p);
        }
    }
    for n in nals {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(n);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_codecs::hw::NalCodec;

    fn info(length_size: usize, codec: NalCodec) -> NalStreamInfo {
        NalStreamInfo {
            codec,
            length_size,
            highest_tid: None,
            parameter_sets: vec![vec![0x67, 1, 2], vec![0x68, 3]],
            coded: (16, 16),
            crop: (0, 0, 16, 16),
            chroma_format_idc: 1,
            bit_depth_luma: 8,
            bit_depth_chroma: 8,
            interlaced: false,
            profile_idc: 100,
            color: filmcraft_color::ColorInfo::REC709,
            par: (1, 1),
            reorder: 0,
        }
    }

    #[test]
    fn converts_lengths_to_start_codes() {
        let i = info(4, NalCodec::H264);
        let s = [0, 0, 0, 2, 0x65, 9, 0, 0, 0, 3, 0x41, 7, 7];
        assert_eq!(to_annex_b(&i, &s, false).unwrap(), [0, 0, 0, 1, 0x65, 9, 0, 0, 0, 1, 0x41, 7, 7]);
        // headers go first, unless the sample brings its own
        let with = to_annex_b(&i, &s, true).unwrap();
        assert_eq!(&with[..11], [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1]);
        assert_eq!(with.len(), 4 + 3 + 4 + 2 + 13);
        let own = [0, 0, 0, 2, 0x67, 5, 0, 0, 0, 2, 0x65, 9];
        assert_eq!(to_annex_b(&i, &own, true).unwrap(), [0, 0, 0, 1, 0x67, 5, 0, 0, 0, 1, 0x65, 9]);
        // two-byte lengths
        assert_eq!(to_annex_b(&info(2, NalCodec::H264), &[0, 2, 0x65, 9], false).unwrap(), [0, 0, 0, 1, 0x65, 9]);
    }

    #[test]
    fn malformed_samples_are_errors() {
        let i = info(4, NalCodec::H264);
        for s in [&[][..], &[0, 0, 0], &[0, 0, 0, 9, 1], &[0, 0, 0, 1, 0x65, 0xff], &[0, 0, 0, 0], &[0xff, 0xff, 0xff, 0xff, 1]] {
            assert!(to_annex_b(&i, s, true).is_err(), "{s:?}");
        }
    }
}
