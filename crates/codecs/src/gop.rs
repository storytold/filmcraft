//! GOP-aware random access shared by the container sources (MP4/MOV, Matroska/WebM).
//!
//! Seeking: find the sample whose presentation interval covers the requested time, decode forward
//! from the preceding sync sample, and cache every decoded frame (keyed by pts). Playback requests
//! for the next frames therefore hit the cache or continue the running decoder without re-seeking.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use filmcraft_color::ColorInfo;
use filmcraft_frame::VideoFrame;

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

/// Run a decoder call, adding its wall time to the process-wide counter.
fn timed<R>(f: impl FnOnce() -> R) -> R {
    let t0 = web_time::Instant::now();
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
}

impl State {
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

/// Decoder + decoded-frame cache for one video track.
pub struct GopCache {
    state: Mutex<State>,
    /// Colour signalled by the container, which wins over the bitstream's.
    explicit_color: Option<ColorInfo>,
    /// Clockwise quarter turns from the container's display matrix, applied to every frame.
    rotation: u8,
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
        Self {
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
            }),
            explicit_color,
            rotation: 0,
            budget: 384 << 20,
        }
    }

    /// Turn every decoded frame clockwise by `quarter_turns` × 90° (the container's display
    /// rotation, e.g. portrait phone video stored landscape).
    pub fn with_rotation(mut self, quarter_turns: u8) -> Self {
        self.rotation = quarter_turns % 4;
        self
    }

    /// Colour signalled by the container wins over the bitstream's (YUV frames only); the
    /// container's display rotation is applied.
    fn finish(&self, mut f: VideoFrame) -> VideoFrame {
        if let Some(c) = self.explicit_color
            && !matches!(f.data, filmcraft_frame::PixelData::Rgba8(_) | filmcraft_frame::PixelData::RgbaF32(_))
        {
            f.color = c;
        }
        if self.rotation != 0 { f.rotated(self.rotation) } else { f }
    }

    fn store(&self, st: &mut State, pts: i64, f: VideoFrame, draft: bool) {
        let f = self.finish(f);
        if draft {
            st.drafts.insert(pts);
            DRAFT.fetch_add(1, Ordering::Relaxed);
        } else {
            st.drafts.remove(&pts);
        }
        let budget = self.budget.max(MIN_FRAMES * f.byte_size());
        st.bytes += f.byte_size();
        if let Some(old) = st.frames.insert(pts, Arc::new(f)) {
            st.bytes -= old.byte_size();
        }
        // evict frames far from the most recent (keep a window around the working position)
        while st.bytes > budget && st.frames.len() > 2 {
            let first = *st.frames.keys().next().expect("non-empty");
            let last = *st.frames.keys().next_back().expect("non-empty");
            let victim = if pts - first > last - pts { first } else { last };
            if let Some(v) = st.frames.remove(&victim) {
                st.drafts.remove(&victim);
                st.bytes -= v.byte_size();
                EVICTED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Store decoder output (in presentation order) and advance `out_max`.
    fn store_output(&self, st: &mut State, out: Vec<crate::video::DecodedFrame>) {
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
        let n = s.count();
        let i = s.sample_at(target.max(0)).or_else(|| (n > 0).then(|| n - 1)).ok_or_else(|| CodecError::Decode("empty track".into()))?;
        let want_pts = s.pts(i);
        let me = self as *const Self as usize;
        if DecodingGuard::active(me) {
            return self.private_frame(s, i, want_pts, n);
        }
        let draft = filmcraft_media::cancel::draft();
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let _decoding = DecodingGuard::enter(me);
        if let Some(f) = st.cached(want_pts, draft) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(f.clone());
        }
        MISSES.fetch_add(1, Ordering::Relaxed);
        if st.decoder.is_none() {
            let d = s.make_decoder()?;
            st.intra = d.intra_only();
            st.decoder = Some(d);
            st.next = usize::MAX;
        }
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
                if st.decoder.as_ref().expect("decoder").is_random_access(&data) != Some(false) {
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
        st.decoder.as_mut().expect("decoder").set_draft(draft);
        while st.next < limit {
            if filmcraft_media::cancel::cancelled() {
                // The decoder state stays consistent (`next`, `out_max`): a later request continues.
                return Err(CodecError::Cancelled);
            }
            let k = st.next;
            let data = s.read(k)?;
            if k != i && s.pts(k) < late && st.decoder.as_ref().expect("decoder").is_disposable(&data) {
                st.next += 1;
                SKIPPED.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let out = timed(|| st.decoder.as_mut().expect("decoder").decode(&data, s.pts(k)))?;
            st.next += 1;
            DECODED.fetch_add(1, Ordering::Relaxed);
            self.store_output(&mut st, out);
            if st.cached(want_pts, draft).is_some() {
                break;
            }
        }
        if st.cached(want_pts, draft).is_none() {
            let out = timed(|| st.decoder.as_mut().expect("decoder").flush());
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
            *next += 1;
            found |= pics.iter().any(|p| p.pts == want_pts);
            out.extend(pics);
            if found {
                break;
            }
        }
        if !found {
            out.extend(timed(|| d.flush()));
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
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.spare.len() < 16 {
            st.spare.push(dec);
        }
        for d in res? {
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
    use std::sync::atomic::AtomicUsize;

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
}
