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
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>>;
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
    /// Decoded frames by presentation pts (bounded).
    frames: BTreeMap<i64, Arc<VideoFrame>>,
    /// The cached frames that were decoded in draft mode: served to draft requests only.
    drafts: std::collections::BTreeSet<i64>,
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
    fn at_or_before(&self, pts: i64, draft_ok: bool) -> Option<Arc<VideoFrame>> {
        self.cached(pts, draft_ok).or_else(|| self.frames.range(..=pts).rev().find(|(p, _)| draft_ok || !self.drafts.contains(p)).map(|(_, f)| f)).cloned()
    }
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

impl GopCache {
    pub fn new(explicit_color: Option<ColorInfo>) -> Self {
        // unit tests count decoder restarts: each of their caches gets a pool of its own
        let pool = if cfg!(test) { Arc::new(Pool::new(MAX_LIVE_DECODERS, IDLE, FRAME_BUDGET).tiered(RECENT, HARD_DECODERS)) } else { Pool::global() };
        Self::in_pool(explicit_color, pool)
    }

    fn in_pool(explicit_color: Option<ColorInfo>, pool: Arc<Pool>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                decoder: None,
                intra: false,
                spare: Vec::new(),
                next: usize::MAX,
                start: 0,
                out_max: i64::MIN,
                frames: BTreeMap::new(),
                drafts: Default::default(),
                bytes: 0,
                last_used: Instant::now(),
                pin: i64::MIN,
                background: false,
            }),
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
    fn finish(&self, mut f: VideoFrame) -> VideoFrame {
        if let Some(c) = self.explicit_color
            && !matches!(f.data, filmcraft_frame::PixelData::Rgba8(_) | filmcraft_frame::PixelData::RgbaF32(_))
        {
            f.color = c;
        }
        if let Some(r) = self.crop {
            f = f.cropped(r);
        }
        if self.rotation != 0 { f.rotated(self.rotation) } else { f }
    }

    fn store(&self, st: &mut State, pts: i64, f: VideoFrame, draft: bool) {
        self.store_finished(st, pts, self.finish(f), draft);
    }

    /// The small-frame reserve may use the configured cache budget, but may never enlarge it.
    fn cache_budget(&self, frame_bytes: usize) -> usize {
        let reserve = MIN_FRAMES.saturating_mul(frame_bytes).min(FRAME_CACHE_CEILING);
        self.budget.min(FRAME_CACHE_CEILING).max(reserve)
    }

    /// Cache a frame [`Self::finish`] has been applied to.
    fn store_finished(&self, st: &mut State, pts: i64, f: VideoFrame, draft: bool) {
        st.last_used = Instant::now();
        if draft {
            st.drafts.insert(pts);
            DRAFT.fetch_add(1, Ordering::Relaxed);
        } else {
            st.drafts.remove(&pts);
        }
        let budget = self.cache_budget(f.byte_size());
        let before = st.bytes;
        st.bytes += f.byte_size();
        if let Some(old) = st.frames.insert(pts, Arc::new(f)) {
            st.bytes -= old.byte_size();
            filmcraft_frame::pool::recycle(old);
        }
        // Evict frames far from the request (pin), not the newest decode. A frame-threaded
        // decoder can emit a burst past the playhead; pivoting on those pts used to drop the
        // wanted picture and walk the GOP again.
        let pivot = if st.pin != i64::MIN { st.pin } else { pts };
        while st.bytes > budget && st.frames.len() > 2 {
            let (Some(&first), Some(&last)) = (st.frames.keys().next(), st.frames.keys().next_back()) else { break };
            let mut victim = if pivot - first > last - pivot { first } else { last };
            if victim == pivot || victim == pts {
                victim = if victim == first { last } else { first };
                if victim == pivot || victim == pts {
                    break;
                }
            }
            if let Some(v) = st.frames.remove(&victim) {
                st.drafts.remove(&victim);
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
    }

    /// Store decoder output (in presentation order) and advance `out_max`.
    fn store_output(&self, st: &mut State, out: Vec<crate::video::DecodedFrame>) {
        count_frames(&out);
        for d in out {
            st.out_max = st.out_max.max(d.pts);
            self.store(st, d.pts, d.frame, d.draft);
        }
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
        let r = self.frame_late_locked(s, target, late_before);
        if filmcraft_media::cancel::background() {
            // Outside the cache's lock (see `release_background`). A thumbnail leaves the decoders
            // of earlier thumbnails no reason to stay: each costs hundreds of MB at 4K.
            drop(self.shared.pool.release_background(&self.shared));
        }
        r
    }

    fn frame_late_locked(&self, s: &dyn VideoSamples, target: i64, late_before: Option<i64>) -> crate::Result<Arc<VideoFrame>> {
        let n = s.count();
        let i = s.sample_at(target.max(0)).or_else(|| (n > 0).then(|| n - 1)).ok_or_else(|| CodecError::Decode("empty track".into()))?;
        let want_pts = s.pts(i);
        let me = self as *const Self as usize;
        if DecodingGuard::active(me) {
            return self.private_frame(s, i, want_pts, n);
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
        if let Some(f) = st.cached(want_pts, draft).cloned() {
            HITS.fetch_add(1, Ordering::Relaxed);
            // a monitor asking for a frame of a thumbnail's clip makes its decoder one in use
            st.background &= background;
            return Ok(f);
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
        if st.decoder.is_none() {
            let d = s.make_decoder()?;
            st.intra = d.intra_only();
            st.decoder = Some(d);
            st.next = usize::MAX;
            self.shared.pool.decoders.fetch_add(1, Ordering::Relaxed);
        }
        released.extend(self.shared.pool.trim_decoders(&self.shared));
        // Nothing cached: the work ahead (possibly a seek and a GOP of decoding) is only worth it
        // while someone still wants the frame.
        if filmcraft_media::cancel::cancelled() {
            return Err(CodecError::Cancelled);
        }
        if st.intra {
            return self.intra_frame(st, s, i, want_pts);
        }
        // with the lock held: see `isolated`
        let st = &mut *st;
        isolated(|| self.decode_until(st, s, i, want_pts, late_before, draft))
    }

    /// Decode (the lock held) until the frame with `want_pts` (sample `i`) is cached; return it.
    fn decode_until(
        &self,
        st: &mut State,
        s: &dyn VideoSamples,
        i: usize,
        want_pts: i64,
        late_before: Option<i64>,
        draft: bool,
    ) -> crate::Result<Arc<VideoFrame>> {
        let n = s.count();
        let mut key = decode_start(s, i);
        // Continue the running decoder when it has passed the wanted sample's sync sample and
        // either has not reached the sample yet or has been fed it without outputting it yet
        // (a frame-threaded decoder holds many pictures in flight). Otherwise the frame was
        // evicted or lies in another GOP: restart at the sync sample. A run that started after
        // `key` (at the random-access picture of a wanted leading picture) left the picture out.
        let running = st.next != usize::MAX && st.next <= n && st.start <= key;
        // Decoding on through a short stretch into the next GOP is cheaper than a restart, and
        // playback wants those frames anyway.
        let near = st.next <= key && key - st.next <= CONTINUE_THROUGH;
        let continuing = running && (st.next > key || near) && (i >= st.next || (st.start <= i && want_pts > st.out_max));
        if !continuing {
            SEEKS.fetch_add(1, Ordering::Relaxed);
            // The container's sync flags may be wrong for the codec (an MP4 without `stss` marks
            // every sample): step back to a sample the decoder can start from.
            while key > 0 {
                let data = s.read(key)?;
                if st.decoder.as_ref().and_then(|d| d.is_random_access(&data)) != Some(false) {
                    break;
                }
                key = s.sync_before(key - 1);
            }
            if let Some(d) = st.decoder.as_mut() {
                d.reset();
            }
            st.next = key;
            st.start = key;
            st.out_max = i64::MIN;
        }
        let limit = (i.max(st.next) + 64).min(n);
        let late = late_before.unwrap_or(i64::MIN).min(want_pts);
        let Some(decoder) = st.decoder.as_mut() else {
            return Err(CodecError::Decode("no video decoder".into()));
        };
        decoder.set_draft(draft);
        while st.next < limit {
            if filmcraft_media::cancel::cancelled() {
                // The decoder state stays consistent (`next`, `out_max`): a later request continues.
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
            self.store_output(st, out);
            if st.cached(want_pts, draft).is_some() {
                break;
            }
        }
        if st.cached(want_pts, draft).is_none() {
            let out = st.decoder.as_mut().map(|d| timed(|| d.flush())).unwrap_or_default();
            self.store_output(st, out);
            st.next = usize::MAX;
        }
        // nearest decoded frame at or before the wanted pts (robust to decoder pts quirks)
        st.at_or_before(want_pts, draft).ok_or_else(|| CodecError::Decode("frame not produced".into()))
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
            // The container's sync flags may be wrong for the codec (an MP4 without `stss` marks
            // every sample): step back to a sample the decoder can start from.
            while key > 0 {
                let data = s.read(key)?;
                if d.is_random_access(&data) != Some(false) {
                    break;
                }
                key = s.sync_before(key - 1);
            }
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
    fn intra_frame(&self, st: std::sync::MutexGuard<'_, State>, s: &dyn VideoSamples, i: usize, want_pts: i64) -> crate::Result<Arc<VideoFrame>> {
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
        // crop and rotate before taking the lock: that work runs on rayon (see `isolated`)
        let res = res.map(|out| {
            count_frames(&out);
            out.into_iter().map(|d| (d.pts, self.finish(d.frame), d.draft)).collect::<Vec<_>>()
        });
        let mut st = self.shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        if st.spare.len() < 16 {
            st.spare.push(dec);
        }
        for (pts, frame, draft) in res? {
            self.store_finished(&mut st, pts, frame, draft);
        }
        st.at_or_before(want_pts, true).ok_or_else(|| CodecError::Decode("frame not produced".into()))
    }
}

#[cfg(test)]
mod tests;
