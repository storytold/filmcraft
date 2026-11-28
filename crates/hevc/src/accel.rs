//! Hardware acceleration hook (stateless decoders such as VA-API).
//!
//! With an [`Accelerator`] installed ([`crate::Decoder::with_accelerator`]), the decoder does all
//! of its usual work (parameter sets, slice segment headers, picture order counts, reference
//! picture sets, reference list construction, RASL handling, output order) but, instead of
//! reconstructing a picture's CTBs, hands the picture to the accelerator as an [`AccelPicture`].
//! Output is [`AccelOutput`]s naming pictures by id, in output order
//! ([`crate::Decoder::decode_accel`]); the accelerator owns the pixels. The reference logic is the
//! software decoder's own, so both decode the same pictures in the same order.
//!
//! Streams the hook does not cover (missing reference pictures, which the software decoder
//! generates) make the decoder return an error, so a caller can switch to software decoding.

use crate::params::{Pps, Sps};
use crate::slice::SliceHeader;

/// A reference picture as an accelerator sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccelRef {
    /// The picture's id ([`AccelPicture::id`] when it was decoded).
    pub id: u32,
    pub poc: i32,
    pub long_term: bool,
}

/// One slice segment of a picture.
pub struct AccelSlice<'a> {
    /// The segment's header (for a dependent segment, completed from its independent segment).
    pub header: &'a SliceHeader,
    /// The NAL unit as it was in the stream (2-byte NAL header first, emulation prevention bytes
    /// in).
    pub nal: &'a [u8],
    /// Bytes from the start of the NAL unit (header included) to slice_segment_data(), counted
    /// after removing emulation prevention bytes.
    pub data_byte_offset: usize,
    /// Emulation prevention bytes inside the slice segment header.
    pub header_emulation_bytes: usize,
    /// RefPicList0 / RefPicList1 (8.3.4).
    pub lists: [Vec<AccelRef>; 2],
}

/// One picture to decode.
pub struct AccelPicture<'a> {
    pub id: u32,
    pub poc: i32,
    pub sps: &'a Sps,
    pub pps: &'a Pps,
    /// nal_unit_type of the picture is IRAP / IDR.
    pub irap: bool,
    pub idr: bool,
    /// Every slice is an I slice.
    pub intra: bool,
    /// The reference picture set: RefPicSetStCurrBefore, StCurrAfter, LtCurr.
    pub st_curr_before: Vec<AccelRef>,
    pub st_curr_after: Vec<AccelRef>,
    pub lt_curr: Vec<AccelRef>,
    /// Every picture in the DPB still marked as used for reference (the current RPS, including
    /// pictures only kept for following pictures).
    pub refs: Vec<AccelRef>,
    pub slices: Vec<AccelSlice<'a>>,
}

/// A picture leaving the decoder in output order.
#[derive(Clone, Debug, PartialEq)]
pub struct AccelOutput {
    pub id: u32,
    pub pts: i64,
    pub key: bool,
    /// Visible rectangle in luma samples: x, y, width, height.
    pub crop: (u32, u32, u32, u32),
    pub full_range: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub sar: (u16, u16),
    pub bit_depth: u32,
}

/// A stateless hardware decoder.
pub trait Accelerator: Send {
    /// Decode `pic` (decode order). An error stops decoding with that error.
    fn decode_picture(&mut self, pic: &AccelPicture<'_>) -> Result<(), String>;
    /// After a picture, the ids still needed (references and pictures not yet output); any other
    /// picture's storage may be reused.
    fn retain(&mut self, live: &[u32]);
}

/// Emulation prevention bytes (`00 00 03`) in the first `rbsp_len` RBSP bytes of `nal_payload`
/// (the NAL unit after its header).
pub(crate) fn emulation_bytes_before(nal_payload: &[u8], rbsp_len: usize) -> usize {
    let (mut zeros, mut out, mut count) = (0usize, 0usize, 0usize);
    for &b in nal_payload {
        if out >= rbsp_len {
            break;
        }
        if zeros >= 2 && b == 3 {
            count += 1;
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::emulation_bytes_before;

    #[test]
    fn counts_emulation_prevention_bytes() {
        assert_eq!(emulation_bytes_before(&[1, 2, 3, 4], 4), 0);
        assert_eq!(emulation_bytes_before(&[0, 0, 3, 1, 0, 0, 3, 0], 4), 1);
        assert_eq!(emulation_bytes_before(&[0, 0, 3, 1, 0, 0, 3, 0], 6), 2);
        assert_eq!(emulation_bytes_before(&[], 10), 0);
    }
}
