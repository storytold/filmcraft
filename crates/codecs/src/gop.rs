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

/// A container's video sample table, in decode (file) order.
pub trait VideoSamples {
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

/// The cache always has room for this many frames, whatever their size: a frame-threaded decoder
/// runs up to ~2x its thread count ahead of the frame it returns, and playback prefetches ahead
/// of the playhead, so a byte budget alone would evict 4K frames before they are shown and force
/// a re-decode from the keyframe.
const MIN_FRAMES: usize = 64;

/// A running decoder up to this many samples before the wanted sample's sync sample keeps going
/// rather than restarting at the sync sample.
const CONTINUE_THROUGH: usize = 48;

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
                background: false,
            }),
            pool: pool.clone(),
        });
        pool.register(&shared);
        Self { shared, explicit_color, rotation: 0, crop: None, budget: 384 << 20 }
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
        let f = self.finish(f);
        st.last_used = Instant::now();
        if draft {
            st.drafts.insert(pts);
            DRAFT.fetch_add(1, Ordering::Relaxed);
        } else {
            st.drafts.remove(&pts);
        }
        let budget = self.budget.max(MIN_FRAMES * f.byte_size());
        let before = st.bytes;
        st.bytes += f.byte_size();
        if let Some(old) = st.frames.insert(pts, Arc::new(f)) {
            st.bytes -= old.byte_size();
            filmcraft_frame::pool::recycle(old);
        }
        // evict frames far from the most recent (keep a window around the working position)
        while st.bytes > budget && st.frames.len() > 2 {
            let (Some(&first), Some(&last)) = (st.frames.keys().next(), st.frames.keys().next_back()) else { break };
            let victim = if pts - first > last - pts { first } else { last };
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
        let mut st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let _decoding = DecodingGuard::enter(me);
        st.last_used = Instant::now();
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
        let mut key = s.sync_before(i);
        // Continue the running decoder when it has passed the wanted sample's sync sample and
        // either has not reached the sample yet or has been fed it without outputting it yet
        // (a frame-threaded decoder holds many pictures in flight). Otherwise the frame was
        // evicted or lies in another GOP: restart at the sync sample.
        let running = st.next != usize::MAX && st.next <= n;
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
            self.store_output(&mut st, out);
            if st.cached(want_pts, draft).is_some() {
                break;
            }
        }
        if st.cached(want_pts, draft).is_none() {
            let out = st.decoder.as_mut().map(|d| timed(|| d.flush())).unwrap_or_default();
            self.store_output(&mut st, out);
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
        let mut key = s.sync_before(i);
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
        let mut st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.spare.len() < 16 {
            st.spare.push(dec);
        }
        let res = res?;
        count_frames(&res);
        for d in res {
            self.store(&mut st, d.pts, d.frame, d.draft);
        }
        st.at_or_before(want_pts, true).ok_or_else(|| CodecError::Decode("frame not produced".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::DecodedFrame;
    use std::collections::VecDeque;

    /// `n` samples, a sync sample every `gop`, pts = 1000 · index (no reordering).
    struct Samples {
        n: usize,
        gop: usize,
        delay: usize,
        intra: bool,
        resets: Arc<AtomicUsize>,
        decodes: Arc<AtomicUsize>,
    }

    /// Outputs each picture `delay` samples late, like a frame-threaded decoder.
    struct Dec {
        delay: usize,
        intra: bool,
        /// (pts, decoded as a draft picture)
        held: VecDeque<(i64, bool)>,
        draft: bool,
        /// made for background work (few threads, like the H.264 decoder's)
        limited: bool,
        resets: Arc<AtomicUsize>,
        decodes: Arc<AtomicUsize>,
    }

    /// A picture whose pixel encodes its index (and, in blue, whether it is a draft picture).
    fn picture((pts, draft): (i64, bool)) -> DecodedFrame {
        let i = (pts / 1000) as u32;
        DecodedFrame { pts, frame: VideoFrame::rgba8(1, 1, vec![i as u8, (i >> 8) as u8, draft as u8, 255]), draft }
    }

    impl VideoDecoder for Dec {
        fn decode(&mut self, sample: &[u8], pts: i64) -> crate::Result<Vec<DecodedFrame>> {
            self.decodes.fetch_add(1, Ordering::Relaxed);
            // like H.264 draft mode: only non-reference (odd) pictures are approximate
            self.held.push_back((pts, self.draft && sample[0] % 2 == 1));
            let mut out = Vec::new();
            while self.held.len() > self.delay {
                out.push(picture(self.held.pop_front().expect("held")));
            }
            Ok(out)
        }
        fn flush(&mut self) -> Vec<DecodedFrame> {
            self.held.drain(..).map(picture).collect()
        }
        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::Relaxed);
            self.held.clear();
        }
        fn set_draft(&mut self, on: bool) {
            self.draft = on;
        }
        fn name(&self) -> &str {
            "test"
        }
        fn thread_limited(&self) -> bool {
            self.limited
        }
        fn intra_only(&self) -> bool {
            self.intra
        }
        // odd samples are non-reference pictures
        fn is_disposable(&self, sample: &[u8]) -> bool {
            sample[0] % 2 == 1
        }
    }

    impl VideoSamples for Samples {
        fn count(&self) -> usize {
            self.n
        }
        fn pts(&self, i: usize) -> i64 {
            i as i64 * 1000
        }
        fn sync_before(&self, i: usize) -> usize {
            if self.intra { i } else { i / self.gop * self.gop }
        }
        fn sample_at(&self, t: i64) -> Option<usize> {
            let i = (t / 1000) as usize;
            (i < self.n).then_some(i)
        }
        fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
            Ok(vec![i as u8])
        }
        fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
            Ok(Box::new(Dec {
                delay: self.delay,
                intra: self.intra,
                held: VecDeque::new(),
                draft: false,
                limited: filmcraft_media::cancel::background(),
                resets: self.resets.clone(),
                decodes: self.decodes.clone(),
            }))
        }
    }

    fn samples(n: usize, gop: usize, delay: usize, intra: bool) -> Samples {
        Samples { n, gop, delay, intra, resets: Default::default(), decodes: Default::default() }
    }

    fn index_of(f: &VideoFrame) -> usize {
        match &f.data {
            filmcraft_frame::PixelData::Rgba8(d) => d[0] as usize | (d[1] as usize) << 8,
            _ => unreachable!(),
        }
    }

    #[test]
    fn frames_held_by_a_threaded_decoder_do_not_restart_it() {
        // 24 pictures in flight (more than any fixed look-ahead margin), one long GOP.
        let s = samples(300, 250, 24, false);
        let c = GopCache::new(None);
        // Playback order with prefetch running ahead: a later frame first, then earlier ones the
        // decoder has been fed but not output yet.
        for i in [10usize, 11, 40, 12, 30, 13, 60, 14, 15] {
            assert_eq!(index_of(&c.frame(&s, i as i64 * 1000).expect("frame")), i);
        }
        assert_eq!(s.resets.load(Ordering::Relaxed), 1, "only the first request seeks");
        assert!(s.decodes.load(Ordering::Relaxed) <= 60 + 24 + 1);
    }

    #[test]
    fn running_decoder_continues_through_a_nearby_sync_sample() {
        let s = samples(200, 30, 0, false);
        let c = GopCache::new(None);
        assert_eq!(index_of(&c.frame(&s, 25_000).expect("frame")), 25);
        // frame 40 lies in the next GOP (sync sample 30): decode 26..40 instead of restarting at 30
        assert_eq!(index_of(&c.frame(&s, 40_000).expect("frame")), 40);
        assert_eq!(index_of(&c.frame(&s, 28_000).expect("frame")), 28);
        assert_eq!(s.resets.load(Ordering::Relaxed), 1);
        // far ahead: restart at the sync sample rather than decode everything in between
        assert_eq!(index_of(&c.frame(&s, 150_000).expect("frame")), 150);
        assert_eq!(s.resets.load(Ordering::Relaxed), 2);
        assert!(s.decodes.load(Ordering::Relaxed) <= 41 + 1);
    }

    #[test]
    fn intra_frames_decode_in_parallel_once_each() {
        let s = Arc::new(samples(64, 1, 0, true));
        let c = Arc::new(GopCache::new(None));
        std::thread::scope(|scope| {
            for t in 0..4usize {
                let (s, c) = (s.clone(), c.clone());
                scope.spawn(move || {
                    for k in 0..64usize {
                        let i = (k * 7 + t * 16) % 64;
                        assert_eq!(index_of(&c.frame(&*s, i as i64 * 1000).expect("frame")), i);
                    }
                });
            }
        });
        // a frame two threads miss at the same moment may decode twice; nothing more
        assert!(s.decodes.load(Ordering::Relaxed) <= 64 + 4 * 4);
        assert_eq!(s.resets.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn catching_up_skips_late_non_reference_samples_only() {
        let s = samples(300, 250, 4, false);
        let c = GopCache::new(None);
        assert_eq!(index_of(&c.frame(&s, 2_000).expect("frame")), 2);
        let before = s.decodes.load(Ordering::Relaxed);
        // playback is behind: frame 41 is due, 7..=40 are late
        let f = c.frame_late(&s, 41_000, Some(41_000)).expect("frame");
        assert_eq!(index_of(&f), 41, "the wanted (odd, disposable) frame itself is decoded");
        let fed = s.decodes.load(Ordering::Relaxed) - before;
        // samples 7..41 minus the odd ones before 41, plus the decoder's output delay
        assert!(fed <= 18 + 4 + 1, "fed {fed}");
        // the decoder continues from there: the next frames are decoded, not skipped
        for i in 42..48usize {
            assert_eq!(index_of(&c.frame(&s, i as i64 * 1000).expect("frame")), i);
        }
        assert_eq!(s.resets.load(Ordering::Relaxed), 1);
        // without the hint nothing is skipped
        let s2 = samples(300, 250, 4, false);
        let c2 = GopCache::new(None);
        assert_eq!(index_of(&c2.frame(&s2, 41_000).expect("frame")), 41);
        assert!(s2.decodes.load(Ordering::Relaxed) >= 42);
        // a prefetch job for frame 41 while frame 30 is due: only frames before 30 are late
        let s3 = samples(300, 250, 0, false);
        let c3 = GopCache::new(None);
        assert_eq!(index_of(&c3.frame_late(&s3, 41_000, Some(30_000)).expect("frame")), 41);
        assert_eq!(s3.decodes.load(Ordering::Relaxed), 42 - 15, "odd samples 1..=29 skipped");
        for i in 30..41usize {
            assert_eq!(index_of(&c3.frame(&s3, i as i64 * 1000).expect("cached")), i);
        }
        assert_eq!(s3.resets.load(Ordering::Relaxed), 1);
    }

    fn is_draft(f: &VideoFrame) -> bool {
        match &f.data {
            filmcraft_frame::PixelData::Rgba8(d) => d[2] == 1,
            _ => unreachable!(),
        }
    }

    #[test]
    fn draft_frames_are_served_to_draft_requests_only() {
        let s = samples(300, 250, 3, false);
        let c = GopCache::new(None);
        let before = gop_stats().draft;
        // reduced-resolution playback: odd (non-reference) pictures come out as drafts
        for i in 0..12usize {
            let f = filmcraft_media::cancel::with_draft(true, || c.frame(&s, i as i64 * 1000)).expect("frame");
            assert_eq!(index_of(&f), i);
            assert_eq!(is_draft(&f), i % 2 == 1, "frame {i}");
        }
        assert!(gop_stats().draft - before >= 6);
        assert_eq!(s.resets.load(Ordering::Relaxed), 1);
        // paused / export (no draft hint): an even frame is exact and cached
        let f = c.frame(&s, 4_000).expect("frame");
        assert!(!is_draft(&f) && index_of(&f) == 4);
        assert_eq!(s.resets.load(Ordering::Relaxed), 1, "exact frames are served from the cache");
        // a draft frame is decoded again, exactly
        let f = c.frame(&s, 5_000).expect("frame");
        assert!(!is_draft(&f) && index_of(&f) == 5);
        assert_eq!(s.resets.load(Ordering::Relaxed), 2, "re-decoded from the sync sample");
        // and a draft request takes the exact frame now cached
        let f = filmcraft_media::cancel::with_draft(true, || c.frame(&s, 5_000)).expect("frame");
        assert!(!is_draft(&f));
        // without the hint nothing is ever a draft
        let s2 = samples(300, 250, 3, false);
        let c2 = GopCache::new(None);
        assert!((0..12).all(|i| !is_draft(&c2.frame(&s2, i * 1000).expect("frame"))));
    }

    #[test]
    fn cancelled_request_stops_decoding_and_keeps_state() {
        let s = samples(300, 250, 0, false);
        let c = GopCache::new(None);
        assert_eq!(index_of(&c.frame(&s, 5_000).expect("frame")), 5);
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let r = filmcraft_media::cancel::with_cancel(&flag, || c.frame(&s, 200_000));
        assert!(matches!(r, Err(CodecError::Cancelled)));
        assert_eq!(s.decodes.load(Ordering::Relaxed), 6);
        // cached frames are still served, and the decoder carries on from where it was
        let r = filmcraft_media::cancel::with_cancel(&flag, || c.frame(&s, 3_000));
        assert_eq!(index_of(&r.expect("cached")), 3);
        assert_eq!(index_of(&c.frame(&s, 7_000).expect("frame")), 7);
        assert_eq!(s.resets.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn idle_caches_give_up_their_decoders_beyond_the_cap() {
        let pool = Arc::new(Pool::new(2, Duration::ZERO, FRAME_BUDGET));
        let live = || pool.decoders.load(Ordering::Relaxed);
        let s: Vec<Samples> = (0..5).map(|_| samples(300, 250, 3, false)).collect();
        let c: Vec<GopCache> = (0..5).map(|_| GopCache::in_pool(None, pool.clone())).collect();
        // a clip after another, as when clips are dropped on a timeline
        for k in 0..5 {
            assert_eq!(index_of(&c[k].frame(&s[k], 10_000).expect("frame")), 10);
            assert!(live() <= 2, "{} live decoders after clip {k}", live());
        }
        assert_eq!(live(), 2);
        // the most recently used keep theirs and carry on without a seek
        assert_eq!(index_of(&c[4].frame(&s[4], 20_000).expect("frame")), 20);
        assert_eq!(s[4].resets.load(Ordering::Relaxed), 1);
        // a cache that lost its decoder still serves what it cached…
        let decodes = s[0].decodes.load(Ordering::Relaxed);
        assert_eq!(index_of(&c[0].frame(&s[0], 10_000).expect("cached")), 10);
        assert_eq!(s[0].decodes.load(Ordering::Relaxed), decodes);
        // …and decodes again with a new decoder, from the sync sample
        assert_eq!(index_of(&c[0].frame(&s[0], 20_000).expect("frame")), 20);
        assert_eq!(s[0].resets.load(Ordering::Relaxed), 2);
        assert_eq!(live(), 2);
        // a dropped cache no longer counts
        drop(c);
        assert_eq!(live(), 0);
    }

    #[test]
    fn caches_in_use_keep_their_decoders_whatever_the_cap() {
        // every layer of one composited frame was asked moments ago: none is idle
        let pool = Arc::new(Pool::new(2, Duration::from_secs(3600), FRAME_BUDGET));
        let s: Vec<Samples> = (0..5).map(|_| samples(300, 250, 3, false)).collect();
        let c: Vec<GopCache> = (0..5).map(|_| GopCache::in_pool(None, pool.clone())).collect();
        for t in [10_000, 11_000, 12_000] {
            for k in 0..5 {
                assert_eq!(index_of(&c[k].frame(&s[k], t).expect("frame")) as i64, t / 1000);
            }
        }
        assert_eq!(pool.decoders.load(Ordering::Relaxed), 5);
        assert!(s.iter().all(|s| s.resets.load(Ordering::Relaxed) == 1), "no decoder restarted");
    }

    /// A burst of navigation (every clip of a timeline asked once within moments, as when
    /// thumbnails are made or the playhead jumps clip to clip) must not keep every clip's decoder
    /// and frames: only the caches of the frame being composited are in use.
    #[test]
    fn a_burst_of_single_requests_does_not_keep_every_decoder_and_frame() {
        // the real decoder caps and idle time; RECENT scaled down from 500 ms to the test's pace
        // (3 ms per clip). With the old rule (recent = idle) all 24 decoders stay live.
        let pool = Arc::new(Pool::new(MAX_LIVE_DECODERS, IDLE, FRAME_BUDGET).tiered(Duration::from_millis(2), HARD_DECODERS));
        let n = 24;
        let s: Vec<Samples> = (0..n).map(|_| samples(300, 250, 3, false)).collect();
        let c: Vec<GopCache> = (0..n).map(|_| GopCache::in_pool(None, pool.clone())).collect();
        for k in 0..n {
            assert_eq!(index_of(&c[k].frame(&s[k], 10_000).expect("frame")), 10);
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(
            pool.decoders.load(Ordering::Relaxed) <= HARD_DECODERS + 1,
            "{} live decoders after a burst over {n} clips",
            pool.decoders.load(Ordering::Relaxed)
        );
        // the clip asked last, and a clip in use together with it, keep theirs and carry on
        let resets = s[n - 1].resets.load(Ordering::Relaxed);
        assert_eq!(index_of(&c[n - 1].frame(&s[n - 1], 11_000).expect("frame")), 11);
        assert_eq!(s[n - 1].resets.load(Ordering::Relaxed), resets);
    }

    /// The thumbnails an import queues: every clip asked once, quickly (the real caps and recency,
    /// no sleeping: all of them are "recent"). Each 4K software decoder costs ~0.5 GB, and 42
    /// clips used to hold 6+ GB at once.
    #[test]
    fn background_requests_leave_no_decoders_behind() {
        use filmcraft_media::cancel::with_background;
        let pool = Arc::new(Pool::new(MAX_LIVE_DECODERS, IDLE, FRAME_BUDGET).tiered(RECENT, HARD_DECODERS));
        let n = 24;
        let s: Vec<Samples> = (0..n).map(|_| samples(300, 250, 3, false)).collect();
        let c: Vec<GopCache> = (0..n).map(|_| GopCache::in_pool(None, pool.clone())).collect();
        // a clip in use by a monitor
        assert_eq!(index_of(&c[0].frame(&s[0], 10_000).expect("frame")), 10);
        for k in 1..n {
            assert_eq!(index_of(&with_background(true, || c[k].frame(&s[k], 10_000)).expect("frame")), 10);
            assert!(pool.decoders.load(Ordering::Relaxed) <= 2, "{} live decoders after thumbnail {k}", pool.decoders.load(Ordering::Relaxed));
        }
        // the monitor's clip and the last thumbnail keep theirs and carry on without a seek
        assert_eq!(pool.decoders.load(Ordering::Relaxed), 2);
        assert_eq!(index_of(&c[0].frame(&s[0], 11_000).expect("frame")), 11);
        assert_eq!(s[0].resets.load(Ordering::Relaxed), 1);
        assert_eq!(index_of(&with_background(true, || c[n - 1].frame(&s[n - 1], 12_000)).expect("frame")), 12);
        assert_eq!(s[n - 1].resets.load(Ordering::Relaxed), 1);
        // a monitor asking for a frame of a thumbnail's clip makes its decoder one in use
        assert_eq!(index_of(&c[n - 1].frame(&s[n - 1], 13_000).expect("frame")), 13);
        for k in 1..4 {
            with_background(true, || c[k].frame(&s[k], 20_000)).expect("frame");
        }
        assert_eq!(index_of(&c[n - 1].frame(&s[n - 1], 14_000).expect("frame")), 14);
        assert!(c[n - 1].shared.state.lock().unwrap().decoder.is_some(), "the thumbnails left its decoder");
        // a cache that lost its decoder still decodes (from the sync sample)
        assert_eq!(index_of(&with_background(true, || c[1].frame(&s[1], 30_000)).expect("frame")), 30);
    }

    #[test]
    fn a_monitor_replaces_a_decoder_made_for_a_thumbnail() {
        use filmcraft_media::cancel::with_background;
        let pool = Arc::new(Pool::new(MAX_LIVE_DECODERS, IDLE, FRAME_BUDGET).tiered(RECENT, HARD_DECODERS));
        let (s, c) = (samples(300, 250, 3, false), GopCache::in_pool(None, pool.clone()));
        assert_eq!(index_of(&with_background(true, || c.frame(&s, 10_000)).expect("frame")), 10);
        // another thumbnail of the same clip carries on with the thumbnail decoder
        assert_eq!(index_of(&with_background(true, || c.frame(&s, 12_000)).expect("frame")), 12);
        assert_eq!(s.resets.load(Ordering::Relaxed), 1);
        // a frame a monitor waits for (not cached: far ahead) is decoded by a full-thread decoder
        assert_eq!(index_of(&c.frame(&s, 100_000).expect("frame")), 100);
        assert!(!c.shared.state.lock().unwrap().decoder.as_ref().is_some_and(|d| d.thread_limited()));
        assert_eq!(pool.decoders.load(Ordering::Relaxed), 1);
        // and keeps it for the next frames
        let resets = s.resets.load(Ordering::Relaxed);
        assert_eq!(index_of(&c.frame(&s, 101_000).expect("frame")), 101);
        assert_eq!(s.resets.load(Ordering::Relaxed), resets);
    }

    #[test]
    fn a_burst_of_single_requests_does_not_keep_every_clips_frames() {
        // 1×1 RGBA pictures (4 bytes): a shared budget of ten of them
        let pool = Arc::new(Pool::new(usize::MAX, Duration::from_secs(3600), 40).tiered(Duration::from_millis(2), usize::MAX));
        let n = 12;
        let s: Vec<Samples> = (0..n).map(|_| samples(300, 250, 0, false)).collect();
        let c: Vec<GopCache> = (0..n).map(|_| GopCache::in_pool(None, pool.clone())).collect();
        for k in 0..n {
            assert_eq!(index_of(&c[k].frame(&s[k], 10_000).expect("frame")), 10);
            std::thread::sleep(Duration::from_millis(5));
        }
        let held = pool.bytes.load(Ordering::Relaxed);
        assert!(held <= 40 + 11 * 4, "{held} bytes held after a burst over {n} clips");
    }

    #[test]
    fn intra_caches_give_up_their_spare_decoders_too() {
        let pool = Arc::new(Pool::new(1, Duration::ZERO, FRAME_BUDGET));
        let (a, b) = (samples(50, 1, 0, true), samples(50, 1, 0, true));
        let (ca, cb) = (GopCache::in_pool(None, pool.clone()), GopCache::in_pool(None, pool.clone()));
        assert_eq!(index_of(&ca.frame(&a, 3_000).expect("frame")), 3);
        assert_eq!(index_of(&cb.frame(&b, 4_000).expect("frame")), 4);
        let st = ca.shared.state.lock().unwrap();
        assert!(st.decoder.is_none() && st.spare.is_empty());
        drop(st);
        assert_eq!(pool.decoders.load(Ordering::Relaxed), 1);
        assert_eq!(index_of(&ca.frame(&a, 5_000).expect("frame")), 5);
    }

    #[test]
    fn idle_caches_give_up_their_frames_beyond_the_shared_budget() {
        // 1×1 RGBA pictures: 4 bytes each, a shared budget of ten of them
        let pool = Arc::new(Pool::new(usize::MAX, Duration::ZERO, 40));
        let held = || pool.bytes.load(Ordering::Relaxed);
        let (a, b) = (samples(300, 250, 0, false), samples(300, 250, 0, false));
        let (ca, cb) = (GopCache::in_pool(None, pool.clone()), GopCache::in_pool(None, pool.clone()));
        // the only cache keeps what its own budget allows
        assert_eq!(index_of(&ca.frame(&a, 10_000).expect("frame")), 10);
        assert_eq!(held(), 44);
        // a second clip takes the room from the idle first one, earliest frames first
        assert_eq!(index_of(&cb.frame(&b, 5_000).expect("frame")), 5);
        assert_eq!(held(), 40);
        let decodes = b.decodes.load(Ordering::Relaxed);
        assert!((0..=5).all(|i| index_of(&cb.frame(&b, i * 1000).expect("cached")) as i64 == i));
        assert_eq!(b.decodes.load(Ordering::Relaxed), decodes, "the clip in use lost nothing");
        let st = ca.shared.state.lock().unwrap();
        assert_eq!((st.frames.len(), st.bytes), (4, 16));
        assert_eq!(st.frames.keys().next(), Some(&7_000));
        drop(st);
        // an evicted frame decodes again
        assert_eq!(index_of(&ca.frame(&a, 2_000).expect("frame")), 2);
        assert!(held() <= 40 + 4 * 3, "{} bytes held", held());
        drop((ca, cb));
        assert_eq!(held(), 0);
    }

    #[test]
    fn caches_in_use_keep_their_frames_whatever_the_shared_budget() {
        let pool = Arc::new(Pool::new(usize::MAX, Duration::from_secs(3600), 40));
        let (a, b) = (samples(300, 250, 0, false), samples(300, 250, 0, false));
        let (ca, cb) = (GopCache::in_pool(None, pool.clone()), GopCache::in_pool(None, pool.clone()));
        assert_eq!(index_of(&ca.frame(&a, 10_000).expect("frame")), 10);
        assert_eq!(index_of(&cb.frame(&b, 10_000).expect("frame")), 10);
        assert_eq!(pool.bytes.load(Ordering::Relaxed), 88);
        let decodes = a.decodes.load(Ordering::Relaxed);
        assert_eq!(index_of(&ca.frame(&a, 0).expect("cached")), 0);
        assert_eq!(a.decodes.load(Ordering::Relaxed), decodes);
    }

    /// Pictures big enough for the plane pool (its own size, so that parallel tests cannot take
    /// the buffers), no reordering, one long GOP.
    struct Big(usize);
    const BIG: (u32, u32) = (200, 101);

    impl VideoDecoder for Big {
        fn decode(&mut self, _sample: &[u8], pts: i64) -> crate::Result<Vec<DecodedFrame>> {
            let mut px = filmcraft_frame::pool::take_u8((BIG.0 * BIG.1 * 4) as usize);
            px.resize((BIG.0 * BIG.1 * 4) as usize, (pts / 1000) as u8);
            Ok(vec![DecodedFrame { pts, frame: VideoFrame::rgba8(BIG.0, BIG.1, px), draft: false }])
        }
        fn flush(&mut self) -> Vec<DecodedFrame> {
            Vec::new()
        }
        fn reset(&mut self) {}
        fn name(&self) -> &str {
            "big"
        }
    }

    impl VideoSamples for Big {
        fn count(&self) -> usize {
            self.0
        }
        fn pts(&self, i: usize) -> i64 {
            i as i64 * 1000
        }
        fn sync_before(&self, _i: usize) -> usize {
            0
        }
        fn sample_at(&self, t: i64) -> Option<usize> {
            let i = (t / 1000) as usize;
            (i < self.0).then_some(i)
        }
        fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
            Ok(vec![i as u8])
        }
        fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
            Ok(Box::new(Big(self.0)))
        }
    }

    #[test]
    fn evicted_frames_are_decoded_into_again() {
        let s = Big(200);
        // room for MIN_FRAMES pictures only
        let mut c = GopCache::new(None);
        c.budget = 0;
        let reused = || filmcraft_frame::pool::stats().reused;
        let before = reused();
        // the cache holds MIN_FRAMES pictures: none is evicted yet, every plane is new
        for i in 0..MIN_FRAMES as i64 {
            assert_eq!(c.frame(&s, i * 1000).expect("frame").width, BIG.0);
        }
        assert_eq!(reused(), before);
        // from here on each picture evicts one, whose buffer the next picture is decoded into
        let more = 40;
        for i in MIN_FRAMES as i64..MIN_FRAMES as i64 + more {
            let f = c.frame(&s, i * 1000).expect("frame");
            assert!(matches!(&f.data, filmcraft_frame::PixelData::Rgba8(d) if d.iter().all(|&b| b == i as u8)), "frame {i} has its own pixels");
        }
        assert!(reused() - before >= more as u64 - 2, "{} of {more} planes recycled", reused() - before);
    }
}
