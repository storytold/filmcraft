//! Length-prefixed (`avcC` / `hvcC`) access units to Annex B byte streams, the input format of
//! Windows' H.264 and HEVC decoder MFTs, and Annex B byte streams (what the hardware encoders
//! produce) back to the NAL units and length-prefixed samples the MP4 muxer takes. Safe code,
//! tested on every target.

use filmcraft_codecs::hw::{NalCodec, NalStreamInfo};

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

/// The NAL units of an Annex B byte stream (start codes `00 00 01` / `00 00 00 01`). Empty NAL
/// units are left out; trailing zero bytes are padding.
pub fn nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while let Some(w) = data.get(i..i.saturating_add(3)) {
        if w == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(data.len(), |n| n.saturating_sub(3));
        // trailing zero bytes belong to the next start code (4-byte form) or are padding
        while e > s && data.get(e - 1) == Some(&0) {
            e -= 1;
        }
        if let Some(n) = data.get(s..e).filter(|n| !n.is_empty()) {
            out.push(n);
        }
    }
    out
}

/// The NAL unit type of `nal` (`None` when it is too short to have a header).
pub fn nal_type(codec: NalCodec, nal: &[u8]) -> Option<u8> {
    match codec {
        NalCodec::H264 => nal.first().map(|b| b & 0x1f),
        // two header bytes: forbidden_zero_bit, nal_unit_type (6), layer id (6), temporal id plus 1 (3)
        NalCodec::Hevc => match nal {
            [b, _, ..] => Some((b >> 1) & 0x3f),
            _ => None,
        },
    }
}

/// Annex B to 4-byte length-prefixed NAL units, dropping the units that belong in the sample
/// entry's configuration record instead of in a sample: parameter sets and access unit
/// delimiters, and for HEVC the end-of-sequence markers (the file is one coded video sequence).
/// A NAL unit too short for its header is dropped. Errors on a stream with no picture data.
pub fn to_length_prefixed(codec: NalCodec, data: &[u8]) -> Result<Vec<u8>, String> {
    let units = nals(data);
    if units.is_empty() {
        return Err("the encoder produced no NAL units".into());
    }
    let mut out = Vec::with_capacity(data.len());
    for n in units {
        let dropped = match nal_type(codec, n) {
            None => true,
            Some(t) => match codec {
                NalCodec::H264 => matches!(t, 7..=9),
                NalCodec::Hevc => matches!(t, 32..=37),
            },
        };
        if dropped {
            continue;
        }
        let len = u32::try_from(n.len()).map_err(|_| "a NAL unit longer than 4 GiB".to_string())?;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(n);
    }
    if out.is_empty() {
        return Err("the encoder produced no picture data".into());
    }
    Ok(out)
}

/// The SPS and PPS NAL units of an H.264 Annex B byte string.
pub fn split_h264_parameter_sets(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let units = nals(data);
    let find = |t: u8| units.iter().find(|n| nal_type(NalCodec::H264, n) == Some(t)).map(|n| n.to_vec());
    match (find(7), find(8)) {
        (Some(s), Some(p)) => Ok((s, p)),
        _ => Err("the encoder returned no SPS / PPS".into()),
    }
}

/// The VPS, SPS and PPS NAL units of an HEVC Annex B byte string.
pub fn split_hevc_parameter_sets(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>), String> {
    let units = nals(data);
    let find = |t: u8| units.iter().find(|n| nal_type(NalCodec::Hevc, n) == Some(t)).map(|n| n.to_vec());
    match (find(32), find(33), find(34)) {
        (Some(v), Some(s), Some(p)) => Ok((v, s, p)),
        _ => Err("the encoder returned no VPS / SPS / PPS".into()),
    }
}

/// Whether an Annex B access unit holds an IDR picture (H.264) or any IRAP picture (HEVC), i.e.
/// whether a decoder can start decoding at it.
pub fn is_key(access_unit: &[u8], codec: NalCodec) -> bool {
    nals(access_unit).iter().any(|n| match nal_type(codec, n) {
        Some(5) if codec == NalCodec::H264 => true,
        Some(t) if codec == NalCodec::Hevc => (16..=23).contains(&t),
        _ => false,
    })
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

    const H264_IDR: [u8; 19] = [0, 0, 0, 1, 0x09, 0x10, 0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3];
    const H264_P: [u8; 10] = [0, 0, 0, 1, 0x09, 0x10, 0, 0, 0, 1];
    const H264_SLICE: [u8; 7] = [0, 0, 0, 1, 0x41, 9, 9];

    #[test]
    fn splits_annex_b_with_both_start_code_forms() {
        let mut s = H264_IDR.to_vec();
        s.extend_from_slice(&H264_SLICE);
        s.push(0);
        let units = nals(&s);
        assert_eq!(units, vec![&[0x09, 0x10][..], &[0x67, 1, 2][..], &[0x68, 3][..], &[0x41, 9, 9][..]]);
        // 4-byte lengths, with the AUD, SPS and PPS left in the configuration record
        assert_eq!(to_length_prefixed(NalCodec::H264, &s).unwrap(), [0, 0, 0, 3, 0x41, 9, 9]);
        assert_eq!(split_h264_parameter_sets(&s).unwrap(), (vec![0x67, 1, 2], vec![0x68, 3]));
    }

    #[test]
    fn tells_idr_from_a_predicted_picture() {
        // an access unit with an IDR slice (type 5) next to one with a predicted slice (type 1)
        let mut idr = H264_IDR.to_vec();
        idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 9, 9]);
        assert!(is_key(&idr, NalCodec::H264));
        assert!(!is_key(&H264_SLICE, NalCodec::H264));
        assert!(!is_key(&H264_P, NalCodec::H264));
    }

    #[test]
    fn hostile_encoder_output_is_an_error_never_a_panic() {
        for s in [&[][..], &[0, 0, 1], &[1, 2, 3], &[0, 0, 1, 0x09, 0xf0], &[0, 0, 0, 1, 0x67, 1]] {
            assert!(to_length_prefixed(NalCodec::H264, s).is_err(), "{s:?}");
        }
        assert!(split_h264_parameter_sets(&[0, 0, 1, 0x65, 1]).is_err());
        assert!(split_h264_parameter_sets(&[0, 0, 1]).is_err());
        assert!(split_hevc_parameter_sets(&[0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3]).is_err());
        assert!(split_hevc_parameter_sets(&[0, 0, 0, 1, 0x40, 0x01, 1, 0, 0, 1, 0x42, 0x01, 2]).is_err());
        assert_eq!(nal_type(NalCodec::H264, &[0xff, 0x1f]), Some(31));
        assert_eq!(nal_type(NalCodec::H264, &[]), None);
        assert_eq!(nal_type(NalCodec::Hevc, &[0x26, 0x01]), Some(19));
        assert_eq!(nal_type(NalCodec::Hevc, &[0x26]), None);
    }

    #[test]
    fn hevc_parameter_sets_and_marker_types() {
        let s = [0, 0, 0, 1, 0x40, 1, 1, 0, 0, 1, 0x42, 1, 2, 0, 0, 1, 0x44, 1, 3, 0, 0, 0, 1, 0x26, 1, 2];
        assert_eq!(split_hevc_parameter_sets(&s).unwrap(), (vec![0x40, 1, 1], vec![0x42, 1, 2], vec![0x44, 1, 3]));
        assert!(is_key(&s, NalCodec::Hevc));
        // the parameter sets and the access unit delimiter are dropped, the IRAP slice stays
        assert_eq!(to_length_prefixed(NalCodec::Hevc, &s).unwrap(), [0, 0, 0, 3, 0x26, 1, 2]);
    }
}
