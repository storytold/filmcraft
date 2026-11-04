//! Frame tag (9.1, 19.1) and the frame header in the first partition (9.2-9.11, 19.2).

use crate::boolcoder::BoolDecoder;
use crate::error::{Error, Result, ensure};

/// The uncompressed data chunk at the start of every frame (9.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTag {
    pub key_frame: bool,
    /// Bitstream version (0-3): reconstruction filter and full-pixel chroma.
    pub version: u8,
    pub show_frame: bool,
    pub first_part_size: usize,
    /// Key frames only: dimensions and upscaling codes.
    pub width: u16,
    pub height: u16,
    pub horizontal_scale: u8,
    pub vertical_scale: u8,
    /// Bytes of the uncompressed chunk (3, or 10 for key frames).
    pub header_len: usize,
}

impl FrameTag {
    pub fn parse(data: &[u8]) -> Result<FrameTag> {
        let [b0, b1, b2, ..] = *data else {
            return Err(Error::Invalid("frame shorter than its 3-byte tag".into()));
        };
        let tmp = u32::from(b0) | u32::from(b1) << 8 | u32::from(b2) << 16;
        let key_frame = tmp & 1 == 0;
        let mut tag = FrameTag {
            key_frame,
            version: ((tmp >> 1) & 7) as u8,
            show_frame: (tmp >> 4) & 1 == 1,
            first_part_size: (tmp >> 5) as usize,
            width: 0,
            height: 0,
            horizontal_scale: 0,
            vertical_scale: 0,
            header_len: 3,
        };
        if key_frame {
            let Some(c) = data.get(3..10) else {
                return Err(Error::Invalid("key frame shorter than its 10-byte header".into()));
            };
            ensure!(c[0] == 0x9d && c[1] == 0x01 && c[2] == 0x2a, "bad key frame start code");
            let w = u16::from_le_bytes([c[3], c[4]]);
            let h = u16::from_le_bytes([c[5], c[6]]);
            tag.width = w & 0x3fff;
            tag.horizontal_scale = (w >> 14) as u8;
            tag.height = h & 0x3fff;
            tag.vertical_scale = (h >> 14) as u8;
            tag.header_len = 10;
        }
        Ok(tag)
    }
}

/// Segment-based adjustments (9.3): carried from frame to frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Segmentation {
    pub enabled: bool,
    pub update_map: bool,
    /// Feature values are absolute (else deltas from the frame values).
    pub absolute: bool,
    pub quant: [i8; 4],
    pub lf_level: [i8; 4],
    pub tree_probs: [u8; 3],
}

/// Loop filter adjustments by reference frame and mode (9.4): carried from frame to frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LfDeltas {
    pub enabled: bool,
    /// Intra, last, golden, altref.
    pub ref_deltas: [i8; 4],
    /// B_PRED, ZEROMV, other MV modes, SPLITMV.
    pub mode_deltas: [i8; 4],
}

/// Probabilities that persist across frames (until the next key frame), saved and restored
/// around a frame whose `refresh_entropy_probs` is 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntropyContext {
    pub coeff: [[[[u8; 11]; 3]; 8]; 4],
    pub ymode: [u8; 4],
    pub uv_mode: [u8; 3],
    pub mv: [[u8; 19]; 2],
}

impl Default for EntropyContext {
    fn default() -> Self {
        use crate::spec_tables::{DEFAULT_COEFF_PROBS, DEFAULT_MV_PROBS, UV_MODE_PROBS, YMODE_PROBS};
        let mut coeff = [[[[0u8; 11]; 3]; 8]; 4];
        for (i, p) in coeff.iter_mut().flatten().flatten().flatten().enumerate() {
            *p = DEFAULT_COEFF_PROBS[i];
        }
        let mut mv = [[0u8; 19]; 2];
        for (i, p) in mv.iter_mut().flatten().enumerate() {
            *p = DEFAULT_MV_PROBS[i];
        }
        EntropyContext { coeff, ymode: YMODE_PROBS, uv_mode: UV_MODE_PROBS, mv }
    }
}

/// Dequantization indices (9.6).
#[derive(Clone, Copy, Debug, Default)]
pub struct QuantIndices {
    pub y_ac: i32,
    pub y_dc_delta: i32,
    pub y2_dc_delta: i32,
    pub y2_ac_delta: i32,
    pub uv_dc_delta: i32,
    pub uv_ac_delta: i32,
}

/// Per-frame header fields (9.2-9.11) that do not persist.
#[derive(Clone, Debug, Default)]
pub struct FrameHeader {
    pub color_space: u8,
    pub clamping_type: u8,
    /// 0 normal, 1 simple.
    pub filter_type: u8,
    pub filter_level: u8,
    pub sharpness: u8,
    pub partitions: usize,
    pub quant: QuantIndices,
    pub refresh_golden: bool,
    pub refresh_altref: bool,
    /// 0 none, 1 last, 2 altref.
    pub copy_to_golden: u8,
    /// 0 none, 1 last, 2 golden.
    pub copy_to_altref: u8,
    pub sign_bias_golden: bool,
    pub sign_bias_altref: bool,
    pub refresh_entropy: bool,
    pub refresh_last: bool,
    /// `prob_skip_false` when `mb_no_skip_coeff` is set.
    pub skip_prob: Option<u8>,
    pub prob_intra: u8,
    pub prob_last: u8,
    pub prob_golden: u8,
}

/// segmentation_header (9.3).
fn read_segmentation(d: &mut BoolDecoder, seg: &mut Segmentation) {
    seg.enabled = d.read_flag();
    seg.update_map = false;
    if !seg.enabled {
        return;
    }
    seg.update_map = d.read_flag();
    let update_data = d.read_flag();
    if update_data {
        seg.absolute = d.read_flag();
        for q in &mut seg.quant {
            *q = d.read_optional_signed(7) as i8;
        }
        for l in &mut seg.lf_level {
            *l = d.read_optional_signed(6) as i8;
        }
    }
    if seg.update_map {
        for p in &mut seg.tree_probs {
            *p = if d.read_flag() { d.read_literal(8) as u8 } else { crate::tables::DEFAULT_SEGMENT_PROB };
        }
    }
}

/// Loop filter delta adjustments (9.6).
fn read_lf_deltas(d: &mut BoolDecoder, lf: &mut LfDeltas) {
    lf.enabled = d.read_flag();
    if lf.enabled && d.read_flag() {
        for v in lf.ref_deltas.iter_mut().chain(lf.mode_deltas.iter_mut()) {
            if d.read_flag() {
                *v = d.read_signed(6) as i8;
            }
        }
    }
}

/// Reference frame refresh / copy / sign bias flags (9.7, 9.8) and the entropy refresh flag.
fn read_reference_updates(d: &mut BoolDecoder, key_frame: bool, h: &mut FrameHeader) {
    if key_frame {
        h.refresh_golden = true;
        h.refresh_altref = true;
        h.refresh_entropy = d.read_flag();
        h.refresh_last = true;
        return;
    }
    h.refresh_golden = d.read_flag();
    h.refresh_altref = d.read_flag();
    if !h.refresh_golden {
        h.copy_to_golden = d.read_literal(2) as u8;
    }
    if !h.refresh_altref {
        h.copy_to_altref = d.read_literal(2) as u8;
    }
    h.sign_bias_golden = d.read_flag();
    h.sign_bias_altref = d.read_flag();
    h.refresh_entropy = d.read_flag();
    h.refresh_last = d.read_flag();
}

/// token_prob_update (13.4).
fn read_token_prob_updates(d: &mut BoolDecoder, ctx: &mut EntropyContext) {
    let mut i = 0;
    for plane in ctx.coeff.iter_mut() {
        for band in plane.iter_mut() {
            for c in band.iter_mut() {
                for p in c.iter_mut() {
                    if d.read_bool(crate::spec_tables::COEFF_UPDATE_PROBS[i]) {
                        *p = d.read_literal(8) as u8;
                    }
                    i += 1;
                }
            }
        }
    }
}

/// Inter frame probabilities: intra/last/golden, mode probabilities and mv_prob_update (17.2).
fn read_inter_probs(d: &mut BoolDecoder, h: &mut FrameHeader, ctx: &mut EntropyContext) {
    h.prob_intra = d.read_literal(8) as u8;
    h.prob_last = d.read_literal(8) as u8;
    h.prob_golden = d.read_literal(8) as u8;
    if d.read_flag() {
        for p in &mut ctx.ymode {
            *p = d.read_literal(8) as u8;
        }
    }
    if d.read_flag() {
        for p in &mut ctx.uv_mode {
            *p = d.read_literal(8) as u8;
        }
    }
    let mut i = 0;
    for comp in ctx.mv.iter_mut() {
        for p in comp.iter_mut() {
            if d.read_bool(crate::spec_tables::MV_UPDATE_PROBS[i]) {
                let x = d.read_literal(7) as u8;
                *p = if x != 0 { x << 1 } else { 1 };
            }
            i += 1;
        }
    }
}

/// Read the frame header from the first partition, updating the persistent state.
/// `saved` receives the entropy context to restore after the frame when
/// `refresh_entropy_probs` is 0.
pub fn read_header(
    d: &mut BoolDecoder,
    key_frame: bool,
    seg: &mut Segmentation,
    lf: &mut LfDeltas,
    ctx: &mut EntropyContext,
    saved: &mut Option<EntropyContext>,
) -> FrameHeader {
    let mut h = FrameHeader::default();
    if key_frame {
        h.color_space = d.read_literal(1) as u8;
        h.clamping_type = d.read_literal(1) as u8;
    }
    read_segmentation(d, seg);
    h.filter_type = d.read_literal(1) as u8;
    h.filter_level = d.read_literal(6) as u8;
    h.sharpness = d.read_literal(3) as u8;
    read_lf_deltas(d, lf);
    h.partitions = 1 << d.read_literal(2);
    h.quant = QuantIndices {
        y_ac: d.read_literal(7) as i32,
        y_dc_delta: d.read_optional_signed(4),
        y2_dc_delta: d.read_optional_signed(4),
        y2_ac_delta: d.read_optional_signed(4),
        uv_dc_delta: d.read_optional_signed(4),
        uv_ac_delta: d.read_optional_signed(4),
    };
    read_reference_updates(d, key_frame, &mut h);
    // The probabilities in effect before this frame's updates come back after it.
    *saved = if h.refresh_entropy { None } else { Some(ctx.clone()) };
    read_token_prob_updates(d, ctx);
    h.skip_prob = if d.read_flag() { Some(d.read_literal(8) as u8) } else { None };
    if !key_frame {
        read_inter_probs(d, &mut h, ctx);
    }
    h
}
