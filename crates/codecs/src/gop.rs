//! GOP-aware random access shared by the container sources (MP4/MOV, Matroska/WebM).
//!
//! Seeking: find the sample whose presentation interval covers the requested time, decode forward
//! from the preceding sync sample, and cache every decoded frame (keyed by pts). Playback requests
//! for the next frames therefore hit the cache or continue the running decoder without re-seeking.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
use std::time::Duration;

use web_time::Instant;

use filmcraft_color::ColorInfo;
use filmcraft_frame::{Region, VideoFrame};

use crate::CodecError;
use crate::video::VideoDecoder;

/// Process-wide decode counters of every [`GopCache`] (benchmarks and diagnostics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GopStats {
    /// Requests answered from the decoded-frame cache.
    pub hits: u64,
    /// Requests that had to decode.
    pub misses: u64,
    /// Decoder restarts at a sync sample (seeks).
    pub seeks: u64,
    /// Samples fed to decoders.
    pub decoded: u64,
    /// Decoded frames evicted from the cache.
    pub evicted: u64,
    /// Wall time spent inside decoders (`decode` / `flush`), nanoseconds.
    pub decode_ns: u64,
    /// Non-reference samples left out while catching up (frames already late).
    pub skipped: u64,
    /// Late requests for a skipped sample answered with nothing instead of a decoder restart.
    pub dropped: u64,
    /// Frames decoded in draft mode (reduced-resolution playback, [`VideoDecoder::set_draft`]).
    pub draft: u64,
    /// Pictures decoders output (hardware and software; [`crate::hw::hw_stats`] counts the
    /// hardware ones).
    pub frames: u64,
}

impl GopStats {
    /// Share of requests answered from the decoded-frame cache (0 when there were none).
    pub fn hit_rate(&self) -> f64 {
        let n = self.hits + self.misses;
        if n == 0 { 0.0 } else { self.hits as f64 / n as f64 }
    }

    /// Mean decoder wall time per sample fed (ms).
    pub fn decode_ms_per_sample(&self) -> f64 {
        if self.decoded == 0 { 0.0 } else { self.decode_ns as f64 / 1e6 / self.decoded as f64 }
    }
}

static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static SEEKS: AtomicU64 = AtomicU64::new(0);
static DECODED: AtomicU64 = AtomicU64::new(0);
static EVICTED: AtomicU64 = AtomicU64::new(0);
static DECODE_NS: AtomicU64 = AtomicU64::new(0);
static SKIPPED: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static DRAFT: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);

/// Count pictures a decoder call output.
fn count_frames<T>(out: &[T]) {
    FRAMES.fetch_add(out.len() as u64, Ordering::Relaxed);
}

/// Run a decoder call, adding its wall time to the process-wide counter.
fn timed<R>(f: impl FnOnce() -> R) -> R {
    let t0 = Instant::now();
    let r = f();
    DECODE_NS.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    r
}

/// The counters so far (they only grow; subtract two snapshots to measure an interval).
pub fn gop_stats() -> GopStats {
    GopStats {
        hits: HITS.load(Ordering::Relaxed),
        misses: MISSES.load(Ordering::Relaxed),
        seeks: SEEKS.load(Ordering::Relaxed),
        decoded: DECODED.load(Ordering::Relaxed),
        evicted: EVICTED.load(Ordering::Relaxed),
        decode_ns: DECODE_NS.load(Ordering::Relaxed),
        skipped: SKIPPED.load(Ordering::Relaxed),
        dropped: DROPPED.load(Ordering::Relaxed),
        draft: DRAFT.load(Ordering::Relaxed),
        frames: FRAMES.load(Ordering::Relaxed),
    }
}

impl std::ops::Sub for GopStats {
    type Output = GopStats;
    fn sub(self, o: GopStats) -> GopStats {
        GopStats {
            hits: self.hits - o.hits,
            misses: self.misses - o.misses,
            seeks: self.seeks - o.seeks,
            decoded: self.decoded - o.decoded,
            evicted: self.evicted - o.evicted,
            decode_ns: self.decode_ns - o.decode_ns,
            skipped: self.skipped - o.skipped,
            dropped: self.dropped - o.dropped,
            draft: self.draft - o.draft,
            frames: self.frames - o.frames,
        }
    }
}

/// A container's video sample table, in decode (file) order. `Sync`: a cache decodes on its own
/// threads ([`isolated`]).
pub trait VideoSamples: Sync {
    fn count(&self) -> usize;
    /// Presentation timestamp of sample `i` (track units).
    fn pts(&self, i: usize) -> i64;
    /// Nearest sync sample at or before `i`.
    fn sync_before(&self, i: usize) -> usize;
    /// The sample presented at `t` (track units), if any.
    fn sample_at(&self, t: i64) -> Option<usize>;
    fn read(&self, i: usize) -> crate::Result<Vec<u8>>;
    /// Bytes sample `i` holds, from the index and without reading it. Implementations that know
    /// it should say so: the random-access search checks a candidate against its byte budget
    /// before [`read`](VideoSamples::read) allocates it. `None`: not known (read, then count).
    fn sample_size(&self, _i: usize) -> Option<u64> {
        None
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>>;
}

/// The track time (units of `num / den` s) to look up the frame shown at `t` by, for a track whose
/// nominal rate is `rate`. `snap`: move the request back to the start of its frame first, see
/// [`snap_coarse_units`].
///
/// Rounded to the nearest unit (#73): muxers round frame times to the timescale (Matroska usually
/// to 1 ms), so a stamp can sit up to half a unit after the true time, and flooring missed every
/// frame stamped late.
pub(crate) fn lookup_time(t: filmcraft_time::Tick, num: i64, den: i64, rate: filmcraft_time::FrameRate, snap: bool) -> i64 {
    (if snap { rate.snap(t) } else { t }).to_rational_round(num, den)
}

/// Whether to look frames up by first moving the request back to the start of its frame (nominal
/// rate `rate`), for a track whose samples are stamped `stamps` (units of `num / den` s).
///
/// #73's tolerance of half a unit is only harmless while a unit is short against a frame. With a
/// unit of half a frame or more (an MP4 whose timescale is its frame rate, one unit per frame),
/// half a unit is a quarter of a frame or more, and from its middle on every frame was looked up
/// as the next one. Such a track can't place frames between its units anyway, so the request
/// first moves back to the start of its frame, which then rounds onto that frame's own stamp.
///
/// Only when every stamp is where that lookup lands for its nearest nominal frame: a coarse unit
/// is no constant rate. A variable-rate track with samples 2, 1, 2 units long at two units a frame
/// has a stamp between two nominal frames, and snapping would move a request at it onto the
/// frame before. Such a track keeps #73's lookup by the nearest unit.
pub(crate) fn snap_coarse_units(stamps: impl IntoIterator<Item = i64>, num: i64, den: i64, rate: filmcraft_time::FrameRate) -> bool {
    let unit = filmcraft_time::Tick::from_rational(1, num, den);
    if unit.0.saturating_mul(2) < rate.frame_duration().0 {
        return false;
    }
    stamps.into_iter().all(|p| lookup_time(rate.snap_nearest(filmcraft_time::Tick::from_rational(p, num, den)), num, den, rate, true) == p)
}

/// A container's sync sample for `i` when leading pictures need the GOP before: `sync` is the
/// index's own sync sample, `leads(i, key)` says `i` is shown before `key`. One step per GOP, so a
/// loop: a crafted index can chain tens of thousands of GOPs (#2803). A `sync` past `i` stops the
/// walk instead of looping on it.
pub(crate) fn open_gop_sync(mut i: usize, sync: impl Fn(usize) -> usize, leads: impl Fn(usize, usize) -> bool) -> usize {
    loop {
        let key = sync(i);
        if key == 0 || key > i || !leads(i, key) {
            return key;
        }
        i = key - 1;
    }
}

/// The sync sample to start decoding at for sample `i`: its sync sample, or an earlier one when
/// `i` is a leading picture of an open GOP (stored after its random-access picture, shown before
/// it, like the RASL pictures after every HEVC CRA of iPhone video). Those are predicted from the
/// GOP before, so a decoder starting at their random-access picture leaves them out.
fn decode_start(s: &dyn VideoSamples, i: usize) -> usize {
    let mut key = s.sync_before(i);
    while key > 0 && s.pts(i) < s.pts(key) {
        key = s.sync_before(key - 1).min(key - 1);
    }
    key
}

struct State {
    decoder: Option<Box<dyn VideoDecoder>>,
    /// The decoder only produces intra pictures: frames decode independently and in parallel.
    intra: bool,
    /// Idle decoders for parallel intra decoding.
    spare: Vec<Box<dyn VideoDecoder>>,
    /// Next sample (decode order) to feed, and the sync sample the current run started from.
    next: usize,
    start: usize,
    /// Highest pts the running decoder has output since it started (pictures leave in pts order).
    out_max: i64,
    /// A thread is decoding ahead for playback ([`GopCache::pump`]) with the decoder taken out
    /// of this state while a sample decodes; requests for the pictures to come wait on
    /// [`Shared::cv`] instead of decoding.
    pump: bool,
    /// A request with time to spare (paused, scrubbing, an export) waits for playback decoding
    /// to stop: it stops at the next sample and does not start again until that request is done.
    pump_stop: bool,
    /// The latest playing request, which playback decoding follows.
    play: Option<Play>,
    /// Frames (by pts) requests are waiting for in [`GopCache::await_pump`], with how many:
    /// never evicted from under them.
    waiting: BTreeMap<i64, u32>,
    /// Decoded frames by presentation pts (bounded).
    frames: BTreeMap<i64, Arc<VideoFrame>>,
    /// The cached frames that were decoded in draft mode: served to draft requests only.
    drafts: std::collections::BTreeSet<i64>,
    /// The cached frames still waiting for [`GopCache::finish`]'s crop and rotation: the request
    /// that takes one does it off the lock (#3467).
    raw: std::collections::BTreeSet<i64>,
    bytes: usize,
    /// When a frame was last asked of this cache.
    last_used: Instant,
    /// Presentation pts of the frame the last request wanted: eviction keeps a window around it
    /// (not around the newest decoded picture). Walking a long GOP dumps pictures in decode
    /// order; a window around those would throw away the playhead frame and force another walk.
    pin: i64,
    /// Only background requests (thumbnails, [`filmcraft_media::cancel::with_background`]) have
    /// used the decoder since it was made: it is given up as soon as another background request
    /// is done, whatever the caches' recency.
    background: bool,
}

impl State {
    /// Give up the decoder (and the spare intra decoders): the next request that has to decode
    /// makes a new one and restarts at a sync sample. Returned so they are dropped off the lock.
    fn release_decoder(&mut self, pool: &Pool) -> Vec<Box<dyn VideoDecoder>> {
        if self.pump {
            // playback decoding has the decoder out and is using this state
            return Vec::new();
        }
        let mut out = std::mem::take(&mut self.spare);
        if let Some(d) = self.decoder.take() {
            pool.decoders.fetch_sub(1, Ordering::Relaxed);
            out.push(d);
        }
        self.next = usize::MAX;
        out
    }

    /// Evict frames, earliest first, while `more` says so.
    fn release_frames(&mut self, pool: &Pool, more: impl Fn() -> bool) {
        while more() {
            let Some((pts, f)) = self.frames.pop_first() else { break };
            self.drafts.remove(&pts);
            self.raw.remove(&pts);
            self.bytes = self.bytes.saturating_sub(f.byte_size());
            pool.bytes.fetch_sub(f.byte_size(), Ordering::Relaxed);
            EVICTED.fetch_add(1, Ordering::Relaxed);
            filmcraft_frame::pool::recycle(f);
        }
    }

    /// The cached frame at `pts`, unless it is a draft frame and the request wants the exact one.
    fn cached(&self, pts: i64, draft_ok: bool) -> Option<&Arc<VideoFrame>> {
        self.frames.get(&pts).filter(|_| draft_ok || !self.drafts.contains(&pts))
    }

    /// The nearest usable frame at or before `pts` (robust to decoder pts quirks).
    fn at_or_before(&self, pts: i64, draft_ok: bool) -> Option<Taken> {
        let (pts, frame) = match self.cached(pts, draft_ok) {
            Some(f) => (pts, f),
            None => self.frames.range(..=pts).rev().find(|(p, _)| draft_ok || !self.drafts.contains(p)).map(|(p, f)| (*p, f))?,
        };
        Some(Taken { pts, frame: frame.clone(), raw: self.raw.contains(&pts) })
    }
}

/// A playing request ([`filmcraft_media::cancel::playing`]): the sample wanted, the pts before
/// which frames are late (the playhead), and when it came.
#[derive(Clone, Copy)]
struct Play {
    i: usize,
    late: i64,
    at: Instant,
}

/// What a request found when playback decoding was running: its frame, or the state to go on with.
enum Awaited<'a> {
    Frame(Taken),
    Go(MutexGuard<'a, State>),
}

/// A cached frame handed to a request, `raw` while it still waits for [`GopCache::finish`].
struct Taken {
    pts: i64,
    frame: Arc<VideoFrame>,
    raw: bool,
}

thread_local! {
    /// Caches whose lock this thread holds while decoding (see [`GopCache::frame`]).
    static DECODING: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Marks a cache as decoding on this thread until dropped (also on early return / `?`).
struct DecodingGuard(usize);

impl DecodingGuard {
    fn enter(cache: usize) -> Self {
        DECODING.with(|d| d.borrow_mut().push(cache));
        Self(cache)
    }
    fn active(cache: usize) -> bool {
        DECODING.with(|d| d.borrow().contains(&cache))
    }
}

impl Drop for DecodingGuard {
    fn drop(&mut self) {
        DECODING.with(|d| {
            let mut d = d.borrow_mut();
            if let Some(k) = d.iter().rposition(|&c| c == self.0) {
                d.remove(k);
            }
        });
    }
}

/// A decoder keeps its reference and in-flight pictures (hundreds of MB for a frame-threaded
/// 4K decoder), so only this many caches keep theirs once idle: a timeline's other clips make a
/// new one when the playhead reaches them, which costs what a seek costs.
const MAX_LIVE_DECODERS: usize = 4;

/// A cache asked for a frame this recently may be needed again soon (the next clip being
/// prefetched, a clip the user scrubs back to): it keeps its decoder up to [`HARD_DECODERS`], and
/// beyond [`MAX_LIVE_DECODERS`] only until it has been idle this long. Its frames are only kept
/// within [`FRAME_BUDGET`] once it has been idle for [`RECENT`]: going back to a clip left (or
/// paused) more than that long ago may cost a re-decode from its keyframe when other clips have
/// filled the budget meanwhile. Frames go least recently used cache first, earliest frames first.
const IDLE: Duration = Duration::from_secs(3);

/// A cache asked for a frame this recently is in use (a layer of the frame being composited): it
/// keeps its decoder and frames whatever the count. Caches idle for less than [`IDLE`] but longer
/// than this are a burst of navigation (thumbnails of every clip, the playhead jumping from clip
/// to clip) that the playhead has already left: they give up decoders beyond [`HARD_DECODERS`]
/// and frames beyond [`FRAME_BUDGET`]. Without this, everything touched within [`IDLE`] counted
/// as in use, so a burst held every clip's decoder (42 live hardware sessions, 6.5 GB of VRAM)
/// and hundreds of MB of frames per clip.
const RECENT: Duration = Duration::from_millis(500);

/// Most decoders caches idle for [`RECENT`] to [`IDLE`] keep together (a composite of a few
/// layers plus the clips just left behind).
const HARD_DECODERS: usize = 8;

/// Decoded frames all caches may hold together before idle ones give theirs up. Each cache also
/// has its own budget, which alone let every clip on a timeline keep hundreds of MB of frames
/// nobody was looking at.
pub const FRAME_BUDGET: usize = 1 << 30;

/// What the caches of a process share: the cap on live decoders and the frame budget.
struct Pool {
    caches: Mutex<Vec<Weak<Shared>>>,
    /// Caches that hold a decoder.
    decoders: AtomicUsize,
    max_decoders: usize,
    idle: Duration,
    /// Caches idle at least this long are no longer in use (see [`RECENT`]).
    recent: Duration,
    /// Decoders the caches idle for [`Self::recent`] but not [`Self::idle`] may keep.
    hard_decoders: usize,
    /// Bytes of the frames the caches hold.
    bytes: AtomicUsize,
    budget: usize,
}

/// The part of a [`GopCache`] its pool reaches.
struct Shared {
    state: Mutex<State>,
    /// Signalled when playback decoding stored pictures or stopped (see [`State::pump`]).
    cv: std::sync::Condvar,
    pool: Arc<Pool>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        let st = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        drop(st.release_decoder(&self.pool));
        self.pool.bytes.fetch_sub(st.bytes, Ordering::Relaxed);
    }
}

/// A cache's state unless another thread is using it (it is then decoding: not idle).
fn try_state(c: &Shared) -> Option<MutexGuard<'_, State>> {
    match c.state.try_lock() {
        Ok(g) => Some(g),
        Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

impl Pool {
    fn new(max_decoders: usize, idle: Duration, budget: usize) -> Self {
        Self {
            caches: Mutex::new(Vec::new()),
            decoders: AtomicUsize::new(0),
            max_decoders,
            idle,
            recent: idle,
            hard_decoders: max_decoders,
            bytes: AtomicUsize::new(0),
            budget,
        }
    }

    /// Caches idle for `recent` (< the idle time) are still trimmed, down to `hard_decoders`.
    fn tiered(mut self, recent: Duration, hard_decoders: usize) -> Self {
        self.recent = recent.min(self.idle);
        self.hard_decoders = hard_decoders.max(self.max_decoders);
        self
    }

    /// The pool of every cache in the process.
    fn global() -> Arc<Pool> {
        static POOL: std::sync::OnceLock<Arc<Pool>> = std::sync::OnceLock::new();
        POOL.get_or_init(|| Arc::new(Pool::new(MAX_LIVE_DECODERS, IDLE, FRAME_BUDGET).tiered(RECENT, HARD_DECODERS))).clone()
    }

    fn register(&self, c: &Arc<Shared>) {
        let mut g = self.caches.lock().unwrap_or_else(PoisonError::into_inner);
        g.retain(|w| w.strong_count() > 0);
        g.push(Arc::downgrade(c));
    }

    /// The caches other than `me` idle for at least `idle` that `keep` selects, least recently used first. Other
    /// caches are only ever try-locked, so two caches trimming each other cannot deadlock.
    fn idle_others(&self, me: &Shared, idle: Duration, keep: impl Fn(&State) -> bool) -> Vec<Arc<Shared>> {
        let all: Vec<Arc<Shared>> = self.caches.lock().unwrap_or_else(PoisonError::into_inner).iter().filter_map(Weak::upgrade).collect();
        let mut idle: Vec<(Instant, Arc<Shared>)> = all
            .into_iter()
            .filter(|c| !std::ptr::eq(Arc::as_ptr(c), me))
            .filter_map(|c| {
                let used = try_state(&c).filter(|st| keep(st) && st.last_used.elapsed() >= idle).map(|st| st.last_used)?;
                Some((used, c))
            })
            .collect();
        idle.sort_by_key(|(used, _)| *used);
        idle.into_iter().map(|(_, c)| c).collect()
    }

    /// Take the decoders of idle caches, least recently used first, down to the cap. The caller
    /// drops them with no cache lock held: dropping a decoder can block (a hardware session waits
    /// for its frames in flight).
    #[must_use = "drop the released decoders with no cache lock held"]
    fn trim_decoders(&self, me: &Shared) -> Vec<Box<dyn VideoDecoder>> {
        let mut released = Vec::new();
        // first the caches idle for long, down to the cap; then those a burst has left behind
        for (idle, cap) in [(self.idle, self.max_decoders), (self.recent, self.hard_decoders)] {
            if self.decoders.load(Ordering::Relaxed) <= cap {
                continue;
            }
            for c in self.idle_others(me, idle, |st| st.decoder.is_some()) {
                if self.decoders.load(Ordering::Relaxed) <= cap {
                    break;
                }
                if let Some(mut st) = try_state(&c) {
                    released.extend(st.release_decoder(self));
                }
            }
        }
        released
    }

    /// Take the decoders of the caches other than `me` that only background requests have used
    /// (see [`State::background`]) and that are not decoding now. A burst of thumbnails (an import
    /// of many clips) touches every clip within moments, so the recency rules treat them all as
    /// in use; each software 4K decoder holds ~0.5 GB. The cache `me` just served keeps its
    /// decoder (hovering a clip asks for more of its frames). Drop the result with no cache lock
    /// held.
    #[must_use = "drop the released decoders with no cache lock held"]
    fn release_background(&self, me: &Shared) -> Vec<Box<dyn VideoDecoder>> {
        let all: Vec<Arc<Shared>> = self.caches.lock().unwrap_or_else(PoisonError::into_inner).iter().filter_map(Weak::upgrade).collect();
        let mut released = Vec::new();
        for c in all.iter().filter(|c| !std::ptr::eq(Arc::as_ptr(c), me)) {
            if let Some(mut st) = try_state(c)
                && st.background
                && st.decoder.is_some()
            {
                released.extend(st.release_decoder(self));
            }
        }
        released
    }

    /// Empty every cache whose lock is free (see [`release_idle`]). Decoders are dropped with no
    /// cache lock held, as for [`Self::trim_decoders`].
    fn release_idle(&self) {
        let all: Vec<Arc<Shared>> = self.caches.lock().unwrap_or_else(PoisonError::into_inner).iter().filter_map(Weak::upgrade).collect();
        let mut released = Vec::new();
        for c in &all {
            // try-locked: a cache another thread holds is decoding, so it is in use
            if let Some(mut st) = try_state(c) {
                st.release_frames(self, || true);
                released.extend(st.release_decoder(self));
            }
        }
        drop(released);
    }

    /// Evict the frames of caches idle for [`RECENT`], least recently used first, down to the
    /// budget. Caches in use keep theirs (each within its own budget): a frame evicted before it is shown costs a
    /// re-decode from the keyframe.
    fn trim_frames(&self, me: &Shared) {
        let over = || self.bytes.load(Ordering::Relaxed) > self.budget;
        if !over() {
            return;
        }
        for c in self.idle_others(me, self.recent, |st| !st.frames.is_empty()) {
            if !over() {
                break;
            }
            if let Some(mut st) = try_state(&c) {
                st.release_frames(self, over);
            }
        }
    }
}

/// Free the decoded frames and the decoders of every cache that nobody is using right now,
/// process-wide. The frame budget only trims caches that were idle for [`RECENT`] while another
/// cache is asking for frames, so after a long job (an export) every source keeps up to
/// [`FRAME_CACHE_CEILING`] of frames and its decoders until the session ends. Whoever finishes
/// such a job calls this. A cache a thread is decoding on (its lock is held) is skipped, so
/// concurrent users lose nothing; a cache emptied here only re-decodes from a sync sample when
/// asked again.
pub fn release_idle() {
    Pool::global().release_idle();
}

/// Caches that hold a decoder right now, process-wide (`perf.stats`).
pub fn live_decoders() -> usize {
    Pool::global().decoders.load(Ordering::Relaxed)
}

/// Bytes of decoded frames the caches hold right now, process-wide (`perf.stats`).
pub fn cached_bytes() -> usize {
    Pool::global().bytes.load(Ordering::Relaxed)
}

/// Decoder + decoded-frame cache for one video track.
pub struct GopCache {
    shared: Arc<Shared>,
    /// Colour signalled by the container, which wins over the bitstream's.
    explicit_color: Option<ColorInfo>,
    /// Clockwise quarter turns from the container's display matrix, applied to every frame.
    rotation: u8,
    /// The container's clean aperture, cut from every frame before the rotation.
    crop: Option<Region>,
    budget: usize,
}

/// The cache reserves room for this many small frames: a frame-threaded decoder runs up to ~2x
/// its thread count ahead of the frame it returns, and playback prefetches ahead of the playhead.
const MIN_FRAMES: usize = 64;
/// A source cache never retains more decoded pixels than this, regardless of frame dimensions.
/// In particular, untrusted VP9 `show_existing_frame` samples must not turn the small-frame
/// reserve above into an unbounded number of full-resolution copies.
const FRAME_CACHE_CEILING: usize = 384 << 20;
/// While playing, the playing source's cache holds this many frames whatever their size (within
/// [`PLAY_CACHE_CEILING`]): the pictures a frame-threaded decoder has in flight (`threads + 2`,
/// up to 18), the frames the frame server asks for ahead of the playhead (14) and a few behind
/// it. [`FRAME_CACHE_CEILING`] alone holds 11 pictures of 4K 4:2:2 10-bit (33 MB each): the
/// playback decoding run ([`GopCache::pump`]) stopped as "full" while its pipeline was still
/// filling, the decoder drained between runs, and frames were evicted before the playhead
/// reached them, so their requests were dropped (FX3 XAVC S 4K 50p: 194 of 400 frames shown,
/// 373 decoded, 347 evicted, 70 dropped).
const PLAY_FRAMES: usize = 40;
/// ... and never more than this, so a source of huge frames (8K) keeps a bounded cache.
const PLAY_CACHE_CEILING: usize = 1536 << 20;
/// A wanted sample at most this many samples past what a running decoder has been fed is
/// decoded even for a request that was cancelled meanwhile: playback that has fallen a little
/// behind a sequential (long-GOP) decoder still gets these frames, late, instead of nothing,
/// and the pictures the decoder has in flight are delivered rather than thrown away. Further
/// ahead, a seek, or a picture of its own (intra) is only worth decoding while someone still
/// wants the frame.
const NEAR_SAMPLES: usize = 8;
/// Playback decodes ahead of the latest playing request by up to this many samples
/// ([`GopCache::pump`]): a frame-threaded decoder outputs a picture only `threads + 2` samples
/// after it was fed, and the frames after the request are the next ones wanted.
const PUMP_AHEAD: usize = 24;
/// Playback decoding stops this long after the last playing request (playback stopped or left
/// for another clip).
const PUMP_IDLE: Duration = Duration::from_millis(300);
/// Playback decoding keeps its output this many samples past the playhead: while it is behind
/// that, the non-reference samples on the way are left out (those frames are dropped) so the
/// decoder catches up instead of delivering every frame late. A frame-threaded decoder
/// outputs a picture only `threads + 2` samples after it was fed, which is counted in.
const PUMP_LEAD: usize = 2;
/// A playing request for a sample the running decoder passed at most this many samples ago
/// (skipped as late, or evicted) is dropped: seeking back would restart the decoder and refill
/// its pipeline, during which nothing is delivered, so playback would stall again at once.
/// Further back the playhead jumped (a loop): the request seeks.
const DROP_BEHIND: usize = 64;

/// A running decoder up to this many samples before the wanted sample's sync sample keeps going
/// rather than restarting at the sync sample.
const CONTINUE_THROUGH: usize = 48;

/// Threads that run the caches' decoding (see [`isolated`]); `None` where none can be made.
#[cfg(not(target_arch = "wasm32"))]
fn decode_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map(std::num::NonZeroUsize::get).unwrap_or(1);
        rayon::ThreadPoolBuilder::new().num_threads(n).thread_name(|i| format!("decode-{i}")).build().ok()
    })
    .as_ref()
}

/// Run a cache's decoding, its lock held by the caller, on [`decode_pool`], the caller waiting
/// without doing other rayon work meanwhile (#480).
///
/// That decoding starts rayon work (a decoder's slices, cropping and rotating the frames). On the
/// pool of the render jobs that ask the cache for frames, a worker that took part of it could,
/// waiting inside it, pick up another of those jobs and block on the lock: the lock holder then
/// waits for its work on the blocked worker's stack, and that worker for the lock, for good (an
/// export of rotated iPhone clips stopped making frames). On the decode pool that work only ever
/// runs beside other caches' decoding, which never waits for a cache's lock. A caller that is a
/// rayon worker would also run other jobs of its pool while it waits for the decode pool, some
/// of them asking for a cache's lock while it holds this one: it waits on a plain thread instead.
fn isolated<T: Send>(f: impl FnOnce() -> crate::Result<T> + Send) -> crate::Result<T> {
    let ctx = filmcraft_media::cancel::Context::current();
    let job = move || ctx.run(f);
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(pool) = decode_pool() {
        if rayon::current_thread_index().is_none() {
            return pool.install(job);
        }
        let slot = Mutex::new(Some(job));
        let waited = std::thread::scope(|scope| {
            let spawned = std::thread::Builder::new().name("decode-wait".into()).spawn_scoped(scope, || {
                let job = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
                job.map(|job| pool.install(job))
            });
            spawned.map(std::thread::ScopedJoinHandle::join)
        });
        return match waited {
            Ok(Ok(Some(r))) => r,
            Ok(Err(_)) => Err(CodecError::Decode("internal error while decoding".into())),
            // no thread to wait on: decode here, as before
            Ok(Ok(None)) | Err(_) => match slot.into_inner().unwrap_or_else(PoisonError::into_inner) {
                Some(job) => job(),
                None => Err(CodecError::Decode("internal error while decoding".into())),
            },
        };
    }
    job()
}

/// Most sync candidates, and bytes read from them, one search for a random-access sample may try.
// 65 536 probes is 18 minutes of an unkeyed 60 fps stream: far past any real GOP, yet each hostile probe stays bounded by the byte budget
const MAX_RANDOM_ACCESS_PROBES: usize = 65_536;
const MAX_RANDOM_ACCESS_PROBE_BYTES: usize = 512 << 20;

/// The container's sync flags may be wrong for the codec (an MP4 without `stss` marks every
/// sample): step back from sync sample `key` to one the decoder can start from (`is_ra` says, with
/// None meaning "cannot tell", which is taken as yes). A file whose index aliases many claimed
/// keyframes onto one large non-random-access sample would otherwise read it once per claim, so
/// the search is bounded in probes and bytes, and stops when the request is cancelled.
fn random_access_at_or_before(s: &dyn VideoSamples, mut key: usize, mut is_ra: impl FnMut(&[u8]) -> Option<bool>) -> crate::Result<usize> {
    let (mut probes, mut bytes) = (0usize, 0usize);
    // polled before the sample-zero exit too: a cancelled request must not go on to decode from there
    if filmcraft_media::cancel::cancelled() {
        return Err(CodecError::Cancelled);
    }
    while key > 0 {
        if filmcraft_media::cancel::cancelled() {
            return Err(CodecError::Cancelled);
        }
        // refused before the read: a claimed keyframe that is one huge sample must not be loaded
        if s.sample_size(key).is_some_and(|n| n > (MAX_RANDOM_ACCESS_PROBE_BYTES - bytes) as u64) {
            return Err(CodecError::Decode("no random-access picture found before the requested frame".into()));
        }
        let data = s.read(key)?;
        // a container that cannot say a sample's size (see `sample_size`) is checked after the read:
        // the initial read of an unknown size is allowed, but one sample past the budget ends the search
        if data.len() > MAX_RANDOM_ACCESS_PROBE_BYTES - bytes {
            return Err(CodecError::Decode("no random-access picture found before the requested frame".into()));
        }
        if is_ra(&data) != Some(false) {
            break;
        }
        probes += 1;
        bytes = bytes.saturating_add(data.len());
        if probes >= MAX_RANDOM_ACCESS_PROBES || bytes > MAX_RANDOM_ACCESS_PROBE_BYTES {
            return Err(CodecError::Decode("no random-access picture found before the requested frame".into()));
        }
        key = s.sync_before(key - 1);
    }
    Ok(key)
}

impl GopCache {
    pub fn new(explicit_color: Option<ColorInfo>) -> Self {
        // unit tests count decoder restarts: each of their caches gets a pool of its own
        let pool = if cfg!(test) { Arc::new(Pool::new(MAX_LIVE_DECODERS, IDLE, FRAME_BUDGET).tiered(RECENT, HARD_DECODERS)) } else { Pool::global() };
        Self::in_pool(explicit_color, pool)
    }

    /// An empty cache whose decoders and frame memory count against `pool`.
    fn in_pool(explicit_color: Option<ColorInfo>, pool: Arc<Pool>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                decoder: None,
                intra: false,
                spare: Vec::new(),
                next: usize::MAX,
                start: 0,
                out_max: i64::MIN,
                pump: false,
                pump_stop: false,
                play: None,
                waiting: BTreeMap::new(),
                frames: BTreeMap::new(),
                drafts: Default::default(),
                raw: Default::default(),
                bytes: 0,
                last_used: Instant::now(),
                pin: i64::MIN,
                background: false,
            }),
            cv: std::sync::Condvar::new(),
            pool: pool.clone(),
        });
        pool.register(&shared);
        Self { shared, explicit_color, rotation: 0, crop: None, budget: FRAME_CACHE_CEILING }
    }

    /// Turn every decoded frame clockwise by `quarter_turns` × 90° (the container's display
    /// rotation, e.g. portrait phone video stored landscape).
    pub fn with_rotation(mut self, quarter_turns: u8) -> Self {
        self.rotation = quarter_turns % 4;
        self
    }

    /// Cut every decoded frame to `region` (the container's clean aperture, in stored picture
    /// coordinates) before the rotation, as cropping and the display matrix compose.
    pub fn with_crop(mut self, region: Option<Region>) -> Self {
        self.crop = region;
        self
    }

    /// Colour signalled by the container wins over the bitstream's (YUV frames only); the
    /// container's clean aperture and then its display rotation are applied.
    fn finish(&self, f: VideoFrame) -> VideoFrame {
        let mut f = self.colored(f);
        if let Some(r) = self.crop {
            f = f.cropped(r);
        }
        if self.rotation != 0 { f.rotated(self.rotation) } else { f }
    }

    /// `f` with the container's colour, which costs nothing (raw frames carry it too).
    fn colored(&self, mut f: VideoFrame) -> VideoFrame {
        if let Some(c) = self.explicit_color
            && !matches!(f.data, filmcraft_frame::PixelData::Rgba8(_) | filmcraft_frame::PixelData::RgbaF32(_))
        {
            f.color = c;
        }
        f
    }

    /// Cache decoder output. Cropping and rotating a 4K frame takes longer than decoding it: with
    /// the lock held, every frame worker and the decoder itself would wait for it, and a rotated
    /// phone clip played at 2 fps (#3467). The frame is cached raw instead and [`Self::finished`]
    /// does that work off the lock, for the frames requests actually take.
    /// Returns the stored picture as a request would take it, which the caller keeps even when
    /// the cache evicts it again to meet the budget.
    fn store(&self, st: &mut State, pts: i64, f: VideoFrame, draft: bool) -> Taken {
        if self.crop.is_none() && self.rotation == 0 {
            let frame = self.store_finished(st, pts, self.finish(f), draft);
            return Taken { pts, frame, raw: false };
        }
        let frame = self.store_finished(st, pts, self.colored(f), draft);
        // an evicted frame leaves no entry to mark
        if st.frames.contains_key(&pts) {
            st.raw.insert(pts);
        }
        Taken { pts, frame, raw: true }
    }

    /// `t`'s frame with [`Self::finish`] applied, which must run without the cache's lock. The
    /// finished frame replaces the raw one in the cache unless that was evicted or replaced meanwhile.
    fn finished(&self, t: Taken) -> Arc<VideoFrame> {
        if !t.raw {
            return t.frame;
        }
        let done = Arc::new(self.finish(VideoFrame::clone(&t.frame)));
        let mut st = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.frames.get(&t.pts).is_some_and(|f| Arc::ptr_eq(f, &t.frame)) {
            let draft = st.drafts.contains(&t.pts);
            self.store_finished(&mut st, t.pts, done.clone(), draft);
        }
        drop(st);
        // the raw planes serve the next decoded pictures once nothing else holds them
        filmcraft_frame::pool::recycle(t.frame);
        done
    }

    /// The small-frame reserve may use the configured cache budget, but may never enlarge it;
    /// `playing` adds the playback window of [`PLAY_FRAMES`].
    fn cache_budget(&self, frame_bytes: usize, playing: bool) -> usize {
        let reserve = MIN_FRAMES.saturating_mul(frame_bytes).min(FRAME_CACHE_CEILING);
        let play = if playing { PLAY_FRAMES.saturating_mul(frame_bytes).min(PLAY_CACHE_CEILING) } else { 0 };
        self.budget.min(FRAME_CACHE_CEILING).max(reserve).max(play)
    }

    /// Cache a frame [`Self::finish`] has been applied to ([`Self::store`] marks a raw one after).
    /// Returns the stored picture, which the caller keeps even when the cache evicts it again.
    fn store_finished(&self, st: &mut State, pts: i64, f: impl Into<Arc<VideoFrame>>, draft: bool) -> Arc<VideoFrame> {
        let f = f.into();
        st.last_used = Instant::now();
        if draft {
            st.drafts.insert(pts);
            DRAFT.fetch_add(1, Ordering::Relaxed);
        } else {
            st.drafts.remove(&pts);
        }
        let budget = self.cache_budget(f.byte_size(), st.play.is_some());
        let before = st.bytes;
        st.bytes += f.byte_size();
        st.raw.remove(&pts);
        if let Some(old) = st.frames.insert(pts, f.clone()) {
            st.bytes -= old.byte_size();
            filmcraft_frame::pool::recycle(old);
        }
        // Evict frames far from the request (pin), not the newest decode. A frame-threaded
        // decoder can emit a burst past the playhead; pivoting on those pts used to drop the
        // wanted picture and walk the GOP again.
        // playing: the frames behind the playhead were shown (or dropped); those ahead are wanted
        let playhead = st.play.map(|p| p.late).filter(|l| *l != i64::MIN);
        let pivot = playhead.unwrap_or(if st.pin != i64::MIN { st.pin } else { pts });
        while st.bytes > budget && st.frames.len() > 1 {
            let (Some(&first), Some(&last)) = (st.frames.keys().next(), st.frames.keys().next_back()) else { break };
            let keep = |p: i64| p == pivot || p == pts || st.waiting.contains_key(&p);
            let mut victim = if (playhead.is_some() && first < pivot) || pivot - first > last - pivot { first } else { last };
            if keep(victim) {
                victim = if victim == first { last } else { first };
                if keep(victim) {
                    // both ends are kept (the request, the new decode, a frame a request waits
                    // for): an interior frame goes, or a reordered batch would stay cached far
                    // above the budget. Of the interior frames nothing keeps, the one farthest
                    // from `pivot` on the side eviction prefers.
                    let low_first = (playhead.is_some() && first < pivot) || pivot - first > last - pivot;
                    let mut interior =
                        st.frames.range((std::ops::Bound::Excluded(first), std::ops::Bound::Excluded(last))).map(|(&k, _)| k).filter(|&k| !keep(k));
                    let k = if low_first { interior.next() } else { interior.next_back() };
                    if let Some(k) = k {
                        victim = k;
                    } else if pts != pivot && !st.waiting.contains_key(&pts) {
                        // only kept frames remain: the new decode goes, the request stays
                        victim = pts;
                    } else {
                        break;
                    }
                }
            }
            if let Some(v) = st.frames.remove(&victim) {
                st.drafts.remove(&victim);
                st.raw.remove(&victim);
                st.bytes -= v.byte_size();
                EVICTED.fetch_add(1, Ordering::Relaxed);
                // its planes serve the next decoded pictures instead of going back to the allocator
                filmcraft_frame::pool::recycle(v);
            }
        }
        let pool = &self.shared.pool;
        if st.bytes >= before {
            pool.bytes.fetch_add(st.bytes - before, Ordering::Relaxed);
        } else {
            pool.bytes.fetch_sub(before - st.bytes, Ordering::Relaxed);
        }
        pool.trim_frames(&self.shared);
        f
    }

    /// Store decoder output (in presentation order) and advance `out_max`. Returns the stored
    /// frame nearest at or before `want_pts`, which the caller keeps even when the cache evicts it.
    fn store_output(&self, st: &mut State, out: Vec<crate::video::DecodedFrame>, want_pts: i64) -> Option<Taken> {
        count_frames(&out);
        let mut nearest: Option<Taken> = None;
        for d in out {
            st.out_max = st.out_max.max(d.pts);
            let pts = d.pts;
            let t = self.store(st, pts, d.frame, d.draft);
            if pts <= want_pts && nearest.as_ref().is_none_or(|n| pts > n.pts) {
                nearest = Some(t);
            }
        }
        nearest
    }

    /// The frame presented at `target` (track units, clamped to the stream).
    ///
    /// Decoders may run slices on rayon, and a rayon thread waiting inside a decode can pick up
    /// another render job that asks this same source for a frame — on the thread that holds this
    /// cache's lock. Such a nested request must not lock again (deadlock): it decodes the frame
    /// with a private decoder instead ([`Self::private_frame`]).
    pub fn frame(&self, s: &dyn VideoSamples, target: i64) -> crate::Result<Arc<VideoFrame>> {
        self.frame_late(s, target, None)
    }

    /// [`Self::frame`] while catching up: frames shown before `late_before` (track units) are
    /// late. Non-reference samples of late frames are left out on the way to the wanted frame
    /// (no other picture depends on them, so the wanted frame decodes exactly as it would
    /// otherwise; a later request for a skipped frame re-seeks).
    ///
    /// Inside [`filmcraft_media::cancel::with_draft`]`(true, …)` the decoder runs in draft mode
    /// ([`VideoDecoder::set_draft`]); its draft frames are cached for draft requests only, and an
    /// exact request for one re-decodes it.
    pub fn frame_late(&self, s: &dyn VideoSamples, target: i64, late_before: Option<i64>) -> crate::Result<Arc<VideoFrame>> {
        let r = self.frame_late_locked(s, target, late_before).map(|t| self.finished(t));
        self.after_request();
        r
    }

    /// Run once a request is answered, outside the cache's lock (see `release_background`). A
    /// thumbnail leaves the decoders of earlier thumbnails no reason to stay: each costs hundreds
    /// of MB at 4K.
    fn after_request(&self) {
        if filmcraft_media::cancel::background() {
            drop(self.shared.pool.release_background(&self.shared));
        }
    }

    /// [`Self::frame_late`] leaving the display rotation to the caller: the frame comes with the
    /// clockwise quarter turns still to apply (0 when it is already upright). The GPU compositor
    /// folds them into the layer's placement for free, where turning a 4K frame on the CPU at
    /// every frame of playback is too slow (#3467).
    pub fn frame_late_unturned(&self, s: &dyn VideoSamples, target: i64, late_before: Option<i64>) -> crate::Result<(Arc<VideoFrame>, u8)> {
        let r = self.frame_late_locked(s, target, late_before).map(|t| {
            // shortcut: a clean aperture is still cut and turned on the CPU; rare outside camera files
            if !t.raw || self.crop.is_some() { (self.finished(t), 0) } else { (t.frame, self.rotation) }
        });
        self.after_request();
        r
    }

    /// The cached or decoded frame for [`Self::frame_late`] and [`Self::frame_late_unturned`], raw
    /// when it still waits for [`Self::finish`]. The cache's lock is released when this returns.
    fn frame_late_locked(&self, s: &dyn VideoSamples, target: i64, late_before: Option<i64>) -> crate::Result<Taken> {
        let n = s.count();
        let i = s.sample_at(target.max(0)).or_else(|| (n > 0).then(|| n - 1)).ok_or_else(|| CodecError::Decode("empty track".into()))?;
        let want_pts = s.pts(i);
        let me = self as *const Self as usize;
        if DecodingGuard::active(me) {
            return Ok(Taken { pts: want_pts, frame: self.private_frame(s, i, want_pts, n)?, raw: false });
        }
        let draft = filmcraft_media::cancel::draft();
        // Idle caches' decoders released below. Declared before the lock guard, so it is dropped
        // after the guard on every return path: dropping a decoder can block (a hardware session
        // waits for its frames in flight) and must not hold this cache's lock.
        let mut released: Vec<Box<dyn VideoDecoder>> = Vec::new();
        let mut st = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _decoding = DecodingGuard::enter(me);
        st.last_used = Instant::now();
        st.pin = want_pts;
        let background = filmcraft_media::cancel::background();
        if let Some(frame) = st.cached(want_pts, draft).cloned() {
            HITS.fetch_add(1, Ordering::Relaxed);
            // a monitor asking for a frame of a thumbnail's clip makes its decoder one in use
            st.background &= background;
            return Ok(Taken { pts: want_pts, frame, raw: st.raw.contains(&want_pts) });
        }
        MISSES.fetch_add(1, Ordering::Relaxed);
        if !background {
            st.background = false;
        } else if st.decoder.is_none() {
            st.background = true;
        }
        if !background && st.decoder.as_ref().is_some_and(|d| d.thread_limited()) {
            // made for a thumbnail (few threads): a monitor gets a full one
            released.extend(st.release_decoder(&self.shared.pool));
        }
        let playing = filmcraft_media::cancel::playing() && !st.intra;
        if playing {
            // no catch-up hint (the preroll, before the clock runs): nothing is late
            st.play = Some(Play { i, late: late_before.unwrap_or(i64::MIN), at: Instant::now() });
        } else {
            // a request with time to spare (paused, scrubbing, an export) stops playback decoding
            st.play = None;
            st.pump_stop |= st.pump;
        }
        if st.pump {
            st = match self.await_pump(st, s, i, want_pts, draft, playing)? {
                Awaited::Frame(t) => return Ok(t),
                Awaited::Go(st) => st,
            };
        }
        if st.decoder.is_none() {
            let d = s.make_decoder()?;
            st.intra = d.intra_only();
            st.decoder = Some(d);
            st.next = usize::MAX;
            self.shared.pool.decoders.fetch_add(1, Ordering::Relaxed);
        }
        released.extend(self.shared.pool.trim_decoders(&self.shared));
        if playing && Self::passed(&st, i, want_pts) {
            return Self::dropped();
        }
        // Nothing cached: the work ahead (possibly a seek and a GOP of decoding) is only worth it
        // while someone still wants the frame, unless it is a few samples of a running decoder
        // (`NEAR_SAMPLES`).
        let near = !st.intra && st.next != usize::MAX && st.start <= i && i < st.next.saturating_add(NEAR_SAMPLES);
        if !near && filmcraft_media::cancel::cancelled() {
            return Err(CodecError::Cancelled);
        }
        if st.intra {
            return self.intra_frame(st, s, i, want_pts);
        }
        if playing && !st.pump_stop {
            Self::position(&mut st, s, i, want_pts)?;
            st.pump = true;
            drop(st);
            // off the lock: see `pump`
            return isolated(|| self.pump(s, i, want_pts, draft));
        }
        // with the lock held: see `isolated`
        let st = &mut *st;
        isolated(|| self.decode_until(st, s, i, want_pts, late_before, draft))
    }

    /// Whether the running decoder passed sample `i` recently without leaving its frame (skipped
    /// as late, or evicted): see [`DROP_BEHIND`]. A sample fed but still in the decoder's
    /// pipeline (its pts past the pictures output so far) is on its way, not passed.
    fn passed(st: &State, i: usize, want_pts: i64) -> bool {
        st.next != usize::MAX && st.start <= i && i < st.next && want_pts <= st.out_max && st.next - i <= DROP_BEHIND
    }

    /// A playing request's frame is dropped: the request gives up (its result is not cached)
    /// and playback shows the latest frame it has.
    fn dropped() -> crate::Result<Taken> {
        filmcraft_media::cancel::give_up();
        DROPPED.fetch_add(1, Ordering::Relaxed);
        Err(CodecError::Cancelled)
    }

    /// Another thread decodes ahead for playback ([`Self::pump`]): a playing request gets its
    /// frame when produced, is dropped when the decoder passed it, and otherwise returns at once
    /// as pending (`filmcraft_media::pending`: the frame server asks again next refresh), so
    /// it does not hold a worker while the picture is in the decoder's pipeline. Any other
    /// request waits for playback decoding to stop (it has, by setting `play` to None).
    fn await_pump<'a>(
        &self,
        st: MutexGuard<'a, State>,
        s: &dyn VideoSamples,
        i: usize,
        want_pts: i64,
        draft: bool,
        playing: bool,
    ) -> crate::Result<Awaited<'a>> {
        let mut st = st;
        *st.waiting.entry(want_pts).or_default() += 1;
        let r = loop {
            // the frame first: the pump may have stored it right before it stopped
            if let Some(frame) = st.cached(want_pts, draft).cloned() {
                break Ok(Some(Taken { pts: want_pts, frame, raw: st.raw.contains(&want_pts) }));
            }
            if !st.pump {
                break Ok(None);
            }
            if playing {
                if Self::passed(&st, i, want_pts) {
                    break Self::dropped().map(Some);
                }
                // the decoder will produce it (the pump follows this request): asked again later
                let coming = st.start <= i && decode_start(s, i) <= st.next.saturating_add(CONTINUE_THROUGH);
                if coming {
                    filmcraft_media::pending::mark();
                    break Err(CodecError::Cancelled);
                }
                // out of the run's reach: the pump stops for it (it is the latest request)
            }
            let (guard, _) = self.shared.cv.wait_timeout(st, Duration::from_millis(20)).unwrap_or_else(PoisonError::into_inner);
            st = guard;
        };
        if let Some(n) = st.waiting.get_mut(&want_pts) {
            *n -= 1;
            if *n == 0 {
                st.waiting.remove(&want_pts);
            }
        }
        match r? {
            Some(t) => Ok(Awaited::Frame(t)),
            None => Ok(Awaited::Go(st)),
        }
    }

    /// Decode ahead for playback: feed the decoder sample by sample with the lock released while
    /// a sample decodes (cache hits and the requests waiting for the pictures get through),
    /// skipping the non-reference samples of frames the latest playing request says are late,
    /// until that request stops coming ([`PUMP_IDLE`]), asks for a frame outside this run, or
    /// is [`PUMP_AHEAD`] samples behind. Positioned by [`Self::position`]; `State::pump` is set.
    /// Returns the wanted frame, dropped if the run passed it meanwhile.
    fn pump(&self, s: &dyn VideoSamples, i: usize, want_pts: i64, draft: bool) -> crate::Result<Taken> {
        let n = s.count();
        let mut st = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        // the wanted frame, held once produced: the cache may evict it before the run ends
        let mut own: Option<Taken> = None;
        let result = loop {
            let Some(play) = st.play else { break Ok(()) };
            if own.is_none() {
                own = st.cached(want_pts, draft).cloned().map(|frame| Taken { pts: want_pts, frame, raw: st.raw.contains(&want_pts) });
            }
            let done = own.is_some() || (i < st.next && want_pts <= st.out_max);
            let reach = play.i >= st.start && decode_start(s, play.i) <= st.next.saturating_add(CONTINUE_THROUGH);
            // enough frames past the request for the cache to hold (more would evict them)
            let full = {
                let frame_bytes = st.frames.values().next().map_or(0, |f| f.byte_size()).max(1);
                let from = if play.late != i64::MIN { play.late } else { s.pts(play.i) };
                let ahead = st.frames.range(from..).count();
                ahead.saturating_add(1).saturating_mul(frame_bytes) >= self.cache_budget(frame_bytes, true)
            };
            let over = done && (play.at.elapsed() > PUMP_IDLE || st.next > play.i.saturating_add(PUMP_AHEAD) || full);
            if st.pump_stop || st.next >= n || !reach || over || (!done && st.next > i.saturating_add(64)) {
                break Ok(());
            }
            let k = st.next;
            let data = match s.read(k) {
                Ok(d) => d,
                Err(e) => break Err(e),
            };
            // Behind the playhead (`play.late`), or so close that the pictures still in the
            // decoder's pipeline would come out behind it: leave out the non-reference samples.
            let behind = play.late != i64::MIN
                && s.sample_at(play.late).is_some_and(|late_i| {
                    let in_flight = st.next.saturating_sub(s.sample_at(st.out_max).map_or(st.next, |o| o + 1));
                    k < late_i.saturating_add(in_flight).saturating_add(PUMP_LEAD)
                });
            if k != i && k != play.i && behind && st.decoder.as_ref().is_some_and(|d| d.is_disposable(&data)) {
                st.next += 1;
                SKIPPED.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let Some(mut d) = st.decoder.take() else { break Err(CodecError::Decode("no video decoder".into())) };
            d.set_draft(draft);
            drop(st);
            let out = timed(|| d.decode(&data, s.pts(k)));
            st = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.decoder = Some(d);
            match out {
                Ok(out) => {
                    st.next += 1;
                    DECODED.fetch_add(1, Ordering::Relaxed);
                    // (the wanted frame is taken from the cache below, exact or not at all)
                    let _ = self.store_output(&mut st, out, want_pts);
                    self.shared.cv.notify_all();
                }
                Err(e) => break Err(e),
            }
        };
        if result.is_ok() && own.is_none() && st.cached(want_pts, draft).is_none() && st.next >= n {
            let out = st.decoder.as_mut().map(|d| timed(|| d.flush())).unwrap_or_default();
            let _ = self.store_output(&mut st, out, want_pts);
            st.next = usize::MAX;
        }
        st.pump = false;
        st.pump_stop = false;
        self.shared.cv.notify_all();
        result?;
        if let Some(t) = own {
            return Ok(t);
        }
        if let Some(frame) = st.cached(want_pts, draft).cloned() {
            return Ok(Taken { pts: want_pts, frame, raw: st.raw.contains(&want_pts) });
        }
        // not produced (a request with time to spare interrupted the run, or the stream ended):
        // dropped; never a neighbouring frame, which the frame cache would keep as this one
        Self::dropped()
    }

    /// Decode (the lock held) until the frame with `want_pts` (sample `i`) is cached; return it.
    /// Position the decoder for sample `i`: continue the running decoder when it has passed the
    /// wanted sample's sync sample and either has not reached the sample yet or has been fed it
    /// without outputting it yet (a frame-threaded decoder holds many pictures in flight).
    /// Otherwise the frame was evicted or lies in another GOP: restart at the sync sample. A run
    /// that started after `key` (at the random-access picture of a wanted leading picture) left
    /// the picture out.
    fn position(st: &mut State, s: &dyn VideoSamples, i: usize, want_pts: i64) -> crate::Result<()> {
        let n = s.count();
        let mut key = decode_start(s, i);
        let running = st.next != usize::MAX && st.next <= n && st.start <= key;
        // Decoding on through a short stretch into the next GOP is cheaper than a restart, and
        // playback wants those frames anyway.
        let near = st.next <= key && key - st.next <= CONTINUE_THROUGH;
        let continuing = running && (st.next > key || near) && (i >= st.next || (st.start <= i && want_pts > st.out_max));
        if !continuing {
            SEEKS.fetch_add(1, Ordering::Relaxed);
            key = random_access_at_or_before(s, key, |data| st.decoder.as_ref().and_then(|d| d.is_random_access(data)))?;
            if let Some(d) = st.decoder.as_mut() {
                d.reset();
            }
            st.next = key;
            st.start = key;
            st.out_max = i64::MIN;
        }
        Ok(())
    }

    fn decode_until(&self, st: &mut State, s: &dyn VideoSamples, i: usize, want_pts: i64, late_before: Option<i64>, draft: bool) -> crate::Result<Taken> {
        let n = s.count();
        Self::position(st, s, i, want_pts)?;
        let limit = (i.max(st.next) + 64).min(n);
        let late = late_before.unwrap_or(i64::MIN).min(want_pts);
        let Some(decoder) = st.decoder.as_mut() else {
            return Err(CodecError::Decode("no video decoder".into()));
        };
        decoder.set_draft(draft);
        // what this decode produced, kept apart from the cache: eviction may drop the only eligible fallback
        let mut mine: Option<Taken> = None;
        let keep = |mine: &mut Option<Taken>, t: Option<Taken>| {
            if let Some(t) = t
                && mine.as_ref().is_none_or(|m| t.pts > m.pts)
            {
                *mine = Some(t);
            }
        };
        while st.next < limit {
            // Giving up (not within `NEAR_SAMPLES` of the wanted sample) leaves the decoder state
            // consistent (`next`, `out_max`): a later request continues.
            if i.saturating_sub(st.next) > NEAR_SAMPLES && filmcraft_media::cancel::cancelled() {
                return Err(CodecError::Cancelled);
            }
            let k = st.next;
            let data = s.read(k)?;
            if k != i && s.pts(k) < late && st.decoder.as_ref().is_some_and(|d| d.is_disposable(&data)) {
                st.next += 1;
                SKIPPED.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let Some(decoder) = st.decoder.as_mut() else {
                return Err(CodecError::Decode("no video decoder".into()));
            };
            let out = timed(|| decoder.decode(&data, s.pts(k)))?;
            st.next += 1;
            DECODED.fetch_add(1, Ordering::Relaxed);
            keep(&mut mine, self.store_output(st, out, want_pts));
            if st.cached(want_pts, draft).is_some() {
                break;
            }
        }
        if st.cached(want_pts, draft).is_none() {
            let out = st.decoder.as_mut().map(|d| timed(|| d.flush())).unwrap_or_default();
            keep(&mut mine, self.store_output(st, out, want_pts));
            st.next = usize::MAX;
        }
        // nearest decoded frame at or before the wanted pts (robust to decoder pts quirks); a closer
        // frame of this decode beats an older cached one, and covers the cache having evicted it
        match (st.at_or_before(want_pts, draft), mine) {
            (Some(c), Some(m)) if m.pts > c.pts => Ok(m),
            (Some(c), _) => Ok(c),
            (None, Some(m)) => Ok(m),
            (None, None) => Err(CodecError::Decode("frame not produced".into())),
        }
    }

    /// A nested request (see [`Self::frame`]): decode from the sync sample with a fresh decoder,
    /// without touching the shared state.
    fn private_frame(&self, s: &dyn VideoSamples, i: usize, want_pts: i64, n: usize) -> crate::Result<Arc<VideoFrame>> {
        let mut d = s.make_decoder()?;
        let mut next = usize::MAX;
        let out = Self::decode_to(s, d.as_mut(), &mut next, i, want_pts, n)?;
        let f = out
            .into_iter()
            .filter(|p| p.pts <= want_pts)
            .max_by_key(|p| p.pts)
            .map(|p| p.frame)
            .ok_or_else(|| CodecError::Decode("frame not produced".into()))?;
        Ok(Arc::new(self.finish(f)))
    }

    /// Decode with `d` (positioned at `next`) until the picture with `want_pts` (sample `i`) comes out.
    fn decode_to(
        s: &dyn VideoSamples,
        d: &mut dyn VideoDecoder,
        next: &mut usize,
        i: usize,
        want_pts: i64,
        n: usize,
    ) -> crate::Result<Vec<crate::video::DecodedFrame>> {
        let mut key = decode_start(s, i);
        // Continue the running decoder when the wanted sample is ahead within this GOP run.
        let continuing = *next != usize::MAX && *next > key && *next <= i + 16 && *next <= n;
        if !continuing {
            key = random_access_at_or_before(s, key, |data| d.is_random_access(data))?;
            d.reset();
            *next = key;
        }
        let limit = (i + 64).min(n);
        let mut out = Vec::new();
        let mut found = false;
        while *next < limit {
            let k = *next;
            let data = s.read(k)?;
            let pics = timed(|| d.decode(&data, s.pts(k)))?;
            count_frames(&pics);
            *next += 1;
            found |= pics.iter().any(|p| p.pts == want_pts);
            out.extend(pics);
            if found {
                break;
            }
        }
        if !found {
            let pics = timed(|| d.flush());
            count_frames(&pics);
            out.extend(pics);
            *next = usize::MAX;
        }
        Ok(out)
    }

    /// Intra-only streams: decode the one sample outside the lock, so several frame workers
    /// decode different frames of the same source at once.
    fn intra_frame(&self, st: std::sync::MutexGuard<'_, State>, s: &dyn VideoSamples, i: usize, want_pts: i64) -> crate::Result<Taken> {
        let mut st = st;
        let spare = st.spare.pop();
        drop(st);
        let mut dec = match spare {
            Some(d) => d,
            None => s.make_decoder()?,
        };
        let res = s.read(i).and_then(|data| {
            DECODED.fetch_add(1, Ordering::Relaxed);
            timed(|| dec.decode(&data, want_pts))
        });
        let res = res.inspect(|out| count_frames(out));
        let mut st = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.spare.len() < 16 {
            st.spare.push(dec);
        }
        // cached raw: the request that takes a frame crops and turns it off the lock, or leaves
        // the turn to the GPU (`frame_late_unturned`)
        // what this decode produced, kept apart from the cache: a concurrent request can move the pin and
        // have the cache evict this frame before it is read back
        let mut mine: Option<Taken> = None;
        for d in res? {
            let t = self.store(&mut st, d.pts, d.frame, d.draft);
            if d.pts <= want_pts && mine.as_ref().is_none_or(|m| d.pts > m.pts) {
                mine = Some(t);
            }
        }
        match (st.at_or_before(want_pts, true), mine) {
            // a closer frame of this decode beats an older cached one
            (Some(c), Some(m)) if m.pts > c.pts => Ok(m),
            (Some(c), _) => Ok(c),
            (None, Some(m)) => Ok(m),
            (None, None) => Err(CodecError::Decode("frame not produced".into())),
        }
    }
}

#[cfg(test)]
mod tests;
