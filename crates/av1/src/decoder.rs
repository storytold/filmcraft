//! Top-level decoder: OBU parsing (5.3), frame / tile group handling, reference frame update
//! (7.20), show-existing-frame and output (7.18).
//!
//! Threading: headers are parsed in order on the calling thread; each frame then becomes a job
//! that waits for the reference frames it uses and decodes on a frame worker thread, so frames
//! that do not depend on each other decode concurrently. Within a frame, tiles and the
//! post-filter row bands run on a shared worker pool. Every result depends only on finished
//! reference frames, so the output is identical whatever the scheduling. Shown pictures are
//! returned in order; with frame threads they can come out of a later `decode` call (or
//! `flush`), each carrying the `pts` of the temporal unit that showed it.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use crate::bits::{BitReader, leb128};
use crate::cdf::Cdfs;
use crate::frame::FrameBuf;
use crate::header::{FilmGrainParams, FrameHeader, HeaderState, LoopFilterDeltas, RefInfo, SegmentationFeatures, SequenceHeader, default_gm_params};
use crate::par::Pool;
use crate::spec_tables::{
    ALTREF_FRAME, ALTREF2_FRAME, BWDREF_FRAME, DEFAULT_INTRA_FRAME_Y_MODE_CDF, DIV_MULT, GOLDEN_FRAME, INTRA_FRAME, INTRA_ONLY_FRAME, KEY_FRAME, LAST_FRAME,
    LAST2_FRAME, MAX_FRAME_DISTANCE, MAX_OFFSET_HEIGHT, MAX_OFFSET_WIDTH, MFMV_STACK_SIZE, NUM_REF_FRAMES, PRIMARY_REF_NONE, REFMVS_LIMIT, REFS_PER_FRAME,
};
use crate::state::{FrameShared, FrameState, TileState};
use crate::stats::{DecodeStats, Stage, Timer};
use crate::tile::TileDecoder;
use crate::{Error, Picture, Result};

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
const OBU_SEQUENCE_HEADER_T: u32 = 1;
const OBU_TEMPORAL_DELIMITER_T: u32 = 2;
const OBU_FRAME_HEADER_T: u32 = 3;
const OBU_TILE_GROUP_T: u32 = 4;
const OBU_METADATA_T: u32 = 5;
const OBU_FRAME_T: u32 = 6;
const OBU_REDUNDANT_FRAME_HEADER_T: u32 = 7;
const OBU_TILE_LIST_T: u32 = 8;

/// A stored reference frame (FrameStore plus the saved per-frame state).
pub(crate) struct RefFrame {
    pub buf: FrameBuf,
    pub cdfs: Box<Cdfs>,
    pub bit_depth: u8,
    pub subsampling_x: u8,
    pub subsampling_y: u8,
    pub segment_ids: Vec<u8>,
    pub mi_cols: usize,
    pub mi_rows: usize,
    pub film_grain_present: bool,
    pub color: (u8, u8, u8, bool),
    /// SavedRefFrames / SavedMvs (MfRefFrames / MfMvs of 7.19), mi units.
    pub saved_ref_frames: Vec<i8>,
    pub saved_mvs: Vec<[i32; 2]>,
}

/// A value produced by a decoding job, waited for by later jobs and by the output.
pub(crate) struct Slot<T> {
    value: Mutex<Option<std::result::Result<T, Error>>>,
    cv: Condvar,
}

impl<T: Clone> Slot<T> {
    fn new() -> Arc<Slot<T>> {
        Arc::new(Slot { value: Mutex::new(None), cv: Condvar::new() })
    }

    fn set(&self, v: Result<T>) {
        let mut g = self.value.lock().unwrap_or_else(|e| e.into_inner());
        *g = Some(v);
        self.cv.notify_all();
    }

    fn is_set(&self) -> bool {
        self.value.lock().map(|g| g.is_some()).unwrap_or(true)
    }

    fn wait(&self) -> Result<T> {
        let mut g = self.value.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(v) = g.as_ref() {
                return v.clone();
            }
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// [`Slot::wait`] for the slot's only consumer: moves the value out instead of cloning it.
    fn take(&self) -> Result<T> {
        let mut g = self.value.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(v) = g.take() {
                return v;
            }
            g = self.cv.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }
}

type FrameSlot = Slot<Arc<RefFrame>>;

/// Everything needed to decode one frame, captured when its header is parsed.
struct FrameJob {
    seq: SequenceHeader,
    fh: FrameHeader,
    /// Reference slots at the start of the frame (only the used ones are waited for).
    refs: [Option<Arc<FrameSlot>>; NUM_REF_FRAMES],
    ref_info: [RefInfo; NUM_REF_FRAMES],
    /// Tile payloads (tile number, data).
    tiles: Vec<(usize, Vec<u8>)>,
    /// Each tile may be supplied by exactly one tile group.
    tile_covered: Vec<bool>,
    slot: Arc<FrameSlot>,
    /// The output picture, when the frame is shown.
    picture: Option<Arc<Slot<Picture>>>,
    pts: i64,
    apply_film_grain: bool,
    /// Draft mode: the frame refreshes no reference slot, so its in-loop filters are skipped.
    draft: bool,
    #[cfg(test)]
    force_panic: bool,
}

/// A shown picture in output order.
enum Output {
    Decoded(Arc<Slot<Picture>>),
    /// show_existing_frame: built from the reference slot when it is finished.
    Existing(Box<ExistingOutput>),
}

/// The parts of a show_existing_frame output, kept behind a box so `Output` stays small.
struct ExistingOutput {
    slot: Arc<FrameSlot>,
    seq: SequenceHeader,
    width: usize,
    height: usize,
    grain: FilmGrainParams,
    apply_film_grain: bool,
    pts: i64,
}

impl Output {
    fn is_ready(&self) -> bool {
        match self {
            Output::Decoded(s) => s.is_set(),
            Output::Existing(e) => e.slot.is_set(),
        }
    }

    fn take(self) -> Result<Picture> {
        match self {
            Output::Decoded(s) => s.take(),
            Output::Existing(e) => {
                let ExistingOutput { slot, seq, width, height, grain, apply_film_grain, pts } = *e;
                let rf = slot.wait()?;
                Ok(output_picture(&seq, &rf, width, height, &grain, apply_film_grain, pts))
            }
        }
    }
}

/// Frame worker threads taking jobs in submission order.
#[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
struct Workers {
    tx: Option<std::sync::mpsc::Sender<Box<FrameJob>>>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

#[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
impl Workers {
    fn new(n: usize, pool: &Pool, stats: &Arc<Mutex<DecodeStats>>) -> Option<Workers> {
        let (tx, rx) = std::sync::mpsc::channel::<Box<FrameJob>>();
        let rx = Arc::new(Mutex::new(rx));
        let mut handles = Vec::new();
        for i in 0..n {
            let (rx, pool, stats) = (rx.clone(), pool.clone(), stats.clone());
            let h = std::thread::Builder::new()
                .name(format!("av1-frame-{i}"))
                .spawn(move || {
                    loop {
                        let job = match rx.lock() {
                            Ok(r) => r.recv(),
                            Err(_) => break,
                        };
                        match job {
                            Ok(job) => run_job(*job, &pool, &stats),
                            Err(_) => break,
                        }
                    }
                })
                .ok()?;
            handles.push(h);
        }
        Some(Workers { tx: Some(tx), handles })
    }
}

#[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
impl Drop for Workers {
    fn drop(&mut self) {
        self.tx.take();
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

/// An AV1 decoder. Feed it temporal units (one MP4 / Matroska sample each, or any run of
/// OBUs in the low-overhead format) and collect the shown frames.
pub struct Decoder {
    seq: Option<SequenceHeader>,
    refs: [RefInfo; NUM_REF_FRAMES],
    ref_frames: [Option<Arc<FrameSlot>>; NUM_REF_FRAMES],
    lf_deltas: LoopFilterDeltas,
    seg_features: SegmentationFeatures,
    prev_gm_params: [[i32; 6]; 8],
    current_frame_id: u32,
    /// Frame being collected (between its header and its last tile group).
    pending: Option<Box<FrameJob>>,
    seen_frame_header: bool,
    /// Apply film grain synthesis to output frames (default true).
    pub apply_film_grain: bool,
    draft: bool,
    stats: Arc<Mutex<DecodeStats>>,
    threads: usize,
    pool: Pool,
    #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
    workers: Option<Workers>,
    /// Frames submitted to the workers and not known to be finished (oldest first).
    in_flight: VecDeque<Arc<FrameSlot>>,
    max_in_flight: usize,
    /// Shown pictures not yet returned, in output order.
    out: VecDeque<Output>,
    /// pts of the temporal unit being decoded.
    pts: i64,
    /// Make every frame job panic (tests of the worker panic handling).
    #[cfg(test)]
    force_panic: bool,
}

fn default_threads() -> usize {
    #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
    {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16)
    }
    #[cfg(not(all(feature = "threads", not(target_arch = "wasm32"))))]
    {
        1
    }
}

/// Frames decoded concurrently with `threads` threads (each in-flight 1080p frame holds a few
/// tens of MB of state).
fn frame_threads(threads: usize) -> usize {
    if threads <= 1 { 1 } else { threads.div_ceil(2).clamp(2, 8) }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    /// A decoder using all available cores (with the `threads` feature).
    pub fn new() -> Decoder {
        Decoder::with_threads(default_threads())
    }

    /// A decoder using up to `threads` worker threads (1 = decode on the calling thread).
    pub fn with_threads(threads: usize) -> Decoder {
        let threads = threads.max(1);
        let pool = Pool::new(threads);
        let stats = Arc::new(Mutex::new(DecodeStats::default()));
        let fthreads = if pool.is_parallel() { frame_threads(threads) } else { 1 };
        #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
        let workers = if fthreads > 1 { Workers::new(fthreads, &pool, &stats) } else { None };
        Decoder {
            seq: None,
            refs: Default::default(),
            ref_frames: Default::default(),
            lf_deltas: LoopFilterDeltas::defaults(),
            seg_features: SegmentationFeatures::default(),
            prev_gm_params: default_gm_params(),
            current_frame_id: 0,
            pending: None,
            seen_frame_header: false,
            apply_film_grain: true,
            draft: false,
            stats,
            threads,
            pool,
            #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
            workers,
            in_flight: VecDeque::new(),
            max_in_flight: fthreads + 2,
            out: VecDeque::new(),
            pts: 0,
            #[cfg(test)]
            force_panic: false,
        }
    }

    /// Frame / tile counts and per-stage busy time of the frames finished so far.
    /// Draft mode for reduced-resolution playback (off by default): shown frames that refresh
    /// no reference slot (nothing can predict from them, read their motion vectors or CDFs)
    /// skip deblocking, CDEF and loop restoration and come out flagged [`Picture::draft`];
    /// every other picture is unchanged.
    pub fn set_draft(&mut self, on: bool) {
        self.draft = on;
    }

    pub fn stats(&self) -> DecodeStats {
        self.stats.lock().map(|s| *s).unwrap_or_default()
    }

    /// The active sequence header, once one has been decoded.
    pub fn sequence_header(&self) -> Option<&SequenceHeader> {
        self.seq.as_ref()
    }

    /// Decode a chunk of OBUs; returns the frames that are ready for output, in order (with
    /// frame threads, pictures shown by this chunk may come from a later call or [`flush`]).
    ///
    /// [`flush`]: Decoder::flush
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<Picture>> {
        self.decode_pts(data, 0)
    }

    /// [`Decoder::decode`] with a timestamp that the pictures shown by this chunk carry.
    pub fn decode_pts(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        self.pts = pts;
        let parsed = self.parse(data);
        // Hand out what is finished, then report a parse error.
        let mut pics = Vec::new();
        let max_queue = 2 * self.max_in_flight;
        while let Some(o) = self.out.front() {
            if !o.is_ready() && self.out.len() <= max_queue {
                break;
            }
            let Some(o) = self.out.pop_front() else { break };
            pics.push(o.take()?);
        }
        parsed?;
        Ok(pics)
    }

    /// Output every picture still held by the decoder (end of stream / before a seek); the
    /// first decoding error is returned instead.
    pub fn flush(&mut self) -> Vec<Picture> {
        self.flush_result().unwrap_or_default()
    }

    /// [`Decoder::flush`] reporting decoding errors.
    pub fn flush_result(&mut self) -> Result<Vec<Picture>> {
        let mut pics = Vec::new();
        let mut err = None;
        while let Some(o) = self.out.pop_front() {
            match o.take() {
                Ok(p) => pics.push(p),
                Err(e) => {
                    err.get_or_insert(e);
                }
            }
        }
        while let Some(s) = self.in_flight.pop_front() {
            let _ = s.wait();
        }
        match err {
            Some(e) => Err(e),
            None => Ok(pics),
        }
    }

    fn parse(&mut self, data: &[u8]) -> Result<()> {
        // (output, temporal unit, spatial_id)
        let mut out: Vec<(Output, usize, u32)> = Vec::new();
        let r = self.parse_obus(data, &mut out);
        // Output policy (7.18.1 note): with scalability, show one frame per temporal unit, the
        // highest spatial layer present.
        let scalable = self.seq.as_ref().is_some_and(|s| s.op_idc != 0);
        if !scalable {
            self.out.extend(out.into_iter().map(|(o, _, _)| o));
        } else {
            let mut last: Option<(usize, u32)> = None;
            let mut sel: Vec<Output> = Vec::new();
            for (o, t, sid) in out {
                match last {
                    Some((lt, lsid)) if lt == t => {
                        if sid >= lsid {
                            if let Some(l) = sel.last_mut() {
                                *l = o;
                            }
                            last = Some((t, sid));
                        }
                    }
                    _ => {
                        sel.push(o);
                        last = Some((t, sid));
                    }
                }
            }
            self.out.extend(sel);
        }
        r
    }

    fn parse_obus(&mut self, data: &[u8], out: &mut Vec<(Output, usize, u32)>) -> Result<()> {
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
                usize::try_from(v).map_err(|_| Error::Truncated)?
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
                OBU_SEQUENCE_HEADER_T => {
                    let s = SequenceHeader::parse(payload)?;
                    self.seq = Some(s);
                }
                OBU_TEMPORAL_DELIMITER_T => {
                    self.seen_frame_header = false;
                    tu += 1;
                }
                OBU_FRAME_HEADER_T | OBU_REDUNDANT_FRAME_HEADER_T | OBU_FRAME_T => {
                    if self.seen_frame_header {
                        // frame_header_copy(): identical to the active header
                        if obu_type != OBU_FRAME_T {
                            continue;
                        }
                    }
                    let mut r = BitReader::new(payload);
                    let shown = self.frame_header(&mut r, temporal_id, spatial_id)?;
                    if let Some(o) = shown {
                        out.push((o, tu, spatial_id));
                        continue;
                    }
                    if obu_type == OBU_FRAME_T {
                        r.byte_align();
                        let rest = payload.get(r.byte_pos()..).ok_or(Error::Truncated)?;
                        if let Some(o) = self.tile_group(rest)? {
                            out.push((o, tu, spatial_id));
                        }
                    }
                }
                OBU_TILE_GROUP_T => {
                    if let Some(o) = self.tile_group(payload)? {
                        out.push((o, tu, spatial_id));
                    }
                }
                OBU_METADATA_T | OBU_TILE_LIST_T => {}
                _ => {}
            }
        }
        Ok(())
    }

    /// frame_header_obu( ): returns the output for show_existing_frame.
    fn frame_header(&mut self, r: &mut BitReader, temporal_id: u32, spatial_id: u32) -> Result<Option<Output>> {
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
            let idx = fh.frame_to_show_map_idx;
            let slot = self.ref_frames[idx].clone().ok_or(Error::Invalid("show_existing_frame of an empty slot"))?;
            let info = self.refs[idx].clone();
            let o = Output::Existing(Box::new(ExistingOutput {
                slot: slot.clone(),
                seq: seq.clone(),
                width: info.upscaled_width as usize,
                height: info.frame_height as usize,
                grain: fh.film_grain.clone(),
                apply_film_grain: self.apply_film_grain,
                pts: self.pts,
            }));
            if fh.frame_type == KEY_FRAME as u8 {
                // reference frame loading process (7.21) then refresh every slot (7.20)
                self.lf_deltas = info.lf_deltas;
                self.seg_features = info.seg_features;
                for i in 0..NUM_REF_FRAMES {
                    self.refs[i] = info.clone();
                    self.ref_frames[i] = Some(slot.clone());
                }
            }
            return Ok(Some(o));
        }
        // Encoders exist that code frames slightly larger than the sequence maximum (e.g. a
        // 270-line stream coded as 272 lines); like other decoders we accept them.
        if !frame_size_allowed(fh.upscaled_width, fh.frame_height) {
            return Err(Error::Invalid("frame size"));
        }
        if !fh.frame_is_intra {
            for i in 0..REFS_PER_FRAME {
                if self.ref_frames[fh.ref_frame_idx[i]].is_none() {
                    return Err(Error::Invalid("missing reference frame"));
                }
            }
        }
        if fh.primary_ref_frame != PRIMARY_REF_NONE && self.ref_frames[fh.ref_frame_idx[fh.primary_ref_frame]].is_none() {
            return Err(Error::Invalid("primary reference frame missing"));
        }
        let picture = fh.show_frame.then(Slot::new);
        let draft = self.draft && fh.show_frame && fh.refresh_frame_flags == 0;
        let tile_count = fh.tile_info.cols.checked_mul(fh.tile_info.rows).ok_or(Error::Invalid("tile count overflows"))?;
        self.pending = Some(Box::new(FrameJob {
            seq,
            fh,
            refs: self.ref_frames.clone(),
            ref_info: self.refs.clone(),
            tiles: Vec::new(),
            tile_covered: vec![false; tile_count],
            slot: Slot::new(),
            picture,
            pts: self.pts,
            apply_film_grain: self.apply_film_grain,
            draft,
            #[cfg(test)]
            force_panic: self.force_panic,
        }));
        Ok(None)
    }

    /// tile_group_obu( sz ): returns the output when the frame is complete and shown.
    fn tile_group(&mut self, data: &[u8]) -> Result<Option<Output>> {
        let mut job = self.pending.take().ok_or(Error::Invalid("tile group without a frame header"))?;
        let (cols, rows, cols_log2, rows_log2, tsb) = {
            let t = &job.fh.tile_info;
            (t.cols, t.rows, t.cols_log2, t.rows_log2, t.tile_size_bytes)
        };
        let num_tiles = cols * rows;
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
                let prefix_end = pos.checked_add(tsb as usize).ok_or(Error::Truncated)?;
                let prefix = data.get(pos..prefix_end).ok_or(Error::Truncated)?;
                let mut v = 0usize;
                for (i, &byte) in prefix.iter().enumerate() {
                    v |= usize::from(byte) << (8 * i);
                }
                pos = prefix_end;
                v.checked_add(1).ok_or(Error::Truncated)?
            };
            let end = pos.checked_add(size).filter(|&e| e <= data.len()).ok_or(Error::Truncated)?;
            if job.tile_covered.get(tile_num).copied() != Some(false) {
                return Err(Error::Invalid("tile is covered by multiple tile groups"));
            }
            job.tiles.push((tile_num, data[pos..end].to_vec()));
            job.tile_covered[tile_num] = true;
            pos = end;
        }
        if tg_end != num_tiles - 1 {
            self.pending = Some(job);
            return Ok(None);
        }
        if job.tile_covered.iter().any(|&covered| !covered) {
            return Err(Error::Invalid("frame is missing tile coverage"));
        }
        self.seen_frame_header = false;
        Ok(self.submit(job))
    }

    /// The frame's data is complete: update the reference state (7.20) and decode it (on a
    /// frame worker when there are any).
    fn submit(&mut self, job: Box<FrameJob>) -> Option<Output> {
        let fh = &job.fh;
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
                self.ref_frames[i] = Some(job.slot.clone());
            }
        }
        let output = job.picture.clone().map(Output::Decoded);
        #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
        if let Some(w) = &self.workers
            && let Some(tx) = &w.tx
        {
            let slot = job.slot.clone();
            match tx.send(job) {
                Ok(()) => {
                    self.in_flight.push_back(slot);
                    // Bound the frames in flight (memory): wait for the oldest.
                    while self.in_flight.len() > self.max_in_flight {
                        let Some(s) = self.in_flight.pop_front() else { break };
                        let _ = s.wait();
                    }
                    self.in_flight.retain(|s| !s.is_set());
                }
                Err(e) => run_job(*e.0, &self.pool, &self.stats),
            }
            return output;
        }
        run_job(*job, &self.pool, &self.stats);
        output
    }
}

/// Decode a frame job and publish its results. A panic while decoding (a decoder bug hit by
/// hostile input) becomes an error in both slots: otherwise a frame worker would die with its
/// slots unset and every later `wait` on them would block forever.
fn run_job(job: FrameJob, pool: &Pool, stats: &Mutex<DecodeStats>) {
    let mut st = DecodeStats::default();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_frame(&job, pool, &mut st)));
    match res.unwrap_or(Err(Error::Invalid("decoder panicked"))) {
        Ok((rf, pic)) => {
            if let Some(ps) = &job.picture {
                ps.set(pic.ok_or(Error::Invalid("shown frame without picture")));
            }
            job.slot.set(Ok(rf));
        }
        Err(e) => {
            if let Some(ps) = &job.picture {
                ps.set(Err(e.clone()));
            }
            job.slot.set(Err(e));
        }
    }
    if let Ok(mut s) = stats.lock() {
        s.merge(&st);
    }
}

/// The whole decoding process of one frame: setup, tiles, post-filters, motion vector storage
/// and the output picture.
fn decode_frame(job: &FrameJob, pool: &Pool, stats: &mut DecodeStats) -> Result<(Arc<RefFrame>, Option<Picture>)> {
    #[cfg(test)]
    assert!(!job.force_panic, "forced frame job panic");
    let fh = &job.fh;
    let seq = &job.seq;
    // Wait for the reference frames this frame reads.
    let mut used = [false; NUM_REF_FRAMES];
    if !fh.frame_is_intra {
        for i in 0..REFS_PER_FRAME {
            used[fh.ref_frame_idx[i]] = true;
        }
    }
    if fh.primary_ref_frame != PRIMARY_REF_NONE {
        used[fh.ref_frame_idx[fh.primary_ref_frame]] = true;
    }
    let mut refs: [Option<Arc<RefFrame>>; NUM_REF_FRAMES] = Default::default();
    for i in 0..NUM_REF_FRAMES {
        if used[i] {
            let slot = job.refs[i].as_ref().ok_or(Error::Invalid("missing reference frame"))?;
            refs[i] = Some(slot.wait()?);
        }
    }
    let t = Timer::start();
    let mut fs = FrameState::new(seq, fh);
    fs.refs = refs;
    fs.ref_info = job.ref_info.clone();
    if fh.primary_ref_frame != PRIMARY_REF_NONE {
        // load_previous_segment_ids( )
        let (mi_cols, mi_rows) = (fs.mi.cols, fs.mi.rows);
        let sh = &mut fs.sh;
        if let Some(rf) = &sh.refs[fh.ref_frame_idx[fh.primary_ref_frame]]
            && fh.seg.enabled
            && rf.mi_cols == mi_cols
            && rf.mi_rows == mi_rows
        {
            sh.prev_segment_ids.copy_from_slice(&rf.segment_ids);
        }
    }
    if fh.use_ref_frame_mvs {
        motion_field_estimation(&mut fs);
    }
    let mut cdfs = if fh.primary_ref_frame == PRIMARY_REF_NONE {
        Cdfs::new(fh.quant.base_q_idx)
    } else {
        let rf = fs.refs[fh.ref_frame_idx[fh.primary_ref_frame]].as_ref().ok_or(Error::Invalid("primary reference frame missing"))?;
        let mut c = rf.cdfs.clone();
        c.clear_counts();
        c
    };
    stats.add(Stage::Setup, t.secs());
    let t = Timer::start();
    let saved = decode_tiles(pool, &mut fs, &cdfs, &job.tiles)?;
    stats.tiles += job.tiles.len() as u64;
    stats.add(Stage::Tiles, t.secs());
    if !fh.disable_frame_end_update_cdf
        && let Some(s) = saved
    {
        cdfs = s;
    }
    crate::postfilter::apply(&mut fs, stats, pool, job.draft);
    stats.draft_frames += job.draft as u64;
    let t = Timer::start();
    stats.frames += 1;
    if fh.seg.enabled && !fh.seg.update_map {
        let prev = std::mem::take(&mut fs.prev_segment_ids);
        fs.mi.segment_id.copy_from_slice(&prev);
    }
    let (mf_refs, mf_mvs) = motion_vector_storage(&fs);
    let rf = Arc::new(RefFrame {
        buf: std::mem::take(&mut fs.cur),
        cdfs,
        bit_depth: seq.color.bit_depth,
        subsampling_x: seq.color.subsampling_x,
        subsampling_y: seq.color.subsampling_y,
        segment_ids: std::mem::take(&mut fs.mi.segment_id),
        mi_cols: fs.mi.cols,
        mi_rows: fs.mi.rows,
        film_grain_present: seq.film_grain_params_present,
        color: (seq.color.color_primaries, seq.color.transfer_characteristics, seq.color.matrix_coefficients, seq.color.color_range),
        saved_ref_frames: mf_refs,
        saved_mvs: mf_mvs,
    });
    let pic = fh.show_frame.then(|| {
        let mut p = output_picture(seq, &rf, fh.upscaled_width as usize, fh.frame_height as usize, &fh.film_grain, job.apply_film_grain, job.pts);
        p.draft = job.draft;
        p
    });
    stats.add(Stage::Output, t.secs());
    Ok((rf, pic))
}

/// Decode the tiles of a frame (tile number, data), in parallel when several; returns the final
/// CDFs of tile context_update_tile_id.
fn decode_tiles(pool: &Pool, fs: &mut FrameState, cdfs: &Cdfs, tiles: &[(usize, Vec<u8>)]) -> Result<Option<Box<Cdfs>>> {
    let ti = fs.fh.tile_info.clone();
    let ctx_tile = ti.context_update_tile_id as usize;
    let run = |sh: &FrameShared, t: &mut TileState, num: usize, data: &[u8]| -> Result<Option<Box<Cdfs>>> {
        t.intra_frame_y_mode_cdf = DEFAULT_INTRA_FRAME_Y_MODE_CDF;
        let mut td = TileDecoder::new(sh, t, data, Box::new(cdfs.clone()), num / ti.cols, num % ti.cols);
        td.decode_tile()?;
        Ok((num == ctx_tile).then_some(td.cdf))
    };
    // Intra block copy reads the current frame through the tile's own buffer: keep whole-frame
    // buffers (tiles one after another) for it.
    if tiles.len() > 1 && pool.is_parallel() && !fs.fh.allow_intrabc {
        let mut states: Vec<TileState> = tiles
            .iter()
            .map(|(num, _)| {
                let (row, col) = (num / ti.cols, num % ti.cols);
                let (r0, r1) = (ti.mi_row_starts[row] as usize, ti.mi_row_starts[row + 1] as usize);
                let (c0, c1) = (ti.mi_col_starts[col] as usize, ti.mi_col_starts[col + 1] as usize);
                fs.tile_state(r0, r1.min(fs.mi.rows), c0, c1.min(fs.mi.cols))
            })
            .collect();
        let sh = &fs.sh;
        let results = pool.map_mut(&mut states, |i, t| run(sh, t, tiles[i].0, &tiles[i].1));
        for t in &states {
            fs.merge_tile_state(t);
        }
        let mut saved = None;
        for r in results {
            if let Some(c) = r? {
                saved = Some(c);
            }
        }
        return Ok(saved);
    }
    let mut t = fs.take_tile_state();
    let mut saved = None;
    let mut res = Ok(());
    for (num, data) in tiles {
        match run(&fs.sh, &mut t, *num, data) {
            Ok(Some(c)) => saved = Some(c),
            Ok(None) => {}
            Err(e) => {
                res = Err(e);
                break;
            }
        }
    }
    fs.put_tile_state(t);
    res.map(|_| saved)
}

fn output_picture(seq: &SequenceHeader, rf: &RefFrame, w: usize, h: usize, grain: &FilmGrainParams, apply_film_grain: bool, pts: i64) -> Picture {
    let ssx = rf.subsampling_x as usize;
    let ssy = rf.subsampling_y as usize;
    let num_planes = rf.buf.num_planes;
    let mut planes: [Vec<u16>; 3] = Default::default();
    for p in 0..num_planes {
        let (pw, ph) = if p == 0 { (w, h) } else { ((w + ssx) >> ssx, (h + ssy) >> ssy) };
        let src = &rf.buf.planes[p];
        let mut v = Vec::with_capacity(pw * ph);
        for y in 0..ph {
            v.extend_from_slice(&src.row(y)[..pw]);
        }
        planes[p] = v;
    }
    let mut pic = Picture {
        width: w as u32,
        height: h as u32,
        bit_depth: rf.bit_depth,
        subsampling_x: rf.subsampling_x,
        subsampling_y: rf.subsampling_y,
        mono_chrome: num_planes == 1,
        planes,
        color_primaries: rf.color.0,
        transfer_characteristics: rf.color.1,
        matrix_coefficients: rf.color.2,
        full_range: rf.color.3,
        pts,
        draft: false,
    };
    if apply_film_grain && seq.film_grain_params_present && grain.apply_grain {
        crate::grain::apply(&mut pic, grain, seq);
    }
    pic
}

/// Motion field motion vector storage process (7.19): (MfRefFrames, MfMvs).
fn motion_vector_storage(fs: &FrameState) -> (Vec<i8>, Vec<[i32; 2]>) {
    let mi = &fs.mi;
    let n = mi.rows * mi.cols;
    let mut refs = vec![-1i8; n];
    let mut mvs = vec![[0i32; 2]; n];
    let fh = &fs.fh;
    for i in 0..n {
        for list in 0..2 {
            let r = mi.ref_frame[i][list];
            if r > INTRA_FRAME as i8 {
                let ref_idx = fh.ref_frame_idx[r as usize - LAST_FRAME];
                let dist = crate::header::relative_dist(&fs.seq, fs.ref_info[ref_idx].order_hint, fh.order_hint);
                if dist < 0 {
                    let m = mi.mv[i][list];
                    let (row, col) = (m.row as i32, m.col as i32);
                    if row.abs() <= REFMVS_LIMIT as i32 && col.abs() <= REFMVS_LIMIT as i32 {
                        refs[i] = r;
                        mvs[i] = [row, col];
                    }
                }
            }
        }
    }
    (refs, mvs)
}

/// Motion field estimation process (7.9).
fn motion_field_estimation(fs: &mut FrameState) {
    let w8 = fs.fh.mi_cols as usize >> 1;
    let h8 = fs.fh.mi_rows as usize >> 1;
    for r in LAST_FRAME..=ALTREF_FRAME {
        fs.motion_field[r] = vec![[crate::mvpred::INVALID_MV, crate::mvpred::INVALID_MV]; w8 * h8];
    }
    let (fh, seq) = (&fs.fh, &fs.seq);
    let after_cur = |r: usize| crate::header::relative_dist(seq, fh.order_hints[r], fh.order_hint) > 0;
    let (bwd, alt2, alt) = (after_cur(BWDREF_FRAME), after_cur(ALTREF2_FRAME), after_cur(ALTREF_FRAME));
    let last_alt = fs.ref_info[fh.ref_frame_idx[0]].saved_order_hints[ALTREF_FRAME];
    if last_alt != fh.order_hints[GOLDEN_FRAME] {
        project(fs, LAST_FRAME, -1);
    }
    let mut ref_stamp = MFMV_STACK_SIZE as i32 - 2;
    if bwd && project(fs, BWDREF_FRAME, 1) {
        ref_stamp -= 1;
    }
    if alt2 && project(fs, ALTREF2_FRAME, 1) {
        ref_stamp -= 1;
    }
    if alt && ref_stamp >= 0 && project(fs, ALTREF_FRAME, 1) {
        ref_stamp -= 1;
    }
    if ref_stamp >= 0 {
        project(fs, LAST2_FRAME, -1);
    }
}

fn get_mv_projection(mv: [i32; 2], numerator: i32, denominator: i32) -> [i32; 2] {
    let den = denominator.min(MAX_FRAME_DISTANCE as i32);
    let num = numerator.clamp(-(MAX_FRAME_DISTANCE as i32), MAX_FRAME_DISTANCE as i32);
    let mut out = [0i32; 2];
    for i in 0..2 {
        let scaled = crate::mvpred::round2_signed64(mv[i] as i64 * num as i64 * DIV_MULT[den as usize] as i64, 14);
        out[i] = (scaled as i32).clamp(-(1 << 14) + 1, (1 << 14) - 1);
    }
    out
}

fn project_pos(v8: i32, delta: i32, dst_sign: i32, max8: i32, max_off8: i32) -> Option<i32> {
    let base8 = (v8 >> 3) << 3;
    let offset8 = if delta >= 0 { delta >> (3 + 1 + 2) } else { -((-delta) >> (3 + 1 + 2)) };
    let v = v8 + dst_sign * offset8;
    if v < 0 || v >= max8 || v < base8 - max_off8 || v >= base8 + 8 + max_off8 { None } else { Some(v) }
}

/// Projection process (7.9.2).
fn project(fs: &mut FrameState, src: usize, dst_sign: i32) -> bool {
    let FrameShared { seq, fh, refs, ref_info, motion_field, .. } = &mut fs.sh;
    let src_idx = fh.ref_frame_idx[src - LAST_FRAME];
    let w8 = fh.mi_cols as i32 >> 1;
    let h8 = fh.mi_rows as i32 >> 1;
    let ri = &ref_info[src_idx];
    if ri.mi_rows != fh.mi_rows || ri.mi_cols != fh.mi_cols || ri.frame_type == INTRA_ONLY_FRAME as u8 || ri.frame_type == KEY_FRAME as u8 {
        return false;
    }
    let Some(rf) = &refs[src_idx] else { return false };
    let rd = |a: u32, b: u32| crate::header::relative_dist(seq, a, b);
    let mi_cols = fh.mi_cols as usize;
    for y8 in 0..h8 {
        for x8 in 0..w8 {
            let row = (2 * y8 + 1) as usize;
            let col = (2 * x8 + 1) as usize;
            let src_ref = rf.saved_ref_frames[row * mi_cols + col];
            if src_ref > INTRA_FRAME as i8 {
                let ref_to_cur = rd(fh.order_hints[src], fh.order_hint);
                let ref_offset = rd(fh.order_hints[src], ri.saved_order_hints[src_ref as usize]);
                let pos_valid = ref_to_cur.abs() <= MAX_FRAME_DISTANCE as i32 && ref_offset.abs() <= MAX_FRAME_DISTANCE as i32 && ref_offset > 0;
                if pos_valid {
                    let mv = rf.saved_mvs[row * mi_cols + col];
                    let proj = get_mv_projection(mv, ref_to_cur * dst_sign, ref_offset);
                    let py = project_pos(y8, proj[0], dst_sign, h8, MAX_OFFSET_HEIGHT as i32);
                    let px = project_pos(x8, proj[1], dst_sign, w8, MAX_OFFSET_WIDTH as i32);
                    if let (Some(py), Some(px)) = (py, px) {
                        for dst in LAST_FRAME..=ALTREF_FRAME {
                            let ref_to_dst = rd(fh.order_hint, fh.order_hints[dst]);
                            let pm = get_mv_projection(mv, ref_to_dst, ref_offset);
                            motion_field[dst][(py * w8 + px) as usize] = pm;
                        }
                    }
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::TileInfo;

    fn tile_group_decoder(tile_size_bytes: u32) -> Decoder {
        let mut decoder = Decoder::with_threads(1);
        decoder.pending = Some(Box::new(FrameJob {
            seq: SequenceHeader::default(),
            fh: FrameHeader { tile_info: TileInfo { cols: 3, rows: 1, cols_log2: 2, tile_size_bytes, ..Default::default() }, ..Default::default() },
            refs: Default::default(),
            ref_info: Default::default(),
            tiles: Vec::new(),
            tile_covered: vec![false; 3],
            slot: Slot::new(),
            picture: None,
            pts: 0,
            apply_film_grain: false,
            draft: false,
            #[cfg(test)]
            force_panic: false,
        }));
        decoder
    }

    #[test]
    fn hostile_lengths_tile_size_does_not_wrap() {
        // #2801: tile_size_minus_1 + 1 overflowed usize on wasm32. A partial group
        // (tiles 0..1 of three) isolates parsing without submitting synthetic tile data.
        let result = std::panic::catch_unwind(|| tile_group_decoder(4).tile_group(&[0x88, 0xff, 0xff, 0xff, 0xff]));
        assert!(matches!(result.expect("tile size parsing must not panic"), Err(Error::Truncated)));
    }

    #[test]
    fn hostile_lengths_obu_size_does_not_truncate() {
        // #2801: the LEB128 size 2^32 became zero on wasm32, accepting a truncated OBU.
        let result = std::panic::catch_unwind(|| Decoder::with_threads(1).decode(&[0x7a, 0x80, 0x80, 0x80, 0x80, 0x10]));
        assert!(matches!(result.expect("OBU size parsing must not panic"), Err(Error::Truncated)));
    }

    #[test]
    fn hostile_lengths_obu_size_requires_termination() {
        let mut data = [0x80; 9];
        data[0] = 0x7a; // Padding OBU with eight unterminated size bytes.
        let result = std::panic::catch_unwind(|| Decoder::with_threads(1).decode(&data));
        assert!(matches!(result.expect("OBU size parsing must not panic"), Err(Error::Truncated)));
        data[8] = 0; // A terminated eight-byte encoding of zero remains valid.
        assert!(Decoder::with_threads(1).decode(&data).unwrap().is_empty());
    }

    #[test]
    fn hostile_lengths_tile_mutations_and_truncations() {
        for width in 1..=4 {
            let mut data = vec![0x88]; // tiles 0..1 of three
            data.extend_from_slice(&0u32.to_le_bytes()[..width]); // first tile has one byte
            data.extend([0xab, 0xcd]);
            let mut decoder = tile_group_decoder(width as u32);
            assert!(decoder.tile_group(&data).unwrap().is_none());
            assert_eq!(decoder.pending.as_ref().unwrap().tiles, vec![(0, vec![0xab]), (1, vec![0xcd])]);
            for end in 0..=data.len() {
                std::panic::catch_unwind(|| tile_group_decoder(width as u32).tile_group(&data[..end])).expect("truncated tile group must not panic").ok();
            }
            // Keep the group header unchanged so mutations cannot submit a decoding job.
            for index in 1..=width {
                for bit in 0..8 {
                    let mut mutated = data.clone();
                    mutated[index] ^= 1 << bit;
                    std::panic::catch_unwind(|| tile_group_decoder(width as u32).tile_group(&mutated)).expect("mutated tile length must not panic").ok();
                }
            }
        }
    }

    #[test]
    fn hostile_lengths_obu_mutations_and_truncations() {
        // Padding OBUs are ignored, but their declared size must still fit the input.
        let data = [0x7a, 3, 0, 0, 0, 0x12, 0]; // padding, then a temporal delimiter
        assert!(Decoder::with_threads(1).decode(&data).unwrap().is_empty());
        for end in 0..=data.len() {
            std::panic::catch_unwind(|| Decoder::with_threads(1).decode(&data[..end])).expect("truncated OBU must not panic").ok();
        }
        for size in [u32::MAX as u64, 1u64 << 32, (1u64 << 32) + 1, (1u64 << 56) - 1] {
            let mut data = vec![0x7a];
            let mut remaining = size;
            loop {
                let byte = (remaining & 0x7f) as u8;
                remaining >>= 7;
                data.push(byte | if remaining == 0 { 0 } else { 0x80 });
                if remaining == 0 {
                    break;
                }
            }
            data.push(0); // the low 32 bits of 2^32 + 1 would fit this payload
            let result = std::panic::catch_unwind(|| Decoder::with_threads(1).decode(&data));
            assert!(matches!(result.expect("oversized OBU must not panic"), Err(Error::Truncated)));
            for index in 1..data.len() - 1 {
                for bit in 0..8 {
                    let mut mutated = data.clone();
                    mutated[index] ^= 1 << bit;
                    std::panic::catch_unwind(|| Decoder::with_threads(1).decode(&mutated)).expect("mutated OBU length must not panic").ok();
                }
            }
        }
    }

    /// A 16x16 libaom key frame: temporal delimiter, sequence header, frame OBU.
    const KEY_FRAME_16X16: &[u8] = &[
        0x12, 0x00, 0x0a, 0x0a, 0x00, 0x00, 0x00, 0x01, 0x9f, 0xf9, 0xf5, 0xf3, 0x00, 0x80, 0x32, 0x0b, 0x10, 0x00, 0xc0, 0x00, 0x00, 0x02, 0x80, 0x00, 0x00,
        0x00, 0x9c,
    ];

    fn decode_key_frame(threads: usize, force_panic: bool) -> crate::Result<usize> {
        let mut dec = Decoder::with_threads(threads);
        dec.force_panic = force_panic;
        let mut n = dec.decode(KEY_FRAME_16X16)?.len();
        n += dec.flush_result()?.len();
        Ok(n)
    }

    /// `f()` on another thread; a hang fails the test instead of blocking it.
    fn within_a_minute<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(60)).expect("decoder hung after a frame job panicked")
    }

    #[test]
    fn a_panicking_frame_job_is_an_error_not_a_hang() {
        for threads in [1, 3] {
            assert_eq!(decode_key_frame(threads, false), Ok(1), "threads {threads}");
            assert_eq!(within_a_minute(move || decode_key_frame(threads, true)), Err(Error::Invalid("decoder panicked")), "threads {threads}");
        }
    }

    #[test]
    fn the_decoder_recovers_at_the_next_key_frame_after_a_panic() {
        let n = within_a_minute(|| {
            let mut dec = Decoder::with_threads(3);
            dec.force_panic = true;
            // The synchronous build reports the error from decode(); the threaded
            // build may report it from decode() or from flush_result().
            let first = dec.decode(KEY_FRAME_16X16).map(|_| ());
            let second = dec.flush_result().map(|_| ());
            assert!(first.is_err() || second.is_err(), "the panicking job reported no error");
            dec.force_panic = false;
            let mut pics = dec.decode(KEY_FRAME_16X16).expect("decode");
            pics.extend(dec.flush_result().expect("flush"));
            pics.len()
        });
        assert_eq!(n, 1);
    }

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
