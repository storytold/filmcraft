//! RFC 9924 APV frame and tile decoder.

use filmcraft_bitstream::BitReader;

use crate::dct::{scale_and_idct8x8, scale_and_idct8x8_dc_only};
use crate::header::{
    FrameHeader, PBU_ALPHA_FRAME, PBU_METADATA, PBU_NON_PRIMARY_FRAME, PBU_PREVIEW_FRAME, PBU_PRIMARY_FRAME, parse_au_pbus, parse_frame_header,
    parse_metadata_pbu, read_byte_alignment,
};
use crate::tables::ZIGZAG_8X8;
use crate::{ChromaFormat, ContentLightLevel, Error, Frame, MasteringDisplay, Result};

/// Options for [`decode_frame_with`] and [`decode_frame_into`].
#[derive(Debug, Clone)]
pub struct DecodeOptions {
    /// Output sample bit depth (`8..=16`). `None` keeps the stream's coded bit depth (`10..=16`).
    pub bit_depth: Option<u8>,
    /// Decode tiles in parallel when the `threads` feature is enabled.
    pub threads: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self { bit_depth: None, threads: true }
    }
}

/// Parse RFC 9924 §7.1.4 variable-length codeword (`h(v)`).
#[inline]
pub(crate) fn read_vlc(r: &mut BitReader, k_param: u32) -> Result<u32> {
    let mut symbol_value: u64 = 0;
    let mut k = k_param;
    let parse_exp_golomb = if r.read_bit().map_err(|_| Error::Truncated)? {
        false
    } else if !r.read_bit().map_err(|_| Error::Truncated)? {
        symbol_value += 1u64 << k;
        false
    } else {
        symbol_value += 2u64 << k;
        true
    };
    if parse_exp_golomb {
        loop {
            if r.read_bit().map_err(|_| Error::Truncated)? {
                break;
            }
            symbol_value += 1u64 << k;
            k += 1;
            if k > 28 {
                return Err(Error::Invalid("VLC codeword exceeds maximum length"));
            }
        }
    }
    if k > 0 {
        symbol_value += r.read_bits(k).map_err(|_| Error::Truncated)? as u64;
    }
    u32::try_from(symbol_value).map_err(|_| Error::Invalid("VLC symbol overflow"))
}

/// Probe the primary frame header of an Access Unit without decoding its tiles.
pub fn probe(data: &[u8]) -> Result<FrameHeader> {
    let pbus = parse_au_pbus(data)?;
    for pbu in &pbus {
        if matches!(pbu.pbu_type, PBU_PRIMARY_FRAME | PBU_NON_PRIMARY_FRAME | PBU_PREVIEW_FRAME)
            && let Some(hdr) = parse_frame_header(pbu.payload)?
        {
            return Ok(hdr);
        }
    }
    Err(Error::Invalid("no decodable frame PBU in access unit"))
}

/// Decode one APV Access Unit with default options.
pub fn decode_frame(data: &[u8]) -> Result<Frame> {
    decode_frame_with(data, &DecodeOptions::default())
}

/// Decode one APV Access Unit with custom [`DecodeOptions`].
pub fn decode_frame_with(data: &[u8], opts: &DecodeOptions) -> Result<Frame> {
    let mut out = Frame::new(2, 2, ChromaFormat::Yuv422, 10, false);
    decode_frame_into(data, opts, &mut out)?;
    Ok(out)
}

/// Decode one APV Access Unit into `out`, reusing its plane allocations when possible.
pub fn decode_frame_into(data: &[u8], opts: &DecodeOptions, out: &mut Frame) -> Result<FrameHeader> {
    if let Some(d) = opts.bit_depth
        && !(8..=16).contains(&d)
    {
        return Err(Error::Invalid("requested output bit_depth must be in 8..=16"));
    }

    let pbus = parse_au_pbus(data)?;
    let mut primary: Option<(u16, FrameHeader, &[u8])> = None;
    let mut alpha_pbu: Option<(FrameHeader, &[u8])> = None;
    let mut mdcv: Option<MasteringDisplay> = None;
    let mut cll: Option<ContentLightLevel> = None;

    for pbu in &pbus {
        match pbu.pbu_type {
            PBU_PRIMARY_FRAME => {
                if primary.is_none()
                    && let Some(hdr) = parse_frame_header(pbu.payload)?
                {
                    primary = Some((pbu.group_id, hdr, pbu.payload));
                }
            }
            PBU_NON_PRIMARY_FRAME | PBU_PREVIEW_FRAME => {
                if primary.is_none()
                    && let Some(hdr) = parse_frame_header(pbu.payload)?
                {
                    primary = Some((pbu.group_id, hdr, pbu.payload));
                }
            }
            PBU_ALPHA_FRAME => {
                if alpha_pbu.is_none()
                    && let Some(hdr) = parse_frame_header(pbu.payload)?
                {
                    alpha_pbu = Some((hdr, pbu.payload));
                }
            }
            PBU_METADATA => {
                let _ = parse_metadata_pbu(pbu.payload, &mut mdcv, &mut cll);
            }
            _ => {}
        }
    }

    let (_group_id, hdr, payload) = primary.ok_or(Error::Invalid("no primary frame PBU in access unit"))?;
    let out_depth = opts.bit_depth.unwrap_or(hdr.bit_depth);

    decode_pbu_planes(&hdr, payload, out_depth, opts.threads, out)?;
    out.mastering_display = mdcv;
    out.content_light = cll;

    // If the primary frame has no 4th component and an auxiliary alpha frame PBU (type 27) is present
    // with matching dimensions, decode its luma plane as the alpha plane.
    if out.alpha.is_none()
        && let Some((ahdr, apayload)) = alpha_pbu
        && ahdr.width == hdr.width
        && ahdr.height == hdr.height
    {
        let mut aframe = Frame::new(ahdr.width, ahdr.height, ahdr.chroma, out_depth, false);
        if decode_pbu_planes(&ahdr, apayload, out_depth, opts.threads, &mut aframe).is_ok() {
            out.alpha = Some(aframe.y);
        }
    }

    Ok(hdr)
}

#[derive(Clone)]
struct DecodedTile {
    x0: u32,
    y0: u32,
    w_luma: u32,
    h_luma: u32,
    /// Decoded pixels for each component `c` in `0..num_comps`, packed with stride `w_luma / sub_w`.
    comps: [Vec<u16>; 4],
}

fn decode_pbu_planes(hdr: &FrameHeader, payload: &[u8], out_depth: u8, threads: bool, out: &mut Frame) -> Result<()> {
    let num_tiles = hdr.num_tiles();
    let mut pos = hdr.header_bytes;
    let mut tile_slices: Vec<(usize, &[u8])> = Vec::with_capacity(num_tiles);

    for tile_idx in 0..num_tiles {
        let sz_bytes = payload.get(pos..pos + 4).ok_or(Error::Truncated)?;
        let tile_size = u32::from_be_bytes([sz_bytes[0], sz_bytes[1], sz_bytes[2], sz_bytes[3]]) as usize;
        if tile_size == 0 {
            return Err(Error::Invalid("tile_size cannot be 0"));
        }
        if let Some(fh_sizes) = &hdr.tile_sizes_in_fh
            && fh_sizes.get(tile_idx).copied() != Some(tile_size as u32)
        {
            return Err(Error::Invalid("tile_size does not match tile_size_in_fh"));
        }
        pos += 4;
        let tile_end = pos.checked_add(tile_size).ok_or(Error::Truncated)?;
        let tile_bytes = payload.get(pos..tile_end).ok_or(Error::Truncated)?;
        pos = tile_end;
        tile_slices.push((tile_idx, tile_bytes));
    }

    // Verify any trailing filler bytes in the frame PBU are 0xFF (RFC 9924 §5.3.4 & §5.3.11)
    if payload.get(pos..).is_some_and(|rest| rest.iter().any(|&b| b != 0xFF)) {
        return Err(Error::Invalid("non-0xFF filler byte at end of frame PBU"));
    }

    #[cfg(feature = "threads")]
    let decoded_tiles: Result<Vec<DecodedTile>> = if threads && tile_slices.len() > 1 {
        use rayon::prelude::*;
        tile_slices.into_par_iter().map(|(idx, bytes)| decode_single_tile(hdr, idx, bytes, out_depth, false)).collect()
    } else {
        tile_slices.into_iter().map(|(idx, bytes)| decode_single_tile(hdr, idx, bytes, out_depth, threads)).collect()
    };

    #[cfg(not(feature = "threads"))]
    let decoded_tiles: Result<Vec<DecodedTile>> = {
        let _ = threads;
        tile_slices.into_iter().map(|(idx, bytes)| decode_single_tile(hdr, idx, bytes, out_depth, false)).collect()
    };

    let mut decoded_tiles = decoded_tiles?;

    // Prepare output frame metadata
    let w = hdr.width as usize;
    let h = hdr.height as usize;
    let cw = hdr.chroma.chroma_width(hdr.width) as usize;
    let ch = hdr.chroma.chroma_height(hdr.height) as usize;
    let n_luma = w.checked_mul(h).ok_or(Error::Invalid("frame luma size overflow"))?;
    let n_chroma = cw.checked_mul(ch).ok_or(Error::Invalid("frame chroma size overflow"))?;

    out.width = hdr.width;
    out.height = hdr.height;
    out.chroma = hdr.chroma;
    out.bit_depth = out_depth;
    out.color = hdr.color;
    out.profile_idc = hdr.profile_idc;
    out.level_idc = hdr.level_idc;
    out.band_idc = hdr.band_idc;

    // Zero-copy fast path when a single tile covers the exact frame dimensions without macroblock padding.
    if decoded_tiles.len() == 1 && decoded_tiles[0].w_luma == hdr.width && decoded_tiles[0].h_luma == hdr.height {
        let [y, cb, cr, a] = std::mem::take(&mut decoded_tiles[0].comps);
        out.y = y;
        if hdr.chroma.num_comps() >= 3 {
            out.cb = cb;
            out.cr = cr;
        } else {
            out.cb.clear();
            out.cr.clear();
        }
        out.alpha = if hdr.chroma == ChromaFormat::Yuv4444 { Some(a) } else { None };
        return Ok(());
    }

    out.y.resize(n_luma, 0);
    out.cb.resize(n_chroma, 0);
    out.cr.resize(n_chroma, 0);
    if hdr.chroma == ChromaFormat::Yuv4444 {
        let a = out.alpha.get_or_insert_with(Vec::new);
        a.resize(n_luma, 0);
    } else {
        out.alpha = None;
    }

    // Copy each tile's reconstructed samples into the cropped output planes (RFC 9924 §6)
    let sub_w = hdr.chroma.sub_width_c() as usize;
    let sub_h = hdr.chroma.sub_height_c() as usize;

    for tile in &decoded_tiles {
        copy_tile_comp(&tile.comps[0], tile.w_luma as usize, tile.h_luma as usize, tile.x0 as usize, tile.y0 as usize, &mut out.y, w, h);
        if hdr.chroma.num_comps() >= 3 {
            let tw_c = (tile.w_luma as usize) / sub_w;
            let th_c = (tile.h_luma as usize) / sub_h;
            let tx_c = (tile.x0 as usize) / sub_w;
            let ty_c = (tile.y0 as usize) / sub_h;
            copy_tile_comp(&tile.comps[1], tw_c, th_c, tx_c, ty_c, &mut out.cb, cw, ch);
            copy_tile_comp(&tile.comps[2], tw_c, th_c, tx_c, ty_c, &mut out.cr, cw, ch);
        }
        if hdr.chroma == ChromaFormat::Yuv4444
            && let Some(alpha) = out.alpha.as_mut()
        {
            copy_tile_comp(&tile.comps[3], tile.w_luma as usize, tile.h_luma as usize, tile.x0 as usize, tile.y0 as usize, alpha, w, h);
        }
    }

    Ok(())
}

fn copy_tile_comp(src: &[u16], tw: usize, th: usize, tx: usize, ty: usize, dst: &mut [u16], fw: usize, fh: usize) {
    if tx >= fw || ty >= fh {
        return;
    }
    let copy_w = tw.min(fw - tx);
    let copy_h = th.min(fh - ty);
    for r in 0..copy_h {
        let s_off = r * tw;
        let d_off = (ty + r) * fw + tx;
        dst[d_off..d_off + copy_w].copy_from_slice(&src[s_off..s_off + copy_w]);
    }
}

fn decode_single_tile(hdr: &FrameHeader, tile_idx: usize, tile_bytes: &[u8], out_depth: u8, par_comps: bool) -> Result<DecodedTile> {
    let num_comps = hdr.chroma.num_comps();
    let min_hdr_size = 5 + 5 * num_comps;
    if tile_bytes.len() < min_hdr_size {
        return Err(Error::Truncated);
    }

    // tile_header(tileIdx) (RFC 9924 §5.3.13)
    let tile_header_size = u16::from_be_bytes([tile_bytes[0], tile_bytes[1]]) as usize;
    let tile_index = u16::from_be_bytes([tile_bytes[2], tile_bytes[3]]) as usize;
    if tile_index != tile_idx {
        return Err(Error::CorruptTile(tile_idx));
    }
    if tile_header_size < min_hdr_size || tile_header_size > tile_bytes.len() {
        return Err(Error::CorruptTile(tile_idx));
    }

    let mut tile_data_size = [0usize; 4];
    for c in 0..num_comps {
        let off = 4 + c * 4;
        let sz = u32::from_be_bytes([tile_bytes[off], tile_bytes[off + 1], tile_bytes[off + 2], tile_bytes[off + 3]]) as usize;
        if sz == 0 {
            return Err(Error::CorruptTile(tile_idx));
        }
        tile_data_size[c] = sz;
    }

    let max_tile_qp = (51 + hdr.qp_bd_offset()) as u8;
    let mut tile_qp = [0u8; 4];
    for c in 0..num_comps {
        let qp = tile_bytes[4 + num_comps * 4 + c];
        if qp > max_tile_qp {
            return Err(Error::CorruptTile(tile_idx));
        }
        tile_qp[c] = qp;
    }

    let reserved_zero_8bits = tile_bytes[4 + num_comps * 5];
    if reserved_zero_8bits != 0 {
        return Err(Error::CorruptTile(tile_idx));
    }

    let tx = tile_idx % (hdr.tile_cols as usize);
    let ty = tile_idx / (hdr.tile_cols as usize);
    let x0 = hdr.col_starts[tx];
    let x1 = hdr.col_starts[tx + 1];
    let y0 = hdr.row_starts[ty];
    let y1 = hdr.row_starts[ty + 1];
    let w_luma = x1.saturating_sub(x0);
    let h_luma = y1.saturating_sub(y0);

    let mut comp_off = tile_header_size;
    let mut comp_slices: Vec<(usize, &[u8])> = Vec::with_capacity(num_comps);
    for c in 0..num_comps {
        let comp_end = comp_off.checked_add(tile_data_size[c]).ok_or(Error::CorruptTile(tile_idx))?;
        let comp_bytes = tile_bytes.get(comp_off..comp_end).ok_or(Error::CorruptTile(tile_idx))?;
        comp_off = comp_end;
        comp_slices.push((c, comp_bytes));
    }

    #[cfg(feature = "threads")]
    let decoded_comps: Result<Vec<Vec<u16>>> = if par_comps && comp_slices.len() > 1 {
        use rayon::prelude::*;
        comp_slices.into_par_iter().map(|(c, comp_bytes)| decode_tile_comp(hdr, tile_idx, c, comp_bytes, tile_qp[c], w_luma, h_luma, out_depth)).collect()
    } else {
        comp_slices.into_iter().map(|(c, comp_bytes)| decode_tile_comp(hdr, tile_idx, c, comp_bytes, tile_qp[c], w_luma, h_luma, out_depth)).collect()
    };

    #[cfg(not(feature = "threads"))]
    let decoded_comps: Result<Vec<Vec<u16>>> = {
        let _ = par_comps;
        comp_slices.into_iter().map(|(c, comp_bytes)| decode_tile_comp(hdr, tile_idx, c, comp_bytes, tile_qp[c], w_luma, h_luma, out_depth)).collect()
    };

    let mut comps: [Vec<u16>; 4] = Default::default();
    for (c, buf) in decoded_comps?.into_iter().enumerate() {
        comps[c] = buf;
    }

    Ok(DecodedTile { x0, y0, w_luma, h_luma, comps })
}

#[allow(clippy::too_many_arguments)]
fn decode_tile_comp(hdr: &FrameHeader, tile_idx: usize, c: usize, comp_bytes: &[u8], qp: u8, w_luma: u32, h_luma: u32, out_depth: u8) -> Result<Vec<u16>> {
    let num_mb_cols = w_luma / 16;
    let num_mb_rows = h_luma / 16;
    let sub_w = if c == 0 { 1 } else { hdr.chroma.sub_width_c() };
    let sub_h = if c == 0 { 1 } else { hdr.chroma.sub_height_c() };
    let tw_c = (w_luma / sub_w) as usize;
    let th_c = (h_luma / sub_h) as usize;
    let blk_w = (16 / sub_w) as usize;
    let blk_h = (16 / sub_h) as usize;
    let blocks_x = blk_w / 8;
    let blocks_y = blk_h / 8;

    // Every coded 8x8 block needs at least one bit for its DC code. Validate
    // that the component can describe every required block before allocating
    // its complete reconstructed plane.
    let block_count = (num_mb_cols as usize)
        .checked_mul(num_mb_rows as usize)
        .and_then(|n| n.checked_mul(blocks_x))
        .and_then(|n| n.checked_mul(blocks_y))
        .ok_or(Error::CorruptTile(tile_idx))?;
    if comp_bytes.len() < block_count.div_ceil(8) {
        return Err(Error::CorruptTile(tile_idx));
    }
    let samples = tw_c.checked_mul(th_c).ok_or(Error::CorruptTile(tile_idx))?;

    let mut comp_buf = vec![0u16; samples];
    let mut r = BitReader::new(comp_bytes);

    let mut prev_dc: i32 = 0;
    let mut prev_dc_diff: u32 = 20;
    let mut prev_1st_ac_level: u32 = 0;

    let q_mat = &hdr.q_matrix[c];
    let coded_depth = hdr.bit_depth;

    for mb_y in 0..(num_mb_rows as usize) {
        for mb_x in 0..(num_mb_cols as usize) {
            for by in 0..blocks_y {
                for bx in 0..blocks_x {
                    // abs_dc_coeff_diff (RFC 9924 §5.3.15 & §7.1.1)
                    let k_dc = (prev_dc_diff >> 1).min(5);
                    let abs_dc_diff = read_vlc(&mut r, k_dc).map_err(|_| Error::CorruptTile(tile_idx))?;
                    let sign_dc = if abs_dc_diff > 0 { r.read_bit().map_err(|_| Error::CorruptTile(tile_idx))? } else { false };
                    let diff = if sign_dc { -(abs_dc_diff as i64) } else { abs_dc_diff as i64 };
                    let dc = prev_dc as i64 + diff;
                    if !(-32768..=32767).contains(&dc) {
                        return Err(Error::CorruptTile(tile_idx));
                    }
                    prev_dc = dc as i32;
                    prev_dc_diff = abs_dc_diff;

                    let mut coeff = [0i16; 64];
                    coeff[0] = dc as i16;

                    // ac_coeff_coding (RFC 9924 §5.3.16 & §7.1.2–7.1.3)
                    let mut scan_pos = 1usize;
                    let mut first_ac = true;
                    let mut prev_level = prev_1st_ac_level;
                    let mut prev_run = 0u32;

                    while scan_pos < 64 {
                        let k_run = (prev_run >> 2).min(2);
                        let run = read_vlc(&mut r, k_run).map_err(|_| Error::CorruptTile(tile_idx))? as usize;
                        scan_pos = scan_pos.checked_add(run).ok_or(Error::CorruptTile(tile_idx))?;
                        if scan_pos > 64 {
                            return Err(Error::CorruptTile(tile_idx));
                        }
                        prev_run = run as u32;
                        if scan_pos < 64 {
                            let k_lvl = (prev_level >> 2).min(4);
                            let abs_ac_minus1 = read_vlc(&mut r, k_lvl).map_err(|_| Error::CorruptTile(tile_idx))?;
                            let sign_ac = r.read_bit().map_err(|_| Error::CorruptTile(tile_idx))?;
                            let abs_level = (abs_ac_minus1 as u64) + 1;
                            let level_i64 = if sign_ac { -(abs_level as i64) } else { abs_level as i64 };
                            if !(-32768..=32767).contains(&level_i64) {
                                return Err(Error::CorruptTile(tile_idx));
                            }
                            coeff[ZIGZAG_8X8[scan_pos] as usize] = level_i64 as i16;
                            scan_pos += 1;
                            prev_level = abs_level as u32;
                            if first_ac {
                                first_ac = false;
                                prev_1st_ac_level = prev_level;
                            }
                        }
                    }

                    let mut rec = [0u16; 64];
                    if first_ac {
                        scale_and_idct8x8_dc_only(coeff[0], q_mat[0], qp, coded_depth, &mut rec);
                    } else {
                        scale_and_idct8x8(&coeff, q_mat, qp, coded_depth, &mut rec);
                    }
                    if out_depth != coded_depth {
                        rescale_block(&mut rec, coded_depth, out_depth);
                    }

                    let px0 = mb_x * blk_w + bx * 8;
                    let py0 = mb_y * blk_h + by * 8;
                    for ry in 0..8 {
                        let dst_row = (py0 + ry) * tw_c + px0;
                        comp_buf[dst_row..dst_row + 8].copy_from_slice(&rec[ry * 8..ry * 8 + 8]);
                    }
                }
            }
        }
    }

    read_byte_alignment(&mut r).map_err(|_| Error::CorruptTile(tile_idx))?;
    Ok(comp_buf)
}

#[inline]
fn rescale_block(block: &mut [u16; 64], from_depth: u8, to_depth: u8) {
    if to_depth > from_depth {
        let shift = (to_depth - from_depth) as u32;
        let fill_shift = from_depth as u32;
        for s in block.iter_mut() {
            let v = *s as u32;
            *s = ((v << shift) | (v >> (fill_shift - shift))) as u16;
        }
    } else if to_depth < from_depth {
        let shift = (from_depth - to_depth) as u32;
        let round = 1u32 << (shift - 1);
        let max_v = (1u32 << to_depth) - 1;
        for s in block.iter_mut() {
            *s = (((*s as u32) + round) >> shift).min(max_v) as u16;
        }
    }
}
