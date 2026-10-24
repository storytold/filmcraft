//! Header-level parsing of an AV1 stream: OBUs (5.3), sequence and frame headers, tile group
//! payloads, and the reference state the headers depend on (7.20, 7.21), without decoding any
//! tile. [`crate::Decoder`] decodes the frames it hands out; a hardware decoder (VA-API) feeds
//! the same frames to the GPU, so both read the stream alike.

use crate::bits::{BitReader, leb128};
use crate::header::{FrameHeader, HeaderState, LoopFilterDeltas, RefInfo, SegmentationFeatures, SequenceHeader, default_gm_params};
use crate::spec_tables::{KEY_FRAME, NUM_REF_FRAMES};
use crate::{Error, Result};

const OBU_SEQUENCE_HEADER_T: u32 = 1;
const OBU_TEMPORAL_DELIMITER_T: u32 = 2;
const OBU_FRAME_HEADER_T: u32 = 3;
const OBU_TILE_GROUP_T: u32 = 4;
const OBU_FRAME_T: u32 = 6;
const OBU_REDUNDANT_FRAME_HEADER_T: u32 = 7;

/// Largest accepted frame: the AV1 level 6.x limits (Annex A: MaxHSize 16384, MaxVSize 8704, MaxPicSize
/// 35 651 584 luma samples; level 6.3 is the highest defined). The sequence header allows 65536 x 65536
/// (gigabytes of buffers), so nothing larger than a level 6.3 frame is decoded.
const MAX_FRAME_WIDTH: u32 = 16384;
const MAX_FRAME_HEIGHT: u32 = 8704;
const MAX_FRAME_AREA: u64 = 35_651_584;

/// Whether a frame of `width` x `height` luma samples is within the decoder's resource limits.
fn frame_size_allowed(width: u32, height: u32) -> bool {
    width <= MAX_FRAME_WIDTH && height <= MAX_FRAME_HEIGHT && u64::from(width) * u64::from(height) <= MAX_FRAME_AREA
}

/// A frame whose header and tile groups are complete.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The sequence header active for the frame.
    pub seq: SequenceHeader,
    pub header: FrameHeader,
    /// The reference slots as the frame header found them (before this frame's refresh).
    pub refs: [RefInfo; NUM_REF_FRAMES],
    /// Tile payloads in tile order: (tile number, data).
    pub tiles: Vec<(usize, Vec<u8>)>,
}

/// What a chunk of OBUs produced, in stream order.
#[derive(Debug, Clone)]
pub enum Event {
    /// A frame to decode (shown when `header.show_frame`).
    Frame { frame: Box<Frame>, temporal_unit: usize, spatial_id: u32 },
    /// show_existing_frame: slot `header.frame_to_show_map_idx` is shown again, with the size and
    /// film grain in `info`. A key frame shown this way has refreshed every slot from that one.
    ShowExisting { seq: Box<SequenceHeader>, header: Box<FrameHeader>, info: Box<RefInfo>, temporal_unit: usize, spatial_id: u32 },
}

/// The header-level state of one AV1 stream.
pub struct Parser {
    seq: Option<SequenceHeader>,
    refs: [RefInfo; NUM_REF_FRAMES],
    lf_deltas: LoopFilterDeltas,
    seg_features: SegmentationFeatures,
    prev_gm_params: [[i32; 6]; 8],
    current_frame_id: u32,
    /// Frame being collected (between its header and its last tile group).
    pending: Option<Box<Frame>>,
    /// Which tiles of the pending frame a tile group has supplied (each exactly once).
    tile_covered: Vec<bool>,
    seen_frame_header: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            seq: None,
            refs: Default::default(),
            lf_deltas: LoopFilterDeltas::defaults(),
            seg_features: SegmentationFeatures::default(),
            prev_gm_params: default_gm_params(),
            current_frame_id: 0,
            pending: None,
            tile_covered: Vec::new(),
            seen_frame_header: false,
        }
    }

    /// The active sequence header, once one has been parsed.
    pub fn sequence_header(&self) -> Option<&SequenceHeader> {
        self.seq.as_ref()
    }

    /// The reference slots now.
    pub fn refs(&self) -> &[RefInfo; NUM_REF_FRAMES] {
        &self.refs
    }

    /// Parse a chunk of OBUs (one temporal unit, or any run of them in the low-overhead format).
    /// The events of OBUs before an error are returned with it in `events`.
    pub fn parse(&mut self, data: &[u8], events: &mut Vec<Event>) -> Result<()> {
        let mut tu = 0usize;
        let mut pos = 0;
        while pos < data.len() {
            let h = data[pos];
            if h & 0x80 != 0 {
                return Err(Error::Invalid("obu_forbidden_bit"));
            }
            let obu_type = ((h >> 3) & 0xf) as u32;
            let ext = h & 4 != 0;
            let has_size = h & 2 != 0;
            let mut p = pos + 1;
            let (mut temporal_id, mut spatial_id) = (0, 0);
            if ext {
                let e = *data.get(p).ok_or(Error::Truncated)?;
                temporal_id = (e >> 5) as u32;
                spatial_id = ((e >> 3) & 3) as u32;
                p += 1;
            }
            let size = if has_size {
                let (v, n) = leb128(data.get(p..).ok_or(Error::Truncated)?)?;
                p += n;
                v as usize
            } else {
                data.len().checked_sub(p).ok_or(Error::Truncated)?
            };
            let end = p.checked_add(size).filter(|&e| e <= data.len()).ok_or(Error::Truncated)?;
            let payload = &data[p..end];
            pos = end;
            if obu_type != OBU_SEQUENCE_HEADER_T
                && obu_type != OBU_TEMPORAL_DELIMITER_T
                && ext
                && let Some(seq) = &self.seq
                && seq.op_idc != 0
            {
                let idc = seq.op_idc;
                let in_t = (idc >> temporal_id) & 1;
                let in_s = (idc >> (spatial_id + 8)) & 1;
                if in_t == 0 || in_s == 0 {
                    continue;
                }
            }
            match obu_type {
                OBU_SEQUENCE_HEADER_T => self.seq = Some(SequenceHeader::parse(payload)?),
                OBU_TEMPORAL_DELIMITER_T => {
                    self.seen_frame_header = false;
                    tu += 1;
                }
                OBU_FRAME_HEADER_T | OBU_REDUNDANT_FRAME_HEADER_T | OBU_FRAME_T => {
                    // frame_header_copy(): identical to the active header
                    if self.seen_frame_header && obu_type != OBU_FRAME_T {
                        continue;
                    }
                    let mut r = BitReader::new(payload);
                    if let Some(e) = self.frame_header(&mut r, temporal_id, spatial_id, tu)? {
                        events.push(e);
                        continue;
                    }
                    if obu_type == OBU_FRAME_T {
                        r.byte_align();
                        let rest = payload.get(r.byte_pos()..).ok_or(Error::Truncated)?;
                        if let Some(e) = self.tile_group(rest, tu, spatial_id)? {
                            events.push(e);
                        }
                    }
                }
                OBU_TILE_GROUP_T => {
                    if let Some(e) = self.tile_group(payload, tu, spatial_id)? {
                        events.push(e);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// frame_header_obu( ): the event for show_existing_frame.
    fn frame_header(&mut self, r: &mut BitReader, temporal_id: u32, spatial_id: u32, tu: usize) -> Result<Option<Event>> {
        let seq = self.seq.clone().ok_or(Error::Invalid("frame before sequence header"))?;
        self.seen_frame_header = true;
        let mut st = HeaderState {
            seq: &seq,
            refs: &mut self.refs,
            lf_deltas: self.lf_deltas,
            seg_features: self.seg_features,
            prev_gm_params: self.prev_gm_params,
            current_frame_id: self.current_frame_id,
        };
        let fh = FrameHeader::parse(r, &mut st, temporal_id, spatial_id)?;
        self.lf_deltas = st.lf_deltas;
        self.seg_features = st.seg_features;
        self.prev_gm_params = st.prev_gm_params;
        self.current_frame_id = st.current_frame_id;
        if fh.show_existing_frame {
            self.seen_frame_header = false;
            let info = self.refs.get(fh.frame_to_show_map_idx).cloned().ok_or(Error::Invalid("frame_to_show_map_idx"))?;
            if fh.frame_type == KEY_FRAME as u8 {
                // reference frame loading process (7.21) then refresh every slot (7.20)
                self.lf_deltas = info.lf_deltas;
                self.seg_features = info.seg_features;
                for r in self.refs.iter_mut() {
                    *r = info.clone();
                }
            }
            return Ok(Some(Event::ShowExisting { seq: Box::new(seq), header: Box::new(fh), info: Box::new(info), temporal_unit: tu, spatial_id }));
        }
        // Encoders exist that code frames slightly larger than the sequence maximum (e.g. a
        // 270-line stream coded as 272 lines); like other decoders we accept them.
        if !frame_size_allowed(fh.upscaled_width, fh.frame_height) {
            return Err(Error::Invalid("frame size"));
        }
        let tile_count = fh.tile_info.cols.checked_mul(fh.tile_info.rows).ok_or(Error::Invalid("tile count overflows"))?;
        self.tile_covered = vec![false; tile_count];
        self.pending = Some(Box::new(Frame { seq, header: fh, refs: self.refs.clone(), tiles: Vec::new() }));
        Ok(None)
    }

    /// tile_group_obu( sz ): the frame's event once its last tile group is in.
    fn tile_group(&mut self, data: &[u8], tu: usize, spatial_id: u32) -> Result<Option<Event>> {
        let mut frame = self.pending.take().ok_or(Error::Invalid("tile group without a frame header"))?;
        let (cols, rows, cols_log2, rows_log2, tsb) = {
            let t = &frame.header.tile_info;
            (t.cols, t.rows, t.cols_log2, t.rows_log2, t.tile_size_bytes)
        };
        let num_tiles = cols.checked_mul(rows).filter(|n| *n > 0).ok_or(Error::Invalid("tile count"))?;
        let mut r = BitReader::new(data);
        let mut tg_start = 0;
        let mut tg_end = num_tiles - 1;
        if num_tiles > 1 && r.flag()? {
            let bits = cols_log2 + rows_log2;
            tg_start = r.f(bits)? as usize;
            tg_end = r.f(bits)? as usize;
        }
        r.byte_align();
        if tg_end >= num_tiles || tg_start > tg_end {
            return Err(Error::Invalid("tile group range"));
        }
        let mut pos = r.byte_pos();
        for tile_num in tg_start..=tg_end {
            let size = if tile_num == tg_end {
                data.len().checked_sub(pos).ok_or(Error::Truncated)?
            } else {
                let mut v = 0usize;
                for i in 0..tsb as usize {
                    v |= (*data.get(pos + i).ok_or(Error::Truncated)? as usize) << (8 * i);
                }
                pos += tsb as usize;
                v + 1
            };
            let end = pos.checked_add(size).filter(|&e| e <= data.len()).ok_or(Error::Truncated)?;
            let covered = self.tile_covered.get_mut(tile_num).filter(|c| !**c).ok_or(Error::Invalid("tile is covered by multiple tile groups"))?;
            *covered = true;
            frame.tiles.push((tile_num, data[pos..end].to_vec()));
            pos = end;
        }
        if tg_end != num_tiles - 1 {
            self.pending = Some(frame);
            return Ok(None);
        }
        if self.tile_covered.iter().any(|&covered| !covered) {
            return Err(Error::Invalid("frame is missing tile coverage"));
        }
        self.seen_frame_header = false;
        self.refresh(&frame.header);
        Ok(Some(Event::Frame { frame, temporal_unit: tu, spatial_id }))
    }

    /// The reference frame update process (7.20) for the header-level state.
    fn refresh(&mut self, fh: &FrameHeader) {
        for (i, ri) in self.refs.iter_mut().enumerate() {
            if (fh.refresh_frame_flags >> i) & 1 == 1 {
                ri.valid = true;
                ri.frame_id = fh.current_frame_id;
                ri.upscaled_width = fh.upscaled_width;
                ri.frame_width = fh.frame_width;
                ri.frame_height = fh.frame_height;
                ri.render_width = fh.render_width;
                ri.render_height = fh.render_height;
                ri.mi_cols = fh.mi_cols;
                ri.mi_rows = fh.mi_rows;
                ri.frame_type = fh.frame_type;
                ri.order_hint = fh.order_hint;
                ri.saved_order_hints = fh.order_hints;
                ri.gm_params = fh.gm_params;
                ri.lf_deltas = fh.lf.deltas;
                ri.seg_features = fh.seg.features;
                ri.grain = fh.film_grain.clone();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::frame_size_allowed;

    #[test]
    fn frames_up_to_the_level_6_3_limits_are_allowed() {
        assert!(frame_size_allowed(16384, 2176)); // exactly MaxPicSize
        assert!(frame_size_allowed(4096, 8704)); // exactly MaxVSize
        assert!(frame_size_allowed(8192, 4352)); // exactly MaxPicSize
        assert!(frame_size_allowed(7680, 4320)); // 8K UHD
        assert!(frame_size_allowed(1, 1));
    }

    #[test]
    fn frames_beyond_the_limits_are_refused() {
        assert!(!frame_size_allowed(16384, 2177)); // one row over MaxPicSize
        assert!(!frame_size_allowed(16384, 8704)); // within both sides, four times MaxPicSize
        assert!(!frame_size_allowed(4096, 8705)); // one row over MaxVSize
        assert!(!frame_size_allowed(16385, 1)); // one column over MaxHSize
        assert!(!frame_size_allowed(65536, 65536));
    }
}
