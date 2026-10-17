//! Hardware acceleration hook (stateless decoders such as VA-API).
//!
//! With an [`Accelerator`] installed ([`crate::Decoder::with_accelerator`]), the decoder does all
//! of its usual work (parameter sets, slice headers, picture order counts, reference marking,
//! reference list construction and modification, output order) but, instead of reconstructing a
//! picture's macroblocks, hands the picture to the accelerator as an [`AccelPicture`]. Output is
//! [`AccelOutput`]s naming pictures by id, in output order ([`crate::Decoder::decode_accel`]); the
//! accelerator owns the pixels. The reference logic is the software decoder's own, so both decode
//! the same pictures in the same order.
//!
//! Streams the hook does not cover (frame_num gaps, which need "non-existing" reference frames)
//! make the decoder return an error, so a caller can switch to software decoding.

use crate::params::{Pps, Sps};
use crate::slice::SliceHeader;

/// A reference picture as an accelerator sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccelRef {
    /// The picture's id ([`AccelPicture::id`] when it was decoded).
    pub id: u32,
    /// PicOrderCnt of the frame.
    pub poc: i32,
    /// frame_num (short-term) of the picture.
    pub frame_num: u32,
    /// LongTermFrameIdx (long-term references).
    pub long_term_frame_idx: u32,
    pub long_term: bool,
}

/// One slice of a picture.
pub struct AccelSlice<'a> {
    pub header: &'a SliceHeader,
    pub pps: &'a Pps,
    /// The NAL unit as it was in the stream (NAL header byte first, emulation prevention bytes in).
    pub nal: &'a [u8],
    /// Bits from the start of the NAL unit (header byte included) to slice_data(), counted after
    /// removing emulation prevention bytes.
    pub data_bit_offset: usize,
    /// RefPicList0 / RefPicList1 after modification (8.2.4).
    pub lists: [Vec<AccelRef>; 2],
}

/// One picture to decode.
pub struct AccelPicture<'a> {
    pub id: u32,
    /// TopFieldOrderCnt / BottomFieldOrderCnt.
    pub poc: (i32, i32),
    pub frame_num: u32,
    /// nal_ref_idc != 0.
    pub reference: bool,
    pub idr: bool,
    pub sps: &'a Sps,
    pub pps: &'a Pps,
    /// The pictures marked as used for reference before this one is stored (the DPB references).
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
}

/// A stateless hardware decoder.
pub trait Accelerator: Send {
    /// Decode `pic` (decode order). An error stops decoding with that error.
    fn decode_picture(&mut self, pic: &AccelPicture<'_>) -> Result<(), String>;
    /// After a picture, the ids still needed (references and pictures not yet output); any other
    /// picture's storage may be reused.
    fn retain(&mut self, live: &[u32]);
}
