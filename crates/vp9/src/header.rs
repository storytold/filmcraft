//! Uncompressed header (6.2) and compressed header (6.3) parsing, plus the state that persists
//! between frames (loop filter deltas, segmentation parameters, colour configuration).

use crate::boolcoder::BoolDecoder;
use crate::error::{Error, Result, ensure};
use crate::probs::FrameContext;
use crate::tables::{
    ALTREF_FRAME, GOLDEN_FRAME, INV_MAP_TABLE, LAST_FRAME, LITERAL_TO_TYPE, MAX_SEGMENTS, MAX_TILE_WIDTH_B64, MIN_TILE_WIDTH_B64, SEGMENTATION_FEATURE_BITS,
    SEGMENTATION_FEATURE_SIGNED, SWITCHABLE, TX_MODE_SELECT, TX_MODE_TO_BIGGEST_TX_SIZE,
};
use filmcraft_bitstream::BitReader;

pub const KEY_FRAME: u8 = 0;

/// Loop filter parameters (6.2.8); the deltas persist across frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoopFilterParams {
    pub level: u8,
    pub sharpness: u8,
    pub delta_enabled: bool,
    pub ref_deltas: [i8; 4],
    pub mode_deltas: [i8; 2],
}

/// Segmentation parameters (6.2.11); feature data persists across frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Segmentation {
    pub enabled: bool,
    pub update_map: bool,
    pub tree_probs: [u8; 7],
    pub pred_probs: [u8; 3],
    pub temporal_update: bool,
    pub abs_or_delta_update: bool,
    pub feature_enabled: [[bool; 4]; 8],
    pub feature_data: [[i16; 4]; 8],
}

impl Segmentation {
    #[inline]
    pub fn feature_active(&self, segment_id: u8, feature: usize) -> bool {
        self.enabled && self.feature_enabled[segment_id as usize][feature]
    }
}

/// Colour configuration (6.2.2); persists for inter frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorConfig {
    pub bit_depth: u8,
    pub color_space: u8,
    pub color_range: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
}

impl Default for ColorConfig {
    fn default() -> Self {
        ColorConfig { bit_depth: 8, color_space: 0, color_range: false, subsampling_x: true, subsampling_y: true }
    }
}

/// Everything read from the headers of one frame.
#[derive(Clone, Debug, Default)]
pub struct FrameHeader {
    pub profile: u8,
    pub show_existing_frame: bool,
    pub frame_to_show_map_idx: u8,
    pub frame_type: u8,
    pub show_frame: bool,
    pub error_resilient_mode: bool,
    pub intra_only: bool,
    pub reset_frame_context: u8,
    pub color: ColorConfig,
    pub refresh_frame_flags: u8,
    pub ref_frame_idx: [u8; 3],
    /// Indexed by reference frame (LAST_FRAME..ALTREF_FRAME); index 0 unused.
    pub ref_frame_sign_bias: [bool; 4],
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub allow_high_precision_mv: bool,
    pub interp_filter: u8,
    pub refresh_frame_context: bool,
    pub frame_parallel_decoding_mode: bool,
    pub frame_context_idx: u8,
    pub base_q_idx: u8,
    pub delta_q_y_dc: i8,
    pub delta_q_uv_dc: i8,
    pub delta_q_uv_ac: i8,
    pub lossless: bool,
    pub tile_cols_log2: u32,
    pub tile_rows_log2: u32,
    pub header_size_in_bytes: u32,
    /// Size of the uncompressed header in bytes.
    pub uncompressed_size: usize,
    // Compressed header.
    pub tx_mode: u8,
    pub reference_mode: u8,
    pub comp_fixed_ref: i8,
    pub comp_var_ref: [i8; 2],
    // Derived.
    pub mi_cols: u32,
    pub mi_rows: u32,
    pub sb64_cols: u32,
    pub sb64_rows: u32,
    pub frame_is_intra: bool,
    /// The frame size was taken from reference `found_ref` (for statistics).
    pub size_from_ref: bool,
}

/// Reference slot information needed while parsing (frame_size_with_refs).
#[derive(Clone, Copy, Debug)]
pub struct RefInfo {
    pub width: u32,
    pub height: u32,
}

/// State carried between frames that header parsing reads and updates.
pub struct HeaderState {
    pub contexts: [FrameContext; 4],
    pub lf: LoopFilterParams,
    pub seg: Segmentation,
    pub color: ColorConfig,
    pub last_frame_type: u8,
    pub frame_type: u8,
    /// Set by setup_past_independence; cleared once consumed by the decoder (PrevSegmentIds reset).
    pub reset_segment_map: bool,
}

impl Default for HeaderState {
    fn default() -> Self {
        let fc = FrameContext::default();
        HeaderState {
            contexts: [fc.clone(), fc.clone(), fc.clone(), fc],
            lf: LoopFilterParams { ref_deltas: [1, 0, -1, -1], delta_enabled: true, ..Default::default() },
            seg: Segmentation::default(),
            color: ColorConfig::default(),
            last_frame_type: KEY_FRAME,
            frame_type: KEY_FRAME,
            reset_segment_map: false,
        }
    }
}

fn read_s(r: &mut BitReader, bits: u32) -> Result<i32> {
    let v = r.read_bits(bits)? as i32;
    Ok(if r.read_flag()? { -v } else { v })
}

fn read_delta_q(r: &mut BitReader) -> Result<i8> {
    Ok(if r.read_flag()? { read_s(r, 4)? as i8 } else { 0 })
}

fn read_prob(r: &mut BitReader) -> Result<u8> {
    Ok(if r.read_flag()? { r.read_bits(8)? as u8 } else { 255 })
}

fn frame_sync_code(r: &mut BitReader) -> Result<()> {
    let (a, b, c) = (r.read_bits(8)?, r.read_bits(8)?, r.read_bits(8)?);
    ensure!(a == 0x49 && b == 0x83 && c == 0x42, "invalid frame sync code");
    Ok(())
}

fn color_config(r: &mut BitReader, profile: u8) -> Result<ColorConfig> {
    let bit_depth = if profile >= 2 { if r.read_flag()? { 12 } else { 10 } } else { 8 };
    let color_space = r.read_bits(3)? as u8;
    let mut c = ColorConfig { bit_depth, color_space, color_range: true, subsampling_x: true, subsampling_y: true };
    if color_space != 7 {
        c.color_range = r.read_flag()?;
        if profile == 1 || profile == 3 {
            c.subsampling_x = r.read_flag()?;
            c.subsampling_y = r.read_flag()?;
            ensure!(!(c.subsampling_x && c.subsampling_y), "4:2:0 subsampling in profile {profile}");
            ensure!(!r.read_flag()?, "reserved bit set in color config");
        }
    } else {
        ensure!(profile == 1 || profile == 3, "RGB color space in profile {profile}");
        c.subsampling_x = false;
        c.subsampling_y = false;
        ensure!(!r.read_flag()?, "reserved bit set in color config");
    }
    Ok(c)
}

fn frame_size(r: &mut BitReader, h: &mut FrameHeader) -> Result<()> {
    h.width = r.read_bits(16)? + 1;
    h.height = r.read_bits(16)? + 1;
    Ok(())
}

fn render_size(r: &mut BitReader, h: &mut FrameHeader) -> Result<()> {
    if r.read_flag()? {
        h.render_width = r.read_bits(16)? + 1;
        h.render_height = r.read_bits(16)? + 1;
    } else {
        h.render_width = h.width;
        h.render_height = h.height;
    }
    Ok(())
}

fn compute_image_size(h: &mut FrameHeader) {
    h.mi_cols = (h.width + 7) >> 3;
    h.mi_rows = (h.height + 7) >> 3;
    h.sb64_cols = (h.mi_cols + 7) >> 3;
    h.sb64_rows = (h.mi_rows + 7) >> 3;
}

/// setup_past_independence (7.2): resets segmentation features, loop filter deltas and the
/// probabilities of `fc`.
fn setup_past_independence(st: &mut HeaderState, fc: &mut FrameContext) {
    st.seg.feature_data = [[0; 4]; 8];
    st.seg.feature_enabled = [[false; 4]; 8];
    st.seg.abs_or_delta_update = false;
    st.lf.delta_enabled = true;
    st.lf.ref_deltas = [1, 0, -1, -1];
    st.lf.mode_deltas = [0, 0];
    st.reset_segment_map = true;
    *fc = FrameContext::default();
}

/// Intra-only (non-key) frame header tail (6.2): sync code, colour config, refresh flags, sizes.
fn intra_only_header(r: &mut BitReader, h: &mut FrameHeader) -> Result<()> {
    frame_sync_code(r)?;
    h.color = if h.profile > 0 {
        color_config(r, h.profile)?
    } else {
        ColorConfig { bit_depth: 8, color_space: 1, color_range: false, subsampling_x: true, subsampling_y: true }
    };
    h.refresh_frame_flags = r.read_bits(8)? as u8;
    frame_size(r, h)?;
    render_size(r, h)
}

/// frame_size_with_refs (6.2.5): the size is copied from the first flagged reference, or read explicitly.
fn frame_size_with_refs(r: &mut BitReader, h: &mut FrameHeader, refs: &[Option<RefInfo>; 8]) -> Result<()> {
    let mut found = false;
    for i in 0..3 {
        if r.read_flag()? {
            let idx = h.ref_frame_idx[i] as usize;
            let Some(info) = refs[idx] else {
                return Err(Error::MissingReference(format!("frame size from empty slot {idx}")));
            };
            h.width = info.width;
            h.height = info.height;
            found = true;
            break;
        }
    }
    if !found {
        frame_size(r, h)?;
    }
    h.size_from_ref = found;
    Ok(())
}

/// Inter frame header tail (6.2): reference selection, sizes, motion vector precision and filter.
fn inter_frame_header(r: &mut BitReader, h: &mut FrameHeader, st: &HeaderState, refs: &[Option<RefInfo>; 8]) -> Result<()> {
    h.color = st.color;
    h.refresh_frame_flags = r.read_bits(8)? as u8;
    for i in 0..3 {
        h.ref_frame_idx[i] = r.read_bits(3)? as u8;
        h.ref_frame_sign_bias[LAST_FRAME as usize + i] = r.read_flag()?;
    }
    frame_size_with_refs(r, h, refs)?;
    render_size(r, h)?;
    h.allow_high_precision_mv = r.read_flag()?;
    // read_interpolation_filter (6.2.7)
    h.interp_filter = if r.read_flag()? { SWITCHABLE } else { LITERAL_TO_TYPE[r.read_bits(2)? as usize] };
    Ok(())
}

/// Resets the probability contexts for intra / error resilient frames (7.2).
fn reset_frame_contexts(st: &mut HeaderState, h: &mut FrameHeader) {
    let mut fc = FrameContext::default();
    setup_past_independence(st, &mut fc);
    if h.frame_type == KEY_FRAME || h.error_resilient_mode || h.reset_frame_context == 3 {
        for c in st.contexts.iter_mut() {
            *c = fc.clone();
        }
    } else if h.reset_frame_context == 2 {
        st.contexts[h.frame_context_idx as usize] = fc.clone();
    }
    h.frame_context_idx = 0;
}

/// loop_filter_params (6.2.8).
fn read_loop_filter_params(r: &mut BitReader, lf: &mut LoopFilterParams) -> Result<()> {
    lf.level = r.read_bits(6)? as u8;
    lf.sharpness = r.read_bits(3)? as u8;
    lf.delta_enabled = r.read_flag()?;
    if lf.delta_enabled && r.read_flag()? {
        for i in 0..4 {
            if r.read_flag()? {
                lf.ref_deltas[i] = read_s(r, 6)? as i8;
            }
        }
        for i in 0..2 {
            if r.read_flag()? {
                lf.mode_deltas[i] = read_s(r, 6)? as i8;
            }
        }
    }
    Ok(())
}

/// segmentation_params (6.2.11).
fn read_segmentation_params(r: &mut BitReader, seg: &mut Segmentation) -> Result<()> {
    seg.enabled = r.read_flag()?;
    seg.update_map = false;
    seg.temporal_update = false;
    if !seg.enabled {
        return Ok(());
    }
    seg.update_map = r.read_flag()?;
    if seg.update_map {
        for i in 0..7 {
            seg.tree_probs[i] = read_prob(r)?;
        }
        seg.temporal_update = r.read_flag()?;
        for i in 0..3 {
            seg.pred_probs[i] = if seg.temporal_update { read_prob(r)? } else { 255 };
        }
    }
    if r.read_flag()? {
        seg.abs_or_delta_update = r.read_flag()?;
        for i in 0..MAX_SEGMENTS {
            for j in 0..4 {
                let mut value = 0i32;
                let enabled = r.read_flag()?;
                seg.feature_enabled[i][j] = enabled;
                if enabled {
                    value = r.read_bits(SEGMENTATION_FEATURE_BITS[j])? as i32;
                    if SEGMENTATION_FEATURE_SIGNED[j] && r.read_flag()? {
                        value = -value;
                    }
                }
                seg.feature_data[i][j] = value as i16;
            }
        }
    }
    Ok(())
}

/// tile_info (6.2.13).
fn read_tile_info(r: &mut BitReader, h: &mut FrameHeader) -> Result<()> {
    let mut min_log2 = 0;
    while (MAX_TILE_WIDTH_B64 << min_log2) < h.sb64_cols {
        min_log2 += 1;
    }
    let mut max_log2 = 1;
    while (h.sb64_cols >> max_log2) >= MIN_TILE_WIDTH_B64 {
        max_log2 += 1;
    }
    max_log2 -= 1;
    h.tile_cols_log2 = min_log2;
    while h.tile_cols_log2 < max_log2 {
        if r.read_flag()? {
            h.tile_cols_log2 += 1;
        } else {
            break;
        }
    }
    h.tile_rows_log2 = r.read_bits(1)?;
    if h.tile_rows_log2 == 1 {
        h.tile_rows_log2 += r.read_bits(1)?;
    }
    Ok(())
}

/// Parse the uncompressed header (6.2). `refs` gives the sizes of the eight reference slots.
/// Updates the persistent state (probability contexts on reset, loop filter deltas,
/// segmentation, colour configuration).
pub fn parse_uncompressed(data: &[u8], st: &mut HeaderState, refs: &[Option<RefInfo>; 8]) -> Result<FrameHeader> {
    let mut r = BitReader::new(data);
    let mut h = FrameHeader::default();
    ensure!(r.read_bits(2)? == 2, "invalid frame marker");
    let lo = r.read_bits(1)? as u8;
    let hi = r.read_bits(1)? as u8;
    h.profile = (hi << 1) | lo;
    if h.profile == 3 {
        ensure!(r.read_bits(1)? == 0, "reserved bit set after profile");
    }
    h.show_existing_frame = r.read_flag()?;
    if h.show_existing_frame {
        h.frame_to_show_map_idx = r.read_bits(3)? as u8;
        h.show_frame = true;
        h.uncompressed_size = r.position().div_ceil(8);
        return Ok(h);
    }
    st.last_frame_type = st.frame_type;
    h.frame_type = r.read_bits(1)? as u8;
    h.show_frame = r.read_flag()?;
    h.error_resilient_mode = r.read_flag()?;
    if h.frame_type == KEY_FRAME {
        frame_sync_code(&mut r)?;
        h.color = color_config(&mut r, h.profile)?;
        frame_size(&mut r, &mut h)?;
        render_size(&mut r, &mut h)?;
        h.refresh_frame_flags = 0xff;
        h.frame_is_intra = true;
    } else {
        h.intra_only = if h.show_frame { false } else { r.read_flag()? };
        h.frame_is_intra = h.intra_only;
        h.reset_frame_context = if h.error_resilient_mode { 0 } else { r.read_bits(2)? as u8 };
        if h.intra_only {
            intra_only_header(&mut r, &mut h)?;
        } else {
            inter_frame_header(&mut r, &mut h, st, refs)?;
        }
    }
    compute_image_size(&mut h);
    if !h.error_resilient_mode {
        h.refresh_frame_context = r.read_flag()?;
        h.frame_parallel_decoding_mode = r.read_flag()?;
    } else {
        h.refresh_frame_context = false;
        h.frame_parallel_decoding_mode = true;
    }
    h.frame_context_idx = r.read_bits(2)? as u8;
    if h.frame_is_intra || h.error_resilient_mode {
        reset_frame_contexts(st, &mut h);
    }
    read_loop_filter_params(&mut r, &mut st.lf)?;
    // quantization_params (6.2.9)
    h.base_q_idx = r.read_bits(8)? as u8;
    h.delta_q_y_dc = read_delta_q(&mut r)?;
    h.delta_q_uv_dc = read_delta_q(&mut r)?;
    h.delta_q_uv_ac = read_delta_q(&mut r)?;
    h.lossless = h.base_q_idx == 0 && h.delta_q_y_dc == 0 && h.delta_q_uv_dc == 0 && h.delta_q_uv_ac == 0;
    read_segmentation_params(&mut r, &mut st.seg)?;
    read_tile_info(&mut r, &mut h)?;
    h.header_size_in_bytes = r.read_bits(16)?;
    // trailing_bits
    h.uncompressed_size = r.position().div_ceil(8);
    if h.frame_is_intra {
        st.color = h.color;
    }
    st.frame_type = h.frame_type;
    ensure!(h.header_size_in_bytes > 0, "empty compressed header");
    Ok(h)
}

/// diff_update_prob (6.3.3).
fn diff_update_prob(bd: &mut BoolDecoder, prob: &mut u8) {
    if bd.read_bool(252) {
        let delta = decode_term_subexp(bd);
        *prob = inv_remap_prob(delta, *prob);
    }
}

fn decode_term_subexp(bd: &mut BoolDecoder) -> usize {
    if bd.read_literal(1) == 0 {
        return bd.read_literal(4) as usize;
    }
    if bd.read_literal(1) == 0 {
        return bd.read_literal(4) as usize + 16;
    }
    if bd.read_literal(1) == 0 {
        return bd.read_literal(5) as usize + 32;
    }
    let v = bd.read_literal(7) as usize;
    if v < 65 {
        return v + 64;
    }
    let bit = bd.read_literal(1) as usize;
    (v << 1) - 1 + bit
}

fn inv_recenter_nonneg(v: i32, m: i32) -> i32 {
    if v > 2 * m {
        v
    } else if v & 1 != 0 {
        m - ((v + 1) >> 1)
    } else {
        m + (v >> 1)
    }
}

fn inv_remap_prob(delta: usize, prob: u8) -> u8 {
    let v = INV_MAP_TABLE[delta.min(254)] as i32;
    let m = prob as i32 - 1;
    let r = if (m << 1) <= 255 { 1 + inv_recenter_nonneg(v, m) } else { 255 - inv_recenter_nonneg(v, 255 - 1 - m) };
    r.clamp(1, 255) as u8
}

/// update_mv_prob (6.3.17).
fn update_mv_prob(bd: &mut BoolDecoder, prob: &mut u8) {
    if bd.read_bool(252) {
        *prob = ((bd.read_literal(7) << 1) | 1) as u8;
    }
}

/// tx_mode_probs (6.3.2): per-size transform probability updates for `TX_MODE_SELECT`.
fn read_tx_mode_probs(bd: &mut BoolDecoder, fc: &mut FrameContext) {
    for i in 0..2 {
        diff_update_prob(bd, &mut fc.tx[1][i][0]);
    }
    for i in 0..2 {
        for j in 0..2 {
            diff_update_prob(bd, &mut fc.tx[2][i][j]);
        }
    }
    for i in 0..2 {
        for j in 0..3 {
            diff_update_prob(bd, &mut fc.tx[3][i][j]);
        }
    }
}

/// read_coef_probs (6.3.7).
fn read_coef_probs(bd: &mut BoolDecoder, h: &FrameHeader, fc: &mut FrameContext) {
    let max_tx = TX_MODE_TO_BIGGEST_TX_SIZE[h.tx_mode as usize] as usize;
    for tx in 0..=max_tx {
        if bd.read_literal(1) == 1 {
            for i in 0..2 {
                for j in 0..2 {
                    for k in 0..6 {
                        let max_l = if k == 0 { 3 } else { 6 };
                        for l in 0..max_l {
                            for m in 0..3 {
                                diff_update_prob(bd, &mut fc.coef[tx][i][j][k][l][m]);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// frame_reference_mode (6.3.12) including setup_compound_reference_mode (6.3.18).
fn read_frame_reference_mode(bd: &mut BoolDecoder, h: &mut FrameHeader) {
    let bias = h.ref_frame_sign_bias;
    let compound_allowed = bias[2] != bias[1] || bias[3] != bias[1];
    h.reference_mode = 0;
    if !compound_allowed {
        return;
    }
    if bd.read_literal(1) == 1 {
        h.reference_mode = if bd.read_literal(1) == 1 { 2 } else { 1 };
    }
    if bias[LAST_FRAME as usize] == bias[GOLDEN_FRAME as usize] {
        h.comp_fixed_ref = ALTREF_FRAME;
        h.comp_var_ref = [LAST_FRAME, GOLDEN_FRAME];
    } else if bias[LAST_FRAME as usize] == bias[ALTREF_FRAME as usize] {
        h.comp_fixed_ref = GOLDEN_FRAME;
        h.comp_var_ref = [LAST_FRAME, ALTREF_FRAME];
    } else {
        h.comp_fixed_ref = LAST_FRAME;
        h.comp_var_ref = [GOLDEN_FRAME, ALTREF_FRAME];
    }
}

/// frame_reference_mode_probs (6.3.13).
fn read_frame_reference_mode_probs(bd: &mut BoolDecoder, reference_mode: u8, fc: &mut FrameContext) {
    if reference_mode == 2 {
        for i in 0..5 {
            diff_update_prob(bd, &mut fc.comp_mode[i]);
        }
    }
    if reference_mode != 1 {
        for i in 0..5 {
            diff_update_prob(bd, &mut fc.single_ref[i][0]);
            diff_update_prob(bd, &mut fc.single_ref[i][1]);
        }
    }
    if reference_mode != 0 {
        for i in 0..5 {
            diff_update_prob(bd, &mut fc.comp_ref[i]);
        }
    }
}

/// mv_probs (6.3.16).
fn read_mv_probs(bd: &mut BoolDecoder, allow_high_precision_mv: bool, fc: &mut FrameContext) {
    for j in 0..3 {
        update_mv_prob(bd, &mut fc.mv_joint[j]);
    }
    for i in 0..2 {
        update_mv_prob(bd, &mut fc.mv_sign[i]);
        for j in 0..10 {
            update_mv_prob(bd, &mut fc.mv_class[i][j]);
        }
        update_mv_prob(bd, &mut fc.mv_class0_bit[i]);
        for j in 0..10 {
            update_mv_prob(bd, &mut fc.mv_bits[i][j]);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            for k in 0..3 {
                update_mv_prob(bd, &mut fc.mv_class0_fr[i][j][k]);
            }
        }
        for k in 0..3 {
            update_mv_prob(bd, &mut fc.mv_fr[i][k]);
        }
    }
    if allow_high_precision_mv {
        for i in 0..2 {
            update_mv_prob(bd, &mut fc.mv_class0_hp[i]);
            update_mv_prob(bd, &mut fc.mv_hp[i]);
        }
    }
}

/// Probability updates that only inter frames carry (6.3.8 to 6.3.16).
fn read_inter_frame_probs(bd: &mut BoolDecoder, h: &mut FrameHeader, fc: &mut FrameContext) {
    for i in 0..7 {
        for j in 0..3 {
            diff_update_prob(bd, &mut fc.inter_mode[i][j]);
        }
    }
    if h.interp_filter == SWITCHABLE {
        for j in 0..4 {
            for i in 0..2 {
                diff_update_prob(bd, &mut fc.interp_filter[j][i]);
            }
        }
    }
    for i in 0..4 {
        diff_update_prob(bd, &mut fc.is_inter[i]);
    }
    read_frame_reference_mode(bd, h);
    read_frame_reference_mode_probs(bd, h.reference_mode, fc);
    for i in 0..4 {
        for j in 0..9 {
            diff_update_prob(bd, &mut fc.y_mode[i][j]);
        }
    }
    for i in 0..16 {
        for j in 0..3 {
            diff_update_prob(bd, &mut fc.partition[i][j]);
        }
    }
    read_mv_probs(bd, h.allow_high_precision_mv, fc);
}

/// Parse the compressed header (6.3) into `h` and the probabilities `fc`.
pub fn parse_compressed(data: &[u8], h: &mut FrameHeader, fc: &mut FrameContext) -> Result<()> {
    let mut bd = BoolDecoder::new(data);
    // read_tx_mode (6.3.1)
    h.tx_mode = if h.lossless {
        0
    } else {
        let mut m = bd.read_literal(2) as u8;
        if m == 3 {
            m += bd.read_literal(1) as u8;
        }
        m
    };
    if h.tx_mode == TX_MODE_SELECT {
        read_tx_mode_probs(&mut bd, fc);
    }
    read_coef_probs(&mut bd, h, fc);
    for i in 0..3 {
        diff_update_prob(&mut bd, &mut fc.skip[i]);
    }
    if !h.frame_is_intra {
        read_inter_frame_probs(&mut bd, h, fc);
    }
    Ok(())
}

/// Superframe index parsing (Annex B): the frames of a chunk.
pub fn split_superframe(data: &[u8]) -> Vec<&[u8]> {
    if let Some(&last) = data.last()
        && last & 0xe0 == 0xc0
    {
        let sz_bytes = ((last >> 3) & 3) as usize + 1;
        let num = (last & 7) as usize + 1;
        let index_size = 2 + num * sz_bytes;
        if data.len() >= index_size && data[data.len() - index_size] == last {
            let mut frames = Vec::with_capacity(num);
            let mut pos = 0usize;
            let mut p = data.len() - index_size + 1;
            let end = data.len() - index_size;
            for _ in 0..num {
                let mut sz = 0usize;
                for b in 0..sz_bytes {
                    sz |= (data[p + b] as usize) << (8 * b);
                }
                p += sz_bytes;
                let Some(next) = pos.checked_add(sz).filter(|&next| next <= end) else {
                    frames.push(&data[pos..end]);
                    return frames;
                };
                frames.push(&data[pos..next]);
                pos = next;
            }
            return frames;
        }
    }
    vec![data]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_lengths_superframe_size_does_not_wrap() {
        // #2801: the second frame's size overflowed usize after the first byte on wasm32.
        let marker = 0xd9; // two frames, four bytes per size
        let data = [0x88, 0x89, marker, 1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, marker];
        let result = std::panic::catch_unwind(|| split_superframe(&data));
        // Preserve the existing truncated-superframe policy: return the remaining payload.
        assert_eq!(result.expect("superframe splitting must not panic"), vec![&data[..1], &data[1..2]]);
    }

    #[test]
    fn hostile_lengths_superframe_mutations_and_truncations() {
        for width in 1..=4 {
            let marker = 0xc1 | ((width - 1) << 3) as u8;
            let mut data = vec![0x88, 0x89, marker];
            data.extend_from_slice(&1u32.to_le_bytes()[..width]);
            data.extend_from_slice(&1u32.to_le_bytes()[..width]);
            data.push(marker);
            assert_eq!(split_superframe(&data), vec![&data[..1], &data[1..2]]);
            for end in 0..=data.len() {
                std::panic::catch_unwind(|| split_superframe(&data[..end])).expect("truncated superframe must not panic");
            }
            for index in 0..data.len() {
                for bit in 0..8 {
                    let mut mutated = data.clone();
                    mutated[index] ^= 1 << bit;
                    std::panic::catch_unwind(|| split_superframe(&mutated)).expect("mutated superframe must not panic");
                }
            }
        }
    }

    #[test]
    fn superframe_index() {
        // Two frames of 3 and 2 bytes, 1 byte per size.
        let mut d = vec![1u8, 2, 3, 4, 5];
        let marker = 0xc0 | 1; // 2 frames, 1 byte sizes
        d.extend([marker, 3, 2, marker]);
        let f = split_superframe(&d);
        assert_eq!(f, vec![&[1u8, 2, 3][..], &[4u8, 5][..]]);
        // Not a superframe: last byte is a marker but the first index byte does not match.
        let d2 = vec![9u8, 9, 0xc1];
        assert_eq!(split_superframe(&d2), vec![&d2[..]]);
    }

    #[test]
    fn inv_remap_is_bijective_per_prob() {
        for p in 1..=255u8 {
            let mut seen = std::collections::HashSet::new();
            for d in 0..254 {
                seen.insert(inv_remap_prob(d, p));
            }
            // Every other probability value is reachable.
            assert!(seen.len() >= 253, "p {p}: {}", seen.len());
        }
    }
}
