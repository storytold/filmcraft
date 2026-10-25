//! GOP-parallel encoding (feature `threads`) for the rate controls whose frame QP needs no feedback
//! from earlier frames: constant QP and constant quality.
//!
//! `try_encode` keeps its serial input side: import, half-resolution analysis against the previous
//! source picture, AQ offsets, the IDR / scene-cut decision and the mini-GOP structure run on the
//! calling thread in display order, exactly as on the serial path, and every picture gets its
//! coding-order index and decoding time there. Only the coding moves. Each GOP gets a thread with
//! its own reference state, started at its IDR; the batches of the GOP (its IDR, then per mini-GOP
//! the P picture and its B pictures, then whatever the next IDR or a flush finishes) go to that
//! thread as they are formed, which codes them in that order with the same `encode_batch` calls as
//! the serial path. The next IDR closes the GOP and starts the next thread, so a GOP that falls
//! behind keeps coding while the next ones start: GOPs run concurrently as soon as the input comes
//! faster than one GOP at a time can code it. Slices still run on the encoder's pool, which all GOP
//! threads share.
//!
//! Closed GOPs make this exact: an IDR empties the DPB and restarts `frame_num` and the POC, so a
//! GOP reads nothing of the GOPs before it but a few stream counters, which its thread starts from:
//! the `idr_pic_id` parity, the reference picture numbering and whether the identification SEI went
//! out (with the first picture of the stream only). Constant QP and constant quality pick a frame's
//! QP from its type alone; the bitrate modes feed each picture's size into the next QP and stay
//! serial.
//!
//! Results come back in decoding order: a GOP's coded pictures (packets, rate-control statistics,
//! reconstructions) are taken (`Encoder::deliver`) as they are coded once every GOP before it is
//! done, so a packet never comes out before one that precedes it. Memory is bounded by
//! [`GOP_BUFFER_BYTES`]. A GOP thread that panics becomes [`Error::Worker`] for that GOP only,
//! reported by the first call after its death is noticed (a batch it cannot take, its results
//! ending, its thread finished). The failed GOP takes no more batches; when the stream would have
//! continued from it (the open GOP, or the last one of a flush), the next picture not scheduled yet
//! is coded as an IDR and the batches before it are lost with the GOP, so nothing ever refers to
//! its pictures.
//! Dropping the encoder stops every GOP thread after its current batch and waits for it, so the
//! GOPs' pictures, buffers and pool work are released by the time the drop returns.

use std::any::Any;
use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;

use crate::nal::SliceType;
use crate::ratecontrol::FixedQp;
use crate::{Batch, Coded, Ctx, Encoder, EncoderConfig, Error, Refs, encode_batch};

/// Bytes of padded pictures the GOP-parallel path may hold: the input pictures not coded yet plus
/// the working set of every GOP thread. 1 GiB holds about four 2-second GOPs of 1080p30 (60
/// pictures of 3.6 MiB each). Two GOPs of the longest kind (`keyint` pictures) must fit or the
/// encoder stays serial: 4K (13 MiB a picture) with a 2-second `keyint` does.
pub(crate) const GOP_BUFFER_BYTES: usize = 1 << 30;

/// Spare pictures the calling thread may keep besides its inputs (its frame pool).
const SPARE_FRAMES: usize = 8;

/// Stack of a GOP thread: slices usually run on the pool, a single-slice picture runs here.
const GOP_STACK: usize = 8 << 20;

/// Coded pictures of one batch, each with the reconstruction capture generation of the batch.
type Out = Vec<(Coded, Option<u64>)>;

/// The GOPs of a GOP-parallel encoder.
pub(crate) struct Gops {
    fixed: FixedQp,
    /// GOPs not taken completely yet, oldest first. The last one is open (it receives the batches)
    /// unless a flush closed it.
    gops: VecDeque<Gop>,
    /// Number of the next GOP.
    next_seq: u64,
    /// IDR and reference (I/P) pictures handed to GOPs so far: a GOP's counters start there.
    idrs: u32,
    refs: u32,
    /// The GOP the stream would continue from failed: the next picture is coded as an IDR, and
    /// pictures scheduled before it belong to the failed GOP and are lost with it.
    restart: bool,
    /// [`GOP_BUFFER_BYTES`] in padded pictures.
    budget: usize,
    /// Padded pictures a GOP thread needs besides its inputs: references with their three
    /// half-sample planes (about three pictures each), the pictures being reconstructed, spares.
    work: usize,
    /// GOP threads at once.
    max_gops: usize,
    /// The pool that analyses the input pictures (see `Encoder::analyse`); `None` if it could not
    /// be built (the encoder's pool then does it).
    analysis: Option<rayon::ThreadPool>,
    /// Set when the encoder goes away: GOP threads stop after their current batch.
    cancel: Arc<AtomicBool>,
    /// Make the GOP with this number panic on its thread.
    #[cfg(test)]
    pub(crate) plant_panic: Option<u64>,
    /// Make every GOP thread hold its batches until the encoder goes away, then take a while over
    /// the one it is coding, as a GOP busy with a long batch would.
    #[cfg(test)]
    plant_hold: bool,
    /// Make every GOP thread wait this long before it codes, as GOPs falling behind the input do.
    #[cfg(test)]
    plant_slow: std::time::Duration,
    /// The most padded pictures held after a batch was scheduled.
    #[cfg(test)]
    peak: usize,
    /// GOP threads started and not ended yet.
    #[cfg(test)]
    live: Arc<AtomicUsize>,
}

/// The test hooks a GOP thread starts with.
#[cfg(test)]
struct Plant {
    /// Panic on the thread.
    panic: bool,
    /// See [`Gops::plant_hold`].
    hold: bool,
    /// See [`Gops::plant_slow`].
    slow: std::time::Duration,
    /// Counts the thread in [`Gops::live`] until it ends.
    alive: Alive,
}

/// One GOP thread counted in [`Gops::live`], from before it is started until it ends.
#[cfg(test)]
struct Alive(Arc<AtomicUsize>);

#[cfg(test)]
impl Alive {
    /// Count one more thread in `live`.
    fn new(live: &Arc<AtomicUsize>) -> Alive {
        live.fetch_add(1, Ordering::SeqCst);
        Alive(Arc::clone(live))
    }
}

#[cfg(test)]
impl Drop for Alive {
    /// The thread ended (or was never started).
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// One GOP (or the rest of one, after a flush) and what codes it.
struct Gop {
    /// Still receiving batches.
    open: bool,
    /// Input pictures sent to the coder, and taken back coded.
    sent: usize,
    taken: usize,
    /// Pictures coded so far (their input pictures are free, their packets come out).
    coded: Arc<AtomicUsize>,
    /// [`Gops::idrs`] when the GOP started.
    idrs_before: u32,
    /// The coder stopped while the GOP was open (a batch could not be sent, or its results ended).
    gone: bool,
    /// The coder failed and the failure was reported: the GOP is closed, and what it coded before
    /// failing still comes out.
    failed: bool,
    coder: Coder,
}

enum Coder {
    Thread {
        /// Batches to code; dropped to close the GOP.
        batches: Option<mpsc::Sender<Batch>>,
        coded: mpsc::Receiver<Out>,
        /// The thread until it is joined: the reference state the GOP ended with, or the panic.
        handle: Option<JoinHandle<Result<Refs, String>>>,
        /// The reference state of a thread joined before its GOP was taken.
        ended: Option<Refs>,
    },
    /// No thread could be started: the GOP is coded on the calling thread as its batches come.
    Here { refs: Refs, done: VecDeque<Out>, failed: Option<String> },
}

/// What [`Gop::next`] found.
enum Next {
    Coded(Out),
    /// Nothing yet.
    Pending,
    /// Every coded picture was taken and the coder is done: the reference state it ended with, or
    /// its failure (`None` when it was reported already).
    Finished(Result<Refs, Option<String>>),
}

impl Gop {
    /// Start coding a GOP from `refs` on a thread (or here, when no thread can be had);
    /// `idrs_before` IDR pictures were handed to GOPs before it.
    fn start(ctx: &Arc<Ctx>, fixed: FixedQp, refs: Refs, idrs_before: u32, cancel: &Arc<AtomicBool>, #[cfg(test)] plant: Plant) -> Gop {
        let coded = Arc::new(AtomicUsize::new(0));
        let (batch_tx, batch_rx) = mpsc::channel::<Batch>();
        let (out_tx, out_rx) = mpsc::channel::<Out>();
        // the reference state goes over a channel so it stays here if the thread cannot be started
        let (refs_tx, refs_rx) = mpsc::sync_channel::<Refs>(1);
        let (c, count, stop) = (Arc::clone(ctx), Arc::clone(&coded), Arc::clone(cancel));
        let body = move || -> Result<Refs, String> {
            #[cfg(test)]
            let Plant { panic: plant_panic, hold, slow, alive: _alive } = plant;
            let refs = refs_rx.recv().map_err(|_| "the GOP never reached its thread".to_string())?;
            catch_unwind(AssertUnwindSafe(|| {
                #[cfg(test)]
                std::thread::sleep(slow);
                #[cfg(test)]
                if hold {
                    while !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    // the rest of a long batch
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                #[cfg(test)]
                if plant_panic {
                    // from a pool task, as a panic in a slice would come
                    let boom = || -> u8 { panic!("planted GOP panic") };
                    c.pool.install(|| rayon::join(|| 0u8, boom));
                }
                code(&c, fixed, refs, &batch_rx, &out_tx, &count, &stop)
            }))
            .map_err(panic_text)
        };
        let spawned = std::thread::Builder::new().name("h264enc-gop".into()).stack_size(GOP_STACK).spawn(body);
        let coder = match spawned {
            Ok(handle) => match refs_tx.send(refs) {
                Ok(()) => Coder::Thread { batches: Some(batch_tx), coded: out_rx, handle: Some(handle), ended: None },
                Err(mpsc::SendError(refs)) => Coder::Here { refs, done: VecDeque::new(), failed: None },
            },
            // no thread to be had: code the GOP here (the state was never sent)
            Err(_) => Coder::Here { refs, done: VecDeque::new(), failed: None },
        };
        Gop { open: true, sent: 0, taken: 0, coded, idrs_before, gone: false, failed: false, coder }
    }

    /// Hand a batch to the coder. A coder that stopped drops it: it is lost with the GOP.
    fn send(&mut self, ctx: &Ctx, fixed: FixedQp, batch: Batch) {
        let n = batch.pics.len();
        self.sent += n;
        match &mut self.coder {
            Coder::Thread { batches, .. } => {
                if let Some(tx) = batches
                    && tx.send(batch).is_err()
                {
                    self.gone = true;
                }
            }
            Coder::Here { refs, done, failed } => {
                if failed.is_none() {
                    let recon = batch.recon;
                    match catch_unwind(AssertUnwindSafe(|| encode_batch(ctx, refs, &mut |st, _, _| fixed.frame_qp(st), batch))) {
                        Ok(out) => {
                            done.push_back(out.into_iter().map(|c| (c, recon)).collect());
                            self.coded.fetch_add(n, Ordering::Release);
                        }
                        Err(p) => *failed = Some(panic_text(p)),
                    }
                }
            }
        }
    }

    /// Join the coder's thread once it has ended (keeping the reference state it ended with), and
    /// return the coder's failure the first time it is seen: a thread that panicked, or one that
    /// ended while the GOP was open. A failed GOP is closed.
    fn check(&mut self) -> Option<String> {
        if self.failed {
            return None;
        }
        let open = self.open;
        let failure = match &mut self.coder {
            Coder::Thread { handle, ended, .. } => {
                // a thread that stopped taking batches or sending results is ending: joining it
                // does not wait for more than that
                if !self.gone && !handle.as_ref().is_some_and(JoinHandle::is_finished) {
                    return None;
                }
                let h = handle.take()?;
                match h.join().unwrap_or_else(|p| Err(panic_text(p))) {
                    Ok(refs) if !open => {
                        *ended = Some(refs);
                        return None;
                    }
                    Ok(_) => "the GOP thread ended before its GOP".to_string(),
                    Err(e) => e,
                }
            }
            Coder::Here { failed, .. } => failed.take()?,
        };
        self.failed = true;
        self.close();
        Some(failure)
    }

    /// No more batches: the coder finishes once it has coded the ones it has.
    fn close(&mut self) {
        self.open = false;
        if let Coder::Thread { batches, .. } = &mut self.coder {
            *batches = None;
        }
    }

    /// Input pictures sent and not coded yet.
    fn uncoded(&self) -> usize {
        self.sent.saturating_sub(self.coded.load(Ordering::Acquire))
    }

    /// Whether [`Gop::next`] with `wait` returns: something is still to come, or the coder ends.
    fn can_wait(&self) -> bool {
        !self.open || (self.taken < self.sent && !self.gone)
    }

    /// The next coded batch, the end of the GOP, or (without `wait`) nothing yet.
    fn next(&mut self, wait: bool) -> Next {
        match &mut self.coder {
            Coder::Thread { coded, handle, ended, .. } => {
                let got = if wait { coded.recv().map_err(|_| mpsc::TryRecvError::Disconnected) } else { coded.try_recv() };
                match got {
                    Ok(out) => {
                        self.taken += out.len();
                        Next::Coded(out)
                    }
                    Err(mpsc::TryRecvError::Empty) => Next::Pending,
                    // the thread stopped with the GOP open: `check` reports it and closes the GOP
                    Err(mpsc::TryRecvError::Disconnected) if self.open => {
                        self.gone = true;
                        Next::Pending
                    }
                    // the thread ended (closed, cancelled or panicked) and everything it sent was taken
                    Err(mpsc::TryRecvError::Disconnected) => Next::Finished(if self.failed {
                        Err(None)
                    } else if let Some(refs) = ended.take() {
                        Ok(refs)
                    } else {
                        match handle.take() {
                            Some(h) => h.join().unwrap_or_else(|p| Err(panic_text(p))).map_err(Some),
                            None => Err(Some("GOP taken twice".into())),
                        }
                    }),
                }
            }
            Coder::Here { refs, done, failed } => match done.pop_front() {
                Some(out) => {
                    self.taken += out.len();
                    Next::Coded(out)
                }
                None if self.open => Next::Pending,
                None => Next::Finished(match failed.take() {
                    Some(e) => Err(Some(e)),
                    None if self.failed => Err(None),
                    None => Ok(std::mem::take(refs)),
                }),
            },
        }
    }
}

/// The GOP thread: code the batches in order as they come, until the GOP is closed.
fn code(ctx: &Ctx, fixed: FixedQp, mut refs: Refs, batches: &mpsc::Receiver<Batch>, out: &mpsc::Sender<Out>, coded: &AtomicUsize, cancel: &AtomicBool) -> Refs {
    for batch in batches {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let (n, recon) = (batch.pics.len(), batch.recon);
        let pics = encode_batch(ctx, &mut refs, &mut |st, _, _| fixed.frame_qp(st), batch);
        coded.fetch_add(n, Ordering::Release);
        if out.send(pics.into_iter().map(|c| (c, recon)).collect()).is_err() {
            break;
        }
    }
    refs
}

/// The message of a panic payload (a `String` or `&str`), or "panic" for any other payload.
fn panic_text(p: Box<dyn Any + Send>) -> String {
    match p.downcast::<String>() {
        Ok(s) => *s,
        Err(p) => p.downcast_ref::<&str>().map_or_else(|| "panic".to_string(), |s| (*s).to_string()),
    }
}

impl Gops {
    /// The GOP-parallel state, or `None` for the serial path: not asked for, a rate control with
    /// feedback, one thread, or two GOPs of `keyint` pictures over the budget.
    pub(crate) fn new(cfg: &EncoderConfig, fixed: Option<FixedQp>, threads: usize, frame_bytes: usize, max_refs: usize) -> Option<Gops> {
        let fixed = fixed?;
        if !cfg.parallel_gops || threads < 2 {
            return None;
        }
        let budget = GOP_BUFFER_BYTES / frame_bytes.max(1);
        let work = 3 * max_refs + usize::from(cfg.bframes) + 4;
        let longest = (cfg.keyint as usize).saturating_add(work);
        // one GOP coding while the next one comes in, at least
        if longest.saturating_mul(2).saturating_add(SPARE_FRAMES) > budget {
            return None;
        }
        Some(Gops {
            fixed,
            gops: VecDeque::new(),
            next_seq: 0,
            idrs: 0,
            refs: 0,
            restart: false,
            budget,
            work,
            max_gops: threads,
            analysis: rayon::ThreadPoolBuilder::new()
                .num_threads(threads.div_ceil(6).clamp(2, 4))
                .thread_name(|i| format!("h264enc-analysis-{i}"))
                .build()
                .ok(),
            cancel: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            plant_panic: None,
            #[cfg(test)]
            plant_hold: false,
            #[cfg(test)]
            plant_slow: std::time::Duration::ZERO,
            #[cfg(test)]
            peak: 0,
            #[cfg(test)]
            live: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Padded pictures held with `inputs` more input pictures on the calling thread.
    fn frames_held(&self, inputs: usize) -> usize {
        self.gops.iter().map(|g| g.uncoded() + self.work).sum::<usize>() + inputs + SPARE_FRAMES
    }

    /// The pool that analyses the input pictures, if it could be built.
    pub(crate) fn analysis_pool(&self) -> Option<&rayon::ThreadPool> {
        self.analysis.as_ref()
    }

    /// The open GOP (the newest one, unless a flush closed it).
    fn open(&mut self) -> Option<&mut Gop> {
        self.gops.back_mut().filter(|g| g.open)
    }

    /// The GOP at `at` failed. When the stream would have continued from it (the last GOP), the
    /// next picture is an IDR; and when none of its pictures comes out, its IDR counts as never
    /// sent, so the next IDR's `idr_pic_id` differs from the one of the IDR before it.
    fn lost(&mut self, at: usize) {
        if at.saturating_add(1) != self.gops.len() {
            return;
        }
        self.restart = true;
        if let Some(g) = self.gops.get(at)
            && g.coded.load(Ordering::Acquire) == 0
        {
            self.idrs = g.idrs_before;
        }
    }
}

impl Drop for Gops {
    /// Stop the GOP threads and wait for them: each ends after its current batch at most, so the
    /// GOPs' reference pictures, buffers and pool work are released before the encoder is gone. A
    /// thread that panicked is joined without passing the panic on.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        // every channel closes before the first wait: a thread waiting for a batch then ends, and
        // no GOP thread waits for this one
        for g in &mut self.gops {
            g.close();
        }
        for g in &mut self.gops {
            if let Coder::Thread { handle, .. } = &mut g.coder
                && let Some(h) = handle.take()
            {
                // the encoder is going away: the GOP's result, or its panic, has no one to go to
                let _ = h.join();
            }
        }
    }
}

impl Encoder {
    /// Before an input picture: take coded pictures of the oldest GOP (waiting for them) while one
    /// more picture would pass the budget.
    pub(crate) fn gop_make_room(&mut self) {
        self.gop_room(1, 0);
    }

    /// Take coded pictures of the oldest GOPs (waiting for them) until `inputs` input pictures
    /// besides the pending ones and `extra` more padded pictures fit in the budget, or no GOP is
    /// left to take. Two GOPs of `keyint` pictures fit (or the encoder is serial), so with every
    /// GOP taken a new one always fits.
    fn gop_room(&mut self, inputs: usize, extra: usize) {
        while let Some(g) = self.gops.as_ref()
            && g.frames_held(self.pending.len().saturating_add(inputs)).saturating_add(extra) > g.budget
        {
            if !self.gop_take_front(true) {
                break;
            }
        }
    }

    /// Notice the GOP coders that ended (see [`Gop::check`]): a failure is reported by the call
    /// that notices it, as [`Error::Worker`], and the stream goes on from an IDR when it would have
    /// continued from the failed GOP.
    pub(crate) fn gop_check(&mut self) {
        let Some(g) = self.gops.as_mut() else { return };
        for at in 0..g.gops.len() {
            let Some(e) = g.gops.get_mut(at).and_then(Gop::check) else { continue };
            g.lost(at);
            self.failed.get_or_insert(Error::Worker(e));
        }
    }

    /// After a failure the stream starts again from an IDR (see [`Gops::restart`]): the oldest
    /// picture not scheduled yet becomes it. Whether there was none, so that the next picture must
    /// be the IDR.
    pub(crate) fn gop_restart(&mut self) -> bool {
        if !self.gops.as_ref().is_some_and(|g| g.restart) {
            return false;
        }
        let Some(first) = self.pending.pop_front() else { return true };
        self.last_idr = first.display;
        self.schedule(vec![(first, SliceType::I, true)]);
        false
    }

    /// Hand a batch to its GOP. An IDR batch closes the open GOP and starts the next one, with a
    /// fresh reference state carrying the stream's counters; the first batch after a flush
    /// continues from the state the flush ended with. After a failure, the batches before the next
    /// IDR belong to the failed GOP and are dropped.
    pub(crate) fn gop_schedule(&mut self, batch: Batch) {
        let idr = batch.pics.iter().any(|p| p.idr);
        let Some(g) = self.gops.as_mut() else { return };
        if idr || g.open().is_none() {
            if let Some(open) = g.open() {
                open.close();
            }
            // at most `max_gops` threads: the oldest GOPs are all closed now
            while self.gops.as_ref().is_some_and(|g| g.gops.len() >= g.max_gops) {
                if !self.gop_take_front(true) {
                    break;
                }
            }
            // the new GOP's working set must fit in the budget too, with the pictures of the batch
            // and the one picture a call may hold aside while it schedules the others
            let work = self.gops.as_ref().map_or(0, |g| g.work);
            self.gop_room(batch.pics.len().saturating_add(1), work);
            let Some(g) = self.gops.as_mut() else { return };
            if !idr && g.restart {
                return;
            }
            g.restart = false;
            let refs = if idr {
                // a state kept from a flush is no longer needed: keep its buffers only
                let stale = self.refs.take_state();
                self.refs.reuse(stale);
                // the identification SEI goes with the first picture of the stream only
                Refs { idr_count: g.idrs, ref_uid: g.refs, sei_sent: batch.first > 0, hpel_pool: std::mem::take(&mut self.refs.hpel_pool), ..Refs::default() }
            } else {
                self.refs.take_state()
            };
            #[cfg(test)]
            let plant = Plant { panic: g.plant_panic == Some(g.next_seq), hold: g.plant_hold, slow: g.plant_slow, alive: Alive::new(&g.live) };
            g.next_seq += 1;
            let gop = Gop::start(
                &self.ctx,
                g.fixed,
                refs,
                g.idrs,
                &g.cancel,
                #[cfg(test)]
                plant,
            );
            g.gops.push_back(gop);
        }
        let Some(g) = self.gops.as_mut() else { return };
        for p in &batch.pics {
            g.idrs += u32::from(p.idr);
            // B pictures always find both references in a GOP, so a planned I/P picture is exactly
            // a reference picture (and the numbering only needs to be consistent within a GOP:
            // reference identities are only compared)
            g.refs += u32::from(p.st != SliceType::B);
        }
        let fixed = g.fixed;
        if let Some(open) = g.open() {
            open.send(&self.ctx, fixed, batch);
        }
        #[cfg(test)]
        {
            g.peak = g.peak.max(g.frames_held(self.pending.len()));
        }
        self.gop_check();
    }

    /// Take what the oldest GOP has coded (with `wait`, at least one batch or its end); a GOP
    /// that is done leaves the queue, and the stream continues from the last one after a flush.
    fn gop_take_front(&mut self, wait: bool) -> bool {
        let Some(front) = self.gops.as_mut().and_then(|g| g.gops.front_mut()) else { return false };
        if wait && !front.can_wait() {
            return false;
        }
        match front.next(wait) {
            Next::Pending => false,
            Next::Coded(out) => {
                for (c, recon) in out {
                    self.deliver(c, recon);
                }
                true
            }
            Next::Finished(end) => {
                let Some(g) = self.gops.as_mut() else { return false };
                if end.is_err() {
                    g.lost(0);
                }
                g.gops.pop_front();
                let last = g.gops.is_empty();
                match end {
                    // the GOP a flush ended with: the stream may continue from its references
                    Ok(refs) if last => self.refs.adopt(refs),
                    Ok(refs) => self.refs.reuse(refs),
                    Err(Some(e)) => {
                        self.failed.get_or_insert(Error::Worker(e));
                    }
                    // reported when it was noticed
                    Err(None) => {}
                }
                true
            }
        }
    }

    /// Notice failed GOPs, then take what is coded, in decoding order, without waiting.
    pub(crate) fn gop_poll(&mut self) {
        self.gop_check();
        while self.gop_take_front(false) {}
    }

    /// Close the open GOP and take every GOP (flush).
    pub(crate) fn gop_finish(&mut self) {
        if let Some(open) = self.gops.as_mut().and_then(Gops::open) {
            open.close();
        }
        while self.gops.as_ref().is_some_and(|g| !g.gops.is_empty()) {
            if !self.gop_take_front(true) {
                break;
            }
        }
    }

    /// Make the GOP numbered `seq` (0 = the first) panic on its thread.
    #[cfg(test)]
    pub(crate) fn plant_gop_panic(&mut self, seq: u64) {
        if let Some(g) = self.gops.as_mut() {
            g.plant_panic = Some(seq);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use crate::{Encoder, EncoderConfig, Error, FrameType, Packet, ReconFrame, YuvFrame};

    /// A slowly moving gradient (no scene cuts).
    fn picture(w: usize, h: usize, t: usize) -> [Vec<u8>; 3] {
        let y = (0..w * h).map(|i| ((i % w + i / w + t) % 256) as u8).collect();
        let c = (0..w * h / 4).map(|i| (128 + (i + t) % 16) as u8).collect::<Vec<u8>>();
        [y, c.clone(), c]
    }

    /// A GOP-parallel encoder (64x48, three threads), the GOP numbered `plant` panicking.
    fn encoder(keyint: u32, plant: Option<u64>) -> Encoder {
        let mut c = EncoderConfig::new(64, 48, 10, 1);
        c.keyint = keyint;
        c.threads = 3;
        let mut enc = Encoder::new(c).unwrap();
        assert!(enc.gop_parallel());
        if let Some(seq) = plant {
            enc.plant_gop_panic(seq);
        }
        enc
    }

    /// What a test does with the encoder: encode the next pictures, or flush.
    #[derive(Clone, Copy)]
    enum Step {
        Encode(usize),
        Flush,
    }

    /// A stream: its packets, the reconstructions of their pictures, and the calls that failed.
    struct Run {
        packets: Vec<Packet>,
        recon: Vec<ReconFrame>,
        failures: usize,
    }

    /// Take the steps (picture `t` has pts `t`), capturing the reconstruction.
    fn run(mut enc: Encoder, steps: &[Step]) -> Run {
        enc.set_recon_capture(true);
        let (mut packets, mut failures, mut t) = (Vec::new(), 0, 0);
        let mut take = |r: Result<Vec<Packet>, Error>, packets: &mut Vec<Packet>| match r {
            Ok(p) => {
                packets.extend(p);
                true
            }
            Err(Error::Worker(msg)) => {
                assert!(msg.contains("planted GOP panic"), "{msg}");
                failures += 1;
                false
            }
            Err(e) => panic!("{e}"),
        };
        for &step in steps {
            match step {
                Step::Encode(n) => {
                    for _ in 0..n {
                        let [y, u, v] = picture(64, 48, t);
                        let frame = YuvFrame { y: &y, u: &u, v: &v, y_stride: 64, uv_stride: 32 };
                        take(enc.try_encode(&frame, t as i64), &mut packets);
                        t += 1;
                    }
                }
                // a failure reported by the flush keeps the other packets for the next call
                Step::Flush => while !take(enc.flush(), &mut packets) {},
            }
        }
        Run { packets, recon: enc.take_recon(), failures }
    }

    /// Check a run whose GOP starting at picture `first` failed against the same run without the
    /// failure: one error; the packets before that GOP unchanged; then an IDR, and every picture
    /// up to `last` but a block from `first` on (the ones scheduled into the failed GOP); and the
    /// whole stream decodes with FilmCraft's H.264 decoder, without errors, to the encoder's
    /// reconstructions.
    fn assert_recovers(name: &str, failed: &Run, clean: &Run, first: i64, last: i64) {
        assert_eq!(clean.failures, 0, "{name}");
        assert_eq!(failed.failures, 1, "{name}: one error, once");
        let before = |r: &Run| r.packets.iter().take_while(|p| p.pts < first).count();
        let n = before(clean);
        assert_eq!(before(failed), n, "{name}: packets before the failed GOP");
        for (a, b) in failed.packets.iter().zip(&clean.packets).take(n) {
            assert!(a.data == b.data && (a.pts, a.dts) == (b.pts, b.dts), "{name}: picture {} changed", a.pts);
        }
        let after = failed.packets.get(n..).unwrap_or_default();
        assert_eq!(after.first().map(|p| p.frame_type), Some(FrameType::Idr), "{name}: the stream goes on from an IDR");
        let mut pts: Vec<i64> = after.iter().map(|p| p.pts).collect();
        pts.sort_unstable();
        let from = pts[0];
        assert!(from >= first, "{name}");
        assert_eq!(pts, (from..=last).collect::<Vec<i64>>(), "{name}: only pictures of the failed GOP are lost");

        let mut dec = filmcraft_h264::Decoder::new();
        let mut pics = Vec::new();
        for p in &failed.packets {
            pics.extend(dec.decode(&p.data, p.pts).unwrap_or_else(|e| panic!("{name}: picture {}: {e}", p.pts)));
        }
        pics.extend(dec.flush());
        assert!(dec.take_error().is_none(), "{name}: decoding error");
        pics.sort_by_key(|p| p.pts);
        let mut recon: Vec<&ReconFrame> = failed.recon.iter().collect();
        recon.sort_by_key(|r| r.pts);
        assert_eq!(pics.len(), failed.packets.len(), "{name}: decoded pictures");
        assert_eq!(recon.len(), pics.len(), "{name}: reconstructions");
        let rows = |plane: &[u8], stride: usize, w: usize, h: usize| (0..h).flat_map(|y| plane[y * stride..y * stride + w].to_vec()).collect::<Vec<u8>>();
        for (r, p) in recon.iter().zip(&pics) {
            assert_eq!(r.pts, p.pts, "{name}");
            let same = rows(&p.y, p.y_stride, 64, 48) == r.y && rows(&p.u, p.uv_stride, 32, 24) == r.u && rows(&p.v, p.uv_stride, 32, 24) == r.v;
            assert!(same, "{name}: picture {} decodes to something else than the encoder's reconstruction", r.pts);
        }
    }

    /// A panic on the thread of a GOP that an IDR started (and the next IDR may close) is reported
    /// once as `Error::Worker`; its pictures are lost, the stream goes on from an IDR and decodes.
    #[test]
    fn a_panicking_gop_is_an_error_and_the_stream_goes_on_from_an_idr() {
        let steps = [Step::Encode(20), Step::Flush];
        let clean = run(encoder(4, None), &steps);
        assert_recovers("keyint 4", &run(encoder(4, Some(1)), &steps), &clean, 4, 19);
    }

    /// The open GOP failing in the middle of the stream (here the one a flush left to continue
    /// from, with no IDR for 250 pictures): the next picture after the failure is noticed is an
    /// IDR, and the stream continues cleanly across a later flush.
    #[test]
    fn a_failed_open_gop_restarts_the_stream_at_an_idr() {
        let steps = [Step::Encode(6), Step::Flush, Step::Encode(14), Step::Flush, Step::Encode(10), Step::Flush];
        let clean = run(encoder(250, None), &steps);
        assert_recovers("open GOP", &run(encoder(250, Some(1)), &steps), &clean, 6, 29);
    }

    /// The final GOP failing before a flush: the stream does not continue from its lost
    /// references after the flush, but from an IDR.
    #[test]
    fn a_failed_final_gop_is_not_continued_after_a_flush() {
        let steps = [Step::Encode(6), Step::Flush, Step::Encode(3), Step::Flush, Step::Encode(11), Step::Flush];
        let clean = run(encoder(250, None), &steps);
        assert_recovers("final GOP", &run(encoder(250, Some(1)), &steps), &clean, 6, 19);
    }

    /// A GOP thread that died is reported by the next call, not when its GOP closes, and the
    /// picture of that call starts the stream again as an IDR: only the pictures already given to
    /// the failed GOP are lost.
    #[test]
    fn a_failure_is_reported_by_the_next_call_and_its_picture_is_an_idr() {
        let mut enc = encoder(250, Some(1));
        // every GOP thread has ended (or was joined)
        let ended = |enc: &Encoder| {
            enc.gops.as_ref().unwrap().gops.iter().all(|g| match &g.coder {
                super::Coder::Thread { handle, .. } => handle.as_ref().is_none_or(std::thread::JoinHandle::is_finished),
                super::Coder::Here { .. } => true,
            })
        };
        let encode = |enc: &mut Encoder, t: usize| {
            let [y, u, v] = picture(64, 48, t);
            enc.try_encode(&YuvFrame { y: &y, u: &u, v: &v, y_stride: 64, uv_stride: 32 }, t as i64)
        };
        let mut packets = Vec::new();
        for t in 0..6 {
            packets.extend(encode(&mut enc, t).unwrap());
        }
        packets.extend(enc.flush().unwrap());
        // pictures 6..=8: the third one sends the first batch (P 8, B 6 and 7) to GOP 1, which
        // panics (that call may already see it die)
        let mut failed = Vec::new();
        for t in 6..9 {
            match encode(&mut enc, t) {
                Ok(p) => packets.extend(p),
                Err(e) => failed.push((t, e.to_string())),
            }
        }
        let t0 = std::time::Instant::now();
        while !ended(&enc) {
            assert!(t0.elapsed().as_secs() < 10, "GOP 1 never ended");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // the thread is gone: the next call says so at the latest
        match encode(&mut enc, 9) {
            Ok(p) => packets.extend(p),
            Err(e) => failed.push((9, e.to_string())),
        }
        assert!(failed.len() == 1 && failed[0].0 >= 8 && failed[0].1.contains("planted GOP panic"), "{failed:?}");
        for t in 10..16 {
            packets.extend(encode(&mut enc, t).unwrap());
        }
        packets.extend(enc.flush().unwrap());
        let after: Vec<(i64, FrameType)> = packets.iter().filter(|p| p.pts >= 6).map(|p| (p.pts, p.frame_type)).collect();
        assert_eq!(after.first(), Some(&(9, FrameType::Idr)), "{after:?}");
        let mut pts: Vec<i64> = after.iter().map(|p| p.0).collect();
        pts.sort_unstable();
        assert_eq!(pts, (9..16).collect::<Vec<i64>>());
    }

    /// Dropping an encoder whose GOPs still have batches to code returns.
    #[test]
    fn dropping_an_encoder_with_gops_running_returns() {
        let mut enc = encoder(4, None);
        for t in 0..40 {
            let [y, u, v] = picture(64, 48, t);
            let frame = YuvFrame { y: &y, u: &u, v: &v, y_stride: 64, uv_stride: 32 };
            enc.try_encode(&frame, t as i64).unwrap();
        }
        drop(enc);
    }

    /// Dropping the encoder waits for its GOP threads: every one was still busy with a batch when
    /// the drop began, and none is left once it returns.
    #[test]
    fn no_gop_thread_outlives_its_encoder() {
        let mut enc = encoder(4, None);
        let live = {
            let g = enc.gops.as_mut().unwrap();
            g.plant_hold = true;
            Arc::clone(&g.live)
        };
        for t in 0..12 {
            let [y, u, v] = picture(64, 48, t);
            let frame = YuvFrame { y: &y, u: &u, v: &v, y_stride: 64, uv_stride: 32 };
            enc.try_encode(&frame, t as i64).unwrap();
        }
        let (gops, busy) = {
            let gops = &enc.gops.as_ref().unwrap().gops;
            (gops.len(), gops.iter().all(|g| g.uncoded() > 0))
        };
        assert!(gops >= 2 && busy, "{gops} GOPs, all still coding: {busy}");
        assert_eq!(live.load(Ordering::SeqCst), gops);
        drop(enc);
        assert_eq!(live.load(Ordering::SeqCst), 0, "GOP threads left after the drop");
    }

    /// With GOPs falling behind the input and a budget just above the smallest the GOP-parallel
    /// path accepts, starting a GOP waits for room for its working set: the pictures held never
    /// pass the budget, and the stream is still the serial one.
    #[test]
    fn starting_a_gop_keeps_the_pictures_held_within_the_budget() {
        let mut enc = encoder(4, None);
        let budget = {
            let g = enc.gops.as_mut().unwrap();
            // two GOPs of `keyint` pictures with their working sets, and the spares (the least
            // `Gops::new` accepts), and 5 more: two GOPs fit while a third one is due
            g.budget = 2 * (4 + g.work) + super::SPARE_FRAMES + 5;
            g.plant_slow = std::time::Duration::from_millis(10);
            g.budget
        };
        let mut packets = Vec::new();
        for t in 0..24 {
            let [y, u, v] = picture(64, 48, t);
            packets.extend(enc.try_encode(&YuvFrame { y: &y, u: &u, v: &v, y_stride: 64, uv_stride: 32 }, t as i64).unwrap());
        }
        packets.extend(enc.flush().unwrap());
        let peak = enc.gops.as_ref().unwrap().peak;
        assert!(peak <= budget, "{peak} padded pictures held, budget {budget}");
        let serial = {
            let mut c = EncoderConfig::new(64, 48, 10, 1);
            c.keyint = 4;
            c.threads = 3;
            c.parallel_gops = false;
            run(Encoder::new(c).unwrap(), &[Step::Encode(24), Step::Flush])
        };
        assert_eq!(packets.len(), serial.packets.len());
        for (a, b) in packets.iter().zip(&serial.packets) {
            assert!(a.data == b.data && (a.pts, a.dts) == (b.pts, b.dts), "picture {}", a.pts);
        }
    }
}
