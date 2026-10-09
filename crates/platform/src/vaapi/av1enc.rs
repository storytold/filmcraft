//! AV1 export encoding on VA-API. [`factory`] is registered with
//! `filmcraft_export::register_encoder`, and [`available`] as the format probe: AV1 has no
//! software encoder, so choosing the format is the opt-in and the format is offered only where a
//! GPU encodes it.
//!
//! Main profile (8-bit 4:2:0 SDR), key + inter frames (one reference: the previous frame, in every
//! slot), 64×64 superblocks, transform mode TX_MODE_SELECT, and none of the tools the GPU does not
//! encode with (warped motion, compound modes, filter intra, superres, loop restoration). Every
//! frame header is written here and handed to the driver as a packed header (without one the
//! driver fails); its rate control writes the base_q_idx, loop filter and CDEF values it chose into
//! it at the bit offsets we give, and appends the tile group OBU. The sequence header is the
//! driver's own (it needs a packed one but ignores it): it goes into the `av1C` and stays in key frame samples;
//! temporal delimiters are dropped. The driver's sequence header codes a width that is not a whole
//! number of superblocks rounded up, and a height that is not a multiple of 16 two rows taller
//! (1080 becomes 1082; decoders show those rows, render_size or not), so such sizes are declined;
//! its sequence header is checked against what our frame headers assume, so a driver that writes
//! something else gives an error, never a broken file.

use filmcraft_bitstream::BitWriter;
use filmcraft_export::{BitrateMode, EncodedPacket, EncoderFrame, ExportError, ExportSettings, FieldOrder, Format, Result, VideoEncoder};
use filmcraft_isobmff::{Av1Config, SampleEntry};
use filmcraft_time::FrameRate;

use super::device::{Display, EncodeSession};
use super::ffi::*;

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_FRAME_HEADER: u8 = 3;
/// OrderHintBits.
const ORDER_HINT_BITS: u32 = 8;
const PRIMARY_REF_NONE: u8 = 7;
/// Starting values; the driver's rate control chooses the ones it codes.
const DEFAULT_QINDEX: u8 = 128;
const DEFAULT_FILTER_LEVEL: u8 = 10;
const DEFAULT_CDEF_PRI: u8 = 4;
const DEFAULT_CDEF_SEC: u8 = 1;
const DEFAULT_CDEF_STRENGTH: u8 = (DEFAULT_CDEF_PRI << 2) | DEFAULT_CDEF_SEC;

/// What the encoder was asked to do.
#[derive(Clone, Debug)]
struct Config {
    width: u32,
    height: u32,
    fps: (u32, u32),
    kbps: u32,
    max_kbps: u32,
    cbr: bool,
    keyint: u32,
    /// seq_level_idx.
    level: u8,
}

/// The lowest level for a picture size, display rate and bitrate (Annex A.3, Main tier):
/// (seq_level_idx, MaxPicSize, MaxHSize, MaxVSize, MaxDisplayRate, MaxBitrate in kbit/s).
fn pick_level(w: u32, h: u32, fps: f64, kbps: u32) -> u8 {
    const LEVELS: [(u8, u64, u32, u32, u64, u32); 13] = [
        (0, 147_456, 2048, 1152, 4_423_680, 1_500),
        (1, 278_784, 2816, 1584, 8_363_520, 3_000),
        (4, 665_856, 4352, 2448, 19_975_680, 6_000),
        (5, 1_065_024, 5504, 3096, 31_950_720, 10_000),
        (8, 2_359_296, 6144, 3456, 70_778_880, 12_000),
        (9, 2_359_296, 6144, 3456, 141_557_760, 20_000),
        (12, 8_912_896, 8192, 4352, 267_386_880, 30_000),
        (13, 8_912_896, 8192, 4352, 534_773_760, 40_000),
        (14, 8_912_896, 8192, 4352, 1_069_547_520, 60_000),
        (15, 8_912_896, 8192, 4352, 1_069_547_520, 60_000),
        (16, 35_651_584, 16384, 8704, 1_069_547_520, 60_000),
        (17, 35_651_584, 16384, 8704, 2_139_095_040, 100_000),
        (18, 35_651_584, 16384, 8704, 4_278_190_080, 160_000),
    ];
    let ps = u64::from(w) * u64::from(h);
    let rate = (ps as f64 * fps).ceil() as u64;
    LEVELS.iter().find(|&&(_, mps, mh, mv, mr, br)| ps <= mps && w <= mh && h <= mv && rate <= mr && kbps <= br).map_or(31, |l| l.0)
}

/// Why the GPU does not take this export, or its configuration.
fn config(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> std::result::Result<Config, String> {
    if format != Format::Av1 || s.format.is_mxf() {
        return Err("only MP4 AV1 exports".into());
    }
    if s.signal.is_hdr() {
        return Err("HDR (10-bit)".into());
    }
    if s.bitrate_mode == BitrateMode::Vbr2Pass {
        return Err("two-pass VBR".into());
    }
    if s.field_order != FieldOrder::Progressive {
        return Err("interlaced output".into());
    }
    if w < 64 || h < 16 || w > 16384 || h > 16384 {
        return Err(format!("{w}x{h}"));
    }
    if !w.is_multiple_of(64) || !h.is_multiple_of(16) {
        return Err(format!(
            "{w}x{h}: this GPU's driver codes such sizes taller or wider than asked (choose a width that is a multiple of 64 and a height that is a multiple of 16, or export H.265)"
        ));
    }
    let (Ok(num), Ok(den)) = (u32::try_from(rate.num), u32::try_from(rate.den)) else { return Err("frame rate".into()) };
    if num == 0 || den == 0 || num > 0xffff || den > 0xffff {
        return Err("frame rate".into());
    }
    let kbps = s.bitrate_kbps.clamp(100, 4_000_000);
    let keyint = s.keyframe_distance.filter(|k| *k > 0).unwrap_or_else(|| (f64::from(num) / f64::from(den) * 2.0).round().max(1.0) as u32).min(10_000);
    let max_kbps = s.max_bitrate_kbps.filter(|m| *m >= kbps).unwrap_or(kbps.saturating_mul(3) / 2);
    Ok(Config {
        width: w,
        height: h,
        fps: (num, den),
        kbps,
        max_kbps,
        cbr: s.bitrate_mode == BitrateMode::Cbr,
        keyint,
        level: pick_level(w, h, f64::from(num) / f64::from(den), max_kbps),
    })
}

/// tile_log2( blkSize, target ) (5.9.15).
fn tile_log2(blk: u32, target: u32) -> u32 {
    let mut k = 0;
    while blk.checked_shl(k).is_some_and(|v| v < target) && k < 31 {
        k += 1;
    }
    k
}

/// The uniform tile layout of a frame (5.9.15) with the fewest tiles allowed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tiles {
    cols_log2: u32,
    rows_log2: u32,
    /// (min, max) log2 of the tile columns / rows the syntax allows.
    cols_range: (u32, u32),
    rows_range: (u32, u32),
    /// Tile widths / heights in superblocks.
    widths: Vec<u32>,
    heights: Vec<u32>,
}

fn tiles(w: u32, h: u32) -> Tiles {
    let (mi_cols, mi_rows) = (2 * w.div_ceil(8), 2 * h.div_ceil(8));
    let (sb_cols, sb_rows) = (mi_cols.div_ceil(16), mi_rows.div_ceil(16));
    let max_tile_width_sb = 4096 >> 6;
    let max_tile_area_sb = (4096 * 2304) >> 12;
    let min_cols = tile_log2(max_tile_width_sb, sb_cols);
    let max_cols = tile_log2(1, sb_cols.min(64));
    let max_rows = tile_log2(1, sb_rows.min(64));
    let min_tiles = min_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));
    let cols_log2 = min_cols;
    let min_rows = min_tiles.saturating_sub(cols_log2);
    let rows_log2 = min_rows;
    let split = |sbs: u32, log2: u32| {
        let size = (sbs + (1 << log2) - 1) >> log2;
        let mut v = Vec::new();
        let mut start = 0;
        while start < sbs {
            v.push(size.min(sbs - start));
            start += size;
        }
        v
    };
    Tiles {
        cols_log2,
        rows_log2,
        cols_range: (min_cols, max_cols),
        rows_range: (min_rows, max_rows),
        widths: split(sb_cols, cols_log2),
        heights: split(sb_rows, rows_log2),
    }
}

/// An OBU with a 4-byte `obu_size` (the driver rewrites it).
fn obu(kind: u8, payload: &[u8]) -> Vec<u8> {
    let n = payload.len() as u32;
    let mut out = vec![(kind << 3) | 2];
    out.extend_from_slice(&[(n & 0x7f) as u8 | 0x80, ((n >> 7) & 0x7f) as u8 | 0x80, ((n >> 14) & 0x7f) as u8 | 0x80, ((n >> 21) & 0x7f) as u8]);
    out.extend_from_slice(payload);
    out
}

/// sequence_header_obu( ) (5.5) for [`Config`] and the tools of the module documentation. The
/// driver needs a packed one but writes its own; ours is never in the file.
fn sequence_header(cfg: &Config) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(0, 3); // seq_profile: Main
    w.write_bit(false); // still_picture
    w.write_bit(false); // reduced_still_picture_header
    w.write_bit(false); // timing_info_present_flag
    w.write_bit(false); // initial_display_delay_present_flag
    w.write_bits(0, 5); // operating_points_cnt_minus_1
    w.write_bits(0, 12); // operating_point_idc[0]
    w.write_bits(u32::from(cfg.level), 5); // seq_level_idx[0]
    if cfg.level > 7 {
        w.write_bit(false); // seq_tier[0]: Main
    }
    w.write_bits(15, 4); // frame_width_bits_minus_1
    w.write_bits(15, 4); // frame_height_bits_minus_1
    w.write_bits(cfg.width - 1, 16); // max_frame_width_minus_1
    w.write_bits(cfg.height - 1, 16); // max_frame_height_minus_1
    w.write_bit(false); // frame_id_numbers_present_flag
    w.write_bit(false); // use_128x128_superblock
    w.write_bit(false); // enable_filter_intra
    w.write_bit(false); // enable_intra_edge_filter
    w.write_bit(false); // enable_interintra_compound
    w.write_bit(false); // enable_masked_compound
    w.write_bit(false); // enable_warped_motion
    w.write_bit(false); // enable_dual_filter
    w.write_bit(true); // enable_order_hint
    w.write_bit(false); // enable_jnt_comp
    w.write_bit(false); // enable_ref_frame_mvs
    w.write_bit(false); // seq_choose_screen_content_tools
    w.write_bit(false); // seq_force_screen_content_tools
    w.write_bits(ORDER_HINT_BITS - 1, 3); // order_hint_bits_minus_1
    w.write_bit(false); // enable_superres
    w.write_bit(true); // enable_cdef
    w.write_bit(false); // enable_restoration
    let color = filmcraft_h264enc::ColorConfig::default();
    w.write_bit(false); // high_bitdepth
    w.write_bit(false); // mono_chrome
    w.write_bit(true); // color_description_present_flag
    w.write_bits(u32::from(color.primaries), 8);
    w.write_bits(u32::from(color.transfer), 8);
    w.write_bits(u32::from(color.matrix), 8);
    w.write_bit(color.full_range); // color_range
    w.write_bits(0, 2); // chroma_sample_position
    w.write_bit(false); // separate_uv_delta_q
    w.write_bit(false); // film_grain_params_present
    w.rbsp_trailing(); // trailing_bits( )
    obu(OBU_SEQUENCE_HEADER, &w.finish())
}

/// A packed frame header OBU and the positions the driver patches.
struct FrameHeader {
    data: Vec<u8>,
    bit_offset_qindex: u32,
    bit_offset_loopfilter: u32,
    bit_offset_cdef: u32,
    cdef_bits: u32,
    /// Up to and including the trailing one bit.
    bits: u32,
}

/// frame_header_obu( ) (5.9) of a shown key frame or an inter frame predicting from the previous
/// frame (in every reference slot, ref_frame_idx all 0), for the driver's sequence header:
/// order hints of 8 bits, no frame ids, screen content tools, superres or loop restoration, CDEF
/// on, no film grain.
fn frame_header(cfg: &Config, key: bool, order_hint: u32) -> FrameHeader {
    let mut w = BitWriter::new();
    // the OBU header byte and the 4-byte obu_size come first: offsets count them
    const HEAD_BITS: u32 = 5 * 8;
    w.write_bit(false); // show_existing_frame
    w.write_bits(if key { 0 } else { 1 }, 2); // frame_type: KEY / INTER
    w.write_bit(true); // show_frame
    if !key {
        w.write_bit(false); // error_resilient_mode (a shown key frame has it inferred 1)
    }
    w.write_bit(false); // disable_cdf_update
    w.write_bit(false); // frame_size_override_flag
    w.write_bits(order_hint & ((1 << ORDER_HINT_BITS) - 1), ORDER_HINT_BITS);
    if !key {
        w.write_bits(u32::from(PRIMARY_REF_NONE), 3); // primary_ref_frame
        w.write_bits(0xff, 8); // refresh_frame_flags: every slot holds the newest frame
        w.write_bit(false); // frame_refs_short_signaling
        for _ in 0..7 {
            w.write_bits(0, 3); // ref_frame_idx[ i ]
        }
    }
    w.write_bit(false); // render_and_frame_size_different
    if !key {
        w.write_bit(true); // allow_high_precision_mv
        w.write_bit(false); // is_filter_switchable
        w.write_bits(0, 2); // interpolation_filter: EIGHTTAP
        w.write_bit(false); // is_motion_mode_switchable
    }
    w.write_bit(false); // disable_frame_end_update_cdf
    // tile_info( )
    let t = tiles(cfg.width, cfg.height);
    w.write_bit(true); // uniform_tile_spacing_flag
    if t.cols_log2 < t.cols_range.1 {
        w.write_bit(false); // increment_tile_cols_log2
    }
    if t.rows_log2 < t.rows_range.1 {
        w.write_bit(false); // increment_tile_rows_log2
    }
    if t.cols_log2 > 0 || t.rows_log2 > 0 {
        w.write_bits(0, t.cols_log2 + t.rows_log2); // context_update_tile_id
        w.write_bits(3, 2); // tile_size_bytes_minus_1: the driver writes 4-byte sizes
    }
    // quantization_params( )
    let bit_offset_qindex = HEAD_BITS + w.bit_len() as u32;
    w.write_bits(u32::from(DEFAULT_QINDEX), 8); // base_q_idx
    w.write_bit(false); // DeltaQYDc
    w.write_bit(false); // DeltaQUDc
    w.write_bit(false); // DeltaQUAc
    w.write_bit(false); // using_qmatrix
    w.write_bit(false); // segmentation_enabled
    w.write_bit(false); // delta_q_present (base_q_idx > 0)
    // loop_filter_params( )
    let bit_offset_loopfilter = HEAD_BITS + w.bit_len() as u32;
    for _ in 0..4 {
        w.write_bits(u32::from(DEFAULT_FILTER_LEVEL), 6); // loop_filter_level[ 0..3 ]
    }
    w.write_bits(0, 3); // loop_filter_sharpness
    w.write_bit(false); // loop_filter_delta_enabled
    // cdef_params( )
    let bit_offset_cdef = HEAD_BITS + w.bit_len() as u32;
    w.write_bits(0, 2); // cdef_damping_minus_3
    w.write_bits(0, 2); // cdef_bits
    w.write_bits(u32::from(DEFAULT_CDEF_PRI), 4); // cdef_y_pri_strength[ 0 ]
    w.write_bits(u32::from(DEFAULT_CDEF_SEC), 2); // cdef_y_sec_strength[ 0 ]
    w.write_bits(u32::from(DEFAULT_CDEF_PRI), 4); // cdef_uv_pri_strength[ 0 ]
    w.write_bits(u32::from(DEFAULT_CDEF_SEC), 2); // cdef_uv_sec_strength[ 0 ]
    let cdef_bits = HEAD_BITS + w.bit_len() as u32 - bit_offset_cdef;
    w.write_bit(true); // tx_mode_select
    if !key {
        w.write_bit(false); // reference_select
    }
    w.write_bit(false); // reduced_tx_set
    if !key {
        for _ in 0..7 {
            w.write_bit(false); // is_global[ LAST_FRAME..ALTREF_FRAME ]
        }
    }
    w.write_bit(true); // trailing one bit
    let bits = HEAD_BITS + w.bit_len() as u32;
    w.align_zero();
    FrameHeader { data: obu(OBU_FRAME_HEADER, &w.finish()), bit_offset_qindex, bit_offset_loopfilter, bit_offset_cdef, cdef_bits, bits }
}

struct VaAv1Encoder {
    session: EncodeSession,
    cfg: Config,
    /// The sequence header handed to the driver (which needs one).
    packed_seq: Vec<u8>,
    /// The driver's sequence header OBU (from the first key frame).
    seq: Vec<u8>,
    gop_index: u32,
    order_hint: u32,
    /// Reconstructed slot holding the previous frame.
    prev: Option<usize>,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl VaAv1Encoder {
    fn new(cfg: Config) -> std::result::Result<Self, String> {
        let rc = if cfg.cbr { VA_RC_CBR } else { VA_RC_VBR };
        let display = Display::open_for_encode(VAProfileAV1Profile0, rc)?;
        if let Ok(Some((mw, mh))) = display.max_encode_size(VAProfileAV1Profile0)
            && (cfg.width > mw || cfg.height > mh)
        {
            return Err(format!("{}x{} (this GPU encodes up to {mw}x{mh})", cfg.width, cfg.height));
        }
        let packed = VA_ENC_PACKED_HEADER_SEQUENCE | VA_ENC_PACKED_HEADER_PICTURE;
        let session = EncodeSession::new(display, VAProfileAV1Profile0, rc, packed, cfg.width, cfg.height, 2)?;
        let packed_seq = sequence_header(&cfg);
        Ok(VaAv1Encoder { session, cfg, packed_seq, seq: Vec::new(), gop_index: 0, order_hint: 0, prev: None, y: Vec::new(), u: Vec::new(), v: Vec::new() })
    }

    fn encode_frame(&mut self) -> std::result::Result<(Vec<u8>, bool), String> {
        let key = self.gop_index == 0 || self.prev.is_none();
        let slot = self.prev.map_or(0, |p| (p + 1) % self.session.recon_count().max(1));
        let s = &self.session;
        let cur = s.recon(slot).ok_or("no reconstructed surface")?;
        let cfg = &self.cfg;
        let (num, den) = cfg.fps;
        let seq = VAEncSequenceParameterBufferAV1 {
            seq_profile: 0,
            seq_level_idx: cfg.level,
            seq_tier: 0,
            hierarchical_flag: 0,
            intra_period: cfg.keyint,
            ip_period: 1,
            bits_per_second: cfg.kbps.saturating_mul(1000),
            // enable_order_hint, enable_cdef, 4:2:0
            seq_fields: pack_bits(&[(0, 8), (1, 1), (0, 3), (1, 1), (0, 1), (0, 3), (1, 1), (1, 1), (0, 1)]),
            order_hint_bits_minus_1: (ORDER_HINT_BITS - 1) as u8,
            va_reserved: [0; 16],
        };
        let mut reference_frames = [VA_INVALID_SURFACE; 8];
        if !key {
            let prev = s.recon(self.prev.ok_or("no reference")?).ok_or("no reference surface")?;
            reference_frames = [prev; 8];
        }
        let t = tiles(cfg.width, cfg.height);
        let mut width_in_sbs_minus_1 = [0u16; 63];
        let mut height_in_sbs_minus_1 = [0u16; 63];
        for (d, v) in width_in_sbs_minus_1.iter_mut().zip(&t.widths) {
            *d = v.saturating_sub(1) as u16;
        }
        for (d, v) in height_in_sbs_minus_1.iter_mut().zip(&t.heights) {
            *d = v.saturating_sub(1) as u16;
        }
        let fh = frame_header(cfg, key, self.order_hint);
        let mut cdef_y_strengths = [0u8; 8];
        let mut cdef_uv_strengths = [0u8; 8];
        cdef_y_strengths[0] = DEFAULT_CDEF_STRENGTH;
        cdef_uv_strengths[0] = DEFAULT_CDEF_STRENGTH;
        let pic = VAEncPictureParameterBufferAV1 {
            frame_width_minus_1: u16::try_from(cfg.width - 1).map_err(|_| "too wide")?,
            frame_height_minus_1: u16::try_from(cfg.height - 1).map_err(|_| "too tall")?,
            reconstructed_frame: cur,
            coded_buf: s.coded_buffer(),
            reference_frames,
            ref_frame_idx: [0; 7],
            hierarchical_level_plus1: 0,
            primary_ref_frame: PRIMARY_REF_NONE,
            order_hint: (self.order_hint & 0xff) as u8,
            refresh_frame_flags: 0xff,
            reserved8bits1: 0,
            ref_frame_ctrl_l0: if key { 0 } else { 1 }, // search LAST_FRAME only
            ref_frame_ctrl_l1: 0,
            // frame_type, error_resilient_mode (inferred 1 for a shown key frame), …,
            // allow_high_precision_mv
            picture_flags: pack_bits(&[(if key { 0 } else { 1 }, 2), (u32::from(key), 1), (0, 1), (0, 1), (u32::from(!key), 1)]),
            seg_id_block_size: 0,
            num_tile_groups_minus1: 0,
            temporal_id: 0,
            filter_level: [DEFAULT_FILTER_LEVEL; 2],
            filter_level_u: DEFAULT_FILTER_LEVEL,
            filter_level_v: DEFAULT_FILTER_LEVEL,
            loop_filter_flags: 0,
            superres_scale_denominator: 8,
            interpolation_filter: 0,
            ref_deltas: [1, 0, 0, 0, -1, 0, -1, -1],
            mode_deltas: [0, 0],
            base_qindex: DEFAULT_QINDEX,
            y_dc_delta_q: 0,
            u_dc_delta_q: 0,
            u_ac_delta_q: 0,
            v_dc_delta_q: 0,
            v_ac_delta_q: 0,
            min_base_qindex: 1,
            max_base_qindex: 255,
            qmatrix_flags: 0,
            reserved16bits1: 0,
            // tx_mode: TX_MODE_SELECT
            mode_control_flags: pack_bits(&[(0, 1), (0, 2), (0, 1), (0, 2), (0, 1), (2, 2)]),
            segments: VAEncSegParamAV1 { seg_flags: 0, segment_number: 0, feature_data: [[0; 8]; 8], feature_mask: [0; 8], va_reserved: [0; 4] },
            tile_cols: u8::try_from(t.widths.len()).map_err(|_| "tile columns")?,
            tile_rows: u8::try_from(t.heights.len()).map_err(|_| "tile rows")?,
            reserved16bits2: 0,
            width_in_sbs_minus_1,
            height_in_sbs_minus_1,
            context_update_tile_id: 0,
            cdef_damping_minus_3: 0,
            cdef_bits: 0,
            cdef_y_strengths,
            cdef_uv_strengths,
            loop_restoration_flags: 0,
            wm: [VAEncWarpedMotionParamsAV1 { wmtype: 0, wmmat: [0, 0, 1 << 16, 0, 0, 1 << 16, 0, 0], invalid: 0, va_reserved: [0; 4] }; 7],
            bit_offset_qindex: fh.bit_offset_qindex,
            bit_offset_segmentation: 0,
            bit_offset_loopfilter_params: fh.bit_offset_loopfilter,
            bit_offset_cdef_params: fh.bit_offset_cdef,
            size_in_bits_cdef_params: fh.cdef_bits,
            byte_offset_frame_hdr_obu_size: 1,
            size_in_bits_frame_hdr_obu: fh.bits,
            tile_group_obu_hdr_info: 2, // obu_has_size_field
            number_skip_frames: 0,
            reserved16bits3: 0,
            skip_frames_reduced_size: 0,
            va_reserved: [0; 16],
        };
        let tg = VAEncTileGroupBufferAV1 { tg_start: 0, tg_end: u8::try_from(t.widths.len() * t.heights.len() - 1).map_err(|_| "tiles")?, va_reserved: [0; 4] };
        let bps = cfg.kbps.saturating_mul(1000);
        let rc = VAEncMisc {
            type_: VAEncMiscParameterTypeRateControl,
            data: VAEncMiscParameterRateControl {
                bits_per_second: if cfg.cbr { bps } else { cfg.max_kbps.saturating_mul(1000) },
                target_percentage: if cfg.cbr { 100 } else { (u64::from(cfg.kbps) * 100 / u64::from(cfg.max_kbps.max(1))).clamp(1, 100) as u32 },
                window_size: 1500,
                initial_qp: 0,
                min_qp: 0,
                max_qp: 255,
                ..Default::default()
            },
        };
        let fr = VAEncMisc { type_: VAEncMiscParameterTypeFrameRate, data: VAEncMiscParameterFrameRate { framerate: (den << 16) | num, ..Default::default() } };
        let hrd = VAEncMisc {
            type_: VAEncMiscParameterTypeHRD,
            data: VAEncMiscParameterHRD {
                buffer_size: cfg.max_kbps.saturating_mul(1000).saturating_mul(2),
                initial_buffer_fullness: cfg.max_kbps.saturating_mul(1000),
                ..Default::default()
            },
        };
        let mut bufs = Vec::with_capacity(10);
        if key {
            bufs.push(s.params(VAEncSequenceParameterBufferType, &seq)?);
            bufs.push(s.params(VAEncMiscParameterBufferType, &rc)?);
            bufs.push(s.params(VAEncMiscParameterBufferType, &fr)?);
            bufs.push(s.params(VAEncMiscParameterBufferType, &hrd)?);
            bufs.extend(s.packed(VAEncPackedHeaderSequence, &self.packed_seq, (self.packed_seq.len() * 8) as u32)?);
        }
        bufs.push(s.params(VAEncPictureParameterBufferType, &pic)?);
        bufs.extend(s.packed(VAEncPackedHeaderPicture, &fh.data, fh.bits)?);
        bufs.push(s.params(VAEncSliceParameterBufferType, &tg)?);
        s.encode(&bufs)?;
        drop(bufs);
        let bytes = s.coded_bytes()?;
        self.prev = Some(slot);
        self.order_hint = self.order_hint.wrapping_add(1);
        self.gop_index += 1;
        if self.gop_index >= self.cfg.keyint {
            self.gop_index = 0;
        }
        Ok((bytes, key))
    }

    /// The OBUs of `data` (low overhead format): (type, whole OBU).
    fn obus(data: &[u8]) -> std::result::Result<Vec<(u8, &[u8])>, String> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while let Some(&h) = data.get(pos) {
            let (kind, ext, has_size) = ((h >> 3) & 0xf, h & 4 != 0, h & 2 != 0);
            let mut p = pos + 1 + usize::from(ext);
            if !has_size {
                return Err("an OBU without a size from the driver".into());
            }
            let (mut size, mut shift) = (0usize, 0u32);
            loop {
                let b = *data.get(p).ok_or("truncated OBU size")?;
                size |= usize::from(b & 0x7f) << shift;
                p += 1;
                if b & 0x80 == 0 {
                    break;
                }
                shift += 7;
                if shift > 28 {
                    return Err("OBU size too long".into());
                }
            }
            let end = p.checked_add(size).filter(|&e| e <= data.len()).ok_or("OBU beyond the coded data")?;
            out.push((kind, &data[pos..end]));
            pos = end;
        }
        Ok(out)
    }
}

impl VideoEncoder for VaAv1Encoder {
    fn sample_entry(&self) -> SampleEntry {
        let (w, h) = (u16::try_from(self.cfg.width).unwrap_or(u16::MAX), u16::try_from(self.cfg.height).unwrap_or(u16::MAX));
        let cfg = Av1Config {
            seq_profile: 0,
            seq_level_idx_0: self.cfg.level,
            seq_tier_0: false,
            high_bitdepth: false,
            twelve_bit: false,
            monochrome: false,
            chroma_subsampling_x: true,
            chroma_subsampling_y: true,
            chroma_sample_position: 0,
            initial_presentation_delay_minus_one: None,
            config_obus: self.seq.clone(),
        };
        SampleEntry::av1(cfg, w, h)
    }

    fn timescale(&self) -> u32 {
        self.cfg.fps.0
    }

    fn encode(&mut self, f: &EncoderFrame) -> Result<Vec<EncodedPacket>> {
        if f.width != self.cfg.width || f.height != self.cfg.height {
            return Err(ExportError::Encode(format!("VA-API: a {}x{} picture for a {}x{} encoder", f.width, f.height, self.cfg.width, self.cfg.height)));
        }
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        filmcraft_export::rgba_to_yuv420_8(f.rgba, w, h, &mut self.y, &mut self.u, &mut self.v);
        let fail = |e: String| ExportError::Encode(format!("VA-API AV1: {e}"));
        self.session.upload(&self.y, &self.u, &self.v, w, h).map_err(fail)?;
        let (bytes, key) = self.encode_frame().map_err(fail)?;
        let mut data = Vec::with_capacity(bytes.len());
        for (kind, o) in Self::obus(&bytes).map_err(fail)? {
            if kind == OBU_SEQUENCE_HEADER && self.seq.is_empty() {
                check_sequence_header(o, &self.cfg).map_err(fail)?;
                self.seq = o.to_vec();
            }
            if kind != OBU_TEMPORAL_DELIMITER {
                data.extend_from_slice(o);
            }
        }
        if self.seq.is_empty() {
            return Err(ExportError::Encode("VA-API AV1: the driver wrote no sequence header".into()));
        }
        if data.is_empty() {
            return Err(ExportError::Encode("VA-API AV1: the driver returned no frame data".into()));
        }
        filmcraft_export::note_hw_encode_frame();
        Ok(vec![EncodedPacket { data, key, duration: self.cfg.fps.1, composition_offset: 0 }])
    }

    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
}

/// The driver's sequence header must be the one our frame headers are written for (see
/// [`frame_header`]); anything else is an error, never a broken file.
fn check_sequence_header(obu: &[u8], cfg: &Config) -> std::result::Result<(), String> {
    // the OBU's header byte and its leb128 size come before the payload
    let start = 1 + obu.iter().skip(1).position(|b| b & 0x80 == 0).ok_or("truncated sequence header")? + 1;
    let h = filmcraft_av1::SequenceHeader::parse(obu.get(start..).ok_or("truncated sequence header")?).map_err(|e| format!("driver sequence header: {e}"))?;
    let expected = h.profile == 0
        && !h.reduced_still_picture_header
        && !h.frame_id_numbers_present
        && h.enable_order_hint
        && h.order_hint_bits == ORDER_HINT_BITS
        && h.seq_force_screen_content_tools == 0
        && !h.enable_superres
        && h.enable_cdef
        && !h.enable_restoration
        && !h.film_grain_params_present
        && !h.use_128x128_superblock
        && h.color.bit_depth == 8
        && !h.color.mono_chrome
        && (h.max_frame_width, h.max_frame_height) == (cfg.width, cfg.height);
    if expected { Ok(()) } else { Err(format!("unexpected sequence header from the driver: {h:?}")) }
}

/// Whether this system can export AV1 (the format probe; asked once).
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| Display::open_for_encode(VAProfileAV1Profile0, VA_RC_VBR).is_ok())
}

/// The Export encoder factory (see the module documentation).
pub fn factory(format: Format, w: u32, h: u32, rate: FrameRate, s: &ExportSettings) -> Option<Result<Box<dyn VideoEncoder>>> {
    if format != Format::Av1 {
        return None;
    }
    let cfg = match config(format, w, h, rate, s) {
        Ok(c) => c,
        Err(why) => return Some(Err(ExportError::Encode(format!("AV1 on this GPU: {why}")))),
    };
    match VaAv1Encoder::new(cfg) {
        Ok(enc) => {
            filmcraft_export::note_hw_encode_session();
            Some(Ok(Box::new(enc)))
        }
        Err(why) => Some(Err(ExportError::Encode(format!("AV1 on this GPU: {why}")))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert_eq!(pick_level(1920, 1080, 30.0, 8_000), 8);
        assert_eq!(pick_level(1920, 1080, 60.0, 8_000), 9);
        assert_eq!(pick_level(3840, 2160, 30.0, 20_000), 12);
        assert_eq!(pick_level(3840, 2160, 60.0, 35_000), 13);
    }

    #[test]
    fn tile_layouts() {
        let t = tiles(1920, 1080);
        assert_eq!((t.cols_log2, t.rows_log2, t.widths.len(), t.heights.len()), (0, 0, 1, 1));
        assert_eq!((t.widths[0], t.heights[0]), (30, 17));
        // 8K needs more than one tile column (64 superblocks wide at most)
        let t = tiles(7680, 4320);
        assert!(t.widths.len() >= 2 && t.widths.iter().all(|w| *w <= 64), "{t:?}");
        assert_eq!(t.widths.iter().sum::<u32>(), 120);
    }
}
