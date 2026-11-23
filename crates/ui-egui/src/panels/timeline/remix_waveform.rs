//! Clip-relative waveform peaks rendered through the same Remix reader as playback.
//! Each arrangement has its own bounded background cache; source peaks stay unchanged.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use web_time::{Duration, Instant};

use filmcraft_media::MediaSource;
use filmcraft_project::{ItemId, ParamValue, TrackItem};

use crate::FilmcraftApp;

const SR: u32 = 48_000;
const BUCKET: usize = 256;
const MAX_PEAKS: usize = 2_000_000;
const MAX_CACHED: usize = 16;
const MAX_CACHED_PEAKS: usize = 4_000_000;
const FIRST_RETRY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    item: ItemId,
    stream: usize,
    duration: i64,
    plan: String,
    levels: String,
}

impl Key {
    fn of(item: &TrackItem) -> Option<Self> {
        if !filmcraft_render::remix::is_remixed(item) {
            return None;
        }
        let effect = item.effects.iter().find(|e| e.effect == filmcraft_render::remix::EFFECT)?;
        let ParamValue::Text(plan) = &effect.param("plan")?.value else { return None };
        let remix = filmcraft_render::remix::Remix::of(item)?;
        let total = remix.pieces.iter().try_fold(0i64, |sum, p| {
            if p.src.0 < 0 || p.len.0 <= 0 || p.src.0.checked_add(p.len.0).is_none() {
                return None;
            }
            sum.checked_add(p.len.0)
        })?;
        if total <= 0 || item.duration.0 <= 0 || remix.xfade.0 < 0 || remix.xfade.0 > total {
            return None;
        }
        let frames = item.duration.to_units_floor(i64::from(SR));
        if frames <= 0 || frames > (MAX_PEAKS * BUCKET) as i64 {
            return None;
        }
        Some(Self {
            item: item.item,
            stream: item.audio_stream,
            duration: item.duration.0,
            plan: plan.clone(),
            // The reader ignores malformed/oversized gain text. Key only its bounded parsed
            // form too, rather than cloning an unbounded ignored parameter into the cache.
            levels: remix.levels.as_ref().map(|levels| levels.to_text()).unwrap_or_default(),
        })
    }
}

#[derive(Clone)]
pub(super) struct Peaks {
    pub(super) values: Arc<Vec<(f32, f32)>>,
    pub(super) maximum: f32,
}

#[derive(Clone, Default)]
pub(super) struct Cache(Arc<Mutex<CacheState>>);

#[derive(Default)]
struct CacheState {
    frame: u64,
    visible: HashSet<Key>,
    visible_peaks: usize,
    entries: HashMap<Key, Entry>,
}

enum Entry {
    Pending { failures: u32 },
    RetryAt { when: Instant, failures: u32 },
    Ready(Peaks),
}

impl Key {
    fn peak_count(&self) -> usize {
        (filmcraft_time::Tick(self.duration).to_units_floor(i64::from(SR)) as usize).div_ceil(BUCKET)
    }
}

impl CacheState {
    fn admit(&mut self, frame: u64, key: &Key) -> bool {
        if frame != self.frame {
            self.frame = frame;
            self.visible.clear();
            self.visible_peaks = 0;
        }
        if self.visible.contains(key) {
            return true;
        }
        let count = key.peak_count();
        if self.visible.len() >= MAX_CACHED || self.visible_peaks + count > MAX_CACHED_PEAKS {
            return false;
        }
        self.visible.insert(key.clone());
        self.visible_peaks += count;
        true
    }

    fn reserve(&mut self, key: &Key) -> bool {
        if self.entries.values().filter(|entry| matches!(entry, Entry::Pending { .. })).count() >= 2 {
            return false;
        }
        if !self.entries.contains_key(key) {
            while self.entries.len() >= MAX_CACHED || self.entries.keys().map(Key::peak_count).sum::<usize>() + key.peak_count() > MAX_CACHED_PEAKS {
                let victim = self.entries.iter().find(|(k, entry)| !self.visible.contains(*k) && !matches!(entry, Entry::Pending { .. }));
                let Some((victim, _)) = victim else { return false };
                let victim = victim.clone();
                self.entries.remove(&victim);
            }
        }
        let failures = match self.entries.get(key) {
            Some(Entry::RetryAt { failures, .. }) => *failures,
            _ => 0,
        };
        self.entries.insert(key.clone(), Entry::Pending { failures });
        true
    }

    fn finish(&mut self, key: Key, result: Option<Peaks>) {
        let Some(Entry::Pending { failures }) = self.entries.get(&key) else { return };
        let entry = match result {
            Some(peaks) => Entry::Ready(peaks),
            None => Entry::RetryAt { when: Instant::now() + FIRST_RETRY * (1 << (*failures).min(5)), failures: failures.saturating_add(1) },
        };
        self.entries.insert(key, entry);
    }
}

pub(super) enum Waveform {
    Source,
    Pending,
    Ready(Peaks),
}

pub(super) fn request(app: &mut FilmcraftApp, ctx: &egui::Context, item: &TrackItem) -> Waveform {
    let Some(key) = Key::of(item) else {
        // The playback reader uses linear audio when the stored plan cannot be parsed.
        return if filmcraft_render::remix::is_remixed(item) && filmcraft_render::remix::Remix::of(item).is_some_and(|remix| !remix.is_empty()) {
            Waveform::Pending
        } else {
            Waveform::Source
        };
    };
    let cache = app.tl.remix_waveforms.clone();
    let frame = ctx.cumulative_frame_nr();
    let now = Instant::now();
    let source;
    {
        let mut state = cache.0.lock().unwrap_or_else(|e| e.into_inner());
        // The same visible keys claim the same bounded budget on every stationary frame.
        // An overflow clip cannot evict another admitted clip and start a repaint loop.
        if !state.admit(frame, &key) {
            return Waveform::Pending;
        }
        match state.entries.get(&key) {
            Some(Entry::Ready(peaks)) => return Waveform::Ready(peaks.clone()),
            Some(Entry::Pending { .. }) => return Waveform::Pending,
            Some(Entry::RetryAt { when: retry, .. }) if *retry > now => {
                ctx.request_repaint_after(retry.saturating_duration_since(now));
                return Waveform::Pending;
            }
            _ => {}
        }
        let Some(online) = app.session.source(item.item) else { return Waveform::Pending };
        source = online;
        if !state.reserve(&key) {
            // The outstanding jobs request a repaint when they release their slots.
            return Waveform::Pending;
        }
    }
    let item = item.clone();
    let repaint = ctx.clone();
    #[cfg(not(target_arch = "wasm32"))]
    let (retry_cache, retry_key) = (cache.clone(), key.clone());
    let run = move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_peaks(&*source, &item))).ok().flatten();
        cache.0.lock().unwrap_or_else(|e| e.into_inner()).finish(key, result);
        repaint.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    if std::thread::Builder::new().name("remix-waveform".into()).spawn(run).is_err() {
        retry_cache.0.lock().unwrap_or_else(|e| e.into_inner()).finish(retry_key, None);
        ctx.request_repaint_after(FIRST_RETRY);
    }
    #[cfg(target_arch = "wasm32")]
    run();
    Waveform::Pending
}

fn render_peaks(source: &dyn MediaSource, item: &TrackItem) -> Option<Peaks> {
    Key::of(item)?;
    let total = usize::try_from(item.duration.to_units_floor(i64::from(SR))).ok()?;
    let mut values = Vec::with_capacity(total.div_ceil(BUCKET));
    let mut maximum = 0f32;
    let mut position = 0usize;
    while position < total {
        // A multiple of the peak bucket, so block boundaries never shift peak positions.
        let count = (BUCKET * 750).min(total - position);
        let failed = Cell::new(false);
        let read = |start, count| match source.audio_stream(item.audio_stream, start, count, SR) {
            Ok(audio) => Some(audio),
            Err(_) => {
                failed.set(true);
                None
            }
        };
        let buffer = filmcraft_render::remix::read(item, &read, position as i64, count, SR)?;
        if failed.get() {
            return None;
        }
        let channel = buffer.channels.first()?;
        for bucket in channel.chunks(BUCKET) {
            let range = bucket.iter().fold((0f32, 0f32), |(lo, hi), &x| (lo.min(x), hi.max(x)));
            maximum = maximum.max(range.0.abs()).max(range.1.abs());
            values.push(range);
        }
        position += count;
    }
    Some(Peaks { values: Arc::new(values), maximum })
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_frame::AudioBuffer;
    use filmcraft_render::remix::{Remix, TickPiece};
    use filmcraft_time::Tick;

    use std::sync::atomic::{AtomicBool, Ordering};

    struct Source(filmcraft_media::MediaInfo, AtomicBool);
    impl MediaSource for Source {
        fn info(&self) -> &filmcraft_media::MediaInfo {
            &self.0
        }
        fn video_frame(&self, _: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
            Err(filmcraft_media::MediaError::NoStream("video"))
        }
        fn audio(&self, start: i64, frames: usize, sr: u32) -> filmcraft_media::Result<AudioBuffer> {
            self.audio_stream(0, start, frames, sr)
        }
        fn audio_stream(&self, stream: usize, start: i64, frames: usize, sr: u32) -> filmcraft_media::Result<AudioBuffer> {
            if self.1.swap(false, Ordering::Relaxed) {
                return Err(filmcraft_media::MediaError::NoStream("audio"));
            }
            let samples = (0..frames)
                .map(|k| {
                    let value = match start + k as i64 {
                        0..512 => 0.1,
                        512..1024 => 0.3,
                        1024..1536 => 0.6,
                        _ => 0.0,
                    };
                    value * if stream == 0 { 1.0 } else { 0.5 }
                })
                .collect();
            Ok(AudioBuffer { sample_rate: sr, channels: vec![samples] })
        }
    }

    fn source() -> Source {
        Source(
            filmcraft_media::MediaInfo {
                name: "levels.wav".into(),
                kind: filmcraft_media::MediaKind::Movie,
                duration: Tick::from_units(1536, i64::from(SR)),
                video: None,
                audio_streams: Vec::new(),
                container: "wav".into(),
                start_timecode: None,
                file_size: None,
            },
            AtomicBool::new(false),
        )
    }

    fn item(pieces: &[(i64, i64)], fade: i64) -> TrackItem {
        let tick = |n| Tick::from_units(n, i64::from(SR));
        let duration: i64 = pieces.iter().map(|(_, len)| len).sum();
        let mut item: TrackItem = serde_json::from_value(serde_json::json!({
            "id":1,"item":2,"name":"music","label":"Violet","start":0,"source_in":0,
            "duration":tick(duration).0,"speed":1.0,"enabled":true,"effects":[]
        }))
        .unwrap();
        Remix {
            target: tick(duration),
            original: tick(1536),
            segments: 50.0,
            variations: 50.0,
            xfade: tick(fade),
            levels: None,
            pieces: pieces.iter().map(|&(src, len)| TickPiece { src: tick(src), len: tick(len) }).collect(),
        }
        .store(&mut item);
        item
    }

    #[test]
    fn stationary_view_stops_starting_jobs_at_capacity() {
        let item = item(&[(0, 512), (1024, 512)], 64);
        let template = Key::of(&item).unwrap();
        let keys: Vec<_> = (0..=MAX_CACHED).map(|id| Key { item: ItemId(id as u64), ..template.clone() }).collect();
        let mut cache = CacheState::default();
        let mut started = 0;
        let peaks = render_peaks(&source(), &item).unwrap();
        for frame in 1..=MAX_CACHED as u64 + 2 {
            for key in &keys {
                if cache.admit(frame, key) && !cache.entries.contains_key(key) && cache.reserve(key) {
                    cache.finish(key.clone(), Some(peaks.clone()));
                    started += 1;
                }
            }
        }
        assert_eq!(started, MAX_CACHED);
        let newly_visible = keys.last().unwrap();
        assert!(cache.admit(MAX_CACHED as u64 + 3, newly_visible));
        assert!(cache.reserve(newly_visible));
        assert_eq!(cache.entries.len(), MAX_CACHED);
    }

    #[test]
    fn transient_read_error_does_not_become_a_silent_waveform() {
        let source = source();
        let item = item(&[(0, 512), (1024, 512)], 64);
        let key = Key::of(&item).unwrap();
        let mut cache = CacheState::default();
        assert!(cache.admit(1, &key) && cache.reserve(&key));
        source.1.store(true, Ordering::Relaxed);
        let failed = render_peaks(&source, &item);
        assert!(failed.is_none());
        cache.finish(key.clone(), failed);
        assert!(matches!(cache.entries.get(&key), Some(Entry::RetryAt { when, failures: 1 }) if *when > Instant::now()));
        assert!(render_peaks(&source, &item).unwrap().maximum > 0.0);
    }

    #[test]
    fn malformed_plan_uses_the_source_waveform() {
        let mut item = item(&[(0, 512), (1024, 512)], 64);
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        assert!(matches!(request(&mut app, &ctx, &item), Waveform::Pending));
        item.effect_mut("remix").unwrap().params.insert("plan".into(), filmcraft_project::Param::new(ParamValue::Text("broken".into())));
        assert!(matches!(request(&mut app, &ctx, &item), Waveform::Source));
    }

    #[test]
    fn ignored_level_metadata_does_not_change_the_waveform_key() {
        let mut item = item(&[(0, 768), (256, 768), (1024, 512)], 64);
        let plain = Key::of(&item).unwrap();
        for text in ["x".repeat(8193), "100;NaN,1,0".into(), "100;1,1,0".into()] {
            item.effect_mut("remix").unwrap().params.insert("joinLevels".into(), filmcraft_project::Param::new(ParamValue::Text(text)));
            assert!(Key::of(&item).unwrap() == plain, "ignored metadata must not grow or change the waveform key");
        }
        let set = |item: &mut TrackItem, text: &str| {
            item.effect_mut("remix").unwrap().params.insert("joinLevels".into(), filmcraft_project::Param::new(ParamValue::Text(text.into())));
        };
        set(&mut item, "100;0.50,1.00,0.000;1.000,0.50,0.0");
        let valid = Key::of(&item).unwrap();
        assert!(valid != plain, "effective gain changes need a new waveform");
        set(&mut item, "100;0.5,1,0;1,0.5,0");
        assert!(Key::of(&item).unwrap() == valid, "equivalent parsed gains share a key");
    }

    #[test]
    fn extended_waveform_continues_past_original_end() {
        let source = source();
        let item = item(&[(0, 768), (256, 768), (1024, 512)], 0);
        let peaks = render_peaks(&source, &item).unwrap();
        assert_eq!(peaks.values.len(), 8);
        assert_eq!(peaks.values[6], (0.0, 0.6));
        assert_eq!(peaks.values[7], (0.0, 0.6), "the extended tail is real audio, not blank source handles");
        assert_eq!(peaks.maximum, 0.6);
    }

    #[test]
    fn shortened_waveform_uses_the_preserved_outro() {
        let item = item(&[(0, 512), (1024, 512)], 0);
        let peaks = render_peaks(&source(), &item).unwrap();
        assert_eq!(&*peaks.values, &[(0.0, 0.1), (0.0, 0.1), (0.0, 0.6), (0.0, 0.6)]);
    }

    #[test]
    fn peaks_include_actual_crossfades_and_selected_stream() {
        let source = source();
        let mut item = item(&[(0, 512), (1024, 512)], 128);
        item.audio_stream = 1;
        let peaks = render_peaks(&source, &item).unwrap();
        let audio = filmcraft_render::remix::read(&item, &|s, n| source.audio_stream(1, s, n, SR).ok(), 0, 1024, SR).unwrap();
        let expected: Vec<_> = audio.channels[0].chunks(BUCKET).map(|c| c.iter().fold((0f32, 0f32), |(lo, hi), &v| (lo.min(v), hi.max(v)))).collect();
        assert_eq!(&*peaks.values, &expected);
        assert!(peaks.maximum <= 0.4);
    }

    #[test]
    fn cache_identity_changes_with_sound_but_not_timeline_position() {
        let first = item(&[(0, 512), (1024, 512)], 128);
        let key = Key::of(&first).unwrap();
        let mut moved = first.clone();
        moved.start = Tick::from_seconds_f64(30.0);
        assert_eq!(Key::of(&moved), Some(key.clone()));
        let changed = item(&[(0, 512), (512, 512)], 128);
        assert_ne!(Key::of(&changed), Some(key.clone()));
        moved.audio_stream = 1;
        assert_ne!(Key::of(&moved), Some(key.clone()));
        moved = first.clone();
        moved.duration += Tick::from_units(256, i64::from(SR));
        assert_ne!(Key::of(&moved), Some(key.clone()));
        moved = first.clone();
        let mut remix = Remix::of(&moved).unwrap();
        remix.levels = Some(filmcraft_audio_dsp::remix::LevelMatch {
            span: Tick::from_seconds_f64(1.0).0,
            joins: vec![filmcraft_audio_dsp::remix::JoinLevel { before: 1.0, after: 0.5, correlation: 0.0 }],
        });
        remix.store(&mut moved);
        assert_ne!(Key::of(&moved), Some(key), "gain changes invalidate the waveform too");
        filmcraft_render::remix::take(&mut moved);
        assert!(Key::of(&moved).is_none(), "revert uses the source waveform again");
    }
}
