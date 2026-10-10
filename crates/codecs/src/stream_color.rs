//! The colour description a video stream carries in its own bitstream, for containers that don't
//! repeat it: the H.264 / HEVC sequence parameter set's VUI (from `avcC` / `hvcC`) and the AV1
//! sequence header's `color_config` (from `av1C`), read with the decoders' own parsers.
//!
//! ffmpeg writes MP4 without a `colr` box unless asked (`-movflags +write_colr`), so for most
//! files the bitstream is the only place their colour is described. [`resolve`] merges it with
//! what the container signals, which wins.

use filmcraft_color::{ColorInfo, Matrix, Range, Transfer};
use filmcraft_isobmff::{AvcConfig, CodecConfig, HevcConfig};

use crate::video::primaries_from_code;

/// ITU-T H.273 `Unspecified`.
const UNSPECIFIED: u8 = 2;

/// Colour code points (ITU-T H.273 / ISO/IEC 23091-2) and range as one place in a file (a
/// container colour box, a sequence header) signals them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorCodes {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    /// `None`: this place doesn't say.
    pub full_range: Option<bool>,
}

impl ColorCodes {
    /// From wider code points (a `colr` box or a Matroska `Colour` element): a value that doesn't
    /// fit a byte is no H.273 code point, so it counts as unspecified.
    pub fn from_wide(primaries: u64, transfer: u64, matrix: u64, full_range: Option<bool>) -> Self {
        let code = |v: u64| u8::try_from(v).unwrap_or(UNSPECIFIED);
        Self { primaries: code(primaries), transfer: code(transfer), matrix: code(matrix), full_range }
    }
}

/// The colour a video codec configuration's bitstream signals: the VUI of the first SPS in an
/// `avcC` / `hvcC`, or the `color_config` of the sequence header in an `av1C`. `None` for other
/// codecs, an SPS without VUI, and a configuration that is truncated or corrupt.
pub fn from_codec_config(codec: &CodecConfig) -> Option<ColorCodes> {
    match codec {
        CodecConfig::Avc(c) => avc(c),
        CodecConfig::Hevc(c) => hevc(c),
        CodecConfig::Av1(c) => av1(&c.config_obus),
        _ => None,
    }
}

fn avc(c: &AvcConfig) -> Option<ColorCodes> {
    // NAL header byte: nal_unit_type 7 is an SPS
    let nal = c.sps.iter().find(|n| n.first().is_some_and(|h| h & 0x1f == 7))?;
    let sps = filmcraft_h264::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(nal.get(1..)?)).ok()?;
    let v = sps.vui?;
    Some(ColorCodes { primaries: v.colour_primaries, transfer: v.transfer_characteristics, matrix: v.matrix_coefficients, full_range: Some(v.full_range) })
}

fn hevc(c: &HevcConfig) -> Option<ColorCodes> {
    // two-byte NAL header: nal_unit_type 33 is an SPS (whatever array the record files it under)
    let nal = c.arrays.iter().flat_map(|a| &a.nalus).find(|n| n.first().is_some_and(|h| (h >> 1) & 0x3f == 33))?;
    let sps = filmcraft_hevc::params::Sps::parse(&filmcraft_bitstream::unescape_rbsp(nal.get(2..)?)).ok()?;
    let v = sps.vui?;
    Some(ColorCodes { primaries: v.colour_primaries, transfer: v.transfer_characteristics, matrix: v.matrix_coefficients, full_range: Some(v.full_range) })
}

fn av1(config_obus: &[u8]) -> Option<ColorCodes> {
    // OBU type 1: the sequence header
    let (_, payload) = crate::hw_frame::obus(config_obus).into_iter().find(|(t, _)| *t == 1)?;
    let c = filmcraft_av1::SequenceHeader::parse(payload).ok()?.color;
    Some(ColorCodes { primaries: c.color_primaries, transfer: c.transfer_characteristics, matrix: c.matrix_coefficients, full_range: Some(c.color_range) })
}

/// Colour from the code points a file signals, most authoritative first (the container's colour
/// box, then the bitstream's): the matrix, transfer, primaries and range each come from the first
/// source that signals a value we know. What no source signals keeps the defaults: the matrix by
/// frame size ([`filmcraft_frame::default_matrix`]: BT.601 for SD, BT.709 otherwise), BT.709
/// transfer and primaries, limited range.
pub fn resolve(width: u32, height: u32, sources: &[ColorCodes]) -> ColorInfo {
    let mut c = ColorInfo { matrix: filmcraft_frame::default_matrix(width, height), ..ColorInfo::REC709 };
    if let Some(m) = sources.iter().find_map(|s| Matrix::from_code(s.matrix)) {
        c.matrix = m;
    }
    if let Some(t) = sources.iter().find_map(|s| Transfer::from_code(s.transfer)) {
        c.transfer = t;
    }
    if let Some(p) = sources.iter().find_map(|s| primaries_from_code(s.primaries)) {
        c.primaries = p;
    }
    if sources.iter().find_map(|s| s.full_range) == Some(true) {
        c.range = Range::Full;
    }
    c
}
