#[test]
fn open_gop_sync_walks_a_long_chain_of_leading_pictures_without_recursing() {
    // Every GOP of 3 starts at a key whose leading picture is shown before it (#2803).
    let gops = 200_000usize;
    let sync = |i: usize| i - i % 3;
    let leads = |i: usize, key: usize| i == key + 2;
    assert_eq!(super::open_gop_sync(gops * 3 - 1, sync, leads), 0);
    // A picture that is not a leading one keeps its own key.
    assert_eq!(super::open_gop_sync(7, sync, |_, _| false), 6);
    // An index whose sync sample lies past `i` ends the walk instead of looping.
    assert_eq!(super::open_gop_sync(5, |_| 9, |_, _| true), 9);
}

use super::*;
use crate::video::DecodedFrame;
use std::collections::VecDeque;

#[derive(Debug)]
struct AccountedSurface;

impl filmcraft_frame::GpuSurface for AccountedSurface {
    fn size(&self) -> (u32, u32) {
        (2, 2)
    }
    fn byte_len(&self) -> usize {
        2 * 2 * 4
    }
    fn id(&self) -> u64 {
        1
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn prepare_cpu(&self, transfer: filmcraft_frame::CpuTransfer) {
        transfer.complete(Ok(filmcraft_frame::PixelData::RgbaF32(Arc::new(vec![0.0; 2 * 2 * 4]))));
    }
}

#[test]
fn materialization_updates_local_and_shared_charges_without_cache_reentry() {
    use filmcraft_frame::{Chroma, GpuFormat, GpuPixels, PixelData};
    let gpu_bytes = 2 * 2 * 4;
    let cpu_bytes = 2 * 2 * 4 * size_of::<f32>();
    let pool = Arc::new(Pool::new(usize::MAX, Duration::ZERO, FRAME_BUDGET));
    let cache = GopCache::in_pool(None, pool.clone());
    let mut state = cache.shared.state.lock().unwrap();
    let frame = VideoFrame {
        data: PixelData::Gpu(GpuPixels::new(Arc::new(AccountedSurface), GpuFormat::Yuv { chroma: Chroma::C420, bits: 8 })),
        ..VideoFrame::rgba8(2, 2, vec![0; gpu_bytes])
    };
    cache.store_finished(&mut state, 0, frame, false);
    let retained = state.cached(0, false).unwrap().clone();
    assert_eq!((state.bytes.bytes(), pool.bytes.bytes()), (gpu_bytes, gpu_bytes));
    retained.cpu().unwrap(); // Completion runs while this cache lock is held.
    assert_eq!((state.bytes.bytes(), pool.bytes.bytes()), (gpu_bytes + cpu_bytes, gpu_bytes + cpu_bytes));
    cache.store_finished(&mut state, 0, VideoFrame::rgba8(2, 2, vec![0; gpu_bytes]), false);
    assert_eq!((state.bytes.bytes(), pool.bytes.bytes()), (gpu_bytes, gpu_bytes));
    state.release_frames(|| true);
    assert_eq!((state.bytes.bytes(), pool.bytes.bytes()), (0, 0));
    assert!(retained.cpu().is_ok(), "cache release must preserve externally retained pixels");
}

#[derive(Debug)]
struct UnreadableSurface;

#[test]
fn late_payload_eviction_and_shared_drop_release_local_and_pool_charges() {
    use filmcraft_frame::{Chroma, GpuFormat, GpuPixels, PixelData};
    let gpu_bytes = 2 * 2 * 4;
    let cpu_bytes = 2 * 2 * 4 * size_of::<f32>();
    let pool = Arc::new(Pool::new(usize::MAX, Duration::ZERO, gpu_bytes * 2));
    let first = GopCache::in_pool(None, pool.clone());
    let second = GopCache::in_pool(None, pool.clone());
    let frame = VideoFrame {
        data: PixelData::Gpu(GpuPixels::new(Arc::new(AccountedSurface), GpuFormat::Yuv { chroma: Chroma::C420, bits: 8 })),
        ..VideoFrame::rgba8(2, 2, vec![0; gpu_bytes])
    };
    let (local, retained) = {
        let mut state = first.shared.state.lock().unwrap();
        first.store_finished(&mut state, 0, frame, false);
        (state.bytes.clone(), state.cached(0, false).unwrap().clone())
    };
    {
        let mut state = second.shared.state.lock().unwrap();
        second.store_finished(&mut state, 0, VideoFrame::rgba8(2, 2, vec![0; gpu_bytes]), false);
    }
    assert_eq!(pool.bytes.bytes(), gpu_bytes * 2);
    retained.cpu().unwrap();
    assert_eq!((local.bytes(), pool.bytes.bytes()), (gpu_bytes + cpu_bytes, gpu_bytes * 2 + cpu_bytes));
    // Completion updates totals; the next explicit pool trim evicts the idle first cache.
    pool.trim_frames(&second.shared);
    assert_eq!((local.bytes(), pool.bytes.bytes()), (0, gpu_bytes));
    assert!(first.shared.state.lock().unwrap().frames.is_empty());
    assert!(retained.cpu().is_ok());
    {
        let mut state = first.shared.state.lock().unwrap();
        first.store_finished(&mut state, 0, (*retained).clone(), false);
    }
    assert_eq!((local.bytes(), pool.bytes.bytes()), (gpu_bytes + cpu_bytes, gpu_bytes + cpu_bytes));
    let shared = Arc::downgrade(&first.shared);
    drop(first);
    assert!(shared.upgrade().is_none());
    assert_eq!((local.bytes(), pool.bytes.bytes()), (0, 0));
    assert!(retained.cpu().is_ok());
}

impl filmcraft_frame::GpuSurface for UnreadableSurface {
    fn size(&self) -> (u32, u32) {
        (2, 2)
    }
    fn byte_len(&self) -> usize {
        0
    }
    fn prepare_cpu(&self, transfer: filmcraft_frame::CpuTransfer) {
        transfer.complete(Err("GPU readback failed".into()));
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn id(&self) -> u64 {
        0
    }
}

#[test]
fn failed_frame_access_is_typed_and_never_cached() {
    use filmcraft_frame::{Chroma, GpuPixels, PixelData};
    let surface = Arc::new(UnreadableSurface);
    let (width, height) = filmcraft_frame::GpuSurface::size(&*surface);
    let frame = VideoFrame {
        width,
        height,
        data: PixelData::Gpu(GpuPixels::new(surface, filmcraft_frame::GpuFormat::Yuv { chroma: Chroma::C420, bits: u8::BITS })),
        color: filmcraft_color::ColorInfo::REC709,
        par: (1, 1),
        pts: filmcraft_time::Tick::ZERO,
    };
    let crop = Region { x: 0, y: 0, w: usize::try_from(width / 2).unwrap(), h: usize::try_from(height).unwrap() };
    // Partial crop and quarter-turn each require unavailable CPU pixels.
    for cache in [GopCache::new(None).with_crop(Some(crop)), GopCache::new(None).with_rotation(1)] {
        let mut state = cache.shared.state.lock().unwrap();
        let previous_max = state.out_max;
        let output = vec![DecodedFrame { pts: 0, frame: frame.clone(), draft: false }];
        let error = cache.store_output(&mut state, output).unwrap_err();
        assert!(matches!(filmcraft_media::MediaError::from(error), filmcraft_media::MediaError::FrameAccess(why) if why == "GPU readback failed"));
        assert!(state.frames.is_empty());
        assert_eq!(state.bytes.bytes(), 0);
        assert_eq!(state.out_max, previous_max);
    }
}

struct FailingOutputSamples {
    samples: Samples,
    failing_pts: i64,
    unreadable: Arc<std::sync::atomic::AtomicBool>,
    at_flush: bool,
    fail_drain: bool,
}

struct FailingOutputDecoder {
    inner: Box<dyn VideoDecoder>,
    failing_pts: i64,
    unreadable: Arc<std::sync::atomic::AtomicBool>,
    at_flush: bool,
    fail_drain: bool,
    held: Vec<DecodedFrame>,
}

impl FailingOutputDecoder {
    fn picture(&self, mut d: DecodedFrame) -> DecodedFrame {
        use filmcraft_frame::{Chroma, GpuPixels, GpuSurface, PixelData};
        let surface = Arc::new(UnreadableSurface);
        let (w, h) = surface.size();
        let index = (d.pts / 1000) as u8;
        d.frame = VideoFrame::rgba8(w, h, [index, 0, 0, 255].repeat((w * h) as usize));
        d.frame.pts = filmcraft_time::Tick(d.pts);
        if d.pts == self.failing_pts && self.unreadable.load(Ordering::Relaxed) {
            d.frame.data = PixelData::Gpu(GpuPixels::new(surface, filmcraft_frame::GpuFormat::Yuv { chroma: Chroma::C420, bits: u8::BITS }));
        }
        d
    }
}

impl VideoDecoder for FailingOutputDecoder {
    fn decode(&mut self, data: &[u8], pts: i64) -> crate::Result<Vec<DecodedFrame>> {
        let decoded = self.inner.decode(data, pts)?;
        let mut out = Vec::new();
        for d in decoded {
            if self.at_flush && d.pts == self.failing_pts {
                self.held.push(d);
            } else {
                out.push(self.picture(d));
            }
        }
        Ok(out)
    }
    fn flush(&mut self) -> crate::Result<Vec<DecodedFrame>> {
        let mut out = self.inner.flush()?;
        out.append(&mut self.held);
        if self.fail_drain && self.unreadable.load(Ordering::Relaxed) {
            return Err(CodecError::Decode("injected drain failure".into()));
        }
        Ok(out.into_iter().map(|d| self.picture(d)).collect())
    }
    fn reset(&mut self) {
        self.inner.reset();
        self.held.clear();
    }
    fn name(&self) -> &str {
        "frame-access failure"
    }
}

impl VideoSamples for FailingOutputSamples {
    fn count(&self) -> usize {
        self.samples.count()
    }
    fn pts(&self, i: usize) -> i64 {
        self.samples.pts(i)
    }
    fn sync_before(&self, i: usize) -> usize {
        self.samples.sync_before(i)
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        self.samples.sample_at(t)
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        self.samples.read(i)
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        Ok(Box::new(FailingOutputDecoder {
            inner: self.samples.make_decoder()?,
            failing_pts: self.failing_pts,
            unreadable: self.unreadable.clone(),
            at_flush: self.at_flush,
            fail_drain: self.fail_drain,
            held: Vec::new(),
        }))
    }
}

#[test]
fn a_repeated_frame_access_failure_never_returns_an_older_picture() {
    // Predecessor, middle picture and end picture: fail during decode or the final drain.
    const SAMPLE_COUNT: usize = 3;
    for at_flush in [false, true] {
        for rotate in [false, true] {
            let samples = samples(SAMPLE_COUNT, SAMPLE_COUNT, 0, false);
            let failing_index = if at_flush { SAMPLE_COUNT - 1 } else { SAMPLE_COUNT / 2 };
            let failing_pts = samples.pts(failing_index);
            let s = FailingOutputSamples { samples, failing_pts, unreadable: Arc::new(std::sync::atomic::AtomicBool::new(true)), at_flush, fail_drain: false };
            let cache = if rotate {
                GopCache::new(None).with_rotation(1)
            } else {
                let (w, h) = filmcraft_frame::GpuSurface::size(&UnreadableSurface);
                GopCache::new(None).with_crop(Some(Region { x: 0, y: 0, w: (w / 2) as usize, h: h as usize }))
            };
            assert_eq!(index_of(&cache.frame(&s, s.pts(0)).unwrap()), 0);
            let resets = s.samples.resets.load(Ordering::Relaxed);
            for attempt in 0..2 {
                let result = cache.frame(&s, failing_pts);
                assert!(
                    matches!(&result, Err(CodecError::FrameAccess(why)) if why == "GPU readback failed"),
                    "flush={at_flush}, rotate={rotate}, attempt={attempt}: {result:?}"
                );
            }
            assert!(s.samples.resets.load(Ordering::Relaxed) > resets, "retry must replay the consumed picture");
            s.unreadable.store(false, Ordering::Relaxed);
            let recovered = cache.frame(&s, failing_pts).unwrap();
            assert_eq!(index_of(&recovered), failing_index);
            assert_eq!(recovered.pts, filmcraft_time::Tick(failing_pts));
            assert_eq!(index_of(&cache.frame(&s, s.pts(SAMPLE_COUNT - 1)).unwrap()), SAMPLE_COUNT - 1);
            assert_eq!(index_of(&cache.frame(&s, s.pts(0)).unwrap()), 0);
        }
    }
}

#[test]
fn a_repeated_flush_failure_never_returns_an_older_picture() {
    // Cached predecessor + middle picture + final picture held until drain.
    const SAMPLE_COUNT: usize = 3;
    let samples = samples(SAMPLE_COUNT, SAMPLE_COUNT, 0, false);
    let failing_pts = samples.pts(SAMPLE_COUNT - 1);
    let s = FailingOutputSamples { samples, failing_pts, unreadable: Arc::new(std::sync::atomic::AtomicBool::new(true)), at_flush: true, fail_drain: true };
    let cache = GopCache::new(None);
    assert_eq!(index_of(&cache.frame(&s, s.pts(0)).unwrap()), 0);
    for _ in 0..2 {
        assert!(matches!(cache.frame(&s, failing_pts), Err(CodecError::Decode(why)) if why == "injected drain failure"));
    }
    {
        let _nested = DecodingGuard::enter(&cache as *const GopCache as usize);
        assert!(matches!(cache.frame(&s, failing_pts), Err(CodecError::Decode(why)) if why == "injected drain failure"));
    }
    s.unreadable.store(false, Ordering::Relaxed);
    assert_eq!(index_of(&cache.frame(&s, failing_pts).unwrap()), SAMPLE_COUNT - 1);
    assert_eq!(index_of(&cache.frame(&s, s.pts(0)).unwrap()), 0);
}

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
    fn flush(&mut self) -> crate::Result<Vec<DecodedFrame>> {
        Ok(self.held.drain(..).map(picture).collect())
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

/// A decoder that, like a slice-threaded one, splits each picture's work over rayon.
struct ParDec(Box<dyn VideoDecoder>);

impl VideoDecoder for ParDec {
    fn decode(&mut self, sample: &[u8], pts: i64) -> crate::Result<Vec<DecodedFrame>> {
        use rayon::prelude::*;
        let rows: u64 = (0..64u32)
            .into_par_iter()
            .with_max_len(1)
            .map(|r| (0..20_000u64).fold(u64::from(r), |a, x| a.wrapping_mul(31).wrapping_add(x)))
            .reduce(|| 0, u64::wrapping_add);
        std::hint::black_box(rows);
        self.0.decode(sample, pts)
    }
    fn flush(&mut self) -> crate::Result<Vec<DecodedFrame>> {
        self.0.flush()
    }
    fn reset(&mut self) {
        self.0.reset()
    }
    fn name(&self) -> &str {
        "par"
    }
}

struct ParSamples(Samples);

impl VideoSamples for ParSamples {
    fn count(&self) -> usize {
        self.0.count()
    }
    fn pts(&self, i: usize) -> i64 {
        self.0.pts(i)
    }
    fn sync_before(&self, i: usize) -> usize {
        self.0.sync_before(i)
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        self.0.sample_at(t)
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        self.0.read(i)
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        Ok(Box::new(ParDec(self.0.make_decoder()?)))
    }
}

/// #480: render jobs on rayon asking one source for frames while its decoding (a decoder's
/// slices, rotating the frames) runs on rayon too. A worker that took part of that work and,
/// waiting inside it, picked up another frame request blocked on the cache's lock for good:
/// an export of rotated iPhone clips stopped making frames.
#[test]
fn rayon_frame_requests_while_decoding_on_rayon_never_deadlock() {
    use rayon::prelude::*;
    let s = Arc::new(ParSamples(samples(240, 30, 4, false)));
    let (tx, rx) = std::sync::mpsc::channel();
    let done = Arc::new(AtomicUsize::new(0));
    let counted = done.clone();
    std::thread::spawn(move || {
        for round in 0..40usize {
            // export-like: many frame workers, frames of several GOPs of a fresh source at once
            let c = GopCache::new(None).with_rotation(1);
            (0..96usize).into_par_iter().with_max_len(1).for_each(|k| {
                let i = (k * 37 + round * 53) % 240;
                assert_eq!(index_of(&c.frame(&*s, i as i64 * 1000).expect("frame")), i);
                counted.fetch_add(1, Ordering::Relaxed);
            });
        }
        let _ = tx.send(());
    });
    // A deadlock stops every request of a round for good. On a loaded machine the 3,840
    // requests can take minutes, so the limit applies to the time between two answered ones.
    let mut answered = 0;
    loop {
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(()) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let now = done.load(Ordering::Relaxed);
                assert!(now > answered, "frame requests deadlocked after {now} frames");
                answered = now;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!("a frame request failed"),
        }
    }
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
    // First decode 64 distinct cold frames: each of the four workers owns
    // 16 indices. Multiplication by seven permutes them without overlap.
    // Then let every worker request all 64 cached frames in overlapping order.
    // Concurrent misses for the same cold frame may legitimately decode twice,
    // so they cannot support a scheduler-independent duplicate allowance.
    for requests_per_worker in [16usize, 64] {
        let start = Arc::new(std::sync::Barrier::new(4));
        std::thread::scope(|scope| {
            for t in 0..4usize {
                let (s, c, start) = (s.clone(), c.clone(), start.clone());
                scope.spawn(move || {
                    start.wait();
                    for k in 0..requests_per_worker {
                        let i = (k * 7 + t * 16) % 64;
                        assert_eq!(index_of(&c.frame(&*s, i as i64 * 1000).expect("frame")), i);
                    }
                });
            }
        });
        assert_eq!(s.decodes.load(Ordering::Relaxed), 64, "cold frames decode once; warm requests hit the cache");
    }
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
    assert!(pool.decoders.load(Ordering::Relaxed) <= HARD_DECODERS + 1, "{} live decoders after a burst over {n} clips", pool.decoders.load(Ordering::Relaxed));
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
    let held = pool.bytes.bytes();
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
    let held = || pool.bytes.bytes();
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
    assert_eq!((st.frames.len(), st.bytes.bytes()), (4, 16));
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
    assert_eq!(pool.bytes.bytes(), 88);
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
    fn flush(&mut self) -> crate::Result<Vec<DecodedFrame>> {
        Ok(Vec::new())
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
fn large_frames_cannot_enlarge_the_gop_cache_ceiling() {
    let mut c = GopCache::new(None);
    c.budget = usize::MAX;
    assert_eq!(c.cache_budget(1), FRAME_CACHE_CEILING);
    assert_eq!(c.cache_budget(FRAME_CACHE_CEILING), FRAME_CACHE_CEILING);
    c.budget = 0;
    assert_eq!(c.cache_budget(4), MIN_FRAMES * 4, "small frames retain the playback reserve");
    assert_eq!(c.cache_budget(FRAME_CACHE_CEILING), FRAME_CACHE_CEILING, "large frames are capped instead of reserving 64 copies");
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

/// An open-GOP stream like iPhone HEVC: 8 pictures a GOP; after the first, each GOP stores
/// its random-access picture (display 8·g) first, then two leading pictures shown before it
/// (8·g − 2, 8·g − 1, predicted from the GOP before), then the rest. pts = 1000 · display.
struct OpenGop {
    /// (display index, sync) in decode order
    order: Vec<(i64, bool)>,
    resets: Arc<AtomicUsize>,
}

impl OpenGop {
    fn new(gops: i64) -> Self {
        let mut order: Vec<(i64, bool)> = (0..6).map(|d| (d, d == 0)).collect();
        for g in 1..gops {
            order.extend([(8 * g, true), (8 * g - 2, false), (8 * g - 1, false)]);
            order.extend((8 * g + 1..8 * g + 6).map(|d| (d, false)));
        }
        Self { order, resets: Default::default() }
    }
}

/// Leaves out the leading pictures of the random-access picture a run starts at (RASL
/// pictures, as the HEVC decoders do), outputs the others at once.
struct OpenGopDec {
    run_start: Option<i64>,
    resets: Arc<AtomicUsize>,
}

impl VideoDecoder for OpenGopDec {
    fn decode(&mut self, _sample: &[u8], pts: i64) -> crate::Result<Vec<DecodedFrame>> {
        let start = *self.run_start.get_or_insert(pts);
        Ok(if pts < start { Vec::new() } else { vec![picture((pts, false))] })
    }
    fn flush(&mut self) -> crate::Result<Vec<DecodedFrame>> {
        Ok(Vec::new())
    }
    fn reset(&mut self) {
        self.resets.fetch_add(1, Ordering::Relaxed);
        self.run_start = None;
    }
    fn name(&self) -> &str {
        "open gop"
    }
}

impl VideoSamples for OpenGop {
    fn count(&self) -> usize {
        self.order.len()
    }
    fn pts(&self, i: usize) -> i64 {
        self.order[i].0 * 1000
    }
    fn sync_before(&self, i: usize) -> usize {
        (0..=i).rev().find(|&k| self.order[k].1).unwrap_or(0)
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        self.order.iter().position(|&(d, _)| d == t / 1000)
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        Ok(vec![i as u8])
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        Ok(Box::new(OpenGopDec { run_start: None, resets: self.resets.clone() }))
    }
}

/// A leading picture of an open GOP, asked for first (a clip trimmed to start on it, the
/// playhead parked on it), came out black: decoding started at the random-access picture
/// after it, which leaves it out.
#[test]
fn leading_pictures_of_an_open_gop_decode_from_the_gop_before() {
    let s = OpenGop::new(6);
    for d in [6usize, 7, 14, 15, 39] {
        let c = GopCache::new(None);
        assert_eq!(index_of(&c.frame(&s, d as i64 * 1000).expect("frame")), d);
    }
    // the random-access picture first, then its leading pictures: the run that started at
    // it left them out, so they restart a GOP earlier
    let c = GopCache::new(None);
    assert_eq!(index_of(&c.frame(&s, 16_000).expect("frame")), 16);
    assert_eq!(index_of(&c.frame(&s, 15_000).expect("frame")), 15);
    assert_eq!(index_of(&c.frame(&s, 14_000).expect("frame")), 14);
    // playing through: one seek, every picture in turn
    let s = OpenGop::new(6);
    let c = GopCache::new(None);
    for d in 0..46usize {
        assert_eq!(index_of(&c.frame(&s, d as i64 * 1000).expect("frame")), d);
    }
    assert_eq!(s.resets.load(Ordering::Relaxed), 1);
}

/// Many pictures in one GOP; a decoder that emits a burst past the wanted sample (frame-
/// threaded H.264). Eviction used to pivot on the newest pts and drop the playhead frame.
struct Burst {
    n: usize,
    /// Pictures held before a decode dumps them all.
    burst: usize,
    held: VecDeque<i64>,
    resets: Arc<AtomicUsize>,
    decodes: Arc<AtomicUsize>,
}

impl VideoDecoder for Burst {
    fn decode(&mut self, _sample: &[u8], pts: i64) -> crate::Result<Vec<DecodedFrame>> {
        self.decodes.fetch_add(1, Ordering::Relaxed);
        self.held.push_back(pts);
        if self.held.len() < self.burst {
            return Ok(Vec::new());
        }
        Ok(self.held.drain(..).map(|p| picture((p, false))).collect())
    }
    fn flush(&mut self) -> crate::Result<Vec<DecodedFrame>> {
        Ok(self.held.drain(..).map(|p| picture((p, false))).collect())
    }
    fn reset(&mut self) {
        self.resets.fetch_add(1, Ordering::Relaxed);
        self.held.clear();
    }
    fn name(&self) -> &str {
        "burst"
    }
}

impl VideoSamples for Burst {
    fn count(&self) -> usize {
        self.n
    }
    fn pts(&self, i: usize) -> i64 {
        i as i64 * 1000
    }
    fn sync_before(&self, _i: usize) -> usize {
        0
    }
    fn sample_at(&self, t: i64) -> Option<usize> {
        let i = (t / 1000) as usize;
        (i < self.n).then_some(i)
    }
    fn read(&self, i: usize) -> crate::Result<Vec<u8>> {
        Ok(vec![i as u8])
    }
    fn make_decoder(&self) -> crate::Result<Box<dyn VideoDecoder>> {
        Ok(Box::new(Burst { n: self.n, burst: self.burst, held: VecDeque::new(), resets: self.resets.clone(), decodes: self.decodes.clone() }))
    }
}

#[test]
fn long_gop_seek_keeps_the_target_and_does_not_walk_twice() {
    let s = Burst { n: 200, burst: 90, held: VecDeque::new(), resets: Default::default(), decodes: Default::default() };
    let mut c = GopCache::new(None);
    c.budget = 0;
    // Want a picture early in the GOP; the burst dumps 90 pictures at once (past MIN_FRAMES).
    assert_eq!(index_of(&c.frame(&s, 10_000).expect("frame")), 10);
    assert_eq!(s.resets.load(Ordering::Relaxed), 1);
    let decodes = s.decodes.load(Ordering::Relaxed);
    assert!(decodes <= 90 + 1, "walked {decodes} samples");
    // The wanted picture is still cached: asking again must not restart or re-feed.
    assert_eq!(index_of(&c.frame(&s, 10_000).expect("cached")), 10);
    assert_eq!(s.resets.load(Ordering::Relaxed), 1);
    assert_eq!(s.decodes.load(Ordering::Relaxed), decodes);
    // Playback continues from the same run.
    assert_eq!(index_of(&c.frame(&s, 11_000).expect("frame")), 11);
    assert_eq!(s.resets.load(Ordering::Relaxed), 1);
}

#[test]
fn long_gop_continue_does_not_restart_for_the_next_frame() {
    let s = samples(300, 250, 8, false);
    let mut c = GopCache::new(None);
    c.budget = 0;
    assert_eq!(index_of(&c.frame(&s, 80_000).expect("frame")), 80);
    let resets = s.resets.load(Ordering::Relaxed);
    let decodes = s.decodes.load(Ordering::Relaxed);
    assert_eq!(index_of(&c.frame(&s, 81_000).expect("frame")), 81);
    assert_eq!(s.resets.load(Ordering::Relaxed), resets);
    assert!(s.decodes.load(Ordering::Relaxed) - decodes <= 8 + 2);
}

#[test]
fn release_idle_empties_idle_caches_and_leaves_one_in_use_alone() {
    let pool = Arc::new(Pool::new(MAX_LIVE_DECODERS, IDLE, FRAME_BUDGET).tiered(RECENT, HARD_DECODERS));
    let s: Vec<Samples> = (0..3).map(|_| samples(300, 250, 3, false)).collect();
    let c: Vec<GopCache> = (0..3).map(|_| GopCache::in_pool(None, pool.clone())).collect();
    for k in 0..3 {
        assert_eq!(index_of(&c[k].frame(&s[k], 10_000).expect("frame")), 10);
    }
    assert!(pool.bytes.bytes() > 0);
    assert_eq!(pool.decoders.load(Ordering::Relaxed), 3);

    // a cache another thread holds (it is decoding) keeps its frames and decoder
    let busy = c[2].shared.state.lock().unwrap();
    let held_bytes = busy.bytes.bytes();
    pool.release_idle();
    assert_eq!(busy.bytes.bytes(), held_bytes);
    assert!(busy.decoder.is_some() && !busy.frames.is_empty());
    drop(busy);
    for k in 0..2 {
        let st = c[k].shared.state.lock().unwrap();
        assert!(st.frames.is_empty() && st.drafts.is_empty() && st.bytes.bytes() == 0 && st.decoder.is_none() && st.spare.is_empty());
    }
    assert_eq!(pool.bytes.bytes(), held_bytes, "only the busy cache still counts");
    assert_eq!(pool.decoders.load(Ordering::Relaxed), 1);

    // an emptied cache decodes again from its sync sample, and a second release is harmless
    assert_eq!(index_of(&c[0].frame(&s[0], 20_000).expect("frame")), 20);
    assert_eq!(pool.bytes.bytes(), held_bytes + c[0].shared.state.lock().unwrap().bytes.bytes());
    // the cache that was in use is idle now: nothing is left, and a second release is harmless
    pool.release_idle();
    pool.release_idle();
    assert_eq!(pool.bytes.bytes(), 0);
    assert_eq!(pool.decoders.load(Ordering::Relaxed), 0);
}
