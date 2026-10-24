//! Parallel decoding inside a picture: the CTB rows of a wavefront picture
//! (`entropy_coding_sync_enabled_flag`, 9.3.1, 9.3.2.4) or the tiles of a tiled picture
//! (`tiles_enabled_flag`, 6.5.1) are decoded by several threads of the decoder's pool.
//!
//! The picture is split into units (a CTB row with WPP, a tile with tiles), each holding the
//! substreams of the slice segments that cover it: the entry points of the slice segment headers
//! (`entry_point_offset_minus1`, 7.4.7.1) give where each substream starts. A thread claims the
//! next unit and decodes it into a private window ([`PicState::window`]); with WPP it first waits
//! for the row above to be two CTBs ahead (above-right availability, 6.4.1) and copies in what the
//! row reads of it: the bottom sample lines, 4x4 block state and motion, the CTB slice addresses
//! and SAO parameters, and the contexts stored after the second CTB of that row (9.3.2.3). Tiles
//! need nothing from each other (no prediction across tile boundaries).
//!
//! Finished windows are copied into the picture state the in-loop filters run on; whichever
//! thread holds it (never waiting for it) deblocks, applies SAO to and publishes the rows that are
//! complete, while the other threads keep decoding. The thread finishing the last unit finishes
//! the picture. No thread ever waits on a unit nobody is decoding: a unit is claimed only by a
//! running thread, and the picture's own job thread claims units until none is left, so a picture
//! completes even if no helper ever starts.

use crate::cabac::Contexts;
use crate::decoder::Shared;
use crate::error::Error;
use crate::filter::StageDeblock;
use crate::params::{Layout, Pps, Sps};
use crate::picture::{FrameRef, Sample};
use crate::slicedec::{Blk, CtuHooks, Mode, MvField, PicState, Resume, SaoParams, SliceDecoder, SliceInfo, SliceJob, SliceMeta, Start};
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

/// A substream of a slice segment: `jobs[slice]`, starting at `rbsp` byte `pos` with CTB `ts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    slice: usize,
    pos: usize,
    ts: u32,
}

/// CTBs `ts0..ts1` (tile scan) decoded by one thread: a CTB row (WPP) or a tile.
#[derive(Debug)]
struct Unit {
    ts0: u32,
    ts1: u32,
    rows: (usize, usize),
    cols: (usize, usize),
    entries: Vec<Entry>,
}

/// How a picture is split into units.
#[derive(Debug)]
pub struct Plan {
    units: Vec<Unit>,
    wpp: bool,
    /// One thread decodes the CTB rows in order (no WPP / tiles to split the parsing); the
    /// in-loop filters of finished rows run on other threads meanwhile.
    serial: bool,
}

impl Plan {
    /// Rows decoded in order by one thread, filtered by others ([`Plan::serial`]); None for a
    /// picture of one CTB row or with tiles (whose CTBs are not decoded row by row).
    pub fn serial(layout: &Layout, tiles: bool) -> Option<Plan> {
        let (w, h) = (layout.width_ctbs as usize, layout.height_ctbs as usize);
        if tiles || h < 2 {
            return None;
        }
        let mut units = Vec::with_capacity(h);
        for r in 0..h {
            let ts0 = u32::try_from(r * w).ok()?;
            let ts1 = u32::try_from((r + 1) * w).ok()?;
            units.push(Unit { ts0, ts1, rows: (r, r + 1), cols: (0, w), entries: Vec::new() });
        }
        Some(Plan { units, wpp: false, serial: true })
    }

    /// The units of a picture whose slice segments are `jobs`, or None when the picture is
    /// decoded serially: neither WPP nor tiles (or both), fewer than two units, or entry points
    /// that don't match the picture (missing, out of range, out of order).
    pub fn new(layout: &Layout, wpp: bool, tiles: bool, jobs: &[SliceJob]) -> Option<Plan> {
        let (w, h) = (layout.width_ctbs as usize, layout.height_ctbs as usize);
        let mut units = Vec::new();
        if wpp && !tiles {
            for r in 0..h {
                let ts0 = u32::try_from(r * w).ok()?;
                let ts1 = u32::try_from((r + 1) * w).ok()?;
                units.push(Unit { ts0, ts1, rows: (r, r + 1), cols: (0, w), entries: Vec::new() });
            }
        } else if tiles && !wpp {
            for j in 0..layout.row_bd.len().saturating_sub(1) {
                for i in 0..layout.col_bd.len().saturating_sub(1) {
                    let (y0, y1) = (*layout.row_bd.get(j)? as usize, *layout.row_bd.get(j + 1)? as usize);
                    let (x0, x1) = (*layout.col_bd.get(i)? as usize, *layout.col_bd.get(i + 1)? as usize);
                    let ts0 = *layout.rs_to_ts.get(y0 * w + x0)?;
                    let n = u32::try_from((y1 - y0) * (x1 - x0)).ok()?;
                    units.push(Unit { ts0, ts1: ts0.checked_add(n)?, rows: (y0, y1), cols: (x0, x1), entries: Vec::new() });
                }
            }
        } else {
            return None;
        }
        if units.len() < 2 {
            return None;
        }
        let unit_of = |ts: u32| -> Option<usize> { if wpp { Some(ts as usize / w.max(1)) } else { layout.tile_id.get(ts as usize).map(|&t| t as usize) } };
        // (unit, CTB) of the last substream of the previous segment
        let mut prev: Option<(usize, u32)> = None;
        for (i, job) in jobs.iter().enumerate() {
            let s = *layout.rs_to_ts.get(job.sh.segment_address as usize)?;
            let u0 = unit_of(s)?;
            if let Some((pu, pts)) = prev
                && (u0 < pu || (u0 == pu && s <= pts))
            {
                return None;
            }
            let starts = job.substream_starts()?;
            let mut last = (u0, s);
            for (k, &pos) in starts.iter().enumerate() {
                let u = u0.checked_add(k)?;
                let unit = units.get_mut(u)?;
                let ts = if k == 0 { s } else { unit.ts0 };
                if !(unit.ts0..unit.ts1).contains(&ts) {
                    return None;
                }
                unit.entries.push(Entry { slice: i, pos, ts });
                last = (u, ts);
            }
            prev = Some(last);
        }
        Some(Plan { units, wpp, serial: false })
    }
}

/// What the row below reads of a WPP row, published CTB by CTB.
#[derive(Default)]
struct Edge<S> {
    /// CTBs of the row published (always the leftmost ones).
    done: usize,
    /// The row is done (decoded, failed or panicked): nothing more will come.
    finished: bool,
    /// The row below, suspended until `done` reaches the first value.
    parked: Option<(usize, Cont<S>)>,
    /// A task to resume `parked` has been started.
    resuming: bool,
    /// Bottom luma line, bottom chroma lines.
    y: Vec<S>,
    cb: Vec<S>,
    cr: Vec<S>,
    /// Bottom row of 4x4 blocks.
    blk: Vec<Blk>,
    mvf: Vec<MvField>,
    /// Per CTB: slice index, SliceAddrRs, SAO parameters.
    ctb: Vec<(u32, u32, SaoParams)>,
    /// Contexts stored after the row's second CTB.
    wpp: Option<Box<Contexts>>,
}

/// A unit being decoded: the next entry, where it resumes (None: from its start), the window.
struct Cont<S> {
    unit: usize,
    entry: usize,
    resume: Option<Resume>,
    win: PicState<S>,
    /// CTBs of the row above copied into the window.
    synced: usize,
    err: Option<Error>,
}

#[allow(clippy::large_enum_variant)] // one per unit, moved once
enum Run<S> {
    /// All entries decoded.
    Done(Cont<S>),
    /// Suspended on the row above, which resumes it.
    Parked,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The picture state the in-loop filters run on, and how many CTBs of each row it holds.
struct Stage<S> {
    pic: PicState<S>,
    ctbs: Vec<usize>,
}

/// One picture decoded in units.
pub struct Job<S> {
    frame: FrameRef,
    sps: Arc<Sps>,
    pps: Arc<Pps>,
    layout: Arc<Layout>,
    scaling: Option<Arc<Vec<Vec<u8>>>>,
    /// Slices of the picture, deblocking anywhere, draft mode (for deblocking in the windows).
    info: Vec<SliceInfo>,
    deblocking: bool,
    draft: bool,
    /// Units deblock their own edges ([`crate::filter::deblock_unit`]).
    unit_deblock: bool,
    slices: Vec<SliceJob>,
    metas: Vec<Option<SliceMeta>>,
    plan: Plan,
    /// Next unit to claim.
    next: AtomicUsize,
    /// Units not finished yet.
    left: AtomicUsize,
    /// Helper threads that may still be started.
    helpers: AtomicUsize,
    edges: Vec<Mutex<Edge<S>>>,
    stage: Mutex<Stage<S>>,
    /// Decoded windows waiting to be copied into the stage.
    pending: Mutex<Vec<(usize, Option<PicState<S>>)>>,
    /// Windows to re-use.
    free: Mutex<Vec<PicState<S>>>,
    error: Mutex<Option<Error>>,
    panicked: AtomicBool,
    shared: Arc<Shared>,
}

impl<S: Sample> Job<S> {
    /// Decode the picture of `pic` (slices registered with [`crate::slicedec::prepare_slices`])
    /// in the units of `plan`, on this thread and up to `helpers` threads of the current rayon
    /// pool. Returns once this thread finds no unit left to decode (the picture may still be
    /// finishing on another thread).
    pub fn run(mut pic: PicState<S>, slices: Vec<SliceJob>, metas: Vec<Option<SliceMeta>>, plan: Plan, helpers: usize, shared: Arc<Shared>) {
        let rows = pic.hctb;
        // WPP rows deblock all but their top edges; tiles everything when no edge crosses a
        // tile boundary, else the whole picture is deblocked after the units
        pic.stage_deblock = if plan.wpp || plan.serial {
            StageDeblock::Top
        } else if !pic.pps.loop_filter_across_tiles {
            StageDeblock::None
        } else {
            StageDeblock::Full
        };
        let n = plan.units.len();
        let edges = if plan.wpp { (0..rows).map(|_| Mutex::new(Edge::default())).collect() } else { Vec::new() };
        let job = Arc::new(Job {
            frame: pic.frame.clone(),
            sps: pic.sps.clone(),
            pps: pic.pps.clone(),
            layout: pic.layout.clone(),
            scaling: pic.scaling.clone(),
            info: pic.slices.clone(),
            deblocking: pic.deblocking_enabled_anywhere,
            draft: pic.draft,
            unit_deblock: pic.stage_deblock != StageDeblock::Full,
            slices,
            metas,
            plan,
            next: AtomicUsize::new(0),
            left: AtomicUsize::new(n),
            helpers: AtomicUsize::new(helpers.min(n.saturating_sub(1))),
            edges,
            stage: Mutex::new(Stage { pic, ctbs: vec![0; rows] }),
            pending: Mutex::new(Vec::new()),
            free: Mutex::new(Vec::new()),
            error: Mutex::new(None),
            panicked: AtomicBool::new(false),
            shared,
        });
        // Tiles are independent: start the helpers now. A WPP row starts one once its second CTB
        // is decoded, i.e. when the next row can run (a helper started earlier would only wait).
        if job.plan.serial {
            job.decode_serial();
            return;
        }
        if !job.plan.wpp {
            while job.spawn_helper() {}
        }
        job.work();
    }

    /// Decode the slices in order on this thread, handing each finished CTB row to another thread
    /// for deblocking, SAO and publication ([`PipeHooks`]).
    fn decode_serial(self: &Arc<Self>) {
        let hooks = PipeHooks { job: self, row: Cell::new(0) };
        let Some(first) = self.plan.units.first() else { return };
        let mut win = self.window_for(first);
        let npic = self.plan.units.last().map_or(0, |u| u.ts1);
        let result = catch_unwind(AssertUnwindSafe(|| {
            for (job, &meta) in self.slices.iter().zip(&self.metas) {
                let Some(&ts) = self.layout.rs_to_ts.get(job.sh.segment_address as usize) else {
                    self.fail(Error::Invalid("slice segment address out of range".into()));
                    continue;
                };
                let start = Start::At { pos: job.sh.data_offset, ts };
                let mode = Mode::Unit { end_ts: npic, hooks: Some(&hooks) };
                if let Err(e) = SliceDecoder::decode_unit(&mut win, job, meta, start, mode) {
                    self.fail(e);
                }
            }
        }));
        let row = hooks.row.get();
        if result.is_err() {
            self.panicked.store(true, Ordering::Relaxed);
            self.fail(Error::Invalid("internal error while decoding a picture".into()));
            // the rows not handed over yet count as done (unfiltered: the picture is grey)
            for r in row..self.plan.units.len() {
                self.finish_unit(r, None);
            }
            return;
        }
        // the current row, then the rows no slice reached
        self.send_row(row, win);
        for r in row + 1..self.plan.units.len() {
            self.finish_unit(r, None);
        }
    }

    /// A window for `unit` (re-used if one is free).
    fn window_for(&self, unit: &Unit) -> PicState<S> {
        match lock(&self.free).pop() {
            Some(mut w) => {
                w.reset_window(unit.rows.0, unit.rows.1, false);
                w
            }
            None => self.new_window(unit),
        }
    }

    /// Hand the window of finished row `r` to another thread: deblock it, then transfer.
    fn send_row(self: &Arc<Self>, r: usize, mut win: PicState<S>) {
        let job = self.clone();
        rayon::spawn(move || {
            let ok = catch_unwind(AssertUnwindSafe(|| crate::filter::deblock_unit(&mut win, r, r + 1)));
            if ok.is_err() {
                job.panicked.store(true, Ordering::Relaxed);
                job.fail(Error::Invalid("internal error while filtering a picture".into()));
            }
            job.finish_unit(r, Some(win));
        });
    }

    /// Unit `u` is done: queue its window for the stage, filter what can be, finish the picture
    /// after the last unit.
    fn finish_unit(&self, u: usize, win: Option<PicState<S>>) {
        self.finish_unit_last(u, win);
    }

    /// Start a helper thread if any is left; false if none.
    fn spawn_helper(self: &Arc<Self>) -> bool {
        if self.next.load(Ordering::Relaxed) >= self.plan.units.len() {
            return false;
        }
        let mut n = self.helpers.load(Ordering::Acquire);
        loop {
            if n == 0 {
                return false;
            }
            match self.helpers.compare_exchange_weak(n, n - 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(v) => n = v,
            }
        }
        let j = self.clone();
        rayon::spawn(move || j.work());
        true
    }

    /// Claim and decode units until none is left.
    fn work(self: &Arc<Self>) {
        self.work_from(None);
    }

    /// Decode `first` (a resumed unit), then claim and decode units until none is left. A WPP
    /// row whose row above is not far enough is parked on that row instead of waiting, and this
    /// thread goes on with other units; the row above resumes it (a task once it is far enough,
    /// or this thread itself once it is done), so a parked row never needs a free thread.
    fn work_from(self: &Arc<Self>, first: Option<Cont<S>>) {
        let mut next = first;
        loop {
            let cont = match next.take() {
                Some(c) => c,
                None => {
                    let u = self.next.fetch_add(1, Ordering::Relaxed);
                    let Some(unit) = self.plan.units.get(u) else { return };
                    Cont { unit: u, entry: 0, resume: None, win: self.window_for(unit), synced: 0, err: None }
                }
            };
            let u = cont.unit;
            let Some(unit) = self.plan.units.get(u) else { return };
            let win = match catch_unwind(AssertUnwindSafe(|| self.run_unit(cont))) {
                Ok(Run::Parked) => continue,
                Ok(Run::Done(c)) => {
                    if let Some(e) = c.err {
                        self.fail(e);
                    }
                    Some(c.win)
                }
                Err(_) => {
                    self.panicked.store(true, Ordering::Relaxed);
                    self.fail(Error::Invalid("internal error while decoding a picture".into()));
                    None
                }
            };
            // the row is done: the row below no longer waits for it; continue with it if parked
            if self.plan.wpp
                && let Some(e) = self.edges.get(unit.rows.0)
            {
                let mut g = lock(e);
                g.finished = true;
                next = g.parked.take().map(|(_, c)| c);
            }
            if self.finish_unit_last(u, win) {
                return;
            }
        }
    }

    /// [`Job::finish_unit`]; true if it was the last unit.
    fn finish_unit_last(&self, u: usize, win: Option<PicState<S>>) -> bool {
        lock(&self.pending).push((u, win));
        self.drain();
        let last = self.left.fetch_sub(1, Ordering::AcqRel) == 1;
        if last {
            self.finish();
        }
        last
    }

    /// Decode the entries of a unit from `c` on; parks it on the row above when that one is not
    /// far enough.
    fn run_unit(self: &Arc<Self>, mut c: Cont<S>) -> Run<S> {
        let Some(unit) = self.plan.units.get(c.unit) else { return Run::Done(c) };
        while let Some(e) = unit.entries.get(c.entry) {
            let (Some(job), Some(&meta)) = (self.slices.get(e.slice), self.metas.get(e.slice)) else {
                c.entry += 1;
                continue;
            };
            let hooks = RowHooks { job: self, synced: Cell::new(c.synced), need: Cell::new(0) };
            let mode = Mode::Unit { end_ts: unit.ts1, hooks: if self.plan.wpp { Some(&hooks) } else { None } };
            let start = match c.resume.take() {
                Some(r) => Start::Resume(r),
                None => Start::At { pos: e.pos, ts: e.ts },
            };
            let result = SliceDecoder::decode_unit(&mut c.win, job, meta, start, mode);
            c.synced = hooks.synced.get();
            match result {
                Ok(None) => c.entry += 1,
                Ok(Some(r)) => {
                    c.resume = Some(r);
                    match self.park(c, unit.rows.0, hooks.need.get()) {
                        Some(back) => c = back,
                        None => return Run::Parked,
                    }
                }
                Err(x) => {
                    c.err.get_or_insert(x);
                    c.entry += 1;
                }
            }
        }
        if self.unit_deblock {
            crate::filter::deblock_unit(&mut c.win, unit.rows.0, unit.rows.1);
        }
        Run::Done(c)
    }

    /// Park row `row` (continuation `c`) until the row above has `need` CTBs; gives it back when
    /// it already has them.
    fn park(&self, c: Cont<S>, row: usize, need: usize) -> Option<Cont<S>> {
        let Some(e) = row.checked_sub(1).and_then(|r| self.edges.get(r)) else { return Some(c) };
        let mut g = lock(e);
        if g.done >= need || g.finished || g.parked.is_some() {
            return Some(c);
        }
        g.parked = Some((need, c));
        g.resuming = false;
        None
    }

    /// Resume the row parked on row `above`, if it can go on.
    fn resume_parked(self: &Arc<Self>, above: usize) {
        let Some(e) = self.edges.get(above) else { return };
        let cont = {
            let mut g = lock(e);
            g.resuming = false;
            match &g.parked {
                Some((need, _)) if g.done >= *need || g.finished => g.parked.take().map(|(_, c)| c),
                _ => None,
            }
        };
        if let Some(c) = cont {
            self.work_from(Some(c));
        }
    }

    fn new_window(&self, unit: &Unit) -> PicState<S> {
        let mut w = PicState::window(self.frame.clone(), self.sps.clone(), self.pps.clone(), self.layout.clone(), self.scaling.clone(), 0, 0);
        if let Some(b) = self.shared.take_bufs::<S>() {
            w.put_bufs(b);
        }
        w.reset_window(unit.rows.0, unit.rows.1, false);
        w.slices = self.info.clone();
        w.deblocking_enabled_anywhere = self.deblocking;
        w.draft = self.draft;
        w
    }

    fn fail(&self, e: Error) {
        lock(&self.error).get_or_insert(e);
    }

    /// Copy decoded windows into the stage and run the filters, unless another thread is doing
    /// so (it then also takes the windows queued meanwhile).
    fn drain(&self) {
        loop {
            let mut stage = match self.stage.try_lock() {
                Ok(g) => g,
                Err(TryLockError::Poisoned(p)) => p.into_inner(),
                Err(TryLockError::WouldBlock) => return,
            };
            self.transfer_pending(&mut stage);
            drop(stage);
            // a window queued after the last look but before the lock was released is ours
            if lock(&self.pending).is_empty() {
                return;
            }
        }
    }

    fn transfer_pending(&self, stage: &mut Stage<S>) {
        let items = std::mem::take(&mut *lock(&self.pending));
        for (u, win) in items {
            if let Some(unit) = self.plan.units.get(u) {
                let ok = catch_unwind(AssertUnwindSafe(|| {
                    transfer(stage, unit, win.as_ref());
                    crate::filter::advance(&mut stage.pic, false);
                }));
                if ok.is_err() {
                    self.panicked.store(true, Ordering::Relaxed);
                    self.fail(Error::Invalid("internal error while filtering a picture".into()));
                }
            }
            if let Some(w) = win {
                lock(&self.free).push(w);
            }
        }
    }

    /// All units are decoded: filter and publish the rest of the picture, report.
    fn finish(&self) {
        let mut stage = lock(&self.stage);
        self.transfer_pending(&mut stage);
        let ok = catch_unwind(AssertUnwindSafe(|| crate::filter::finish(&mut stage.pic)));
        if ok.is_err() {
            self.panicked.store(true, Ordering::Relaxed);
            self.fail(Error::Invalid("internal error while filtering a picture".into()));
        }
        if self.panicked.load(Ordering::Relaxed) {
            // make sure readers never block on this frame
            stage.pic.frame.publish_gray();
        }
        for mut w in lock(&self.free).drain(..) {
            self.shared.put_bufs(w.take_bufs());
        }
        let err = lock(&self.error).take();
        self.shared.report(&stage.pic, err, true);
    }
}

/// Copy the decoded CTBs of `unit` from its window (none: the unit failed) into the stage and
/// count its rows' CTBs.
fn transfer<S: Sample>(stage: &mut Stage<S>, unit: &Unit, win: Option<&PicState<S>>) {
    let pic = &mut stage.pic;
    let ctb = 1usize << pic.log2_ctb;
    let (r0, r1) = unit.rows;
    let wctb = pic.wctb;
    let (c0, c1) = (unit.cols.0.min(wctb), unit.cols.1.min(wctb));
    for r in r0..r1.min(pic.hctb) {
        if let Some(n) = stage.ctbs.get_mut(r) {
            *n += c1 - c0;
        }
        let Some(win) = win else { continue };
        // runs of decoded CTBs (usually the whole row of the unit)
        let decoded = |x: usize| win.ctb_slice.get(r * wctb + x).is_some_and(|&s| s != u32::MAX);
        let mut x = c0;
        while x < c1 {
            if !decoded(x) {
                x += 1;
                continue;
            }
            let start = x;
            while x < c1 && decoded(x) {
                x += 1;
            }
            copy_ctbs(pic, win, r, start, x, ctb);
        }
        let (a, b) = (r * wctb + c0, r * wctb + c1);
        for (dst, src) in [(&mut pic.ctb_slice, &win.ctb_slice), (&mut pic.ctb_addr, &win.ctb_addr)] {
            if let (Some(s), Some(d)) = (src.get(a..b), dst.get_mut(a..b)) {
                d.copy_from_slice(s);
            }
        }
        if let (Some(s), Some(d)) = (win.sao.get(a..b), pic.sao.get_mut(a..b)) {
            d.copy_from_slice(s);
        }
    }
    while pic.rows_complete < pic.hctb && stage.ctbs.get(pic.rows_complete).is_some_and(|&n| n >= wctb) {
        pic.rows_complete += 1;
    }
}

/// Copy the samples, block state and motion of CTBs `x0..x1` of row `r` from `win` into `pic`.
fn copy_ctbs<S: Sample>(pic: &mut PicState<S>, win: &PicState<S>, r: usize, x0: usize, x1: usize, ctb: usize) {
    for c in 0..3 {
        let (pw, ph, cs) = if c == 0 { (pic.width, pic.height, ctb) } else { (pic.cwidth, pic.cheight, ctb / 2) };
        let (a, b) = ((x0 * cs).min(pw), (x1 * cs).min(pw));
        let yb = win.line0(c);
        for y in (r * cs).min(ph)..((r + 1) * cs).min(ph) {
            let src = win.planes[c].get((y - yb) * pw + a..(y - yb) * pw + b);
            let dst = pic.planes[c].get_mut(y * pw + a..y * pw + b);
            if let (Some(s), Some(d)) = (src, dst) {
                d.copy_from_slice(s);
            }
        }
    }
    let (w4, h4) = (pic.w4, pic.h4);
    let (a, b) = ((x0 * ctb / 4).min(w4), (x1 * ctb / 4).min(w4));
    let yb = win.y_base / 4;
    for by in (r * ctb / 4).min(h4)..((r + 1) * ctb / 4).min(h4) {
        let (s, d) = ((by - yb) * w4, by * w4);
        if let (Some(src), Some(dst)) = (win.blk.get(s + a..s + b), pic.blk.get_mut(d + a..d + b)) {
            dst.copy_from_slice(src);
        }
        if let (Some(src), Some(dst)) = (win.mvf.get(s + a..s + b), pic.mvf.get_mut(d + a..d + b)) {
            dst.copy_from_slice(src);
        }
    }
}

/// Wavefront synchronisation of the window of one CTB row with the row above.
struct RowHooks<'j, S> {
    job: &'j Arc<Job<S>>,
    /// CTBs of the row above copied in so far.
    synced: Cell<usize>,
    /// CTBs of the row above needed when `before` said to stop.
    need: Cell<usize>,
}

impl<S: Sample> CtuHooks<S> for RowHooks<'_, S> {
    fn before(&self, pic: &mut PicState<S>, rx: usize, ry: usize) -> bool {
        let wctb = pic.wctb;
        // CTB rx reads the row above up to its above-right CTB (6.4.1), and its contexts are
        // those stored after the second CTB of the row above (9.3.2.4)
        let need = (rx + 2).min(wctb);
        let synced = self.synced.get();
        if ry == 0 || synced >= need {
            return true;
        }
        let Some(e) = self.job.edges.get(ry - 1) else { return true };
        let g = lock(e);
        if g.done < need && !g.finished {
            self.need.set(need);
            return false;
        }
        let upto = if g.finished { wctb } else { g.done.min(wctb) };
        let ctb = 1usize << pic.log2_ctb;
        // the bottom line of the row above is the window's line 3 (luma), 1 (chroma), the
        // bottom 4x4 row its first one
        let line = |c: usize| if c == 0 { (ry * ctb - 1) - pic.y_base } else { (ry * ctb / 2 - 1) - pic.y_base / 2 };
        for c in 0..3 {
            let (pw, cs) = if c == 0 { (pic.width, ctb) } else { (pic.cwidth, ctb / 2) };
            let (x0, x1) = ((synced * cs).min(pw), (upto * cs).min(pw));
            let src = match c {
                0 => &g.y,
                1 => &g.cb,
                _ => &g.cr,
            };
            let o = line(c) * pw;
            if let (Some(s), Some(d)) = (src.get(x0..x1), pic.planes[c].get_mut(o + x0..o + x1)) {
                d.copy_from_slice(s);
            }
        }
        let (x0, x1) = ((synced * ctb / 4).min(pic.w4), (upto * ctb / 4).min(pic.w4));
        if let (Some(s), Some(d)) = (g.blk.get(x0..x1), pic.blk.get_mut(x0..x1)) {
            d.copy_from_slice(s);
        }
        if let (Some(s), Some(d)) = (g.mvf.get(x0..x1), pic.mvf.get_mut(x0..x1)) {
            d.copy_from_slice(s);
        }
        let base = (ry - 1) * wctb;
        for x in synced..upto {
            if let (Some(&(slice, addr, sao)), Some(rs)) = (g.ctb.get(x), base.checked_add(x)) {
                if let Some(v) = pic.ctb_slice.get_mut(rs) {
                    *v = slice;
                }
                if let Some(v) = pic.ctb_addr.get_mut(rs) {
                    *v = addr;
                }
                if let Some(v) = pic.sao.get_mut(rs) {
                    *v = sao;
                }
            }
        }
        if synced == 0 {
            pic.wpp_ctx = g.wpp.clone();
        }
        self.synced.set(upto);
        true
    }

    fn after(&self, pic: &mut PicState<S>, rx: usize, ry: usize, stored_wpp: bool) {
        if ry + 1 >= pic.hctb {
            return; // no row below
        }
        let Some(e) = self.job.edges.get(ry) else { return };
        let ctb = 1usize << pic.log2_ctb;
        let mut guard = lock(e);
        let g = &mut *guard;
        if g.y.len() != pic.width {
            g.y = vec![S::default(); pic.width];
            g.cb = vec![S::default(); pic.cwidth];
            g.cr = vec![S::default(); pic.cwidth];
            g.blk = vec![Blk::default(); pic.w4];
            g.mvf = vec![MvField::default(); pic.w4];
            g.ctb = vec![(u32::MAX, u32::MAX, SaoParams::default()); pic.wctb];
        }
        for c in 0..3 {
            let (pw, cs) = if c == 0 { (pic.width, ctb) } else { (pic.cwidth, ctb / 2) };
            let line = ((ry + 1) * cs - 1).saturating_sub(pic.line0(c));
            let (x0, x1) = ((rx * cs).min(pw), ((rx + 1) * cs).min(pw));
            let dst = match c {
                0 => &mut g.y,
                1 => &mut g.cb,
                _ => &mut g.cr,
            };
            let o = line * pw;
            if let (Some(s), Some(d)) = (pic.planes[c].get(o + x0..o + x1), dst.get_mut(x0..x1)) {
                d.copy_from_slice(s);
            }
        }
        let row = ((ry + 1) * ctb / 4 - 1).saturating_sub(pic.y_base / 4) * pic.w4;
        let (x0, x1) = ((rx * ctb / 4).min(pic.w4), ((rx + 1) * ctb / 4).min(pic.w4));
        if let (Some(s), Some(d)) = (pic.blk.get(row + x0..row + x1), g.blk.get_mut(x0..x1)) {
            d.copy_from_slice(s);
        }
        if let (Some(s), Some(d)) = (pic.mvf.get(row + x0..row + x1), g.mvf.get_mut(x0..x1)) {
            d.copy_from_slice(s);
        }
        let rs = ry * pic.wctb + rx;
        if let (Some(slot), Some(&slice), Some(&addr), Some(&sao)) = (g.ctb.get_mut(rx), pic.ctb_slice.get(rs), pic.ctb_addr.get(rs), pic.sao.get(rs)) {
            *slot = (slice, addr, sao);
        }
        if stored_wpp {
            g.wpp = pic.wpp_ctx.clone();
        }
        g.done = g.done.max(rx + 1);
        // the row below is parked and can go on now: resume it on another thread
        let resume = !g.resuming && g.parked.as_ref().is_some_and(|(need, _)| g.done >= *need);
        if resume {
            g.resuming = true;
        }
        drop(guard);
        if resume {
            let j = self.job.clone();
            rayon::spawn(move || j.resume_parked(ry));
        }
        // the next row can start: give it a thread
        if rx + 1 == 2.min(pic.wctb) {
            self.job.spawn_helper();
        }
    }
}

/// Serial decoding into one window per CTB row: at the end of each row the window is handed to
/// another thread ([`Job::send_row`]) and decoding continues in a fresh one, which gets what the
/// next row reads of the finished row (its bottom sample lines, 4x4 block state and motion, CTB
/// slice addresses and SAO parameters, stored contexts).
struct PipeHooks<'j, S> {
    job: &'j Arc<Job<S>>,
    /// Row of the current window.
    row: Cell<usize>,
}

impl<S: Sample> PipeHooks<'_, S> {
    /// Hand over the current row and continue in a window for the next one.
    fn rotate(&self, pic: &mut PicState<S>) {
        let r = self.row.get();
        let Some(unit) = self.job.plan.units.get(r + 1) else { return };
        let mut next = self.job.window_for(unit);
        let ctb = 1usize << pic.log2_ctb;
        for c in 0..3 {
            let (pw, cs) = if c == 0 { (pic.width, ctb) } else { (pic.cwidth, ctb / 2) };
            let line = (r + 1) * cs - 1;
            let (s, d) = ((line - pic.line0(c)) * pw, (line - next.line0(c)) * pw);
            if let (Some(src), Some(dst)) = (pic.planes[c].get(s..s + pw), next.planes[c].get_mut(d..d + pw)) {
                dst.copy_from_slice(src);
            }
        }
        let by = (r + 1) * ctb / 4 - 1;
        let w4 = pic.w4;
        let (s, d) = ((by - pic.y_base / 4) * w4, (by - next.y_base / 4) * w4);
        if let (Some(src), Some(dst)) = (pic.blk.get(s..s + w4), next.blk.get_mut(d..d + w4)) {
            dst.copy_from_slice(src);
        }
        if let (Some(src), Some(dst)) = (pic.mvf.get(s..s + w4), next.mvf.get_mut(d..d + w4)) {
            dst.copy_from_slice(src);
        }
        let ctbs = r * pic.wctb..(r + 1) * pic.wctb;
        if let (Some(src), Some(dst)) = (pic.ctb_slice.get(ctbs.clone()), next.ctb_slice.get_mut(ctbs.clone())) {
            dst.copy_from_slice(src);
        }
        if let (Some(src), Some(dst)) = (pic.ctb_addr.get(ctbs.clone()), next.ctb_addr.get_mut(ctbs.clone())) {
            dst.copy_from_slice(src);
        }
        if let (Some(src), Some(dst)) = (pic.sao.get(ctbs.clone()), next.sao.get_mut(ctbs)) {
            dst.copy_from_slice(src);
        }
        next.wpp_ctx = pic.wpp_ctx.take();
        next.ds_ctx = pic.ds_ctx.take();
        std::mem::swap(pic, &mut next);
        self.row.set(r + 1);
        self.job.send_row(r, next);
    }
}

impl<S: Sample> CtuHooks<S> for PipeHooks<'_, S> {
    fn before(&self, pic: &mut PicState<S>, _rx: usize, ry: usize) -> bool {
        // a slice starting in a later row: hand over the rows in between
        while self.row.get() < ry && self.row.get() + 1 < self.job.plan.units.len() {
            self.rotate(pic);
        }
        true
    }

    fn after(&self, pic: &mut PicState<S>, rx: usize, ry: usize, _stored_wpp: bool) {
        if rx + 1 == pic.wctb && ry == self.row.get() {
            self.rotate(pic);
        }
    }
}
