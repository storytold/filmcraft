//! Clean-room pure-Rust baseline AV1 software encoder.
//!
//! Encodes conformant AV1 streams according to the AOMedia AV1 Specification v1.0.0.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod coeff;
pub mod frame_header;
pub mod intra;
pub mod obu;
pub mod symbol;
pub mod tables;
pub mod transform;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid AV1 encoder configuration: {0}")]
    Invalid(&'static str),
    #[error("input frame buffer too small")]
    BufferTooSmall,
}
pub type Result<T> = std::result::Result<T, Error>;

/// Rate control mode for AV1 software encoder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RateControl {
    /// Constant Quality / CRF-like mode (crf 0.0..51.0, mapped to qindex 1..=255).
    ConstantQuality(f32),
    /// Direct quantization index (1 = minimum DCT quantizer / highest quality, 255 = maximum compression; 0 is clamped to 1).
    Qindex(u8),
    /// 1-pass adaptive CBR rate control targeting `kbps`.
    Cbr { kbps: u32 },
    /// Variable Bitrate with target and peak rate cap.
    Vbr { target_kbps: u32, max_kbps: u32 },
}

/// Encoding speed vs quality preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Preset {
    Speed,
    #[default]
    Balanced,
    Quality,
}

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub qindex: u8,
    pub key_int: u32,
    pub rate_control: Option<RateControl>,
    pub preset: Preset,
    pub spatial_aq: bool,
    pub enable_cdef: bool,
    pub scene_cut_detection: bool,
}

impl EncoderConfig {
    pub fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Self {
        Self {
            width,
            height,
            fps_num,
            fps_den,
            qindex: 100,
            key_int: 30,
            rate_control: None,
            preset: Preset::Balanced,
            spatial_aq: false,
            enable_cdef: true,
            scene_cut_detection: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub pts: i64,
}

pub struct YuvFrame<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
    pub y_stride: usize,
    pub uv_stride: usize,
}

#[derive(Debug)]
pub struct Encoder {
    config: EncoderConfig,
    seq_level_idx: u8,
    frame_count: u64,
    active_qindex: u8,
    bit_reservoir: i64,
    recon_y: Vec<u8>,
    recon_u: Vec<u8>,
    recon_v: Vec<u8>,
    ref_y: Vec<u8>,
    ref_u: Vec<u8>,
    ref_v: Vec<u8>,
    ref_src_y: Vec<u8>,
    last_key_frame: u64,
    last_frame_inter_blocks: usize,
    last_frame_intra_blocks: usize,
    last_frame_4x4_blocks: usize,
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn compute_block_sad(src: &[u8], src_stride: usize, ref_plane: &[u8], ref_stride: usize, x0: usize, y0: usize, w: usize, h: usize) -> u64 {
    let mut sad = 0u64;
    for r in 0..h {
        let Some(y) = y0.checked_add(r) else { continue };
        let Some(src_off) = y.checked_mul(src_stride).and_then(|o| o.checked_add(x0)) else { continue };
        let Some(ref_off) = y.checked_mul(ref_stride).and_then(|o| o.checked_add(x0)) else { continue };
        let (Some(src_end), Some(ref_end)) = (src_off.checked_add(w), ref_off.checked_add(w)) else { continue };
        if let (Some(s_row), Some(r_row)) = (src.get(src_off..src_end), ref_plane.get(ref_off..ref_end)) {
            let (chunks_s, rem_s) = s_row.as_chunks::<16>();
            let (chunks_r, rem_r) = r_row.as_chunks::<16>();
            for (cs, cr) in chunks_s.iter().zip(chunks_r.iter()) {
                let mut sum16 = 0u32;
                for i in 0..16 {
                    sum16 += cs[i].abs_diff(cr[i]) as u32;
                }
                sad += sum16 as u64;
            }
            for (&s, &re) in rem_s.iter().zip(rem_r.iter()) {
                sad += s.abs_diff(re) as u64;
            }
        }
    }
    sad
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn copy_block(src: &[u8], src_stride: usize, dst: &mut [u8], dst_stride: usize, x0: usize, y0: usize, w: usize, h: usize) {
    for r in 0..h {
        let Some(y) = y0.checked_add(r) else { continue };
        let Some(src_off) = y.checked_mul(src_stride).and_then(|o| o.checked_add(x0)) else { continue };
        let Some(dst_off) = y.checked_mul(dst_stride).and_then(|o| o.checked_add(x0)) else { continue };
        let (Some(src_end), Some(dst_end)) = (src_off.checked_add(w), dst_off.checked_add(w)) else { continue };
        if let (Some(s), Some(d)) = (src.get(src_off..src_end), dst.get_mut(dst_off..dst_end)) {
            d.copy_from_slice(s);
        }
    }
}

fn apply_spatial_aq(qindex: u8, var: u64) -> u8 {
    let adjusted = if var > 3000 {
        qindex.saturating_add(6)
    } else if var > 1500 {
        qindex.saturating_add(3)
    } else if var < 80 {
        qindex.saturating_sub(6).max(qindex.min(12))
    } else if var < 250 {
        qindex.saturating_sub(3).max(qindex.min(16))
    } else {
        qindex
    };
    adjusted.max(1)
}

impl Encoder {
    pub fn new(config: EncoderConfig) -> Result<Self> {
        let sb_cols = config.width.div_ceil(64);
        let sb_rows = config.height.div_ceil(64);
        if config.width == 0
            || config.height == 0
            || !config.width.is_multiple_of(2)
            || !config.height.is_multiple_of(2)
            || sb_cols > 64
            || config.height > 4352
            || u64::from(sb_cols).saturating_mul(u64::from(sb_rows)) > 2304
            || u64::from(config.width).saturating_mul(u64::from(config.height)) > 8_912_896
        {
            return Err(Error::Invalid("dimensions must be even and within the 4K Level 5.0 single-tile limit"));
        }
        if config.fps_num == 0 || config.fps_den == 0 || config.key_int == 0 {
            return Err(Error::Invalid("frame rate and keyframe interval must be positive"));
        }
        let seq_level_idx = obu::select_seq_level_idx(config.width, config.height, config.fps_num, config.fps_den)
            .ok_or(Error::Invalid("dimensions and frame rate exceed supported AV1 Level 5.3 single-tile limits"))?;
        if let Some(rc) = config.rate_control {
            match rc {
                RateControl::Cbr { kbps: 0 } => {
                    return Err(Error::Invalid("CBR bitrate must be positive"));
                }
                RateControl::Vbr { target_kbps, max_kbps } if target_kbps == 0 || max_kbps < target_kbps => {
                    return Err(Error::Invalid("VBR target bitrate must be positive and <= max bitrate"));
                }
                _ => {}
            }
        }
        let initial_qindex = match config.rate_control {
            Some(RateControl::ConstantQuality(crf)) => (crf * 4.0).round().clamp(1.0, 255.0) as u8,
            Some(RateControl::Qindex(q)) => q.max(1),
            Some(RateControl::Cbr { .. } | RateControl::Vbr { .. }) => config.qindex.max(1),
            None => config.qindex.max(1),
        };
        let y_len = (config.width as usize).saturating_mul(config.height as usize);
        let uv_len = ((config.width as usize) / 2).saturating_mul((config.height as usize) / 2);
        Ok(Self {
            config,
            seq_level_idx,
            frame_count: 0,
            active_qindex: initial_qindex,
            bit_reservoir: 0,
            recon_y: vec![128; y_len],
            recon_u: vec![128; uv_len],
            recon_v: vec![128; uv_len],
            ref_y: vec![128; y_len],
            ref_u: vec![128; uv_len],
            ref_v: vec![128; uv_len],
            ref_src_y: vec![0; y_len],
            last_key_frame: 0,
            last_frame_inter_blocks: 0,
            last_frame_intra_blocks: 0,
            last_frame_4x4_blocks: 0,
        })
    }

    pub fn last_frame_inter_blocks(&self) -> usize {
        self.last_frame_inter_blocks
    }

    pub fn last_frame_intra_blocks(&self) -> usize {
        self.last_frame_intra_blocks
    }

    pub fn last_frame_4x4_blocks(&self) -> usize {
        self.last_frame_4x4_blocks
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.config
    }

    pub fn seq_level_idx(&self) -> u8 {
        self.seq_level_idx
    }

    pub fn sequence_header_obu(&self) -> Vec<u8> {
        let seq_cfg = obu::SequenceConfig {
            width: self.config.width,
            height: self.config.height,
            bit_depth: 8,
            mono_chrome: false,
            enable_cdef: self.config.enable_cdef,
            seq_level_idx: self.seq_level_idx,
        };
        obu::write_sequence_header(&seq_cfg)
    }

    pub fn encode(&mut self, frame: &YuvFrame<'_>, pts: i64) -> Result<Packet> {
        let width = self.config.width as usize;
        let height = self.config.height as usize;

        if frame.y_stride < width {
            return Err(Error::Invalid("y_stride must be at least frame width"));
        }
        let uv_w = width / 2;
        let uv_h = height / 2;
        if frame.uv_stride < uv_w {
            return Err(Error::Invalid("uv_stride must be at least half frame width"));
        }

        let required_y = height
            .checked_sub(1)
            .and_then(|h| h.checked_mul(frame.y_stride))
            .and_then(|o| o.checked_add(width))
            .ok_or(Error::Invalid("frame buffer dimensions or stride overflow"))?;
        if frame.y.len() < required_y {
            return Err(Error::BufferTooSmall);
        }

        let required_uv = uv_h
            .checked_sub(1)
            .and_then(|h| h.checked_mul(frame.uv_stride))
            .and_then(|o| o.checked_add(uv_w))
            .ok_or(Error::Invalid("chroma buffer dimensions or stride overflow"))?;
        if frame.u.len() < required_uv || frame.v.len() < required_uv {
            return Err(Error::BufferTooSmall);
        }

        let mut is_key = self.frame_count == 0 || (self.frame_count.saturating_sub(self.last_key_frame)) >= u64::from(self.config.key_int);

        if !is_key && self.config.scene_cut_detection && self.frame_count > 0 {
            // Fast subsampled SAD across the frame (step by 4 rows and 4 cols: only 1/16th of pixels)
            let mut diff_sum = 0u64;
            let mut sample_count = 0u64;
            for r in (0..height).step_by(4) {
                let Some(curr_off) = r.checked_mul(frame.y_stride) else { continue };
                let Some(prev_off) = r.checked_mul(width) else { continue };
                let (Some(curr_end), Some(prev_end)) = (curr_off.checked_add(width), prev_off.checked_add(width)) else { continue };
                if let (Some(curr_row), Some(prev_row)) = (frame.y.get(curr_off..curr_end), self.ref_src_y.get(prev_off..prev_end)) {
                    for (&c, &p) in curr_row.iter().step_by(4).zip(prev_row.iter().step_by(4)) {
                        diff_sum += c.abs_diff(p) as u64;
                        sample_count += 1;
                    }
                }
            }
            if sample_count > 0 && (diff_sum / sample_count) > 42 {
                is_key = true;
            }
        }

        if is_key {
            self.last_key_frame = self.frame_count;
        }

        let frame_type = if is_key { frame_header::KEY_FRAME } else { frame_header::INTER_FRAME };
        let order_hint = (self.frame_count & 0x7f) as u32;
        let seq_cfg = obu::SequenceConfig {
            width: self.config.width,
            height: self.config.height,
            bit_depth: 8,
            mono_chrome: false,
            enable_cdef: self.config.enable_cdef,
            seq_level_idx: self.seq_level_idx,
        };

        // Determine base quantization index for this frame (clamped to 1..=255 for DCT coding)
        let mut qindex = match self.config.rate_control {
            Some(RateControl::ConstantQuality(crf)) => (crf * 4.0).round().clamp(1.0, 255.0) as u8,
            Some(RateControl::Qindex(q)) => q.max(1),
            Some(RateControl::Cbr { .. } | RateControl::Vbr { .. }) => self.active_qindex.max(1),
            None => self.config.qindex.max(1),
        };

        // IP offset: Boost Keyframe quality slightly to establish superior reference for downstream P-frames
        if is_key && qindex > 16 {
            qindex = qindex.saturating_sub(4).max(12);
        }

        if self.config.spatial_aq && !frame.y.is_empty() {
            let var = intra::block_variance(frame.y, frame.y_stride, self.config.width as usize, self.config.height as usize);
            qindex = apply_spatial_aq(qindex, var);
        }

        let fh_cfg =
            frame_header::FrameHeaderConfig { frame_type, show_frame: true, error_resilient_mode: false, disable_cdf_update: true, qindex, order_hint };

        let mut tu = Vec::with_capacity(512);
        tu.extend_from_slice(&obu::write_temporal_delimiter());
        if is_key || self.frame_count == 0 {
            tu.extend_from_slice(&obu::write_sequence_header(&seq_cfg));
        }
        tu.extend_from_slice(&obu::write_obu(obu::OBU_FRAME_HEADER, &frame_header::write_frame_header(&fh_cfg, &seq_cfg)));

        let sb_cols = self.config.width.div_ceil(64);
        let sb_rows = self.config.height.div_ceil(64);
        let total_sbs = (sb_cols as usize).saturating_mul(sb_rows as usize);
        let mut sym = symbol::SymbolEncoder::with_capacity(total_sbs.saturating_mul(4).max(64));

        #[derive(Clone, Copy, Debug)]
        struct SbAnalysis {
            mode: u8,
            split: bool,
            is_flat: bool,
            is_inter: bool,
        }

        let ref_y_plane = &self.ref_y;
        let has_prev_frame = self.frame_count > 0;

        // Row Analysis (Wavefront/Row parallelism):
        // Compute best intra mode, quad-tree split decision, and flat-skip flag concurrently across CPU cores.
        let analyze_row = |r: u32| -> Vec<SbAnalysis> {
            let y0 = (r as usize) * 64;
            let block_h = 64.min(height.saturating_sub(y0));
            let avail_u = r > 0;

            let mut row_analyses = Vec::with_capacity(sb_cols as usize);
            for c in 0..sb_cols {
                let avail_l = c > 0;
                let x0 = (c as usize) * 64;
                let block_w = 64.min(width.saturating_sub(x0));

                let is_inter = if !is_key && has_prev_frame && block_w == 64 && block_h == 64 {
                    let y_sad = compute_block_sad(frame.y, frame.y_stride, ref_y_plane, width, x0, y0, block_w, block_h);
                    let u_sad = compute_block_sad(frame.u, frame.uv_stride, &self.ref_u, width / 2, x0 / 2, y0 / 2, 32, 32);
                    let v_sad = compute_block_sad(frame.v, frame.uv_stride, &self.ref_v, width / 2, x0 / 2, y0 / 2, 32, 32);
                    let per_px = if qindex <= 8 {
                        0
                    } else if qindex <= 60 {
                        1
                    } else {
                        2
                    };
                    y_sad <= (64 * 64) * per_px && u_sad <= (32 * 32) * per_px && v_sad <= (32 * 32) * per_px
                } else {
                    false
                };

                if is_inter {
                    row_analyses.push(SbAnalysis { mode: intra::DC_PRED, split: false, is_flat: false, is_inter: true });
                    continue;
                }

                let above_slice = if avail_u && y0 > 0 {
                    let row_offset = (y0 - 1) * frame.y_stride + x0;
                    frame.y.get(row_offset..row_offset + block_w)
                } else {
                    None
                };

                let mut left_col = [0u8; 64];
                let left_slice = if avail_l && x0 > 0 {
                    for i in 0..block_h {
                        if let Some(&p) = frame.y.get((y0 + i) * frame.y_stride + x0 - 1) {
                            left_col[i] = p;
                        }
                    }
                    Some(&left_col[..block_h])
                } else {
                    None
                };

                let src_sub = frame.y.get(y0 * frame.y_stride + x0..);
                let mode = if self.config.preset != Preset::Speed {
                    if let Some(src) = src_sub { pick_dc_v_h_mode(src, frame.y_stride, block_w, block_h, above_slice, left_slice) } else { intra::DC_PRED }
                } else {
                    intra::DC_PRED
                };

                let split = should_split_64x64(frame.y, frame.y_stride, x0, y0, block_w, block_h, self.config.preset, qindex);

                let is_flat = if let Some(src) = src_sub {
                    let var = intra::block_variance(src, frame.y_stride, block_w, block_h);
                    var <= 16
                } else {
                    false
                };

                row_analyses.push(SbAnalysis { mode, split, is_flat, is_inter: false });
            }
            row_analyses
        };

        let precomputed_analyses: Vec<Vec<SbAnalysis>> = {
            #[cfg(feature = "threads")]
            {
                use rayon::prelude::*;
                (0..sb_rows).into_par_iter().map(analyze_row).collect()
            }
            #[cfg(not(feature = "threads"))]
            {
                (0..sb_rows).map(analyze_row).collect()
            }
        };

        let uv_width = width / 2;

        if is_key || self.frame_count == 0 {
            self.recon_y.fill(128);
            self.recon_u.fill(128);
            self.recon_v.fill(128);
        }

        let mi_cols = 2 * ((width + 7) >> 3);
        let mi_rows = 2 * ((height + 7) >> 3);
        let grid_len = mi_cols.saturating_mul(mi_rows);
        let mut grid_y_mode = vec![intra::DC_PRED; grid_len];
        let mut grid_skip = vec![false; grid_len];
        let mut grid_bsl = vec![4u8; grid_len];
        let mut grid_ref_frame = vec![0i8; grid_len];

        let mut above_dc = [vec![0u8; mi_cols + 16], vec![0u8; (mi_cols / 2) + 16], vec![0u8; (mi_cols / 2) + 16]];
        let mut left_dc = [vec![0u8; mi_rows + 16], vec![0u8; (mi_rows / 2) + 16], vec![0u8; (mi_rows / 2) + 16]];

        let mut enc_ctx = TileEncodeCtx {
            sym: &mut sym,
            frame,
            width,
            height,
            uv_width,
            mi_rows,
            mi_cols,
            is_key,
            has_prev_frame,
            qindex,
            preset: self.config.preset,
            recon_y: &mut self.recon_y,
            recon_u: &mut self.recon_u,
            recon_v: &mut self.recon_v,
            ref_y: &self.ref_y,
            ref_u: &self.ref_u,
            ref_v: &self.ref_v,
            grid_y_mode: &mut grid_y_mode,
            grid_skip: &mut grid_skip,
            grid_bsl: &mut grid_bsl,
            grid_ref_frame: &mut grid_ref_frame,
            above_dc: &mut above_dc,
            left_dc: &mut left_dc,
            inter_blocks: 0,
            intra_blocks: 0,
            blocks_4x4: 0,
        };

        for sb_r in 0..sb_rows as usize {
            for p in 0..3 {
                enc_ctx.left_dc[p].fill(0);
            }
            for sb_c in 0..sb_cols as usize {
                let r = sb_r * 16;
                let c = sb_c * 16;
                let sb_info = precomputed_analyses.get(sb_r).and_then(|row| row.get(sb_c)).copied().unwrap_or(SbAnalysis {
                    mode: intra::DC_PRED,
                    split: false,
                    is_flat: false,
                    is_inter: false,
                });
                encode_partition(&mut enc_ctx, r, c, 4, sb_info.split, Some((sb_info.mode, sb_info.is_flat, sb_info.is_inter)));
            }
        }

        self.last_frame_inter_blocks = enc_ctx.inter_blocks;
        self.last_frame_intra_blocks = enc_ctx.intra_blocks;
        self.last_frame_4x4_blocks = enc_ctx.blocks_4x4;

        let tile_payload = sym.finish();
        tu.extend_from_slice(&obu::write_obu(obu::OBU_TILE_GROUP, &tile_payload));

        // Update CBR/VBR bit reservoir and active qindex
        if let Some(rc) = self.config.rate_control {
            match rc {
                RateControl::Cbr { kbps } => {
                    let target_bits_128 = (u128::from(kbps) * 1000 * u128::from(self.config.fps_den)) / u128::from(self.config.fps_num).max(1);
                    let target_bits = target_bits_128.min((i64::MAX / 4) as u128) as i64;
                    let frame_bits = (tu.len() as i64).saturating_mul(8);
                    self.bit_reservoir = self.bit_reservoir.saturating_add(target_bits.saturating_sub(frame_bits));
                    let buffer_threshold = target_bits.saturating_mul(2);
                    if self.bit_reservoir < -buffer_threshold {
                        self.active_qindex = self.active_qindex.saturating_add(6);
                    } else if self.bit_reservoir > buffer_threshold {
                        self.active_qindex = self.active_qindex.saturating_sub(4).max(20);
                    }
                }
                RateControl::Vbr { target_kbps, max_kbps } => {
                    let target_bits_128 = (u128::from(target_kbps) * 1000 * u128::from(self.config.fps_den)) / u128::from(self.config.fps_num).max(1);
                    let target_bits = target_bits_128.min((i64::MAX / 4) as u128) as i64;
                    let max_bits_128 = (u128::from(max_kbps) * 1000 * u128::from(self.config.fps_den)) / u128::from(self.config.fps_num).max(1);
                    let max_bits = max_bits_128.min((i64::MAX / 4) as u128) as i64;
                    let frame_bits = (tu.len() as i64).saturating_mul(8);
                    if frame_bits > max_bits {
                        self.active_qindex = self.active_qindex.saturating_add(8);
                    } else {
                        self.bit_reservoir = self.bit_reservoir.saturating_add(target_bits.saturating_sub(frame_bits));
                        let buffer_threshold = target_bits.saturating_mul(3);
                        if self.bit_reservoir < -buffer_threshold {
                            self.active_qindex = self.active_qindex.saturating_add(4);
                        } else if self.bit_reservoir > buffer_threshold {
                            self.active_qindex = self.active_qindex.saturating_sub(2).max(20);
                        }
                    }
                }
                _ => {}
            }
        }

        self.ref_y.copy_from_slice(&self.recon_y);
        self.ref_u.copy_from_slice(&self.recon_u);
        self.ref_v.copy_from_slice(&self.recon_v);
        copy_block(frame.y, frame.y_stride, &mut self.ref_src_y, width, 0, 0, width, height);

        self.frame_count = self.frame_count.saturating_add(1);
        Ok(Packet { data: tu, keyframe: is_key, pts })
    }

    pub fn flush(&mut self) -> Result<Vec<Packet>> {
        Ok(Vec::new())
    }
}

struct TileEncodeCtx<'a, 'f> {
    sym: &'a mut symbol::SymbolEncoder,
    frame: &'a YuvFrame<'f>,
    width: usize,
    height: usize,
    uv_width: usize,
    mi_rows: usize,
    mi_cols: usize,
    is_key: bool,
    has_prev_frame: bool,
    qindex: u8,
    preset: Preset,
    recon_y: &'a mut [u8],
    recon_u: &'a mut [u8],
    recon_v: &'a mut [u8],
    ref_y: &'a [u8],
    ref_u: &'a [u8],
    ref_v: &'a [u8],
    grid_y_mode: &'a mut [u8],
    grid_skip: &'a mut [bool],
    grid_bsl: &'a mut [u8],
    grid_ref_frame: &'a mut [i8],
    above_dc: &'a mut [Vec<u8>; 3],
    left_dc: &'a mut [Vec<u8>; 3],
    inter_blocks: usize,
    intra_blocks: usize,
    blocks_4x4: usize,
}

fn write_split_bool(sym: &mut symbol::SymbolEncoder, pcdf: &[u16], split_or_horz: bool) {
    let pr = |k: usize| pcdf.get(k).copied().unwrap_or(0) as i32 - if k == 0 { 0 } else { pcdf.get(k - 1).copied().unwrap_or(0) as i32 };
    let mut psum = if split_or_horz {
        pr(tables::PARTITION_VERT)
            + pr(tables::PARTITION_SPLIT)
            + pr(tables::PARTITION_HORZ_A)
            + pr(tables::PARTITION_VERT_A)
            + pr(tables::PARTITION_VERT_B)
            + pr(tables::PARTITION_VERT_4)
    } else {
        pr(tables::PARTITION_HORZ)
            + pr(tables::PARTITION_SPLIT)
            + pr(tables::PARTITION_HORZ_A)
            + pr(tables::PARTITION_HORZ_B)
            + pr(tables::PARTITION_VERT_A)
            + pr(tables::PARTITION_HORZ_4)
    };
    psum = psum.clamp(1, (1 << 15) - 1);
    let cdf = [((1i32 << 15) - psum) as u16, 1 << 15, 0];
    sym.write_symbol_no_update(1, &cdf);
}

fn encode_partition(ctx: &mut TileEncodeCtx<'_, '_>, r: usize, c: usize, bsl: usize, force_split_64: bool, sb64_analysis: Option<(u8, bool, bool)>) {
    if r >= ctx.mi_rows || c >= ctx.mi_cols {
        return;
    }
    if bsl == 0 {
        encode_block(ctx, r, c, 0, None);
        return;
    }
    let num4x4 = 1usize << bsl;
    let half = num4x4 >> 1;
    let has_rows = (r + half) < ctx.mi_rows;
    let has_cols = (c + half) < ctx.mi_cols;

    let avail_u = r > 0;
    let avail_l = c > 0;
    let above_split = avail_u && (ctx.grid_bsl.get((r - 1) * ctx.mi_cols + c).copied().unwrap_or(4) as usize) < bsl;
    let left_split = avail_l && (ctx.grid_bsl.get(r * ctx.mi_cols + c - 1).copied().unwrap_or(4) as usize) < bsl;
    let part_ctx = (left_split as usize) * 2 + (above_split as usize);

    let do_split = if bsl == 0 {
        false
    } else if !has_rows || !has_cols {
        bsl > 1
    } else if bsl == 4 && sb64_analysis.is_some_and(|(_, _, inter)| inter) {
        false
    } else {
        let bw = (num4x4 * 4).min(ctx.width.saturating_sub(c * 4));
        let bh = (num4x4 * 4).min(ctx.height.saturating_sub(r * 4));
        (bsl == 4 && force_split_64)
            || should_split_block(
                ctx.frame.y,
                ctx.frame.y_stride,
                ctx.frame.u,
                ctx.frame.v,
                ctx.frame.uv_stride,
                c * 4,
                r * 4,
                bw,
                bh,
                bsl,
                ctx.preset,
                ctx.qindex,
            )
    };

    if has_rows && has_cols {
        let sym_val = if do_split { tables::PARTITION_SPLIT } else { tables::PARTITION_NONE };
        match bsl {
            4 => ctx.sym.write_symbol_no_update(sym_val, tables::DEFAULT_PARTITION_W64_CDF.get(part_ctx).unwrap_or(&tables::DEFAULT_PARTITION_W64_CDF[0])),
            3 => ctx.sym.write_symbol_no_update(sym_val, tables::DEFAULT_PARTITION_W32_CDF.get(part_ctx).unwrap_or(&tables::DEFAULT_PARTITION_W32_CDF[0])),
            2 => ctx.sym.write_symbol_no_update(sym_val, tables::DEFAULT_PARTITION_W16_CDF.get(part_ctx).unwrap_or(&tables::DEFAULT_PARTITION_W16_CDF[0])),
            _ => ctx.sym.write_symbol_no_update(sym_val, tables::DEFAULT_PARTITION_W8_CDF.get(part_ctx).unwrap_or(&tables::DEFAULT_PARTITION_W8_CDF[0])),
        }
    } else if has_cols || has_rows {
        let pcdf: &[u16] = match bsl {
            4 => tables::DEFAULT_PARTITION_W64_CDF.get(part_ctx).unwrap_or(&tables::DEFAULT_PARTITION_W64_CDF[0]),
            3 => tables::DEFAULT_PARTITION_W32_CDF.get(part_ctx).unwrap_or(&tables::DEFAULT_PARTITION_W32_CDF[0]),
            _ => tables::DEFAULT_PARTITION_W16_CDF.get(part_ctx).unwrap_or(&tables::DEFAULT_PARTITION_W16_CDF[0]),
        };
        write_split_bool(ctx.sym, pcdf, has_cols);
    }

    if do_split {
        let next_bsl = bsl - 1;
        encode_partition(ctx, r, c, next_bsl, false, None);
        encode_partition(ctx, r, c + half, next_bsl, false, None);
        encode_partition(ctx, r + half, c, next_bsl, false, None);
        encode_partition(ctx, r + half, c + half, next_bsl, false, None);
    } else {
        encode_block(ctx, r, c, bsl, sb64_analysis);
    }
}

#[allow(clippy::too_many_arguments)]
fn get_dc_sign_ctx(x4: usize, y4: usize, w4: usize, h4: usize, max_x4: usize, max_y4: usize, above: &[u8], left: &[u8]) -> usize {
    let mut dc_sign = 0i32;
    for k in 0..w4 {
        if x4 + k < max_x4
            && let Some(&cat) = above.get(x4 + k)
        {
            match cat {
                1 => dc_sign -= 1,
                2 => dc_sign += 1,
                _ => {}
            }
        }
    }
    for k in 0..h4 {
        if y4 + k < max_y4
            && let Some(&cat) = left.get(y4 + k)
        {
            match cat {
                1 => dc_sign -= 1,
                2 => dc_sign += 1,
                _ => {}
            }
        }
    }
    match dc_sign.signum() {
        -1 => 1,
        1 => 2,
        _ => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn get_chroma_txb_skip_ctx(x4: usize, y4: usize, w4: usize, h4: usize, max_x4: usize, max_y4: usize, above: &[u8], left: &[u8]) -> usize {
    let mut a = 0u8;
    let mut l = 0u8;
    for k in 0..w4 {
        if x4 + k < max_x4
            && let Some(&cat) = above.get(x4 + k)
        {
            a |= cat;
        }
    }
    for k in 0..h4 {
        if y4 + k < max_y4
            && let Some(&cat) = left.get(y4 + k)
        {
            l |= cat;
        }
    }
    7 + (a != 0) as usize + (l != 0) as usize
}

#[allow(clippy::too_many_arguments)]
fn update_dc_sign(above: &mut [u8], left: &mut [u8], x4: usize, y4: usize, w4: usize, h4: usize, level: u32, is_neg: bool) {
    let cat = if level > 0 { if is_neg { 1 } else { 2 } } else { 0 };
    for k in 0..w4 {
        if let Some(slot) = above.get_mut(x4 + k) {
            *slot = cat;
        }
    }
    for k in 0..h4 {
        if let Some(slot) = left.get_mut(y4 + k) {
            *slot = cat;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn compute_new_mv_ctx(grid_ref_frame: &[i8], grid_bsl: &[u8], mi_rows: usize, mi_cols: usize, r: usize, c: usize, bw4: usize, bh4: usize) -> usize {
    let inside = |mr: isize, mc: isize| mr >= 0 && (mr as usize) < mi_rows && mc >= 0 && (mc as usize) < mi_cols;
    let is_inter_at = |mr: isize, mc: isize| inside(mr, mc) && grid_ref_frame.get((mr as usize) * mi_cols + (mc as usize)).copied().unwrap_or(0) == 1;
    let scan_row = |delta_row: isize| -> bool {
        let end4 = (bw4 as isize).min(mi_cols as isize - c as isize).min(16);
        let use_step16 = bw4 >= 16;
        let (dr, dc) = if delta_row.abs() > 1 { (delta_row + (r as isize & 1), 1 - (c as isize & 1)) } else { (delta_row, 0) };
        let mv_row = r as isize + dr;
        let mut found = false;
        let mut i = 0isize;
        while i < end4 {
            let mv_col = c as isize + dc + i;
            if !inside(mv_row, mv_col) {
                break;
            }
            if is_inter_at(mv_row, mv_col) {
                found = true;
            }
            let bsl = grid_bsl.get((mv_row as usize) * mi_cols + (mv_col as usize)).copied().unwrap_or(4);
            let mut len = (bw4 as isize).min(1isize << bsl);
            if delta_row.abs() > 1 {
                len = len.max(2);
            }
            if use_step16 {
                len = len.max(4);
            }
            i += len.max(1);
        }
        found
    };
    let scan_col = |delta_col: isize| -> bool {
        let end4 = (bh4 as isize).min(mi_rows as isize - r as isize).min(16);
        let use_step16 = bh4 >= 16;
        let (dr, dc) = if delta_col.abs() > 1 { (1 - (r as isize & 1), delta_col + (c as isize & 1)) } else { (0, delta_col) };
        let mv_col = c as isize + dc;
        let mut found = false;
        let mut i = 0isize;
        while i < end4 {
            let mv_row = r as isize + dr + i;
            if !inside(mv_row, mv_col) {
                break;
            }
            if is_inter_at(mv_row, mv_col) {
                found = true;
            }
            let bsl = grid_bsl.get((mv_row as usize) * mi_cols + (mv_col as usize)).copied().unwrap_or(4);
            let mut len = (bh4 as isize).min(1isize << bsl);
            if delta_col.abs() > 1 {
                len = len.max(2);
            }
            if use_step16 {
                len = len.max(4);
            }
            i += len.max(1);
        }
        found
    };

    let mut found_above = scan_row(-1);
    let mut found_left = scan_col(-1);
    // For 64x64 superblocks (bw4 == 16), row r - 1 is in the previous superblock row and already written.
    if bw4.max(bh4) <= 16 && is_inter_at(r as isize - 1, (c + bw4) as isize) {
        found_above = true;
    }
    let close_matches = found_above as usize + found_left as usize;
    if is_inter_at(r as isize - 1, c as isize - 1) {
        found_above = true;
    }
    if scan_row(-3) {
        found_above = true;
    }
    if scan_col(-3) {
        found_left = true;
    }
    if bh4 > 1 && scan_row(-5) {
        found_above = true;
    }
    if bw4 > 1 && scan_col(-5) {
        found_left = true;
    }
    let total_matches = found_above as usize + found_left as usize;
    match close_matches {
        0 => total_matches.min(1),
        1 => 3,
        _ => 5,
    }
}

#[allow(clippy::cognitive_complexity)]
fn encode_block(ctx: &mut TileEncodeCtx<'_, '_>, r: usize, c: usize, bsl: usize, sb64_analysis: Option<(u8, bool, bool)>) {
    let bw4 = 1usize << bsl;
    let bh4 = 1usize << bsl;
    let x0 = c * 4;
    let y0 = r * 4;
    let block_w = (bw4 * 4).min(ctx.width.saturating_sub(x0));
    let block_h = (bh4 * 4).min(ctx.height.saturating_sub(y0));
    let avail_u = r > 0;
    let avail_l = c > 0;

    let above_skip = if avail_u { ctx.grid_skip.get((r - 1) * ctx.mi_cols + c).copied().unwrap_or(false) } else { false };
    let left_skip = if avail_l { ctx.grid_skip.get(r * ctx.mi_cols + c - 1).copied().unwrap_or(false) } else { false };
    let skip_ctx = (above_skip as usize) + (left_skip as usize);

    let (pre_mode, _is_flat, is_inter) = match sb64_analysis {
        Some((m, flat, inter)) if bsl == 4 => (Some(m), flat, inter),
        _ => {
            let inter = if !ctx.is_key && ctx.has_prev_frame && bsl >= 1 && x0 + bw4 * 4 <= ctx.width && y0 + bh4 * 4 <= ctx.height {
                let y_sad = compute_block_sad(ctx.frame.y, ctx.frame.y_stride, ctx.ref_y, ctx.width, x0, y0, block_w, block_h);
                let uw = (block_w / 2).max(1);
                let uh = (block_h / 2).max(1);
                let u_sad = compute_block_sad(ctx.frame.u, ctx.frame.uv_stride, ctx.ref_u, ctx.uv_width, x0 / 2, y0 / 2, uw, uh);
                let v_sad = compute_block_sad(ctx.frame.v, ctx.frame.uv_stride, ctx.ref_v, ctx.uv_width, x0 / 2, y0 / 2, uw, uh);
                let per_px = if ctx.qindex <= 8 {
                    0
                } else if ctx.qindex <= 60 {
                    1
                } else {
                    2
                };
                y_sad <= ((block_w * block_h) as u64) * per_px && u_sad <= ((uw * uh) as u64) * per_px && v_sad <= ((uw * uh) as u64) * per_px
            } else {
                false
            };
            (None, false, inter)
        }
    };

    if !ctx.is_key && is_inter {
        ctx.sym.write_symbol_no_update(1, tables::DEFAULT_SKIP_CDF.get(skip_ctx).unwrap_or(&tables::DEFAULT_SKIP_CDF[0]));

        let above_intra = if avail_u { ctx.grid_ref_frame.get((r - 1) * ctx.mi_cols + c).copied().unwrap_or(0) == 0 } else { true };
        let left_intra = if avail_l { ctx.grid_ref_frame.get(r * ctx.mi_cols + c - 1).copied().unwrap_or(0) == 0 } else { true };
        let is_inter_ctx = if avail_u && avail_l {
            if left_intra && above_intra { 3 } else { (left_intra || above_intra) as usize }
        } else if avail_u || avail_l {
            2 * (if avail_u { above_intra } else { left_intra }) as usize
        } else {
            0
        };
        ctx.sym.write_symbol_no_update(1, tables::DEFAULT_IS_INTER_CDF.get(is_inter_ctx).unwrap_or(&tables::DEFAULT_IS_INTER_CDF[0]));

        let count_last = {
            let mut cnt = 0;
            if avail_u && ctx.grid_ref_frame.get((r - 1) * ctx.mi_cols + c).copied().unwrap_or(0) == 1 {
                cnt += 1;
            }
            if avail_l && ctx.grid_ref_frame.get(r * ctx.mi_cols + c - 1).copied().unwrap_or(0) == 1 {
                cnt += 1;
            }
            cnt
        };
        let ref_ctx = if count_last > 0 { 2 } else { 1 };
        let single_ref = tables::DEFAULT_SINGLE_REF_CDF.get(ref_ctx).unwrap_or(&tables::DEFAULT_SINGLE_REF_CDF[0]);
        ctx.sym.write_symbol_no_update(0, single_ref.first().unwrap_or(&single_ref[0]));
        ctx.sym.write_symbol_no_update(0, single_ref.get(2).unwrap_or(&single_ref[0]));
        ctx.sym.write_symbol_no_update(0, single_ref.get(3).unwrap_or(&single_ref[0]));

        let new_mv_ctx = compute_new_mv_ctx(ctx.grid_ref_frame, ctx.grid_bsl, ctx.mi_rows, ctx.mi_cols, r, c, bw4, bh4);
        ctx.sym.write_symbol_no_update(1, tables::DEFAULT_NEW_MV_CDF.get(new_mv_ctx).unwrap_or(&tables::DEFAULT_NEW_MV_CDF[0]));
        ctx.sym.write_symbol_no_update(0, tables::DEFAULT_ZERO_MV_CDF.first().unwrap_or(&tables::DEFAULT_ZERO_MV_CDF[0]));

        copy_block(ctx.ref_y, ctx.width, ctx.recon_y, ctx.width, x0, y0, block_w, block_h);
        copy_block(ctx.ref_u, ctx.uv_width, ctx.recon_u, ctx.uv_width, x0 / 2, y0 / 2, (block_w / 2).max(1), (block_h / 2).max(1));
        copy_block(ctx.ref_v, ctx.uv_width, ctx.recon_v, ctx.uv_width, x0 / 2, y0 / 2, (block_w / 2).max(1), (block_h / 2).max(1));

        let uv_w4 = (bw4 / 2).max(1);
        let uv_h4 = (bh4 / 2).max(1);
        update_dc_sign(&mut ctx.above_dc[0], &mut ctx.left_dc[0], c, r, bw4, bh4, 0, false);
        update_dc_sign(&mut ctx.above_dc[1], &mut ctx.left_dc[1], c / 2, r / 2, uv_w4, uv_h4, 0, false);
        update_dc_sign(&mut ctx.above_dc[2], &mut ctx.left_dc[2], c / 2, r / 2, uv_w4, uv_h4, 0, false);

        let rows = bh4.min(ctx.mi_rows.saturating_sub(r));
        let cols = bw4.min(ctx.mi_cols.saturating_sub(c));
        for dy in 0..rows {
            let row_start = (r + dy) * ctx.mi_cols + c;
            let span = row_start..row_start + cols;
            ctx.grid_y_mode[span.clone()].fill(intra::DC_PRED);
            ctx.grid_skip[span.clone()].fill(true);
            ctx.grid_ref_frame[span.clone()].fill(1);
            ctx.grid_bsl[span].fill(bsl as u8);
        }
        ctx.inter_blocks = ctx.inter_blocks.saturating_add(1);
        return;
    }

    let above_mode = if avail_u { ctx.grid_y_mode.get((r - 1) * ctx.mi_cols + c).copied().unwrap_or(intra::DC_PRED) } else { intra::DC_PRED };
    let left_mode = if avail_l { ctx.grid_y_mode.get(r * ctx.mi_cols + c - 1).copied().unwrap_or(intra::DC_PRED) } else { intra::DC_PRED };
    let actx = tables::INTRA_MODE_CONTEXT.get(above_mode as usize).copied().unwrap_or(0) as usize;
    let lctx = tables::INTRA_MODE_CONTEXT.get(left_mode as usize).copied().unwrap_or(0) as usize;

    let raw_y_mode = if let Some(m) = pre_mode {
        m
    } else if ctx.preset != Preset::Speed {
        let mut above_buf = [128u8; 64];
        let has_above_ref = if avail_u && y0 > 0 {
            let off = (y0 - 1) * ctx.width + x0;
            if let Some(row) = ctx.recon_y.get(off..off + block_w) {
                above_buf[..block_w].copy_from_slice(row);
                true
            } else {
                false
            }
        } else {
            false
        };

        let mut left_buf = [128u8; 64];
        let has_left_ref = if avail_l && x0 > 0 {
            for y in 0..block_h {
                if let Some(&p) = ctx.recon_y.get((y0 + y) * ctx.width + x0 - 1) {
                    left_buf[y] = p;
                }
            }
            true
        } else {
            false
        };

        let src_off = y0 * ctx.frame.y_stride + x0;
        if let Some(src_sub) = ctx.frame.y.get(src_off..) {
            pick_dc_v_h_mode(
                src_sub,
                ctx.frame.y_stride,
                block_w,
                block_h,
                if has_above_ref { Some(&above_buf[..block_w]) } else { None },
                if has_left_ref { Some(&left_buf[..block_h]) } else { None },
            )
        } else {
            intra::DC_PRED
        }
    } else {
        intra::DC_PRED
    };
    let y_mode = match raw_y_mode {
        intra::V_PRED if avail_u && y0 > 0 => intra::V_PRED,
        intra::H_PRED if avail_l && x0 > 0 => intra::H_PRED,
        _ => intra::DC_PRED,
    };

    let has_chroma = bsl > 0 || ((r & 1) == 1 && (c & 1) == 1);
    let avail_u_chroma = if bsl == 0 { r >= 2 } else { avail_u };
    let avail_l_chroma = if bsl == 0 { c >= 2 } else { avail_l };
    let uv_mode = intra::DC_PRED;

    let luma_scale: i64 = match bsl {
        4 => 512,
        3 => 256,
        2 => 128,
        1 => 64,
        _ => 32,
    };
    let chroma_scale: i64 = match bsl {
        4 => 256,
        3 => 128,
        2 => 64,
        _ => 32,
    };
    let luma_tx_w = bw4 * 4;
    let luma_tx_h = bh4 * 4;
    let chroma_tx_w = (luma_tx_w / 2).max(4);
    let chroma_tx_h = (luma_tx_h / 2).max(4);
    let chroma_x0 = (c / 2) * 4;
    let chroma_y0 = (r / 2) * 4;
    let chroma_bw = if bsl == 0 { 4.min(ctx.uv_width.saturating_sub(chroma_x0)) } else { (block_w / 2).max(1) };
    let chroma_bh = if bsl == 0 { 4.min((ctx.height / 2).saturating_sub(chroma_y0)) } else { (block_h / 2).max(1) };

    let (dc_level, is_neg) = compute_and_reconstruct_plane_dc(
        ctx.frame.y,
        ctx.frame.y_stride,
        ctx.recon_y,
        ctx.width,
        x0,
        y0,
        block_w,
        block_h,
        luma_tx_w,
        luma_tx_h,
        y_mode,
        avail_u,
        avail_l,
        ctx.qindex,
        luma_scale,
    );
    let (u_dc_level, u_is_neg, v_dc_level, v_is_neg) = if has_chroma {
        let (ul, un) = compute_and_reconstruct_plane_dc(
            ctx.frame.u,
            ctx.frame.uv_stride,
            ctx.recon_u,
            ctx.uv_width,
            chroma_x0,
            chroma_y0,
            chroma_bw,
            chroma_bh,
            chroma_tx_w,
            chroma_tx_h,
            uv_mode,
            avail_u_chroma,
            avail_l_chroma,
            ctx.qindex,
            chroma_scale,
        );
        let (vl, vn) = compute_and_reconstruct_plane_dc(
            ctx.frame.v,
            ctx.frame.uv_stride,
            ctx.recon_v,
            ctx.uv_width,
            chroma_x0,
            chroma_y0,
            chroma_bw,
            chroma_bh,
            chroma_tx_w,
            chroma_tx_h,
            uv_mode,
            avail_u_chroma,
            avail_l_chroma,
            ctx.qindex,
            chroma_scale,
        );
        (ul, un, vl, vn)
    } else {
        (0, false, 0, false)
    };

    let has_residual = dc_level > 0 || u_dc_level > 0 || v_dc_level > 0;
    let cfl_allowed = bsl <= 3 && ctx.qindex > 0;
    // SIZE_GROUP in AV1 spec: BLOCK_4X4 -> 0, BLOCK_8X8 -> 1, BLOCK_16X16 -> 2, BLOCK_32X32/64X64 -> 3
    let size_grp = match bsl {
        4 | 3 => 3,
        2 => 2,
        1 => 1,
        _ => 0,
    };

    if ctx.is_key {
        ctx.sym.write_symbol_no_update(if has_residual { 0 } else { 1 }, tables::DEFAULT_SKIP_CDF.get(skip_ctx).unwrap_or(&tables::DEFAULT_SKIP_CDF[0]));
        let intra_y_cdf = tables::DEFAULT_INTRA_FRAME_Y_MODE_CDF.get(actx).and_then(|a| a.get(lctx)).unwrap_or(&tables::DEFAULT_INTRA_FRAME_Y_MODE_CDF[0][0]);
        ctx.sym.write_symbol_no_update(y_mode as usize, intra_y_cdf);
    } else {
        let above_intra = if avail_u { ctx.grid_ref_frame.get((r - 1) * ctx.mi_cols + c).copied().unwrap_or(0) == 0 } else { true };
        let left_intra = if avail_l { ctx.grid_ref_frame.get(r * ctx.mi_cols + c - 1).copied().unwrap_or(0) == 0 } else { true };
        let is_inter_ctx = if avail_u && avail_l {
            if left_intra && above_intra { 3 } else { (left_intra || above_intra) as usize }
        } else if avail_u || avail_l {
            2 * (if avail_u { above_intra } else { left_intra }) as usize
        } else {
            0
        };
        ctx.sym.write_symbol_no_update(if has_residual { 0 } else { 1 }, tables::DEFAULT_SKIP_CDF.get(skip_ctx).unwrap_or(&tables::DEFAULT_SKIP_CDF[0]));
        ctx.sym.write_symbol_no_update(0, tables::DEFAULT_IS_INTER_CDF.get(is_inter_ctx).unwrap_or(&tables::DEFAULT_IS_INTER_CDF[0]));
        ctx.sym.write_symbol_no_update(y_mode as usize, tables::DEFAULT_Y_MODE_CDF.get(size_grp).unwrap_or(&tables::DEFAULT_Y_MODE_CDF[0]));
    }

    if bsl >= 1 && (y_mode == intra::V_PRED || y_mode == intra::H_PRED) {
        let delta_idx = (y_mode as usize).saturating_sub(1);
        ctx.sym.write_symbol_no_update(3, tables::DEFAULT_ANGLE_DELTA_CDF.get(delta_idx).unwrap_or(&tables::DEFAULT_ANGLE_DELTA_CDF[0]));
    }

    if has_chroma {
        if cfl_allowed {
            ctx.sym.write_symbol_no_update(
                uv_mode as usize,
                tables::DEFAULT_UV_MODE_CFL_ALLOWED_CDF.get(y_mode as usize).unwrap_or(&tables::DEFAULT_UV_MODE_CFL_ALLOWED_CDF[0]),
            );
        } else {
            ctx.sym.write_symbol_no_update(
                uv_mode as usize,
                tables::DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF.get(y_mode as usize).unwrap_or(&tables::DEFAULT_UV_MODE_CFL_NOT_ALLOWED_CDF[0]),
            );
        }

        if bsl >= 1 && (uv_mode == intra::V_PRED || uv_mode == intra::H_PRED) {
            let delta_idx = (uv_mode as usize).saturating_sub(1);
            ctx.sym.write_symbol_no_update(3, tables::DEFAULT_ANGLE_DELTA_CDF.get(delta_idx).unwrap_or(&tables::DEFAULT_ANGLE_DELTA_CDF[0]));
        }
    }

    let uv_w4 = (bw4 / 2).max(1);
    let uv_h4 = (bh4 / 2).max(1);
    let max_uv_x4 = ctx.mi_cols >> 1;
    let max_uv_y4 = ctx.mi_rows >> 1;

    if has_residual {
        let luma_tx_sz_ctx = bsl;
        let chroma_tx_sz_ctx = bsl.saturating_sub(1);

        let luma_dc_ctx = get_dc_sign_ctx(c, r, bw4, bh4, ctx.mi_cols, ctx.mi_rows, &ctx.above_dc[0], &ctx.left_dc[0]);
        if dc_level > 0 {
            coeff::encode_dc_residual(ctx.sym, ctx.qindex, 0, luma_tx_sz_ctx, 0, luma_dc_ctx, dc_level, is_neg, y_mode);
        } else {
            coeff::write_txb_skip(ctx.sym, ctx.qindex, luma_tx_sz_ctx, 0, true);
        }

        if has_chroma {
            let u_dc_ctx = get_dc_sign_ctx(c / 2, r / 2, uv_w4, uv_h4, max_uv_x4, max_uv_y4, &ctx.above_dc[1], &ctx.left_dc[1]);
            let v_dc_ctx = get_dc_sign_ctx(c / 2, r / 2, uv_w4, uv_h4, max_uv_x4, max_uv_y4, &ctx.above_dc[2], &ctx.left_dc[2]);

            let u_skip_ctx = get_chroma_txb_skip_ctx(c / 2, r / 2, uv_w4, uv_h4, max_uv_x4, max_uv_y4, &ctx.above_dc[1], &ctx.left_dc[1]);
            let v_skip_ctx = get_chroma_txb_skip_ctx(c / 2, r / 2, uv_w4, uv_h4, max_uv_x4, max_uv_y4, &ctx.above_dc[2], &ctx.left_dc[2]);

            if u_dc_level > 0 {
                coeff::encode_dc_residual(ctx.sym, ctx.qindex, 1, chroma_tx_sz_ctx, u_skip_ctx, u_dc_ctx, u_dc_level, u_is_neg, uv_mode);
            } else {
                coeff::write_txb_skip(ctx.sym, ctx.qindex, chroma_tx_sz_ctx, u_skip_ctx, true);
            }

            if v_dc_level > 0 {
                coeff::encode_dc_residual(ctx.sym, ctx.qindex, 2, chroma_tx_sz_ctx, v_skip_ctx, v_dc_ctx, v_dc_level, v_is_neg, uv_mode);
            } else {
                coeff::write_txb_skip(ctx.sym, ctx.qindex, chroma_tx_sz_ctx, v_skip_ctx, true);
            }
        }
    }

    update_dc_sign(&mut ctx.above_dc[0], &mut ctx.left_dc[0], c, r, bw4, bh4, dc_level, is_neg);
    if has_chroma {
        update_dc_sign(&mut ctx.above_dc[1], &mut ctx.left_dc[1], c / 2, r / 2, uv_w4, uv_h4, u_dc_level, u_is_neg);
        update_dc_sign(&mut ctx.above_dc[2], &mut ctx.left_dc[2], c / 2, r / 2, uv_w4, uv_h4, v_dc_level, v_is_neg);
    }

    let rows = bh4.min(ctx.mi_rows.saturating_sub(r));
    let cols = bw4.min(ctx.mi_cols.saturating_sub(c));
    for dy in 0..rows {
        let row_start = (r + dy) * ctx.mi_cols + c;
        let span = row_start..row_start + cols;
        ctx.grid_y_mode[span.clone()].fill(y_mode);
        ctx.grid_skip[span.clone()].fill(!has_residual);
        ctx.grid_ref_frame[span.clone()].fill(0);
        ctx.grid_bsl[span].fill(bsl as u8);
    }
    ctx.intra_blocks = ctx.intra_blocks.saturating_add(1);
    if bsl == 0 {
        ctx.blocks_4x4 = ctx.blocks_4x4.saturating_add(1);
    }
}

#[allow(clippy::too_many_arguments)]
fn should_split_block(
    y_plane: &[u8],
    y_stride: usize,
    u_plane: &[u8],
    v_plane: &[u8],
    uv_stride: usize,
    x0: usize,
    y0: usize,
    block_w: usize,
    block_h: usize,
    bsl: usize,
    preset: Preset,
    qindex: u8,
) -> bool {
    let target = 1usize << (bsl + 2);
    if bsl == 0 || block_w < target || block_h < target {
        return false;
    }
    if preset == Preset::Speed && bsl < 4 && qindex > 40 {
        return false;
    }
    let y_var = y0
        .checked_mul(y_stride)
        .and_then(|o| o.checked_add(x0))
        .and_then(|idx| y_plane.get(idx..))
        .map(|sub| intra::block_variance(sub, y_stride, block_w, block_h))
        .unwrap_or(0);
    let uw = (block_w / 2).max(1);
    let uh = (block_h / 2).max(1);
    let u_var = (y0 / 2)
        .checked_mul(uv_stride)
        .and_then(|o| o.checked_add(x0 / 2))
        .and_then(|idx| u_plane.get(idx..))
        .map(|sub| intra::block_variance(sub, uv_stride, uw, uh))
        .unwrap_or(0);
    let v_var = (y0 / 2)
        .checked_mul(uv_stride)
        .and_then(|o| o.checked_add(x0 / 2))
        .and_then(|idx| v_plane.get(idx..))
        .map(|sub| intra::block_variance(sub, uv_stride, uw, uh))
        .unwrap_or(0);
    let var_thresh = if qindex <= 16 {
        1
    } else if bsl == 1 {
        if qindex >= 70 {
            u64::MAX
        } else {
            let q_scale = (qindex as u64).max(20);
            match preset {
                Preset::Quality => q_scale * 2,
                Preset::Balanced => q_scale * 3,
                Preset::Speed => u64::MAX,
            }
        }
    } else if bsl == 2 {
        let q_scale = if qindex >= 70 { (qindex as u64).saturating_mul(2) } else { (qindex as u64 / 3).max(8) };
        match preset {
            Preset::Quality => q_scale,
            Preset::Balanced => q_scale * 2,
            Preset::Speed => q_scale * 4,
        }
    } else {
        let base = if qindex >= 70 { 45 } else { 12 };
        match preset {
            Preset::Quality => base / 2,
            Preset::Balanced => base,
            Preset::Speed => base * 4,
        }
    };
    y_var >= var_thresh || u_var >= var_thresh || v_var >= var_thresh
}

#[allow(clippy::too_many_arguments)]
fn should_split_64x64(y_plane: &[u8], stride: usize, x0: usize, y0: usize, block_w: usize, block_h: usize, preset: Preset, qindex: u8) -> bool {
    if preset == Preset::Speed || block_w < 64 || block_h < 64 {
        return false;
    }
    if qindex <= 16 {
        return true;
    }

    let mut means = [0i64; 4];
    for (idx, &(dx, dy)) in [(0, 0), (32, 0), (0, 32), (32, 32)].iter().enumerate() {
        let mut sum = 0i64;
        for y in 0..32 {
            let Some(row_y) = y0.checked_add(dy).and_then(|v| v.checked_add(y)) else { continue };
            let Some(row_x) = x0.checked_add(dx) else { continue };
            let Some(row_off) = row_y.checked_mul(stride).and_then(|o| o.checked_add(row_x)) else { continue };
            let Some(row_end) = row_off.checked_add(32) else { continue };
            if let Some(row) = y_plane.get(row_off..row_end) {
                sum += row.iter().map(|&p| p as i64).sum::<i64>();
            }
        }
        means[idx] = sum / (32 * 32);
    }

    let min_m = means.iter().copied().min().unwrap_or(0);
    let max_m = means.iter().copied().max().unwrap_or(0);
    let range = max_m - min_m;

    let threshold = match preset {
        Preset::Quality => 6,
        Preset::Balanced => 12,
        Preset::Speed => i64::MAX,
    };

    if range >= threshold {
        return true;
    }

    let var_threshold = match preset {
        Preset::Quality => 12,
        Preset::Balanced => 24,
        Preset::Speed => u64::MAX,
    };

    let var = y0
        .checked_mul(stride)
        .and_then(|o| o.checked_add(x0))
        .and_then(|idx| y_plane.get(idx..))
        .map(|sub| intra::block_variance(sub, stride, block_w, block_h))
        .unwrap_or(0);

    var >= var_threshold
}

fn pick_dc_v_h_mode(src: &[u8], src_stride: usize, w: usize, h: usize, above: Option<&[u8]>, left: Option<&[u8]>) -> u8 {
    let dc = intra::get_dc_val(above, left, w, h);
    let mut best_sad = intra::sad_dc(src, src_stride, w, h, dc);
    let mut best_mode = intra::DC_PRED;
    if let Some(ab) = above.filter(|a| a.len() >= w) {
        let sad = intra::sad_v(src, src_stride, w, h, ab);
        if sad.saturating_add(sad / 16) < best_sad {
            best_sad = sad;
            best_mode = intra::V_PRED;
        }
    }
    if let Some(lf) = left.filter(|l| l.len() >= h) {
        let sad = intra::sad_h(src, src_stride, w, h, lf);
        if sad.saturating_add(sad / 16) < best_sad {
            best_mode = intra::H_PRED;
        }
    }
    best_mode
}

fn av1_dc_recon_shift(dc_level: u32, is_negative: bool, dc_q: i64, log2: u32) -> i64 {
    if dc_level == 0 {
        return 0;
    }
    let dq_shift = match log2 {
        5 => 1,
        6 => 2,
        _ => 0,
    };
    let row_shift = match log2 {
        2 => 0,
        3 => 1,
        _ => 2,
    };
    let mag = ((dc_level as i64 * dc_q) & 0xFFFFFF) >> dq_shift;
    let dq = (if is_negative { -mag } else { mag }).clamp(-32768, 32767);
    let round2 = |x: i64, n: u32| -> i64 { if n == 0 { x } else { (x + (1i64 << (n - 1))) >> n } };
    let r1 = round2(dq * 2896, 12);
    let v = round2(r1, row_shift).clamp(-32768, 32767);
    let c1 = round2(v * 2896, 12);
    round2(c1, 4)
}

#[allow(clippy::too_many_arguments)]
fn compute_and_reconstruct_plane_dc(
    src_plane: &[u8],
    src_stride: usize,
    recon_plane: &mut [u8],
    recon_stride: usize,
    x0: usize,
    y0: usize,
    block_w: usize,
    block_h: usize,
    tx_w: usize,
    tx_h: usize,
    mode: u8,
    avail_u: bool,
    avail_l: bool,
    qindex: u8,
    scale: i64,
) -> (u32, bool) {
    let mut src_sum = 0i64;
    let mut count = 0i64;
    for i in 0..block_h {
        let Some(row_off) = y0.checked_add(i).and_then(|y| y.checked_mul(src_stride)).and_then(|o| o.checked_add(x0)) else { continue };
        let Some(row_end) = row_off.checked_add(block_w) else { continue };
        if let Some(row) = src_plane.get(row_off..row_end) {
            for &p in row {
                src_sum += p as i64;
                count += 1;
            }
        }
    }
    if count == 0 {
        return (0, false);
    }
    let src_mean = src_sum / count;

    let pred_mean = match mode {
        intra::V_PRED if avail_u && y0 > 0 => {
            let row_offset = y0.saturating_sub(1).checked_mul(recon_stride).and_then(|o| o.checked_add(x0));
            row_offset
                .and_then(|off| off.checked_add(block_w).and_then(|end| recon_plane.get(off..end)))
                .map(|row| row.iter().map(|&p| p as i64).sum::<i64>() / (block_w.max(1) as i64))
                .unwrap_or(128)
        }
        intra::H_PRED if avail_l && x0 > 0 => {
            let mut left_sum = 0i64;
            for i in 0..block_h {
                let off = y0.checked_add(i).and_then(|y| y.checked_mul(recon_stride)).and_then(|o| o.checked_add(x0.saturating_sub(1)));
                if let Some(&p) = off.and_then(|idx| recon_plane.get(idx)) {
                    left_sum += p as i64;
                }
            }
            left_sum / (block_h.max(1) as i64)
        }
        _ => {
            let mut sum = 0i64;
            let mut n = 0i64;
            if avail_u && y0 > 0 {
                let row_offset = y0.saturating_sub(1).checked_mul(recon_stride).and_then(|o| o.checked_add(x0));
                if let Some(row) = row_offset.and_then(|off| off.checked_add(block_w).and_then(|end| recon_plane.get(off..end))) {
                    let row_sum: i64 = row.iter().map(|&p| p as i64).sum();
                    let last = row.last().copied().unwrap_or(128) as i64;
                    let ext = tx_w.saturating_sub(block_w) as i64;
                    sum += row_sum + last * ext;
                    n += tx_w as i64;
                }
            }
            if avail_l && x0 > 0 {
                let mut last = 128i64;
                for i in 0..block_h {
                    let off = y0.checked_add(i).and_then(|y| y.checked_mul(recon_stride)).and_then(|o| o.checked_add(x0.saturating_sub(1)));
                    if let Some(&p) = off.and_then(|idx| recon_plane.get(idx)) {
                        last = p as i64;
                        sum += last;
                    }
                }
                let ext = tx_h.saturating_sub(block_h) as i64;
                sum += last * ext;
                n += tx_h as i64;
            }
            if n > 0 { (sum + (n / 2)) / n } else { 128 }
        }
    };

    let diff = src_mean - pred_mean;
    let dc_q = tables::DC_QLOOKUP_8BIT[qindex as usize] as i64;
    let approx_level = ((diff.abs().saturating_mul(scale) + (dc_q / 2)) / dc_q.max(1)) as u32;
    let is_negative = diff < 0;
    let log2 = tx_w.trailing_zeros();
    let mut dc_level = approx_level;
    let mut recon_shift = av1_dc_recon_shift(dc_level, is_negative, dc_q, log2);
    let mut best_err = (diff - recon_shift).abs();
    for cand in [approx_level.saturating_sub(1), approx_level.saturating_add(1)] {
        let sh = av1_dc_recon_shift(cand, is_negative, dc_q, log2);
        let err = (diff - sh).abs();
        if err < best_err || (err == best_err && cand < dc_level) {
            best_err = err;
            dc_level = cand;
            recon_shift = sh;
        }
    }
    if dc_level == 0 {
        recon_shift = 0;
    }

    let recon_val = (pred_mean + recon_shift).clamp(0, 255) as u8;
    match mode {
        intra::V_PRED if avail_u && y0 > 0 => {
            let above_off = y0.saturating_sub(1).checked_mul(recon_stride).and_then(|o| o.checked_add(x0));
            if let Some(above_slice) = above_off.and_then(|off| off.checked_add(block_w).and_then(|end| recon_plane.get(off..end))) {
                let mut temp = [0u8; 64];
                let len = block_w.min(64);
                for (dst, &src) in temp[..len].iter_mut().zip(&above_slice[..len]) {
                    *dst = (src as i64 + recon_shift).clamp(0, 255) as u8;
                }
                for i in 0..block_h {
                    let row_off = y0.checked_add(i).and_then(|y| y.checked_mul(recon_stride)).and_then(|o| o.checked_add(x0));
                    if let Some(row) = row_off.and_then(|off| off.checked_add(len).and_then(|end| recon_plane.get_mut(off..end))) {
                        row.copy_from_slice(&temp[..len]);
                    }
                }
            }
        }
        intra::H_PRED if avail_l && x0 > 0 => {
            for i in 0..block_h {
                let left_off = y0.checked_add(i).and_then(|y| y.checked_mul(recon_stride)).and_then(|o| o.checked_add(x0.saturating_sub(1)));
                let left_val = left_off.and_then(|idx| recon_plane.get(idx)).copied().unwrap_or(128) as i64;
                let val = (left_val + recon_shift).clamp(0, 255) as u8;
                let row_off = y0.checked_add(i).and_then(|y| y.checked_mul(recon_stride)).and_then(|o| o.checked_add(x0));
                if let Some(row) = row_off.and_then(|off| off.checked_add(block_w).and_then(|end| recon_plane.get_mut(off..end))) {
                    row.fill(val);
                }
            }
        }
        _ => {
            for i in 0..block_h {
                let row_off = y0.checked_add(i).and_then(|y| y.checked_mul(recon_stride)).and_then(|o| o.checked_add(x0));
                if let Some(row) = row_off.and_then(|off| off.checked_add(block_w).and_then(|end| recon_plane.get_mut(off..end))) {
                    row.fill(recon_val);
                }
            }
        }
    }

    (dc_level, is_negative)
}

/// Compute Mean Squared Error (MSE) between two byte planes of identical length.
pub fn compute_mse(src: &[u8], dst: &[u8]) -> Option<f64> {
    if src.is_empty() || src.len() != dst.len() {
        return None;
    }
    let sum_sq_err: u64 = src.iter().zip(dst.iter()).map(|(&a, &b)| (a as i64 - b as i64).pow(2) as u64).sum();
    Some(sum_sq_err as f64 / src.len() as f64)
}

/// Compute Peak Signal-to-Noise Ratio (PSNR in dB) between two 8-bit byte planes.
///
/// Returns 100.0 dB when planes are identical (zero error).
pub fn compute_psnr(src: &[u8], dst: &[u8]) -> Option<f64> {
    let mse = compute_mse(src, dst)?;
    if mse <= 1e-10 {
        return Some(100.0);
    }
    Some(10.0 * (255.0 * 255.0 / mse).log10())
}

/// Compute PSNR comparing an 8-bit reference plane with 16-bit decoder picture samples.
pub fn compute_psnr_u16(src: &[u8], dst: &[u16]) -> Option<f64> {
    if src.is_empty() || src.len() != dst.len() {
        return None;
    }
    let sum_sq_err: u64 = src.iter().zip(dst.iter()).map(|(&a, &b)| (a as i64 - b as i64).pow(2) as u64).sum();
    let mse = sum_sq_err as f64 / src.len() as f64;
    if mse <= 1e-10 {
        return Some(100.0);
    }
    Some(10.0 * (255.0 * 255.0 / mse).log10())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_lifecycle_and_decode() {
        let mut enc = Encoder::new(EncoderConfig::new(64, 64, 25, 1)).unwrap();
        let y_plane = vec![128u8; 64 * 64];
        let uv_plane = vec![128u8; 32 * 32];
        let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: 64, uv_stride: 32 };

        let packet1 = enc.encode(&frame, 0).unwrap();
        assert!(!packet1.data.is_empty());
        assert!(packet1.keyframe);
        let packet2 = enc.encode(&frame, 1).unwrap();
        assert!(!packet2.data.is_empty());

        let mut dec = craft_video::av1::decode::Decoder::new();
        let mut pics = dec.decode(&packet1.data).unwrap();
        pics.extend(dec.flush());
        assert_eq!(pics.len(), 1);
        assert_eq!(pics[0].width, 64);
        assert_eq!(pics[0].height, 64);
        assert_eq!(pics[0].planes[0][0], 128);

        let flush_res = enc.flush().unwrap();
        assert!(flush_res.is_empty());
    }

    #[test]
    fn test_encoder_rate_control_modes() {
        for rc in [
            RateControl::ConstantQuality(20.0),
            RateControl::ConstantQuality(50.0),
            RateControl::Qindex(60),
            RateControl::Qindex(180),
            RateControl::Cbr { kbps: 2000 },
            RateControl::Vbr { target_kbps: 2000, max_kbps: 4000 },
        ] {
            let mut cfg = EncoderConfig::new(64, 64, 30, 1);
            cfg.rate_control = Some(rc);
            cfg.spatial_aq = true;
            let mut enc = Encoder::new(cfg).unwrap();
            let y_plane = vec![128u8; 64 * 64];
            let uv_plane = vec![128u8; 32 * 32];
            let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: 64, uv_stride: 32 };

            let p = enc.encode(&frame, 0).unwrap();
            let mut dec = craft_video::av1::decode::Decoder::new();
            let mut pics = dec.decode(&p.data).unwrap();
            pics.extend(dec.flush());
            assert_eq!(pics.len(), 1);
        }
    }

    #[test]
    fn test_encoder_smooth_and_paeth_modes() {
        let (w, h) = (128, 128);
        let mut enc = Encoder::new(EncoderConfig::new(w, h, 24, 1)).unwrap();
        let mut y_plane = vec![0u8; (w * h) as usize];
        // 2D diagonal ramp to exercise smooth and paeth prediction
        for y in 0..h {
            for x in 0..w {
                y_plane[(y * w + x) as usize] = ((x * 128 / w) + (y * 128 / h)) as u8;
            }
        }
        let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
        let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

        let packet = enc.encode(&frame, 0).unwrap();
        let mut dec = craft_video::av1::decode::Decoder::new();
        let mut pics = dec.decode(&packet.data).unwrap();
        pics.extend(dec.flush());
        assert_eq!(pics.len(), 1);
        assert_eq!(pics[0].width, w);
        assert_eq!(pics[0].height, h);
    }

    #[test]
    fn test_encoder_gradient_directional_modes() {
        let (w, h) = (128, 128);
        let mut enc = Encoder::new(EncoderConfig::new(w, h, 24, 1)).unwrap();
        let mut y_plane = vec![0u8; (w * h) as usize];
        // Vertical gradient
        for y in 0..h {
            for x in 0..w {
                y_plane[(y * w + x) as usize] = (y * 2).min(255) as u8;
            }
        }
        let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
        let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

        let packet = enc.encode(&frame, 0).unwrap();
        let mut dec = craft_video::av1::decode::Decoder::new();
        let mut pics = dec.decode(&packet.data).unwrap();
        pics.extend(dec.flush());
        assert_eq!(pics.len(), 1);
        assert_eq!(pics[0].width, w);
        assert_eq!(pics[0].height, h);
    }

    #[test]
    fn test_encoder_multi_superblock_decode() {
        let mut enc = Encoder::new(EncoderConfig::new(128, 128, 30, 1)).unwrap();
        let y_plane = vec![128u8; 128 * 128];
        let uv_plane = vec![128u8; 64 * 64];
        let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: 128, uv_stride: 64 };

        let packet = enc.encode(&frame, 0).unwrap();
        let mut dec = craft_video::av1::decode::Decoder::new();
        let mut pics = dec.decode(&packet.data).unwrap();
        pics.extend(dec.flush());
        assert_eq!(pics.len(), 1);
        assert_eq!(pics[0].width, 128);
        assert_eq!(pics[0].height, 128);
        assert_eq!(pics[0].planes[0][0], 128);
    }

    #[test]
    fn test_encoder_non_multiple_of_64() {
        let mut enc = Encoder::new(EncoderConfig::new(320, 240, 24, 1)).unwrap();
        let y_plane = vec![128u8; 320 * 240];
        let uv_plane = vec![128u8; 160 * 120];
        let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: 320, uv_stride: 160 };

        let packet = enc.encode(&frame, 0).unwrap();
        let mut dec = craft_video::av1::decode::Decoder::new();
        let mut pics = dec.decode(&packet.data).unwrap();
        pics.extend(dec.flush());
        assert_eq!(pics.len(), 1);
        assert_eq!(pics[0].width, 320);
        assert_eq!(pics[0].height, 240);
    }

    #[test]
    fn test_encoder_720p_and_1080p() {
        for (w, h) in [(1280, 720), (1920, 1080)] {
            let mut enc = Encoder::new(EncoderConfig::new(w, h, 24, 1)).unwrap();
            let y_plane = vec![128u8; (w * h) as usize];
            let uv_plane = vec![128u8; ((w / 2) * (h / 2)) as usize];
            let frame = YuvFrame { y: &y_plane, u: &uv_plane, v: &uv_plane, y_stride: w as usize, uv_stride: (w / 2) as usize };

            let packet = enc.encode(&frame, 0).unwrap();
            let mut dec = craft_video::av1::decode::Decoder::new();
            let mut pics = dec.decode(&packet.data).unwrap();
            pics.extend(dec.flush());
            assert_eq!(pics.len(), 1);
            assert_eq!(pics[0].width, w);
            assert_eq!(pics[0].height, h);
        }
    }

    #[test]
    fn hostile_configuration_returns_errors_without_panicking() {
        for (w, h, n, d) in [(0, 2, 25, 1), (2, 0, 25, 1), (3, 2, 25, 1), (131072, 2, 25, 1), (u32::MAX, u32::MAX, 25, 1), (64, 64, 0, 1), (64, 64, 25, 0)] {
            let result = std::panic::catch_unwind(|| Encoder::new(EncoderConfig::new(w, h, n, d)));
            assert!(matches!(result, Ok(Err(Error::Invalid(_)))));
        }
        let mut c = EncoderConfig::new(64, 64, 25, 1);
        c.key_int = 0;
        assert!(matches!(Encoder::new(c), Err(Error::Invalid(_))));

        // Hostile rate control configs
        let mut c_cbr_zero = EncoderConfig::new(64, 64, 25, 1);
        c_cbr_zero.rate_control = Some(RateControl::Cbr { kbps: 0 });
        assert!(matches!(Encoder::new(c_cbr_zero), Err(Error::Invalid(_))));

        let mut c_vbr_zero = EncoderConfig::new(64, 64, 25, 1);
        c_vbr_zero.rate_control = Some(RateControl::Vbr { target_kbps: 0, max_kbps: 1000 });
        assert!(matches!(Encoder::new(c_vbr_zero), Err(Error::Invalid(_))));

        let mut c_vbr_inv = EncoderConfig::new(64, 64, 25, 1);
        c_vbr_inv.rate_control = Some(RateControl::Vbr { target_kbps: 2000, max_kbps: 1000 });
        assert!(matches!(Encoder::new(c_vbr_inv), Err(Error::Invalid(_))));
    }

    #[test]
    fn test_hostile_buffers_and_strides_do_not_panic() {
        let mut enc = Encoder::new(EncoderConfig::new(64, 64, 24, 1)).unwrap();
        let valid_y = vec![128u8; 64 * 64];
        let valid_uv = vec![128u8; 32 * 32];

        // Empty buffers must return BufferTooSmall without panicking
        let empty_frame = YuvFrame { y: &[], u: &[], v: &[], y_stride: 64, uv_stride: 32 };
        assert!(matches!(enc.encode(&empty_frame, 0), Err(Error::BufferTooSmall)));

        // Undersized Y buffer
        let short_y = YuvFrame { y: &valid_y[..10], u: &valid_uv, v: &valid_uv, y_stride: 64, uv_stride: 32 };
        assert!(matches!(enc.encode(&short_y, 0), Err(Error::BufferTooSmall)));

        // Undersized UV buffer
        let short_uv = YuvFrame { y: &valid_y, u: &valid_uv[..5], v: &valid_uv, y_stride: 64, uv_stride: 32 };
        assert!(matches!(enc.encode(&short_uv, 0), Err(Error::BufferTooSmall)));

        // Hostile strides: usize::MAX must return Err without overflow panic
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let max_stride_frame = YuvFrame { y: &valid_y, u: &valid_uv, v: &valid_uv, y_stride: usize::MAX, uv_stride: 32 };
            enc.encode(&max_stride_frame, 0)
        }));
        assert!(res.is_ok());
        assert!(res.unwrap().is_err());

        // Stride smaller than width
        let narrow_stride_frame = YuvFrame { y: &valid_y, u: &valid_uv, v: &valid_uv, y_stride: 32, uv_stride: 32 };
        assert!(matches!(enc.encode(&narrow_stride_frame, 0), Err(Error::Invalid(_))));

        let narrow_uv_stride_frame = YuvFrame { y: &valid_y, u: &valid_uv, v: &valid_uv, y_stride: 64, uv_stride: 16 };
        assert!(matches!(enc.encode(&narrow_uv_stride_frame, 0), Err(Error::Invalid(_))));
    }

    #[test]
    fn test_hostile_rate_control_extremes_do_not_panic() {
        let mut cfg = EncoderConfig::new(64, 64, 1, u32::MAX);
        cfg.rate_control = Some(RateControl::Cbr { kbps: u32::MAX });
        let mut enc = Encoder::new(cfg).unwrap();

        let y = vec![128u8; 64 * 64];
        let uv = vec![128u8; 32 * 32];
        let frame = YuvFrame { y: &y, u: &uv, v: &uv, y_stride: 64, uv_stride: 32 };

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = enc.encode(&frame, 0);
            let _ = enc.encode(&frame, 1);
        }));
        assert!(res.is_ok());

        let mut vbr_cfg = EncoderConfig::new(64, 64, 1, u32::MAX);
        vbr_cfg.rate_control = Some(RateControl::Vbr { target_kbps: u32::MAX, max_kbps: u32::MAX });
        let mut vbr_enc = Encoder::new(vbr_cfg).unwrap();
        let res_vbr = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = vbr_enc.encode(&frame, 0);
            let _ = vbr_enc.encode(&frame, 1);
        }));
        assert!(res_vbr.is_ok());
    }

    #[test]
    fn test_quality_metrics_psnr_and_mse() {
        let p1 = vec![128u8; 100];
        let p2 = vec![128u8; 100];
        assert_eq!(compute_mse(&p1, &p2), Some(0.0));
        assert_eq!(compute_psnr(&p1, &p2), Some(100.0));

        let p3 = vec![129u8; 100];
        assert_eq!(compute_mse(&p1, &p3), Some(1.0));
        let psnr = compute_psnr(&p1, &p3).unwrap();
        // 10 * log10(255^2 / 1) ≈ 48.13 dB
        assert!((psnr - 48.13).abs() < 0.1);

        assert_eq!(compute_mse(&[], &[]), None);
        assert_eq!(compute_mse(&[1], &[1, 2]), None);
    }

    #[test]
    fn enforces_4k_level_5_0_single_tile_dimensions() {
        for (w, h) in [(5120, 2880), (7680, 4320), (4098, 2160)] {
            assert!(
                matches!(Encoder::new(EncoderConfig::new(w, h, 24, 1)), Err(Error::Invalid(_))),
                "{w}x{h} must be rejected under single-tile Level 5.0 limits"
            );
        }
        for (w, h) in [(4096, 2160), (3840, 2160)] {
            assert!(Encoder::new(EncoderConfig::new(w, h, 24, 1)).is_ok(), "{w}x{h} must be accepted");
        }
    }

    #[test]
    fn spatial_aq_never_raises_low_crf_qindex_on_low_variance() {
        for q in 1..=16u8 {
            for var in [0u64, 50, 150, 249] {
                let adj = apply_spatial_aq(q, var);
                assert!(adj >= 1 && adj <= q, "q={q} var={var} raised qindex to {adj}");
            }
        }
    }

    #[test]
    fn selects_annex_a_level_from_dimensions_and_rational_frame_rate() {
        assert_eq!(obu::select_seq_level_idx(1920, 1080, 30, 1), Some(8)); // Level 4.0
        assert_eq!(obu::select_seq_level_idx(1920, 1080, 60, 1), Some(9)); // Level 4.1 (124,416,000 <= 141,557,760)
        assert_eq!(obu::select_seq_level_idx(1920, 1080, 60000, 1001), Some(9)); // Level 4.1
        assert_eq!(obu::select_seq_level_idx(1920, 1080, 120, 1), Some(12)); // Level 5.0 (248,832,000 <= 267,386,880)
        assert_eq!(obu::select_seq_level_idx(3840, 2160, 30, 1), Some(12)); // Level 5.0
        assert_eq!(obu::select_seq_level_idx(3840, 2160, 60, 1), Some(13)); // Level 5.1 (497,664,000 <= 534,773,760)
        assert_eq!(obu::select_seq_level_idx(3840, 2160, 60000, 1001), Some(13)); // Level 5.1
        assert_eq!(obu::select_seq_level_idx(3840, 2160, 120, 1), Some(14)); // Level 5.2 (995,328,000 <= 1,069,547,520)
        assert_eq!(obu::select_seq_level_idx(3840, 2160, 240, 1), None);
        assert_eq!(obu::select_seq_level_idx(3840, 2160, u32::MAX, 1), None);

        for (w, h, num, den, expected_level) in [(1920, 1080, 60, 1, 9u8), (3840, 2160, 60, 1, 13u8)] {
            let enc = Encoder::new(EncoderConfig::new(w, h, num, den)).unwrap();
            assert_eq!(enc.seq_level_idx(), expected_level);
            let seq_obu = enc.sequence_header_obu();
            let payload_start = if (seq_obu[1] & 0x80) != 0 { 3 } else { 2 };
            // Parse level from seq_level_idx[0]: bits 24..29 (top 5 bits of payload[3])
            let payload = &seq_obu[payload_start..];
            let level = (payload[3] >> 3) & 0x1F;
            assert_eq!(level, expected_level, "{w}x{h}@{num}/{den} header level mismatch");
        }

        assert!(matches!(Encoder::new(EncoderConfig::new(3840, 2160, 240, 1)), Err(Error::Invalid(_))));
    }
}
