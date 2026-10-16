//! Clip-relative waveform peaks rendered through the same Remix reader as playback.
//! Each arrangement has its own bounded background cache; source peaks stay unchanged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use filmcraft_media::MediaSource;
use filmcraft_project::{ItemId, ParamValue, TrackItem};

use crate::FilmcraftApp;

const SR: u32 = 48_000;
const BUCKET: usize = 256;
const MAX_PEAKS: usize = 2_000_000;
const MAX_CACHED: usize = 16;
const MAX_CACHED_PEAKS: usize = 4_000_000;

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
            levels: effect.param("joinLevels").and_then(|p| if let ParamValue::Text(s) = &p.value { Some(s.clone()) } else { None }).unwrap_or_default(),
        })
    }
}

#[derive(Clone)]
pub(super) struct Peaks {
    pub(super) values: Arc<Vec<(f32, f32)>>,
    pub(super) maximum: f32,
}

#[derive(Clone, Default)]
pub(super) struct Cache {
    entries: Arc<Mutex<HashMap<Key, Option<Peaks>>>>,
    pending: Arc<Mutex<Vec<Key>>>,
}

/// None while the current plan is being generated: never display the old plan's waveform.
pub(super) fn request(app: &mut FilmcraftApp, ctx: &egui::Context, item: &TrackItem) -> Option<Peaks> {
    let key = Key::of(item)?;
    let cache = app.tl.remix_waveforms.clone();
    if let Some(have) = cache.entries.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return have.clone();
    }
    let source = app.session.source(item.item)?;
    {
        let mut pending = cache.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.contains(&key) || pending.len() >= 2 {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return None;
        }
        pending.push(key.clone());
    }
    let item = item.clone();
    let ctx = ctx.clone();
    #[cfg(not(target_arch = "wasm32"))]
    let retry = (cache.clone(), key.clone());
    let run = move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_peaks(&*source, &item))).ok().flatten();
        {
            let mut entries = cache.entries.lock().unwrap_or_else(|e| e.into_inner());
            let incoming = result.as_ref().map_or(0, |p| p.values.len());
            while !entries.is_empty()
                && (entries.len() >= MAX_CACHED || entries.values().flatten().map(|p| p.values.len()).sum::<usize>() + incoming > MAX_CACHED_PEAKS)
            {
                let oldest = entries.keys().next().cloned();
                if let Some(oldest) = oldest {
                    entries.remove(&oldest);
                }
            }
            entries.insert(key.clone(), result);
        }
        cache.pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| *k != key);
        ctx.request_repaint();
    };
    #[cfg(not(target_arch = "wasm32"))]
    if std::thread::Builder::new().name("remix-waveform".into()).spawn(run).is_err() {
        retry.0.pending.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| *k != retry.1);
    }
    #[cfg(target_arch = "wasm32")]
    run();
    None
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
        let buffer =
            filmcraft_render::remix::read(item, &|start, count| source.audio_stream(item.audio_stream, start, count, SR).ok(), position as i64, count, SR)?;
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

    struct Source(filmcraft_media::MediaInfo);
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
        Source(filmcraft_media::MediaInfo {
            name: "levels.wav".into(),
            kind: filmcraft_media::MediaKind::Movie,
            duration: Tick::from_units(1536, i64::from(SR)),
            video: None,
            audio_streams: Vec::new(),
            container: "wav".into(),
            start_timecode: None,
            file_size: None,
        })
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
