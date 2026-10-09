//! Hardware acceleration front end (stateless decoders such as VA-API).
//!
//! [`AccelDecoder`] does the part of decoding a stateless hardware decoder leaves to the
//! application, with the software decoder's own code: OBU parsing, the sequence and frame headers
//! ([`crate::header`], including the loop filter deltas, segmentation features and global motion
//! carried from frame to frame), tile groups, the reference slots and their saved state (7.20,
//! 7.21) and `show_existing_frame`. Each complete frame goes to the [`Accelerator`] as an
//! [`AccelFrame`]; the hardware does the rest (entropy decoding with its own CDFs, reconstruction,
//! loop filters, motion field projection). Output is [`AccelOutput`]s naming frames by id; the
//! accelerator owns the pixels.
//!
//! Streams the hardware cannot take as they are (scalable operating points, a missing reference)
//! make it return an error, so that a caller can switch to the software decoder.

use crate::bits::{BitReader, leb128};
use crate::header::{HeaderState, LoopFilterDeltas, RefInfo, SegmentationFeatures, default_gm_params};
use crate::inter::setup_shear;
use crate::spec_tables::{KEY_FRAME, NUM_REF_FRAMES, PRIMARY_REF_NONE};
use crate::{Error, Result};

pub use crate::header::{FilmGrainParams, FrameHeader, SequenceHeader};

const OBU_SEQUENCE_HEADER: u8 = 1;
const OBU_TEMPORAL_DELIMITER: u8 = 2;
const OBU_FRAME_HEADER: u8 = 3;
const OBU_TILE_GROUP: u8 = 4;
const OBU_FRAME: u8 = 6;
const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;

/// One tile of a frame: where its data is in [`AccelFrame::data`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccelTile {
    /// Tile number (raster order: `row * cols + col`).
    pub num: usize,
    pub row: usize,
    pub col: usize,
    /// The tile group it came in.
    pub tg_start: usize,
    pub tg_end: usize,
    pub offset: usize,
    pub size: usize,
}

/// One complete frame.
pub struct AccelFrame<'a> {
    /// Id of the frame being decoded.
    pub id: u32,
    pub seq: &'a SequenceHeader,
    pub header: &'a FrameHeader,
    /// The frame ids in the eight reference slots before this frame (`None`: empty slot).
    pub slots: [Option<u32>; 8],
    /// warpValid of each reference frame's global motion (7.11.3.6), indexed LAST..ALTREF (index
    /// 0 unused).
    pub gm_valid: [bool; 8],
    pub tiles: &'a [AccelTile],
    /// Every tile's data, back to back.
    pub data: &'a [u8],
}

/// A frame leaving the decoder, in output order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccelOutput {
    pub id: u32,
    pub pts: i64,
    pub key: bool,
    /// Output size: the upscaled width and the frame height.
    pub width: u32,
    pub height: u32,
    /// Film grain to apply to this output (`apply_grain` false: none).
    pub film_grain: FilmGrainParams,
}

/// A stateless hardware decoder.
pub trait Accelerator: Send {
    /// Decode `frame`. An error stops decoding with that error.
    fn decode_frame(&mut self, frame: &AccelFrame<'_>) -> std::result::Result<(), String>;
    /// After a frame, the ids still needed (the reference slots and the frame just output); any
    /// other frame's storage may be reused.
    fn retain(&mut self, live: &[u32]);
}

struct Pending {
    header: FrameHeader,
    id: u32,
    slots: [Option<u32>; 8],
    tiles: Vec<AccelTile>,
    data: Vec<u8>,
}

/// The AV1 decoding front end for an [`Accelerator`] (see the module documentation).
pub struct AccelDecoder {
    seq: Option<SequenceHeader>,
    refs: [RefInfo; NUM_REF_FRAMES],
    ids: [Option<u32>; NUM_REF_FRAMES],
    lf_deltas: LoopFilterDeltas,
    seg_features: SegmentationFeatures,
    prev_gm_params: [[i32; 6]; 8],
    current_frame_id: u32,
    seen_frame_header: bool,
    pending: Option<Pending>,
    next_id: u32,
    accel: Box<dyn Accelerator>,
}

impl AccelDecoder {
    pub fn new(accel: Box<dyn Accelerator>) -> Self {
        AccelDecoder {
            seq: None,
            refs: Default::default(),
            ids: [None; NUM_REF_FRAMES],
            lf_deltas: LoopFilterDeltas::defaults(),
            seg_features: SegmentationFeatures::default(),
            prev_gm_params: default_gm_params(),
            current_frame_id: 0,
            seen_frame_header: false,
            pending: None,
            next_id: 0,
            accel,
        }
    }

    /// The active sequence header.
    pub fn sequence_header(&self) -> Option<&SequenceHeader> {
        self.seq.as_ref()
    }

    /// Forget every reference and the carried header state, keeping the sequence header (before
    /// decoding from another key frame after a seek).
    pub fn reset(&mut self) {
        self.refs = Default::default();
        self.ids = [None; NUM_REF_FRAMES];
        self.lf_deltas = LoopFilterDeltas::defaults();
        self.seg_features = SegmentationFeatures::default();
        self.prev_gm_params = default_gm_params();
        self.current_frame_id = 0;
        self.seen_frame_header = false;
        self.pending = None;
        self.accel.retain(&[]);
    }

    /// Decode a chunk of OBUs (a temporal unit); returns the frames it shows, in order, each with
    /// `pts`.
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Vec<AccelOutput>> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < data.len() {
            let h = data[pos];
            if h & 0x80 != 0 {
                return Err(Error::Invalid("obu_forbidden_bit"));
            }
            let obu_type = (h >> 3) & 0xf;
            let ext = h & 4 != 0;
            let has_size = h & 2 != 0;
            let mut p = pos + 1;
            let (mut temporal_id, mut spatial_id) = (0, 0);
            if ext {
                let e = *data.get(p).ok_or(Error::Truncated)?;
                temporal_id = u32::from(e >> 5);
                spatial_id = u32::from((e >> 3) & 3);
                p += 1;
            }
            let size = if has_size {
                let (v, n) = leb128(data.get(p..).ok_or(Error::Truncated)?)?;
                p += n;
                usize::try_from(v).map_err(|_| Error::Truncated)?
            } else {
                data.len().checked_sub(p).ok_or(Error::Truncated)?
            };
            let end = p.checked_add(size).filter(|&e| e <= data.len()).ok_or(Error::Truncated)?;
            let payload = &data[p..end];
            pos = end;
            match obu_type {
                OBU_SEQUENCE_HEADER => {
                    let s = SequenceHeader::parse(payload)?;
                    if s.op_idc != 0 {
                        return Err(Error::Unsupported("scalable streams in hardware"));
                    }
                    self.seq = Some(s);
                }
                OBU_TEMPORAL_DELIMITER => self.seen_frame_header = false,
                OBU_FRAME_HEADER | OBU_REDUNDANT_FRAME_HEADER | OBU_FRAME => {
                    if self.seen_frame_header && obu_type != OBU_FRAME {
                        // frame_header_copy(): identical to the active header
                        continue;
                    }
                    let mut r = BitReader::new(payload);
                    if let Some(o) = self.frame_header(&mut r, temporal_id, spatial_id, pts)? {
                        out.push(o);
                        continue;
                    }
                    if obu_type == OBU_FRAME {
                        r.byte_align();
                        let rest = payload.get(r.byte_pos()..).ok_or(Error::Truncated)?;
                        out.extend(self.tile_group(rest, pts)?);
                    }
                }
                OBU_TILE_GROUP => out.extend(self.tile_group(payload, pts)?),
                _ => {}
            }
        }
        Ok(out)
    }

    fn frame_header(&mut self, r: &mut BitReader, temporal_id: u32, spatial_id: u32, pts: i64) -> Result<Option<AccelOutput>> {
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
            let idx = fh.frame_to_show_map_idx & 7;
            let id = self.ids[idx].ok_or(Error::Invalid("show_existing_frame of an empty slot"))?;
            let info = self.refs[idx].clone();
            let o = AccelOutput {
                id,
                pts,
                key: info.frame_type == KEY_FRAME as u8,
                width: info.upscaled_width,
                height: info.frame_height,
                film_grain: fh.film_grain.clone(),
            };
            if fh.frame_type == KEY_FRAME as u8 {
                // reference frame loading process (7.21) then refresh every slot (7.20)
                self.lf_deltas = info.lf_deltas;
                self.seg_features = info.seg_features;
                for i in 0..NUM_REF_FRAMES {
                    self.refs[i] = info.clone();
                    self.ids[i] = Some(id);
                }
                self.accel.retain(&[id]);
            }
            return Ok(Some(o));
        }
        if fh.upscaled_width > 65536 || fh.frame_height > 65536 {
            return Err(Error::Invalid("frame size"));
        }
        if !fh.frame_is_intra && fh.ref_frame_idx.iter().any(|&i| self.ids.get(i).copied().flatten().is_none()) {
            return Err(Error::Invalid("missing reference frame"));
        }
        if fh.primary_ref_frame != PRIMARY_REF_NONE && fh.ref_frame_idx.get(fh.primary_ref_frame).and_then(|&i| self.ids.get(i).copied().flatten()).is_none() {
            return Err(Error::Invalid("primary reference frame missing"));
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.pending = Some(Pending { header: fh, id, slots: self.ids, tiles: Vec::new(), data: Vec::new() });
        Ok(None)
    }

    fn tile_group(&mut self, data: &[u8], pts: i64) -> Result<Option<AccelOutput>> {
        let mut job = self.pending.take().ok_or(Error::Invalid("tile group without a frame header"))?;
        let t = &job.header.tile_info;
        let (cols, rows, cols_log2, rows_log2, tsb) = (t.cols, t.rows, t.cols_log2, t.rows_log2, t.tile_size_bytes);
        let num_tiles = cols.checked_mul(rows).filter(|&n| n > 0).ok_or(Error::Invalid("tile count"))?;
        let mut r = BitReader::new(data);
        let (mut tg_start, mut tg_end) = (0, num_tiles - 1);
        if num_tiles > 1 && r.flag()? {
            let bits = cols_log2 + rows_log2;
            tg_start = r.f(bits)? as usize;
            tg_end = r.f(bits)? as usize;
        }
        r.byte_align();
        if tg_end >= num_tiles || tg_start > tg_end || tg_start != job.tiles.len() {
            return Err(Error::Invalid("tile group range"));
        }
        let mut pos = r.byte_pos();
        for num in tg_start..=tg_end {
            let size = if num == tg_end {
                data.len().checked_sub(pos).ok_or(Error::Truncated)?
            } else {
                let mut v = 0usize;
                for i in 0..tsb as usize {
                    v |= usize::from(*data.get(pos + i).ok_or(Error::Truncated)?) << (8 * i);
                }
                pos += tsb as usize;
                v + 1
            };
            let end = pos.checked_add(size).filter(|&e| e <= data.len()).ok_or(Error::Truncated)?;
            job.tiles.push(AccelTile { num, row: num / cols, col: num % cols, tg_start, tg_end, offset: job.data.len(), size });
            job.data.extend_from_slice(&data[pos..end]);
            pos = end;
        }
        if tg_end != num_tiles - 1 {
            self.pending = Some(job);
            return Ok(None);
        }
        self.seen_frame_header = false;
        self.submit(job, pts)
    }

    /// The frame's data is complete: decode it, then update the reference state (7.20).
    fn submit(&mut self, job: Pending, pts: i64) -> Result<Option<AccelOutput>> {
        let seq = self.seq.as_ref().ok_or(Error::Invalid("frame before sequence header"))?;
        let fh = &job.header;
        let gm_valid: [bool; 8] = std::array::from_fn(|i| i > 0 && setup_shear(&fh.gm_params[i]).0);
        let frame = AccelFrame { id: job.id, seq, header: fh, slots: job.slots, gm_valid, tiles: &job.tiles, data: &job.data };
        self.accel.decode_frame(&frame).map_err(|_| Error::Invalid("hardware decoding failed"))?;
        for i in 0..NUM_REF_FRAMES {
            if (fh.refresh_frame_flags >> i) & 1 == 1 {
                let ri = &mut self.refs[i];
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
                self.ids[i] = Some(job.id);
            }
        }
        let mut live: Vec<u32> = self.ids.iter().flatten().copied().collect();
        if fh.show_frame {
            live.push(job.id);
        }
        self.accel.retain(&live);
        Ok(fh.show_frame.then(|| AccelOutput {
            id: job.id,
            pts,
            key: fh.frame_type == KEY_FRAME as u8,
            width: fh.upscaled_width,
            height: fh.frame_height,
            film_grain: fh.film_grain.clone(),
        }))
    }
}
