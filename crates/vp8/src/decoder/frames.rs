//! Frames on worker threads (the `threads` feature). [`Decoder::decode`] reads a frame's header
//! and first partition on the calling thread (the next frame's probabilities and segment map
//! depend on them) and hands the frame to a pipeline of threads:
//!
//! - token readers ([`readers`]): each reads the token partitions (13) of a whole
//!   frame, so that several frames are read at once (their tokens depend on nothing outside
//!   them), and hands the coefficients on row by row;
//! - for the luma plane and for the chroma planes (whose prediction, residue and loop filter
//!   read and change only their own samples) a reconstruction and a loop filter thread each:
//!   - the reconstruction: the frames in order, each from the reference frames left by the one
//!     before, its rows as they are read, each into a buffer of its own under a copy of the
//!     unfiltered last line of the row above (what intra prediction reads, 12);
//!   - the loop filter: the rows into the frame as they come, each filtered (15) at once. At
//!     the end of a frame the luma one puts the planes together and updates the reference
//!     frames (9.7, 9.8) for the next frame.
//!
//! [`Decoder::decode`] crops the finished frames into pictures. The frames only meet as
//! finished, shared reference frames: no stage writes what another reads.
//!
//! A stage that panics on a frame abandons that frame (it comes out with no picture, as a frame
//! that fails to decode does) and goes on with the next: no other stage is left waiting for its
//! rows. A thread that ends anyway closes its channels, so the caller never waits for it.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;

use super::{
    CoeffProbs, Frame, FrameHeader, FrameParams, InterRecon, MbRec, MbResidual, Refs, filter_level, loop_filter_row, loop_filter_rows, picture, read_mb_tokens,
    reconstruct_mb, updated_refs,
};
use crate::Picture;
use crate::boolcoder::BoolDecoder;
use crate::error::{Error, Result};

/// A frame whose first partition is read, for the worker threads.
pub(super) struct Job {
    pub(super) f: FrameParams,
    /// The coefficient probabilities of the frame (its header's updates included).
    pub(super) coeff: Box<CoeffProbs>,
    pub(super) data: Arc<[u8]>,
    /// Where in `data` the token partitions are.
    pub(super) parts: Vec<std::ops::Range<usize>>,
    /// The prediction records of each row.
    pub(super) recs: Vec<Vec<MbRec>>,
    /// Visible size (pixels) and size in macroblocks.
    pub(super) size: (usize, usize),
    pub(super) mb: (usize, usize),
    pub(super) pts: i64,
    pub(super) show: bool,
}

/// A frame in the pipeline: its job and its rows of coefficients as they are read, of the luma
/// blocks (and Y2) and of the chroma blocks.
struct Work {
    job: Job,
    rows: [Rows<PackedRow>; 2],
}

impl Work {
    fn abandon(&self) {
        self.rows.iter().for_each(Rows::abandon);
    }
}

/// The blocks of a macroblock each plane's stages read: 0..16 and Y2 (24) luma, 16..24 chroma.
const PLANE_BLOCKS: [u32; 2] = [0xffff | 1 << 24, 0xff << 16];

/// The coefficients of one row of macroblocks, only of the blocks that have any: for each
/// macroblock the blocks present (bit b: block b) and whether it has coefficients (13).
struct PackedRow {
    mbs: Vec<(u32, bool)>,
    blocks: Vec<[i16; 16]>,
}

impl PackedRow {
    fn with_capacity(mbs: usize) -> PackedRow {
        PackedRow { mbs: Vec::with_capacity(mbs), blocks: Vec::with_capacity(mbs * 4) }
    }

    /// The coefficients of `r` into the rows of the planes (`rows[p]` the blocks of
    /// `PLANE_BLOCKS[p]`), leaving `r` zero.
    fn take(rows: &mut [PackedRow; 2], r: &mut MbResidual) {
        let mut present = [0u32; 2];
        for (b, (c, nz)) in r.coeffs.iter_mut().zip(r.nonzero.iter_mut()).enumerate().filter(|(_, (_, nz))| **nz) {
            let p = usize::from(PLANE_BLOCKS[0] >> b & 1 == 0);
            present[p] |= 1 << b;
            if let Some(row) = rows.get_mut(p) {
                row.blocks.push(std::mem::take(c));
            }
            *nz = false;
        }
        for (row, present) in rows.iter_mut().zip(present) {
            row.mbs.push((present, r.has_coeffs));
        }
        r.has_coeffs = false;
    }

    /// The coefficients of macroblock `i` into `r` (zero), whose blocks start at `*next`; the
    /// blocks they went to (bit b: block b), for [`MbResidual::clear_blocks`].
    fn unpack(&self, i: usize, next: &mut usize, r: &mut MbResidual) -> u32 {
        let Some(&(present, has_coeffs)) = self.mbs.get(i) else { return 0 };
        let mut left = present;
        while left != 0 {
            let b = left.trailing_zeros() as usize;
            left &= left - 1;
            if let (Some(block), Some(c), Some(nz)) = (self.blocks.get(*next), r.coeffs.get_mut(b), r.nonzero.get_mut(b)) {
                *c = *block;
                *nz = true;
            }
            *next += 1;
        }
        r.has_coeffs = has_coeffs;
        present
    }
}

/// Rows handed from one stage to the next, by row index: [`Rows::take`] waits for its row.
struct Rows<T> {
    slots: Mutex<Slots<T>>,
    ready: Condvar,
}

struct Slots<T> {
    /// The rows not yet taken.
    rows: Vec<Option<T>>,
    abandoned: bool,
    /// Whether a stage waits for a row (only then is it woken: a wake-up is a system call).
    waiting: bool,
}

impl<T> Rows<T> {
    fn new(n: usize) -> Rows<T> {
        Rows { slots: Mutex::new(Slots { rows: (0..n).map(|_| None).collect(), abandoned: false, waiting: false }), ready: Condvar::new() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Slots<T>> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn put(&self, y: usize, row: T) {
        let mut s = self.lock();
        if let Some(slot) = s.rows.get_mut(y) {
            *slot = Some(row);
        }
        if s.waiting {
            self.ready.notify_all();
        }
    }

    /// Row `y` once it is there; None when the frame was abandoned first.
    fn take(&self, y: usize) -> Option<T> {
        let mut s = self.lock();
        loop {
            if let Some(row) = s.rows.get_mut(y).and_then(Option::take) {
                return Some(row);
            }
            if s.abandoned {
                return None;
            }
            s.waiting = true;
            s = self.ready.wait(s).unwrap_or_else(PoisonError::into_inner);
            s.waiting = false;
        }
    }

    fn abandon(&self) {
        self.lock().abandoned = true;
        self.ready.notify_all();
    }

    fn abandoned(&self) -> bool {
        self.lock().abandoned
    }
}

/// One plane's part (the luma plane, or the chroma planes) of a reconstructed row on its way to
/// its loop filter: in the two-row buffer `buf` (its other planes empty) from luma row `first`
/// (0 for the frame's first row, 16 for the others, under the line above), with the loop filter
/// level and inner edge flag of each macroblock.
struct PlaneRow {
    buf: Frame,
    first: usize,
    lf: Arc<[(u8, bool)]>,
}

/// From the reconstruction to a loop filter thread, in order.
enum ToFilter {
    /// A frame begins.
    Start(Arc<Work>),
    /// Its next rows (a few at a time: each message may wake the thread, a system call).
    Rows(Vec<PlaneRow>),
    /// The frame ends: whole, or not (its reconstruction failed or was abandoned).
    End(bool),
}

/// A finished frame for [`Decoder::decode`]: its picture to crop when it is shown.
struct Finished {
    frame: Option<Arc<Frame>>,
    size: (usize, usize),
    pts: i64,
    key: bool,
    color_space: u8,
}

/// Plane buffers handed back for reuse: the two-row buffers from the loop filters to the
/// reconstruction, and the planes of frames no reference keeps (luma ones from the luma loop
/// filter to itself, chroma ones to the chroma loop filter).
#[derive(Default)]
struct Spare {
    rows_y: Mutex<Vec<Vec<u8>>>,
    rows_uv: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
    frames_y: Mutex<Vec<Vec<u8>>>,
    frames_uv: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
}

/// A buffer of `len` samples: `spare`'s last if it is that long (all its samples are
/// overwritten), else a new one.
fn reuse(spare: &Mutex<Vec<Vec<u8>>>, len: usize) -> Vec<u8> {
    let v = spare.lock().unwrap_or_else(PoisonError::into_inner).pop();
    v.filter(|v| v.len() == len).unwrap_or_else(|| vec![0; len])
}

fn reuse2(spare: &Mutex<Vec<(Vec<u8>, Vec<u8>)>>, len: usize) -> (Vec<u8>, Vec<u8>) {
    let v = spare.lock().unwrap_or_else(PoisonError::into_inner).pop();
    v.filter(|(u, v)| u.len() == len && v.len() == len).unwrap_or_else(|| (vec![0; len], vec![0; len]))
}

fn give_back<T>(spare: &Mutex<Vec<T>>, v: T) {
    let mut s = spare.lock().unwrap_or_else(PoisonError::into_inner);
    // a few are enough: one per row or frame in flight
    if s.len() < 64 {
        s.push(v);
    }
}

/// Frames in flight at most: enough for the token readers to read ahead (each frame is read on
/// one of them), about 3 MB per 1080p frame. A caller pairing the pictures of two decoders (the
/// WebM alpha layer) holds more than this many.
pub(crate) const MAX_IN_FLIGHT: usize = 12;

/// Token readers besides the 4 threads of the planes: those left, but at least half of
/// `threads` (each reads a whole frame, the slowest stage of all).
fn readers(threads: usize) -> usize {
    threads.saturating_sub(4).max(threads / 2).max(1)
}

/// The worker threads and the frames in flight.
pub(super) struct Pipeline {
    threads: usize,
    to_readers: Option<Sender<Arc<Work>>>,
    /// To the luma and the chroma reconstruction.
    to_recon: Option<[Sender<Arc<Work>>; 2]>,
    /// The outcome of each frame, in order.
    out: Receiver<Finished>,
    in_flight: VecDeque<Arc<Work>>,
    handles: Vec<JoinHandle<()>>,
}

impl Pipeline {
    /// The threads ([`readers`] token readers); None when they cannot be started.
    pub(super) fn new(threads: usize) -> Option<Pipeline> {
        let (to_readers, jobs) = channel::<Arc<Work>>();
        let jobs = Arc::new(Mutex::new(jobs));
        let (to_recon_y, frames_y) = channel::<Arc<Work>>();
        let (to_recon_uv, frames_uv) = channel::<Arc<Work>>();
        let (refs_y, refs_rx_y) = channel::<Refs>();
        let (refs_uv, refs_rx_uv) = channel::<Refs>();
        let (to_luma, luma_rx) = channel::<ToFilter>();
        let (to_chroma, chroma_rx) = channel::<ToFilter>();
        let (chroma_tx, chroma_done) = channel::<(Vec<u8>, Vec<u8>, bool)>();
        let (out_tx, out) = channel::<Finished>();
        let spare = Arc::new(Spare::default());
        let mut handles = Vec::new();
        // A thread that panics outside the frame it works on just ends: its channels close, and
        // the stages after it, then the caller, see that instead of waiting.
        let spawn = |name: String, f: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new()
                .name(name)
                .spawn(move || {
                    let _ = catch_unwind(AssertUnwindSafe(f));
                })
                .ok()
        };
        for i in 0..readers(threads) {
            let jobs = jobs.clone();
            handles.push(spawn(
                format!("vp8-read-{i}"),
                Box::new(move || {
                    loop {
                        let work = jobs.lock().unwrap_or_else(PoisonError::into_inner).recv();
                        let Ok(work) = work else { return };
                        if catch_unwind(AssertUnwindSafe(|| read_tokens(&work))).is_err() {
                            work.abandon();
                        }
                    }
                }),
            )?);
        }
        let s = spare.clone();
        handles.push(spawn("vp8-recon-y".into(), Box::new(move || reconstruct_frames::<true>(&frames_y, &refs_rx_y, &to_luma, &s)))?);
        let s = spare.clone();
        handles.push(spawn("vp8-recon-uv".into(), Box::new(move || reconstruct_frames::<false>(&frames_uv, &refs_rx_uv, &to_chroma, &s)))?);
        let s = spare.clone();
        handles.push(spawn("vp8-filter-uv".into(), Box::new(move || filter_chroma(&chroma_rx, &chroma_tx, &s)))?);
        handles.push(spawn("vp8-filter-y".into(), Box::new(move || filter_luma(&luma_rx, &chroma_done, [&refs_y, &refs_uv], &out_tx, &spare)))?);
        Some(Pipeline { threads, to_readers: Some(to_readers), to_recon: Some([to_recon_y, to_recon_uv]), out, in_flight: VecDeque::new(), handles })
    }

    pub(super) fn threads(&self) -> usize {
        self.threads
    }

    /// Hands `job` to the threads; returns the pictures finished so far.
    pub(super) fn submit(&mut self, job: Job) -> Result<Vec<Picture>> {
        let work = Arc::new(Work { rows: [Rows::new(job.mb.1), Rows::new(job.mb.1)], job });
        let sent = self.to_readers.as_ref().is_some_and(|t| t.send(work.clone()).is_ok())
            && self.to_recon.as_ref().is_some_and(|ts| ts.iter().all(|t| t.send(work.clone()).is_ok()));
        if !sent {
            return Err(Error::Invalid("the decoder threads stopped".into()));
        }
        self.in_flight.push_back(work);
        Ok(self.drain(false))
    }

    /// The pictures of the frames at the front that are finished (`all`: of every frame in
    /// flight). Beyond [`MAX_IN_FLIGHT`] frames in flight (fewer with fewer threads), the oldest
    /// is waited for.
    pub(super) fn drain(&mut self, all: bool) -> Vec<Picture> {
        let max_in_flight = (self.threads + 1).clamp(2, MAX_IN_FLIGHT);
        let mut pictures = Vec::new();
        while !self.in_flight.is_empty() {
            let next = if all || self.in_flight.len() > max_in_flight {
                self.out.recv().ok()
            } else {
                match self.out.try_recv() {
                    Ok(p) => Some(p),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => None,
                }
            };
            let Some(done) = next else {
                // the threads are gone: nothing more comes out
                self.in_flight.clear();
                break;
            };
            self.in_flight.pop_front();
            if let Some(frame) = &done.frame {
                pictures.push(picture(frame, done.size, done.pts, done.key, done.color_space));
            }
        }
        pictures
    }
}

impl Drop for Pipeline {
    /// Abandons the frames in flight and waits for the threads to end.
    fn drop(&mut self) {
        for work in &self.in_flight {
            work.abandon();
        }
        self.to_readers = None;
        self.to_recon = None;
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

/// The tokens of every row of a frame (any number of partitions).
fn read_tokens(work: &Work) {
    let job = &work.job;
    let mut parts: Vec<BoolDecoder> = job.parts.iter().map(|r| BoolDecoder::new(job.data.get(r.clone()).unwrap_or(&[]))).collect();
    let n = parts.len();
    let mut above_nz = vec![[0u8; 9]; job.mb.0];
    let mut res = MbResidual::default();
    for (mb_y, recs) in job.recs.iter().enumerate() {
        let Some(td) = parts.get_mut(mb_y % n.max(1)).filter(|_| !work.rows[0].abandoned()) else {
            work.abandon();
            return;
        };
        #[cfg(test)]
        fault::hit(fault::Stage::Read, job.pts, mb_y);
        let mut left_nz = [0u8; 9];
        let mut rows = PLANE_BLOCKS.map(|_| PackedRow::with_capacity(recs.len()));
        for (rec, an) in recs.iter().zip(above_nz.iter_mut()) {
            read_mb_tokens(td, &job.f, &job.coeff, rec, (an, &mut left_nz), &mut res);
            PackedRow::take(&mut rows, &mut res);
        }
        for (r, row) in work.rows.iter().zip(rows) {
            r.put(mb_y, row);
        }
    }
}

/// The frames in order (of the luma plane with `LUMA`, else of the chroma planes), each from the
/// reference frames the luma loop filter thread left after the one before.
fn reconstruct_frames<const LUMA: bool>(frames: &Receiver<Arc<Work>>, refs: &Receiver<Refs>, to_filter: &Sender<ToFilter>, spare: &Spare) {
    let mut res = MbResidual::default();
    while let Ok(work) = frames.recv() {
        let Ok(refs) = refs.recv() else { return };
        if to_filter.send(ToFilter::Start(work.clone())).is_err() {
            return;
        }
        let whole = match catch_unwind(AssertUnwindSafe(|| reconstruct_rows::<LUMA>(&work, refs, to_filter, spare, &mut res))) {
            Ok(r) => r.is_ok(),
            Err(_) => {
                // Its rows may be left half unpacked; the other planes' stages stop too.
                res = MbResidual::default();
                work.abandon();
                false
            }
        };
        res.clear();
        if to_filter.send(ToFilter::End(whole)).is_err() {
            return;
        }
    }
}

/// The rows of a frame's plane(s) as they are read, each into a buffer of its own: the
/// unfiltered last lines of the row above are kept for intra prediction.
fn reconstruct_rows<const LUMA: bool>(work: &Work, refs: Refs, to_filter: &Sender<ToFilter>, spare: &Spare, res: &mut MbResidual) -> Result<()> {
    let job = &work.job;
    let (mb_w, mb_h) = job.mb;
    let (w, cw) = (mb_w * 16, mb_w * 8);
    let recon = InterRecon { refs, filters: job.f.filters, full_pixel: job.f.full_pixel };
    let mut above = if LUMA { (vec![0u8; w], Vec::new(), Vec::new()) } else { (Vec::new(), vec![0u8; cw], vec![0u8; cw]) };
    let mut batch = Vec::new();
    for mb_y in 0..mb_h {
        let (Some(row), Some(recs)) = (work.rows[usize::from(!LUMA)].take(mb_y), job.recs.get(mb_y)) else {
            return Err(Error::Invalid("frame abandoned".into()));
        };
        let mut buf = if LUMA {
            Frame { y: reuse(&spare.rows_y, 32 * w), u: Vec::new(), v: Vec::new(), w, h: 32 }
        } else {
            let (u, v) = reuse2(&spare.rows_uv, 16 * cw);
            Frame { y: Vec::new(), u, v, w, h: 32 }
        };
        // The frame's first row at the buffer's first row (where intra prediction sees the top
        // of the frame), the others at its second, under the line above.
        let first = if mb_y == 0 { 0 } else { 16 };
        if mb_y > 0 {
            copy_lines(&mut buf.y, 15 * w, &above.0, 0, above.0.len());
            copy_lines(&mut buf.u, 7 * cw, &above.1, 0, above.1.len());
            copy_lines(&mut buf.v, 7 * cw, &above.2, 0, above.2.len());
        }
        #[cfg(test)]
        fault::hit(fault::Stage::Recon(LUMA), job.pts, mb_y);
        let top = mb_y * 16 - first;
        let mut lf = Vec::with_capacity(mb_w);
        let mut next = 0;
        for (mb_x, rec) in recs.iter().enumerate() {
            let blocks = row.unpack(mb_x, &mut next, res);
            lf.push((filter_level(&job.f, rec.segment, &rec.modes.info), !rec.has_y2() || res.has_coeffs));
            if LUMA {
                reconstruct_mb::<true, false>(&mut buf, (mb_x, mb_y), top, mb_w, &rec.modes, res, &recon)?;
            } else {
                reconstruct_mb::<false, true>(&mut buf, (mb_x, mb_y), top, mb_w, &rec.modes, res, &recon)?;
            }
            res.clear_blocks(blocks);
        }
        let n = above.0.len();
        copy_lines(&mut above.0, 0, &buf.y, (first + 15) * w, n);
        let n = above.1.len();
        copy_lines(&mut above.1, 0, &buf.u, (first / 2 + 7) * cw, n);
        copy_lines(&mut above.2, 0, &buf.v, (first / 2 + 7) * cw, n);
        batch.push(PlaneRow { buf, first, lf: lf.into() });
        if (batch.len() == 4 || mb_y + 1 == mb_h) && to_filter.send(ToFilter::Rows(std::mem::take(&mut batch))).is_err() {
            return Err(Error::Invalid("a loop filter thread stopped".into()));
        }
    }
    Ok(())
}

/// What a loop filter thread does with each frame: `plane` the rows into its plane(s) as they
/// come, each filtered at once; the frame's planes and whether it is whole at its end. A row
/// that panics leaves the frame broken: not whole at its end.
fn filter_rows<P>(rows: &Receiver<ToFilter>, mut plane: P, mut end: impl FnMut(Arc<Work>, Frame, bool) -> bool)
where
    P: FnMut(&mut Frame, usize, PlaneRow, &Job),
{
    // The frame, its next row, and whether a row of it broke.
    let mut cur: Option<(Arc<Work>, Frame, usize, bool)> = None;
    while let Ok(msg) = rows.recv() {
        match msg {
            ToFilter::Start(work) => {
                let (mb_w, mb_h) = work.job.mb;
                cur = Some((work, Frame { y: Vec::new(), u: Vec::new(), v: Vec::new(), w: mb_w * 16, h: mb_h * 16 }, 0, false));
            }
            ToFilter::Rows(rows) => {
                let Some((work, frame, mb_y, broken)) = &mut cur else { continue };
                for row in rows {
                    if !*broken && catch_unwind(AssertUnwindSafe(|| plane(frame, *mb_y, row, &work.job))).is_err() {
                        *broken = true;
                        work.abandon();
                    }
                    *mb_y += 1;
                }
            }
            ToFilter::End(whole) => {
                let Some((work, frame, _, broken)) = cur.take() else { continue };
                if !end(work, frame, whole && !broken) {
                    return;
                }
            }
        }
    }
}

/// The luma loop filter thread, which also finishes the frames: their chroma planes from the
/// other loop filter, the reference frames for the next frame, the finished frame out.
fn filter_luma(rows: &Receiver<ToFilter>, chroma: &Receiver<(Vec<u8>, Vec<u8>, bool)>, refs_tx: [&Sender<Refs>; 2], out: &Sender<Finished>, spare: &Spare) {
    let mut refs: Refs = [None, None, None];
    let send_refs = |refs: &Refs| refs_tx.iter().all(|t| t.send(refs.clone()).is_ok());
    if !send_refs(&refs) {
        return;
    }
    let mut upper = None;
    let plane = |frame: &mut Frame, mb_y: usize, row: PlaneRow, job: &Job| {
        #[cfg(test)]
        fault::hit(fault::Stage::Filter(true), job.pts, mb_y);
        let (hdr, key_frame) = (&job.f.hdr, job.f.key_frame);
        let w = frame.w;
        if frame.y.is_empty() {
            frame.y = reuse(&spare.frames_y, w * frame.h);
        }
        copy_lines(&mut frame.y, mb_y * 16 * w, &row.buf.y, row.first * w, 16 * w);
        give_back(&spare.rows_y, row.buf.y);
        if hdr.filter_level > 0 {
            filter_in_pairs::<true, false>(frame, (mb_y, row.lf), &mut upper, hdr, key_frame);
        }
    };
    let end = |work: Arc<Work>, mut frame: Frame, whole: bool| {
        let Ok((u, v, chroma_whole)) = chroma.recv() else { return false };
        (frame.u, frame.v) = (u, v);
        let whole = whole && chroma_whole;
        let job = &work.job;
        // Whatever happens here, the references and the frame's outcome go out: the
        // reconstruction waits for the one, the caller for the other.
        let finish = || {
            #[cfg(test)]
            fault::hit(fault::Stage::Finish, job.pts, 0);
            let frame = Arc::new(frame);
            let new = updated_refs(&refs, &job.f.hdr, &frame);
            let old = std::mem::replace(&mut refs, new);
            // the planes of the frames no reference keeps any more, for later frames
            for f in old.into_iter().flatten().filter_map(|f| Arc::try_unwrap(f).ok()) {
                give_back(&spare.frames_y, f.y);
                give_back(&spare.frames_uv, (f.u, f.v));
            }
            frame
        };
        let frame = if whole { catch_unwind(AssertUnwindSafe(finish)).ok() } else { None };
        let done = Finished { frame: frame.filter(|_| job.show), size: job.size, pts: job.pts, key: job.f.key_frame, color_space: job.f.hdr.color_space };
        send_refs(&refs) && out.send(done).is_ok()
    };
    filter_rows(rows, plane, end);
}

/// The chroma loop filter thread: its planes of each frame, and whether they are whole, to the
/// luma one at the end.
fn filter_chroma(rows: &Receiver<ToFilter>, done: &Sender<(Vec<u8>, Vec<u8>, bool)>, spare: &Spare) {
    let mut upper = None;
    let plane = |frame: &mut Frame, mb_y: usize, row: PlaneRow, job: &Job| {
        #[cfg(test)]
        fault::hit(fault::Stage::Filter(false), job.pts, mb_y);
        let (hdr, key_frame) = (&job.f.hdr, job.f.key_frame);
        let cw = frame.w / 2;
        if frame.u.is_empty() {
            (frame.u, frame.v) = reuse2(&spare.frames_uv, cw * frame.h / 2);
        }
        copy_lines(&mut frame.u, mb_y * 8 * cw, &row.buf.u, row.first / 2 * cw, 8 * cw);
        copy_lines(&mut frame.v, mb_y * 8 * cw, &row.buf.v, row.first / 2 * cw, 8 * cw);
        give_back(&spare.rows_uv, (row.buf.u, row.buf.v));
        if hdr.filter_level > 0 {
            filter_in_pairs::<false, true>(frame, (mb_y, row.lf), &mut upper, hdr, key_frame);
        }
    };
    filter_rows(rows, plane, |_, frame, whole| done.send((frame.u, frame.v, whole)).is_ok());
}

/// The loop filter of the rows of a frame as they arrive, two at a time ([`loop_filter_rows`]):
/// `upper` holds a row waiting for the one below (none at a frame's first row) and its levels.
fn filter_in_pairs<const LUMA: bool, const CHROMA: bool>(
    frame: &mut Frame,
    (mb_y, lf): (usize, Arc<[(u8, bool)]>),
    upper: &mut Option<Arc<[(u8, bool)]>>,
    hdr: &FrameHeader,
    key_frame: bool,
) {
    let pending = if mb_y == 0 { None } else { upper.take() };
    match pending {
        Some(above) => loop_filter_rows::<LUMA, CHROMA>(frame, mb_y - 1, [&above, &lf], hdr, key_frame),
        None if (mb_y + 1) * 16 < frame.h => *upper = Some(lf),
        None => loop_filter_row::<LUMA, CHROMA>(frame, mb_y, &lf, hdr, key_frame),
    }
}

/// `n` samples of `src` from `s` into `dst` from `d` (nothing when either does not hold them).
fn copy_lines(dst: &mut [u8], d: usize, src: &[u8], s: usize, n: usize) {
    if let (Some(dst), Some(src)) = (dst.get_mut(d..d + n), src.get(s..s + n)) {
        dst.copy_from_slice(src);
    }
}

/// Panics on purpose, at a stage of the frames (by pts) a test chose, for the panic tests.
#[cfg(test)]
pub(super) mod fault {
    use std::sync::{Mutex, PoisonError};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Stage {
        Read,
        /// The luma reconstruction (true) or the chroma one.
        Recon(bool),
        /// The luma loop filter (true) or the chroma one.
        Filter(bool),
        /// The luma loop filter finishing the frame.
        Finish,
    }

    static ARMED: Mutex<Vec<(Stage, i64)>> = Mutex::new(Vec::new());

    /// Panic at `stage` of the frame decoded with `pts`.
    pub(crate) fn arm(stage: Stage, pts: i64) {
        ARMED.lock().unwrap_or_else(PoisonError::into_inner).push((stage, pts));
    }

    /// Panics when `stage` of the frame with `pts` was armed, at its second row (`mb_y` 1), or
    /// at once for a stage without rows.
    pub(crate) fn hit(stage: Stage, pts: i64, mb_y: usize) {
        let row = if stage == Stage::Finish { 0 } else { 1 };
        if mb_y == row && ARMED.lock().unwrap_or_else(PoisonError::into_inner).contains(&(stage, pts)) {
            panic!("a panic injected at {stage:?} of the frame at {pts}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Rows;
    use super::fault::{Stage, arm};
    use crate::{Decoder, Picture};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// A stage waiting for a row returns once the frame is abandoned instead of waiting forever.
    #[test]
    fn abandoning_wakes_a_waiting_stage() {
        let rows = Rows::<u8>::new(4);
        std::thread::scope(|s| {
            let waiting = s.spawn(|| rows.take(2));
            rows.put(1, 7);
            rows.abandon();
            assert_eq!(waiting.join().unwrap(), None);
        });
        assert_eq!(rows.take(1), Some(7), "rows put before still come out");
    }

    /// A 160 x 96 clip of 12 frames, key frames at 0 and 6, in 4 token partitions, encoded by
    /// ffmpeg's libvpx; None without them.
    fn stream() -> Option<Vec<Vec<u8>>> {
        let ff = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].into_iter().map(PathBuf::from).find(|p| p.exists())?;
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/vp8-unit");
        std::fs::create_dir_all(&dir).ok()?;
        let ivf = dir.join("faults.ivf");
        if !ivf.exists() {
            let tmp = dir.join(format!("faults.{}.ivf", std::process::id()));
            let ok = Command::new(ff)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=160x96:rate=25,format=yuv420p"])
                .args(["-frames:v", "12", "-c:v", "libvpx", "-g", "6", "-slices", "4", "-b:v", "400k", "-f", "ivf"])
                .arg(&tmp)
                .status()
                .is_ok_and(|s| s.success());
            if !ok || (std::fs::rename(&tmp, &ivf).is_err() && !ivf.exists()) {
                return None;
            }
        }
        let data = std::fs::read(&ivf).ok()?;
        let mut pos = usize::from(u16::from_le_bytes([data[6], data[7]]));
        let mut frames = Vec::new();
        while pos + 12 <= data.len() {
            let size = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
            pos += 12;
            frames.push(data[pos..(pos + size).min(data.len())].to_vec());
            pos += size;
        }
        Some(frames)
    }

    /// Every frame of `frames` from pts `base` on, then flushed.
    fn decode(dec: &mut Decoder, frames: &[Vec<u8>], base: i64) -> Vec<Picture> {
        let mut out = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            out.extend(dec.decode(f, base + i as i64).unwrap_or_default());
        }
        out.extend(dec.flush());
        out
    }

    fn same(a: &Picture, b: &Picture) -> bool {
        (a.width, a.height, &a.y, &a.u, &a.v) == (b.width, b.height, &b.y, &b.u, &b.v)
    }

    /// A stage that panics costs its frame's picture, and nothing waits for it: the frames before
    /// it and from the next key frame on are the serial decoder's, and the decoder goes on.
    #[test]
    fn a_panicking_stage_costs_its_frame_and_nothing_else() {
        let Some(frames) = stream() else {
            eprintln!("skipping: no ffmpeg with libvpx");
            return;
        };
        assert_eq!(frames.len(), 12);
        let want = decode(&mut Decoder::with_threads(1), &frames, 0);
        assert_eq!(want.len(), 12);
        let stages = [Stage::Read, Stage::Recon(true), Stage::Recon(false), Stage::Filter(true), Stage::Filter(false), Stage::Finish];
        for (k, stage) in stages.into_iter().enumerate() {
            // pts no other test uses: the panics are armed for the whole process
            let base = (1i64 << 40) + 1000 * k as i64;
            let bad = 2;
            arm(stage, base + bad);
            let (tx, rx) = std::sync::mpsc::channel();
            let input = frames.clone();
            std::thread::spawn(move || {
                let mut dec = Decoder::with_threads(4);
                let got = decode(&mut dec, &input, base);
                // still usable: the stream again
                let again = decode(&mut dec, &input, 0);
                drop(dec);
                let _ = tx.send((got, again));
            });
            let (got, again) = rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap_or_else(|_| panic!("{stage:?}: the decoder hung"));
            assert_eq!(got.len(), 11, "{stage:?}: all pictures but the broken frame's");
            assert!(got.iter().all(|p| p.pts != base + bad), "{stage:?}");
            for p in &got {
                let i = (p.pts - base) as usize;
                if i < bad as usize || i >= 6 {
                    assert!(same(p, &want[i]), "{stage:?}: frame {i} differs from the serial decode");
                }
            }
            assert_eq!(again.len(), 12, "{stage:?}");
            assert!(again.iter().zip(&want).all(|(a, b)| same(a, b)), "{stage:?}: the decoder after the panic");
        }
    }
}
