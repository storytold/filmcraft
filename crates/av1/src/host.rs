//! The headers of an AV1 stream for a host that does not reconstruct pictures itself (a stateless
//! hardware decoder): the same OBU walk, frame header parser and reference state as [`Decoder`],
//! so the two read a stream alike, and the tiles of each frame instead of the pictures.
//!
//! [`Decoder`]: crate::Decoder

use std::ops::Range;

use crate::bits::{BitReader, leb128};
use crate::header::{FrameHeader, HeaderState, LoopFilterDeltas, RefInfo, SegmentationFeatures, SequenceHeader, TileInfo};
use crate::spec_tables::{KEY_FRAME, NUM_REF_FRAMES};
use crate::{Error, Result};

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_TILE_GROUP: u8 = 4;
const OBU_FRAME: u8 = 6;
const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;

/// One tile's coded data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostTile {
    /// TileNum: `row * cols + column`.
    pub number: usize,
    pub data: Vec<u8>,
}

/// A frame whose tiles have all arrived.
#[derive(Clone, Debug)]
pub struct HostFrame {
    pub sequence: SequenceHeader,
    pub header: FrameHeader,
    pub tiles: Vec<HostTile>,
}

/// What a temporal unit asks of the decoder, in order.
#[derive(Clone, Debug)]
pub enum HostEvent {
    /// Decode this frame, output it when `header.show_frame`, then store it in the slots of
    /// `header.refresh_frame_flags`.
    Frame(Box<HostFrame>),
    /// Output the frame in reference `slot` again, with film grain when `grain`. A `key_frame` also
    /// goes into every slot (7.21).
    ShowExisting { slot: usize, key_frame: bool, grain: bool },
}

struct Pending {
    sequence: SequenceHeader,
    header: FrameHeader,
    tiles: Vec<HostTile>,
    covered: Vec<bool>,
}

/// Parses a stream's headers. Scalable streams are refused: which layers to output is a decision
/// of the full decoder.
pub struct HostParser {
    sequence: Option<SequenceHeader>,
    refs: [RefInfo; NUM_REF_FRAMES],
    lf_deltas: LoopFilterDeltas,
    seg_features: SegmentationFeatures,
    prev_gm_params: [[i32; 6]; 8],
    current_frame_id: u32,
    seen_frame_header: bool,
    pending: Option<Pending>,
}

impl Default for HostParser {
    fn default() -> Self {
        Self::new()
    }
}

impl HostParser {
    pub fn new() -> Self {
        Self {
            sequence: None,
            refs: Default::default(),
            lf_deltas: LoopFilterDeltas::defaults(),
            seg_features: SegmentationFeatures::default(),
            prev_gm_params: crate::header::default_gm_params(),
            current_frame_id: 0,
            seen_frame_header: false,
            pending: None,
        }
    }

    /// The events of `data`, one or more low-overhead OBUs (a temporal unit, configuration OBUs).
    pub fn parse(&mut self, data: &[u8]) -> Result<Vec<HostEvent>> {
        let mut events = Vec::new();
        let mut pos = 0usize;
        while pos < data.len() {
            let h = *data.get(pos).ok_or(Error::Truncated)?;
            if h & 0x80 != 0 {
                return Err(Error::Invalid("obu_forbidden_bit"));
            }
            let obu_type = (h >> 3) & 0xf;
            let mut p = pos + 1;
            let (mut temporal_id, mut spatial_id) = (0, 0);
            if h & 4 != 0 {
                let e = *data.get(p).ok_or(Error::Truncated)?;
                temporal_id = u32::from(e >> 5);
                spatial_id = u32::from((e >> 3) & 3);
                p += 1;
            }
            let size = if h & 2 != 0 {
                let (v, n) = leb128(data.get(p..).ok_or(Error::Truncated)?)?;
                p += n;
                usize::try_from(v).map_err(|_| Error::Truncated)?
            } else {
                data.len().checked_sub(p).ok_or(Error::Truncated)?
            };
            let end = p.checked_add(size).filter(|&e| e <= data.len()).ok_or(Error::Truncated)?;
            let payload = data.get(p..end).ok_or(Error::Truncated)?;
            pos = end;
            match obu_type {
                OBU_SEQUENCE_HEADER => {
                    let s = SequenceHeader::parse(payload)?;
                    if s.op_idc != 0 {
                        return Err(Error::Unsupported("scalable stream"));
                    }
                    self.sequence = Some(s);
                }
                OBU_TEMPORAL_DELIMITER => self.seen_frame_header = false,
                OBU_FRAME_HEADER | OBU_REDUNDANT_FRAME_HEADER | OBU_FRAME => {
                    // frame_header_copy(): identical to the active header
                    if self.seen_frame_header && obu_type != OBU_FRAME {
                        continue;
                    }
                    let mut r = BitReader::new(payload);
                    if let Some(event) = self.frame_header(&mut r, temporal_id, spatial_id)? {
                        events.push(event);
                        continue;
                    }
                    if obu_type == OBU_FRAME {
                        r.byte_align();
                        let rest = payload.get(r.byte_pos()..).ok_or(Error::Truncated)?;
                        events.extend(self.tile_group(rest)?);
                    }
                }
                OBU_TILE_GROUP => events.extend(self.tile_group(payload)?),
                _ => {}
            }
        }
        Ok(events)
    }

    fn frame_header(&mut self, r: &mut BitReader, temporal_id: u32, spatial_id: u32) -> Result<Option<HostEvent>> {
        let sequence = self.sequence.clone().ok_or(Error::Invalid("frame before sequence header"))?;
        self.seen_frame_header = true;
        let mut st = HeaderState {
            seq: &sequence,
            refs: &mut self.refs,
            lf_deltas: self.lf_deltas,
            seg_features: self.seg_features,
            prev_gm_params: self.prev_gm_params,
            current_frame_id: self.current_frame_id,
        };
        let header = FrameHeader::parse(r, &mut st, temporal_id, spatial_id)?;
        self.lf_deltas = st.lf_deltas;
        self.seg_features = st.seg_features;
        self.prev_gm_params = st.prev_gm_params;
        self.current_frame_id = st.current_frame_id;
        if header.show_existing_frame {
            self.seen_frame_header = false;
            let slot = header.frame_to_show_map_idx;
            let info = self.refs.get(slot).ok_or(Error::Invalid("reference slot"))?.clone();
            let key_frame = header.frame_type == KEY_FRAME as u8;
            if key_frame {
                self.lf_deltas = info.lf_deltas;
                self.seg_features = info.seg_features;
                for r in &mut self.refs {
                    *r = info.clone();
                }
            }
            let grain = sequence.film_grain_params_present && header.film_grain.apply_grain;
            return Ok(Some(HostEvent::ShowExisting { slot, key_frame, grain }));
        }
        let count = header.tile_info.cols.checked_mul(header.tile_info.rows).ok_or(Error::Invalid("tile count overflows"))?;
        self.pending = Some(Pending { sequence, header, tiles: Vec::new(), covered: vec![false; count] });
        Ok(None)
    }

    fn tile_group(&mut self, data: &[u8]) -> Result<Option<HostEvent>> {
        let mut pending = self.pending.take().ok_or(Error::Invalid("tile group without a frame header"))?;
        let (entries, last) = tile_group_entries(data, &pending.header.tile_info)?;
        for (number, range) in entries {
            let covered = pending.covered.get_mut(number).ok_or(Error::Invalid("tile group range"))?;
            if *covered {
                return Err(Error::Invalid("tile is covered by multiple tile groups"));
            }
            *covered = true;
            pending.tiles.push(HostTile { number, data: data.get(range).ok_or(Error::Truncated)?.to_vec() });
        }
        if !last {
            self.pending = Some(pending);
            return Ok(None);
        }
        if pending.covered.iter().any(|&c| !c) {
            return Err(Error::Invalid("frame is missing tile coverage"));
        }
        self.seen_frame_header = false;
        let Pending { sequence, header, mut tiles, .. } = pending;
        tiles.sort_by_key(|t| t.number);
        for (i, r) in self.refs.iter_mut().enumerate() {
            if (header.refresh_frame_flags >> i) & 1 == 1 {
                r.store(&header);
            }
        }
        Ok(Some(HostEvent::Frame(Box::new(HostFrame { sequence, header, tiles }))))
    }
}

/// tile_group_obu( ): the number and byte range in `data` of each tile, and whether the group ends
/// with the frame's last tile.
pub(crate) fn tile_group_entries(data: &[u8], info: &TileInfo) -> Result<(Vec<(usize, Range<usize>)>, bool)> {
    let num_tiles = info.cols.checked_mul(info.rows).ok_or(Error::Invalid("tile count overflows"))?;
    let last_tile = num_tiles.checked_sub(1).ok_or(Error::Invalid("frame without tiles"))?;
    let mut r = BitReader::new(data);
    let (mut start, mut end) = (0, last_tile);
    if num_tiles > 1 && r.flag()? {
        let bits = info.cols_log2 + info.rows_log2;
        start = r.f(bits)? as usize;
        end = r.f(bits)? as usize;
    }
    r.byte_align();
    if end >= num_tiles || start > end {
        return Err(Error::Invalid("tile group range"));
    }
    let mut pos = r.byte_pos();
    let mut entries = Vec::with_capacity(end - start + 1);
    for number in start..=end {
        let size = if number == end {
            data.len().checked_sub(pos).ok_or(Error::Truncated)?
        } else {
            let prefix_end = pos.checked_add(info.tile_size_bytes as usize).ok_or(Error::Truncated)?;
            let prefix = data.get(pos..prefix_end).ok_or(Error::Truncated)?;
            let mut v = 0usize;
            for (i, &byte) in prefix.iter().enumerate() {
                v |= usize::from(byte) << (8 * i);
            }
            pos = prefix_end;
            v.checked_add(1).ok_or(Error::Truncated)?
        };
        let tile_end = pos.checked_add(size).filter(|&e| e <= data.len()).ok_or(Error::Truncated)?;
        entries.push((number, pos..tile_end));
        pos = tile_end;
    }
    Ok((entries, end == last_tile))
}

/// warpValid of the setup shear process (7.11.3.6) for a set of global motion parameters.
pub fn global_warp_valid(params: &[i32; 6]) -> bool {
    crate::inter::setup_shear(params).0
}
