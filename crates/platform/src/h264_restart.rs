//! H.264 parameter sets carried by an independently decodable access unit.
//! ISO/IEC 14496-15 AVC sample entries (avc3) and ITU-T H.264 §§7.3.2, 7.3.3.

use filmcraft_bitstream::{BitReader, unescape_rbsp};
use filmcraft_codecs::hw::NalStreamInfo;
use filmcraft_isobmff::{AvcConfig, CodecConfig, SampleEntry};

fn header(nal: &[u8]) -> Option<Vec<u8>> {
    // The IDs and leading slice fields occupy only the start of the NAL.
    Some(unescape_rbsp(nal.get(1..nal.len().min(64))?))
}

fn sps_id(nal: &[u8]) -> Option<u32> {
    let rbsp = header(nal)?;
    // Skip profile, constraint flags and level before seq_parameter_set_id.
    BitReader::new(rbsp.get(3..)?).read_ue().ok().filter(|&id| id <= 31)
}

fn pps_ids(nal: &[u8]) -> Option<(u32, u32)> {
    let rbsp = header(nal)?;
    let mut bits = BitReader::new(&rbsp);
    let pps = bits.read_ue().ok()?;
    let sps = bits.read_ue().ok()?;
    (pps <= 255 && sps <= 31).then_some((pps, sps))
}

/// Read the last SPS/PPS preceding the IDR slices. Handle replacement of a
/// single pair by the same IDs so the new session and fallback both retain
/// every parameter set that subsequent access units may reference.
pub(crate) fn entry(info: &NalStreamInfo, sample: &[u8], original: &SampleEntry) -> Option<SampleEntry> {
    let CodecConfig::Avc(initial) = &original.codec else { return None };
    let (mut sps, mut pps) = (None, None);
    let mut slice_pps = None;
    for nal in info.nals(sample) {
        let tag = *nal.first()?;
        if tag & 0x80 != 0 {
            return None;
        }
        match tag & 31 {
            7 if slice_pps.is_none() => {
                let id = sps_id(nal)?;
                if sps.replace((id, nal)).is_some_and(|(previous, _)| previous != id) {
                    return None;
                }
            }
            8 if slice_pps.is_none() => {
                let ids = pps_ids(nal)?;
                if pps.replace((ids, nal)).is_some_and(|(previous, _)| previous != ids) {
                    return None;
                }
            }
            5 => {
                let rbsp = header(nal)?;
                let mut bits = BitReader::new(&rbsp);
                bits.read_ue().ok()?; // first_mb_in_slice
                bits.read_ue().ok()?; // slice_type
                let id = bits.read_ue().ok()?;
                if slice_pps.replace(id).is_some_and(|previous| previous != id) {
                    return None;
                }
            }
            1..=4 | 7 | 8 | 13..=15 | 19..=21 => return None,
            _ => {}
        }
    }
    let ((sps_id_value, sps), (ids, pps)) = (sps?, pps?);
    if ids != (slice_pps?, sps_id_value)
        || sps.len() > usize::from(u16::MAX)
        || pps.len() > usize::from(u16::MAX)
        || initial.sps.iter().any(|nal| sps_id(nal) != Some(sps_id_value))
        || initial.pps.iter().any(|nal| pps_ids(nal) != Some(ids))
    {
        return None;
    }
    let mut entry = original.clone();
    entry.codec = CodecConfig::Avc(AvcConfig::new(vec![sps.to_vec()], vec![pps.to_vec()], info.length_size as u8));
    Some(entry)
}
