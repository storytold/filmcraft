//! `LinkedSource`: a [`MediaSource`] that shows a linked EffectCraft composition live.
//!
//! `video_frame(t)` asks a [`FrameSupply`] for the frame of the composition at `t` and gives it
//! a deadline: one frame duration while the viewer plays, [`PAUSED_WAIT`] otherwise. A frame that
//! is not there in time is shown from the result file (`fallback`), so a worker is never held
//! longer than the deadline. The frame still arrives later and the next ask finds it in the RAM
//! cache of the supply. Audio always comes from the fallback: linked audio stays on the FilmCraft
//! clips.
//!
//! [`PeerFrames`] is the real supply: a [`FrameFetcher`] per scale bucket on EffectCraft's bulk
//! connections. Tests use their own supply.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use craft_ipc::bulk::{Frame, FrameFormat};
use craft_ipc::fetch::{FetchOptions, Fetched, FrameFetcher, FrameKey};
use craft_ipc::{InstanceRecord, LinkError};
use filmcraft_engine::time::{FrameRate, Tick};
use filmcraft_frame::{AudioBuffer, VideoFrame};
use filmcraft_media::{FrameRequest, MediaInfo, MediaSource, Result, SharedSource};

/// How long a paused viewer waits for a live frame before it shows the result file's frame.
pub const PAUSED_WAIT: Duration = Duration::from_millis(250);
/// Requests this close in time and in frame number, in order, count as playback.
const PLAY_GAP: Duration = Duration::from_millis(300);
/// How far apart (in frames) the requests of one play run may be: workers ask ahead and out of order.
const PLAY_SPAN: i64 = 48;
/// Forward steps in a row before requests count as playback.
const PLAY_STREAK: u32 = 3;
/// Scale buckets, largest first. A request uses the smallest bucket that is at least as large, so
/// the frame is never smaller than asked for ("sources may return a larger frame").
const BUCKETS: [u32; 4] = [1000, 500, 250, 125];

/// What a [`FrameSupply`] answered.
#[derive(Debug, Clone)]
pub enum Supplied {
    Frame(Arc<Frame>),
    /// Not ready within the time given. It may be ready for the next ask.
    Late,
    /// The peer cannot give this frame.
    Failed,
    /// The peer is gone.
    Down,
}

/// Where a [`LinkedSource`] gets live frames.
pub trait FrameSupply: Send + Sync {
    /// The frame at `frame` (`time_s` seconds of comp time) at an epoch of at least `epoch`,
    /// at `scale` (at least that large), waiting at most `wait`.
    fn frame(&self, scale: f32, frame: i64, time_s: f64, epoch: u64, wait: Duration) -> Supplied;
}

/// Counts of where frames came from, for the badge and `perf.stats`.
#[derive(Debug, Default)]
pub struct Counters {
    pub live: AtomicU64,
    /// Frames shown from the result file because the live frame was late.
    pub late: AtomicU64,
    /// Frames shown from the result file because the peer failed or went away.
    pub fallback: AtomicU64,
}

impl Counters {
    /// `(live, late, fallback)` so far.
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (self.live.load(Ordering::Relaxed), self.late.load(Ordering::Relaxed), self.fallback.load(Ordering::Relaxed))
    }
}

/// Decides whether the requests that arrive look like playback. Pure, so it is easy to test.
#[derive(Debug, Default)]
pub struct Cadence {
    high: Option<i64>,
    at: Option<Instant>,
    streak: u32,
}

impl Cadence {
    /// Note a request for `frame` at `now`. Returns true while the requests look like playback.
    pub fn note(&mut self, frame: i64, now: Instant) -> bool {
        let recent = self.at.is_some_and(|at| now.saturating_duration_since(at) < PLAY_GAP);
        match self.high {
            Some(high) if recent && frame > high && frame - high <= PLAY_SPAN => {
                self.streak += 1;
                self.high = Some(frame);
            }
            // Workers ask out of order and ahead: a frame inside the run keeps it going.
            Some(high) if recent && frame <= high && high - frame <= PLAY_SPAN => {}
            _ => {
                self.streak = 0;
                self.high = Some(frame);
            }
        }
        self.at = Some(now);
        self.streak >= PLAY_STREAK
    }
}

/// A linked composition as a media source. See the module docs.
pub struct LinkedSource {
    link_id: String,
    fallback: SharedSource,
    supply: Arc<dyn FrameSupply>,
    /// The newest epoch the peer announced. Shared with the manager, which raises it on `link.changed`.
    epoch: Arc<AtomicU64>,
    rate: FrameRate,
    cadence: Mutex<Cadence>,
    counters: Arc<Counters>,
}

impl LinkedSource {
    pub fn new(link_id: &str, fallback: SharedSource, supply: Arc<dyn FrameSupply>, epoch: Arc<AtomicU64>, rate: FrameRate) -> Self {
        Self {
            link_id: link_id.to_string(),
            fallback,
            supply,
            epoch,
            rate: rate.sane(),
            cadence: Mutex::new(Cadence::default()),
            counters: Arc::new(Counters::default()),
        }
    }

    pub fn link_id(&self) -> &str {
        &self.link_id
    }

    /// The result-file (or slate) source this one falls back to.
    pub fn fallback(&self) -> &SharedSource {
        &self.fallback
    }

    pub fn counters(&self) -> Arc<Counters> {
        Arc::clone(&self.counters)
    }

    fn cadence(&self) -> MutexGuard<'_, Cadence> {
        self.cadence.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_for(&self, playing: bool) -> Duration {
        if playing { duration_of(self.rate.frame_duration()) } else { PAUSED_WAIT }
    }
}

fn duration_of(tick: Tick) -> Duration {
    Duration::from_secs_f64(tick.seconds().max(0.0))
}

impl MediaSource for LinkedSource {
    fn info(&self) -> &MediaInfo {
        self.fallback.info()
    }

    fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>> {
        let frame = self.rate.frame_at(req.time);
        let playing = self.cadence().note(frame, Instant::now());
        let time_s = self.rate.tick_of(frame).seconds();
        let epoch = self.epoch.load(Ordering::SeqCst);
        match self.supply.frame(req.scale, frame, time_s, epoch, self.wait_for(playing)) {
            Supplied::Frame(live) => {
                self.counters.live.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::new(video_frame_of(&live, req.time)))
            }
            Supplied::Late => {
                self.counters.late.fetch_add(1, Ordering::Relaxed);
                self.fallback.video_frame(req)
            }
            Supplied::Failed | Supplied::Down => {
                self.counters.fallback.fetch_add(1, Ordering::Relaxed);
                self.fallback.video_frame(req)
            }
        }
    }

    fn audio(&self, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        self.fallback.audio(start, frames, sample_rate)
    }

    fn audio_stream(&self, stream: usize, start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
        self.fallback.audio_stream(stream, start, frames, sample_rate)
    }
}

/// EffectCraft sends premultiplied RGBA8. FilmCraft's `Rgba8` pixels are straight alpha.
fn video_frame_of(live: &Frame, pts: Tick) -> VideoFrame {
    let mut data = Vec::with_capacity(live.w as usize * live.h as usize * 4);
    let row = live.w as usize * 4;
    for line in live.pixels.chunks(live.stride.max(1) as usize).take(live.h as usize) {
        data.extend_from_slice(line.get(..row).unwrap_or(line));
    }
    unpremultiply(&mut data);
    VideoFrame::rgba8(live.w, live.h, data).with_pts(pts)
}

/// Divide colour by alpha, in place, rounding to nearest. Opaque and empty pixels are untouched.
pub fn unpremultiply(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        if a == 255 {
            continue;
        }
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        for channel in &mut px[..3] {
            *channel = ((u32::from(*channel) * 255 + a / 2) / a).min(255) as u8;
        }
    }
}

/// The smallest bucket (in thousandths of full size) that is at least `scale`.
pub fn bucket_for(scale: f32) -> u32 {
    let wanted = (scale.clamp(0.0, 1.0) * 1000.0).ceil() as u32;
    BUCKETS.iter().rev().copied().find(|bucket| *bucket >= wanted).unwrap_or(BUCKETS[0])
}

/// Live frames from a peer: one [`FrameFetcher`] per scale bucket, opened when first needed.
pub struct PeerFrames {
    record: InstanceRecord,
    link_id: String,
    options: FetchOptions,
    fetchers: Mutex<BTreeMap<u32, Arc<FrameFetcher>>>,
}

impl PeerFrames {
    /// Open the full-size fetcher now, so a peer that cannot serve frames fails here.
    pub fn connect(record: &InstanceRecord, link_id: &str, options: FetchOptions) -> std::result::Result<Self, LinkError> {
        let first = FrameFetcher::connect(record, link_id, FrameFormat::Rgba8Premul, 1.0, options.clone())?;
        let mut fetchers = BTreeMap::new();
        fetchers.insert(BUCKETS[0], Arc::new(first));
        Ok(Self { record: record.clone(), link_id: link_id.to_string(), options, fetchers: Mutex::new(fetchers) })
    }

    /// True once the full-size connections are gone.
    pub fn is_down(&self) -> bool {
        self.fetchers().get(&BUCKETS[0]).is_none_or(|fetcher| fetcher.is_down())
    }

    fn fetchers(&self) -> MutexGuard<'_, BTreeMap<u32, Arc<FrameFetcher>>> {
        self.fetchers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn fetcher(&self, bucket: u32) -> Option<Arc<FrameFetcher>> {
        if let Some(found) = self.fetchers().get(&bucket) {
            return Some(Arc::clone(found));
        }
        // Smaller scales hold fewer bytes per frame, so a smaller cache keeps as many frames.
        let options = FetchOptions { cache_bytes: self.options.cache_bytes / 2, ..self.options.clone() };
        let opened = FrameFetcher::connect(&self.record, &self.link_id, FrameFormat::Rgba8Premul, f64::from(bucket) / 1000.0, options).ok()?;
        let mut fetchers = self.fetchers();
        Some(Arc::clone(fetchers.entry(bucket).or_insert_with(|| Arc::new(opened))))
    }
}

impl FrameSupply for PeerFrames {
    fn frame(&self, scale: f32, frame: i64, time_s: f64, epoch: u64, wait: Duration) -> Supplied {
        let bucket = bucket_for(scale);
        let Some(fetcher) = self.fetcher(bucket) else { return Supplied::Failed };
        match fetcher.fetch(FrameKey { frame, scale_milli: bucket }, time_s, epoch, wait) {
            Fetched::Frame(live) => Supplied::Frame(live),
            Fetched::TimedOut => Supplied::Late,
            Fetched::Failed(_) => Supplied::Failed,
            Fetched::Down => Supplied::Down,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use filmcraft_media::{AudioStreamInfo, MediaKind, VideoStreamInfo};

    use super::*;

    fn info() -> MediaInfo {
        MediaInfo {
            name: "result".into(),
            kind: MediaKind::Movie,
            duration: FrameRate::FPS_24.tick_of(48),
            video: Some(VideoStreamInfo {
                width: 4,
                height: 2,
                frame_rate: FrameRate::FPS_24,
                par: (1, 1),
                codec: "ProRes 4444".into(),
                pixel_format: "yuva444p12".into(),
                color: filmcraft_color::ColorInfo::SRGB_FULL,
                color_assumed: None,
                has_alpha: true,
                bitrate: None,
                hdr: None,
            }),
            audio_streams: Vec::<AudioStreamInfo>::new(),
            container: "mov".into(),
            start_timecode: None,
            file_size: None,
        }
    }

    /// Always answers with a gray frame whose red value is the result file's marker, 200.
    struct FileSource(MediaInfo);

    impl MediaSource for FileSource {
        fn info(&self) -> &MediaInfo {
            &self.0
        }
        fn video_frame(&self, req: FrameRequest) -> Result<Arc<VideoFrame>> {
            Ok(Arc::new(VideoFrame::rgba8(4, 2, [200, 0, 0, 255].repeat(8)).with_pts(req.time)))
        }
        fn audio(&self, _start: i64, frames: usize, sample_rate: u32) -> Result<AudioBuffer> {
            Ok(AudioBuffer::silence(sample_rate, 2, frames))
        }
    }

    #[derive(Clone)]
    enum Mode {
        Live,
        Late,
        Failed,
        Down,
    }

    struct Fake {
        mode: Mutex<Mode>,
        asked: Mutex<Vec<(f32, i64, f64, u64, Duration)>>,
    }

    impl FrameSupply for Fake {
        fn frame(&self, scale: f32, frame: i64, time_s: f64, epoch: u64, wait: Duration) -> Supplied {
            self.asked.lock().unwrap().push((scale, frame, time_s, epoch, wait));
            match self.mode.lock().unwrap().clone() {
                // Premultiplied: red 64 at alpha 128 is straight red 128.
                Mode::Live => Supplied::Frame(Arc::new(Frame {
                    req: 1,
                    epoch,
                    w: 4,
                    h: 2,
                    stride: 16,
                    format: FrameFormat::Rgba8Premul,
                    render_ms: 1.0,
                    pixels: [64, 0, 0, 128].repeat(8),
                })),
                Mode::Late => Supplied::Late,
                Mode::Failed => Supplied::Failed,
                Mode::Down => Supplied::Down,
            }
        }
    }

    fn source(mode: Mode) -> (LinkedSource, Arc<Fake>, Arc<AtomicU64>) {
        let fake = Arc::new(Fake { mode: Mutex::new(mode), asked: Mutex::new(Vec::new()) });
        let epoch = Arc::new(AtomicU64::new(3));
        let linked =
            LinkedSource::new("link-a", Arc::new(FileSource(info())), Arc::clone(&fake) as Arc<dyn FrameSupply>, Arc::clone(&epoch), FrameRate::FPS_24);
        (linked, fake, epoch)
    }

    fn pixel(frame: &VideoFrame) -> [u8; 4] {
        match &frame.data {
            filmcraft_frame::PixelData::Rgba8(data) => [data[0], data[1], data[2], data[3]],
            other => panic!("expected Rgba8, got {other:?}"),
        }
    }

    #[test]
    fn a_live_frame_is_unpremultiplied_and_asked_for_at_the_current_epoch() {
        let (linked, fake, epoch) = source(Mode::Live);
        let frame = linked.video_frame(FrameRequest { time: FrameRate::FPS_24.tick_of(5), scale: 1.0 }).unwrap();
        assert_eq!(pixel(&frame), [128, 0, 0, 128], "64 at alpha 128 is straight 128");
        assert_eq!((frame.width, frame.height), (4, 2));
        let asked = fake.asked.lock().unwrap().clone();
        assert_eq!((asked[0].1, asked[0].3), (5, 3));
        assert!((asked[0].2 - 5.0 / 24.0).abs() < 1e-9);
        epoch.store(9, Ordering::SeqCst);
        linked.video_frame(FrameRequest { time: FrameRate::FPS_24.tick_of(5), scale: 1.0 }).unwrap();
        assert_eq!(fake.asked.lock().unwrap()[1].3, 9, "a new epoch is asked for at once");
        assert_eq!(linked.counters().live.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_late_frame_comes_from_the_result_file_without_waiting() {
        let (linked, fake, _) = source(Mode::Late);
        let frame = linked.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(1))).unwrap();
        assert_eq!(pixel(&frame), [200, 0, 0, 255], "the result file's frame");
        assert_eq!(fake.asked.lock().unwrap()[0].4, PAUSED_WAIT, "a viewer that is not playing waits 250 ms");
        assert_eq!(linked.counters().late.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn failed_and_down_peers_fall_back_to_the_file() {
        for mode in [Mode::Failed, Mode::Down] {
            let (linked, _, _) = source(mode);
            let frame = linked.video_frame(FrameRequest::full(Tick::ZERO)).unwrap();
            assert_eq!(pixel(&frame), [200, 0, 0, 255]);
            assert_eq!(linked.counters().fallback.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn playback_shortens_the_wait_to_one_frame_duration() {
        let (linked, fake, _) = source(Mode::Live);
        for n in 0..6 {
            linked.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(n))).unwrap();
        }
        let asked = fake.asked.lock().unwrap().clone();
        assert_eq!(asked[0].4, PAUSED_WAIT);
        let frame_time = duration_of(FrameRate::FPS_24.frame_duration());
        assert_eq!(asked[5].4, frame_time);
        assert!(frame_time < PAUSED_WAIT);
        // A jump far away is a seek, not playback.
        linked.video_frame(FrameRequest::full(FrameRate::FPS_24.tick_of(400))).unwrap();
        assert_eq!(fake.asked.lock().unwrap()[6].4, PAUSED_WAIT);
    }

    #[test]
    fn cadence_ignores_out_of_order_asks_inside_a_run_and_ends_after_a_pause() {
        let mut cadence = Cadence::default();
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert!(!cadence.note(10, at(0)));
        assert!(!cadence.note(11, at(40)));
        assert!(!cadence.note(12, at(80)));
        assert!(cadence.note(13, at(120)));
        assert!(cadence.note(11, at(130)), "a worker asking for an earlier frame of the run keeps it going");
        assert!(cadence.note(14, at(160)));
        assert!(!cadence.note(15, at(160 + 400)), "a pause longer than the gap restarts");
    }

    #[test]
    fn audio_and_info_come_from_the_fallback() {
        let (linked, _, _) = source(Mode::Live);
        assert_eq!(linked.info().name, "result");
        let audio = linked.audio(0, 100, 48_000).unwrap();
        assert_eq!(audio.frames(), 100);
    }

    #[test]
    fn unpremultiply_handles_empty_opaque_and_translucent_pixels() {
        let mut px = vec![9, 9, 9, 0, 10, 20, 30, 255, 50, 100, 100, 200, 1, 1, 1, 1];
        unpremultiply(&mut px);
        assert_eq!(px[..4], [0, 0, 0, 0]);
        assert_eq!(px[4..8], [10, 20, 30, 255]);
        assert_eq!(px[8..12], [64, 128, 128, 200]);
        assert_eq!(px[12..16], [255, 255, 255, 1], "never above 255");
    }

    #[test]
    fn scale_buckets_never_go_below_the_ask() {
        assert_eq!(bucket_for(1.0), 1000);
        assert_eq!(bucket_for(0.9), 1000);
        assert_eq!(bucket_for(0.5), 500);
        assert_eq!(bucket_for(0.26), 500);
        assert_eq!(bucket_for(0.25), 250);
        assert_eq!(bucket_for(0.1), 125);
        assert_eq!(bucket_for(0.001), 125);
    }
}
